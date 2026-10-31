// Conference subsystem: LiveKit room lifecycle, tile-grid DOM sync,
// cam/mic/screen-share toggles, device picker, presenter moderation
// (kick + mute). Co-located because tile rendering is tightly coupled to
// LiveKit's track state.

import { confirmModal, noticeModal } from '../shared/components.js';
import { esc, toast } from '../shared/utils.js';
import {
  clearQuality,
  countConfReconnect,
  logDiag,
  setConfState,
  setQuality,
  type Quality,
} from './diagnostics.js';
import {
  applySpeaker,
  canSelectSpeaker,
  deviceNoun,
  forgetRemoteAudio,
  listDevices,
  playTestTone,
  rememberDevice,
  restoreSavedInput,
  savedDevice,
  startMicMeter,
  stopMicMeter,
  trackRemoteAudio,
  type InputKind,
} from './devices.js';
import { morphStage, sizeStage } from './layout.js';
import { disablePointerMode } from './pointer.js';
import { getParticipantId, getToken, PREF_KEY, slug } from './session.js';
import { viewerStore } from './state.js';
import type { LivekitTokenResponse, RosterEntry, TileId, WsClientMessage } from './types.js';

let livekitRoom: LkRoom | null = null;
let activeScreenShareId: string | null = null; // participant.identity or 'local'

// Name for a diagnostic line. Mirrors the tile-label convention used throughout
// this module: LiveKit's display name, falling back to the identity.
const participantLabel = (p: LkRemoteParticipant | LkLocalParticipant): string =>
  ('name' in p ? p.name : '') || p.identity;

// ---- Audio capture preferences (per-room override over admin default) ----
// Keys are slug-scoped so a participant's toggle sticks for this room only.
// Absent → fall back to the room's admin default (viewerStore noise/echoDefault);
// '1'/'0' → explicit participant override.
const noiseKey = (): string => `viewer_noise_reduction_${slug}`;
const echoKey = (): string => `viewer_echo_cancel_${slug}`;
const noiseReductionOn = (): boolean => {
  const v = localStorage.getItem(noiseKey());
  return v === null ? viewerStore.get().noiseDefault : v !== '0';
};
const echoCancelOn = (): boolean => {
  const v = localStorage.getItem(echoKey());
  return v === null ? viewerStore.get().echoDefault : v !== '0';
};

// Push-to-talk preference, slug-scoped like the audio toggles above. Absent →
// fall back to the room's admin default (viewerStore.pttDefault); '1'/'0' →
// explicit participant override.
const pttKey = (): string => `viewer_ptt_${slug}`;
const pttOn = (): boolean => {
  const v = localStorage.getItem(pttKey());
  return v === null ? viewerStore.get().pttDefault : v === '1';
};
// One-time-per-room flag for the push-to-talk entry notice.
const pttNoticeKey = (): string => `viewer_ptt_notice_${slug}`;

// Capture constraints applied whenever the mic track is (re)published.
// voiceIsolation is the stronger, browser-native isolation that supersedes
// noiseSuppression where supported; ignored elsewhere.
function audioCaptureOpts(): AudioCaptureOptions {
  return {
    echoCancellation: echoCancelOn(),
    noiseSuppression: noiseReductionOn(),
    voiceIsolation: noiseReductionOn(),
    autoGainControl: true,
  };
}
let activeScreenShareTrack: LkTrack | null = null;
let selfMuteInFlight = false;

// Wired by main.ts so conference.ts can broadcast host-pin events without
// importing ws.ts (which already imports from here).
let wsSend: (msg: WsClientMessage) => void = () => {};
export function configureConference(opts: { send: (msg: WsClientMessage) => void }): void {
  wsSend = opts.send;
}

const SVG_USER =
  '<svg viewBox="0 0 24 24" stroke="currentColor" fill="none" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/></svg>';
const SVG_MIC =
  '<svg viewBox="0 0 24 24" stroke="currentColor" fill="none" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="1" width="6" height="11" rx="3"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>';
const SVG_MIC_OFF =
  '<svg viewBox="0 0 24 24" stroke="currentColor" fill="none" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><line x1="1" y1="1" x2="23" y2="23"/><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6"/><path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2a7 7 0 0 1-.11 1.23"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>';

// ---- Focus / auto-pin ----

// Resolves a TileId to its DOM element. Self-tile and remote participant
// tiles both map through participantId.
function findTileEl(tileId: TileId): HTMLElement | null {
  if (tileId === 'stream') return document.getElementById('tile-stream');
  if (tileId === 'share') return document.getElementById('tile-share');
  if (tileId === getParticipantId()) return document.getElementById('self-tile');
  return document.getElementById(`conf-tile-${tileId}`);
}

// ---- Active speaker highlight (#248) ----

// Light the tiles of everyone LiveKit currently hears, and clear the rest.
// Kept as a full re-application from the event's complete set: a tile that was
// removed mid-speech (participant left) simply isn't found, and no stale
// highlight can survive a re-render.
const speakingTiles = new Set<HTMLElement>();

function markSpeaking(identities: string[]): void {
  const next = new Set<HTMLElement>();
  for (const id of identities) {
    const tile = findTileEl(id);
    if (tile) next.add(tile);
  }
  for (const tile of speakingTiles) {
    if (!next.has(tile)) tile.classList.remove('is-speaking');
  }
  for (const tile of next) tile.classList.add('is-speaking');
  speakingTiles.clear();
  for (const tile of next) speakingTiles.add(tile);
}

// Inverse of findTileEl — used by the tile click handler to know which tile
// the user pressed.
function getTileIdFromEl(tile: HTMLElement): TileId | null {
  if (tile.id === 'tile-stream') return 'stream';
  if (tile.id === 'tile-share') return 'share';
  if (tile.id === 'self-tile') return getParticipantId();
  if (tile.id.startsWith('conf-tile-')) return tile.id.slice('conf-tile-'.length);
  return null;
}

// Pin a tile to the focus stage (strip on the side), or unpin (grid view).
// `override: true` records the choice as manual so auto-pin won't fight it.
export function setFocus(tileId: TileId | null, opts: { override?: boolean } = {}): void {
  const patch: { focusedTile: TileId | null; focusOverride?: boolean } = {
    focusedTile: tileId,
  };
  if (opts.override !== undefined) patch.focusOverride = opts.override;
  viewerStore.set(patch);

  // The pointer overlay only lives on the stream tile. Any layout where the
  // stream isn't the focused tile makes leftover pointers stale and the
  // active overlay blocks the pin buttons, so disable it on the change.
  if (tileId !== 'stream') disablePointerMode();

  const stage = document.getElementById('stage');
  const strip = document.getElementById('stage-strip');
  if (!stage || !strip) return;

  // Everything below moves tiles between the stage and the call panel and flips
  // the layout mode. morphStage measures around it and animates each tile from
  // where it was to where it lands (#248) — without it the switch is a snap,
  // and re-parented tiles replay their entrance animation mid-jump.
  morphStage(() => {
    if (tileId === null) {
      // Grid mode: all tiles back to the main stage as direct children.
      document.body.classList.remove('has-focus');
      for (const tile of Array.from(strip.querySelectorAll<HTMLElement>(':scope > .tile'))) {
        stage.insertBefore(tile, strip);
      }
      stage
        .querySelectorAll<HTMLElement>('[data-focused]')
        .forEach((el) => el.removeAttribute('data-focused'));
    } else {
      document.body.classList.add('has-focus');
      // Sync the strip-toggle button's lit state to the current confOpen flag
      // — without this it looks "off" on first entry into focus mode even
      // though the strip is visible.
      document
        .getElementById('conf-toggle')
        ?.classList.toggle('panel-open', viewerStore.get().confOpen);
      const focusedEl = findTileEl(tileId);
      // Everything not the focused tile goes into the strip.
      const all = [
        ...Array.from(stage.querySelectorAll<HTMLElement>(':scope > .tile')),
        ...Array.from(strip.querySelectorAll<HTMLElement>(':scope > .tile')),
      ];
      for (const tile of all) {
        if (tile === focusedEl) {
          tile.setAttribute('data-focused', '');
          if (tile.parentElement !== stage) stage.insertBefore(tile, strip);
        } else {
          tile.removeAttribute('data-focused');
          if (tile.parentElement !== strip) strip.appendChild(tile);
        }
      }
    }
  });
}

