//! OME SignedPolicy token minting for the SRT publisher (playback, port 9998).
//!
//! `<SignedPolicy>` is enabled for `<Publishers>srt` in
//! `ome/origin_conf/Server.xml`, and OME rejects an SRT playback request whose
//! streamid carries **no** signature just as it rejects an invalid one — so
//! every playback target Farbstrom hands out has to be signed here.
//!
//! Both callers mint through this module so the streamid shape cannot diverge
//! again (gh #226): the Farbplay room-link flow (`routes::watch`, ~30 s TTL) and
//! the admin Stream Keys tab (`routes::stream_keys`, minted on demand).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppError;

type HmacSha1 = Hmac<Sha1>;

/// `{vhost}/{app}` prefix every playback streamid carries. The stream name is
/// the ingest stream key (`OutputStreamName=${OriginStreamName}` in Server.xml).
///
/// Nothing is appended after the stream name: OME's SRT streamid is
/// `{vhost}/{app}/{stream}/{playlist}`, and the publisher's auto-created default
/// playlist is named `master`, so a `/playlist` suffix addresses a playlist that
/// does not exist. SignedPolicy signs the path, so the two forms are not
/// interchangeable anyway.
pub const SRT_PATH_PREFIX: &str = "default/live";

/// Mint an OME SignedPolicy streamid for SRT playback, valid for `ttl_seconds`.
///
/// The client transmits only the path form `default/live/<stream>?policy=…&signature=…`
/// as the SRT `streamid`, but OME reconstructs the request URL as
/// `srt://default/live/<stream>?policy=…` (scheme + vhost as host) and signs
/// **that** — so the HMAC must be computed over the `srt://`-prefixed URL, not
/// the bare path. OME then validates the HMAC + `url_expire` on connect.
/// (Verified against OME v0.20.5 by matching its logged `expected` signature.
/// Still valid on v0.21.0: its `signed_policy.cpp` is byte-identical to v0.20.5's.
/// Re-check this the same way whenever the OME pin moves.)
///
/// `url_expire` is checked at connect time only, so a session established inside
/// the window survives past it — the TTL bounds when playback may *start*.
pub fn sign_streamid(
    secret: &str,
    stream_name: &str,
    ttl_seconds: u64,
) -> Result<String, AppError> {
    let expire_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        + (ttl_seconds as u128 * 1000);

    let path = format!("{}/{}", SRT_PATH_PREFIX, stream_name);
    let policy_json = json!({ "url_expire": expire_ms }).to_string();
    let policy = URL_SAFE_NO_PAD.encode(policy_json);

    // String OME signs (includes the srt:// scheme).
    let signed_url = format!("srt://{}?policy={}", path, policy);
    let mut mac = HmacSha1::new_from_slice(secret.as_bytes())
        .map_err(|e| AppError::Internal(format!("HMAC init error: {}", e)))?;
    mac.update(signed_url.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    // Streamid the client actually sends (path form, no scheme).
    Ok(format!(
        "{}?policy={}&signature={}",
        path, policy, signature
    ))
}
