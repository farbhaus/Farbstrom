// Scope renderers (#229). Pure drawing — each function owns its canvas and
// reads nothing but the frame it is handed, so the sampling tier in scopes.ts
// can change without touching any of this.
//
// Waveform, parade and vectorscope all share one technique: accumulate hits
// into an intensity buffer, then blend that buffer over the panel background
// into an ImageData and blit it in one call. The background is baked into the
// ImageData deliberately — putImageData *replaces* the destination rather than
// compositing over it, so painting a background first and then blitting a
// trace with alpha would simply erase it.

import {
  chroma,
  codeToNits,
  coeffsFor,
  nitsToCode,
  outsideRec709,
  skinLineDirection,
  type Scale,
  type WorkingSpace,
} from './scope-color.js';

export interface ScopeFrame {
  // w*h*3 coded R'G'B', 0..1.
  data: Float32Array;
  w: number;
  h: number;
}

export interface DrawOpts {
  space: WorkingSpace;
  scale: Scale;
  // True when the samples came from a canvas readback, i.e. the browser's
  // tone-mapped display output rather than the source signal. Nits labels are
  // dimmed and '~'-prefixed in this mode — see codeToNits() in scope-color.ts.
  approxNits: boolean;
  gamutWarn: boolean;
  dpr: number;
  // Vectorscope magnification, 1 = full 100%-saturation circle fits the panel.
  // Ignored by every other scope.
  zoom: number;
}

// Scope chrome and trace colours are fixed measurement semantics, not brandable
// UI — a "red channel" that tracked the accent colour would be nonsense. Same
// rationale as POINTER_COLORS in pointer.ts. (design-lint.sh only scans CSS and
// HTML for raw colours, so these are also outside its remit by construction.)
const GROUND: readonly [number, number, number] = [10, 10, 10];
// Neutral, deliberately. This used to be a green-tinted white, which read as a
// colour cast on a tool whose whole job is judging colour.
const TRACE_LUMA: readonly [number, number, number] = [242, 246, 250];
const TRACE_R: readonly [number, number, number] = [255, 80, 80];
const TRACE_G: readonly [number, number, number] = [80, 255, 110];
const TRACE_B: readonly [number, number, number] = [90, 140, 255];
const GRATICULE = 'rgba(255,255,255,0.26)';
const GRATICULE_KEY = 'rgba(255,255,255,0.48)';
const LABEL = 'rgba(255,255,255,0.78)';
const LABEL_DIM = 'rgba(255,255,255,0.4)';
const GAMUT_MARK = 'rgba(255,64,255,0.9)';

// Alpha ramp for the accumulated trace. A lone hit still has to be clearly
// visible, hence the floor — without it, sparse parts of the trace (exactly the
// parts you are squinting at) faded to nothing.
const TRACE_GAIN = 0.55;
const TRACE_MIN_ALPHA = 0.25;

// Line and text sizes are quoted in CSS pixels and multiplied up by dpr, so
// they stay the same physical size on any display.
const HAIR_PX = 1;
const LABEL_PX = 11;
const TARGET_LABEL_PX = 10;

// Device pixels to deposit per sample, per axis. A single device pixel is a
// half-CSS-pixel hairline on a 2x display — which is why the trace read as far
// too thin — so stamp enough to be at least one CSS pixel wherever it renders.
function stampSize(dpr: number): number {
  return Math.max(1, Math.round(dpr));
}

// Deposit one sample as a stampSize² block, clipped to the buffer.
function deposit(buf: Uint16Array, w: number, h: number, px: number, py: number, t: number): void {
  for (let dy = 0; dy < t; dy++) {
    const y = py + dy;
    if (y < 0 || y >= h) continue;
    const row = y * w;
    for (let dx = 0; dx < t; dx++) {
      const x = px + dx;
      if (x < 0 || x >= w) continue;
      const i = row + x;
      if (buf[i]! < 65535) buf[i]!++;
    }
  }
}

// ---- Scratch buffers -------------------------------------------------------

// Panels redraw at up to 15fps; allocating a fresh buffer per frame per scope
// would hand the GC several MB a second.
const intensityCache = new Map<string, Uint16Array>();
function intensity(w: number, h: number): Uint16Array {
  const key = `${w}x${h}`;
  let buf = intensityCache.get(key);
  if (!buf) {
    buf = new Uint16Array(w * h);
    intensityCache.set(key, buf);
  } else {
    buf.fill(0);
  }
  return buf;
}

