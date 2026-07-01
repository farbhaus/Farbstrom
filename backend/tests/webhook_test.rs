mod common;

use axum::http::header;
use base64::Engine;
use hmac::{Hmac, Mac};
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
    let (hname, hval) = (
        header::AUTHORIZATION,
        format!("Bearer {}", admin_tok)
            .parse::<axum::http::HeaderValue>()
            .unwrap(),
    );
    let room: Value = server
        .get(&format!("/api/rooms/{}", room_id))
        .add_header(hname, hval)
        .await
        .json();
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
    let (hname, hval) = (
        header::AUTHORIZATION,
        format!("Bearer {}", admin_tok)
            .parse::<axum::http::HeaderValue>()
            .unwrap(),
    );
    let room_res = server
        .get(&format!("/api/rooms/{}", room_id))
        .add_header(hname, hval)
        .await;
    let room: Value = room_res.json();
    assert_eq!(room["status"], "live");
}

// ---------------------------------------------------------------------------
// Outgoing direction -> always allowed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_allows_outgoing() {
    let state = common::test_state();
    let server = common::test_app(state);

    let body = json!({
        "request": {
            "direction": "outgoing",
            "url": "rtmp://host/live/any-key"
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
