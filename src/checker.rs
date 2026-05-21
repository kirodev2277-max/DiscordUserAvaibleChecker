//! Async HTTP client for Discord's public username-attempt endpoint.
//!
//! The endpoint is unauthenticated and is what the Discord web client
//! itself calls while a user is picking a new handle:
//!
//! ```text
//! POST https://discord.com/api/v9/unique-username/username-attempt-unauthed
//! Content-Type: application/json
//! {"username": "<name>"}
//! ```
//!
//! On 200 the response body is `{"taken": true|false}`. Validation
//! errors come back as 400/422 with a JSON body; rate limiting as 429
//! with an optional `Retry-After` header / `retry_after` body field.
//!
//! Errors are folded into [`Status`] variants — `check_username` never
//! returns `Err` for ordinary failures so batch loops keep flowing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::result::{CheckResult, Status};

const API_URL: &str = "https://discord.com/api/v9/unique-username/username-attempt-unauthed";

const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/124.0.0.0 Safari/537.36"
);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_AUTO_RETRY_AFTER: Duration = Duration::from_secs(10);

/// Tunables for a [`Checker`] / batch run.
#[derive(Debug, Clone)]
pub struct CheckerConfig {
    /// Maximum simultaneous in-flight HTTP requests.
    pub workers: usize,
    /// Minimum delay between task starts. Helpful for polite throttling.
    pub delay: Duration,
    /// Per-request timeout for HTTP I/O.
    pub timeout: Duration,
    /// If a 429 quotes a Retry-After shorter than this, auto-retry once.
    pub max_auto_retry_after: Duration,
}

impl Default for CheckerConfig {
    fn default() -> Self {
        Self {
            workers: 8,
            delay: Duration::from_millis(0),
            timeout: REQUEST_TIMEOUT,
            max_auto_retry_after: MAX_AUTO_RETRY_AFTER,
        }
    }
}

/// Streaming callback invoked from worker tasks as results arrive.
///
/// The order of invocations is non-deterministic; the final aggregated
/// `Vec<CheckResult>` returned by [`Checker::run_batch`] always matches
/// the input order.
pub type BatchProgress = Arc<dyn Fn(usize, &CheckResult) + Send + Sync>;

/// Reusable async HTTP client wrapper.
#[derive(Clone)]
pub struct Checker {
    client: Client,
    config: CheckerConfig,
}

impl Checker {
    pub fn new(config: CheckerConfig) -> reqwest::Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(config.timeout)
            .pool_idle_timeout(Some(Duration::from_secs(30)))
            .build()?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &CheckerConfig {
        &self.config
    }

    /// Check a single username. Never panics; failures are encoded as
    /// `Status::Error` / `Status::Invalid` / `Status::RateLimited`.
    pub async fn check(&self, username: &str) -> CheckResult {
        let name = username.trim();
        if name.is_empty() {
            return CheckResult::new(
                "",
                Status::Invalid {
                    reason: "empty username".into(),
                },
            );
        }

        match self.do_attempt(name).await {
            Ok(res) => res,
            Err(err) => CheckResult::new(
                name,
                Status::Error {
                    reason: format!("network error: {err}"),
                },
            ),
        }
    }

    async fn do_attempt(&self, username: &str) -> reqwest::Result<CheckResult> {
        let resp = self.post_attempt(username).await?;

        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            let wait = parse_retry_after(&resp).await;
            match wait {
                Some(d) if d <= self.config.max_auto_retry_after => {
                    tokio::time::sleep(d).await;
                    let retry = self.post_attempt(username).await?;
                    if retry.status() == StatusCode::TOO_MANY_REQUESTS {
                        let again = parse_retry_after(&retry).await;
                        return Ok(CheckResult::new(
                            username,
                            Status::RateLimited {
                                retry_after_ms: again.map(|d| d.as_millis() as u64),
                            },
                        ));
                    }
                    return interpret_response(username, retry).await;
                }
                Some(d) => {
                    return Ok(CheckResult::new(
                        username,
                        Status::RateLimited {
                            retry_after_ms: Some(d.as_millis() as u64),
                        },
                    ));
                }
                None => {
                    return Ok(CheckResult::new(
                        username,
                        Status::RateLimited {
                            retry_after_ms: None,
                        },
                    ));
                }
            }
        }

