// Connection stats panel (gh #40) — reached from the Stats button in the roster
// header, which swaps the participants list for this view inside the same box.
//
// Two jobs, and the second is the reason this exists. The readouts say whether
// the link is healthy *now*; the sampler below runs for the whole session, not
// just while the panel is open, so the event log can answer "what happened
// during the session" after a participant reports it went unstable. Nothing is
// persisted — it dies with the tab, like the rest of the room's state.
//
// Deliberately not here: a throughput speed test. Saturating the link to
// measure it is exactly the wrong thing to do to a live review, and a figure
// bought at the cost of the picture the room is judging isn't worth having.
// Bitrate is likewise absent rather than guessed: neither the LL-HLS nor the
// WebRTC path hands us a byte counter we can trust, and a wrong number is worse
// than no number.

import { esc } from '../shared/utils.js';
import {
  getConfState,
  getDiagCounters,
  getDiagLog,
  getQuality,
  getRtt,
  logDiag,
  subscribeDiag,
  type DiagEvent,
  type Quality,
} from './diagnostics.js';
import { codecLabel, getStreamDiagnostics } from './player.js';
import { getParticipantId } from './session.js';
import { viewerStore } from './state.js';

const QUALITY_WORD: Record<Quality, string> = {
  excellent: 'Excellent',
  good: 'Good',
  poor: 'Poor',
  lost: 'Lost',
  unknown: 'Not measured',
};

// 1 Hz is enough to compute a stable frame rate and to catch a stall within a
// second or two, and cheap enough to leave running all session.
const SAMPLE_MS = 1000;

interface Snapshot {
  width: number;
  height: number;
  fps: number | null;
  droppedTotal: number;
  framesTotal: number;
  bufferAhead: number | null;
  stalled: boolean;
}

let snapshot: Snapshot | null = null;
let sampleTimer: ReturnType<typeof setInterval> | null = null;
let renderTimer: ReturnType<typeof setInterval> | null = null;

// Frame counters are per-element and reset when OvenPlayer remounts, so carry
// the finished element's totals forward rather than letting the numbers jump
// backwards mid-session.
let carriedFrames = 0;
let carriedDropped = 0;
let lastFrames = 0;
let lastDropped = 0;
// Consecutive samples where playback was live but no new frame arrived. One
// tick is ordinary jitter; two is a stall worth recording.
let stalledTicks = 0;
let stallLogged = false;

function playbackQuality(video: HTMLVideoElement): { total: number; dropped: number } | null {
  if (typeof video.getVideoPlaybackQuality !== 'function') return null;
  const q = video.getVideoPlaybackQuality();
  return { total: q.totalVideoFrames, dropped: q.droppedVideoFrames };
}

function sample(): void {
  const { video } = getStreamDiagnostics();
  if (!video) {
    snapshot = null;
    stalledTicks = 0;
    stallLogged = false;
    return;
  }

  const q = playbackQuality(video);
  let fps: number | null = null;
  let stalled = false;

  if (q) {
    // A remount hands us a fresh element whose counters start at zero.
    if (q.total < lastFrames) {
      carriedFrames += lastFrames;
      carriedDropped += lastDropped;
    }
    const delta = q.total - (q.total < lastFrames ? 0 : lastFrames);
    fps = Math.max(0, delta);
    lastFrames = q.total;
    lastDropped = q.dropped;

    // Playing, unmuted by the pipeline, but no new frame this second.
    const playing = !video.paused && !video.ended && video.readyState >= 2;
    if (playing && delta === 0) {
      stalledTicks += 1;
      if (stalledTicks >= 2) {
        stalled = true;
        if (!stallLogged) {
          logDiag('player', 'warn', 'Video stalled — no new frames');
          stallLogged = true;
        }
      }
    } else {
      if (stallLogged && delta > 0) logDiag('player', 'info', 'Video recovered');
      stalledTicks = 0;
      stallLogged = false;
    }
  }

  let bufferAhead: number | null = null;
  try {
    const b = video.buffered;
    if (b.length > 0) bufferAhead = Math.max(0, b.end(b.length - 1) - video.currentTime);
  } catch {
    // Safari throws on buffered access for some live sources — not worth a log.
  }

  snapshot = {
    width: video.videoWidth,
    height: video.videoHeight,
    fps,
    droppedTotal: carriedDropped + lastDropped,
    framesTotal: carriedFrames + lastFrames,
    bufferAhead,
    stalled,
  };
}

// ---- Rendering -------------------------------------------------------------

