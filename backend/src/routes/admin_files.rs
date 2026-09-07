use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use tracing::info;

use crate::auth::AdminAuth;
use crate::error::AppError;
use crate::events::{FileSharedEvent, FileUnsharedEvent};
use crate::routes::files::{extract_extension, sanitize_mime, MAX_FILE_SIZE, SAFE_MIMES};
use crate::routes::sql::FILE_ROOM_SLUGS;
use crate::state::AppState;
use crate::uploads::stream_field_to_temp;

const SORT_WHITELIST: &[&str] = &["created_at", "original_name", "size_bytes", "mime_type"];

#[derive(Deserialize)]
struct FileListQuery {
    search: Option<String>,
    #[serde(rename = "mimePrefix")]
    mime_prefix: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    #[serde(rename = "unassigned")]
    unassigned: Option<String>,
}

fn row_to_file(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let mime: String = row.get(2)?;
    let size: i64 = row.get(3)?;
    let created: String = row.get(4)?;
    let uploader: Option<String> = row.get(5)?;
    let role: Option<String> = row.get(6)?;
    let rooms_csv: Option<String> = row.get(7)?;

    let assigned_rooms: Vec<Value> = match rooms_csv {
        Some(s) if !s.is_empty() => s
            .split('\u{1f}')
            .filter_map(|chunk| {
                let mut parts = chunk.splitn(3, '\u{1e}');
                let id = parts.next()?;
                let slug = parts.next()?;
                let name = parts.next()?;
                Some(json!({ "id": id, "slug": slug, "name": name }))
            })
            .collect(),
        _ => Vec::new(),
    };

    Ok(json!({
        "id": id,
        "name": name,
        "mime": mime,
        "size": size,
        "createdAt": created,
        "uploaderName": uploader,
        "uploaderRole": role,
        "assignedRooms": assigned_rooms,
    }))
}

async fn list_files(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Query(q): Query<FileListQuery>,
) -> Result<Json<Vec<Value>>, AppError> {
    let sort_col: &'static str = q
        .sort
        .as_deref()
        .and_then(|s| SORT_WHITELIST.iter().copied().find(|w| *w == s))
        .unwrap_or("created_at");
    let sort_dir: &'static str = match q.order.as_deref() {
        Some("asc") => "ASC",
        _ => "DESC",
    };
    let search = q.search.clone().unwrap_or_default();
    let mime_prefix = q.mime_prefix.clone().unwrap_or_default();
    let unassigned = matches!(q.unassigned.as_deref(), Some("1") | Some("true"));

    let conn = state.db.get()?;
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<Value>, AppError> {
        let mut sql = String::from(
            "SELECT sf.id, sf.original_name, sf.mime_type, sf.size_bytes, sf.created_at, \
             p.name AS uploader_name, p.role AS uploader_role, \
             GROUP_CONCAT(r.id || char(30) || r.slug || char(30) || r.name, char(31)) AS rooms_csv \
             FROM session_files sf \
             LEFT JOIN participants p ON p.id = sf.uploader_id \
             LEFT JOIN room_files rf ON rf.file_id = sf.id \
             LEFT JOIN rooms r ON r.id = rf.room_id \
             WHERE sf.is_shared = 1 ",
        );
        let mut args: Vec<String> = Vec::new();

        if !search.is_empty() {
            sql.push_str("AND sf.original_name LIKE ?1 ");
            args.push(format!("%{}%", search));
        }
        if !mime_prefix.is_empty() {
            sql.push_str(&format!("AND sf.mime_type LIKE ?{} ", args.len() + 1));
            args.push(format!("{}%", mime_prefix));
        }
        if unassigned {
            sql.push_str(
                "AND NOT EXISTS (SELECT 1 FROM room_files rf2 WHERE rf2.file_id = sf.id) ",
            );
        }
        sql.push_str(&format!(
            "GROUP BY sf.id ORDER BY sf.{} {}",
            sort_col, sort_dir
        ));

        let mut stmt = conn.prepare(&sql)?;
        let params_dyn: Vec<&dyn rusqlite::ToSql> =
            args.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let out = stmt
            .query_map(params_dyn.as_slice(), row_to_file)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(rows))
}

