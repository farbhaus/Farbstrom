// Viewer bootstrap. Stitches the modules together and runs init order.

import { applyBranding } from '../shared/branding.js';
import { configureChat, initChat, refreshShowButtons } from './chat.js';
import { initRoster } from './roster.js';
import {
  configureConference,
  initConference,
  maybeShowPttNotice,
  refreshConfButtons,
  requestAutoFocus,
  showConfPrompt,
  syncConferenceTiles,
  syncPttMode,
  disconnectLiveKit,
} from './conference.js';
import { initLayout, sizeStage, wakeChrome } from './layout.js';
import {
  destroyPlayer,
  getPlayer,
  getPlayerMode,
  initPlayer,
  initPlayerControls,
  remountLivePlayer,
  configurePlayer,
} from './player.js';
import { configurePointer, initPointer } from './pointer.js';
import { closeScopes, initScopes } from './scopes.js';
import { initShortcuts } from './shortcuts.js';
import { initStats } from './stats.js';
import { maybeStartTour, stopTour } from './tour.js';
import {
  configureJoinOutcome,
  configureScreens,
  doJoin,
  initJoinForm,
  initLandingForm,
  loadRoomInfo,
  pollAdmission,
  showEnded,
  showJoin,
  showKicked,
  showLanding,
  showLeft,
  showScheduledScreen,
  showWaitingScreen,
  stopAdmissionPoll,
  type RoomInfoOutcome,
} from './screens.js';
import { consumePresession, isPresenter, slug, updateSavedStreamKey } from './session.js';
import { getState, setState, viewerStore } from './state.js';
import type { DeliveryMode, RoomStatus } from './types.js';
import {
  closeWs,
  configureWs,
  connectWs,
  startKickedPoller,
  stopKickedPoller,
  wsSend,
} from './ws.js';

// ---- Room status side effects ----

function setRoomStatus(status: RoomStatus, playerPlaying?: boolean): void {
  if (getState().status !== status) setState({ status });
  refreshStatusOverlay(playerPlaying);
}

// Sync the offline overlay + live badge to the current room status,
// player mode, and live-player state. Pure DOM read/write — never
// touches viewerStore, so it's safe to call from the store subscriber
// without recursing.
// Set by player.ts when this browser can't decode the stream's codec. Outranks
// every status below *while the room is live*: the stream really is running, so
// the normal overlay copy ("Waiting for livestream source...") would be a lie,
// and the live badge would sit on top of a black tile. Once the stream stops
// the codec is no longer why there's nothing to watch, so the normal copy takes
// over again — and the probe re-runs on the next mount.
let blockedMsg: string | null = null;

function refreshStatusOverlay(playerPlaying?: boolean): void {
  const status = getState().status;
  const offline = document.getElementById('offline-screen');
  const badge = document.getElementById('live-badge');
  const msg = document.getElementById('offline-msg');
  if (!offline || !badge || !msg) return;

  if (blockedMsg && status === 'live') {
    msg.textContent = blockedMsg;
    offline.classList.add('visible');
    badge.classList.remove('visible');
    return;
  }

  // The unified stage is showing a file (image or video) — that's valid
  // content in its own right, no offline overlay or live badge.
  const showingFile = getPlayerMode() === 'file' || getPlayerMode() === 'image';
  if (showingFile) {
    offline.classList.remove('visible');
    badge.classList.remove('visible');
    return;
  }

  // App-only delivery: the broadcast plays in the native Farbplay app (SRT /
  // H.265), never in the browser. The room is call-only here — no stream tile
  // and no offline/live overlay; the layout is the participant grid. (A
  // presenter-displayed file, handled above, still takes over the stage.)
  if (getState().deliveryMode === 'srt') {
    offline.classList.remove('visible');
    badge.classList.remove('visible');
    return;
  }

  // If the caller didn't tell us, read the live player's state directly.
  const pState =
    playerPlaying ??
    (getPlayerMode() === 'live' && getPlayer()?.getState() === 'playing');
  if (status === 'live' && pState) {
    // Actually playing → live badge on, overlay off.
    offline.classList.remove('visible');
    badge.classList.add('visible');
  } else if (status === 'live') {
    // Live but not playing yet. 'live' means OME *admitted* the ingest, which
    // it does within ~0.2s, but the first segments aren't packaged for another
    // ~6s. Clearing the overlay here (as this used to) shows a black tile for
    // that whole gap on every stream start. Hold the waiting copy until the
    // player reports `playing` — the same signal the live badge already
    // depends on, so this doesn't add a new way to get stuck.
    msg.textContent = 'Waiting for livestream source...';
    offline.classList.add('visible');
    badge.classList.remove('visible');
  } else if (status === 'ended') {
    msg.textContent = 'Session has ended.';
    offline.classList.add('visible');
    badge.classList.remove('visible');
  } else {
    msg.textContent = 'Waiting for livestream source...';
    offline.classList.add('visible');
    badge.classList.remove('visible');
  }
}

