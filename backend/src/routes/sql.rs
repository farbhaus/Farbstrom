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
         r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
         sk.key_token, sk.name as stream_key_name \
         FROM rooms r \
         LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id"
    };
}

/// The admin-facing room projection, joined to its stream key.
///
/// Callers append their own `WHERE`. Column order must stay in step with
/// [`ROOM_COLS`].
pub const ROOM_SELECT: &str = room_select!();

/// Key names for [`ROOM_SELECT`], in projection order.
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
    "password_hash",
    "key_token",
    "stream_key_name",
];

/// [`ROOM_SELECT`] for exactly one room. The bind parameter is the room id.
/// This is the form six of the seven original copies used.
pub const ROOM_SELECT_BY_ID: &str = concat!(room_select!(), " WHERE r.id = ?1");

/// The room list for the admin index. Deliberately *not* [`ROOM_SELECT`]: it
/// carries an extra `waiting_count` subquery, so it has its own projection and
/// its own column names ([`ROOM_LIST_COLS`]).
pub const ROOM_LIST_SELECT: &str = "SELECT r.id, r.name, r.slug, r.delivery_mode, r.waiting_room, \
     r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
     r.starts_at, r.expires_at, r.status, r.stream_key_id, r.created_at, \
     r.started_at, r.ended_at, r.presenter_key, r.password_hash, \
     (SELECT COUNT(*) FROM participants p \
      WHERE p.room_id = r.id AND p.is_admitted = 0 AND p.is_kicked = 0) as waiting_count, \
     sk.key_token, sk.name as stream_key_name \
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
    "password_hash",
    "waiting_count",
    "key_token",
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

    /// The projection and the column names are decoded positionally, so a
    /// mismatch would silently mislabel every field.
    #[test]
    fn room_projection_matches_its_column_names() {
        let select_list = ROOM_SELECT
            .strip_prefix("SELECT ")
            .unwrap()
            .split(" FROM ")
            .next()
            .unwrap();
        assert_eq!(
            select_list.split(',').count(),
            ROOM_COLS.len(),
            "ROOM_SELECT projects a different number of columns than ROOM_COLS names"
        );
    }

    #[test]
    fn by_id_variant_shares_the_projection() {
        assert!(
            ROOM_SELECT_BY_ID.starts_with(ROOM_SELECT),
            "ROOM_SELECT_BY_ID has drifted from ROOM_SELECT"
        );
        assert!(ROOM_SELECT_BY_ID.ends_with(" WHERE r.id = ?1"));
    }
}
