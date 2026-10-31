mod common;

use serde_json::Value;

#[tokio::test]
async fn login_returns_400_without_password() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server
        .post("/api/auth/login")
        .json(&serde_json::json!({}))
        .await;
    assert_eq!(res.status_code(), 400);
}

#[tokio::test]
async fn login_returns_401_wrong_password() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "wrong"}))
        .await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn login_returns_token_on_correct_password() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "test-admin-password"}))
        .await;
    assert_eq!(res.status_code(), 200);
    let body: Value = res.json();
    assert!(body.get("token").is_some());
}

/// S3 regression: a JWT signed with the same `jwt_secret` but missing
/// `admin: true` must NOT be accepted by the AdminAuth extractor. This
/// guards against any future token type signed with the same secret
/// silently turning into an admin credential.
#[tokio::test]
async fn admin_endpoint_rejects_token_without_admin_claim() {
    use serde::Serialize;

    #[derive(Serialize)]
    struct NonAdminClaims {
        admin: bool,
        exp: u64,
    }

    let state = common::test_state();
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let token = stream_backend::jwt::sign(
        &NonAdminClaims { admin: false, exp },
        state.config.jwt_secret.as_bytes(),
    );

    let server = common::test_app(state);
    let res = server
        .get("/api/rooms")
        .add_header("authorization", format!("Bearer {token}"))
        .await;
    assert_eq!(res.status_code(), 401);
}