// Since #248 the pinned tile is the background layer — it fills the stage and
// the video inside it is `object-fit: contain`, so the browser does the
// letterboxing and nothing needs to know the content's aspect ratio. The old
// --focus-aspect plumbing (and layout.ts's limiting-axis pick that consumed it)
// went with that change.

// Apply auto-pin rules unless the viewer has set a manual override.
// preferred is an explicit hint (e.g. the share just started) — useful when
// the call site knows the natural target.
export function requestAutoFocus(preferred?: TileId): void {
  const { focusOverride, playbackId, focusedTile, displayFile, deliveryMode } = viewerStore.get();
  if (focusOverride) return;
  // App-only (SRT) rooms have no browser broadcast tile to focus.
  const hasBrowserBroadcast = !!playbackId && deliveryMode !== 'srt';
  let target: TileId | null;
  if (preferred && findTileEl(preferred)) {
    target = preferred;
  } else if (activeScreenShareTrack) {
    target = 'share';
  } else if (hasBrowserBroadcast || displayFile) {
    // 'stream' tile now hosts either the live broadcast or the
    // presenter-displayed file.
    target = 'stream';
  } else {
    target = null;
  }
  if (focusedTile === target) return;
  setFocus(target, { override: false });
}

// ---- Screen share ----

function showScreenShare(track: LkTrack, label: string): void {
  if (activeScreenShareTrack) {
    activeScreenShareTrack.detach(document.getElementById('screenshare-video'));
  }
  activeScreenShareTrack = track;
  track.attach(document.getElementById('screenshare-video'));
  const lblEl = document.getElementById('tile-share-name');
  if (lblEl) lblEl.textContent = label;
  document.getElementById('tile-share')?.classList.remove('hidden');
  document.body.classList.add('sharing-screen');
  requestAutoFocus('share');
  // requestAutoFocus no-ops when the focus target is unchanged (e.g. the user
  // is already in grid mode), so re-flow the grid explicitly to account for
  // the share tile's visibility flip.
  requestAnimationFrame(sizeStage);
}

function hideScreenShare(): void {
  if (activeScreenShareTrack) {
    activeScreenShareTrack.detach(document.getElementById('screenshare-video'));
    activeScreenShareTrack = null;
  }
  document.getElementById('tile-share')?.classList.add('hidden');
  document.body.classList.remove('sharing-screen');
  activeScreenShareId = null;
  // If the viewer had pinned the share, that target no longer exists —
  // clear the override and let auto-pin pick the next natural target.
  if (viewerStore.get().focusedTile === 'share') {
    viewerStore.set({ focusOverride: false });
  }
  requestAutoFocus();
  // Re-flow the grid even when requestAutoFocus no-ops (already in grid) — the
  // hidden share tile must give up its cell so the remaining tiles re-centre.
  requestAnimationFrame(sizeStage);
}

// ---- LiveKit init ----

