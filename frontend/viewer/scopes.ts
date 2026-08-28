// Scopes window (#229) — a floating, draggable, resizable video scope for the
// stream tile. One scope visible at a time, its drawing area locked to 16:9.
//
// Owns the window chrome, one shared per-frame sample, and the render loop.
//
// SAMPLING PROVENANCE — the caveat that shapes what these numbers mean.
// Pixels come from a 2D-canvas readback, which is the browser's *tone-mapped
// display output*, not the source signal. For an SDR stream that is the signal,
// which is why the scopes read it as Rec.709 / IRE and say nothing further. It
// would NOT be true of an HDR stream: PQ highlights are compressed into an SDR
// buffer before we see a pixel, so absolute nits are not measurable this way.
// scope-draw.ts is still parameterised by working space and scale (including PQ
// and ARRI LogC) and scope-color.ts still carries the maths — none of it is
// reachable from the UI today. Anything re-exposing those controls has to
// restore the provenance labelling with them; see CLAUDE.md.

import { getPlayerMode } from './player.js';
import type { Scale, WorkingSpace } from './scope-color.js';
import {
  drawLumaWaveform,
  drawRgbParade,
  drawVectorscope,
  releaseScopeBuffers,
  type DrawOpts,
  type ScopeFrame,
} from './scope-draw.js';
import { viewerStore } from './state.js';

type ScopeId = 'luma' | 'parade' | 'vector';

// Fixed while the working-space / scale selectors are out of the UI.
const SPACE: WorkingSpace = 'rec709';
const SCALE: Scale = 'sdr';

interface PanelDef {
  id: ScopeId;
  draw: (ctx: CanvasRenderingContext2D, f: ScopeFrame, o: DrawOpts) => void;
}

// Histogram and false colour are built and tested but deliberately not offered
// — see drawHistogram / drawFalseColor in scope-draw.ts. Re-listing them here
// is all it takes to bring them back.
const PANELS: readonly PanelDef[] = [
  { id: 'luma', draw: drawLumaWaveform },
  { id: 'parade', draw: drawRgbParade },
  { id: 'vector', draw: drawVectorscope },
];

const PANEL_IDS = new Set<string>(PANELS.map((p) => p.id));

// ---- Preferences -----------------------------------------------------------

// Not slug-namespaced on purpose: which scope you like and where you park the
// window is a per-user tool preference, not per-room session state (contrast
// the slug-scoped keys in session.ts).
const PREFS_KEY = 'viewer_scopes';

interface Prefs {
  open: boolean;
  scope: ScopeId;
  zoom: number;
  // Sampling resolution divisor: 1 = full, 2 = half, 4 = quarter.
  res: number;
  x: number;
  y: number;
  w: number;
  h: number;
}

const MIN_W = 260;
const MAX_ZOOM = 16;
const ZOOM_STOPS = [1, 2, 4, 8, 16];
// Halving the sampling width quarters the pixel count, which cuts the readback,
// the normalise pass and the per-pixel accumulation proportionally.
//
// It does NOT quarter the total: blitting the trace and stroking the graticule
// walk the whole canvas whatever the sample size, so there is a fixed floor.
// Measured on the JS side at a 920x518 panel, dpr 2 — 1:2 is 1.6–2.3x faster
// and 1:4 is 2.1–3.7x, depending on the scope. Real, not the 4x/16x that pixel
// count alone would suggest.
const RES_STEPS = [1, 2, 4];
// Fallback only, for the one call that can happen while the window is still
// display:none and reports no height.
const HEADER_FALLBACK = 34;
const PANEL_ASPECT = 16 / 9;

function defaultGeometry(): { x: number; y: number; w: number } {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  // Mirrors the 700px / 440px-landscape breakpoints in the viewer stylesheet.
  if (vh <= 440 && vw > vh) return { x: vw - 372, y: 10, w: 300 };
  if (vw <= 700) return { x: 8, y: Math.round(vh * 0.12), w: vw - 16 };
  return { x: vw - 552, y: vh - 420, w: 520 };
}

let prefs: Prefs = loadPrefs();

