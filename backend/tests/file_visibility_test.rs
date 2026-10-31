//! Draft visibility, shared-row blast radius, upload limits, and delete
//! accounting.
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

/// The admin library can link one `session_files` row into several rooms. A host
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

// ---------------------------------------------------------------------------
// Participant uploads
// ---------------------------------------------------------------------------

async fn upload(
    server: &axum_test::TestServer,
    slug: &str,
    pid: &str,
    tok: &str,
    query: &str,
    bytes: &'static [u8],
) -> axum_test::TestResponse {
    use axum_test::multipart::{MultipartForm, Part};
    server
        .post(&format!(
            "/api/public/rooms/{slug}/files?participantId={pid}&token={tok}{query}"
        ))
        .multipart(
            MultipartForm::new().add_part(
                "file",
                Part::bytes(bytes)
                    .file_name("same.png")
                    .mime_type("image/png"),
            ),
        )
        .await
}

/// Every participant upload is a draft until the uploader sends it, whatever
/// the request says. The immediate-share mode this replaced de-duplicated by
/// content: a guest in room B who uploaded room A's bytes got room A's row back
/// — its filename and uploader — and the file joined room B's list.
#[tokio::test]
async fn identical_bytes_from_another_room_reveal_nothing() {
    let state = common::test_state();
    let room_a = common::seed_room(&state, "A", "upload-a");
    let room_b = common::seed_room(&state, "B", "upload-b");
    let (pa, ta) = common::seed_participant(&state, &room_a, "Alice", "viewer", true, false);
    let (pb, tb) = common::seed_participant(&state, &room_b, "Bob", "viewer", true, false);
    seed_file(
        &state,
        "a-original",
        Some(&room_a),
        Some(&pa),
        "secret-cut.png",
        1,
        None,
    );
    let server = common::test_app(state.clone());

    // `defer=false` used to select the immediate-share path.
    let res = upload(&server, "upload-b", &pb, &tb, "&defer=false", b"same bytes").await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_ne!(body["id"], "a-original");
    assert_eq!(body["name"], "same.png");

    let (shared, room): (i64, String) = state
        .db
        .get()
        .unwrap()
        .query_row(
            "SELECT is_shared, room_id FROM session_files WHERE id = ?1",
            [body["id"].as_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(shared, 0, "an upload must stay a draft until it is sent");
    assert_eq!(room, room_b);

    // Room B's list holds nothing of room A's.
    let list: Vec<Value> = server
        .get(&format!(
            "/api/public/rooms/upload-b/files?participantId={pb}&token={tb}"
        ))
        .await
        .json();
    assert!(list.is_empty(), "room B sees: {list:?}");
    let _ = ta;
}

/// A refusal sent before the body is read reaches the client only if the body
/// is read first (`uploads::drain`) — but never more than the drain allowance of
/// it: a body past that is refused without waiting for the rest.
#[tokio::test]
async fn a_refused_upload_larger_than_the_drain_allowance_still_answers() {
    use axum_test::multipart::{MultipartForm, Part};
    let state = common::test_state();
    let room = common::seed_room(&state, "Over", "upload-over-big");
    let (pid, tok) = common::seed_participant(&state, &room, "Ana", "viewer", true, false);
    state
        .db
        .get()
        .unwrap()
        .execute("UPDATE rooms SET status = 'ended' WHERE id = ?1", [&room])
        .unwrap();
    let server = common::test_app(state);
    let big = vec![7u8; 70 * 1024 * 1024];
    let res = server
        .post(&format!(
            "/api/public/rooms/upload-over-big/files?participantId={pid}&token={tok}"
        ))
        .multipart(MultipartForm::new().add_part(
            "file",
            Part::bytes(big).file_name("big.mov").mime_type("video/mp4"),
        ))
        .await;
    assert_eq!(res.status_code(), 410);
}

/// Uploads used to keep working after a room had ended.
#[tokio::test]
async fn a_room_that_is_over_takes_no_uploads() {
    let state = common::test_state();
    let room = common::seed_room(&state, "Over", "upload-over");
    let (pid, tok) = common::seed_participant(&state, &room, "Ana", "viewer", true, false);
    state
        .db
        .get()
        .unwrap()
        .execute("UPDATE rooms SET status = 'ended' WHERE id = ?1", [&room])
        .unwrap();
    let server = common::test_app(state);

    let res = upload(&server, "upload-over", &pid, &tok, "", b"late").await;
    assert_eq!(res.status_code(), 410);
}

/// A room's uploads stop at `ROOM_UPLOAD_QUOTA`, counting drafts — refused
/// before reading the body when the room is already full, and after it when
/// the new file is what pushes it over.
#[tokio::test]
async fn uploads_stop_at_the_room_quota() {
    use stream_backend::routes::files::ROOM_UPLOAD_QUOTA;
    let state = common::test_state();
    let room = common::seed_room(&state, "Full", "upload-full");
    let (pid, tok) = common::seed_participant(&state, &room, "Ana", "viewer", true, false);
    let server = common::test_app(state.clone());
    let set_used = |bytes: u64| {
        state
            .db
            .get()
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO session_files \
                 (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, is_shared) \
                 VALUES ('ballast', ?1, NULL, 'b', 'b', 'image/png', ?2, 0)",
                rusqlite::params![room, bytes as i64],
            )
            .unwrap();
    };

    set_used(ROOM_UPLOAD_QUOTA);
    let full = upload(&server, "upload-full", &pid, &tok, "", b"one more").await;
    assert_eq!(full.status_code(), 413);
    assert!(full.json::<Value>()["error"]
        .as_str()
        .unwrap()
        .contains("full"));

    // Room for 3 bytes; the 8-byte file is what tips it over.
    set_used(ROOM_UPLOAD_QUOTA - 3);
    let over = upload(&server, "upload-full", &pid, &tok, "", b"8 bytes!").await;
    assert_eq!(over.status_code(), 413);
    let fits = upload(&server, "upload-full", &pid, &tok, "", b"3by").await;
    assert_eq!(fits.status_code(), 200);

    // The refused upload left no blob behind.
    let blobs = std::fs::read_dir(format!("{}/files", state.config.data_path))
        .unwrap()
        .count();
    assert_eq!(blobs, 1, "only the accepted upload is on disk");
}

// ---------------------------------------------------------------------------
// Admin file links
// ---------------------------------------------------------------------------

/// The library's `<img>`, `<video>` and download URLs used to carry the admin's
/// session JWT as `?token=`, putting a 7- or 90-day credential in browser
/// history. They take a 10-minute file-only token now, and no admin endpoint
/// accepts the session in a query string.
#[tokio::test]
async fn library_links_use_a_file_only_token() {
    let state = common::test_state();
    seed_file(&state, "lib1", None, None, "still.png", 1, Some("h1"));
    let files_dir = format!("{}/files", state.config.data_path);
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(format!("{files_dir}/lib1.png"), b"png").unwrap();
    let session = common::admin_token(&state);
    let server = common::test_app(state.clone());

    // The session no longer works from a URL — anywhere.
    for path in ["/api/admin/files/lib1/preview", "/api/rooms"] {
        let res = server.get(&format!("{path}?token={session}")).await;
        assert_eq!(res.status_code(), 401, "{path} accepted ?token=");
    }

    let link: Value = server
        .get("/api/admin/files/link-token")
        .add_header(header::AUTHORIZATION, auth_val(&session))
        .await
        .json();
    let ft = link["token"].as_str().unwrap().to_string();
    assert_eq!(link["ttlSeconds"], 600);
    for path in ["preview", "download"] {
        let res = server
            .get(&format!("/api/admin/files/lib1/{path}?ft={ft}"))
            .await;
        assert_eq!(res.status_code(), 200, "{path} refused a valid file link");
        assert_eq!(res.as_bytes().as_ref(), b"png");
    }
    // It opens files and nothing else.
    assert_eq!(
        server
            .get(&format!("/api/rooms?ft={ft}"))
            .await
            .status_code(),
        401
    );
    assert_eq!(
        server
            .get("/api/admin/files/link-token")
            .add_header(header::AUTHORIZATION, auth_val(&ft))
            .await
            .status_code(),
        401
    );
}

/// "Sign out other devices" ends outstanding file links with the sessions.
#[tokio::test]
async fn revoking_sessions_ends_file_links() {
    let state = common::test_state();
    seed_file(&state, "lib2", None, None, "still.png", 1, Some("h2"));
    let files_dir = format!("{}/files", state.config.data_path);
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(format!("{files_dir}/lib2.png"), b"png").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let secret = state.config.jwt_secret.clone();
    let ft = stream_backend::auth::mint_file_link_token(&secret, 0, now);
    let expired = stream_backend::auth::mint_file_link_token(&secret, 0, now - 601);
    let server = common::test_app(state.clone());

    let url = |t: &str| format!("/api/admin/files/lib2/preview?ft={t}");
    assert_eq!(server.get(&url(&ft)).await.status_code(), 200);
    assert_eq!(server.get(&url(&expired)).await.status_code(), 401);
    state
        .admin_token_version
        .store(1, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        server.get(&url(&ft)).await.status_code(),
        401,
        "a file link outlived the sessions it was minted under"
    );
}

/// Replacing a library file swaps its contents and keeps its name; the rooms
/// it is assigned to used to be told the *uploaded* file's name, which the
/// database never stored — so it changed back on their next reload.
#[tokio::test]
async fn replacing_a_library_file_announces_the_name_it_keeps() {
    use axum_test::multipart::{MultipartForm, Part};
    let state = common::test_state();
    let room = common::seed_room(&state, "Review", "replace-name");
    seed_file(&state, "lib3", None, None, "Grade v1.png", 1, Some("h3"));
    link_file_to_room(&state, &room, "lib3");
    let files_dir = format!("{}/files", state.config.data_path);
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(format!("{files_dir}/lib3.png"), b"old").unwrap();
    let mut shared = state.events.file_shared.subscribe();
    let token = common::admin_token(&state);
    let server = common::test_app(state.clone());

    let res = server
        .put("/api/admin/files/lib3")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .multipart(
            MultipartForm::new().add_part(
                "file",
                Part::bytes(b"new contents".as_slice())
                    .file_name("export_final_FINAL.png")
                    .mime_type("image/png"),
            ),
        )
        .await;
    assert_eq!(res.status_code(), 200);

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), shared.recv())
        .await
        .expect("no file:shared announcement")
        .unwrap();
    assert_eq!(event.slug, "replace-name");
    assert_eq!(event.name, "Grade v1.png");
    let stored: String = state
        .db
        .get()
        .unwrap()
        .query_row(
            "SELECT original_name FROM session_files WHERE id = 'lib3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, event.name);
}
