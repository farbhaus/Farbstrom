//! WebSocket hub coverage (`src/ws.rs`).
//!
//! Before this suite the hub had none at all: `common::test_app` builds only
//! `routes::build_router`, and the WS routes are merged in `app::build_app`.
//!
//! See the hazard notes in `common/mod.rs` — unique slugs (the hub's room maps
//! are process-global statics), a multi-thread runtime, and a 3 s disconnect
//! grace period.

mod common;

use common::*;
use serde_json::json;
use std::sync::Arc;
use stream_backend::state::AppState;

/// Room + one admitted participant, named so no other test can collide.
fn room_with(state: &Arc<AppState>, prefix: &str, role: &str) -> (String, String, String, String) {
    let slug = unique_slug(prefix);
    let room = seed_room(state, "Room", &slug);
    let (pid, token) = seed_participant(state, &room, "Ana", role, true, false);
    (slug, room, pid, token)
}

// ---------------------------------------------------------------------------
// Auth gate
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn valid_credentials_authenticate() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "auth-ok", "viewer");
    let server = test_app_http(state);
    let _ws = ws_auth(&server, &slug, &pid, &token, None).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_token_is_rejected() {
    let state = test_state();
    let (slug, _room, pid, _token) = room_with(&state, "auth-bad", "viewer");
    let server = test_app_http(state);

    let mut ws = ws_connect(&server, &slug).await;
    ws.send_text(json!({"type": "auth", "participantId": pid, "token": "wrong"}).to_string())
        .await;
    assert_rejected(&mut ws, "error").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unadmitted_participant_is_rejected() {
    let state = test_state();
    let slug = unique_slug("auth-unadmitted");
    let room = seed_room(&state, "Room", &slug);
    let (pid, token) = seed_participant(&state, &room, "Ana", "viewer", false, false);
    let server = test_app_http(state);

    let mut ws = ws_connect(&server, &slug).await;
    ws.send_text(json!({"type": "auth", "participantId": pid, "token": token}).to_string())
        .await;
    assert_rejected(&mut ws, "error").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kicked_participant_gets_the_kicked_frame() {
    let state = test_state();
    let slug = unique_slug("auth-kicked");
    let room = seed_room(&state, "Room", &slug);
    let (pid, token) = seed_participant(&state, &room, "Ana", "viewer", true, true);
    let server = test_app_http(state);

    let mut ws = ws_connect(&server, &slug).await;
    ws.send_text(json!({"type": "auth", "participantId": pid, "token": token}).to_string())
        .await;
    // A kick is distinguishable from a bad token: the client keys its "Removed"
    // screen off this frame specifically.
    assert_rejected(&mut ws, "kicked").await;
}

/// Regression for the audit's C6 change. The room:ended listener force-closes
/// sockets that are already open; nothing stopped a *new* one authenticating
/// against a room that is over.
#[tokio::test(flavor = "multi_thread")]
async fn ended_room_refuses_new_sockets() {
    let state = test_state();
    let (slug, room, pid, token) = room_with(&state, "auth-ended", "viewer");
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "UPDATE rooms SET status = 'ended' WHERE id = ?1",
            rusqlite::params![room],
        )
        .unwrap();
    }
    let server = test_app_http(state);

    let mut ws = ws_connect(&server, &slug).await;
    ws.send_text(json!({"type": "auth", "participantId": pid, "token": token}).to_string())
        .await;
    assert_rejected(&mut ws, "error").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_room_refuses_new_sockets() {
    let state = test_state();
    let (slug, room, pid, token) = room_with(&state, "auth-expired", "viewer");
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "UPDATE rooms SET expires_at = '2000-01-01 00:00:00' WHERE id = ?1",
            rusqlite::params![room],
        )
        .unwrap();
    }
    let server = test_app_http(state);

    let mut ws = ws_connect(&server, &slug).await;
    ws.send_text(json!({"type": "auth", "participantId": pid, "token": token}).to_string())
        .await;
    assert_rejected(&mut ws, "error").await;
}

