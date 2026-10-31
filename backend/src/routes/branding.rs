use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::auth::AdminAuth;
use crate::error::{multipart_error, AppError};
use crate::state::AppState;

async fn get_branding_status(State(state): State<Arc<AppState>>) -> Result<Json<Value>, AppError> {
    let data_path = state.config.data_path.clone();
    let has_logo = tokio::fs::metadata(format!("{}/branding/logo", data_path))
        .await
        .is_ok();
    let has_bg = tokio::fs::metadata(format!("{}/branding/bg", data_path))
        .await
        .is_ok();
    let has_favicon = tokio::fs::metadata(format!("{}/branding/favicon", data_path))
        .await
        .is_ok();

    // Include color palette + site name in branding status for one-call loading
    let conn = state.db.get()?;
    let (colors, site_name) = tokio::task::spawn_blocking(move || {
        let mut result = serde_json::Map::new();
        for &key in COLOR_KEYS {
            if let Ok(val) = conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            ) {
                result.insert(key.to_string(), Value::String(val));
            }
        }
        let site_name = read_site_name(&conn);
        (result, site_name)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(json!({
        "hasLogo": has_logo,
        "hasBg": has_bg,
        "hasFavicon": has_favicon,
        "siteName": site_name,
        "colors": colors,
    })))
}

async fn get_asset(
    State(state): State<Arc<AppState>>,
    Path(asset): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if asset != "logo" && asset != "bg" && asset != "favicon" {
        return Err(AppError::NotFound("Asset not found".into()));
    }

    let file_path = format!("{}/branding/{}", state.config.data_path, asset);
    let data = tokio::fs::read(&file_path)
        .await
        .map_err(|_| AppError::NotFound("Asset not found".into()))?;

    // Get MIME type from settings
    let key = format!("{}_mime", asset);
    let conn = state.db.get()?;
    let mime: String = tokio::task::spawn_blocking(move || {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "application/octet-stream".into())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=3600".into()),
            // Defense-in-depth: never let the browser sniff a served asset
            // into something executable, even though uploads are now
            // restricted to PNG/JPEG.
            (
                header::HeaderName::from_static("x-content-type-options"),
                "nosniff".to_string(),
            ),
        ],
        data,
    ))
}

/// Largest logo / background / favicon accepted. Without its own limit the
/// route had axum's 2 MB default, which a full-resolution background JPEG
/// overruns — and the failure surfaced as a multipart parse error.
pub const BRANDING_MAX_BYTES: usize = 10 * 1024 * 1024;

/// The route's body limit sits above the image cap by the drain allowance, so an
/// oversized image can be read to the end and refused cleanly (see
/// `uploads::drain`) rather than cut off mid-request — only the first
/// `BRANDING_MAX_BYTES` are ever held in memory.
const BRANDING_BODY_LIMIT: usize = BRANDING_MAX_BYTES + crate::uploads::DRAIN_MAX_BYTES as usize;

fn image_too_large() -> AppError {
    AppError::PayloadTooLarge(format!(
        "Image too large (max {} MB)",
        BRANDING_MAX_BYTES / (1024 * 1024)
    ))
}

fn too_large(e: axum::extract::multipart::MultipartError) -> AppError {
    match multipart_error(e) {
        AppError::PayloadTooLarge(_) => image_too_large(),
        other => other,
    }
}

async fn upload_asset(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(asset): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    if asset != "logo" && asset != "bg" && asset != "favicon" {
        return Err(AppError::BadRequest("Invalid asset type".into()));
    }

    while let Some(mut field) = multipart.next_field().await.map_err(too_large)? {
        if field.name() == Some("file") {
            let mime = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();
            // Strict per-asset image allowlist. Branding assets are served
            // publicly from this origin, so an HTML/SVG upload served back with
            // its own content-type would be same-origin stored XSS — the CSP's
            // `script-src 'self'` would not stop a script served from here.
            // SVG is deliberately excluded (it can embed <script>); logo is
            // PNG-only, background is JPEG-only.
            let allowed = match asset.as_str() {
                "logo" => mime == "image/png",
                "bg" => mime == "image/jpeg" || mime == "image/jpg",
                // Favicon: raster only. SVG is excluded here too — it can embed
                // <script> and is served same-origin. PNG or ICO covers every
                // browser (modern picks PNG, legacy /favicon.ico picks ICO).
                "favicon" => {
                    mime == "image/png"
                        || mime == "image/x-icon"
                        || mime == "image/vnd.microsoft.icon"
                }
                _ => false,
            };
            if !allowed {
                return Err(AppError::BadRequest(match asset.as_str() {
                    "logo" => "Logo must be a PNG".into(),
                    "favicon" => "Favicon must be a PNG or ICO".into(),
                    _ => "Background must be a JPEG".into(),
                }));
            }
            let mut data = Vec::new();
            while let Some(chunk) = field.chunk().await.map_err(too_large)? {
                if data.len() + chunk.len() > BRANDING_MAX_BYTES {
                    crate::uploads::discard_rest(&mut field).await;
                    return Err(image_too_large());
                }
                data.extend_from_slice(&chunk);
            }

            let dir = format!("{}/branding", state.config.data_path);
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            tokio::fs::write(format!("{}/{}", dir, asset), &data)
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;

            // Store MIME in settings
            let key = format!("{}_mime", asset);
            let conn = state.db.get()?;
            tokio::task::spawn_blocking(move || {
                conn.execute(
                    "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                    params![key, mime],
                )
            })
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;

            return Ok(Json(json!({ "ok": true })));
        }
    }

    Err(AppError::BadRequest("No file".into()))
}

