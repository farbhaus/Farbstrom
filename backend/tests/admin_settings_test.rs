mod common;

use axum::http::header;
use serde_json::Value;
use stream_backend::credentials;

fn auth_val(token: &str) -> axum::http::HeaderValue {
    format!("Bearer {}", token).parse().unwrap()
}

#[tokio::test]
async fn settings_status_requires_auth() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/admin/settings/status").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn password_change_then_old_rejected_new_accepted() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let res = server
        .post("/api/admin/settings/password")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({
            "current": "test-admin-password",
            "new": "a-brand-new-strong-password"
        }))
        .await;
    assert_eq!(res.status_code(), 200);

    // Old password no longer works.
    let res = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "test-admin-password"}))
        .await;
    assert_eq!(res.status_code(), 401);

    // New password works.
    let res = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "a-brand-new-strong-password"}))
        .await;
    assert_eq!(res.status_code(), 200);
    assert!(res.json::<Value>().get("token").is_some());
}

#[tokio::test]
async fn password_change_rejects_short_and_wrong_current() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let short = server
        .post("/api/admin/settings/password")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({"current": "test-admin-password", "new": "short"}))
        .await;
    assert_eq!(short.status_code(), 400);

    let wrong = server
        .post("/api/admin/settings/password")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({"current": "nope", "new": "a-long-enough-password"}))
        .await;
    assert_eq!(wrong.status_code(), 401);
}

#[tokio::test]
async fn totp_full_flow_and_recovery_code() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    // Setup → returns a secret.
    let setup = server
        .post("/api/admin/settings/totp/setup")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(setup.status_code(), 200);
    let secret = setup.json::<Value>()["secret"]
        .as_str()
        .unwrap()
        .to_string();

    let totp = credentials::totp_from_secret(&secret).unwrap();
    let code = totp.generate_current().unwrap();

    // Enable with a valid code → returns recovery codes.
    let enable = server
        .post("/api/admin/settings/totp/enable")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({ "code": code }))
        .await;
    assert_eq!(enable.status_code(), 200);
    let recovery: Vec<String> = enable.json::<Value>()["recoveryCodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(recovery.len(), 10);

    // Login without a code → totpRequired, no token.
    let need = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "test-admin-password"}))
        .await;
    assert_eq!(need.status_code(), 200);
    let body = need.json::<Value>();
    assert_eq!(body["totpRequired"], Value::Bool(true));
    assert!(body.get("token").is_none());

    // Login with a valid TOTP code → token.
    let code = totp.generate_current().unwrap();
    let ok = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "test-admin-password", "totp_code": code}))
        .await;
    assert_eq!(ok.status_code(), 200);
    assert!(ok.json::<Value>().get("token").is_some());

    // A recovery code works once...
    let rc = recovery[0].clone();
    let ok = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "test-admin-password", "totp_code": rc}))
        .await;
    assert_eq!(ok.status_code(), 200);
    assert!(ok.json::<Value>().get("token").is_some());

    // ...and is rejected on reuse.
    let reuse = server
        .post("/api/auth/login")
        .json(&serde_json::json!({"password": "test-admin-password", "totp_code": recovery[0]}))
        .await;
    assert_eq!(reuse.status_code(), 401);
}

#[tokio::test]
async fn methods_reflects_state_and_passkey_login_needs_passkey() {
    let state = common::test_state();
    let server = common::test_app(state);

    let m = server.get("/api/auth/methods").await;
    assert_eq!(m.status_code(), 200);
    let body = m.json::<Value>();
    assert_eq!(body["totpEnabled"], Value::Bool(false));
    assert_eq!(body["passkeyEnabled"], Value::Bool(false));

    // No passkeys registered → start is a 400.
    let start = server.post("/api/auth/passkey/start").await;
    assert_eq!(start.status_code(), 400);
}

