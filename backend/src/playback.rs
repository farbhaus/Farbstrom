//! Who may *watch* a stream in the browser.
//!
//! OME publishes every stream under its key's `playback_id`, never under the
//! ingest key: the admission webhook renames each incoming stream with
//! `new_url` (`routes::webhook`). What a viewer is handed therefore cannot push
//! video, and the ingest key stays with the admin.
//!
//! Browser playback (WebRTC and LL-HLS) is admission-gated too — `<Publishers>`
//! in the webhook block of `Server.xml` — and OME asks before starting every
//! playback session. A session is allowed for:
//!
//! - an admitted, non-kicked participant of a running room whose stream key is
//!   the one being played, identified by the same `participantId` + `token`
//!   query pair the file endpoints take; or
//! - an admin preview carrying `apt`, a short-lived token minted by
//!   `GET /api/ome/playback-token`.
//!
//! OME asks once per session — LL-HLS chunklists ride on its own `session`
//! parameter — so a kick or a room ending refuses *new* sessions, and one that
//! is already playing runs on until the viewer reconnects. SRT playback is gated
//! separately, by SignedPolicy (`signed_policy`), over the same `playback_id`.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use rusqlite::{params, OptionalExtension};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Lifetime of an admin preview token. OME checks it when a playback session
/// starts, so it only has to outlast the click that opens the preview.
pub const ADMIN_TOKEN_TTL_SECS: u64 = 300;

/// Domain separator, so an admin playback signature can never be mistaken for
/// any other HMAC computed with the same secret.
const ADMIN_TOKEN_DOMAIN: &str = "farbstrom-admin-playback";

fn admin_signature(secret: &str, expires_at: u64) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(format!("{ADMIN_TOKEN_DOMAIN}|{expires_at}").as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// Mint an admin preview token, `<expires_at_secs>.<signature>`.
pub fn mint_admin_token(secret: &str, now_secs: u64) -> String {
    let expires_at = now_secs + ADMIN_TOKEN_TTL_SECS;
    format!("{expires_at}.{}", admin_signature(secret, expires_at))
}

/// True for an unexpired token this server minted.
pub fn verify_admin_token(secret: &str, token: &str, now_secs: u64) -> bool {
    let Some((expires_at, signature)) = token.split_once('.') else {
        return false;
    };
    let Ok(expires_at) = expires_at.parse::<u64>() else {
        return false;
    };
    expires_at > now_secs
        && admin_signature(secret, expires_at)
            .as_bytes()
            .ct_eq(signature.as_bytes())
            .into()
}

/// Whether `(participant_id, token)` may start a playback session of the
/// stream OME publishes as `playback_id`: admitted, not kicked, in a room that
/// has neither ended nor expired and currently points at that stream.
pub fn participant_may_play(
    conn: &rusqlite::Connection,
    participant_id: &str,
    token: &str,
    playback_id: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM participants p \
         JOIN rooms r ON r.id = p.room_id \
         JOIN stream_keys sk ON sk.id = r.stream_key_id \
         WHERE p.id = ?1 AND p.token = ?2 \
           AND p.is_admitted = 1 AND p.is_kicked = 0 \
           AND r.status != 'ended' \
           AND (r.expires_at IS NULL OR r.expires_at > CURRENT_TIMESTAMP) \
           AND sk.playback_id = ?3",
        params![participant_id, token, playback_id],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-that-is-at-least-thirty-two-characters-long";

    #[test]
    fn a_fresh_admin_token_verifies() {
        let token = mint_admin_token(SECRET, 1_000);
        assert!(verify_admin_token(SECRET, &token, 1_000));
        assert!(verify_admin_token(
            SECRET,
            &token,
            1_000 + ADMIN_TOKEN_TTL_SECS - 1
        ));
    }

    #[test]
    fn an_expired_admin_token_is_refused() {
        let token = mint_admin_token(SECRET, 1_000);
        assert!(!verify_admin_token(
            SECRET,
            &token,
            1_000 + ADMIN_TOKEN_TTL_SECS
        ));
    }

    #[test]
    fn a_tampered_or_foreign_admin_token_is_refused() {
        let token = mint_admin_token(SECRET, 1_000);
        // Extending the expiry invalidates the signature over it.
        let (_, signature) = token.split_once('.').unwrap();
        assert!(!verify_admin_token(
            SECRET,
            &format!("999999.{signature}"),
            1_000
        ));
        // Another server's secret.
        assert!(!verify_admin_token(
            "some-other-secret-of-sufficient-length!!",
            &token,
            1_000
        ));
        for junk in ["", ".", "abc", "123", "123.", ".sig", "x.y"] {
            assert!(!verify_admin_token(SECRET, junk, 1_000), "{junk:?}");
        }
    }
}
