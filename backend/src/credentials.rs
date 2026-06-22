//! Single-admin credential helpers: settings-table accessors, the
//! DB-or-env password resolver, TOTP, recovery codes, and the WebAuthn
//! relying-party builder. Kept in one module so the login path
//! (`routes::auth`) and the management UI (`routes::admin_settings`) share
//! exactly the same logic.

use crate::error::AppError;
use crate::state::AppState;
use rand::{Rng, RngExt};
use rusqlite::params;
use totp_rs::{Algorithm, Secret, TOTP};
use webauthn_rs::prelude::*;

pub const KEY_PASSWORD_HASH: &str = "admin_password_hash";
pub const KEY_TOTP_SECRET: &str = "totp_secret";
pub const KEY_TOTP_ENABLED: &str = "totp_enabled";
pub const KEY_TOTP_RECOVERY: &str = "totp_recovery";

/// Synchronous settings read — call inside `spawn_blocking` or a blocking
/// closure that already holds a pooled connection.
pub fn settings_get(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

pub fn settings_set(
    conn: &rusqlite::Connection,
    key: &str,
    value: &str,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

pub fn settings_del(conn: &rusqlite::Connection, key: &str) -> Result<(), rusqlite::Error> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
    Ok(())
}

/// The bcrypt hash to verify admin logins against: the DB value if the
/// operator has set a custom password, otherwise the env-derived bootstrap
/// hash. Clearing the `admin_password_hash` settings row reverts to env
/// (break-glass).
pub async fn current_password_hash(state: &AppState) -> Result<(String, bool), AppError> {
    let conn = state.db.get()?;
    let db_hash = tokio::task::spawn_blocking(move || settings_get(&conn, KEY_PASSWORD_HASH))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    match db_hash {
        Some(h) => Ok((h, true)),
        None => Ok((state.admin_password_hash.clone(), false)),
    }
}

/// Verify a candidate password against the current admin hash.
pub async fn verify_password(state: &AppState, password: String) -> Result<bool, AppError> {
    let (hash, _) = current_password_hash(state).await?;
    if hash.is_empty() {
        return Err(AppError::Internal("Server misconfigured".into()));
    }
    tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash).unwrap_or(false))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))
}

// ---- TOTP -----------------------------------------------------------------

/// Build a `TOTP` from a stored base32 secret (RFC 6238 defaults: SHA1, 6
/// digits, 30s step, ±1 step skew — what every authenticator app expects).
pub fn totp_from_secret(secret_b32: &str) -> Result<TOTP, AppError> {
    let bytes = Secret::Encoded(secret_b32.to_string())
        .to_bytes()
        .map_err(|_| AppError::Internal("Bad TOTP secret".into()))?;
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        Some("Farbstrom".to_string()),
        "admin".to_string(),
    )
    .map_err(|e| AppError::Internal(format!("TOTP init: {e}")))
}

/// Generate a fresh random base32 TOTP secret (160-bit, RFC 4226 §4 minimum).
pub fn gen_totp_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    Secret::Raw(bytes.to_vec()).to_encoded().to_string()
}

// ---- Recovery codes -------------------------------------------------------

/// 10 human-typable one-time codes, returned plaintext (shown once) plus
/// their bcrypt hashes (the only thing persisted).
pub fn gen_recovery_codes() -> Result<(Vec<String>, Vec<String>), AppError> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no I/O/0/1
    let mut rng = rand::rng();
    let mut plain = Vec::with_capacity(10);
    let mut hashed = Vec::with_capacity(10);
    for _ in 0..10 {
        let raw: String = (0..10)
            .map(|i| {
                if i == 5 {
                    '-'
                } else {
                    ALPHABET[rng.random_range(0..ALPHABET.len())] as char
                }
            })
            .collect();
        hashed.push(bcrypt::hash(&raw, 10).map_err(AppError::from)?);
        plain.push(raw);
    }
    Ok((plain, hashed))
}

// ---- WebAuthn -------------------------------------------------------------

/// Build the WebAuthn relying party from the public origin. The RP ID is the
/// host of `public_origin` (e.g. `stream.yourdomain.com` / `localhost`).
/// Panics on a malformed origin — a misconfigured deployment should fail fast.
pub fn build_webauthn(public_origin: &str) -> Webauthn {
    let url = Url::parse(public_origin)
        .unwrap_or_else(|e| panic!("FATAL: PUBLIC_ORIGIN is not a valid URL: {e}"));
    let rp_id = url
        .host_str()
        .unwrap_or_else(|| panic!("FATAL: PUBLIC_ORIGIN has no host"))
        .to_string();
    WebauthnBuilder::new(&rp_id, &url)
        .expect("FATAL: invalid WebAuthn RP config")
        .rp_name("Farbstrom")
        .build()
        .expect("FATAL: failed to build WebAuthn")
}
