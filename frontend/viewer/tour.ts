// First-run room tour (#230) — a spotlight walkthrough of the viewer chrome for
// people who have never been in a Farbstrom room.
//
// The tour is deliberately *descriptive*, never interactive: a full-screen
// blocker swallows every click while it runs, so no step can leave the room in
// a state the participant didn't ask for. The one thing it drives itself is the
// chat panel (opened for the chat step, restored on the way out).
//
// Steps are resolved against the LIVE DOM: each names the selectors it points
// at, the spotlight covers whichever of them are visible, and a step with none
// left is dropped before the tour starts. That is what keeps it honest across
// the three toolbar layouts (desktop inline, mobile ⋯ sheet, landscape side
// pills — see layout.ts) and across room shapes: no pointer button outside
// focus view, no player controls in an app-only (SRT) room.

import { setChatOpen, switchPanelTab } from './layout.js';
import { scopesAvailable } from './scopes.js';
import { getState } from './state.js';

// Once per device, for every room. Deliberately NOT slug-scoped like the rest of
// the viewer's keys: the question is "has this person ever been in a room", not
// "in *this* room" (#230). Every room is the same origin, so one record covers
// all of them. Bump the suffix to re-show the tour after a big UI change.
const SEEN_KEY = 'farbstrom_tour_v1';
const SEEN_COOKIE = 'farbstrom_tour';
const TWO_YEARS = 60 * 60 * 24 * 730;

// Recorded twice, and either one counts. localStorage is the primary; the cookie
// is what survives a browser that blocks DOM storage but allows cookies, and it
// is what #230 asked for in the first place. Neither survives a private window
// or a manual "clear site data" — a fresh browser profile is a first visit by
// definition, which is what makes testing the tour look like it never sticks.
function tourSeen(): boolean {
  try {
    if (localStorage.getItem(SEEN_KEY) === '1') return true;
  } catch {
    /* storage blocked — fall through to the cookie */
  }
  return document.cookie.split('; ').some((c) => c === `${SEEN_COOKIE}=1`);
}

function markTourSeen(): void {
  try {
    localStorage.setItem(SEEN_KEY, '1');
  } catch {
    /* storage blocked — the cookie below still carries it */
  }
  const secure = location.protocol === 'https:' ? '; Secure' : '';
  document.cookie = `${SEEN_COOKIE}=1; path=/; max-age=${TWO_YEARS}; SameSite=Lax${secure}`;
}

// ---- Steps ----

interface TourStep {
  title: string;
  // Trusted HTML — hardcoded copy only, never user or room input.
  body: string;
  // Selectors the spotlight covers (their union). Omit for a centered card.
  targets?: string[];
  // Overrides the default "at least one target is visible" applicability test.
  // Only needed for a step whose `before` hook is what makes the target visible.
  available?: () => boolean;
  // Prepares the UI for the step. Must be idempotent — stepping back re-runs it.
  before?: () => void;
  // Marks the step that opens the chat panel, so any step that isn't it hands
  // the panel back to however the participant had it.
  usesChatPanel?: boolean;
}

// Key chip, shared with the ? shortcuts sheet (see .key-cap in the page CSS).
function key(k: string): string {
  return `<span class="key-cap">${k}</span>`;
}

// Chat panel state from before the tour touched it, so the chat/files steps can
// hand the room back exactly as they found it.
let chatWasOpen: boolean | null = null;

function openChatPanel(tab: 'chat' | 'files'): void {
  if (chatWasOpen === null) chatWasOpen = getState().chatOpen;
  setChatOpen(true);
  switchPanelTab(tab);
}

function restoreChatPanel(): void {
  if (chatWasOpen === null) return;
  switchPanelTab('chat');
  setChatOpen(chatWasOpen);
  chatWasOpen = null;
}

