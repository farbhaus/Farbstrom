// Unified stage owner. One OvenPlayer instance lives in #tile-stream and
// its source swaps between:
//   • 'live'  — the LL-HLS / WebRTC broadcast (default when a stream key
//               is set and no file is being displayed).
//   • 'file'  — a presenter-driven file (mp4 / webm / .mov H.264) shown
//               to the room via display:state.
//   • 'image' — OvenPlayer destroyed; <img id="display-img"> overlay
//               shown instead (OvenPlayer is video-only).
//   • null    — nothing to show.
// Toolbar play/pause/mute/volume/resync drive whichever source is loaded.

import { getParticipantId, getToken, slug } from './session.js';
import { viewerStore } from './state.js';
import { countPlayerError, logDiag } from './diagnostics.js';
import type { DisplayFileState, WsClientMessage } from './types.js';

type Mode = 'live' | 'file' | 'image' | null;

let player: OvenPlayerInstance | null = null;
let mode: Mode = null;
// Only set while mode === 'file', tracks which file the player is bound
// to so we can no-op when display:state repeats the same fileId.
let currentFileId: string | null = null;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
// Programmatic play/pause/seek (driven by applyDisplayState's
// applyTransport) must not bounce back to the server as a fresh
// presenter transport event. Bumped while a programmatic call is in
// flight; the OvenPlayer event handlers ignore events when > 0.
let suppressTransport = 0;
// True while the user is dragging the seek slider, so the `time` event
// feed doesn't yank the thumb out from under them mid-drag.
let isScrubbing = false;
// Latest server display state for the current file, and a flag set on each
// (re)mount. autoStart races ahead of the synchronous applyTransport in
// applyDisplayFile, so we reconcile once the player actually reaches
// 'playing' — otherwise a late joiner autoplays even when the room is
// paused (the pause branch in applyTransport no-ops while not yet playing).
let lastFileState: DisplayFileState | null = null;
let fileSyncPending = false;

// Set when the browser can't decode a codec the stream is actually using, so
// the error handler stops retrying a source that will never play. Cleared on
// every (re)mount.
let blockedCodec: string | null = null;

// Codecs the startup probe read out of the LL-HLS playlist. Kept for the stats
// panel (gh #40) — the probe already has to parse them, and "what is this
// stream actually sending" is the first thing you want when a client reports a
// bad picture. Empty until a live stream has been probed.
let liveCodecs: string[] = [];

let onPlayingChange: () => void = () => {};
let onPlaybackBlocked: (message: string | null) => void = () => {};
let wsSend: (msg: WsClientMessage) => void = () => {};

export function configurePlayer(opts: {
  onPlayingChange: () => void;
  onPlaybackBlocked: (message: string | null) => void;
  send: (msg: WsClientMessage) => void;
}): void {
  onPlayingChange = opts.onPlayingChange;
  onPlaybackBlocked = opts.onPlaybackBlocked;
  wsSend = opts.send;
}

export function getPlayer(): OvenPlayerInstance | null {
  return player;
}

export interface StreamDiagnostics {
  mode: Mode;
  /** Codec strings from the LL-HLS playlist (LL-HLS describes the stream for
   *  WebRTC rooms too, so this is populated either way). */
  codecs: readonly string[];
  /** The codec this browser refused, when playback is blocked. */
  blockedCodec: string | null;
  /** The element the stats panel samples. OvenPlayer owns it, so it's looked
   *  up rather than held — a remount replaces it. */
  video: HTMLVideoElement | null;
}

export function getStreamDiagnostics(): StreamDiagnostics {
  return {
    mode,
    codecs: liveCodecs,
    blockedCodec,
    video: document.querySelector<HTMLVideoElement>('#player video'),
  };
}

// What's loaded right now. Used by callers like the offline overlay and
// the toolbar (image mode disables play/mute/volume).
export function getPlayerMode(): Mode {
  return mode;
}

// File source URL with the display flag set, so the backend serves it
// inline + relabels video/quicktime → video/mp4 for H.264-in-MOV.
function fileSourceUrl(fileId: string): string {
  return (
    `/api/public/rooms/${encodeURIComponent(slug)}/files/${encodeURIComponent(fileId)}/download` +
    `?participantId=${encodeURIComponent(getParticipantId())}` +
    `&token=${encodeURIComponent(getToken())}&display=1`
  );
}