const row = (label: string, value: string, extraClass = ''): string =>
  `<div class="stat-row${extraClass ? ` ${extraClass}` : ''}">
     <span class="stat-key">${esc(label)}</span>
     <span class="stat-val">${esc(value)}</span>
   </div>`;

const section = (title: string, body: string): string =>
  `<div class="stat-section">
     <div class="stat-head">${esc(title)}</div>
     ${body}
   </div>`;

function sourceLabel(): string {
  const { mode } = getStreamDiagnostics();
  const { deliveryMode } = viewerStore.get();
  if (mode === 'live') return `Live broadcast · ${deliveryMode === 'llhls' ? 'LL-HLS' : 'WebRTC'}`;
  if (mode === 'file') return 'Shared file';
  if (mode === 'image') return 'Shared image';
  if (deliveryMode === 'srt') return 'Farbplay app only';
  return 'Nothing playing';
}

function videoSection(): string {
  const { codecs, blockedCodec } = getStreamDiagnostics();
  const rows: string[] = [row('Source', sourceLabel())];

  if (blockedCodec) {
    rows.push(row('Codec', `${codecLabel(blockedCodec)} — not decodable here`, 'is-bad'));
  } else if (codecs.length > 0) {
    rows.push(row('Codec', codecs.map(codecLabel).join(', ')));
  }

  if (!snapshot) {
    rows.push(row('Picture', 'No video to measure'));
    return section('Video', rows.join(''));
  }

  const { width, height, fps, droppedTotal, framesTotal, bufferAhead, stalled } = snapshot;
  if (width && height) rows.push(row('Resolution', `${width} × ${height}`));
  if (fps !== null) rows.push(row('Frame rate', `${fps} fps`, stalled ? 'is-bad' : ''));
  if (framesTotal > 0) {
    const pct = (droppedTotal / framesTotal) * 100;
    // 1% dropped is where a colourist starts seeing it rather than measuring it.
    rows.push(
      row(
        'Dropped frames',
        `${droppedTotal.toLocaleString()} of ${framesTotal.toLocaleString()} (${pct.toFixed(1)}%)`,
        pct >= 1 ? 'is-bad' : '',
      ),
    );
  }
  if (bufferAhead !== null) rows.push(row('Buffer ahead', `${bufferAhead.toFixed(1)} s`));
  if (stalled) rows.push(row('Status', 'Stalled — no new frames', 'is-bad'));

  return section('Video', rows.join(''));
}

// LiveKit only measures a participant who is actually in the conference. A
// watch-only client — the common case for this product — never publishes
// anything, so its quality stays 'unknown' forever. Falling back to what we can
// measure ourselves keeps the headline useful for them, and the caption says
// which of the two it is rather than passing one off as the other.
function headline(): { q: Quality; word: string; basis: string } {
  const reported = getQuality(getParticipantId());
  if (reported !== 'unknown') {
    return { q: reported, word: QUALITY_WORD[reported], basis: 'Measured by the conference' };
  }

  const rtt = getRtt();
  if (rtt === null) {
    return { q: 'unknown', word: QUALITY_WORD.unknown, basis: 'Waiting for a first reading' };
  }

  const droppedPct =
    snapshot && snapshot.framesTotal > 0 ? (snapshot.droppedTotal / snapshot.framesTotal) * 100 : 0;
  const bad = rtt > 300 || droppedPct >= 1 || !!snapshot?.stalled;
  const fair = rtt > 150 || droppedPct >= 0.2;
  const q: Quality = bad ? 'poor' : fair ? 'good' : 'excellent';
  return { q, word: QUALITY_WORD[q], basis: 'From round trip and dropped frames' };
}

function yourConnectionSection(): string {
  const { q, word, basis } = headline();
  const rtt = getRtt();
  const conf = getConfState();
  const confWord =
    conf === 'connected'
      ? 'Connected'
      : conf === 'reconnecting'
        ? 'Reconnecting'
        : conf === 'disconnected'
          ? 'Disconnected'
          : 'Watching only';

  const wsLabel = document.getElementById('ws-label')?.textContent || 'Unknown';

  return section(
    'Your connection',
    `<div class="stat-headline">
       <span class="q-dot q-${q}"></span>
       <span class="stat-headline-word">${esc(word)}</span>
     </div>
     <div class="stat-basis">${esc(basis)}</div>` +
      row('Round trip', rtt === null ? 'Measuring…' : `${rtt} ms`, rtt !== null && rtt > 300 ? 'is-bad' : '') +
      row('Room link', wsLabel, wsLabel === 'Connected' ? '' : 'is-bad') +
      row('Conference', confWord, conf === 'connected' || conf === 'idle' ? '' : 'is-bad'),
  );
}

