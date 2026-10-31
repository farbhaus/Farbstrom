//! OME admission webhook. OME asks here before every ingest and before every
//! browser playback session, and notifies here when either closes.
//!
//! - **Ingest** (`direction: incoming`): the key in the URL must be one an admin
//!   created and has not blocked. An allowed stream is renamed to its key's
//!   `playback_id` through `new_url`, so the name OME publishes under — the one
//!   viewers are handed — can never be used to push video.
//! - **Playback** (`direction: outgoing`, WebRTC and LL-HLS): see
//!   [`crate::playback`].

use axum::{body::Bytes, extract::State, http::HeaderMap, routing::post, Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha1::Sha1;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use url::Url;

use crate::error::AppError;
use crate::playback;
use crate::state::AppState;

type HmacSha1 = Hmac<Sha1>;

/// How long after a publisher's `closing` notification its rooms are checked
/// against OME before dropping to `pending`.
///
/// The notification alone can't decide it: OME sends `closing` for *every*
/// session that ends, including a second encoder it refused as a duplicate of
/// the one still live, and including one the webhook itself denied. Demoting on
/// the notification let anyone holding the key blank the stream for the whole
/// room. OME takes a stream off its list well inside this window when the real
/// publisher leaves (measured at under 300 ms), and the 30 s poller remains the
/// backstop when it doesn't.
const CLOSING_RECHECK: Duration = Duration::from_secs(2);

/// The stream an OME request URL addresses, with the URL parsed.
///
/// OME names a stream by the path segment after the application, whatever the
/// protocol: `rtmp://host:1935/live/<s>`, `srt://default/live/<s>` (vhost as
/// host), `http://host/live/<s>?direction=whip`, `http://host/live/<s>/llhls.m3u8`
/// and `ws://host:3333/live/<s>`.
fn parse_ome_url(raw: &str) -> Option<(Url, String)> {
    let url = Url::parse(raw)
        .or_else(|_| Url::parse(&format!("http://x{raw}")))
        .ok()?;
    let stream = url
        .path_segments()?
        .filter(|s| !s.is_empty())
        .nth(1)?
        .to_string();
    Some((url, stream))
}

/// `url` with its stream segment replaced by `stream`. Scheme, host, port, app
/// and query are untouched — OME refuses a `new_url` that changes any of them.
fn with_stream_name(url: &Url, stream: &str) -> Option<String> {
    let mut segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    *segments.get_mut(1)? = stream;
    let mut renamed = url.clone();
    renamed.set_path(&format!("/{}", segments.join("/")));
    Some(renamed.to_string())
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
    let field = |name: &str| request.get(name).and_then(|v| v.as_str()).unwrap_or("");
    let (direction, status, url) = (field("direction"), field("status"), field("url"));

    match direction {
        "incoming" => ingest(&state, url, status).await,
        "outgoing" => playback_session(&state, url, status).await,
        // OME sends nothing else; refuse rather than guess.
        _ => Ok(Json(json!({ "allowed": false }))),
    }
}

async fn ingest(state: &Arc<AppState>, url: &str, status: &str) -> Result<Json<Value>, AppError> {
    let (url, stream_key) = parse_ome_url(url)
        .ok_or_else(|| AppError::BadRequest("Cannot extract stream key".into()))?;

    // A publisher went away. Answer at once and re-check afterwards: OME is
    // blocked on this reply, and asking OME's API from inside it stalls both
    // until OME's 5 s webhook timeout (measured).
    if status == "closing" {
        tokio::spawn(recheck_after_close(state.clone(), stream_key));
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
    let (key, slugs) = tokio::task::spawn_blocking(move || {
        let key: Option<(i64, String)> = conn
            .query_row(
                "SELECT blocked, playback_id FROM stream_keys WHERE key_token = ?1",
                params![stream_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        // Unknown or admin-blocked keys never go live — bail before touching rooms.
        if !matches!(key, Some((0, _))) {
            return Ok::<_, rusqlite::Error>((key, Vec::new()));
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
        Ok((key, slugs))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let playback_id = match key {
        Some((0, playback_id)) => playback_id,
        // Deny ingests whose key was never created in the admin.
        None => {
            tracing::warn!(
                action = "admission_denied",
                "ingest rejected: unknown stream key"
            );
            return Ok(Json(json!({ "allowed": false })));
        }
        // Deny a key an admin kicked — this is what stops the encoder's
        // immediate auto-reconnect after a stream kick, until the admin clears
        // the block.
        Some(_) => {
            tracing::warn!(
                action = "admission_denied",
                "ingest rejected: stream key blocked"
            );
            return Ok(Json(json!({ "allowed": false })));
        }
    };

    // Emit room:live events
    for slug in &slugs {
        let _ = events.room_live.send(slug.clone());
    }

    let new_url = with_stream_name(&url, &playback_id)
        .ok_or_else(|| AppError::BadRequest("Cannot rename stream".into()))?;
    Ok(Json(json!({ "allowed": true, "new_url": new_url })))
}

/// Drop the rooms on `key_token` to `pending` if OME no longer publishes its
/// stream; see [`CLOSING_RECHECK`].
async fn recheck_after_close(state: Arc<AppState>, key_token: String) {
    tokio::time::sleep(CLOSING_RECHECK).await;

    let Ok(conn) = state.db.get() else { return };
    let key = key_token.clone();
    let playback_id: Option<String> = tokio::task::spawn_blocking(move || {
        conn.query_row(
            "SELECT playback_id FROM stream_keys WHERE key_token = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    })
    .await
    .ok()
    .flatten();
    let Some(playback_id) = playback_id else {
        return;
    };

    let still_live = match crate::tasks::fetch_active_streams(&state).await {
        Ok(active) => active.contains(&playback_id),
        Err(e) => {
            // Unknown is not "gone" — leave it to the poller.
            tracing::debug!("[webhook] OME unreachable after a close: {e}");
            return;
        }
    };
    if still_live {
        return;
    }

    let Ok(conn) = state.db.get() else { return };
    let slugs = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.slug FROM rooms r \
             JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE sk.key_token = ?1 AND r.status = 'live'",
        )?;
        let rooms: Vec<(String, String)> = stmt
            .query_map(params![key_token], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut slugs = Vec::new();
        for (room_id, slug) in &rooms {
            let changed = conn.execute(
                "UPDATE rooms SET status = 'pending' WHERE id = ?1 AND status = 'live'",
                params![room_id],
            )?;
            if changed > 0 {
                slugs.push(slug.clone());
            }
        }
        Ok::<_, rusqlite::Error>(slugs)
    })
    .await;

    if let Ok(Ok(slugs)) = slugs {
        for slug in slugs {
            let _ = state.events.room_pending.send(slug);
        }
    }
}

/// Admit (or refuse) a WebRTC / LL-HLS playback session; see [`crate::playback`].
async fn playback_session(
    state: &Arc<AppState>,
    url: &str,
    status: &str,
) -> Result<Json<Value>, AppError> {
    if status == "closing" {
        return Ok(Json(json!({ "allowed": true })));
    }
    let Some((url, playback_id)) = parse_ome_url(url) else {
        return Ok(Json(json!({ "allowed": false })));
    };
    let query = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };

    let allowed = if let Some(apt) = query("apt") {
        let now_secs = crate::time::now_ms() / 1000;
        playback::verify_admin_token(&state.config.jwt_secret, &apt, now_secs)
    } else if let (Some(participant_id), Some(token)) = (query("participantId"), query("token")) {
        let conn = state.db.get()?;
        tokio::task::spawn_blocking(move || {
            playback::participant_may_play(&conn, &participant_id, &token, &playback_id)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??
    } else {
        false
    };

    if !allowed {
        tracing::info!(
            action = "playback_denied",
            "browser playback session refused"
        );
    }
    Ok(Json(json!({ "allowed": allowed })))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", post(webhook_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_name_is_the_segment_after_the_app() {
        for (raw, stream) in [
            ("rtmp://localhost:11935/live/k1", "k1"),
            ("srt://default/live/k2", "k2"),
            ("http://localhost:18443/live/k3?direction=whip", "k3"),
            (
                "http://localhost:18443/live/pb/llhls.m3u8?participantId=p&token=t",
                "pb",
            ),
            ("ws://localhost:3333/live/pb?participantId=p&token=t", "pb"),
            ("/live/bare", "bare"),
        ] {
            assert_eq!(
                parse_ome_url(raw).map(|(_, s)| s).as_deref(),
                Some(stream),
                "{raw}"
            );
        }
        assert!(parse_ome_url("rtmp://host/live").is_none());
        assert!(parse_ome_url("rtmp://host/").is_none());
    }

    #[test]
    fn renaming_keeps_everything_but_the_stream_name() {
        // The shapes OME v0.21.0 was measured accepting, per protocol.
        for (raw, renamed) in [
            (
                "rtmp://localhost:11935/live/k1",
                "rtmp://localhost:11935/live/pb",
            ),
            ("srt://default/live/k2", "srt://default/live/pb"),
            (
                "http://localhost:18443/live/k3?direction=whip",
                "http://localhost:18443/live/pb?direction=whip",
            ),
        ] {
            let (url, _) = parse_ome_url(raw).unwrap();
            assert_eq!(
                with_stream_name(&url, "pb").as_deref(),
                Some(renamed),
                "{raw}"
            );
        }
    }
}
