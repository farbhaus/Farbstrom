//! Authenticated credential management for the single admin: password
//! change, TOTP enrolment, and passkey (WebAuthn) registration. Mounted at
//! `/api/admin/settings`; every handler requires `AdminAuth`.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::auth::AdminAuth;
use crate::credentials as cred;
use crate::error::AppError;
use crate::state::{AppState, CEREMONY_TTL_SECS};

/// Stable WebAuthn user handle for the one admin identity. Constant so
/// `excludeCredentials` and the user handle stay consistent across
/// registrations.
fn admin_user_id() -> Uuid {
    Uuid::from_u128(0x5a655f4d617269615f73747265616d31) // "Zé Maria stream1"
}

// ---- status ---------------------------------------------------------------

async fn status(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let (_, password_is_custom) = cred::current_password_hash(&state).await?;
    let conn = state.db.get()?;
    let (totp_enabled, passkeys) = tokio::task::spawn_blocking(move || {
        let totp = cred::settings_get(&conn, cred::KEY_TOTP_ENABLED).as_deref() == Some("1");
        let mut stmt = conn.prepare(
            "SELECT id, label, created_at, last_used_at FROM admin_passkeys ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "label": r.get::<_, String>(1)?,
                    "created_at": r.get::<_, String>(2)?,
                    "last_used_at": r.get::<_, Option<String>>(3)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>((totp, rows))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({
        "passwordIsCustom": password_is_custom,
        "totpEnabled": totp_enabled,
        "passkeys": passkeys,
    })))
}

// ---- password -------------------------------------------------------------

#[derive(Deserialize)]
struct PasswordBody {
    current: String,
    new: String,
}

async fn change_password(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<PasswordBody>,
) -> Result<Json<Value>, AppError> {
    if body.new.len() < 12 {
        return Err(AppError::BadRequest(
            "New password must be at least 12 characters".into(),
        ));
    }
    if !cred::verify_password(&state, body.current).await? {
        return Err(AppError::Unauthorized("Current password is wrong".into()));
    }
    let new = body.new;
    let hash = tokio::task::spawn_blocking(move || bcrypt::hash(new, 12))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || cred::settings_set(&conn, cred::KEY_PASSWORD_HASH, &hash))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
    Ok(Json(json!({ "ok": true })))
}

// ---- TOTP -----------------------------------------------------------------

