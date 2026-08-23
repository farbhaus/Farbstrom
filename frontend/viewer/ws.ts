// WebSocket connection + typed message router. Owns reconnect backoff and
// the kicked-poller. Other modules call `wsSend()` to push client messages.

import { addFileToSection, appendChatHistory, appendChatMessage, appendFileMessage, loadSessionFiles, removeFileEverywhere, setChatEnabled } from './chat.js';
import { disconnectLiveKit, requestAutoFocus, requestSelfUnmute, setFocus, syncConferenceTiles } from './conference.js';
import { applyDisplayState, destroyPlayer, remountLivePlayer } from './player.js';
import { applyModerationUpdate } from './roster.js';
import { clearAllPointers, hidePointer, pruneCursorsToRoster, renderPointer } from './pointer.js';
import {
  clearKicked,
  clearSession,
  getParticipantId,
  getToken,
  KICKED_KEY,
  markKicked,
  PREF_KEY,
  SESSION_KEY,
  slug,
  updateSavedStreamKey,
} from './session.js';
import { viewerStore } from './state.js';
import type { DeliveryMode, RosterEntry, WsClientMessage, WsMessage } from './types.js';

let ws: WebSocket | null = null;
let wsReconnect = true;
let kickedPollTimer: ReturnType<typeof setInterval> | null = null;

let onAuthOk: () => void = () => {};
let onRoomLive: () => void = () => {};
let onRoomPending: () => void = () => {};
let onRoomEnded: () => void = () => {};
let onStreamAssigned: (key: string) => void = () => {};
let onStreamRemoved: () => void = () => {};
let onDeliveryModeChanged: (mode: DeliveryMode) => void = () => {};
let onKicked: () => void = () => {};

export interface WsHandlers {
  onAuthOk: () => void;
  onRoomLive: () => void;
  onRoomPending: () => void;
  onRoomEnded: () => void;
  onStreamAssigned: (key: string) => void;
  onStreamRemoved: () => void;
  onDeliveryModeChanged: (mode: DeliveryMode) => void;
  onKicked: () => void;
}

export function configureWs(h: WsHandlers): void {
  onAuthOk = h.onAuthOk;
  onRoomLive = h.onRoomLive;
  onRoomPending = h.onRoomPending;
  onRoomEnded = h.onRoomEnded;
  onStreamAssigned = h.onStreamAssigned;
  onStreamRemoved = h.onStreamRemoved;
  onDeliveryModeChanged = h.onDeliveryModeChanged;
  onKicked = h.onKicked;
}

export function wsSend(msg: WsClientMessage): void {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  ws.send(JSON.stringify(msg));
}

function setWsStatus(state: string, label: string): void {
  const dot = document.getElementById('ws-dot');
  const lbl = document.getElementById('ws-label');
  if (dot) dot.className = state;
  if (lbl) lbl.textContent = label;
}

function updateRoster(participants: RosterEntry[]): void {
  const list = Array.isArray(participants) ? participants : [];
  // The participant-count badge is set in renderRoster (it also counts Farbplay
  // viewers); setting viewerStore.roster triggers renderRoster via subscription.
  viewerStore.set({ roster: list });
  pruneCursorsToRoster(new Set(list.map((p) => p.id)));
  syncConferenceTiles();
}

function handleMessage(msg: WsMessage): void {
  switch (msg.type) {
    case 'auth:ok':
      setWsStatus('connected', 'Connected');
      setChatEnabled(true);
      void loadSessionFiles();
      onAuthOk();
      return;
    case 'kicked':
      clearAllPointers();
      void disconnectLiveKit();
      destroyPlayer();
      // Keep SESSION_KEY so we can poll status with the existing token and
      // auto-rejoin once the admin unkicks.
      markKicked();
      wsReconnect = false;
      onKicked();
      startKickedPoller();
      return;
    case 'moderation:update':
      applyModerationUpdate({
        waiting: msg.waiting,
        kicked: msg.kicked,
        admitted: msg.admitted,
        newWaiting: msg.newWaiting,
      });
      return;
    case 'host:revoked':
      // The admin rotated the room's host link. Our session token has been
      // invalidated server-side; drop the saved session and reload. The URL
      // still carries the (now stale) presenter_key, which will downgrade
      // us to viewer on rejoin.
      clearAllPointers();
      void disconnectLiveKit();
      destroyPlayer();
      clearSession();
      wsReconnect = false;
      location.reload();
      return;
    case 'room:live':
      onRoomLive();
      // Remount rather than reload: the stream may be a different one than
      // the player is holding, and only a remount re-runs the codec probe.
      remountLivePlayer();
      return;
    case 'room:pending':
      onRoomPending();
      return;
    case 'room:ended':
      clearAllPointers();
      void disconnectLiveKit();
      destroyPlayer();
      clearSession();
      localStorage.removeItem(PREF_KEY);
      wsReconnect = false;
      onRoomEnded();
      return;
    case 'stream:assigned':
      onStreamAssigned(msg.streamKey);
      return;
    case 'delivery:changed':
      onDeliveryModeChanged(msg.deliveryMode);
      return;
    case 'stream:removed':
      onStreamRemoved();
      return;
    case 'participants:update':
      updateRoster(msg.participants);
      return;
    case 'chat:history':
      appendChatHistory(msg.messages as never);
      return;
    case 'chat:message':
      appendChatMessage(msg);
      return;
    case 'file:shared':
      appendFileMessage(msg);
      addFileToSection({
        id: msg.id,
        name: msg.name,
        size: msg.size,
        ...(msg.mime ? { mime: msg.mime } : {}),
        uploaderName: msg.uploaderName,
        role: msg.role,
      });
      return;
    case 'file:removed':
      removeFileEverywhere(msg.id);
      return;
    case 'conference:unmute':
      void requestSelfUnmute();
      return;
    case 'pointer:move':
      if (msg.participantId !== getParticipantId()) {
        renderPointer(msg.participantId, msg.name, msg.x, msg.y);
      }
      return;
    case 'pointer:hide':
      hidePointer(msg.participantId);
      return;
    case 'focus:set':
      // Host has driven a pin (or unpin). Apply with override=false so
      // viewers can still click locally to override.
      setFocus(msg.tileId, { override: false });
      return;
    case 'display:state':
      if (msg.fileId) {
        applyDisplayState({
          fileId: msg.fileId,
          name: msg.name ?? '',
          mime: msg.mime ?? '',
          size: msg.size ?? 0,
          playing: msg.playing ?? false,
          position: msg.position ?? 0,
          updatedAtMs: msg.updatedAtMs ?? Date.now(),
        });
        // The stream tile now hosts a file. Pin it (auto-focus resolves to
        // 'stream' unless a screen share is active or the viewer overrode).
        requestAutoFocus();
      } else {
        applyDisplayState(null);
        // File gone. If the stream tile was pinned only because of the file
        // (no live stream key), that target is now hidden — clear any manual
        // pin so auto-focus can fall back to grid. Mirrors handleStreamRemoved.
        const { streamKey, focusedTile } = viewerStore.get();
        if (!streamKey && focusedTile === 'stream') {
          viewerStore.set({ focusOverride: false });
        }
        requestAutoFocus();
      }
      return;
  }
}