/// The first frame must be `auth`; anything else is a protocol violation.
#[tokio::test(flavor = "multi_thread")]
async fn first_frame_must_be_auth() {
    let state = test_state();
    let (slug, _room, _pid, _token) = room_with(&state, "auth-first", "viewer");
    let server = test_app_http(state);

    let mut ws = ws_connect(&server, &slug).await;
    ws.send_text(json!({"type": "chat:message", "text": "hi"}).to_string())
        .await;
    let (_frames, close) = ws_drain(&mut ws, 4).await;
    assert_eq!(close, Some(1008), "non-auth first frame should close 1008");
}

// ---------------------------------------------------------------------------
// Kick mid-session (CLAUDE.md recommended test #4)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn kick_event_closes_the_victims_socket() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "kick-live", "viewer");
    stream_backend::ws::spawn_event_listeners(state.clone());
    let server = test_app_http(state.clone());

    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    state
        .events
        .participant_kicked
        .send(stream_backend::events::KickedEvent {
            slug: slug.clone(),
            participant_id: pid.clone(),
        })
        .unwrap();

    assert_rejected(&mut ws, "kicked").await;
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

/// Regression for the audit's C2 change: every timestamp on the wire is epoch
/// **milliseconds**, because the viewer feeds them all to one `new Date(ts)`.
#[tokio::test(flavor = "multi_thread")]
async fn chat_message_broadcasts_with_millisecond_timestamp() {
    let state = test_state();
    let slug = unique_slug("chat-ms");
    let room = seed_room(&state, "Room", &slug);
    let (a_pid, a_tok) = seed_participant(&state, &room, "Ana", "viewer", true, false);
    let (b_pid, b_tok) = seed_participant(&state, &room, "Bo", "viewer", true, false);
    let server = test_app_http(state.clone());

    let mut a = ws_auth(&server, &slug, &a_pid, &a_tok, None).await;
    let mut b = ws_auth(&server, &slug, &b_pid, &b_tok, None).await;

    a.send_text(json!({"type": "chat:message", "text": "hello room"}).to_string())
        .await;

    // Both the sender and the other participant see it.
    for ws in [&mut a, &mut b] {
        let msg = expect_frame(ws, "chat:message").await;
        assert_eq!(msg["text"], "hello room");
        assert_eq!(msg["name"], "Ana");
        let ts = msg["ts"].as_u64().expect("ts must be a number");
        assert!(
            ts > 1_600_000_000_000,
            "ts {ts} looks like seconds, not milliseconds"
        );
    }

    // ...and it was persisted.
    let conn = state.db.get().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE room_id = ?1 AND text = 'hello room'",
            rusqlite::params![room],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "chat message was not persisted");
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_truncates_long_text_and_drops_blank() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "chat-limits", "viewer");
    let server = test_app_http(state);
    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    // Blank is ignored outright, so the long message is the first one back.
    ws.send_text(json!({"type": "chat:message", "text": "   "}).to_string())
        .await;
    let long = "x".repeat(600);
    ws.send_text(json!({"type": "chat:message", "text": long}).to_string())
        .await;

    let msg = expect_frame(&mut ws, "chat:message").await;
    assert_eq!(
        msg["text"].as_str().unwrap().chars().count(),
        500,
        "text should be truncated to 500 chars"
    );
}

