// Ambient declarations for CDN-loaded libraries used by the viewer.
// We don't pull in the upstream types because we'd then need npm runtime
// deps; only the surface we actually call is declared.

declare const OvenPlayer: {
  create(elementId: string, config: unknown): OvenPlayerInstance;
};

interface OvenPlayerInstance {
  on(
    event: string,
    handler: (e?: { newstate?: string; position?: number; offset?: number; duration?: number }) => void,
  ): void;
  getState(): string;
  getMute(): boolean;
  setMute(muted: boolean): void;
  getVolume(): number;
  setVolume(vol: number): void;
  play(): void;
  pause(): void;
  seek(position: number): void;
  getPosition(): number;
  getDuration(): number;
  load(): void;
  remove(): void;
}

// LiveKit client (loaded as window.LivekitClient by the UMD bundle).
// Only the bits the viewer touches are typed.
declare const LivekitClient: LivekitClientNS;

// Subset of LiveKit's AudioCaptureOptions we configure. voiceIsolation is a
// stronger, browser-native noise/voice isolation (Chrome ML) that supersedes
// noiseSuppression when supported; unsupported browsers ignore it.
interface AudioCaptureOptions {
  echoCancellation?: boolean;
  noiseSuppression?: boolean;
  autoGainControl?: boolean;
  voiceIsolation?: boolean;
  deviceId?: string;
}

// Subset of LiveKit's VideoEncoding / publish defaults we configure for the
// screen-share track. See conference.ts initLiveKit().
interface VideoEncoding {
  maxBitrate: number;
  maxFramerate?: number;
}

interface TrackPublishDefaults {
  videoCodec?: 'vp8' | 'vp9' | 'h264' | 'av1';
  screenShareEncoding?: VideoEncoding;
  degradationPreference?: 'maintain-framerate' | 'maintain-resolution' | 'balanced';
  screenShareSimulcastLayers?: unknown[];
}

// Subset of LiveKit's ScreenShareCaptureOptions passed to setScreenShareEnabled.
interface ScreenShareCaptureOptions {
  resolution?: { width: number; height: number };
  contentHint?: 'detail' | 'motion' | 'text' | 'none';
}

interface RoomOptions {
  audioCaptureDefaults?: AudioCaptureOptions;
  publishDefaults?: TrackPublishDefaults;
}

interface LivekitClientNS {
  Room: new (options?: RoomOptions) => LkRoom;
  RoomEvent: {
    ParticipantConnected: string;
    ParticipantDisconnected: string;
    TrackPublished: string;
    TrackUnpublished: string;
    TrackMuted: string;
    TrackUnmuted: string;
    TrackSubscribed: string;
    TrackUnsubscribed: string;
    LocalTrackUnpublished: string;
    // Connection diagnostics (gh #40). LiveKit reports quality for *every*
    // participant to everyone, so subscribing here gives the host a whole-room
    // view without any backend involvement.
    ConnectionQualityChanged: string;
    Reconnecting: string;
    Reconnected: string;
    Disconnected: string;
  };
  Track: {
    Source: {
      Camera: string;
      Microphone: string;
      ScreenShare: string;
    };
    Kind: {
      Audio: string;
      Video: string;
    };
  };
}

interface LkTrack {
  kind: string;
  source: string;
  mediaStreamTrack: MediaStreamTrack;
  attach(el?: HTMLElement | HTMLMediaElement | null): void;
  detach(el?: HTMLElement | HTMLMediaElement | null): void;
}

interface LkPublication {
  trackSid: string;
  source: string;
  isMuted: boolean;
  track: LkTrack | null;
}

// LiveKit's ConnectionQuality enum serialises as these literals. Declared as a
// bare union rather than imported, in keeping with this file being a hand-kept
// subset — 'unknown' is what you get before the first report lands.
type LkConnectionQuality = 'excellent' | 'good' | 'poor' | 'lost' | 'unknown';

interface LkLocalParticipant {
  identity: string;
  connectionQuality?: LkConnectionQuality;
  setCameraEnabled(on: boolean): Promise<void>;
  setMicrophoneEnabled(on: boolean, options?: AudioCaptureOptions): Promise<void>;
  setScreenShareEnabled(on: boolean, options?: ScreenShareCaptureOptions): Promise<void>;
  getTrackPublication(source: string): LkPublication | undefined;
}

interface LkRemoteParticipant {
  identity: string;
  name?: string;
  metadata?: string;
  connectionQuality?: LkConnectionQuality;
  getTrackPublication(source: string): LkPublication | undefined;
  trackPublications: Map<string, LkPublication>;
}

interface LkRoom {
  localParticipant: LkLocalParticipant;
  remoteParticipants: Map<string, LkRemoteParticipant>;
  // `arg1` also carries a bare string for the diagnostic events —
  // ConnectionQualityChanged passes the quality, Disconnected a reason.
  on(
    event: string,
    handler: (
      arg1?: LkPublication | LkTrack | string,
      arg2?: LkRemoteParticipant | LkLocalParticipant,
      arg3?: LkRemoteParticipant,
    ) => void,
  ): void;
  connect(url: string, token: string): Promise<void>;
  disconnect(): Promise<void>;
  switchActiveDevice(kind: 'videoinput' | 'audioinput' | 'audiooutput', deviceId: string): Promise<void>;
}

// Vendor-prefixed fullscreen API on Safari/iOS.
interface Document {
  webkitFullscreenElement?: Element | null;
  webkitExitFullscreen?: () => void;
}
interface Element {
  webkitRequestFullscreen?: () => void;
}
interface HTMLVideoElement {
  webkitEnterFullscreen?: () => void;
}
