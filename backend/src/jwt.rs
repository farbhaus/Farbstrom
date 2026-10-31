//! HS256 JSON Web Tokens: the admin session token and LiveKit access tokens.
//!
//! This was the `jsonwebtoken` crate, which carries every algorithm it
//! supports — RSA, ECDSA, EdDSA — and with them two advisories that
//! `cargo audit` had to be told to ignore (`rsa`'s Marvin timing attack,
//! non-constant-time arithmetic in `crypto-bigint`). Nothing here signs with
//! anything but HMAC-SHA256, so the token is a few lines over `hmac` + `sha2`,
//! which the webhook and SignedPolicy code already depend on.
//!
//! Verification is deliberately narrow: exactly three segments of unpadded
//! base64url, a header naming `HS256` — an `alg: none`, or any other algorithm,
//! is refused before the signature is looked at — a constant-time MAC check,
//! and a required `exp`. Tokens minted by `jsonwebtoken` verify unchanged,
//! because the MAC covers the encoded segments exactly as sent; a test pins
//! that, so an upgrade does not sign the operator out.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// How far past `exp` a token is still accepted — what `jsonwebtoken`'s default
/// validation allowed, kept so a token's lifetime did not shrink in the switch.
const EXP_LEEWAY_SECS: u64 = 60;

/// Why a token was refused. Callers map every variant to the same 401; the
/// distinction exists for logs and tests, never for the client.
#[derive(Debug, PartialEq, Eq)]
pub enum JwtError {
    Malformed,
    Algorithm,
    Signature,
    Expired,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            JwtError::Malformed => "malformed token",
            JwtError::Algorithm => "unsupported algorithm",
            JwtError::Signature => "bad signature",
            JwtError::Expired => "expired",
        })
    }
}

/// Field order is `jsonwebtoken`'s (`typ` before `alg`), so a token signed here
/// is byte-for-byte the one it would have produced.
#[derive(Serialize, Deserialize)]
struct Header<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    typ: Option<&'a str>,
    alg: &'a str,
}

#[derive(Deserialize)]
struct Expiry {
    exp: u64,
}

fn mac(secret: &[u8]) -> HmacSha256 {
    // HMAC is defined for keys of any length, so this cannot fail.
    HmacSha256::new_from_slice(secret).expect("HMAC accepts a key of any length")
}

/// Sign `claims` as a compact HS256 JWT.
pub fn sign<T: Serialize>(claims: &T, secret: &[u8]) -> String {
    let header = serde_json::to_vec(&Header {
        typ: Some("JWT"),
        alg: "HS256",
    })
    .expect("a static header serialises");
    let payload = serde_json::to_vec(claims).expect("token claims serialise");
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(payload)
    );
    let mut m = mac(secret);
    m.update(signing_input.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(m.finalize().into_bytes());
    format!("{signing_input}.{signature}")
}