// ---- Stream-key transitions ----

// Toggle the stream tile's presence and (re)mount OvenPlayer accordingly.
// Auto-focus is delegated to conference.ts which knows about the share state.
function handleStreamAssigned(newKey: string): void {
  updateSavedStreamKey(newKey);
  setState({ streamKey: newKey });
  // App-only (SRT) rooms stay call-only in the browser — no stream tile.
  if (getState().deliveryMode === 'srt') {
    document.getElementById('tile-stream')?.classList.add('hidden');
    setRoomStatus(getState().status);
    syncConferenceTiles();
    requestAutoFocus();
    return;
  }
  document.getElementById('tile-stream')?.classList.remove('hidden');
  // Remount, not initPlayer: on a key *swap* a player is already mounted on the
  // old key, and initPlayer no-ops whenever one exists — which left the viewer
  // watching the previous stream and skipped the codec probe for the new one.
  remountLivePlayer();
  setRoomStatus(getState().status);
  syncConferenceTiles();
  requestAutoFocus('stream');
}

function handleStreamRemoved(): void {
  updateSavedStreamKey(null);
  setState({ streamKey: null });
  destroyPlayer();
  document.getElementById('tile-stream')?.classList.add('hidden');
  // If the viewer had pinned the stream tile, that target no longer exists.
  if (getState().focusedTile === 'stream') setState({ focusOverride: false });
  syncConferenceTiles();
  requestAutoFocus();
}

// Admin switched the room's delivery mode. Re-derive the stream tile's
// presence the same way showApp does — an SRT room is call-only in the
// browser, webrtc/llhls shows and auto-pins the broadcast — then re-layout.
function handleDeliveryModeChanged(mode: DeliveryMode): void {
  setState({ deliveryMode: mode });
  const hasBrowserBroadcast = !!getState().streamKey && mode !== 'srt';
  if (hasBrowserBroadcast) {
    document.getElementById('tile-stream')?.classList.remove('hidden');
    initPlayer();
  } else {
    destroyPlayer();
    document.getElementById('tile-stream')?.classList.add('hidden');
    // The stream tile is gone — drop a stale pin so auto-focus can fall back.
    if (getState().focusedTile === 'stream') setState({ focusOverride: false });
  }
  syncConferenceTiles();
  sizeStage();
  requestAutoFocus();
}

// ---- App show / leave ----

function showApp(initialStatus?: RoomStatus): void {
  stopAdmissionPoll();
  document.getElementById('join-screen')?.classList.add('hidden');
  document.getElementById('waiting-screen')?.classList.add('hidden');
  document.getElementById('scheduled-screen')?.classList.add('hidden');
  document.getElementById('app')?.classList.add('visible');
  // The idle clock has been running since initLayout, behind the join screen —
  // restart it so the room never opens on already-faded chrome (#248).
  wakeChrome();

  const roomInfo = getState().roomInfo;
  const label = document.getElementById('room-name-label');
  if (label) label.textContent = roomInfo?.name || slug;

  setRoomStatus(initialStatus || 'pending');

  // Set up the stream tile presence based on whether the broadcast plays in
  // this browser, mount OvenPlayer (or not), seed the participant tiles, then
  // let auto-focus pick the natural target (share > stream > grid). For
  // app-only (SRT) rooms there is no browser broadcast — it's a call-only room,
  // so the stream tile stays hidden and the layout defaults to the grid.
  if (getState().streamKey && getState().deliveryMode !== 'srt') {
    document.getElementById('tile-stream')?.classList.remove('hidden');
    initPlayer();
  } else {
    document.getElementById('tile-stream')?.classList.add('hidden');
  }
  syncConferenceTiles();
  sizeStage();
  requestAutoFocus();

  // Apply the room's push-to-talk default now that roomInfo/pttDefault are
  // seeded, so the conf prompt's initLiveKit keeps the mic muted under PTT.
  syncPttMode();
  connectWs();
  // Show the one-time PTT explainer (if applicable) before the cam/mic prompt
  // so the two dialogs don't stack; resolves immediately when PTT is off. The
  // first-run tour (#230) comes last, once the room is in its resting state:
  // showConfPrompt resolves as the prompt closes, so the tour spotlights a
  // toolbar the participant can actually see.
  void maybeShowPttNotice()
    .then(() => showConfPrompt())
    .finally(() => maybeStartTour());
}

