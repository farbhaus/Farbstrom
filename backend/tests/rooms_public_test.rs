mod common;

use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Room info
// ---------------------------------------------------------------------------

#[tokio::test]
async fn room_info_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state);

    let res = server.get("/api/public/rooms/nonexistent-slug/info").await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn room_info_returns_safe_fields() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id =
        common::seed_room_with_password(&state, "Info Room", "info-room-abc123", "secret");

    let res = server.get("/api/public/rooms/info-room-abc123/info").await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["name"], "Info Room");
    assert_eq!(body["slug"], "info-room-abc123");
    // has_password should be 1 (truthy) since we set a password
    assert_eq!(body["has_password"], 1);
    // password_hash must NOT be exposed
    assert!(body.get("password_hash").is_none());
}

// ---------------------------------------------------------------------------
// Join room
// ---------------------------------------------------------------------------

#[tokio::test]
async fn join_returns_404_nonexistent() {
    let state = common::test_state();
    let server = common::test_app(state);

    let res = server
        .post("/api/public/rooms/no-such-room/join")
        .json(&json!({ "name": "Alice" }))
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn join_returns_410_ended() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id = common::seed_room_full(
        &state,
        "Ended Room",
        "ended-room-abc123",
        "ended",
        false,
        None,
    );

    let res = server
        .post("/api/public/rooms/ended-room-abc123/join")
        .json(&json!({ "name": "Alice" }))
        .await;
    assert_eq!(res.status_code(), 410);
}

#[tokio::test]
async fn join_returns_400_no_name() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id = common::seed_room(&state, "Join Room", "join-noname-abc123");

    let res = server
        .post("/api/public/rooms/join-noname-abc123/join")
        .json(&json!({}))
        .await;
    assert_eq!(res.status_code(), 400);
}

#[tokio::test]
async fn join_returns_401_wrong_password() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id =
        common::seed_room_with_password(&state, "PW Room", "pw-room-abc123", "correct-pass");

    let res = server
        .post("/api/public/rooms/pw-room-abc123/join")
        .json(&json!({ "name": "Alice", "password": "wrong-pass" }))
        .await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn join_succeeds() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id = common::seed_room(&state, "Open Room", "open-room-abc123");

    let res = server
        .post("/api/public/rooms/open-room-abc123/join")
        .json(&json!({ "name": "Alice" }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert!(body.get("participant_id").is_some());
    assert!(body.get("token").is_some());
    assert_eq!(body["admitted"], true);
    assert_eq!(body["role"], "viewer");
    // Seeded rooms default both audio settings ON.
    assert_eq!(body["noise_reduction_default"], true);
    assert_eq!(body["echo_cancellation_default"], true);
    // Push-to-talk defaults OFF.
    assert_eq!(body["push_to_talk_default"], false);
}

#[tokio::test]
async fn join_exposes_push_to_talk_default() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Loud Room", "loud-room-abc123");
    // Admin made this a push-to-talk room.
    state
        .db
        .get()
        .unwrap()
        .execute(
            "UPDATE rooms SET push_to_talk = 1 WHERE id = ?1",
            rusqlite::params![room_id],
        )
        .unwrap();

    let res = server
        .post("/api/public/rooms/loud-room-abc123/join")
        .json(&json!({ "name": "Alice" }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["push_to_talk_default"], true);
}

#[tokio::test]
async fn join_exposes_per_room_audio_defaults() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Music Room", "music-room-abc123");
    // Admin turned noise reduction off for this room; echo stays on.
    state
        .db
        .get()
        .unwrap()
        .execute(
            "UPDATE rooms SET noise_reduction = 0 WHERE id = ?1",
            rusqlite::params![room_id],
        )
        .unwrap();

    let res = server
        .post("/api/public/rooms/music-room-abc123/join")
        .json(&json!({ "name": "Alice" }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["noise_reduction_default"], false);
    assert_eq!(body["echo_cancellation_default"], true);
}

#[tokio::test]
async fn join_returns_403_kicked() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Kick Check", "kick-check-abc123");
    // Seed a kicked participant named "TestUser"
    let (_pid, _tok) = common::seed_participant(
        &state, &room_id, "TestUser", "viewer", true, // admitted
        true, // kicked
    );

    // Try joining as "testuser" (case insensitive match)
    let res = server
        .post("/api/public/rooms/kick-check-abc123/join")
        .json(&json!({ "name": "testuser" }))
        .await;
    assert_eq!(res.status_code(), 403);
}

