//! Domain types for a single check outcome.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Outcome of a single username check.
///
/// `RateLimited` carries the Retry-After hint that Discord (or our retry
/// logic) reported so a caller can decide whether to back off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Status {
    Available,
    Taken,
    Invalid { reason: String },
    RateLimited { retry_after_ms: Option<u64> },
    Error { reason: String },
}

impl Status {
    pub fn token(&self) -> &'static str {
        match self {
            Status::Available => "AVAILABLE",
            Status::Taken => "TAKEN",
            Status::Invalid { .. } => "INVALID",
            Status::RateLimited { .. } => "RATE_LIMITED",
            Status::Error { .. } => "ERROR",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Status::Invalid { reason } | Status::Error { reason } => Some(reason.as_str()),
            Status::RateLimited { .. } | Status::Available | Status::Taken => None,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Status::RateLimited {
                retry_after_ms: Some(ms),
            } => Some(Duration::from_millis(*ms)),
            _ => None,
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Status::Available)
    }

    pub fn is_terminal_failure(&self) -> bool {
        matches!(self, Status::Error { .. })
    }
}

/// A single (username, status) pair plus a wall-clock timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub username: String,
    pub status: Status,
    /// Unix seconds since epoch when the check finished. 0 means "unset".
    #[serde(default)]
    pub checked_at: u64,
}

impl CheckResult {
    pub fn new(username: impl Into<String>, status: Status) -> Self {
        Self {
            username: username.into(),
            status,
            checked_at: now_secs(),
        }
    }

    /// One-line console rendering (without colors) that mirrors the legacy
    /// Python format so existing log scrapers continue to work.
    pub fn format_line(&self) -> String {
        match &self.status {
            Status::Available => format!("[+] {}: AVAILABLE", self.username),
            Status::Taken => format!("[-] {}: taken", self.username),
            Status::Invalid { reason } => format!("[!] {}: invalid ({reason})", self.username),
            Status::RateLimited { retry_after_ms } => match retry_after_ms {
                Some(ms) => format!(
                    "[?] {}: rate limited (retry after {:.2}s)",
                    self.username,
                    *ms as f64 / 1000.0
                ),
                None => format!("[?] {}: rate limited", self.username),
            },
            Status::Error { reason } => format!("[?] {}: error ({reason})", self.username),
        }
    }

    /// One-line text-log rendering: `<name> - <STATUS>[: <reason>]`.
    pub fn format_file_line(&self) -> String {
        let token = self.status.token();
        match &self.status {
            Status::Invalid { reason } | Status::Error { reason } => {
                format!("{} - {}: {}", self.username, token, reason)
            }
            Status::RateLimited {
                retry_after_ms: Some(ms),
            } => {
                format!(
                    "{} - {}: retry after {:.2}s",
                    self.username,
                    token,
                    *ms as f64 / 1000.0
                )
            }
            _ => format!("{} - {}", self.username, token),
        }
    }
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_available() {
        let r = CheckResult::new("alice", Status::Available);
        assert_eq!(r.format_line(), "[+] alice: AVAILABLE");
        assert_eq!(r.format_file_line(), "alice - AVAILABLE");
    }

    #[test]
    fn formats_invalid_with_reason() {
        let r = CheckResult::new(
            "x",
            Status::Invalid {
                reason: "too short".into(),
            },
        );
        assert_eq!(r.format_line(), "[!] x: invalid (too short)");
        assert_eq!(r.format_file_line(), "x - INVALID: too short");
    }

    #[test]
    fn formats_rate_limited() {
        let r = CheckResult::new(
            "bob",
            Status::RateLimited {
                retry_after_ms: Some(2500),
            },
        );
        assert!(r.format_line().contains("rate limited"));
        assert!(r.format_file_line().contains("RATE_LIMITED"));
    }
}
