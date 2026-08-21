// Colour maths behind the scopes (#229). Pure functions — no DOM, no state, so
// scope-draw.ts stays about drawing and scopes.ts stays about the window.
//
// Three things live here and nowhere else:
//   • working-space definitions (primaries → luma coefficients),
//   • the PQ / HLG / sRGB transfer functions,
//   • primaries conversion, used for the out-of-Rec.709 markers.
//
// Convention: R'G'B' is the *coded* (non-linear) signal. Luma below is
// therefore Y' — the non-constant-luminance form every broadcast scope plots —
// NOT linear-light luminance. That distinction is the whole reason a waveform
// of a log or PQ signal looks the way it does.

export type WorkingSpace = 'rec709' | 'p3' | 'rec2020';
// 'sdr' and 'log' share an IRE axis and differ only in where the false-colour
// exposure zones sit — see FALSE_DISPLAY vs FALSE_LOGC in scope-draw.ts.
export type Scale = 'sdr' | 'log' | 'pq' | 'hlg';

// ---- Working spaces --------------------------------------------------------

// Row-major RGB→XYZ for each space's primaries at D65. The middle row IS the
// luma coefficient triple by definition, so coefficients and gamut conversion
// can never disagree — there is deliberately no second table to keep in sync.
type Mat3 = readonly [number, number, number, number, number, number, number, number, number];

interface SpaceDef {
  readonly label: string;
  readonly toXyz: Mat3;
}

const SPACES: Record<WorkingSpace, SpaceDef> = {
  rec709: {
    label: 'Rec.709',
    toXyz: [
      0.4123907992659595, 0.35758433938387796, 0.1804807884018343,
      0.21263900587151036, 0.7151686787677559, 0.07219231536073371,
      0.019330818715591851, 0.11919477979462599, 0.9505321522496606,
    ],
  },
  p3: {
    label: 'Display P3',
    toXyz: [
      0.48657094864821634, 0.26566769316909306, 0.1982172852343625,
      0.22897456406974878, 0.6917385218365062, 0.079286914093745,
      0.0, 0.045113381858902575, 1.0439443689009757,
    ],
  },
  rec2020: {
    label: 'Rec.2020',
    toXyz: [
      0.6369580483012914, 0.14461690358620832, 0.16888097516417208,
      0.2627002120112671, 0.6779980715188708, 0.05930171646986196,
      0.0, 0.028072693049087428, 1.060985057710791,
    ],
  },
};

// Everything a per-pixel loop needs, resolved once. The renderers run these
// formulas ~130k times per scope per frame, so they hoist this out of the inner
// loop rather than calling luma()/chroma() — which would repeat the record
// lookup and the reciprocals on every pixel.
export interface Coeffs {
  kr: number;
  kg: number;
  kb: number;
  cbScale: number;
  crScale: number;
}

export function coeffsFor(space: WorkingSpace): Coeffs {
  const m = SPACES[space].toXyz;
  const kr = m[3];
  const kg = m[4];
  const kb = m[5];
  return { kr, kg, kb, cbScale: 1 / (2 * (1 - kb)), crScale: 1 / (2 * (1 - kr)) };
}

// Cb/Cr in ±0.5, from the space's own coefficients, so the vectorscope's trace
// and its graticule targets can never drift apart. The per-pixel trace inlines
// this via coeffsFor(); this form is for the handful of graticule targets.
export function chroma(
  space: WorkingSpace,
  r: number,
  g: number,
  b: number,
): [number, number] {
  const { kr, kg, kb, cbScale, crScale } = coeffsFor(space);
  const y = kr * r + kg * g + kb * b;
  return [(b - y) * cbScale, (r - y) * crScale];
}

// A representative Caucasian skin chromaticity, in coded R'G'B'. Only its
// direction matters, not its level.
const REF_SKIN: readonly [number, number, number] = [0.9, 0.66, 0.55];

