//! Rate limits as a state: one entry per credential fingerprint.
//!
//! Triggers are HTTP 429, a 403 whose quota is exhausted (`x-ratelimit-remaining: 0`) or whose
//! text says `rate limit` / `abuse`, and GraphQL's `RATE_LIMITED` error. A trigger pauses every
//! request of that credential until `retry-after` or the reset header; without either the pause
//! is 30 s, doubling on each consecutive trigger up to 15 min. A response that is not a trigger
//! ends the streak. Background requests also stop while less than 10 % of the quota is left, so
//! interactive reads keep a reserve. Gitea sends no limit headers; GitLab uses `ratelimit-*`.
//!
//! Every method takes `now`, so tests drive the clock; `global()` is the process-wide table the
//! transports share.

use crate::credential::Secret;
use crate::transport::ApiResponse;
use crate::transport::Class;
use crate::transport::Cli;
use crate::transport::HostKey;
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

pub const FIRST_BACKOFF: Duration = Duration::from_secs(30);
pub const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// Background work stops when fewer than this share of the quota is left (1/10).
pub const RESERVE_DIVISOR: u64 = 10;
/// Without a credential the quota is small (60/hour on github.com) and shared by everyone
/// behind the same address, so background work stops once half of it is spent.
pub const ANONYMOUS_RESERVE_DIVISOR: u64 = 2;
/// A reset header further away than this is not trusted (clock skew, garbage).
const MAX_RESET: Duration = Duration::from_secs(2 * 60 * 60);

/// Who a quota belongs to: a hash of the API base and the token, or of the CLI (its
/// executable, which carries its own sign-in) and the host. Never the token itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Fingerprint(u64);

impl Fingerprint {
    fn digest(parts: &[&[u8]]) -> Self {
        let mut hasher = sha2::Sha256::new();
        for part in parts {
            hasher.update(part);
            hasher.update([0]);
        }
        let digest = hasher.finalize();
        Self(u64::from_be_bytes(digest[..8].try_into().expect("8 bytes")))
    }

    /// `api_base` rather than the host: one token can sign in to several API bases only on
    /// odd installs, and tests run many fake servers for the same logical host.
    pub fn token(api_base: &str, token: &Secret) -> Self {
        Self::digest(&[b"token", api_base.as_bytes(), token.expose().as_bytes()])
    }

    /// Requests without a credential: the forge counts them per client address, so every
    /// anonymous transport for the same API base shares one quota.
    pub fn anonymous(api_base: &str) -> Self {
        Self::digest(&[b"anonymous", api_base.as_bytes()])
    }

    pub fn cli(host: &HostKey, cli: Cli, executable: &std::path::Path) -> Self {
        Self::digest(&[
            b"cli",
            cli.name().as_bytes(),
            executable.as_os_str().as_encoded_bytes(),
            host.to_string().as_bytes(),
        ])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerKind {
    /// HTTP 429.
    TooMany,
    /// 403 with `x-ratelimit-remaining: 0`.
    QuotaExhausted,
    /// 403 whose message names a secondary / abuse limit.
    Abuse,
    /// GraphQL `errors[].type == "RATE_LIMITED"`.
    GraphQl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// From `retry-after` or the reset header; `None` means "back off".
    pub wait: Option<Duration>,
}

/// The quota headers of one response (`x-ratelimit-*` on GitHub, `ratelimit-*` on GitLab).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Quota {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    /// Seconds from now until the window resets.
    pub reset_in: Option<Duration>,
}

fn header_number(response: &ApiResponse, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| response.header(name))
        .and_then(|value| value.trim().parse().ok())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

impl Quota {
    pub fn of(response: &ApiResponse) -> Self {
        Self::at(response, unix_now())
    }

    /// `unix_now` is injectable for tests; reset headers are epoch seconds.
    pub fn at(response: &ApiResponse, unix_now: u64) -> Self {
        let reset_in = header_number(response, &["x-ratelimit-reset", "ratelimit-reset"])
            .map(|reset| Duration::from_secs(reset.saturating_sub(unix_now)))
            .filter(|wait| *wait <= MAX_RESET);
        let quota = Self {
            limit: header_number(response, &["x-ratelimit-limit", "ratelimit-limit"]),
            remaining: header_number(response, &["x-ratelimit-remaining", "ratelimit-remaining"]),
            reset_in,
        };
        if quota.limit.is_some() || quota.remaining.is_some() {
            return quota;
        }
        Self::structured(response).unwrap_or(quota)
    }

