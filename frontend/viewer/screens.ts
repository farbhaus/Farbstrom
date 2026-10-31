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
  presenterKey,
  savedPassword,
  savePassword,
  saveSession,
  SESSION_KEY,
  setSession,
  slug,
  updateSavedPlaybackId,
} from './session.js';
import { viewerStore } from './state.js';
import { getBrandName } from '../shared/branding.js';
import { browserZone, isKnownZone, offsetLabel, zoneLabel } from '../shared/timezone.js';
import type { JoinResponse, RoomInfo, StatusResponse } from './types.js';

const API = '/api/public/rooms';

let onAdmitted: () => void = () => {};
let onDenied: () => void = () => {};
let onEnded: () => void = () => {};
let statusInterval: ReturnType<typeof setInterval> | null = null;
let countdownInterval: ReturnType<typeof setInterval> | null = null;
// Remembered so the scheduled -> waiting-room transition (once the room starts)
// can re-render the waiting screen with the viewer's name.
let waitingDisplayName = '';

export function configureScreens(opts: {
  onAdmitted: () => void;
  /** The host turned this joiner away (or kicked them) while they waited. */
  onDenied: () => void;
  /** The room ended or was deleted while they waited. */
  onEnded: () => void;
}): void {
  onAdmitted = opts.onAdmitted;
  onDenied = opts.onDenied;
  onEnded = opts.onEnded;
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

// A held viewer parked on the scheduled ("starting soon") screen moves to the
// waiting-room screen once the room actually starts. Only waiting-room rooms hit
// this — a no-waiting-room room admits at start and delivers `admitted` instead.
// (issue #200 follow-up: the scheduled screen otherwise stuck around until admit.)
function onRoomStarted(): void {
  if (el('scheduled-screen')?.classList.contains('hidden') !== false) return;
  stopScheduledCountdown();
  hideScreen('scheduled-screen');
  showWaitingScreen(waitingDisplayName);
}

async function fetchStatus(pid: string, tok: string): Promise<StatusResponse | null> {
  try {
    const res = await fetch(`${API}/${slug}/status/${pid}?token=${encodeURIComponent(tok)}`);
    return res.ok ? ((await res.json()) as StatusResponse) : null;
  } catch {
    return null;
  }
}

// The room's stream is read on admission, not kept from the join: a waiting
// joiner holds no socket, so a key attached, swapped or detached meanwhile
// never reached it, and one who refreshed while waiting was told none (the
// status endpoint names the stream only to someone who may play it).
function adoptStream(status: StatusResponse): void {
  viewerStore.set({ playbackId: status.playback_id });
  updateSavedPlaybackId(status.playback_id);
}

// The admission stream says `kicked` and `room_ended` as well as `admitted`;
// only the last was ever listened for, so someone the host turned away — or
// whose room ended — sat on "waiting for the host" indefinitely.
export function pollAdmission(): void {
  const pid = loadSavedSession()?.participantId || '';
  const tok = loadSavedSession()?.token || '';
  if (!pid || !tok) return;
  const sse = new EventSource(
    `${API}/${slug}/waiting/events/${pid}?token=${encodeURIComponent(tok)}`,
  );
  sse.addEventListener('admitted', async () => {
    sse.close();
    const status = await fetchStatus(pid, tok);
    if (status) adoptStream(status);
    onAdmitted();
  });
  sse.addEventListener('kicked', () => {
    sse.close();
    onDenied();
  });
  sse.addEventListener('room_ended', () => {
    sse.close();
    onEnded();
  });
  sse.addEventListener('ping', (e) => {
    let data: { started?: boolean; room_status?: string } = {};
    try {
      data = JSON.parse((e as MessageEvent).data || '{}');
    } catch {
      return;
    }
    if (data.started === true || (!!data.room_status && data.room_status !== 'scheduled')) {
      onRoomStarted();
    }
  });
  sse.addEventListener('error', () => {
    sse.close();
    clearAdmissionPoll();
    statusInterval = setInterval(async () => {
      const data = await fetchStatus(pid, tok);
      if (!data) return;
      if (data.kicked) {
        clearAdmissionPoll();
        onDenied();
      } else if (data.room_status === 'ended') {
        clearAdmissionPoll();
        onEnded();
      } else if (data.admitted) {
        clearAdmissionPoll();
        adoptStream(data);
        onAdmitted();
      } else if (data.started === true || (data.room_status && data.room_status !== 'scheduled')) {
        onRoomStarted();
      }
    }, 3000);
  });
}

export function stopAdmissionPoll(): void {
  clearAdmissionPoll();
  stopScheduledCountdown();
}

/** Take down the waiting and scheduled screens, and the polling behind them. */
export function leaveWaitingScreens(): void {
  stopAdmissionPoll();
  hideScreen('waiting-screen');
  hideScreen('scheduled-screen');
}

// ---- Scheduled start screen (issue #200) ----

function stopScheduledCountdown(): void {
  if (countdownInterval) {
    clearInterval(countdownInterval);
    countdownInterval = null;
  }
}

function fmtStart(at: Date, zone: string): string {
  const when = at.toLocaleString(undefined, {
    timeZone: zone,
    weekday: 'long',
    year: 'numeric',
    month: 'long',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
  return `${when} ${zoneLabel(zone, at)}`;
}

// Renders the absolute start time + a live countdown. startsAt is stored UTC
// ("YYYY-MM-DD HH:MM:SS", no zone marker), so force a UTC parse.
//
// The time is given in the studio's zone when the admin has set one (gh #263)
// — the clock the session was arranged on — and, for a guest whose own clock
// reads differently at that moment, in theirs on a second line. Unset, it is
// the guest's own time alone, named so it cannot be mistaken for the host's.
export function showScheduledScreen(startsAt: string | null, name: string): void {
  waitingDisplayName = name;
  hideScreen('join-screen');
  showHidden('scheduled-screen');
  const nm = el('scheduled-name');
  if (nm) nm.textContent = name;

  const whenEl = el('scheduled-when');
  const localRow = el('scheduled-local');
  const localEl = el('scheduled-local-when');
  const countEl = el('scheduled-countdown');
  stopScheduledCountdown();
  localRow?.classList.add('u-hidden');
  if (!startsAt) {
    if (whenEl) whenEl.textContent = '';
    if (countEl) countEl.textContent = '';
    return;
  }
  const target = new Date(startsAt.replace(' ', 'T') + 'Z');
  const studio = viewerStore.get().roomInfo?.timezone;
  const mine = browserZone();
  const zone = isKnownZone(studio) ? studio : mine;
  if (whenEl) whenEl.textContent = fmtStart(target, zone);
  if (zone !== mine && offsetLabel(zone, target) !== offsetLabel(mine, target)) {
    if (localEl) localEl.textContent = fmtStart(target, mine);
    localRow?.classList.remove('u-hidden');
  }
  const tick = (): void => {
    if (!countEl) return;
    const ms = target.getTime() - Date.now();
    if (ms <= 0) {
      countEl.textContent = 'Starting…';
      return;
    }
    const s = Math.floor(ms / 1000);
    const parts: string[] = [];
    const d = Math.floor(s / 86400);
    const h = Math.floor((s % 86400) / 3600);
    const m = Math.floor((s % 3600) / 60);
    if (d) parts.push(`${d}d`);
    if (d || h) parts.push(`${h}h`);
    parts.push(`${m}m`);
    parts.push(`${s % 60}s`);
    countEl.textContent = `Starts in ${parts.join(' ')}`;
  };
  tick();
  countdownInterval = setInterval(tick, 1000);
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
  kind:
    | 'show-app'
    | 'show-waiting'
    | 'show-scheduled'
    | 'show-kicked'
    | 'show-join'
    | 'show-landing';
  roomInfo?: RoomInfo;
  initialStatus?: RoomInfo['status'];
  waitingName?: string;
  startsAt?: string | null;
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
    const savedPass = savedPassword();
    if (savedName) (el('name-input') as HTMLInputElement).value = savedName;
    if (savedPass) (el('password-input') as HTMLInputElement).value = savedPass;

    // If this tab was kicked, stay on kicked screen but poll so we
    // automatically rejoin when the admin clears the kick.
    if (sessionStorage.getItem(KICKED_KEY)) {
      return { kind: 'show-kicked', roomInfo };
    }

    const savedSession = loadSavedSession();

    if (savedSession && roomInfo.status !== 'ended') {
      setSession(savedSession.participantId, savedSession.token);
      const role = isPresenter ? 'presenter' : savedSession.role || 'viewer';
      viewerStore.set({
        role,
        deliveryMode: roomInfo.delivery_mode || savedSession.deliveryMode || 'webrtc',
        playbackId: savedSession.playbackId ?? null,
        noiseDefault: !!roomInfo.noise_reduction,
        echoDefault: !!roomInfo.echo_cancellation,
        pttDefault: !!roomInfo.push_to_talk,
      });

      // Re-check admission on resume, and take the room's stream from the
      // server rather than from what was saved — the admin may have attached,
      // swapped or detached a key since, and a session saved before playback
      // ids existed holds no usable one at all. A participant who refreshed
      // while in the waiting room must land back on waiting-screen, not sneak
      // into the app shell.
      try {
        const sres = await fetch(
          `${API}/${slug}/status/${savedSession.participantId}?token=${encodeURIComponent(savedSession.token)}`,
        );
        if (sres.status === 404 || sres.status === 401) {
          sessionStorage.removeItem(SESSION_KEY);
          return { kind: 'show-join', roomInfo };
        }
        const sdata: StatusResponse = await sres.json();
        if (sdata.kicked) {
          sessionStorage.setItem(KICKED_KEY, '1');
          return { kind: 'show-kicked', roomInfo };
        }
        viewerStore.set({ playbackId: sdata.playback_id });
        updateSavedPlaybackId(sdata.playback_id);
        // Presenters are admitted by role.
        if (role === 'presenter' || sdata.admitted) {
          return { kind: 'show-app', roomInfo, initialStatus: roomInfo.status };
        }
        // A held joiner in a scheduled room lands on the scheduled screen
        // (issue #200); a plain waiting-room joiner on the waiting screen.
        if (roomInfo.status === 'scheduled') {
          return {
            kind: 'show-scheduled',
            roomInfo,
            startsAt: roomInfo.starts_at,
            waitingName: savedName,
          };
        }
        return {
          kind: 'show-waiting',
          roomInfo,
          waitingName: savedName,
        };
      } catch {
        // Unreachable server: a host still resumes on what was saved, as
        // before; anyone else is asked to join again.
        if (role === 'presenter') {
          return { kind: 'show-app', roomInfo, initialStatus: roomInfo.status };
        }
        return { kind: 'show-join', roomInfo };
      }
    } else if (savedSession && roomInfo.status === 'ended') {
      sessionStorage.removeItem(SESSION_KEY);
      return { kind: 'show-join', roomInfo };
    } else if (autoName) {
      // ?n= from admin one-shot links — skip the join form.
      (el('name-input') as HTMLInputElement).value = autoName;
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
      playbackId: data.playback_id,
      noiseDefault: data.noise_reduction_default,
      echoDefault: data.echo_cancellation_default,
      pttDefault: data.push_to_talk_default,
    });

    saveSession({
      participantId: data.participant_id,
      token: data.token,
      deliveryMode: data.delivery_mode,
      playbackId: data.playback_id,
      role: data.role || 'viewer',
    });
    localStorage.setItem(NAME_KEY, name);
    savePassword(password);

    if (!data.admitted) {
      // A scheduled room holds every joiner (regardless of waiting_room) on
      // the scheduled screen until the start poller admits them (issue #200).
      if (data.status === 'scheduled') {
        return { kind: 'show-scheduled', startsAt: data.starts_at, waitingName: name };
      }
      if (data.waiting_room) {
        hideScreen('join-screen');
        showHidden('waiting-screen');
        const wn = el('waiting-name');
        if (wn) wn.textContent = name;
        return { kind: 'show-waiting', waitingName: name };
      }
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
  waitingDisplayName = name;
  hideScreen('join-screen');
  showHidden('waiting-screen');
  const wn = el('waiting-name');
  if (wn) wn.textContent = name;
}