export async function initLiveKit(): Promise<void> {
  if (livekitRoom) {
    await livekitRoom.disconnect();
    livekitRoom = null;
    forgetRemoteAudio();
  }

  const res = await fetch(
    `/api/public/rooms/${encodeURIComponent(slug)}/livekit-token` +
      `?participantId=${encodeURIComponent(getParticipantId())}&token=${encodeURIComponent(getToken())}`,
  );
  if (!res.ok) throw new Error('Could not get LiveKit token');
  const { token: lkToken, url: lkUrl } = (await res.json()) as LivekitTokenResponse;

  // Saved devices are requested as a bare (ideal) deviceId so an unplugged one
  // falls back to the system default rather than failing the capture;
  // restoreSavedInput (on LocalTrackPublished) re-finds a renamed id by label.
  const savedMic = savedDevice('audioinput');
  const savedCam = savedDevice('videoinput');
  const savedSpeaker = savedDevice('audiooutput');

  // Screen-share defaults tuned for detail-heavy colour-grading review (GH #171).
  // UHD30 @ 16 Mbps VP9, maintain-resolution under congestion (drop framerate,
  // not sharpness), single non-simulcast layer so the SFU can't forward a low one.
  const room = new LivekitClient.Room({
    audioCaptureDefaults: { ...audioCaptureOpts(), ...(savedMic ? { deviceId: savedMic.id } : {}) },
    ...(savedCam ? { videoCaptureDefaults: { deviceId: savedCam.id } } : {}),
    // Only where LiveKit routes output itself; on Safari devices.ts does it.
    ...(savedSpeaker && LivekitClient.supportsAudioOutputSelection()
      ? { audioOutput: { deviceId: savedSpeaker.id } }
      : {}),
    publishDefaults: {
      videoCodec: 'vp9',
      screenShareEncoding: { maxBitrate: 16_000_000, maxFramerate: 30 },
      degradationPreference: 'maintain-resolution',
      screenShareSimulcastLayers: [],
    },
  });
  livekitRoom = room;

  room.on(LivekitClient.RoomEvent.ParticipantConnected, () => syncConferenceTiles());
  room.on(LivekitClient.RoomEvent.ParticipantDisconnected, () => syncConferenceTiles());
  room.on(LivekitClient.RoomEvent.TrackPublished, () => syncConferenceTiles());
  room.on(LivekitClient.RoomEvent.TrackUnpublished, () => syncConferenceTiles());
  room.on(LivekitClient.RoomEvent.TrackMuted, (pub, participant) => {
    syncConferenceTiles();
    syncLocalMuteState(pub as LkPublication, participant as LkLocalParticipant);
  });
  room.on(LivekitClient.RoomEvent.TrackUnmuted, (pub, participant) => {
    syncConferenceTiles();
    syncLocalMuteState(pub as LkPublication, participant as LkLocalParticipant);
  });
  room.on(LivekitClient.RoomEvent.TrackSubscribed, (track, _pub, participant) => {
    attachTrack(track as LkTrack, participant as LkRemoteParticipant);
  });
  room.on(LivekitClient.RoomEvent.TrackUnsubscribed, (track, _pub, participant) => {
    const t = track as LkTrack;
    const p = participant as LkRemoteParticipant;
    if (t.source === LivekitClient.Track.Source.ScreenShare) {
      if (activeScreenShareId === p.identity) hideScreenShare();
      return;
    }
    forgetRemoteAudio(t.detach());
    syncConferenceTiles();
  });
  room.on(LivekitClient.RoomEvent.LocalTrackPublished, (pub) => {
    const p = pub as LkPublication;
    if (p.source === LivekitClient.Track.Source.Microphone) void restoreSavedInput(room, 'audioinput');
    else if (p.source === LivekitClient.Track.Source.Camera) void restoreSavedInput(room, 'videoinput');
  });
  room.on(LivekitClient.RoomEvent.LocalTrackUnpublished, (pub) => {
    const p = pub as LkPublication;
    if (p.source === LivekitClient.Track.Source.ScreenShare && activeScreenShareId === 'local') {
      hideScreenShare();
      viewerStore.set({ screenOn: false });
      document.getElementById('screen-btn')?.classList.remove('active');
    }
  });
  // Active speakers (#248). One event carries the whole current set — local
  // participant included — so the handler just replaces the highlight wholesale
  // rather than tracking start/stop per person.
  room.on(LivekitClient.RoomEvent.ActiveSpeakersChanged, (speakers) => {
    markSpeaking(((speakers as LkSpeaker[] | undefined) ?? []).map((s) => s.identity));
  });

  // ---- Device health ----
  // The browser refused to autoplay remote audio (Safari does when the call was
  // joined without a click — a saved cam/mic pref connects on page load). LiveKit
  // only reports it; unblocking needs room.startAudio() inside a user gesture.
  room.on(LivekitClient.RoomEvent.AudioPlaybackStatusChanged, () => {
    if (!room.canPlaybackAudio) armAudioUnlock();
  });
  // A freshly captured mic that delivers nothing but digital zeros — the wrong
  // input, or one muted in hardware. LiveKit checks once per capture.
  room.on(LivekitClient.RoomEvent.LocalAudioSilenceDetected, () => {
    logDiag('conference', 'warn', 'Your microphone is sending silence');
    toast("Your microphone isn't picking up any sound — check it in Device Settings.", 6000);
  });
  // LiveKit switches device on its own when the OS default changes or the
  // active one is unplugged; keep an open picker truthful.
  room.on(LivekitClient.RoomEvent.ActiveDeviceChanged, () => void refreshDevicePicker());

  // ---- Connection diagnostics (gh #40) ----
  // LiveKit publishes every participant's quality to every participant, so this
  // one subscription is what lets a host see the whole room's health without a
  // single extra request. State lands in diagnostics.ts, which notifies the
  // roster — going through it rather than calling renderRoster directly keeps
  // conference.ts and roster.ts from importing each other.
  room.on(LivekitClient.RoomEvent.ConnectionQualityChanged, (q, participant) => {
    const who = participant as LkRemoteParticipant | LkLocalParticipant | undefined;
    if (!who) return;
    const next = (q as Quality) || 'unknown';
    const previous = setQuality(who.identity, next);
    // Only log the transitions worth reading back later. A healthy link flaps
    // between excellent and good constantly; poor and lost do not.
    if (previous !== next && (next === 'poor' || next === 'lost')) {
      const mine = who.identity === getParticipantId();
      const label = mine ? 'Your connection' : `${participantLabel(who)}'s connection`;
      logDiag('conference', next === 'lost' ? 'error' : 'warn', `${label} is ${next}`);
    }
  });
  room.on(LivekitClient.RoomEvent.Reconnecting, () => {
    setConfState('reconnecting');
    countConfReconnect();
    logDiag('conference', 'warn', 'Conference connection lost — reconnecting');
  });
  room.on(LivekitClient.RoomEvent.Reconnected, () => {
    setConfState('connected');
    logDiag('conference', 'info', 'Conference reconnected');
  });
  room.on(LivekitClient.RoomEvent.Disconnected, (reason) => {
    setConfState('disconnected');
    clearQuality();
    logDiag(
      'conference',
      'error',
      `Conference disconnected${reason ? ` (${String(reason)})` : ''}`,
    );
  });

  await room.connect(lkUrl, lkToken);
  setConfState('connected');
  logDiag('conference', 'info', 'Conference connected');

  // Attach any tracks already subscribed (participants present before we
  // joined). The post-connect snapshot avoids missing peers that joined
  // during the connect roundtrip.
  syncConferenceTiles();
  for (const p of room.remoteParticipants.values()) {
    for (const pub of p.trackPublications.values()) {
      if (pub.track) attachTrack(pub.track, p);
    }
  }

  const { cameraOn, micOn, pttEnabled } = viewerStore.get();
  if (cameraOn) await room.localParticipant.setCameraEnabled(true);
  // In push-to-talk mode the mic stays muted until the user holds the control,
  // so don't auto-open it on join even if the saved pref wanted mic on — and
  // reflect that muted reality in the store.
  if (micOn && pttEnabled) viewerStore.set({ micOn: false });
  else if (micOn) await room.localParticipant.setMicrophoneEnabled(true, audioCaptureOpts());

  updateSelfTile();
}

export async function disconnectLiveKit(): Promise<void> {
  if (livekitRoom) {
    try {
      await livekitRoom.disconnect();
    } catch {}
    livekitRoom = null;
    forgetRemoteAudio();
  }
  // Deliberate teardown, not a fault — go back to 'idle' rather than the
  // 'disconnected' the Disconnected handler sets, so the stats panel doesn't
  // report a healthy leave as a failure.
  setConfState('idle');
  clearQuality();
  // No longer in the conference — hide the self tile. (The both-off path in
  // updateSelfTile now keeps it visible while connected, so hiding has to
  // happen explicitly here.)
  const selfTile = document.getElementById('self-tile');
  if (selfTile) {
    selfTile.classList.remove('mic-only', 'cam-off');
    selfTile.style.display = 'none';
    selfTile.classList.add('hidden');
  }
  requestAnimationFrame(sizeStage);
}

// ---- Mute state sync (forced-mute detection) ----

function startMicBreathe(): void {
  document.getElementById('mic-btn')?.classList.add('force-muted');
}
function stopMicBreathe(): void {
  document.getElementById('mic-btn')?.classList.remove('force-muted');
}

function syncLocalMuteState(pub: LkPublication, participant: unknown): void {
  if (!livekitRoom || participant !== livekitRoom.localParticipant) return;
  if (pub.source === LivekitClient.Track.Source.Microphone) {
    const { micOn } = viewerStore.get();
    // Forced mute detection: muted event arrived while we still thought mic
    // was on and no local toggle is in flight → host/presenter muted us.
    if (pub.isMuted && micOn && !selfMuteInFlight) startMicBreathe();
    if (!pub.isMuted) stopMicBreathe();
    viewerStore.set({ micOn: !pub.isMuted });
    refreshConfButtons();
    updateSelfTile();
  }
  if (pub.source === LivekitClient.Track.Source.Camera) {
    viewerStore.set({ cameraOn: !pub.isMuted });
    refreshConfButtons();
    updateSelfTile();
  }
}

