// Shapes returned by the Rust backend. Kept in sync by hand with the
// route handlers in backend/src/routes/. snake_case fields come straight
// from the SQLite `row_to_json` helper; camelCase fields are explicit
// JSON objects assembled in the route handler.

export type RoomStatus = 'live' | 'idle' | 'ended' | 'scheduled';
export type DeliveryMode = 'webrtc' | 'llhls' | 'srt';

export interface Room {
  id: string;
  slug: string;
  name: string;
  status: RoomStatus;
  delivery_mode: DeliveryMode;
  waiting_room: boolean | number;
  noise_reduction: boolean | number;
  echo_cancellation: boolean | number;
  push_to_talk: boolean | number;
  expires_at: string | null;
  password_hash: string | null;
  presenter_key: string;
  stream_key_id: string | null;
  stream_key_name: string | null;
}

export interface StreamKey {
  id: string;
  name: string;
  key_token: string;
  room_names?: string;
}

export interface Participant {
  id: string;
  name: string;
}

export interface AssignedRoom {
  id: string;
  name?: string;
  slug?: string;
}

export interface FileEntry {
  id: string;
  name: string;
  mime: string;
  size: number;
  createdAt: string;
  assignedRooms?: AssignedRoom[];
}

export interface StorageBucket {
  prefix: string;
  count: number;
  bytes: number;
}

export interface StorageStats {
  totalBytes: number;
  totalCount: number;
  byMime?: StorageBucket[];
}

export interface BrandingResponse {
  hasLogo: boolean;
  hasBg: boolean;
  hasFavicon: boolean;
  siteName?: string;
  colors?: Record<string, string>;
}

export interface MetricsResponse {
  cpu: { percent: number; cores?: number[] };
  memory: {
    percent: number;
    total_bytes: number;
    used_bytes: number;
    buffers_bytes: number;
    cached_bytes: number;
  };
  network: { interface: string; rx_bps: number; tx_bps: number };
  loadavg?: [number, number, number];
  uptime_secs?: number;
}

export interface OmeTrack {
  type: 'Video' | 'Audio';
  video?: { codec: string; width: number; height: number; framerate: number; bitrateLatest: number };
  audio?: { codec: string; samplerate: number; channel: number; bitrateLatest: number };
}

export interface OmeStreamInput {
  tracks?: OmeTrack[];
  sourceType?: string;
  sourceUrl?: string;
  createdTime?: string;
}

export interface OmeStream {
  name: string;
  key_name?: string;
  room_name?: string;
  detail?: { input?: OmeStreamInput };
}

export interface OmeData {
  error?: string;
  streams: OmeStream[];
  conf_count: number;
}

export interface EnterRoomResponse {
  participantId: string;
  token: string;
  slug: string;
  deliveryMode: DeliveryMode;
  streamKey?: string | null;
}

export type TabId = 'rooms' | 'keys' | 'files' | 'ome' | 'branding' | 'settings';

export interface PasskeyInfo {
  id: string;
  label: string;
  created_at: string;
  last_used_at: string | null;
}

export interface SettingsStatus {
  passwordIsCustom: boolean;
  totpEnabled: boolean;
  passkeys: PasskeyInfo[];
}

export interface AuthMethods {
  totpEnabled: boolean;
  passkeyEnabled: boolean;
}
