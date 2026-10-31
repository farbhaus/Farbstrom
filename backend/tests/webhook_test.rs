mod common;

use axum::http::header;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// The webhook secret used in the test AppConfig.
const TEST_WEBHOOK_SECRET: &str = "test-webhook-secret";

fn sign_webhook(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha1::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let result = mac.finalize().into_bytes();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(result)
}

// ---------------------------------------------------------------------------
// Missing / wrong signature
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_returns_401_without_signature() {
    let state = common::test_state();
    let server = common::test_app(state);

    let body = json!({"request": {"direction": "incoming", "url": "rtmp://host/live/key"}});
    let body_bytes = serde_json::to_vec(&body).unwrap();

    let res = server
        .post("/api/webhook/admission")
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn webhook_returns_401_wrong_signature() {
    let state = common::test_state();
    let server = common::test_app(state);

    let body = json!({"request": {"direction": "incoming", "url": "rtmp://host/live/key"}});
    let body_bytes = serde_json::to_vec(&body).unwrap();

    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", "bad-signature")
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 401);
}

// ---------------------------------------------------------------------------
// Unknown stream key (valid signature, key not in DB) -> denied
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_denies_unknown_stream_key() {
    // A correctly-signed request whose stream key was never created in the
    // admin must be denied — this is the actual ingest authorization, not just
    // the HMAC check. OME honours {allowed: false} by rejecting the publish.
    let state = common::test_state();
    let server = common::test_app(state);

    let body = json!({
        "request": {
            "direction": "incoming",
            "url": "rtmp://host/live/unknown-key-12345"
        }
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 200);

    let resp: Value = res.json();
    assert_eq!(resp["allowed"], false);
}

#[tokio::test]
async fn webhook_allows_known_key_without_room() {
    // A valid admin-created key is allowed even when it isn't assigned to any
    // room yet (no room to set live, but the ingest is authorized).
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (_sk_id, key_token) = common::seed_stream_key(&state, "Unassigned Key");

    let body = json!({
        "request": {
            "direction": "incoming",
            "url": format!("rtmp://host/live/{}", key_token)
        }
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 200);

    let resp: Value = res.json();
    assert_eq!(resp["allowed"], true);
}

#[tokio::test]
async fn webhook_denies_blocked_stream_key() {
    // A stream kick blocks the key (stream_keys.blocked = 1). A known-but-blocked
    // key must be denied so the kicked encoder can't auto-reconnect, and its room
    // must NOT be flipped live.
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let admin_tok = common::admin_token(&state);

    let (sk_id, key_token) = common::seed_stream_key(&state, "Kicked Key");
    let room_id = common::seed_room_full(
        &state,
        "Blocked Room",
        "blocked-room-abc123",
        "pending",
        false,
        Some(&sk_id),
    );
    // Simulate the kick having blocked the key.
    state
        .db
        .get()
        .unwrap()
        .execute(
            "UPDATE stream_keys SET blocked = 1 WHERE id = ?1",
            rusqlite::params![sk_id],
        )
        .unwrap();

    let body = json!({
        "request": {
            "direction": "incoming",
            "url": format!("rtmp://host/live/{}", key_token)
        }
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 200);
    let resp: Value = res.json();
    assert_eq!(resp["allowed"], false);

    // Room stays pending — a blocked ingest never goes live.
    let room = common::admin_room(&server, &admin_tok, &room_id).await;
    assert_eq!(room["status"], "pending");
}

// ---------------------------------------------------------------------------
// Valid stream key -> room goes live
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_accepts_valid_stream_key_and_sets_room_live() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let admin_tok = common::admin_token(&state);

    // Seed a stream key and a room linked to it
    let (sk_id, key_token) = common::seed_stream_key(&state, "Test Key");
    let room_id = common::seed_room_full(
        &state,
        "Webhook Room",
        "webhook-room-abc123",
        "pending",
        false,
        Some(&sk_id),
    );

    let body = json!({
        "request": {
            "direction": "incoming",
            "url": format!("rtmp://host/live/{}", key_token)
        }
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 200);

    let resp: Value = res.json();
    assert_eq!(resp["allowed"], true);

    // Verify the room status was set to "live"
    let room = common::admin_room(&server, &admin_tok, &room_id).await;
    assert_eq!(room["status"], "live");
}

// ---------------------------------------------------------------------------
// 'ended' is terminal
// ---------------------------------------------------------------------------

/// Reads a room's status straight from the DB — no admin token needed.
fn db_status(state: &std::sync::Arc<stream_backend::state::AppState>, id: &str) -> String {
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

/// Posts a valid, signed admission request for `key_token`.
async fn admit(server: &axum_test::TestServer, key_token: &str) -> u16 {
    admit_with_status(server, key_token, None).await
}

/// Same, with OME's `status` field ("opening" / "closing") set.
async fn admit_with_status(
    server: &axum_test::TestServer,
    key_token: &str,
    status: Option<&str>,
) -> u16 {
    let mut request = json!({
        "direction": "incoming",
        "url": format!("rtmp://host/live/{}", key_token)
    });
    if let Some(s) = status {
        request["status"] = json!(s);
    }
    let body = json!({ "request": request });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, &body_bytes);
    server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await
        .status_code()
        .as_u16()
}

// ---------------------------------------------------------------------------
// status: "closing" — the publisher disconnected
// ---------------------------------------------------------------------------

/// Longer than the webhook's post-close re-check (2 s), which runs after the
/// reply so OME isn't kept waiting on its own API.
const AFTER_RECHECK: std::time::Duration = std::time::Duration::from_millis(2600);

/// Without this the room sat on a dead stream until the next 30s poll, so
/// viewers kept staring at a frozen tile with no "waiting" overlay.
#[tokio::test]
async fn closing_webhook_drops_a_live_room_to_pending() {
    // OME no longer lists the stream: the publisher really left.
    let (state, _broadcasting) = common::state_with_fake_ome().await;
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "K");
    let room_id =
        common::seed_room_full(&state, "Room", "closing-room", "live", false, Some(&sk_id));

    let mut rx = state.events.room_pending.subscribe();

    assert_eq!(
        admit_with_status(&server, &key_token, Some("closing")).await,
        200
    );

    tokio::time::sleep(AFTER_RECHECK).await;
    assert_eq!(db_status(&state, &room_id), "pending");
    assert_eq!(
        rx.try_recv().unwrap(),
        "closing-room",
        "viewers must be told the stream stopped"
    );
}

/// OME sends `closing` for every session that ends — including a second encoder
/// it refused as a duplicate of the one still live. Acting on that let anyone
/// holding the key blank the stream for the whole room (verified against OME
/// v0.21.0), so a close only counts once OME has stopped publishing.
#[tokio::test]
async fn closing_from_a_refused_duplicate_leaves_the_live_room_alone() {
    let (state, broadcasting) = common::state_with_fake_ome().await;
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "K");
    let room_id =
        common::seed_room_full(&state, "Room", "closing-dup", "live", false, Some(&sk_id));
    // The original encoder is still publishing.
    broadcasting
        .lock()
        .unwrap()
        .push(common::playback_id(&state, &sk_id));

    let mut rx = state.events.room_pending.subscribe();
    assert_eq!(
        admit_with_status(&server, &key_token, Some("closing")).await,
        200
    );

    tokio::time::sleep(AFTER_RECHECK).await;
    assert_eq!(db_status(&state, &room_id), "live");
    assert!(
        rx.try_recv().is_err(),
        "no room:pending for a stream still live"
    );
}

/// When OME can't be asked, a close is not proof the stream stopped — the
/// poller decides instead.
#[tokio::test]
async fn closing_with_ome_unreachable_leaves_the_room_to_the_poller() {
    let state = common::test_state(); // OME API points at nothing
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "K");
    let room_id =
        common::seed_room_full(&state, "Room", "closing-noome", "live", false, Some(&sk_id));

    assert_eq!(
        admit_with_status(&server, &key_token, Some("closing")).await,
        200
    );

    tokio::time::sleep(AFTER_RECHECK).await;
    assert_eq!(db_status(&state, &room_id), "live");
}