// ---- Self tile ----

function updateSelfTile(): void {
  const v = document.getElementById('self-preview') as HTMLVideoElement;
  const selfTile = document.getElementById('self-tile') as HTMLElement;
  const micIcon = document.getElementById('self-mic-icon') as HTMLElement;
  const userIcon = document.getElementById('self-user-icon') as HTMLElement;
  const { cameraOn, micOn } = viewerStore.get();

  if (!cameraOn && !micOn) {
    if (livekitRoom) {
      // In the conference but fully muted: keep the tile visible with a
      // placeholder so everyone still knows who's in the room.
      v.srcObject = null;
      selfTile.classList.add('mic-only');
      micIcon.style.display = 'none';
      userIcon.style.display = 'flex';
      selfTile.style.display = 'flex';
      selfTile.classList.remove('hidden');
    } else {
      // Not in the conference — tile stays hidden. The class as well as the
      // inline style: the call panel's "nothing to show" rule can only see
      // `.hidden`, so without it this tile kept an empty panel on screen.
      selfTile.style.display = 'none';
      selfTile.classList.add('hidden');
      selfTile.classList.remove('mic-only');
      micIcon.style.display = 'none';
      userIcon.style.display = 'none';
    }
  } else if (cameraOn && livekitRoom) {
    userIcon.style.display = 'none';
    const camPub = livekitRoom.localParticipant.getTrackPublication(
      LivekitClient.Track.Source.Camera,
    );
    if (camPub?.track) {
      v.srcObject = new MediaStream([camPub.track.mediaStreamTrack]);
    }
    selfTile.classList.remove('mic-only', 'hidden');
    micIcon.style.display = 'none';
    selfTile.style.display = 'block';
  } else {
    v.srcObject = null;
    selfTile.classList.add('mic-only');
    selfTile.classList.remove('hidden');
    micIcon.style.display = '';
    userIcon.style.display = 'none';
    selfTile.style.display = 'flex';
  }
  // Visibility just changed — re-run the grid sizer so the column count
  // matches the new visible-tile total.
  requestAnimationFrame(sizeStage);
}

// ---- Tile grid sync ----

export function syncConferenceTiles(): void {
  const { focusedTile, roster, role: myRole, cameraOn, micOn } = viewerStore.get();
  const stage = document.getElementById('stage');
  const strip = document.getElementById('stage-strip');
  const emptyEl = document.getElementById('stage-empty');
  if (!stage || !strip) return;
  // New tiles created below go into the strip when focus mode is active,
  // otherwise straight into the stage. Existing tiles stay where they are
  // (setFocus is the only thing that re-parents tiles).
  const newTileHost = focusedTile !== null ? strip : stage;

  const lkMap: Map<string, LkRemoteParticipant> = livekitRoom
    ? new Map(Array.from(livekitRoom.remoteParticipants.values()).map((p) => [p.identity, p]))
    : new Map();

  // Every remote participant from the roster gets a tile. Watch-only users
  // (not in LiveKit) render as a placeholder; LK peers attach their cam/mic
  // to the same tile.
  const myPid = getParticipantId();
  const byId = new Map<string, RosterEntry>();
  for (const p of roster) {
    if (p.id !== myPid) byId.set(p.id, p);
  }
  // Race safety: a LK peer might briefly be missing from the roster (e.g.
  // participants:update lagging). Synthesize a minimal entry.
  for (const [id, lkp] of lkMap) {
    if (!byId.has(id)) {
      let role: RosterEntry['role'] = 'viewer';
      try {
        const meta = JSON.parse(lkp.metadata || '{}');
        if (meta.role === 'presenter') role = 'presenter';
      } catch {}
      byId.set(id, { id, name: lkp.name || id, role });
    }
  }

  // Remove tiles for participants no longer present (search both stage and strip).
  for (const tile of [
    ...Array.from(stage.querySelectorAll<HTMLElement>('.tile[id^="conf-tile-"]')),
    ...Array.from(strip.querySelectorAll<HTMLElement>('.tile[id^="conf-tile-"]')),
  ]) {
    const id = tile.id.slice('conf-tile-'.length);
    if (!byId.has(id)) tile.remove();
  }

  for (const [pid, rp] of byId) {
    const lkp = lkMap.get(pid);
    const camPub = lkp?.getTrackPublication(LivekitClient.Track.Source.Camera);
    const micPub = lkp?.getTrackPublication(LivekitClient.Track.Source.Microphone);
    const hasCam = !!(camPub && !camPub.isMuted);
    const hasMic = !!(micPub && !micPub.isMuted);

    let tile = document.getElementById(`conf-tile-${pid}`);
    if (!tile) {
      tile = document.createElement('div');
      tile.id = `conf-tile-${pid}`;
      tile.className = 'tile';

      const isTargetPresenter = rp.role === 'presenter';
      const micSid = micPub?.trackSid || '';
      const micMuted = micPub?.isMuted ?? true;

      tile.innerHTML =
        `<div id="conf-player-${pid}" class="tile-inner">` +
        `<video autoplay playsinline></video></div>` +
        `<div class="conf-user-icon">${SVG_USER}</div>` +
        `<div class="conf-mic-icon" style="display:none">${SVG_MIC}</div>` +
        `<div class="tile-name">${esc(rp.name || pid)}</div>` +
        (myRole === 'presenter' && !isTargetPresenter
          ? `<div class="tile-actions">` +
            `<button class="tile-btn${micMuted ? ' muted-indicator' : ''}" title="${micMuted ? 'Unmute' : 'Mute'}" ` +
            `data-action="presenter-mute" data-identity="${esc(pid)}" data-sid="${esc(micSid)}">${micMuted ? SVG_MIC_OFF : SVG_MIC}</button>` +
            `<button class="tile-btn danger" title="Remove from conference" ` +
            `data-action="presenter-kick" data-identity="${esc(pid)}">` +
            `<svg viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg></button>` +
            `</div>`
          : '');
      newTileHost.appendChild(tile);
    }

    // Keep the display name in sync with roster updates.
    const nameEl = tile.querySelector('.tile-name');
    const nextName = rp.name || pid;
    if (nameEl && nameEl.textContent !== nextName) nameEl.textContent = nextName;

    if (myRole === 'presenter') {
      const muteBtn = tile.querySelector<HTMLButtonElement>('[data-action="presenter-mute"]');
      if (muteBtn) {
        const micSid = micPub?.trackSid || '';
        const micMuted = micPub?.isMuted ?? true;
        muteBtn.className = `tile-btn${micMuted ? ' muted-indicator' : ''}`;
        muteBtn.title = micMuted ? 'Unmute' : 'Mute';
        muteBtn.dataset['sid'] = micSid;
        muteBtn.innerHTML = micMuted ? SVG_MIC_OFF : SVG_MIC;
      }
    }

    tile.classList.toggle('cam-off', !hasCam);
    const micIconEl = tile.querySelector<HTMLElement>('.conf-mic-icon');
    const userIconEl = tile.querySelector<HTMLElement>('.conf-user-icon');
    if (micIconEl) micIconEl.style.display = hasMic && !hasCam ? 'flex' : 'none';
    if (userIconEl) userIconEl.style.display = !hasMic && !hasCam ? 'flex' : 'none';
  }

  // Empty-state placeholder: only meaningful in grid view when there are no
  // tiles at all. In focus mode the focused tile is always visible.
  const hasRemoteTiles = byId.size > 0;
  const streamVisible = !document.getElementById('tile-stream')?.classList.contains('hidden');
  const shareVisible = !document.getElementById('tile-share')?.classList.contains('hidden');
  if (emptyEl) {
    const anyTile = hasRemoteTiles || cameraOn || micOn || streamVisible || shareVisible;
    emptyEl.style.display = anyTile ? 'none' : '';
  }
  sizeStage();
}

