//! Request descriptors shared by both transports: the provider layer builds
//! `ApiRequest`s once and sends them either directly over HTTP or through the forge's CLI
//! (`gh api --include`). Also host keys, in-flight bounds and transport selection.

use crate::credential;
use crate::credential::CredentialSource;
use crate::credential::CredentialState;
use crate::provider::ProviderKind;
use crate::request::ForgeError;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(60);
pub const LOG_TIMEOUT: Duration = Duration::from_secs(300);
pub const BODY_CAP: usize = 8 * 1024 * 1024;
pub const LARGE_BODY_CAP: usize = 16 * 1024 * 1024;
/// A `Read` may fan out to several requests; all of them share this deadline.
pub const READ_DEADLINE: Duration = Duration::from_secs(30);
pub const HTTP_IN_FLIGHT: usize = 8;
pub const HTTP_BACKGROUND_IN_FLIGHT: usize = 2;
pub const CLI_IN_FLIGHT: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum Target {
    /// A path relative to the API base, without a leading slash: `repos/o/r/pulls`.
    Rest(String),
    GraphQl,
    /// A full URL, e.g. a blob a log redirect points at.
    Absolute(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Body {
    Json(Value),
    Form(Vec<(String, String)>),
    Raw(Vec<u8>),
}

impl Body {
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Body::Json(value) => value.to_string().into_bytes(),
            Body::Form(pairs) => pairs
                .iter()
                .map(|(key, value)| format!("{}={}", form_escape(key), form_escape(value)))
                .collect::<Vec<_>>()
                .join("&")
                .into_bytes(),
            Body::Raw(bytes) => bytes.clone(),
        }
    }

    pub fn content_type(&self) -> &'static str {
        match self {
            Body::Json(_) => "application/json",
            Body::Form(_) => "application/x-www-form-urlencoded",
            Body::Raw(_) => "application/octet-stream",
        }
    }
}