// Direction of the vectorscope's skin-tone line, as a unit vector in our
// (Cb, Cr) plane. Derived by pushing a reference skin tone through the very
// same chroma() as the trace and the bar targets, so all three stay consistent
// and the line follows a working-space change automatically.
//
// It is tempting to instead rotate the traditionally-quoted "123°" — DON'T.
// That angle is defined in the YUV (U,V) plane, whose two axes are scaled
// differently from Cb/Cr, and an anisotropic scaling does not preserve angles.
// Rotated naively it lands at 134.5°, about 11° off: measured skin tones
// cluster at 124–126° in this plot, and this reference lands at 123.4°.
export function skinLineDirection(space: WorkingSpace): [number, number] {
  const [cb, cr] = chroma(space, REF_SKIN[0], REF_SKIN[1], REF_SKIN[2]);
  const len = Math.hypot(cb, cr) || 1;
  return [cb / len, cr / len];
}

// ---- Transfer functions ----------------------------------------------------

// SMPTE ST 2084 (PQ). Constants verbatim from the standard.
const PQ_M1 = 2610 / 16384;
const PQ_M2 = (2523 / 4096) * 128;
const PQ_C1 = 3424 / 4096;
const PQ_C2 = (2413 / 4096) * 32;
const PQ_C3 = (2392 / 4096) * 32;

// PQ code value (0..1) → absolute luminance in cd/m² (nits), 0..10000.
export function pqToNits(v: number): number {
  const p = Math.pow(Math.max(v, 0), 1 / PQ_M2);
  const num = Math.max(p - PQ_C1, 0);
  const den = PQ_C2 - PQ_C3 * p;
  if (den <= 0) return 10000;
  return 10000 * Math.pow(num / den, 1 / PQ_M1);
}

// Nits → PQ code value. Used to place the nits graticule, which is why the
// marks are non-linearly spaced.
export function nitsToPq(nits: number): number {
  const y = Math.min(Math.max(nits, 0) / 10000, 1);
  const ym = Math.pow(y, PQ_M1);
  return Math.pow((PQ_C1 + PQ_C2 * ym) / (1 + PQ_C3 * ym), PQ_M2);
}

// HLG is graduated in signal percent rather than absolute nits — it is a
// relative, display-referred system, so there is no fixed code-value-to-nits
// mapping to draw. The scale's one meaningful reference is 75% (HLG reference
// white), which the graticule marks; that needs no transfer function, which is
// why the HLG OETF is deliberately absent here.