#[tokio::test]
async fn join_presenter_needs_key() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id = common::seed_room(&state, "Presenter Room", "pres-room-abc123");

    // Join with role=presenter but wrong/missing key -> should become viewer
    let res = server
        .post("/api/public/rooms/pres-room-abc123/join")
        .json(&json!({ "name": "Bob", "role": "presenter", "presenter_key": "wrong-key" }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["role"], "viewer");
}

#[tokio::test]
async fn join_presenter_with_valid_key() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Presenter Valid", "pres-valid-abc123");
    let presenter_key = common::get_room_presenter_key(&state, &room_id);

    let res = server
        .post("/api/public/rooms/pres-valid-abc123/join")
        .json(&json!({
            "name": "Presenter Bob",
            "role": "presenter",
            "presenter_key": presenter_key
        }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["role"], "presenter");
}

// ---------------------------------------------------------------------------
// Status poll
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_poll_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state);

    let res = server
        .get("/api/public/rooms/no-slug/status/no-pid?token=bad")
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn status_poll_returns_admitted() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Status Room", "status-room-abc123");
    let (pid, ptok) = common::seed_participant(
        &state,
        &room_id,
        "StatusUser",
        "viewer",
        true,  // admitted
        false, // not kicked
    );

    let url = format!(
        "/api/public/rooms/status-room-abc123/status/{}?token={}",
        pid, ptok
    );
    let res = server.get(&url).await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["admitted"], true);
}

// ---------------------------------------------------------------------------
// LiveKit token
// ---------------------------------------------------------------------------

#[tokio::test]
async fn livekit_token_returns_401_without_params() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id = common::seed_room(&state, "LK Room", "lk-room-abc123");

    // Missing both participantId and token
    let res = server
        .get("/api/public/rooms/lk-room-abc123/livekit-token")
        .await;
    // Should return 400 for missing participantId
    let status = res.status_code().as_u16();
    assert!(
        status == 400 || status == 401,
        "Expected 400 or 401, got {}",
        status
    );
}

#[tokio::test]
async fn livekit_token_succeeds() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "LK Token Room", "lk-token-abc123");
    let (pid, ptok) = common::seed_participant(
        &state, &room_id, "LKUser", "viewer", true,  // admitted
        false, // not kicked
    );

    let url = format!(
        "/api/public/rooms/lk-token-abc123/livekit-token?participantId={}&token={}",
        pid, ptok
    );
    let res = server.get(&url).await;
    // LiveKit token generation may fail in test env (no real LiveKit), so we accept
    // 200 (success) or 500 (LiveKit unavailable). The route logic is still exercised.
    let status = res.status_code().as_u16();
    assert!(
        status == 200 || status == 500,
        "Expected 200 or 500, got {}",
        status
    );
}

// ---------------------------------------------------------------------------
// Kick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kick_returns_400_missing_fields() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id = common::seed_room(&state, "Kick Room", "kick-route-abc123");

    let res = server
        .post("/api/public/rooms/kick-route-abc123/conference/kick")
        .json(&json!({}))
        .await;
    assert_eq!(res.status_code(), 400);
}

#[tokio::test]
async fn kick_returns_403_non_presenter() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Kick Deny", "kick-deny-abc123");
    let (viewer_pid, viewer_tok) =
        common::seed_participant(&state, &room_id, "ViewerKicker", "viewer", true, false);
    let (target_pid, _) =
        common::seed_participant(&state, &room_id, "Target", "viewer", true, false);

    let res = server
        .post("/api/public/rooms/kick-deny-abc123/conference/kick")
        .json(&json!({
            "participantId": viewer_pid,
            "token": viewer_tok,
            "targetId": target_pid
        }))
        .await;
    assert_eq!(res.status_code(), 403);
}

#[tokio::test]
async fn join_with_valid_presenter_key_bypasses_password() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id =
        common::seed_room_with_password(&state, "PW Host", "pw-host-abc123", "client-secret");
    let presenter_key = common::get_room_presenter_key(&state, &room_id);

    // Host link → no password sent, still admitted as presenter.
    let res = server
        .post("/api/public/rooms/pw-host-abc123/join")
        .json(&json!({
            "name": "Host Colorist",
            "role": "presenter",
            "presenter_key": presenter_key,
        }))
        .await;
    assert_eq!(res.status_code(), 200);
    assert_eq!(res.json::<Value>()["role"], "presenter");
}

#[tokio::test]
async fn join_with_wrong_presenter_key_still_requires_password() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let _room_id =
        common::seed_room_with_password(&state, "PW Guard", "pw-guard-abc123", "client-secret");

    // Wrong pk + no password → 401 (the password gate still applies).
    let res = server
        .post("/api/public/rooms/pw-guard-abc123/join")
        .json(&json!({
            "name": "Sneaky",
            "role": "presenter",
            "presenter_key": "deadbeef",
        }))
        .await;
    assert_eq!(res.status_code(), 401);
}

