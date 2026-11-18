//! Tokens. Dusty never asks for one: it reads the forge CLI's sign-in or the
//! documented environment variables, first hit wins:
//!
//! | provider | env | CLI store | CLI command |
//! |---|---|---|---|
//! | GitHub | `DUSTY_FORGE_TOKEN`, `GH_TOKEN`/`GITHUB_TOKEN` (github.com, `*.ghe.com`), `GH_ENTERPRISE_TOKEN`/`GITHUB_ENTERPRISE_TOKEN` (others) | — | `gh auth token --hostname H` |
//! | GitLab | `DUSTY_FORGE_TOKEN`, `GITLAB_TOKEN` | glab `config.yml` `hosts.H.token` | `glab auth status --hostname H --show-token` |
//! | Gitea/Forgejo | `DUSTY_FORGE_TOKEN`, `GITEA_SERVER_TOKEN` (with `GITEA_SERVER_URL`), `FORGEJO_TOKEN`, `GITEA_TOKEN` | fj `keys.json`, tea `config.yml` | — |
//!
//! Scoped variables only apply to their host: `DUSTY_FORGE_TOKEN` to `DUSTY_FORGE_HOST` (any
//! host when unset), `GITLAB_TOKEN` to `GITLAB_HOST`/`GL_HOST`, the Gitea ones to
//! `GITEA_SERVER_URL`. A success is cached 5 min, a miss 10 s, a crash never. Tokens never
//! enter argv, logs, notices, the cache or `hosts.json`; `Secret` prints `<redacted>` and every
//! token read is registered with `cli::redact`, which strips it from any text verbatim.

