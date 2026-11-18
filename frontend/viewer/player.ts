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

import { forceFallback, getParticipantId, getToken, slug } from './session.js';
import { viewerStore } from './state.js';
import { applySavedSpeaker } from './devices.js';
import { countPlayerError, logDiag } from './diagnostics.js';
import type { DisplayFileState, WsClientMessage } from './types.js';
import {
  FALLBACK_PLAYLIST,
  codecLabel,
  fetchPlaylist,
  firstVariantUri,
  isAvcCodec,
  parseCodecs,
  parseTargetDuration,
  unplayableViaMse,
  unplayableViaWebrtc,
  videoCodecOf,
} from '../shared/codecs.js';

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

// Codecs the startup probe read out of the LL-HLS playlist of the version that
// is mounted. Kept for the stats panel (gh #40) — the probe already has to
// parse them, and "what is this stream actually sending" is the first thing you
// want when a client reports a bad picture. Empty until a live stream has been
// probed.
let liveCodecs: string[] = [];

// Which of the stream's two versions is mounted. An encoder can send an H.264
// fallback beside its HEVC in the same SRT stream (Farbcode does); OME then
// publishes the main picture on the usual URLs and the H.264 on its `fallback`
// playlist (FALLBACK_PLAYLIST, and the <Playlist> block in Server.xml). Every
// browser that can decode the main picture gets it; the rest get the fallback.
//
// Two ways onto the fallback. The startup probe takes it at once when the main
// picture's codec is one this browser can't decode. And when the browser
// accepts the codec but can't decode it — getCapabilities has no HEVC profile
// detail, so a browser advertising video/h265 can still fail on 4:2:2 — the
// decode watchdog and the player's decode errors take it after two startup
// failures, the first having restarted the main picture. (`#stream=fallback`
// forces it, for testing.) A main picture that has decoded once is never
// abandoned: a flaky one gets the ordinary retry. A switch is sticky
// (`variantLock`) until the stream changes (remountLivePlayer) or the viewer
// presses Resync, which is the way back from a wrong call; reloads and the
// relay remount keep it.
type Variant = 'primary' | 'fallback';
let variant: Variant = 'primary';
let variantLock: Variant | null = null;
// The fallback playlist's codecs once the probe has found one with a video
// track; empty for a stream that has no fallback (or before it was looked for).
let fallbackCodecs: string[] = [];
let decodeWatchdog: ReturnType<typeof setInterval> | null = null;
// Startup decode failures — a decode error, or a silent watchdog window — the
// main picture has had without ever showing a frame. One isn't proof (measured:
// headless Chrome intermittently fails the first second of an H.265 LL-HLS
// stream that its retry then plays), so it takes two.
let startupDecodeFailures = 0;
// Whether the main picture has decoded at all since the stream was (re)chosen.
// Held here rather than read off the element: OvenPlayer's own quick recovery
// rebuilds the <video> (measured: four decode errors inside 0.7 s, frame count
// back to zero after the first two), and a picture that has decoded once is a
// flaky one for the retry, never one this browser can't decode.
let primaryEverDecoded = false;
// How long the fallback playlist is polled after the main one is up. OME serves
// it from the moment the stream exists, but a non-2xx is "not yet", not "no".
const FALLBACK_WAIT_MS = 3000;
// The watchdog's window: twice the keyframe interval — a session can only start
// decoding at a keyframe — and never less than this. With SegmentDuration 1 in
// Server.xml an LL-HLS segment is one GOP, so #EXT-X-TARGETDURATION is the
// keyframe interval; when it can't be read, a 5 s GOP is assumed.
const WATCHDOG_MIN_MS = 5000;
const WATCHDOG_DEFAULT_MS = 10000;