    /// The IETF draft's structured fields (Forgejo/Codeberg): `ratelimit: "baseline";r=1999;t=600`
    /// with `ratelimit-policy: "baseline";q=2000;w=600`. `t` is seconds, not an epoch.
    fn structured(response: &ApiResponse) -> Option<Self> {
        let parameter = |value: &str, key: &str| {
            value.split([';', ',']).find_map(|part| {
                let (name, number) = part.trim().split_once('=')?;
                (name.trim() == key)
                    .then(|| number.trim().parse::<u64>().ok())
                    .flatten()
            })
        };
        let current = response.header("ratelimit")?;
        let remaining = parameter(current, "r")?;
        Some(Self {
            limit: response
                .header("ratelimit-policy")
                .and_then(|policy| parameter(policy, "q")),
            remaining: Some(remaining),
            reset_in: parameter(current, "t")
                .map(Duration::from_secs)
                .filter(|wait| *wait <= MAX_RESET),
        })
    }

    /// Below `1/divisor` of the quota.
    pub fn in_reserve_of(&self, divisor: u64) -> bool {
        match (self.limit, self.remaining) {
            (Some(limit), Some(remaining)) if limit > 0 => {
                remaining.saturating_mul(divisor) < limit
            }
            _ => false,
        }
    }
}

fn retry_after(response: &ApiResponse) -> Option<Duration> {
    let seconds: u64 = response.header("retry-after")?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds)).filter(|wait| *wait <= MAX_RESET)
}

fn graphql_rate_limited(body: &[u8]) -> bool {
    // Cheap check first: most GraphQL bodies are large and never mention it.
    if !body.windows(12).any(|window| window == b"RATE_LIMITED") {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    value["errors"].as_array().is_some_and(|errors| {
        errors.iter().any(|error| {
            error["type"].as_str() == Some("RATE_LIMITED")
                || error["extensions"]["code"].as_str() == Some("RATE_LIMITED")
        })
    })
}

fn message(response: &ApiResponse) -> String {
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
    body["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            String::from_utf8_lossy(&response.body[..response.body.len().min(512)]).into_owned()
        })
        .to_ascii_lowercase()
}

/// Whether a response is a rate-limit trigger, and how long it asks us to wait.
pub fn trigger(response: &ApiResponse, graphql: bool) -> Option<Trigger> {
    trigger_at(response, graphql, unix_now())
}

pub fn trigger_at(response: &ApiResponse, graphql: bool, unix_now: u64) -> Option<Trigger> {
    let kind = match response.status {
        429 => TriggerKind::TooMany,
        403 => {
            let quota = Quota::at(response, unix_now);
            if quota.remaining == Some(0) {
                TriggerKind::QuotaExhausted
            } else {
                let text = message(response);
                if text.contains("abuse") || text.contains("secondary rate limit") {
                    TriggerKind::Abuse
                } else if text.contains("rate limit") {
                    TriggerKind::QuotaExhausted
                } else {
                    return None;
                }
            }
        }
        200 if graphql && graphql_rate_limited(&response.body) => TriggerKind::GraphQl,
        _ => return None,
    };
    let wait = retry_after(response).or_else(|| {
        let quota = Quota::at(response, unix_now);
        // A reset only means something when the quota is actually spent.
        (quota.remaining == Some(0) || kind == TriggerKind::GraphQl)
            .then_some(quota.reset_in)
            .flatten()
    });
    Some(Trigger { kind, wait })
}

#[derive(Clone, Debug, Default)]
struct Entry {
    paused_until: Option<Instant>,
    /// Consecutive triggers without an explicit wait; drives the doubling.
    strikes: u32,
    quota: Quota,
    /// When `quota` was observed, so `reset_in` can be aged.
    observed: Option<Instant>,
}

/// What the table says about one credential right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub paused_until: Option<Instant>,
    pub strikes: u32,
    pub quota: Quota,
}

#[derive(Default)]
pub struct RateLimiter {
    entries: Mutex<HashMap<Fingerprint, Entry>>,
}

