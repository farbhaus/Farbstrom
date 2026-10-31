//! Rate limiting (CLAUDE.md "Recommended tests to add" #6).
//!
//! Two constraints shape this file, both worth knowing before adding to it.
//!
//! **It must stay in its own test binary.** `rate_limit::enabled()` reads
//! `STREAM_DISABLE_RATE_LIMIT` when the router is built, `common::test_app` sets
//! that variable, and environment is process-global — so one call to `test_app`
//! anywhere in this binary would silently disable the thing being tested.
//! Nothing here may use it; `test_app_http_rate_limited` leaves the limiter on.
//! It also needs a real socket, because `SmartIpKeyExtractor` has no key to
//! extract without `ConnectInfo`.
//!
//! **It is deliberately one test.** `tower_governor` keeps its bucket inside the
//! `GovernorConfig` (`governor.rs`: `limiter: SharedRateLimiter`, built once in
//! `finish()`), and `rate_limit::login_layer()` caches that config in a
//! `static OnceLock`. The bucket is therefore process-global and keyed by client
//! IP — every `TestServer` in this binary shares it, and every request comes
//! from 127.0.0.1. Split into separate `#[test]` functions they run in parallel,
//! drain each other's allowance, and fail at random. One function makes the
//! ordering explicit.

mod common;

use common::*;
use serde_json::json;

/// `login_layer` is 5/minute with a burst of 2, `passkey_layer` is a separate
/// bucket with its own allowance, and the four public room buckets each hold 30.
#[tokio::test(flavor = "multi_thread")]
async fn login_is_limited_per_ip_and_passkeys_have_their_own_budget() {
    let state = test_state();
    let server = test_app_http_rate_limited(state);

    // (1) A correct password on a fresh bucket succeeds — the limiter must not
    //     break the normal case.
    let ok = server
        .post("/api/auth/login")
        .json(&json!({ "password": "test-admin-password" }))
        .await;
    assert_eq!(
        ok.status_code(),
        200,
        "the first login was refused; the burst allowance is not working"
    );
    assert!(ok.json::<serde_json::Value>().get("token").is_some());

    // (2) Sustained hammering from one IP is refused. The exact index depends on
    //     how much of the bucket refilled mid-test, so assert the shape.
    let mut statuses = Vec::new();
    for _ in 0..10 {
        let res = server
            .post("/api/auth/login")
            .json(&json!({ "password": "definitely-wrong" }))
            .await;
        statuses.push(res.status_code().as_u16());
    }
    assert!(
        statuses.contains(&429),
        "10 rapid logins from one IP were never limited: {statuses:?}"
    );

    // (3) The passkey ceremony endpoints are on a different bucket, so the login
    //     flood above must not have drained them — otherwise a login flood locks
    //     the operator out of the one factor that still works.
    //
    //     400 is "no passkeys registered", this fixture's state; getting it
    //     proves the request was handled rather than throttled.
    let passkey = server.post("/api/auth/passkey/start").await;
    assert_eq!(
        passkey.status_code(),
        400,
        "the passkey bucket was drained by login traffic"
    );

    // (4) The routes a whole room hits at once — the room socket, the waiting
    //     room's event stream, the room's public info, uploads — each have a
    //     bucket of 30 + 1/s that nothing had before. None needs a real room:
    //     the limiter answers before the handler, so the handler's own refusal
    //     (no upgrade headers, unknown participant, unknown room) is what a
    //     request inside the allowance gets, and 429 is what the rest get.
    //     Each bucket is its own, so the first flood must not touch the next.
    let routes: [(&str, &str); 4] = [
        ("GET", "/ws/room/rate-room"),
        (
            "GET",
            "/api/public/rooms/rate-room/waiting/events/nobody?token=x",
        ),
        ("GET", "/api/public/rooms/rate-room/info"),
        (
            "POST",
            "/api/public/rooms/rate-room/files?participantId=nobody&token=x",
        ),
    ];
    for (method, path) in routes {
        let mut statuses = Vec::new();
        for _ in 0..45 {
            let res = match method {
                "POST" => server.post(path).await,
                _ => server.get(path).await,
            };
            statuses.push(res.status_code().as_u16());
        }
        assert_ne!(
            statuses[0], 429,
            "{method} {path} was throttled on its first request — a shared bucket?"
        );
        assert!(
            statuses.contains(&429),
            "45 rapid {method} {path} from one IP were never limited: {statuses:?}"
        );
        let allowed = statuses.iter().filter(|&&s| s != 429).count();
        assert!(
            (30..=35).contains(&allowed),
            "{method} {path}: {allowed} of 45 got through, expected the burst of 30"
        );
    }
}