use crate::cli;
use crate::provider::ProviderKind;
use crate::request::ForgeError;
use crate::transport::ConnectOptions;
use crate::transport::Endpoints;
use crate::transport::HostKey;
use crate::yaml_lite;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(token: impl Into<String>) -> Self {
        let secret = Self(token.into());
        cli::remember_secret(&secret.0);
        secret
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub enum CredentialSource {
    Env(&'static str),
    GhAuthToken,
    GlabConfig,
    GlabAuthStatus,
    FjKeys,
    /// A tea login, by name (`tea login "cb"`); passthrough uses the same name.
    TeaConfig(String),
}

impl std::fmt::Display for CredentialSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialSource::Env(name) => write!(formatter, "${name}"),
            CredentialSource::GhAuthToken => formatter.write_str("gh token"),
            CredentialSource::GlabConfig => formatter.write_str("glab config"),
            CredentialSource::GlabAuthStatus => formatter.write_str("glab auth status"),
            CredentialSource::FjKeys => formatter.write_str("fj keys"),
            CredentialSource::TeaConfig(login) => write!(formatter, "tea login \"{login}\""),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Credential {
    pub token: Secret,
    pub source: CredentialSource,
    /// The account, when the store names it or after the live `GET /user` check.
    pub user: Option<String>,
    pub obtained: Instant,
}

impl Credential {
    pub fn new(token: impl Into<String>, source: CredentialSource, user: Option<String>) -> Self {
        Self {
            token: Secret::new(token),
            source,
            user,
            obtained: Instant::now(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum CredentialState {
    Present(Credential),
    Missing {
        tried: Vec<CredentialSource>,
        hint: String,
    },
    /// The lookup itself failed (timeout, unreadable output); retried on the next request.
    Crashed(String),
}

pub const PRESENT_TTL: Duration = Duration::from_secs(5 * 60);
pub const MISSING_TTL: Duration = Duration::from_secs(10);
const AUTH_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const STORE_CAP: u64 = 1024 * 1024;

/// The environment credential lookup reads: the process's, or an explicit map (tests).
/// Debug prints names only.
#[derive(Clone, Default)]
pub struct Env {
    vars: Option<BTreeMap<String, String>>,
}

impl std::fmt::Debug for Env {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.vars {
            None => formatter.write_str("Env(process)"),
            Some(vars) => formatter.debug_list().entries(vars.keys()).finish(),
        }
    }
}

impl Env {
    pub fn process() -> Self {
        Self { vars: None }
    }

    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Self {
            vars: Some(
                pairs
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            ),
        }
    }

    /// A set, non-blank variable, trimmed.
    pub fn get(&self, name: &str) -> Option<String> {
        let value = match &self.vars {
            None => crate::sandbox::var(name),
            Some(vars) => vars.get(name).cloned(),
        }?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.get(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    fn home(&self) -> Option<PathBuf> {
        self.path("HOME")
    }
}

/// Where the CLIs keep their sign-ins. `from_env` follows each tool's own lookup.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Stores {
    pub glab_config: Option<PathBuf>,
    pub fj_keys: Vec<PathBuf>,
    pub tea_config: Option<PathBuf>,
}

impl Stores {
    pub fn from_env(env: &Env) -> Self {
        let config_home = env
            .path("XDG_CONFIG_HOME")
            .or_else(|| env.home().map(|home| home.join(".config")));
        let data_home = env
            .path("XDG_DATA_HOME")
            .or_else(|| env.home().map(|home| home.join(".local/share")));
        let glab_config = env
            .path("GLAB_CONFIG_DIR")
            .or_else(|| config_home.as_ref().map(|home| home.join("glab-cli")))
            .map(|directory| directory.join("config.yml"));
        let mut fj_keys: Vec<PathBuf> = data_home
            .iter()
            .map(|home| home.join("forgejo-cli/keys.json"))
            .collect();
        let mut tea_config = config_home.as_ref().map(|home| home.join("tea/config.yml"));
        if cfg!(target_os = "macos")
            && let Some(home) = env.home()
        {
            let support = home.join("Library/Application Support");
            for organisation in ["forgejo-cli", "Cyborus"] {
                fj_keys.push(support.join(format!("{organisation}.forgejo-cli/keys.json")));
            }
            if !tea_config.as_ref().is_some_and(|path| path.is_file()) {
                tea_config = Some(support.join("tea/config.yml"));
            }
        }
        Self {
            glab_config,
            fj_keys,
            tea_config,
        }
    }
}

/// The cache of resolutions, with the clock passed in so tests can age it.
#[derive(Default)]
pub struct CredentialCache {
    entries: Mutex<HashMap<CacheKey, (CredentialState, Instant)>>,
}

type CacheKey = (HostKey, ProviderKind, ConnectOptions);

impl CredentialCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, (CredentialState, Instant)>> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn get(
        &self,
        endpoints: &Endpoints,
        options: &ConnectOptions,
        now: Instant,
    ) -> Option<CredentialState> {
        let key = (endpoints.host.clone(), endpoints.kind, options.clone());
        let entries = self.entries();
        let (state, at) = entries.get(&key)?;
        let ttl = match state {
            CredentialState::Present(_) => PRESENT_TTL,
            _ => MISSING_TTL,
        };
        (now.saturating_duration_since(*at) < ttl).then(|| state.clone())
    }

    /// Store a resolution; a crash is never cached.
    pub fn put(
        &self,
        endpoints: &Endpoints,
        options: &ConnectOptions,
        state: &CredentialState,
        now: Instant,
    ) {
        if matches!(state, CredentialState::Crashed(_)) {
            return;
        }
        self.entries().insert(
            (endpoints.host.clone(), endpoints.kind, options.clone()),
            (state.clone(), now),
        );
    }

    pub fn evict(&self, endpoints: &Endpoints, options: &ConnectOptions) {
        self.entries()
            .remove(&(endpoints.host.clone(), endpoints.kind, options.clone()));
    }

    pub fn clear(&self) {
        self.entries().clear();
    }
}

fn cache() -> &'static CredentialCache {
    static CACHE: OnceLock<CredentialCache> = OnceLock::new();
    CACHE.get_or_init(CredentialCache::new)
}

/// A host's token, from the process environment and the CLI stores, cached.
pub fn resolve(
    endpoints: &Endpoints,
    options: &ConnectOptions,
    cancel: &AtomicBool,
) -> CredentialState {
    if let Some(state) = cache().get(endpoints, options, Instant::now()) {
        return state;
    }
    let state = lookup(endpoints, options, &Env::process(), cancel);
    cache().put(endpoints, options, &state, Instant::now());
    state
}

/// The forge refused the token (401): forget it, and remember that one was refused so the
/// screens can say "expired or revoked" instead of "not signed in".
pub fn evict(endpoints: &Endpoints, options: &ConnectOptions) {
    cache().evict(endpoints, options);
    refused_hosts()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(endpoints.host.to_string());
}

fn refused_hosts() -> &'static Mutex<std::collections::BTreeSet<String>> {
    static REFUSED: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
    REFUSED.get_or_init(Default::default)
}

/// A token for `host` was refused this session and none has been accepted since.
pub fn refused(host: &str) -> bool {
    refused_hosts()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(host)
}

/// A token for `host` was accepted (`GET /user` answered).
pub fn accepted(host: &HostKey) {
    refused_hosts()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&host.to_string());
}

/// `r` in the accounts popover: forget every cached credential.
pub fn clear_cache() {
    cache().clear();
    gh_tokens()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

/// The command that signs a host in, shown verbatim in notices and the accounts popover.
pub fn login_hint(kind: ProviderKind, host: &HostKey) -> String {
    match kind {
        ProviderKind::GitHub => format!("gh auth login --hostname {host}"),
        ProviderKind::GitLab => format!("glab auth login --hostname {host}"),
        ProviderKind::Gitea | ProviderKind::Forgejo => {
            format!("tea login add --url https://{host}")
        }
    }
}

/// The one sign-in to offer while a host is browsed anonymously: the CLI's login when it is
/// installed, else its token variable (never `gh auth login` to someone without gh).
pub fn anonymous_hint(kind: ProviderKind, host: &HostKey) -> String {
    let cli = match kind {
        ProviderKind::GitHub => crate::transport::Cli::Gh,
        ProviderKind::GitLab => crate::transport::Cli::Glab,
        ProviderKind::Gitea | ProviderKind::Forgejo => crate::transport::Cli::Tea,
    };
    let installed = crate::sandbox::active() || cli.executable().is_some_and(|path| path.is_file());
    let mut hints = sign_in_hints(kind, host);
    if installed || hints.len() < 2 {
        hints.swap_remove(0)
    } else {
        hints.pop().unwrap_or_default()
    }
}

/// Every way to sign in to a host, the CLI's own login first: the copy for an instance that
/// refuses anonymous API calls, and for the accounts popover.
pub fn sign_in_hints(kind: ProviderKind, host: &HostKey) -> Vec<String> {
    let mut hints = vec![login_hint(kind, host)];
    match kind {
        ProviderKind::GitHub => {
            hints.push(if host.host == "github.com" {
                "GH_TOKEN=<token>".into()
            } else {
                "GH_ENTERPRISE_TOKEN=<token>".into()
            });
        }
        ProviderKind::GitLab => hints.push("GITLAB_TOKEN=<token>".into()),
        ProviderKind::Gitea | ProviderKind::Forgejo => {
            hints.push(format!("fj -H {host} auth login"));
            hints.push(format!("FORGEJO_TOKEN=<token> FORGEJO_URL=https://{host}"));
        }
    }
    hints
}

/// One read or write that needs a credential while a host is browsed anonymously: `what` is
/// the action (`read logs`, `comment`), the copy names the CLI login that fixes it.
pub fn sign_in_to(what: &str, kind: ProviderKind, host: &HostKey) -> ForgeError {
    ForgeError::Unsupported {
        capability: "sign_in",
        reason: {
            let hint = anonymous_hint(kind, host);
            let verb = if hint.contains('=') { "set" } else { "run" };
            format!("Sign in to {what} · {verb} {hint}")
        },
    }
}

/// The uncached lookup, in precedence order.
pub fn lookup(
    endpoints: &Endpoints,
    options: &ConnectOptions,
    env: &Env,
    cancel: &AtomicBool,
) -> CredentialState {
    let host = &endpoints.host;
    let mut tried = Vec::new();
    let hint = login_hint(endpoints.kind, host);
    if let Some(token) = dusty_token(env, host) {
        return CredentialState::Present(Credential::new(
            token,
            CredentialSource::Env("DUSTY_FORGE_TOKEN"),
            None,
        ));
    }
    tried.push(CredentialSource::Env("DUSTY_FORGE_TOKEN"));
    let found = match endpoints.kind {
        ProviderKind::GitHub => github(host, options, env, &mut tried, cancel),
        ProviderKind::GitLab => gitlab(host, options, env, &mut tried, cancel),
        ProviderKind::Gitea | ProviderKind::Forgejo => gitea(host, options, env, &mut tried),
    };
    match found {
        Ok(Some(credential)) => CredentialState::Present(credential),
        Ok(None) => CredentialState::Missing { tried, hint },
        Err(crash) => CredentialState::Crashed(cli::redact(&crash)),
    }
}

fn dusty_token(env: &Env, host: &HostKey) -> Option<String> {
    let scoped = env.get("DUSTY_FORGE_HOST");
    if scoped.is_some_and(|scope| HostKey::new(&scope) != *host) {
        return None;
    }
    env.get("DUSTY_FORGE_TOKEN")
}

/// A variable scoped to the host named by another variable (unset scope = any host).
fn scoped_env(
    env: &Env,
    name: &'static str,
    scopes: &[&str],
    host: &HostKey,
    scope_required: bool,
) -> Option<String> {
    let scope = scopes.iter().find_map(|scope| env.get(scope));
    let matches = match scope {
        Some(scope) => url_host(&scope).is_some_and(|scope| scope == *host),
        None => !scope_required,
    };
    matches.then(|| env.get(name)).flatten()
}

/// `https://h:1/x` / `h:1` / `h` → the host key.
pub fn url_host(text: &str) -> Option<HostKey> {
    let text = text.trim();
    if text.contains("://") {
        let url = reqwest::Url::parse(text).ok()?;
        let host = url.host_str()?;
        return Some(match url.port() {
            Some(port) => HostKey::new(&format!("{host}:{port}")),
            None => HostKey::new(host),
        });
    }
    let host = text.split('/').next()?.trim();
    (!host.is_empty()).then(|| HostKey::new(host))
}

/// The env variables gh itself honours for this host.
pub fn github_env_names(host: &HostKey) -> &'static [&'static str] {
    if host.host == "github.com" || host.host.ends_with(".ghe.com") {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    }
}

fn github(
    host: &HostKey,
    options: &ConnectOptions,
    env: &Env,
    tried: &mut Vec<CredentialSource>,
    cancel: &AtomicBool,
) -> Result<Option<Credential>, String> {
    for name in github_env_names(host) {
        tried.push(CredentialSource::Env(name));
        if let Some(token) = env.get(name) {
            return Ok(Some(Credential::new(
                token,
                CredentialSource::Env(name),
                None,
            )));
        }
    }
    let Some(gh) = &options.gh else {
        return Ok(None);
    };
    tried.push(CredentialSource::GhAuthToken);
    Ok(gh_auth_token(gh, host, cancel)?
        .map(|token| Credential::new(token.expose(), CredentialSource::GhAuthToken, None)))
}

type GhTokens = HashMap<(PathBuf, HostKey), (Option<Secret>, Instant)>;

fn gh_tokens() -> &'static Mutex<GhTokens> {
    static TOKENS: OnceLock<Mutex<GhTokens>> = OnceLock::new();
    TOKENS.get_or_init(Default::default)
}