fn form_escape(text: &str) -> String {
    let mut escaped = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum Accept {
    Json,
    Text,
    Diff,
    /// File contents (`application/vnd.github.raw`).
    Raw,
    Bytes,
}

impl Accept {
    pub fn header(self, kind: ProviderKind) -> &'static str {
        match (self, kind) {
            (Accept::Json, ProviderKind::GitHub) => "application/vnd.github+json",
            (Accept::Diff, ProviderKind::GitHub) => "application/vnd.github.diff",
            (Accept::Raw, ProviderKind::GitHub) => "application/vnd.github.raw",
            (Accept::Json, _) => "application/json",
            (Accept::Diff, _) => "text/plain",
            (Accept::Text | Accept::Raw, _) => "text/plain, */*",
            (Accept::Bytes, _) => "*/*",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize)]
pub enum Class {
    #[default]
    Interactive,
    Background,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ApiRequest {
    pub method: Method,
    pub target: Target,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
    pub accept: Accept,
    pub class: Class,
    #[serde(skip)]
    pub timeout: Duration,
    pub body_cap: usize,
    /// Over CLI passthrough the body goes in a private temporary file (`--input FILE`,
    /// `-d @FILE`) instead of stdin: long authored text (a pull request's description, a
    /// review with its drafts), like `gh pr create --body-file`. Direct HTTP ignores it.
    #[serde(skip)]
    pub body_file: bool,
}

impl ApiRequest {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            target: Target::Rest(path.into().trim_start_matches('/').to_owned()),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            accept: Accept::Json,
            class: Class::Interactive,
            timeout: DEFAULT_TIMEOUT,
            body_cap: BODY_CAP,
            body_file: false,
        }
    }

    /// Send the body through a temporary file over CLI passthrough (see `body_file`).
    pub fn body_file(mut self) -> Self {
        self.body_file = true;
        self
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::Get, path)
    }

    /// A GraphQL document; its operation name identifies it in logs and fixtures.
    pub fn graphql(document: &str, variables: Value) -> Self {
        Self {
            method: Method::Post,
            target: Target::GraphQl,
            body: Some(Body::Json(
                serde_json::json!({ "query": document, "variables": variables }),
            )),
            ..Self::get("")
        }
    }

    pub fn query(mut self, key: &str, value: impl ToString) -> Self {
        self.query.push((key.into(), value.to_string()));
        self
    }

    pub fn header(mut self, key: &str, value: impl Into<String>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    pub fn json(mut self, body: Value) -> Self {
        self.body = Some(Body::Json(body));
        self
    }

    pub fn accept(mut self, accept: Accept) -> Self {
        self.accept = accept;
        if matches!(
            accept,
            Accept::Diff | Accept::Text | Accept::Raw | Accept::Bytes
        ) {
            self.timeout = self.timeout.max(DIFF_TIMEOUT);
            self.body_cap = self.body_cap.max(LARGE_BODY_CAP);
        }
        self
    }

    pub fn class(mut self, class: Class) -> Self {
        self.class = class;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// `repos/o/r/pulls?state=open&page=2`, the form CLIs take and fixtures are keyed by.
    pub fn path_and_query(&self) -> String {
        let path = match &self.target {
            Target::Rest(path) => path.clone(),
            Target::GraphQl => "graphql".into(),
            Target::Absolute(url) => url.clone(),
        };
        if self.query.is_empty() {
            return path;
        }
        let query = self
            .query
            .iter()
            .map(|(key, value)| format!("{}={}", form_escape(key), form_escape(value)))
            .collect::<Vec<_>>()
            .join("&");
        format!(
            "{path}{}{query}",
            if path.contains('?') { '&' } else { '?' }
        )
    }

    /// The operation name of a GraphQL document (`query PullRequestList(…)` → `PullRequestList`).
    pub fn graphql_operation(&self) -> Option<String> {
        let Some(Body::Json(body)) = &self.body else {
            return None;
        };
        operation_name(body["query"].as_str()?)
    }

    /// One log-safe line: method, path or operation, never headers or the body.
    pub fn describe(&self) -> String {
        match (&self.target, self.graphql_operation()) {
            (Target::GraphQl, Some(operation)) => format!("POST graphql {operation}"),
            _ => format!("{} {}", self.method.as_str(), self.path_and_query()),
        }
    }
}

pub fn operation_name(document: &str) -> Option<String> {
    let document = document.trim_start();
    let rest = document
        .strip_prefix("query")
        .or_else(|| document.strip_prefix("mutation"))?;
    let name = rest
        .trim_start()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()?;
    (!name.is_empty()).then(|| name.to_owned())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    /// Lower-cased names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ApiResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<Value, ForgeError> {
        serde_json::from_slice(&self.body)
            .map_err(|error| ForgeError::BadResponse(format!("invalid JSON: {error}")))
    }

    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Link: <…>; rel="next"` (GitHub, Gitea).
    pub fn next_link(&self) -> Option<String> {
        self.header("link")?.split(',').find_map(|part| {
            let (url, params) = part.split_once(';')?;
            params
                .split(';')
                .any(|param| param.trim() == "rel=\"next\"")
                .then(|| {
                    url.trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_owned()
                })
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
pub enum Cli {
    Gh,
    Glab,
    Tea,
}

impl Cli {
    pub fn name(self) -> &'static str {
        match self {
            Cli::Gh => "gh",
            Cli::Glab => "glab",
            Cli::Tea => "tea",
        }
    }

    /// The environment variable naming an explicit executable (tests, odd installs).
    pub fn env_override(self) -> &'static str {
        match self {
            Cli::Gh => "DUSTY_FORGE_GH",
            Cli::Glab => "DUSTY_FORGE_GLAB",
            Cli::Tea => "DUSTY_FORGE_TEA",
        }
    }

    pub fn executable(self) -> Option<PathBuf> {
        // The tutor's practice forge runs no CLI: none is installed as far as it knows.
        if crate::sandbox::active() {
            return None;
        }
        if let Some(path) = std::env::var_os(self.env_override()).filter(|path| !path.is_empty()) {
            return Some(PathBuf::from(path));
        }
        crate::process::discover_executable(self.name())
            .and_then(|path| std::path::absolute(path).ok())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum TransportKind {
    Http {
        credential_source: CredentialSource,
    },
    Cli(Cli),
    /// Direct HTTP without a credential (the anonymous read path): reads of public
    /// repositories only, every write refused before it is sent.
    Anonymous,
}

impl TransportKind {
    pub fn anonymous(&self) -> bool {
        matches!(self, TransportKind::Anonymous)
    }
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportKind::Http { .. } => formatter.write_str("direct"),
            TransportKind::Cli(cli) => write!(formatter, "via {}", cli.name()),
            TransportKind::Anonymous => formatter.write_str("anonymous"),
        }
    }
}

/// A forge host; the port is part of the identity (`git.corp.example:8443`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize)]
pub struct HostKey {
    pub host: String,
    pub port: Option<u16>,
}

impl HostKey {
    pub fn new(host: &str) -> Self {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        match host.rsplit_once(':') {
            Some((name, port)) if port.parse::<u16>().is_ok() && !name.contains(':') => Self {
                host: name.into(),
                port: port.parse().ok(),
            },
            _ => Self { host, port: None },
        }
    }
}

impl std::fmt::Display for HostKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.port {
            Some(port) => write!(formatter, "{}:{port}", self.host),
            None => formatter.write_str(&self.host),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    Network(String),
    Timeout,
    Cancelled,
    TooLarge(usize),
    Insecure(String),
    Spawn(String),
    /// The CLI failed without an HTTP response to show for it.
    Cli {
        cli: Cli,
        code: Option<i32>,
        stderr: String,
    },
    /// Refused or answered as rate limited; `ratelimit.rs` holds the pause.
    RateLimited {
        until: Instant,
    },
    /// The anonymous transport refused a write (or GitHub GraphQL) without sending it.
    NeedsSignIn {
        host: String,
        hint: String,
    },
    /// `DUSTY_FORGE_READONLY=1` refused a request that is not a GET, before it was sent.
    ReadOnly(String),
}

impl From<TransportError> for ForgeError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Network(message) => ForgeError::Network(crate::cli::redact(&message)),
            TransportError::Timeout => ForgeError::Timeout,
            TransportError::Cancelled => ForgeError::Cancelled,
            TransportError::TooLarge(cap) => {
                ForgeError::BadResponse(format!("response exceeds {} MiB", cap / (1024 * 1024)))
            }
            TransportError::Insecure(message) => ForgeError::Failed(message),
            TransportError::Spawn(message) => ForgeError::Failed(crate::cli::redact(&message)),
            TransportError::Cli { cli, code, stderr } => crate::cli::classify(cli, &stderr, code),
            TransportError::RateLimited { until } => ForgeError::RateLimited { until },
            TransportError::NeedsSignIn { host, hint } => {
                ForgeError::NotAuthenticated { host, hint }
            }
            TransportError::ReadOnly(what) => ForgeError::Failed(format!(
                "{what} not sent: read-only run (DUSTY_FORGE_READONLY=1)"
            )),
        }
    }
}

pub trait Transport: Send + Sync {
    fn kind(&self) -> TransportKind;
    fn send(
        &self,
        host: &HostKey,
        request: &ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<ApiResponse, TransportError>;
    fn graphql_batching(&self) -> bool;
}

/// Where a host's API lives. F1 derives it from the host; F2's `detect.rs`
/// adds probes, config and `hosts.json`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct Endpoints {
    pub kind: ProviderKind,
    pub host: HostKey,
    pub api_base: String,
    pub graphql: Option<String>,
    pub web_base: String,
}

impl Endpoints {
    /// github.com → api.github.com; `*.ghe.com` → api.<host>; other hosts → GHES paths.
    /// `DUSTY_FORGE_API_BASE` / `DUSTY_FORGE_GRAPHQL_URL` override them (tests, odd installs).
    pub fn github(host: &HostKey) -> Self {
        let name = host.to_string();
        let (api_base, graphql) = if host.host == "github.com" {
            (
                "https://api.github.com".to_owned(),
                "https://api.github.com/graphql".to_owned(),
            )
        } else if host.host.ends_with(".ghe.com") {
            (
                format!("https://api.{name}"),
                format!("https://api.{name}/graphql"),
            )
        } else {
            (
                format!("https://{name}/api/v3"),
                format!("https://{name}/api/graphql"),
            )
        };
        let mut endpoints = Self {
            kind: ProviderKind::GitHub,
            host: host.clone(),
            api_base,
            graphql: Some(graphql),
            web_base: format!("https://{name}"),
        };
        // `{host}` lets one loopback test server stand in for several hosts.
        if let Some(base) =
            crate::sandbox::var("DUSTY_FORGE_API_BASE").filter(|base| !base.is_empty())
        {
            endpoints.override_api_base(&base.replace("{host}", &name));
        }
        if let Some(url) =
            crate::sandbox::var("DUSTY_FORGE_GRAPHQL_URL").filter(|url| !url.is_empty())
        {
            endpoints.graphql = Some(url.replace("{host}", &name));
        }
        endpoints
    }

    pub fn override_api_base(&mut self, base: &str) {
        let base = base.trim_end_matches('/').to_owned();
        self.graphql = Some(match base.strip_suffix("/api/v3") {
            Some(root) => format!("{root}/api/graphql"),
            None => format!("{base}/graphql"),
        });
        self.api_base = base;
    }

    /// Plain `http://` is only for a loopback test server.
    pub fn check_scheme(url: &str) -> Result<(), TransportError> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| TransportError::Insecure(format!("invalid forge URL {url}")))?;
        match parsed.scheme() {
            "https" => Ok(()),
            "http" if matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) => {
                Ok(())
            }
            _ => Err(TransportError::Insecure(
                "forge APIs are only reached over https".into(),
            )),
        }
    }
}

