//! Room/participant state gates that several endpoints disagreed about.
//!
//! `watch.rs` filters ended and expired rooms in SQL; the LiveKit token and the
//! WS auth query did not. And presenter-gated moderation checked `role` without
//! checking `is_kicked`, so a kicked presenter's token still worked.

mod common;

use axum::http::header;
use serde_json::Value;

fn auth_val(token: &str) -> axum::http::HeaderValue {
    format!("Bearer {}", token).parse().unwrap()
}

fn set_room(
    state: &std::sync::Arc<stream_backend::state::AppState>,
    room_id: &str,
    col: &str,
    val: &str,
) {
    let conn = state.db.get().unwrap();
    conn.execute(
        &format!("UPDATE rooms SET {col} = ?1 WHERE id = ?2"),
        rusqlite::params![val, room_id],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Ended / expired rooms
// ---------------------------------------------------------------------------

/// An ended room must not keep minting LiveKit tokens — that is a live A/V
/// grant for a session that is over.
#[tokio::test]
async fn livekit_token_refused_for_ended_room() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Done", "lk-ended");
    let (pid, tok) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);
    set_room(&state, &room_id, "status", "ended");

    let server = common::test_app(state);
    let res = server
        .get(&format!(
            "/api/public/rooms/lk-ended/livekit-token?participantId={pid}&token={tok}"
        ))
        .await;
    assert_ne!(
        res.status_code(),
        200,
        "ended room still handed out a LiveKit token"
    );
}

/// Same for a room past its expiry, which the expiry poller has not yet swept.
#[tokio::test]
async fn livekit_token_refused_for_expired_room() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Old", "lk-expired");
    let (pid, tok) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);
    set_room(&state, &room_id, "expires_at", "2000-01-01 00:00:00");

    let server = common::test_app(state);
    let res = server
        .get(&format!(
            "/api/public/rooms/lk-expired/livekit-token?participantId={pid}&token={tok}"
        ))
        .await;
    assert_ne!(
        res.status_code(),
        200,
        "expired room still handed out a LiveKit token"
    );
}

/// A live room must of course still work — the guard above must not be a
/// blanket refusal.
#[tokio::test]
async fn livekit_token_issued_for_live_room() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Live", "lk-live");
    let (pid, tok) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);

    let server = common::test_app(state);
    let res = server
        .get(&format!(
            "/api/public/rooms/lk-live/livekit-token?participantId={pid}&token={tok}"
        ))
        .await;
    assert_eq!(res.status_code(), 200);
    assert!(res.json::<Value>().get("token").is_some());
}

// ---------------------------------------------------------------------------
// Kicked presenters
// ---------------------------------------------------------------------------

/// Kicking a presenter must actually strip their powers. `role` survives a
/// kick, so a check that reads role alone leaves the moderation surface open.
#[tokio::test]
async fn kicked_presenter_cannot_moderate() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Mod", "kicked-host");
    let (host, host_tok) =
        common::seed_participant(&state, &room_id, "Host", "presenter", true, true);
    let (victim, _) = common::seed_participant(&state, &room_id, "Ana", "viewer", false, false);

    let server = common::test_app(state);

    let waiting = server
        .get(&format!(
            "/api/public/rooms/kicked-host/conference/waiting?participantId={host}&token={host_tok}"
        ))
        .await;
    assert_eq!(
        waiting.status_code(),
        403,
        "kicked presenter read the waiting list"
    );

    let admit = server
        .post(&format!(
            "/api/public/rooms/kicked-host/conference/admit/{victim}"
        ))
        .json(&serde_json::json!({ "participantId": host, "token": host_tok }))
        .await;
    assert_eq!(
        admit.status_code(),
        403,
        "kicked presenter admitted someone"
    );

    let kick = server
        .post("/api/public/rooms/kicked-host/conference/kick")
        .json(&serde_json::json!({
            "participantId": host, "token": host_tok, "targetId": victim
        }))
        .await;
    assert_eq!(kick.status_code(), 403, "kicked presenter kicked someone");
}

