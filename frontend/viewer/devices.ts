// Device plumbing for the conference: which camera / microphone / speaker this
// participant chose, finding them again after a reload, routing remote audio
// and the broadcast to the chosen speaker, and the device picker's input-level
// meter. conference.ts owns the LiveKit room and the picker's DOM wiring;
// everything here takes the room as an argument so the two don't import each
// other.

import { logDiag } from './diagnostics.js';
import { slug } from './session.js';

export type InputKind = 'audioinput' | 'videoinput';
export type DeviceKind = InputKind | 'audiooutput';

// ---- Saved choices (per room, like the audio-processing toggles) ----
// The label is kept beside the id because Safari's device ids are salted per
// origin and the salt does not survive clearing website data — a stale id is
// matched by name instead (see restoreSavedInput).
interface SavedDevice {
  id: string;
  label: string;
}

const deviceKey = (kind: DeviceKind): string => `viewer_device_${kind}_${slug}`;

export function savedDevice(kind: DeviceKind): SavedDevice | null {
  try {
    const v = JSON.parse(localStorage.getItem(deviceKey(kind)) || 'null') as SavedDevice | null;
    return v && typeof v.id === 'string' ? v : null;
  } catch {
    return null;
  }
}

export function rememberDevice(kind: DeviceKind, id: string, label: string): void {
  localStorage.setItem(deviceKey(kind), JSON.stringify({ id, label }));
}

// A saved input is requested at capture time as a bare (ideal) deviceId, so an
// unplugged device falls back to the system default instead of failing the
// capture. When the saved id no longer matches anything — Safari's salt rotated,
// or the device was replugged — this finds it again by label once a capture has
// made labels readable, and switches to it.
export async function restoreSavedInput(room: LkRoom, kind: InputKind): Promise<void> {
  const saved = savedDevice(kind);
  if (!saved) return;
  const source =
    kind === 'audioinput' ? LivekitClient.Track.Source.Microphone : LivekitClient.Track.Source.Camera;
  const track = room.localParticipant.getTrackPublication(source)?.track;
  if (!track) return;
  const current = track.mediaStreamTrack.getSettings().deviceId;
  if (current === saved.id) return;
  const devices = await navigator.mediaDevices.enumerateDevices();
  const ofKind = devices.filter((d) => d.kind === kind);
  const match =
    ofKind.find((d) => d.deviceId === saved.id) ??
    ofKind.find((d) => d.label !== '' && d.label === saved.label);
  if (!match || match.deviceId === current) return;
  try {
    if (await room.switchActiveDevice(kind, match.deviceId)) {
      rememberDevice(kind, match.deviceId, match.label);
      return;
    }
  } catch {}
  logDiag('conference', 'warn', `Couldn't restore your saved ${deviceNoun(kind)} (${saved.label})`);
}

export function deviceNoun(kind: DeviceKind): string {
  return kind === 'audioinput' ? 'microphone' : kind === 'videoinput' ? 'camera' : 'speaker';
}

// ---- Device lists for the picker ----

export interface DeviceListing {
  devices: MediaDeviceInfo[];
  /** Permission failures for the kinds we had to ask for, for the caller to report. */
  errors: { kind: 'audio' | 'video'; err: unknown }[];
}

