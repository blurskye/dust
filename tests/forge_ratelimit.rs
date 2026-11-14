#![cfg(unix)]
//! Rate limits as a state (FORGE.md §3.5, F2): 429, a 403 with the quota spent, an abuse
//! answer and GraphQL `RATE_LIMITED` pause the credential until `retry-after` / the reset, else
//! 30 s doubling to 15 min. The pause is per credential fingerprint and is honoured locally: no
//! request reaches the server while it lasts. Background work keeps off the last 10 % of the
//! quota. At most 8 HTTP requests and 4 CLI processes run at once.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dust::forge::cli::CliTransport;
use dust::forge::credential::Credential;
use dust::forge::credential::CredentialSource;
use dust::forge::credential::Secret;
use dust::forge::host_state;
use dust::forge::host_state::HostState;
use dust::forge::http::HttpTransport;
use dust::forge::provider::ForgeContext;
use dust::forge::ratelimit;
use dust::forge::ratelimit::Fingerprint;
use dust::forge::ratelimit::RateLimiter;
use dust::forge::ratelimit::TriggerKind;
use dust::forge::request::ForgeError;
use dust::forge::transport;
use dust::forge::transport::ApiRequest;
use dust::forge::transport::ApiResponse;
use dust::forge::transport::Class;
use dust::forge::transport::Cli;
use dust::forge::transport::ConnectOptions;
use dust::forge::transport::Endpoints;
use dust::forge::transport::HostKey;
use dust::forge::transport::Transport;
use dust::forge::transport::TransportError;
use forge_fake::FAKE_TOKEN;
use forge_fake::ForgeServer;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;

struct World {
    directory: tempfile::TempDir,
    server: ForgeServer,
    gh: PathBuf,
}

impl World {
    fn new(state: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        std::fs::write(&path, state.to_string()).unwrap();
        let server = ForgeServer::start(&path, "legacy");
        let gh = server.install_gh(&directory.path().join("bin"));
        Self { directory, server, gh }
    }

    fn requests(&self) -> usize {
        forge_fake::requests(&self.directory.path().join("state.json")).len()
    }

    fn endpoints(&self, host: &str) -> Endpoints {
        let mut endpoints = Endpoints::github(&HostKey::new(host));
        endpoints.override_api_base(&format!("{}/{host}/api/v3", self.server.url()));
        endpoints
    }

    fn http(&self, host: &str) -> HttpTransport {
        HttpTransport::new(
            self.endpoints(host),
            Credential::new(FAKE_TOKEN, CredentialSource::GhAuthToken, None),
        )
        .unwrap()
    }

    fn gh(&self) -> CliTransport {
        CliTransport::new(Cli::Gh, self.gh.clone(), None)
    }

    fn max_in_flight(&self) -> u64 {
        let text = reqwest::blocking::get(format!("{}/_fake/stats", self.server.url()))
            .unwrap()
            .text()
            .unwrap();
        let stats: Value = serde_json::from_str(&text).unwrap();
        stats["max_in_flight"].as_u64().unwrap()
    }
}

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

fn send(transport: &dyn Transport, host: &str) -> Result<ApiResponse, TransportError> {
    transport.send(&HostKey::new(host), &ApiRequest::get("user"), &AtomicBool::new(false))
}