function buildSteps(): TourStep[] {
  const ptt = getState().pttEnabled;
  const appOnly = getState().deliveryMode === 'srt';
  // A call-only room has no scopes button to point at (#246). The spotlight
  // would drop the target on its own, but the copy has to stop naming it.
  const scopes = scopesAvailable();
  // Compact toolbar (mobile portrait): layout.ts has moved the secondary
  // controls into the ⋯ sheet, so the last step points at the sheet instead of
  // at buttons that aren't on screen.
  const compact = visibleRect('#more-btn') !== null;

  const micCopy = ptt
    ? `Push-to-talk is on in this room: hold the mic button, or the ${key('S')}
       key, to talk. Camera (${key('A')}) is a normal toggle.`
    : `Toggle your camera (${key('A')}) and mic (${key('S')}). Screen share sits
       next to them.`;

  const lastStep: TourStep = compact
    ? {
        title: 'More controls',
        body: `Screen share, playback, focus view,${scopes ? ' scopes,' : ''}
               devices, resync and the ? shortcuts sheet are behind the ⋯
               button.`,
        targets: ['#more-btn'],
      }
    : {
        title: 'Playback and devices',
        body: `${appOnly ? '' : 'Play, pause and your own volume. '}Device
               settings, resync to live, fullscreen (${key('F')}), and ? for the
               keyboard shortcuts.`,
        targets: [
          ...(appOnly ? [] : ['#play-btn', '#mute-btn', '#volume-slider']),
          '#device-btn',
          '#resync-btn',
          '#fullscreen-btn',
          '#help-btn',
        ],
      };

  const steps: TourStep[] = [
    {
      title: 'Room tour',
      body: `A quick pass over the controls. Skip it if you like — the ? button
             in the toolbar lists the shortcuts and can start it again.`,
    },
    {
      title: 'The stage',
      body: appOnly
        ? `Shared screens and everyone's cameras show up here. Pin a shared
           screen to give it the full stage.`
        : `The broadcast, shared screens and everyone's cameras show up here.
           Pin the broadcast or a shared screen to give it the full stage.`,
      targets: ['#stage'],
    },
    {
      title: 'Camera and mic',
      body: micCopy,
      targets: ['#cam-btn', '#mic-btn', '#screen-btn'],
    },
    {
      title: scopes ? 'Pointer, scopes, layout' : 'Pointer and layout',
      body:
        `The pointer (${key('D')}) puts your cursor on the picture for everyone
         to see. ` +
        (scopes
          ? `Scopes (${key('W')}) opens waveform, RGB parade and vectorscope. `
          : '') +
        `The rest switch between grid and focus view, show the participant
         strip, and open chat.`,
      targets: [
        '#pointer-btn',
        ...(scopes ? ['#scopes-btn'] : []),
        '#focus-btn',
        '#conf-toggle',
        '#chat-toggle',
      ],
    },
    {
      title: 'Chat and files',
      body: `Chat is here (${key('C')}). Attach a file with the clip, or drop it
             anywhere on the page. Everything shared lands in the Files tab.`,
      targets: ['#right-panel'],
      available: () => true, // the panel is always present; `before` opens it
      before: () => openChatPanel('chat'),
      usesChatPanel: true,
    },
    lastStep,
  ];

  return steps.filter(applicable);
}

// ---- Geometry ----

const SPOT_PAD = 6; // breathing room between the target and the ring
const CARD_GAP = 12; // between the spotlight and the card
const EDGE = 8; // viewport margin

// A target counts only when it is actually on screen: `offsetParent === null`
// catches `display: none` anywhere up the tree (the ⋯ sheet, the hidden top bar
// in landscape, `body:not(.has-focus) #pointer-btn`), and a zero-size rect
// catches the collapsed chat panel.
function visibleRect(sel: string): DOMRect | null {
  const el = document.querySelector<HTMLElement>(sel);
  if (!el || el.offsetParent === null) return null;
  const r = el.getBoundingClientRect();
  return r.width >= 1 && r.height >= 1 ? r : null;
}

function unionRect(sels: string[]): DOMRect | null {
  let l = Infinity;
  let t = Infinity;
  let r = -Infinity;
  let b = -Infinity;
  for (const sel of sels) {
    const rect = visibleRect(sel);
    if (!rect) continue;
    l = Math.min(l, rect.left);
    t = Math.min(t, rect.top);
    r = Math.max(r, rect.right);
    b = Math.max(b, rect.bottom);
  }
  return r > l && b > t ? new DOMRect(l, t, r - l, b - t) : null;
}

function applicable(step: TourStep): boolean {
  if (step.available) return step.available();
  if (!step.targets) return true;
  return step.targets.some((s) => visibleRect(s) !== null);
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(Math.max(v, lo), hi);
}

// ---- Engine ----

let steps: TourStep[] = [];
let idx = 0;
let root: HTMLElement | null = null;
let rafId = 0;
// Last laid-out geometry, so the rAF loop only writes when something moved.
let lastGeom = '';

export function isTourActive(): boolean {
  return root !== null;
}

function el(id: string): HTMLElement {
  return document.getElementById(id) as HTMLElement;
}