/// `gh auth token --hostname H`: `Ok(None)` when gh has no sign-in for H. Shared by credential
/// lookup and detection (a host gh holds a token for is GitHub), so gh runs once per TTL.
pub fn gh_auth_token(
    gh: &Path,
    host: &HostKey,
    cancel: &AtomicBool,
) -> Result<Option<Secret>, String> {
    let key = (gh.to_path_buf(), host.clone());
    if let Some((token, at)) = gh_tokens()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
    {
        let ttl = if token.is_some() {
            PRESENT_TTL
        } else {
            MISSING_TTL
        };
        if at.elapsed() < ttl {
            return Ok(token.clone());
        }
    }
    let output = run(
        gh,
        &["auth", "token", "--hostname", &host.to_string()],
        cancel,
    )?;
    let token = match output {
        None => None,
        Some((0, stdout, _)) => {
            let token = stdout.trim();
            if token.is_empty() || token.contains(char::is_whitespace) {
                return Err("gh auth token printed no token".into());
            }
            Some(Secret::new(token))
        }
        Some(_) => None,
    };
    gh_tokens()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key, (token.clone(), Instant::now()));
    Ok(token)
}

/// Run a CLI's auth command: `Ok(None)` when it is not installed, `Err` when it crashed.
fn run(
    program: &Path,
    args: &[&str],
    cancel: &AtomicBool,
) -> Result<Option<(i32, String, String)>, String> {
    let spec = cli::ProcessSpec {
        program: program.to_path_buf(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        cwd: std::env::temp_dir(),
        env: cli::child_env(),
        stdin: None,
        timeout: AUTH_COMMAND_TIMEOUT,
        cap: 256 * 1024,
    };
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match cli::run_process(&spec, cancel) {
        Ok(output) => Ok(Some((
            output.code.unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))),
        Err(crate::transport::TransportError::Spawn(_)) => Ok(None),
        Err(error) => Err(format!("{name} {}: {error:?}", args.join(" "))),
    }
}

fn read_store(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(STORE_CAP).read_to_string(&mut text).ok()?;
    Some(text)
}

/// glab's `config.yml`: `hosts.<H>.token` (matching the key or its `api_host`).
pub fn glab_config(path: &Path, host: &HostKey) -> Option<(String, Option<String>)> {
    let value = yaml_lite::parse(&read_store(path)?).ok()?;
    value
        .get("hosts")?
        .entries()
        .iter()
        .find_map(|(name, entry)| {
            let matches = HostKey::new(name) == *host
                || entry
                    .text("api_host")
                    .and_then(url_host)
                    .is_some_and(|api| api == *host);
            let token = entry.text("token")?;
            matches.then(|| (token.to_owned(), entry.text("user").map(str::to_owned)))
        })
}

/// The hosts glab's config holds a token for (detection claims).
pub fn glab_hosts(path: &Path) -> Vec<HostKey> {
    let Some(value) = read_store(path).and_then(|text| yaml_lite::parse(&text).ok()) else {
        return Vec::new();
    };
    value
        .get("hosts")
        .map(|hosts| {
            hosts
                .entries()
                .iter()
                .filter(|(_, entry)| entry.text("token").is_some())
                .map(|(name, _)| HostKey::new(name))
                .collect()
        })
        .unwrap_or_default()
}

/// `glab auth status --hostname H --show-token` → (`Token: …`, `Logged in to H as U`).
pub fn parse_glab_status(text: &str) -> (Option<String>, Option<String>) {
    let mut token = None;
    let mut user = None;
    for line in text.lines() {
        let line = line
            .trim()
            .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
            .trim();
        if let Some(rest) = line.strip_prefix("Token:") {
            let value = rest.trim();
            if !value.is_empty() && !value.chars().all(|c| c == '*') && !value.contains(' ') {
                token = Some(value.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("Logged in to ")
            && let Some((_, account)) = rest.split_once(" as ")
        {
            user = account
                .split_whitespace()
                .next()
                .map(|account| account.trim_end_matches(',').to_owned());
        }
    }
    (token, user)
}

fn gitlab(
    host: &HostKey,
    options: &ConnectOptions,
    env: &Env,
    tried: &mut Vec<CredentialSource>,
    cancel: &AtomicBool,
) -> Result<Option<Credential>, String> {
    tried.push(CredentialSource::Env("GITLAB_TOKEN"));
    if let Some(token) = scoped_env(
        env,
        "GITLAB_TOKEN",
        &["GITLAB_HOST", "GL_HOST"],
        host,
        false,
    ) {
        return Ok(Some(Credential::new(
            token,
            CredentialSource::Env("GITLAB_TOKEN"),
            None,
        )));
    }
    if let Some(path) = &options.stores.glab_config {
        tried.push(CredentialSource::GlabConfig);
        if let Some((token, user)) = glab_config(path, host) {
            return Ok(Some(Credential::new(
                token,
                CredentialSource::GlabConfig,
                user,
            )));
        }
    }
    let Some(glab) = &options.glab else {
        return Ok(None);
    };
    tried.push(CredentialSource::GlabAuthStatus);
    let host = host.to_string();
    match run(
        glab,
        &["auth", "status", "--hostname", &host, "--show-token"],
        cancel,
    )? {
        Some((code, stdout, stderr)) => {
            let (token, user) = parse_glab_status(&format!("{stdout}\n{stderr}"));
            Ok(token
                .filter(|_| code == 0)
                .map(|token| Credential::new(token, CredentialSource::GlabAuthStatus, user)))
        }
        None => Ok(None),
    }
}

/// fj's `keys.json`: `{"hosts": {"codeberg.org": {"type": "Application", "name": "maya", "token": "…"}}}`.
pub fn fj_keys(path: &Path, host: &HostKey) -> Option<(String, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(&read_store(path)?).ok()?;
    value["hosts"]
        .as_object()?
        .iter()
        .find_map(|(name, entry)| {
            let token = entry["token"]
                .as_str()
                .map(str::trim)
                .filter(|token| !token.is_empty())?;
            (url_host(name)? == *host)
                .then(|| (token.to_owned(), entry["name"].as_str().map(str::to_owned)))
        })
}

pub fn fj_hosts(path: &Path) -> Vec<HostKey> {
    let Some(value) =
        read_store(path).and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return Vec::new();
    };
    value["hosts"]
        .as_object()
        .map(|hosts| {
            hosts
                .iter()
                .filter(|(_, entry)| {
                    entry["token"]
                        .as_str()
                        .is_some_and(|token| !token.trim().is_empty())
                })
                .filter_map(|(name, _)| url_host(name))
                .collect()
        })
        .unwrap_or_default()
}

/// One tea login: `logins[]` entry with `name`, `url`, `token`, `user`, `default`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeaLogin {
    pub name: String,
    pub host: HostKey,
    pub user: Option<String>,
    pub default: bool,
    pub has_token: bool,
}

fn tea_entries(path: &Path) -> Option<yaml_lite::Yaml> {
    yaml_lite::parse(&read_store(path)?).ok()
}

pub fn tea_logins(path: &Path) -> Vec<TeaLogin> {
    let Some(value) = tea_entries(path) else {
        return Vec::new();
    };
    value
        .get("logins")
        .map(|logins| {
            logins
                .as_list()
                .iter()
                .filter_map(|login| {
                    Some(TeaLogin {
                        name: login.text("name")?.to_owned(),
                        host: url_host(login.text("url")?)?,
                        user: login.text("user").map(str::to_owned),
                        default: login.text("default") == Some("true"),
                        has_token: login.text("token").is_some(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tea login for a host: the `default` one when several match, else the first.
pub fn tea_login(path: &Path, host: &HostKey) -> Option<TeaLogin> {
    let logins: Vec<TeaLogin> = tea_logins(path)
        .into_iter()
        .filter(|login| login.host == *host)
        .collect();
    logins
        .iter()
        .find(|login| login.default)
        .or_else(|| logins.first())
        .cloned()
}

fn tea_token(path: &Path, host: &HostKey) -> Option<(String, String, Option<String>)> {
    let login = tea_login(path, host)?;
    let value = tea_entries(path)?;
    let token = value
        .get("logins")?
        .as_list()
        .iter()
        .find(|entry| entry.text("name") == Some(login.name.as_str()))?
        .text("token")?
        .to_owned();
    Some((token, login.name, login.user))
}

fn gitea(
    host: &HostKey,
    options: &ConnectOptions,
    env: &Env,
    tried: &mut Vec<CredentialSource>,
) -> Result<Option<Credential>, String> {
    tried.push(CredentialSource::Env("GITEA_SERVER_TOKEN"));
    if let Some(token) = scoped_env(env, "GITEA_SERVER_TOKEN", &["GITEA_SERVER_URL"], host, true) {
        return Ok(Some(Credential::new(
            token,
            CredentialSource::Env("GITEA_SERVER_TOKEN"),
            None,
        )));
    }
    for name in ["FORGEJO_TOKEN", "GITEA_TOKEN"] {
        tried.push(CredentialSource::Env(name));
        if let Some(token) =
            scoped_env(env, name, &["GITEA_SERVER_URL", "FORGEJO_URL"], host, false)
        {
            return Ok(Some(Credential::new(
                token,
                CredentialSource::Env(name),
                None,
            )));
        }
    }
    for path in &options.stores.fj_keys {
        tried.push(CredentialSource::FjKeys);
        if let Some((token, user)) = fj_keys(path, host) {
            return Ok(Some(Credential::new(token, CredentialSource::FjKeys, user)));
        }
    }
    if let Some(path) = &options.stores.tea_config {
        if let Some((token, name, user)) = tea_token(path, host) {
            return Ok(Some(Credential::new(
                token,
                CredentialSource::TeaConfig(name),
                user,
            )));
        }
        tried.push(CredentialSource::TeaConfig(String::new()));
    }
    Ok(None)
}

/// Which provider a CLI's stored credential says an unknown host is. Files first (cheap), then
/// `gh auth token`; host-scoped env variables count as the CLI's own.
pub fn claim(
    host: &HostKey,
    options: &ConnectOptions,
    env: &Env,
    cancel: &AtomicBool,
) -> Option<(ProviderKind, CredentialSource)> {
    if env
        .get("GH_HOST")
        .is_some_and(|scope| HostKey::new(&scope) == *host)
        && let Some(name) = ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN", "GH_TOKEN"]
            .into_iter()
            .find(|name| env.get(name).is_some())
    {
        return Some((ProviderKind::GitHub, CredentialSource::Env(name)));
    }
    for scope in ["GITLAB_HOST", "GL_HOST"] {
        if env
            .get(scope)
            .and_then(|scope| url_host(&scope))
            .is_some_and(|scope| scope == *host)
            && env.get("GITLAB_TOKEN").is_some()
        {
            return Some((ProviderKind::GitLab, CredentialSource::Env("GITLAB_TOKEN")));
        }
    }
    if env
        .get("GITEA_SERVER_URL")
        .and_then(|scope| url_host(&scope))
        .is_some_and(|scope| scope == *host)
        && env.get("GITEA_SERVER_TOKEN").is_some()
    {
        return Some((
            ProviderKind::Gitea,
            CredentialSource::Env("GITEA_SERVER_TOKEN"),
        ));
    }
    if let Some(path) = &options.stores.glab_config
        && glab_hosts(path).contains(host)
    {
        return Some((ProviderKind::GitLab, CredentialSource::GlabConfig));
    }
    if options
        .stores
        .fj_keys
        .iter()
        .any(|path| fj_hosts(path).contains(host))
    {
        return Some((ProviderKind::Forgejo, CredentialSource::FjKeys));
    }
    if let Some(path) = &options.stores.tea_config
        && let Some(login) = tea_login(path, host)
    {
        return Some((ProviderKind::Gitea, CredentialSource::TeaConfig(login.name)));
    }
    if let Some(gh) = &options.gh
        && matches!(gh_auth_token(gh, host, cancel), Ok(Some(_)))
    {
        return Some((ProviderKind::GitHub, CredentialSource::GhAuthToken));
    }
    if let Some(glab) = &options.glab
        && let Ok(Some((0, stdout, stderr))) = run(
            glab,
            &["auth", "status", "--hostname", &host.to_string()],
            cancel,
        )
        && parse_glab_status(&format!("{stdout}\n{stderr}"))
            .1
            .is_some()
    {
        return Some((ProviderKind::GitLab, CredentialSource::GlabAuthStatus));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print() {
        let secret = Secret::new("ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(format!("{secret:?} {secret}"), "<redacted> <redacted>");
        let credential = Credential::new(secret.expose(), CredentialSource::GhAuthToken, None);
        assert!(!format!("{credential:?}").contains("ghp_"));
        let env = Env::from_pairs([("GITLAB_TOKEN", "glpat-should-not-print")]);
        assert!(!format!("{env:?}").contains("should-not-print"));
    }

    #[test]
    fn enterprise_hosts_use_enterprise_tokens() {
        assert_eq!(
            github_env_names(&HostKey::new("github.com")),
            &["GH_TOKEN", "GITHUB_TOKEN"]
        );
        assert_eq!(
            github_env_names(&HostKey::new("git.corp.example")),
            &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
        );
    }

    #[test]
    fn glab_status_text_yields_token_and_account() {
        let text = "gitlab.com\n  ✓ Logged in to gitlab.com as maya (/home/m/.config/glab-cli/config.yml)\n  ✓ Git operations for gitlab.com configured to use ssh protocol.\n  ✓ Token: glpat-0123456789abcdefghij\n";
        assert_eq!(
            parse_glab_status(text),
            (
                Some("glpat-0123456789abcdefghij".into()),
                Some("maya".into())
            )
        );
        let masked = "  ✓ Logged in to gitlab.com as maya\n  ✓ Token: **************************\n";
        assert_eq!(parse_glab_status(masked).0, None);
    }

    #[test]
    fn url_hosts_keep_ports_and_drop_paths() {
        assert_eq!(
            url_host("https://Codeberg.org/"),
            Some(HostKey::new("codeberg.org"))
        );
        assert_eq!(
            url_host("https://git.corp:8443/gitlab"),
            Some(HostKey::new("git.corp:8443"))
        );
        assert_eq!(url_host("codeberg.org"), Some(HostKey::new("codeberg.org")));
    }
}
