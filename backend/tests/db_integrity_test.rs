//! Pool-level database invariants.
//!
//! Connection-scoped pragmas are per-connection, not per-database, and r2d2's
//! `min_idle` defaults to `max_size` — so all 8 connections exist by the time
//! `build()` returns. A pragma run on one connection checked out afterwards
//! configures only that one.
//!
//! `foreign_keys` and `busy_timeout` survived that mistake because they are on
//! by default here anyway (libsqlite3-sys's bundled build compiles SQLite with
//! `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`; rusqlite calls `sqlite3_busy_timeout(db,
//! 5000)` on open). `synchronous` did not, and 7 of 8 connections ran at `FULL`.
//!
//! These tests pin all three at the pool level, so the guarantee stops depending
//! on a transitive dependency's build flags — dropping the `bundled` feature
//! would otherwise silently turn every `ON DELETE CASCADE` in `schema.sql` into
//! a no-op, and four call sites document themselves as relying on it
//! (`rooms::delete_room`, `stream_keys::delete_key`, `files::delete_room_file`,
//! `admin_files::delete_files_inner`).

mod common;

use common::*;

/// Every connection the pool can hand out must carry the same pragmas — not
/// just whichever one happened to be checked out at init.
#[test]
fn all_pooled_connections_share_the_same_pragmas() {
    let state = test_state();

    // Hold all 8 at once so we genuinely inspect distinct connections rather
    // than the same one returned to the pool and handed back repeatedly.
    let conns: Vec<_> = (0..8).map(|_| state.db.get().unwrap()).collect();

    for (i, conn) in conns.iter().enumerate() {
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "connection {i} has foreign_keys OFF");

        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(busy > 0, "connection {i} has no busy_timeout");

        // 1 == NORMAL. This is the one that was actually only reaching a single
        // connection; the other seven sat at 2 (FULL), fsyncing every commit.
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "connection {i} is not on synchronous = NORMAL");
    }
}

/// Deleting a room must take its children with it, on any connection.
#[test]
fn deleting_a_room_cascades_to_children() {
    let state = test_state();
    let room_id = seed_room(&state, "Cascade Room", "cascade-room");
    let (participant_id, _) = seed_participant(&state, &room_id, "Ana", "viewer", true, false);

    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "INSERT INTO chat_messages (id, room_id, name, role, text) \
             VALUES ('m1', ?1, 'Ana', 'viewer', 'hello')",
            rusqlite::params![room_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_files \
             (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes) \
             VALUES ('f1', ?1, ?2, 'a.png', 'f1.png', 'image/png', 10)",
            rusqlite::params![room_id, participant_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO room_files (room_id, file_id) VALUES (?1, 'f1')",
            rusqlite::params![room_id],
        )
        .unwrap();
    }

    // Exercise every pooled connection rather than one: a per-connection pragma
    // regression would otherwise show up only on whichever connection the test
    // happened to draw.
    for _ in 0..8 {
        let conn = state.db.get().unwrap();
        conn.execute(
            "DELETE FROM rooms WHERE id = ?1",
            rusqlite::params![room_id],
        )
        .unwrap();
    }

    let conn = state.db.get().unwrap();
    let participants: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM participants WHERE room_id = ?1",
            rusqlite::params![room_id],
            |r| r.get(0),
        )
        .unwrap();
    let messages: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE room_id = ?1",
            rusqlite::params![room_id],
            |r| r.get(0),
        )
        .unwrap();
    let links: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM room_files WHERE room_id = ?1",
            rusqlite::params![room_id],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(participants, 0, "participants orphaned by room delete");
    assert_eq!(messages, 0, "chat_messages orphaned by room delete");
    assert_eq!(links, 0, "room_files orphaned by room delete");

    // session_files.room_id is SET NULL, not CASCADE — the row survives as a
    // library file so `cleanup_room_files` can still find its blob.
    let file_room: Option<String> = conn
        .query_row(
            "SELECT room_id FROM session_files WHERE id = 'f1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(file_room, None, "session_files.room_id should be SET NULL");
}

/// The chat-history replay converts `created_at` to epoch milliseconds in SQL.
/// If that expression ever returned NULL — an unparseable stored format, say —
/// the `i64` row read would fail and `send_chat_history`'s `filter_map(|r|
/// r.ok())` would silently drop the message, emptying history with no error.
/// So assert the conversion actually produces a sane number for both the
/// `CURRENT_TIMESTAMP` default and the explicit `datetime(?, 'unixepoch')` form
/// that `files::upload_file` writes.
#[test]
fn created_at_converts_to_epoch_millis() {
    let state = test_state();
    let room_id = seed_room(&state, "Times", "times-room");
    let conn = state.db.get().unwrap();

    // (a) the CURRENT_TIMESTAMP default, as chat_messages uses.
    conn.execute(
        "INSERT INTO chat_messages (id, room_id, name, role, text) \
         VALUES ('m1', ?1, 'Ana', 'viewer', 'hi')",
        rusqlite::params![room_id],
    )
    .unwrap();

    // (b) the explicit unixepoch form, as session_files uses. 1_700_000_000 s
    // == 2023-11-14T22:13:20Z, so we know the exact expected millisecond value.
    conn.execute(
        "INSERT INTO session_files \
         (id, room_id, original_name, stored_path, mime_type, size_bytes, created_at) \
         VALUES ('f1', ?1, 'a.png', 'f1.png', 'image/png', 10, datetime(1700000000, 'unixepoch'))",
        rusqlite::params![room_id],
    )
    .unwrap();

    let chat_ms: i64 = conn
        .query_row(
            "SELECT CAST(strftime('%s', created_at) AS INTEGER) * 1000 \
             FROM chat_messages WHERE id = 'm1'",
            [],
            |r| r.get(0),
        )
        .expect("strftime returned NULL for a CURRENT_TIMESTAMP row");
    assert!(
        chat_ms > 1_600_000_000_000,
        "chat ts {chat_ms} is not epoch milliseconds"
    );

    let file_ms: i64 = conn
        .query_row(
            "SELECT CAST(strftime('%s', created_at) AS INTEGER) * 1000 \
             FROM session_files WHERE id = 'f1'",
            [],
            |r| r.get(0),
        )
        .expect("strftime returned NULL for a datetime(unixepoch) row");
    assert_eq!(
        file_ms, 1_700_000_000_000,
        "created_at must round-trip as UTC, not local time"
    );
}

/// Deleting a stream key must NULL the pointer on any room using it. A dangling
/// `stream_key_id` makes `room_info` report `has_stream_key: 1` while `/join`
/// hands back `stream_key: null`, and pins the room at `live` forever because
/// the reconciler's demote query inner-joins `stream_keys`.
#[test]
fn deleting_a_stream_key_nulls_the_room_pointer() {
    let state = test_state();
    let (key_id, _) = seed_stream_key(&state, "Key");
    let room_id = seed_room_full(&state, "Keyed", "keyed-room", "live", false, Some(&key_id));

    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "DELETE FROM stream_keys WHERE id = ?1",
            rusqlite::params![key_id],
        )
        .unwrap();
    }

    let conn = state.db.get().unwrap();
    let sk: Option<String> = conn
        .query_row(
            "SELECT stream_key_id FROM rooms WHERE id = ?1",
            rusqlite::params![room_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sk, None, "rooms.stream_key_id left dangling");
}