function sessionSection(): string {
  const c = getDiagCounters();
  return section(
    'This session',
    row('Room drops', String(c.wsDrops), c.wsDrops > 0 ? 'is-warn' : '') +
      row('Conference reconnects', String(c.confReconnects), c.confReconnects > 0 ? 'is-warn' : '') +
      row('Playback errors', String(c.playerErrors), c.playerErrors > 0 ? 'is-warn' : ''),
  );
}

// Host-only. Everyone receives every participant's quality from LiveKit, but
// only the host has a reason to act on someone else's.
function roomSection(): string {
  if (viewerStore.get().role !== 'presenter') return '';
  const self = getParticipantId();
  const others = viewerStore.get().roster.filter((p) => p.id !== self);
  if (others.length === 0) return section('Room', `<div class="stat-empty">Nobody else here.</div>`);

  const rows = others
    .map((p) => {
      const q = getQuality(p.id);
      // Farbplay watches over SRT and joins no conference, so it has nothing to
      // report — say so rather than leaving the host wondering.
      const label = p.client === 'farbplay' ? `${p.name} · Farbplay` : p.name;
      return `<div class="stat-row">
          <span class="stat-key"><span class="q-dot q-${q}"></span>${esc(label)}</span>
          <span class="stat-val">${esc(QUALITY_WORD[q])}</span>
        </div>`;
    })
    .join('');
  return section('Room', rows);
}

function fmtClock(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function eventsSection(): string {
  const log = getDiagLog();
  if (log.length === 0) {
    return section('Recent events', `<div class="stat-empty">Nothing to report.</div>`);
  }
  const rows = log
    .map(
      (e: DiagEvent) =>
        `<div class="stat-event sev-${e.severity}">
           <span class="stat-event-time">${esc(fmtClock(e.at))}</span>
           <span class="stat-event-text">${esc(e.text)}</span>
         </div>`,
    )
    .join('');
  return section('Recent events', rows);
}

function render(): void {
  const el = document.getElementById('stats-body');
  if (!el || !isOpen()) return;
  el.innerHTML =
    yourConnectionSection() + videoSection() + roomSection() + sessionSection() + eventsSection();
}

/** The status badge is the trigger, so keep its tooltip current — the quality
 *  word is available on hover without changing how the badge looks. Its dot and
 *  label stay owned by ws.ts: they mean "is the room link up", which is a
 *  different question from "how good is the connection", and overloading one
 *  indicator with both would make neither readable. */
function renderTrigger(): void {
  const btn = document.getElementById('ws-status');
  if (!btn) return;
  const { word } = headline();
  btn.setAttribute('title', `Connection: ${word} — click for details`);
  btn.setAttribute('aria-label', `Connection stats — ${word}`);
}

// ---- Panel open/close ------------------------------------------------------

function isOpen(): boolean {
  const el = document.getElementById('stats-overlay');
  return !!el && !el.classList.contains('hidden');
}

function setOpen(open: boolean): void {
  // The roster shares this layer, so close it rather than stacking. Hidden by
  // id here instead of calling into roster.ts, which already imports this
  // module — going the other way too would put a cycle between them.
  if (open) document.getElementById('roster-overlay')?.classList.add('hidden');
  document.getElementById('stats-overlay')?.classList.toggle('hidden', !open);
  document.getElementById('ws-status')?.classList.toggle('active', open);

  if (renderTimer) {
    clearInterval(renderTimer);
    renderTimer = null;
  }
  if (open) {
    render();
    renderTimer = setInterval(render, SAMPLE_MS);
  }
}

export function closeStats(): void {
  if (isOpen()) setOpen(false);
}

export function initStats(): void {
  // Sampler runs for the whole session, not just while the panel is open —
  // that's what makes the event log worth reading afterwards, and it's what
  // keeps the pill's dot honest.
  if (!sampleTimer) {
    sampleTimer = setInterval(() => {
      sample();
      renderTrigger();
    }, SAMPLE_MS);
  }

  document.getElementById('ws-status')?.addEventListener('click', () => setOpen(!isOpen()));
  document.getElementById('stats-close')?.addEventListener('click', closeStats);
  document.getElementById('stats-overlay')?.addEventListener('click', (e) => {
    if ((e.target as HTMLElement).id === 'stats-overlay') closeStats();
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && isOpen()) closeStats();
  });

  // A quality report or a new event should show immediately, not on the next
  // render tick.
  subscribeDiag(() => {
    renderTrigger();
    if (isOpen()) render();
  });

  renderTrigger();
}
