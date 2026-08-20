mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

// ---------------------------------------------------------------------------
// SRT encryption toggle (gh #208)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn srt_config_requires_admin() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/stream-keys/srt-config").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn srt_encryption_toggle_requires_admin() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server
        .post("/api/stream-keys/srt-encryption")
        .json(&serde_json::json!({ "ingest": true, "playback": true }))
        .await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn srt_config_defaults_to_disabled() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/stream-keys/srt-config")
        .add_header("Authorization", format!("Bearer {}", token))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["ingestEnabled"], false);
    assert_eq!(body["playbackEnabled"], false);
    assert!(body["ingestPassphrase"].is_null());
    assert!(body["playbackPassphrase"].is_null());
}

#[tokio::test]
async fn srt_encryption_legs_are_independent() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let auth = format!("Bearer {}", token);

    // Enable playback only — ingest must stay off and unadvertised.
    let res = server
        .post("/api/stream-keys/srt-encryption")
        .add_header("Authorization", auth.clone())
        .json(&serde_json::json!({ "ingest": false, "playback": true }))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["ingestEnabled"], false);
    assert_eq!(body["playbackEnabled"], true);
    assert!(body["ingestPassphrase"].is_null());
    let playback = body["playbackPassphrase"].as_str().unwrap().to_string();
    assert!((10..=79).contains(&playback.len()));

    // Now enable ingest too — playback passphrase is unchanged (reused).
    let res = server
        .post("/api/stream-keys/srt-encryption")
        .add_header("Authorization", auth.clone())
        .json(&serde_json::json!({ "ingest": true, "playback": true }))
        .await;
    let body2: Value = res.json();
    assert_eq!(body2["ingestEnabled"], true);
    assert_eq!(body2["playbackEnabled"], true);
    let ingest = body2["ingestPassphrase"].as_str().unwrap().to_string();
    assert!((10..=79).contains(&ingest.len()));
    assert_eq!(body2["playbackPassphrase"], playback);

    // Disable playback, keep ingest — only playback drops.
    let res = server
        .post("/api/stream-keys/srt-encryption")
        .add_header("Authorization", auth.clone())
        .json(&serde_json::json!({ "ingest": true, "playback": false }))
        .await;
    let body3: Value = res.json();
    assert_eq!(body3["ingestEnabled"], true);
    assert_eq!(body3["playbackEnabled"], false);
    assert_eq!(body3["ingestPassphrase"], ingest);
    assert!(body3["playbackPassphrase"].is_null());

    // GET reflects the persisted per-leg state.
    let res = server
        .get("/api/stream-keys/srt-config")
        .add_header("Authorization", auth)
        .await;
    let body4: Value = res.json();
    assert_eq!(body4["ingestEnabled"], true);
    assert_eq!(body4["playbackEnabled"], false);
    assert_eq!(body4["ingestPassphrase"], ingest);
    assert!(body4["playbackPassphrase"].is_null());
}

#[tokio::test]
async fn srt_legacy_combined_flag_migrates_to_both_legs() {
    // A deploy from the first (combined) cut has srt_encryption_enabled=1 plus
    // both passphrases. Startup must migrate that into the per-leg flags without
    // changing the effective state (both legs stay on, passphrases preserved).
    let state = common::test_state();
    {
        let conn = state.db.get().unwrap();
        stream_backend::credentials::settings_set(&conn, "srt_encryption_enabled", "1").unwrap();
        stream_backend::credentials::settings_set(
            &conn,
            "srt_ingest_passphrase",
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        stream_backend::credentials::settings_set(
            &conn,
            "srt_playback_passphrase",
            "fedcba9876543210fedcba9876543210",
        )
        .unwrap();
        stream_backend::srt::init_startup(&conn, &state.config.data_path);
        // Legacy key is removed.
        assert!(
            stream_backend::credentials::settings_get(&conn, "srt_encryption_enabled").is_none()
        );
    }
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/stream-keys/srt-config")
        .add_header("Authorization", format!("Bearer {}", token))
        .await;
    let body: Value = res.json();
    assert_eq!(body["ingestEnabled"], true);
    assert_eq!(body["playbackEnabled"], true);
    assert_eq!(body["ingestPassphrase"], "0123456789abcdef0123456789abcdef");
    assert_eq!(
        body["playbackPassphrase"],
        "fedcba9876543210fedcba9876543210"
    );
}

#[tokio::test]
async fn list_keys_returns_401_without_auth() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/stream-keys").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn list_keys_returns_empty_array() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/stream-keys")
        .add_header("Authorization", format!("Bearer {}", token))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Vec<Value> = res.json();
    assert!(body.is_empty());
}