#[test]
fn triggers_and_waits() {
    let limiter = RateLimiter::new();
    let token = Fingerprint::token("https://api.github.com", &Secret::new("ghp_triggerfixture0123456789abcd"));
    let other = Fingerprint::token("https://api.github.com", &Secret::new("ghp_otherfixture0123456789abcdef"));
    let now = Instant::now();
    let epoch = 1_800_000_000;

    // 429 + retry-after.
    let until = limiter
        .observe_at(token, &response(429, &[("retry-after", "7")], "{}"), false, now, epoch)
        .unwrap();
    assert_eq!(until, now + Duration::from_secs(7));
    assert_eq!(limiter.check(token, Class::Interactive, now + Duration::from_secs(6)), Err(until));
    assert_eq!(limiter.check(other, Class::Interactive, now), Ok(()), "only that credential pauses");
    assert_eq!(limiter.check(token, Class::Interactive, now + Duration::from_secs(7)), Ok(()));

    // 403 with the quota spent: until the reset header.
    let spent = response(
        403,
        &[("x-ratelimit-limit", "5000"), ("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "1800000090")],
        r#"{"message":"API rate limit exceeded"}"#,
    );
    assert_eq!(ratelimit::trigger_at(&spent, false, epoch).unwrap().kind, TriggerKind::QuotaExhausted);
    let until = limiter.observe_at(token, &spent, false, now, epoch).unwrap();
    assert_eq!(until, now + Duration::from_secs(90));

    // GitLab's headers.
    let gitlab = response(429, &[("ratelimit-remaining", "0"), ("ratelimit-reset", "1800000030")], "{}");
    let trigger = ratelimit::trigger_at(&gitlab, false, epoch).unwrap();
    assert_eq!(trigger.wait, Some(Duration::from_secs(30)));

    // Abuse answers without a wait back off 30 s, 60 s, … at most 15 min; a success resets.
    let abuse = response(403, &[], r#"{"message":"You have exceeded a secondary rate limit"}"#);
    assert_eq!(ratelimit::trigger_at(&abuse, false, epoch).unwrap().kind, TriggerKind::Abuse);
    let fresh = Fingerprint::token("https://api.github.com", &Secret::new("ghp_abusefixture0123456789abcdef"));
    let mut waits = Vec::new();
    let mut at = now;
    for _ in 0..7 {
        let until = limiter.observe_at(fresh, &abuse, false, at, epoch).unwrap();
        waits.push(until.duration_since(at).as_secs());
        at = until;
    }
    assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900]);
    limiter.observe_at(fresh, &response(200, &[], "{}"), false, at, epoch);
    assert_eq!(limiter.snapshot(fresh).unwrap().strikes, 0);

    // A plain 403 is a permission problem, not a limit.
    assert!(ratelimit::trigger_at(&response(403, &[], r#"{"message":"Resource not accessible"}"#), false, epoch).is_none());
    // GraphQL reports limits inside a 200.
    let graphql = response(200, &[], r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#);
    assert_eq!(ratelimit::trigger_at(&graphql, true, epoch).unwrap().kind, TriggerKind::GraphQl);
    assert!(ratelimit::trigger_at(&graphql, false, epoch).is_none(), "only GraphQL bodies are read");
}

#[test]
fn background_requests_keep_a_ten_percent_reserve() {
    let limiter = RateLimiter::new();
    let token = Fingerprint::token("https://gitlab.example/api/v4", &Secret::new("glpat-reservefixture0123456789"));
    let now = Instant::now();
    let epoch = 1_800_000_000;
    let low = response(
        200,
        &[("ratelimit-limit", "2000"), ("ratelimit-remaining", "150"), ("ratelimit-reset", "1800000060")],
        "{}",
    );
    assert_eq!(limiter.observe_at(token, &low, false, now, epoch), None);
    assert_eq!(limiter.check(token, Class::Interactive, now), Ok(()));
    assert_eq!(limiter.check(token, Class::Background, now), Err(now + Duration::from_secs(60)));
    assert_eq!(limiter.check(token, Class::Background, now + Duration::from_secs(61)), Ok(()), "after the reset");
    let plenty = response(200, &[("ratelimit-limit", "2000"), ("ratelimit-remaining", "201")], "{}");
    limiter.observe_at(token, &plenty, false, now, epoch);
    assert_eq!(limiter.check(token, Class::Background, now), Ok(()));
}

#[test]
fn a_paused_credential_sends_nothing_over_http_or_the_cli() {
    for via in ["http", "gh"] {
        let world = World::new(json!({
            "fail": [{"path": "user", "status": 429, "headers": {"Retry-After": "120"},
                      "message": "Too many requests", "times": 1}]
        }));
        let host = format!("{via}.paused.test");
        let transport: Arc<dyn Transport> = match via {
            "http" => Arc::new(world.http(&host)),
            _ => Arc::new(world.gh()),
        };
        let started = Instant::now();
        let Err(TransportError::RateLimited { until }) = send(transport.as_ref(), &host) else {
            panic!("{via}: the 429 is a rate limit");
        };
        let wait = until.duration_since(started);
        assert!(wait > Duration::from_secs(110) && wait <= Duration::from_secs(121), "{via}: {wait:?}");
        let seen = world.requests();
        for _ in 0..3 {
            assert!(matches!(send(transport.as_ref(), &host), Err(TransportError::RateLimited { .. })), "{via}");
        }
        assert_eq!(world.requests(), seen, "{via}: nothing reached the server while paused");
        let error = ForgeError::from(TransportError::RateLimited { until });
        assert!(error.to_string().starts_with("Rate limited · retry in "), "{error}");
    }
}

#[test]
fn a_rate_limit_becomes_the_hosts_state() {
    let world = World::new(json!({
        "fail": [{"path": "user", "status": 403, "message": "API rate limit exceeded",
                  "headers": {"X-RateLimit-Remaining": "0", "X-RateLimit-Limit": "60"}, "times": 1}]
    }));
    let host = "state.paused.test";
    let ctx = ForgeContext::new(
        world.directory.path().to_path_buf(),
        world.endpoints(host),
        ConnectOptions {
            gh: Some(world.gh.clone()),
            ..ConnectOptions::default()
        },
        None,
        Arc::new(world.http(host)),
    );
    let error = ctx.send(&ApiRequest::get("user"), &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(error, ForgeError::RateLimited { .. }), "{error:?}");
    let account = host_state::account(&HostKey::new(host)).unwrap();
    let HostState::RateLimited { .. } = account.state else {
        panic!("{:?}", account.state);
    };
    let (notice, _) = account.state.notice(host, None).unwrap();
    assert!(notice.starts_with("⚠ Rate limited by state.paused.test · resumes in "), "{notice}");
}

#[test]
fn at_most_eight_http_requests_and_four_cli_processes_run_at_once() {
    assert_eq!((transport::HTTP_IN_FLIGHT, transport::CLI_IN_FLIGHT), (8, 4));
    for (via, bound, count) in [("http", 8, 20), ("gh", 4, 10)] {
        let world = World::new(json!({"delay_ms": 250}));
        let host = format!("{via}.bound.test");
        let transport: Arc<dyn Transport> = match via {
            "http" => Arc::new(world.http(&host)),
            _ => Arc::new(world.gh()),
        };
        let threads: Vec<_> = (0..count)
            .map(|_| {
                let transport = transport.clone();
                let host = host.clone();
                std::thread::spawn(move || send(transport.as_ref(), &host).map(|response| response.status))
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap().unwrap(), 200, "{via}");
        }
        let seen = world.max_in_flight();
        assert!(seen <= bound, "{via}: {seen} requests at once");
        assert!(seen >= 2, "{via}: requests did run in parallel ({seen})");
    }
}