async fn files_stats(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let payload = tokio::task::spawn_blocking(move || -> Result<Value, AppError> {
        // `is_shared = 0` rows are drafts sitting in someone's chat composer,
        // not library files — they are excluded from the listing above, so they
        // must be excluded from its totals too or the two disagree.
        let (total_count, total_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) \
                 FROM session_files WHERE is_shared = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        let mut stmt = conn.prepare(
            "SELECT mime_type, COUNT(*), COALESCE(SUM(size_bytes), 0) \
             FROM session_files WHERE is_shared = 1 GROUP BY mime_type",
        )?;
        let mut buckets: std::collections::HashMap<&str, (i64, i64)> =
            std::collections::HashMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (mime, count, bytes) = row?;
            let bucket = mime_bucket(&mime);
            let entry = buckets.entry(bucket).or_insert((0, 0));
            entry.0 += count;
            entry.1 += bytes;
        }

        let by_mime: Vec<Value> = buckets
            .into_iter()
            .map(|(bucket, (count, bytes))| {
                json!({ "bucket": bucket, "count": count, "bytes": bytes })
            })
            .collect();

        Ok(json!({
            "totalCount": total_count,
            "totalBytes": total_bytes,
            "byMime": by_mime,
        }))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(payload))
}

fn mime_bucket(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime == "application/pdf" {
        "pdf"
    } else if mime.starts_with("application/") || mime.starts_with("text/") {
        "doc"
    } else {
        "other"
    }
}

async fn upload_library_file(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }

        let original_name = field.file_name().unwrap_or("upload").to_string();
        let mime_raw = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let mime = sanitize_mime(&mime_raw);

        // Stream the field to disk in chunks instead of slurping into a
        // Vec — keeps backend RSS bounded on large uploads.
        let files_dir = format!("{}/files", state.config.data_path);
        let uploaded = stream_field_to_temp(&mut field, &files_dir, MAX_FILE_SIZE as u64).await?;
        let size = uploaded.size;
        let content_hash = uploaded.sha256_hex.clone();
        let temp_path = format!("{}/{}", files_dir, uploaded.temp_name);

        let conn = state.db.get()?;
        let hash_lookup = content_hash.clone();
        let existing: Option<(String, String)> = tokio::task::spawn_blocking(move || {
            conn.query_row(
                "SELECT id, original_name FROM session_files WHERE content_hash = ?1 LIMIT 1",
                params![hash_lookup],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| AppError::Internal(e.to_string()))
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

        let (file_id, effective_name, deduped) = if let Some((id, existing_name)) = existing {
            // Dedup hit — discard the temp we just wrote.
            let _ = tokio::fs::remove_file(&temp_path).await;
            info!(file_id = %id, action = "admin_upload_dedup", "upload hit existing blob");
            (id, existing_name, true)
        } else {
            let new_id = uuid::Uuid::new_v4().to_string();
            let ext = extract_extension(&original_name);
            let stored_name = format!("{}{}", new_id, ext);

            tokio::fs::rename(&temp_path, format!("{}/{}", files_dir, stored_name))
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;

            let conn = state.db.get()?;
            let new_id_clone = new_id.clone();
            let original_name_clone = original_name.clone();
            let stored_name_clone = stored_name.clone();
            let mime_clone = mime.clone();
            let hash_clone = content_hash.clone();
            tokio::task::spawn_blocking(move || {
                conn.execute(
                    "INSERT INTO session_files (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, content_hash) \
                     VALUES (?1, NULL, NULL, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        new_id_clone,
                        original_name_clone,
                        stored_name_clone,
                        mime_clone,
                        size as i64,
                        hash_clone,
                    ],
                )
            })
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;

            (new_id, original_name.clone(), false)
        };

        return Ok(Json(json!({
            "id": file_id,
            "name": effective_name,
            "size": size,
            "mime": mime,
            "deduped": deduped,
        })));
    }

    Err(AppError::BadRequest("No file".into()))
}

#[derive(Deserialize)]
struct RenameBody {
    #[serde(rename = "name")]
    original_name: Option<String>,
}