/// Generate a provisional secret (NOT yet enabled) and return the QR + secret
/// for the operator to scan. Enrolment is confirmed by `/totp/enable`.
async fn totp_setup(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let secret = cred::gen_totp_secret();
    let totp = cred::totp_from_secret(&secret)?;
    let qr = totp
        .get_qr_base64()
        .map_err(|e| AppError::Internal(format!("QR: {e}")))?;
    let conn = state.db.get()?;
    let s = secret.clone();
    tokio::task::spawn_blocking(move || {
        cred::settings_set(&conn, cred::KEY_TOTP_SECRET, &s)?;
        cred::settings_set(&conn, cred::KEY_TOTP_ENABLED, "0")
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    Ok(Json(json!({
        "secret": secret,
        "qr": format!("data:image/png;base64,{qr}"),
    })))
}

#[derive(Deserialize)]
struct TotpEnableBody {
    code: String,
}

async fn totp_enable(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<TotpEnableBody>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let secret =
        tokio::task::spawn_blocking(move || cred::settings_get(&conn, cred::KEY_TOTP_SECRET))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .ok_or_else(|| AppError::BadRequest("Run setup first".into()))?;

    let totp = cred::totp_from_secret(&secret)?;
    let valid = totp
        .check_current(&body.code)
        .map_err(|e| AppError::Internal(format!("TOTP: {e}")))?;
    if !valid {
        return Err(AppError::BadRequest("Incorrect code".into()));
    }

    let (plain, hashed) = cred::gen_recovery_codes()?;
    let recovery_json = serde_json::to_string(&hashed).unwrap();
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        cred::settings_set(&conn, cred::KEY_TOTP_ENABLED, "1")?;
        cred::settings_set(&conn, cred::KEY_TOTP_RECOVERY, &recovery_json)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(json!({ "ok": true, "recoveryCodes": plain })))
}

#[derive(Deserialize)]
struct TotpDisableBody {
    password: String,
}

async fn totp_disable(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<TotpDisableBody>,
) -> Result<Json<Value>, AppError> {
    if !cred::verify_password(&state, body.password).await? {
        return Err(AppError::Unauthorized("Wrong password".into()));
    }
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        cred::settings_del(&conn, cred::KEY_TOTP_SECRET)?;
        cred::settings_del(&conn, cred::KEY_TOTP_ENABLED)?;
        cred::settings_del(&conn, cred::KEY_TOTP_RECOVERY)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    Ok(Json(json!({ "ok": true })))
}

// ---- passkeys -------------------------------------------------------------

/// Load every stored passkey (used to exclude already-registered creds and
/// for the authentication challenge).
pub fn load_passkeys(conn: &rusqlite::Connection) -> Result<Vec<Passkey>, AppError> {
    let mut stmt = conn.prepare("SELECT credential FROM admin_passkeys")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|s| serde_json::from_str::<Passkey>(&s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Internal(format!("Corrupt passkey: {e}")))
}

#[derive(Deserialize)]
struct RegisterStartBody {
    label: String,
}

async fn passkey_register_start(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterStartBody>,
) -> Result<Json<Value>, AppError> {
    if body.label.trim().is_empty() {
        return Err(AppError::BadRequest("Label required".into()));
    }
    let conn = state.db.get()?;
    let existing = tokio::task::spawn_blocking(move || load_passkeys(&conn))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
    let exclude: Vec<CredentialID> = existing.iter().map(|p| p.cred_id().clone()).collect();

    let (ccr, reg) = state
        .webauthn
        .start_passkey_registration(admin_user_id(), "admin", "Farbstrom Admin", Some(exclude))
        .map_err(|e| AppError::Internal(format!("WebAuthn: {e}")))?;

    let id = Uuid::new_v4();
    {
        let mut map = state.passkey_reg.lock().await;
        sweep(&mut map);
        map.insert(id, (Instant::now(), reg));
    }
    Ok(Json(
        json!({ "id": id, "label": body.label, "options": ccr }),
    ))
}

#[derive(Deserialize)]
struct RegisterFinishBody {
    id: Uuid,
    label: String,
    credential: RegisterPublicKeyCredential,
}

async fn passkey_register_finish(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterFinishBody>,
) -> Result<Json<Value>, AppError> {
    let reg = {
        let mut map = state.passkey_reg.lock().await;
        sweep(&mut map);
        map.remove(&body.id)
            .ok_or_else(|| AppError::BadRequest("Registration expired — retry".into()))?
            .1
    };
    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.credential, &reg)
        .map_err(|e| AppError::BadRequest(format!("Registration failed: {e}")))?;

    let cred_json =
        serde_json::to_string(&passkey).map_err(|e| AppError::Internal(e.to_string()))?;
    let pk_id = Uuid::new_v4().to_string();
    let label = body.label;
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || {
        conn.execute(
            "INSERT INTO admin_passkeys (id, label, credential) VALUES (?1, ?2, ?3)",
            rusqlite::params![pk_id, label, cred_json],
        )
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    Ok(Json(json!({ "ok": true })))
}

async fn passkey_delete(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let n = tokio::task::spawn_blocking(move || {
        conn.execute(
            "DELETE FROM admin_passkeys WHERE id = ?1",
            rusqlite::params![id],
        )
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    if n == 0 {
        return Err(AppError::NotFound("Passkey not found".into()));
    }
    Ok(Json(json!({ "ok": true })))
}

/// Drop ceremony entries older than the TTL.
fn sweep<T>(map: &mut std::collections::HashMap<Uuid, (Instant, T)>) {
    map.retain(|_, (ts, _)| ts.elapsed().as_secs() < CEREMONY_TTL_SECS);
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(status))
        .route("/password", post(change_password))
        .route("/totp/setup", post(totp_setup))
        .route("/totp/enable", post(totp_enable))
        .route("/totp/disable", post(totp_disable))
        .route("/passkeys/register/start", post(passkey_register_start))
        .route("/passkeys/register/finish", post(passkey_register_finish))
        .route("/passkeys/{id}", axum::routing::delete(passkey_delete))
}
