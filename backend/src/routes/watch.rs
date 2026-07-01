//! Farbplay ⇄ Farbstrom room-link SRT integration (see GitHub #165). The
//! waiting-room/kick admission gate is documented inline below.
//!
//! Native SRT viewers (Farbplay) connect from a shared room link
//! `https://<host>/watch/<slug>` instead of a raw `srt://` URL. The flow mirrors
//! the browser viewer: the app first `join`s the room
//! (`POST /api/public/rooms/:slug/join`) to become a `participants` row, waits
//! on the admission SSE if the room has a waiting room, then `GET`s
//! `/api/watch/<slug>?participantId=&token=` for a short-lived, HMAC-signed
//! `streamid` (OME SignedPolicy). It re-fetches on every (re)connect, so a ~30 s
//! TTL is plenty — the token only needs to survive the SRT handshake.
//!
//! This endpoint is **admission-gated**: the signed streamid is minted only for
//! an admitted, non-kicked participant. Missing/invalid credentials → 403, and a
//! kicked/not-admitted participant → 403 — so a kicked viewer cannot reconnect
//! (the reconnect backstop behind the SSE self-disconnect; contract O1/O2).
//! Password is enforced at `join`, not here. Direct `srt://…` URLs (power users)
//! bypass rooms entirely and are unaffected.
//!
//! Security caveat: in Farbstrom the OME stream name *is* the ingest stream key
//! (`OutputStreamName=${OriginStreamName}`), so the signed streamid necessarily
//! contains `default/live/<key_token>` in plaintext. SignedPolicy here provides
//! expiry / replay-limiting, **not** path secrecy — the key is already handed to
//! web viewers on join. See #165 and the credential-separation follow-up.
//!
//! When `SRT_PLAYBACK_PASSPHRASE` is configured, this endpoint also returns the
//! passphrase + pbkeylen so Farbplay can AES-encrypt the SRT playback leg. That
//! protects the *media payload* from a passive network eavesdropper; the
//! streamid (with the key) is SRT handshake metadata and is not covered.

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha1::Sha1;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppError;
use crate::state::AppState;

type HmacSha1 = Hmac<Sha1>;

/// Token lifetime. The app re-fetches on every (re)connect, so this only needs
/// to outlast the SRT handshake.
const TTL_SECONDS: u64 = 30;

#[derive(Deserialize)]
struct WatchQuery {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
}

/// Mint an OME SignedPolicy streamid for SRT playback.
///
/// The client transmits only the path form `default/live/<stream>?policy=…&signature=…`
/// as the SRT `streamid`, but OME reconstructs the request URL as
/// `srt://default/live/<stream>?policy=…` (scheme + vhost as host) and signs
/// **that** — so the HMAC must be computed over the `srt://`-prefixed URL, not
/// the bare path. OME then validates the HMAC + `url_expire` on connect.
/// (Verified against OME v0.20.5 by matching its logged `expected` signature.)
fn sign_streamid(secret: &str, stream_name: &str, expire_ms: u128) -> Result<String, AppError> {
    let path = format!("default/live/{}", stream_name);
    let policy_json = json!({ "url_expire": expire_ms }).to_string();
    let policy = URL_SAFE_NO_PAD.encode(policy_json);

    // String OME signs (includes the srt:// scheme).
    let signed_url = format!("srt://{}?policy={}", path, policy);
    let mut mac = HmacSha1::new_from_slice(secret.as_bytes())
        .map_err(|e| AppError::Internal(format!("HMAC init error: {}", e)))?;
    mac.update(signed_url.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    // Streamid the client actually sends (path form, no scheme).
    Ok(format!(
        "{}?policy={}&signature={}",
        path, policy, signature
    ))
}

/// GET /:slug?participantId=&token= — admission-gated SRT connection details.
///
/// Requires `participantId` + `token` from a prior `join` (contract O1). Mints
/// the signed streamid only for an admitted, non-kicked participant:
/// - missing `participantId`/`token` → 403
/// - no matching participant (wrong token, wrong slug) → 404
/// - room ended/expired → 404 (folded into the SQL filter)
/// - kicked → 403; not yet admitted → 403
/// - room has no stream key → 404
async fn watch(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Query(query): Query<WatchQuery>,
) -> Result<Json<Value>, AppError> {
    // Join is always required for the room-link flow (contract O1). Password is
    // checked at join, not here.
    let participant_id = query
        .participant_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Forbidden("Participant credentials required".into()))?;
    let token = query
        .token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Forbidden("Participant credentials required".into()))?;

    let conn = state.db.get()?;
    let slug_clone = slug.clone();
    // Look up the participant by (id, token) within this room (same pattern as
    // rooms_public::participant_status), joined to the room's stream key. Ended/
    // expired rooms are filtered in SQL so they collapse to "no row" → 404, the
    // same as an unknown room or a bad token. `expires_at` is a UTC
    // "YYYY-MM-DD HH:MM:SS" string (see rooms::normalize_datetime), so it
    // compares directly against CURRENT_TIMESTAMP.
    let (is_admitted, is_kicked, room_name, key_token) = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT p.is_admitted, p.is_kicked, r.name, sk.key_token \
             FROM participants p \
             JOIN rooms r ON r.id = p.room_id \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE p.id = ?1 AND p.token = ?2 AND r.slug = ?3 \
               AND r.status != 'ended' \
               AND (r.expires_at IS NULL OR r.expires_at > CURRENT_TIMESTAMP)",
        )?;
        stmt.query_row(
            rusqlite::params![participant_id, token, slug_clone],
            |row| {
                Ok((
                    row.get::<_, i32>(0)?,            // is_admitted
                    row.get::<_, i32>(1)?,            // is_kicked
                    row.get::<_, String>(2)?,         // room name
                    row.get::<_, Option<String>>(3)?, // stream key_token
                ))
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
            _ => AppError::Internal(e.to_string()),
        })
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // Kicked viewers cannot reconnect (the reconnect backstop behind the SSE
    // self-disconnect; contract O2).
    if is_kicked == 1 {
        return Err(AppError::Forbidden(
            "You have been kicked from this room".into(),
        ));
    }
    // Guard: a not-yet-admitted participant waits on the SSE, not here.
    if is_admitted == 0 {
        return Err(AppError::Forbidden("Not yet admitted".into()));
    }

    // 404 if no stream key is assigned — there is nothing to watch.
    let stream_name = key_token.ok_or_else(|| AppError::NotFound("Room not found".into()))?;

    let expire_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        + (TTL_SECONDS as u128 * 1000);
    let streamid = sign_streamid(
        &state.config.ome_signed_policy_secret,
        &stream_name,
        expire_ms,
    )?;

    // SRT playback details. When playback encryption is configured, hand the
    // passphrase + pbkeylen to Farbplay so it can decrypt (issue: SRT encryption).
    let mut srt = json!({
        "host": state.config.srt_public_host,
        "port": state.config.srt_public_port,
        "streamid": streamid,
        "latency": state.config.srt_latency_ms,
    });
    if let Some(ref passphrase) = state.config.srt_playback_passphrase {
        srt["passphrase"] = json!(passphrase);
        srt["pbkeylen"] = json!(state.config.srt_pbkeylen);
    }

    Ok(Json(json!({
        "srt": srt,
        "ttlSeconds": TTL_SECONDS,
        "title": room_name,
    })))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/{slug}", get(watch))
}
