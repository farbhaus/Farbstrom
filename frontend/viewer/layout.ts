// Panel toggles, stage grid sizing, fullscreen.
// Mobile-Safari resize listeners (visualViewport) live here too.

import { getPlayer } from './player.js';
import { viewerStore } from './state.js';

// Compute the optimal column count for the stage grid so 16:9 tiles fill
// the container as efficiently as possible without overflowing. Skipped
// when the stage is in focus sub-layout (CSS handles that case).
// In focus mode the pinned stream/share tile is constrained to the content
// aspect ratio (--focus-aspect, set by conference.updateFocusAspect). CSS
// can't know whether the grid cell is wider or taller than that ratio, so
// pick the limiting axis here: choose which dimension is 100% and let the
// other follow aspect-ratio. Without this the tile letterboxes (or
// pillarboxes) inside its own cell instead of hugging the image.
function pxList(s: string): number[] {
  return s
    .split(' ')
    .map((v) => parseFloat(v))
    .filter((v) => !Number.isNaN(v));
}

function readFocusAspect(tile: HTMLElement): number {
  const raw = getComputedStyle(tile).getPropertyValue('--focus-aspect').trim();
  if (raw) {
    const [a, b] = raw.split('/').map((v) => parseFloat(v));
    if (a && b && a > 0 && b > 0) return a / b;
  }
  return 16 / 9;
}

function focusedTile(stage: HTMLElement): HTMLElement | null {
  return stage.querySelector<HTMLElement>(
    '#tile-stream[data-focused], #tile-share[data-focused]',
  );
}

function fitFocusedTile(stage: HTMLElement): void {
  const tile = focusedTile(stage);
  if (!tile) return;
  const cs = getComputedStyle(stage);
  const cols = pxList(cs.gridTemplateColumns);
  const rows = pxList(cs.gridTemplateRows);
  // The focused tile sits in the last column (desktop: strip | tile) and
  // last row (mobile: strip row, then tile row).
  const cellW = cols.length ? cols[cols.length - 1]! : stage.clientWidth;
  const cellH = rows.length ? rows[rows.length - 1]! : stage.clientHeight;
  if (!(cellW > 0) || !(cellH > 0)) return;
  const contentAspect = readFocusAspect(tile);
  const widthLimited = cellW / cellH < contentAspect;
  tile.classList.toggle('focus-fit-w', widthLimited);
  tile.classList.toggle('focus-fit-h', !widthLimited);
}