// WebRTC transport for the live broadcast. The choice rides on the playback URL
// as `?transport=`, which both ends read (`webrtc_publisher.cpp` in OME,
// `searchParams.get("transport")` in OvenPlayer):
//
//   all   — OME offers its direct UDP candidates *and* its TCP relay (TURN on
//           :3478), and OvenPlayer builds the RTCPeerConnection with
//           `iceTransportPolicy: 'all'`, so the browser races the two and takes
//           UDP when it gets through. Measured 2026-11-13: Chrome and Firefox on
//           UDP in 0.2–0.4 s, and with their UDP dropped, on the relay in
//           0.3–0.55 s.
//   relay — only the relay. The fallback, see below.
//
// `all` must be in the URL itself: OvenPlayer sets `iceTransportPolicy:
// 'relay'` whenever OME offers a relay server and the URL doesn't say `all`, so
// OME's `<DefaultTransport>all</DefaultTransport>` alone meant relay-only.
//
// The relay used to be forced for everyone. That put every viewer behind their
// own TCP stack, and one MacBook collapsed its receive window for 1–2.5 s every
// 30 s, in Chrome and Safari alike, stalling delivery while UDP to the same
// machine was unaffected.
//
// The safety net: if nothing plays 6 s after a mount or reload, remount on
// `relay`, sticky for the page. The race cannot catch a path that connects but
// carries no video, and OME sends media down whichever pair the browser
// nominates *first*, ignoring later nominations. Safari with iCloud Private
// Relay hits exactly that: its UDP goes through Apple's relay, connects and
// completes DTLS in under a second, and no picture arrives. When that pair wins
// the race, only this timer gets it onto the relay.
type WebrtcTransport = 'all' | 'relay';
let webrtcTransport: WebrtcTransport = 'all';
let relayFallbackTimer: ReturnType<typeof setTimeout> | null = null;
// A session normally plays within 1–3 s (ICE well under a second, then the
// next keyframe). Kept under OvenPlayer's own 8 s connectionTimeout so the
// fallback fires before it starts retrying the same dead path.
const RELAY_FALLBACK_MS = 6000;

// The path the broadcast is actually arriving on, read from the peer
// connection's stats: the candidate pair with the most bytes received, and its
// local candidate's type (`relay` is the TCP relay; host/srflx/prflx is UDP).
// Bytes rather than the browser's selected pair, because OME keeps sending on
// the first nominated pair even after the browser prefers another — Safari
// showed its UDP pair selected while every byte came over the relay. Null until
// the first video bytes land. Refreshed by the stats sampler once a second.
type WebrtcPath = 'UDP' | 'TCP relay';
let peerConnection: RTCPeerConnection | null = null;
let webrtcPath: WebrtcPath | null = null;
let pathQueryInFlight = false;

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
  /** The path a WebRTC broadcast is actually arriving on, or null until the
   *  first bytes land (and always for LL-HLS). */
  webrtcPath: WebrtcPath | null;
  /** Which version of the broadcast is mounted, or null when no broadcast is. */
  variant: Variant | null;
  /** Whether the stream carries an H.264 fallback this browser can play. */
  fallbackAvailable: boolean;
}

export function getStreamDiagnostics(): StreamDiagnostics {
  return {
    mode,
    codecs: liveCodecs,
    blockedCodec,
    video: document.querySelector<HTMLVideoElement>('#player video'),
    webrtcPath,
    variant: mode === 'live' && player ? variant : null,
    fallbackAvailable: fallbackCodecs.length > 0,
  };
}

// Re-read which path the broadcast arrives on. Called by the stats sampler once
// a second; cheap, and a no-op without a live WebRTC peer connection. Logs a
// change of path to the diagnostics panel, so a fallback shows up there.
export function refreshWebrtcPath(): void {
  const pc = peerConnection;
  if (!pc || pathQueryInFlight || mode !== 'live') return;
  pathQueryInFlight = true;
  pc.getStats()
    .then((report) => {
      if (pc !== peerConnection) return;
      let bestBytes = 0;
      let bestLocal: string | null = null;
      report.forEach((s) => {
        if (s.type === 'candidate-pair' && typeof s.bytesReceived === 'number' && s.bytesReceived > bestBytes) {
          bestBytes = s.bytesReceived;
          bestLocal = typeof s.localCandidateId === 'string' ? s.localCandidateId : null;
        }
      });
      if (!bestLocal) return;
      const local = report.get(bestLocal);
      const path: WebrtcPath = local?.candidateType === 'relay' ? 'TCP relay' : 'UDP';
      if (path !== webrtcPath) {
        webrtcPath = path;
        logDiag('player', 'info', `Broadcast arriving over ${path === 'UDP' ? 'UDP' : 'the TCP relay'}`);
      }
    })
    .catch(() => {
      // A closed or half-built connection; the next tick tries again.
    })
    .finally(() => {
      pathQueryInFlight = false;
    });
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
  clearRelayFallback();
  clearDecodeWatchdog();
  markFallback(false);
  peerConnection = null;
  webrtcPath = null;
  // Let go of the instance before removing it: remove() can emit a last state
  // change, and with `player` still pointing at it the live handlers took it for
  // the current player — logging an error and arming a retry for one that was
  // already gone.
  const inst = player;
  player = null;
  if (inst) {
    try {
      inst.remove();
    } catch {}
  }
  if (retryTimer) clearTimeout(retryTimer);
  retryTimer = null;
}