        interpret_response(username, resp).await
    }

    async fn post_attempt(&self, username: &str) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(API_URL)
            .json(&serde_json::json!({ "username": username }))
            .header("Accept", "application/json")
            .send()
            .await
    }

    /// Run a concurrent batch of checks. Results come back in input
    /// order. `progress` is called from worker tasks for each completed
    /// item — keep the callback cheap and non-blocking.
    ///
    /// If `cancel` is provided and flips to `true`, any task that has
    /// not yet started will short-circuit; in-flight tasks finish
    /// normally. The returned `Vec` is truncated to only those slots
    /// that produced a result.
    pub async fn run_batch(
        &self,
        usernames: Vec<String>,
        progress: Option<BatchProgress>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Vec<CheckResult> {
        if usernames.is_empty() {
            return Vec::new();
        }

        let workers = self.config.workers.max(1).min(usernames.len());
        let semaphore = Arc::new(Semaphore::new(workers));
        let delay = self.config.delay;
        let start = Instant::now();

        let mut handles = Vec::with_capacity(usernames.len());
        for (idx, name) in usernames.iter().enumerate() {
            let checker = self.clone();
            let name = name.clone();
            let sem = semaphore.clone();
            let progress = progress.clone();
            let cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                if let Some(c) = &cancel {
                    if c.load(Ordering::SeqCst) {
                        return (idx, None);
                    }
                }
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                if let Some(c) = &cancel {
                    if c.load(Ordering::SeqCst) {
                        return (idx, None);
                    }
                }
                if idx > 0 && delay > Duration::ZERO {
                    // Stagger by `delay * idx` from batch start, capped so a
                    // long batch doesn't drift unboundedly.
                    let target = start + delay.saturating_mul(idx.min(1024) as u32);
                    let now = Instant::now();
                    if target > now {
                        tokio::time::sleep(target - now).await;
                    }
                }
                let result = checker.check(&name).await;
                if let Some(cb) = progress {
                    cb(idx, &result);
                }
                (idx, Some(result))
            }));
        }

        let mut bucket: Vec<Option<CheckResult>> = (0..usernames.len()).map(|_| None).collect();
        for h in handles {
            if let Ok((idx, res)) = h.await {
                if let (Some(r), Some(slot)) = (res, bucket.get_mut(idx)) {
                    *slot = Some(r);
                }
            }
        }
        bucket.into_iter().flatten().collect()
    }
}

async fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
    if let Some(header) = resp.headers().get(reqwest::header::RETRY_AFTER) {
        if let Ok(s) = header.to_str() {
            if let Ok(secs) = s.parse::<f64>() {
                return Some(Duration::from_millis((secs.max(0.0) * 1000.0) as u64));
            }
        }
    }
    // Falling through to body parsing requires consuming the response,
    // which the caller still wants to read. So we only consult the
    // header here; body-side retry_after is parsed later in
    // `interpret_response` when we already own the body.
    None
}

async fn interpret_response(
    username: &str,
    resp: reqwest::Response,
) -> reqwest::Result<CheckResult> {
    let status_code = resp.status();
    let text = resp.text().await?;
    let parsed: Option<serde_json::Value> = serde_json::from_str(&text).ok();

    if status_code == StatusCode::OK {
        if let Some(value) = &parsed {
            if let Some(taken) = value.get("taken").and_then(|v| v.as_bool()) {
                let status = if taken {
                    Status::Taken
                } else {
                    Status::Available
                };
                return Ok(CheckResult::new(username, status));
            }
        }
        return Ok(CheckResult::new(
            username,
            Status::Error {
                reason: format!("HTTP 200, unexpected body: {}", clip(&text, 200)),
            },
        ));
    }

    if status_code == StatusCode::TOO_MANY_REQUESTS {
        let ms = parsed
            .as_ref()
            .and_then(|v| v.get("retry_after"))
            .and_then(|v| v.as_f64())
            .map(|s| (s.max(0.0) * 1000.0) as u64);
        return Ok(CheckResult::new(
            username,
            Status::RateLimited { retry_after_ms: ms },
        ));
    }

    let reason = parsed
        .as_ref()
        .and_then(extract_error_reason)
        .unwrap_or_else(|| format!("HTTP {}", status_code.as_u16()));

    let status = if matches!(status_code.as_u16(), 400 | 422) {
        Status::Invalid { reason }
    } else {
        Status::Error { reason }
    };
    Ok(CheckResult::new(username, status))
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

#[derive(Deserialize)]
struct DiscordFieldError {
    message: Option<String>,
}

fn extract_error_reason(value: &serde_json::Value) -> Option<String> {
    if !value.is_object() {
        return None;
    }

    // Field-specific: errors.<field>._errors[].message
    let mut field_messages: Vec<String> = Vec::new();
    if let Some(errors) = value.get("errors").and_then(|v| v.as_object()) {
        for (field, payload) in errors {
            if let Some(inner) = payload.get("_errors").and_then(|v| v.as_array()) {
                for entry in inner {
                    if let Ok(parsed) = serde_json::from_value::<DiscordFieldError>(entry.clone()) {
                        if let Some(msg) = parsed.message {
                            field_messages.push(format!("{field}: {msg}"));
                        }
                    }
                }
            }
        }
    }
    if !field_messages.is_empty() {
        return Some(field_messages.join("; "));
    }

    // Top-level message fallback.
    value
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_field_error() {
        let json = serde_json::json!({
            "errors": {
                "username": {
                    "_errors": [
                        {"code": "USERNAME_TOO_SHORT", "message": "Must be 2-32 chars."}
                    ]
                }
            }
        });
        let reason = extract_error_reason(&json).unwrap();
        assert!(reason.contains("username"));
        assert!(reason.contains("Must be 2-32 chars."));
    }

    #[test]
    fn extracts_top_level_message() {
        let json = serde_json::json!({ "message": "You are being rate limited" });
        let reason = extract_error_reason(&json).unwrap();
        assert_eq!(reason, "You are being rate limited");
    }

    #[test]
    fn clip_truncates() {
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("abc", 10), "abc");
    }
}