function attachTrack(track: LkTrack, participant: LkRemoteParticipant): void {
  // Audio (mic or screen share audio) — auto-attach to a new <audio> element,
  // registered so a speaker choice can reach it where LiveKit won't route it.
  if (track.kind === LivekitClient.Track.Kind.Audio) {
    trackRemoteAudio(track.attach());
    return;
  }
  // Screen share — route to center overlay.
  if (track.source === LivekitClient.Track.Source.ScreenShare) {
    activeScreenShareId = participant.identity;
    activeScreenShareTrack = track;
    showScreenShare(track, (participant.name || participant.identity) + ' — Screen');
    return;
  }
  // Camera — attach to the participant's conf tile.
  const inner = document.getElementById(`conf-player-${participant.identity}`);
  if (!inner) {
    syncConferenceTiles();
    const innerRetry = document.getElementById(`conf-player-${participant.identity}`);
    const v = innerRetry?.querySelector('video');
    if (v) track.attach(v);
    return;
  }
  const video = inner.querySelector('video');
  if (video) track.attach(video);
  syncConferenceTiles();
}

// ---- Conference buttons (cam/mic/screen) ----

interface BtnState {
  active?: boolean;
  muted?: boolean;
  disabled?: boolean;
}

function setConfBtns(camState: BtnState, micState: BtnState): void {
  const camBtn = document.getElementById('cam-btn') as HTMLButtonElement;
  const micBtn = document.getElementById('mic-btn') as HTMLButtonElement;
  camBtn.classList.toggle('active', !!camState.active);
  camBtn.classList.toggle('muted', !!camState.muted);
  camBtn.disabled = !!camState.disabled;
  micBtn.classList.toggle('active', !!micState.active);
  micBtn.classList.toggle('muted', !!micState.muted);
  micBtn.disabled = !!micState.disabled;
}

export function refreshConfButtons(): void {
  const { cameraOn, micOn } = viewerStore.get();
  setConfBtns({ active: cameraOn, muted: !cameraOn }, { active: micOn, muted: !micOn });
}

// Browsers (Safari especially) surface permission/hardware failures as
// DOMException names rather than messages. Map them to actionable copy
// so the toggle doesn't just silently revert.
function deviceErrorMessage(err: unknown, kind: 'cam' | 'mic'): string {
  const label = kind === 'mic' ? 'Microphone' : 'Camera';
  const name = err instanceof Error ? err.name : '';
  switch (name) {
    case 'NotAllowedError':
    case 'SecurityError':
      return `${label} blocked — enable it in your browser's site settings.`;
    case 'NotFoundError':
    case 'OverconstrainedError':
      return `No ${label.toLowerCase()} found on this device.`;
    case 'NotReadableError':
    case 'AbortError':
      return `${label} is in use by another app.`;
    default:
      return `Couldn't enable ${label.toLowerCase()}.`;
  }
}

async function toggleCamera(): Promise<void> {
  const { cameraOn, micOn } = viewerStore.get();
  localStorage.setItem(PREF_KEY, !cameraOn ? (micOn ? 'both' : 'cam') : micOn ? 'mic' : 'none');
  const next = !cameraOn;
  viewerStore.set({ cameraOn: next });
  setConfBtns({ disabled: true }, { active: micOn, disabled: true });
  try {
    if (livekitRoom) {
      await livekitRoom.localParticipant.setCameraEnabled(next);
      updateSelfTile();
    } else {
      await initLiveKit();
    }
  } catch (err) {
    console.error('[conf cam]', err);
    viewerStore.set({ cameraOn });
    toast(deviceErrorMessage(err, 'cam'));
  }
  refreshConfButtons();
}

// Drive the local mic to a target state. Shared by the click toggle and the
// push-to-talk press/release. `persistPref` records the cam+mic combo as the
// join preference (PREF_KEY) — momentary PTT toggles skip it so they don't
// rewrite the saved choice every keypress.
async function setMicLive(on: boolean, persistPref = true): Promise<void> {
  stopMicBreathe(); // user acknowledged — clear any force-mute alert immediately
  const { cameraOn, micOn: prev } = viewerStore.get();
  if (persistPref) {
    localStorage.setItem(PREF_KEY, cameraOn ? (on ? 'both' : 'cam') : on ? 'mic' : 'none');
  }
  viewerStore.set({ micOn: on });
  selfMuteInFlight = true;
  setConfBtns({ active: cameraOn, disabled: true }, { disabled: true });
  try {
    if (livekitRoom) {
      await livekitRoom.localParticipant.setMicrophoneEnabled(on, audioCaptureOpts());
      updateSelfTile();
    } else if (on) {
      await initLiveKit();
    }
  } catch (err) {
    console.error('[conf mic]', err);
    viewerStore.set({ micOn: prev });
    toast(deviceErrorMessage(err, 'mic'));
  } finally {
    selfMuteInFlight = false;
  }
  refreshConfButtons();
}

async function toggleMic(): Promise<void> {
  await setMicLive(!viewerStore.get().micOn);
}

// ---- Push-to-talk ----
// While PTT is enabled the mic control (button hold or `S` key hold) opens the
// mic only for the duration of the hold. `pttHeld` guards re-entry: pointer
// drift and keyboard auto-repeat both fire repeatedly while held.
let pttHeld = false;

export async function pttPress(): Promise<void> {
  // PTT only makes sense once in the conference; otherwise there's no mic to open.
  if (!viewerStore.get().pttEnabled || !livekitRoom) return;
  if (pttHeld || viewerStore.get().micOn) return;
  pttHeld = true;
  await setMicLive(true, false);
}

export async function pttRelease(): Promise<void> {
  if (!pttHeld) return;
  pttHeld = false;
  if (!viewerStore.get().pttEnabled || !livekitRoom) return;
  await setMicLive(false, false);
}

// Reflect PTT vs. toggle mode in the mic button tooltip.
function updateMicTooltip(): void {
  const micBtn = document.getElementById('mic-btn');
  if (micBtn) micBtn.title = viewerStore.get().pttEnabled ? 'Push to talk — hold (S)' : 'Microphone (S)';
}

// Re-evaluate PTT mode once the room default is known (seeded into the store at
// join/resume in screens.ts). initConference() seeds an early default before
// roomInfo has arrived, so showApp() calls this to apply the room's setting.
export function syncPttMode(): void {
  viewerStore.set({ pttEnabled: pttOn() });
  updateMicTooltip();
}

