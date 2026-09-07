//! The complete Axum router.
//!
//! This used to be assembled inline in `main.rs`, which meant the only thing a
//! test could reach was `routes::build_router` — the `/api/*` half. The
//! WebSocket hub, the static mounts, the SPA fallback and the `Cache-Control`
//! layer existed nowhere a test could see them, so `ws.rs` had no coverage at
//! all and the cache header could only be checked by curling a running server.
//!
//! Background work (pollers, WS event listeners) deliberately stays in `main`:
//! it is process lifecycle, not routing, and a test wants to opt into listeners
//! per-case rather than have a 30 s OME poller running underneath it.

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::Router;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::routes;
use crate::state::AppState;
use crate::ws;

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

/// Build the full application router: API, WebSocket, static assets, the SPA
/// fallback, and the response layers.
pub fn build_app(state: Arc<AppState>) -> Router {
    let web = state.config.web_root.trim_end_matches('/').to_string();

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(routes::build_router(state.clone()))
        .merge(ws::router().with_state(state.clone()))
        .nest_service(
            "/admin",
            ServeDir::new(format!("{web}/admin"))
                .fallback(ServeFile::new(format!("{web}/admin/index.html"))),
        )
        .nest_service("/shared", ServeDir::new(format!("{web}/shared")))
        .nest_service("/dist", ServeDir::new(format!("{web}/dist")))
        // Legacy /favicon.ico probes (browsers that ignore <link rel="icon">):
        // serve the shipped default rather than falling through to the SPA HTML.
        .route_service(
            "/favicon.ico",
            ServeFile::new(format!("{web}/shared/favicon.png")),
        )
        // Privacy / functional-cookie disclosure (static; themed via JS branding).
        .route_service(
            "/privacy",
            ServeFile::new(format!("{web}/privacy/index.html")),
        )
        // Landing + viewer go through handlers that inject brand-aware
        // link-preview (Open Graph) tags; the viewer handler is the SPA
        // catch-all for /watch/{slug} and any other unmatched path. These need
        // AppState, so they live in a state-applied sub-router that is merged in
        // (its fallback becomes the app's fallback).
        .merge(
            Router::new()
                .route("/", get(routes::pages::serve_landing))
                .fallback(get(routes::pages::serve_viewer))
                .with_state(state),
        )
        // The SPA is served as un-hashed plain ES modules / HTML. Without an
        // explicit Cache-Control, browsers apply *heuristic* caching from
        // Last-Modified and can serve a stale bundle for hours after a
        // deploy (manifesting as "the new tab/feature doesn't work").
        // `no-cache` forces revalidation; ETag/Last-Modified still yield
        // cheap 304s, so this isn't a bandwidth regression.
        //
        // `if_not_present`, NOT `overriding`: this is an app-wide layer, and
        // `overriding` replaced the header on routes that deliberately set
        // their own — which silently defeated the 1-hour cache on
        // `/api/branding/{asset}`, so every page load re-fetched the logo and
        // background. Nothing else sets Cache-Control (ServeDir does not), so
        // the SPA still gets `no-cache`; a route that wants a different policy
        // now just says so and wins.
        .layer(SetResponseHeaderLayer::if_not_present(
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
        )
}

#[cfg(test)]
mod tests {
    use super::redact_query;

    #[test]
    fn secrets_are_redacted_from_logged_queries() {
        assert_eq!(redact_query("token=abc123"), "token=<redacted>");
        assert_eq!(
            redact_query("participantId=p1&token=abc"),
            "participantId=p1&token=<redacted>"
        );
        assert_eq!(
            redact_query("presenter_key=k&password=p"),
            "presenter_key=<redacted>&password=<redacted>"
        );
    }

    #[test]
    fn non_secret_params_survive() {
        assert_eq!(redact_query("display=1&n=Ana"), "display=1&n=Ana");
        // A param merely *containing* a secret name is not one.
        assert_eq!(redact_query("mytoken=keep"), "mytoken=keep");
    }
}