// ---------------------------------------------------------------------------
// Second-factor teardown must itself need the second factor.
//
// Both endpoints below sit behind an admin JWT, but that JWT is minted from the
// password alone — so gating them on the password re-check only is gating them
// on the exact factor TOTP exists to backstop. Someone with a stolen password
// (or a live admin session) could otherwise strip 2FA silently.
// ---------------------------------------------------------------------------

/// Enrol TOTP and return (secret, recovery codes).
async fn enrol_totp(server: &axum_test::TestServer, token: &str) -> (String, Vec<String>) {
    let setup = server
        .post("/api/admin/settings/totp/setup")
        .add_header(header::AUTHORIZATION, auth_val(token))
        .await;
    assert_eq!(setup.status_code(), 200);
    let secret = setup.json::<Value>()["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let totp = credentials::totp_from_secret(&secret).unwrap();
    let enable = server
        .post("/api/admin/settings/totp/enable")
        .add_header(header::AUTHORIZATION, auth_val(token))
        .json(&serde_json::json!({ "code": totp.generate_current().unwrap() }))
        .await;
    assert_eq!(enable.status_code(), 200);
    let recovery: Vec<String> = enable.json::<Value>()["recoveryCodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    (secret, recovery)
}

async fn totp_is_enabled(server: &axum_test::TestServer) -> bool {
    server.get("/api/auth/methods").await.json::<Value>()["totpEnabled"] == Value::Bool(true)
}

/// Re-running setup on an already-enrolled account must not quietly rotate the
/// secret and switch 2FA off — that turns "I clicked the wrong tab" into a
/// silently unprotected account.
#[tokio::test]
async fn totp_setup_refuses_to_clobber_an_active_enrolment() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let (secret, _) = enrol_totp(&server, &token).await;
    assert!(totp_is_enabled(&server).await);

    let again = server
        .post("/api/admin/settings/totp/setup")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(
        again.status_code(),
        400,
        "re-running setup while enrolled must be refused"
    );
    assert!(
        totp_is_enabled(&server).await,
        "TOTP was silently disabled by a second setup call"
    );

    // ...and the original secret still works, i.e. it was not rotated.
    let totp = credentials::totp_from_secret(&secret).unwrap();
    let login = server
        .post("/api/auth/login")
        .json(&serde_json::json!({
            "password": "test-admin-password",
            "totp_code": totp.generate_current().unwrap(),
        }))
        .await;
    assert_eq!(login.status_code(), 200, "original TOTP secret was rotated");
}

/// Disabling TOTP needs a current code, not just the password.
#[tokio::test]
async fn totp_disable_requires_a_second_factor() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let (secret, _) = enrol_totp(&server, &token).await;

    // Password alone is not enough.
    let bare = server
        .post("/api/admin/settings/totp/disable")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({ "password": "test-admin-password" }))
        .await;
    assert_eq!(bare.status_code(), 401, "password alone disabled 2FA");
    assert!(totp_is_enabled(&server).await);

    // A wrong code is not enough either.
    let wrong = server
        .post("/api/admin/settings/totp/disable")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({ "password": "test-admin-password", "code": "000000" }))
        .await;
    assert_eq!(wrong.status_code(), 401);
    assert!(totp_is_enabled(&server).await);

    // Password + a current code succeeds.
    let totp = credentials::totp_from_secret(&secret).unwrap();
    let ok = server
        .post("/api/admin/settings/totp/disable")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({
            "password": "test-admin-password",
            "code": totp.generate_current().unwrap(),
        }))
        .await;
    assert_eq!(ok.status_code(), 200);
    assert!(!totp_is_enabled(&server).await);
}

/// A recovery code is the documented way back in when the authenticator is
/// lost, so it must also work for teardown — otherwise losing the phone means
/// losing the account.
#[tokio::test]
async fn totp_disable_accepts_a_recovery_code() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let (_, recovery) = enrol_totp(&server, &token).await;

    let ok = server
        .post("/api/admin/settings/totp/disable")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({
            "password": "test-admin-password",
            "code": recovery[0],
        }))
        .await;
    assert_eq!(ok.status_code(), 200);
    assert!(!totp_is_enabled(&server).await);
}