// In focus mode the player is height-limited and centres in its grid cell,
// leaving grey leftover space on either side. Absorb that leftover into
// the chat panel and the side strip (issue #125 originally for chat;
// issue #151 extends it to the strip) so the gap between player and the
// neighbouring panels stays constant.
//
// Strategy: chat absorbs first up to --panel-w-max, then any residual
// goes to the strip up to --strip-w-max.
//
// Target widths are computed from #main-row dimensions plus the discrete
// --strip-w / .strip-hidden state — NOT from computed grid columns,
// because those return interpolated values during the strip's CSS
// transition. Reading interpolated values caused the chat target to
// chase a moving cellW each tick, restarting chat's own width transition
// and making the player resize mid-animation.
// Mobile breakpoint. Must stay in sync with the `@media (max-width: 700px)`
// rules in www/viewer/index.html (and the shared 700px breakpoint documented in
// www/shared/tokens.css).
const MOBILE_BP = 700;
const STAGE_PAD = 16; // 8px on each side
const COL_GAP = 8;
const CHAT_MARGIN_R = 8;
function sizeFocusPanels(stage: HTMLElement): void {
  const panel = document.getElementById('right-panel');
  const focus = document.body.classList.contains('has-focus');
  const desktop = window.innerWidth > MOBILE_BP;
  if (!focus || !desktop) {
    if (panel) panel.style.width = '';
    stage.style.gridTemplateColumns = '';
    return;
  }
  const tile = focusedTile(stage);
  const mainRow = stage.parentElement;
  if (!tile || !mainRow) {
    if (panel) panel.style.width = '';
    stage.style.gridTemplateColumns = '';
    return;
  }

  const open = !!panel?.classList.contains('open');
  const root = getComputedStyle(document.documentElement);
  const chatBase = parseFloat(root.getPropertyValue('--panel-w')) || 320;
  const chatMax = parseFloat(root.getPropertyValue('--panel-w-max')) || 560;
  const stripBase = parseFloat(root.getPropertyValue('--strip-w')) || 220;
  const stripMin = parseFloat(root.getPropertyValue('--strip-w-min')) || 180;
  const stripMax = parseFloat(root.getPropertyValue('--strip-w-max')) || 360;
  const stripHidden = document.body.classList.contains('strip-hidden');
  // When the strip is hidden the CSS also collapses the column gap to 0
  // (see `body.has-focus.strip-hidden #stage`), so drop it here too.
  const strip = stripHidden ? 0 : stripBase;
  const colGap = stripHidden ? 0 : COL_GAP;
  // When chat is closed the right panel collapses out of flow.
  const chatW = open ? chatBase : 0;
  const chatMarginR = open ? CHAT_MARGIN_R : 0;

  // Final cell width assuming both panels sit at their base widths —
  // independent of any in-flight transitions.
  const mainW = mainRow.clientWidth;
  const finalCellW = mainW - chatW - chatMarginR - STAGE_PAD - strip - colGap;
  // Stage vertical sizing isn't affected by horizontal transitions, so
  // clientHeight is stable.
  const cellH = stage.clientHeight - STAGE_PAD;
  if (!(finalCellW > 0) || !(cellH > 0)) {
    if (panel) panel.style.width = '';
    stage.style.gridTemplateColumns = '';
    return;
  }

  const aspect = readFocusAspect(tile);
  const playerW = cellH * aspect;
  // Signed leftover relative to bases. Positive = room to grow; negative =
  // we need to *narrow* the strip below its base to keep the tile at full
  // height.
  const leftover = finalCellW - playerW;

  let chatExtra = 0;
  let stripExtra = 0;
  if (leftover >= 0) {
    // Plenty of room: chat absorbs first, strip absorbs residual.
    chatExtra = open ? Math.min(chatMax - chatBase, leftover) : 0;
    stripExtra = stripHidden
      ? 0
      : Math.min(stripMax - stripBase, leftover - chatExtra);
  } else if (!stripHidden) {
    // Tight: chat stays at its base; auto-narrow the strip (down to
    // --strip-w-min) so the player tile keeps its height-limited size.
    stripExtra = Math.max(-(stripBase - stripMin), leftover);
  }

  if (panel) {
    panel.style.width = open ? `${Math.round(chatBase + chatExtra)}px` : '';
  }
  // Leave .strip-hidden to its CSS rule (collapses to `0 1fr`).
  if (stripHidden || stripExtra === 0) {
    stage.style.gridTemplateColumns = '';
  } else {
    stage.style.gridTemplateColumns = `${Math.round(stripBase + stripExtra)}px 1fr`;
  }
}

export function sizeStage(): void {
  const stage = document.getElementById('stage');
  if (!stage) return;
  if (document.body.classList.contains('has-focus')) {
    // Grid mode sets an inline grid-template-columns (repeat(...)); an inline
    // style outranks the focus-mode stylesheet rule, so leaving it set would
    // override `var(--strip-w) 1fr` and make the strip + focused tile collide.
    // Clear it so the focus / strip-hidden CSS governs (grid mode re-sets it).
    stage.style.gridTemplateColumns = '';
    sizeFocusPanels(stage);
    fitFocusedTile(stage);
    return;
  }
  sizeFocusPanels(stage);
  // Visible tiles only — hidden #tile-stream / #tile-share don't take grid cells.
  const tiles = Array.from(stage.querySelectorAll<HTMLElement>(':scope > .tile')).filter(
    (el) => !el.classList.contains('hidden') && el.offsetParent !== null,
  );
  const n = tiles.length;
  if (n === 0) {
    stage.style.gridTemplateColumns = '';
    return;
  }
  const gap = 8;
  const pad = 16;
  const cw = stage.clientWidth - pad;
  const ch = stage.clientHeight - pad;
  const RATIO = 16 / 9;
  let bestCols = 1;
  let bestArea = 0;
  let bestTileW = cw;
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
    }
  }
  const colW = (cw - gap * (bestCols - 1)) / bestCols;
  if (bestTileW < colW - 1) {
    stage.style.gridTemplateColumns = `repeat(${bestCols}, ${Math.floor(bestTileW)}px)`;
  } else {
    stage.style.gridTemplateColumns = `repeat(${bestCols}, 1fr)`;
  }
}