const imageCache = new Map<string, ImageData>();
function scratchImage(ctx: CanvasRenderingContext2D, w: number, h: number): ImageData {
  const key = `${w}x${h}`;
  let img = imageCache.get(key);
  if (!img || img.width !== w || img.height !== h) {
    img = ctx.createImageData(w, h);
    imageCache.set(key, img);
  }
  return img;
}

let offscreen: HTMLCanvasElement | null = null;
function offscreenCtx(w: number, h: number): CanvasRenderingContext2D | null {
  if (!offscreen) offscreen = document.createElement('canvas');
  if (offscreen.width !== w || offscreen.height !== h) {
    offscreen.width = w;
    offscreen.height = h;
  }
  return offscreen.getContext('2d');
}

// Release everything held for redraws. Called when the window closes so an
// idle viewer isn't sitting on scope buffers for the rest of the session.
export function releaseScopeBuffers(): void {
  intensityCache.clear();
  imageCache.clear();
  offscreen = null;
}

// Blend an intensity buffer over the ground colour and blit it at (dx, 0).
function blitTrace(
  ctx: CanvasRenderingContext2D,
  buf: Uint16Array,
  w: number,
  h: number,
  rgb: readonly [number, number, number],
  dx = 0,
): void {
  const img = scratchImage(ctx, w, h);
  const d = img.data;
  const [br, bg, bb] = GROUND;
  const [fr, fg, fb] = rgb;
  for (let i = 0, p = 0; i < buf.length; i++, p += 4) {
    const n = buf[i]!;
    if (n === 0) {
      d[p] = br;
      d[p + 1] = bg;
      d[p + 2] = bb;
      d[p + 3] = 255;
      continue;
    }
    const a = TRACE_MIN_ALPHA + (1 - TRACE_MIN_ALPHA) * (1 - Math.exp(-n * TRACE_GAIN));
    d[p] = (br + (fr - br) * a) | 0;
    d[p + 1] = (bg + (fg - bg) * a) | 0;
    d[p + 2] = (bb + (fb - bb) * a) | 0;
    d[p + 3] = 255;
  }
  ctx.putImageData(img, dx, 0);
}

function font(dpr: number, px = LABEL_PX): string {
  return `${Math.round(px * dpr)}px ui-monospace, monospace`;
}

// ---- Vertical scale --------------------------------------------------------

interface Mark {
  // Code value 0..1.
  v: number;
  label: string;
  key: boolean;
}

// The trace is always the coded value, so the y-axis geometry never changes —
// only the graticule labelling does. That is exactly how a hardware scope
// switches between IRE and nits.
function marksFor(o: DrawOpts): { marks: Mark[]; unit: string } {
  if (o.scale === 'pq') {
    const nits = [0.1, 1, 10, 100, 203, 1000, 4000, 10000];
    const marks: Mark[] = [];
    for (const n of nits) {
      const v = nitsToCode(n, o.approxNits);
      // In approximate mode anything above the 203-nit reference maps past
      // full scale; dropping those marks is itself the honest signal that the
      // path cannot see highlights.
      if (v > 1.0001) continue;
      marks.push({
        v,
        label: (o.approxNits ? '~' : '') + (n >= 1 ? String(n) : n.toFixed(1)),
        key: n === 203,
      });
    }
    return { marks, unit: 'nits' };
  }
  if (o.scale === 'hlg') {
    return {
      marks: [0, 25, 50, 75, 100].map((p) => ({
        v: p / 100,
        label: String(p),
        // 75% is HLG reference white.
        key: p === 75,
      })),
      unit: '%',
    };
  }
  if (o.scale === 'log') {
    // 40 stands in for ARRI's 38–42 mid-grey zone.
    return {
      marks: [0, 25, 40, 75, 100].map((p) => ({ v: p / 100, label: String(p), key: p === 40 })),
      unit: 'IRE',
    };
  }
  return {
    marks: [0, 25, 50, 75, 100].map((p) => ({ v: p / 100, label: String(p), key: p === 0 || p === 100 })),
    unit: 'IRE',
  };
}

