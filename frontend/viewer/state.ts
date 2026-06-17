// Cross-module shared state. Subscribers re-render when state changes
// instead of every site calling syncConferenceTiles + sizePlayer + etc.
// manually after a mutation. Modules that own their own internal state
// (chat list, pointer cursors, upload XHR) keep it private.

import { createStore } from '../shared/store.js';
import type {
  DeliveryMode,
  Role,
  RoomInfo,
  RoomStatus,
  RosterEntry,
  TileId,
} from './types.js';

export interface ViewerState {
  // Room metadata loaded once on /info
  roomInfo: RoomInfo | null;
  status: RoomStatus;
  // Tile currently pinned to the main stage. null = grid view, every tile
  // shares the stage equally.
  focusedTile: TileId | null;
  // True once the user has manually pinned/unpinned. Suppresses auto-pin
  // until the user resets (clicking the focused tile clears it).
  focusOverride: boolean;
  // Session
  role: Role;
  deliveryMode: DeliveryMode;
  streamKey: string | null;
  // Per-room admin defaults for participant audio processing. Seed the NR/ER
  // toggles unless the participant has overridden them for this room.
  noiseDefault: boolean;
  echoDefault: boolean;
  // Per-room admin default for push-to-talk. Seeds the participant's PTT mode
  // unless they've overridden it for this room.
  pttDefault: boolean;
  // Conference local state
  cameraOn: boolean;
  micOn: boolean;
  screenOn: boolean;
  // Push-to-talk: when true the mic is muted by default and only goes live
  // while the user holds the mic control (button or `S` key).
  pttEnabled: boolean;
  // Roster from participants:update WS messages
  roster: RosterEntry[];
  // Panels
  chatOpen: boolean;
  confOpen: boolean;
  // Pointer mode toggle
  pointerMode: boolean;
  // Currently presenter-displayed file (image/video) in the room, or null
  // if nothing is being displayed. Drives the #tile-display tile.
  displayFile: { fileId: string; name: string; mime: string } | null;
}

export const viewerStore = createStore<ViewerState>({
  roomInfo: null,
  status: 'pending',
  focusedTile: null,
  focusOverride: false,
  role: 'viewer',
  deliveryMode: 'webrtc',
  streamKey: null,
  noiseDefault: true,
  echoDefault: true,
  pttDefault: false,
  cameraOn: false,
  micOn: false,
  screenOn: false,
  pttEnabled: false,
  roster: [],
  chatOpen: false,
  // Strip is "open" by default — entering focus mode shows it.
  confOpen: true,
  pointerMode: false,
  displayFile: null,
});

// Convenience helpers — modules that just want a single field don't need
// to spell out viewerStore.get().mode each time.
export const getState = (): ViewerState => viewerStore.get();
export const setState = viewerStore.set;