function loadPrefs(): Prefs {
  const geo = defaultGeometry();
  const fallback: Prefs = { open: false, scope: 'luma', zoom: 1, res: 1, ...geo, h: 0 };
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return fallback;
    const p = JSON.parse(raw) as Partial<Prefs> & { scopes?: unknown };
    // Older builds stored a multi-select `scopes` array and could hold scopes
    // that are no longer offered; take the first still-valid entry.
    let scope: ScopeId = 'luma';
    if (typeof p.scope === 'string' && PANEL_IDS.has(p.scope)) {
      scope = p.scope as ScopeId;
    } else if (Array.isArray(p.scopes)) {
      const first = (p.scopes as unknown[]).find(
        (s): s is ScopeId => typeof s === 'string' && PANEL_IDS.has(s),
      );
      if (first) scope = first;
    }
    return {
      open: !!p.open,
      scope,
      zoom: typeof p.zoom === 'number' && p.zoom >= 1 && p.zoom <= MAX_ZOOM ? p.zoom : 1,
      res: typeof p.res === 'number' && RES_STEPS.includes(p.res) ? p.res : 1,
      x: typeof p.x === 'number' ? p.x : geo.x,
      y: typeof p.y === 'number' ? p.y : geo.y,
      w: typeof p.w === 'number' ? p.w : geo.w,
      h: 0,
    };
  } catch {
    return fallback;
  }
}

function savePrefs(): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {}
}

// ---- Elements --------------------------------------------------------------

let win: HTMLElement | null = null;
let body: HTMLElement | null = null;
let headEl: HTMLElement | null = null;
let canvas: HTMLCanvasElement | null = null;

function el(id: string): HTMLElement | null {
  return document.getElementById(id);
}

// Height follows width so the drawing area is always 16:9, and the whole thing
// is clamped into the viewport — a window saved on a large monitor must not
// open off-screen on a phone, and the header has to stay grabbable.
function fitGeometry(): void {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const headH = headEl?.offsetHeight || HEADER_FALLBACK;
  let w = Math.max(MIN_W, Math.min(prefs.w, vw - 8));
  let h = headH + w / PANEL_ASPECT;
  const maxH = vh - 8;
  if (h > maxH) {
    h = maxH;
    w = Math.max(MIN_W, (h - headH) * PANEL_ASPECT);
  }
  prefs.w = Math.round(w);
  prefs.h = Math.round(h);
  prefs.x = Math.max(4, Math.min(prefs.x, vw - prefs.w - 4));
  prefs.y = Math.max(4, Math.min(prefs.y, vh - prefs.h - 4));
}

// ---- Sampling --------------------------------------------------------------

const SAMPLE_W_FULL = 480;
const SCOPE_FPS = 15;
const FRAME_MS = 1000 / SCOPE_FPS;

let sampleCanvas: HTMLCanvasElement | null = null;
let sampleCtx: CanvasRenderingContext2D | null = null;
let frame: ScopeFrame | null = null;
// Set once a getImageData throws SecurityError, so we stop retrying every frame.
let readBlocked = false;

function ensureSampler(w: number, h: number): boolean {
  if (!sampleCanvas) {
    sampleCanvas = document.createElement('canvas');
    sampleCtx = sampleCanvas.getContext('2d', { willReadFrequently: true });
  }
  if (!sampleCtx) return false;
  if (sampleCanvas.width !== w || sampleCanvas.height !== h) {
    sampleCanvas.width = w;
    sampleCanvas.height = h;
  }
  if (!frame || frame.w !== w || frame.h !== h) {
    frame = { data: new Float32Array(w * h * 3), w, h };
  }
  return true;
}

type Source = HTMLVideoElement | HTMLImageElement;

// The stream tile's content. Reuses getPlayerMode() rather than re-deriving
// player state; OvenPlayer can briefly hold two <video>s across a source swap,
// hence picking the first one that actually has dimensions.
function resolveSource(): Source | null {
  const mode = getPlayerMode();
  if (mode === 'live' || mode === 'file') {
    const vids = document.querySelectorAll<HTMLVideoElement>('#player video');
    for (const v of Array.from(vids)) {
      if (v.videoWidth > 0 && v.videoHeight > 0 && v.readyState >= 2) return v;
    }
    return null;
  }
  if (mode === 'image') {
    const img = el('display-img') as HTMLImageElement | null;
    if (img && img.naturalWidth > 0 && img.style.display !== 'none') return img;
  }
  return null;
}

function sourceSize(src: Source): [number, number] {
  return src instanceof HTMLVideoElement
    ? [src.videoWidth, src.videoHeight]
    : [src.naturalWidth, src.naturalHeight];
}