/// History replay is the other half of the C2 fix: `created_at` is a UTC
/// datetime string in the DB, and `new Date("YYYY-MM-DD HH:MM:SS")` parses as
/// *local* time — so it must arrive as an integer.
#[tokio::test(flavor = "multi_thread")]
async fn chat_history_replays_as_millisecond_integers_oldest_first() {
    let state = test_state();
    let slug = unique_slug("chat-history");
    let room = seed_room(&state, "Room", &slug);
    let (pid, token) = seed_participant(&state, &room, "Ana", "viewer", true, false);
    {
        let conn = state.db.get().unwrap();
        for (id, text, at) in [
            ("m-old", "first", "2024-01-01 10:00:00"),
            ("m-new", "second", "2024-01-01 11:00:00"),
        ] {
            conn.execute(
                "INSERT INTO chat_messages (id, room_id, name, role, text, created_at) \
                 VALUES (?1, ?2, 'Ana', 'viewer', ?3, ?4)",
                rusqlite::params![id, room, text, at],
            )
            .unwrap();
        }
    }
    let server = test_app_http(state);
    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    let hist = expect_frame(&mut ws, "chat:history").await;
    let msgs = hist["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["text"], "first", "history must be oldest-first");
    assert_eq!(msgs[1]["text"], "second");

    let ts = msgs[0]["ts"]
        .as_u64()
        .expect("history ts must be an integer, not a datetime string");
    // 2024-01-01T10:00:00Z exactly — proves it was read as UTC, not local.
    assert_eq!(ts, 1_704_103_200_000);
}

/// Drafts are files a participant attached but never sent. C4 hid them from
/// every read path; history is one of them.
#[tokio::test(flavor = "multi_thread")]
async fn chat_history_excludes_unsent_drafts() {
    let state = test_state();
    let slug = unique_slug("chat-drafts");
    let room = seed_room(&state, "Room", &slug);
    let (pid, token) = seed_participant(&state, &room, "Ana", "viewer", true, false);
    {
        let conn = state.db.get().unwrap();
        for (id, name, shared) in [("f-sent", "sent.png", 1), ("f-draft", "draft.png", 0)] {
            conn.execute(
                "INSERT INTO session_files \
                 (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, is_shared) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'image/png', 3, ?6)",
                rusqlite::params![id, room, pid, name, format!("{id}.png"), shared],
            )
            .unwrap();
        }
    }
    let server = test_app_http(state);
    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    let hist = expect_frame(&mut ws, "chat:history").await;
    let names: Vec<&str> = hist["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(names.contains(&"sent.png"));
    assert!(
        !names.contains(&"draft.png"),
        "an unsent draft leaked into chat history: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// Roster / the farbplay marker (gh #227)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn roster_marks_farbplay_and_leaves_browsers_null() {
    let state = test_state();
    let slug = unique_slug("roster-mark");
    let room = seed_room(&state, "Room", &slug);
    let (b_pid, b_tok) = seed_participant(&state, &room, "Browser", "viewer", true, false);
    let (f_pid, f_tok) = seed_participant(&state, &room, "Farbplay", "viewer", true, false);
    let server = test_app_http(state);

    let mut browser = ws_auth(&server, &slug, &b_pid, &b_tok, None).await;
    let _fp = ws_auth(&server, &slug, &f_pid, &f_tok, Some("farbplay")).await;

    // The browser socket sees the roster update caused by the second join.
    let roster = loop {
        let f = expect_frame(&mut browser, "participants:update").await;
        if f["participants"].as_array().unwrap().len() == 2 {
            break f;
        }
    };
    for p in roster["participants"].as_array().unwrap() {
        match p["id"].as_str().unwrap() {
            id if id == f_pid => assert_eq!(p["client"], "farbplay"),
            id if id == b_pid => assert!(
                p["client"].is_null(),
                "a browser viewer must not carry a client marker"
            ),
            other => panic!("unexpected participant {other}"),
        }
    }
}

/// `normalize_client` whitelists the marker rather than echoing it — otherwise
/// a browser viewer could hide itself in the host's Farbplay section.
#[tokio::test(flavor = "multi_thread")]
async fn bogus_client_marker_is_normalised_away() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "roster-bogus", "viewer");
    let server = test_app_http(state);

    let mut ws = ws_auth(&server, &slug, &pid, &token, Some("not-a-real-client")).await;
    let roster = expect_frame(&mut ws, "participants:update").await;
    assert!(
        roster["participants"][0]["client"].is_null(),
        "an unrecognised client marker must be dropped"
    );
}

/// The reconnect branch must re-read `client` off the new auth frame.
///
/// It reuses the existing `WsParticipant` rather than inserting a fresh one, so
/// a marker it does not reassign keeps whatever the *previous* connection had —
/// which is the gh #227 failure mode. Reconnecting with a *different* marker
/// than before is what actually pins that line: a farbplay→farbplay reconnect
/// passes either way, because the value it fails to update is already correct.
#[tokio::test(flavor = "multi_thread")]
async fn reconnect_updates_the_client_marker() {
    let state = test_state();
    let slug = unique_slug("roster-remark");
    let room = seed_room(&state, "Room", &slug);
    let (watcher, w_tok) = seed_participant(&state, &room, "Watcher", "viewer", true, false);
    let (sub, sub_tok) = seed_participant(&state, &room, "Switcher", "viewer", true, false);
    let server = test_app_http(state);

    let mut watcher_ws = ws_auth(&server, &slug, &watcher, &w_tok, None).await;

    // First connection is an unmarked browser…
    let browser = ws_auth(&server, &slug, &sub, &sub_tok, None).await;
    browser.close().await;

    // …and the reconnect, well inside the 3 s grace period so the hub takes the
    // reuse branch, claims the farbplay marker.
    let _fp = ws_auth(&server, &slug, &sub, &sub_tok, Some("farbplay")).await;

    let marker = loop {
        let f = expect_frame(&mut watcher_ws, "participants:update").await;
        let ps = f["participants"].as_array().unwrap();
        if ps.len() == 2 {
            if let Some(p) = ps.iter().find(|p| p["id"] == sub.as_str()) {
                if !p["client"].is_null() {
                    break p["client"].clone();
                }
            }
        }
    };
    assert_eq!(
        marker, "farbplay",
        "the reconnect branch did not re-read the client marker (gh #227)"
    );
}

/// The gh #227 scenario end to end: a Farbplay viewer whose socket drops and
/// comes back must still be in the host's Farbplay section, not the browser one.
#[tokio::test(flavor = "multi_thread")]
async fn farbplay_marker_survives_a_reconnect() {
    let state = test_state();
    let slug = unique_slug("roster-reconnect");
    let room = seed_room(&state, "Room", &slug);
    let (watcher, w_tok) = seed_participant(&state, &room, "Watcher", "viewer", true, false);
    let (fp, fp_tok) = seed_participant(&state, &room, "Farbplay", "viewer", true, false);
    let server = test_app_http(state);

    let mut watcher_ws = ws_auth(&server, &slug, &watcher, &w_tok, None).await;
    let fp_ws = ws_auth(&server, &slug, &fp, &fp_tok, Some("farbplay")).await;
    fp_ws.close().await;

    // Reconnect well inside the grace period, so the hub takes the reuse branch.
    let _fp2 = ws_auth(&server, &slug, &fp, &fp_tok, Some("farbplay")).await;

    let roster = loop {
        let f = expect_frame(&mut watcher_ws, "participants:update").await;
        let ps = f["participants"].as_array().unwrap();
        if ps.len() == 2 {
            break f;
        }
    };
    let marker = roster["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == fp.as_str())
        .expect("farbplay viewer missing from roster")["client"]
        .clone();
    assert_eq!(
        marker, "farbplay",
        "reconnect demoted a Farbplay viewer to the browser list (gh #227)"
    );
}

// ---------------------------------------------------------------------------
// Presenter gating
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn presenter_focus_broadcasts_viewer_focus_is_ignored() {
    let state = test_state();
    let slug = unique_slug("focus");
    let room = seed_room(&state, "Room", &slug);
    let (host, host_tok) = seed_participant(&state, &room, "Host", "presenter", true, false);
    let (viewer, viewer_tok) = seed_participant(&state, &room, "Ana", "viewer", true, false);
    let server = test_app_http(state);

    let mut host_ws = ws_auth(&server, &slug, &host, &host_tok, None).await;
    let mut viewer_ws = ws_auth(&server, &slug, &viewer, &viewer_tok, None).await;

    // A viewer's pin is local-only and must never reach the server's state.
    viewer_ws
        .send_text(json!({"type": "focus:set", "tileId": "stream"}).to_string())
        .await;
    expect_no_frame(&mut host_ws, "focus:set", 4).await;

    // The presenter's does broadcast.
    host_ws
        .send_text(json!({"type": "focus:set", "tileId": "stream"}).to_string())
        .await;
    let f = expect_frame(&mut viewer_ws, "focus:set").await;
    assert_eq!(f["tileId"], "stream");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_tile_id_is_ignored() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "focus-bogus", "presenter");
    let server = test_app_http(state);
    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    ws.send_text(json!({"type": "focus:set", "tileId": "not-a-tile"}).to_string())
        .await;
    expect_no_frame(&mut ws, "focus:set", 3).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn display_transport_without_a_display_is_a_noop() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "display-noop", "presenter");
    let server = test_app_http(state);
    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    ws.send_text(
        json!({"type": "display:transport", "playing": true, "position": 1.0}).to_string(),
    )
    .await;
    expect_no_frame(&mut ws, "display:state", 3).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn presenter_can_display_a_room_file_and_drive_transport() {
    let state = test_state();
    let slug = unique_slug("display");
    let room = seed_room(&state, "Room", &slug);
    let (host, host_tok) = seed_participant(&state, &room, "Host", "presenter", true, false);
    let (viewer, viewer_tok) = seed_participant(&state, &room, "Ana", "viewer", true, false);
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "INSERT INTO session_files \
             (id, room_id, uploader_id, original_name, stored_path, mime_type, size_bytes, is_shared) \
             VALUES ('vid', ?1, ?2, 'clip.mp4', 'vid.mp4', 'video/mp4', 42, 1)",
            rusqlite::params![room, host],
        )
        .unwrap();
    }
    let server = test_app_http(state);

    let mut host_ws = ws_auth(&server, &slug, &host, &host_tok, None).await;
    let mut viewer_ws = ws_auth(&server, &slug, &viewer, &viewer_tok, None).await;

    // A viewer cannot drive the room's display.
    viewer_ws
        .send_text(json!({"type": "display:set", "fileId": "vid"}).to_string())
        .await;
    expect_no_frame(&mut host_ws, "display:state", 4).await;

    host_ws
        .send_text(json!({"type": "display:set", "fileId": "vid"}).to_string())
        .await;
    let shown = expect_frame(&mut viewer_ws, "display:state").await;
    assert_eq!(shown["fileId"], "vid");
    assert_eq!(shown["name"], "clip.mp4");
    assert_eq!(shown["mime"], "video/mp4");
    assert_eq!(shown["playing"], false);

    host_ws
        .send_text(
            json!({"type": "display:transport", "playing": true, "position": 12.5}).to_string(),
        )
        .await;
    let moved = loop {
        let f = expect_frame(&mut viewer_ws, "display:state").await;
        if f["playing"] == true {
            break f;
        }
    };
    assert_eq!(moved["position"], 12.5);
}

