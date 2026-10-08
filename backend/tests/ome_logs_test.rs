mod common;

use axum::http::header;
use serde_json::Value;

fn auth_val(token: &str) -> axum::http::HeaderValue {
    format!("Bearer {}", token).parse().unwrap()
}

fn line(level: char, msg: &str) -> String {
    format!("[2026-10-08 09:09:43.187] {level} [Thread:1] Tag | file.cpp:1  | {msg}\n")
}

fn write_log(state: &std::sync::Arc<stream_backend::state::AppState>, text: &str) {
    let path = std::path::Path::new(&state.config.ome_log_path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

async fn get(state: std::sync::Arc<stream_backend::state::AppState>, query: &str) -> Value {
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get(&format!("/api/admin/ome-logs{query}"))
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 200);
    res.json()
}

fn msgs(body: &Value) -> Vec<String> {
    body["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["msg"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn requires_admin() {
    let state = common::test_state();
    let server = common::test_app(state);
    let res = server.get("/api/admin/ome-logs").await;
    assert_eq!(res.status_code(), 401);
}

#[tokio::test]
async fn missing_file_is_unavailable_not_an_error() {
    let state = common::test_state();
    let body = get(state, "").await;
    assert_eq!(body["available"], false);
    assert_eq!(body["lines"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn returns_parsed_lines() {
    let state = common::test_state();
    write_log(&state, &line('I', "hello"));
    let body = get(state, "").await;
    assert_eq!(body["available"], true);
    let l = &body["lines"][0];
    assert_eq!(l["ts"], "2026-10-08 09:09:43.187");
    assert_eq!(l["level"], "I");
    assert_eq!(l["tag"], "Tag");
    assert_eq!(l["msg"], "hello");
}

#[tokio::test]
async fn filters_by_level_before_capping() {
    let state = common::test_state();
    let mut text = line('E', "early error");
    for i in 0..5 {
        text += &line('I', &format!("info {i}"));
    }
    text += &line('W', "warning");
    text += &line('C', "critical");
    write_log(&state, &text);

    // The cap applies to what passed the filter, so an older error is still
    // reachable behind a run of newer info lines.
    let errs = get(state.clone(), "?level=error&lines=2").await;
    assert_eq!(msgs(&errs), ["early error", "critical"]);

    let warns = get(state.clone(), "?level=warn").await;
    assert_eq!(msgs(&warns), ["early error", "warning", "critical"]);

    let all = get(state, "?lines=2").await;
    assert_eq!(msgs(&all), ["warning", "critical"]);
}

#[tokio::test]
async fn rejects_an_unknown_level() {
    let state = common::test_state();
    let token = common::admin_token(&state);
    let server = common::test_app(state);
    let res = server
        .get("/api/admin/ome-logs?level=debug")
        .add_header(header::AUTHORIZATION, auth_val(&token))
        .await;
    assert_eq!(res.status_code(), 400);
}

#[tokio::test]
async fn large_file_returns_only_whole_lines_from_the_tail() {
    let state = common::test_state();
    // ~600 KiB, well past the 256 KiB tail window, so the read starts mid-line.
    let mut text = String::new();
    let mut n = 0;
    while text.len() < 600 * 1024 {
        text += &line('I', &format!("line {n} {}", "x".repeat(40)));
        n += 1;
    }
    write_log(&state, &text);

    let body = get(state, "?lines=1000").await;
    let lines = body["lines"].as_array().unwrap();
    assert!(lines.len() < n, "should not have read the whole file");
    // Every returned line parsed, i.e. none is a fragment of a cut line.
    assert!(lines.iter().all(|l| l["level"] == "I"));
    assert_eq!(
        lines.last().unwrap()["msg"],
        format!("line {} {}", n - 1, "x".repeat(40))
    );
}