// Files we can play through OvenPlayer / <video>. .mov is included
// because the backend display-mode relabel makes H.264-in-MOV work in
// Chromium. ProRes / DNxHD will still fail at decode time — handled by
// the OvenPlayer error path.
const PLAYABLE_VIDEO = new Set(['video/mp4', 'video/webm', 'video/quicktime']);
function mimeKind(mime: string): 'image' | 'video' | 'other' {
  const m = mime.toLowerCase();
  if (m.startsWith('image/')) return 'image';
  if (PLAYABLE_VIDEO.has(m)) return 'video';
  return 'other';
}

function destroyPlayerInstance(): void {
  if (retryTimer) clearTimeout(retryTimer);
  retryTimer = null;
  if (player) {
    try {
      player.remove();
    } catch {}
    player = null;
  }
}

export function destroyPlayer(): void {
  destroyPlayerInstance();
  showSeekBar(false);
  mode = null;
  currentFileId = null;
  lastFileState = null;
  fileSyncPending = false;
  blockedCodec = null;
  onPlaybackBlocked(null);
}

// Reload current source. For live mode that pings OvenPlayer's load();
// for file mode it's the same.
export function reloadPlayer(): void {
  if (mode === 'image' || mode === null) return;
  if (retryTimer) clearTimeout(retryTimer);
  if (!player) {
    if (mode === 'live') initLivePlayer();
    return;
  }
  try {
    if (player.getState() === 'playing') return;
    player.load();
  } catch {
    if (mode === 'file' && currentFileId) {
      const id = currentFileId;
      destroyPlayerInstance();
      initFilePlayer(id);
    } else {
      destroyPlayerInstance();
      initLivePlayer();
    }
  }
}

// Public entrypoint: mount the live player if appropriate. A
// presenter-displayed file always wins — when displayFile is set we
// leave the stage to applyDisplayState (triggered by the ws hello
// replay shortly after connect).
export function initPlayer(): void {
  if (viewerStore.get().displayFile) return;
  if (player) return;
  initLivePlayer();
}

// Tear down and mount again, for the two events that mean "this is a different
// stream than the one you have": the room going live, and a stream key being
// assigned or swapped.
//
// `reloadPlayer` is not enough for either. It calls `player.load()` when an
// instance already exists, which re-fetches the same source but never re-runs
// the codec probe — so a viewer sitting in the room while the stream was down
// (probe found no playlist, gave up) would keep a mounted player and get no
// warning when an undecodable stream came up. Likewise `initPlayer` no-ops when
// a player exists, so a key swap left the old key mounted.
//
// Skips the remount when playback is already healthy, so a duplicate room:live
// can't cause a visible blip for someone happily watching.
export function remountLivePlayer(): void {
  if (viewerStore.get().displayFile) return;
  try {
    if (player && player.getState() === 'playing') return;
  } catch {
    // getState can throw on a half-torn-down instance; fall through and remount.
  }
  destroyPlayerInstance();
  blockedCodec = null;
  onPlaybackBlocked(null);
  mode = null;
  initLivePlayer();
}

// ---- Codec support probe ---------------------------------------------------

// The LL-HLS master playlist names the exact codecs OME is packaging (avc1.*,
// hvc1.*, av01.*). Browser support for those diverges sharply — Safari ships no
// AV1 software decoder and needs M3-or-later hardware, and HEVC is
// hardware-gated nearly everywhere — so a perfectly healthy stream can render
// as a permanent black tile. Nothing else in the pipeline can tell the viewer
// that: OvenPlayer just errors, and the handler below would retry every 8 s
// forever.
//
// OME publishes LL-HLS for every stream regardless of the room's delivery mode,
// so this playlist is the codec source of truth for WebRTC rooms too.
async function playlistCodecs(playlistUrl: string): Promise<string[]> {
  let manifest: string;
  try {
    const res = await fetch(playlistUrl);
    // Not live yet, or a transient blip — that's the retry loop's job, not ours.
    if (!res.ok) return [];
    manifest = await res.text();
  } catch {
    return [];
  }
  const codecs = new Set<string>();
  for (const match of manifest.matchAll(/CODECS="([^"]+)"/g)) {
    const list = match[1];
    if (!list) continue;
    for (const codec of list.split(',')) codecs.add(codec.trim());
  }
  return [...codecs];
}

