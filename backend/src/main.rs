//! Process lifecycle: load config, open the DB, start background work, serve.
//!
//! The router itself lives in [`stream_backend::app::build_app`] so tests can
//! build the same one.

use stream_backend::app::build_app;
use stream_backend::config;
use stream_backend::db;
use stream_backend::events;
use stream_backend::state;
use stream_backend::tasks;
use stream_backend::ws;

use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    // Load .env file (optional, for local dev)
    let _ = dotenvy::dotenv();

    // Load and validate config (panics on missing required vars)
    let config = config::AppConfig::from_env();
    let port = config.port;

    // Hash the admin password once at startup, then forget the plaintext.
    // The value comes from the already-validated config rather than a second
    // `env::var` — the old duplicate read meant the minimum-length check and
    // the value actually used were two separate lookups.
    let admin_password = config.admin_password.clone();
    let admin_password_hash = tokio::task::spawn_blocking(move || {
        bcrypt::hash(admin_password, 12).expect("Failed to hash admin password")
    })
    .await
    .unwrap();
    tracing::info!("[startup] Admin password hashed");

    // Initialize database
    let db = db::init_pool(&config.db_path, &config.data_path);

    // SRT encryption is DB-managed (gh #208): write <data>/srt.env from the
    // settings table so OME — which starts after the backend — reads the current
    // passphrase from it.
    if let Ok(conn) = db.get() {
        stream_backend::srt::init_startup(&conn, &config.data_path);
    }

    // Create shared state
    let events = events::EventChannels::new();
    let http_client = reqwest::Client::new();
    let webauthn = Arc::new(stream_backend::credentials::build_webauthn(
        &config.public_origin,
    ));
    let state = Arc::new(state::AppState {
        db,
        events,
        config,
        http_client,
        admin_password_hash,
        metrics_samples: tokio::sync::Mutex::new(state::MetricsSamples::default()),
        webauthn,
        passkey_reg: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        passkey_auth: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    });

    // Ensure branding directory exists
    let branding_dir = format!("{}/branding", state.config.data_path);
    tokio::fs::create_dir_all(&branding_dir).await.ok();

    // Sweep any leftover upload-temp files from a previous crash. We
    // only care about files we wrote more than an hour ago — anything
    // newer might be an in-flight upload from a sibling worker (we
    // currently only run one, but the time bound is cheap insurance).
    stream_backend::uploads::sweep_stale_temps(
        &format!("{}/files", state.config.data_path),
        std::time::Duration::from_secs(3600),
    )
    .await;

    // Spawn background tasks
    tasks::spawn_ome_poller(state.clone());
    tasks::spawn_expiry_poller(state.clone());
    tasks::spawn_start_poller(state.clone());
    tasks::spawn_weekly_cleanup(state.clone());
    tasks::spawn_room_ended_cleanup(state.clone());

    // Spawn WebSocket event listeners
    ws::spawn_event_listeners(state.clone());

    let app = build_app(state);

    // Start server
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("stream-backend running on port {}", port);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    // `into_make_service_with_connect_info` surfaces the peer SocketAddr so
    // tower_governor's SmartIpKeyExtractor has a fallback when X-Forwarded-For
    // is absent (e.g., direct container-to-container traffic).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
