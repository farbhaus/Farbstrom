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
/// Generation counter stamped into every admin JWT. Bumping it invalidates
/// every token minted before the bump — see [`bump_token_version`].
pub const KEY_TOKEN_VERSION: &str = "admin_token_version";
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

// ---- Admin session invalidation -------------------------------------------

/// Current admin token generation, straight from the DB. Absent or unparseable
/// means 0, which is also what a token minted before this existed decodes to —
/// so upgrading does not spuriously sign anyone out.
pub fn token_version_get(conn: &rusqlite::Connection) -> u64 {
    settings_get(conn, KEY_TOKEN_VERSION)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Invalidate every admin token issued so far.
///
/// Admin JWTs are stateless: nothing tied them to the password, so changing it
/// revoked nothing and a stolen token stayed valid for its full 7 days. Each
/// token now carries the generation it was minted under, and `AdminAuth` refuses
/// any that does not match the current one.
///
/// The DB row is the source of truth; the `AppState` counter is a cache so the
/// check costs no I/O on a path that runs for every admin request. One process
/// serves the DB, so the two cannot diverge — and a restart reloads from the row
/// regardless.
///
/// Returns the new generation, so the caller can mint a replacement token for
/// whoever triggered this and keep *their* session alive.
pub async fn bump_token_version(state: &AppState) -> Result<u64, AppError> {
    let conn = state.db.get()?;
    let next = tokio::task::spawn_blocking(move || -> Result<u64, rusqlite::Error> {
        let next = token_version_get(&conn).wrapping_add(1);
        settings_set(&conn, KEY_TOKEN_VERSION, &next.to_string())?;
        Ok(next)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    state
        .admin_token_version
        .store(next, std::sync::atomic::Ordering::SeqCst);
    Ok(next)
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

/// Verify a submitted second factor: a current TOTP code, or — if that fails —
/// one of the stored one-time recovery codes, which is consumed on use.
///
/// Shared by the login path and by TOTP teardown, so "what counts as a valid
/// second factor" is defined once. Returns `false` when TOTP is not enrolled at
/// all, so callers must decide whether a second factor is required.
pub async fn verify_totp_or_recovery(state: &AppState, code: &str) -> Result<bool, AppError> {
    let code = code.trim();
    if code.is_empty() {
        return Ok(false);
    }
    let conn = state.db.get()?;
    let secret = tokio::task::spawn_blocking(move || settings_get(&conn, KEY_TOTP_SECRET))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let Some(secret) = secret else {
        return Ok(false);
    };
    let totp = totp_from_secret(&secret)?;
    if totp
        .check_current(code)
        .map_err(|e| AppError::Internal(format!("TOTP: {e}")))?
    {
        return Ok(true);
    }
    consume_recovery_code(state, code).await
}

/// Consume a one-time recovery code (bcrypt-matched). Returns true and persists
/// the shortened list if the code was valid and unused.
pub async fn consume_recovery_code(state: &AppState, code: &str) -> Result<bool, AppError> {
    let conn = state.db.get()?;
    let stored = tokio::task::spawn_blocking(move || settings_get(&conn, KEY_TOTP_RECOVERY))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let Some(json) = stored else { return Ok(false) };
    let hashes: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
    let code = code.to_string();
    let (matched, remaining) = tokio::task::spawn_blocking(move || {
        let mut remaining = Vec::with_capacity(hashes.len());
        let mut matched = false;
        for h in hashes {
            if !matched && bcrypt::verify(&code, &h).unwrap_or(false) {
                matched = true; // drop this one
            } else {
                remaining.push(h);
            }
        }
        (matched, remaining)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    if matched {
        let conn = state.db.get()?;
        let json = serde_json::to_string(&remaining)
            .map_err(|e| AppError::Internal(format!("recovery codes: {e}")))?;
        tokio::task::spawn_blocking(move || settings_set(&conn, KEY_TOTP_RECOVERY, &json))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;
    }
    Ok(matched)
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