function drawScaleGraticule(ctx: CanvasRenderingContext2D, w: number, h: number, o: DrawOpts): void {
  const { marks, unit } = marksFor(o);
  ctx.save();
  ctx.lineWidth = Math.max(1, o.dpr * HAIR_PX);
  ctx.font = font(o.dpr);
  ctx.textBaseline = 'middle';
  const dim = o.scale === 'pq' && o.approxNits;
  for (const m of marks) {
    const y = Math.round((1 - m.v) * (h - 1)) + 0.5;
    ctx.strokeStyle = m.key ? GRATICULE_KEY : GRATICULE;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.fillStyle = dim ? LABEL_DIM : LABEL;
    ctx.fillText(m.label, 3 * o.dpr, Math.min(Math.max(y, 6 * o.dpr), h - 6 * o.dpr));
  }
  ctx.fillStyle = dim ? LABEL_DIM : LABEL;
  ctx.textAlign = 'right';
  ctx.fillText(unit, w - 3 * o.dpr, 7 * o.dpr);
  ctx.restore();
}

// ---- Luma waveform ---------------------------------------------------------

export function drawLumaWaveform(
  ctx: CanvasRenderingContext2D,
  f: ScopeFrame,
  o: DrawOpts,
): void {
  const w = ctx.canvas.width;
  const h = ctx.canvas.height;
  if (w < 2 || h < 2) return;
  const buf = intensity(w, h);
  const { data } = f;
  const xs = w / f.w;
  const t = stampSize(o.dpr);
  const { kr, kg, kb } = coeffsFor(o.space);
  for (let sy = 0; sy < f.h; sy++) {
    for (let sx = 0; sx < f.w; sx++) {
      const i = (sy * f.w + sx) * 3;
      const y = kr * data[i]! + kg * data[i + 1]! + kb * data[i + 2]!;
      const py = ((1 - Math.min(Math.max(y, 0), 1)) * (h - 1)) | 0;
      const px = (sx * xs) | 0;
      deposit(buf, w, h, px, py, t);
    }
  }
  blitTrace(ctx, buf, w, h, TRACE_LUMA);
  drawScaleGraticule(ctx, w, h, o);
}

// ---- RGB parade ------------------------------------------------------------

const PARADE_TINTS = [TRACE_R, TRACE_G, TRACE_B] as const;

export function drawRgbParade(ctx: CanvasRenderingContext2D, f: ScopeFrame, o: DrawOpts): void {
  const w = ctx.canvas.width;
  const h = ctx.canvas.height;
  if (w < 6 || h < 2) return;
  const pw = Math.floor(w / 3);
  const { data } = f;
  const xs = pw / f.w;
  const t = stampSize(o.dpr);

  for (let ch = 0; ch < 3; ch++) {
    const buf = intensity(pw, h);
    for (let sy = 0; sy < f.h; sy++) {
      for (let sx = 0; sx < f.w; sx++) {
        const v = data[(sy * f.w + sx) * 3 + ch]!;
        const py = ((1 - Math.min(Math.max(v, 0), 1)) * (h - 1)) | 0;
        const px = (sx * xs) | 0;
        deposit(buf, pw, h, px, py, t);
      }
    }
    blitTrace(ctx, buf, pw, h, PARADE_TINTS[ch]!, ch * pw);
  }
  // Any remainder column left by the floor() above would show through as an
  // uninitialised strip.
  if (pw * 3 < w) {
    ctx.fillStyle = `rgb(${GROUND[0]},${GROUND[1]},${GROUND[2]})`;
    ctx.fillRect(pw * 3, 0, w - pw * 3, h);
  }
  drawScaleGraticule(ctx, w, h, o);

  ctx.save();
  ctx.strokeStyle = GRATICULE;
  ctx.lineWidth = Math.max(1, o.dpr * HAIR_PX);
  for (let i = 1; i < 3; i++) {
    const x = Math.round(i * pw) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, h);
    ctx.stroke();
  }
  ctx.restore();
}

// ---- Vectorscope -----------------------------------------------------------

// 75% colour bars. Targets are derived by pushing these through the very same
// chroma() the trace uses, so they track a working-space change together and
// can never drift apart.
const BARS_75: ReadonlyArray<readonly [string, number, number, number]> = [
  ['R', 0.75, 0, 0],
  ['Yl', 0.75, 0.75, 0],
  ['G', 0, 0.75, 0],
  ['Cy', 0, 0.75, 0.75],
  ['B', 0, 0, 0.75],
  ['Mg', 0.75, 0, 0.75],
];

