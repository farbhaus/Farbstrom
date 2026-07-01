use std::env;

#[derive(Clone)]
pub struct AppConfig {
    pub jwt_secret: String,
    pub ome_webhook_secret: String,
    /// HMAC-SHA1 key for minting OME SignedPolicy streamids (SRT playback via
    /// the Farbplay room-link flow). Must match `<SignedPolicy><SecretKey>` in
    /// the OME `Server.xml`.
    pub ome_signed_policy_secret: String,
    pub ome_api_url: String,
    pub ome_api_token: String,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
    pub livekit_internal_url: String,
    pub livekit_url: String,
    pub port: u16,
    pub db_path: String,
    pub data_path: String,
    /// Public origin the admin panel is served from, e.g.
    /// `https://stream.yourdomain.com`. Used as the WebAuthn relying-party
    /// origin; the RP ID is the host parsed from it. Set to
    /// `http://localhost:4001` for local dev so passkeys work.
    pub public_origin: String,
    /// Public hostname native SRT clients (Farbplay) connect to. Defaults to
    /// the host parsed from `PUBLIC_ORIGIN`.
    pub srt_public_host: String,
    /// Public UDP port for OME SRT playback. Defaults to `9998`.
    pub srt_public_port: u16,
    /// SRT latency (ms) advertised to clients. Defaults to `500`.
    pub srt_latency_ms: u32,
    /// SRT ingest encryption passphrase (`SRTO_PASSPHRASE` on the OME provider,
    /// port 9999). `None` ⇒ ingest is unencrypted (backward compatible). When
    /// set, 10–79 chars (libsrt) and exposed to the admin UI so the ingest URL
    /// carries `&passphrase=…`.
    pub srt_ingest_passphrase: Option<String>,
    /// SRT playback encryption passphrase (`SRTO_PASSPHRASE` on the OME
    /// publisher, port 9998). Returned to Farbplay by `/api/watch/:slug`.
    /// `None` ⇒ playback is unencrypted.
    pub srt_playback_passphrase: Option<String>,
    /// SRT AES key length in bytes (`SRTO_PBKEYLEN`): 16/24/32. Default 16.
    pub srt_pbkeylen: u32,
}

/// Extract the host portion of a URL-ish origin (`https://host:port/path` →
/// `host`), without pulling in the `url` crate. Strips an optional scheme,
/// then any port and path.
fn host_from_origin(origin: &str) -> String {
    let after_scheme = origin.split("://").last().unwrap_or(origin);
    after_scheme
        .split(['/', ':'])
        .next()
        .unwrap_or(after_scheme)
        .to_string()
}

/// Require an env var to be set, panicking with a clear message if not.
fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("FATAL: {} must be set", name))
}

/// Require an env var to be set and meet a minimum length.
fn required_min_len(name: &str, min_len: usize) -> String {
    let value = required(name);
    if value.len() < min_len {
        panic!("FATAL: {} must be at least {} chars", name, min_len);
    }
    value
}

/// Read an optional SRT passphrase. Unset/empty ⇒ `None` (encryption off on that
/// leg). A present value must be 10–79 chars, the libsrt `SRTO_PASSPHRASE` range
/// — fail fast so a misconfigured passphrase can't silently break every SRT
/// connection at cutover.
fn optional_srt_passphrase(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(v) if !v.is_empty() => {
            if !(10..=79).contains(&v.len()) {
                panic!("FATAL: {} must be 10-79 characters", name);
            }
            Some(v)
        }
        _ => None,
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        // Signing keys — all used as HMAC secrets, enforce 32-char minimum.
        let jwt_secret = required_min_len("JWT_SECRET", 32);
        let ome_webhook_secret = required_min_len("OME_WEBHOOK_SECRET", 32);
        let ome_signed_policy_secret = required_min_len("OME_SIGNED_POLICY_SECRET", 32);
        let livekit_api_secret = required_min_len("LIVEKIT_API_SECRET", 32);
        let ome_api_token = required_min_len("OME_API_TOKEN", 32);

        // Admin password is bcrypt-hashed at startup; enforce a sensible minimum.
        let _admin_password = required_min_len("ADMIN_PASSWORD", 12);

        // LiveKit API key is an identifier (becomes the `iss` JWT claim), not a
        // secret — require presence but don't enforce length.
        let livekit_api_key = required("LIVEKIT_API_KEY");

        let public_origin =
            env::var("PUBLIC_ORIGIN").unwrap_or_else(|_| "http://localhost:4001".into());

        // SRT encryption (opt-in per leg). Must match OME's Server.xml, which
        // reads the same env vars via ${env:...}.
        let srt_ingest_passphrase = optional_srt_passphrase("SRT_INGEST_PASSPHRASE");
        let srt_playback_passphrase = optional_srt_passphrase("SRT_PLAYBACK_PASSPHRASE");
        let srt_pbkeylen = env::var("SRT_PBKEYLEN")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(16);
        if ![16, 24, 32].contains(&srt_pbkeylen) {
            panic!("FATAL: SRT_PBKEYLEN must be 16, 24, or 32");
        }

        Self {
            jwt_secret,
            ome_webhook_secret,
            ome_signed_policy_secret,
            ome_api_url: env::var("OME_API_URL")
                .unwrap_or_else(|_| "http://localhost:8081/v1".into()),
            ome_api_token,
            livekit_api_key,
            livekit_api_secret,
            livekit_internal_url: env::var("LIVEKIT_INTERNAL_URL")
                .unwrap_or_else(|_| "http://localhost:7880".into()),
            livekit_url: env::var("LIVEKIT_URL").unwrap_or_else(|_| "ws://localhost:7880".into()),
            port: env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(4001),
            db_path: env::var("DB_PATH").unwrap_or_else(|_| "/data/stream.db".into()),
            data_path: env::var("DATA_PATH").unwrap_or_else(|_| "/data".into()),
            srt_public_host: env::var("SRT_PUBLIC_HOST")
                .unwrap_or_else(|_| host_from_origin(&public_origin)),
            srt_public_port: env::var("SRT_PUBLIC_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(9998),
            srt_latency_ms: env::var("SRT_LATENCY_MS")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(500),
            srt_ingest_passphrase,
            srt_playback_passphrase,
            srt_pbkeylen,
            public_origin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::optional_srt_passphrase;

    // Each test uses a unique env var name so the shared process env doesn't
    // race across parallel tests.

    #[test]
    fn passphrase_unset_is_none() {
        std::env::remove_var("TEST_SRT_UNSET");
        assert!(optional_srt_passphrase("TEST_SRT_UNSET").is_none());
    }

    #[test]
    fn passphrase_empty_is_none() {
        std::env::set_var("TEST_SRT_EMPTY", "");
        assert!(optional_srt_passphrase("TEST_SRT_EMPTY").is_none());
        std::env::remove_var("TEST_SRT_EMPTY");
    }

    #[test]
    fn passphrase_valid_length_is_some() {
        std::env::set_var("TEST_SRT_OK", "0123456789"); // exactly 10 (min)
        assert_eq!(
            optional_srt_passphrase("TEST_SRT_OK").as_deref(),
            Some("0123456789")
        );
        std::env::remove_var("TEST_SRT_OK");
    }

    #[test]
    #[should_panic(expected = "10-79 characters")]
    fn passphrase_too_short_panics() {
        std::env::set_var("TEST_SRT_SHORT", "short"); // 5 chars
        let _ = optional_srt_passphrase("TEST_SRT_SHORT");
    }
}