// The chat panel / focus strip animate their width over ~0.25s. sizeStage()
// (and its fitFocusedTile axis pick) must be re-run *every frame* through that
// window, not at a few discrete points — otherwise the wrong limiting axis
// stays active between ticks and max-width/height clamps the focused tile to a
// non-16:9 box, the letterbox/pillarbox that "sticks" to the moving cell until
// the next tick. sizeFocusPanels() computes its targets from stable #main-row
// dims, so re-asserting them each frame doesn't restart the panel's own
// transition.
const PANEL_TRANSITION_MS = 320; // a touch over the 0.25s CSS transition
let panelReflowUntil = 0;
function reflowDuringPanelTransition(): void {
  const alreadyTicking = panelReflowUntil > performance.now();
  panelReflowUntil = performance.now() + PANEL_TRANSITION_MS;
  if (alreadyTicking) return; // running loop will honour the extended deadline
  const tick = (): void => {
    sizeStage();
    if (performance.now() < panelReflowUntil) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

export function toggleChat(): void {
  const next = !viewerStore.get().chatOpen;
  viewerStore.set({ chatOpen: next });
  document.getElementById('right-panel')?.classList.toggle('open', next);
  document.getElementById('chat-toggle')?.classList.toggle('panel-open', next);
  if (next) document.getElementById('chat-toggle')?.classList.remove('has-notification');
  // Refresh the pill: on mobile it hides while chat is open and reappears when
  // chat closes.
  revealPill();
  reflowDuringPanelTransition();
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

export function toggleConf(): void {
  // In the unified stage model, the "strip" is the focus-mode side strip.
  // The Participants toggle button now shows/hides that strip; when not in
  // focus mode it's a no-op (the CSS hides the button via body.no-strip).
  const next = !viewerStore.get().confOpen;
  viewerStore.set({ confOpen: next });
  document.body.classList.toggle('strip-hidden', !next);
  document.getElementById('conf-toggle')?.classList.toggle('panel-open', next);
  reflowDuringPanelTransition();
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
    // Fullscreen the focused tile if there is one, otherwise the whole stage.
    const focused = document.querySelector<HTMLElement>('#stage > .tile[data-focused]');
    const target = focused || document.getElementById('stage');
    if (!target) return;
    if (target.requestFullscreen) {
      void target.requestFullscreen();
    } else if (target.webkitRequestFullscreen) {
      target.webkitRequestFullscreen();
    } else {
      // iPhone: only the <video> can go fullscreen, and exiting it doesn't
      // fire document fullscreenchange — resume on its own end event.
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
    // Layout/aspect can change coming out of fullscreen — re-fit the tile.
    requestAnimationFrame(sizeStage);
  };
  document.addEventListener('fullscreenchange', onFsChange);
  document.addEventListener('webkitfullscreenchange', onFsChange);
}

// The bottom toolbar is a floating "disappearing" pill (#186). On desktop it
// behaves like a video player's controls: it appears only when the mouse
// approaches the bottom edge (the hot zone) and tucks away otherwise, so moving
// or clicking mid-video — e.g. while using the pointer tool or chatting — never
// makes it flicker. On touch a tap toggles it. Hiding is suppressed while the
// user is interacting with the pill (hover / focus inside it), the ⋯ sheet is
// open, or the chat panel is open. CSS (#bottom-toolbar.toolbar-hidden) fades it.
const PILL_IDLE_MS = 3000;
// Reveal only when the mouse is within this band of the viewport bottom (where
// the pill lives), so moving/clicking mid-video never summons it.
const PILL_HOTZONE_PX = 140;
// How quickly it tucks away once the cursor leaves the bottom band.
const PILL_LEAVE_MS = 600;
let pillHideTimer: ReturnType<typeof setTimeout> | null = null;

function pillEl(): HTMLElement | null {
  return document.getElementById('bottom-toolbar');
}

// On mobile the chat panel is a full-width overlay, so a visible pill would sit
// on top of the chat composer. Keep the pill hidden while chat is open there;
// chat is closed via its own header button.
function pillSuppressed(): boolean {
  return viewerStore.get().chatOpen && window.innerWidth <= MOBILE_BP;
}

// Keep the pill visible while the pointer is over it, keyboard focus is inside
// it, the ⋯ More sheet is open, or (on desktop) the chat panel is open.
function pillPinned(pill: HTMLElement): boolean {
  if (pill.matches(':hover')) return true;
  if (pill.contains(document.activeElement)) return true;
  if (pill.classList.contains('more-open')) return true;
  if (viewerStore.get().chatOpen && window.innerWidth > MOBILE_BP) return true;
  return false;
}

// The ⋯ More button (mobile) toggles the secondary-controls sheet above the
// pill. (Tapping outside the pill closes it — handled in setupPillAutohide.)
function setupMoreMenu(): void {
  const pill = pillEl();
  const moreBtn = document.getElementById('more-btn');
  if (!pill || !moreBtn) return;
  moreBtn.addEventListener('click', (e) => {
    e.stopPropagation();
    const open = pill.classList.toggle('more-open');
    moreBtn.classList.toggle('active', open);
    revealPill();
  });
}

// On mobile the secondary controls move out of the pill into #more-sheet (the
// ⋯ popup) so the pill stays a slim single row of essentials; on desktop they
// return to their original inline positions (preserving the full toolbar
// order). DOM-move (not clone) keeps their event listeners intact.
const TOOLBAR_EXTRA_IDS = ['focus-btn', 'conf-toggle', 'player-controls', 'device-btn', 'resync-btn'];
type ToolbarAnchor = { parent: Node; next: Node | null };
const toolbarAnchors = new Map<Element, ToolbarAnchor>();

function applyMobileToolbar(mobile: boolean): void {
  const sheet = document.getElementById('more-sheet');
  if (!sheet) return;
  if (mobile) {
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
    pillEl()?.classList.remove('more-open');
    document.getElementById('more-btn')?.classList.remove('active');
  }
  requestAnimationFrame(sizeStage);
}

function setupResponsiveToolbar(): void {
  const mql = window.matchMedia(`(max-width: ${MOBILE_BP}px)`);
  const apply = (): void => applyMobileToolbar(mql.matches);
  apply();
  mql.addEventListener?.('change', apply);
}

// Collapse the pill (and the ⋯ sheet) immediately — used on a touch outside tap.
function hidePill(): void {
  const pill = pillEl();
  if (!pill) return;
  if (pillHideTimer) clearTimeout(pillHideTimer);
  pill.classList.remove('more-open');
  document.getElementById('more-btn')?.classList.remove('active');
  pill.classList.add('toolbar-hidden');
}

// Hide after `delay`, unless the pill is still being used (re-arm at idle pace).
function armHide(delay: number): void {
  if (pillHideTimer) clearTimeout(pillHideTimer);
  pillHideTimer = setTimeout(() => {
    const p = pillEl();
    if (!p) return;
    if (pillPinned(p)) armHide(PILL_IDLE_MS); // still busy — keep it up
    else p.classList.add('toolbar-hidden');
  }, delay);
}

function revealPill(): void {
  const pill = pillEl();
  if (!pill) return;
  if (pillHideTimer) clearTimeout(pillHideTimer);
  if (pillSuppressed()) {
    pill.classList.add('toolbar-hidden');
    return;
  }
  pill.classList.remove('toolbar-hidden');
  armHide(PILL_IDLE_MS);
}

function setupPillAutohide(): void {
  // Desktop: reveal only when the cursor is near the bottom edge (where the pill
  // lives); leaving that band tucks it away. Mid-video movement never reveals it.
  document.addEventListener(
    'pointermove',
    (e) => {
      if (e.pointerType !== 'mouse') return;
      const pill = pillEl();
      if (!pill) return;
      const inZone = e.clientY >= window.innerHeight - PILL_HOTZONE_PX;
      if (inZone) revealPill();
      else if (!pill.classList.contains('toolbar-hidden') && !pillPinned(pill)) {
        armHide(PILL_LEAVE_MS);
      }
    },
    { passive: true },
  );
  // Keyboard shortcuts reveal the pill — but not while typing in a field.
  document.addEventListener('keydown', (e) => {
    const t = e.target as HTMLElement | null;
    if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
    revealPill();
  });
  // Touch: a tap toggles the pill (inside keeps it up; outside hides, or reveals
  // when hidden). Skipped while the pointer tool is active so annotation taps
  // pass through to the overlay. Mouse taps do nothing — the hot zone governs.
  document.addEventListener(
    'pointerdown',
    (e) => {
      if (e.pointerType === 'mouse') return;
      if (viewerStore.get().pointerMode) return;
      const pill = pillEl();
      if (!pill) return;
      if (pill.contains(e.target as Node) || pill.classList.contains('toolbar-hidden')) {
        revealPill();
      } else {
        hidePill();
      }
    },
    { passive: true },
  );
  // Re-arm the idle countdown when the pointer leaves the pill.
  pillEl()?.addEventListener('mouseleave', () => revealPill());
  revealPill(); // start visible, begin the idle countdown
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
  setupResponsiveToolbar();
  setupPillAutohide();
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
