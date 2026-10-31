use axum::{
    extract::{Path, State},
    routing::{delete, get},
    Json, Router,
};
use base64::Engine;
use serde_json::Value;
use std::sync::Arc;

use crate::auth::AdminAuth;
use crate::error::AppError;
use crate::state::AppState;

async fn ome_request(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
) -> Result<Value, AppError> {
    let url = format!("{}{}", base_url, path);
    let basic = base64::engine::general_purpose::STANDARD.encode(token);

    let resp = client
        .request(method, &url)
        .header("Authorization", format!("Basic {}", basic))
        .send()
        .await
        .map_err(|e| AppError::BadGateway(format!("OME request failed: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::BadGateway(format!(
            "OME returned {}: {}",
            status, body
        )));
    }

    let json = resp
        .json::<Value>()
        .await
        .map_err(|e| AppError::BadGateway(format!("OME invalid JSON: {}", e)))?;

    Ok(json)
}

async fn get_status(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let ome_data = ome_request(
        &state.http_client,
        &state.config.ome_api_url,
        &state.config.ome_api_token,
        reqwest::Method::GET,
        "/vhosts/default/apps/live/streams",
    )
    .await?;

    // OME response is an array of stream name strings
    let names: Vec<String> = ome_data
        .pointer("/response")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Fetch per-stream detail from OME for each stream
    let client = &state.http_client;
    let ome_url = &state.config.ome_api_url;
    let ome_token = &state.config.ome_api_token;
    let mut stream_details: Vec<(String, Option<Value>)> = Vec::new();
    for name in &names {
        let detail_path = format!("/vhosts/default/apps/live/streams/{}", name);
        let detail = match ome_request(
            client,
            ome_url,
            ome_token,
            reqwest::Method::GET,
            &detail_path,
        )
        .await
        {
            Ok(d) => d.get("response").cloned(),
            Err(_) => None,
        };
        stream_details.push((name.clone(), detail));
    }

    // Enrich with DB data
    let conn = state.db.get()?;
    let enriched = tokio::task::spawn_blocking(move || {
        let mut results = Vec::new();
        for (stream_name, detail) in &stream_details {
            let mut stmt = conn.prepare(
                "SELECT sk.name as key_name, r.name as room_name, r.id as room_id, r.slug \
                 FROM stream_keys sk \
                 LEFT JOIN rooms r ON r.stream_key_id = sk.id \
                 WHERE sk.playback_id = ?1",
            )?;
            let row = stmt
                .query_row(rusqlite::params![stream_name], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .ok();
            let (key_name, room_name, room_id, slug) = match row {
                Some((kn, rn, ri, s)) => (kn, rn, ri, s),
                None => (None, None, None, None),
            };
            results.push(serde_json::json!({
                "name": stream_name,
                "key_name": key_name,
                "room_name": room_name,
                "room_id": room_id,
                "slug": slug,
                "detail": detail,
            }));
        }
        Ok::<_, rusqlite::Error>(results)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(serde_json::json!({ "streams": enriched })))
}

/// OME stream names are our own playback ids (24 random bytes, hex), so a real
/// one is always ASCII alphanumeric.
///
/// The value is a path segment that gets spliced into the upstream OME API URL,
/// and axum has already percent-decoded it — so without this check a crafted
/// name containing `/`, `..` or `?` would address a different OME endpoint
/// entirely. Admin-only, but the handler shouldn't be the thing standing
/// between an admin typo and an arbitrary OME API call.
fn valid_stream_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn delete_stream(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(stream_name): Path<String>,
) -> Result<Json<Value>, AppError> {
    if !valid_stream_name(&stream_name) {
        return Err(AppError::BadRequest("Invalid stream name".into()));
    }

    // Block the key BEFORE disconnecting so the encoder can't win the race by
    // reconnecting between the OME DELETE and the block landing. OME publishes
    // under the key's playback id, so that is what the stream name matches.
    {
        let conn = state.db.get()?;
        let name = stream_name.clone();
        tokio::task::spawn_blocking(move || {
            conn.execute(
                "UPDATE stream_keys SET blocked = 1 WHERE playback_id = ?1",
                rusqlite::params![name],
            )
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    }

    let path = format!("/vhosts/default/apps/live/streams/{}", stream_name);
    let data = ome_request(
        &state.http_client,
        &state.config.ome_api_url,
        &state.config.ome_api_token,
        reqwest::Method::DELETE,
        &path,
    )
    .await?;

    Ok(Json(data))
}

/// A short-lived token that lets this admin's browser play any stream — the
/// dashboard preview and the Stream Keys tab's playback URLs (`?apt=`). OME
/// asks the admission webhook before every browser playback session; see
/// [`crate::playback`].
async fn playback_token(_auth: AdminAuth, State(state): State<Arc<AppState>>) -> Json<Value> {
    let now_secs = crate::time::now_ms() / 1000;
    Json(serde_json::json!({
        "token": crate::playback::mint_admin_token(&state.config.jwt_secret, now_secs),
        "ttlSeconds": crate::playback::ADMIN_TOKEN_TTL_SECS,
    }))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(get_status))
        .route("/playback-token", get(playback_token))
        .route("/streams/{streamName}", delete(delete_stream))
}
