use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSharedEvent {
    pub slug: String,
    pub id: String,
    pub participant_id: String,
    pub uploader_name: String,
    pub role: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub ts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUnsharedEvent {
    pub slug: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KickedEvent {
    pub slug: String,
    pub participant_id: String,
}

/// Emitted when an admin rotates the room's presenter_key. Connected
/// presenters get force-rejoined so their old link no longer grants host
/// privileges — they reload, re-hit the join API with the (now stale) pk,
/// and are downgraded to viewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostRevokedEvent {
    pub slug: String,
    pub participant_id: String,
}

/// Emitted when a presenter asks a participant to unmute. LiveKit cannot
/// force-unmute a track (privacy), so the request is pushed to the target's
/// own client over WS — their browser prompts and, on consent, re-enables the
/// mic itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceUnmuteEvent {
    pub slug: String,
    pub participant_id: String,
}

/// Emitted whenever a participant's admission/kick state changes for a
/// room: new waiting joiner, admit, admit-all, kick, unkick. The WS layer
/// reacts by pushing the current waiting + kicked lists to every connected
/// presenter (and only presenters — viewers never see these names).
#[derive(Debug, Clone)]
pub struct ModerationChangedEvent {
    pub slug: String,
}

/// Emitted when an admin attaches a stream key to a room. Carries the new
/// key_token so connected clients can swap it into their session and reload
/// the player without bouncing through /join (which would require re-auth
/// and, for presenters handed off via pre-session, would lose the role).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamKeyAssignedEvent {
    pub slug: String,
    pub stream_key: String,
}

/// Emitted when an admin changes a room's delivery mode. Connected viewers
/// re-evaluate their layout (browser broadcast tile vs. call-grid only) the
/// same way attaching/detaching a stream key does — an SRT room is call-only
/// in the browser, while webrtc/llhls shows and auto-pins the stream tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryModeChangedEvent {
    pub slug: String,
    pub mode: String,
}

#[derive(Clone)]
pub struct EventChannels {
    pub room_live: broadcast::Sender<String>,
    pub room_pending: broadcast::Sender<String>,
    pub room_ended: broadcast::Sender<String>,
    pub stream_key_assigned: broadcast::Sender<StreamKeyAssignedEvent>,
    pub stream_key_removed: broadcast::Sender<String>,
    pub delivery_mode_changed: broadcast::Sender<DeliveryModeChangedEvent>,
    pub file_shared: broadcast::Sender<FileSharedEvent>,
    pub file_unshared: broadcast::Sender<FileUnsharedEvent>,
    pub participant_kicked: broadcast::Sender<KickedEvent>,
    pub host_revoked: broadcast::Sender<HostRevokedEvent>,
    pub moderation_changed: broadcast::Sender<ModerationChangedEvent>,
    pub conference_unmute: broadcast::Sender<ConferenceUnmuteEvent>,
}

impl EventChannels {
    pub fn new() -> Self {
        Self {
            room_live: broadcast::channel(64).0,
            room_pending: broadcast::channel(64).0,
            room_ended: broadcast::channel(64).0,
            stream_key_assigned: broadcast::channel(64).0,
            stream_key_removed: broadcast::channel(64).0,
            delivery_mode_changed: broadcast::channel(64).0,
            file_shared: broadcast::channel(64).0,
            file_unshared: broadcast::channel(64).0,
            participant_kicked: broadcast::channel(64).0,
            host_revoked: broadcast::channel(64).0,
            moderation_changed: broadcast::channel(64).0,
            conference_unmute: broadcast::channel(64).0,
        }
    }
}

impl Default for EventChannels {
    fn default() -> Self {
        Self::new()
    }
}
