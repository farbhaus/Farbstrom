//! In-memory presence registry for native SRT (Farbplay) viewers.
//!
//! Browser viewers signal presence by holding a WebSocket open (tracked in
//! `ws::WS_ROOMS`). Native SRT viewers instead hold the admission SSE
//! (`GET /api/public/rooms/:slug/waiting/events/:pid`) open for the entire
//! session per the Farbplay contract, so that connection *is* their heartbeat:
//! registered while the SSE stream is alive, removed when it drops (quit /
//! network loss). Current Farbplay builds also open a WS (pointer
//! collaboration, gh #227) and are marked there with `client: "farbplay"`, but
//! the SSE remains the heartbeat every build holds.
//!
//! The host roster's "admitted" list is scoped to the IDs present here so it
//! shows only currently-connected SRT viewers (see `ws.rs` `moderation:update`).
//!
//! A plain `std::sync::Mutex` is used (not `tokio`) because the critical
//! sections are tiny and never span an `.await`, and `remove` must be callable
//! from a synchronous `Drop` guard.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

// slug -> (participant_id -> open SSE connection count). A refcount tolerates a
// client briefly holding two streams across a reconnect.
type Presence = HashMap<String, HashMap<String, u32>>;

static SSE_PRESENCE: LazyLock<Mutex<Presence>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register one open SSE connection for `(slug, participant_id)`.
pub fn add(slug: &str, participant_id: &str) {
    let mut map = SSE_PRESENCE.lock().expect("SSE_PRESENCE poisoned");
    *map.entry(slug.to_string())
        .or_default()
        .entry(participant_id.to_string())
        .or_insert(0) += 1;
}

/// Deregister one open SSE connection. The participant is considered gone once
/// its connection count reaches zero.
pub fn remove(slug: &str, participant_id: &str) {
    let mut map = SSE_PRESENCE.lock().expect("SSE_PRESENCE poisoned");
    if let Some(room) = map.get_mut(slug) {
        if let Some(count) = room.get_mut(participant_id) {
            *count -= 1;
            if *count == 0 {
                room.remove(participant_id);
            }
        }
        if room.is_empty() {
            map.remove(slug);
        }
    }
}

/// Snapshot of participant IDs with at least one open SSE connection in `slug`.
pub fn present_ids(slug: &str) -> HashSet<String> {
    let map = SSE_PRESENCE.lock().expect("SSE_PRESENCE poisoned");
    map.get(slug)
        .map(|room| room.keys().cloned().collect())
        .unwrap_or_default()
}
