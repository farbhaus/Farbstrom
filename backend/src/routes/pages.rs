//! HTML index pages (landing + viewer) served with server-rendered
//! link-preview metadata.
//!
//! The viewer is a static SPA served as `index.html` for every room slug, and
//! the tab title is set by JS — so social/link-preview crawlers (which don't run
//! JS) would otherwise only ever see the generic shipped `<title>`. These
//! handlers read the on-disk HTML, then inject Open Graph / Twitter tags and a
//! brand-aware `<title>` built from the configured `site_name` and logo before
//! serving. Real static assets (CSS/JS) are still served by `ServeDir` on their
//! own paths; only the HTML documents flow through here.

use axum::{extract::State, response::Html};
use std::sync::Arc;

use crate::error::AppError;
use crate::routes::branding::read_site_name;
use crate::state::AppState;

/// Paths of the two HTML documents these handlers rewrite, relative to
/// `config.web_root` (`/www` in the container).
const LANDING_HTML: &str = "landing/index.html";
const VIEWER_HTML: &str = "viewer/index.html";

/// Minimal HTML-attribute escaping for values injected into `content="…"` /
/// `<title>…</title>`. `site_name` is admin-set, but escape anyway so the brand
/// name can never break out of the attribute (defence-in-depth).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `GET /` — landing page with brand-level preview tags.
pub async fn serve_landing(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    serve_page(&state, LANDING_HTML, false).await
}

/// File extensions that mean "this was meant to be a static asset". A request
/// for one of these that reached the SPA fallback is a missing file, not a room
/// slug — room slugs never contain a dot (see `rooms::generate_slug`).
const ASSET_EXTENSIONS: &[&str] = &[
    "js",
    "mjs",
    "css",
    "map",
    "json",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "avif",
    "svg",
    "ico",
    "woff",
    "woff2",
    "ttf",
    "otf",
    "wasm",
    "txt",
    "xml",
    "webmanifest",
];

/// Catch-all viewer (`/watch/{slug}` and any other unmatched path) with
/// "<brand> streaming room" preview tags.
///
/// Asset-shaped paths get a real 404 instead. As the app-wide fallback this
/// handler would otherwise answer a mistyped `/dist/foo.js` with `200 OK` and a
/// body of HTML — which the browser then tries to parse as JavaScript, turning
/// a missing file into an unrelated syntax error. It also costs a file read, an
/// `fs::metadata` and a DB query per miss.
pub async fn serve_viewer(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> Result<Html<String>, AppError> {
    if path_looks_like_asset(uri.path()) {
        return Err(AppError::NotFound("Not found".into()));
    }
    serve_page(&state, VIEWER_HTML, true).await
}

/// True when the last path segment ends in a known static-asset extension.
pub fn path_looks_like_asset(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|seg| seg.rsplit_once('.'))
        .is_some_and(|(_, ext)| ASSET_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

async fn serve_page(
    state: &Arc<AppState>,
    relative_path: &str,
    is_room: bool,
) -> Result<Html<String>, AppError> {
    let path = format!(
        "{}/{}",
        state.config.web_root.trim_end_matches('/'),
        relative_path
    );
    let html = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::Internal(format!("read {path}: {e}")))?;

    let has_logo = tokio::fs::metadata(format!("{}/branding/logo", state.config.data_path))
        .await
        .is_ok();

    let conn = state.db.get()?;
    let site_name = tokio::task::spawn_blocking(move || read_site_name(&conn))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Absolute URLs so crawlers can fetch the preview image off-origin. Falls
    // back to the shipped default favicon when no brand logo is set.
    let origin = state.config.public_origin.trim_end_matches('/');
    let image = if has_logo {
        format!("{origin}/api/branding/logo")
    } else {
        format!("{origin}/shared/favicon.png")
    };

    let title = if is_room {
        format!("{site_name} — Streaming Room")
    } else {
        site_name.clone()
    };
    let description = if is_room {
        "Private low-latency streaming room."
    } else {
        "Private low-latency streaming platform."
    };

    let (t, d, s, img, sn) = (
        esc(&title),
        esc(description),
        esc(&site_name),
        esc(&image),
        esc(&site_name),
    );
    let meta = format!(
        "<meta name=\"brand-name\" content=\"{sn}\">\n\
         <meta property=\"og:title\" content=\"{t}\">\n\
         <meta property=\"og:description\" content=\"{d}\">\n\
         <meta property=\"og:type\" content=\"website\">\n\
         <meta property=\"og:site_name\" content=\"{sn}\">\n\
         <meta property=\"og:image\" content=\"{img}\">\n\
         <meta name=\"twitter:card\" content=\"summary_large_image\">\n\
         <meta name=\"twitter:title\" content=\"{t}\">\n\
         <meta name=\"twitter:description\" content=\"{d}\">\n\
         <meta name=\"twitter:image\" content=\"{img}\">\n"
    );

    // Replace the shipped placeholder title with the brand title (no-op when the
    // brand is the default), then inject the preview tags just before </head>.
    let out = html
        .replacen(
            "<title>Farbstrom</title>",
            &format!("<title>{s}</title>"),
            1,
        )
        .replacen("</head>", &format!("{meta}</head>"), 1);

    Ok(Html(out))
}

#[cfg(test)]
mod tests {
    use super::path_looks_like_asset;

    #[test]
    fn asset_paths_are_recognised() {
        for p in [
            "/dist/viewer/main.js",
            "/dist/admin/main.js.map",
            "/shared/tokens.css",
            "/favicon.ico",
            "/x/y/font.woff2",
            "/UPPER.PNG", // extension match is case-insensitive
        ] {
            assert!(path_looks_like_asset(p), "{p} should look like an asset");
        }
    }

    #[test]
    fn room_and_page_paths_are_not_assets() {
        // Room slugs are `<name>-<hex>` and never contain a dot, so no real
        // room URL can be mistaken for a static file.
        for p in [
            "/",
            "/watch/grade-review-a1b2c3",
            "/grade-review-a1b2c3",
            "/room-a1b2c3",
            "/privacy",
            "/admin",
        ] {
            assert!(!path_looks_like_asset(p), "{p} must still reach the SPA");
        }
    }

    #[test]
    fn unknown_extensions_still_reach_the_spa() {
        // Only *known* asset extensions 404. Anything else is treated as a
        // potential room path, so an unfamiliar suffix can't black-hole a URL.
        assert!(!path_looks_like_asset("/some.room.name"));
        assert!(!path_looks_like_asset("/v1.2.3"));
    }
}
