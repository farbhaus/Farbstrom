//! Single-admin credential helpers: settings-table accessors, the
//! DB-or-env password resolver, TOTP, recovery codes, and the WebAuthn
//! relying-party builder. Kept in one module so the login path
//! (`routes::auth`) and the management UI (`routes::admin_settings`) share
//! exactly the same logic.

use crate::error::AppError;
use crate::state::AppState;
use rand::{Rng, RngExt};
use rusqlite::params;
use totp_rs::{Algorithm, Builder, Secret, Totp};
use webauthn_rs::prelude::*;

pub const KEY_PASSWORD_HASH: &str = "admin_password_hash";
/// Generation counter stamped into every admin JWT. Bumping it invalidates
/// every token minted before the bump — see [`bump_token_version`].
pub const KEY_TOKEN_VERSION: &str = "admin_token_version";
pub const KEY_TOTP_SECRET: &str = "totp_secret";
pub const KEY_TOTP_ENABLED: &str = "totp_enabled";
pub const KEY_TOTP_RECOVERY: &str = "totp_recovery";
/// The latest TOTP time step accepted; see [`verify_totp_code`].
pub const KEY_TOTP_LAST_STEP: &str = "totp_last_step";

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
        // One statement, so concurrent bumps cannot lose an increment by both
        // reading the same value before either writes. A read-then-write pair
        // did exactly that: eight concurrent calls landed on generation 4.
        // `RETURNING` hands back the value *this* call established.
        conn.query_row(
            "INSERT INTO settings (key, value) VALUES (?1, '1') \
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1 \
             RETURNING CAST(value AS INTEGER)",
            params![KEY_TOKEN_VERSION],
            |row| row.get::<_, i64>(0),
        )
        .map(|v| v.max(0) as u64)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // `fetch_max`, not `store`: the DB write and this update are separate steps,
    // so a slower call finishing last must not walk the cache backwards below
    // the row it would be read from on restart.
    state
        .admin_token_version
        .fetch_max(next, std::sync::atomic::Ordering::SeqCst);
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

/// Build a `Totp` from a stored base32 secret (RFC 6238 defaults: SHA1, 6
/// digits, 30s step, ±1 step skew — what every authenticator app expects).
pub fn totp_from_secret(secret_b32: &str) -> Result<Totp, AppError> {
    let secret = Secret::try_from_base32(secret_b32)
        .map_err(|_| AppError::Internal("Bad TOTP secret".into()))?;
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(secret)
        .with_issuer(Some("Farbstrom"))
        .with_account_name("admin")
        .build()
        .map_err(|e| AppError::Internal(format!("TOTP init: {e}")))
}

/// Record `step` as used, atomically. False when it — or a later one — has
/// already been accepted. A single statement, so two concurrent submissions of
/// the same code cannot both pass the check before either records it.
fn claim_totp_step(conn: &rusqlite::Connection, step: u64) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value \
         WHERE CAST(settings.value AS INTEGER) < CAST(excluded.value AS INTEGER)",
        params![KEY_TOTP_LAST_STEP, step.to_string()],
    )?;
    Ok(changed > 0)
}

/// Check a TOTP code against `secret` and use it up.
///
/// A code stays valid for the whole skew window (±1 step, ~90 s), and this used
/// to accept it as often as it was offered — a code read over a shoulder or
/// relayed by a phishing proxy logged in again for that long. RFC 6238 §5.2
/// says a verifier MUST NOT accept the same OTP twice, so the step a code
/// belongs to is recorded, and only later steps are accepted after it.
pub async fn verify_totp_code(
    state: &AppState,
    secret: &str,
    code: &str,
) -> Result<bool, AppError> {
    let totp = totp_from_secret(secret)?;
    let now_secs = crate::time::now_ms() / 1000;
    // The step the code belongs to within the skew window, compared in
    // constant time.
    let Some(step) = totp.check(code.trim(), now_secs) else {
        return Ok(false);
    };
    let conn = state.db.get()?;
    Ok(
        tokio::task::spawn_blocking(move || claim_totp_step(&conn, step))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??,
    )
}