const isAudioCodec = (codec: string): boolean => /^(mp4a|opus|ac-3|ec-3)/.test(codec);

// LL-HLS goes through MSE, which takes the playlist's codec strings verbatim.
// Returns the first codec this browser can't decode, or null.
function unplayableViaMse(codecs: string[]): string | null {
  if (typeof MediaSource === 'undefined') return null;
  for (const codec of codecs) {
    const container = isAudioCodec(codec) ? 'audio/mp4' : 'video/mp4';
    if (!MediaSource.isTypeSupported(`${container}; codecs="${codec}"`)) return codec;
  }
  return null;
}

// Playlist fourcc -> the mime type WebRTC negotiates under. The two namespaces
// are unrelated, so the mapping has to be explicit.
const WEBRTC_MIME: ReadonlyArray<readonly [RegExp, string]> = [
  [/^avc[13]/, 'video/h264'],
  [/^(hvc1|hev1)/, 'video/h265'],
  [/^av01/, 'video/av1'],
  [/^vp0?8/, 'video/vp8'],
  [/^vp0?9/, 'video/vp9'],
];

// WebRTC negotiates codecs in SDP, so MSE support is the wrong question — what
// matters is whether the browser will *accept* the codec in its answer. When it
// won't, OME finds no matching rendition and 403s the session, which is the
// Firefox-plus-H.265 case that looks like a dead stream.
//
// Audio is deliberately ignored: OME transcodes to Opus for WebRTC while the
// playlist advertises the AAC variant, so the playlist's audio entry says
// nothing about this path.
//
// Fails open. Only blocks when the browser reports a non-empty codec list that
// doesn't contain ours — an unknown fourcc, a missing API, or an empty list all
// mean "let it try". A false "can't play" on a working stream would be worse
// than the black tile this replaces.
//
// Catches codec mismatch, NOT profile mismatch: getCapabilities exposes no
// profile detail for HEVC, so a browser advertising video/h265 can still fail on
// profile 4 (Range Extensions, 4:2:2). Don't treat this guard as total.
function unplayableViaWebrtc(codecs: string[]): string | null {
  if (typeof RTCRtpReceiver === 'undefined' || !RTCRtpReceiver.getCapabilities) return null;
  const supported = RTCRtpReceiver.getCapabilities('video')?.codecs ?? [];
  if (supported.length === 0) return null;
  const mimes = new Set(supported.map((c) => c.mimeType.toLowerCase()));

  for (const codec of codecs) {
    if (isAudioCodec(codec)) continue;
    const mapped = WEBRTC_MIME.find(([re]) => re.test(codec))?.[1];
    if (mapped && !mimes.has(mapped)) return codec;
  }
  return null;
}

// Also used by the stats panel, so the readout and the blocked-playback
// message never disagree about a codec's name.
export function codecLabel(codec: string): string {
  if (codec.startsWith('av01')) return 'AV1';
  if (codec.startsWith('hvc1') || codec.startsWith('hev1')) return 'H.265 (HEVC)';
  if (codec.startsWith('avc1')) return 'H.264';
  return codec;
}

// Bumped on every (re)mount and teardown so a poll still in flight from a
// previous stream can't apply its verdict to the current one.
let probeGeneration = 0;

// Polled while a stream is starting, so the interval is the granularity of
// perceived startup: at 1000ms a stream ready at 1.65s isn't mounted until 2s.
// The request is a small playlist fetch and only runs during startup.
const PROBE_INTERVAL_MS = 250;
const PROBE_TIMEOUT_MS = 30000;

// ---- Live broadcast mount --------------------------------------------------

