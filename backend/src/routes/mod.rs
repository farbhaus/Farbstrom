pub mod admin_files;
pub mod admin_settings;
pub mod auth;
pub mod branding;
pub mod files;
pub mod metrics;
pub mod ome;
pub mod pages;
pub mod rate_limit;
pub mod rooms;
pub mod rooms_public;
pub mod stream_keys;
pub mod watch;
pub mod webhook;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

pub fn build_router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .nest("/api/auth", auth::router())
        .nest("/api/stream-keys", stream_keys::router())
        .nest("/api/rooms", rooms::router())
        .nest("/api/ome", ome::router())
        .nest("/api/public/rooms", rooms_public::router())
        .nest("/api/public/rooms", files::router())
        .nest("/api/watch", watch::router())
        .nest("/api/webhook/admission", webhook::router())
        .nest("/api/branding", branding::public_router())
        .nest("/api/admin/branding", branding::admin_router())
        .nest("/api/admin/metrics", metrics::router())
        .nest("/api/admin/files", admin_files::files_router())
        .nest("/api/admin/rooms", admin_files::room_assign_router())
        .nest("/api/admin/settings", admin_settings::router());

    api.with_state(state)
}
