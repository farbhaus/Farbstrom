use crate::config::AppConfig;
use crate::db::DbPool;
use crate::events::EventChannels;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use uuid::Uuid;
use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration};
use webauthn_rs::Webauthn;

/// In-flight WebAuthn ceremony state, keyed by an opaque id handed to the
/// browser. Entries older than this TTL are swept lazily on the next access;
/// a lost entry (e.g. across a restart) just means the user retries.
pub const CEREMONY_TTL_SECS: u64 = 300;

pub type RegStates = Mutex<HashMap<Uuid, (Instant, PasskeyRegistration)>>;
pub type AuthStates = Mutex<HashMap<Uuid, (Instant, PasskeyAuthentication)>>;

pub type SharedState = Arc<AppState>;

/// Last sample of cumulative network counters and CPU jiffies, used by the
/// metrics handler to compute rates/percentages over the interval since the
/// previous request.
#[derive(Default)]
pub struct MetricsSamples {
    pub net: Option<(u64, u64, Instant)>, // (rx_total, tx_total, ts) for primary iface
    pub cpu: Option<(u64, u64)>,          // (idle_total, total_total) jiffies — aggregate
    pub cpu_per: Vec<(u64, u64)>,         // per-core (idle, total) jiffies, index = core
}

pub struct AppState {
    pub db: DbPool,
    pub events: EventChannels,
    pub config: AppConfig,
    pub http_client: reqwest::Client,
    /// Env-derived bootstrap bcrypt hash. Used only when no custom password
    /// has been set via the Settings tab (see `credentials::current_password_hash`).
    pub admin_password_hash: String,
    /// Generation every admin JWT is stamped with. `AdminAuth` rejects tokens
    /// carrying an older one, which is how a password change or an explicit
    /// "sign out other devices" revokes live sessions.
    ///
    /// A cache of the `admin_token_version` settings row, loaded at startup and
    /// updated by `credentials::bump_token_version`. It lives here rather than
    /// being read per request because the check runs on every admin request and
    /// must not touch the DB — a single process owns it, so the two agree.
    pub admin_token_version: std::sync::atomic::AtomicU64,
    pub metrics_samples: Mutex<MetricsSamples>,
    pub webauthn: Arc<Webauthn>,
    pub passkey_reg: RegStates,
    pub passkey_auth: AuthStates,
}
