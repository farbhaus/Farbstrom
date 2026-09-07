use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    /// The request is well-formed but collides with existing state — e.g.
    /// replacing a library file with content another row already holds, which
    /// the UNIQUE `content_hash` index forbids. Distinct from `BadRequest` so
    /// the client can tell "you sent something wrong" from "this clashes".
    Conflict(String),
    Gone(String),
    Internal(String),
    BadGateway(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Author-written messages are safe to return. Internal/BadGateway
        // carry raw dependency error strings (DB, HTTP, JWT) — log those and
        // return a generic body so we don't leak implementation details to
        // whoever is poking the API.
        let (status, code, public_message) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, "FORBIDDEN", msg),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg),
            AppError::Gone(msg) => (StatusCode::GONE, "GONE", msg),
            AppError::Internal(msg) => {
                tracing::error!(error = %msg, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL",
                    "Internal server error".to_string(),
                )
            }
            AppError::BadGateway(msg) => {
                tracing::error!(error = %msg, "upstream service error");
                (
                    StatusCode::BAD_GATEWAY,
                    "BAD_GATEWAY",
                    "Upstream service unavailable".to_string(),
                )
            }
        };
        (
            status,
            Json(json!({ "error": public_message, "code": code })),
        )
            .into_response()
    }
}

impl From<r2d2::Error> for AppError {
    fn from(e: r2d2::Error) -> Self {
        AppError::Internal(format!("Database pool error: {}", e))
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(format!("Database error: {}", e))
    }
}

impl From<bcrypt::BcryptError> for AppError {
    fn from(e: bcrypt::BcryptError) -> Self {
        AppError::Internal(format!("Bcrypt error: {}", e))
    }
}

impl From<jsonwebtoken::errors::Error> for AppError {
    fn from(e: jsonwebtoken::errors::Error) -> Self {
        // Don't leak parser internals (which key kind, which claim failed,
        // signature vs expiry) to whoever is poking at the token. Log the
        // detail; return a generic message.
        tracing::warn!(error = %e, "jwt decode/encode failed");
        AppError::Unauthorized("Invalid or expired token".into())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::BadGateway(format!("HTTP request error: {}", e))
    }
}
