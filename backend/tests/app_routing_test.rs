//! The half of the app that lives outside `routes::build_router`: static
//! mounts, the SPA fallback, and the response layers.
//!
//! None of this was reachable before `app::build_app` existed. The cache-header
//! behaviour in particular had only ever been checked by starting a server and
//! curling it by hand.

mod common;

use axum::http::header;
use common::*;

// ---------------------------------------------------------------------------
// Cache-Control
// ---------------------------------------------------------------------------

/// The app-wide layer is `if_not_present`, not `overriding`.
///
/// With `overriding` it replaced the header on routes that set their own, which
/// silently defeated the 1-hour cache on branding assets — so every page load
/// re-fetched the logo and background.
#[tokio::test(flavor = "multi_thread")]
async fn branding_assets_keep_their_own_cache_policy() {
    let state = test_state();
    let dir = format!("{}/branding", state.config.data_path);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/logo"), b"\x89PNG\r\n\x1a\n").unwrap();

    let server = test_app_http(state);
    let res = server.get("/api/branding/logo").await;
    assert_eq!(res.status_code(), 200);
    assert_eq!(
        res.header(header::CACHE_CONTROL),
        "public, max-age=3600",
        "the global layer clobbered the branding route's own policy"
    );
}

/// ...while everything without its own policy still gets `no-cache`, which is
/// what that layer exists for: the SPA ships un-hashed modules and would
/// otherwise be heuristically cached for hours after a deploy.
#[tokio::test(flavor = "multi_thread")]
async fn everything_else_gets_no_cache() {
    let state = test_state();
    let server = test_app_http(state);

    for path in ["/healthz", "/", "/dist/app.js"] {
        let res = server.get(path).await;
        assert_eq!(
            res.header(header::CACHE_CONTROL),
            "no-cache",
            "{path} should revalidate"
        );
    }
}

// ---------------------------------------------------------------------------
// SPA fallback
// ---------------------------------------------------------------------------

/// A missing asset must 404, not return the viewer page with a 200. The browser
/// parses that HTML as JavaScript and reports a syntax error, which hides the
/// real problem.
///
/// This is the case my earlier attempt got wrong: written against the API-only
/// router it passed while testing nothing, because everything unmatched 404s
/// there anyway. Here the fallback is genuinely mounted.
#[tokio::test(flavor = "multi_thread")]
async fn missing_assets_404_rather_than_serving_the_spa() {
    let state = test_state();
    let server = test_app_http(state);

    for path in [
        "/dist/viewer/typo.js",
        "/dist/admin/main.js.map",
        "/shared/missing.css",
        "/nope.png",
        "/x/y/z.woff2",
    ] {
        let res = server.get(path).await;
        assert_eq!(res.status_code(), 404, "{path} should 404");
    }
}

/// ...and a room slug still renders the viewer. Slugs never contain a dot, so
/// the extension check cannot swallow one.
#[tokio::test(flavor = "multi_thread")]
async fn room_paths_render_the_viewer() {
    let state = test_state();
    let server = test_app_http(state);

    for path in [
        "/watch/grade-review-a1b2c3",
        "/grade-review-a1b2c3",
        "/room-a1b2c3",
    ] {
        let res = server.get(path).await;
        assert_eq!(res.status_code(), 200, "{path} should render the viewer");
        assert!(
            res.text().contains("viewer"),
            "{path} served the wrong page"
        );
    }
}

/// Real static files are still served.
#[tokio::test(flavor = "multi_thread")]
async fn static_mounts_serve_real_files() {
    let state = test_state();
    let server = test_app_http(state);

    assert_eq!(server.get("/dist/app.js").await.status_code(), 200);
    assert_eq!(server.get("/shared/tokens.css").await.status_code(), 200);
    assert_eq!(server.get("/favicon.ico").await.status_code(), 200);
    assert_eq!(server.get("/privacy").await.status_code(), 200);
}

// ---------------------------------------------------------------------------
// Link-preview injection
// ---------------------------------------------------------------------------

/// Crawlers do not run JS, so the brand title and Open Graph tags are injected
/// server-side. The injection works by string-replacing `<title>Farbstrom</title>`
/// and `</head>` — if either marker ever changes in the shipped HTML, this
/// silently becomes a no-op.
#[tokio::test(flavor = "multi_thread")]
async fn pages_inject_open_graph_tags() {
    let state = test_state();
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('site_name', 'Acme Colour')",
            [],
        )
        .unwrap();
    }
    let server = test_app_http(state);

    let body = server.get("/").await.text();
    assert!(
        body.contains("<title>Acme Colour</title>"),
        "brand title was not substituted — did the <title> marker change?"
    );
    assert!(body.contains(r#"<meta property="og:title" content="Acme Colour">"#));
    assert!(body.contains(r#"<meta name="brand-name" content="Acme Colour">"#));

    // The viewer gets the room-flavoured variant.
    let room = server.get("/watch/some-room-a1b2c3").await.text();
    assert!(room.contains("Acme Colour — Streaming Room"));
}

/// `site_name` is admin-set and lands inside an HTML attribute, so it is
/// escaped on the way in.
#[tokio::test(flavor = "multi_thread")]
async fn brand_name_is_escaped_into_the_meta_tags() {
    let state = test_state();
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('site_name', ?1)",
            rusqlite::params![r#"Ev"il<script>"#],
        )
        .unwrap();
    }
    let server = test_app_http(state);

    let body = server.get("/").await.text();
    assert!(
        !body.contains("<script>Ev") && !body.contains(r#"content="Ev"il"#),
        "brand name was not escaped into the page"
    );
    assert!(body.contains("&quot;il&lt;script&gt;"));
}