async fn delete_asset(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(asset): Path<String>,
) -> Result<Json<Value>, AppError> {
    if asset != "logo" && asset != "bg" && asset != "favicon" {
        return Err(AppError::BadRequest("Invalid asset type".into()));
    }

    let file_path = format!("{}/branding/{}", state.config.data_path, asset);
    let _ = tokio::fs::remove_file(&file_path).await;

    // Remove MIME setting
    let key = format!("{}_mime", asset);
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({ "ok": true })))
}

/// Accept only a CSS hex colour (`#rgb`, `#rrggbb`, `#rrggbbaa`).
///
/// These values are written straight into a CSS custom property client-side via
/// `setProperty`, so a malformed one is not an injection vector — the CSSOM
/// takes the value, not a declaration. It is a *breakage* vector: an
/// unparseable custom property makes every `var(--accent)` in the sheet resolve
/// to nothing, which strips colour from the whole UI with no error anywhere.
/// Every other input in this file is strictly allowlisted; this one wasn't.
fn is_hex_color(v: &str) -> bool {
    let Some(hex) = v.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

const COLOR_KEYS: &[&str] = &[
    "color_accent",
    "color_bg",
    "color_surface",
    "color_text",
    "color_danger",
    "color_green",
];

/// Fallback brand name when no `site_name` setting is configured. Used for the
/// browser title, the wordmark, and the link-preview (Open Graph) tags.
pub const DEFAULT_SITE_NAME: &str = "Farbstrom";

/// Read the configured brand name, falling back to [`DEFAULT_SITE_NAME`].
/// Synchronous — call from a `spawn_blocking` context or a blocking handler.
pub fn read_site_name(conn: &rusqlite::Connection) -> String {
    conn.query_row(
        "SELECT value FROM settings WHERE key = 'site_name'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .filter(|s| !s.trim().is_empty())
    .unwrap_or_else(|| DEFAULT_SITE_NAME.to_string())
}

async fn save_site_name(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    // Trim and length-cap; this string is injected into HTML <meta>/<title>
    // (escaped at render time) and shown as the wordmark, so keep it sane.
    let name = body
        .get("siteName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .chars()
        .take(64)
        .collect::<String>();

    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        if name.is_empty() {
            // Empty resets to the default brand name.
            conn.execute("DELETE FROM settings WHERE key = 'site_name'", [])
        } else {
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('site_name', ?1)",
                params![name],
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({ "ok": true })))
}

async fn save_colors(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    // Validate before opening a transaction, so a bad value rejects the whole
    // request rather than leaving a half-applied palette.
    for &key in COLOR_KEYS {
        if let Some(val) = body.get(key).and_then(|v| v.as_str()) {
            if !val.is_empty() && !is_hex_color(val) {
                return Err(AppError::BadRequest(format!(
                    "{key} must be a hex colour like #1a2b3c"
                )));
            }
        }
    }

    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        for &key in COLOR_KEYS {
            if let Some(val) = body.get(key).and_then(|v| v.as_str()) {
                if val.is_empty() {
                    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
                } else {
                    conn.execute(
                        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                        params![key, val],
                    )?;
                }
            }
        }
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({ "ok": true })))
}

pub fn public_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(get_branding_status))
        .route("/{asset}", get(get_asset))
}

pub fn admin_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/colors", post(save_colors))
        .route("/site-name", post(save_site_name))
        .route(
            "/{asset}",
            post(upload_asset)
                .layer(DefaultBodyLimit::max(BRANDING_BODY_LIMIT))
                .delete(delete_asset),
        )
}
