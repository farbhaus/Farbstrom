#![allow(dead_code)]

use axum_test::TestServer;
use std::sync::Arc;

use stream_backend::auth;
use stream_backend::config::AppConfig;
use stream_backend::db;
use stream_backend::events::EventChannels;
use stream_backend::routes;
use stream_backend::state::AppState;

pub fn test_config() -> AppConfig {
    // Use a unique temp file for each test to avoid cross-test interference
    let db_path = format!("/tmp/zstream-test-{}.db", uuid::Uuid::new_v4());
    AppConfig {
        jwt_secret: "test-secret-that-is-at-least-thirty-two-characters-long".into(),
        ome_webhook_secret: "test-webhook-secret".into(),
        ome_signed_policy_secret: "test-signed-policy-secret-32-chars-min".into(),
        ome_api_url: "http://localhost:9999".into(),
        ome_api_token: "test:token".into(),
        livekit_api_key: "test-lk-key".into(),
        livekit_api_secret: "test-lk-secret".into(),
        livekit_internal_url: "http://localhost:7880".into(),
        livekit_url: "ws://localhost:7880".into(),
        port: 0,
        db_path,
        data_path: "/tmp/zstream-test".into(),
        public_origin: "http://localhost:4001".into(),
        srt_public_host: "stream.example.com".into(),
        srt_public_port: 9998,
        srt_latency_ms: 500,
    }
}

pub fn test_state() -> Arc<AppState> {
    test_state_with_config(test_config())
}

pub fn test_state_with_config(config: AppConfig) -> Arc<AppState> {
    let pool = db::init_pool(&config.db_path, &config.data_path);
    let events = EventChannels::new();
    let admin_password_hash = bcrypt::hash("test-admin-password", 4).unwrap();
    let webauthn = std::sync::Arc::new(stream_backend::credentials::build_webauthn(
        &config.public_origin,
    ));

    Arc::new(AppState {
        db: pool,
        events,
        config,
        http_client: reqwest::Client::new(),
        admin_password_hash,
        metrics_samples: tokio::sync::Mutex::new(stream_backend::state::MetricsSamples::default()),
        webauthn,
        passkey_reg: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        passkey_auth: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    })
}

pub fn test_app(state: Arc<AppState>) -> TestServer {
    // tower_governor rejects requests that can't yield a key, which happens
    // under axum-test's TestServer since no ConnectInfo is set. Disable the
    // limiter here; its behaviour is exercised by integration smoke tests
    // that hit the real HTTP server.
    std::env::set_var("STREAM_DISABLE_RATE_LIMIT", "1");
    // The SRT-encryption toggle would otherwise shell out to `supervisorctl` to
    // restart OME (gh #208), which doesn't exist in CI. The DB write still runs.
    std::env::set_var("STREAM_DISABLE_OME_RESTART", "1");
    let router = routes::build_router(state);
    TestServer::new(router).unwrap()
}

pub fn admin_token(state: &Arc<AppState>) -> String {
    auth::create_admin_token(&state.config.jwt_secret).unwrap()
}

pub fn seed_stream_key(state: &Arc<AppState>, name: &str) -> (String, String) {
    let conn = state.db.get().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let key_token: String = (0..24)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    conn.execute(
        "INSERT INTO stream_keys (id, name, key_token) VALUES (?1, ?2, ?3)",
        rusqlite::params![id, name, key_token],
    )
    .unwrap();
    (id, key_token)
}

pub fn seed_room(state: &Arc<AppState>, name: &str, slug: &str) -> String {
    let conn = state.db.get().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let presenter_key: String = (0..16)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    conn.execute(
        "INSERT INTO rooms (id, name, slug, presenter_key, delivery_mode, waiting_room, status) VALUES (?1, ?2, ?3, ?4, 'webrtc', 0, 'pending')",
        rusqlite::params![id, name, slug, presenter_key],
    ).unwrap();
    id
}

pub fn seed_room_with_password(
    state: &Arc<AppState>,
    name: &str,
    slug: &str,
    password: &str,
) -> String {
    let conn = state.db.get().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let presenter_key: String = (0..16)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    let password_hash = bcrypt::hash(password, 4).unwrap();
    conn.execute(
        "INSERT INTO rooms (id, name, slug, presenter_key, password_hash, delivery_mode, waiting_room, status) VALUES (?1, ?2, ?3, ?4, ?5, 'webrtc', 0, 'pending')",
        rusqlite::params![id, name, slug, presenter_key, password_hash],
    ).unwrap();
    id
}

#[allow(dead_code)]
pub fn seed_room_full(
    state: &Arc<AppState>,
    name: &str,
    slug: &str,
    status: &str,
    waiting_room: bool,
    stream_key_id: Option<&str>,
) -> String {
    let conn = state.db.get().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let presenter_key: String = (0..16)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    let wr: i32 = if waiting_room { 1 } else { 0 };
    conn.execute(
        "INSERT INTO rooms (id, name, slug, presenter_key, delivery_mode, waiting_room, status, stream_key_id) VALUES (?1, ?2, ?3, ?4, 'webrtc', ?5, ?6, ?7)",
        rusqlite::params![id, name, slug, presenter_key, wr, status, stream_key_id],
    ).unwrap();
    id
}

#[allow(dead_code)]
pub fn seed_participant(
    state: &Arc<AppState>,
    room_id: &str,
    name: &str,
    role: &str,
    admitted: bool,
    kicked: bool,
) -> (String, String) {
    let conn = state.db.get().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let token: String = (0..32)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    let adm: i32 = if admitted { 1 } else { 0 };
    let kick: i32 = if kicked { 1 } else { 0 };
    conn.execute(
        "INSERT INTO participants (id, room_id, name, role, is_admitted, is_kicked, token) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![id, room_id, name, role, adm, kick, token],
    ).unwrap();
    (id, token)
}

#[allow(dead_code)]
pub fn get_room_presenter_key(state: &Arc<AppState>, room_id: &str) -> String {
    let conn = state.db.get().unwrap();
    conn.query_row(
        "SELECT presenter_key FROM rooms WHERE id = ?1",
        rusqlite::params![room_id],
        |row| row.get(0),
    )
    .unwrap()
}
