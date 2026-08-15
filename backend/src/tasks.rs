use crate::livekit::LiveKitClient;
use crate::state::AppState;
use base64::Engine;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

// ---------------------------------------------------------------------------
// OME Poller -- every 30s
// Reconciles room status against the OME API in BOTH directions: a room whose
// stream key stopped broadcasting drops to 'pending', and a 'pending' room
// whose key is already broadcasting is promoted to 'live'.
// ---------------------------------------------------------------------------

pub fn spawn_ome_poller(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Err(e) = poll_ome(&state).await {
                tracing::debug!("[poller] OME poll error: {}", e);
            }
        }
    });
}

async fn poll_ome(state: &Arc<AppState>) -> Result<(), Box<dyn std::error::Error>> {
    let token = base64::engine::general_purpose::STANDARD.encode(&state.config.ome_api_token);
    let url = format!(
        "{}/vhosts/default/apps/live/streams",
        state.config.ome_api_url
    );

    let res = state
        .http_client
        .get(&url)
        .header("Authorization", format!("Basic {}", token))
        .send()
        .await?;

    if !res.status().is_success() {
        return Ok(());
    }

    let data: serde_json::Value = res.json().await?;
    let active_keys: std::collections::HashSet<String> = data
        .get("response")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let db = state.db.get()?;
    let transitions = reconcile_rooms(&db, &active_keys)?;

    for slug in &transitions.went_live {
        let _ = state.events.room_live.send(slug.clone());
    }
    for slug in &transitions.went_pending {
        let _ = state.events.room_pending.send(slug.clone());
    }

    Ok(())
}

/// Room slugs whose status the reconciler changed, by direction.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RoomTransitions {
    pub went_live: Vec<String>,
    pub went_pending: Vec<String>,
}

/// Reconcile room status against the set of stream keys OME reports as
/// broadcasting.
///
/// Bidirectional on purpose. The admission webhook is the only other path to
/// 'live', and it fires exactly once, when a publisher *starts* — so attaching
/// a stream key to a room while that key is already broadcasting used to leave
/// the room stuck in 'pending' until the encoder reconnected (gh #225). This
/// closes that gap.
///
/// The promote side is deliberately narrower than the demote side:
///
/// - **Only `pending` rooms.** Promoting a `scheduled` room would start it
///   before its start time, and an `ended` room must stay ended.
/// - **Only unblocked keys.** `routes/ome.rs` sets `blocked = 1` *before*
///   asking OME to drop the stream, precisely to win that race; without the
///   same filter here the poller could re-promote a room an admin just kicked,
///   in the window before the stream leaves OME's list.
///
/// Split out from the HTTP fetch so it is testable without a live OME.
pub fn reconcile_rooms(
    conn: &rusqlite::Connection,
    active_keys: &std::collections::HashSet<String>,
) -> Result<RoomTransitions, rusqlite::Error> {
    let mut transitions = RoomTransitions::default();

    // Demote: live rooms whose key stopped broadcasting.
    let live_rooms: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT r.id, r.slug, sk.key_token \
             FROM rooms r \
             JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.status = 'live'",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (id, slug, key_token) in live_rooms {
        if !active_keys.contains(&key_token) {
            conn.execute(
                "UPDATE rooms SET status = 'pending' WHERE id = ?1",
                rusqlite::params![id],
            )?;
            tracing::info!("[poller] Room {} -> pending (stream dropped)", id);
            transitions.went_pending.push(slug);
        }
    }

    // Promote: pending rooms whose key is already broadcasting.
    let pending_rooms: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT r.id, r.slug, sk.key_token \
             FROM rooms r \
             JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.status = 'pending' AND sk.blocked = 0",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (id, slug, key_token) in pending_rooms {
        if active_keys.contains(&key_token) {
            conn.execute(
                "UPDATE rooms SET status = 'live' WHERE id = ?1",
                rusqlite::params![id],
            )?;
            tracing::info!("[poller] Room {} -> live (stream already broadcasting)", id);
            transitions.went_live.push(slug);
        }
    }

    Ok(transitions)
}

// ---------------------------------------------------------------------------
// Expiry Poller -- every 60s
// Ends rooms that have passed their expires_at timestamp.
// ---------------------------------------------------------------------------

pub fn spawn_expiry_poller(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = poll_expiry(&state).await {
                tracing::debug!("[poller] Expiry poll error: {}", e);
            }
        }
    });
}

