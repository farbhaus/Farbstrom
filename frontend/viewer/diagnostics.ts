// Connection state shared by the WS, conference and player subsystems, read by
// the roster dots and the stats panel (gh #40).
//
// This module imports nothing from the viewer. That is deliberate: conference.ts
// writes connection quality and roster.ts reads it, so parking the state in
// either one would put a cycle between them. Re-render notification goes through
// `subscribeDiag` rather than a direct call, the same handler-injection shape
// `configureWs` already uses.
//
// The panel's live readings answer "is it bad right now"; the event log answers
// "what happened five minutes ago", which is the question you actually have
// after a participant says the session was unstable. Nothing here is persisted —
// it is per-tab and dies with the tab, in keeping with the room's session model.

export type DiagSource = 'ws' | 'conference' | 'player';
export type DiagSeverity = 'info' | 'warn' | 'error';

/** LiveKit's ConnectionQuality, plus 'unknown' before the first report lands. */
export type Quality = 'excellent' | 'good' | 'poor' | 'lost' | 'unknown';

export type ConfState = 'idle' | 'connected' | 'reconnecting' | 'disconnected';

export interface DiagEvent {
  at: number;
  source: DiagSource;
  severity: DiagSeverity;
  text: string;
}

// Deep enough to cover a long session's blips, small enough that re-rendering
// the whole list every second stays free.
const MAX_EVENTS = 60;

const events: DiagEvent[] = [];
const quality = new Map<string, Quality>();

let confState: ConfState = 'idle';
let rtt: number | null = null;

const counters = { wsDrops: 0, confReconnects: 0, playerErrors: 0 };
export type DiagCounters = Readonly<typeof counters>;

const listeners = new Set<() => void>();

/** Register a re-render callback. Returns an unsubscribe. */
export function subscribeDiag(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function notify(): void {
  for (const fn of listeners) fn();
}

// ---- Event log -------------------------------------------------------------

/** Append an event. Consecutive identical messages collapse into the existing
 *  entry so one flapping connection can't push everything else out of the
 *  buffer — the timestamp moves to the most recent occurrence. */
export function logDiag(source: DiagSource, severity: DiagSeverity, text: string): void {
  const last = events[events.length - 1];
  if (last && last.source === source && last.text === text) {
    last.at = Date.now();
    last.severity = severity;
    return;
  }
  events.push({ at: Date.now(), source, severity, text });
  if (events.length > MAX_EVENTS) events.shift();
}

/** Newest first — the panel reads top-down. */
export function getDiagLog(): readonly DiagEvent[] {
  return [...events].reverse();
}

export function getDiagCounters(): DiagCounters {
  return counters;
}

export function countWsDrop(): void {
  counters.wsDrops += 1;
}

export function countConfReconnect(): void {
  counters.confReconnects += 1;
}

export function countPlayerError(): void {
  counters.playerErrors += 1;
}

// ---- Per-participant connection quality ------------------------------------

/** Keyed by participant id — LiveKit's identity is the participant id the WS
 *  roster uses, so the two line up without a translation table. */
export function setQuality(participantId: string, q: Quality): Quality | undefined {
  const previous = quality.get(participantId);
  quality.set(participantId, q);
  notify();
  return previous;
}

export function getQuality(participantId: string): Quality {
  return quality.get(participantId) ?? 'unknown';
}

export function clearQuality(): void {
  quality.clear();
  notify();
}

// ---- Conference + network state --------------------------------------------

export function setConfState(next: ConfState): void {
  confState = next;
  notify();
}

export function getConfState(): ConfState {
  return confState;
}

/** Round-trip time in ms from the app-level WS ping, or null before the first
 *  reply (or while the socket is down). */
export function setRtt(ms: number | null): void {
  rtt = ms;
}

export function getRtt(): number | null {
  return rtt;
}