/// Explicit choices for one connection; `from_env` reads them for the app.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ConnectOptions {
    pub force: Option<Force>,
    pub gh: Option<PathBuf>,
    pub glab: Option<PathBuf>,
    pub tea: Option<PathBuf>,
    pub offline: bool,
    /// Where glab, fj and tea keep their sign-ins.
    pub stores: credential::Stores,
    /// `[forge.hosts."H"] login`: the tea login passthrough uses for that host.
    pub logins: std::collections::BTreeMap<HostKey, String>,
    /// `DUSTY_FORGE_ANONYMOUS=0`: no anonymous read path; a host without a credential or a
    /// signed-in CLI is a state (CLI missing, signed out) instead.
    pub no_anonymous: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Force {
    Http,
    Cli,
}

impl ConnectOptions {
    pub fn from_env() -> Self {
        Self {
            force: match crate::sandbox::var("DUSTY_FORGE_TRANSPORT").as_deref() {
                Some("http") => Some(Force::Http),
                Some("cli") => Some(Force::Cli),
                _ => None,
            },
            gh: Cli::Gh.executable(),
            glab: Cli::Glab.executable(),
            tea: Cli::Tea.executable(),
            offline: crate::sandbox::var("DUSTY_FORGE_OFFLINE").is_some_and(|value| value == "1"),
            no_anonymous: crate::sandbox::var("DUSTY_FORGE_ANONYMOUS")
                .is_some_and(|value| value == "0"),
            stores: credential::Stores::from_env(&credential::Env::process()),
            logins: crate::detect::ForgeConfig::load()
                .hosts
                .into_iter()
                .filter_map(|(host, config)| Some((host, config.login?)))
                .collect(),
        }
    }
}

