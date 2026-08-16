use axum::{body::Bytes, extract::State, http::HeaderMap, routing::post, Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha1::Sha1;
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::error::AppError;
use crate::state::AppState;

type HmacSha1 = Hmac<Sha1>;

fn extract_stream_key(url: &str) -> Option<String> {
    let url_str = if url.starts_with("http") || url.starts_with("rtmp") {
        url.to_string()
    } else {
        format!("http://x{}", url)
    };
    let path = if let Some(pos) = url_str.find("://") {
        let after_scheme = &url_str[pos + 3..];
        after_scheme
            .find('/')
            .map(|p| &after_scheme[p..])
            .unwrap_or("")
    } else {
        &url_str
    };
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    parts
        .last()
        .map(|s| s.split('?').next().unwrap_or(s).to_string())
}

async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, AppError> {
    // Verify HMAC signature
    let signature = headers
        .get("x-ome-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized("Missing signature".into()))?;

    let secret = &state.config.ome_webhook_secret;
    let mut mac = HmacSha1::new_from_slice(secret.as_bytes())
        .map_err(|e| AppError::Internal(format!("HMAC init error: {}", e)))?;
    mac.update(&body);
    let result = mac.finalize().into_bytes();
    let expected = URL_SAFE_NO_PAD.encode(result);

    if expected.as_bytes().ct_eq(signature.as_bytes()).unwrap_u8() == 0 {
        return Err(AppError::Unauthorized("Invalid signature".into()));
    }

    // Parse body as JSON
    let payload: Value =
        serde_json::from_slice(&body).map_err(|e| AppError::BadRequest(e.to_string()))?;

    let request = payload
        .get("request")
        .ok_or_else(|| AppError::BadRequest("Missing request object".into()))?;

    let direction = request
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if direction != "incoming" {
        return Ok(Json(json!({ "allowed": true })));
    }

    // Extract stream key from URL
    let url = request.get("url").and_then(|v| v.as_str()).unwrap_or("");

    let stream_key = extract_stream_key(url)
        .ok_or_else(|| AppError::BadRequest("Cannot extract stream key".into()))?;

    // OME calls back with status "closing" when the publisher disconnects.
    // Acting on it drops the room to 'pending' at once instead of leaving
    // viewers on a dead stream for up to a poll cycle. The poller stays the
    // backstop for ingests that die without a clean close (killed encoder,
    // dropped network), which is why it isn't replaced by this.
    let status = request.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "closing" {
        let conn = state.db.get()?;
        let events = state.events.clone();
        let key = stream_key.clone();
        let slugs = tokio::task::spawn_blocking(move || {
            let mut stmt = conn.prepare(
                "SELECT r.id, r.slug FROM rooms r \
                 JOIN stream_keys sk ON sk.id = r.stream_key_id \
                 WHERE sk.key_token = ?1 AND r.status = 'live'",
            )?;
            let rooms: Vec<(String, String)> = stmt
                .query_map(params![key], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            let mut slugs = Vec::new();
            for (room_id, slug) in &rooms {
                conn.execute(
                    "UPDATE rooms SET status = 'pending' WHERE id = ?1",
                    params![room_id],
                )?;
                slugs.push(slug.clone());
            }
            Ok::<_, rusqlite::Error>(slugs)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

        for slug in &slugs {
            let _ = events.room_pending.send(slug.clone());
        }
        // A close is a notification, not a request to authorise.
        return Ok(Json(json!({ "allowed": true })));
    }

    // Authorize the ingest: the key must be one an admin explicitly created.
    // The HMAC check above only proves the request came from OME — this is the
    // actual stream-key gate. An unrecognised key (e.g. someone guessing
    // "stream") is denied with {"allowed": false}, which OME honours by
    // rejecting the publish. For a known key, mark any rooms it's assigned to
    // live.
    let events = state.events.clone();
    let conn = state.db.get()?;
    let (key_known, blocked, slugs) = tokio::task::spawn_blocking(move || {
        let block_state: Option<i64> = conn
            .query_row(
                "SELECT blocked FROM stream_keys WHERE key_token = ?1",
                params![stream_key],
                |row| row.get(0),
            )
            .optional()?;
        let key_known = block_state.is_some();
        let blocked = block_state == Some(1);
        // Unknown or admin-blocked keys never go live — bail before touching rooms.
        if !key_known || blocked {
            return Ok::<_, rusqlite::Error>((key_known, blocked, Vec::new()));
        }

        // Find rooms with this stream key and mark them live.
        //
        // 'ended' is terminal: an expired or explicitly ended room must not be
        // resurrected just because an encoder started pushing its old key. The
        // expiry and start pollers both already treat 'ended' that way; this
        // was the one path that didn't.
        //
        // 'scheduled' deliberately still goes live — a host starting early
        // opens the room, and `tasks::poll_starts` is written to expect exactly
        // that (it also releases held viewers for rooms the webhook already
        // flipped). Don't narrow this to 'pending' without reading that poller.
        let mut stmt = conn.prepare(
            "SELECT r.id, r.slug FROM rooms r \
             JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE sk.key_token = ?1 AND r.status != 'ended'",
        )?;
        let rooms: Vec<(String, String)> = stmt
            .query_map(params![stream_key], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut slugs = Vec::new();
        for (room_id, slug) in &rooms {
            // Re-check the status in the UPDATE: a room can be ended between
            // the SELECT above and here. Only announce rooms we actually
            // changed, so a lost race can't emit room:live for an ended room.
            let changed = conn.execute(
                "UPDATE rooms SET status = 'live' WHERE id = ?1 AND status != 'ended'",
                params![room_id],
            )?;
            if changed > 0 {
                slugs.push(slug.clone());
            }
        }
        Ok((true, false, slugs))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // Deny ingests whose key was never created in the admin.
    if !key_known {
        tracing::warn!(
            action = "admission_denied",
            "ingest rejected: unknown stream key"
        );
        return Ok(Json(json!({ "allowed": false })));
    }

    // Deny a key an admin kicked — this is what stops the encoder's immediate
    // auto-reconnect after a stream kick, until the admin clears the block.
    if blocked {
        tracing::warn!(
            action = "admission_denied",
            "ingest rejected: stream key blocked"
        );
        return Ok(Json(json!({ "allowed": false })));
    }

    // Emit room:live events
    for slug in &slugs {
        let _ = events.room_live.send(slug.clone());
    }

    Ok(Json(json!({ "allowed": true })))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", post(webhook_handler))
}