// One-time-per-room explainer shown on room entry when push-to-talk is active,
// so a participant whose mic is muted by the room default learns how to speak
// and how to opt out. Resolves immediately (no modal) when PTT is off or the
// notice has already been shown for this room.
export async function maybeShowPttNotice(): Promise<void> {
  if (!viewerStore.get().pttEnabled) return;
  if (localStorage.getItem(pttNoticeKey())) return;
  localStorage.setItem(pttNoticeKey(), '1');
  // Inline copies of the toolbar mic + device-settings glyphs, self-styled
  // (the notice <p> isn't a .tb-btn) and sized/aligned to the surrounding text.
  const iconAttrs =
    'fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" ' +
    'stroke-linejoin="round" style="width:1.05em;height:1.05em;vertical-align:-0.2em"';
  const micIcon =
    `<svg viewBox="0 0 24 24" ${iconAttrs}><rect x="9" y="1" width="6" height="11" rx="3"/>` +
    '<path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/>' +
    '<line x1="8" y1="23" x2="16" y2="23"/></svg>';
  const gearIcon =
    `<svg viewBox="0 0 24 24" ${iconAttrs}><circle cx="12" cy="12" r="3"/>` +
    '<path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>';
  await noticeModal({
    title: 'Push to talk is on',
    messageHtml:
      'Your microphone stays muted in this room until you choose to speak.\n\n' +
      `Hold the <strong style="color: var(--accent)">S</strong> key — or press and hold ${micIcon} — to talk, then release to mute again.\n\n` +
      `To turn off push-to-talk go to ${gearIcon} Device Settings.`,
    buttonLabel: 'Got it',
  });
}

// Host asked us to unmute. LiveKit can't force a remote mic back on, so the
// server forwards the request here and we ask for consent before re-enabling
// — no surprise hot mic.
export async function requestSelfUnmute(): Promise<void> {
  if (!livekitRoom) return; // not in the conference — nothing to unmute
  if (viewerStore.get().micOn) return; // already live
  const ok = await confirmModal({
    title: 'Unmute?',
    message: 'The host is asking you to unmute your microphone.',
    confirmLabel: 'Unmute',
    cancelLabel: 'Stay muted',
  });
  if (!ok) return;
  if (viewerStore.get().micOn) return; // toggled on while the prompt was open
  await toggleMic();
}

async function toggleScreenShare(): Promise<void> {
  const next = !viewerStore.get().screenOn;
  viewerStore.set({ screenOn: next });
  document.getElementById('screen-btn')?.classList.toggle('active', next);
  try {
    if (!livekitRoom) {
      if (next) await initLiveKit();
      else return;
    }
    await livekitRoom!.localParticipant.setScreenShareEnabled(
      next,
      next ? { resolution: { width: 3840, height: 2160 }, contentHint: 'detail' } : undefined,
    );
    if (next) {
      const pub = livekitRoom!.localParticipant.getTrackPublication(
        LivekitClient.Track.Source.ScreenShare,
      );
      if (pub?.track) {
        activeScreenShareId = 'local';
        activeScreenShareTrack = pub.track;
        showScreenShare(pub.track, 'You — Screen');
      }
    } else {
      hideScreenShare();
    }
  } catch (err) {
    console.error('[screen share]', err);
    viewerStore.set({ screenOn: false });
    document.getElementById('screen-btn')?.classList.remove('active');
    hideScreenShare();
  }
}

// ---- Conference permission prompt ----

// Resolves showConfPrompt()'s promise once the prompt is off screen, so the
// caller can chain something that must not stack on top of it (the first-run
// tour, #230). Deliberately fires when the overlay hides, not when initLiveKit
// finishes — the browser's own camera/mic permission prompt is next in line and
// nothing should wait on the participant answering it.
let confPromptDone: (() => void) | null = null;

async function applyConfPref(pref: 'both' | 'cam' | 'mic' | 'none', save = true): Promise<void> {
  const overlay = document.getElementById('conf-prompt-overlay') as HTMLElement & {
    _dismissHandler?: ((e: MouseEvent) => void) | null;
  };
  if (overlay._dismissHandler) {
    overlay.removeEventListener('click', overlay._dismissHandler);
    overlay._dismissHandler = null;
  }
  overlay.classList.add('hidden');
  confPromptDone?.();
  confPromptDone = null;
  if (save) localStorage.setItem(PREF_KEY, pref);

  let cameraOn = false;
  let micOn = false;
  if (pref === 'both') {
    cameraOn = true;
    micOn = true;
  } else if (pref === 'cam') {
    cameraOn = true;
  } else if (pref === 'mic') {
    micOn = true;
  }
  viewerStore.set({ cameraOn, micOn });

  refreshConfButtons();
  setConfBtns({ disabled: true }, { disabled: true });
  try {
    await initLiveKit();
  } catch (err) {
    console.error('[conf prompt]', err);
    viewerStore.set({ cameraOn: false, micOn: false });
  }
  refreshConfButtons();
}

export function showConfPrompt(): Promise<void> {
  const saved = localStorage.getItem(PREF_KEY);
  if (saved) {
    void applyConfPref(saved as 'both' | 'cam' | 'mic' | 'none', false);
    return Promise.resolve();
  }
  document.getElementById('prompt-mic')?.classList.add('pref-saved');
  const overlay = document.getElementById('conf-prompt-overlay') as HTMLElement & {
    _dismissHandler?: ((e: MouseEvent) => void) | null;
  };
  overlay.classList.remove('hidden');
  if (overlay._dismissHandler) overlay.removeEventListener('click', overlay._dismissHandler);
  const dismiss = (e: MouseEvent): void => {
    if (e.target !== overlay) return;
    overlay.removeEventListener('click', dismiss);
    overlay._dismissHandler = null;
    void applyConfPref('mic');
  };
  overlay._dismissHandler = dismiss;
  overlay.addEventListener('click', dismiss);
  return new Promise((resolve) => {
    confPromptDone = resolve;
  });
}

// ---- Device picker ----

const pickerOpen = (): boolean =>
  !document.getElementById('device-picker-overlay')?.classList.contains('hidden');

function localMediaTrack(source: string): MediaStreamTrack | null {
  return livekitRoom?.localParticipant.getTrackPublication(source)?.track?.mediaStreamTrack ?? null;
}

// The device a dropdown should show: whatever the live track is actually
// capturing from. A muted track may be holding a switch until it unmutes
// (LiveKit defers it), so for those — and before any capture — the saved
// choice is the truth.
function selectedInput(kind: InputKind): string | undefined {
  const source =
    kind === 'audioinput' ? LivekitClient.Track.Source.Microphone : LivekitClient.Track.Source.Camera;
  const pub = livekitRoom?.localParticipant.getTrackPublication(source);
  const live = pub && !pub.isMuted ? pub.track?.mediaStreamTrack.getSettings().deviceId : undefined;
  return live ?? savedDevice(kind)?.id;
}