function initLivePlayer(): void {
  if (player) return;
  const { deliveryMode, streamKey } = viewerStore.get();
  // App-only delivery: the broadcast is watched in the native Farbplay app over
  // SRT (H.265). Don't mount a browser player — the room is conference/chat only
  // here. The "watch in Farbplay" placeholder is shown by refreshStatusOverlay.
  if (deliveryMode === 'srt') {
    mode = null;
    enablePlayerControls(false);
    return;
  }
  if (!streamKey) {
    mode = null;
    enablePlayerControls(false);
    return;
  }

  mode = 'live';
  currentFileId = null;
  blockedCodec = null;
  onPlaybackBlocked(null);
  // Don't mount yet — wait until OME has actually packaged the stream. See
  // waitForStreamThenMount.
  waitForStreamThenMount(streamKey, deliveryMode === 'llhls');
}

// The room flips to 'live' the moment OME *admits* the ingest, but the stream
// isn't playable for a moment after that. Mounting straight away means
// OvenPlayer fails against a stream that isn't there and then sits on its 8s
// `connectionTimeout` before trying again — which is why a stream start took a
// stopwatch-measured 11s, and up to 17s when it burned two retries, even though
// the stream itself was ready in well under a second.
//
// So: poll until the stream is packaged, then mount once, already knowing the
// codec. First frame now tracks OME's actual readiness instead of a retry
// timer, and the codec verdict is available at mount time rather than after a
// mount-then-tear-down flash.
function waitForStreamThenMount(streamKey: string, isLlhls: boolean): void {
  const generation = ++probeGeneration;
  const host = location.host;
  const proto = location.protocol === 'https:' ? 'https' : 'http';
  const url = `${proto}://${host}/live/${streamKey}/llhls.m3u8`;
  const deadline = Date.now() + PROBE_TIMEOUT_MS;

  const attempt = async (): Promise<void> => {
    if (generation !== probeGeneration || mode !== 'live') return;
    const codecs = await playlistCodecs(url);
    if (generation !== probeGeneration || mode !== 'live') return;

    if (codecs.length === 0) {
      if (Date.now() < deadline) {
        setTimeout(() => void attempt(), PROBE_INTERVAL_MS);
      } else {
        // Never showed up. Mount anyway rather than leaving a permanently dead
        // tile — LL-HLS could be disabled, or this could be a stream shape the
        // playlist doesn't describe. The player's own retry takes it from here.
        mountLivePlayer(streamKey, isLlhls);
      }
      return;
    }

    liveCodecs = codecs;
    const codec = isLlhls ? unplayableViaMse(codecs) : unplayableViaWebrtc(codecs);
    if (codec) {
      blockedCodec = codec;
      onPlaybackBlocked(`This browser can't decode the ${codecLabel(codec)} video in this stream.`);
      return;
    }
    mountLivePlayer(streamKey, isLlhls);
  };

  void attempt();
}

function mountLivePlayer(streamKey: string, isLlhls: boolean): void {
  if (player || mode !== 'live') return;
  const host = location.host;
  const proto = location.protocol === 'https:' ? 'https' : 'http';
  const wsproto = location.protocol === 'https:' ? 'wss' : 'ws';
  const sources = isLlhls
    ? [{ type: 'll-hls', file: `${proto}://${host}/live/${streamKey}/llhls.m3u8` }]
    : [{ type: 'webrtc', file: `${wsproto}://${host}/live/${streamKey}` }];

  player = OvenPlayer.create('player', {
    autoStart: true,
    autoFallback: false,
    mute: true,
    sources,
    parseStream: { enabled: true },
    webrtcConfig: { timeoutMaxRetry: 3, connectionTimeout: 8000 },
    hlsConfig: { liveSyncDuration: 1, liveMaxLatencyDuration: 2, maxLiveSyncPlaybackRate: 1 },
  });

  enablePlayerControls(true);
  showSeekBar(false);
  syncPlayerControls();

  // Event handlers — wired exactly as on dev to avoid behavioural drift.
  // (file mode uses bindCommonEvents + an extra `play`/`pause`/`seek`
  // wiring; for live we stay 1:1 with what worked before.)
  player.on('stateChanged', (e) => {
    syncPlayerControls();
    if (!player || mode !== 'live') return;
    if (e?.newstate === 'playing') {
      onPlayingChange();
    } else if (e?.newstate === 'error') {
      // Retry silently. Offline overlay is driven by setRoomStatus /
      // room:pending — don't race it from here. A codec this browser can't
      // decode is the one error retrying can never fix, so don't loop on it.
      countPlayerError();
      logDiag(
        'player',
        'error',
        blockedCodec
          ? `Stream playback blocked — ${codecLabel(blockedCodec)} not decodable here`
          : 'Stream playback error — retrying',
      );
      if (viewerStore.get().status !== 'ended' && !blockedCodec) {
        retryTimer = setTimeout(reloadPlayer, 8000);
      }
    }
  });
  player.on('mute', () => syncPlayerControls());
  player.on('volumeChanged', () => syncPlayerControls());
}