export function destroyPlayer(): void {
  destroyPlayerInstance();
  showSeekBar(false);
  mode = null;
  currentFileId = null;
  lastFileState = null;
  fileSyncPending = false;
  blockedCodec = null;
  forgetVariant();
  onPlaybackBlocked(null);
}

// The stream changed, or the viewer asked to start over: decide the version
// afresh on the next mount.
function forgetVariant(): void {
  variant = 'primary';
  variantLock = null;
  fallbackCodecs = [];
  startupDecodeFailures = 0;
  primaryEverDecoded = false;
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
    // A reload re-runs ICE on the same transport, so it gets the same
    // deadline: a path that worked at mount can stop carrying the picture
    // (the viewer moved networks, or the race now lands on a dead pair).
    if (mode === 'live') armRelayFallback();
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
// can't cause a visible blip for someone happily watching — unless `force`:
// a new delivery mode means a different transport, healthy or not.
export function remountLivePlayer(force = false): void {
  if (viewerStore.get().displayFile) return;
  try {
    if (!force && player && player.getState() === 'playing') return;
  } catch {
    // getState can throw on a half-torn-down instance; fall through and remount.
  }
  destroyPlayerInstance();
  blockedCodec = null;
  onPlaybackBlocked(null);
  forgetVariant();
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
// forever. (The support checks live in frontend/shared/codecs.ts, shared with
// the admin dashboard's preview.)
//
// OME publishes LL-HLS for every stream regardless of the room's delivery mode,
// so this playlist is the codec source of truth for WebRTC rooms too — and the
// fallback's LL-HLS playlist is how a WebRTC room learns there is a fallback.

// Bumped on every (re)mount and teardown so a poll still in flight from a
// previous stream can't apply its verdict to the current one.
let probeGeneration = 0;

// Polled while a stream is starting, so the interval is the granularity of
// perceived startup: at 1000ms a stream ready at 1.65s isn't mounted until 2s.
// The request is a small playlist fetch and only runs during startup.
const PROBE_INTERVAL_MS = 250;
const PROBE_TIMEOUT_MS = 30000;

const unplayableHere = (codecs: readonly string[], isLlhls: boolean): string | null =>
  isLlhls ? unplayableViaMse(codecs) : unplayableViaWebrtc(codecs);

const delay = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

// The fallback playlist's codecs, empty when the stream has none. OME answers
// 200 with no variant for a stream without a second video track — a definite
// no — so only a missing playlist is asked again, until `waitMs` runs out.
async function probeFallback(
  playbackId: string,
  generation: number,
  waitMs: number,
  intervalMs: number,
): Promise<string[]> {
  const url = liveUrls(playbackId, 'fallback').llhls;
  const until = Date.now() + waitMs;
  for (;;) {
    const manifest = await fetchPlaylist(url);
    if (generation !== probeGeneration) return [];
    if (manifest !== null) return parseCodecs(manifest);
    if (Date.now() >= until) return [];
    await delay(intervalMs);
  }
}

// ---- Live broadcast mount --------------------------------------------------

function initLivePlayer(): void {
  if (player) return;
  const { deliveryMode, playbackId } = viewerStore.get();
  // App-only delivery: the broadcast is watched in the native Farbplay app over
  // SRT (H.265). Don't mount a browser player — the room is conference/chat only
  // here. The "watch in Farbplay" placeholder is shown by refreshStatusOverlay.
  if (deliveryMode === 'srt') {
    mode = null;
    enablePlayerControls(false);
    return;
  }
  if (!playbackId) {
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
  waitForStreamThenMount(playbackId, deliveryMode === 'llhls');
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
// Browser playback is admission-gated: OME asks the backend before starting
// every WebRTC or LL-HLS session, and the backend wants this participant's own
// credentials — the same pair the file endpoints take (`backend/src/playback.rs`).
function playbackQuery(): string {
  return `participantId=${encodeURIComponent(getParticipantId())}&token=${encodeURIComponent(getToken())}`;
}

// Both versions' URLs. The main picture is OME's default playlist; the
// fallback is the `fallback` one, for LL-HLS and WebRTC alike. Either way the
// stream is the path segment after the app, which is what the admission webhook
// gates on (backend/src/routes/webhook.rs).
function liveUrls(playbackId: string, which: Variant): { llhls: string; webrtc: string } {
  const host = location.host;
  const proto = location.protocol === 'https:' ? 'https' : 'http';
  const wsproto = location.protocol === 'https:' ? 'wss' : 'ws';
  const base = `/live/${playbackId}`;
  const fallback = which === 'fallback';
  return {
    llhls: `${proto}://${host}${base}/${fallback ? FALLBACK_PLAYLIST : 'llhls'}.m3u8?${playbackQuery()}`,
    webrtc:
      `${wsproto}://${host}${base}${fallback ? `/${FALLBACK_PLAYLIST}` : ''}` +
      `?${playbackQuery()}&transport=${webrtcTransport}`,
  };
}

function waitForStreamThenMount(playbackId: string, isLlhls: boolean): void {
  const generation = ++probeGeneration;
  const url = liveUrls(playbackId, 'primary').llhls;
  const deadline = Date.now() + PROBE_TIMEOUT_MS;
  const stale = (): boolean => generation !== probeGeneration || mode !== 'live';

  const attempt = async (): Promise<void> => {
    if (stale()) return;
    const manifest = await fetchPlaylist(url);
    if (stale()) return;
    // Not live yet, or a transient blip — that's the retry loop's job.
    const codecs = manifest === null ? [] : parseCodecs(manifest);

    if (codecs.length === 0) {
      if (Date.now() < deadline) {
        setTimeout(() => void attempt(), PROBE_INTERVAL_MS);
      } else {
        // Never showed up. Mount anyway rather than leaving a permanently dead
        // tile — LL-HLS could be disabled, or this could be a stream shape the
        // playlist doesn't describe. The player's own retry takes it from here.
        mountLivePlayer(playbackId, isLlhls, 'primary');
      }
      return;
    }

    const blocked = unplayableHere(codecs, isLlhls);
    if (!blocked && !forceFallback && variantLock !== 'fallback') {
      liveCodecs = codecs;
      mountLivePlayer(playbackId, isLlhls, 'primary');
      // An H.264 main picture plays in every browser: no fallback worth looking
      // for, and nothing for the watchdog to catch. Anything else, find out —
      // without holding up the mount — whether there is one to fall back to.
      const video = videoCodecOf(codecs);
      if (video && !isAvcCodec(video)) {
        void armWatchdogIfFallback(playbackId, isLlhls, generation, manifest ?? '', url);
      }
      return;
    }

    // The main picture won't play here, or the fallback was asked for. A
    // fallback is only taken when this browser can play it. (Nothing reorders
    // a stream whose encoder put the H.264 on the lower PID: an H.264 main
    // picture plays everywhere and is never probed past, which keeps H.264-only
    // streams — most of them — at one playlist request. Farbcode's contract puts
    // the HEVC first.)
    const fallback = await probeFallback(playbackId, generation, FALLBACK_WAIT_MS, PROBE_INTERVAL_MS);
    if (stale()) return;
    if (videoCodecOf(fallback) && !unplayableHere(fallback, isLlhls)) {
      fallbackCodecs = fallback;
      liveCodecs = fallback;
      if (blocked) {
        logDiag('player', 'info', `${codecLabel(blocked)} isn't decodable here — playing the 8-bit H.264 fallback`);
      }
      mountLivePlayer(playbackId, isLlhls, 'fallback');
      return;
    }
    liveCodecs = codecs;
    if (blocked) {
      blockedCodec = blocked;
      onPlaybackBlocked(`This browser can't decode the ${codecLabel(blocked)} video in this stream.`);
      return;
    }
    // The fallback was asked for, but this stream has none: the main picture.
    mountLivePlayer(playbackId, isLlhls, 'primary');
  };

  void attempt();
}

// ---- Decode watchdog -------------------------------------------------------

// Armed on the main picture once the probe has found a fallback this browser can
// play. It covers what the pre-flight check cannot see: a browser that accepts
// the codec — getCapabilities lists video/h265 — and then never decodes a frame
// of it (HEVC 4:2:2 on hardware without a Range Extensions decoder). The tile
// stays black over running audio, and nothing errors.
//
// The verdict needs video arriving AND no frame decoded for a whole window.
// Decoding can only start at a keyframe, so the window is twice the keyframe
// interval; nothing arriving at all is a transport problem — the relay timer's,
// not a codec one. Once a frame decodes, it stands down for the session.
async function armWatchdogIfFallback(
  playbackId: string,
  isLlhls: boolean,
  generation: number,
  manifest: string,
  masterUrl: string,
): Promise<void> {
  const fallback = await probeFallback(playbackId, generation, FALLBACK_WAIT_MS, 1000);
  if (generation !== probeGeneration || variant !== 'primary') return;
  if (!videoCodecOf(fallback) || unplayableHere(fallback, isLlhls)) return;
  fallbackCodecs = fallback;
  const windowMs = await keyframeWindow(manifest, masterUrl);
  if (generation !== probeGeneration || variant !== 'primary') return;
  armDecodeWatchdog(windowMs, generation);
}

// Twice the keyframe interval, from the main picture's chunklist: with
// SegmentDuration 1 (Server.xml) an LL-HLS segment is one GOP, so
// #EXT-X-TARGETDURATION is the interval. OME's chunklist URI carries its own
// `session`, so the request needs no credentials of ours.
async function keyframeWindow(manifest: string, masterUrl: string): Promise<number> {
  const uri = firstVariantUri(manifest);
  const chunklist = uri ? await fetchPlaylist(new URL(uri, masterUrl).toString()) : null;
  const seconds = chunklist ? parseTargetDuration(chunklist) : null;
  return seconds ? Math.max(WATCHDOG_MIN_MS, 2 * seconds * 1000) : WATCHDOG_DEFAULT_MS;
}

function armDecodeWatchdog(windowMs: number, generation: number): void {
  clearDecodeWatchdog();
  let arrivingSince: number | null = null;
  let lastTime = -1;
  let busy = false;
  decodeWatchdog = setInterval(() => {
    if (busy) return;
    if (generation !== probeGeneration || mode !== 'live' || variant !== 'primary' || !player) {
      clearDecodeWatchdog();
      return;
    }
    busy = true;
    void decodeProgress()
      .then((progress) => {
        if (!progress || generation !== probeGeneration || variant !== 'primary') return;
        if (progress.decoded > 0) {
          // It decodes here: an earlier startup failure was a hiccup.
          primaryEverDecoded = true;
          startupDecodeFailures = 0;
          clearDecodeWatchdog();
          return;
        }
        // WebRTC says whether video bytes are coming in; LL-HLS has no such
        // count, but its clock runs on the audio, so a moving clock with no
        // frame shown is the same picture.
        const arriving = progress.bytes !== null ? progress.bytes > 0 : lastTime >= 0 && progress.time > lastTime;
        lastTime = progress.time;
        if (!arriving) {
          arrivingSince = null;
          return;
        }
        arrivingSince ??= Date.now();
        if (Date.now() - arrivingSince < windowMs) return;
        const label = codecLabel(videoCodecOf(liveCodecs) ?? 'video');
        const silent = `No ${label} frame decoded in ${Math.round(windowMs / 1000)} s with video arriving`;
        // One silent window is no more proof than one decode error, and counts
        // with them (measured: Chrome decoding nothing of an H.265 session for
        // 5 s that every other session decoded at once). The first starts the
        // main picture over; the second gives up on it.
        startupDecodeFailures += 1;
        if (startupDecodeFailures < 2) {
          logDiag('player', 'warn', `${silent} — restarting the main picture`);
          destroyPlayerInstance();
          initLivePlayer();
          return;
        }
        switchToFallback(`${silent} — switched to the 8-bit H.264 fallback`);
      })
      .finally(() => {
        busy = false;
      });
  }, 1000);
}

function clearDecodeWatchdog(): void {
  if (decodeWatchdog) clearInterval(decodeWatchdog);
  decodeWatchdog = null;
}

// What the browser has decoded of the main picture, and whether video is
// arriving to decode: WebRTC from the receiver's stats, LL-HLS from the element
// (frames shown, and its clock). Null when this browser can't tell, which
// leaves the main picture alone — fail open, like the pre-flight check.
async function decodeProgress(): Promise<{ decoded: number; bytes: number | null; time: number } | null> {
  const video = document.querySelector<HTMLVideoElement>('#player video');
  const shown = video?.getVideoPlaybackQuality?.().totalVideoFrames ?? 0;
  const time = video?.currentTime ?? 0;
  const pc = peerConnection;
  if (pc) {
    try {
      const inbound = await inboundVideo(pc);
      if (!inbound) return { decoded: shown, bytes: 0, time };
      if (typeof inbound.framesDecoded !== 'number') return null;
      return { decoded: Math.max(shown, inbound.framesDecoded), bytes: inbound.bytesReceived ?? 0, time };
    } catch {
      return null;
    }
  }
  if (viewerStore.get().deliveryMode !== 'llhls' || !video?.getVideoPlaybackQuality) return null;
  return { decoded: shown, bytes: null, time };
}

interface InboundVideo {
  bytesReceived?: number;
  framesDecoded?: number;
}

// The receiver's inbound video stats, or null before the first packet lands.
async function inboundVideo(pc: RTCPeerConnection): Promise<InboundVideo | null> {
  const report = await pc.getStats();
  for (const s of report.values()) {
    if (s.type === 'inbound-rtp' && (s.kind ?? s.mediaType) === 'video') return s as InboundVideo;
  }
  return null;
}

// Whether the mounted player's <video> has decoded any frame of its source —
// cumulative for the element, so it still answers after an error.
function hasDecodedFrames(): boolean {
  const video = document.querySelector<HTMLVideoElement>('#player video');
  return (video?.getVideoPlaybackQuality?.().totalVideoFrames ?? 0) > 0;
}

// OvenPlayer reports a <video> decode failure as 303 and an unsupported source
// as 304, but its hls.js provider reports EVERY fatal hls.js error as 302 (306–
// 308 for an HTTP status) with hls.js's own error attached — so a fatal media
// error from LL-HLS is told apart by that error's type, not by the code.
function isDecodeError(e: { code?: number; error?: { type?: string; fatal?: boolean } } | undefined): boolean {
  const video = document.querySelector<HTMLVideoElement>('#player video');
  return (
    e?.code === 303 ||
    e?.code === 304 ||
    (e?.error?.type === 'mediaError' && e.error.fatal === true) ||
    video?.error?.code === MediaError.MEDIA_ERR_DECODE
  );
}

// Move this page onto the fallback until the stream changes or the viewer
// presses Resync. Callers inside OvenPlayer's own event dispatch defer it, so
// an instance is never removed mid-emit.
function switchToFallback(reason: string): void {
  if (mode !== 'live' || variant === 'fallback' || fallbackCodecs.length === 0) return;
  variantLock = 'fallback';
  logDiag('player', 'warn', reason);
  destroyPlayerInstance();
  initLivePlayer();
}

// While the fallback is what's on the stage, the top bar's LIVE badge wears a
// dashed border (www/viewer/index.html) — the only sign of it in the room
// chrome; the connection stats panel spells it out.
function markFallback(on: boolean): void {
  document.body.classList.toggle('stream-fallback', on);
}

// Arm the 6 s deadline for a live WebRTC mount or reload. No-op for LL-HLS and
// once the page has fallen back to the relay.
function armRelayFallback(): void {
  clearRelayFallback();
  if (webrtcTransport !== 'all' || viewerStore.get().deliveryMode === 'llhls') return;
  relayFallbackTimer = setTimeout(() => {
    relayFallbackTimer = null;
    const inst = player;
    if (mode !== 'live' || !inst) return;
    try {
      if (inst.getState() === 'playing') return;
    } catch {
      // A half-torn-down instance can throw; treat it as not playing.
    }
    const pc = peerConnection;
    void (pc ? inboundVideo(pc).catch(() => null) : Promise.resolve(null)).then((inbound) => {
      if (mode !== 'live' || player !== inst || webrtcTransport !== 'all') return;
      // Video is reaching the browser, so the path works and the relay would
      // change nothing. A picture that arrives and doesn't play is the decode
      // watchdog's to judge.
      if ((inbound?.bytesReceived ?? 0) > 0) return;
      webrtcTransport = 'relay';
      logDiag(
        'player',
        'warn',
        `Broadcast did not start within ${RELAY_FALLBACK_MS / 1000} s — switching to the TCP relay`,
      );
      destroyPlayerInstance();
      initLivePlayer();
    });
  }, RELAY_FALLBACK_MS);
}

function clearRelayFallback(): void {
  if (relayFallbackTimer) clearTimeout(relayFallbackTimer);
  relayFallbackTimer = null;
}

function mountLivePlayer(playbackId: string, isLlhls: boolean, which: Variant): void {
  if (player || mode !== 'live') return;
  variant = which;
  const urls = liveUrls(playbackId, which);
  const sources = isLlhls ? [{ type: 'll-hls', file: urls.llhls }] : [{ type: 'webrtc', file: urls.webrtc }];

  const inst = OvenPlayer.create('player', {
    autoStart: true,
    autoFallback: false,
    mute: true,
    sources,
    parseStream: { enabled: true },
    webrtcConfig: { timeoutMaxRetry: 3, connectionTimeout: 8000 },
    hlsConfig: { liveSyncDuration: 1, liveMaxLatencyDuration: 2, maxLiveSyncPlaybackRate: 1 },
  });
  player = inst;

  enablePlayerControls(true);
  showSeekBar(false);
  markFallback(which === 'fallback');
  syncPlayerControls();
  armRelayFallback();

  // Each (re)connect builds a new RTCPeerConnection; keep the latest so the
  // stats panel can tell which path the picture arrives on, and the decode
  // watchdog what has been decoded.
  inst.on('peerConnectionPrepared', (pc) => {
    if (player !== inst) return;
    peerConnection = pc;
    webrtcPath = null;
  });

  // Event handlers — wired exactly as on dev to avoid behavioural drift.
  // (file mode uses bindCommonEvents + an extra `play`/`pause`/`seek`
  // wiring; for live we stay 1:1 with what worked before.)
  inst.on('stateChanged', (e) => {
    syncPlayerControls();
    if (player !== inst || mode !== 'live') return;
    if (e?.newstate === 'playing') {
      clearRelayFallback();
      onPlayingChange();
      routeStageAudio();
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
        // One pending retry, for this instance only. OvenPlayer's own quick
        // recovery can fail several times in under a second, and each failure
        // used to arm another timer over the last — an orphan that later
        // reloaded whatever was on the stage by then (measured: the fallback,
        // 8 s after the main picture's first decode error).
        if (retryTimer) clearTimeout(retryTimer);
        retryTimer = setTimeout(() => {
          if (player === inst) reloadPlayer();
        }, 8000);
      }
    }
  });
  // A main picture that fails to decode twice without ever showing a frame
  // won't fix itself on another retry: with a fallback to go to, go now. The
  // first such failure, and everything else, keeps the retry above — network
  // errors, and decode errors after frames have decoded (measured in WebKit: a
  // fatal decode error 7.7 s into an H.265 LL-HLS stream that a retry plays
  // again) — so a blip never sends anyone to 8-bit.
  inst.on('error', (e) => {
    if (player !== inst || variant !== 'primary' || fallbackCodecs.length === 0 || !isDecodeError(e)) return;
    if (hasDecodedFrames()) primaryEverDecoded = true;
    if (primaryEverDecoded) return;
    startupDecodeFailures += 1;
    if (startupDecodeFailures < 2) return;
    setTimeout(() => {
      if (player !== inst) return;
      const label = codecLabel(videoCodecOf(liveCodecs) ?? 'video');
      switchToFallback(`The ${label} picture failed to decode — switched to the 8-bit H.264 fallback`);
    }, 0);
  });
  inst.on('mute', () => syncPlayerControls());
  inst.on('volumeChanged', () => syncPlayerControls());
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
  const { playbackId, displayFile, deliveryMode } = viewerStore.get();
  const tile = document.getElementById('tile-stream');
  if (!tile) return;
  // App-only (SRT) rooms have no browser broadcast — the tile only appears for
  // a presenter-displayed file.
  const hasBrowserBroadcast = !!playbackId && deliveryMode !== 'srt';
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
  player.on('stateChanged', (e) => {
    syncPlayerControls();
    if (e?.newstate === 'playing') routeStageAudio();
  });
}

// OvenPlayer builds a fresh <video> on every mount, so the participant's chosen
// speaker is re-applied each time playback starts (a no-op when it already is).
function routeStageAudio(): void {
  const video = document.querySelector<HTMLVideoElement>('#player video');
  if (video) applySavedSpeaker(video);
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
      // Resync also decides the version afresh — the way back for a viewer the
      // decode watchdog moved to the fallback.
      destroyPlayerInstance();
      forgetVariant();
      mode = null;
      initLivePlayer();
    }
  });
}