export function drawVectorscope(ctx: CanvasRenderingContext2D, f: ScopeFrame, o: DrawOpts): void {
  const w = ctx.canvas.width;
  const h = ctx.canvas.height;
  if (w < 8 || h < 8) return;
  const cx = w / 2;
  const cy = h / 2;
  // Cb/Cr span ±0.5, so doubling puts 100% saturation on the outer circle and
  // the 75% bar targets land at 0.75R — the standard graticule.
  const radius = Math.min(w, h) / 2 - 8 * o.dpr;

  const buf = intensity(w, h);
  const { data } = f;
  const gamut: number[] = [];
  const t = stampSize(o.dpr);
  const { kr, kg, kb, cbScale, crScale } = coeffsFor(o.space);
  // Zoom magnifies ONLY the trace. The graticule — ring, crosshair, skin line
  // and the 75% targets — stays put as a fixed reference, so zooming reads the
  // signal against a scale that never moves. The trace simply clips at the
  // panel edge once it outgrows the ring.
  const gain = 2 * radius * o.zoom;
  const graticule = 2 * radius;
  for (let i = 0; i < data.length; i += 3) {
    const r = data[i]!;
    const g = data[i + 1]!;
    const b = data[i + 2]!;
    const y = kr * r + kg * g + kb * b;
    const cb = (b - y) * cbScale;
    const cr = (r - y) * crScale;
    // Rounded, not truncated. `| 0` truncates toward zero, which is right for a
    // bin index but asymmetric about the centre here: exact neutral carries a
    // float epsilon, and multiplied by a high zoom gain that epsilon flips the
    // truncation across the boundary, jittering the centre point by a pixel.
    const px = Math.round(cx + cb * gain);
    const py = Math.round(cy - cr * gain);
    if (px < 0 || px >= w || py < 0 || py >= h) continue;
    deposit(buf, w, h, px, py, t);
    // Sampling the gamut test is deliberate: outsideRec709 linearises three
    // channels and multiplies a 3x3, far too heavy to run on every pixel of
    // every frame. Every 37th pixel is plenty to light up an offending region.
    if (o.gamutWarn && gamut.length < 2000 && i % 111 === 0 && outsideRec709(o.space, r, g, b)) {
      gamut.push(px, py);
    }
  }
  blitTrace(ctx, buf, w, h, TRACE_LUMA);

  ctx.save();
  ctx.lineWidth = Math.max(1, o.dpr * HAIR_PX);
  ctx.strokeStyle = GRATICULE;
  ctx.beginPath();
  ctx.arc(cx, cy, radius, 0, Math.PI * 2);
  ctx.stroke();
  // Crosshair spans the panel rather than stopping at the ring, so it stays a
  // usable reference for trace that has zoomed past it.
  ctx.beginPath();
  ctx.moveTo(cx, 0);
  ctx.lineTo(cx, h);
  ctx.moveTo(0, cy);
  ctx.lineTo(w, cy);
  ctx.stroke();

  // Skin-tone (I-axis) line.
  const [sx, sy] = skinLineDirection(o.space);
  const reach = Math.max(w, h);
  ctx.strokeStyle = GRATICULE_KEY;
  ctx.setLineDash([4 * o.dpr, 4 * o.dpr]);
  ctx.beginPath();
  ctx.moveTo(cx, cy);
  ctx.lineTo(cx + sx * reach, cy - sy * reach);
  ctx.stroke();
  ctx.setLineDash([]);

  // 75% bar targets.
  const box = 4 * o.dpr;
  ctx.strokeStyle = GRATICULE_KEY;
  ctx.fillStyle = LABEL;
  ctx.font = font(o.dpr, TARGET_LABEL_PX);
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  for (const [name, r, g, b] of BARS_75) {
    const [cb, cr] = chroma(o.space, r, g, b);
    const tx = cx + cb * graticule;
    const ty = cy - cr * graticule;
    ctx.strokeRect(tx - box, ty - box, box * 2, box * 2);
    ctx.fillText(name, tx, ty - box * 2.4);
  }

  if (gamut.length) {
    ctx.fillStyle = GAMUT_MARK;
    for (let i = 0; i < gamut.length; i += 2) {
      ctx.fillRect(gamut[i]!, gamut[i + 1]!, o.dpr, o.dpr);
    }
  }

  if (o.zoom !== 1) {
    ctx.fillStyle = LABEL;
    ctx.font = font(o.dpr);
    ctx.textAlign = 'right';
    ctx.textBaseline = 'bottom';
    // Rounded: wheel zoom is continuous, so the raw value prints as 2.371373×.
    const z = Math.round(o.zoom * 10) / 10;
    ctx.fillText(`${Number.isInteger(z) ? z : z.toFixed(1)}×`, w - 4 * o.dpr, h - 4 * o.dpr);
  }
  ctx.restore();
}