/// A counting semaphore: the per-process in-flight bounds.
pub struct Bound {
    limit: usize,
    used: Mutex<usize>,
    ready: Condvar,
}

pub struct Permit<'a>(&'a Bound);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut used = self
            .0
            .used
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *used -= 1;
        self.0.ready.notify_one();
    }
}

impl Bound {
    pub const fn new(limit: usize) -> Self {
        Self {
            limit,
            used: Mutex::new(0),
            ready: Condvar::new(),
        }
    }

    pub fn acquire(&self, cancel: &AtomicBool) -> Result<Permit<'_>, TransportError> {
        let mut used = self.used.lock().unwrap_or_else(|error| error.into_inner());
        while *used >= self.limit {
            if cancel.load(Ordering::Acquire) {
                return Err(TransportError::Cancelled);
            }
            used = self
                .ready
                .wait_timeout(used, Duration::from_millis(20))
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
        *used += 1;
        Ok(Permit(self))
    }
}

pub static HTTP_BOUND: Bound = Bound::new(HTTP_IN_FLIGHT);
pub static HTTP_BACKGROUND_BOUND: Bound = Bound::new(HTTP_BACKGROUND_IN_FLIGHT);
pub static CLI_BOUND: Bound = Bound::new(CLI_IN_FLIGHT);

type Selected = (Arc<dyn Transport>, Instant);

