//! Draft visibility, dedup blast radius, and delete accounting.
//!
//! `session_files.is_shared = 0` marks a file a participant has attached to
//! their chat composer but not yet sent. `schema.sql` states drafts are hidden
//! "from chat history, the Files side panel, and the admin library" — these
//! tests hold every read path to that claim, and cover the delete paths that
//! decide whether one room's action can reach another room's files.

mod common;

use axum::http::header;
use serde_json::Value;

fn auth_val(token: &str) -> axum::http::HeaderValue {
    format!("Bearer {}", token).parse().unwrap()
}

/// Insert a `session_files` row directly. Going through the HTTP upload would
/// need multipart plumbing for what is really a fixture.
fn seed_file(
    state: &std::sync::Arc<stream_backend::state::AppState>,
    id: &str,
    room_id: Option<&str>,
    uploader_id: Option<&str>,
    name: &str,
    is_shared: i64,
    content_hash: Option<&str>,
) {
    let conn = state.db.get().unwrap();
    conn.execute(
        "INSERT INTO session_files \
         (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, content_hash, is_shared) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'image/png', 3, ?6, ?7)",
        rusqlite::params![id, room_id, uploader_id, name, format!("{id}.png"), content_hash, is_shared],
    )
    .unwrap();
}

fn link_file_to_room(
    state: &std::sync::Arc<stream_backend::state::AppState>,
    room_id: &str,
    file_id: &str,
) {
    let conn = state.db.get().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO room_files (room_id, file_id) VALUES (?1, ?2)",
        rusqlite::params![room_id, file_id],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Drafts must not leak
// ---------------------------------------------------------------------------

/// The admin library lists every `session_files` row; drafts are not library
/// files and `schema.sql` says so explicitly.
#[tokio::test]
async fn admin_library_hides_participant_drafts() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Drafts", "drafts-admin");
    let (pid, _) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);
    seed_file(
        &state,
        "shared1",
        Some(&room_id),
        Some(&pid),
        "sent.png",
        1,
        Some("h-sent"),
    );
    seed_file(
        &state,
        "draft1",
        Some(&room_id),
        Some(&pid),
        "unsent.png",
        0,
        None,
    );

    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/admin/files")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 200);
    let names: Vec<String> = res
        .json::<Vec<Value>>()
        .iter()
        .map(|f| f["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(names.contains(&"sent.png".to_string()));
    assert!(
        !names.contains(&"unsent.png".to_string()),
        "admin library exposed an unsent draft: {names:?}"
    );
}

/// ...and the library's size/count totals must not include them either.
#[tokio::test]
async fn admin_library_stats_exclude_drafts() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Drafts", "drafts-stats");
    let (pid, _) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);
    seed_file(
        &state,
        "shared1",
        Some(&room_id),
        Some(&pid),
        "sent.png",
        1,
        Some("h-sent"),
    );
    seed_file(
        &state,
        "draft1",
        Some(&room_id),
        Some(&pid),
        "unsent.png",
        0,
        None,
    );

    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/admin/files/stats")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 200);
    assert_eq!(
        res.json::<Value>()["totalCount"],
        1,
        "draft counted in library stats"
    );
}

/// `list_files` filters drafts but `download_file` did not, so a room member
/// who guessed a file id could pull another participant's unsent upload.
#[tokio::test]
async fn participants_cannot_download_someone_elses_draft() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Drafts", "drafts-dl");
    let (uploader, _) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);
    let (snooper, snooper_tok) =
        common::seed_participant(&state, &room_id, "Bo", "viewer", true, false);
    seed_file(
        &state,
        "draft1",
        Some(&room_id),
        Some(&uploader),
        "unsent.png",
        0,
        None,
    );
    // The blob MUST exist, or the handler 404s on the missing file and the test
    // passes without ever reaching the authorization check.
    let dir = format!("{}/files", state.config.data_path);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/draft1.png"), b"png").unwrap();

    let server = common::test_app(state);
    let res = server
        .get(&format!(
            "/api/public/rooms/drafts-dl/files/draft1/download?participantId={snooper}&token={snooper_tok}"
        ))
        .await;
    assert_eq!(
        res.status_code(),
        404,
        "another participant's draft was downloadable"
    );
}

/// The uploader still reaches their own draft — that is what the chat composer
/// preview and the draft chip rely on.
#[tokio::test]
async fn uploader_can_still_reach_their_own_draft() {
    let state = common::test_state();
    let room_id = common::seed_room(&state, "Drafts", "drafts-own");
    let (uploader, tok) = common::seed_participant(&state, &room_id, "Ana", "viewer", true, false);
    seed_file(
        &state,
        "draft1",
        Some(&room_id),
        Some(&uploader),
        "unsent.png",
        0,
        None,
    );
    // The blob has to exist for the handler to stream it.
    let dir = format!("{}/files", state.config.data_path);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/draft1.png"), b"png").unwrap();

    let server = common::test_app(state);
    let res = server
        .get(&format!(
            "/api/public/rooms/drafts-own/files/draft1/download?participantId={uploader}&token={tok}"
        ))
        .await;
    assert_eq!(res.status_code(), 200, "uploader lost access to own draft");
}

// ---------------------------------------------------------------------------
// Dedup blast radius
// ---------------------------------------------------------------------------

