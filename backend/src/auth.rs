use crate::state::AppState;
use axum::{
    extract::{FromRequestParts, Query},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
pub struct AdminClaims {
    pub admin: bool,
    pub exp: usize,
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

/// Extractor that validates JWT Bearer token from Authorization header.
pub struct AdminAuth(pub AdminClaims);

impl FromRequestParts<Arc<AppState>> for AdminAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok());

        let header_token = match auth_header {
            Some(h) if h.starts_with("Bearer ") => Some(h[7..].to_string()),
            _ => None,
        };

        // Fall back to ?token=… query param so <img>/<video>/<iframe>/window.open
        // can hit authenticated endpoints (admin preview/download) without a
        // way to attach headers.
        let query_token = Query::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .ok()
            .and_then(|Query(m)| m.get("token").cloned());

        let token = match header_token.or(query_token) {
            Some(t) => t,
            None => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "Unauthorised" })),
                )
                    .into_response());
            }
        };

        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp"]);

        match decode::<AdminClaims>(
            &token,
            &DecodingKey::from_secret(state.config.jwt_secret.as_bytes()),
            &validation,
        ) {
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
            Ok(data)
                if data.claims.admin
                    && data.claims.ver
                        == state
                            .admin_token_version
                            .load(std::sync::atomic::Ordering::SeqCst) =>
            {
                Ok(AdminAuth(data.claims))
            }
            _ => Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Invalid or expired token" })),
            )
                .into_response()),
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
pub fn create_admin_token(
    secret: &str,
    version: u64,
    ttl_secs: u64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_add(ttl_secs) as usize;

    let claims = AdminClaims {
        admin: true,
        exp,
        ver: version,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}