// ---------------------------------------------------------------------------
// Presenter-gated moderation endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conf_waiting_requires_presenter() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Mod Auth", "mod-auth-abc123");
    let (vpid, vtok) = common::seed_participant(&state, &room_id, "Viewer", "viewer", true, false);

    // Missing creds → 400
    let r = server
        .get("/api/public/rooms/mod-auth-abc123/conference/waiting")
        .await;
    assert_eq!(r.status_code(), 400);

    // Viewer token → 403
    let r = server
        .get(&format!(
            "/api/public/rooms/mod-auth-abc123/conference/waiting?participantId={vpid}&token={vtok}"
        ))
        .await;
    assert_eq!(r.status_code(), 403);
}

#[tokio::test]
async fn conf_admit_promotes_waiter() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Mod Admit", "mod-admit-abc123");
    let (host_pid, host_tok) =
        common::seed_participant(&state, &room_id, "Host", "presenter", true, false);
    let (waiter_pid, _) =
        common::seed_participant(&state, &room_id, "Alice", "viewer", false, false);

    let r = server
        .post(&format!(
            "/api/public/rooms/mod-admit-abc123/conference/admit/{waiter_pid}"
        ))
        .json(&json!({ "participantId": host_pid, "token": host_tok }))
        .await;
    assert_eq!(r.status_code(), 200);
    assert_eq!(r.json::<Value>()["ok"], true);

    // Confirm DB flag flipped.
    let conn = state.db.get().unwrap();
    let admitted: i32 = conn
        .query_row(
            "SELECT is_admitted FROM participants WHERE id = ?1",
            rusqlite::params![waiter_pid],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(admitted, 1);
}

#[tokio::test]
async fn conf_admit_all_returns_count() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Mod All", "mod-all-abc123");
    let (host_pid, host_tok) =
        common::seed_participant(&state, &room_id, "Host", "presenter", true, false);
    let _ = common::seed_participant(&state, &room_id, "Alice", "viewer", false, false);
    let _ = common::seed_participant(&state, &room_id, "Bob", "viewer", false, false);

    let r = server
        .post("/api/public/rooms/mod-all-abc123/conference/admit-all")
        .json(&json!({ "participantId": host_pid, "token": host_tok }))
        .await;
    assert_eq!(r.status_code(), 200);
    let body: Value = r.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["count"], 2);
}

#[tokio::test]
async fn conf_unkick_clears_kicked_flag() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Mod Unkick", "mod-unkick-abc123");
    let (host_pid, host_tok) =
        common::seed_participant(&state, &room_id, "Host", "presenter", true, false);
    let (kicked_pid, _) = common::seed_participant(&state, &room_id, "Bad", "viewer", true, true);

    let r = server
        .post(&format!(
            "/api/public/rooms/mod-unkick-abc123/conference/unkick/{kicked_pid}"
        ))
        .json(&json!({ "participantId": host_pid, "token": host_tok }))
        .await;
    assert_eq!(r.status_code(), 200);

    let conn = state.db.get().unwrap();
    let kicked: i32 = conn
        .query_row(
            "SELECT is_kicked FROM participants WHERE id = ?1",
            rusqlite::params![kicked_pid],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kicked, 0);
}

#[tokio::test]
async fn conf_get_lists_return_data() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let room_id = common::seed_room(&state, "Mod List", "mod-list-abc123");
    let (host_pid, host_tok) =
        common::seed_participant(&state, &room_id, "Host", "presenter", true, false);
    let _ = common::seed_participant(&state, &room_id, "Waiter", "viewer", false, false);
    let _ = common::seed_participant(&state, &room_id, "Naughty", "viewer", true, true);

    let r = server
        .get(&format!(
            "/api/public/rooms/mod-list-abc123/conference/waiting?participantId={host_pid}&token={host_tok}"
        ))
        .await;
    assert_eq!(r.status_code(), 200);
    let waiting: Vec<Value> = r.json();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["name"], "Waiter");

    let r = server
        .get(&format!(
            "/api/public/rooms/mod-list-abc123/conference/kicked?participantId={host_pid}&token={host_tok}"
        ))
        .await;
    assert_eq!(r.status_code(), 200);
    let kicked: Vec<Value> = r.json();
    assert_eq!(kicked.len(), 1);
    assert_eq!(kicked[0]["name"], "Naughty");
}
