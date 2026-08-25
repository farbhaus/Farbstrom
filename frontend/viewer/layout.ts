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
  // The strip animates its own width (like the chat panel), with the grid column
  // sized `auto` to follow it.
  const stripEl = document.getElementById('stage-strip');
  const focus = document.body.classList.contains('has-focus');
  const desktop = window.innerWidth > MOBILE_BP;
  if (!focus || !desktop) {
    if (panel) panel.style.width = '';
    if (stripEl) stripEl.style.width = '';
    return;
  }
  const tile = focusedTile(stage);
  const mainRow = stage.parentElement;
  if (!tile || !mainRow) {
    if (panel) panel.style.width = '';
    if (stripEl) stripEl.style.width = '';
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
    if (stripEl) stripEl.style.width = '';
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
  // Leave .strip-hidden / the base width to their CSS rules when there's no
  // extra to absorb; otherwise widen the strip inline (the grid `auto` column
  // follows it). Mirrors the chat panel's inline width above.
  if (stripEl) {
    stripEl.style.width = stripHidden || stripExtra === 0 ? '' : `${Math.round(stripBase + stripExtra)}px`;
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
  // Set the inline grid target synchronously (same tick as the class toggle) so
  // the CSS transition has a single, stable destination. Otherwise the first
  // reflow runs a frame late and swaps the target mid-flight — restarting the
  // easing, which makes opening the strip crawl then jump (closing is unaffected
  // because the strip width is cleared while hidden).
  sizeStage();
  const alreadyTicking = panelReflowUntil > performance.now();
  panelReflowUntil = performance.now() + PANEL_TRANSITION_MS;
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
  if (open) document.getElementById('chat-toggle')?.classList.remove('has-notification');
  reflowDuringPanelTransition();
}

export function toggleChat(): void {
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

// The bottom toolbar (#186) is an in-flow, always-visible centered pill at the
// bottom of the #app column, so the stage/tiles size to the space left over. On
// mobile it runs in "compact" mode: the secondary controls move into the ⋯ popup
// so the pill stays a single row.
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
const LANDSCAPE_LEFT_IDS = ['cam-btn', 'mic-btn', 'screen-btn', 'pointer-btn', 'focus-btn', 'conf-toggle'];
const LANDSCAPE_RIGHT_IDS = ['chat-toggle', 'play-btn', 'mute-btn', 'scopes-btn', 'device-btn', 'resync-btn', 'fullscreen-btn', 'help-btn'];
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
