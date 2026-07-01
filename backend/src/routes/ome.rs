use axum::{
    extract::{Path, State},
    routing::get,
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

    let conf_names: Vec<&String> = names.iter().filter(|n| n.starts_with("conf-")).collect();
    let main_names: Vec<String> = names
        .iter()
        .filter(|n| !n.starts_with("conf-"))
        .cloned()
        .collect();
    let conf_count = conf_names.len();

    // Fetch per-stream detail from OME for each main stream
    let client = &state.http_client;
    let ome_url = &state.config.ome_api_url;
    let ome_token = &state.config.ome_api_token;
    let mut stream_details: Vec<(String, Option<Value>)> = Vec::new();
    for name in &main_names {
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
                 WHERE sk.key_token = ?1",
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

    Ok(Json(
        serde_json::json!({ "streams": enriched, "conf_count": conf_count }),
    ))
}

async fn list_streams(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let data = ome_request(
        &state.http_client,
        &state.config.ome_api_url,
        &state.config.ome_api_token,
        reqwest::Method::GET,
        "/vhosts/default/apps/live/streams",
    )
    .await?;

    Ok(Json(data))
}

async fn get_stream(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(stream_key): Path<String>,
) -> Result<Json<Value>, AppError> {
    let path = format!("/vhosts/default/apps/live/streams/{}", stream_key);
    let data = ome_request(
        &state.http_client,
        &state.config.ome_api_url,
        &state.config.ome_api_token,
        reqwest::Method::GET,
        &path,
    )
    .await?;

    Ok(Json(data))
}

async fn delete_stream(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(stream_key): Path<String>,
) -> Result<Json<Value>, AppError> {
    // Block the key BEFORE disconnecting so the encoder can't win the race by
    // reconnecting between the OME DELETE and the block landing. The OME stream
    // name is the ingest key token, so it matches key_token directly. Conference
    // streams (conf-*) have no key row — the UPDATE is a harmless no-op.
    {
        let conn = state.db.get()?;
        let key = stream_key.clone();
        tokio::task::spawn_blocking(move || {
            conn.execute(
                "UPDATE stream_keys SET blocked = 1 WHERE key_token = ?1",
                rusqlite::params![key],
            )
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    }

    let path = format!("/vhosts/default/apps/live/streams/{}", stream_key);
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

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(get_status))
        .route("/streams", get(list_streams))
        .route(
            "/streams/{streamKey}",
            get(get_stream).delete(delete_stream),
        )
}
