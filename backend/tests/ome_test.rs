mod common;

use axum::http::header;

fn auth_val(token: &str) -> axum::http::HeaderValue {
    format!("Bearer {}", token).parse().unwrap()
}

#[tokio::test]
async fn ome_status_returns_401_without_auth() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/ome/status").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn ome_status_returns_502_when_ome_unavailable() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/ome/status")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 502);
}

#[tokio::test]
async fn ome_delete_returns_401_without_auth() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.delete("/api/ome/streams/some-key").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn ome_delete_returns_502_when_ome_unavailable() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .delete("/api/ome/streams/some-key")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 502);
}