function populateDeviceSelect(
  selectId: string,
  devices: MediaDeviceInfo[],
  selectedId: string | undefined,
  defaultLabel?: string,
): void {
  const sel = document.getElementById(selectId) as HTMLSelectElement;
  sel.innerHTML = '';
  // Without permission the browser lists placeholders with an empty id; they
  // can't be selected, so they only tell us something exists.
  const real = devices.filter((d) => d.deviceId !== '');
  const entries = real.map((d, i): [string, string] => [d.deviceId, d.label || `Device ${i + 1}`]);
  // Safari lists no "default" output, so the speaker list gets one; an empty
  // sink id routes to the system output.
  if (defaultLabel && !real.some((d) => d.deviceId === 'default')) entries.unshift(['', defaultLabel]);
  if (!entries.length) {
    const opt = document.createElement('option');
    opt.textContent = devices.length ? 'Allow access to list devices' : 'No devices found';
    sel.appendChild(opt);
    sel.disabled = true;
    return;
  }
  sel.disabled = false;
  for (const [id, label] of entries) {
    const opt = document.createElement('option');
    opt.value = id;
    opt.textContent = label;
    sel.appendChild(opt);
  }
  if (selectedId !== undefined && entries.some(([id]) => id === selectedId)) sel.value = selectedId;
}

// `askPermission` only on an explicit open: a devicechange refresh must never
// raise a browser prompt (Safari re-prompts on every request until allowed).
async function refreshDevicePicker(askPermission = false): Promise<void> {
  if (!pickerOpen()) return;
  const mic = localMediaTrack(LivekitClient.Track.Source.Microphone);
  const cam = localMediaTrack(LivekitClient.Track.Source.Camera);
  const { devices, errors } = await listDevices({
    audio: !askPermission || mic?.readyState === 'live',
    video: !askPermission || cam?.readyState === 'live',
  });
  for (const { kind, err } of errors) {
    console.warn('[device picker] permission request failed', err);
    toast(deviceErrorMessage(err, kind === 'audio' ? 'mic' : 'cam'));
  }
  populateDeviceSelect(
    'device-camera',
    devices.filter((d) => d.kind === 'videoinput'),
    selectedInput('videoinput'),
  );
  populateDeviceSelect(
    'device-mic',
    devices.filter((d) => d.kind === 'audioinput'),
    selectedInput('audioinput'),
  );
  populateDeviceSelect(
    'device-speaker',
    devices.filter((d) => d.kind === 'audiooutput'),
    savedDevice('audiooutput')?.id ?? livekitRoom?.getActiveDevice('audiooutput') ?? '',
    'System default',
  );
}

function openDevicePicker(): void {
  document.getElementById('device-picker-overlay')?.classList.remove('hidden');
  (document.getElementById('device-noise') as HTMLInputElement).checked = noiseReductionOn();
  (document.getElementById('device-echo') as HTMLInputElement).checked = echoCancelOn();
  (document.getElementById('device-ptt') as HTMLInputElement).checked = pttOn();
  // No setSinkId at all → there is nothing to choose; say nothing rather than
  // offer a dropdown that can't work.
  const speakerRow = document.getElementById('device-speaker-row');
  if (speakerRow) speakerRow.style.display = canSelectSpeaker() ? '' : 'none';
  // Started synchronously, inside the click, so Safari lets its AudioContext run.
  startMicMeter(
    () => localMediaTrack(LivekitClient.Track.Source.Microphone),
    document.getElementById('device-mic-level') as HTMLElement,
    document.getElementById('device-mic-hint') as HTMLElement,
  );
  void refreshDevicePicker(true);
}

function closeDevicePicker(): void {
  document.getElementById('device-picker-overlay')?.classList.add('hidden');
  stopMicMeter();
}

async function switchInput(kind: InputKind, sel: HTMLSelectElement): Promise<void> {
  const id = sel.value;
  const label = sel.selectedOptions[0]?.textContent ?? '';
  if (!id) return;
  // Not in the call yet: the choice is applied at the next capture.
  if (!livekitRoom) {
    rememberDevice(kind, id, label);
    return;
  }
  const previous = selectedInput(kind);
  try {
    if (await livekitRoom.switchActiveDevice(kind, id)) {
      rememberDevice(kind, id, label);
    } else {
      toast(`Couldn't switch to ${label} — the browser kept a different ${deviceNoun(kind)}.`, 5000);
    }
  } catch (err) {
    console.error(`[device switch ${kind}]`, err);
    toast(deviceErrorMessage(err, kind === 'audioinput' ? 'mic' : 'cam'));
    // LiveKit stops the old track before capturing the new one, so a failed
    // switch leaves nothing capturing at all. Go back to what worked.
    if (previous && previous !== id) {
      await livekitRoom.switchActiveDevice(kind, previous).catch(() => {});
    }
  }
  updateSelfTile();
  void refreshDevicePicker();
}

async function switchSpeaker(sel: HTMLSelectElement): Promise<void> {
  const id = sel.value;
  const label = sel.selectedOptions[0]?.textContent ?? '';
  if (await applySpeaker(livekitRoom, id)) {
    rememberDevice('audiooutput', id, label);
  } else {
    toast(`Couldn't switch the speaker to ${label} in this browser.`, 5000);
  }
}

// ---- Blocked audio playback ----
// Any click or key unblocks it: room.startAudio() has to run inside a user
// gesture, and making the participant find one specific button would leave
// them deaf until they did.
let audioUnlockArmed = false;

function armAudioUnlock(): void {
  if (audioUnlockArmed) return;
  audioUnlockArmed = true;
  logDiag('conference', 'warn', 'Browser blocked conference audio — waiting for a click');
  toast('Your browser blocked the room audio — click anywhere to hear the others.', 6000);
  const unlock = (): void => {
    document.removeEventListener('pointerdown', unlock, true);
    document.removeEventListener('keydown', unlock, true);
    audioUnlockArmed = false;
    livekitRoom?.startAudio().catch((err: unknown) => {
      console.warn('[startAudio]', err);
      armAudioUnlock();
    });
  };
  document.addEventListener('pointerdown', unlock, true);
  document.addEventListener('keydown', unlock, true);
}

// ---- Presenter moderation ----

async function presenterKick(targetId: string): Promise<void> {
  try {
    await fetch(`/api/public/rooms/${slug}/conference/kick`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        participantId: getParticipantId(),
        token: getToken(),
        targetId,
      }),
    });
  } catch (err) {
    console.error('[kick]', err);
  }
}

async function presenterMute(targetId: string): Promise<void> {
  const micPub = livekitRoom?.remoteParticipants
    .get(targetId)
    ?.getTrackPublication(LivekitClient.Track.Source.Microphone);
  if (!micPub) return;
  const nowMuted = !micPub.isMuted;
  try {
    await fetch(`/api/public/rooms/${slug}/conference/mute`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        participantId: getParticipantId(),
        token: getToken(),
        targetId,
        trackSid: micPub.trackSid,
        muted: nowMuted,
      }),
    });
  } catch (err) {
    console.error('[mute]', err);
  }
}

// ---- Wire DOM ----