// enumerateDevices only names devices once the page holds capture permission,
// so an unnamed kind gets a one-off getUserMedia. Never for a kind that is
// already live: Safari mutes an existing capture when the page opens a second
// one of the same kind (WebKit bug 179363), and LiveKit stops sending a muted
// track 5 s later — which is how opening this picker mid-call could silence a
// participant. Each kind is asked separately so a missing camera can't cost
// the microphone list its names.
export async function listDevices(live: { audio: boolean; video: boolean }): Promise<DeviceListing> {
  const errors: DeviceListing['errors'] = [];
  let devices = await navigator.mediaDevices.enumerateDevices();
  const unnamed = (kind: MediaDeviceKind): boolean =>
    devices.some((d) => d.kind === kind && d.label === '');
  const ask: ('audio' | 'video')[] = [];
  if (!live.audio && unnamed('audioinput')) ask.push('audio');
  if (!live.video && unnamed('videoinput')) ask.push('video');
  if (ask.length === 0) return { devices, errors };
  for (const kind of ask) {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ [kind]: true });
      stream.getTracks().forEach((t) => t.stop());
    } catch (err) {
      // No such device at all is not worth a toast — the list just says so.
      if (!(err instanceof Error && err.name === 'NotFoundError')) errors.push({ kind, err });
    }
  }
  devices = await navigator.mediaDevices.enumerateDevices();
  return { devices, errors };
}

// ---- Speaker routing ----

export function canSelectSpeaker(): boolean {
  return typeof HTMLMediaElement !== 'undefined' && 'setSinkId' in HTMLMediaElement.prototype;
}

// LiveKit refuses output selection on every Safari-based browser
// (supportsSetSinkId() returns false there without checking), although Safari
// has shipped HTMLMediaElement.setSinkId since 18.4. Where LiveKit says no, the
// remote audio elements it hands back from attach() are routed here instead.
const remoteAudio = new Set<HTMLMediaElement>();

const livekitRoutesOutput = (): boolean => LivekitClient.supportsAudioOutputSelection();

export function trackRemoteAudio(el: HTMLMediaElement): void {
  remoteAudio.add(el);
  // LiveKit applies its own audioOutput to new tracks when it supports it.
  if (!livekitRoutesOutput()) applySavedSpeaker(el);
}

export function forgetRemoteAudio(els?: HTMLMediaElement[]): void {
  // No argument: the room is going away, forget all of them.
  if (!els) remoteAudio.clear();
  else for (const el of els) remoteAudio.delete(el);
}

function stageMedia(): HTMLMediaElement | null {
  return document.querySelector<HTMLVideoElement>('#player video');
}

async function setSink(el: HTMLMediaElement, id: string): Promise<boolean> {
  if (!('setSinkId' in el) || el.sinkId === id) return true;
  try {
    await el.setSinkId(id);
    return true;
  } catch (err) {
    const name = err instanceof Error ? err.name : 'error';
    logDiag('conference', 'warn', `Couldn't route audio to the selected speaker (${name})`);
    return false;
  }
}

// Route every audio source in the room — participants and the broadcast — to
// `id`. Called from the picker's change event: WebKit only honours setSinkId
// inside a user gesture, so every element's call is started in the same tick
// rather than one per await.
export async function applySpeaker(room: LkRoom | null, id: string): Promise<boolean> {
  const pending: Promise<boolean>[] = [];
  if (room && livekitRoutesOutput()) {
    pending.push(
      room.switchActiveDevice('audiooutput', id).then(
        () => true,
        (err: unknown) => {
          console.error('[device switch speaker]', err);
          return false;
        },
      ),
    );
  } else {
    for (const el of remoteAudio) pending.push(setSink(el, id));
  }
  const stage = stageMedia();
  if (stage) pending.push(setSink(stage, id));
  return (await Promise.all(pending)).every(Boolean);
}

// Best effort for an element that appears later (a new participant, a player
// remount). Outside a user gesture WebKit may refuse; setSink logs that.
export function applySavedSpeaker(el: HTMLMediaElement): void {
  const saved = savedDevice('audiooutput');
  if (saved && canSelectSpeaker()) void setSink(el, saved.id);
}