#[tokio::test]
async fn create_key_returns_400_without_name() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .post("/api/stream-keys")
        .add_header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({}))
        .await;
    assert_eq!(res.status_code(), 400);
}

#[tokio::test]
async fn create_key_returns_201() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .post("/api/stream-keys")
        .add_header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"name": "Test Key"}))
        .await;
    let body: Value = res.json();
    assert!(body.get("id").is_some());
    assert!(body.get("key_token").is_some());
    assert_eq!(body["name"], "Test Key");
}

#[tokio::test]
async fn update_key_returns_404_for_nonexistent() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .put("/api/stream-keys/nonexistent")
        .add_header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"name": "Updated"}))
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn update_key_renames() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let (key_id, _) = common::seed_stream_key(&state, "Original");
    let server = common::test_app(state);
    let res = server
        .put(&format!("/api/stream-keys/{}", key_id))
        .add_header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"name": "Renamed"}))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["name"], "Renamed");
}

#[tokio::test]
async fn delete_key_returns_404_for_nonexistent() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .delete("/api/stream-keys/nonexistent")
        .add_header("Authorization", format!("Bearer {}", token))
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn delete_key_succeeds() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let (key_id, _) = common::seed_stream_key(&state, "To Delete");
    let server = common::test_app(state);
    let res = server
        .delete(&format!("/api/stream-keys/{}", key_id))
        .add_header("Authorization", format!("Bearer {}", token))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["ok"], true);
}

// ---------------------------------------------------------------------------
// Admin SRT playback URL (gh #226)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn srt_playback_requires_admin() {
    let state = common::test_state();
    let (key_id, _) = common::seed_stream_key(&state, "Cam A");
    let server = common::test_app(state);
    let res = server
        .get(&format!("/api/stream-keys/{}/srt-playback", key_id))
        .await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn srt_playback_returns_404_for_nonexistent() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/stream-keys/nonexistent/srt-playback")
        .add_header("Authorization", format!("Bearer {}", token))
        .await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn srt_playback_returns_signed_streamid() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let (key_id, key_token) = common::seed_stream_key(&state, "Cam A");
    let server = common::test_app(state.clone());

    let res = server
        .get(&format!("/api/stream-keys/{}/srt-playback", key_id))
        .add_header("Authorization", format!("Bearer {}", token))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    assert_eq!(body["host"], "stream.example.com");
    assert_eq!(body["port"], 9998);
    assert_eq!(body["ttlSeconds"], 300);

    // Same shape the Farbplay flow mints (both go through signed_policy):
    // default/live/<key>?policy=<b64url>&signature=<b64url-hmac>. No `/playlist`
    // suffix — the SRT publisher's default playlist is named `master`, and
    // SignedPolicy signs the path, so the two forms are not interchangeable.
    let streamid = body["streamid"].as_str().unwrap();
    let (signed, sig) = streamid.split_once("&signature=").unwrap();
    assert!(signed.starts_with(&format!("default/live/{}?policy=", key_token)));
    assert!(!streamid.contains("/playlist"));

    let expected = common::expected_signature(&state.config.ome_signed_policy_secret, signed);
    assert_eq!(sig, expected);

    // Policy decodes to a url_expire ~300 s out (epoch ms).
    let policy_b64 = signed.split("?policy=").nth(1).unwrap();
    let policy_json =
        String::from_utf8(base64::Engine::decode(&URL_SAFE_NO_PAD, policy_b64).unwrap()).unwrap();
    let policy: Value = serde_json::from_str(&policy_json).unwrap();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let expire = policy["url_expire"].as_u64().unwrap();
    assert!(expire > now_ms + 290_000 && expire <= now_ms + 300_000);
}