fn selected() -> &'static Mutex<HashMap<(Endpoints, ConnectOptions), Selected>> {
    static SELECTED: OnceLock<Mutex<HashMap<(Endpoints, ConnectOptions), Selected>>> =
        OnceLock::new();
    SELECTED.get_or_init(Default::default)
}

const SELECTION_TTL: Duration = Duration::from_secs(5 * 60);

/// `DUSTY_FORGE_READONLY=1`: smoke runs against real forges send reads only.
pub fn read_only() -> bool {
    crate::sandbox::var("DUSTY_FORGE_READONLY").is_some_and(|value| value == "1")
}

/// `DUSTY_FORGE_READONLY_LOG=<path>`: one line per request of a read-only run, for counting
/// against anonymous rate limits: host, transport, outcome, milliseconds and `describe()` (method and
/// path only, never headers, tokens or bodies).
fn read_only_log(
    kind: &TransportKind,
    host: &HostKey,
    request: &ApiRequest,
    outcome: &str,
    took: Duration,
) {
    let Some(path) =
        crate::sandbox::var("DUSTY_FORGE_READONLY_LOG").filter(|path| !path.is_empty())
    else {
        return;
    };
    let line = format!(
        "{host} {kind} {outcome} {}ms {}\n",
        took.as_millis(),
        request.describe()
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = std::io::Write::write_all(&mut file, line.as_bytes());
    }
}

/// Counts one more request to `host` and says whether it is still under `cap`.
fn read_only_admit(host: &HostKey, cap: Option<usize>) -> bool {
    static SENT: OnceLock<Mutex<HashMap<HostKey, usize>>> = OnceLock::new();
    let mut sent = SENT
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let count = sent.entry(host.clone()).or_default();
    if cap.is_some_and(|cap| *count >= cap) {
        return false;
    }
    *count += 1;
    true
}

/// Wraps whichever transport was chosen (direct, CLI or anonymous) and refuses every request
/// but a read before it reaches the network: a GET, or a GraphQL document that cannot be a
/// write (`read_only_query`).
pub struct ReadOnly(pub Arc<dyn Transport>);

/// A GraphQL POST that can only read: a write must be declared with `mutation` (and a stream
/// with `subscription`), so a document without either word anywhere, even inside a string,
/// is a query. A false refusal is safe; nothing else gets through.
fn read_only_query(request: &ApiRequest) -> bool {
    let (Method::Post, Target::GraphQl, Some(Body::Json(body))) =
        (&request.method, &request.target, &request.body)
    else {
        return false;
    };
    let only_query = body.as_object().is_some_and(|fields| {
        fields
            .keys()
            .all(|key| matches!(key.as_str(), "query" | "variables" | "operationName"))
    });
    only_query
        && body["query"].as_str().is_some_and(|document| {
            !document.contains("mutation") && !document.contains("subscription")
        })
}

impl Transport for ReadOnly {
    fn kind(&self) -> TransportKind {
        self.0.kind()
    }