function buildDom(): void {
  const r = document.createElement('div');
  r.id = 'tour-root';
  r.innerHTML = `
    <div id="tour-blocker"></div>
    <div id="tour-spot" class="is-off" aria-hidden="true"></div>
    <div id="tour-card" role="dialog" aria-modal="true" aria-labelledby="tour-title">
      <button class="btn-icon btn-icon-sm btn-icon-ghost" id="tour-skip" title="Skip tour" aria-label="Skip tour">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
      </button>
      <div class="tour-title" id="tour-title"></div>
      <div class="tour-body" id="tour-body"></div>
      <div class="tour-foot">
        <span class="tour-count" id="tour-count"></span>
        <div class="tour-foot-actions">
          <button class="btn btn-sm btn-ghost" id="tour-back"></button>
          <button class="btn btn-sm btn-primary" id="tour-next"></button>
        </div>
      </div>
      <div class="tour-caret" id="tour-caret" aria-hidden="true"></div>
    </div>`;
  document.body.appendChild(r);
  root = r;

  el('tour-skip').addEventListener('click', endTour);
  el('tour-back').addEventListener('click', () => {
    if (idx === 0) endTour();
    else goto(-1);
  });
  el('tour-next').addEventListener('click', () => goto(1));
}

// Move `delta` steps, skipping anything that stopped being applicable while the
// tour was open (a stream that ended takes the pointer button with it). Walking
// off the end finishes the tour; walking off the front stays put.
function goto(delta: number): void {
  let i = idx + delta;
  while (i >= 0 && i < steps.length && !applicable(steps[i]!)) i += delta;
  if (i >= steps.length) {
    endTour();
    return;
  }
  if (i < 0) return;
  idx = i;
  render();
}

function render(): void {
  const step = steps[idx];
  if (!step || !root) return;
  if (!step.usesChatPanel) restoreChatPanel();
  step.before?.();

  el('tour-title').textContent = step.title;
  el('tour-body').innerHTML = step.body;
  el('tour-count').textContent = `${idx + 1} / ${steps.length}`;
  el('tour-back').textContent = idx === 0 ? 'Not now' : 'Back';
  el('tour-next').textContent =
    idx === 0 ? 'Take the tour' : idx === steps.length - 1 ? 'Done' : 'Next';

  // Restart the card's entry animation.
  const card = el('tour-card');
  card.classList.remove('is-in');
  void card.offsetWidth;
  card.classList.add('is-in');

  lastGeom = ''; // force a reposition even if the target didn't move
  layout();
  (el('tour-next') as HTMLButtonElement).focus({ preventScroll: true });
}

function layout(): void {
  if (!root) return;
  const step = steps[idx];
  if (!step) return;
  const spot = el('tour-spot');
  const card = el('tour-card');
  const caret = el('tour-caret');
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const rect = step.targets ? unionRect(step.targets) : null;
  const cw = card.offsetWidth;
  const ch = card.offsetHeight;

  const geom = rect
    ? `${Math.round(rect.left)},${Math.round(rect.top)},${Math.round(rect.width)},${Math.round(rect.height)},${cw},${ch},${vw},${vh}`
    : `none,${cw},${ch},${vw},${vh}`;
  if (geom === lastGeom) return;
  lastGeom = geom;

  // No target: dim the whole viewport and centre the card.
  if (!rect) {
    spot.classList.add('is-off');
    el('tour-blocker').classList.add('is-dim');
    card.classList.remove('place-below', 'place-above', 'place-left', 'place-right');
    caret.style.display = 'none';
    card.style.left = `${Math.round((vw - cw) / 2)}px`;
    card.style.top = `${Math.round((vh - ch) / 2)}px`;
    return;
  }

  const left = Math.max(EDGE / 2, rect.left - SPOT_PAD);
  const top = Math.max(EDGE / 2, rect.top - SPOT_PAD);
  const width = Math.min(rect.width + SPOT_PAD * 2, vw - left - EDGE / 2);
  const height = Math.min(rect.height + SPOT_PAD * 2, vh - top - EDGE / 2);

  // Coming back from a card-only step the spot has no meaningful old position,
  // so suppress the glide for that one frame — otherwise it flies in from 0,0.
  const wasOff = spot.classList.contains('is-off');
  if (wasOff) spot.classList.add('no-anim');
  spot.classList.remove('is-off');
  el('tour-blocker').classList.remove('is-dim');
  spot.style.left = `${Math.round(left)}px`;
  spot.style.top = `${Math.round(top)}px`;
  spot.style.width = `${Math.round(width)}px`;
  spot.style.height = `${Math.round(height)}px`;
  if (wasOff) {
    void spot.offsetWidth;
    spot.classList.remove('no-anim');
  }

  // Card placement: hug the spotlight on whichever side has room. Below and
  // above first (most targets are toolbar-shaped), then the two sides — that
  // last pair is what a full-height target like the chat panel needs, since
  // neither of the first two ever fits beside it.
  const spotRight = left + width;
  const spotBottom = top + height;
  const centreX = clamp(left + width / 2 - cw / 2, EDGE, Math.max(EDGE, vw - cw - EDGE));
  const centreY = clamp(top + height / 2 - ch / 2, EDGE, Math.max(EDGE, vh - ch - EDGE));
  let place: 'place-below' | 'place-above' | 'place-left' | 'place-right';
  let cardLeft: number;
  let cardTop: number;
  if (spotBottom + CARD_GAP + ch <= vh - EDGE) {
    place = 'place-below';
    cardLeft = centreX;
    cardTop = spotBottom + CARD_GAP;
  } else if (top - CARD_GAP - ch >= EDGE) {
    place = 'place-above';
    cardLeft = centreX;
    cardTop = top - CARD_GAP - ch;
  } else if (left - CARD_GAP - cw >= EDGE) {
    place = 'place-left';
    cardLeft = left - CARD_GAP - cw;
    cardTop = centreY;
  } else if (spotRight + CARD_GAP + cw <= vw - EDGE) {
    place = 'place-right';
    cardLeft = spotRight + CARD_GAP;
    cardTop = centreY;
  } else {
    // Nothing fits beside it (a spotlight covering most of the viewport, e.g.
    // the stage, or the chat panel on a phone) — sit on top of it instead.
    place = vh - spotBottom >= top ? 'place-below' : 'place-above';
    cardLeft = centreX;
    cardTop = clamp(
      place === 'place-below' ? spotBottom + CARD_GAP : top - CARD_GAP - ch,
      EDGE,
      Math.max(EDGE, vh - ch - EDGE),
    );
  }

  card.classList.remove('place-below', 'place-above', 'place-left', 'place-right');
  card.classList.add(place);
  card.style.left = `${Math.round(cardLeft)}px`;
  card.style.top = `${Math.round(cardTop)}px`;

  // The caret only makes sense while the card sits clear of the spotlight — in
  // the overlap fallback above it doesn't, and a caret would point at nothing.
  const clear =
    place === 'place-below'
      ? cardTop >= spotBottom
      : place === 'place-above'
        ? cardTop + ch <= top
        : place === 'place-left'
          ? cardLeft + cw <= left
          : cardLeft >= spotRight;
  caret.style.display = clear ? '' : 'none';
  if (!clear) return;
  if (place === 'place-below' || place === 'place-above') {
    caret.style.top = '';
    caret.style.left = `${Math.round(clamp(left + width / 2 - cardLeft, 16, Math.max(16, cw - 16)))}px`;
  } else {
    caret.style.left = '';
    caret.style.top = `${Math.round(clamp(top + height / 2 - cardTop, 16, Math.max(16, ch - 16)))}px`;
  }
}