async fn rename_file(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<Json<Value>, AppError> {
    let new_name = body
        .original_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::BadRequest("Name is required".into()))?;

    let conn = state.db.get()?;
    let name_clone = new_name.clone();
    let updated = tokio::task::spawn_blocking(move || -> Result<usize, AppError> {
        let n = conn.execute(
            "UPDATE session_files SET original_name = ?1 WHERE id = ?2",
            params![name_clone, file_id],
        )?;
        Ok(n)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    if updated == 0 {
        return Err(AppError::NotFound("File not found".into()));
    }
    Ok(Json(json!({ "ok": true, "name": new_name })))
}

async fn replace_file(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    // Load existing row so we can delete the old blob after success
    let conn = state.db.get()?;
    let file_id_lookup = file_id.clone();
    let existing: Option<(String, Option<String>)> = tokio::task::spawn_blocking(move || {
        conn.query_row(
            "SELECT stored_path, content_hash FROM session_files WHERE id = ?1",
            params![file_id_lookup],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|e| AppError::Internal(e.to_string()))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let (old_stored, _old_hash) =
        existing.ok_or_else(|| AppError::NotFound("File not found".into()))?;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }

        let original_name = field.file_name().unwrap_or("upload").to_string();
        let mime_raw = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let mime = sanitize_mime(&mime_raw);

        // Stream the field to disk in chunks; rename to its final name
        // once we know the upload finished and we want to keep it.
        let files_dir = format!("{}/files", state.config.data_path);
        let uploaded = stream_field_to_temp(&mut field, &files_dir, MAX_FILE_SIZE as u64).await?;
        let size = uploaded.size;
        let content_hash = uploaded.sha256_hex.clone();
        let temp_path = format!("{}/{}", files_dir, uploaded.temp_name);

        // `content_hash` is UNIQUE, so replacing a file with bytes that already
        // exist under a *different* row would violate the index. Check before
        // the rename below commits the new blob: otherwise the UPDATE fails
        // after the blob is in place and it is orphaned on disk with nothing
        // referencing it (and the admin sees a bare 500).
        let conn = state.db.get()?;
        let hash_probe = content_hash.clone();
        let file_id_probe = file_id.clone();
        let clash: Option<String> = tokio::task::spawn_blocking(move || {
            conn.query_row(
                "SELECT original_name FROM session_files \
                 WHERE content_hash = ?1 AND id != ?2 LIMIT 1",
                params![hash_probe, file_id_probe],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| AppError::Internal(e.to_string()))
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

        if let Some(existing_name) = clash {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(AppError::Conflict(format!(
                "That file's contents are already in the library as “{existing_name}”"
            )));
        }

        let ext = extract_extension(&original_name);
        let new_stored = format!("{}{}", uuid::Uuid::new_v4(), ext);
        tokio::fs::rename(&temp_path, format!("{}/{}", files_dir, new_stored))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        // Update row first
        let conn = state.db.get()?;
        let file_id_update = file_id.clone();
        let new_stored_clone = new_stored.clone();
        let mime_clone = mime.clone();
        let hash_clone = content_hash.clone();
        let updated = tokio::task::spawn_blocking(move || -> Result<usize, AppError> {
            let n = conn.execute(
                "UPDATE session_files SET stored_path = ?1, mime_type = ?2, size_bytes = ?3, content_hash = ?4 WHERE id = ?5",
                params![new_stored_clone, mime_clone, size as i64, hash_clone, file_id_update],
            )?;
            Ok(n)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

        if updated == 0 {
            // Row vanished between our check and update; clean up new blob.
            let _ = tokio::fs::remove_file(format!("{}/{}", files_dir, new_stored)).await;
            return Err(AppError::NotFound("File not found".into()));
        }

        // Delete old blob if nothing else references it.
        let old_path = format!("{}/{}", files_dir, old_stored);
        let conn = state.db.get()?;
        let old_stored_clone = old_stored.clone();
        let still_referenced: i64 =
            tokio::task::spawn_blocking(move || -> Result<i64, AppError> {
                let n: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM session_files WHERE stored_path = ?1",
                        params![old_stored_clone],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                Ok(n)
            })
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;

        if still_referenced == 0 {
            let _ = tokio::fs::remove_file(&old_path).await;
        }

        // Notify each assigned room to refresh
        broadcast_shared_to_assigned(&state, &file_id, &mime, size, &original_name).await;

        return Ok(Json(json!({
            "id": file_id,
            "size": size,
            "mime": mime,
        })));
    }

    Err(AppError::BadRequest("No file".into()))
}

async fn delete_file(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    delete_files_inner(&state, &[file_id]).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BulkIds {
    #[serde(rename = "fileIds")]
    file_ids: Vec<String>,
}

async fn bulk_delete_files(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<BulkIds>,
) -> Result<Json<Value>, AppError> {
    if body.file_ids.is_empty() {
        return Ok(Json(json!({ "ok": true, "deleted": 0 })));
    }
    let n = delete_files_inner(&state, &body.file_ids).await?;
    Ok(Json(json!({ "ok": true, "deleted": n })))
}

/// Delete library files by id. Returns the number of **files** actually
/// removed — not the number of rooms notified, which is a different number
/// whenever a file is assigned to zero rooms (reported 0) or several
/// (over-counted).
async fn delete_files_inner(state: &Arc<AppState>, ids: &[String]) -> Result<usize, AppError> {
    // For each file: collect assigned room slugs (for WS notify), collect
    // the stored_path, then DELETE the row (room_files cascades). If no
    // remaining row references the same stored_path, remove the blob.
    let conn = state.db.get()?;
    let ids_owned: Vec<String> = ids.to_vec();
    type UnshareResult = Result<(Vec<(String, String)>, Vec<String>, usize), AppError>;
    let (to_notify, stored_paths, deleted) =
        tokio::task::spawn_blocking(move || -> UnshareResult {
            let mut notify: Vec<(String, String)> = Vec::new();
            let mut paths: Vec<String> = Vec::new();
            let mut deleted = 0usize;
            for id in &ids_owned {
                let stored: Option<String> = conn
                    .query_row(
                        "SELECT stored_path FROM session_files WHERE id = ?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()
                    .unwrap_or(None);
                if let Some(p) = stored {
                    paths.push(p);
                }

                // Collect all slugs this file is associated with (direct room_id
                // or via room_files).
                let mut stmt = conn.prepare(FILE_ROOM_SLUGS)?;
                let slugs = stmt
                    .query_map(params![id], |row| row.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .collect::<Vec<_>>();
                for slug in slugs {
                    notify.push((slug, id.clone()));
                }

                deleted += conn.execute("DELETE FROM session_files WHERE id = ?1", params![id])?;
            }
            Ok((notify, paths, deleted))
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

    let files_dir = format!("{}/files", state.config.data_path);
    for stored in stored_paths {
        // Only remove the blob if no surviving row still references it.
        let conn = state.db.get()?;
        let path_check = stored.clone();
        let n: i64 = tokio::task::spawn_blocking(move || -> Result<i64, AppError> {
            conn.query_row(
                "SELECT COUNT(*) FROM session_files WHERE stored_path = ?1",
                params![path_check],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Internal(e.to_string()))
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
        if n == 0 {
            let _ = tokio::fs::remove_file(format!("{}/{}", files_dir, stored)).await;
        }
    }

    for (slug, id) in to_notify {
        let _ = state
            .events
            .file_unshared
            .send(FileUnsharedEvent { slug, id });
    }

    Ok(deleted)
}

async fn download_library_file(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    serve_file(&state, &file_id, "attachment").await
}

async fn preview_library_file(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    serve_file(&state, &file_id, "inline").await
}

async fn serve_file(
    state: &Arc<AppState>,
    file_id: &str,
    disposition: &str,
) -> Result<impl IntoResponse, AppError> {
    let conn = state.db.get()?;
    let file_id_owned = file_id.to_string();
    let row = tokio::task::spawn_blocking(move || -> Result<(String, String, String), AppError> {
        let (stored, name, mime): (String, String, String) = conn
            .query_row(
                "SELECT stored_path, original_name, mime_type FROM session_files WHERE id = ?1",
                params![file_id_owned],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| AppError::Internal(e.to_string()))?
            .ok_or_else(|| AppError::NotFound("File not found".into()))?;
        Ok((stored, name, mime))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let (stored, name, mime) = row;
    let path = format!("{}/files/{}", state.config.data_path, stored);
    // Stream from disk rather than buffering the whole blob in memory —
    // library files can be multi-GB and several admins may pull at once.
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::NotFound("File not found".into()))?;
    let len = file
        .metadata()
        .await
        .map(|m| m.len())
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let body = Body::from_stream(ReaderStream::new(file));

    // Downloads force attachment; preview requests serve inline but only for
    // whitelisted MIME types to prevent hostile renders.
    let final_disposition = if disposition == "inline" && SAFE_MIMES.contains(&mime.as_str()) {
        format!("inline; filename=\"{}\"", name.replace('"', "\\\""))
    } else {
        format!("attachment; filename=\"{}\"", name.replace('"', "\\\""))
    };

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_DISPOSITION, final_disposition),
            // Explicit length so the body isn't chunked and download
            // progress works in the browser.
            (header::CONTENT_LENGTH, len.to_string()),
            (
                header::HeaderName::from_static("x-content-type-options"),
                "nosniff".to_string(),
            ),
        ],
        body,
    ))
}

async fn assign_files_to_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(body): Json<BulkIds>,
) -> Result<Json<Value>, AppError> {
    if body.file_ids.is_empty() {
        return Ok(Json(json!({ "ok": true, "assigned": 0 })));
    }

    let conn = state.db.get()?;
    let room_id_clone = room_id.clone();
    let ids_clone = body.file_ids.clone();
    let (slug, newly_assigned): (String, Vec<String>) =
        tokio::task::spawn_blocking(move || -> Result<(String, Vec<String>), AppError> {
            let slug: String = conn
                .query_row(
                    "SELECT slug FROM rooms WHERE id = ?1",
                    params![room_id_clone],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| AppError::Internal(e.to_string()))?
                .ok_or_else(|| AppError::NotFound("Room not found".into()))?;

            let mut newly = Vec::new();
            for fid in &ids_clone {
                let changed = conn.execute(
                    "INSERT OR IGNORE INTO room_files (room_id, file_id) VALUES (?1, ?2)",
                    params![room_id_clone, fid],
                )?;
                if changed > 0 {
                    newly.push(fid.clone());
                }
            }
            Ok((slug, newly))
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

    // Emit file:shared for each newly assigned file so viewers' right-panel
    // Files section updates live.
    let ts = crate::time::now_ms();

    for fid in &newly_assigned {
        let conn = state.db.get()?;
        let fid_clone = fid.clone();
        let meta: Option<(String, String, i64)> = tokio::task::spawn_blocking(move || {
            conn.query_row(
                "SELECT original_name, mime_type, size_bytes FROM session_files WHERE id = ?1",
                params![fid_clone],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

        if let Some((name, mime, size)) = meta {
            let _ = state.events.file_shared.send(FileSharedEvent {
                slug: slug.clone(),
                id: fid.clone(),
                participant_id: String::new(),
                uploader_name: "Admin".into(),
                role: "admin".into(),
                name,
                size: size as u64,
                mime,
                ts,
            });
        }
    }

    Ok(Json(json!({
        "ok": true,
        "assigned": newly_assigned.len(),
    })))
}

async fn unassign_file_from_room(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path((room_id, file_id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let room_id_clone = room_id.clone();
    let file_id_clone = file_id.clone();
    let slug: Option<String> =
        tokio::task::spawn_blocking(move || -> Result<Option<String>, AppError> {
            let slug: Option<String> = conn
                .query_row(
                    "SELECT slug FROM rooms WHERE id = ?1",
                    params![room_id_clone],
                    |row| row.get(0),
                )
                .optional()
                .unwrap_or(None);
            conn.execute(
                "DELETE FROM room_files WHERE room_id = ?1 AND file_id = ?2",
                params![room_id_clone, file_id_clone],
            )?;
            // Also clear direct room_id FK if this file was uploaded in that
            // room originally, so viewers in that room lose access entirely.
            conn.execute(
                "UPDATE session_files SET room_id = NULL WHERE id = ?1 AND room_id = ?2",
                params![file_id_clone, room_id_clone],
            )?;
            Ok(slug)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

    if let Some(slug) = slug {
        let _ = state
            .events
            .file_unshared
            .send(FileUnsharedEvent { slug, id: file_id });
    }

    Ok(Json(json!({ "ok": true })))
}

async fn broadcast_shared_to_assigned(
    state: &Arc<AppState>,
    file_id: &str,
    mime: &str,
    size: u64,
    name: &str,
) {
    let conn = match state.db.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let file_id_owned = file_id.to_string();
    let slugs: Vec<String> =
        match tokio::task::spawn_blocking(move || -> Result<Vec<String>, AppError> {
            let mut stmt = conn.prepare(FILE_ROOM_SLUGS)?;
            let out = stmt
                .query_map(params![file_id_owned], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(out)
        })
        .await
        {
            Ok(Ok(v)) => v,
            _ => return,
        };

    let ts = crate::time::now_ms();
    for slug in slugs {
        let _ = state.events.file_shared.send(FileSharedEvent {
            slug,
            id: file_id.to_string(),
            participant_id: String::new(),
            uploader_name: "Admin".into(),
            role: "admin".into(),
            name: name.to_string(),
            size,
            mime: mime.to_string(),
            ts,
        });
    }
}

pub fn files_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_files).post(upload_library_file))
        .route("/stats", get(files_stats))
        .route("/bulk-delete", post(bulk_delete_files))
        .route(
            "/{id}",
            patch(rename_file).put(replace_file).delete(delete_file),
        )
        .route("/{id}/download", get(download_library_file))
        .route("/{id}/preview", get(preview_library_file))
        .layer(DefaultBodyLimit::max(MAX_FILE_SIZE))
}

pub fn room_assign_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/{id}/files", post(assign_files_to_room))
        .route("/{id}/files/{fileId}", delete(unassign_file_from_room))
}
