//! Who may call `renderer serve`, and how much.
//!
//! A fleet shares one renderer, so a single shared token says nothing about
//! which agent sent a request and lets one of them starve the rest. Per-caller
//! tokens fix both: `<caller>.<hex HMAC-SHA256(key, caller)>`, issued by
//! whoever holds `AGENT_BROWSER_RENDERER_HMAC_KEY` (an orchestrator, or
//! `agent-browser renderer token <caller>`). The service only verifies them,
//! and limits each caller's concurrent and per-minute renders.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const HMAC_KEY_ENV: &str = "AGENT_BROWSER_RENDERER_HMAC_KEY";

/// Caller recorded for requests authorized by the shared token.
pub const SHARED_CALLER: &str = "shared";
/// Caller recorded when the service requires no authentication.
pub const ANONYMOUS_CALLER: &str = "anonymous";

const MAX_CALLER_LEN: usize = 128;
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Idle callers are forgotten once the table grows past this.
const SWEEP_ABOVE: usize = 4096;

type HmacSha256 = Hmac<Sha256>;
type UsageTable = Arc<Mutex<HashMap<String, Usage>>>;

/// Characters a caller id may use. `.` separates the id from its signature.
pub fn valid_caller(caller: &str) -> bool {
    !caller.is_empty()
        && caller.len() <= MAX_CALLER_LEN
        && caller
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
}

/// Issues the token for `caller` under `key`.
pub fn issue_token(key: &[u8], caller: &str) -> Result<String, String> {
    if !valid_caller(caller) {
        return Err(format!(
            "Invalid caller id '{}': use 1 to {} letters, digits, '-', '_' or ':'",
            caller, MAX_CALLER_LEN
        ));
    }
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| e.to_string())?;
    mac.update(caller.as_bytes());
    Ok(format!(
        "{}.{}",
        caller,
        hex::encode(mac.finalize().into_bytes())
    ))
}

/// Verifies bearer tokens: per-caller HMAC tokens, the shared token, or
/// nothing when neither is configured.
#[derive(Clone, Default)]
pub struct Authenticator {
    shared_token: Option<String>,
    hmac_key: Option<Vec<u8>>,
}

impl Authenticator {
    pub fn new(shared_token: Option<String>, hmac_key: Option<Vec<u8>>) -> Self {
        Self {
            shared_token: shared_token.filter(|t| !t.is_empty()),
            hmac_key: hmac_key.filter(|k| !k.is_empty()),
        }
    }

    pub fn requires_auth(&self) -> bool {
        self.shared_token.is_some() || self.hmac_key.is_some()
    }

    /// The caller behind an `Authorization` header, or `None` when rejected.
    pub fn authenticate(&self, header: Option<&str>) -> Option<String> {
        if !self.requires_auth() {
            return Some(ANONYMOUS_CALLER.to_string());
        }
        let presented = header
            .and_then(|h| {
                h.strip_prefix("Bearer ")
                    .or_else(|| h.strip_prefix("bearer "))
            })?
            .trim();
        if let Some(caller) = self.verify_caller_token(presented) {
            return Some(caller);
        }
        let shared = self.shared_token.as_deref()?;
        constant_time_eq(shared.trim().as_bytes(), presented.as_bytes())
            .then(|| SHARED_CALLER.to_string())
    }