/// Verify an HS256 JWT signed with `secret` and decode its claims. `now_secs`
/// is the current Unix time; the token must carry an `exp` no more than
/// [`EXP_LEEWAY_SECS`] behind it.
pub fn verify<T: DeserializeOwned>(
    token: &str,
    secret: &[u8],
    now_secs: u64,
) -> Result<T, JwtError> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };

    let header_json = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| JwtError::Malformed)?;
    let header: Header = serde_json::from_slice(&header_json).map_err(|_| JwtError::Malformed)?;
    if header.alg != "HS256" {
        return Err(JwtError::Algorithm);
    }

    let signature = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| JwtError::Malformed)?;
    let mut m = mac(secret);
    m.update(header_b64.as_bytes());
    m.update(b".");
    m.update(payload_b64.as_bytes());
    m.verify_slice(&signature)
        .map_err(|_| JwtError::Signature)?;

    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| JwtError::Malformed)?;
    let Expiry { exp } = serde_json::from_slice(&payload).map_err(|_| JwtError::Malformed)?;
    if exp.saturating_add(EXP_LEEWAY_SECS) < now_secs {
        return Err(JwtError::Expired);
    }
    serde_json::from_slice(&payload).map_err(|_| JwtError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret-that-is-at-least-thirty-two-characters-long";
    /// 2100-01-01 — far enough out that these fixtures never expire.
    const FAR_FUTURE: u64 = 4_102_444_800;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Claims {
        admin: bool,
        exp: u64,
        #[serde(default)]
        ver: u64,
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn round_trips() {
        let claims = Claims {
            admin: true,
            exp: now() + 60,
            ver: 7,
        };
        let token = sign(&claims, SECRET);
        assert_eq!(verify::<Claims>(&token, SECRET, now()), Ok(claims));
    }

    /// Minted by `jsonwebtoken` 10.4.0 (`Header::new(Algorithm::HS256)`) with
    /// the test secret, before the crate was removed. Live admin sessions are
    /// exactly these tokens, so they must keep verifying.
    #[test]
    fn verifies_tokens_minted_by_jsonwebtoken() {
        let with_ver = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhZG1pbiI6dHJ1ZSwiZXhwIjo0MTAyNDQ0ODAwLCJ2ZXIiOjN9.FyIcJP-D6zO5AsEMzBShyCZYc1Nk7iY5xDRkQgWvRXQ";
        let legacy = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhZG1pbiI6dHJ1ZSwiZXhwIjo0MTAyNDQ0ODAwfQ.07rnnU7PekLsKgL0NA_v5dfS6_xJ3P5AnoH4ULVHINw";
        assert_eq!(
            verify::<Claims>(with_ver, SECRET, now()),
            Ok(Claims {
                admin: true,
                exp: FAR_FUTURE,
                ver: 3
            })
        );
        assert_eq!(
            verify::<Claims>(legacy, SECRET, now()),
            Ok(Claims {
                admin: true,
                exp: FAR_FUTURE,
                ver: 0
            })
        );
    }

    #[test]
    fn signs_what_jsonwebtoken_signed() {
        // Same claims, same secret: byte-identical to the jsonwebtoken fixture
        // above, so LiveKit is handed exactly the token shape it always was.
        #[derive(Serialize)]
        struct Ordered {
            admin: bool,
            exp: u64,
            ver: u64,
        }
        let token = sign(
            &Ordered {
                admin: true,
                exp: FAR_FUTURE,
                ver: 3,
            },
            SECRET,
        );
        assert_eq!(
            token,
            "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhZG1pbiI6dHJ1ZSwiZXhwIjo0MTAyNDQ0ODAwLCJ2ZXIiOjN9.FyIcJP-D6zO5AsEMzBShyCZYc1Nk7iY5xDRkQgWvRXQ"
        );
    }

    #[test]
    fn refuses_another_secret() {
        let token = sign(
            &Claims {
                admin: true,
                exp: FAR_FUTURE,
                ver: 0,
            },
            SECRET,
        );
        assert_eq!(
            verify::<Claims>(&token, b"some-other-secret", now()),
            Err(JwtError::Signature)
        );
    }

    #[test]
    fn refuses_a_tampered_payload() {
        let token = sign(
            &Claims {
                admin: false,
                exp: FAR_FUTURE,
                ver: 0,
            },
            SECRET,
        );
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(br#"{"admin":true,"exp":4102444800}"#);
        parts[1] = &forged;
        assert_eq!(
            verify::<Claims>(&parts.join("."), SECRET, now()),
            Err(JwtError::Signature)
        );
    }

    /// The classic JWT forgery: claim no algorithm, send no signature.
    #[test]
    fn refuses_alg_none_and_other_algorithms() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"admin":true,"exp":4102444800}"#);
        for alg in ["none", "None", "HS512", "RS256", "hs256"] {
            let header = URL_SAFE_NO_PAD.encode(format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#));
            for token in [
                format!("{header}.{payload}."),
                format!("{header}.{payload}"),
            ] {
                assert!(
                    verify::<Claims>(&token, SECRET, now()).is_err(),
                    "accepted alg {alg}: {token}"
                );
            }
        }
    }

    #[test]
    fn refuses_an_expired_token_after_the_leeway() {
        let issued = now();
        let token = sign(
            &Claims {
                admin: true,
                exp: issued,
                ver: 0,
            },
            SECRET,
        );
        assert!(verify::<Claims>(&token, SECRET, issued + EXP_LEEWAY_SECS).is_ok());
        assert_eq!(
            verify::<Claims>(&token, SECRET, issued + EXP_LEEWAY_SECS + 1),
            Err(JwtError::Expired)
        );
    }

    #[test]
    fn requires_exp() {
        #[derive(Serialize)]
        struct NoExp {
            admin: bool,
        }
        let token = sign(&NoExp { admin: true }, SECRET);
        assert_eq!(
            verify::<Claims>(&token, SECRET, now()),
            Err(JwtError::Malformed)
        );
    }

    #[test]
    fn refuses_malformed_input() {
        let good = sign(
            &Claims {
                admin: true,
                exp: FAR_FUTURE,
                ver: 0,
            },
            SECRET,
        );
        let padded = format!("{good}=");
        for token in ["", ".", "..", "a.b.c", "a.b.c.d", &padded] {
            assert!(verify::<Claims>(token, SECRET, now()).is_err(), "{token}");
        }
    }
}