    fn send(
        &self,
        host: &HostKey,
        request: &ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<ApiResponse, TransportError> {
        if request.method != Method::Get && !read_only_query(request) {
            read_only_log(&self.0.kind(), host, request, "refused", Duration::ZERO);
            return Err(TransportError::ReadOnly(request.describe()));
        }
        // `DUSTY_FORGE_READONLY_MAX=<n>`: at most n requests per host for the whole process,
        // so a runaway poll cannot hammer a real forge.
        let cap = crate::sandbox::var("DUSTY_FORGE_READONLY_MAX")
            .and_then(|max| max.parse::<usize>().ok());
        if !read_only_admit(host, cap) {
            read_only_log(&self.0.kind(), host, request, "capped", Duration::ZERO);
            return Err(TransportError::ReadOnly(format!(
                "{} (request cap reached)",
                request.describe()
            )));
        }
        let started = Instant::now();
        let result = self.0.send(host, request, cancel);
        let outcome = match &result {
            Ok(response) => response.status.to_string(),
            Err(error) => match error {
                TransportError::Network(_) => "network",
                TransportError::Timeout => "timeout",
                TransportError::Cancelled => "cancelled",
                TransportError::TooLarge(_) => "too-large",
                TransportError::Insecure(_) => "insecure",
                TransportError::Spawn(_) => "spawn",
                TransportError::Cli { .. } => "cli",
                TransportError::RateLimited { .. } => "rate-limited",
                TransportError::NeedsSignIn { .. } => "needs-sign-in",
                TransportError::ReadOnly(_) => "refused",
            }
            .to_owned(),
        };
        read_only_log(&self.0.kind(), host, request, &outcome, started.elapsed());
        result
    }

    fn graphql_batching(&self) -> bool {
        false
    }
}

/// Drop every remembered transport choice (the accounts popover's re-check).
pub fn forget_selections() {
    selected()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

/// The transport for a host: direct HTTP when a credential exists and its live
/// `GET /user` check passed; otherwise the CLI when it is installed (and, for tea, signed in);
/// otherwise anonymous direct HTTP for reads. With `no_anonymous` the last step is an
/// error the UI shows as a state.
pub fn for_host(
    endpoints: &Endpoints,
    options: &ConnectOptions,
    cancel: &AtomicBool,
) -> Result<Arc<dyn Transport>, ForgeError> {
    if options.offline {
        return Err(ForgeError::Network(
            "offline (DUSTY_FORGE_OFFLINE=1)".into(),
        ));
    }
    let key = (endpoints.clone(), options.clone());
    if let Some((transport, at)) = selected()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
        && at.elapsed() < SELECTION_TTL
    {
        return Ok(transport.clone());
    }
    let transport = match choose(endpoints, options, cancel) {
        Ok(transport) if read_only() => Arc::new(ReadOnly(transport)) as Arc<dyn Transport>,
        Ok(transport) => transport,
        Err(error) => {
            crate::host_state::observe_error(endpoints, &error);
            return Err(error);
        }
    };
    crate::host_state::observe_transport(endpoints, transport.kind());
    selected()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key, (transport.clone(), Instant::now()));
    Ok(transport)
}

fn choose(
    endpoints: &Endpoints,
    options: &ConnectOptions,
    cancel: &AtomicBool,
) -> Result<Arc<dyn Transport>, ForgeError> {
    if options.force != Some(Force::Cli) {
        match credential::resolve(endpoints, options, cancel) {
            CredentialState::Present(credential) => {
                let http = crate::http::HttpTransport::new(endpoints.clone(), credential)?;
                match http.check_viewer(cancel) {
                    Ok(()) => return Ok(Arc::new(http)),
                    Err(ForgeError::NotAuthenticated { .. }) => {
                        credential::evict(endpoints, options);
                    }
                    Err(error) if options.force == Some(Force::Http) => return Err(error),
                    Err(ForgeError::Cancelled) => return Err(ForgeError::Cancelled),
                    // An unreachable host is not a reason to stop trusting the token.
                    Err(error @ (ForgeError::Network(_) | ForgeError::Timeout)) => {
                        return Err(error);
                    }
                    // The forge knows the token and refused `GET /user` for a scope (a Gitea
                    // token without `read:user`) or a quota: keep it, so each read says what
                    // it lacks instead of going anonymous ("not signed in").
                    Err(ForgeError::Forbidden(_) | ForgeError::RateLimited { .. }) => {
                        return Ok(Arc::new(http));
                    }
                    Err(_) => {}
                }
            }
            CredentialState::Missing { hint, .. } if options.force == Some(Force::Http) => {
                return Err(ForgeError::NotAuthenticated {
                    host: endpoints.host.to_string(),
                    hint,
                });
            }
            CredentialState::Crashed(error) if options.force == Some(Force::Http) => {
                return Err(ForgeError::Failed(error));
            }
            _ => {}
        }
        if options.force == Some(Force::Http) {
            return Err(ForgeError::NotAuthenticated {
                host: endpoints.host.to_string(),
                hint: credential::login_hint(endpoints.kind, &endpoints.host),
            });
        }
    }
    match passthrough(endpoints, options) {
        // No CLI (or one that cannot sign in): public repositories stay readable.
        Err(
            ForgeError::CliMissing(_)
            | ForgeError::CliTooOld { .. }
            | ForgeError::NotAuthenticated { .. },
        ) if options.force.is_none() && !options.no_anonymous => anonymous(endpoints),
        result => result,
    }
}

/// Direct HTTP without a credential: GETs (and GitLab GraphQL queries) only.
pub fn anonymous(endpoints: &Endpoints) -> Result<Arc<dyn Transport>, ForgeError> {
    Ok(Arc::new(crate::http::HttpTransport::anonymous(
        endpoints.clone(),
    )?))
}

/// A passthrough CLI turned out to be signed out (`gh` installed, never logged in): reads of
/// this host go anonymous from now on, until the selection expires or `r` re-checks.
pub fn demote(endpoints: &Endpoints, options: &ConnectOptions) -> Option<Arc<dyn Transport>> {
    if options.force.is_some() || options.no_anonymous {
        return None;
    }
    let transport = anonymous(endpoints).ok()?;
    selected()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            (endpoints.clone(), options.clone()),
            (transport.clone(), Instant::now()),
        );
    crate::host_state::observe_transport(endpoints, transport.kind());
    Some(transport)
}