function sample(src: Source): ScopeFrame | null {
  if (readBlocked) return null;
  const [sw, sh] = sourceSize(src);
  if (!(sw > 0) || !(sh > 0)) return null;
  // The scaling happens in drawImage below, so a lower setting means the
  // readback and every per-pixel loop downstream get proportionally smaller —
  // it is not a cosmetic downsample after the fact.
  const w = Math.max(16, Math.min(Math.round(SAMPLE_W_FULL / prefs.res), sw));
  const h = Math.max(2, Math.round(w * (sh / sw)));
  if (!ensureSampler(w, h) || !sampleCtx || !frame) return null;

  try {
    sampleCtx.drawImage(src, 0, 0, w, h);
  } catch {
    return null;
  }

  let img: ImageData;
  try {
    img = sampleCtx.getImageData(0, 0, w, h);
  } catch (err) {
    // Every source feeding the stream tile is same-origin (MediaStream for
    // WebRTC, same-origin MSE for LL-HLS, the same-origin file API for
    // presenter media), so this should never fire — but a tainted canvas
    // throws on every single frame, so latch it rather than loop on it.
    if (err instanceof DOMException && err.name === 'SecurityError') readBlocked = true;
    return null;
  }

  const d = img.data;
  const out = frame.data;
  for (let p = 0, j = 0; j < out.length; p += 4, j += 3) {
    out[j] = d[p]! / 255;
    out[j + 1] = d[p + 1]! / 255;
    out[j + 2] = d[p + 2]! / 255;
  }
  return frame;
}

// ---- Render loop -----------------------------------------------------------

let rafId = 0;
let vfcId = 0;
let idleTimer = 0;
let vfcTarget: HTMLVideoElement | null = null;
let lastDrawAt = 0;
// Bumped on every stop so a callback already queued from a previous chain
// can't resurrect it. Without this, each entry point into the loop (opening
// the window, switching scope, returning from a hidden tab) would start a
// second chain alongside the first and the two would compound.
let generation = 0;

function running(): boolean {
  return prefs.open && available && !document.hidden;
}

function stopLoop(): void {
  generation++;
  if (rafId) cancelAnimationFrame(rafId);
  rafId = 0;
  if (idleTimer) clearTimeout(idleTimer);
  idleTimer = 0;
  if (vfcId && vfcTarget?.cancelVideoFrameCallback) {
    try {
      vfcTarget.cancelVideoFrameCallback(vfcId);
    } catch {}
  }
  vfcId = 0;
  vfcTarget = null;
}

// The single way to (re)start rendering — every caller goes through here, so
// there is only ever one live chain.
function startLoop(): void {
  stopLoop();
  if (!running()) return;
  lastDrawAt = 0;
  tick(generation);
}

function scheduleNext(g: number, src: Source | null): void {
  if (g !== generation || !running()) return;
  // requestVideoFrameCallback fires once per decoded frame, which is both
  // cheaper and better-aligned than polling; Firefox has no such thing.
  if (src instanceof HTMLVideoElement && typeof src.requestVideoFrameCallback === 'function') {
    vfcTarget = src;
    vfcId = src.requestVideoFrameCallback(() => {
      vfcId = 0;
      tick(g);
    });
    return;
  }
  rafId = requestAnimationFrame(() => {
    rafId = 0;
    tick(g);
  });
}

function tick(g: number): void {
  if (g !== generation || !running()) return;
  const src = resolveSource();
  win?.classList.toggle('no-source', !src);
  if (!src) {
    // Nothing to measure yet — keep a slow heartbeat so the scope lights up on
    // its own when the stream arrives.
    idleTimer = window.setTimeout(() => {
      idleTimer = 0;
      tick(g);
    }, 500);
    return;
  }

  const now = performance.now();
  if (now - lastDrawAt >= FRAME_MS) {
    lastDrawAt = now;
    const f = sample(src);
    if (f) draw(f);
  }
  scheduleNext(g, src);
}

function draw(f: ScopeFrame): void {
  if (!canvas) return;
  const def = PANELS.find((p) => p.id === prefs.scope);
  if (!def) return;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const cw = Math.max(1, Math.round(canvas.clientWidth * dpr));
  const ch = Math.max(1, Math.round(canvas.clientHeight * dpr));
  if (canvas.width !== cw || canvas.height !== ch) {
    canvas.width = cw;
    canvas.height = ch;
  }
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  def.draw(ctx, f, {
    space: SPACE,
    scale: SCALE,
    approxNits: true,
    gamutWarn: false,
    dpr,
    zoom: prefs.zoom,
  });
}

