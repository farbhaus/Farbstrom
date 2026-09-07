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
    // 403, not 401: the *session* is fine, the submitted password is not. The
    // admin SPA signs you out on any 401, so a 401 here meant a typo in the
    // change-password form logged you out of the panel.
    assert_eq!(wrong.status_code(), 403);
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
    assert_eq!(bare.status_code(), 403, "password alone disabled 2FA");
    assert!(totp_is_enabled(&server).await);

    // A wrong code is not enough either.
    let wrong = server
        .post("/api/admin/settings/totp/disable")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({ "password": "test-admin-password", "code": "000000" }))
        .await;
    assert_eq!(wrong.status_code(), 403);
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

// ---------------------------------------------------------------------------
// Session revocation
// ---------------------------------------------------------------------------
//
// Admin JWTs are stateless and nothing tied them to the password, so changing
// it revoked nothing and a stolen token stayed valid for its full expiry. Each
// token now carries the generation it was minted under.

/// Decode a JWT payload without verifying — we only want to read claims.
fn claims_of(jwt: &str) -> Value {
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(jwt.split('.').nth(1).unwrap())
        .unwrap();
    serde_json::from_slice(&payload).unwrap()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Changing the password must end every other session, and hand the caller a
/// replacement so the tab they did it in keeps working.
#[tokio::test]
async fn password_change_revokes_other_sessions_but_not_this_one() {
    let state = common::test_state();
    let stale = common::admin_token(&state);
    let server = common::test_app(state);

    // The old token works beforehand.
    let before = server
        .get("/api/admin/settings/status")
        .add_header(header::AUTHORIZATION, auth_val(&stale))
        .await;
    assert_eq!(before.status_code(), 200);

    let changed = server
        .post("/api/admin/settings/password")
        .add_header(header::AUTHORIZATION, auth_val(&stale))
        .json(&serde_json::json!({
            "current": "test-admin-password",
            "new": "a-brand-new-long-password",
        }))
        .await;
    assert_eq!(changed.status_code(), 200);

    // The token that made the request is now dead...
    let after = server
        .get("/api/admin/settings/status")
        .add_header(header::AUTHORIZATION, auth_val(&stale))
        .await;
    assert_eq!(
        after.status_code(),
        401,
        "a token minted before the password change is still valid"
    );

    // ...and the replacement handed back works.
    let fresh = changed.json::<Value>()["token"]
        .as_str()
        .expect("password change must return a replacement token")
        .to_string();
    let with_fresh = server
        .get("/api/admin/settings/status")
        .add_header(header::AUTHORIZATION, auth_val(&fresh))
        .await;
    assert_eq!(
        with_fresh.status_code(),
        200,
        "the replacement token should keep the current session alive"
    );
}

/// The explicit button, password-gated so a stolen token cannot use it to lock
/// the real operator out while keeping itself alive.
#[tokio::test]
async fn sign_out_everywhere_revokes_and_reissues() {
    let state = common::test_state();
    let stale = common::admin_token(&state);
    let server = common::test_app(state);

    let refused = server
        .post("/api/admin/settings/sign-out-everywhere")
        .add_header(header::AUTHORIZATION, auth_val(&stale))
        .json(&serde_json::json!({ "password": "wrong" }))
        .await;
    assert_eq!(refused.status_code(), 403, "should need the password");

    // Nothing was revoked by the refused attempt.
    assert_eq!(
        server
            .get("/api/admin/settings/status")
            .add_header(header::AUTHORIZATION, auth_val(&stale))
            .await
            .status_code(),
        200
    );

    let done = server
        .post("/api/admin/settings/sign-out-everywhere")
        .add_header(header::AUTHORIZATION, auth_val(&stale))
        .json(&serde_json::json!({ "password": "test-admin-password" }))
        .await;
    assert_eq!(done.status_code(), 200);

    assert_eq!(
        server
            .get("/api/admin/settings/status")
            .add_header(header::AUTHORIZATION, auth_val(&stale))
            .await
            .status_code(),
        401,
        "sign-out-everywhere did not revoke the old token"
    );
    let fresh = done.json::<Value>()["token"].as_str().unwrap().to_string();
    assert_eq!(
        server
            .get("/api/admin/settings/status")
            .add_header(header::AUTHORIZATION, auth_val(&fresh))
            .await
            .status_code(),
        200
    );
}

/// Revocation must survive a restart, or it is theatre: the generation lives in
/// the settings table and `AppState` only caches it.
#[tokio::test]
async fn revocation_survives_a_restart() {
    let config = common::test_config();
    let state = common::test_state_with_config(config.clone());
    let stale = common::admin_token(&state);
    let server = common::test_app(state);

    server
        .post("/api/admin/settings/sign-out-everywhere")
        .add_header(header::AUTHORIZATION, auth_val(&stale))
        .json(&serde_json::json!({ "password": "test-admin-password" }))
        .await;

    // A fresh AppState over the same database — i.e. a process restart.
    let restarted = common::test_state_with_config(config);
    let server2 = common::test_app(restarted);
    assert_eq!(
        server2
            .get("/api/admin/settings/status")
            .add_header(header::AUTHORIZATION, auth_val(&stale))
            .await
            .status_code(),
        401,
        "the revoked token came back to life after a restart"
    );
}

// ---------------------------------------------------------------------------
// "Trust this browser"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trust_this_browser_extends_the_session() {
    let state = common::test_state();
    let server = common::test_app(state);

    let normal = server
        .post("/api/auth/login")
        .json(&serde_json::json!({ "password": "test-admin-password" }))
        .await;
    let trusted = server
        .post("/api/auth/login")
        .json(&serde_json::json!({ "password": "test-admin-password", "trust": true }))
        .await;

    let normal_exp = claims_of(normal.json::<Value>()["token"].as_str().unwrap())["exp"]
        .as_u64()
        .unwrap();
    let trusted_exp = claims_of(trusted.json::<Value>()["token"].as_str().unwrap())["exp"]
        .as_u64()
        .unwrap();
    let now = now_secs();

    // ~7 days vs ~90 days, with slack for test execution time.
    let seven_days = 7 * 24 * 60 * 60;
    let ninety_days = 90 * 24 * 60 * 60;
    assert!(
        normal_exp.abs_diff(now + seven_days) < 120,
        "default session should be ~7 days, exp={normal_exp} now={now}"
    );
    assert!(
        trusted_exp.abs_diff(now + ninety_days) < 120,
        "trusted session should be ~90 days, exp={trusted_exp} now={now}"
    );
}