pub async fn poll_expiry(state: &Arc<AppState>) -> Result<(), Box<dyn std::error::Error>> {
    let expired_rooms: Vec<(String, String)> = {
        let db = state.db.get()?;
        let mut stmt = db.prepare(
            "SELECT id, slug FROM rooms \
             WHERE expires_at IS NOT NULL \
             AND expires_at < CURRENT_TIMESTAMP \
             AND status != 'ended'",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if expired_rooms.is_empty() {
        return Ok(());
    }

    let livekit = LiveKitClient::new(&state.config, state.http_client.clone());

    for (id, slug) in expired_rooms {
        let db = state.db.get()?;
        db.execute(
            "UPDATE rooms SET status = 'ended', ended_at = CURRENT_TIMESTAMP WHERE id = ?1",
            rusqlite::params![id],
        )?;

        let _ = state.events.room_ended.send(slug.clone());
        tracing::info!("[poller] Room {} expired -> ended", id);

        // Delete LiveKit room (best-effort)
        if let Err(e) = livekit.delete_room(&slug).await {
            tracing::debug!("[poller] LiveKit deleteRoom error for {}: {}", slug, e);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Start Poller -- every 60s
// Opens scheduled rooms (issue #200): once starts_at has passed, flips the
// room to 'pending' and — when the waiting room is off — auto-admits everyone
// who was held during the scheduled window. Mirrors the expiry poller. Held
// viewers hold the admission SSE, which emits "admitted" on its next tick once
// is_admitted flips, so no new event type is needed.
//
// The admit is decoupled from the status flip: the OME admission webhook flips
// a still-'scheduled' room straight to 'live' the moment the host starts
// streaming (webhook.rs) and admits nobody, so keying only off status='scheduled'
// would miss those rooms. Instead we also release holds on any past-start,
// waiting-room-off room that still has held viewers, whatever its current status.
// Admission stays time-based: a host going live *before* starts_at does not
// admit anyone (the query requires starts_at <= now).
//
// Waiting-room-ON rooms transition out of 'scheduled' at start but keep everyone
// waiting for manual admit.
// ---------------------------------------------------------------------------

pub fn spawn_start_poller(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = poll_starts(&state).await {
                tracing::debug!("[poller] Start poll error: {}", e);
            }
        }
    });
}

pub async fn poll_starts(state: &Arc<AppState>) -> Result<(), Box<dyn std::error::Error>> {
    // Rooms whose scheduled start has passed and still need action: either a
    // 'scheduled' -> 'pending' flip, or (waiting room off) held viewers to
    // release. The EXISTS branch also catches rooms the webhook already flipped
    // to 'live'. Once a room's holds are cleared it stops matching, so this
    // stays bounded and idempotent.
    let started_rooms: Vec<(String, String, String)> = {
        let db = state.db.get()?;
        let mut stmt = db.prepare(
            "SELECT r.id, r.slug, r.status FROM rooms r \
             WHERE r.starts_at IS NOT NULL \
             AND r.starts_at <= CURRENT_TIMESTAMP \
             AND r.status != 'ended' \
             AND ( \
                 r.status = 'scheduled' \
                 OR (r.waiting_room = 0 AND EXISTS ( \
                     SELECT 1 FROM participants p \
                     WHERE p.room_id = r.id \
                     AND p.is_admitted = 0 AND p.is_kicked = 0)) \
             )",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if started_rooms.is_empty() {
        return Ok(());
    }

    for (id, slug, status) in started_rooms {
        {
            let db = state.db.get()?;
            // Time-based flip only: leave a webhook-set 'live' status alone.
            if status == "scheduled" {
                db.execute(
                    "UPDATE rooms SET status = 'pending' WHERE id = ?1 AND status = 'scheduled'",
                    rusqlite::params![id],
                )?;
            }
            // Release holds only when the waiting room is off; the subquery gate
            // avoids threading waiting_room through Rust and stays idempotent.
            db.execute(
                "UPDATE participants SET is_admitted = 1 \
                 WHERE room_id = ?1 AND is_kicked = 0 \
                 AND (SELECT waiting_room FROM rooms WHERE id = ?1) = 0",
                rusqlite::params![id],
            )?;
        }

        // Refresh connected presenters' moderation view (waiting -> admitted).
        let _ = state
            .events
            .moderation_changed
            .send(crate::events::ModerationChangedEvent { slug: slug.clone() });
        tracing::info!("[poller] Room {} reached start -> holds released", id);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Room-Ended File Cleanup -- immediate cleanup when a room ends
// ---------------------------------------------------------------------------

pub fn spawn_room_ended_cleanup(state: Arc<AppState>) {
    let mut rx = state.events.room_ended.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(slug) => {
                    if let Err(e) = cleanup_room_files(&state, &slug).await {
                        tracing::debug!("[files] Room ended cleanup error for {}: {}", slug, e);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("[files] Room ended cleanup lagged by {} events", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Drops this room's `room_files` assignments and deletes `session_files` rows
/// that originated here AND aren't still assigned to another room via
/// `room_files` (library protection). Blobs on disk are removed only after
/// confirming no other row shares the `stored_path` (dedup protection).
pub async fn cleanup_room_files(
    state: &Arc<AppState>,
    slug: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let db = state.db.get()?;
    let slug_owned = slug.to_string();
    let data_path = state.config.data_path.clone();

    let stored_paths: Vec<String> =
        tokio::task::spawn_blocking(move || -> Result<Vec<String>, rusqlite::Error> {
            let room_id: Option<String> = db
                .query_row(
                    "SELECT id FROM rooms WHERE slug = ?1",
                    rusqlite::params![slug_owned],
                    |row| row.get(0),
                )
                .ok();

            let room_id = match room_id {
                Some(id) => id,
                None => return Ok(Vec::new()),
            };

            // Unassign every file this room had (drop junction rows).
            db.execute(
                "DELETE FROM room_files WHERE room_id = ?1",
                rusqlite::params![room_id],
            )?;

            // session_files rows originating in this room AND not assigned
            // elsewhere via room_files are safe to delete.
            let orphans: Vec<(String, String)> = {
                let mut stmt = db.prepare(
                    "SELECT sf.id, sf.stored_path FROM session_files sf \
                     WHERE sf.room_id = ?1 \
                     AND NOT EXISTS (SELECT 1 FROM room_files rf WHERE rf.file_id = sf.id)",
                )?;
                let rows: Vec<(String, String)> = stmt
                    .query_map(rusqlite::params![room_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            };

            let mut safe_paths = Vec::with_capacity(orphans.len());
            for (id, stored) in &orphans {
                db.execute(
                    "DELETE FROM session_files WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                let still_refs: i64 = db
                    .query_row(
                        "SELECT COUNT(*) FROM session_files WHERE stored_path = ?1",
                        rusqlite::params![stored],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if still_refs == 0 {
                    safe_paths.push(stored.clone());
                }
            }
            Ok(safe_paths)
        })
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })??;

    let mut removed = 0u64;
    for stored in &stored_paths {
        let full_path = format!("{}/files/{}", data_path, stored);
        match tokio::fs::remove_file(&full_path).await {
            Ok(_) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::debug!("[files] Failed to delete {}: {}", full_path, e);
            }
        }
    }

    if !stored_paths.is_empty() {
        tracing::info!(
            "[files] Room {} ended: removed {} blob(s), {} DB row(s)",
            slug,
            removed,
            stored_paths.len()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Weekly File Cleanup -- 60s initial delay, then every 7 days
// Removes files from disk for ended/expired rooms.
// ---------------------------------------------------------------------------

pub fn spawn_weekly_cleanup(state: Arc<AppState>) {
    tokio::spawn(async move {
        // Initial delay before first run
        time::sleep(Duration::from_secs(60)).await;

        let interval_duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 days
        let mut interval = time::interval(interval_duration);

        loop {
            interval.tick().await;
            if let Err(e) = cleanup_files(&state).await {
                tracing::debug!("[cleanup] Weekly file cleanup error: {}", e);
            }
        }
    });
}

/// Library-aware weekly sweep: only deletes `session_files` rows whose
/// originating room is ended/expired AND which aren't assigned to any other
/// room via `room_files`. Blobs are removed only if no surviving row shares
/// their `stored_path` (dedup protection).
async fn cleanup_files(state: &Arc<AppState>) -> Result<(), Box<dyn std::error::Error>> {
    let data_path = state.config.data_path.clone();
    let db = state.db.get()?;

    let stored_paths: Vec<String> =
        tokio::task::spawn_blocking(move || -> Result<Vec<String>, rusqlite::Error> {
            let orphans: Vec<(String, String)> = {
                let mut stmt = db.prepare(
                    "SELECT sf.id, sf.stored_path \
                     FROM session_files sf \
                     JOIN rooms r ON r.id = sf.room_id \
                     WHERE (r.status = 'ended' \
                            OR (r.expires_at IS NOT NULL AND r.expires_at < CURRENT_TIMESTAMP)) \
                     AND NOT EXISTS (SELECT 1 FROM room_files rf WHERE rf.file_id = sf.id)",
                )?;
                let rows: Vec<(String, String)> = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            };

            if orphans.is_empty() {
                return Ok(Vec::new());
            }

            let mut safe_paths = Vec::with_capacity(orphans.len());
            for (id, stored) in &orphans {
                db.execute(
                    "DELETE FROM session_files WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                let still_refs: i64 = db
                    .query_row(
                        "SELECT COUNT(*) FROM session_files WHERE stored_path = ?1",
                        rusqlite::params![stored],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if still_refs == 0 {
                    safe_paths.push(stored.clone());
                }
            }
            Ok(safe_paths)
        })
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })??;

    if stored_paths.is_empty() {
        tracing::info!("[cleanup] No files to clean up");
        return Ok(());
    }

    tracing::info!("[cleanup] Cleaning up {} files", stored_paths.len());
    let mut deleted_count = 0u64;
    for stored in &stored_paths {
        let full_path = format!("{}/files/{}", data_path, stored);
        match tokio::fs::remove_file(&full_path).await {
            Ok(_) => deleted_count += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::debug!("[cleanup] Failed to delete {}: {}", full_path, e),
        }
    }

    tracing::info!(
        "[cleanup] Deleted {} blobs from disk, {} DB rows removed",
        deleted_count,
        stored_paths.len()
    );
    Ok(())
}