/// A close must not resurrect or otherwise disturb a room that already ended.
#[tokio::test]
async fn closing_webhook_leaves_an_ended_room_alone() {
    let (state, _broadcasting) = common::state_with_fake_ome().await;
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "K");
    let room_id = common::seed_room_full(
        &state,
        "Room",
        "closing-ended",
        "ended",
        false,
        Some(&sk_id),
    );

    assert_eq!(
        admit_with_status(&server, &key_token, Some("closing")).await,
        200
    );

    tokio::time::sleep(AFTER_RECHECK).await;
    assert_eq!(db_status(&state, &room_id), "ended");
}

/// "opening" is the normal admission path and must still promote.
#[tokio::test]
async fn opening_webhook_still_sets_the_room_live() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "K");
    let room_id = common::seed_room_full(
        &state,
        "Room",
        "opening-room",
        "pending",
        false,
        Some(&sk_id),
    );

    assert_eq!(
        admit_with_status(&server, &key_token, Some("opening")).await,
        200
    );

    assert_eq!(db_status(&state, &room_id), "live");
}

/// An expired or explicitly ended room must not come back just because an
/// encoder started pushing its old stream key.
#[tokio::test]
async fn webhook_does_not_resurrect_an_ended_room() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "Old Key");
    let room_id = common::seed_room_full(
        &state,
        "Ended Room",
        "webhook-ended-room",
        "ended",
        false,
        Some(&sk_id),
    );

    assert_eq!(admit(&server, &key_token).await, 200);

    assert_eq!(
        db_status(&state, &room_id),
        "ended",
        "an ended room must stay ended"
    );
}