/// A trusted token is still revocable — that is the whole reason a 90-day
/// session is acceptable.
#[tokio::test]
async fn a_trusted_session_is_still_revocable() {
    let state = common::test_state();
    let server = common::test_app(state);

    let login = server
        .post("/api/auth/login")
        .json(&serde_json::json!({ "password": "test-admin-password", "trust": true }))
        .await;
    let trusted = login.json::<Value>()["token"].as_str().unwrap().to_string();

    server
        .post("/api/admin/settings/sign-out-everywhere")
        .add_header(header::AUTHORIZATION, auth_val(&trusted))
        .json(&serde_json::json!({ "password": "test-admin-password" }))
        .await;

    // Use a *different* token to check, since the call above re-issued one.
    let stale_check = server
        .get("/api/admin/settings/status")
        .add_header(header::AUTHORIZATION, auth_val(&trusted))
        .await;
    assert_eq!(
        stale_check.status_code(),
        401,
        "a 90-day trusted token must still be revocable"
    );
}

/// Tokens minted before the `ver` claim existed decode as generation 0, so an
/// upgrade must not sign the operator out for no reason.
#[tokio::test]
async fn tokens_without_a_version_claim_still_work_on_a_fresh_install() {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    #[derive(serde::Serialize)]
    struct LegacyClaims {
        admin: bool,
        exp: usize,
    }

    let state = common::test_state();
    let secret = state.config.jwt_secret.clone();
    let legacy = encode(
        &Header::new(Algorithm::HS256),
        &LegacyClaims {
            admin: true,
            exp: (now_secs() + 3600) as usize,
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap();

    let server = common::test_app(state);
    assert_eq!(
        server
            .get("/api/admin/settings/status")
            .add_header(header::AUTHORIZATION, auth_val(&legacy))
            .await
            .status_code(),
        200,
        "a pre-upgrade token should survive until something actually revokes it"
    );
}
