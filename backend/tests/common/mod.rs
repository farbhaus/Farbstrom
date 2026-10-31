#![allow(dead_code)]

use axum_test::TestServer;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
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
    let ome_log_path = format!("{}/ovenmediaengine.log", data_path);
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
        // Absent until a test writes it, which is the "no log file" case.
        ome_log_path,
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
    let token_version = stream_backend::credentials::token_version_get(&pool.get().unwrap());

    Arc::new(AppState {
        db: pool,
        events,
        config,
        http_client: reqwest::Client::new(),
        admin_password_hash,
        admin_token_version: std::sync::atomic::AtomicU64::new(token_version),
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
    TestServer::new(router)
}

/// An admin JWT stamped with this state's current token generation. Minting it
/// from the live counter (rather than a hardcoded 0) keeps it valid after a test
/// exercises revocation.
pub fn admin_token(state: &Arc<AppState>) -> String {
    auth::create_admin_token(
        &state.config.jwt_secret,
        state
            .admin_token_version
            .load(std::sync::atomic::Ordering::SeqCst),
        auth::ADMIN_TOKEN_TTL_SECS,
    )
}

/// One room as the admin sees it, read from the room list (`GET /api/rooms`) —
/// the only way the admin UI reads rooms. Panics if the room is not there.
pub async fn admin_room(server: &TestServer, admin_token: &str, room_id: &str) -> Value {
    let rooms: Vec<Value> = server
        .get("/api/rooms")
        .add_header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {admin_token}")
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        )
        .await
        .json();
    rooms
        .into_iter()
        .find(|r| r["id"] == room_id)
        .unwrap_or_else(|| panic!("room {room_id} is not in the admin room list"))
}

/// Returns `(id, key_token)`. The key also gets its own `playback_id` — the
/// name OME publishes it under — deliberately different from the key_token, so
/// a test can't pass by matching the wrong one. Read it with [`playback_id`].
pub fn seed_stream_key(state: &Arc<AppState>, name: &str) -> (String, String) {
    let conn = state.db.get().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let hex = || -> String {
        (0..24)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect()
    };
    let key_token = hex();
    conn.execute(
        "INSERT INTO stream_keys (id, name, key_token, playback_id) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, name, key_token, hex()],
    )
    .unwrap();
    (id, key_token)
}