// ---- DOM sync --------------------------------------------------------------

function fmtZoom(z: number): string {
  const r = Math.round(z * 10) / 10;
  return Number.isInteger(r) ? String(r) : r.toFixed(1);
}

function syncControls(): void {
  document.querySelectorAll<HTMLElement>('.scope-chip[data-scope]').forEach((chip) => {
    chip.classList.toggle('is-active', chip.dataset['scope'] === prefs.scope);
  });
  // The zoom control only means anything for the vectorscope.
  win?.classList.toggle('is-vector', prefs.scope === 'vector');
  const label = el('scopes-zoom-label');
  if (label) label.textContent = `${fmtZoom(prefs.zoom)}×`;
  const res = el('scopes-res');
  if (res) {
    res.textContent = `1:${prefs.res}`;
    res.title =
      prefs.res === 1
        ? 'Sampling resolution: full. Click for half — coarser trace, roughly half the CPU per frame.'
        : `Sampling resolution: 1:${prefs.res} — coarser trace, less CPU. Click to cycle.`;
  }
}

function cycleRes(): void {
  const i = RES_STEPS.indexOf(prefs.res);
  prefs.res = RES_STEPS[(i + 1) % RES_STEPS.length]!;
  savePrefs();
  syncControls();
  lastDrawAt = 0;
}

function applyGeometry(): void {
  if (!win) return;
  fitGeometry();
  win.style.left = `${prefs.x}px`;
  win.style.top = `${prefs.y}px`;
  win.style.width = `${prefs.w}px`;
  win.style.height = `${prefs.h}px`;
  // The window resizes independently of the viewport, so its own width decides
  // whether the header controls still fit.
  win.classList.toggle('narrow', prefs.w < 380);
}

function setZoom(z: number): void {
  const next = Math.min(MAX_ZOOM, Math.max(1, z));
  if (next === prefs.zoom) return;
  prefs.zoom = next;
  syncControls();
  savePrefs();
  lastDrawAt = 0;
}

function stepZoom(dir: 1 | -1): void {
  const stops = dir === 1 ? ZOOM_STOPS : [...ZOOM_STOPS].reverse();
  const next = stops.find((s) => (dir === 1 ? s > prefs.zoom + 1e-6 : s < prefs.zoom - 1e-6));
  setZoom(next ?? prefs.zoom);
}

// ---- Availability ----------------------------------------------------------

// Scopes measure whatever the stage tile is showing, so they are offered
// exactly when this browser has something to sample: a broadcast it plays
// itself, or a presenter-displayed file. A call-only room — no stream key —
// has neither, so the button is hidden until one is attached (#246). App-only
// (SRT) delivery is the same case for the same reason: that broadcast plays in
// Farbplay and no frame of it ever reaches this browser. Mirrors the tile's own
// visibility rule in player.ts (updateStageVisibility).
export function scopesAvailable(): boolean {
  const { streamKey, deliveryMode, displayFile } = viewerStore.get();
  return (!!streamKey && deliveryMode !== 'srt') || !!displayFile;
}

let available = false;

// Availability is separate from `prefs.open` on purpose: `open` is what the
// user asked for and persists across rooms, so a call-only room parks the
// window rather than clearing the preference — attach a stream key and it
// comes back open.
function syncAvailability(): void {
  const next = scopesAvailable();
  document.body.classList.toggle('no-scopes', !next);
  if (next === available) return;
  available = next;
  applyOpenState();
}

// ---- Open / close ----------------------------------------------------------

function applyOpenState(): void {
  const showing = prefs.open && available;
  win?.classList.toggle('hidden', !showing);
  el('scopes-btn')?.classList.toggle('panel-open', showing);
  if (showing) {
    // Order matters: the window has to be visible before fitGeometry can read
    // a real header height.
    applyGeometry();
    syncControls();
    startLoop();
  } else {
    stopLoop();
    releaseScopeBuffers();
  }
}

function setOpen(open: boolean): void {
  prefs.open = open;
  savePrefs();
  applyOpenState();
}

export function closeScopes(): void {
  if (prefs.open) setOpen(false);
  else stopLoop();
}

// ---- Drag + resize ---------------------------------------------------------