/// A file that belongs to no room must not be displayable in it.
#[tokio::test(flavor = "multi_thread")]
async fn display_set_rejects_a_file_outside_the_room() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "display-foreign", "presenter");
    {
        let conn = state.db.get().unwrap();
        conn.execute(
            "INSERT INTO session_files \
             (id, room_id, original_name, stored_path, mime_type, size_bytes, is_shared) \
             VALUES ('lib', NULL, 'other.png', 'lib.png', 'image/png', 3, 1)",
            [],
        )
        .unwrap();
    }
    let server = test_app_http(state);
    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    ws.send_text(json!({"type": "display:set", "fileId": "lib"}).to_string())
        .await;
    expect_no_frame(&mut ws, "display:state", 3).await;
}

/// Late joiners land in the same view as everyone else.
#[tokio::test(flavor = "multi_thread")]
async fn late_joiner_receives_the_current_focus() {
    let state = test_state();
    let slug = unique_slug("focus-replay");
    let room = seed_room(&state, "Room", &slug);
    let (host, host_tok) = seed_participant(&state, &room, "Host", "presenter", true, false);
    let (late, late_tok) = seed_participant(&state, &room, "Ana", "viewer", true, false);
    let server = test_app_http(state);

    let mut host_ws = ws_auth(&server, &slug, &host, &host_tok, None).await;
    host_ws
        .send_text(json!({"type": "focus:set", "tileId": "share"}).to_string())
        .await;
    expect_frame(&mut host_ws, "focus:set").await;

    let mut late_ws = ws_auth(&server, &slug, &late, &late_tok, None).await;
    let replayed = expect_frame(&mut late_ws, "focus:set").await;
    assert_eq!(replayed["tileId"], "share");
}

