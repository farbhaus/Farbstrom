use crate::livekit::LiveKitClient;
use crate::state::AppState;
use base64::Engine;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

// ---------------------------------------------------------------------------
// OME Poller -- every 30s
// Checks active streams against OME API; resets rooms to 'pending' if their
// stream key is no longer broadcasting.
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
    let mut stmt = db.prepare(
        "SELECT r.id, r.slug, sk.key_token \
         FROM rooms r \
         JOIN stream_keys sk ON sk.id = r.stream_key_id \
         WHERE r.status = 'live'",
    )?;
    let live_rooms: Vec<(String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (id, slug, key_token) in live_rooms {
        if !active_keys.contains(&key_token) {
            db.execute(
                "UPDATE rooms SET status = 'pending' WHERE id = ?1",
                rusqlite::params![id],
            )?;
            let _ = state.events.room_pending.send(slug.clone());
            tracing::info!("[poller] Room {} -> pending (stream dropped)", id);
        }
    }

    Ok(())
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
// room to 'pending' and auto-admits everyone who was held waiting. Mirrors the
// expiry poller. Held viewers hold the admission SSE, which emits "admitted"
// on its next tick once is_admitted flips, so no new event type is needed.
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
    let started_rooms: Vec<(String, String)> = {
        let db = state.db.get()?;
        let mut stmt = db.prepare(
            "SELECT id, slug FROM rooms \
             WHERE starts_at IS NOT NULL \
             AND starts_at <= CURRENT_TIMESTAMP \
             AND status = 'scheduled'",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if started_rooms.is_empty() {
        return Ok(());
    }

    for (id, slug) in started_rooms {
        {
            let db = state.db.get()?;
            db.execute(
                "UPDATE rooms SET status = 'pending' WHERE id = ?1 AND status = 'scheduled'",
                rusqlite::params![id],
            )?;
            db.execute(
                "UPDATE participants SET is_admitted = 1 \
                 WHERE room_id = ?1 AND is_kicked = 0",
                rusqlite::params![id],
            )?;
        }

        // Refresh connected presenters' moderation view (waiting -> admitted).
        let _ = state
            .events
            .moderation_changed
            .send(crate::events::ModerationChangedEvent { slug: slug.clone() });
        tracing::info!(
            "[poller] Room {} started -> pending, participants admitted",
            id
        );
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
