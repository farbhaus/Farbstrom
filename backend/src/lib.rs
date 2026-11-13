/// The release this binary was built from, e.g. `1.2.2`. Compiled in from
/// `Cargo.toml`, which `scripts/bump-version.sh` keeps equal to the release
/// tag — the release workflow refuses to publish a tag it does not match.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod app;
pub mod auth;
pub mod config;
pub mod credentials;
pub mod db;
pub mod error;
pub mod events;
pub mod jwt;
pub mod livekit;
pub mod playback;
pub mod presence;
pub mod routes;
pub mod signed_policy;
pub mod srt;
pub mod state;
pub mod tasks;
pub mod time;
pub mod uploads;
pub mod ws;
