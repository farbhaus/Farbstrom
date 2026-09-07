use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::auth::create_admin_token;
use crate::credentials as cred;
use crate::error::AppError;
use crate::routes::admin_settings::load_passkeys;
use crate::routes::rate_limit;
use crate::state::{AppState, CEREMONY_TTL_SECS};

#[derive(Deserialize)]
struct LoginBody {
    password: Option<String>,
    totp_code: Option<String>,
}

/// Public: which extra factors the login screen should offer. Reveals only
/// whether 2FA / passkeys are configured — acceptable for a single-operator
/// private tool, and required so the UI can render the right fields.
async fn methods(State(state): State<Arc<AppState>>) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let (totp, passkeys) = tokio::task::spawn_blocking(move || {
        let totp = cred::settings_get(&conn, cred::KEY_TOTP_ENABLED).as_deref() == Some("1");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM admin_passkeys", [], |r| r.get(0))
            .unwrap_or(0);
        (totp, count > 0)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(
        json!({ "totpEnabled": totp, "passkeyEnabled": passkeys }),
    ))
}

async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginBody>,
) -> Result<Json<Value>, AppError> {
    let password = body
        .password
        .ok_or_else(|| AppError::BadRequest("Password required".into()))?;

    if !cred::verify_password(&state, password).await? {
        return Err(AppError::Unauthorized("Wrong password".into()));
    }

    // Second factor, if the operator has enrolled one.
    let conn = state.db.get()?;
    let totp_enabled =
        tokio::task::spawn_blocking(move || cred::settings_get(&conn, cred::KEY_TOTP_ENABLED))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .as_deref()
            == Some("1");

    if totp_enabled {
        let Some(code) = body.totp_code.filter(|c| !c.trim().is_empty()) else {
            // Password OK, code still needed — tell the UI to prompt.
            return Ok(Json(json!({ "totpRequired": true })));
        };
        if !cred::verify_totp_or_recovery(&state, &code).await? {
            return Err(AppError::Unauthorized("Invalid code".into()));
        }
    }

    let token = create_admin_token(&state.config.jwt_secret)?;
    Ok(Json(json!({ "token": token })))
}

async fn logout() -> Json<Value> {
    Json(json!({ "ok": true }))
}

// ---- passkey login --------------------------------------------------------

async fn passkey_start(State(state): State<Arc<AppState>>) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let passkeys = tokio::task::spawn_blocking(move || load_passkeys(&conn))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
    if passkeys.is_empty() {
        return Err(AppError::BadRequest("No passkeys registered".into()));
    }
    let (rcr, auth) = state
        .webauthn
        .start_passkey_authentication(&passkeys)
        .map_err(|e| AppError::Internal(format!("WebAuthn: {e}")))?;
    let id = Uuid::new_v4();
    {
        let mut map = state.passkey_auth.lock().await;
        map.retain(|_, (ts, _)| ts.elapsed().as_secs() < CEREMONY_TTL_SECS);
        map.insert(id, (Instant::now(), auth));
    }
    Ok(Json(json!({ "id": id, "options": rcr })))
}

#[derive(Deserialize)]
struct PasskeyFinishBody {
    id: Uuid,
    credential: PublicKeyCredential,
}

async fn passkey_finish(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PasskeyFinishBody>,
) -> Result<Json<Value>, AppError> {
    let auth = {
        let mut map = state.passkey_auth.lock().await;
        map.retain(|_, (ts, _)| ts.elapsed().as_secs() < CEREMONY_TTL_SECS);
        map.remove(&body.id)
            .ok_or_else(|| AppError::BadRequest("Challenge expired — retry".into()))?
            .1
    };
    let result = state
        .webauthn
        .finish_passkey_authentication(&body.credential, &auth)
        .map_err(|e| AppError::Unauthorized(format!("Passkey rejected: {e}")))?;

    // Persist the updated signature counter and stamp last_used_at on the
    // matching row.
    let conn = state.db.get()?;
    let cred_id = result.cred_id().clone();
    tokio::task::spawn_blocking(move || -> Result<(), rusqlite::Error> {
        let mut stmt = conn.prepare("SELECT id, credential FROM admin_passkeys")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        for (row_id, json) in rows {
            if let Ok(mut pk) = serde_json::from_str::<Passkey>(&json) {
                if pk.cred_id() == &cred_id {
                    pk.update_credential(&result);
                    let new_json = serde_json::to_string(&pk).unwrap_or(json);
                    conn.execute(
                        "UPDATE admin_passkeys SET credential = ?1, \
                         last_used_at = CURRENT_TIMESTAMP WHERE id = ?2",
                        rusqlite::params![new_json, row_id],
                    )?;
                    break;
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let token = create_admin_token(&state.config.jwt_secret)?;
    Ok(Json(json!({ "token": token })))
}

pub fn router() -> Router<Arc<AppState>> {
    let (login_handler, pk_start, pk_finish) = if rate_limit::enabled() {
        (
            post(login).layer(rate_limit::login_layer()),
            post(passkey_start).layer(rate_limit::passkey_layer()),
            post(passkey_finish).layer(rate_limit::passkey_layer()),
        )
    } else {
        (post(login), post(passkey_start), post(passkey_finish))
    };
    Router::new()
        .route("/login", login_handler)
        .route("/logout", post(logout))
        .route("/methods", get(methods))
        .route("/passkey/start", pk_start)
        .route("/passkey/finish", pk_finish)
}
