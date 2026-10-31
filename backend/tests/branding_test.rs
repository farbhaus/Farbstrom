mod common;

use axum::http::header;
use serde_json::Value;

fn auth_val(token: &str) -> axum::http::HeaderValue {
    format!("Bearer {}", token).parse().unwrap()
}

#[tokio::test]
async fn branding_status_no_files() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/branding").await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["hasLogo"], false);
    assert_eq!(body["hasBg"], false);
}

#[tokio::test]
async fn branding_get_logo_404_when_missing() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/branding/logo").await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn branding_get_bg_404_when_missing() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/branding/bg").await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn branding_get_invalid_asset_404() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/branding/invalid").await;
    assert_eq!(res.status_code(), 404);
}

#[tokio::test]
async fn branding_upload_requires_auth() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.post("/api/admin/branding/logo").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn branding_delete_requires_auth() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.delete("/api/admin/branding/logo").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn branding_delete_with_auth_no_file() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .delete("/api/admin/branding/logo")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn branding_delete_invalid_asset_with_auth() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .delete("/api/admin/branding/invalid")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 400);
}

async fn upload_bg(
    server: &axum_test::TestServer,
    token: &str,
    size: usize,
) -> axum_test::TestResponse {
    use axum_test::multipart::{MultipartForm, Part};
    server
        .post("/api/admin/branding/bg")
        .add_header(header::AUTHORIZATION, auth_val(token))
        .multipart(
            MultipartForm::new().add_part(
                "file",
                Part::bytes(vec![0xffu8; size])
                    .file_name("bg.jpg")
                    .mime_type("image/jpeg"),
            ),
        )
        .await
}

/// The route had axum's 2 MB default body limit, so a full-resolution
/// background failed — as "Error parsing multipart/form-data request". It has
/// its own 10 MB limit now, and going over it says so.
#[tokio::test]
async fn backgrounds_up_to_ten_megabytes_upload_and_larger_ones_say_why_not() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let three_mb = upload_bg(&server, &token, 3 * 1024 * 1024).await;
    assert_eq!(three_mb.status_code(), 200, "{}", three_mb.text());

    let eleven_mb = upload_bg(&server, &token, 11 * 1024 * 1024).await;
    assert_eq!(eleven_mb.status_code(), 413);
    assert_eq!(
        eleven_mb.json::<Value>()["error"],
        "Image too large (max 10 MB)"
    );

    // Past the route's body limit (the cap plus the drain allowance) the request
    // is cut off rather than read to the end — and still says why.
    let huge = upload_bg(&server, &token, 80 * 1024 * 1024).await;
    assert_eq!(huge.status_code(), 413);
    assert_eq!(huge.json::<Value>()["error"], "Image too large (max 10 MB)");
}
