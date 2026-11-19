// Codec support for the live broadcast, shared by the viewer's player and the
// admin dashboard's stream preview so both answer "can this browser play it?"
// the same way. Moved here from frontend/viewer/player.ts.

/**
 * The playlist OME publishes a stream's second video track under: the H.264
 * fallback an encoder sends beside its HEVC (`<Playlist>` in
 * ome/origin_conf/Server.xml). `/live/<pid>/fallback.m3u8` for LL-HLS,
 * `/live/<pid>/fallback` for WebRTC. A stream with one video track still
 * answers on it, with no video: an empty LL-HLS master playlist, and a WebRTC
 * offer carrying audio alone — so only ask WebRTC for it once the LL-HLS
 * playlist has shown a video codec.
 */
export const FALLBACK_PLAYLIST = 'fallback';

export const isAudioCodec = (codec: string): boolean => /^(mp4a|opus|ac-3|ec-3)/.test(codec);

export const isAvcCodec = (codec: string): boolean => /^avc[13]/.test(codec);

/** The first video codec in a CODECS list, or null for an audio-only one. */
export function videoCodecOf(codecs: readonly string[]): string | null {
  return codecs.find((c) => !isAudioCodec(c)) ?? null;
}

/**
 * Fetch a playlist. Null when it isn't there to read — a non-2xx (not live yet,
 * or a transient blip) or a network failure — as distinct from a playlist that
 * was served but lists nothing, which comes back as its (empty) text.
 */
export async function fetchPlaylist(url: string): Promise<string | null> {
  try {
    const res = await fetch(url);
    if (!res.ok) return null;
    return await res.text();
  } catch {
    return null;
  }
}

/** Every codec a master playlist's CODECS attributes name, deduplicated. */
export function parseCodecs(manifest: string): string[] {
  const codecs = new Set<string>();
  for (const match of manifest.matchAll(/CODECS="([^"]+)"/g)) {
    const list = match[1];
    if (!list) continue;
    for (const codec of list.split(',')) codecs.add(codec.trim());
  }
  return [...codecs];
}

/** The URI of a master playlist's first variant — the line after its first
 *  `#EXT-X-STREAM-INF`. OME writes an absolute path carrying its own `session`. */
export function firstVariantUri(manifest: string): string | null {
  const lines = manifest.split(/\r?\n/);
  const at = lines.findIndex((l) => l.startsWith('#EXT-X-STREAM-INF'));
  if (at < 0) return null;
  const uri = lines.slice(at + 1).find((l) => l.trim() && !l.startsWith('#'));
  return uri ? uri.trim() : null;
}

/** A media playlist's `#EXT-X-TARGETDURATION`, in seconds. */
export function parseTargetDuration(chunklist: string): number | null {
  const match = /#EXT-X-TARGETDURATION:(\d+(?:\.\d+)?)/.exec(chunklist);
  const seconds = match?.[1] ? Number(match[1]) : NaN;
  return Number.isFinite(seconds) && seconds > 0 ? seconds : null;
}

// LL-HLS goes through MSE, which takes the playlist's codec strings verbatim.
// Returns the first codec this browser can't decode, or null.
export function unplayableViaMse(codecs: readonly string[]): string | null {
  if (typeof MediaSource === 'undefined') return null;
  for (const codec of codecs) {
    const container = isAudioCodec(codec) ? 'audio/mp4' : 'video/mp4';
    if (!MediaSource.isTypeSupported(`${container}; codecs="${codec}"`)) return codec;
  }
  return null;
}

// Playlist fourcc -> the mime type WebRTC negotiates under. The two namespaces
// are unrelated, so the mapping has to be explicit.
const WEBRTC_MIME: ReadonlyArray<readonly [RegExp, string]> = [
  [/^avc[13]/, 'video/h264'],
  [/^(hvc1|hev1)/, 'video/h265'],
  [/^av01/, 'video/av1'],
  [/^vp0?8/, 'video/vp8'],
  [/^vp0?9/, 'video/vp9'],
];

// WebRTC negotiates codecs in SDP, so MSE support is the wrong question — what
// matters is whether the browser will *accept* the codec in its answer. When it
// won't, OME finds no matching rendition and 403s the session, which is the
// Firefox-plus-H.265 case that looks like a dead stream.
//
// Audio is deliberately ignored: OME transcodes to Opus for WebRTC while the
// playlist advertises the AAC variant, so the playlist's audio entry says
// nothing about this path.
//
// Fails open. Only blocks when the browser reports a non-empty codec list that
// doesn't contain ours — an unknown fourcc, a missing API, or an empty list all
// mean "let it try". A false "can't play" on a working stream would be worse
// than the black tile this replaces.
//
// Catches codec mismatch, NOT profile mismatch: getCapabilities exposes no
// profile detail for HEVC, so a browser advertising video/h265 can still fail on
// profile 4 (Range Extensions, 4:2:2). The viewer's decode watchdog
// (frontend/viewer/player.ts) is what catches that.
export function unplayableViaWebrtc(codecs: readonly string[]): string | null {
  if (typeof RTCRtpReceiver === 'undefined' || !RTCRtpReceiver.getCapabilities) return null;
  const supported = RTCRtpReceiver.getCapabilities('video')?.codecs ?? [];
  if (supported.length === 0) return null;
  const mimes = new Set(supported.map((c) => c.mimeType.toLowerCase()));

  for (const codec of codecs) {
    if (isAudioCodec(codec)) continue;
    const mapped = WEBRTC_MIME.find(([re]) => re.test(codec))?.[1];
    if (mapped && !mimes.has(mapped)) return codec;
  }
  return null;
}

// Also used by the stats panel, so the readout and the blocked-playback
// message never disagree about a codec's name.
export function codecLabel(codec: string): string {
  if (codec.startsWith('av01')) return 'AV1';
  if (codec.startsWith('hvc1') || codec.startsWith('hev1')) return 'H.265 (HEVC)';
  if (codec.startsWith('avc1')) return 'H.264';
  return codec;
}