// ---- File mount ------------------------------------------------------------

function initFilePlayer(fileId: string): void {
  destroyPlayerInstance();
  showImageOverlay(null);
  const isPresenter = viewerStore.get().role === 'presenter';
  player = OvenPlayer.create('player', {
    // autoStart so the player at least renders the first frame instead
    // of a blank black box. applyTransport then aligns play/pause/seek
    // to whatever state the server has for the room.
    autoStart: true,
    autoFallback: false,
    mute: !isPresenter,
    sources: [{ type: 'mp4', file: fileSourceUrl(fileId) }],
  });
  mode = 'file';
  currentFileId = fileId;

  enablePlayerControls(true);
  syncPlayerControls();
  bindCommonEvents();

  // Seek bar: visible for everyone in file mode, draggable only for the
  // presenter (viewers stay synced via applyTransport, so their bar is
  // read-only).
  showSeekBar(true);
  (document.getElementById('seek-slider') as HTMLInputElement).disabled = !isPresenter;
  player.on('time', (e) => {
    if (mode !== 'file') return;
    updateSeekBar(e?.position ?? 0, e?.duration ?? 0);
  });

  // autoStart will race ahead and start playing; reconcile to the room's
  // transport once playback has actually begun so a late joiner lands on
  // the right play/pause + position instead of just autoplaying.
  fileSyncPending = true;

  player.on('stateChanged', (e) => {
    if (mode !== 'file') return;
    if (e?.newstate === 'playing') {
      onPlayingChange();
      if (fileSyncPending && lastFileState) {
        fileSyncPending = false;
        applyTransport(lastFileState);
      }
    }
    if (e?.newstate === 'error') {
      // The file can't be decoded in this browser (ProRes / DNxHD MOV,
      // unsupported codec, etc.). Drop the source so the presenter knows.
      handleFileError();
    }
  });

  // Presenter transport echo: tell the server when local playback state
  // changes so other viewers stay in sync. Skip when we're applying a
  // server-driven state (suppressTransport > 0).
  if (isPresenter) {
    const emit = (override?: { playing?: boolean; position?: number }): void => {
      if (suppressTransport > 0 || !player) return;
      const state = player.getState();
      const playing = override?.playing ?? state === 'playing';
      const position = override?.position ?? player.getPosition();
      if (!Number.isFinite(position)) return;
      wsSend({ type: 'display:transport', playing, position });
    };
    player.on('play', () => emit({ playing: true }));
    player.on('pause', () => emit({ playing: false }));
    player.on('seek', (e) => {
      const pos = typeof e?.offset === 'number' ? e.offset : undefined;
      emit(pos !== undefined ? { position: pos } : {});
    });
  }
}

function handleFileError(): void {
  const isPresenter = viewerStore.get().role === 'presenter';
  if (isPresenter) {
    // Clear it for everyone — server validates that we're the presenter.
    wsSend({ type: 'display:set', fileId: null });
    // Toast deferred to display.ts caller? Inline here to keep player.ts
    // standalone — import toast helper.
    void import('../shared/utils.js').then(({ toast }) => {
      toast("Couldn't play this file in the browser. Try MP4 or WebM.");
    });
  }
}

// ---- Image overlay --------------------------------------------------------