function tick(): void {
  if (!root) return;
  layout();
  rafId = requestAnimationFrame(tick);
}

function onKey(e: KeyboardEvent): void {
  if (e.key === 'Escape') {
    e.preventDefault();
    e.stopPropagation();
    endTour();
  } else if (e.key === 'ArrowRight') {
    e.preventDefault();
    e.stopPropagation();
    goto(1);
  } else if (e.key === 'ArrowLeft' && idx > 0) {
    e.preventDefault();
    e.stopPropagation();
    goto(-1);
  }
}

export function startTour(): void {
  if (root) return;
  steps = buildSteps();
  if (!steps.length) return;
  // Offering the tour counts as having shown it: a reload mid-tour shouldn't
  // re-prompt, and the ? button replays it on demand.
  markTourSeen();
  idx = 0;
  lastGeom = '';
  buildDom();
  // The blocker swallows the mouse; `inert` is what stops Tab + Enter reaching
  // a control behind the scrim (and hides the room from assistive tech while a
  // modal dialog is up). Harmless where unsupported — it's just an attribute.
  document.getElementById('app')?.toggleAttribute('inert', true);
  document.addEventListener('keydown', onKey, true);
  window.addEventListener('resize', layout);
  render();
  tick();
}

// Ends the tour if one is running. Exported because leaving the room, being
// kicked, or the session ending all replace the app with a full-page screen —
// which sits *below* --z-tour, so a tour left open would cover it.
export function stopTour(): void {
  endTour();
}

function endTour(): void {
  if (!root) return;
  document.getElementById('app')?.toggleAttribute('inert', false);
  document.removeEventListener('keydown', onKey, true);
  window.removeEventListener('resize', layout);
  cancelAnimationFrame(rafId);
  root.remove();
  root = null;
  restoreChatPanel();
}

// First visit on this device — offer the tour. Called once the join flow is
// done and the cam/mic prompt has been answered, so the tour never stacks on
// top of another dialog.
//
// Hosts are never offered it: they built the room. Deliberately no markTourSeen()
// on that path, so the same browser still gets the tour if it later joins a room
// as a participant. They can always start it from the ? sheet.
export function maybeStartTour(): void {
  if (getState().role === 'presenter') return;
  if (tourSeen()) return;
  startTour();
}
