mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Seed a room with a stream key assigned, returning (room_id, key_token).
fn seed_room_with_key(
    state: &std::sync::Arc<stream_backend::state::AppState>,
    name: &str,
    slug: &str,
    status: &str,
    waiting_room: bool,
) -> (String, String) {
    let (key_id, key_token) = common::seed_stream_key(state, &format!("{}-key", slug));
    let room_id = common::seed_room_full(state, name, slug, status, waiting_room, Some(&key_id));
    (room_id, key_token)
}

fn set_room_expiry(
    state: &std::sync::Arc<stream_backend::state::AppState>,
    room_id: &str,
    expires_at: &str,
) {
    let conn = state.db.get().unwrap();
    conn.execute(
        "UPDATE rooms SET expires_at = ?1 WHERE id = ?2",
        rusqlite::params![expires_at, room_id],
    )
    .unwrap();
}

/// Recompute the expected HMAC-SHA1 signature for the path-form prefix
/// (`default/live/<key>?policy=<...>`). OME signs the `srt://`-prefixed URL, so
/// the recompute must prepend the scheme just like the endpoint does.
fn expected_signature(secret: &str, signed_path: &str) -> String {
    let mut mac = HmacSha1::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("srt://{}", signed_path).as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn watch_url(slug: &str, participant_id: &str, token: &str) -> String {
    format!(
        "/api/watch/{}?participantId={}&token={}",
        slug, participant_id, token
    )
}

// ---------------------------------------------------------------------------
// Credential gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_credentials_returns_403() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    seed_room_with_key(&state, "Open", "watch-open", "live", false);

    // No params at all.
    assert_eq!(server.get("/api/watch/watch-open").await.status_code(), 403);
    // Only participantId.
    assert_eq!(
        server
            .get("/api/watch/watch-open?participantId=abc")
            .await
            .status_code(),
        403
    );
    // Only token.
    assert_eq!(
        server
            .get("/api/watch/watch-open?token=abc")
            .await
            .status_code(),
        403
    );
}

#[tokio::test]
async fn unknown_participant_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    seed_room_with_key(&state, "Open", "watch-unknown", "live", false);

    let res = server
        .get(&watch_url("watch-unknown", "no-such-pid", "no-such-token"))
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn wrong_token_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (room_id, _key) = seed_room_with_key(&state, "Open", "watch-wrongtok", "live", false);
    let (pid, _tok) = common::seed_participant(&state, &room_id, "Alice", "viewer", true, false);

    let res = server
        .get(&watch_url("watch-wrongtok", &pid, "bad-token"))
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn participant_from_other_room_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (room_a, _ka) = seed_room_with_key(&state, "A", "watch-room-a", "live", false);
    seed_room_with_key(&state, "B", "watch-room-b", "live", false);
    let (pid, tok) = common::seed_participant(&state, &room_a, "Alice", "viewer", true, false);

    // Valid participant of room A, but requested against room B's slug.
    let res = server.get(&watch_url("watch-room-b", &pid, &tok)).await;
    assert_eq!(res.status_code(), 404);
}

// ---------------------------------------------------------------------------
// Admission / kick gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admitted_participant_gets_signed_srt_details() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (room_id, key_token) = seed_room_with_key(&state, "Project X", "watch-ok", "live", false);
    let (pid, tok) = common::seed_participant(&state, &room_id, "Alice", "viewer", true, false);

    let res = server.get(&watch_url("watch-ok", &pid, &tok)).await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["srt"]["host"], "stream.example.com");
    assert_eq!(body["srt"]["port"], 9998);
    assert_eq!(body["srt"]["latency"], 500);
    assert_eq!(body["ttlSeconds"], 30);
    assert_eq!(body["title"], "Project X");
    // No SRT encryption configured → no passphrase handed out.
    assert!(body["srt"]["passphrase"].is_null());
    assert!(body["srt"]["pbkeylen"].is_null());

    // streamid: default/live/<key>?policy=<b64url>&signature=<b64url-hmac>
    let streamid = body["srt"]["streamid"].as_str().unwrap();
    let (signed, sig) = streamid.split_once("&signature=").unwrap();
    assert!(signed.starts_with(&format!("default/live/{}?policy=", key_token)));

    let expected = expected_signature(&state.config.ome_signed_policy_secret, signed);
    assert_eq!(sig, expected);

    // Policy decodes to a future url_expire (epoch ms).
    let policy_b64 = signed.split("?policy=").nth(1).unwrap();
    let policy_json = String::from_utf8(URL_SAFE_NO_PAD.decode(policy_b64).unwrap()).unwrap();
    let policy: Value = serde_json::from_str(&policy_json).unwrap();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(policy["url_expire"].as_u64().unwrap() > now_ms);
}