/// The name OME publishes a seeded key's stream under.
pub fn playback_id(state: &Arc<AppState>, key_id: &str) -> String {
    state
        .db
        .get()
        .unwrap()
        .query_row(
            "SELECT playback_id FROM stream_keys WHERE id = ?1",
            rusqlite::params![key_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// A stand-in OME whose "currently broadcasting" list can be changed after the
/// server is up — so a test can build its AppState first, seed a stream key,
/// and only then declare its stream live. Entries are OME stream names, i.e.
/// playback ids.
pub type Broadcasting = Arc<std::sync::Mutex<Vec<String>>>;

pub async fn fake_ome() -> (String, Broadcasting) {
    let active: Broadcasting = Arc::new(std::sync::Mutex::new(Vec::new()));
    let handler_state = active.clone();
    let app = axum::Router::new().route(
        "/vhosts/default/apps/live/streams",
        axum::routing::get(move || {
            let active = handler_state.clone();
            async move {
                let names = active.lock().unwrap().clone();
                axum::Json(serde_json::json!({
                    "statusCode": 200,
                    "message": "OK",
                    "response": names,
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{}", addr), active)
}

/// AppState wired to a fake OME. Returns the handle for declaring streams live.
pub async fn state_with_fake_ome() -> (Arc<AppState>, Broadcasting) {
    let (url, active) = fake_ome().await;
    let mut config = test_config();
    config.ome_api_url = url;
    (test_state_with_config(config), active)
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
    TestServer::builder().http_transport().build(
        stream_backend::app::build_app(state)
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
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

// ---------------------------------------------------------------------------
// A WebSocket client that only does what it is told
// ---------------------------------------------------------------------------

/// What [`RawWs::next`] read off the wire.
#[derive(Debug, PartialEq)]
pub enum RawFrame {
    Text(String),
    Close(u16),
}

/// A minimal WebSocket client that never answers a Close frame.
///
/// axum-test's client (tungstenite) replies to a server Close on its own, which
/// makes it a cooperative client by construction. The hub has to hold up against
/// one that isn't — a kicked participant whose modified client simply keeps
/// sending — so these tests speak the protocol by hand.
pub struct RawWs {
    stream: tokio::net::TcpStream,
    buf: Vec<u8>,
}

impl RawWs {
    pub async fn connect(server: &TestServer, slug: &str) -> RawWs {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = server.server_address().expect("needs test_app_http");
        let host = format!("{}:{}", addr.host_str().unwrap(), addr.port().unwrap());
        let mut stream = tokio::net::TcpStream::connect(&host).await.unwrap();
        let request = format!(
            "GET /ws/room/{slug} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut chunk = [0u8; 1024];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "server closed during the handshake");
            buf.extend_from_slice(&chunk[..n]);
        };
        assert!(
            buf.starts_with(b"HTTP/1.1 101"),
            "handshake refused: {}",
            String::from_utf8_lossy(&buf[..end])
        );
        RawWs {
            stream,
            buf: buf[end..].to_vec(),
        }
    }

    /// Connect and authenticate, returning once `auth:ok` has arrived.
    pub async fn auth(server: &TestServer, slug: &str, participant_id: &str, token: &str) -> RawWs {
        let mut ws = RawWs::connect(server, slug).await;
        ws.send_json(&serde_json::json!({
            "type": "auth", "participantId": participant_id, "token": token,
        }))
        .await;
        match ws.next().await {
            Some(RawFrame::Text(t)) if t.contains("auth:ok") => ws,
            other => panic!("expected auth:ok, got {other:?}"),
        }
    }

    /// Send a (masked) text frame. False once the server has hung up.
    pub async fn send_json(&mut self, value: &Value) -> bool {
        use tokio::io::AsyncWriteExt;
        let payload = value.to_string().into_bytes();
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        let mut frame = vec![0x81u8];
        match payload.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n < 65_536 => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&frame).await.is_ok()
    }

    /// The next frame, or `None` if nothing arrives within the frame timeout or
    /// the connection is gone. A Close is reported, never answered.
    pub async fn next(&mut self) -> Option<RawFrame> {
        use tokio::io::AsyncReadExt;
        loop {
            if self.buf.len() >= 2 {
                let opcode = self.buf[0] & 0x0f;
                let (len, head) = match self.buf[1] & 0x7f {
                    126 if self.buf.len() >= 4 => {
                        (u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize, 4)
                    }
                    127 if self.buf.len() >= 10 => (
                        u64::from_be_bytes(self.buf[2..10].try_into().unwrap()) as usize,
                        10,
                    ),
                    n @ 0..=125 => (n as usize, 2),
                    _ => (usize::MAX, 0),
                };
                if len != usize::MAX && self.buf.len() >= head + len {
                    let payload: Vec<u8> = self.buf.drain(..head + len).skip(head).collect();
                    match opcode {
                        1 => return Some(RawFrame::Text(String::from_utf8(payload).unwrap())),
                        8 => {
                            let code = payload
                                .get(..2)
                                .map(|b| u16::from_be_bytes([b[0], b[1]]))
                                .unwrap_or(1005);
                            return Some(RawFrame::Close(code));
                        }
                        _ => continue,
                    }
                }
            }
            let mut chunk = [0u8; 4096];
            match tokio::time::timeout(FRAME_TIMEOUT, self.stream.read(&mut chunk)).await {
                Ok(Ok(n)) if n > 0 => self.buf.extend_from_slice(&chunk[..n]),
                _ => return None,
            }
        }
    }
}