// A short tone through a MediaStream-backed <audio> element routed to `id`.
// MediaStream-backed on purpose: remote participants' audio is too, so this
// takes the same WebKit path a participant's voice does (STP 251 fixed a
// setSinkId bug specific to single-track MediaStreams, and which Safari
// releases carry that fix is unknown).
export async function playTestTone(id: string): Promise<void> {
  const ctx = new AudioContext();
  try {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    const dest = ctx.createMediaStreamDestination();
    osc.frequency.value = 660;
    osc.connect(gain).connect(dest);
    const el = new Audio();
    el.srcObject = dest.stream;
    if (id && canSelectSpeaker()) await el.setSinkId(id);
    await el.play();
    const t = ctx.currentTime;
    gain.gain.setValueAtTime(0.2, t);
    gain.gain.exponentialRampToValueAtTime(0.001, t + 0.6);
    osc.start(t);
    osc.stop(t + 0.6);
    await new Promise((resolve) => setTimeout(resolve, 700));
    el.pause();
    el.srcObject = null;
  } finally {
    void ctx.close();
  }
}

// ---- Input-level meter ----
// Reads the live microphone track the room is actually publishing — not a
// separate preview capture — so a moving bar means the browser is getting sound
// from the device being sent. The 26 Oct Safari session sent near-silence for
// 29 minutes and nothing on screen said so.

interface MeterState {
  ctx: AudioContext;
  analyser: AnalyserNode;
  buf: Float32Array<ArrayBuffer>;
  source: MediaStreamAudioSourceNode | null;
  bound: MediaStreamTrack | null;
  frame: number;
  level: number;
  quietSince: number;
}

let meter: MeterState | null = null;

// Below this the bar reads empty and the "no sound" hint starts counting.
const METER_FLOOR_DB = -60;
const NO_SOUND_HINT_MS = 3000;

export function startMicMeter(
  getTrack: () => MediaStreamTrack | null,
  fill: HTMLElement,
  hint: HTMLElement,
): void {
  stopMicMeter();
  const ctx = new AudioContext();
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 1024;
  const state: MeterState = {
    ctx,
    analyser,
    buf: new Float32Array(analyser.fftSize),
    source: null,
    bound: null,
    frame: 0,
    level: 0,
    quietSince: performance.now(),
  };
  meter = state;

  const tick = (): void => {
    if (meter !== state) return;
    const track = getTrack();
    const live = track && track.readyState === 'live' ? track : null;
    // A device switch or audio-setting change restarts the track underneath
    // us, so rebind whenever the published track is a different object.
    if (live !== state.bound) {
      state.source?.disconnect();
      state.source = live ? ctx.createMediaStreamSource(new MediaStream([live])) : null;
      state.source?.connect(analyser);
      state.bound = live;
      state.quietSince = performance.now();
    }

    let target = 0;
    if (live && live.enabled) {
      analyser.getFloatTimeDomainData(state.buf);
      let sum = 0;
      for (const s of state.buf) sum += s * s;
      const rms = Math.sqrt(sum / state.buf.length);
      const db = rms > 0 ? 20 * Math.log10(rms) : -Infinity;
      target = Math.min(1, Math.max(0, (db - METER_FLOOR_DB) / -METER_FLOOR_DB));
    }
    // Fast attack, slow release — a readable bar rather than a flicker.
    state.level = target > state.level ? target : state.level * 0.9 + target * 0.1;
    fill.style.width = `${(state.level * 100).toFixed(1)}%`;

    const now = performance.now();
    if (target > 0.05) state.quietSince = now;
    if (!live) hint.textContent = 'Microphone is off';
    else if (!live.enabled) hint.textContent = 'Microphone is muted';
    else if (now - state.quietSince > NO_SOUND_HINT_MS) hint.textContent = 'No sound from this microphone';
    else hint.textContent = '';

    state.frame = requestAnimationFrame(tick);
  };
  // Created inside the picker's click, so Safari lets it run — but say so
  // explicitly in case a browser starts it suspended anyway.
  void ctx.resume();
  tick();
}

export function stopMicMeter(): void {
  if (!meter) return;
  cancelAnimationFrame(meter.frame);
  meter.source?.disconnect();
  void meter.ctx.close();
  meter = null;
}
