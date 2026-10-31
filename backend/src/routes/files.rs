use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use tracing::info;

use crate::error::{multipart_error, AppError};
use crate::events::FileUnsharedEvent;
use crate::state::AppState;
use crate::uploads::{drain, stream_field_to_temp};

/// Upload size ceiling, shared by the participant and admin-library upload
/// routes. `uploads::stream_field_to_temp` formats its rejection message from
/// this value, so the number and the message it produces cannot disagree.
pub const MAX_FILE_SIZE: usize = 2560 * 1024 * 1024; // 2.5 GB

pub const SAFE_MIMES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/avif",
    // image/svg+xml is intentionally NOT here: SVGs served inline execute any
    // embedded <script> in the browser's document context, which would be a
    // stored-XSS vector against the admin previewing a participant upload.
    // `nosniff` doesn't help — the content-type really is image/svg+xml.
    // SVGs fall through to application/octet-stream + attachment disposition.
    "video/mp4",
    "video/quicktime",
    "video/x-quicktime",
    "video/webm",
    "audio/mpeg",
    "audio/wav",
    "audio/ogg",
    "audio/flac",
    "application/pdf",
    "text/plain",
    "application/zip",
    "application/x-zip-compressed",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
];

#[derive(Deserialize)]
struct ParticipantQuery {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
    // Set by the in-room display flow (Show button → OvenPlayer source).
    // Switches Content-Disposition to inline so the browser plays the file
    // rather than triggering a download, and relabels video/quicktime as
    // video/mp4 so H.264-in-MOV files (common from phones / cameras) are
    // not rejected upfront by Chrome/Firefox.
    display: Option<String>,
}

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
}

struct ParticipantInfo {
    id: String,
    role: String,
    room_id: String,
    slug: String,
    /// Not ended and not past its expiry. Reads stay allowed in a room that is
    /// over; only uploads check this.
    room_open: bool,
}

