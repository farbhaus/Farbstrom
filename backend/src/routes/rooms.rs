use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use rand::RngExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::auth::AdminAuth;
use crate::error::AppError;
use crate::events::{HostRevokedEvent, ModerationChangedEvent};
use crate::livekit::LiveKitClient;
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

fn generate_slug(name: &str) -> String {
    let slug_base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug_base = slug_base
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug_base = &slug_base[..slug_base.len().min(80)];
    let random_suffix: [u8; 3] = rand::rng().random();
    let hex: String = random_suffix.iter().map(|b| format!("{:02x}", b)).collect();
    format!("{}-{}", slug_base, hex)
}

fn generate_presenter_key() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// GET / - list all rooms
async fn list_rooms(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, AppError> {
    let conn = state.db.get()?;
    let rooms = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
             r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
             r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
             (SELECT COUNT(*) FROM participants p \
              WHERE p.room_id = r.id AND p.is_admitted = 0 AND p.is_kicked = 0) as waiting_count, \
             sk.key_token, sk.name as stream_key_name \
             FROM rooms r \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             ORDER BY r.created_at DESC",
        )?;
        let cols = &[
            "id",
            "name",
            "slug",
            "delivery_mode",
            "waiting_room",
            "noise_reduction",
            "echo_cancellation",
            "push_to_talk",
            "starts_at",
            "expires_at",
            "status",
            "stream_key_id",
            "created_at",
            "started_at",
            "ended_at",
            "presenter_key",
            "password_hash",
            "waiting_count",
            "key_token",
            "stream_key_name",
        ];
        let rows = stmt
            .query_map([], |row| row_to_json(row, cols))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(rooms))
}

// GET /:id - single room
async fn get_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let room = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
             r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
             r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
             sk.key_token, sk.name as stream_key_name \
             FROM rooms r \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.id = ?1",
        )?;
        let cols = &[
            "id",
            "name",
            "slug",
            "delivery_mode",
            "waiting_room",
            "noise_reduction",
            "echo_cancellation",
            "push_to_talk",
            "starts_at",
            "expires_at",
            "status",
            "stream_key_id",
            "created_at",
            "started_at",
            "ended_at",
            "presenter_key",
            "password_hash",
            "key_token",
            "stream_key_name",
        ];
        let row = stmt
            .query_row(rusqlite::params![id], |row| row_to_json(row, cols))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok::<_, AppError>(row)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(room))
}

#[derive(Deserialize)]
struct CreateRoomBody {
    name: Option<String>,
    password: Option<String>,
    delivery_mode: Option<String>,
    waiting_room: Option<bool>,
    noise_reduction: Option<bool>,
    echo_cancellation: Option<bool>,
    push_to_talk: Option<bool>,
    starts_at: Option<String>,
    expires_at: Option<String>,
    stream_key_id: Option<String>,
}

