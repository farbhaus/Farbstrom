mod common;

use axum::http::header;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn auth_header(token: &str) -> (axum::http::HeaderName, axum::http::HeaderValue) {
    (
        header::AUTHORIZATION,
        format!("Bearer {}", token).parse().unwrap(),
    )
}

fn seed_scheduled_room(
    state: &std::sync::Arc<stream_backend::state::AppState>,
    name: &str,
    slug: &str,
    starts_at: &str,
) -> String {
    let conn = state.db.get().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let presenter_key: String = (0..16)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    conn.execute(
        "INSERT INTO rooms (id, name, slug, presenter_key, delivery_mode, waiting_room, status, starts_at) \
         VALUES (?1, ?2, ?3, ?4, 'webrtc', 0, 'scheduled', ?5)",
        rusqlite::params![id, name, slug, presenter_key, starts_at],
    )
    .unwrap();
    id
}

fn participant_admitted(
    state: &std::sync::Arc<stream_backend::state::AppState>,
    participant_id: &str,
) -> i32 {
    let conn = state.db.get().unwrap();
    conn.query_row(
        "SELECT is_admitted FROM participants WHERE id = ?1",
        rusqlite::params![participant_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn room_status(state: &std::sync::Arc<stream_backend::state::AppState>, room_id: &str) -> String {
    let conn = state.db.get().unwrap();
    conn.query_row(
        "SELECT status FROM rooms WHERE id = ?1",
        rusqlite::params![room_id],
        |row| row.get(0),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Join gating (issue #200)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scheduled_room_holds_viewer() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    seed_scheduled_room(&state, "Sched", "sched-hold-abc123", "2099-12-31 23:59:59");

    let res = server
        .post("/api/public/rooms/sched-hold-abc123/join")
        .json(&json!({ "name": "Alice" }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    // Held: not admitted, told the room is scheduled with its start time.
    assert_eq!(body["admitted"], false);
    assert_eq!(body["status"], "scheduled");
    assert_eq!(body["starts_at"], "2099-12-31 23:59:59");
}

#[tokio::test]
async fn scheduled_room_admits_presenter() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let room_id = seed_scheduled_room(&state, "Sched", "sched-host-abc123", "2099-12-31 23:59:59");
    let pk = common::get_room_presenter_key(&state, &room_id);

    let res = server
        .post("/api/public/rooms/sched-host-abc123/join")
        .json(&json!({ "name": "Host", "role": "presenter", "presenter_key": pk }))
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Value = res.json();
    // A host setting up before the scheduled start bypasses the gate.
    assert_eq!(body["role"], "presenter");
    assert_eq!(body["admitted"], true);
}

// ---------------------------------------------------------------------------
// poll_starts (issue #200)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_starts_opens_past_scheduled_room() {
    let state = common::test_state();
    let room_id = seed_scheduled_room(&state, "Sched", "sched-open", "2020-01-01 00:00:00");
    // A held viewer and a kicked one.
    let (held, _) = common::seed_participant(&state, &room_id, "Held", "viewer", false, false);
    let (kicked, _) = common::seed_participant(&state, &room_id, "Kicked", "viewer", false, true);

    stream_backend::tasks::poll_starts(&state).await.unwrap();

    assert_eq!(room_status(&state, &room_id), "pending");
    assert_eq!(
        participant_admitted(&state, &held),
        1,
        "held viewer admitted"
    );
    assert_eq!(
        participant_admitted(&state, &kicked),
        0,
        "kicked viewer stays out"
    );
}

#[tokio::test]
async fn poll_starts_ignores_future_scheduled_room() {
    let state = common::test_state();
    let room_id = seed_scheduled_room(&state, "Future", "sched-future", "2099-12-31 23:59:59");
    let (held, _) = common::seed_participant(&state, &room_id, "Held", "viewer", false, false);

    stream_backend::tasks::poll_starts(&state).await.unwrap();

    assert_eq!(room_status(&state, &room_id), "scheduled");
    assert_eq!(participant_admitted(&state, &held), 0, "still held");
}

// ---------------------------------------------------------------------------
// Roster endpoint (issue #201) — scoped to live presence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn roster_returns_only_present_admitted() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let token = common::admin_token(&state);
    let room_id = common::seed_room(&state, "Roster", "roster-room-abc123");

    // Present + admitted (native SRT viewer, presence via SSE registry).
    let (present, _) = common::seed_participant(&state, &room_id, "Present", "viewer", true, false);
    // Admitted but not connected — must NOT appear (rows persist after leave).
    common::seed_participant(&state, &room_id, "Ghost", "viewer", true, false);
    // Present but kicked — excluded.
    let (blocked, _) = common::seed_participant(&state, &room_id, "Blocked", "viewer", true, true);
    stream_backend::presence::add("roster-room-abc123", &present);
    stream_backend::presence::add("roster-room-abc123", &blocked);

    let (name, val) = auth_header(&token);
    let res = server
        .get(&format!("/api/rooms/{}/roster", room_id))
        .add_header(name, val)
        .await;
    assert_eq!(res.status_code(), 200);

    let body: Vec<Value> = res.json();
    assert_eq!(
        body.len(),
        1,
        "only the present, admitted, non-kicked viewer"
    );
    assert_eq!(body[0]["name"], "Present");
}