// The unified stage tile (#tile-stream) is visible whenever there's
// something to show: a live stream key OR a presenter-displayed file.
function updateStageVisibility(): void {
  const { streamKey, displayFile, deliveryMode } = viewerStore.get();
  const tile = document.getElementById('tile-stream');
  if (!tile) return;
  // App-only (SRT) rooms have no browser broadcast — the tile only appears for
  // a presenter-displayed file.
  const hasBrowserBroadcast = !!streamKey && deliveryMode !== 'srt';
  if (hasBrowserBroadcast || displayFile) tile.classList.remove('hidden');
  else tile.classList.add('hidden');
}

function showImageOverlay(url: string | null): void {
  const img = document.getElementById('display-img') as HTMLImageElement | null;
  if (!img) return;
  if (url) {
    if (img.getAttribute('src') !== url) img.src = url;
    img.style.display = '';
  } else {
    img.style.display = 'none';
    img.removeAttribute('src');
  }
}

// ---- display:state entrypoint ---------------------------------------------

// Called by ws.ts whenever the server reports a new display state. Owns
// switching modes, swapping sources, and applying the transport snapshot.
export function applyDisplayState(state: DisplayFileState | null): void {
  if (!state || !state.fileId) {
    // Clear the file/image and fall back to live (if any) or unmount FIRST,
    // so `mode` is settled before the store write below fires the subscriber
    // that re-evaluates the offline overlay (which keys off getPlayerMode()).
    showImageOverlay(null);
    if (mode === 'file' || mode === 'image') {
      destroyPlayerInstance();
      mode = null;
      currentFileId = null;
      initLivePlayer();
    }
    viewerStore.set({ displayFile: null });
    updateStageVisibility();
    return;
  }

  viewerStore.set({
    displayFile: { fileId: state.fileId, name: state.name, mime: state.mime },
  });
  updateStageVisibility();

  const kind = mimeKind(state.mime);
  if (kind === 'image') {
    // Image mode: tear down OvenPlayer, show <img>.
    destroyPlayerInstance();
    showImageOverlay(fileSourceUrl(state.fileId));
    mode = 'image';
    currentFileId = state.fileId;
    enablePlayerControls(false);
    showSeekBar(false);
    return;
  }
  if (kind === 'video') {
    applyDisplayFile(state);
    return;
  }
  // Unknown / unplayable — clear (shouldn't happen, canShow filters).
  applyDisplayState(null);
}

function applyDisplayFile(state: DisplayFileState): void {
  lastFileState = state;
  // (Re)mount the file player only when the file actually changes.
  if (mode !== 'file' || currentFileId !== state.fileId) {
    initFilePlayer(state.fileId);
  }
  applyTransport(state);
}

// Predict the presenter's current head: if `playing`, extrapolate from
// the last position + elapsed wall-clock since the server timestamp.
function predictedHead(state: DisplayFileState): number {
  if (!state.playing) return state.position;
  const elapsed = Math.max(0, (Date.now() - state.updatedAtMs) / 1000);
  return state.position + elapsed;
}

function applyTransport(state: DisplayFileState): void {
  if (!player || mode !== 'file') return;
  const head = predictedHead(state);
  const current = player.getPosition();
  if (Number.isFinite(head) && Number.isFinite(current) && Math.abs(current - head) > 0.5) {
    suppressTransport++;
    try {
      player.seek(head);
    } finally {
      suppressTransport--;
    }
  }
  const playerState = player.getState();
  const isPlaying = playerState === 'playing';
  if (state.playing && !isPlaying) {
    suppressTransport++;
    try {
      player.play();
    } finally {
      suppressTransport--;
    }
  } else if (!state.playing && isPlaying) {
    suppressTransport++;
    try {
      player.pause();
    } finally {
      suppressTransport--;
    }
  }
}

// ---- Controls -------------------------------------------------------------

function bindCommonEvents(): void {
  if (!player) return;
  player.on('mute', () => syncPlayerControls());
  player.on('volumeChanged', () => syncPlayerControls());
  player.on('stateChanged', () => syncPlayerControls());
}

