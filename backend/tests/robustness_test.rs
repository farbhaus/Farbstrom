//! Behaviours from the runtime-robustness pass that are observable over HTTP.

mod common;

use axum::http::header;
use serde_json::Value;

fn auth_val(token: &str) -> axum::http::HeaderValue {
    format!("Bearer {}", token).parse().unwrap()
}

// NOTE: the SPA fallback is wired in `main.rs`, not `routes::build_router`, so
// `common::test_app` cannot exercise it — everything unmatched 404s there for
// unrelated reasons. The asset-vs-room decision is unit-tested against
// `pages::path_looks_like_asset` instead (see src/routes/pages.rs).

/// Branding colours land in a CSS custom property. A malformed value is not an
/// injection (the client uses `setProperty`), but it makes every `var()` that
/// reads it resolve to nothing, stripping colour from the UI with no error.
#[tokio::test]
async fn branding_rejects_non_hex_colors() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    for bad in ["red", "rgb(1,2,3)", "#12", "#gggggg", "blue; display:none"] {
        let res = server
            .post("/api/admin/branding/colors")
            .add_header(header::AUTHORIZATION, auth_val(&token))
            .json(&serde_json::json!({ "color_accent": bad }))
            .await;
        assert_eq!(res.status_code(), 400, "{bad:?} should be rejected");
    }

    // A rejected request must not have written anything.
    let colors = server.get("/api/branding/colors").await.json::<Value>();
    assert!(
        colors.get("color_accent").is_none(),
        "a rejected colour was persisted"
    );
}

/// The valid forms all still work, including the empty-string reset.
#[tokio::test]
async fn branding_accepts_hex_colors_and_reset() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    for good in ["#fff", "#1A2B3C", "#12345678"] {
        let res = server
            .post("/api/admin/branding/colors")
            .add_header(header::AUTHORIZATION, auth_val(&token))
            .json(&serde_json::json!({ "color_accent": good }))
            .await;
        assert_eq!(res.status_code(), 200, "{good:?} should be accepted");
    }
    assert_eq!(
        server.get("/api/branding/colors").await.json::<Value>()["color_accent"],
        Value::String("#12345678".into())
    );

    let reset = server
        .post("/api/admin/branding/colors")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .json(&serde_json::json!({ "color_accent": "" }))
        .await;
    assert_eq!(reset.status_code(), 200);
    assert!(server.get("/api/branding/colors").await.json::<Value>()["color_accent"].is_null());
}

/// OME stream names are our own key tokens or `conf-<uuid>`; the value is
/// spliced into the upstream OME API URL.
///
/// The separators have to be **percent-encoded** to be interesting: axum routes
/// on the raw path, so a literal `/` simply fails to match the single-segment
/// route, and `?`/`#` are never part of the path at all. `%2F` and `%2E%2E` do
/// match, and axum hands the handler the *decoded* value — which is how a
/// crafted name reaches the URL builder.
#[tokio::test]
async fn ome_rejects_percent_encoded_path_escapes() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    for bad in [
        "%2E%2E%2Fvhosts",        // ../vhosts
        "a%2Fb",                  // a/b
        "key%3Fx%3D1",            // key?x=1
        "%2E%2E%2F%2E%2E%2Froot", // ../../root
    ] {
        let res = server
            .get(&format!("/api/ome/streams/{bad}"))
            .add_header(header::AUTHORIZATION, auth_val(&token))
            .await;
        assert_eq!(
            res.status_code(),
            400,
            "{bad:?} was not rejected (502 would mean it reached the OME client)"
        );
    }
}

/// A real key token must still get through to the OME client — 502 here means
/// it tried to reach OME, which is the correct behaviour with no OME running.
#[tokio::test]
async fn ome_accepts_a_real_key_token() {
    let state = common::test_state();
    let (_, key_token) = common::seed_stream_key(&state, "Key");
    let token = common::admin_token(&state);
    let server = common::test_app(state);

    let res = server
        .get(&format!("/api/ome/streams/{key_token}"))
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(
        res.status_code(),
        502,
        "a valid key token should have reached the OME client"
    );
}