// ---------------------------------------------------------------------------
// moderation:update — waiting/kicked names are presenter-only
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn viewers_never_receive_waiting_or_kicked_names() {
    let state = test_state();
    let slug = unique_slug("moderation");
    let room = seed_room(&state, "Room", &slug);
    let (host, host_tok) = seed_participant(&state, &room, "Host", "presenter", true, false);
    let (viewer, viewer_tok) = seed_participant(&state, &room, "Ana", "viewer", true, false);
    seed_participant(&state, &room, "Waiting Wendy", "viewer", false, false);
    seed_participant(&state, &room, "Kicked Kim", "viewer", true, true);

    stream_backend::ws::spawn_event_listeners(state.clone());
    let server = test_app_http(state.clone());

    let mut host_ws = ws_auth(&server, &slug, &host, &host_tok, None).await;
    let mut viewer_ws = ws_auth(&server, &slug, &viewer, &viewer_tok, None).await;

    state
        .events
        .moderation_changed
        .send(stream_backend::events::ModerationChangedEvent { slug: slug.clone() })
        .unwrap();

    let host_view = expect_frame(&mut host_ws, "moderation:update").await;
    let waiting: Vec<&str> = host_view["waiting"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    let kicked: Vec<&str> = host_view["kicked"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(waiting, vec!["Waiting Wendy"]);
    assert_eq!(kicked, vec!["Kicked Kim"]);

    let viewer_view = expect_frame(&mut viewer_ws, "moderation:update").await;
    assert!(
        viewer_view["waiting"].as_array().unwrap().is_empty(),
        "a viewer was sent the waiting list: {viewer_view}"
    );
    assert!(
        viewer_view["kicked"].as_array().unwrap().is_empty(),
        "a viewer was sent the kicked list: {viewer_view}"
    );
}

// ---------------------------------------------------------------------------
// Application-level ping (gh #40)
// ---------------------------------------------------------------------------

/// The client's own clock reading is echoed back untouched, so the RTT
/// measurement never depends on the two clocks agreeing.
#[tokio::test(flavor = "multi_thread")]
async fn ping_echoes_the_clients_clock_reading() {
    let state = test_state();
    let (slug, _room, pid, token) = room_with(&state, "ping", "viewer");
    let server = test_app_http(state);
    let mut ws = ws_auth(&server, &slug, &pid, &token, None).await;

    ws.send_text(json!({"type": "ping", "t": 1234567u64}).to_string())
        .await;
    let pong = expect_frame(&mut ws, "pong").await;
    assert_eq!(pong["t"], 1234567u64);
}