/// A 'scheduled' room going live when the host starts early is deliberate --
/// `tasks::poll_starts` is written to expect it and releases held viewers for
/// rooms the webhook already flipped. Pinned so the ended-room guard above
/// isn't later "tidied up" into a pending-only filter.
#[tokio::test]
async fn webhook_still_starts_a_scheduled_room() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "Sched Key");
    let room_id = common::seed_room_full(
        &state,
        "Scheduled Room",
        "webhook-scheduled-room",
        "scheduled",
        false,
        Some(&sk_id),
    );

    assert_eq!(admit(&server, &key_token).await, 200);

    assert_eq!(db_status(&state, &room_id), "live");
}

/// One key, one ended room and one pending room: the live one is promoted and
/// the ended one is left alone.
#[tokio::test]
async fn webhook_promotes_only_the_non_ended_rooms_on_a_shared_key() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (sk_id, key_token) = common::seed_stream_key(&state, "Shared Key");
    let ended = common::seed_room_full(
        &state,
        "Ended",
        "webhook-shared-ended",
        "ended",
        false,
        Some(&sk_id),
    );
    let pending = common::seed_room_full(
        &state,
        "Pending",
        "webhook-shared-pending",
        "pending",
        false,
        Some(&sk_id),
    );

    assert_eq!(admit(&server, &key_token).await, 200);

    assert_eq!(db_status(&state, &ended), "ended");
    assert_eq!(db_status(&state, &pending), "live");
}

// ---------------------------------------------------------------------------
// Ingest is renamed to the key's playback id
// ---------------------------------------------------------------------------

/// What a viewer is handed must never be able to push video, so OME publishes
/// under the key's playback id, not the key the encoder used (`new_url`).
#[tokio::test]
async fn allowed_ingest_is_renamed_to_the_playback_id() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, key_token) = common::seed_stream_key(&state, "K");
    let playback_id = common::playback_id(&state, &sk_id);

    for (url, renamed) in [
        (
            format!("rtmp://host:1935/live/{key_token}"),
            format!("rtmp://host:1935/live/{playback_id}"),
        ),
        (
            format!("srt://default/live/{key_token}"),
            format!("srt://default/live/{playback_id}"),
        ),
        (
            format!("http://host/live/{key_token}?direction=whip"),
            format!("http://host/live/{playback_id}?direction=whip"),
        ),
    ] {
        let resp = post_request(
            &server,
            json!({ "direction": "incoming", "status": "opening", "url": url }),
        )
        .await;
        assert_eq!(resp["allowed"], true, "{url}");
        assert_eq!(resp["new_url"], renamed, "{url}");
    }
}

/// The playback id is handed to every viewer, so it must not double as an
/// ingest credential.
#[tokio::test]
async fn the_playback_id_cannot_ingest() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, _key_token) = common::seed_stream_key(&state, "K");
    let playback_id = common::playback_id(&state, &sk_id);

    let resp = post_request(
        &server,
        json!({ "direction": "incoming", "status": "opening", "url": format!("rtmp://host/live/{playback_id}") }),
    )
    .await;
    assert_eq!(resp["allowed"], false);
}

// ---------------------------------------------------------------------------
// Outgoing direction -> browser playback admission
// ---------------------------------------------------------------------------

/// Posts a signed admission request and returns OME's view of the reply.
async fn post_request(server: &axum_test::TestServer, request: Value) -> Value {
    let body_bytes = serde_json::to_vec(&json!({ "request": request })).unwrap();
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, &body_bytes);
    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 200);
    res.json()
}

/// OME's playback-session request for `url`, as either protocol sends it.
async fn may_play(server: &axum_test::TestServer, url: &str) -> bool {
    let resp = post_request(
        server,
        json!({ "direction": "outgoing", "status": "opening", "url": url }),
    )
    .await;
    resp["allowed"] == json!(true)
}

/// Both URL shapes OME passes for browser playback.
fn playback_urls(stream: &str, query: &str) -> [String; 2] {
    [
        format!("http://host/live/{stream}/llhls.m3u8?{query}"),
        format!("ws://host:3333/live/{stream}?{query}"),
    ]
}