#[tokio::test]
async fn playback_passphrase_from_db_toggle() {
    // DB-managed SRT encryption (gh #208): enabling the playback leg makes
    // /api/watch hand out the generated playback passphrase. Ingest stays off to
    // confirm the legs are independent — only playback drives the watch response.
    let state = common::test_state();
    {
        let conn = state.db.get().unwrap();
        stream_backend::srt::apply(&conn, &state.config.data_path, false, true).unwrap();
    }
    let server = common::test_app(state.clone());

    let (room_id, _key) = seed_room_with_key(&state, "Enc", "watch-db-enc", "live", false);
    let (pid, tok) = common::seed_participant(&state, &room_id, "Alice", "viewer", true, false);

    let res = server.get(&watch_url("watch-db-enc", &pid, &tok)).await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    let pass = body["srt"]["passphrase"].as_str().unwrap();
    assert!((10..=79).contains(&pass.len()));
    assert_eq!(body["srt"]["pbkeylen"], 16);
}

#[tokio::test]
async fn kicked_participant_returns_403() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (room_id, _key) = seed_room_with_key(&state, "Open", "watch-kicked", "live", false);
    // admitted but kicked
    let (pid, tok) = common::seed_participant(&state, &room_id, "Mallory", "viewer", true, true);

    let res = server.get(&watch_url("watch-kicked", &pid, &tok)).await;
    assert_eq!(res.status_code(), 403);
}

#[tokio::test]
async fn not_admitted_participant_returns_403() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    // waiting-room room; participant not yet admitted
    let (room_id, _key) = seed_room_with_key(&state, "Waiting", "watch-waiting", "live", true);
    let (pid, tok) = common::seed_participant(&state, &room_id, "Bob", "viewer", false, false);

    let res = server.get(&watch_url("watch-waiting", &pid, &tok)).await;
    assert_eq!(res.status_code(), 403);
}

// ---------------------------------------------------------------------------
// Room state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ended_room_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (room_id, _key) = seed_room_with_key(&state, "Ended", "watch-ended", "ended", false);
    let (pid, tok) = common::seed_participant(&state, &room_id, "Alice", "viewer", true, false);

    let res = server.get(&watch_url("watch-ended", &pid, &tok)).await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn expired_room_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    let (room_id, _key) = seed_room_with_key(&state, "Expired", "watch-expired", "live", false);
    set_room_expiry(&state, &room_id, "2020-01-01 00:00:00");
    let (pid, tok) = common::seed_participant(&state, &room_id, "Alice", "viewer", true, false);

    let res = server.get(&watch_url("watch-expired", &pid, &tok)).await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn room_without_stream_key_returns_404() {
    let state = common::test_state();
    let server = common::test_app(state.clone());

    // Room with no stream key assigned.
    let room_id = common::seed_room(&state, "No Key", "watch-no-key");
    let (pid, tok) = common::seed_participant(&state, &room_id, "Alice", "viewer", true, false);

    let res = server.get(&watch_url("watch-no-key", &pid, &tok)).await;
    assert_eq!(res.status_code(), 404);
}