/// The pause after `strikes` consecutive triggers without a server-given wait: 30 s, 60 s, …,
/// at most 15 min.
pub fn backoff(strikes: u32) -> Duration {
    let doublings = strikes.saturating_sub(1).min(16);
    FIRST_BACKOFF
        .saturating_mul(1 << doublings)
        .min(MAX_BACKOFF)
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<Fingerprint, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Before a request: `Err(until)` while the credential is paused, or for background work
    /// while the quota is inside the reserve (until its reset).
    pub fn check(
        &self,
        fingerprint: Fingerprint,
        class: Class,
        now: Instant,
    ) -> Result<(), Instant> {
        self.check_with(fingerprint, class, now, RESERVE_DIVISOR)
    }

    /// `check` for an anonymous caller: background work stops at half the
    /// quota, and once the forge has said none is left nothing is sent until its reset (the
    /// answer could only be a 403, and anonymous quotas are per address, shared with every
    /// other anonymous client there).
    pub fn check_anonymous(
        &self,
        fingerprint: Fingerprint,
        class: Class,
        now: Instant,
    ) -> Result<(), Instant> {
        if let Some(entry) = self.entries().get(&fingerprint)
            && entry.quota.remaining == Some(0)
            && let Some(reset) = entry
                .observed
                .zip(entry.quota.reset_in)
                .map(|(observed, reset_in)| observed + reset_in)
            && reset > now
        {
            return Err(reset);
        }
        self.check_with(fingerprint, class, now, ANONYMOUS_RESERVE_DIVISOR)
    }

    /// `check` with another reserve: background work stops below `1/divisor` of the quota.
    pub fn check_with(
        &self,
        fingerprint: Fingerprint,
        class: Class,
        now: Instant,
        divisor: u64,
    ) -> Result<(), Instant> {
        let mut entries = self.entries();
        let Some(entry) = entries.get_mut(&fingerprint) else {
            return Ok(());
        };
        if let Some(until) = entry.paused_until {
            if until > now {
                return Err(until);
            }
            entry.paused_until = None;
        }
        if class == Class::Background && entry.quota.in_reserve_of(divisor) {
            let reset = entry
                .observed
                .zip(entry.quota.reset_in)
                .map(|(observed, reset_in)| observed + reset_in);
            match reset {
                Some(reset) if reset > now => return Err(reset),
                // The window has reset since; the next response will say how much is left.
                _ => entry.quota = Quota::default(),
            }
        }
        Ok(())
    }

    /// After a response: record its quota; a trigger pauses the credential and returns until
    /// when.
    pub fn observe(
        &self,
        fingerprint: Fingerprint,
        response: &ApiResponse,
        graphql: bool,
        now: Instant,
    ) -> Option<Instant> {
        self.observe_at(fingerprint, response, graphql, now, unix_now())
    }

    pub fn observe_at(
        &self,
        fingerprint: Fingerprint,
        response: &ApiResponse,
        graphql: bool,
        now: Instant,
        unix_now: u64,
    ) -> Option<Instant> {
        let quota = Quota::at(response, unix_now);
        let trigger = trigger_at(response, graphql, unix_now);
        let mut entries = self.entries();
        let entry = entries.entry(fingerprint).or_default();
        // GitHub's search API has its own small quota (`x-ratelimit-resource: search`); it
        // says nothing about the core quota every other request spends.
        let other_resource = response
            .header("x-ratelimit-resource")
            .is_some_and(|resource| resource.contains("search"));
        if (quota.limit.is_some() || quota.remaining.is_some()) && !other_resource {
            entry.quota = quota;
            entry.observed = Some(now);
        }
        match trigger {
            Some(trigger) => Some(Self::pause(entry, trigger.wait, now)),
            None => {
                entry.strikes = 0;
                None
            }
        }
    }

    /// A trigger seen without a response (a CLI that only printed `rate limit` on stderr).
    pub fn trip(&self, fingerprint: Fingerprint, wait: Option<Duration>, now: Instant) -> Instant {
        let mut entries = self.entries();
        Self::pause(entries.entry(fingerprint).or_default(), wait, now)
    }

    fn pause(entry: &mut Entry, wait: Option<Duration>, now: Instant) -> Instant {
        let wait = match wait {
            // A server-given wait of 0 still needs a moment: the next request would trip again.
            Some(wait) => {
                entry.strikes = 0;
                wait.max(Duration::from_secs(1))
            }
            None => {
                entry.strikes = entry.strikes.saturating_add(1);
                backoff(entry.strikes)
            }
        };
        let until = now + wait;
        // Never shorten a pause that is already longer.
        let until = entry
            .paused_until
            .filter(|old| *old > until)
            .unwrap_or(until);
        entry.paused_until = Some(until);
        until
    }

    pub fn snapshot(&self, fingerprint: Fingerprint) -> Option<Snapshot> {
        self.entries().get(&fingerprint).map(|entry| Snapshot {
            paused_until: entry.paused_until,
            strikes: entry.strikes,
            quota: entry.quota,
        })
    }

    /// `r` in the accounts popover: forget every pause (the next response re-arms it).
    pub fn clear(&self) {
        self.entries().clear();
    }
}

