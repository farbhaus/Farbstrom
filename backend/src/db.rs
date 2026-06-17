use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::fs;
use std::path::Path;

pub type DbPool = Pool<SqliteConnectionManager>;

/// Rebuild `session_files` to drop NOT NULL on room_id and add content_hash,
/// then mirror every row with a non-null room_id into room_files so existing
/// uploads keep their original room assignment and also surface in the admin
/// library. Runs only if the `content_hash` column is missing.
fn migrate_session_files_library(conn: &rusqlite::Connection) {
    let has_content_hash: bool = conn
        .prepare("PRAGMA table_info(session_files)")
        .expect("PRAGMA table_info(session_files) failed")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("PRAGMA table_info(session_files) query_map failed")
        .any(|name| name.as_deref() == Ok("content_hash"));

    if has_content_hash {
        return;
    }

    // Disable FK enforcement for the duration of the schema migration — the
    // canonical SQLite pattern for table-level rewrites. Without this, the
    // RENAME ⇒ CREATE ⇒ COPY ⇒ DROP dance can interact badly with any table
    // that already has a FK into session_files (e.g. room_files created by
    // schema.sql immediately before this runs).
    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("Failed to disable foreign_keys for session_files migration");
    let result = conn.execute_batch(
        "BEGIN;

         DROP TABLE IF EXISTS session_files_old;
         ALTER TABLE session_files RENAME TO session_files_old;

         CREATE TABLE session_files (
             id            TEXT PRIMARY KEY,
             room_id       TEXT REFERENCES rooms(id) ON DELETE CASCADE,
             uploader_id   TEXT REFERENCES participants(id) ON DELETE SET NULL,
             original_name TEXT NOT NULL,
             stored_path   TEXT NOT NULL,
             mime_type     TEXT NOT NULL,
             size_bytes    INTEGER NOT NULL,
             content_hash  TEXT,
             created_at    DATETIME DEFAULT CURRENT_TIMESTAMP
         );

         INSERT INTO session_files
             (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, content_hash, created_at)
             SELECT id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, NULL, created_at
             FROM session_files_old;

         DROP TABLE session_files_old;

         CREATE TABLE IF NOT EXISTS room_files (
             room_id     TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
             file_id     TEXT NOT NULL REFERENCES session_files(id) ON DELETE CASCADE,
             assigned_at DATETIME DEFAULT CURRENT_TIMESTAMP,
             PRIMARY KEY (room_id, file_id)
         );

         INSERT OR IGNORE INTO room_files (room_id, file_id, assigned_at)
             SELECT room_id, id, created_at FROM session_files WHERE room_id IS NOT NULL;

         COMMIT;",
    );
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("Failed to re-enable foreign_keys after session_files migration");
    result.expect("Failed to migrate session_files for library support");

    tracing::info!(
        "[migration] session_files: room_id nullable + content_hash + legacy room_files mirror"
    );
}

