//! Wall-clock helpers.
//!
//! Every timestamp that crosses the wire to a browser is **milliseconds since
//! the Unix epoch**, so it drops straight into `new Date(ts)`. That was not
//! always true: `chat:message` sent milliseconds while `file:shared` sent
//! seconds from four separate call sites, and the viewer's single `fmtTime`
//! rendered the latter as January 1970. Route every such timestamp through
//! [`now_ms`] rather than re-deriving it, so the unit can't drift again.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch. Saturates to 0 for a clock before the
/// epoch rather than panicking.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_is_milliseconds_not_seconds() {
        let t = now_ms();
        // 1_600_000_000_000 ms == Sep 2020. A seconds-valued clock would be
        // ~1.7e9 and fail this, which is exactly the bug this guards.
        assert!(
            t > 1_600_000_000_000,
            "now_ms returned {t}, which looks like seconds, not milliseconds"
        );
        // Sanity ceiling: year 5138 in ms.
        assert!(
            t < 100_000_000_000_000,
            "now_ms returned {t}, implausibly large"
        );
    }
}
