mod common;

use std::collections::HashSet;
use stream_backend::tasks::reconcile_rooms;

/// The set OME's `/v1/.../streams` returns, as the poller builds it.
fn active(keys: &[&str]) -> HashSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

fn room_status(state: &std::sync::Arc<stream_backend::state::AppState>, id: &str) -> String {
    state
        .db
        .get()
        .unwrap()
        .query_row(
            "SELECT status FROM rooms WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap()
}

fn block_key(state: &std::sync::Arc<stream_backend::state::AppState>, key_id: &str) {
    state
        .db
        .get()
        .unwrap()
        .execute(
            "UPDATE stream_keys SET blocked = 1 WHERE id = ?1",
            rusqlite::params![key_id],
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// Promote — the gh #225 gap. The admission webhook only fires when a publisher
// starts, so a key attached to an already-broadcasting stream never triggers it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pending_room_goes_live_when_its_key_is_already_broadcasting() {
    let state = common::test_state();
    let (sk_id, key_token) = common::seed_stream_key(&state, "Key");
    let room_id =
        common::seed_room_full(&state, "Room", "room-225", "pending", false, Some(&sk_id));

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&[&key_token])).unwrap();

    assert_eq!(room_status(&state, &room_id), "live");
    assert_eq!(t.went_live.len(), 1, "should report the promotion");
    assert!(t.went_pending.is_empty());
}

#[tokio::test]
async fn pending_room_stays_pending_when_its_key_is_not_broadcasting() {
    let state = common::test_state();
    let (sk_id, _key_token) = common::seed_stream_key(&state, "Key");
    let room_id =
        common::seed_room_full(&state, "Room", "room-idle", "pending", false, Some(&sk_id));

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&["some-other-key"])).unwrap();

    assert_eq!(room_status(&state, &room_id), "pending");
    assert!(t.went_live.is_empty());
}

/// The kick flow sets `blocked = 1` *before* asking OME to drop the stream
/// (`routes/ome.rs`), so there is a window where the key is blocked but still
/// in OME's list. The poller must not undo the kick during that window.
#[tokio::test]
async fn blocked_key_does_not_promote_a_pending_room() {
    let state = common::test_state();
    let (sk_id, key_token) = common::seed_stream_key(&state, "Kicked Key");
    let room_id = common::seed_room_full(
        &state,
        "Room",
        "room-kicked",
        "pending",
        false,
        Some(&sk_id),
    );
    block_key(&state, &sk_id);

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&[&key_token])).unwrap();

    assert_eq!(
        room_status(&state, &room_id),
        "pending",
        "a kicked key must not re-promote its room"
    );
    assert!(t.went_live.is_empty());
}

/// Scheduled rooms must not start early, and ended rooms must stay ended, even
/// while their key is broadcasting.
#[tokio::test]
async fn scheduled_and_ended_rooms_are_never_promoted() {
    let state = common::test_state();
    let (sk_id, key_token) = common::seed_stream_key(&state, "Key");
    let scheduled = common::seed_room_full(
        &state,
        "Scheduled",
        "room-sched",
        "scheduled",
        false,
        Some(&sk_id),
    );
    let ended = common::seed_room_full(&state, "Ended", "room-ended", "ended", false, Some(&sk_id));

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&[&key_token])).unwrap();

    assert_eq!(room_status(&state, &scheduled), "scheduled");
    assert_eq!(room_status(&state, &ended), "ended");
    assert!(t.went_live.is_empty());
}

/// Several rooms can share one stream key; the webhook promotes all of them, so
/// the poller must too.
#[tokio::test]
async fn all_rooms_sharing_a_broadcasting_key_are_promoted() {
    let state = common::test_state();
    let (sk_id, key_token) = common::seed_stream_key(&state, "Shared Key");
    let a = common::seed_room_full(&state, "A", "room-a", "pending", false, Some(&sk_id));
    let b = common::seed_room_full(&state, "B", "room-b", "pending", false, Some(&sk_id));

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&[&key_token])).unwrap();

    assert_eq!(room_status(&state, &a), "live");
    assert_eq!(room_status(&state, &b), "live");
    assert_eq!(t.went_live.len(), 2);
}

// ---------------------------------------------------------------------------
// Demote — pre-existing behaviour, kept green by these tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_room_drops_to_pending_when_its_stream_stops() {
    let state = common::test_state();
    let (sk_id, _key_token) = common::seed_stream_key(&state, "Key");
    let room_id = common::seed_room_full(&state, "Room", "room-drop", "live", false, Some(&sk_id));

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&[])).unwrap();

    assert_eq!(room_status(&state, &room_id), "pending");
    assert_eq!(t.went_pending.len(), 1);
    assert!(t.went_live.is_empty());
}

/// A blocked key still demotes — the block only guards the promote side, so a
/// kicked stream's room must not stay stuck on 'live'.
#[tokio::test]
async fn live_room_with_a_blocked_key_still_drops_to_pending() {
    let state = common::test_state();
    let (sk_id, _key_token) = common::seed_stream_key(&state, "Kicked Key");
    let room_id = common::seed_room_full(
        &state,
        "Room",
        "room-kick-drop",
        "live",
        false,
        Some(&sk_id),
    );
    block_key(&state, &sk_id);

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&[])).unwrap();

    assert_eq!(room_status(&state, &room_id), "pending");
    assert_eq!(t.went_pending.len(), 1);
}

/// A room with no stream key attached is invisible to both directions.
#[tokio::test]
async fn room_without_a_stream_key_is_untouched() {
    let state = common::test_state();
    let room_id = common::seed_room_full(&state, "Room", "room-nokey", "pending", false, None);

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&["anything"])).unwrap();

    assert_eq!(room_status(&state, &room_id), "pending");
    assert!(t.went_live.is_empty() && t.went_pending.is_empty());
}

/// One pass must settle both directions at once: a stopped stream demotes while
/// a newly-attached key promotes.
#[tokio::test]
async fn one_pass_handles_both_directions() {
    let state = common::test_state();
    let (stopped_id, _stopped_token) = common::seed_stream_key(&state, "Stopped");
    let (running_id, running_token) = common::seed_stream_key(&state, "Running");

    let dropping = common::seed_room_full(
        &state,
        "Dropping",
        "room-dropping",
        "live",
        false,
        Some(&stopped_id),
    );
    let rising = common::seed_room_full(
        &state,
        "Rising",
        "room-rising",
        "pending",
        false,
        Some(&running_id),
    );

    let conn = state.db.get().unwrap();
    let t = reconcile_rooms(&conn, &active(&[&running_token])).unwrap();

    assert_eq!(room_status(&state, &dropping), "pending");
    assert_eq!(room_status(&state, &rising), "live");
    assert_eq!(t.went_pending.len(), 1);
    assert_eq!(t.went_live.len(), 1);
}
