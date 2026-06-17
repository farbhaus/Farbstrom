mod common;

use axum::http::header;
use serde_json::{json, Value};

fn auth_header(token: &str) -> (axum::http::HeaderName, axum::http::HeaderValue) {
    (
        header::AUTHORIZATION,
        format!("Bearer {}", token).parse().unwrap(),
    )
}

// ---------------------------------------------------------------------------
// List rooms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_rooms_returns_401_without_auth() {
    let state = common::test_state();
    let server = common::test_app(state);

    let res = server.get("/api/rooms").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn list_rooms_returns_empty() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let res = server.get("/api/rooms").add_header(name, val).await;
    assert_eq!(res.status_code(), 200);

    let body: Vec<Value> = res.json();
    assert!(body.is_empty());
}

// ---------------------------------------------------------------------------
// Create room
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_room_returns_400_without_name() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let res = server
        .post("/api/rooms")
        .add_header(name, val)
        .json(&json!({}))
        .await;
    assert_eq!(res.status_code(), 400);
}

#[tokio::test]
async fn create_room_succeeds() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let res = server
        .post("/api/rooms")
        .add_header(name, val)
        .json(&json!({ "name": "Test Room" }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert!(body.get("id").is_some());
    assert!(body.get("slug").is_some());
    assert_eq!(body["status"], "pending");
    assert_eq!(body["delivery_mode"], "webrtc");
    assert!(body.get("presenter_key").is_some());
    assert_eq!(body["name"], "Test Room");
    // Participant-audio defaults are ON unless the admin opts out.
    assert_eq!(body["noise_reduction"], 1);
    assert_eq!(body["echo_cancellation"], 1);
    // Push-to-talk defaults OFF.
    assert_eq!(body["push_to_talk"], 0);
}

#[tokio::test]
async fn create_room_push_to_talk_round_trips() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    // Opt the room into push-to-talk at creation.
    let res = server
        .post("/api/rooms")
        .add_header(name.clone(), val.clone())
        .json(&json!({
            "name": "PTT Room",
            "push_to_talk": true,
        }))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    let room_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["push_to_talk"], 1);
    // Audio defaults are untouched (still ON).
    assert_eq!(body["noise_reduction"], 1);

    // Persisted.
    let res = server
        .get(&format!("/api/rooms/{}", room_id))
        .add_header(name.clone(), val.clone())
        .await;
    assert_eq!(res.json::<Value>()["push_to_talk"], 1);

    // Turning it back off via update leaves the audio settings alone.
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(name, val)
        .json(&json!({ "push_to_talk": false }))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["push_to_talk"], 0);
    assert_eq!(body["noise_reduction"], 1);
}

#[tokio::test]
async fn create_room_audio_defaults_off_round_trips() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let res = server
        .post("/api/rooms")
        .add_header(name.clone(), val.clone())
        .json(&json!({
            "name": "Quiet Room",
            "noise_reduction": false,
            "echo_cancellation": false,
        }))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    let room_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["noise_reduction"], 0);
    assert_eq!(body["echo_cancellation"], 0);

    // Re-fetch to confirm it persisted.
    let res = server
        .get(&format!("/api/rooms/{}", room_id))
        .add_header(name.clone(), val.clone())
        .await;
    let body: Value = res.json();
    assert_eq!(body["noise_reduction"], 0);
    assert_eq!(body["echo_cancellation"], 0);

    // Toggling one back on via update leaves the other untouched.
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(name, val)
        .json(&json!({ "noise_reduction": true }))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["noise_reduction"], 1);
    assert_eq!(body["echo_cancellation"], 0);
}

// ---------------------------------------------------------------------------
// Get room
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_room_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let res = server
        .get("/api/rooms/nonexistent-id")
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn get_room_succeeds() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "My Room", "my-room-abc123");

    let (name, val) = auth_header(&token);
    let res = server
        .get(&format!("/api/rooms/{}", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["id"], room_id);
    assert_eq!(body["name"], "My Room");
    assert_eq!(body["slug"], "my-room-abc123");
}

// ---------------------------------------------------------------------------
// Update room
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_room_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let res = server
        .put("/api/rooms/nonexistent-id")
        .add_header(name, val)
        .json(&json!({ "name": "New Name" }))
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn update_room_changes_name() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Old Name", "old-name-abc123");

    let (name, val) = auth_header(&token);
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(name, val)
        .json(&json!({ "name": "New Name" }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["name"], "New Name");
}

