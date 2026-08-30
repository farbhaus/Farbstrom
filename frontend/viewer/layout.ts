// Panel toggles, stage grid sizing, chrome auto-hide, fullscreen.
// Mobile-Safari resize listeners (visualViewport) live here too.

import { getPlayer } from './player.js';
import { viewerStore } from './state.js';

// Mobile breakpoint. Must stay in sync with the `@media (max-width: 700px)`
// rules in www/viewer/index.html (and the shared 700px breakpoint documented in
// www/shared/tokens.css).
const MOBILE_BP = 700;

// Compute the optimal column count for the stage grid so 16:9 tiles fill
// the container as efficiently as possible without overflowing.
//
// Focus view needs none of this since #248: the pinned tile is the background
// layer, absolutely filling the stage with the video `object-fit: contain`
// inside it, and every panel floats on top rather than taking a grid track. So
// there is no cell to fit the tile into and no leftover width to redistribute —
// the whole --focus-aspect / panel-absorption machinery went with the change.
function clearGrid(stage: HTMLElement): void {
  stage.style.gridTemplateColumns = '';
  stage.style.gridAutoRows = '';
}

export function sizeStage(): void {
  const stage = document.getElementById('stage');
  if (!stage) return;
  if (document.body.classList.contains('has-focus')) {
    // Grid mode leaves inline grid sizing behind; an inline style outranks the
    // focus-mode stylesheet rule. Clear it so the focus CSS governs (grid mode
    // re-sets it on the way back).
    clearGrid(stage);
    return;
  }
  // Visible tiles only — hidden #tile-stream / #tile-share don't take grid cells.
  const tiles = Array.from(stage.querySelectorAll<HTMLElement>(':scope > .tile')).filter(
    (el) => !el.classList.contains('hidden') && el.offsetParent !== null,
  );
  const n = tiles.length;
  if (n === 0) {
    // #stage-empty is the only child left and it sizes itself.
    clearGrid(stage);
    return;
  }
  const gap = 8;
  // Read the real padding rather than assuming a uniform gutter: the stage
  // insets past the floating chrome (--chrome-top/--chrome-bottom) and grows a
  // right inset again when chat is open, so the padding is neither symmetric
  // nor constant.
  const cs = getComputedStyle(stage);
  const padX = parseFloat(cs.paddingLeft) + parseFloat(cs.paddingRight);
  const padY = parseFloat(cs.paddingTop) + parseFloat(cs.paddingBottom);
  const cw = stage.clientWidth - padX;
  const ch = stage.clientHeight - padY;
  const RATIO = 16 / 9;
  let bestCols = 1;
  let bestArea = 0;
  let bestTileW = cw;
  let bestTileH = cw / RATIO;
  for (let cols = 1; cols <= n; cols++) {
    const rows = Math.ceil(n / cols);
    const tw = (cw - gap * (cols - 1)) / cols;
    const th = (ch - gap * (rows - 1)) / rows;
    const tileH = Math.min(th, tw / RATIO);
    const tileW = tileH * RATIO;
    const area = tileW * tileH;
    if (area > bestArea) {
      bestArea = area;
      bestCols = cols;
      bestTileW = tileW;
      bestTileH = tileH;
    }
  }
  const colW = (cw - gap * (bestCols - 1)) / bestCols;
  if (bestTileW < colW - 1) {
    stage.style.gridTemplateColumns = `repeat(${bestCols}, ${Math.floor(bestTileW)}px)`;
  } else {
    stage.style.gridTemplateColumns = `repeat(${bestCols}, 1fr)`;
  }
  // Pin the row height too. Without it the rows are `auto`, and an auto row
  // sized against a stretched item that gets its height from `aspect-ratio` is
  // circular: the row resolves from the tile's min-content height (which
  // ignores the ratio), comes out shorter than the tile, and the tiles spill
  // over each other. That is the overlap you get on the way out of focus view.
  // We already know the exact height, so make the track definite.
  stage.style.gridAutoRows = `${Math.floor(bestTileH)}px`;
}