/// Self-heal room_files if its foreign key still points at any transient
/// session_files rebuild table. SQLite ≥3.25's `ALTER TABLE ... RENAME TO`
/// rewrites every dependent FK to the new name, so any rebuild of
/// `session_files` via the rename ⇒ create ⇒ copy ⇒ drop dance leaves
/// `room_files` pointing at the dropped transient. This runs after every
/// `session_files` migration to repair the breakage.
fn repair_room_files_fk(conn: &rusqlite::Connection) {
    let broken: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='room_files' \
             AND (sql LIKE '%session_files_old%' \
                  OR sql LIKE '%session_files_setnull_old%')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    if !broken {
        return;
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("Failed to disable foreign_keys for room_files FK repair");
    let result = conn.execute_batch(
        "BEGIN;
         ALTER TABLE room_files RENAME TO room_files_bad;
         CREATE TABLE room_files (
             room_id     TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
             file_id     TEXT NOT NULL REFERENCES session_files(id) ON DELETE CASCADE,
             assigned_at DATETIME DEFAULT CURRENT_TIMESTAMP,
             PRIMARY KEY (room_id, file_id)
         );
         INSERT OR IGNORE INTO room_files (room_id, file_id, assigned_at)
             SELECT room_id, file_id, assigned_at FROM room_files_bad
             WHERE file_id IN (SELECT id FROM session_files);
         DROP TABLE room_files_bad;
         CREATE INDEX IF NOT EXISTS idx_room_files_file ON room_files(file_id);
         COMMIT;",
    );
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("Failed to re-enable foreign_keys after room_files FK repair");
    result.expect("Failed to repair room_files foreign key");
    tracing::info!("[migration] room_files FK repaired (was pointing at session_files_old)");
}

/// Change `session_files.room_id` FK from `ON DELETE CASCADE` to
/// `ON DELETE SET NULL`. Without this, deleting a room cascade-wipes
/// session_files rows before `cleanup_room_files` can find them, leaking
/// blobs on disk; and it destroys library attachments for files originating
/// in the deleted room. Detected by inspecting the table DDL in sqlite_master.
fn migrate_session_files_room_id_setnull(conn: &rusqlite::Connection) {
    let needs_migration: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name='session_files' \
             AND sql LIKE '%room_id%REFERENCES rooms(id) ON DELETE CASCADE%'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    if !needs_migration {
        return;
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("Failed to disable foreign_keys for session_files room_id FK migration");
    let result = conn.execute_batch(
        "BEGIN;

         DROP TABLE IF EXISTS session_files_setnull_old;
         ALTER TABLE session_files RENAME TO session_files_setnull_old;

         CREATE TABLE session_files (
             id            TEXT PRIMARY KEY,
             room_id       TEXT REFERENCES rooms(id) ON DELETE SET NULL,
             uploader_id   TEXT REFERENCES participants(id) ON DELETE SET NULL,
             original_name TEXT NOT NULL,
             stored_path   TEXT NOT NULL,
             mime_type     TEXT NOT NULL,
             size_bytes    INTEGER NOT NULL,
             content_hash  TEXT,
             created_at    DATETIME DEFAULT CURRENT_TIMESTAMP
         );

         INSERT INTO session_files
             (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, content_hash, created_at)
             SELECT id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, content_hash, created_at
             FROM session_files_setnull_old;

         DROP TABLE session_files_setnull_old;

         COMMIT;",
    );
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("Failed to re-enable foreign_keys after session_files room_id FK migration");
    result.expect("Failed to migrate session_files.room_id to ON DELETE SET NULL");

    tracing::info!("[migration] session_files.room_id: ON DELETE CASCADE -> ON DELETE SET NULL");
}

/// One-shot: move every blob from `{data_path}/uploads/{room_id}/{file}` to
/// `{data_path}/files/{file}`, rewrite `session_files.stored_path` to just
/// the filename (drop the `{room_id}/` prefix), and backfill `content_hash`
/// by hashing each blob. Gated by a `settings('files_blob_migrated')='1'`
/// sentinel so it runs exactly once per database.
fn migrate_blobs_to_flat_layout(conn: &rusqlite::Connection, data_path: &str) {
    use sha2::{Digest, Sha256};

    let already_done: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'files_blob_migrated'",
            [],
            |row| row.get(0),
        )
        .ok();
    if already_done.as_deref() == Some("1") {
        return;
    }

    let target_dir = format!("{}/files", data_path);
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        tracing::warn!("[migration] failed to create {}: {}", target_dir, e);
        return;
    }

    let mut stmt = match conn.prepare("SELECT id, room_id, stored_path FROM session_files") {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("[migration] prepare failed: {}", e);
            return;
        }
    };

    let rows: Vec<(String, Option<String>, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .and_then(|iter| iter.collect())
        .unwrap_or_default();

    for (id, room_id, stored) in rows {
        // Determine old path: if stored already looks flat (no slash) AND the
        // file already lives in the new /files dir, skip the move step.
        let already_flat =
            !stored.contains('/') && Path::new(&format!("{}/{}", target_dir, stored)).exists();
        let old_path = if already_flat {
            format!("{}/{}", target_dir, stored)
        } else if let Some(rid) = room_id.as_deref() {
            format!("{}/uploads/{}/{}", data_path, rid, stored)
        } else {
            // No room_id and not flat — legacy shouldn't have produced this, skip.
            continue;
        };

        let flat_name: String = stored.rsplit('/').next().unwrap_or(&stored).to_string();
        let new_path = format!("{}/{}", target_dir, flat_name);

        if !Path::new(&old_path).exists() && !Path::new(&new_path).exists() {
            tracing::warn!("[migration] blob missing for {}: {}", id, old_path);
            continue;
        }

        if Path::new(&old_path).exists() && !Path::new(&new_path).exists() {
            if let Err(e) = std::fs::rename(&old_path, &new_path) {
                // rename across devices can fail; fall back to copy+remove
                if std::fs::copy(&old_path, &new_path).is_ok() {
                    let _ = std::fs::remove_file(&old_path);
                } else {
                    tracing::warn!("[migration] rename failed for {}: {}", id, e);
                    continue;
                }
            }
        }

        // Hash the (now-flat) blob
        let hash = match std::fs::read(&new_path) {
            Ok(bytes) => {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                format!("{:x}", hasher.finalize())
            }
            Err(e) => {
                tracing::warn!("[migration] read failed for {}: {}", new_path, e);
                continue;
            }
        };

        // Best-effort update; unique-index collisions (duplicate hashes across
        // old rooms) are handled by leaving content_hash NULL on the loser.
        if let Err(e) = conn.execute(
            "UPDATE session_files SET stored_path = ?1, content_hash = ?2 WHERE id = ?3",
            rusqlite::params![flat_name, hash, id],
        ) {
            tracing::warn!("[migration] update failed for {}: {}", id, e);
            let _ = conn.execute(
                "UPDATE session_files SET stored_path = ?1 WHERE id = ?2",
                rusqlite::params![flat_name, id],
            );
        }
    }

    // Clean up empty per-room upload dirs (best-effort)
    if let Ok(entries) = std::fs::read_dir(format!("{}/uploads", data_path)) {
        for e in entries.flatten() {
            let _ = std::fs::remove_dir(e.path());
        }
        let _ = std::fs::remove_dir(format!("{}/uploads", data_path));
    }

    let _ = conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('files_blob_migrated', '1')",
        [],
    );
    tracing::info!("[migration] blob layout flattened to {}", target_dir);
}

