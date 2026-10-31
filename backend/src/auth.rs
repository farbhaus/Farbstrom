use crate::state::AppState;
use axum::{
    extract::{FromRequestParts, Query},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
pub struct AdminClaims {
    pub admin: bool,
    pub exp: u64,
    /// Generation this token was minted under. `AdminAuth` rejects it once the
    /// current generation moves on, which is what makes a password change (or
    /// an explicit sign-out) actually revoke live sessions rather than leaving
    /// them valid for the rest of the 7-day expiry.
    ///
    /// `serde(default)` so a token issued before this claim existed decodes as
    /// generation 0 — the same value a fresh install starts at — and an upgrade
    /// therefore does not sign the operator out for no reason.
    #[serde(default)]
    pub ver: u64,
}

/// Extractor that validates JWT Bearer token from Authorization header. A unit
/// struct: handlers only ever need to know the caller *is* the admin.
///
/// Header only. It used to accept `?token=<admin JWT>` too, so `<img>` and
/// `window.open` could reach the library's preview and download — which put a
/// 7- or 90-day session token in URLs, and so in browser history. Those two
/// routes take [`AdminFileAccess`] now; nothing else ever needed the fallback.
pub struct AdminAuth;

fn unauthorised(message: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": message }))).into_response()
}

/// Whether the request carries a valid, current admin session as a Bearer
/// token. `None` when it carries none at all.
fn bearer_session(parts: &Parts, state: &AppState) -> Option<bool> {
    let token = parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))?;
    let now_secs = crate::time::now_ms() / 1000;
    Some(
        match crate::jwt::verify::<AdminClaims>(token, state.config.jwt_secret.as_bytes(), now_secs)
        {
            // Defence-in-depth: the only token type signed with jwt_secret
            // today is the admin token, but reject anything without the
            // `admin: true` claim so a future non-admin token signed with
            // the same secret cannot silently escalate.
            //
            // The generation check is what makes revocation work: a token
            // minted before the last password change (or "sign out other
            // devices") carries an older `ver` and is refused here, however
            // long its expiry still has to run. Reading the cached counter
            // costs no I/O, which matters on a path every admin request takes.
            Ok(claims) => claims.admin && claims.ver == current_version(state),
            Err(_) => false,
        },
    )
}

fn current_version(state: &AppState) -> u64 {
    state
        .admin_token_version
        .load(std::sync::atomic::Ordering::SeqCst)
}

impl FromRequestParts<Arc<AppState>> for AdminAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        match bearer_session(parts, state) {
            Some(true) => Ok(AdminAuth),
            Some(false) => Err(unauthorised("Invalid or expired token")),
            None => Err(unauthorised("Unauthorised")),
        }
    }
}

/// How long a file link (`?ft=`) stays usable. Each one is minted for the page
/// that renders it, and a preview or download is a single request.
pub const FILE_LINK_TTL_SECS: u64 = 600;

/// Domain separator, so a file-link signature can never pass for any other HMAC
/// computed with the same secret (the admin playback token is one).
const FILE_LINK_DOMAIN: &str = "farbstrom-admin-file-link";

fn file_link_signature(secret: &str, version: u64, expires_at: u64) -> String {
    use base64::Engine;
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(format!("{FILE_LINK_DOMAIN}|{version}|{expires_at}").as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// A capability to fetch library files and nothing else: `<expires>.<sig>`,
/// stamped with the session generation so "sign out other devices" (or a
/// password change) ends outstanding links along with the sessions.
pub fn mint_file_link_token(secret: &str, version: u64, now_secs: u64) -> String {
    let expires_at = now_secs + FILE_LINK_TTL_SECS;
    format!(
        "{expires_at}.{}",
        file_link_signature(secret, version, expires_at)
    )
}

pub fn verify_file_link_token(secret: &str, version: u64, token: &str, now_secs: u64) -> bool {
    use subtle::ConstantTimeEq;
    let Some((expires_at, signature)) = token.split_once('.') else {
        return false;
    };
    let Ok(expires_at) = expires_at.parse::<u64>() else {
        return false;
    };
    expires_at > now_secs
        && file_link_signature(secret, version, expires_at)
            .as_bytes()
            .ct_eq(signature.as_bytes())
            .into()
}

/// Access to a library file's bytes (preview / download): the Bearer session,
/// or a file-link token in `?ft=` for the `<img>`, `<video>` and download
/// requests that cannot carry a header.
pub struct AdminFileAccess;

impl FromRequestParts<Arc<AppState>> for AdminFileAccess {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if bearer_session(parts, state) == Some(true) {
            return Ok(AdminFileAccess);
        }
        let link = Query::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .ok()
            .and_then(|Query(m)| m.get("ft").cloned());
        let now_secs = crate::time::now_ms() / 1000;
        match link {
            Some(t)
                if verify_file_link_token(
                    &state.config.jwt_secret,
                    current_version(state),
                    &t,
                    now_secs,
                ) =>
            {
                Ok(AdminFileAccess)
            }
            _ => Err(unauthorised("Invalid or expired link")),
        }
    }
}

/// Default admin session lifetime — the "this might not be my computer" case.
pub const ADMIN_TOKEN_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// Lifetime when the operator ticks "trust this browser" at sign-in.
///
/// A token this long-lived is only reasonable because it can now be revoked:
/// changing the password or hitting "sign out other devices" bumps the
/// generation in [`AdminClaims::ver`] and every outstanding token dies at once.
/// Without that, this would be a 90-day liability sitting in localStorage.
pub const ADMIN_TOKEN_TRUSTED_TTL_SECS: u64 = 90 * 24 * 60 * 60;

/// Mint an admin token valid for `ttl_secs`, stamped with the current
/// generation `version`.
pub fn create_admin_token(secret: &str, version: u64, ttl_secs: u64) -> String {
    let exp = (crate::time::now_ms() / 1000).saturating_add(ttl_secs);
    let claims = AdminClaims {
        admin: true,
        exp,
        ver: version,
    };
    crate::jwt::sign(&claims, secret.as_bytes())
}