pub fn global() -> &'static RateLimiter {
    static GLOBAL: OnceLock<RateLimiter> = OnceLock::new();
    GLOBAL.get_or_init(RateLimiter::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> ApiResponse {
        ApiResponse {
            status,
            headers: headers
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn structured_headers_and_the_search_quota() {
        let codeberg = response(
            200,
            &[
                ("ratelimit-policy", "\"baseline\";q=2000;w=600"),
                ("ratelimit", "\"baseline\";r=1999;t=600"),
            ],
            "{}",
        );
        assert_eq!(
            Quota::at(&codeberg, 0),
            Quota {
                limit: Some(2000),
                remaining: Some(1999),
                reset_in: Some(Duration::from_secs(600)),
            }
        );
        let limiter = RateLimiter::new();
        let fingerprint = Fingerprint::anonymous("https://api.github.com");
        let now = Instant::now();
        let core = response(
            200,
            &[("x-ratelimit-limit", "60"), ("x-ratelimit-remaining", "40")],
            "{}",
        );
        limiter.observe_at(fingerprint, &core, false, now, 0);
        let search = response(
            200,
            &[
                ("x-ratelimit-limit", "10"),
                ("x-ratelimit-remaining", "1"),
                ("x-ratelimit-resource", "search"),
            ],
            "{}",
        );
        limiter.observe_at(fingerprint, &search, false, now, 0);
        assert_eq!(
            limiter.snapshot(fingerprint).unwrap().quota.remaining,
            Some(40)
        );
        // 40 of 60 is outside both reserves; 20 of 60 is inside the anonymous one only.
        assert!(
            limiter
                .check_with(
                    fingerprint,
                    Class::Background,
                    now,
                    ANONYMOUS_RESERVE_DIVISOR
                )
                .is_ok()
        );
        let low = response(
            200,
            &[
                ("x-ratelimit-limit", "60"),
                ("x-ratelimit-remaining", "20"),
                ("x-ratelimit-reset", "600"),
            ],
            "{}",
        );
        limiter.observe_at(fingerprint, &low, false, now, 0);
        assert!(limiter.check(fingerprint, Class::Background, now).is_ok());
        assert!(
            limiter
                .check_with(
                    fingerprint,
                    Class::Background,
                    now,
                    ANONYMOUS_RESERVE_DIVISOR
                )
                .is_err()
        );
        assert!(
            limiter
                .check_with(
                    fingerprint,
                    Class::Interactive,
                    now,
                    ANONYMOUS_RESERVE_DIVISOR
                )
                .is_ok()
        );
        // Spent: an anonymous caller sends nothing until the reset; a credential is unaffected
        // (its 403 is the forge's to give).
        let spent = response(
            200,
            &[
                ("x-ratelimit-limit", "60"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "600"),
            ],
            "{}",
        );
        limiter.observe_at(fingerprint, &spent, false, now, 0);
        assert!(
            limiter
                .check_anonymous(fingerprint, Class::Interactive, now)
                .is_err()
        );
        assert!(limiter.check(fingerprint, Class::Interactive, now).is_ok());
        assert!(
            limiter
                .check_anonymous(
                    fingerprint,
                    Class::Interactive,
                    now + Duration::from_secs(601)
                )
                .is_ok()
        );
    }

    #[test]
    fn backoff_doubles_from_thirty_seconds_to_fifteen_minutes() {
        let waits: Vec<u64> = (1..=8).map(|strikes| backoff(strikes).as_secs()).collect();
        assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900, 900]);
    }

    #[test]
    fn triggers_cover_429_quota_abuse_and_graphql() {
        let now = 1_000_000;
        let reset = (now + 42).to_string();
        let cases = [
            (
                response(429, &[("retry-after", "7")], "{}"),
                false,
                Some(TriggerKind::TooMany),
                Some(7),
            ),
            (
                response(
                    403,
                    &[
                        ("x-ratelimit-remaining", "0"),
                        ("x-ratelimit-reset", &reset),
                    ],
                    "{}",
                ),
                false,
                Some(TriggerKind::QuotaExhausted),
                Some(42),
            ),
            (
                response(
                    403,
                    &[],
                    r#"{"message":"You have exceeded a secondary rate limit"}"#,
                ),
                false,
                Some(TriggerKind::Abuse),
                None,
            ),
            (
                response(403, &[], r#"{"message":"Resource not accessible"}"#),
                false,
                None,
                None,
            ),
            (
                response(
                    200,
                    &[],
                    r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"x"}]}"#,
                ),
                true,
                Some(TriggerKind::GraphQl),
                None,
            ),
            (
                response(200, &[], r#"{"data":{"x":"RATE_LIMITED"}}"#),
                true,
                None,
                None,
            ),
            (
                response(404, &[("retry-after", "9")], "{}"),
                false,
                None,
                None,
            ),
        ];
        for (response, graphql, kind, wait) in cases {
            let found = trigger_at(&response, graphql, now);
            assert_eq!(found.map(|found| found.kind), kind, "{response:?}");
            assert_eq!(
                found
                    .and_then(|found| found.wait)
                    .map(|wait| wait.as_secs()),
                wait
            );
        }
        // GitLab's unprefixed headers.
        let gitlab = response(
            429,
            &[("ratelimit-remaining", "0"), ("ratelimit-reset", &reset)],
            "",
        );
        assert_eq!(
            trigger_at(&gitlab, false, now).unwrap().wait,
            Some(Duration::from_secs(42))
        );
    }
}