pub fn init_pool(db_path: &str, data_path: &str) -> DbPool {
    let manager = SqliteConnectionManager::file(db_path);
    let pool = Pool::builder()
        .max_size(8)
        .build(manager)
        .expect("Failed to create database pool");

    let conn = pool.get().expect("Failed to get connection");
    conn.execute_batch("PRAGMA journal_mode = WAL;")
        .expect("Failed to set PRAGMA journal_mode = WAL");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("Failed to set PRAGMA foreign_keys = ON");
    conn.execute_batch("PRAGMA synchronous = NORMAL;")
        .expect("Failed to set PRAGMA synchronous = NORMAL");

    // Apply schema
    let schema = fs::read_to_string("schema.sql")
        .or_else(|_| fs::read_to_string("/app/schema.sql"))
        .expect("Failed to read schema.sql");
    conn.execute_batch(&schema)
        .expect("Failed to apply schema.sql");

    // Migrations for existing databases
    let has_stream_key_id: bool = conn
        .prepare("PRAGMA table_info(rooms)")
        .expect("PRAGMA table_info(rooms) failed")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("PRAGMA table_info(rooms) query_map failed")
        .any(|name| name.as_deref() == Ok("stream_key_id"));

    if !has_stream_key_id {
        conn.execute_batch(
            "ALTER TABLE rooms ADD COLUMN stream_key_id TEXT REFERENCES stream_keys(id) ON DELETE SET NULL;
             UPDATE rooms SET stream_key_id = (SELECT id FROM stream_keys WHERE stream_keys.room_id = rooms.id LIMIT 1);
             CREATE INDEX IF NOT EXISTS idx_rooms_stream_key ON rooms(stream_key_id);"
        ).expect("Failed migration: add rooms.stream_key_id");
    }

    let has_is_kicked: bool = conn
        .prepare("PRAGMA table_info(participants)")
        .expect("PRAGMA table_info(participants) failed")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("PRAGMA table_info(participants) query_map failed")
        .any(|name| name.as_deref() == Ok("is_kicked"));

    if !has_is_kicked {
        conn.execute_batch(
            "ALTER TABLE participants ADD COLUMN is_kicked INTEGER NOT NULL DEFAULT 0",
        )
        .expect("Failed migration: add participants.is_kicked");
    }

    let has_presenter_key: bool = conn
        .prepare("PRAGMA table_info(rooms)")
        .expect("PRAGMA table_info(rooms) failed (presenter_key check)")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("PRAGMA table_info(rooms) query_map failed (presenter_key check)")
        .any(|name| name.as_deref() == Ok("presenter_key"));

    if !has_presenter_key {
        conn.execute_batch(
            "ALTER TABLE rooms ADD COLUMN presenter_key TEXT;
             UPDATE rooms SET presenter_key = lower(hex(randomblob(16))) WHERE presenter_key IS NULL;"
        ).expect("Failed migration: add rooms.presenter_key");
    }

    // Per-room participant-audio defaults (issue #160). Default ON to preserve
    // the previous always-on behavior for existing rooms.
    for col in ["noise_reduction", "echo_cancellation"] {
        let has_col: bool = conn
            .prepare("PRAGMA table_info(rooms)")
            .expect("PRAGMA table_info(rooms) failed (audio defaults check)")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("PRAGMA table_info(rooms) query_map failed (audio defaults check)")
            .any(|name| name.as_deref() == Ok(col));
        if !has_col {
            conn.execute_batch(&format!(
                "ALTER TABLE rooms ADD COLUMN {col} INTEGER NOT NULL DEFAULT 1"
            ))
            .unwrap_or_else(|_| panic!("Failed migration: add rooms.{col}"));
        }
    }

    // Per-room push-to-talk default (issue #188). Default OFF (DEFAULT 0) to
    // preserve the always-toggle mic behavior for existing rooms — the inverse
    // of the audio defaults above, so it gets its own block.
    let has_ptt: bool = conn
        .prepare("PRAGMA table_info(rooms)")
        .expect("PRAGMA table_info(rooms) failed (push_to_talk check)")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("PRAGMA table_info(rooms) query_map failed (push_to_talk check)")
        .any(|name| name.as_deref() == Ok("push_to_talk"));
    if !has_ptt {
        conn.execute_batch("ALTER TABLE rooms ADD COLUMN push_to_talk INTEGER NOT NULL DEFAULT 0")
            .expect("Failed migration: add rooms.push_to_talk");
    }

    migrate_session_files_library(&conn);
    repair_room_files_fk(&conn);
    migrate_session_files_room_id_setnull(&conn);
    // Re-run repair: the room_id setnull migration also rewrites FK refs.
    repair_room_files_fk(&conn);

    // is_shared column: drafts are uploaded files not yet posted to chat.
    let has_is_shared: bool = conn
        .prepare("PRAGMA table_info(session_files)")
        .expect("PRAGMA table_info(session_files) failed")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("PRAGMA table_info(session_files) query_map failed")
        .any(|name| name.as_deref() == Ok("is_shared"));
    if !has_is_shared {
        conn.execute_batch(
            "ALTER TABLE session_files ADD COLUMN is_shared INTEGER NOT NULL DEFAULT 1",
        )
        .expect("Failed migration: add session_files.is_shared");
    }

    // Ensure indexes that reference content_hash exist on both fresh + migrated DBs.
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_session_files_hash    ON session_files(content_hash);
         CREATE INDEX        IF NOT EXISTS idx_session_files_room    ON session_files(room_id);
         CREATE INDEX        IF NOT EXISTS idx_session_files_created ON session_files(created_at);
         CREATE INDEX        IF NOT EXISTS idx_room_files_file       ON room_files(file_id);",
    )
    .expect("Failed to create session_files/room_files indexes");

    migrate_blobs_to_flat_layout(&conn, data_path);

    pool
}