// ---------------------------------------------------------------------------
// End room
// ---------------------------------------------------------------------------

#[tokio::test]
async fn end_room_succeeds() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Live Room", "live-room-abc123");

    let (name, val) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/end", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["ok"], true);

    // Verify status changed to ended
    let (name2, val2) = auth_header(&token);
    let res2 = server
        .get(&format!("/api/rooms/{}", room_id))
        .add_header(name2, val2)
        .await;
    let room: Value = res2.json();
    assert_eq!(room["status"], "ended");
}

// ---------------------------------------------------------------------------
// Reactivate room
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reactivate_room_resets_status_and_clears_expiry() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Old Room", "old-room-abc123");
    // Mark ended with a past expires_at and ended_at set.
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "UPDATE rooms SET status='ended', ended_at=CURRENT_TIMESTAMP, \
             expires_at='2020-01-01 00:00:00' WHERE id=?1",
            rusqlite::params![room_id],
        )
        .unwrap();
    }

    let (name, val) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/reactivate", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["status"], "pending");
    assert!(body["ended_at"].is_null());
    assert!(body["expires_at"].is_null());
}

#[tokio::test]
async fn reactivate_room_rejects_non_ended() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Pending Room", "pending-room-abc123");

    let (name, val) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/reactivate", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 400);
}

#[tokio::test]
async fn reactivate_room_returns_404_for_missing() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let (name, val) = auth_header(&token);
    let res = server
        .post("/api/rooms/missing-id/reactivate")
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn reactivate_room_requires_auth() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let room_id = common::seed_room(&state, "Auth Room", "auth-room-abc123");

    let res = server
        .post(&format!("/api/rooms/{}/reactivate", room_id))
        .await;
    assert_eq!(res.status_code(), 401);
}

// ---------------------------------------------------------------------------
// Delete room
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_room_succeeds() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Delete Me", "delete-me-abc123");

    let (name, val) = auth_header(&token);
    let res = server
        .delete(&format!("/api/rooms/{}", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn delete_room_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let res = server
        .delete("/api/rooms/nonexistent-id")
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 404);
}

// ---------------------------------------------------------------------------
// Waiting list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn waiting_list_empty() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Wait Room", "wait-room-abc123");

    let (name, val) = auth_header(&token);
    let res = server
        .get(&format!("/api/rooms/{}/waiting", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Vec<Value> = res.json();
    assert!(body.is_empty());
}

// ---------------------------------------------------------------------------
// Admit participant
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admit_participant() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Admit Room", "admit-room-abc123");
    let (pid, _ptok) = common::seed_participant(
        &state,
        &room_id,
        "Waiting User",
        "viewer",
        false, // not admitted
        false, // not kicked
    );

    let (name, val) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/admit/{}", room_id, pid))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["ok"], true);
}

// ---------------------------------------------------------------------------
// Admit all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admit_all() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Admit All Room", "admit-all-abc123");
    let (_p1, _) = common::seed_participant(&state, &room_id, "User1", "viewer", false, false);
    let (_p2, _) = common::seed_participant(&state, &room_id, "User2", "viewer", false, false);

    let (name, val) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/admit-all", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["ok"], true);
}

// ---------------------------------------------------------------------------
// Kicked list and unkick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kicked_list_and_unkick() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Kick Room", "kick-room-abc123");
    let (pid, _ptok) = common::seed_participant(
        &state,
        &room_id,
        "Kicked User",
        "viewer",
        true, // admitted
        true, // kicked
    );

    // GET kicked list
    let (name, val) = auth_header(&token);
    let res = server
        .get(&format!("/api/rooms/{}/kicked", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Vec<Value> = res.json();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["name"], "Kicked User");

    // POST unkick
    let (name2, val2) = auth_header(&token);
    let res2 = server
        .post(&format!("/api/rooms/{}/unkick/{}", room_id, pid))
        .add_header(name2, val2)
        .await;
    assert_eq!(res2.status_code(), 200);

    let body2: Value = res2.json();
    assert_eq!(body2["ok"], true);

    // Verify kicked list is now empty
    let (name3, val3) = auth_header(&token);
    let res3 = server
        .get(&format!("/api/rooms/{}/kicked", room_id))
        .add_header(name3, val3)
        .await;
    let body3: Vec<Value> = res3.json();
    assert!(body3.is_empty());
}