// sRGB EOTF — the transfer a display-p3 or srgb canvas readback carries.
export function srgbToLinear(v: number): number {
  const c = Math.max(v, 0);
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

export function linearToSrgb(v: number): number {
  const c = Math.max(v, 0);
  return c <= 0.0031308 ? c * 12.92 : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
}

// ---- Code value ⇄ nits, honest about which path produced it ----------------

// BT.2408 reference: HDR graphic/diffuse white sits at 203 cd/m².
export const DIFFUSE_WHITE_NITS = 203;

// On the true-pixel path the sample IS a PQ code value, so this is exact.
//
// On a canvas readback it is not: the browser has already tone-mapped the HDR
// signal into an SDR, sRGB-encoded buffer, and no amount of maths recovers the
// original. `approx` therefore reinterprets the value as display light relative
// to a 203-nit reference white — useful for *relative* comparison, meaningless
// as an absolute measurement, which is why the UI dims these labels and
// prefixes them with '~'. See the provenance badge in scopes.ts.
export function codeToNits(v: number, approx: boolean): number {
  if (approx) return srgbToLinear(v) * DIFFUSE_WHITE_NITS;
  return pqToNits(v);
}

export function nitsToCode(nits: number, approx: boolean): number {
  if (approx) return linearToSrgb(Math.max(nits, 0) / DIFFUSE_WHITE_NITS);
  return nitsToPq(nits);
}

// ---- Gamut ----------------------------------------------------------------

function invert3(m: Mat3): Mat3 {
  const [a, b, c, d, e, f, g, h, i] = m;
  const det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
  if (det === 0) return [1, 0, 0, 0, 1, 0, 0, 0, 1];
  const inv = 1 / det;
  return [
    (e * i - f * h) * inv, (c * h - b * i) * inv, (b * f - c * e) * inv,
    (f * g - d * i) * inv, (a * i - c * g) * inv, (c * d - a * f) * inv,
    (d * h - e * g) * inv, (b * g - a * h) * inv, (a * e - b * d) * inv,
  ];
}

const FROM_XYZ: Record<WorkingSpace, Mat3> = {
  rec709: invert3(SPACES.rec709.toXyz),
  p3: invert3(SPACES.p3.toXyz),
  rec2020: invert3(SPACES.rec2020.toXyz),
};

function apply3(m: Mat3, x: number, y: number, z: number): [number, number, number] {
  return [
    m[0] * x + m[1] * y + m[2] * z,
    m[3] * x + m[4] * y + m[5] * z,
    m[6] * x + m[7] * y + m[8] * z,
  ];
}

// Linear RGB in `from` primaries → linear RGB in `to` primaries, via XYZ.
export function convertPrimaries(
  from: WorkingSpace,
  to: WorkingSpace,
  r: number,
  g: number,
  b: number,
): [number, number, number] {
  if (from === to) return [r, g, b];
  const [x, y, z] = apply3(SPACES[from].toXyz, r, g, b);
  return apply3(FROM_XYZ[to], x, y, z);
}

// True when a colour sits outside the Rec.709 gamut — i.e. it cannot be shown
// on an SDR broadcast display without clipping. Slightly negative/over-unity
// values are normal rounding noise, hence the epsilon.
const GAMUT_EPS = 0.002;
export function outsideRec709(space: WorkingSpace, r: number, g: number, b: number): boolean {
  if (space === 'rec709') return false;
  const [lr, lg, lb] = convertPrimaries(
    space,
    'rec709',
    srgbToLinear(r),
    srgbToLinear(g),
    srgbToLinear(b),
  );
  return (
    lr < -GAMUT_EPS || lr > 1 + GAMUT_EPS ||
    lg < -GAMUT_EPS || lg > 1 + GAMUT_EPS ||
    lb < -GAMUT_EPS || lb > 1 + GAMUT_EPS
  );
}

// ---- Stream colour-space detection -----------------------------------------

// What a VideoFrame told us about the signal. Deliberately plain strings: the
// pinned TypeScript DOM lib still ships the *old* WebCodecs enums (no 'pq',
// no 'hlg', no 'bt2020', no 'I420P10'), so comparing against those literal
// types is a compile error even though browsers emit them. Reading everything
// as string sidesteps the stale lib and gives us room to tolerate spelling
// variants between implementations.
export interface DetectedColor {
  primaries: string;
  transfer: string;
  fullRange: boolean;
}

export interface Detection {
  space: WorkingSpace;
  scale: Scale;
  // Short label for the header chip.
  label: 'HDR10' | 'HLG' | 'SDR';
}

// Map a stream's reported colour space onto a working space + scale. Unknown
// values fall through to SDR/Rec.709 rather than throwing — an implementation
// spelling something unexpectedly must degrade, not break the scopes.
export function detectFromColor(c: DetectedColor | null): Detection {
  const transfer = (c?.transfer ?? '').toLowerCase();
  const primaries = (c?.primaries ?? '').toLowerCase();

  const space: WorkingSpace = primaries.includes('2020')
    ? 'rec2020'
    : primaries.includes('432') || primaries.includes('p3')
      ? 'p3'
      : 'rec709';

  // 'pq' is the WebCodecs spelling; 'smpte2084'/'st2084' show up in other
  // surfaces reporting the same thing.
  if (transfer.includes('pq') || transfer.includes('2084')) {
    return { space, scale: 'pq', label: 'HDR10' };
  }
  if (transfer.includes('hlg') || transfer.includes('arib')) {
    return { space, scale: 'hlg', label: 'HLG' };
  }
  return { space, scale: 'sdr', label: 'SDR' };
}