function enablePlayerControls(on: boolean): void {
  (document.getElementById('play-btn') as HTMLButtonElement).disabled = !on;
  (document.getElementById('mute-btn') as HTMLButtonElement).disabled = !on;
  (document.getElementById('volume-slider') as HTMLInputElement).disabled = !on;
}

function fmtTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const total = Math.floor(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, '0')}`;
}

// The seek bar only makes sense for a seekable file. Live broadcast isn't
// seekable and image mode has no player, so it's hidden in those modes.
function showSeekBar(on: boolean): void {
  const group = document.getElementById('seek-group');
  if (group) group.style.display = on ? 'flex' : 'none';
  if (!on) {
    isScrubbing = false;
    const slider = document.getElementById('seek-slider') as HTMLInputElement | null;
    if (slider) {
      slider.value = '0';
      slider.max = '0';
    }
    const time = document.getElementById('seek-time');
    if (time) time.textContent = '0:00 / 0:00';
  }
}

function updateSeekBar(position: number, duration: number): void {
  const slider = document.getElementById('seek-slider') as HTMLInputElement | null;
  const time = document.getElementById('seek-time');
  if (Number.isFinite(duration) && duration > 0) {
    if (slider) slider.max = String(duration);
  }
  if (!isScrubbing && slider && Number.isFinite(position)) {
    slider.value = String(position);
  }
  if (time) time.textContent = `${fmtTime(position)} / ${fmtTime(duration)}`;
}

function syncPlayerControls(): void {
  if (!player) return;
  const playing = player.getState() === 'playing';
  const iconPlay = document.getElementById('icon-play');
  const iconPause = document.getElementById('icon-pause');
  if (iconPlay) iconPlay.style.display = playing ? 'none' : '';
  if (iconPause) iconPause.style.display = playing ? '' : 'none';

  const muted = player.getMute();
  const iconVol = document.getElementById('icon-vol');
  const iconMuted = document.getElementById('icon-muted');
  if (iconVol) iconVol.style.display = muted ? 'none' : '';
  if (iconMuted) iconMuted.style.display = muted ? '' : 'none';
  const muteBtn = document.getElementById('mute-btn');
  muteBtn?.classList.toggle('muted', muted);
  if (muteBtn) muteBtn.title = muted ? 'Unmute (M)' : 'Mute (M)';

  const slider = document.getElementById('volume-slider') as HTMLInputElement;
  slider.value = String(player.getVolume());
}

export function initPlayerControls(): void {
  document.getElementById('play-btn')?.addEventListener('click', () => {
    if (!player) return;
    if (player.getState() === 'playing') player.pause();
    else player.play();
  });
  document.getElementById('mute-btn')?.addEventListener('click', () => {
    if (!player) return;
    player.setMute(!player.getMute());
  });
  document.getElementById('volume-slider')?.addEventListener('input', (e) => {
    if (!player) return;
    const vol = parseInt((e.target as HTMLInputElement).value, 10);
    if (player.getMute() && vol > 0) player.setMute(false);
    player.setVolume(vol);
    syncPlayerControls();
  });
  const seekSlider = document.getElementById('seek-slider') as HTMLInputElement | null;
  if (seekSlider) {
    // While dragging: hold the time-feed off the thumb and show the
    // target time live. On release: commit the seek — for the presenter
    // this fires OvenPlayer's `seek` event, which already broadcasts
    // display:transport, so viewers follow.
    seekSlider.addEventListener('input', () => {
      isScrubbing = true;
      const pos = parseFloat(seekSlider.value);
      const time = document.getElementById('seek-time');
      if (time) time.textContent = `${fmtTime(pos)} / ${fmtTime(parseFloat(seekSlider.max))}`;
    });
    seekSlider.addEventListener('change', () => {
      isScrubbing = false;
      if (player) player.seek(parseFloat(seekSlider.value));
    });
  }
  document.getElementById('resync-btn')?.addEventListener('click', () => {
    // For file mode, rebind to the same file (forces a fresh source).
    // For live mode, destroy + remount.
    if (mode === 'file' && currentFileId) {
      const id = currentFileId;
      destroyPlayerInstance();
      initFilePlayer(id);
    } else {
      destroyPlayerInstance();
      mode = null;
      initLivePlayer();
    }
  });
}
