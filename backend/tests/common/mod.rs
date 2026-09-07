#![allow(dead_code)]

use axum_test::TestServer;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha1::Sha1;
use std::sync::Arc;

use stream_backend::auth;
use stream_backend::config::AppConfig;
use stream_backend::db;
use stream_backend::events::EventChannels;
use stream_backend::routes;
use stream_backend::state::AppState;

pub fn test_config() -> AppConfig {
    // Use a unique temp file/dir per test to avoid cross-test interference —
    // the SRT toggle writes `<data>/srt.env` via a `.tmp` + rename, and a shared
    // data_path lets parallel tests race that rename (ENOENT) and 500.
    let id = uuid::Uuid::new_v4();
    let db_path = format!("/tmp/zstream-test-{}.db", id);
    let data_path = format!("/tmp/zstream-test-{}", id);
    let web_root = format!("{}/www", data_path);
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
        data_path,
        public_origin: "http://localhost:4001".into(),
        srt_public_host: "stream.example.com".into(),
        srt_public_port: 9998,
        srt_latency_ms: 500,
        // `test_state_with_config` hashes its own bootstrap password below, so
        // this is only here to satisfy the struct; keep the two in step if a
        // test ever exercises the env-derived password path.
        admin_password: "test-admin-password".into(),
        // Per-test fixture tree, so the static mounts and the SPA handlers have
        // something real to serve. `seed_web_root` populates it.
        web_root,
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

/// Recompute the expected HMAC-SHA1 signature for the path-form prefix
/// (`default/live/<key>?policy=<...>`) of an OME SignedPolicy streamid. OME signs
/// the `srt://`-prefixed URL, so the recompute must prepend the scheme just like
/// `signed_policy::sign_streamid` does. Shared by the Farbplay `/api/watch` tests
/// and the admin `/srt-playback` ones — both mint through the same helper.
pub fn expected_signature(secret: &str, signed_path: &str) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("srt://{}", signed_path).as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
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

// ---------------------------------------------------------------------------
// Real-transport fixture, for WebSockets and anything else that needs a socket
// ---------------------------------------------------------------------------
//
// `test_app` above uses axum-test's default *mock* transport and builds only
// `routes::build_router` — the `/api/*` half. That is deliberately left alone:
// 200-odd tests run through it and a real port would only make them slower.
//
// Everything below builds the **whole** app (`app::build_app`) over a real
// HTTP port, which is what makes three things reachable that previously were
// not: the WebSocket hub, the static/SPA routes, and `ConnectInfo` (so the rate
// limiter actually has a key to extract).
//
// THREE HAZARDS, in rough order of how likely they are to bite:
//
// 1. `ws::WS_ROOMS`, `WS_ROOM_FOCUS`, `WS_ROOM_DISPLAY` and
//    `presence::SSE_PRESENCE` are process-global statics, and cargo runs a test
//    binary's cases on parallel threads. Two tests sharing a room slug will see
//    each other's sockets. **Always name rooms with `unique_slug`.**
// 2. WS tests need `#[tokio::test(flavor = "multi_thread")]`: the server task,
//    each socket's send task and the test body all have to make progress at the
//    same time.
// 3. The disconnect grace period is 3 s (`start_disconnect_timer`). A test
//    asserting post-disconnect state has to wait it out; a test asserting the
//    *reconnect* path has to reconnect inside it.

/// A room slug no other test can collide with. See hazard 1 above.
pub fn unique_slug(prefix: &str) -> String {
    format!("{}-{}", prefix, uuid::Uuid::new_v4().simple())
}

/// Populate the `web_root` fixture tree with the handful of files the static
/// mounts and the SPA handlers expect. The two HTML documents carry the same
/// `<title>Farbstrom</title>` and `</head>` markers the real pages do, since
/// `routes::pages` rewrites those exact strings.
pub fn seed_web_root(state: &Arc<AppState>) {
    let web = &state.config.web_root;
    for dir in ["admin", "shared", "dist", "privacy", "viewer", "landing"] {
        std::fs::create_dir_all(format!("{web}/{dir}")).unwrap();
    }
    let page = |body: &str| {
        format!(
            "<!doctype html><html><head><title>Farbstrom</title></head><body>{body}</body></html>"
        )
    };
    std::fs::write(format!("{web}/viewer/index.html"), page("viewer")).unwrap();
    std::fs::write(format!("{web}/landing/index.html"), page("landing")).unwrap();
    std::fs::write(format!("{web}/admin/index.html"), page("admin")).unwrap();
    std::fs::write(format!("{web}/privacy/index.html"), page("privacy")).unwrap();
    std::fs::write(format!("{web}/shared/favicon.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(format!("{web}/shared/tokens.css"), ":root{}").unwrap();
    std::fs::write(format!("{web}/dist/app.js"), "export {};").unwrap();
}

/// The full app on a real HTTP port. Use for WebSocket tests, the static/SPA
/// routes, and the response layers — none of which `test_app` can reach.
pub fn test_app_http(state: Arc<AppState>) -> TestServer {
    std::env::set_var("STREAM_DISABLE_RATE_LIMIT", "1");
    std::env::set_var("STREAM_DISABLE_OME_RESTART", "1");
    seed_web_root(&state);
    build_http_server(state)
}

/// `test_app_http` without the rate-limiter opt-out, for the one suite that
/// tests the limiter itself. It must live in its own test binary:
/// `rate_limit::enabled()` reads the env var when the router is built, and env
/// is process-global.
pub fn test_app_http_rate_limited(state: Arc<AppState>) -> TestServer {
    std::env::set_var("STREAM_DISABLE_OME_RESTART", "1");
    seed_web_root(&state);
    build_http_server(state)
}

fn build_http_server(state: Arc<AppState>) -> TestServer {
    // `into_make_service_with_connect_info` is what populates `ConnectInfo`, and
    // axum-test refuses to mock it — hence the real transport.
    TestServer::builder()
        .http_transport()
        .build(
            stream_backend::app::build_app(state)
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// WebSocket helpers
// ---------------------------------------------------------------------------

/// Open a socket to `/ws/room/{slug}` without authenticating.
pub async fn ws_connect(server: &TestServer, slug: &str) -> axum_test::TestWebSocket {
    server
        .get_websocket(&format!("/ws/room/{slug}"))
        .await
        .into_websocket()
        .await
}

/// Open a socket and authenticate. `client` is the native-client marker
/// (`Some("farbplay")`); browser viewers send none.
///
/// Returns the socket positioned immediately after `auth:ok` — the frames that
/// follow are the replayed focus/display state, chat history, and the roster.
pub async fn ws_auth(
    server: &TestServer,
    slug: &str,
    participant_id: &str,
    token: &str,
    client: Option<&str>,
) -> axum_test::TestWebSocket {
    let mut ws = ws_connect(server, slug).await;
    let mut frame = serde_json::json!({
        "type": "auth",
        "participantId": participant_id,
        "token": token,
    });
    if let Some(c) = client {
        frame["client"] = serde_json::json!(c);
    }
    ws.send_text(frame.to_string()).await;
    let first = next_json(&mut ws).await;
    assert_eq!(
        first["type"], "auth:ok",
        "expected auth:ok, got {first} — check the participant is admitted and the room is live"
    );
    ws
}

/// How long any single frame read will wait before giving up.
///
/// Everything here is local and in-process, so a frame that has not arrived in
/// this long is not coming. The timeout exists so a *failing* assertion fails
/// instead of hanging: `receive_text()` blocks forever once the server stops
/// sending, which turns a wrong expectation into a wedged test run rather than a
/// red one.
const FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Next frame as JSON. Panics if none arrives within [`FRAME_TIMEOUT`].
pub async fn next_json(ws: &mut axum_test::TestWebSocket) -> Value {
    let text = tokio::time::timeout(FRAME_TIMEOUT, ws.receive_text())
        .await
        .unwrap_or_else(|_| panic!("no WebSocket frame arrived within {FRAME_TIMEOUT:?}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("frame was not JSON ({e}): {text}"))
}

/// Read frames until one has `type == wanted`, or the budget runs out.
///
/// The hub interleaves roster broadcasts, chat history and state replay with
/// whatever a test is actually waiting for, so asserting on "the next frame" is
/// inherently racy. Panics with everything it saw, which is what you want when
/// a test fails.
pub async fn expect_frame(ws: &mut axum_test::TestWebSocket, wanted: &str) -> Value {
    let mut seen = Vec::new();
    for _ in 0..12 {
        let v = next_json(ws).await;
        if v["type"] == wanted {
            return v;
        }
        seen.push(v["type"].as_str().unwrap_or("?").to_string());
    }
    panic!("never saw a {wanted:?} frame; got: {seen:?}");
}

/// Assert no frame of type `unwanted` arrives among the next few.
///
/// Used for the negative cases — a viewer's `focus:set` must not broadcast, a
/// viewer must not receive waiting/kicked names. Bounded rather than timed, so
/// it cannot hang.
pub async fn expect_no_frame(ws: &mut axum_test::TestWebSocket, unwanted: &str, budget: usize) {
    for _ in 0..budget {
        match tokio::time::timeout(std::time::Duration::from_millis(300), ws.receive_text()).await {
            Ok(text) => {
                let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                assert_ne!(v["type"], unwanted, "unexpected {unwanted:?} frame: {v}");
            }
            // Nothing more is coming, which is the outcome we wanted.
            Err(_) => return,
        }
    }
}

/// Read up to `budget` messages, stopping at a Close frame.
///
/// Returns every JSON frame seen plus the close code, so a rejection test can
/// assert on both halves — the hub sends an explanatory frame *and then* closes,
/// and which one you get first is not something a test should depend on.
pub async fn ws_drain(
    ws: &mut axum_test::TestWebSocket,
    budget: usize,
) -> (Vec<Value>, Option<u16>) {
    let mut frames = Vec::new();
    for _ in 0..budget {
        let msg =
            match tokio::time::timeout(std::time::Duration::from_secs(2), ws.receive_message())
                .await
            {
                Ok(m) => m,
                Err(_) => break,
            };
        match msg {
            axum_test::WsMessage::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    frames.push(v);
                }
            }
            axum_test::WsMessage::Close(frame) => {
                return (frames, frame.map(|f| u16::from(f.code)));
            }
            _ => {}
        }
    }
    (frames, None)
}

/// Assert a socket was rejected: it saw a frame of `expected_type` and the
/// connection was closed with 1008 (policy violation).
pub async fn assert_rejected(ws: &mut axum_test::TestWebSocket, expected_type: &str) {
    let (frames, close) = ws_drain(ws, 6).await;
    let types: Vec<&str> = frames
        .iter()
        .map(|f| f["type"].as_str().unwrap_or("?"))
        .collect();
    assert!(
        types.contains(&expected_type),
        "expected a {expected_type:?} frame, saw {types:?}"
    );
    assert_eq!(close, Some(1008), "expected close 1008, got {close:?}");
}
