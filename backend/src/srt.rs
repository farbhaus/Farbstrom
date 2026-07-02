//! Runtime SRT wire-encryption state (gh #208).
//!
//! SRT AES encryption used to be controlled solely by the `SRT_INGEST_PASSPHRASE`
//! / `SRT_PLAYBACK_PASSPHRASE` env vars, read once at startup by both the backend
//! and OME (`Server.xml` `${env:...}`) — so flipping it meant editing `.env` and
//! restarting the whole stack. This module makes the passphrases **DB-managed**
//! (the `settings` table) and toggleable from the admin Settings tab.
//!
//! The two legs are **independent**: ingest (encoder → server, port 9999) and
//! playback (server → viewers, port 9998) each have their own enable flag and
//! passphrase, so the exposed playback leg can be encrypted without cutting over
//! every encoder. OME already binds them separately.
//!
//! OME still only reads its SRT passphrase at process startup (v0.20.5 can't
//! hot-reload `Server.xml`; its REST API rejects bind changes), so applying a
//! change is unavoidably an **OME restart**. To bridge that: the backend writes
//! the current passphrases to `<data>/srt.env`; the `ome_start.sh` wrapper sources
//! that file before launching OME, and the toggle handler restarts the `ome`
//! supervisor program. A restart drops every stream briefly and is a hard cutover
//! (affected encoders/players must switch to the new passphrase) — surfaced in
//! the UI, which batches both legs behind one Apply → one restart.
//!
//! Source of truth: the `settings` table (no `.env` involvement — the admin
//! toggle owns it entirely).

use crate::credentials as cred;
use crate::error::AppError;
use rand::RngExt;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

pub const KEY_INGEST_ENABLED: &str = "srt_ingest_enabled";
pub const KEY_PLAYBACK_ENABLED: &str = "srt_playback_enabled";
pub const KEY_INGEST_PASSPHRASE: &str = "srt_ingest_passphrase";
pub const KEY_PLAYBACK_PASSPHRASE: &str = "srt_playback_passphrase";

/// Pre-split combined flag (gh #208 first cut). Migrated to the per-leg flags on
/// startup, then deleted. Kept only as a migration source.
const KEY_LEGACY_ENABLED: &str = "srt_encryption_enabled";

/// SRT AES key length in bytes (`SRTO_PBKEYLEN`). Fixed at 16 (AES-128); the
/// admin toggle generates the passphrases, so there is no need to expose the
/// key length.
pub const PBKEYLEN: u32 = 16;

/// Effective SRT encryption config, resolved from the DB. Each leg's passphrase
/// is `Some` only when that leg is enabled, so callers never advertise a stale
/// secret or leave a disabled leg's bind encrypted.
pub struct EffectiveSrt {
    pub ingest_enabled: bool,
    pub playback_enabled: bool,
    pub ingest_passphrase: Option<String>,
    pub playback_passphrase: Option<String>,
    pub pbkeylen: u32,
}

/// A fresh random SRT passphrase: 32 hex chars, comfortably inside libsrt's
/// 10–79 range and free of shell-special characters (it is written into
/// `srt.env` and sourced by `ome_start.sh`).
fn gen_passphrase() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Resolve the effective SRT encryption config from the DB. Absent flag ⇒
/// that leg disabled (a fresh install starts unencrypted; the admin enables it).
pub fn resolve(conn: &rusqlite::Connection) -> EffectiveSrt {
    let ingest_enabled = cred::settings_get(conn, KEY_INGEST_ENABLED).as_deref() == Some("1");
    let playback_enabled = cred::settings_get(conn, KEY_PLAYBACK_ENABLED).as_deref() == Some("1");
    EffectiveSrt {
        ingest_enabled,
        playback_enabled,
        ingest_passphrase: ingest_enabled
            .then(|| cred::settings_get(conn, KEY_INGEST_PASSPHRASE))
            .flatten()
            .filter(|s| !s.is_empty()),
        playback_passphrase: playback_enabled
            .then(|| cred::settings_get(conn, KEY_PLAYBACK_PASSPHRASE))
            .flatten()
            .filter(|s| !s.is_empty()),
        pbkeylen: PBKEYLEN,
    }
}

/// Enable or disable one SRT leg in the DB. On the first enable it generates and
/// stores that leg's passphrase (reused thereafter, so re-enabling keeps the same
/// secret). Disabling keeps the stored passphrase but flips the flag, which
/// [`resolve`] turns into a `None` passphrase.
fn set_leg(
    conn: &rusqlite::Connection,
    key_enabled: &str,
    key_passphrase: &str,
    enabled: bool,
) -> Result<(), rusqlite::Error> {
    if enabled
        && cred::settings_get(conn, key_passphrase)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        cred::settings_set(conn, key_passphrase, &gen_passphrase())?;
    }
    cred::settings_set(conn, key_enabled, if enabled { "1" } else { "0" })
}

