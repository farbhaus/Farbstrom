use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tower_governor::{
    governor::{GovernorConfig, GovernorConfigBuilder},
    key_extractor::SmartIpKeyExtractor,
    GovernorLayer,
};

/// Rate limiting can be disabled by setting `STREAM_DISABLE_RATE_LIMIT=1`.
/// Integration tests set this so the limiter does not reject `TestServer`
/// requests that lack a `ConnectInfo` extension.
pub fn enabled() -> bool {
    std::env::var("STREAM_DISABLE_RATE_LIMIT").is_err()
}

type SmartConfig = GovernorConfig<SmartIpKeyExtractor, ::governor::middleware::NoOpMiddleware>;

fn build_config(period: Duration, burst: u32) -> Arc<SmartConfig> {
    Arc::new(
        GovernorConfigBuilder::default()
            .period(period)
            .burst_size(burst)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("governor config must be valid"),
    )
}

/// 5 requests/minute per IP, burst 2. For `POST /api/auth/login`.
pub fn login_layer() -> GovernorLayer<SmartIpKeyExtractor, ::governor::middleware::NoOpMiddleware> {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    let config = CFG
        .get_or_init(|| build_config(Duration::from_secs(12), 2))
        .clone();
    GovernorLayer { config }
}

/// 30 requests/minute per IP, burst 10 — the shared shape of the two
/// non-login buckets below. They are deliberately *separate* `OnceLock`s
/// despite identical parameters: sharing one would let a room-join flood
/// exhaust the passkey budget and lock the operator out of their own admin.
fn thirty_per_minute() -> Arc<SmartConfig> {
    build_config(Duration::from_secs(2), 10)
}

/// For `POST /api/public/rooms/:slug/join`.
pub fn join_layer() -> GovernorLayer<SmartIpKeyExtractor, ::governor::middleware::NoOpMiddleware> {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    let config = CFG.get_or_init(thirty_per_minute).clone();
    GovernorLayer { config }
}

/// For the WebAuthn passkey login endpoints — a single passkey login is
/// start+finish (2 requests, with a human-paced OS prompt between), so the
/// strict `login_layer` budget made legitimate retries fail. Separate from both
/// `login_layer` and `join_layer`; these endpoints are already gated by a
/// server-issued challenge id.
pub fn passkey_layer() -> GovernorLayer<SmartIpKeyExtractor, ::governor::middleware::NoOpMiddleware>
{
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    let config = CFG.get_or_init(thirty_per_minute).clone();
    GovernorLayer { config }
}