function leaveRoom(): void {
  stopTour();
  stopAdmissionPoll();
  closeWs();
  void disconnectLiveKit();
  destroyPlayer();
  closeScopes();
  showLeft(getState().roomInfo?.name || slug);
}

function dispatchOutcome(o: RoomInfoOutcome): void {
  switch (o.kind) {
    case 'show-app':
      showApp(o.initialStatus);
      break;
    case 'show-waiting':
      showWaitingScreen(o.waitingName || '');
      pollAdmission();
      break;
    case 'show-scheduled':
      showScheduledScreen(o.startsAt ?? null, o.waitingName || '');
      pollAdmission();
      break;
    case 'show-kicked':
      showKicked();
      startKickedPoller();
      break;
    case 'show-join':
      showJoin();
      break;
    case 'show-landing':
      showLanding();
      break;
  }
}

// ---- Init ----

function init(): void {
  // Enforce canonical /watch/{slug} URL — redirect anything else.
  if (slug && !location.pathname.startsWith('/watch/')) {
    location.replace('/watch/' + slug + location.search);
    return;
  }

  // Apply branding (logo + colors + bg). Read-only — same as landing.
  // Logos and wordmark fallbacks both start hidden in the HTML so neither
  // flashes before /api/branding resolves; reveal exactly one here (the
  // wordmark also covers a failed fetch — applyBranding returns null).
  void applyBranding({
    bgTarget: document.documentElement,
  }).then((data) => {
    if (data?.hasLogo) {
      document.querySelectorAll<HTMLImageElement>('.screen-logo, .brand-logo').forEach((img) => {
        img.src = '/api/branding/logo';
        img.classList.remove('u-hidden');
      });
    } else {
      document.querySelectorAll<HTMLElement>('.brand-fallback').forEach((el) => {
        el.classList.remove('u-hidden');
      });
    }
    if (data?.hasBg) {
      document.body.style.background = 'transparent';
    }
  });

  // Wire all subsystems.
  configureChat({ send: wsSend });
  configurePointer({ send: wsSend });
  configureConference({ send: wsSend });
  configurePlayer({
    onPlayingChange: () => {
      // Only the live broadcast playing means the room is live. A presenter
      // file reaching 'playing' must NOT flip room status, or the offline
      // overlay won't return when the file is later cleared.
      if (getPlayerMode() === 'live') setRoomStatus('live', true);
    },
    onPlaybackBlocked: (message) => {
      blockedMsg = message;
      refreshStatusOverlay();
    },
    send: wsSend,
  });
  configureWs({
    onAuthOk: () => {},
    onRoomLive: () => setRoomStatus('live'),
    onRoomPending: () => setRoomStatus('pending'),
    onRoomEnded: () => {
      stopTour();
      showEnded();
    },
    onStreamAssigned: handleStreamAssigned,
    onStreamRemoved: handleStreamRemoved,
    onDeliveryModeChanged: handleDeliveryModeChanged,
    onKicked: () => {
      stopTour();
      document.getElementById('app')?.classList.remove('visible');
      showKicked();
    },
  });
  configureScreens({
    onAdmitted: () => showApp(),
  });
  configureJoinOutcome(dispatchOutcome);

  // DOM listeners.
  initLayout();
  initPlayerControls();
  initPointer();
  initScopes();
  initChat();
  initRoster();
  initStats();
  initConference();
  initLandingForm();
  initJoinForm();
  initShortcuts();

  document.getElementById('leave-btn')?.addEventListener('click', leaveRoom);
  document.getElementById('left-rejoin-btn')?.addEventListener('click', () => location.reload());

  // Subscribe: keep `participant-num` and tile sync responsive to roster changes.
  // (ws.ts already triggers syncConferenceTiles on roster updates; subscribing
  // here is a no-op safety net for any other state-driven re-renders.)
  viewerStore.subscribe((s) => {
    refreshConfButtons();
    refreshShowButtons();
    // Re-evaluate the offline / live badge — displayFile changes (file
    // start / stop) flip whether the overlay should be visible at all.
    // Use the pure DOM refresher, not setRoomStatus, so we don't write
    // back to the store and re-enter the subscriber.
    refreshStatusOverlay();
    void s;
  });

  // Boot.
  if (!slug) {
    showLanding();
    return;
  }

  consumePresession();
  void loadRoomInfo().then(dispatchOutcome);
}

void isPresenter;
void stopKickedPoller;

init();
