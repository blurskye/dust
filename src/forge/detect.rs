//! Which remote, which host, which forge (FORGE.md §2.2–2.3).
//!
//! Provider order, first answer wins: `[forge.hosts."H"].provider` in config.toml →
//! `DUSTY_FORGE_PROVIDER` (for `DUSTY_FORGE_HOST`, or any host when that is unset) → well-known
//! hosts → `hosts.json` (results of earlier claims and probes, 7 days) → CLI credential stores
//! (a host gh, glab, fj or tea holds a credential for *is* that provider; t3code's rule that an
//! unknown host is only claimed with a credential) → an unauthenticated version probe
//! (`/api/v1/version` Gitea/Forgejo, `/api/v4/version` GitLab, `/api/v3/meta` GHES, 3 s each) →
//! `Unknown forge at H` (DESIGN §3.45 X-PROVIDER).
//!
//! Claims and probes are cached in `$XDG_STATE_HOME/dusty/forge/hosts.json`
//! (`DUSTY_FORGE_STATE_DIR` overrides the directory) together with the capabilities observed
//! for the host; the file never holds a token or a login.

use crate::forge::credential;
use crate::forge::credential::Env;
use crate::forge::provider::ProviderKind;
use crate::forge::provider::RepoRef;
use crate::forge::request::ForgeError;
use crate::forge::transport::Cli;
use crate::forge::transport::ConnectOptions;
use crate::forge::transport::Endpoints;
use crate::forge::transport::HostKey;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteRef {
    pub remote: String,
    pub host: HostKey,
    pub repo: RepoRef,
}

/// `https://host[:port]/o/r(.git)`, `ssh://git@host[:22]/o/r.git`, `git@host:o/r.git`.
/// A Git transport port is not the web/API port, so only http(s) ports are kept. GitLab keeps
/// every segment (`group/sub/repo`).
pub fn parse_remote(url: &str) -> Option<(HostKey, RepoRef)> {
    let base = crate::git::web_base(url)?;
    let host = match base.port() {
        Some(port) => format!("{}:{port}", base.host_str()?),
        None => base.host_str()?.to_owned(),
    };
    let repo = RepoRef::parse(base.path().trim_matches('/'))?;
    Some((HostKey::new(&host), repo))
}

/// `DUSTY_FORGE_REMOTE` → `[forge].remote` → gh's choice (`remote.<name>.gh-resolved = base`)
/// → `upstream`, `github`, `origin` → any other remote with a parseable URL.
pub fn choose_remote(root: &Path) -> Result<RemoteRef, ForgeError> {
    let preferred = Env::process()
        .get("DUSTY_FORGE_REMOTE")
        .or_else(|| ForgeConfig::load().remote);
    choose_remote_with(root, preferred.as_deref())
}

pub fn choose_remote_with(root: &Path, preferred: Option<&str>) -> Result<RemoteRef, ForgeError> {
    let repository = git2::Repository::discover(root)
        .map_err(|_| ForgeError::Failed("not a Git repository".into()))?;
    let config = repository.config().ok();
    let names = repository
        .remotes()
        .map_err(|error| ForgeError::Failed(error.message().to_owned()))?
        .iter()
        .flatten()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let resolved = |name: &str| {
        config
            .as_ref()
            .and_then(|config| {
                config
                    .get_string(&format!("remote.{name}.gh-resolved"))
                    .ok()
            })
            .is_some_and(|value| value == "base")
    };
    let rank = |name: &str| {
        if preferred == Some(name) {
            0
        } else if resolved(name) {
            1
        } else {
            match name {
                "upstream" => 2,
                "github" => 3,
                "origin" => 4,
                _ => 5,
            }
        }
    };
    let mut ordered = names.clone();
    ordered.sort_by_key(|name| (rank(name), name.clone()));
    for name in ordered {
        let Ok(remote) = repository.find_remote(&name) else {
            continue;
        };
        if let Some((host, repo)) = remote.url().and_then(parse_remote) {
            return Ok(RemoteRef {
                remote: name,
                host,
                repo,
            });
        }
    }
    Err(ForgeError::Failed(
        "no Git remote points at a forge · add one with git remote add origin <url>".into(),
    ))
}