/// The CLI transport for a host, regardless of credentials.
pub fn passthrough(
    endpoints: &Endpoints,
    options: &ConnectOptions,
) -> Result<Arc<dyn Transport>, ForgeError> {
    match endpoints.kind {
        ProviderKind::GitHub => {
            let gh = options.gh.clone().ok_or(ForgeError::CliMissing(Cli::Gh))?;
            // A configured path that is not there (`DUSTY_FORGE_GH`) is a missing gh, so the
            // host is read anonymously instead of failing on the first request.
            if !gh.is_file() {
                return Err(ForgeError::CliMissing(Cli::Gh));
            }
            Ok(Arc::new(crate::cli::CliTransport::new(Cli::Gh, gh, None)))
        }
        ProviderKind::GitLab => {
            let glab = options
                .glab
                .clone()
                .ok_or(ForgeError::CliMissing(Cli::Glab))?;
            crate::cli::check_version(Cli::Glab, &glab, &AtomicBool::new(false))?;
            Ok(Arc::new(crate::cli::CliTransport::new(
                Cli::Glab,
                glab,
                None,
            )))
        }
        ProviderKind::Gitea | ProviderKind::Forgejo => {
            let tea = options
                .tea
                .clone()
                .ok_or(ForgeError::CliMissing(Cli::Tea))?;
            // tea passthrough needs a login for the host; without one tea is signed out. A
            // configured `login` wins over the tea login whose URL matches the host.
            let login = options
                .logins
                .get(&endpoints.host)
                .cloned()
                .or_else(|| {
                    options
                        .stores
                        .tea_config
                        .as_deref()
                        .and_then(|path| credential::tea_login(path, &endpoints.host))
                        .map(|login| login.name)
                })
                .ok_or_else(|| ForgeError::NotAuthenticated {
                    host: endpoints.host.to_string(),
                    hint: credential::login_hint(endpoints.kind, &endpoints.host),
                })?;
            crate::cli::check_version(Cli::Tea, &tea, &AtomicBool::new(false))?;
            Ok(Arc::new(crate::cli::CliTransport::new(
                Cli::Tea,
                tea,
                Some(login),
            )))
        }
    }
}