// POST / - create room
async fn create_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRoomBody>,
) -> Result<Json<Value>, AppError> {
    let name = body
        .name
        .ok_or_else(|| AppError::BadRequest("Name is required".into()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let slug = generate_slug(&name);
    let presenter_key = generate_presenter_key();
    let delivery_mode = body.delivery_mode.unwrap_or_else(|| "webrtc".into());
    let waiting_room: i32 = if body.waiting_room.unwrap_or(false) {
        1
    } else {
        0
    };
    // Participant-audio defaults are ON unless the admin unticks them.
    let noise_reduction: i32 = i32::from(body.noise_reduction.unwrap_or(true));
    let echo_cancellation: i32 = i32::from(body.echo_cancellation.unwrap_or(true));
    // Push-to-talk defaults OFF — preserve the always-toggle mic behavior unless
    // the admin opts the room into it (issue #188).
    let push_to_talk: i32 = i32::from(body.push_to_talk.unwrap_or(false));
    let starts_at = body.starts_at.map(|s| normalize_datetime(&s));
    let expires_at = body.expires_at.map(|s| normalize_datetime(&s));
    let stream_key_id = body.stream_key_id;

    let password_hash = match body.password {
        Some(ref pw) if !pw.is_empty() => {
            let pw = pw.clone();
            Some(
                tokio::task::spawn_blocking(move || bcrypt::hash(pw, 10))
                    .await
                    .map_err(|e| AppError::Internal(e.to_string()))??,
            )
        }
        _ => None,
    };

    let conn = state.db.get()?;
    let room = {
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            // status: a future starts_at schedules the room (issue #200) so
            // joiners are held until the start poller admits them. ?11 is
            // starts_at, reused in the CASE so the DB clock is the single
            // source of truth for the future/past comparison.
            conn.execute(
                "INSERT INTO rooms (id, name, slug, password_hash, presenter_key, \
                 delivery_mode, waiting_room, noise_reduction, echo_cancellation, push_to_talk, \
                 starts_at, expires_at, stream_key_id, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                 CASE WHEN ?11 IS NOT NULL AND ?11 > CURRENT_TIMESTAMP THEN 'scheduled' ELSE 'pending' END)",
                rusqlite::params![
                    id,
                    name,
                    slug,
                    password_hash,
                    presenter_key,
                    delivery_mode,
                    waiting_room,
                    noise_reduction,
                    echo_cancellation,
                    push_to_talk,
                    starts_at,
                    expires_at,
                    stream_key_id,
                ],
            )?;
            let mut stmt = conn.prepare(
                "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
                 r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
                 r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
                 sk.key_token, sk.name as stream_key_name \
                 FROM rooms r \
                 LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
                 WHERE r.id = ?1",
            )?;
            let cols = &[
                "id",
                "name",
                "slug",
                "delivery_mode",
                "waiting_room",
                "noise_reduction",
                "echo_cancellation",
                "push_to_talk",
                "starts_at",
            "expires_at",
                "status",
                "stream_key_id",
                "created_at",
                "started_at",
                "ended_at",
                "presenter_key",
                "password_hash",
                "key_token",
                "stream_key_name",
            ];
            let row = stmt.query_row(rusqlite::params![id], |row| row_to_json(row, cols))?;
            Ok::<_, rusqlite::Error>(row)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    }?;

    Ok(Json(room))
}

// PUT /:id - partial update
async fn update_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    // Check room exists first
    let conn = state.db.get()?;
    let id_check = id.clone();
    let exists = tokio::task::spawn_blocking(move || {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rooms WHERE id = ?1",
            rusqlite::params![id_check],
            |row| row.get(0),
        )?;
        Ok::<_, rusqlite::Error>(count > 0)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    if !exists {
        return Err(AppError::NotFound("Room not found".into()));
    }

    // Capture the current stream_key_id + slug so we can emit the right
    // WS event (stream:assigned / stream:removed) after the UPDATE commits.
    // Only queried when the request actually touches stream_key_id.
    let wants_stream_key_change = body.get("stream_key_id").is_some();
    let (old_sk_id, slug_for_event): (Option<String>, String) = if wants_stream_key_change {
        let conn = state.db.get()?;
        let id_q = id.clone();
        tokio::task::spawn_blocking(move || {
            conn.query_row(
                "SELECT stream_key_id, slug FROM rooms WHERE id = ?1",
                rusqlite::params![id_q],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
            )
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?
    } else {
        (None, String::new())
    };

    // Build dynamic SET clauses
    let mut set_clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql + Send>> = Vec::new();
    // Detaching the key from a live room must also clear its live status (set
    // below, after the param-bearing clauses, so it doesn't disturb `?N`).
    let mut reset_live_on_detach = false;

    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        set_clauses.push(format!("name = ?{}", set_clauses.len() + 1));
        params.push(Box::new(name.to_string()));
    }

    if let Some(dm) = body.get("delivery_mode").and_then(|v| v.as_str()) {
        set_clauses.push(format!("delivery_mode = ?{}", set_clauses.len() + 1));
        params.push(Box::new(dm.to_string()));
    }

    if let Some(wr) = body.get("waiting_room") {
        let val: i32 = if wr.as_bool().unwrap_or(false) { 1 } else { 0 };
        set_clauses.push(format!("waiting_room = ?{}", set_clauses.len() + 1));
        params.push(Box::new(val));
    }

    for (key, col, default_on) in [
        ("noise_reduction", "noise_reduction", true),
        ("echo_cancellation", "echo_cancellation", true),
        ("push_to_talk", "push_to_talk", false),
    ] {
        if let Some(v) = body.get(key) {
            let val: i32 = i32::from(v.as_bool().unwrap_or(default_on));
            set_clauses.push(format!("{col} = ?{}", set_clauses.len() + 1));
            params.push(Box::new(val));
        }
    }

    // starts_at: null clears, string sets, absent keeps (issue #200).
    if body.get("starts_at").is_some() {
        let val = body
            .get("starts_at")
            .and_then(|v| v.as_str())
            .map(normalize_datetime);
        set_clauses.push(format!("starts_at = ?{}", set_clauses.len() + 1));
        params.push(Box::new(val));
    }

    // expires_at: null clears, string sets, absent keeps
    if body.get("expires_at").is_some() {
        let val = body
            .get("expires_at")
            .and_then(|v| v.as_str())
            .map(normalize_datetime);
        set_clauses.push(format!("expires_at = ?{}", set_clauses.len() + 1));
        params.push(Box::new(val));
    }

    // stream_key_id: null clears, string sets, absent keeps
    if body.get("stream_key_id").is_some() {
        let val = body
            .get("stream_key_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        reset_live_on_detach = val.is_none();
        set_clauses.push(format!("stream_key_id = ?{}", set_clauses.len() + 1));
        params.push(Box::new(val));
    }

    // password: "" clears, value re-hashes, absent keeps
    if let Some(pw_val) = body.get("password") {
        let pw_str = pw_val.as_str().unwrap_or("");
        if pw_str.is_empty() {
            set_clauses.push(format!("password_hash = ?{}", set_clauses.len() + 1));
            params.push(Box::new(None::<String>));
        } else {
            let pw = pw_str.to_string();
            let hashed = tokio::task::spawn_blocking(move || bcrypt::hash(pw, 10))
                .await
                .map_err(|e| AppError::Internal(e.to_string()))??;
            set_clauses.push(format!("password_hash = ?{}", set_clauses.len() + 1));
            params.push(Box::new(Some(hashed)));
        }
    }

    // Status recompute — one clause covering two concerns, because saveRoom
    // sends both starts_at and stream_key_id on every save and SQLite forbids
    // assigning `status` twice:
    //   (a) issue #200: a future starts_at schedules the room; clearing it (or
    //       a past value) releases a 'scheduled' room back to 'pending'.
    //   (b) detaching the key from a live room must flip it to 'pending' — the
    //       OME poller only demotes rooms that still JOIN a stream key, so it
    //       would otherwise stay stuck 'live'.
    // References the OLD `status` column (SET expressions see pre-update row
    // values) and the NEW starts_at via a fresh bind param. Pushed last, after
    // every other param-bearing clause; the final id index is params.len()+1.
    let sets_starts = body.get("starts_at").is_some();
    if sets_starts || reset_live_on_detach {
        let mut case = String::from("status = CASE WHEN status = 'ended' THEN 'ended' ");
        if sets_starts {
            let val = body
                .get("starts_at")
                .and_then(|v| v.as_str())
                .map(normalize_datetime);
            let idx = params.len() + 1;
            case.push_str(&format!(
                "WHEN ?{idx} IS NOT NULL AND ?{idx} > CURRENT_TIMESTAMP THEN 'scheduled' \
                 WHEN status = 'scheduled' THEN 'pending' "
            ));
            params.push(Box::new(val));
        }
        if reset_live_on_detach {
            case.push_str("WHEN status = 'live' THEN 'pending' ");
        }
        case.push_str("ELSE status END");
        set_clauses.push(case);
    }

    if set_clauses.is_empty() {
        // Nothing to update, just return the room
        let conn = state.db.get()?;
        let room = tokio::task::spawn_blocking(move || {
            let mut stmt = conn.prepare(
                "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
                 r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
                 r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
                 sk.key_token, sk.name as stream_key_name \
                 FROM rooms r \
                 LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
                 WHERE r.id = ?1",
            )?;
            let cols = &[
                "id",
                "name",
                "slug",
                "delivery_mode",
                "waiting_room",
                "noise_reduction",
                "echo_cancellation",
                "push_to_talk",
                "starts_at",
            "expires_at",
                "status",
                "stream_key_id",
                "created_at",
                "started_at",
                "ended_at",
                "presenter_key",
                "password_hash",
                "key_token",
                "stream_key_name",
            ];
            let row = stmt.query_row(rusqlite::params![id], |row| row_to_json(row, cols))?;
            Ok::<_, rusqlite::Error>(row)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
        return Ok(Json(room));
    }

    // Add id as the last parameter. Index off params.len(), not
    // set_clauses.len(): the live-status CASE clause adds a set clause with no
    // bind param, so the two counts can differ.
    let id_param_idx = params.len() + 1;
    let sql = format!(
        "UPDATE rooms SET {} WHERE id = ?{}",
        set_clauses.join(", "),
        id_param_idx
    );

    let conn = state.db.get()?;
    let id_clone = id.clone();
    let room = tokio::task::spawn_blocking(move || {
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::types::ToSql)
            .collect();
        let mut all_params = param_refs;
        all_params.push(&id_clone as &dyn rusqlite::types::ToSql);
        conn.execute(&sql, all_params.as_slice())?;

        let mut stmt = conn.prepare(
            "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
             r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
             r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
             sk.key_token, sk.name as stream_key_name \
             FROM rooms r \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.id = ?1",
        )?;
        let cols = &[
            "id",
            "name",
            "slug",
            "delivery_mode",
            "waiting_room",
            "noise_reduction",
            "echo_cancellation",
            "push_to_talk",
            "starts_at",
            "expires_at",
            "status",
            "stream_key_id",
            "created_at",
            "started_at",
            "ended_at",
            "presenter_key",
            "password_hash",
            "key_token",
            "stream_key_name",
        ];
        let row = stmt.query_row(rusqlite::params![id_clone], |row| row_to_json(row, cols))?;
        Ok::<_, rusqlite::Error>(row)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // issue #200: releasing a scheduled room by clearing (or back-dating) its
    // start also releases the viewers held during the scheduled window — but
    // only when the waiting room is off. The start poller covers a back-dated
    // starts_at (still <= now); clearing it to NULL is caught only here.
    if sets_starts {
        let opened = room
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s != "scheduled" && s != "ended")
            .unwrap_or(false);
        let waiting_off = room.get("waiting_room").and_then(|v| v.as_i64()) == Some(0);
        if opened && waiting_off {
            let conn = state.db.get()?;
            let id_admit = id.clone();
            let admitted = tokio::task::spawn_blocking(move || {
                conn.execute(
                    "UPDATE participants SET is_admitted = 1 \
                     WHERE room_id = ?1 AND is_kicked = 0 AND is_admitted = 0",
                    rusqlite::params![id_admit],
                )
            })
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;
            if admitted > 0 {
                if let Some(slug) = room.get("slug").and_then(|v| v.as_str()) {
                    let _ = state.events.moderation_changed.send(
                        crate::events::ModerationChangedEvent {
                            slug: slug.to_string(),
                        },
                    );
                }
            }
        }
    }

    if wants_stream_key_change {
        let new_sk_id = body
            .get("stream_key_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        match (old_sk_id.is_some(), new_sk_id.is_some()) {
            (false, true) => {
                // Pull the fresh key_token off the row we just returned so
                // the WS event can ship it to clients (avoids a second query
                // and keeps the value consistent with what /join would give).
                let key_token = room
                    .get("key_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let _ =
                    state
                        .events
                        .stream_key_assigned
                        .send(crate::events::StreamKeyAssignedEvent {
                            slug: slug_for_event,
                            stream_key: key_token,
                        });
            }
            (true, false) => {
                // Mirror the OME poller's live -> pending transition for viewers
                // in the room (the DB status was reset above).
                let _ = state.events.room_pending.send(slug_for_event.clone());
                let _ = state.events.stream_key_removed.send(slug_for_event);
            }
            _ => {}
        }
    }

    // Delivery-mode change: tell viewers so they re-layout (browser broadcast
    // tile vs. call-grid only), mirroring the stream-key events above.
    if let Some(dm) = body.get("delivery_mode").and_then(|v| v.as_str()) {
        if let Some(slug) = room.get("slug").and_then(|v| v.as_str()) {
            let _ =
                state
                    .events
                    .delivery_mode_changed
                    .send(crate::events::DeliveryModeChangedEvent {
                        slug: slug.to_string(),
                        mode: dm.to_string(),
                    });
        }
    }

    Ok(Json(room))
}

// POST /:id/end - end a room
async fn end_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let slug = tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE rooms SET status = 'ended', ended_at = CURRENT_TIMESTAMP WHERE id = ?1 AND status != 'ended'",
            rusqlite::params![id],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Room not found or already ended".into()));
        }
        let slug: String = conn.query_row(
            "SELECT slug FROM rooms WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        Ok::<_, AppError>(slug)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let _ = state.events.room_ended.send(slug);

    Ok(Json(json!({ "ok": true })))
}

// POST /:id/reactivate - reactivate an ended room
async fn reactivate_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let room = tokio::task::spawn_blocking(move || {
        let status: String = conn
            .query_row(
                "SELECT status FROM rooms WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
                other => AppError::Internal(other.to_string()),
            })?;
        if status != "ended" {
            return Err(AppError::BadRequest(
                "Only ended rooms can be reactivated".into(),
            ));
        }
        conn.execute(
            "UPDATE rooms SET status = 'pending', ended_at = NULL, expires_at = NULL, starts_at = NULL \
             WHERE id = ?1",
            rusqlite::params![id],
        )?;
        let mut stmt = conn.prepare(
            "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
             r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
             r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
             sk.key_token, sk.name as stream_key_name \
             FROM rooms r \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.id = ?1",
        )?;
        let cols = &[
            "id",
            "name",
            "slug",
            "delivery_mode",
            "waiting_room",
            "noise_reduction",
            "echo_cancellation",
            "push_to_talk",
            "starts_at",
            "expires_at",
            "status",
            "stream_key_id",
            "created_at",
            "started_at",
            "ended_at",
            "presenter_key",
            "password_hash",
            "key_token",
            "stream_key_name",
        ];
        let row = stmt.query_row(rusqlite::params![id], |row| row_to_json(row, cols))?;
        Ok::<_, AppError>(row)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(room))
}

// DELETE /:id - delete a room
async fn delete_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let slug: String = {
        let conn = state.db.get()?;
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            conn.query_row(
                "SELECT slug FROM rooms WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
                _ => AppError::Internal(e.to_string()),
            })
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    }?;

    // Run file cleanup BEFORE deleting the room. Schema cascade on
    // session_files.room_id would otherwise wipe the rows before
    // cleanup_room_files (which keys off room_id) has a chance to find
    // them, leaking blobs on disk.
    if let Err(e) = crate::tasks::cleanup_room_files(&state, &slug).await {
        tracing::warn!(
            "[files] cleanup before room delete failed for {}: {}",
            slug,
            e
        );
    }

    {
        let conn = state.db.get()?;
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            conn.execute("DELETE FROM rooms WHERE id = ?1", rusqlite::params![id])
                .map_err(|e| AppError::Internal(e.to_string()))
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
    }

    let _ = state.events.room_ended.send(slug.clone());

    let livekit = LiveKitClient::new(&state.config, state.http_client.clone());
    let _ = livekit.delete_room(&slug).await;

    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct EnterRoomBody {
    name: Option<String>,
}

// POST /:id/enter - admin enters as presenter
async fn enter_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<EnterRoomBody>,
) -> Result<Json<Value>, AppError> {
    let name = body
        .name
        .map(|n| {
            let trimmed = n.trim().to_string();
            if trimmed.is_empty() {
                "Host".to_string()
            } else {
                trimmed.chars().take(50).collect()
            }
        })
        .unwrap_or_else(|| "Host".to_string());

    let conn = state.db.get()?;
    let id_clone = id.clone();
    let room_data = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT r.slug, r.delivery_mode, sk.key_token \
             FROM rooms r \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.id = ?1",
        )?;
        let result = stmt
            .query_row(rusqlite::params![id_clone], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok::<_, AppError>(result)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let (slug, delivery_mode, stream_key) = room_data;

    let participant_id = uuid::Uuid::new_v4().to_string();
    let token_bytes: [u8; 24] = rand::rng().random();
    let token: String = token_bytes.iter().map(|b| format!("{:02x}", b)).collect();

    let conn = state.db.get()?;
    let pid = participant_id.clone();
    let rid = id.clone();
    let tok = token.clone();
    let p_name = name.clone();
    tokio::task::spawn_blocking(move || {
        conn.execute(
            "INSERT INTO participants (id, room_id, name, role, is_admitted, token) \
             VALUES (?1, ?2, ?3, 'presenter', 1, ?4)",
            rusqlite::params![pid, rid, p_name, tok],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({
        "participantId": participant_id,
        "token": token,
        "slug": slug,
        "deliveryMode": delivery_mode,
        "streamKey": stream_key,
        "role": "presenter",
    })))
}

// POST /:id/rotate-presenter-key - generate a fresh host link key
async fn rotate_presenter_key(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let id_clone = id.clone();
    let key_clone = generate_presenter_key();
    let updated = tokio::task::spawn_blocking(move || {
        conn.execute(
            "UPDATE rooms SET presenter_key = ?1 WHERE id = ?2",
            rusqlite::params![key_clone, id_clone],
        )
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .map_err(|e| AppError::Internal(e.to_string()))?;

    if updated == 0 {
        return Err(AppError::NotFound("Room not found".into()));
    }

    // Look up the room slug + current presenter participants. The old host
    // link must stop granting host privileges immediately — delete those
    // participant rows (invalidates their session token) and push a WS
    // event that force-rejoins them through the public join API, which now
    // sees the stale pk and downgrades them to viewer.
    let conn = state.db.get()?;
    let id_clone = id.clone();
    let (slug, presenter_ids): (String, Vec<String>) = tokio::task::spawn_blocking(move || {
        let slug: String =
            conn.query_row("SELECT slug FROM rooms WHERE id = ?1", [&id_clone], |row| {
                row.get(0)
            })?;
        let mut stmt = conn.prepare(
            "SELECT id FROM participants WHERE room_id = ?1 AND role = 'presenter' AND is_kicked = 0",
        )?;
        let ids: Vec<String> = stmt
            .query_map([&id_clone], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        conn.execute(
            "DELETE FROM participants WHERE room_id = ?1 AND role = 'presenter' AND is_kicked = 0",
            [&id_clone],
        )?;
        Ok::<_, rusqlite::Error>((slug, ids))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .map_err(|e| AppError::Internal(e.to_string()))?;

    for pid in &presenter_ids {
        let _ = state.events.host_revoked.send(HostRevokedEvent {
            slug: slug.clone(),
            participant_id: pid.clone(),
        });
    }

    // Return the full room row so the admin UI can refresh the card.
    let conn = state.db.get()?;
    let room = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
             r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
             r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
             sk.key_token, sk.name as stream_key_name \
             FROM rooms r \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.id = ?1",
        )?;
        let cols = &[
            "id",
            "name",
            "slug",
            "delivery_mode",
            "waiting_room",
            "noise_reduction",
            "echo_cancellation",
            "push_to_talk",
            "starts_at",
            "expires_at",
            "status",
            "stream_key_id",
            "created_at",
            "started_at",
            "ended_at",
            "presenter_key",
            "password_hash",
            "key_token",
            "stream_key_name",
        ];
        let row = stmt
            .query_row(rusqlite::params![id], |row| row_to_json(row, cols))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok::<_, AppError>(row)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(room))
}

// GET /:id/waiting - non-admitted participants
async fn get_waiting(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, AppError> {
    let conn = state.db.get()?;
    let participants = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT id, name, role, joined_at FROM participants \
             WHERE room_id = ?1 AND is_admitted = 0 AND is_kicked = 0 \
             ORDER BY joined_at ASC",
        )?;
        let cols = &["id", "name", "role", "joined_at"];
        let rows = stmt
            .query_map(rusqlite::params![id], |row| row_to_json(row, cols))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(participants))
}

// GET /:id/roster - currently-connected admitted participants (issue #201).
// Admitted, non-kicked participant rows persist after disconnect, so the DB
// list is scoped to the live presence set (browser/presenter WS connections
// ∪ native SRT SSE presence) — otherwise every past viewer would show.
async fn get_roster(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, AppError> {
    let conn = state.db.get()?;
    let id_clone = id.clone();
    let (slug, admitted): (String, Vec<Value>) = tokio::task::spawn_blocking(move || {
        let slug: String = conn
            .query_row(
                "SELECT slug FROM rooms WHERE id = ?1",
                rusqlite::params![id_clone],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
                _ => AppError::Internal(e.to_string()),
            })?;
        let mut stmt = conn.prepare(
            "SELECT id, name, role, joined_at FROM participants \
             WHERE room_id = ?1 AND is_admitted = 1 AND is_kicked = 0 \
             ORDER BY joined_at ASC",
        )?;
        let cols = &["id", "name", "role", "joined_at"];
        let rows = stmt
            .query_map(rusqlite::params![id_clone], |row| row_to_json(row, cols))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, AppError>((slug, rows))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // Intersect with the live presence set.
    let mut present = crate::ws::connected_ids(&slug).await;
    present.extend(crate::presence::present_ids(&slug));
    let roster: Vec<Value> = admitted
        .into_iter()
        .filter(|p| {
            p.get("id")
                .and_then(|v| v.as_str())
                .map(|pid| present.contains(pid))
                .unwrap_or(false)
        })
        .collect();

    Ok(Json(roster))
}

// POST /:id/admit/:participantId - admit one participant
async fn admit_participant(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path((id, participant_id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let id_clone = id.clone();
    tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE participants SET is_admitted = 1 WHERE id = ?1 AND room_id = ?2",
            rusqlite::params![participant_id, id_clone],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Participant not found".into()));
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    fire_moderation_changed(&state, &id).await;
    Ok(Json(json!({ "ok": true })))
}

// POST /:id/admit-all - admit all non-admitted
async fn admit_all(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let id_clone = id.clone();
    tokio::task::spawn_blocking(move || {
        conn.execute(
            "UPDATE participants SET is_admitted = 1 WHERE room_id = ?1 AND is_admitted = 0 AND is_kicked = 0",
            rusqlite::params![id_clone],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    fire_moderation_changed(&state, &id).await;
    Ok(Json(json!({ "ok": true })))
}

// GET /:id/kicked - kicked participants
async fn get_kicked(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, AppError> {
    let conn = state.db.get()?;
    let participants = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT id, name, role, joined_at FROM participants \
             WHERE room_id = ?1 AND is_kicked = 1 \
             ORDER BY joined_at ASC",
        )?;
        let cols = &["id", "name", "role", "joined_at"];
        let rows = stmt
            .query_map(rusqlite::params![id], |row| row_to_json(row, cols))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(participants))
}

// POST /:id/unkick/:participantId - unkick a participant
async fn unkick_participant(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path((id, participant_id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let id_clone = id.clone();
    tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE participants SET is_kicked = 0 WHERE id = ?1 AND room_id = ?2",
            rusqlite::params![participant_id, id_clone],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Participant not found".into()));
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    fire_moderation_changed(&state, &id).await;
    Ok(Json(json!({ "ok": true })))
}

/// Look up the room slug from its id and broadcast a moderation-changed
/// event so the WS layer can push the new waiting/kicked lists to any
/// connected presenters. Admin handlers know the room by id; the WS layer
/// keys by slug.
async fn fire_moderation_changed(state: &Arc<AppState>, room_id: &str) {
    let conn = match state.db.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let room_id = room_id.to_string();
    let slug: Option<String> = tokio::task::spawn_blocking(move || {
        conn.query_row(
            "SELECT slug FROM rooms WHERE id = ?1",
            rusqlite::params![room_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    })
    .await
    .ok()
    .flatten();
    if let Some(slug) = slug {
        let _ = state
            .events
            .moderation_changed
            .send(ModerationChangedEvent { slug });
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_rooms).post(create_room))
        .route("/{id}", get(get_room).put(update_room).delete(delete_room))
        .route("/{id}/end", post(end_room))
        .route("/{id}/reactivate", post(reactivate_room))
        .route("/{id}/enter", post(enter_room))
        .route("/{id}/rotate-presenter-key", post(rotate_presenter_key))
        .route("/{id}/waiting", get(get_waiting))
        .route("/{id}/roster", get(get_roster))
        .route("/{id}/admit/{participantId}", post(admit_participant))
        .route("/{id}/admit-all", post(admit_all))
        .route("/{id}/kicked", get(get_kicked))
        .route("/{id}/unkick/{participantId}", post(unkick_participant))
}

/// Normalize ISO 8601 datetime (e.g. "2025-04-15T22:00:00.000Z") to SQLite
/// CURRENT_TIMESTAMP format ("2025-04-15 22:00:00") so string comparisons work.
fn normalize_datetime(s: &str) -> String {
    // "2025-04-15T22:00:00.000Z" → "2025-04-15 22:00:00"
    let s = s.replace('T', " ");
    // Strip fractional seconds and trailing Z
    match s.find('.') {
        Some(i) => s[..i].to_string(),
        None => s.trim_end_matches('Z').to_string(),
    }
}