pub fn parse_kind(text: &str) -> Option<ProviderKind> {
    match text.trim().to_ascii_lowercase().as_str() {
        "github" | "ghe" | "ghes" => Some(ProviderKind::GitHub),
        "gitlab" => Some(ProviderKind::GitLab),
        "gitea" => Some(ProviderKind::Gitea),
        "forgejo" | "codeberg" => Some(ProviderKind::Forgejo),
        _ => None,
    }
}

pub fn kind_word(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::GitHub => "github",
        ProviderKind::GitLab => "gitlab",
        ProviderKind::Gitea => "gitea",
        ProviderKind::Forgejo => "forgejo",
    }
}

/// The hosts everybody knows.
pub fn known_host(host: &HostKey) -> Option<ProviderKind> {
    if host.port.is_some() {
        return None;
    }
    match host.host.as_str() {
        "github.com" => Some(ProviderKind::GitHub),
        name if name.ends_with(".ghe.com") => Some(ProviderKind::GitHub),
        "gitlab.com" => Some(ProviderKind::GitLab),
        "codeberg.org" | "next.forgejo.org" => Some(ProviderKind::Forgejo),
        "gitea.com" => Some(ProviderKind::Gitea),
        _ => None,
    }
}

/// `[forge.hosts."H"]` (FORGE.md §2.4); routing only, never tokens.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostConfig {
    pub provider: Option<ProviderKind>,
    pub api_base: Option<String>,
    /// tea login / glab alias for passthrough.
    pub login: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ForgeConfig {
    pub remote: Option<String>,
    pub hosts: BTreeMap<HostKey, HostConfig>,
}