// ---------------------------------------------------------------------------
// Rotate presenter key
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotate_presenter_key_requires_admin() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let room_id = common::seed_room(&state, "Rotate Room", "rotate-room-abc123");

    let res = server
        .post(&format!("/api/rooms/{}/rotate-presenter-key", room_id))
        .await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn rotate_presenter_key_changes_key_and_invalidates_old_link() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Rotate Invalidate", "rotate-inv-abc123");
    let old_key = common::get_room_presenter_key(&state, &room_id);

    // Confirm old key works first.
    let res_pre = server
        .post("/api/public/rooms/rotate-inv-abc123/join")
        .json(&json!({
            "name": "Pre Rotate",
            "role": "presenter",
            "presenter_key": old_key,
        }))
        .await;
    assert_eq!(res_pre.status_code(), 200);
    assert_eq!(res_pre.json::<Value>()["role"], "presenter");

    // Rotate.
    let (h, v) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/rotate-presenter-key", room_id))
        .add_header(h, v)
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    let new_key = body["presenter_key"].as_str().unwrap().to_string();
    assert_ne!(new_key, old_key);
    assert_eq!(new_key.len(), 32);

    // Old key now downgrades to viewer.
    let res_old = server
        .post("/api/public/rooms/rotate-inv-abc123/join")
        .json(&json!({
            "name": "Stale Link",
            "role": "presenter",
            "presenter_key": old_key,
        }))
        .await;
    assert_eq!(res_old.status_code(), 200);
    assert_eq!(res_old.json::<Value>()["role"], "viewer");

    // New key grants presenter.
    let res_new = server
        .post("/api/public/rooms/rotate-inv-abc123/join")
        .json(&json!({
            "name": "Fresh Link",
            "role": "presenter",
            "presenter_key": new_key,
        }))
        .await;
    assert_eq!(res_new.status_code(), 200);
    assert_eq!(res_new.json::<Value>()["role"], "presenter");
}

#[tokio::test]
async fn rotate_presenter_key_returns_404_for_missing_room() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (h, v) = auth_header(&token);

    let res = server
        .post("/api/rooms/no-such-room/rotate-presenter-key")
        .add_header(h, v)
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn rotate_presenter_key_deletes_existing_presenters() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Rotate Drop", "rotate-drop-abc123");
    let (pres_id, _) =
        common::seed_participant(&state, &room_id, "Old Host", "presenter", true, false);
    let (viewer_id, _) =
        common::seed_participant(&state, &room_id, "Old Viewer", "viewer", true, false);

    let (h, v) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/rotate-presenter-key", room_id))
        .add_header(h, v)
        .await;
    assert_eq!(res.status_code(), 200);

    let conn = state.db.get().unwrap();
    let pres_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM participants WHERE id = ?1",
            rusqlite::params![pres_id],
            |row| row.get(0),
        )
        .unwrap();
    let viewer_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM participants WHERE id = ?1",
            rusqlite::params![viewer_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pres_exists, 0, "presenter should be removed");
    assert_eq!(viewer_exists, 1, "viewer should remain");
}

// ---------------------------------------------------------------------------
// Detaching a stream key resets a live room (GitHub #173)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn detaching_stream_key_resets_live_room_to_pending() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let (key_id, _) = common::seed_stream_key(&state, "Key A");
    let room_id = common::seed_room(&state, "Live Room", "live-room-abc123");
    // Attach the key, then force the room live as the webhook would.
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "UPDATE rooms SET stream_key_id=?1, status='live' WHERE id=?2",
            rusqlite::params![key_id, room_id],
        )
        .unwrap();
    }

    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(name, val)
        .json(&json!({ "stream_key_id": null }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert!(body["stream_key_id"].is_null());
    assert_eq!(body["status"], "pending");
}

#[tokio::test]
async fn detaching_stream_key_leaves_ended_room_ended() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let (name, val) = auth_header(&token);

    let (key_id, _) = common::seed_stream_key(&state, "Key B");
    let room_id = common::seed_room(&state, "Ended Room", "ended-room-abc123");
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "UPDATE rooms SET stream_key_id=?1, status='ended', \
             ended_at=CURRENT_TIMESTAMP WHERE id=?2",
            rusqlite::params![key_id, room_id],
        )
        .unwrap();
    }

    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(name, val)
        .json(&json!({ "stream_key_id": null }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert!(body["stream_key_id"].is_null());
    assert_eq!(body["status"], "ended");
}
