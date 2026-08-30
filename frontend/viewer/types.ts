// Shapes returned by the public viewer endpoints. Kept in sync by hand
// with handlers under backend/src/routes/rooms_public.rs and the websocket
// hub in backend/src/ws.rs. snake_case fields come straight from SQLite
// row_to_json; camelCase fields are explicit JSON.

export type Role = 'presenter' | 'viewer';
export type RoomStatus = 'pending' | 'scheduled' | 'live' | 'ended';
export type DeliveryMode = 'webrtc' | 'llhls' | 'srt';
// A tile in the unified viewer stage. 'stream' is the unified OvenPlayer
// stage tile (live broadcast OR a presenter-displayed file), 'share' is
// the active screenshare, anything else is a LiveKit participant identity.
export type TileId = 'stream' | 'share' | string;

export interface DisplayFileState {
  fileId: string;
  name: string;
  mime: string;
  size: number;
  playing: boolean;
  position: number;
  updatedAtMs: number;
}

export interface RoomInfo {
  name: string;
  status: RoomStatus;
  starts_at: string | null;
  delivery_mode: DeliveryMode;
  has_password: boolean;
  has_stream_key: boolean;
  waiting_room: boolean;
  // Per-room participant-audio defaults (0/1 from row_to_json).
  noise_reduction: boolean | number;
  echo_cancellation: boolean | number;
  push_to_talk: boolean | number;
}

export interface JoinResponse {
  participant_id: string;
  token: string;
  role: Role;
  delivery_mode: DeliveryMode;
  stream_key: string | null;
  status: RoomStatus;
  starts_at: string | null;
  admitted: boolean;
  waiting_room: boolean;
  noise_reduction_default: boolean;
  echo_cancellation_default: boolean;
  push_to_talk_default: boolean;
  error?: string;
}

export interface StatusResponse {
  admitted: boolean;
  kicked: boolean;
  room_status: RoomStatus;
}

export interface LivekitTokenResponse {
  token: string;
  url: string;
}

export interface RosterEntry {
  id: string;
  name: string;
  role: Role;
  /** Native client id ("farbplay"); absent or null for browser viewers (gh #227). */
  client?: string | null;
}

export interface SessionFile {
  id: string;
  name: string;
  size: number;
  mime?: string;
  uploaderName?: string;
  role?: Role;
}

// ---- WebSocket message variants (server → client) ----

interface ChatHistoryItem {
  type: 'chat:message' | 'file:shared';
  ts: number;
  name: string;
  role: Role;
  text?: string;
  // file:shared fields
  id?: string;
  size?: number;
  uploaderName?: string;
}

export type WsMessage =
  | { type: 'auth:ok' }
  | { type: 'kicked' }
  | { type: 'host:revoked' }
  | {
      type: 'moderation:update';
      waiting: { id: string; name: string }[];
      kicked: { id: string; name: string }[];
      // Admitted, non-kicked participants. The roster shows the ones not in the
      // live WS presence list — Farbplay builds predating pointer collaboration,
      // which open no WS. Current ones are marked on the WS roster (gh #227).
      admitted: { id: string; name: string }[];
      newWaiting: string[];
    }
  | { type: 'room:live' }
  | { type: 'room:pending' }
  | { type: 'room:ended' }
  | { type: 'stream:assigned'; streamKey: string }
  | { type: 'stream:removed' }
  | { type: 'delivery:changed'; deliveryMode: DeliveryMode }
  | { type: 'participants:update'; participants: RosterEntry[] }
  | { type: 'chat:history'; messages: ChatHistoryItem[] }
  | {
      type: 'chat:message';
      ts: number;
      name: string;
      role: Role;
      text: string;
    }
  | {
      type: 'file:shared';
      ts: number;
      name: string;
      role: Role;
      id: string;
      size: number;
      mime?: string;
      uploaderName: string;
    }
  | { type: 'file:removed'; id: string }
  | { type: 'conference:unmute' }
  // Reply to the client's own `ping`, echoing its clock reading back untouched
  // so RTT needs no clock agreement between the two ends (gh #40).
  | { type: 'pong'; t: number }
  | { type: 'pointer:move'; participantId: string; name: string; x: number; y: number }
  | { type: 'pointer:hide'; participantId: string }
  | { type: 'focus:set'; tileId: TileId | null }
  | {
      type: 'display:state';
      fileId: string | null;
      name?: string;
      mime?: string;
      size?: number;
      playing?: boolean;
      position?: number;
      updatedAtMs?: number;
    };

// ---- WebSocket message variants (client → server) ----

export type WsClientMessage =
  | { type: 'auth'; participantId: string; token: string }
  | { type: 'ping'; t: number }
  | { type: 'chat:message'; text: string }
  | { type: 'pointer:move'; x: number; y: number }
  | { type: 'pointer:hide' }
  | { type: 'focus:set'; tileId: TileId | null }
  | { type: 'file:share'; fileId: string }
  | { type: 'display:set'; fileId: string | null }
  | { type: 'display:transport'; playing: boolean; position: number };

// ---- Saved session (sessionStorage) ----

export interface SavedSession {
  participantId: string;
  token: string;
  deliveryMode: DeliveryMode;
  streamKey: string | null;
  role: Role;
}