export function connectWs(): void {
  setWsStatus('', 'Connecting');
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${proto}://${location.host}/ws/room/${slug}`);
  ws.onopen = () => {
    wsSend({ type: 'auth', participantId: getParticipantId(), token: getToken() });
  };
  ws.onmessage = (e) => {
    let msg: WsMessage;
    try {
      msg = JSON.parse(e.data);
    } catch {
      return;
    }
    handleMessage(msg);
  };
  ws.onclose = (e) => {
    clearAllPointers();
    if (e.code === 1001) {
      // Room ended/deleted — stop reconnecting.
      void disconnectLiveKit();
      destroyPlayer();
      clearSession();
      localStorage.removeItem(PREF_KEY);
      wsReconnect = false;
      onRoomEnded();
      return;
    }
    if (e.code === 1008) {
      // Policy violation = kicked or stale auth.
      if (!document.getElementById('kicked-screen')?.classList.contains('hidden')) {
        // Kicked screen already showing — keep the sentinel and let the
        // poller handle it.
        markKicked();
        return;
      }
      if (sessionStorage.getItem(KICKED_KEY)) {
        // Hub sent kicked flag (reconnect after kick) — show the screen and
        // start polling for unkick.
        onKicked();
        startKickedPoller();
        return;
      }
      // Auth rejected — session is stale; return to join form.
      sessionStorage.removeItem(SESSION_KEY);
      document.getElementById('app')?.classList.remove('visible');
      document.getElementById('join-screen')?.classList.remove('hidden');
      const errEl = document.getElementById('join-error');
      if (errEl) errEl.textContent = 'Session expired. Please re-enter your name.';
      return;
    }
    setWsStatus('error', 'Reconnecting');
    setChatEnabled(false);
    if (wsReconnect) setTimeout(connectWs, 3000);
  };
  ws.onerror = () => {
    setWsStatus('error', 'Error');
  };
}

export function closeWs(): void {
  wsReconnect = false;
  if (ws) {
    try {
      ws.close();
    } catch {}
    ws = null;
  }
}

// Poll the server while the kicked screen is showing. As soon as the admin
// clears is_kicked, reload so the saved-session resume path puts the user
// back into the room from the same tab.
export function startKickedPoller(): void {
  if (kickedPollTimer) return;
  const tick = async (): Promise<void> => {
    const sess = JSON.parse(sessionStorage.getItem(SESSION_KEY) || 'null');
    if (!sess || !sess.participantId || !sess.token) {
      stopKickedPoller();
      return;
    }
    try {
      const res = await fetch(
        `/api/public/rooms/${slug}/status/${sess.participantId}?token=${encodeURIComponent(sess.token)}`,
      );
      if (res.status === 404 || res.status === 401) {
        stopKickedPoller();
        clearSession();
        clearKicked();
        location.reload();
        return;
      }
      if (!res.ok) return;
      const data = await res.json();
      if (!data.kicked || data.room_status === 'ended') {
        stopKickedPoller();
        clearKicked();
        location.reload();
      }
    } catch {
      // Network blip — try again on the next tick.
    }
  };
  kickedPollTimer = setInterval(tick, 5000);
  void tick();
}

export function stopKickedPoller(): void {
  if (kickedPollTimer) {
    clearInterval(kickedPollTimer);
    kickedPollTimer = null;
  }
}
