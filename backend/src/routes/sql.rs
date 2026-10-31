//! Shared SQL fragments and row helpers.
//!
//! Three route modules carried byte-identical copies of `row_to_json`, and the
//! full room projection was written out seven times in `rooms.rs` alone — with
//! the column list restated next to it each time. Keeping one copy of each here
//! means a new room column is added in one place rather than seven, and the
//! projection can't drift from the names it is decoded into.

use base64::Engine;
use serde_json::{json, Value};

/// Decode a row into a JSON object using `columns` as the key names.
///
/// `columns` must line up with the SELECT's projection order — that pairing is
/// why the room projection and [`ROOM_COLS`] live side by side below.
pub fn row_to_json(row: &rusqlite::Row, columns: &[&str]) -> rusqlite::Result<Value> {
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

/// The room projection, joined to its stream key. A macro so the `WHERE`
/// variants below can be `concat!`ed at compile time from this single copy.
macro_rules! room_select {
    () => {
        "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, \
         r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
         r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
         r.started_at, r.ended_at, r.presenter_key, \
         (r.password_hash IS NOT NULL AND r.password_hash != '') AS has_password, \
         sk.key_token, sk.playback_id, sk.name as stream_key_name \
         FROM rooms r \
         LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id"
    };
}

/// Key names for the room projection (`room_select!`), in projection order.
///
/// `has_password`, not the hash: the admin UI only ever asks whether a room has
/// a password, and the bcrypt hash used to ride along to the browser with it.
pub const ROOM_COLS: &[&str] = &[
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
    "has_password",
    "key_token",
    "playback_id",
    "stream_key_name",
];

/// The admin-facing room projection for exactly one room, joined to its stream
/// key. Column order must stay in step with [`ROOM_COLS`]. The bind parameter is the room id.
/// This is the form six of the seven original copies used.
pub const ROOM_SELECT_BY_ID: &str = concat!(room_select!(), " WHERE r.id = ?1");

/// The room list for the admin index. Deliberately *not* the single-room
/// projection: it carries an extra `waiting_count` subquery, so it has its own
/// projection and its own column names ([`ROOM_LIST_COLS`]).
pub const ROOM_LIST_SELECT: &str = "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, \
     r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
     r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
     r.started_at, r.ended_at, r.presenter_key, \
     (r.password_hash IS NOT NULL AND r.password_hash != '') AS has_password, \
     (SELECT COUNT(*) FROM participants p \
      WHERE p.room_id = r.id AND p.is_admitted = 0 AND p.is_kicked = 0) as waiting_count, \
     sk.key_token, sk.playback_id, sk.name as stream_key_name \
     FROM rooms r \
     LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
     ORDER BY r.created_at DESC";

/// Key names for [`ROOM_LIST_SELECT`]. Same as [`ROOM_COLS`] with
/// `waiting_count` inserted before the stream-key columns.
pub const ROOM_LIST_COLS: &[&str] = &[
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
    "has_password",
    "waiting_count",
    "key_token",
    "playback_id",
    "stream_key_name",
];

/// Rooms a file is visible in — directly via `session_files.room_id`, or
/// assigned via the `room_files` junction. Bind parameter is the file id.
///
/// Written out three times in `admin_files.rs` before this.
pub const FILE_ROOM_SLUGS: &str = "SELECT DISTINCT r.slug FROM rooms r \
     WHERE r.id IN (SELECT room_id FROM session_files WHERE id = ?1 AND room_id IS NOT NULL) \
        OR r.id IN (SELECT room_id FROM room_files WHERE file_id = ?1)";

#[cfg(test)]
mod tests {
    use super::*;

    /// Top-level commas in a projection — the ones that separate columns, not
    /// the ones inside a function call or subquery.
    fn projected_columns(select: &str) -> usize {
        let list = select
            .strip_prefix("SELECT ")
            .unwrap()
            .split(" FROM rooms r")
            .next()
            .unwrap();
        let mut depth = 0i32;
        let mut columns = 1;
        for c in list.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => columns += 1,
                _ => {}
            }
        }
        columns
    }

    /// The projection and the column names are decoded positionally, so a
    /// mismatch would silently mislabel every field.
    #[test]
    fn room_projection_matches_its_column_names() {
        assert_eq!(
            projected_columns(ROOM_SELECT_BY_ID),
            ROOM_COLS.len(),
            "ROOM_SELECT_BY_ID projects a different number of columns than ROOM_COLS names"
        );
        assert!(ROOM_SELECT_BY_ID.ends_with(" WHERE r.id = ?1"));
    }

    #[test]
    fn list_projection_matches_its_column_names() {
        assert_eq!(projected_columns(ROOM_LIST_SELECT), ROOM_LIST_COLS.len());
    }

    /// Neither admin projection may carry the room password's hash.
    #[test]
    fn no_projection_sends_the_password_hash() {
        for (select, cols) in [
            (ROOM_SELECT_BY_ID, ROOM_COLS),
            (ROOM_LIST_SELECT, ROOM_LIST_COLS),
        ] {
            assert!(!cols.contains(&"password_hash"));
            assert!(!select.contains("r.password_hash,"));
        }
    }
}