/// Content-hash dedup makes two rooms share one `session_files` row. A host
/// deleting it in the room it originated from must not vaporise it for the
/// other room — only their own room's link should go.
#[tokio::test]
async fn host_delete_does_not_reach_into_another_room() {
    let state = common::test_state();
    let room_a = common::seed_room(&state, "A", "dedup-a");
    let room_b = common::seed_room(&state, "B", "dedup-b");
    let (host_a, host_a_tok) =
        common::seed_participant(&state, &room_a, "Host A", "presenter", true, false);
    let (up, _) = common::seed_participant(&state, &room_a, "Ana", "viewer", true, false);

    // One row, originating in A, also linked into B — exactly what dedup builds.
    seed_file(
        &state,
        "shared1",
        Some(&room_a),
        Some(&up),
        "a.png",
        1,
        Some("h1"),
    );
    link_file_to_room(&state, &room_a, "shared1");
    link_file_to_room(&state, &room_b, "shared1");

    let server = common::test_app(state.clone());
    let res = server
        .delete(&format!(
            "/api/public/rooms/dedup-a/files/shared1?participantId={host_a}&token={host_a_tok}"
        ))
        .await;
    assert_eq!(res.status_code(), 204);

    let conn = state.db.get().unwrap();
    let still_in_b: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM room_files WHERE room_id = ?1 AND file_id = 'shared1'",
            rusqlite::params![room_b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still_in_b, 1, "room B lost a file room A deleted");

    let row_survives: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_files WHERE id = 'shared1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        row_survives, 1,
        "shared row hard-deleted out from under room B"
    );
}

/// With no other room holding it, the same delete should still fully remove it.
#[tokio::test]
async fn host_delete_removes_a_file_only_their_room_holds() {
    let state = common::test_state();
    let room_a = common::seed_room(&state, "A", "solo-a");
    let (host, host_tok) =
        common::seed_participant(&state, &room_a, "Host", "presenter", true, false);
    let (up, _) = common::seed_participant(&state, &room_a, "Ana", "viewer", true, false);
    seed_file(
        &state,
        "solo1",
        Some(&room_a),
        Some(&up),
        "a.png",
        1,
        Some("h2"),
    );
    link_file_to_room(&state, &room_a, "solo1");

    let server = common::test_app(state.clone());
    let res = server
        .delete(&format!(
            "/api/public/rooms/solo-a/files/solo1?participantId={host}&token={host_tok}"
        ))
        .await;
    assert_eq!(res.status_code(), 204);

    let conn = state.db.get().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_files WHERE id = 'solo1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "file should be gone when no other room holds it");
}

// ---------------------------------------------------------------------------
// Delete accounting
// ---------------------------------------------------------------------------

/// `deleted` counted notification pairs, so deleting unassigned library files
/// reported 0 and a file in three rooms reported 3.
#[tokio::test]
async fn bulk_delete_counts_files_not_notifications() {
    let state = common::test_state();
    let room = common::seed_room(&state, "R", "bulk-count");
    seed_file(&state, "lib1", None, None, "l1.png", 1, Some("hb1"));
    seed_file(&state, "lib2", None, None, "l2.png", 1, Some("hb2"));
    // lib3 is assigned to a room, so it produces a notification; lib1/lib2 don't.
    seed_file(&state, "lib3", None, None, "l3.png", 1, Some("hb3"));
    link_file_to_room(&state, &room, "lib3");

    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .post("/api/admin/files/bulk-delete")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({ "fileIds": ["lib1", "lib2", "lib3"] }))
        .await;
    assert_eq!(res.status_code(), 200);
    assert_eq!(
        res.json::<Value>()["deleted"],
        3,
        "deleted count should be files removed, not rooms notified"
    );
}

// ---------------------------------------------------------------------------
// Replace with duplicate content
// ---------------------------------------------------------------------------

/// `content_hash` carries a UNIQUE index, and `replace_file` renames the new
/// blob into place *before* the UPDATE. Replacing a library file with bytes
/// that already exist under another row therefore violated the index: a 500,
/// and the freshly written blob orphaned on disk with nothing referencing it.
#[tokio::test]
async fn replace_with_duplicate_content_is_rejected_cleanly() {
    use axum_test::multipart::{MultipartForm, Part};

    // sha256("hello")
    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    let state = common::test_state();
    // `keeper` already owns the hash the replacement will produce.
    seed_file(
        &state,
        "keeper",
        None,
        None,
        "keeper.png",
        1,
        Some(HELLO_SHA256),
    );
    seed_file(
        &state,
        "target",
        None,
        None,
        "target.png",
        1,
        Some("other-hash"),
    );

    let files_dir = format!("{}/files", state.config.data_path);
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(format!("{files_dir}/target.png"), b"original").unwrap();
    let before: Vec<_> = std::fs::read_dir(&files_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();

    let token = common::admin_token(&state);
    let server = common::test_app(state.clone());
    let res = server
        .put("/api/admin/files/target")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .multipart(
            MultipartForm::new().add_part(
                "file",
                Part::bytes(b"hello".as_slice())
                    .file_name("dup.png")
                    .mime_type("image/png"),
            ),
        )
        .await;

    assert_eq!(
        res.status_code(),
        409,
        "duplicate-content replace should be a clean conflict, not a 500"
    );

    // The original row is untouched...
    let conn = state.db.get().unwrap();
    let hash: String = conn
        .query_row(
            "SELECT content_hash FROM session_files WHERE id = 'target'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        hash, "other-hash",
        "target row was mutated by a failed replace"
    );

    // ...and no stray blob was left behind.
    let after: Vec<_> = std::fs::read_dir(&files_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert_eq!(
        after.len(),
        before.len(),
        "a rejected replace orphaned a blob on disk: {before:?} -> {after:?}"
    );
}
