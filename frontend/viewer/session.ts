// Slug + URL parameter parsing, presenter-handoff consumption, sessionStorage
// helpers. Most session values are immutable for the page's lifetime once set.

import type { SavedSession } from './types.js';

const path = location.pathname.replace(/^\//, '').split('/').filter(Boolean);
export const slug: string = path[path.length - 1] || '';

const query = new URLSearchParams(location.search);
// Host links carry `role` + `pk` in the URL fragment so the secret never
// hits the server (no Referer leak, no access logs) and we can wipe it
// from the address bar immediately after capture.
const hash = new URLSearchParams(location.hash.replace(/^#/, ''));

// A room password can ride in the link too: `#p=` keeps it out of the request
// entirely, while `?p=` reaches the server (whose request log redacts it).
// Either way it comes out of the address bar — and so out of history — as soon
// as it has been read.
export const autoPassword = hash.get('p') || query.get('p') || '';
export const autoName = query.get('n') || '';
export const presenterKey = hash.get('pk') || '';
export const isPresenter = hash.get('role') === 'presenter' && presenterKey.length > 0;

if (location.hash || query.has('p')) {
  const kept = new URLSearchParams(location.search);
  kept.delete('p');
  const search = kept.toString();
  history.replaceState(null, '', location.pathname + (search ? `?${search}` : ''));
}

// Mutable session identity, populated on join() or session resume.
let participantId: string | null = null;
let token: string | null = null;

export function setSession(pid: string, tok: string): void {
  participantId = pid;
  token = tok;
}

export function getParticipantId(): string {
  return participantId ?? '';
}

export function getToken(): string {
  return token ?? '';
}

// Storage key helpers — every key is namespaced by slug so a single browser
// can hold sessions for multiple rooms without collision.
export const PREF_KEY = `conf_pref_${slug}`;
export const NAME_KEY = `viewer_name_${slug}`;
const PASS_KEY = `viewer_pass_${slug}`;
export const SESSION_KEY = `viewer_session_${slug}`;
export const KICKED_KEY = `viewer_kicked_${slug}`;
const PRESESSION_KEY = `_presession_${slug}`;

// The room password a joiner typed is kept for this tab only, so a refresh —
// or the automatic rejoin once a kick is lifted — can fill it back in, and
// closing the tab forgets it. It used to sit in localStorage: shared by every
// tab, and kept indefinitely. Those copies are moved (this room's) or dropped
// (every other room's) on the first room page load since.
try {
  const legacy = localStorage.getItem(PASS_KEY);
  if (legacy && !sessionStorage.getItem(PASS_KEY)) sessionStorage.setItem(PASS_KEY, legacy);
  for (let i = localStorage.length - 1; i >= 0; i--) {
    const key = localStorage.key(i);
    if (key?.startsWith('viewer_pass_')) localStorage.removeItem(key);
  }
} catch {}

export function savedPassword(): string {
  return sessionStorage.getItem(PASS_KEY) || '';
}

export function savePassword(password: string): void {
  if (password) sessionStorage.setItem(PASS_KEY, password);
  else sessionStorage.removeItem(PASS_KEY);
}

export function loadSavedSession(): SavedSession | null {
  try {
    return JSON.parse(sessionStorage.getItem(SESSION_KEY) || 'null');
  } catch {
    return null;
  }
}

export function saveSession(s: SavedSession): void {
  sessionStorage.setItem(SESSION_KEY, JSON.stringify(s));
}

export function updateSavedPlaybackId(newKey: string | null): void {
  const raw = sessionStorage.getItem(SESSION_KEY);
  if (!raw) return;
  try {
    const s = JSON.parse(raw) as SavedSession;
    s.playbackId = newKey;
    sessionStorage.setItem(SESSION_KEY, JSON.stringify(s));
  } catch {}
}

export function clearSession(): void {
  sessionStorage.removeItem(SESSION_KEY);
}

export function markKicked(): void {
  sessionStorage.setItem(KICKED_KEY, '1');
}

export function clearKicked(): void {
  sessionStorage.removeItem(KICKED_KEY);
}

// Admin "Enter Room" — consume one-time presenter session handed off via
// localStorage. Run synchronously before loadRoomInfo() so the resume path
// finds the saved session. Idempotent (consumes + removes in one go).
export function consumePresession(): void {
  try {
    const raw = localStorage.getItem(PRESESSION_KEY);
    if (!raw) return;
    localStorage.removeItem(PRESESSION_KEY);
    const ps = JSON.parse(raw);
    if (!ps.participantId || !ps.token) return;
    saveSession({
      participantId: ps.participantId,
      token: ps.token,
      deliveryMode: ps.deliveryMode || 'webrtc',
      playbackId: ps.playbackId || null,
      role: ps.role || 'presenter',
    });
  } catch {}
}