    fn verify_caller_token(&self, presented: &str) -> Option<String> {
        let key = self.hmac_key.as_deref()?;
        let (caller, signature) = presented.split_once('.')?;
        if !valid_caller(caller) {
            return None;
        }
        let signature = hex::decode(signature).ok()?;
        let mut mac = HmacSha256::new_from_slice(key).ok()?;
        mac.update(caller.as_bytes());
        // verify_slice compares in constant time.
        mac.verify_slice(&signature).ok()?;
        Some(caller.to_string())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Per-caller ceilings. Requests on the shared token or without
/// authentication are exempt: they would all share one budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerLimits {
    pub concurrent: usize,
    pub per_minute: u32,
}

impl Default for CallerLimits {
    fn default() -> Self {
        Self {
            concurrent: 2,
            per_minute: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Throttled {
    pub retry_after: Duration,
    pub reason: &'static str,
}

#[derive(Default)]
struct Usage {
    in_flight: usize,
    window_start: Option<Instant>,
    in_window: u32,
}

/// Admits renders under [`CallerLimits`]. A [`Permit`] holds a concurrency
/// slot until dropped.
#[derive(Clone)]
pub struct Admission {
    limits: CallerLimits,
    usage: UsageTable,
}

impl Admission {
    pub fn new(limits: CallerLimits) -> Self {
        Self {
            limits,
            usage: Arc::default(),
        }
    }

    pub fn admit(&self, caller: &str, now: Instant) -> Result<Permit, Throttled> {
        if caller == SHARED_CALLER || caller == ANONYMOUS_CALLER {
            return Ok(Permit { slot: None });
        }
        let mut table = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        if table.len() > SWEEP_ABOVE {
            table.retain(|_, u| {
                u.in_flight > 0 || u.window_start.is_some_and(|s| now - s < RATE_WINDOW)
            });
        }
        let usage = table.entry(caller.to_string()).or_default();
        let window_start = match usage.window_start {
            Some(start) if now - start < RATE_WINDOW => start,
            _ => {
                usage.in_window = 0;
                now
            }
        };
        usage.window_start = Some(window_start);
        if usage.in_window >= self.limits.per_minute {
            return Err(Throttled {
                retry_after: RATE_WINDOW - (now - window_start),
                reason: "per-minute render limit reached",
            });
        }
        if usage.in_flight >= self.limits.concurrent {
            return Err(Throttled {
                retry_after: Duration::from_secs(1),
                reason: "concurrent render limit reached",
            });
        }
        usage.in_flight += 1;
        usage.in_window += 1;
        Ok(Permit {
            slot: Some((self.usage.clone(), caller.to_string())),
        })
    }
}

pub struct Permit {
    slot: Option<(UsageTable, String)>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some((usage, caller)) = self.slot.take() {
            let mut table = usage.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(u) = table.get_mut(&caller) {
                u.in_flight = u.in_flight.saturating_sub(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test-key";

    fn bearer(token: &str) -> String {
        format!("Bearer {}", token)
    }

    #[test]
    fn issued_tokens_identify_their_caller() {
        let auth = Authenticator::new(None, Some(KEY.to_vec()));
        let token = issue_token(KEY, "assistant-42").unwrap();
        assert_eq!(
            auth.authenticate(Some(&bearer(&token))).as_deref(),
            Some("assistant-42")
        );
    }

    #[test]
    fn forged_or_swapped_tokens_are_rejected() {
        let auth = Authenticator::new(None, Some(KEY.to_vec()));
        let mine = issue_token(KEY, "assistant-1").unwrap();
        let signature = mine.split_once('.').unwrap().1;
        // Someone else's id with my signature.
        assert_eq!(
            auth.authenticate(Some(&bearer(&format!("assistant-2.{signature}")))),
            None
        );
        // Signed with another key.
        let other = issue_token(b"other-key", "assistant-1").unwrap();
        assert_eq!(auth.authenticate(Some(&bearer(&other))), None);
        assert_eq!(auth.authenticate(Some(&bearer("assistant-1.zz"))), None);
        assert_eq!(auth.authenticate(Some(&bearer("assistant-1"))), None);
        assert_eq!(auth.authenticate(None), None);
    }

    #[test]
    fn shared_token_still_works_beside_caller_tokens() {
        let auth = Authenticator::new(Some("s3cret".to_string()), Some(KEY.to_vec()));
        assert_eq!(
            auth.authenticate(Some("Bearer s3cret")).as_deref(),
            Some(SHARED_CALLER)
        );
        assert_eq!(auth.authenticate(Some("Bearer nope")), None);
        assert_eq!(auth.authenticate(Some("Basic s3cret")), None);
    }

    #[test]
    fn no_configuration_means_anonymous() {
        let auth = Authenticator::new(None, None);
        assert!(!auth.requires_auth());
        assert_eq!(auth.authenticate(None).as_deref(), Some(ANONYMOUS_CALLER));
    }

    #[test]
    fn caller_ids_are_restricted() {
        assert!(valid_caller("8a7d1b9e-2c3f-4d5e-9f00-112233445566"));
        assert!(valid_caller("tenant:assistant_1"));
        assert!(!valid_caller(""));
        assert!(!valid_caller("a.b"));
        assert!(!valid_caller("a b"));
        assert!(!valid_caller(&"x".repeat(MAX_CALLER_LEN + 1)));
        assert!(issue_token(KEY, "a.b").is_err());
    }

    #[test]
    fn concurrency_is_limited_per_caller_and_released_on_drop() {
        let admission = Admission::new(CallerLimits {
            concurrent: 1,
            per_minute: 100,
        });
        let now = Instant::now();
        let first = admission.admit("a", now).unwrap();
        let err = admission.admit("a", now).err().unwrap();
        assert_eq!(err.reason, "concurrent render limit reached");
        // Another caller is unaffected.
        let _other = admission.admit("b", now).unwrap();
        drop(first);
        assert!(admission.admit("a", now).is_ok());
    }

    #[test]
    fn per_minute_budget_resets_with_the_window() {
        let admission = Admission::new(CallerLimits {
            concurrent: 10,
            per_minute: 2,
        });
        let start = Instant::now();
        drop(admission.admit("a", start).unwrap());
        drop(admission.admit("a", start).unwrap());
        let err = admission
            .admit("a", start + Duration::from_secs(20))
            .err()
            .unwrap();
        assert_eq!(err.reason, "per-minute render limit reached");
        assert_eq!(err.retry_after, Duration::from_secs(40));
        assert!(admission.admit("a", start + RATE_WINDOW).is_ok());
    }

    #[test]
    fn shared_and_anonymous_callers_are_not_throttled() {
        let admission = Admission::new(CallerLimits {
            concurrent: 1,
            per_minute: 1,
        });
        let now = Instant::now();
        let _a = admission.admit(SHARED_CALLER, now).unwrap();
        let _b = admission.admit(SHARED_CALLER, now).unwrap();
        let _c = admission.admit(ANONYMOUS_CALLER, now).unwrap();
    }
}
