//! OvenMediaEngine log tail for the admin dashboard (GitHub #261).
//!
//! OME's REST API has no log endpoint, so this reads OME's own file sink
//! (`<Path>` in the image's `origin_conf/Logger.xml`). The file is root-owned
//! but world-readable, so the unprivileged backend can tail it as-is.
//!
//! Lines carry stream names, which are the ingest stream keys, and client IPs.
//! That is fine behind `AdminAuth` — the admin sees both elsewhere — but this
//! must never be exposed on a public route.

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use crate::auth::AdminAuth;
use crate::error::AppError;
use crate::state::AppState;

/// How much of the end of the file is read. The file runs to megabytes; a
/// dashboard only ever wants the recent past, and a filtered view that finds
/// fewer matches than asked for in this window simply shows fewer.
const TAIL_BYTES: u64 = 256 * 1024;
const DEFAULT_LINES: usize = 200;
const MAX_LINES: usize = 1000;

#[derive(Deserialize)]
struct LogQuery {
    lines: Option<usize>,
    level: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct LogLine {
    /// OME's own timestamp, passed through as display text. It is the
    /// container's local time (UTC) with no zone, so it is not a wire
    /// timestamp in the `time::now_ms` sense and is never parsed.
    pub ts: Option<String>,
    /// `D`, `I`, `W`, `E` or `C`; `None` for a line that doesn't parse.
    pub level: Option<String>,
    pub tag: Option<String>,
    pub msg: String,
}

/// Parse `[2026-10-08 09:09:43.187] W [thread:728] Tag | file.cpp:183  | msg`.
/// Anything else (a continuation line, a banner) comes back as a bare message.
pub fn parse_line(line: &str) -> LogLine {
    let bare = || LogLine {
        ts: None,
        level: None,
        tag: None,
        msg: line.to_string(),
    };
    let Some(rest) = line.strip_prefix('[') else {
        return bare();
    };
    let Some((ts, rest)) = rest.split_once("] ") else {
        return bare();
    };
    let mut chars = rest.chars();
    let level = match chars.next() {
        Some(c @ ('D' | 'I' | 'W' | 'E' | 'C')) => c,
        _ => return bare(),
    };
    // Skip " [thread] ".
    let Some(rest) = chars.as_str().strip_prefix(" [") else {
        return bare();
    };
    let Some((_thread, rest)) = rest.split_once("] ") else {
        return bare();
    };
    // The message is last and may itself contain '|', so split at most twice.
    let mut parts = rest.splitn(3, '|').map(str::trim);
    let (tag, msg) = match (parts.next(), parts.next(), parts.next()) {
        (Some(tag), Some(_location), Some(msg)) => (tag, msg),
        _ => return bare(),
    };
    LogLine {
        ts: Some(ts.to_string()),
        level: Some(level.to_string()),
        tag: Some(tag.to_string()),
        msg: msg.to_string(),
    }
}

fn passes(level: Option<&str>, filter: &str) -> bool {
    match filter {
        "warn" => matches!(level, Some("W" | "E" | "C")),
        "error" => matches!(level, Some("E" | "C")),
        _ => true,
    }
}

/// The last `max` lines of `text` that pass `filter`. An unparsed line is
/// judged by the level of the line it continues, so a multi-line error is not
/// cut down to its first line by the Error filter.
pub fn select(text: &str, filter: &str, max: usize) -> Vec<LogLine> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        if raw.is_empty() {
            continue;
        }
        let line = parse_line(raw);
        if line.level.is_some() {
            current = line.level.clone();
        }
        if passes(current.as_deref(), filter) {
            out.push(line);
        }
    }
    let skip = out.len().saturating_sub(max);
    out.split_off(skip)
}

/// The text of the file's last `TAIL_BYTES`, starting on a line boundary.
/// `Ok(None)` when the file does not exist (e.g. a dev backend outside the
/// container).
fn read_tail(path: &str) -> std::io::Result<Option<String>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    // A mid-file start almost certainly lands inside a line; drop that
    // fragment rather than show it as a garbled bare message.
    let text = match (start > 0, text.find('\n')) {
        (true, Some(i)) => text[i + 1..].to_string(),
        (true, None) => String::new(),
        (false, _) => text.into_owned(),
    };
    Ok(Some(text))
}

async fn get_logs(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Query(q): Query<LogQuery>,
) -> Result<Json<Value>, AppError> {
    let filter = q.level.unwrap_or_else(|| "all".into());
    if !matches!(filter.as_str(), "all" | "warn" | "error") {
        return Err(AppError::BadRequest(
            "level must be all, warn or error".into(),
        ));
    }
    let max = q.lines.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES);

    let path = state.config.ome_log_path.clone();
    let tail = tokio::task::spawn_blocking(move || read_tail(&path))
        .await
        .map_err(|e| AppError::Internal(format!("ome log task: {e}")))?
        .map_err(|e| AppError::Internal(format!("read ome log: {e}")))?;

    Ok(Json(match tail {
        Some(text) => json!({ "available": true, "lines": select(&text, &filter, max) }),
        None => json!({ "available": false, "lines": [] }),
    }))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(get_logs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_ome_line() {
        let l = parse_line(
            "[2026-10-08 09:09:43.187] W [SPRtcSig-t3333:728] Publisher | publisher.cpp:183  | Could not pull | stream",
        );
        assert_eq!(l.ts.as_deref(), Some("2026-10-08 09:09:43.187"));
        assert_eq!(l.level.as_deref(), Some("W"));
        assert_eq!(l.tag.as_deref(), Some("Publisher"));
        assert_eq!(l.msg, "Could not pull | stream");
    }

    #[test]
    fn unparsed_lines_are_bare_messages() {
        for raw in [
            "  continuation",
            "[no close bracket",
            "[ts] X [t] A | b | c",
        ] {
            let l = parse_line(raw);
            assert_eq!(l.level, None);
            assert_eq!(l.msg, raw);
        }
    }

    #[test]
    fn continuation_inherits_the_previous_level() {
        let text = "[t] E [x] A | f:1 | boom\n  detail\n[t] I [x] A | f:1 | fine\n  more\n";
        let got = select(text, "error", 10);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].msg, "  detail");
    }

    #[test]
    fn keeps_the_last_lines() {
        let text = "[t] I [x] A | f:1 | 1\n[t] I [x] A | f:1 | 2\n[t] I [x] A | f:1 | 3\n";
        let got: Vec<_> = select(text, "all", 2).into_iter().map(|l| l.msg).collect();
        assert_eq!(got, ["2", "3"]);
    }
}