// ---- False colour ----------------------------------------------------------

interface Band {
  // Upper bound, exclusive: IRE for the SDR palette, nits for the PQ one.
  max: number;
  rgb: readonly [number, number, number];
  label: string;
}

// ARRI's exposure zones and palette, re-anchored to a DISPLAY-REFERRED signal.
//
// ARRI publish these bands over LogC *exposure*: 18% grey at 38–42 IRE, skin at
// 52–56. Those numbers are only correct for a log signal. A graded Rec.709 feed
// — the normal case for a review session here — puts 18% grey at 46.1 IRE and
// skin one stop over at 63.4, so ARRI's raw numbers would light up ~28% grey and
// call it middle grey. In a tool people use to judge exposure that is worse than
// useless, so the bands below sit where those tones actually land in a
// display-referred signal; pick the 'log' scale for ARRI's published values.
const FALSE_DISPLAY: readonly Band[] = [
  { max: 2.5, rgb: [130, 40, 190], label: 'clip' },
  { max: 4, rgb: [40, 90, 255], label: '' },
  { max: 44, rgb: [0, 0, 0], label: '' },
  { max: 48, rgb: [40, 210, 60], label: '18%' },
  { max: 61, rgb: [0, 0, 0], label: '' },
  { max: 66, rgb: [255, 130, 190], label: 'skin' },
  { max: 97, rgb: [0, 0, 0], label: '' },
  { max: 99, rgb: [235, 220, 40], label: '' },
  { max: 101, rgb: [230, 40, 40], label: 'clip' },
];

// ARRI's published LogC zones, verbatim. Correct when the stream really is
// carrying log — and only then.
const FALSE_LOGC: readonly Band[] = [
  { max: 2.5, rgb: [130, 40, 190], label: 'clip' },
  { max: 4, rgb: [40, 90, 255], label: '' },
  { max: 38, rgb: [0, 0, 0], label: '' },
  { max: 42, rgb: [40, 210, 60], label: '18%' },
  { max: 52, rgb: [0, 0, 0], label: '' },
  { max: 56, rgb: [255, 130, 190], label: 'skin' },
  { max: 97, rgb: [0, 0, 0], label: '' },
  { max: 99, rgb: [235, 220, 40], label: '' },
  { max: 101, rgb: [230, 40, 40], label: 'clip' },
];

// Nit bands for PQ. 203 nits is BT.2408 diffuse white — the reference a
// colourist actually places faces and paper against.
const FALSE_PQ: readonly Band[] = [
  { max: 0.1, rgb: [40, 40, 60], label: '' },
  { max: 1, rgb: [70, 60, 160], label: '' },
  { max: 10, rgb: [40, 90, 255], label: '' },
  { max: 100, rgb: [0, 0, 0], label: '' },
  { max: 203, rgb: [40, 210, 60], label: '203' },
  { max: 400, rgb: [235, 220, 40], label: '' },
  { max: 1000, rgb: [245, 150, 40], label: '' },
  { max: 4000, rgb: [230, 40, 40], label: '' },
  { max: Infinity, rgb: [255, 255, 255], label: '4k+' },
];

// A zeroed band renders the picture's own luma as a grey ramp instead of a flat
// colour, so the untinted parts stay readable as an image.
function bandColor(bands: readonly Band[], value: number, y: number): readonly [number, number, number] {
  for (const b of bands) {
    if (value < b.max) {
      if (b.rgb[0] === 0 && b.rgb[1] === 0 && b.rgb[2] === 0) {
        const g = (Math.min(Math.max(y, 0), 1) * 205 + 25) | 0;
        return [g, g, g];
      }
      return b.rgb;
    }
  }
  return [255, 255, 255];
}