impl ForgeConfig {
    /// The `[forge]` table of the user's config.toml; anything unreadable is ignored.
    pub fn load() -> Self {
        std::fs::read_to_string(crate::config::user_config_path())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let Ok(value) = toml::from_str::<toml::Value>(text) else {
            return Self::default();
        };
        let Some(forge) = value.get("forge") else {
            return Self::default();
        };
        let text = |table: &toml::Value, key: &str| {
            table
                .get(key)
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let hosts = forge
            .get("hosts")
            .and_then(toml::Value::as_table)
            .map(|hosts| {
                hosts
                    .iter()
                    .map(|(name, table)| {
                        (
                            HostKey::new(name),
                            HostConfig {
                                provider: text(table, "provider").as_deref().and_then(parse_kind),
                                api_base: text(table, "api_base"),
                                login: text(table, "login"),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            remote: text(forge, "remote"),
            hosts,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DetectSource {
    Config,
    Env,
    KnownHost,
    /// `hosts.json`, from an earlier claim or probe.
    Cache,
    /// A CLI holds a credential for the host (`gh token`, `glab config`, `fj keys`, …).
    Credential(String),
    Probe,
}

impl std::fmt::Display for DetectSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetectSource::Config => formatter.write_str("config"),
            DetectSource::Env => formatter.write_str("$DUSTY_FORGE_PROVIDER"),
            DetectSource::KnownHost => formatter.write_str("known host"),
            DetectSource::Cache => formatter.write_str("hosts.json"),
            DetectSource::Credential(source) => formatter.write_str(source),
            DetectSource::Probe => formatter.write_str("version probe"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Detected {
    pub host: HostKey,
    pub kind: ProviderKind,
    pub endpoints: Endpoints,
    /// The CLI passthrough would use.
    pub cli: Option<Cli>,
    /// `[forge.hosts.H].login` or the claiming tea login (passthrough).
    pub login: Option<String>,
    pub server_version: Option<String>,
    pub source: DetectSource,
    /// The probes were refused because the instance requires a sign-in for every API call
    /// (Forgejo/Gitea `REQUIRE_SIGNIN_VIEW`, GHES private mode): `kind` is a guess from the
    /// refusal, and anonymous reads cannot work (FORGE.md §2.5).
    pub sign_in_required: bool,
}

/// Everything detection reads; `from_env` is the app's, tests build their own.
#[derive(Clone, Debug)]
pub struct DetectInputs {
    pub config: ForgeConfig,
    pub env: Env,
    pub options: ConnectOptions,
    /// `None` disables `hosts.json`.
    pub hosts_file: Option<PathBuf>,
    pub probe: bool,
}

impl DetectInputs {
    pub fn from_env(options: &ConnectOptions) -> Self {
        let env = Env::process();
        Self {
            config: ForgeConfig::load(),
            hosts_file: hosts_path(&env),
            probe: !options.offline,
            options: options.clone(),
            env,
        }
    }
}

/// `$DUSTY_FORGE_STATE_DIR/hosts.json`, else `$XDG_STATE_HOME/dusty/forge/hosts.json`.
pub fn hosts_path(env: &Env) -> Option<PathBuf> {
    if let Some(directory) = env.get("DUSTY_FORGE_STATE_DIR") {
        return Some(PathBuf::from(directory).join("hosts.json"));
    }
    let state = env
        .get("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env.get("HOME")
                .map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state.join("dusty/forge/hosts.json"))
}

/// Where probes and derived (non-GitHub) API bases go: `https://H`, or `DUSTY_FORGE_ORIGIN`
/// with `{host}` (tests point it at the loopback fake). Web links stay `https://H`.
fn origin(host: &HostKey, env: &Env) -> String {
    match env.get("DUSTY_FORGE_ORIGIN") {
        Some(origin) => origin
            .replace("{host}", &host.to_string())
            .trim_end_matches('/')
            .to_owned(),
        None => format!("https://{host}"),
    }
}

fn graphql_for(kind: ProviderKind, api_base: &str) -> Option<String> {
    match kind {
        ProviderKind::GitHub => Some(match api_base.strip_suffix("/api/v3") {
            Some(root) => format!("{root}/api/graphql"),
            None => format!("{api_base}/graphql"),
        }),
        ProviderKind::GitLab => Some(match api_base.strip_suffix("/api/v4") {
            Some(root) => format!("{root}/api/graphql"),
            None => format!("{api_base}/graphql"),
        }),
        ProviderKind::Gitea | ProviderKind::Forgejo => None,
    }
}

/// API bases per provider (§2.2), then `api_base` from config, then `DUSTY_FORGE_API_BASE` /
/// `DUSTY_FORGE_GRAPHQL_URL` (both with `{host}`).
pub fn endpoints_for(
    kind: ProviderKind,
    host: &HostKey,
    api_base: Option<&str>,
    env: &Env,
) -> Endpoints {
    let root = origin(host, env);
    let web_base = format!("https://{host}");
    let mut endpoints = match kind {
        ProviderKind::GitHub => Endpoints::github(host),
        ProviderKind::GitLab => Endpoints {
            kind,
            host: host.clone(),
            api_base: format!("{root}/api/v4"),
            graphql: Some(format!("{root}/api/graphql")),
            web_base,
        },
        ProviderKind::Gitea | ProviderKind::Forgejo => Endpoints {
            kind,
            host: host.clone(),
            api_base: format!("{root}/api/v1"),
            graphql: None,
            web_base,
        },
    };
    let name = host.to_string();
    // F1's `DUSTY_FORGE_API_BASE` names a GitHub REST base; other kinds use the origin.
    let base = env
        .get("DUSTY_FORGE_API_BASE")
        .filter(|_| kind == ProviderKind::GitHub)
        .map(|base| base.replace("{host}", &name))
        .or_else(|| api_base.map(str::to_owned));
    if let Some(base) = base {
        let base = base.trim_end_matches('/').to_owned();
        endpoints.graphql = graphql_for(kind, &base);
        endpoints.api_base = base;
    }
    if let Some(url) = env.get("DUSTY_FORGE_GRAPHQL_URL")
        && kind != ProviderKind::Gitea
        && kind != ProviderKind::Forgejo
    {
        endpoints.graphql = Some(url.replace("{host}", &name));
    }
    endpoints
}

// ------------------------------------------------------------------------------ hosts.json

pub const HOSTS_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostRecord {
    pub kind: String,
    pub api_base: String,
    #[serde(default)]
    pub graphql: Option<String>,
    pub web_base: String,
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(default)]
    pub source: String,
    /// Seconds since the epoch.
    pub probed_at: u64,
    /// Capability name → `yes` / `no` from a probe (`Cap::Probe`, FORGE.md §4.1).
    #[serde(default)]
    pub capabilities_observed: BTreeMap<String, String>,
    /// The version probe was refused for want of a sign-in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sign_in_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostsFile {
    pub schema: u32,
    #[serde(default)]
    pub hosts: BTreeMap<String, HostRecord>,
}

impl Default for HostsFile {
    fn default() -> Self {
        Self {
            schema: 1,
            hosts: BTreeMap::new(),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn hosts_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

impl HostsFile {
    /// A missing, unreadable or newer-schema file reads as empty.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .filter(|file| file.schema == 1)
            .unwrap_or_default()
    }

    /// Atomic: a temporary file in the same directory, then a rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        std::io::Write::write_all(
            &mut temporary,
            serde_json::to_string_pretty(self)?.as_bytes(),
        )?;
        temporary.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    /// A record younger than the TTL.
    pub fn fresh(&self, host: &HostKey, unix_now: u64) -> Option<&HostRecord> {
        self.hosts
            .get(&host.to_string())
            .filter(|record| unix_now.saturating_sub(record.probed_at) < HOSTS_TTL.as_secs())
    }

    /// Read-modify-write under the process lock (several workers may record at once).
    pub fn update(path: &Path, change: impl FnOnce(&mut HostsFile)) -> std::io::Result<()> {
        let _guard = hosts_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut file = Self::load(path);
        change(&mut file);
        file.save(path)
    }
}

fn record_detected(path: &Path, detected: &Detected) {
    let _ = HostsFile::update(path, |file| {
        let previous = file
            .hosts
            .remove(&detected.host.to_string())
            .unwrap_or_default();
        file.hosts.insert(
            detected.host.to_string(),
            HostRecord {
                kind: kind_word(detected.kind).into(),
                api_base: detected.endpoints.api_base.clone(),
                graphql: detected.endpoints.graphql.clone(),
                web_base: detected.endpoints.web_base.clone(),
                server_version: detected.server_version.clone(),
                source: match &detected.source {
                    DetectSource::Probe => "probe".into(),
                    _ => "credential".into(),
                },
                probed_at: unix_now(),
                capabilities_observed: previous.capabilities_observed,
                sign_in_required: detected.sign_in_required,
            },
        );
    });
}

/// A probe found out whether a capability exists on this host (`Cap::Probe` → `Yes`/`No`).
/// Persisted in `hosts.json` so the next start knows; F4/F6 call this on a probe's 404.
pub fn record_capability(host: &HostKey, endpoints: &Endpoints, capability: &str, supported: bool) {
    let Some(path) = hosts_path(&Env::process()) else {
        return;
    };
    let _ = HostsFile::update(&path, |file| {
        let record = file
            .hosts
            .entry(host.to_string())
            .or_insert_with(|| HostRecord {
                kind: kind_word(endpoints.kind).into(),
                api_base: endpoints.api_base.clone(),
                graphql: endpoints.graphql.clone(),
                web_base: endpoints.web_base.clone(),
                source: "capability".into(),
                probed_at: unix_now(),
                ..HostRecord::default()
            });
        record.capabilities_observed.insert(
            capability.into(),
            if supported { "yes" } else { "no" }.into(),
        );
    });
}

pub fn observed_capabilities(host: &HostKey) -> BTreeMap<String, String> {
    hosts_path(&Env::process())
        .map(|path| HostsFile::load(&path))
        .and_then(|file| file.hosts.get(&host.to_string()).cloned())
        .map(|record| record.capabilities_observed)
        .unwrap_or_default()
}

// ------------------------------------------------------------------------------ probes

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// One unauthenticated GET; `None` when the host did not answer over an allowed scheme.
fn probe_get(client: &reqwest::blocking::Client, url: &str) -> Option<(u16, serde_json::Value)> {
    Endpoints::check_scheme(url).ok()?;
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .ok()?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(response, 64 * 1024), &mut body).ok()?;
    Some((status, serde_json::from_slice(&body).unwrap_or_default()))
}

/// `11.0.1+gitea-1.22.0` is Forgejo; a plain `1.22.6` is Gitea.
pub fn gitea_family(version: &str) -> ProviderKind {
    if version.contains("+gitea") || version.to_ascii_lowercase().contains("forgejo") {
        ProviderKind::Forgejo
    } else {
        ProviderKind::Gitea
    }
}

/// Gitea's API errors carry the swagger URL; it proves the family even when anonymous reads
/// are refused (`REQUIRE_SIGNIN_VIEW`). Forgejo's sign-in refusal (forgejo.example.org's
/// 403) carries only its fixed message, which proves the family as well.
fn gitea_error(body: &serde_json::Value) -> bool {
    body["message"].is_string()
        && (body["url"]
            .as_str()
            .is_some_and(|url| url.contains("/api/swagger"))
            || sign_in_message(body))
}

/// `{"message":"Only signed in user is allowed to call APIs."}` (Gitea/Forgejo).
fn sign_in_message(body: &serde_json::Value) -> bool {
    body["message"].as_str().is_some_and(|message| {
        message
            .to_ascii_lowercase()
            .contains("only signed in user is allowed")
    })
}

/// What the version probes found (§2.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Probed {
    pub kind: ProviderKind,
    pub version: Option<String>,
    /// The probes were refused for want of a sign-in; `kind` is guessed from the refusal.
    pub sign_in_required: bool,
}

/// Probe a host's version endpoints (§2.3). Returns the provider and its version if known.
pub fn probe(
    host: &HostKey,
    env: &Env,
    cancel: &AtomicBool,
) -> Option<(ProviderKind, Option<String>)> {
    probe_host(host, env, cancel).map(|probed| (probed.kind, probed.version))
}

/// `probe`, also saying whether the instance refuses anonymous API calls.
pub fn probe_host(host: &HostKey, env: &Env, cancel: &AtomicBool) -> Option<Probed> {
    let found = |kind, version| {
        Some(Probed {
            kind,
            version,
            sign_in_required: false,
        })
    };
    let origin = origin(host, env);
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("dusty/", env!("CARGO_PKG_VERSION")))
        .timeout(PROBE_TIMEOUT)
        .connect_timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let version = |body: &serde_json::Value, key: &str| body[key].as_str().map(str::to_owned);
    if let Some((status, body)) = probe_get(&client, &format!("{origin}/api/v1/version")) {
        if status == 200
            && let Some(text) = version(&body, "version")
        {
            return found(gitea_family(&text), Some(text));
        }
        // `/version` is public on Gitea and Forgejo; refusing it means every API call needs a
        // sign-in.
        if matches!(status, 401 | 403) && gitea_error(&body) {
            // Forgejo also serves `/api/forgejo/v1/version`; Gitea answers 404 there.
            let forgejo = probe_get(&client, &format!("{origin}/api/forgejo/v1/version"));
            let (kind, version) = match forgejo {
                Some((200, body)) => (ProviderKind::Forgejo, version(&body, "version")),
                Some((401 | 403, _)) => (ProviderKind::Forgejo, None),
                _ => (ProviderKind::Gitea, None),
            };
            return Some(Probed {
                kind,
                version,
                sign_in_required: true,
            });
        }
    }
    if cancel.load(Ordering::Acquire) {
        return None;
    }
    if let Some((status, body)) = probe_get(&client, &format!("{origin}/api/v4/version")) {
        if status == 200
            && let Some(text) = version(&body, "version")
        {
            return found(ProviderKind::GitLab, Some(text));
        }
        // An anonymous `/version` is 401 on GitLab, which still proves GitLab.
        if status == 401
            && body["message"]
                .as_str()
                .is_some_and(|message| message.contains("401"))
        {
            return found(ProviderKind::GitLab, None);
        }
    }
    if cancel.load(Ordering::Acquire) {
        return None;
    }
    match probe_get(&client, &format!("{origin}/api/v3/meta")) {
        Some((200, body))
            if body.get("installed_version").is_some()
                || body.get("verifiable_password_authentication").is_some() =>
        {
            found(ProviderKind::GitHub, version(&body, "installed_version"))
        }
        // GHES in private mode refuses even `/meta` without a token.
        Some((401, body))
            if body["message"]
                .as_str()
                .is_some_and(|message| message.contains("Must authenticate"))
                || body["documentation_url"]
                    .as_str()
                    .is_some_and(|url| url.contains("docs.github.com")) =>
        {
            Some(Probed {
                kind: ProviderKind::GitHub,
                version: None,
                sign_in_required: true,
            })
        }
        _ => None,
    }
}

// ------------------------------------------------------------------------------ detection

pub fn unknown_provider(host: &HostKey) -> ForgeError {
    ForgeError::Unsupported {
        capability: "provider",
        reason: format!("Unknown forge at {host} · set [forge.hosts.\"{host}\"].provider"),
    }
}

fn detected(
    host: &HostKey,
    kind: ProviderKind,
    source: DetectSource,
    inputs: &DetectInputs,
    server_version: Option<String>,
    login: Option<String>,
) -> Detected {
    let config = inputs.config.hosts.get(host);
    let api_base = config.and_then(|config| config.api_base.as_deref());
    Detected {
        host: host.clone(),
        kind,
        endpoints: endpoints_for(kind, host, api_base, &inputs.env),
        cli: Some(crate::forge::host_state::cli_for(kind)),
        login: config.and_then(|config| config.login.clone()).or(login),
        server_version,
        source,
        sign_in_required: false,
    }
}

/// Detection without the in-memory cache.
pub fn detect_with(
    host: &HostKey,
    inputs: &DetectInputs,
    cancel: &AtomicBool,
) -> Result<Detected, ForgeError> {
    let build = |kind, source, version, login| detected(host, kind, source, inputs, version, login);
    if let Some(kind) = inputs
        .config
        .hosts
        .get(host)
        .and_then(|config| config.provider)
    {
        return Ok(build(kind, DetectSource::Config, None, None));
    }
    if let Some(kind) = inputs
        .env
        .get("DUSTY_FORGE_PROVIDER")
        .as_deref()
        .and_then(parse_kind)
        && inputs
            .env
            .get("DUSTY_FORGE_HOST")
            .is_none_or(|scope| HostKey::new(&scope) == *host)
    {
        return Ok(build(kind, DetectSource::Env, None, None));
    }
    if let Some(kind) = known_host(host) {
        return Ok(build(kind, DetectSource::KnownHost, None, None));
    }
    let file = inputs.hosts_file.as_deref();
    if let Some(record) = file
        .map(HostsFile::load)
        .and_then(|hosts| hosts.fresh(host, unix_now()).cloned())
        && let Some(kind) = parse_kind(&record.kind)
    {
        let mut found = build(
            kind,
            DetectSource::Cache,
            record.server_version.clone(),
            None,
        );
        // A recorded base only stands in for the derived one; config and env still win.
        if inputs
            .config
            .hosts
            .get(host)
            .and_then(|config| config.api_base.as_ref())
            .is_none()
            && inputs.env.get("DUSTY_FORGE_API_BASE").is_none()
            && inputs.env.get("DUSTY_FORGE_ORIGIN").is_none()
        {
            found.endpoints.api_base = record.api_base;
            found.endpoints.graphql = record.graphql;
            found.endpoints.web_base = record.web_base;
        }
        found.sign_in_required = record.sign_in_required;
        return Ok(found);
    }
    if cancel.load(Ordering::Acquire) {
        return Err(ForgeError::Cancelled);
    }
    let claimed = credential::claim(host, &inputs.options, &inputs.env, cancel);
    let found = match claimed {
        Some((kind, source)) => {
            let login = match &source {
                credential::CredentialSource::TeaConfig(name) => Some(name.clone()),
                _ => None,
            };
            // tea and fj serve both Gitea and Forgejo; the version string tells them apart.
            let (kind, version, sign_in_required) = match kind {
                ProviderKind::Gitea | ProviderKind::Forgejo if inputs.probe => {
                    match probe_host(host, &inputs.env, cancel) {
                        Some(
                            probed @ Probed {
                                kind: ProviderKind::Gitea | ProviderKind::Forgejo,
                                ..
                            },
                        ) => (probed.kind, probed.version, probed.sign_in_required),
                        _ => (kind, None, false),
                    }
                }
                _ => (kind, None, false),
            };
            let mut found = build(
                kind,
                DetectSource::Credential(source.to_string()),
                version,
                login,
            );
            found.sign_in_required = sign_in_required;
            Some(found)
        }
        None if inputs.probe => probe_host(host, &inputs.env, cancel).map(|probed| {
            let mut found = build(probed.kind, DetectSource::Probe, probed.version, None);
            found.sign_in_required = probed.sign_in_required;
            found
        }),
        None => None,
    };
    if cancel.load(Ordering::Acquire) {
        return Err(ForgeError::Cancelled);
    }
    let Some(found) = found else {
        return Err(unknown_provider(host));
    };
    if let Some(path) = file {
        record_detected(path, &found);
    }
    Ok(found)
}

type Memo = HashMap<(HostKey, ConnectOptions), (Result<Detected, ForgeError>, Instant)>;

fn memo() -> &'static Mutex<Memo> {
    static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
    MEMO.get_or_init(Default::default)
}

const DETECTED_TTL: Duration = Duration::from_secs(5 * 60);
const UNDETECTED_TTL: Duration = Duration::from_secs(10);

/// The forge behind a host, for the app (config, env, CLI stores and probes of this process).
/// Remembered for 5 min (10 s when unknown); `forget` drops it (`r` in the accounts popover).
pub fn detect(
    host: &HostKey,
    options: &ConnectOptions,
    cancel: &AtomicBool,
) -> Result<Detected, ForgeError> {
    let key = (host.clone(), options.clone());
    if let Some((result, at)) = memo()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
    {
        let ttl = if result.is_ok() {
            DETECTED_TTL
        } else {
            UNDETECTED_TTL
        };
        if at.elapsed() < ttl {
            return result.clone();
        }
    }
    let result = detect_with(host, &DetectInputs::from_env(options), cancel);
    if let Err(error) = &result {
        crate::forge::host_state::observe_host_error(host, None, error);
    }
    if !matches!(result, Err(ForgeError::Cancelled)) {
        memo()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, (result.clone(), Instant::now()));
    }
    result
}

pub fn forget() {
    memo()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

/// F1's name for the provider of a host, kept for callers that only need the kind. Unknown
/// hosts are no longer assumed to be GitHub: they must be detected.
pub fn provider_for(host: &HostKey) -> ProviderKind {
    detect(host, &ConnectOptions::from_env(), &AtomicBool::new(false))
        .map(|detected| detected.kind)
        .unwrap_or(ProviderKind::GitHub)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_forms_parse_to_host_and_repo() {
        for (url, host, repo) in [
            (
                "git@github.com:orbit/orbit.git",
                "github.com",
                "orbit/orbit",
            ),
            (
                "https://github.com/orbit/orbit",
                "github.com",
                "orbit/orbit",
            ),
            (
                "ssh://git@git.corp.example:2222/orbit/orbit.git",
                "git.corp.example",
                "orbit/orbit",
            ),
            (
                "https://git.corp.example:8443/orbit/orbit.git",
                "git.corp.example:8443",
                "orbit/orbit",
            ),
            (
                "https://gitlab.com/orbit/team/orbit.git",
                "gitlab.com",
                "orbit/team/orbit",
            ),
        ] {
            let (parsed_host, parsed_repo) = parse_remote(url).unwrap();
            assert_eq!(parsed_host.to_string(), host, "{url}");
            assert_eq!(parsed_repo.full(), repo, "{url}");
        }
        assert!(parse_remote("/srv/git/orbit.git").is_none());
    }

    #[test]
    fn gh_remote_order_is_followed() {
        let directory = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(directory.path()).unwrap();
        repository
            .remote("origin", "git@github.com:me/orbit.git")
            .unwrap();
        repository
            .remote("upstream", "git@github.com:orbit/orbit.git")
            .unwrap();
        let chosen = choose_remote_with(directory.path(), None).unwrap();
        assert_eq!(
            (chosen.remote.as_str(), chosen.repo.full()),
            ("upstream", "orbit/orbit".into())
        );
        repository
            .config()
            .unwrap()
            .set_str("remote.origin.gh-resolved", "base")
            .unwrap();
        assert_eq!(
            choose_remote_with(directory.path(), None).unwrap().remote,
            "origin"
        );
        repository
            .remote("mirror", "https://codeberg.org/orbit/orbit.git")
            .unwrap();
        assert_eq!(
            choose_remote_with(directory.path(), Some("mirror"))
                .unwrap()
                .remote,
            "mirror"
        );
    }

    #[test]
    fn config_hosts_parse_provider_base_and_login() {
        let config = ForgeConfig::parse(
            "[forge]\nremote = \"origin\"\n[forge.hosts.\"forgejo.example.org\"]\nprovider = \"forgejo\"\nlogin = \"blurskye\"\n[forge.hosts.\"git.corp.example:8443\"]\nprovider = \"gitlab\"\napi_base = \"https://git.corp.example:8443/gitlab/api/v4\"\n",
        );
        assert_eq!(config.remote.as_deref(), Some("origin"));
        let corp = &config.hosts[&HostKey::new("git.corp.example:8443")];
        assert_eq!(corp.provider, Some(ProviderKind::GitLab));
        let endpoints = endpoints_for(
            ProviderKind::GitLab,
            &HostKey::new("git.corp.example:8443"),
            corp.api_base.as_deref(),
            &Env::from_pairs::<&str, &str>([]),
        );
        assert_eq!(
            endpoints.api_base,
            "https://git.corp.example:8443/gitlab/api/v4"
        );
        assert_eq!(
            endpoints.graphql.as_deref(),
            Some("https://git.corp.example:8443/gitlab/api/graphql")
        );
        assert_eq!(endpoints.web_base, "https://git.corp.example:8443");
        assert_eq!(
            config.hosts[&HostKey::new("forgejo.example.org")]
                .login
                .as_deref(),
            Some("blurskye")
        );
        assert_eq!(ForgeConfig::parse("not toml ["), ForgeConfig::default());
    }

    #[test]
    fn versions_tell_gitea_from_forgejo() {
        assert_eq!(gitea_family("11.0.1+gitea-1.22.0"), ProviderKind::Forgejo);
        assert_eq!(gitea_family("1.22.6"), ProviderKind::Gitea);
    }
}
