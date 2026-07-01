use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use base64::Engine;
use rand::RngExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::auth::AdminAuth;
use crate::error::AppError;
use crate::state::AppState;

fn row_to_json(row: &rusqlite::Row, columns: &[&str]) -> rusqlite::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for (i, col) in columns.iter().enumerate() {
        let val: rusqlite::types::Value = row.get(i)?;
        map.insert(
            col.to_string(),
            match val {
                rusqlite::types::Value::Null => Value::Null,
                rusqlite::types::Value::Integer(n) => json!(n),
                rusqlite::types::Value::Real(f) => json!(f),
                rusqlite::types::Value::Text(s) => json!(s),
                rusqlite::types::Value::Blob(b) => {
                    json!(base64::engine::general_purpose::STANDARD.encode(b))
                }
            },
        );
    }
    Ok(Value::Object(map))
}

async fn list_keys(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, AppError> {
    let conn = state.db.get()?;
    let keys = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT sk.id, sk.name, sk.key_token, sk.blocked, sk.created_at, \
             GROUP_CONCAT(r.name, ', ') as room_names \
             FROM stream_keys sk \
             LEFT JOIN rooms r ON r.stream_key_id = sk.id \
             GROUP BY sk.id \
             ORDER BY sk.created_at DESC",
        )?;
        let cols = &[
            "id",
            "name",
            "key_token",
            "blocked",
            "created_at",
            "room_names",
        ];
        let rows = stmt
            .query_map([], |row| row_to_json(row, cols))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(keys))
}

#[derive(Deserialize)]
struct CreateKeyBody {
    name: Option<String>,
}

async fn create_key(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateKeyBody>,
) -> Result<Json<Value>, AppError> {
    let name = body
        .name
        .ok_or_else(|| AppError::BadRequest("Name is required".into()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let key_bytes: [u8; 24] = rand::rng().random();
    let key_token: String = key_bytes.iter().map(|b| format!("{:02x}", b)).collect();

    let conn = state.db.get()?;
    let row = {
        let id = id.clone();
        let name = name.clone();
        let key_token = key_token.clone();
        tokio::task::spawn_blocking(move || {
            conn.execute(
                "INSERT INTO stream_keys (id, name, key_token) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, name, key_token],
            )?;
            let mut stmt = conn
                .prepare("SELECT id, name, key_token, created_at FROM stream_keys WHERE id = ?1")?;
            let cols = &["id", "name", "key_token", "created_at"];
            let row = stmt.query_row(rusqlite::params![id], |row| row_to_json(row, cols))?;
            Ok::<_, rusqlite::Error>(row)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    }?;

    Ok(Json(row))
}

#[derive(Deserialize)]
struct UpdateKeyBody {
    name: Option<String>,
}

async fn update_key(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateKeyBody>,
) -> Result<Json<Value>, AppError> {
    let name = body
        .name
        .ok_or_else(|| AppError::BadRequest("Name is required".into()))?;

    let conn = state.db.get()?;
    let row = tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE stream_keys SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Stream key not found".into()));
        }
        let mut stmt =
            conn.prepare("SELECT id, name, key_token, created_at FROM stream_keys WHERE id = ?1")?;
        let cols = &["id", "name", "key_token", "created_at"];
        let row = stmt.query_row(rusqlite::params![id], |row| row_to_json(row, cols))?;
        Ok(row)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(row))
}

// POST /:id/unblock — clear a stream kick so the encoder may ingest again.
async fn unblock_key(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE stream_keys SET blocked = 0 WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Stream key not found".into()));
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({ "ok": true })))
}

async fn delete_key(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "DELETE FROM stream_keys WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Stream key not found".into()));
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({ "ok": true })))
}

// GET /srt-config — SRT encryption passphrases for the admin's raw SRT URLs.
// Admin-only; nulls when a leg is unencrypted. Static path, so it takes
// precedence over the `/{id}` capture.
async fn srt_config(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({
        "ingestPassphrase": state.config.srt_ingest_passphrase,
        "playbackPassphrase": state.config.srt_playback_passphrase,
        "pbkeylen": state.config.srt_pbkeylen,
    })))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_keys).post(create_key))
        .route("/srt-config", get(srt_config))
        .route("/{id}", put(update_key).delete(delete_key))
        .route("/{id}/unblock", post(unblock_key))
}
