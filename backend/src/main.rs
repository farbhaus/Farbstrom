use stream_backend::config;
use stream_backend::db;
use stream_backend::events;
use stream_backend::routes;
use stream_backend::state;
use stream_backend::tasks;
use stream_backend::ws;

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

/// Redact sensitive query-string values before they land in tracing spans.
/// The admin `?token=…` fallback exists so `<img>` / `window.open` can reach
/// authenticated endpoints, but we do not want JWTs in request logs.
fn redact_query(q: &str) -> String {
    q.split('&')
        .map(|kv| {
            let mut it = kv.splitn(2, '=');
            let k = it.next().unwrap_or("");
            match k {
                "token" | "presenter_key" | "password" => format!("{k}=<redacted>"),
                _ => kv.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

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

    // Hash admin password at startup (same as Node.js: hash once, then forget plaintext)
    let admin_password = std::env::var("ADMIN_PASSWORD").expect("ADMIN_PASSWORD must be set");
    let admin_password_hash = tokio::task::spawn_blocking(move || {
        bcrypt::hash(admin_password, 12).expect("Failed to hash admin password")
    })
    .await
    .unwrap();
    tracing::info!("[startup] Admin password hashed");

    // Initialize database
    let db = db::init_pool(&config.db_path, &config.data_path);

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
    tasks::spawn_weekly_cleanup(state.clone());
    tasks::spawn_room_ended_cleanup(state.clone());

    // Spawn WebSocket event listeners
    ws::spawn_event_listeners(state.clone());

    // Build router
    let api_router = routes::build_router(state.clone());

    // Merge WS routes
    let ws_router = ws::router();

    // Build final app with static file serving
    let app = axum::Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(api_router)
        .merge(ws_router.with_state(state.clone()))
        .nest_service(
            "/admin",
            ServeDir::new("/www/admin").fallback(ServeFile::new("/www/admin/index.html")),
        )
        .nest_service("/shared", ServeDir::new("/www/shared"))
        .nest_service("/dist", ServeDir::new("/www/dist"))
        // Legacy /favicon.ico probes (browsers that ignore <link rel="icon">):
        // serve the shipped default rather than falling through to the SPA HTML.
        .route_service("/favicon.ico", ServeFile::new("/www/shared/favicon.png"))
        // Privacy / functional-cookie disclosure (static; themed via JS branding).
        .route_service("/privacy", ServeFile::new("/www/privacy/index.html"))
        // Landing + viewer go through handlers that inject brand-aware
        // link-preview (Open Graph) tags; the viewer handler is the SPA
        // catch-all for /watch/{slug} and any other unmatched path. These need
        // AppState, so they live in a state-applied sub-router that is merged in
        // (its fallback becomes the app's fallback).
        .merge(
            axum::Router::new()
                .route("/", get(routes::pages::serve_landing))
                .fallback(get(routes::pages::serve_viewer))
                .with_state(state.clone()),
        )
        // The SPA is served as un-hashed plain ES modules / HTML. Without an
        // explicit Cache-Control, browsers apply *heuristic* caching from
        // Last-Modified and can serve a stale bundle for hours after a
        // deploy (manifesting as "the new tab/feature doesn't work").
        // `no-cache` forces revalidation; ETag/Last-Modified still yield
        // cheap 304s, so this isn't a bandwidth regression.
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-cache"),
        ))
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &Request<Body>| {
                let uri_display = match req.uri().query() {
                    Some(q) => format!("{}?{}", req.uri().path(), redact_query(q)),
                    None => req.uri().path().to_string(),
                };
                tracing::info_span!(
                    "http_request",
                    method = %req.method(),
                    uri = %uri_display,
                )
            }),
        );

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