/// An un-kicked presenter still moderates normally.
#[tokio::test]
async fn active_presenter_can_still_moderate() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Mod", "active-host");
    let (host, host_tok) =
        common::seed_participant(&state, &room_id, "Host", "presenter", true, false);
    let (waiter, _) = common::seed_participant(&state, &room_id, "Ana", "viewer", false, false);

    let server = common::test_app(state);
    let waiting = server
        .get(&format!(
            "/api/public/rooms/active-host/conference/waiting?participantId={host}&token={host_tok}"
        ))
        .await;
    assert_eq!(waiting.status_code(), 200);

    let admit = server
        .post(&format!(
            "/api/public/rooms/active-host/conference/admit/{waiter}"
        ))
        .json(&serde_json::json!({ "participantId": host, "token": host_tok }))
        .await;
    assert_eq!(admit.status_code(), 200);
}

// ---------------------------------------------------------------------------
// Slugs
// ---------------------------------------------------------------------------

/// Every non-ASCII-alphanumeric character maps to '-', which empty segments
/// then drop — so a wholly non-Latin name produced a bare "-a1b2c3".
#[tokio::test]
async fn non_latin_room_names_get_a_usable_slug() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    for name in ["日本語", "Привет", "Ελλάδα", "!!!"] {
        let res = server
            .post("/api/rooms")
            .add_header(header::AUTHORIZATION, auth_val(&token))
            .json(&serde_json::json!({ "name": name }))
            .await;
        assert_eq!(res.status_code(), 200);
        let slug = res.json::<Value>()["slug"].as_str().unwrap().to_string();
        assert!(
            !slug.starts_with('-'),
            "slug for {name:?} starts with a dash: {slug:?}"
        );
        assert!(
            slug.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric()),
            "slug for {name:?} does not start with an alphanumeric: {slug:?}"
        );
    }
}

/// A Latin name still yields a readable slug — the fallback must not swallow
/// the normal case.
#[tokio::test]
async fn latin_room_names_keep_their_slug() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let res = server
        .post("/api/rooms")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({ "name": "Grade Review" }))
        .await;
    let slug = res.json::<Value>()["slug"].as_str().unwrap().to_string();
    assert!(
        slug.starts_with("grade-review-"),
        "unexpected slug {slug:?}"
    );
}

// ---------------------------------------------------------------------------
// Stream-key deletion
// ---------------------------------------------------------------------------

/// Deleting a key detaches it from its rooms via ON DELETE SET NULL, but that
/// is invisible to anyone already watching: they keep a player pointed at a
/// stream that no longer exists. `update_room` announces a manual detach; this
/// path announced nothing, and left the room stuck at `live` because the OME
/// reconciler's demote query inner-joins `stream_keys`.
#[tokio::test]
async fn deleting_a_key_demotes_and_announces_its_rooms() {
    let state = common::test_state();
    let (key_id, _) = common::seed_stream_key(&state, "Key");
    let room_id = common::seed_room_full(&state, "Show", "key-del", "live", false, Some(&key_id));

    let mut pending = state.events.room_pending.subscribe();
    let mut removed = state.events.stream_key_removed.subscribe();

    let token = common::admin_token(&state);
    let server = common::test_app(state.clone());
    let res = server
        .delete(&format!("/api/stream-keys/{key_id}"))
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 200);

    assert_eq!(
        pending.try_recv().ok().as_deref(),
        Some("key-del"),
        "no room:pending emitted for a room whose key was deleted"
    );
    assert_eq!(
        removed.try_recv().ok().as_deref(),
        Some("key-del"),
        "no stream:removed emitted for a room whose key was deleted"
    );

    let conn = state.db.get().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM rooms WHERE id = ?1",
            rusqlite::params![room_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending", "room left stuck at 'live' with no key");
}