// Pointer Events cover mouse, pen and touch in one path, which is why there is
// no separate touch handling here.
function initDragResize(): void {
  const grip = el('scopes-grip');
  if (!win || !headEl || !grip) return;
  const head = headEl;

  let mode: 'move' | 'size' | null = null;
  let startX = 0;
  let startY = 0;
  let baseX = 0;
  let baseY = 0;
  let baseW = 0;

  const begin = (e: PointerEvent, m: 'move' | 'size'): void => {
    mode = m;
    startX = e.clientX;
    startY = e.clientY;
    baseX = prefs.x;
    baseY = prefs.y;
    baseW = prefs.w;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    e.preventDefault();
  };

  head.addEventListener('pointerdown', (e) => {
    // Let the controls in the header work normally.
    if ((e.target as HTMLElement).closest('button, select, input')) return;
    begin(e, 'move');
  });
  grip.addEventListener('pointerdown', (e) => begin(e, 'size'));

  const move = (e: PointerEvent): void => {
    if (!mode) return;
    if (mode === 'move') {
      prefs.x = baseX + (e.clientX - startX);
      prefs.y = baseY + (e.clientY - startY);
    } else {
      // Width only — height follows from the 16:9 lock in fitGeometry(), so the
      // grip tracks the horizontal drag and the box keeps its shape.
      prefs.w = baseW + (e.clientX - startX);
    }
    applyGeometry();
  };
  const end = (): void => {
    if (!mode) return;
    mode = null;
    savePrefs();
  };

  for (const target of [head, grip]) {
    target.addEventListener('pointermove', move);
    target.addEventListener('pointerup', end);
    target.addEventListener('pointercancel', end);
  }
}

// ---- Init ------------------------------------------------------------------

export function initScopes(): void {
  win = el('scopes-window');
  body = el('scopes-body');
  headEl = el('scopes-head');
  canvas = el('scopes-canvas') as HTMLCanvasElement | null;
  if (!win || !body || !canvas) {
    // No window in the DOM means the button would be inert; hide it rather
    // than offer a control that does nothing.
    document.body.classList.add('no-scopes');
    return;
  }

  el('scopes-btn')?.addEventListener('click', () => setOpen(!prefs.open));
  el('scopes-close')?.addEventListener('click', () => setOpen(false));

  win.addEventListener('click', (e) => {
    const target = e.target as HTMLElement;
    const zoomBtn = target.closest<HTMLElement>('[data-zoom]');
    if (zoomBtn) {
      stepZoom(zoomBtn.dataset['zoom'] === 'in' ? 1 : -1);
      return;
    }
    if (target.closest('#scopes-res')) {
      cycleRes();
      return;
    }
    const chip = target.closest<HTMLElement>('.scope-chip[data-scope]');
    if (!chip) return;
    const id = chip.dataset['scope'];
    if (!id || !PANEL_IDS.has(id) || id === prefs.scope) return;
    prefs.scope = id as ScopeId;
    savePrefs();
    syncControls();
    startLoop();
  });

  // Wheel over the vectorscope zooms continuously, which is faster than
  // stepping for finding a skin-tone cluster. Non-passive so the page behind
  // doesn't scroll with it.
  body.addEventListener(
    'wheel',
    (e) => {
      if (prefs.scope !== 'vector') return;
      e.preventDefault();
      setZoom(prefs.zoom * Math.exp(-e.deltaY * 0.002));
    },
    { passive: false },
  );

  initDragResize();

  // A viewport change can force a re-clamp. Persist on a debounce — a browser
  // window drag fires `resize` continuously, and one localStorage write per
  // event is pointless churn. (Dragging the scopes window itself doesn't come
  // through here; that path saves once on pointerup.)
  let saveTimer = 0;
  window.addEventListener('resize', () => {
    applyGeometry();
    clearTimeout(saveTimer);
    saveTimer = window.setTimeout(savePrefs, 300);
  });
  const onRotate = (): void => {
    applyGeometry();
    // iOS animates rotation over ~300ms and reports stale dimensions mid-flight
    // — same cadence as sizeStage()'s rotate handling in layout.ts.
    for (const ms of [50, 150, 300, 500]) setTimeout(applyGeometry, ms);
  };
  screen.orientation?.addEventListener('change', onRotate);
  window.addEventListener('orientationchange', onRotate);

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) stopLoop();
    else if (prefs.open) startLoop();
  });

  syncControls();
  applyGeometry();
  applyOpenState();
  // Fires immediately, and on every stream-key / delivery-mode / displayed-file
  // change after that — the three inputs to scopesAvailable().
  viewerStore.subscribe(syncAvailability);
}
