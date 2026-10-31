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
    assert_eq!(
        common::admin_room(&server, &token, &room_id).await["push_to_talk"],
        1
    );

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
    let body = common::admin_room(&server, &token, &room_id).await;
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

/// The room list carries what the admin UI reads per room — and, since the
/// hash stopped riding along, only whether a password is set.
#[tokio::test]
async fn room_list_says_whether_a_password_is_set_without_the_hash() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let open_id = common::seed_room(&state, "My Room", "my-room-abc123");
    let locked_id =
        common::seed_room_with_password(&state, "Locked", "locked-abc123", "hunter2-hunter2");

    let open = common::admin_room(&server, &token, &open_id).await;
    assert_eq!(open["name"], "My Room");
    assert_eq!(open["slug"], "my-room-abc123");
    assert_eq!(open["has_password"], 0);
    let locked = common::admin_room(&server, &token, &locked_id).await;
    assert_eq!(locked["has_password"], 1);
    for room in [&open, &locked] {
        assert!(room.get("password_hash").is_none(), "hash sent: {room}");
    }
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
    let room = common::admin_room(&server, &token, &room_id).await;
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

/// Rotating revokes the presenters the old link let in, and nobody else: not
/// viewers, and not the admin's own "Enter Room" session — which is a presenter
/// too, and used to be signed out of the room by the admin's own rotation.
#[tokio::test]
async fn rotate_presenter_key_revokes_host_link_presenters_only() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);

    let room_id = common::seed_room(&state, "Rotate Drop", "rotate-drop-abc123");
    let key = common::get_room_presenter_key(&state, &room_id);
    let host = server
        .post("/api/public/rooms/rotate-drop-abc123/join")
        .json(&json!({ "name": "Colorist", "role": "presenter", "presenter_key": key }))
        .await;
    assert_eq!(host.json::<Value>()["role"], "presenter");
    let host_id = host.json::<Value>()["participant_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (h, v) = auth_header(&token);
    let entered = server
        .post(&format!("/api/rooms/{}/enter", room_id))
        .add_header(h, v)
        .json(&json!({ "name": "Admin" }))
        .await;
    assert_eq!(entered.status_code(), 200);
    let admin_id = entered.json::<Value>()["participantId"]
        .as_str()
        .unwrap()
        .to_string();
    let (viewer_id, _) =
        common::seed_participant(&state, &room_id, "Old Viewer", "viewer", true, false);

    let (h, v) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/rotate-presenter-key", room_id))
        .add_header(h, v)
        .await;
    assert_eq!(res.status_code(), 200);

    let conn = state.db.get().unwrap();
    let exists = |id: &str| -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM participants WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    };
    assert!(
        !exists(&host_id),
        "the host-link presenter should be revoked"
    );
    assert!(exists(&admin_id), "the admin's own session must survive");
    assert!(exists(&viewer_id), "viewers are untouched");
}

