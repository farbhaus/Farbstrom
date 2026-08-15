mod common;

use axum::http::header;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// A stand-in OME whose "currently broadcasting" list can be changed after the
/// server is up — so a test can build its AppState first, seed a stream key,
/// and only then declare that key live.
type Broadcasting = Arc<Mutex<Vec<String>>>;

async fn fake_ome() -> (String, Broadcasting) {
    let active: Broadcasting = Arc::new(Mutex::new(Vec::new()));
    let handler_state = active.clone();
    let app = axum::Router::new().route(
        "/vhosts/default/apps/live/streams",
        axum::routing::get(move || {
            let active = handler_state.clone();
            async move {
                let names = active.lock().unwrap().clone();
                axum::Json(json!({
                    "statusCode": 200,
                    "message": "OK",
                    "response": names,
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{}", addr), active)
}

/// AppState wired to a fake OME. Returns the handle for declaring keys live.
async fn state_with_fake_ome() -> (Arc<stream_backend::state::AppState>, Broadcasting) {
    let (url, active) = fake_ome().await;
    let mut config = common::test_config();
    config.ome_api_url = url;
    (common::test_state_with_config(config), active)
}

fn auth(token: &str) -> (header::HeaderName, axum::http::HeaderValue) {
    (
        header::AUTHORIZATION,
        format!("Bearer {}", token).parse().unwrap(),
    )
}

// ---------------------------------------------------------------------------
// Instant promotion — gh #225 follow-up. The 30s poller already covers these
// eventually; these assert it happens without waiting for a tick.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attaching_an_already_broadcasting_key_promotes_immediately() {
    let (state, broadcasting) = state_with_fake_ome().await;
    let server = common::test_app(state.clone());
    let (sk_id, key_token) = common::seed_stream_key(&state, "Live Key");
    let room_id = common::seed_room_full(&state, "Room", "promote-attach", "pending", false, None);
    broadcasting.lock().unwrap().push(key_token);

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": sk_id }))
        .await;

    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(
        body["status"], "live",
        "the response itself must report live, not just the DB"
    );
}

#[tokio::test]
async fn creating_a_room_with_an_already_broadcasting_key_starts_live() {
    let (state, broadcasting) = state_with_fake_ome().await;
    let server = common::test_app(state.clone());
    let (sk_id, key_token) = common::seed_stream_key(&state, "Live Key");
    broadcasting.lock().unwrap().push(key_token);

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .post("/api/rooms")
        .add_header(n, v)
        .json(&json!({ "name": "Created Live", "stream_key_id": sk_id }))
        .await;

    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["status"], "live");
}

#[tokio::test]
async fn swapping_to_another_broadcasting_key_promotes_immediately() {
    let (state, broadcasting) = state_with_fake_ome().await;
    let server = common::test_app(state.clone());
    let (old_id, _old_token) = common::seed_stream_key(&state, "Idle");
    let (new_id, new_token) = common::seed_stream_key(&state, "Live");
    let room_id = common::seed_room_full(
        &state,
        "Room",
        "promote-swap",
        "pending",
        false,
        Some(&old_id),
    );
    broadcasting.lock().unwrap().push(new_token);

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": new_id }))
        .await;

    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["status"], "live");
}

/// The kick guard has to hold on the new fast path, not just in the poller.
#[tokio::test]
async fn attaching_a_blocked_key_does_not_promote() {
    let (state, broadcasting) = state_with_fake_ome().await;
    let server = common::test_app(state.clone());
    let (sk_id, key_token) = common::seed_stream_key(&state, "Kicked Key");
    let room_id = common::seed_room_full(&state, "Room", "promote-blocked", "pending", false, None);
    state
        .db
        .get()
        .unwrap()
        .execute(
            "UPDATE stream_keys SET blocked = 1 WHERE id = ?1",
            rusqlite::params![sk_id],
        )
        .unwrap();
    broadcasting.lock().unwrap().push(key_token);

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": sk_id }))
        .await;

    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["status"], "pending");
}

/// An idle key must not promote anything — guards against the fast path
/// promoting purely because a key was attached.
#[tokio::test]
async fn attaching_an_idle_key_leaves_the_room_pending() {
    let (state, _broadcasting) = state_with_fake_ome().await;
    let server = common::test_app(state.clone());
    let (sk_id, _key_token) = common::seed_stream_key(&state, "Idle Key");
    let room_id = common::seed_room_full(&state, "Room", "promote-idle", "pending", false, None);

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": sk_id }))
        .await;

    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["status"], "pending");
}

/// An admin action must never fail because OME hiccuped — the 30s poller is the
/// backstop. `common::test_config` already points at a dead port.
#[tokio::test]
async fn attaching_still_succeeds_when_ome_is_unreachable() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, _key_token) = common::seed_stream_key(&state, "K");
    let room_id = common::seed_room_full(&state, "Room", "promote-no-ome", "pending", false, None);

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": sk_id }))
        .await;

    assert_eq!(res.status_code(), 200, "must not 5xx when OME is down");
    let body: Value = res.json();
    assert_eq!(body["status"], "pending");
    assert_eq!(body["stream_key_id"], sk_id, "the key must still be saved");
}

// ---------------------------------------------------------------------------
// Swap notification — replacing one key with another used to emit nothing, so
// viewers stayed pointed at the old stream and never re-mounted.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn swapping_the_stream_key_notifies_viewers() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (old_id, _old_token) = common::seed_stream_key(&state, "Old");
    let (new_id, new_token) = common::seed_stream_key(&state, "New");
    let room_id =
        common::seed_room_full(&state, "Room", "swap-room", "pending", false, Some(&old_id));

    let mut rx = state.events.stream_key_assigned.subscribe();

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": new_id }))
        .await;
    assert_eq!(res.status_code(), 200);

    let evt = rx.try_recv().expect("a swap must emit stream_key_assigned");
    assert_eq!(evt.slug, "swap-room");
    assert_eq!(evt.stream_key, new_token, "must carry the NEW key token");
}

/// Re-saving the same key is a no-op save from the admin form; it must not make
/// every viewer tear down and re-mount the player.
#[tokio::test]
async fn resaving_the_same_stream_key_emits_nothing() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, _token) = common::seed_stream_key(&state, "K");
    let room_id = common::seed_room_full(
        &state,
        "Room",
        "resave-room",
        "pending",
        false,
        Some(&sk_id),
    );

    let mut rx = state.events.stream_key_assigned.subscribe();

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": sk_id }))
        .await;
    assert_eq!(res.status_code(), 200);

    assert!(
        rx.try_recv().is_err(),
        "re-saving the same key must stay silent"
    );
}

#[tokio::test]
async fn detaching_the_stream_key_still_notifies() {
    let state = common::test_state();
    let server = common::test_app(state.clone());
    let (sk_id, _token) = common::seed_stream_key(&state, "K");
    let room_id =
        common::seed_room_full(&state, "Room", "detach-room", "live", false, Some(&sk_id));

    let mut removed = state.events.stream_key_removed.subscribe();
    let mut pending = state.events.room_pending.subscribe();

    let (n, v) = auth(&common::admin_token(&state));
    let res = server
        .put(&format!("/api/rooms/{}", room_id))
        .add_header(n, v)
        .json(&json!({ "stream_key_id": Value::Null }))
        .await;
    assert_eq!(res.status_code(), 200);

    assert_eq!(removed.try_recv().unwrap(), "detach-room");
    assert_eq!(pending.try_recv().unwrap(), "detach-room");
}