/// Generate a fresh random base32 TOTP secret (160-bit, RFC 4226 §4 minimum).
pub fn gen_totp_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    Secret::from(bytes).to_base32()
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
    if verify_totp_code(state, &secret, code).await? {
        return Ok(true);
    }
    consume_recovery_code(state, code).await
}

/// Consume a one-time recovery code (bcrypt-matched). Returns true and persists
/// the shortened list if the code was valid and unused.
///
/// The list is read, matched (slowly — bcrypt), then written back, and two
/// requests used to interleave across that gap: the same code submitted twice
/// at once passed twice, and two different codes at once each wrote back a list
/// still holding the other's — restoring a code that had just been used. The
/// write is now conditional on the list being exactly what was matched against;
/// if another request changed it meanwhile, this one matches again against what
/// is there now.
pub async fn consume_recovery_code(state: &AppState, code: &str) -> Result<bool, AppError> {
    let code = code.to_string();
    let conn = state.db.get()?;
    tokio::task::spawn_blocking(move || -> Result<bool, AppError> {
        // Each pass is one bcrypt sweep; a lost race costs one more. Bounded
        // so a pathological stream of concurrent changes cannot spin forever.
        for _ in 0..8 {
            let Some(stored) = settings_get(&conn, KEY_TOTP_RECOVERY) else {
                return Ok(false);
            };
            let hashes: Vec<String> = serde_json::from_str(&stored).unwrap_or_default();
            let Some(i) = hashes
                .iter()
                .position(|h| bcrypt::verify(&code, h).unwrap_or(false))
            else {
                return Ok(false);
            };
            let mut remaining = hashes;
            remaining.remove(i);
            let json = serde_json::to_string(&remaining)
                .map_err(|e| AppError::Internal(format!("recovery codes: {e}")))?;
            let swapped = conn.execute(
                "UPDATE settings SET value = ?1 WHERE key = ?2 AND value = ?3",
                params![json, KEY_TOTP_RECOVERY, stored],
            )?;
            if swapped == 1 {
                return Ok(true);
            }
        }
        Err(AppError::Internal(
            "recovery codes: list kept changing under the update".into(),
        ))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `claim_totp_step` records whatever `check` returns, so the step must be
    /// the code's own — not the current one — for replay protection to hold.
    #[test]
    fn a_code_matches_only_its_own_step_within_the_skew() {
        let totp = totp_from_secret(&gen_totp_secret()).unwrap();
        let now = 1_700_000_000;
        let step = now / 30;
        for offset in [-1i64, 0, 1] {
            let s = (step as i64 + offset) as u64;
            let code = totp.generate(s * 30).to_string();
            assert_eq!(totp.check(&code, now), Some(s));
        }
        // Two steps out is outside the ±1 window.
        let late = totp.generate((step + 2) * 30).to_string();
        assert_eq!(totp.check(&late, now), None);
        assert_eq!(totp.check("not-a-code", now), None);
    }

    /// Secrets enrolled under totp-rs 5 are stored as unpadded RFC 4648 base32;
    /// they must still decode to the same key, or an upgrade would silently
    /// invalidate the operator's authenticator.
    #[test]
    fn a_secret_stored_before_the_upgrade_still_yields_the_same_codes() {
        // RFC 6238 Appendix B's SHA-1 key, "12345678901234567890", in the
        // encoding the old crate wrote.
        let totp = totp_from_secret("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
        // RFC 6238's published SHA-1 vectors, truncated to 6 digits.
        assert_eq!(totp.generate(59).to_string(), "287082");
        assert_eq!(totp.generate(1_111_111_109).to_string(), "081804");
        assert_eq!(totp.generate(2_000_000_000).to_string(), "279037");
    }

    #[test]
    fn a_step_can_be_claimed_once_and_never_after_a_later_one() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        assert!(claim_totp_step(&conn, 100).unwrap());
        assert!(!claim_totp_step(&conn, 100).unwrap(), "same code twice");
        assert!(claim_totp_step(&conn, 101).unwrap(), "the next code");
        assert!(
            !claim_totp_step(&conn, 100).unwrap(),
            "an older code after a newer one"
        );
    }
}