export function initConference(): void {
  viewerStore.set({ pttEnabled: pttOn() });
  updateMicTooltip();

  document.getElementById('cam-btn')?.addEventListener('click', toggleCamera);
  // Mic button: a plain toggle normally, press-and-hold in push-to-talk mode.
  const micBtn = document.getElementById('mic-btn');
  micBtn?.addEventListener('click', () => {
    if (viewerStore.get().pttEnabled) return; // PTT drives the mic via pointer hold
    void toggleMic();
  });
  micBtn?.addEventListener('pointerdown', (e) => {
    if (!viewerStore.get().pttEnabled) return;
    e.preventDefault(); // suppress the synthesized click so it doesn't toggle
    void pttPress();
  });
  const pttPointerRelease = (): void => {
    if (viewerStore.get().pttEnabled) void pttRelease();
  };
  micBtn?.addEventListener('pointerup', pttPointerRelease);
  micBtn?.addEventListener('pointerleave', pttPointerRelease);
  micBtn?.addEventListener('pointercancel', pttPointerRelease);
  // Losing focus (alt-tab, OS prompt) can swallow the keyup/pointerup that would
  // re-mute — release defensively so the mic never sticks open.
  window.addEventListener('blur', () => void pttRelease());
  // Screen share can't work on iOS (every iOS browser is WebKit, and even those
  // that expose getDisplayMedia — e.g. Firefox iOS — reject it at call time).
  // Detect iOS directly rather than trusting feature detection, and also cover
  // browsers that simply lack the API. Hide the button instead of letting it fail.
  const ua = navigator.userAgent;
  const isIOS =
    /iPad|iPhone|iPod/.test(ua) ||
    // iPadOS 13+ reports as "Macintosh"; disambiguate via touch support.
    (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
  const screenBtn = document.getElementById('screen-btn');
  if (screenBtn && (isIOS || !navigator.mediaDevices?.getDisplayMedia)) {
    // Inline style beats the `.tb-btn { display: flex }` rule in the page's
    // inline <style> (`.u-hidden` would lose that cascade battle).
    screenBtn.style.display = 'none';
  } else {
    screenBtn?.addEventListener('click', toggleScreenShare);
  }

  document.getElementById('focus-btn')?.addEventListener('click', () => {
    const { focusedTile, role } = viewerStore.get();
    const isPresenter = role === 'presenter';
    if (focusedTile !== null) {
      // Unpin. Host broadcasts; viewer's is local-only.
      if (isPresenter) wsSend({ type: 'focus:set', tileId: null });
      setFocus(null, { override: false });
    } else {
      // Pin whatever the auto-pin target would be (share > stream).
      requestAutoFocus();
      const newFocus = viewerStore.get().focusedTile;
      if (isPresenter && newFocus !== null) {
        wsSend({ type: 'focus:set', tileId: newFocus });
      }
    }
  });

  // Pin button (top-right of #tile-stream / #tile-share) → toggle focus on
  // that tile. Host pins broadcast via focus:set; viewer pins are local
  // override. The strip lives inside #stage so this delegator covers both
  // the grid and the strip.
  document.getElementById('stage')?.addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('[data-action="toggle-focus"]');
    if (!btn) return;
    const tile = btn.closest<HTMLElement>('.tile');
    if (!tile) return;
    const tileId = getTileIdFromEl(tile);
    if (!tileId) return;
    const current = viewerStore.get().focusedTile;
    const isPresenter = viewerStore.get().role === 'presenter';
    if (current === tileId) {
      if (isPresenter) wsSend({ type: 'focus:set', tileId: null });
      setFocus(null, { override: false });
    } else if (isPresenter) {
      wsSend({ type: 'focus:set', tileId });
      setFocus(tileId, { override: false });
    } else {
      setFocus(tileId, { override: true });
    }
  });

  document.getElementById('prompt-both')?.addEventListener('click', () => void applyConfPref('both'));
  document.getElementById('prompt-mic')?.addEventListener('click', () => void applyConfPref('mic'));
  document.getElementById('prompt-skip')?.addEventListener('click', () => void applyConfPref('none'));

  document.getElementById('device-btn')?.addEventListener('click', openDevicePicker);
  document.getElementById('device-picker-close')?.addEventListener('click', closeDevicePicker);
  document.getElementById('device-picker-overlay')?.addEventListener('click', (e) => {
    if (e.target === e.currentTarget) closeDevicePicker();
  });
  document.getElementById('device-camera')?.addEventListener('change', (e) => {
    void switchInput('videoinput', e.target as HTMLSelectElement);
  });
  document.getElementById('device-mic')?.addEventListener('change', (e) => {
    void switchInput('audioinput', e.target as HTMLSelectElement);
  });
  document.getElementById('device-speaker')?.addEventListener('change', (e) => {
    void switchSpeaker(e.target as HTMLSelectElement);
  });
  document.getElementById('device-speaker-test')?.addEventListener('click', () => {
    const sel = document.getElementById('device-speaker') as HTMLSelectElement;
    playTestTone(sel.value).catch((err: unknown) => {
      console.error('[speaker test]', err);
      toast("Couldn't play a test sound on that speaker.");
    });
  });
  // Plugging or unplugging a device while the picker is open.
  navigator.mediaDevices?.addEventListener('devicechange', () => void refreshDevicePicker());

  // Audio-processing toggles. Constraints only change by re-capturing:
  // setMicrophoneEnabled(true, opts) on an existing track just unmutes it and
  // ignores opts, which is why these used to need a page refresh. A restart
  // keeps the current device and the muted state. With no mic track yet there
  // is nothing to do — the next capture reads audioCaptureOpts().
  const onAudioPrefChange = (key: string) => async (e: Event): Promise<void> => {
    localStorage.setItem(key, (e.target as HTMLInputElement).checked ? '1' : '0');
    const track = livekitRoom?.localParticipant.getTrackPublication(LivekitClient.Track.Source.Microphone)
      ?.track as LkLocalAudioTrack | null | undefined;
    if (!track) return;
    const deviceId = track.mediaStreamTrack.getSettings().deviceId;
    try {
      await track.restartTrack({ ...audioCaptureOpts(), ...(deviceId ? { deviceId } : {}) });
    } catch (err) {
      console.error('[audio pref]', err);
      toast(deviceErrorMessage(err, 'mic'));
    }
  };
  document.getElementById('device-noise')?.addEventListener('change', (e) => void onAudioPrefChange(noiseKey())(e));
  document.getElementById('device-echo')?.addEventListener('change', (e) => void onAudioPrefChange(echoKey())(e));

  document.getElementById('device-ptt')?.addEventListener('change', (e) => {
    const on = (e.target as HTMLInputElement).checked;
    localStorage.setItem(pttKey(), on ? '1' : '0');
    viewerStore.set({ pttEnabled: on });
    updateMicTooltip();
    // Enabling PTT while the mic is live: drop straight to muted.
    if (on && viewerStore.get().micOn) void setMicLive(false, false);
  });

  // Presenter moderation, delegated at #app level — tiles live in #stage
  // (focused) or #stage-strip (unfocused). One listener handles both.
  document.getElementById('app')?.addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('[data-action]');
    if (!btn || viewerStore.get().role !== 'presenter') return;
    const action = btn.dataset['action'];
    const identity = btn.dataset['identity'];
    if (!identity) return;
    if (action === 'presenter-kick') {
      e.stopPropagation();
      void presenterKick(identity);
    } else if (action === 'presenter-mute') {
      e.stopPropagation();
      void presenterMute(identity);
    }
  });
}