export function drawFalseColor(ctx: CanvasRenderingContext2D, f: ScopeFrame, o: DrawOpts): void {
  const w = ctx.canvas.width;
  const h = ctx.canvas.height;
  if (w < 2 || h < 2) return;
  const bands = o.scale === 'pq' ? FALSE_PQ : o.scale === 'log' ? FALSE_LOGC : FALSE_DISPLAY;
  const src = offscreenCtx(f.w, f.h);
  if (!src) return;
  const img = src.createImageData(f.w, f.h);
  const d = img.data;
  const { data } = f;
  const { kr, kg, kb } = coeffsFor(o.space);
  const pq = o.scale === 'pq';

  for (let i = 0, p = 0; i < data.length; i += 3, p += 4) {
    const y = kr * data[i]! + kg * data[i + 1]! + kb * data[i + 2]!;
    const value = pq ? codeToNits(y, o.approxNits) : y * 100;
    const [r, g, b] = bandColor(bands, value, y);
    d[p] = r;
    d[p + 1] = g;
    d[p + 2] = b;
    d[p + 3] = 255;
  }
  src.putImageData(img, 0, 0);

  // Letterbox into the panel so the picture keeps its aspect ratio.
  ctx.fillStyle = `rgb(${GROUND[0]},${GROUND[1]},${GROUND[2]})`;
  ctx.fillRect(0, 0, w, h);
  const scale = Math.min(w / f.w, h / f.h);
  const dw = f.w * scale;
  const dh = f.h * scale;
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(offscreen!, (w - dw) / 2, (h - dh) / 2, dw, dh);

  // Legend: only the named stops, so it stays readable at panel size.
  ctx.save();
  ctx.font = font(o.dpr, TARGET_LABEL_PX);
  ctx.textBaseline = 'bottom';
  let x = 4 * o.dpr;
  const sw = 8 * o.dpr;
  for (const b of bands) {
    if (!b.label) continue;
    ctx.fillStyle = `rgb(${b.rgb[0]},${b.rgb[1]},${b.rgb[2]})`;
    ctx.fillRect(x, h - 9 * o.dpr, sw, sw);
    x += sw + 2 * o.dpr;
    ctx.fillStyle = LABEL;
    ctx.textAlign = 'left';
    ctx.fillText(b.label, x, h - 3 * o.dpr);
    x += ctx.measureText(b.label).width + 6 * o.dpr;
  }
  ctx.restore();
}

// ---- Histogram -------------------------------------------------------------

const HIST_BINS = 256;
const histBins = [new Uint32Array(HIST_BINS), new Uint32Array(HIST_BINS), new Uint32Array(HIST_BINS)];

export function drawHistogram(ctx: CanvasRenderingContext2D, f: ScopeFrame, o: DrawOpts): void {
  const w = ctx.canvas.width;
  const h = ctx.canvas.height;
  if (w < 2 || h < 2) return;
  for (const b of histBins) b.fill(0);
  const { data } = f;
  // Binning on the coded value keeps the axis perceptually uniform in every
  // scale — including PQ, where binning on linear light would pile everything
  // into the bottom bin.
  for (let i = 0; i < data.length; i += 3) {
    for (let ch = 0; ch < 3; ch++) {
      const v = Math.min(Math.max(data[i + ch]!, 0), 1);
      histBins[ch]![(v * (HIST_BINS - 1)) | 0]!++;
    }
  }

  let peak = 1;
  for (const b of histBins) {
    for (let i = 0; i < HIST_BINS; i++) if (b[i]! > peak) peak = b[i]!;
  }

  ctx.fillStyle = `rgb(${GROUND[0]},${GROUND[1]},${GROUND[2]})`;
  ctx.fillRect(0, 0, w, h);

  ctx.save();
  ctx.globalCompositeOperation = 'lighter';
  const tints = [TRACE_R, TRACE_G, TRACE_B] as const;
  const bw = w / HIST_BINS;
  for (let ch = 0; ch < 3; ch++) {
    const t = tints[ch]!;
    ctx.fillStyle = `rgba(${t[0]},${t[1]},${t[2]},0.55)`;
    ctx.beginPath();
    ctx.moveTo(0, h);
    for (let i = 0; i < HIST_BINS; i++) {
      const v = histBins[ch]![i]! / peak;
      ctx.lineTo(i * bw, h - v * (h - 1));
    }
    ctx.lineTo(w, h);
    ctx.closePath();
    ctx.fill();
  }
  ctx.restore();

  ctx.save();
  ctx.strokeStyle = GRATICULE;
  ctx.lineWidth = Math.max(1, o.dpr * HAIR_PX);
  for (let i = 1; i < 4; i++) {
    const x = Math.round((i / 4) * w) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, h);
    ctx.stroke();
  }
  ctx.restore();
}