#[tokio::test]
async fn an_admitted_participant_may_play_their_rooms_stream() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, _) = common::seed_stream_key(&state, "K");
    let room_id = common::seed_room_full(&state, "Room", "play-ok", "live", false, Some(&sk_id));
    let (pid, tok) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);

    for url in playback_urls(
        &common::playback_id(&state, &sk_id),
        &format!("participantId={pid}&token={tok}"),
    ) {
        assert!(may_play(&server, &url).await, "{url}");
    }
}

/// The waiting-room bypass: a joiner the host never admitted used to get the
/// stream key at join and could play it directly.
#[tokio::test]
async fn playback_is_refused_to_everyone_else() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, key_token) = common::seed_stream_key(&state, "K");
    let playback_id = common::playback_id(&state, &sk_id);
    let room_id = common::seed_room_full(&state, "Room", "play-no", "live", true, Some(&sk_id));
    let (waiting, waiting_tok) =
        common::seed_participant(&state, &room_id, "W", "viewer", false, false);
    let (kicked, kicked_tok) =
        common::seed_participant(&state, &room_id, "K", "viewer", true, true);
    let (ok, ok_tok) = common::seed_participant(&state, &room_id, "A", "viewer", true, false);
    let (other_key, _) = common::seed_stream_key(&state, "Other");

    let cases = [
        ("no credentials", playback_id.clone(), String::new()),
        (
            "not admitted",
            playback_id.clone(),
            format!("participantId={waiting}&token={waiting_tok}"),
        ),
        (
            "kicked",
            playback_id.clone(),
            format!("participantId={kicked}&token={kicked_tok}"),
        ),
        (
            "wrong token",
            playback_id.clone(),
            format!("participantId={ok}&token=nope"),
        ),
        (
            "another room's stream",
            common::playback_id(&state, &other_key),
            format!("participantId={ok}&token={ok_tok}"),
        ),
        (
            "the ingest key as the name",
            key_token,
            format!("participantId={ok}&token={ok_tok}"),
        ),
    ];
    for (label, stream, query) in cases {
        for url in playback_urls(&stream, &query) {
            assert!(!may_play(&server, &url).await, "{label}: {url}");
        }
    }
}

#[tokio::test]
async fn playback_stops_being_granted_once_the_room_ends() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, _) = common::seed_stream_key(&state, "K");
    let room_id =
        common::seed_room_full(&state, "Room", "play-ended", "ended", false, Some(&sk_id));
    let (pid, tok) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);

    for url in playback_urls(
        &common::playback_id(&state, &sk_id),
        &format!("participantId={pid}&token={tok}"),
    ) {
        assert!(!may_play(&server, &url).await, "{url}");
    }
}

#[tokio::test]
async fn an_admin_preview_token_plays_any_stream_until_it_expires() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, _) = common::seed_stream_key(&state, "K");
    let playback_id = common::playback_id(&state, &sk_id);

    let res = server
        .get("/api/ome/playback-token")
        .add_header(
            header::AUTHORIZATION,
            format!("Bearer {}", common::admin_token(&state)),
        )
        .await;
    assert_eq!(res.status_code(), 200);
    let token = res.json::<Value>()["token"].as_str().unwrap().to_string();
    for url in playback_urls(&playback_id, &format!("apt={token}")) {
        assert!(may_play(&server, &url).await, "{url}");
    }

    let expired = stream_backend::playback::mint_admin_token(&state.config.jwt_secret, 0);
    for url in playback_urls(&playback_id, &format!("apt={expired}")) {
        assert!(!may_play(&server, &url).await, "expired: {url}");
    }

    // Minting one takes an admin session.
    assert_eq!(
        server.get("/api/ome/playback-token").await.status_code(),
        401
    );
}

/// A playback session ending is a notification, not a request.
#[tokio::test]
async fn closing_playback_sessions_are_acknowledged() {
    let state = common::test_state();
    let server = common::test_app(state);
    let resp = post_request(
        &server,
        json!({ "direction": "outgoing", "status": "closing", "url": "ws://host:3333/live/anything" }),
    )
    .await;
    assert_eq!(resp["allowed"], true);
}

// ---------------------------------------------------------------------------
// Missing request object
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_returns_400_missing_request_object() {
    let state = common::test_state();
    let server = common::test_app(state);

    let body = json!({ "something": "else" });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.into())
        .await;
    assert_eq!(res.status_code(), 400);
}

// ---------------------------------------------------------------------------
// Invalid JSON body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_returns_400_invalid_json() {
    let state = common::test_state();
    let server = common::test_app(state);

    let body_bytes = b"not valid json";
    let sig = sign_webhook(TEST_WEBHOOK_SECRET, body_bytes);

    let res = server
        .post("/api/webhook/admission")
        .add_header("x-ome-signature", sig.as_str())
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(body_bytes.to_vec().into())
        .await;
    assert_eq!(res.status_code(), 400);
}