/// Path of the env file OME's `ome_start.sh` wrapper sources at startup.
fn env_file_path(data_path: &str) -> String {
    format!("{}/srt.env", data_path)
}

/// Write `<data>/srt.env` reflecting `eff`, atomically and `0600` (it holds the
/// passphrase). Empty passphrases mean encryption off — OME's `${env:...}`
/// default. `ome_start.sh` sources this; generated passphrases are hex, but the
/// values are single-quoted defensively so sourcing stays safe regardless.
pub fn write_env_file(data_path: &str, eff: &EffectiveSrt) -> std::io::Result<()> {
    let ingest = eff.ingest_passphrase.as_deref().unwrap_or("");
    let playback = eff.playback_passphrase.as_deref().unwrap_or("");
    let body = format!(
        "# Generated by the backend (gh #208). Sourced by ome_start.sh.\n\
         export SRT_INGEST_PASSPHRASE={}\n\
         export SRT_PLAYBACK_PASSPHRASE={}\n\
         export SRT_PBKEYLEN={}\n",
        sh_squote(ingest),
        sh_squote(playback),
        eff.pbkeylen,
    );

    let path = env_file_path(data_path);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = format!("{}.tmp", path);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

/// POSIX single-quote a string for safe `source`ing (`'` → `'\''`).
fn sh_squote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Restart the OME supervisor program so it re-reads `srt.env`. No-op when
/// `STREAM_DISABLE_OME_RESTART` is set (integration tests) or when `supervisorctl`
/// is unavailable (dev outside the container) — the DB state is already persisted,
/// so the change applies on the next OME start regardless.
pub fn restart_ome() {
    if std::env::var("STREAM_DISABLE_OME_RESTART").is_ok() {
        tracing::debug!("[srt] OME restart skipped (STREAM_DISABLE_OME_RESTART)");
        return;
    }
    match std::process::Command::new("supervisorctl")
        .args([
            "-c",
            "/etc/supervisor/conf.d/supervisord.conf",
            "restart",
            "ome",
        ])
        .output()
    {
        Ok(o) if o.status.success() => {
            tracing::info!("[srt] restarted OME to apply SRT encryption change")
        }
        Ok(o) => tracing::error!(
            "[srt] `supervisorctl restart ome` failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => tracing::error!("[srt] could not run supervisorctl: {e}"),
    }
}

/// Startup hook: migrate the pre-split combined flag (if present), then write
/// `srt.env` from the current DB state so the OME process — which starts after
/// the backend — reads the current passphrases.
pub fn init_startup(conn: &rusqlite::Connection, data_path: &str) {
    migrate_legacy_flag(conn);
    let eff = resolve(conn);
    if let Err(e) = write_env_file(data_path, &eff) {
        tracing::error!("[srt] failed to write srt.env: {e}");
    }
}

/// One-time migration from the combined `srt_encryption_enabled` flag to the
/// per-leg flags: copy its value into both legs (preserving existing state) and
/// delete it. Idempotent — a no-op once the legacy key is gone.
fn migrate_legacy_flag(conn: &rusqlite::Connection) {
    if let Some(v) = cred::settings_get(conn, KEY_LEGACY_ENABLED) {
        for key in [KEY_INGEST_ENABLED, KEY_PLAYBACK_ENABLED] {
            if cred::settings_get(conn, key).is_none() {
                let _ = cred::settings_set(conn, key, &v);
            }
        }
        let _ = cred::settings_del(conn, KEY_LEGACY_ENABLED);
        tracing::info!("[srt] migrated combined encryption flag into per-leg flags");
    }
}

/// Shared JSON body for the admin `srt-config` GET and `srt-encryption` POST.
pub fn to_json(eff: &EffectiveSrt) -> serde_json::Value {
    serde_json::json!({
        "ingestEnabled": eff.ingest_enabled,
        "playbackEnabled": eff.playback_enabled,
        "ingestPassphrase": eff.ingest_passphrase,
        "playbackPassphrase": eff.playback_passphrase,
        "pbkeylen": eff.pbkeylen,
    })
}

/// Apply the desired enabled state for both legs: persist the flags (generating a
/// leg's passphrase on its first enable), rewrite `srt.env`, and return the new
/// effective config. The caller restarts OME once (kept separate so it can run
/// outside the DB critical section).
pub fn apply(
    conn: &rusqlite::Connection,
    data_path: &str,
    ingest: bool,
    playback: bool,
) -> Result<EffectiveSrt, AppError> {
    set_leg(conn, KEY_INGEST_ENABLED, KEY_INGEST_PASSPHRASE, ingest)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    set_leg(
        conn,
        KEY_PLAYBACK_ENABLED,
        KEY_PLAYBACK_PASSPHRASE,
        playback,
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;
    let eff = resolve(conn);
    write_env_file(data_path, &eff).map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(eff)
}
