//! Labels for the engine containers, and the sweep that removes abandoned ones.
//!
//! Include in a test binary with:
//!
//! ```rust,ignore
//! #[path = "support/containers.rs"]
//! mod containers;
//! ```
//!
//! The exit slot removes a container on every ordinary exit. A terminating
//! signal (`SIGINT`, `SIGTERM`, `SIGKILL`) runs no exit code, so a run ended
//! that way leaves its container behind. Every engine container therefore
//! carries a role label and a start stamp, and a start sweeps any stamped
//! long enough ago that no live run can own it.

use std::{
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const CONTAINER_LABEL: &str = "io.sql-values.harness";
pub const STARTED_LABEL: &str = "io.sql-values.harness.started";

/// How old a labelled container must be before the sweep removes it. Longer
/// than any test binary by a wide margin, because a sibling binary owns
/// containers this one must not touch.
const STALE_AFTER: Duration = Duration::from_secs(2 * 60 * 60);

/// Seconds since the epoch, or zero if the clock is behind it.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// What one container carries: which engine it is, and when it started.
pub fn labels(role: &str) -> [(String, String); 2] {
    [
        (CONTAINER_LABEL.to_owned(), role.to_owned()),
        (STARTED_LABEL.to_owned(), now_secs().to_string()),
    ]
}

/// Remove containers an earlier run abandoned.
///
/// Age is what makes removal safe: a container a sibling binary is using is
/// minutes old, never hours. A container whose stamp is missing or unreadable
/// comes from an older build of this harness and goes.
pub fn sweep_abandoned() {
    let Ok(listed) = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("label={CONTAINER_LABEL}"),
            "--format",
            "{{.ID}} {{.Labels}}",
        ])
        .output()
    else {
        return;
    };
    let now = now_secs();
    for line in String::from_utf8_lossy(&listed.stdout).lines() {
        let Some((id, labels)) = line.split_once(' ') else {
            continue;
        };
        let started = labels
            .split(',')
            .find_map(|label| label.strip_prefix(STARTED_LABEL)?.strip_prefix('='))
            .and_then(|stamp| stamp.parse::<u64>().ok())
            .unwrap_or(0);
        if now.saturating_sub(started) < STALE_AFTER.as_secs() {
            continue;
        }
        let _ = Command::new("docker").args(["rm", "-f", "-v", id]).output();
    }
}
