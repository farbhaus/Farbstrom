// Pre-app screens: landing, join, waiting, kicked, ended, left.
// Owns room-info fetch, join form submission, and the SSE/poll fallback
// for waiting-room admission.

import {
  autoName,
  autoPassword,
  isPresenter,
  KICKED_KEY,
  loadSavedSession,
  NAME_KEY,
  PASS_KEY,
  PREF_KEY,
  presenterKey,
  saveSession,
  SESSION_KEY,
  setSession,
  slug,
} from './session.js';
import { viewerStore } from './state.js';
import { getBrandName } from '../shared/branding.js';
import type { JoinResponse, RoomInfo } from './types.js';

const API = '/api/public/rooms';

let onAdmitted: () => void = () => {};
let statusInterval: ReturnType<typeof setInterval> | null = null;

export function configureScreens(opts: { onAdmitted: () => void }): void {
  onAdmitted = opts.onAdmitted;
}

function el(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function showHidden(id: string): void {
  el(id)?.classList.remove('hidden');
}
function hideScreen(id: string): void {
  el(id)?.classList.add('hidden');
}

export function showLanding(): void {
  hideScreen('join-screen');
  showHidden('landing-screen');
}

export function showJoin(): void {
  showHidden('join-screen');
}

export function showKicked(): void {
  showHidden('kicked-screen');
}

export function showEnded(): void {
  el('app')?.classList.remove('visible');
  showHidden('session-ended-screen');
}

export function showLeft(roomName: string): void {
  el('app')?.classList.remove('visible');
  const nameEl = el('left-room-name');
  if (nameEl) nameEl.textContent = roomName;
  showHidden('left-screen');
}

function clearAdmissionPoll(): void {
  if (statusInterval) {
    clearInterval(statusInterval);
    statusInterval = null;
  }
}

export function pollAdmission(): void {
  const pid = loadSavedSession()?.participantId || '';
  const tok = loadSavedSession()?.token || '';
  if (!pid || !tok) return;
  const sse = new EventSource(
    `${API}/${slug}/waiting/events/${pid}?token=${encodeURIComponent(tok)}`,
  );
  sse.addEventListener('admitted', () => {
    sse.close();
    onAdmitted();
  });
  sse.addEventListener('error', () => {
    sse.close();
    clearAdmissionPoll();
    statusInterval = setInterval(async () => {
      const res = await fetch(`${API}/${slug}/status/${pid}?token=${encodeURIComponent(tok)}`);
      if (!res.ok) return;
      let data;
      try {
        data = await res.json();
      } catch {
        return;
      }
      if (data.admitted) {
        clearAdmissionPoll();
        onAdmitted();
      }
    }, 3000);
  });
}

export function stopAdmissionPoll(): void {
  clearAdmissionPoll();
}

// ---- Landing form ----

function landingGo(): void {
  const input = el('landing-input') as HTMLInputElement;
  const errEl = el('landing-error');
  const raw = input.value.trim();
  if (errEl) errEl.textContent = '';
  if (!raw) {
    if (errEl) errEl.textContent = 'Please enter a room URL.';
    return;
  }
  let dest: string | undefined;
  try {
    const url = raw.startsWith('http')
      ? new URL(raw)
      : new URL('https://x/' + raw.replace(/^\/+/, ''));
    const parts = url.pathname.replace(/^\//, '').split('/').filter(Boolean);
    dest = parts[parts.length - 1];
  } catch {
    dest = raw.replace(/^\/+|\/+$/g, '').split('/').pop();
  }
  if (!dest) {
    if (errEl) errEl.textContent = 'Could not parse a room from that URL.';
    return;
  }
  location.href = '/watch/' + dest + location.search;
}

export function initLandingForm(): void {
  el('landing-btn')?.addEventListener('click', landingGo);
  el('landing-input')?.addEventListener('keydown', (e) => {
    if ((e as KeyboardEvent).key === 'Enter') landingGo();
  });
}

// ---- Room info / session resume ----

export interface RoomInfoOutcome {
  kind: 'show-app' | 'show-waiting' | 'show-kicked' | 'show-join' | 'show-landing';
  roomInfo?: RoomInfo;
  initialStatus?: RoomInfo['status'];
  waitingName?: string;
}

export async function loadRoomInfo(): Promise<RoomInfoOutcome> {
  try {
    const res = await fetch(`${API}/${slug}/info`);
    if (res.status === 429) {
      // Rate limited — retry once after a small delay; let the caller decide
      // what to do with the second attempt's outcome.
      await new Promise((r) => setTimeout(r, 2000));
      return loadRoomInfo();
    }
    if (!res.ok) {
      const errEl = el('landing-error');
      if (errEl) errEl.textContent = 'Room not found. Please check your link.';
      return { kind: 'show-landing' };
    }
    const roomInfo: RoomInfo = await res.json();
    document.title = roomInfo.name + ' — ' + getBrandName();
    const nameEl = el('join-room-name');
    if (nameEl) nameEl.textContent = roomInfo.name;
    if (!roomInfo.has_stream_key && nameEl) {
      const badge = document.createElement('span');
      badge.className = 'call-room-badge';
      badge.textContent = 'Call room';
      nameEl.appendChild(badge);
    }
    // Host links carry their own credential (presenter_key) and bypass the
    // password gate server-side, so don't ask the colorist for it.
    if (roomInfo.has_password && !isPresenter) {
      const row = el('password-row');
      if (row) row.style.display = '';
    }
    viewerStore.set({ roomInfo });

    const savedName = localStorage.getItem(NAME_KEY) || '';
    const savedPass = localStorage.getItem(PASS_KEY) || '';
    if (savedName) (el('name-input') as HTMLInputElement).value = savedName;
    if (savedPass) (el('password-input') as HTMLInputElement).value = savedPass;

    // If this tab was kicked, stay on kicked screen but poll so we
    // automatically rejoin when the admin clears the kick.
    if (sessionStorage.getItem(KICKED_KEY)) {
      return { kind: 'show-kicked', roomInfo };
    }

    const savedSession = loadSavedSession();
    // If the room's broadcast mode flipped since we joined (admin attached
    // or cleared a stream key), the saved session is stale: its streamKey
    // would lock the page into the old mode. Drop it, then fall through
    // to the auto-rejoin path so the user is put back into the room
    // without having to re-enter their name.
    const savedHasKey = !!(savedSession && savedSession.streamKey);
    const serverHasKey = !!roomInfo.has_stream_key;
    const modeFlipped = savedSession && savedHasKey !== serverHasKey;
    if (modeFlipped) sessionStorage.removeItem(SESSION_KEY);

    if (savedSession && !modeFlipped && roomInfo.status !== 'ended') {
      setSession(savedSession.participantId, savedSession.token);
      const role = isPresenter ? 'presenter' : savedSession.role || 'viewer';
      viewerStore.set({
        role,
        deliveryMode: roomInfo.delivery_mode || savedSession.deliveryMode || 'webrtc',
        streamKey: savedSession.streamKey,
        noiseDefault: !!roomInfo.noise_reduction,
        echoDefault: !!roomInfo.echo_cancellation,
        pttDefault: !!roomInfo.push_to_talk,
      });

      // Re-check admission on resume. A participant who refreshed while in
      // the waiting room must land back on waiting-screen, not sneak into
      // the app shell. Presenters skip the check — the role guarantees
      // admission server-side.
      if (role === 'presenter') {
        return { kind: 'show-app', roomInfo, initialStatus: roomInfo.status };
      }
      try {
        const sres = await fetch(
          `${API}/${slug}/status/${savedSession.participantId}?token=${encodeURIComponent(savedSession.token)}`,
        );
        if (sres.status === 404 || sres.status === 401) {
          sessionStorage.removeItem(SESSION_KEY);
          return { kind: 'show-join', roomInfo };
        }
        const sdata = await sres.json();
        if (sdata.kicked) {
          sessionStorage.setItem(KICKED_KEY, '1');
          return { kind: 'show-kicked', roomInfo };
        }
        if (sdata.admitted) {
          return { kind: 'show-app', roomInfo, initialStatus: roomInfo.status };
        }
        return {
          kind: 'show-waiting',
          roomInfo,
          waitingName: savedName,
        };
      } catch {
        return { kind: 'show-join', roomInfo };
      }
    } else if (savedSession && !modeFlipped && roomInfo.status === 'ended') {
      sessionStorage.removeItem(SESSION_KEY);
      return { kind: 'show-join', roomInfo };
    } else if (autoName || (modeFlipped && savedName)) {
      // ?n= from admin one-shot links OR a known returning user whose
      // session we just invalidated — skip the join form. URL params
      // (role, pk) survive location.reload(), so host privilege is
      // preserved automatically.
      (el('name-input') as HTMLInputElement).value = autoName || savedName;
      const pw = autoPassword || savedPass;
      if (pw) (el('password-input') as HTMLInputElement).value = pw;
      const joined = await doJoin();
      return joined;
    }
    return { kind: 'show-join', roomInfo };
  } catch {
    const nameEl = el('join-room-name');
    if (nameEl) nameEl.textContent = 'Connection error';
    return { kind: 'show-join' };
  }
}

// ---- Join ----

export async function doJoin(): Promise<RoomInfoOutcome> {
  const errEl = el('join-error');
  if (errEl) errEl.textContent = '';
  const name = (el('name-input') as HTMLInputElement).value.trim();
  const password = (el('password-input') as HTMLInputElement).value;
  if (!name) {
    if (errEl) errEl.textContent = 'Please enter your name';
    return { kind: 'show-join' };
  }

  const joinBtn = el('join-btn') as HTMLButtonElement;
  joinBtn.textContent = 'Joining...';
  joinBtn.disabled = true;

  try {
    const res = await fetch(`${API}/${slug}/join`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        name,
        password,
        role: isPresenter ? 'presenter' : 'viewer',
        presenter_key: presenterKey,
      }),
    });
    const data: JoinResponse = await res.json();

    if (!res.ok) {
      if (errEl) errEl.textContent = data.error || 'Could not join';
      joinBtn.textContent = 'Join Session';
      joinBtn.disabled = false;
      return { kind: 'show-join' };
    }

    setSession(data.participant_id, data.token);
    viewerStore.set({
      role: data.role || 'viewer',
      deliveryMode: data.delivery_mode,
      streamKey: data.stream_key,
      noiseDefault: data.noise_reduction_default,
      echoDefault: data.echo_cancellation_default,
      pttDefault: data.push_to_talk_default,
    });

    saveSession({
      participantId: data.participant_id,
      token: data.token,
      deliveryMode: data.delivery_mode,
      streamKey: data.stream_key,
      role: data.role || 'viewer',
    });
    localStorage.setItem(NAME_KEY, name);
    if (password) localStorage.setItem(PASS_KEY, password);
    else localStorage.removeItem(PASS_KEY);

    if (data.waiting_room && !data.admitted) {
      hideScreen('join-screen');
      showHidden('waiting-screen');
      const wn = el('waiting-name');
      if (wn) wn.textContent = name;
      return { kind: 'show-waiting', waitingName: name };
    }
    return { kind: 'show-app', initialStatus: data.status };
  } catch {
    if (errEl) errEl.textContent = 'Connection error';
    joinBtn.textContent = 'Join Session';
    joinBtn.disabled = false;
    return { kind: 'show-join' };
  }
}

export function initJoinForm(): void {
  el('join-form')?.addEventListener('submit', (e) => {
    e.preventDefault();
    void doJoin().then(handleJoinOutcome);
  });
}

let onJoinOutcome: (o: RoomInfoOutcome) => void = () => {};
export function configureJoinOutcome(fn: (o: RoomInfoOutcome) => void): void {
  onJoinOutcome = fn;
}
function handleJoinOutcome(o: RoomInfoOutcome): void {
  onJoinOutcome(o);
}

export function showWaitingScreen(name: string): void {
  hideScreen('join-screen');
  showHidden('waiting-screen');
  const wn = el('waiting-name');
  if (wn) wn.textContent = name;
}

// localStorage cleanup on session-end.
export function cleanupSessionStorage(): void {
  localStorage.removeItem(PREF_KEY);
}