// #stage animates its padding — opening chat grows the right inset, and
// entering/leaving focus view swaps the whole chrome safe band for 0. sizeStage
// reads that padding to size the grid, so it has to re-run every frame through
// the transition, not once at each end.
//
// Running it once was a real bug (#248 follow-up): leaving focus view fired a
// single requestAnimationFrame, which landed while the padding was still ~0, so
// the column count was picked for a box 118px taller than the grid would
// actually get. The tiles came out stacked and overlapping, and stayed that way
// until something else fired a resize — which is exactly why toggling
// fullscreen "fixed" it.
export const STAGE_TRANSITION_MS = 320; // a touch over the 0.25s CSS transition
let panelReflowUntil = 0;
export function reflowStage(): void {
  sizeStage();
  const alreadyTicking = panelReflowUntil > performance.now();
  panelReflowUntil = performance.now() + STAGE_TRANSITION_MS;
  if (alreadyTicking) return; // running loop will honour the extended deadline
  const tick = (): void => {
    sizeStage();
    if (performance.now() < panelReflowUntil) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

export function setChatOpen(open: boolean): void {
  if (viewerStore.get().chatOpen === open) return;
  viewerStore.set({ chatOpen: open });
  document.getElementById('right-panel')?.classList.toggle('open', open);
  document.getElementById('chat-toggle')?.classList.toggle('panel-open', open);
  // Drives the grid's right inset — the panel floats, but grid tiles still
  // step aside for it (#248).
  document.body.classList.toggle('chat-open', open);
  if (open) document.getElementById('chat-toggle')?.classList.remove('has-notification');
  reflowStage();
}

export function toggleChat(): void {
  // A deliberate toggle outranks the fullscreen auto-hide — see below.
  chatHiddenByFullscreen = false;
  setChatOpen(!viewerStore.get().chatOpen);
}

// Swap the chat panel between its Chat and Files tab panes. Pure DOM state —
// no store field, since nothing outside the panel needs to read it.
export function switchPanelTab(tab: 'chat' | 'files'): void {
  document.querySelectorAll<HTMLElement>('.panel-tab').forEach((b) => {
    b.classList.toggle('is-active', b.dataset['tab'] === tab);
  });
  document.querySelectorAll<HTMLElement>('.tab-pane').forEach((p) => {
    p.hidden = p.dataset['tab'] !== tab;
  });
  if (tab === 'files') {
    document.getElementById('tab-files')?.classList.remove('has-notification');
  }
}

// Show/hide the focus-mode call panel. When not in focus mode it's a no-op
// (the CSS hides the button too).
function setConfOpen(open: boolean): void {
  if (viewerStore.get().confOpen === open) return;
  viewerStore.set({ confOpen: open });
  document.body.classList.toggle('strip-hidden', !open);
  document.getElementById('conf-toggle')?.classList.toggle('panel-open', open);
  reflowStage();
}

export function toggleConf(): void {
  // A deliberate toggle outranks the fullscreen auto-hide — see below.
  confHiddenByFullscreen = false;
  setConfOpen(!viewerStore.get().confOpen);
}

// Fullscreen is the "give me the picture" gesture, so both floating panels step
// out of the way for it and come back on exit.
//
// The per-panel flag is what makes that safe: only a panel *we* closed is ever
// reopened, and toggleChat/toggleConf clear their flag, so a deliberate choice
// always survives the round trip. Closed it yourself before going fullscreen?
// It stays closed. Reopened it while fullscreen? It stays open on the way out.
let confHiddenByFullscreen = false;
let chatHiddenByFullscreen = false;

function syncPanelsToFullscreen(inFs: boolean): void {
  const { confOpen, chatOpen } = viewerStore.get();
  if (inFs) {
    if (confOpen) {
      confHiddenByFullscreen = true;
      setConfOpen(false);
    }
    if (chatOpen) {
      chatHiddenByFullscreen = true;
      setChatOpen(false);
    }
    return;
  }
  if (confHiddenByFullscreen) {
    confHiddenByFullscreen = false;
    setConfOpen(true);
  }
  if (chatHiddenByFullscreen) {
    chatHiddenByFullscreen = false;
    setChatOpen(true);
  }
}

function setupFullscreen(): void {
  const btn = document.getElementById('fullscreen-btn');
  // iOS pauses the underlying media element when leaving fullscreen. Resume
  // the live player so the stream doesn't sit frozen on a still frame.
  const resumePlayback = (): void => {
    const p = getPlayer();
    if (!p) return;
    const kick = (): void => {
      try {
        if (p.getState() !== 'playing') p.play();
      } catch {}
    };
    kick();
    setTimeout(kick, 300);
  };
  btn?.addEventListener('click', () => {
    const fsEl = document.fullscreenElement || document.webkitFullscreenElement;
    if (fsEl) {
      (document.exitFullscreen || document.webkitExitFullscreen)?.call(document);
      return;
    }
    // Fullscreen the whole page, not a tile (#248). The pinned tile already
    // fills the window as the background layer, so maximising it would gain
    // nothing; what this button is for now is dropping the browser's own
    // chrome so the room reads as an app. Chat, the call panel, the toolbar
    // and every modal come along, which they would not if we fullscreened a
    // single element.
    const target = document.documentElement;
    if (target.requestFullscreen) {
      void target.requestFullscreen();
    } else if (target.webkitRequestFullscreen) {
      target.webkitRequestFullscreen();
    } else {
      // iPhone Safari has no element fullscreen at all — only the <video> can
      // go fullscreen there, and exiting it doesn't fire document
      // fullscreenchange, so resume on its own end event. The app effect isn't
      // available on that platform; a maximised player is the best it can do.
      const video = document.querySelector<HTMLVideoElement>('#player video');
      if (video?.webkitEnterFullscreen) {
        video.addEventListener('webkitendfullscreen', resumePlayback, { once: true });
        video.webkitEnterFullscreen();
      }
    }
  });
  const onFsChange = (): void => {
    const inFs = !!(document.fullscreenElement || document.webkitFullscreenElement);
    btn?.classList.toggle('active', inFs);
    if (!inFs) resumePlayback();
    syncPanelsToFullscreen(inFs);
    // The viewport just changed size, and the panels may be animating out or in
    // — reflow across the whole transition, not on a single frame.
    reflowStage();
  };
  document.addEventListener('fullscreenchange', onFsChange);
  document.addEventListener('webkitfullscreenchange', onFsChange);
}

// ---- Chrome auto-hide (#248) ----
//
// The top chips and the bottom toolbar fade out after a couple of idle seconds
// so the picture underneath is unobstructed, and come straight back on any sign
// of life. Deliberately narrow in scope: only the two chrome bars and the
// landscape side pills fade. The chat and call panels don't — the participant
// opened those, and a panel that vanished while being read would be a bug, not
// an effect.
//
// Idle is measured from the last input event, not from a timer the events
// restart, so a burst of pointermove costs one class check rather than one
// timer teardown each.
const CHROME_IDLE_MS = 2500;
let chromeIdleTimer: number | null = null;
let chromeLocks = 0;

function chromeHovered(): boolean {
  // :hover on the chrome keeps it up — the pointer resting on the toolbar
  // without moving is not idleness. Also covers the open ⋯ sheet, which is a
  // child of the pill.
  return !!document.querySelector('#top-bar:hover, #bottom-toolbar:hover, .side-pill:hover');
}

function chromeFocused(): boolean {
  // Keyboard users: never fade the control that currently has focus out from
  // under them.
  const el = document.activeElement;
  return !!el && !!el.closest?.('#top-bar, #bottom-toolbar, .side-pill');
}

function hideChrome(): void {
  if (chromeLocks > 0 || chromeHovered() || chromeFocused()) {
    scheduleChromeHide();
    return;
  }
  document.body.classList.add('chrome-hidden');
}

function scheduleChromeHide(): void {
  if (chromeIdleTimer !== null) clearTimeout(chromeIdleTimer);
  chromeIdleTimer = window.setTimeout(hideChrome, CHROME_IDLE_MS);
}

export function wakeChrome(): void {
  document.body.classList.remove('chrome-hidden');
  scheduleChromeHide();
}

// Hold the chrome open for as long as something else owns the screen — the
// tour, which spotlights these very controls, is the caller that matters.
// Counted rather than boolean so overlapping holders can't un-hold each other.
export function lockChrome(): () => void {
  chromeLocks++;
  wakeChrome();
  let released = false;
  return () => {
    if (released) return;
    released = true;
    chromeLocks--;
    wakeChrome();
  };
}

function setupChromeIdle(): void {
  for (const ev of ['pointermove', 'pointerdown', 'keydown', 'wheel', 'touchstart'] as const) {
    document.addEventListener(ev, wakeChrome, { passive: true });
  }
  // Tabbing into a control that has already faded brings it back.
  document.addEventListener('focusin', wakeChrome);
  scheduleChromeHide();
}

// The bottom toolbar (#186) is a floating centered pill over the stage (#248).
// On mobile it runs in "compact" mode: the secondary controls move into the ⋯
// popup so the pill stays a single row.
function pillEl(): HTMLElement | null {
  return document.getElementById('bottom-toolbar');
}

// The ⋯ More button (mobile) toggles the secondary-controls sheet above the pill.
function setupMoreMenu(): void {
  const pill = pillEl();
  const moreBtn = document.getElementById('more-btn');
  if (!pill || !moreBtn) return;
  moreBtn.addEventListener('click', (e) => {
    e.stopPropagation();
    const open = pill.classList.toggle('more-open');
    moreBtn.classList.toggle('active', open);
  });
  // Tapping outside the pill closes the sheet.
  document.addEventListener('pointerdown', (e) => {
    if (!pill.classList.contains('more-open')) return;
    if (pill.contains(e.target as Node)) return;
    pill.classList.remove('more-open');
    moreBtn.classList.remove('active');
  });
}

// In compact mode (mobile) the secondary controls move out of the pill into
// #more-sheet (the ⋯ popup) so the pill stays a slim single row; otherwise they
// return to their original inline positions (preserving the full toolbar order).
// DOM-move (not clone) keeps their event listeners intact. Screen-share lives in
// the sheet (rarely usable on mobile) while the strip toggle stays in the pill.
const TOOLBAR_EXTRA_IDS = ['focus-btn', 'screen-btn', 'player-controls', 'scopes-btn', 'device-btn', 'resync-btn', 'help-btn'];
type ToolbarAnchor = { parent: Node; next: Node | null };
const toolbarAnchors = new Map<Element, ToolbarAnchor>();

function applyCompactToolbar(compact: boolean): void {
  const sheet = document.getElementById('more-sheet');
  const pill = pillEl();
  if (!sheet || !pill) return;
  pill.classList.toggle('compact', compact);
  if (compact) {
    for (const id of TOOLBAR_EXTRA_IDS) {
      const el = document.getElementById(id);
      if (!el) continue;
      if (!toolbarAnchors.has(el)) {
        toolbarAnchors.set(el, { parent: el.parentNode!, next: el.nextSibling });
      }
      sheet.appendChild(el);
    }
  } else if (toolbarAnchors.size) {
    // Restore in reverse insert order so nextSibling references resolve even
    // when several siblings moved out of the same parent.
    for (const [el, a] of Array.from(toolbarAnchors.entries()).reverse()) {
      if (a.next && a.next.parentNode === a.parent) a.parent.insertBefore(el, a.next);
      else a.parent.appendChild(el);
    }
    toolbarAnchors.clear();
    pill.classList.remove('more-open');
    document.getElementById('more-btn')?.classList.remove('active');
  }
  requestAnimationFrame(sizeStage);
}

// Short landscape (phones held sideways) splits the toolbar into two side pills.
const landscapeMql = window.matchMedia('(max-height: 440px) and (orientation: landscape)');

// Compact mode follows the mobile breakpoint — unless the landscape split is
// active, which owns the toolbar layout.
function setupResponsiveToolbar(): void {
  const mql = window.matchMedia(`(max-width: ${MOBILE_BP}px)`);
  const apply = (): void => {
    if (landscapeMql.matches) return;
    applyCompactToolbar(mql.matches);
  };
  apply();
  mql.addEventListener?.('change', apply);
}

// Mobile-landscape: move the individual toolbar buttons into the two floating
// side pills, freeing vertical space. Buttons (not groups) so the pills are a
// clean vertical stack with no leftover separators. DOM-move keeps listeners.
//
// The split is by meaning, not by count: left is what you put *into* the room
// (your camera, mic, screen, which devices they use) plus who you are in it
// with (chat, participants); right is the picture itself — how it is laid out,
// what you measure on it, and how it plays.
//
// That axis happens to balance, which the old split badly did not: it put five
// of the six always-present controls on the right, and both of the buttons that
// disappear outside focus view on the left, so grid view left you with three
// buttons facing eight. The two focus-only controls are now one per side
// (participants left, pointer right) and the two conditional ones (screen share,
// hidden on iOS; scopes, hidden with no source) sit on opposite sides too — so
// the pills stay within one button of each other in every room shape:
//
//              focus  grid   focus/iOS  grid/iOS  grid/iOS/no-scopes
//   left         7      6        6          5             5
//   right        7      6        7          6             5
const LANDSCAPE_LEFT_IDS = [
  'cam-btn', 'mic-btn', 'screen-btn', 'device-btn', // my inputs, and their settings
  'chat-toggle', 'conf-toggle', // the people in here
  'help-btn',
];
const LANDSCAPE_RIGHT_IDS = [
  'focus-btn', 'pointer-btn', 'scopes-btn', // what is on screen, and reading it
  'play-btn', 'mute-btn', 'resync-btn', // how it plays
  'fullscreen-btn',
];
const landscapeAnchors = new Map<Element, ToolbarAnchor>();

function applyLandscapeSplit(active: boolean): void {
  const left = document.getElementById('left-toolbar');
  const right = document.getElementById('right-toolbar');
  if (!left || !right) return;
  if (active) {
    // The split needs every control inline (not collapsed into the ⋯ sheet).
    applyCompactToolbar(false);
    const place = (ids: string[], dest: Element): void => {
      for (const id of ids) {
        const el = document.getElementById(id);
        if (!el) continue;
        if (!landscapeAnchors.has(el)) {
          landscapeAnchors.set(el, { parent: el.parentNode!, next: el.nextSibling });
        }
        dest.appendChild(el);
      }
    };
    place(LANDSCAPE_LEFT_IDS, left);
    place(LANDSCAPE_RIGHT_IDS, right);
  } else if (landscapeAnchors.size) {
    for (const [el, a] of Array.from(landscapeAnchors.entries()).reverse()) {
      if (a.next && a.next.parentNode === a.parent) a.parent.insertBefore(el, a.next);
      else a.parent.appendChild(el);
    }
    landscapeAnchors.clear();
    // Back to portrait/desktop — re-evaluate compact mode for the bottom pill.
    applyCompactToolbar(window.innerWidth <= MOBILE_BP);
  }
  requestAnimationFrame(sizeStage);
}

function setupLandscapeSplit(): void {
  const apply = (): void => applyLandscapeSplit(landscapeMql.matches);
  apply();
  landscapeMql.addEventListener?.('change', apply);
}

export function initLayout(): void {
  document.getElementById('chat-toggle')?.addEventListener('click', toggleChat);
  document.getElementById('chat-close')?.addEventListener('click', () => {
    if (viewerStore.get().chatOpen) toggleChat();
  });
  document.getElementById('conf-toggle')?.addEventListener('click', toggleConf);
  document.querySelectorAll<HTMLElement>('.panel-tab').forEach((btn) => {
    btn.addEventListener('click', () => {
      const tab = btn.dataset['tab'];
      if (tab === 'chat' || tab === 'files') switchPanelTab(tab);
    });
  });

  setupFullscreen();
  setupChromeIdle(); // fades the chips + toolbar when nothing is happening
  setupResponsiveToolbar(); // applies compact mode
  setupLandscapeSplit(); // mobile-landscape: split into side pills
  setupMoreMenu();

  window.addEventListener('resize', sizeStage);
  // iOS animates rotation over ~300ms and reports stale dimensions mid-flight,
  // so re-fit on a delayed cadence after the orientation settles.
  const onRotate = (): void => {
    sizeStage();
    for (const ms of [50, 150, 300, 500]) setTimeout(sizeStage, ms);
  };
  screen.orientation?.addEventListener('change', onRotate);
  window.addEventListener('orientationchange', onRotate);
  if (window.visualViewport) {
    window.visualViewport.addEventListener('resize', sizeStage);
  }
}