/// A 401 on the hot path: forget the credential and the chosen transport for this host.
pub fn evict(endpoints: &Endpoints, options: &ConnectOptions) {
    credential::evict(endpoints, options);
    selected()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|(key, _), _| key != endpoints);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_only_run_sends_gets_and_refuses_everything_else_unsent() {
        use std::sync::atomic::AtomicUsize;
        struct Counting(AtomicUsize);
        impl Transport for Counting {
            fn kind(&self) -> TransportKind {
                TransportKind::Anonymous
            }
            fn send(
                &self,
                _: &HostKey,
                _: &ApiRequest,
                _: &AtomicBool,
            ) -> Result<ApiResponse, TransportError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ApiResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: b"{}".to_vec(),
                })
            }
            fn graphql_batching(&self) -> bool {
                true
            }
        }
        let inner = Arc::new(Counting(AtomicUsize::new(0)));
        let guard = ReadOnly(inner.clone());
        let host = HostKey::new("github.com");
        let cancel = AtomicBool::new(false);
        let writes = [
            ApiRequest::new(Method::Post, "/repos/o/r/issues"),
            ApiRequest::new(Method::Patch, "/notifications/threads/1"),
            ApiRequest::new(Method::Put, "/repos/o/r/pulls/1/merge"),
            ApiRequest::new(Method::Delete, "/repos/o/r/issues/comments/1"),
            // A GraphQL POST passes only when it cannot be a write.
            ApiRequest::graphql(
                "mutation Close { closeIssue(input: {}) { clientMutationId } }",
                serde_json::json!({}),
            ),
            ApiRequest::graphql(
                "{ closeIssue(input: {}) { x } } mutation M { y }",
                serde_json::json!({}),
            ),
            ApiRequest::graphql("subscription S { events }", serde_json::json!({})),
        ];
        for request in &writes {
            let error = guard.send(&host, request, &cancel).unwrap_err();
            assert!(matches!(error, TransportError::ReadOnly(_)), "{error:?}");
            let shown = ForgeError::from(error);
            assert!(format!("{shown:?}").contains("read-only run"), "{shown:?}");
        }
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            0,
            "a refused request reached the transport"
        );
        assert!(!guard.graphql_batching());
        guard
            .send(&host, &ApiRequest::get("/repos/o/r/pulls"), &cancel)
            .unwrap();
        assert_eq!(inner.0.load(Ordering::SeqCst), 1);
        guard
            .send(
                &host,
                &ApiRequest::graphql("query Viewer { viewer { login } }", serde_json::json!({})),
                &cancel,
            )
            .unwrap();
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            2,
            "a GraphQL query is a read"
        );
    }

    #[test]
    fn a_read_only_run_stops_reading_a_host_at_its_cap() {
        // A host of its own, so other tests' requests don't count toward it.
        let host = HostKey::new("cap.example");
        let other = HostKey::new("other.example");
        assert!(read_only_admit(&host, Some(2)));
        assert!(read_only_admit(&host, Some(2)));
        assert!(!read_only_admit(&host, Some(2)), "the third read went out");
        assert!(!read_only_admit(&host, Some(2)));
        // Each host has its own budget, and no cap is no limit.
        assert!(read_only_admit(&other, Some(2)));
        assert!(read_only_admit(&host, None));
    }

    #[test]
    fn hosts_keep_ports_and_drop_case_and_trailing_dots() {
        assert_eq!(HostKey::new("GitHub.COM.").to_string(), "github.com");
        let key = HostKey::new("git.corp.example:8443");
        assert_eq!(
            (key.host.as_str(), key.port),
            ("git.corp.example", Some(8443))
        );
    }

    #[test]
    fn github_endpoints_follow_the_host_shape() {
        let endpoints = Endpoints::github(&HostKey::new("example.ghe.com"));
        assert_eq!(endpoints.api_base, "https://api.example.ghe.com");
        let mut endpoints = Endpoints::github(&HostKey::new("git.corp.example"));
        assert_eq!(endpoints.api_base, "https://git.corp.example/api/v3");
        assert_eq!(
            endpoints.graphql.as_deref(),
            Some("https://git.corp.example/api/graphql")
        );
        endpoints.override_api_base("http://127.0.0.1:9/");
        assert_eq!(
            endpoints.graphql.as_deref(),
            Some("http://127.0.0.1:9/graphql")
        );
    }

    #[test]
    fn only_loopback_may_skip_tls() {
        assert!(Endpoints::check_scheme("https://api.github.com").is_ok());
        assert!(Endpoints::check_scheme("http://127.0.0.1:4000/x").is_ok());
        assert!(Endpoints::check_scheme("http://api.github.com").is_err());
        assert!(Endpoints::check_scheme("file:///etc/passwd").is_err());
    }

    #[test]
    fn descriptors_name_graphql_operations_and_sorted_paths() {
        let request = ApiRequest::graphql(
            "query PullRequestList($q: String!) { search }",
            serde_json::json!({}),
        );
        assert_eq!(request.describe(), "POST graphql PullRequestList");
        let request = ApiRequest::get("/repos/o/r/issues")
            .query("state", "open")
            .query("q", "a b");
        assert_eq!(
            request.path_and_query(),
            "repos/o/r/issues?state=open&q=a%20b"
        );
    }

    #[test]
    fn next_links_are_parsed() {
        let response = ApiResponse {
            status: 200,
            headers: vec![(
                "link".into(),
                "<https://x/a?page=2>; rel=\"next\", <https://x/a?page=9>; rel=\"last\"".into(),
            )],
            body: Vec::new(),
        };
        assert_eq!(response.next_link().as_deref(), Some("https://x/a?page=2"));
    }
}