fn validate_participant(
    conn: &r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>,
    pid: &str,
    token: &str,
    slug: &str,
) -> Result<ParticipantInfo, AppError> {
    let row = conn
        .query_row(
            "SELECT p.id, p.role, r.id AS room_id, r.slug, \
             (r.status != 'ended' \
              AND (r.expires_at IS NULL OR r.expires_at > CURRENT_TIMESTAMP)) AS room_open \
             FROM participants p \
             JOIN rooms r ON r.id = p.room_id \
             WHERE p.id = ?1 AND p.token = ?2 AND r.slug = ?3 \
             AND p.is_admitted = 1 AND p.is_kicked = 0",
            params![pid, token, slug],
            |row| {
                Ok(ParticipantInfo {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    room_id: row.get(2)?,
                    slug: row.get(3)?,
                    room_open: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::Unauthorized("Unauthorized".into()))?;
    Ok(row)
}

pub fn sanitize_mime(mime: &str) -> String {
    if SAFE_MIMES.contains(&mime) {
        mime.to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// Derive a `.ext` suffix (including leading dot) from a filename, or "" if
/// the filename has no usable extension. Used for the stored blob filename so
/// downloads can hint at the type.
///
/// The result is concatenated straight into an on-disk path, so it MUST stay
/// path-safe: ASCII alphanumeric only, 1-10 chars, no `/`, `\`, or `.`.
/// Anything else is dropped — a crafted filename like `x.../../../../tmp/p`
/// would otherwise escape the files dir and let an uploader write anywhere
/// the backend user can.
pub fn extract_extension(name: &str) -> String {
    let ext = match name.rsplit('.').next() {
        Some(e) if e != name => e,
        _ => return String::new(),
    };
    if ext.is_empty() || ext.len() > 10 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return String::new();
    }
    format!(".{}", ext)
}

/// Total a room's participants may keep uploaded (drafts included), on top of
/// the per-file [`MAX_FILE_SIZE`]. Rooms are the admin's to create, so this and
/// the free-space floor in `uploads` together bound what guests can put on the
/// disk; there used to be no limit on the number of uploads at all.
pub const ROOM_UPLOAD_QUOTA: u64 = 20 * 1024 * 1024 * 1024;

fn room_uploaded_bytes(
    conn: &r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>,
    room_id: &str,
) -> Result<u64, AppError> {
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size_bytes), 0) FROM session_files WHERE room_id = ?1",
        params![room_id],
        |row| row.get(0),
    )?;
    Ok(n.max(0) as u64)
}

fn quota_exceeded() -> AppError {
    AppError::PayloadTooLarge(format!(
        "This room's uploads are full ({} GB)",
        ROOM_UPLOAD_QUOTA / 1_073_741_824
    ))
}

/// A participant upload. Always a draft (`is_shared = 0`): it stays invisible —
/// no `file:shared`, no library mirror — until the uploader sends it with the
/// WS `file:share` message, which is the only way the viewer has ever uploaded.
///
/// There used to be a second, immediate-share mode that de-duplicated by
/// content hash, handing the uploader the *existing* row for identical bytes —
/// so a guest in room B who uploaded the same file as someone in room A got
/// room A's filename and uploader name back, and the file joined room B's list.
/// No client used that mode; it went rather than being made safe.
async fn upload_file(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    let pid = query
        .participant_id
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();
    let token = query
        .token
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();

    // Validate participant, and check the room's quota before reading a byte.
    let conn = state.db.get()?;
    let slug_clone = slug.clone();
    let pid_clone = pid.clone();
    let token_clone = token.clone();
    let (participant, used) = tokio::task::spawn_blocking(move || {
        let participant = validate_participant(&conn, &pid_clone, &token_clone, &slug_clone)?;
        let used = room_uploaded_bytes(&conn, &participant.room_id)?;
        Ok::<_, AppError>((participant, used))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    if !participant.room_open {
        drain(&mut multipart).await;
        return Err(AppError::Gone("Room has ended".into()));
    }
    if used >= ROOM_UPLOAD_QUOTA {
        drain(&mut multipart).await;
        return Err(quota_exceeded());
    }

    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        if field.name() != Some("file") {
            continue;
        }
        let original_name = field.file_name().unwrap_or("upload").to_string();
        let mime_raw = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let mime = sanitize_mime(&mime_raw);

        // Stream the field body to disk in chunks (bounded memory) instead of
        // materialising it as a single Vec. The helper computes Sha256 + size
        // as it goes and deletes its temp file on any error path.
        let files_dir = format!("{}/files", state.config.data_path);
        let uploaded = stream_field_to_temp(&mut field, &files_dir, MAX_FILE_SIZE as u64).await?;
        let size = uploaded.size;
        let temp_path = format!("{}/{}", files_dir, uploaded.temp_name);

        // Re-checked with the real size, now that it is known.
        if used.saturating_add(size) > ROOM_UPLOAD_QUOTA {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(quota_exceeded());
        }

        let file_id = uuid::Uuid::new_v4().to_string();
        let stored_name = format!("{}{}", file_id, extract_extension(&original_name));
        tokio::fs::rename(&temp_path, format!("{}/{}", files_dir, stored_name))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        // `created_at` goes in as seconds through SQLite's `unixepoch` modifier;
        // the wire carries milliseconds (see `crate::time`).
        let ts_secs = (crate::time::now_ms() / 1000) as i64;
        let conn = state.db.get()?;
        let (id_db, room_id, pid_db, name_db, stored_db, mime_db) = (
            file_id.clone(),
            participant.room_id.clone(),
            participant.id.clone(),
            original_name.clone(),
            stored_name,
            mime,
        );
        // No content hash: drafts never take part in de-duplication.
        tokio::task::spawn_blocking(move || {
            conn.execute(
                "INSERT INTO session_files (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, content_hash, is_shared, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 0, datetime(?8, 'unixepoch'))",
                params![id_db, room_id, pid_db, name_db, stored_db, mime_db, size as i64, ts_secs],
            )
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

        info!(
            room_slug = %participant.slug,
            actor_id = %participant.id,
            file_id = %file_id,
            size,
            action = "upload_complete",
            "file upload complete",
        );

        return Ok(Json(json!({
            "id": file_id,
            "name": original_name,
            "size": size,
        })));
    }

    Err(AppError::BadRequest("No file".into()))
}

async fn list_files(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Query(query): Query<ParticipantQuery>,
) -> Result<Json<Vec<Value>>, AppError> {
    let pid = query
        .participant_id
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();
    let token = query
        .token
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();

    let conn = state.db.get()?;
    let slug_clone = slug.clone();
    let files = tokio::task::spawn_blocking(move || {
        let participant = validate_participant(&conn, &pid, &token, &slug_clone)?;

        // Union of (a) files whose room_id matches this room (legacy path)
        // and (b) files linked via room_files (library + mirrored participant
        // uploads). DISTINCT because both sources overlap after migration.
        // Drafts (is_shared = 0) stay invisible until the uploader hits send.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT sf.id, sf.original_name, sf.mime_type, sf.size_bytes, sf.created_at, \
             p.name AS uploader_name, p.role AS uploader_role \
             FROM session_files sf \
             LEFT JOIN participants p ON p.id = sf.uploader_id \
             WHERE sf.is_shared = 1 \
               AND (sf.room_id = ?1 \
                    OR sf.id IN (SELECT file_id FROM room_files WHERE room_id = ?1)) \
             ORDER BY sf.created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![participant.room_id], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "mime": row.get::<_, String>(2)?,
                    "size": row.get::<_, i64>(3)?,
                    "createdAt": row.get::<_, String>(4)?,
                    "uploaderName": row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "Admin".into()),
                    "uploaderRole": row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "admin".into()),
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, AppError>(rows)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(files))
}

async fn download_file(
    State(state): State<Arc<AppState>>,
    Path((slug, file_id)): Path<(String, String)>,
    Query(query): Query<ParticipantQuery>,
) -> Result<impl IntoResponse, AppError> {
    let pid = query
        .participant_id
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();
    let token = query
        .token
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();

    let conn = state.db.get()?;
    let data_path = state.config.data_path.clone();
    let (file_data, original_name, mime) = tokio::task::spawn_blocking(move || {
        let participant = validate_participant(&conn, &pid, &token, &slug)?;

        // Authorised if the file is directly attached to the room OR assigned
        // to it via room_files (admin library).
        //
        // Drafts (`is_shared = 0`) are excluded for everyone except the person
        // who uploaded them: they are files attached to a chat composer and not
        // yet sent, so room membership alone must not grant access. `list_files`
        // has always filtered them; this path did not, which left them
        // retrievable by anyone in the room who knew the id.
        let (stored_name, original_name, mime): (String, String, String) = conn
            .query_row(
                "SELECT stored_path, original_name, mime_type FROM session_files \
                 WHERE id = ?1 \
                   AND (is_shared = 1 OR uploader_id = ?3) \
                   AND (room_id = ?2 \
                    OR id IN (SELECT file_id FROM room_files WHERE room_id = ?2))",
                params![file_id, participant.room_id, participant.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| AppError::Internal(e.to_string()))?
            .ok_or_else(|| AppError::NotFound("File not found".into()))?;

        let file_path = format!("{}/files/{}", data_path, stored_name);
        Ok::<_, AppError>((file_path, original_name, mime))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // Stream the blob from disk instead of buffering it whole — a 2.5 GB
    // upload would otherwise spike backend RSS per concurrent download
    // (mirrors the streaming upload path in uploads.rs).
    let file = tokio::fs::File::open(&file_data)
        .await
        .map_err(|_| AppError::NotFound("File not found".into()))?;
    let len = file
        .metadata()
        .await
        .map(|m| m.len())
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let body = Body::from_stream(ReaderStream::new(file));

    let is_display = matches!(query.display.as_deref(), Some("1" | "true"));
    let disposition = if is_display {
        "inline".to_string()
    } else {
        format!(
            "attachment; filename=\"{}\"",
            original_name.replace('"', "\\\"")
        )
    };
    // .mov H.264 plays fine in Chromium-family browsers but only when
    // the Content-Type is video/mp4 (the QuickTime container header is
    // an ISO BMFF superset). Browsers reject `video/quicktime` upfront
    // regardless of inner codec, so on the display path we relabel.
    let served_mime = if is_display && mime == "video/quicktime" {
        "video/mp4".to_string()
    } else {
        mime
    };

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, served_mime),
            (header::CONTENT_DISPOSITION, disposition),
            // Explicit length so the streamed body isn't chunked and the
            // browser's download progress bar works.
            (header::CONTENT_LENGTH, len.to_string()),
            (
                header::HeaderName::from_static("x-content-type-options"),
                "nosniff".to_string(),
            ),
        ],
        body,
    ))
}

/// Delete a file from the room. Two callers / two behaviours, handled
/// on the same endpoint:
///
/// 1. **Draft cleanup** — the uploading participant removes the chip
///    from their chat composer before sending. Allowed for anyone on
///    their own un-shared draft.
/// 2. **Host delete** — a presenter removes a shared file from the
///    room. If the file was uploaded directly into this room
///    (`session_files.room_id` matches), the row is dropped and the
///    blob is reclaimed when no other row still references it. If the
///    file is a library file assigned via `room_files`, only the
///    assignment is removed — the library copy survives.
///
/// The host path broadcasts `file:removed` so every connected client
/// can drop the row from their chat and files panel.
#[derive(Debug)]
enum DeleteOutcome {
    /// Nothing to do — file doesn't exist (or was already gone).
    NotFound,
    /// Draft removed (no broadcast — no one ever saw it).
    DraftCleanup { stored_path: String },
    /// Host removed the row (and possibly the blob).
    HostHardDelete { stored_path: String },
    /// Host removed only the room_files link (library copy survives).
    HostUnassign,
}

async fn delete_room_file(
    State(state): State<Arc<AppState>>,
    Path((slug, file_id)): Path<(String, String)>,
    Query(query): Query<ParticipantQuery>,
) -> Result<StatusCode, AppError> {
    let pid = query
        .participant_id
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();
    let token = query
        .token
        .as_deref()
        .ok_or(AppError::Unauthorized("Unauthorized".into()))?
        .to_string();

    let conn = state.db.get()?;
    let data_path = state.config.data_path.clone();
    let slug_for_block = slug.clone();
    let file_id_for_block = file_id.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<DeleteOutcome, AppError> {
        let participant = validate_participant(&conn, &pid, &token, &slug_for_block)?;

        // Look up the file row. Anyone in the room may *attempt* delete;
        // we authorize by inspecting ownership / role next.
        type Row = (String, Option<String>, String, i64);
        let row: Option<Row> = conn
            .query_row(
                "SELECT stored_path, room_id, uploader_id, is_shared \
                 FROM session_files WHERE id = ?1",
                params![file_id_for_block],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| AppError::Internal(e.to_string()))?;

        let (stored_path, file_room_id, uploader_id, is_shared) = match row {
            Some(r) => r,
            None => return Ok(DeleteOutcome::NotFound),
        };

        // (1) Draft cleanup — own un-shared upload. Allowed for any role.
        if is_shared == 0 && uploader_id == participant.id {
            conn.execute(
                "DELETE FROM session_files WHERE id = ?1",
                params![file_id_for_block],
            )
            .map_err(|e| AppError::Internal(e.to_string()))?;
            return Ok(DeleteOutcome::DraftCleanup { stored_path });
        }

        // (2) Host delete — presenter only, file must be visible in this room.
        if participant.role != "presenter" {
            return Err(AppError::Forbidden("Host only".into()));
        }
        let direct_here = file_room_id.as_deref() == Some(participant.room_id.as_str());
        let linked_here: bool = conn
            .query_row(
                "SELECT 1 FROM room_files WHERE room_id = ?1 AND file_id = ?2",
                params![participant.room_id, file_id_for_block],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| AppError::Internal(e.to_string()))?
            .is_some();
        if !direct_here && !linked_here {
            return Ok(DeleteOutcome::NotFound);
        }

        // One `session_files` row can serve several rooms: the admin library
        // assigns any shared file — this room's uploads included — to other
        // rooms through `room_files`. So "this file originated in my room" does
        // not imply "only my room has it" — hard-deleting on `direct_here` alone
        // removed the file, and its blob, out from under every other room
        // holding it. Only take the row when no other room still does.
        let other_rooms: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM room_files WHERE file_id = ?1 AND room_id != ?2",
                params![file_id_for_block, participant.room_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| AppError::Internal(e.to_string()))?
            .unwrap_or(0);

        if direct_here && other_rooms == 0 {
            // Hard delete the row (room_files links cascade via FK).
            conn.execute(
                "DELETE FROM session_files WHERE id = ?1",
                params![file_id_for_block],
            )
            .map_err(|e| AppError::Internal(e.to_string()))?;
            Ok(DeleteOutcome::HostHardDelete { stored_path })
        } else {
            // Detach from this room only. When the row also *originated* here,
            // clearing `room_id` is part of detaching — `list_files` matches on
            // `sf.room_id` as well as the junction table, so dropping only the
            // link would leave the file still listed in this room. It survives
            // as a library row for whichever rooms still hold it.
            if direct_here {
                conn.execute(
                    "UPDATE session_files SET room_id = NULL WHERE id = ?1 AND room_id = ?2",
                    params![file_id_for_block, participant.room_id],
                )
                .map_err(|e| AppError::Internal(e.to_string()))?;
            }
            conn.execute(
                "DELETE FROM room_files WHERE room_id = ?1 AND file_id = ?2",
                params![participant.room_id, file_id_for_block],
            )
            .map_err(|e| AppError::Internal(e.to_string()))?;
            Ok(DeleteOutcome::HostUnassign)
        }
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    match outcome {
        DeleteOutcome::NotFound => {}
        DeleteOutcome::DraftCleanup { stored_path } => {
            let _ = tokio::fs::remove_file(format!("{}/files/{}", data_path, stored_path)).await;
        }
        DeleteOutcome::HostHardDelete { stored_path } => {
            // Reclaim the blob only when no surviving row still references it.
            let conn = state.db.get()?;
            let path_check = stored_path.clone();
            let refs: i64 = tokio::task::spawn_blocking(move || -> Result<i64, AppError> {
                conn.query_row(
                    "SELECT COUNT(*) FROM session_files WHERE stored_path = ?1",
                    params![path_check],
                    |row| row.get(0),
                )
                .map_err(|e| AppError::Internal(e.to_string()))
            })
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;
            if refs == 0 {
                let _ =
                    tokio::fs::remove_file(format!("{}/files/{}", data_path, stored_path)).await;
            }
            let _ = state.events.file_unshared.send(FileUnsharedEvent {
                slug: slug.clone(),
                id: file_id.clone(),
            });
        }
        DeleteOutcome::HostUnassign => {
            let _ = state.events.file_unshared.send(FileUnsharedEvent {
                slug: slug.clone(),
                id: file_id.clone(),
            });
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<Arc<AppState>> {
    let upload_handler = if crate::routes::rate_limit::enabled() {
        post(upload_file).layer(crate::routes::rate_limit::upload_layer())
    } else {
        post(upload_file)
    };
    Router::new()
        .route("/{slug}/files", upload_handler.get(list_files))
        .route("/{slug}/files/{fileId}/download", get(download_file))
        .route(
            "/{slug}/files/{fileId}",
            axum::routing::delete(delete_room_file),
        )
        .layer(DefaultBodyLimit::max(MAX_FILE_SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_extension_normal_cases() {
        assert_eq!(extract_extension("photo.jpg"), ".jpg");
        assert_eq!(extract_extension("doc.pdf"), ".pdf");
        assert_eq!(extract_extension("archive.tar.gz"), ".gz");
        assert_eq!(extract_extension("noext"), "");
        assert_eq!(extract_extension(""), "");
        // Dotfile: the whole basename after the dot is treated as the ext.
        // Path-safe so we keep it.
        assert_eq!(extract_extension(".hidden"), ".hidden");
    }

    #[test]
    fn extract_extension_rejects_path_traversal() {
        // The dangerous cases: anything that would let the on-disk write
        // escape the files dir or smuggle a separator.
        assert_eq!(extract_extension("evil.../../../../tmp/p"), "");
        assert_eq!(extract_extension("x.tar/../etc/passwd"), "");
        assert_eq!(extract_extension("y.a/b"), "");
        assert_eq!(extract_extension("z.a\\b"), "");
        assert_eq!(extract_extension("w..ext"), ".ext"); // single trailing ext OK
    }

    #[test]
    fn extract_extension_caps_length_and_charset() {
        // 10 alnum chars OK, 11 rejected.
        assert_eq!(extract_extension("a.abcdefghij"), ".abcdefghij");
        assert_eq!(extract_extension("a.abcdefghijk"), "");
        // Non-alnum (unicode, punctuation) rejected.
        assert_eq!(extract_extension("a.exé"), "");
        assert_eq!(extract_extension("a.ex!"), "");
    }

    #[test]
    fn sanitize_mime_drops_svg() {
        // Regression for the stored-XSS preview path: SVG must not survive
        // sanitize_mime, so admin preview falls through to octet-stream
        // and gets attachment disposition.
        assert_eq!(sanitize_mime("image/svg+xml"), "application/octet-stream");
        assert_eq!(sanitize_mime("image/png"), "image/png");
        assert_eq!(sanitize_mime("text/html"), "application/octet-stream");
    }
}