/// Presenters from before `via_host_link` existed cannot be told apart, so the
/// migration marks them all as host-link sessions — a rotation must still
/// revoke what it revoked before the upgrade.
#[test]
fn presenters_from_before_the_column_still_rotate_out() {
    let dir = std::env::temp_dir().join(format!("zstream-migrate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("stream.db");
    let (db, data) = (db_path.to_str().unwrap(), dir.to_str().unwrap());
    // A database as it was before the column: today's schema with it dropped.
    drop(stream_backend::db::init_pool(db, data));
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "ALTER TABLE participants DROP COLUMN via_host_link;
             INSERT INTO rooms (id, name, slug) VALUES ('r', 'R', 'r');
             INSERT INTO participants (id, room_id, name, role) VALUES
                 ('p', 'r', 'Host', 'presenter'), ('v', 'r', 'Guest', 'viewer');",
        )
        .unwrap();
    }
    let pool = stream_backend::db::init_pool(db, data);
    let conn = pool.get().unwrap();
    let marked = |id: &str| -> i64 {
        conn.query_row(
            "SELECT via_host_link FROM participants WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(marked("p"), 1);
    assert_eq!(marked("v"), 0);
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

// ---------------------------------------------------------------------------
// delivery:changed fires on a change, not on every save
// ---------------------------------------------------------------------------

/// The admin modal sends every field on every save. Treating "the body carries
/// delivery_mode" as "it changed" pushed delivery:changed to every viewer on a
/// rename — and each re-pinned its stage, overriding the host's grid layout.
#[tokio::test]
async fn saving_a_room_announces_a_delivery_mode_change_only_when_it_changes() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let room_id = common::seed_room(&state, "Room", "dm-change");
    let mut rx = state.events.delivery_mode_changed.subscribe();

    let save = |body: serde_json::Value| {
        server
            .put(&format!("/api/rooms/{room_id}"))
            .add_header("Authorization", format!("Bearer {token}"))
            .json(&body)
    };

    // A rename, with the unchanged mode along for the ride as the modal sends it.
    assert_eq!(
        save(json!({ "name": "Renamed", "delivery_mode": "webrtc" }))
            .await
            .status_code(),
        200
    );
    assert!(
        rx.try_recv().is_err(),
        "nothing changed, nothing to announce"
    );

    assert_eq!(
        save(json!({ "delivery_mode": "llhls" }))
            .await
            .status_code(),
        200
    );
    assert_eq!(
        rx.try_recv().expect("a real change is announced").mode,
        "llhls"
    );
}

// ---------------------------------------------------------------------------
// Deny: the waiting room's missing "no"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_can_deny_a_waiting_participant() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let room_id = common::seed_room(&state, "Room", "deny-room");
    let (pid, ptok) =
        common::seed_participant(&state, &room_id, "Unwanted", "viewer", false, false);
    let mut kicked = state.events.participant_kicked.subscribe();

    let res = server
        .post(&format!("/api/rooms/{room_id}/kick/{pid}"))
        .add_header("Authorization", format!("Bearer {token}"))
        .await;
    assert_eq!(res.status_code(), 200);
    assert_eq!(kicked.try_recv().unwrap().participant_id, pid);

    // The waiting client polls this, and its admission stream says the same.
    let status: Value = server
        .get(&format!(
            "/api/public/rooms/deny-room/status/{pid}?token={ptok}"
        ))
        .await
        .json();
    assert_eq!(status["kicked"], true);
    assert_eq!(status["admitted"], false);
}

#[tokio::test]
async fn deny_needs_an_admin_and_a_participant_of_that_room() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let room_a = common::seed_room(&state, "A", "deny-a");
    let room_b = common::seed_room(&state, "B", "deny-b");
    let (pid, _) = common::seed_participant(&state, &room_b, "Elsewhere", "viewer", false, false);

    let anon = server
        .post(&format!("/api/rooms/{room_a}/kick/{pid}"))
        .await;
    assert_eq!(anon.status_code(), 401);
    let wrong_room = server
        .post(&format!("/api/rooms/{room_a}/kick/{pid}"))
        .add_header("Authorization", format!("Bearer {token}"))
        .await;
    assert_eq!(wrong_room.status_code(), 404);
}

/// Enter Room makes the admin an admitted presenter straight away — and, since
/// it holds no host link, not one a link rotation revokes.
#[tokio::test]
async fn enter_room_creates_an_admitted_presenter_outside_the_host_link() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let room_id = common::seed_room(&state, "Enter", "enter-state-abc123");

    let (h, v) = auth_header(&token);
    let res = server
        .post(&format!("/api/rooms/{}/enter", room_id))
        .add_header(h, v)
        .json(&json!({ "name": "Admin" }))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["role"], "presenter");
    let pid = body["participantId"].as_str().unwrap().to_string();

    let (role, admitted, via_link): (String, i64, i64) = state
        .db
        .get()
        .unwrap()
        .query_row(
            "SELECT role, is_admitted, via_host_link FROM participants WHERE id = ?1",
            [&pid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((role.as_str(), admitted, via_link), ("presenter", 1, 0));
}
