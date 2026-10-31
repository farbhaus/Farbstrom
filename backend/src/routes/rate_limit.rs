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

/// A per-IP rate limit, as applied to a route with `.layer(...)`.
pub type RateLimitLayer =
    GovernorLayer<SmartIpKeyExtractor, ::governor::middleware::NoOpMiddleware, axum::body::Body>;

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
pub fn login_layer() -> RateLimitLayer {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    let config = CFG
        .get_or_init(|| build_config(Duration::from_secs(12), 2))
        .clone();
    GovernorLayer::new(config)
}

/// 30 requests/minute per IP, burst 10 — the shared shape of the two
/// non-login buckets below. They are deliberately *separate* `OnceLock`s
/// despite identical parameters: sharing one would let a room-join flood
/// exhaust the passkey budget and lock the operator out of their own admin.
fn thirty_per_minute() -> Arc<SmartConfig> {
    build_config(Duration::from_secs(2), 10)
}

/// For `POST /api/public/rooms/:slug/join`.
pub fn join_layer() -> RateLimitLayer {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    let config = CFG.get_or_init(thirty_per_minute).clone();
    GovernorLayer::new(config)
}

/// For the WebAuthn passkey login endpoints — a single passkey login is
/// start+finish (2 requests, with a human-paced OS prompt between), so the
/// strict `login_layer` budget made legitimate retries fail. Separate from both
/// `login_layer` and `join_layer`; these endpoints are already gated by a
/// server-issued challenge id.
pub fn passkey_layer() -> RateLimitLayer {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    let config = CFG.get_or_init(thirty_per_minute).clone();
    GovernorLayer::new(config)
}

/// 60 requests/minute per IP, burst 30 — the shape of the buckets below, each a
/// route a whole room hits at once: opening the room socket, the waiting room's
/// event stream, the room's public info, an upload. The burst is sized for an
/// office behind one NAT address arriving together; the sustained rate is still
/// a hard ceiling on one client opening them in a loop, which nothing did before
/// (one waiting joiner held 150 event streams open). Separate `OnceLock`s, like
/// the two above, so a flood on one cannot spend another's allowance.
fn sixty_per_minute() -> Arc<SmartConfig> {
    build_config(Duration::from_secs(1), 30)
}

/// For the `/ws/room/:slug` upgrade. A viewer opens one socket and reconnects
/// with a backoff; Farbplay holds one too.
pub fn ws_layer() -> RateLimitLayer {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    GovernorLayer::new(CFG.get_or_init(sixty_per_minute).clone())
}

/// For `/waiting/events/:pid`. Farbplay re-subscribes 2 s after a dropped
/// stream, i.e. at most 30/minute, which this leaves room for.
pub fn waiting_events_layer() -> RateLimitLayer {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    GovernorLayer::new(CFG.get_or_init(sixty_per_minute).clone())
}

/// For `/:slug/info`, the one unauthenticated lookup by room slug — so also the
/// one a slug guesser would have to go through.
pub fn room_info_layer() -> RateLimitLayer {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    GovernorLayer::new(CFG.get_or_init(sixty_per_minute).clone())
}

/// For participant uploads. Bytes are bounded separately (the room quota and the
/// free-space floor in `routes::files`); this bounds the count.
pub fn upload_layer() -> RateLimitLayer {
    static CFG: OnceLock<Arc<SmartConfig>> = OnceLock::new();
    GovernorLayer::new(CFG.get_or_init(sixty_per_minute).clone())
}
