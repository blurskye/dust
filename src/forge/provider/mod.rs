//! The provider layer (FORGE.md §3.2): one `Provider` per forge kind maps `Read`/`Write` onto
//! `ApiRequest`s and responses onto the normalised model. Providers never pick a transport;
//! `ForgeContext::send` does, and owns the per-Read deadline and the 401 fallback.

mod closing;
mod common;
mod parallel;
mod reactions;
pub mod gitea;
pub mod github;
pub mod gitlab;

use crate::forge::capability::Capabilities;
use crate::forge::capability::Terms;
use crate::forge::model::Fingerprint;
use crate::forge::model::Timestamp;
use crate::forge::model::User;
use crate::forge::request::ForgeError;
use crate::forge::request::Payload;
use crate::forge::request::Read;
use crate::forge::request::Receipt;
use crate::forge::request::Write;
use crate::forge::transport;
use crate::forge::transport::Accept;
use crate::forge::transport::ApiRequest;
use crate::forge::transport::ApiResponse;
use crate::forge::transport::ConnectOptions;
use crate::forge::transport::Endpoints;
use crate::forge::transport::HostKey;
use crate::forge::transport::Transport;
use crate::forge::transport::TransportError;
use crate::forge::transport::TransportKind;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize)]
pub enum ProviderKind {
    GitHub,
    GitLab,
    Gitea,
    Forgejo,
}

impl ProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::GitHub => "GitHub",
            ProviderKind::GitLab => "GitLab",
            ProviderKind::Gitea => "Gitea",
            ProviderKind::Forgejo => "Forgejo",
        }
    }
}

/// `owner/name`; GitLab's owner may be a nested group path (`team/sub`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn parse(full: &str) -> Option<Self> {
        let (owner, name) = full.trim().trim_matches('/').rsplit_once('/')?;
        (!owner.is_empty() && !name.is_empty()).then(|| Self {
            owner: owner.into(),
            name: name.trim_end_matches(".git").into(),
        })
    }

    pub fn full(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Everything one request needs. The transport can change mid-flight (a 401 drops to
/// passthrough), so it sits behind a lock; the worker owns the context.
pub struct ForgeContext {
    pub root: PathBuf,
    pub endpoints: Endpoints,
    pub options: ConnectOptions,
    /// `None` for host-wide reads (inbox search, notifications).
    pub repo: Option<RepoRef>,
    pub viewer: Option<User>,
    transport: Mutex<Arc<dyn Transport>>,
    deadline: Mutex<Option<Instant>>,
    fell_back: AtomicBool,
    /// Work nobody is waiting for (the warm worker, a cached entry's check): its requests are
    /// sent as `Class::Background`, so they take few of the in-flight slots and stop once the
    /// quota's reserve is reached (half the small quota while anonymous).
    background: AtomicBool,
}

impl ForgeContext {
    pub fn new(
        root: PathBuf,
        endpoints: Endpoints,
        options: ConnectOptions,
        repo: Option<RepoRef>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            root,
            endpoints,
            options,
            repo,
            viewer: None,
            transport: Mutex::new(transport),
            deadline: Mutex::new(None),
            fell_back: AtomicBool::new(false),
            background: AtomicBool::new(false),
        }
    }

    /// Mark this context's reads as background work (the warm worker's prefetches).
    pub fn set_background(&self, background: bool) {
        self.background
            .store(background, std::sync::atomic::Ordering::Release);
    }

    pub fn background(&self) -> bool {
        self.background.load(std::sync::atomic::Ordering::Acquire)
    }

    /// No credential: public reads only (FORGE.md §2.5).
    pub fn anonymous(&self) -> bool {
        self.transport_kind().anonymous()
    }

    /// Refuse a write before anything is sent while the host is browsed anonymously. Writes
    /// this provider keeps on this computer (`local`) are let through.
    pub fn require_credential(&self, write: &Write, local: bool) -> Result<(), ForgeError> {
        if !self.anonymous() || local || write.porcelain() {
            return Ok(());
        }
        Err(ForgeError::NotAuthenticated {
            host: self.host().to_string(),
            hint: crate::forge::credential::login_hint(self.endpoints.kind, self.host()),
        })
    }

    /// One read that needs a sign-in while anonymous (`read logs`, `see notifications`).
    pub fn sign_in_to(&self, what: &str) -> ForgeError {
        crate::forge::credential::sign_in_to(what, self.endpoints.kind, self.host())
    }

    /// A context whose transport refuses to send; `describe()` and plans use it.
    pub fn offline(endpoints: Endpoints, repo: Option<RepoRef>) -> Self {
        Self::new(
            std::env::temp_dir(),
            endpoints,
            ConnectOptions::default(),
            repo,
            Arc::new(Offline),
        )
    }

    pub fn host(&self) -> &HostKey {
        &self.endpoints.host
    }

    pub fn repo(&self) -> Result<&RepoRef, ForgeError> {
        self.repo
            .as_ref()
            .ok_or_else(|| ForgeError::Failed("this request needs a repository".into()))
    }

    pub fn transport(&self) -> Arc<dyn Transport> {
        self.transport
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn transport_kind(&self) -> TransportKind {
        self.transport().kind()
    }

    /// Start a Read's overall deadline (§3.5); every request it fans out to shares it.
    pub fn start_deadline(&self, budget: Duration) {
        *self
            .deadline
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Instant::now() + budget);
    }

    pub fn clear_deadline(&self) {
        *self
            .deadline
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    /// Send one request. A 401 over direct HTTP evicts the credential and retries once
    /// through the CLI (§1.2); the CLI attaches its own sign-in.
    pub fn send(
        &self,
        request: &ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<ApiResponse, ForgeError> {
        // Every REST GET revalidates what this process already holds (§7.5).
        let conditional = crate::forge::etag::key(&self.endpoints.api_base, request);
        let prepared;
        let request = match &conditional {
            Some(key) => {
                let mut copy = request.clone();
                crate::forge::etag::prepare(key, &mut copy);
                prepared = copy;
                &prepared
            }
            None => request,
        };
        // Host states for the notices and the accounts popover (FORGE.md §2.5).
        let result = self.send_once(request, cancel);
        let result = match (result, &conditional) {
            (Ok(response), Some(key)) => Ok(crate::forge::etag::settle(key, response)),
            (result, _) => result,
        };
        match &result {
            Ok(response) => {
                crate::forge::host_state::observe_response(
                    &self.endpoints,
                    &self.transport().kind(),
                );
                // A 401 or an exhausted quota reached the forge but still says the host is
                // signed out or paused; item-level and server errors leave the state alone.
                if matches!(response.status, 401 | 403 | 429) {
                    let error = crate::forge::http::classify(&self.endpoints, response);
                    crate::forge::host_state::observe_error(&self.endpoints, &error);
                }
            }
            Err(error) => crate::forge::host_state::observe_error(&self.endpoints, error),
        }
        result
    }

    fn send_once(
        &self,
        request: &ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<ApiResponse, ForgeError> {
        let mut request = request.clone();
        if let Some(deadline) = *self
            .deadline
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ForgeError::Timeout);
            }
            request.timeout = request.timeout.min(remaining);
        }
        let transport = self.transport();
        if self.background() {
            request.class = transport::Class::Background;
        }
        let result = transport
            .send(self.host(), &request, cancel)
            .map_err(|error| self.transport_error(error));
        let signed_out = match &result {
            Ok(response) => response.status == 401,
            Err(error) => matches!(error, ForgeError::NotAuthenticated { .. }),
        };
        let kind = transport.kind();
        if !signed_out
            || kind.anonymous()
            || self
                .fell_back
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return result;
        }
        // A token the forge refused: retry through the CLI's own sign-in (§1.2). A CLI that
        // turns out to be signed out (`gh` installed, never logged in) goes anonymous (§2.5).
        let next = match kind {
            TransportKind::Http { .. } => {
                transport::evict(&self.endpoints, &self.options);
                transport::passthrough(&self.endpoints, &self.options).ok()
            }
            TransportKind::Cli(_) => transport::demote(&self.endpoints, &self.options),
            TransportKind::Anonymous => None,
        };
        let Some(next) = next else {
            return result;
        };
        *self
            .transport
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = next.clone();
        if self.background() {
            request.class = transport::Class::Background;
        }
        next.send(self.host(), &request, cancel)
            .map_err(|error| self.transport_error(error))
    }

    fn transport_error(&self, error: TransportError) -> ForgeError {
        match ForgeError::from(error) {
            ForgeError::NotAuthenticated { host, hint } if host.is_empty() => {
                ForgeError::NotAuthenticated {
                    host: self.host().to_string(),
                    hint: format!("{hint} --hostname {}", self.host()),
                }
            }
            error => error,
        }
    }

    /// Send and require a 2xx, classifying anything else (§3.5).
    pub fn ok(&self, request: &ApiRequest, cancel: &AtomicBool) -> Result<ApiResponse, ForgeError> {
        let response = self.send(request, cancel)?;
        if response.success() {
            Ok(response)
        } else {
            Err(crate::forge::http::classify(&self.endpoints, &response))
        }
    }

    pub fn json(&self, request: &ApiRequest, cancel: &AtomicBool) -> Result<Value, ForgeError> {
        let response = self.ok(request, cancel)?;
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        response.json()
    }
}

struct Offline;

impl Transport for Offline {
    fn kind(&self) -> TransportKind {
        TransportKind::Cli(transport::Cli::Gh)
    }

    fn send(
        &self,
        _host: &HostKey,
        _request: &ApiRequest,
        _cancel: &AtomicBool,
    ) -> Result<ApiResponse, TransportError> {
        Err(TransportError::Network("offline".into()))
    }

    fn graphql_batching(&self) -> bool {
        true
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Conditional {
    pub etag: Option<String>,
    pub since: Option<Timestamp>,
    pub fingerprint: Option<Fingerprint>,
}

// `NotModified` is the small variant; every read returns one `Fetched` and moves the payload
// straight out, so boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Fetched {
    Payload {
        value: Payload,
        etag: Option<String>,
        fingerprint: Option<Fingerprint>,
    },
    NotModified,
}

impl Fetched {
    pub fn payload(value: Payload) -> Self {
        Fetched::Payload {
            value,
            etag: None,
            fingerprint: None,
        }
    }
}

/// The first requests a Read sends (later pages follow cursors or `Link`), for fixtures and
/// `describe()`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Plan {
    pub requests: Vec<ApiRequest>,
    /// Set when the read is served without the API (porcelain, local git).
    pub note: Option<&'static str>,
}

pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn terms(&self) -> &'static Terms;
    fn capabilities(&self, ctx: &ForgeContext) -> Capabilities;
    fn plan(&self, ctx: &ForgeContext, read: &Read, cond: &Conditional) -> Plan;
    fn read(
        &self,
        ctx: &ForgeContext,
        read: &Read,
        cond: &Conditional,
        cancel: &AtomicBool,
    ) -> Result<Fetched, ForgeError>;
    fn write(
        &self,
        ctx: &ForgeContext,
        write: &Write,
        cancel: &AtomicBool,
    ) -> Result<Receipt, ForgeError>;
    /// Whether `read` honours `Conditional::since` by answering only what changed (§7.5);
    /// `incremental::merge` folds such an answer into the cached copy.
    fn incremental(&self, _ctx: &ForgeContext, _read: &Read) -> bool {
        false
    }
}

/// Write an artifact archive to `directory/<name>.zip` (§F6 `DownloadArtifact`).
pub fn save_artifact(
    bytes: &[u8],
    name: &str,
    directory: &std::path::Path,
) -> Result<Receipt, ForgeError> {
    let safe = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            c => c,
        })
        .collect::<String>();
    let safe = safe.trim_matches('.').trim();
    let safe = if safe.is_empty() { "artifact" } else { safe };
    std::fs::create_dir_all(directory)
        .map_err(|error| ForgeError::Failed(format!("Could not create the folder: {error}")))?;
    let path = directory.join(format!("{safe}.zip"));
    let partial = directory.join(format!(".{safe}.zip.part"));
    std::fs::write(&partial, bytes)
        .and_then(|_| std::fs::rename(&partial, &path))
        .map_err(|error| ForgeError::Failed(format!("Could not save the artifact: {error}")))?;
    Ok(Receipt {
        summary: format!("Saved {}", path.display()),
        ..Receipt::default()
    })
}

/// A large binary GET (artifact archives): no JSON accept, a long timeout, a high body cap.
pub fn download(request: ApiRequest) -> ApiRequest {
    let mut request = request
        .accept(Accept::Bytes)
        .timeout(crate::forge::transport::LOG_TIMEOUT);
    request.body_cap = ARTIFACT_CAP;
    request
}

/// Archives above this are opened in the browser instead (F6). The process supervisor caps
/// a CLI's output at 256 MiB, and the passthrough adds up to 256 KiB of headers.
pub const ARTIFACT_CAP: usize = 255 * 1024 * 1024;

/// The artifact reads every provider serves from the archive bytes.
pub fn artifact_payload(read: &Read, bytes: &[u8]) -> Result<Payload, ForgeError> {
    match read {
        Read::ArtifactFile { path, .. } => Ok(Payload::Text(
            String::from_utf8_lossy(&crate::forge::zip::read(bytes, path)?).into_owned(),
        )),
        _ => Ok(Payload::ArtifactFiles(crate::forge::zip::list(bytes)?)),
    }
}

pub fn build(kind: ProviderKind) -> Result<Arc<dyn Provider>, ForgeError> {
    match kind {
        ProviderKind::GitHub => Ok(Arc::new(github::GitHub)),
        ProviderKind::GitLab => Ok(Arc::new(gitlab::GitLab)),
        ProviderKind::Gitea | ProviderKind::Forgejo => Ok(Arc::new(gitea::Gitea)),
    }
}

/// The overall budget for one Read: 30 s, longer for diffs and logs (§3.5).
pub fn read_budget(read: &Read) -> Duration {
    match read {
        Read::Diff(_) | Read::CommitDiff(_) => transport::DIFF_TIMEOUT,
        Read::JobLog(_) | Read::ArtifactFiles(_) | Read::ArtifactFile { .. } => {
            transport::LOG_TIMEOUT
        }
        _ => transport::READ_DEADLINE,
    }
}

/// Every `ApiRequest` each Read would send first, without sending anything. `forge-record
/// --plan` consumes this (§8.1) through the env-gated `describe_plans` test.
pub fn describe(kind: ProviderKind, host: &HostKey, repo: &RepoRef) -> Vec<(Read, Plan)> {
    let endpoints = match kind {
        ProviderKind::GitHub => Endpoints::github(host),
        ProviderKind::GitLab | ProviderKind::Gitea | ProviderKind::Forgejo => {
            crate::forge::detect::endpoints_for(
                kind,
                host,
                None,
                &crate::forge::credential::Env::process(),
            )
        }
    };
    let Ok(provider) = build(kind) else {
        return Vec::new();
    };
    let ctx = ForgeContext::offline(endpoints, Some(repo.clone()));
    let cond = Conditional::default();
    sample_reads()
        .into_iter()
        .map(|read| {
            let plan = provider.plan(&ctx, &read, &cond);
            (read, plan)
        })
        .collect()
}

/// One of every Read, with ids the recorder substitutes from the live repository.
pub fn sample_reads() -> Vec<Read> {
    use crate::forge::model::ItemId;
    use crate::forge::model::ItemKind;
    use crate::forge::request::Filter;
    use crate::forge::request::NotifyFilter;
    use crate::forge::request::Page;
    use crate::forge::request::RunFilter;
    let page = Page::default();
    vec![
        Read::Repository,
        Read::Viewer,
        Read::Labels,
        Read::Milestones,
        Read::Collaborators,
        Read::List {
            kind: ItemKind::PullRequest,
            filter: Filter::default(),
            page,
        },
        Read::List {
            kind: ItemKind::Issue,
            filter: Filter::default(),
            page,
        },
        Read::Search {
            kind: ItemKind::PullRequest,
            filter: Filter {
                query: "review-requested:@me".into(),
                ..Filter::default()
            },
            page,
        },
        Read::PullRequest(1),
        Read::Issue(1),
        Read::Fingerprint(1),
        Read::Timeline(1),
        Read::Threads(1),
        Read::Diff(1),
        Read::CommitDiff("HEAD".into()),
        Read::Viewed(1),
        Read::PullRequestForBranch("main".into()),
        Read::Workflows,
        Read::WorkflowSource {
            id: ItemId::new(1),
            reference: None,
        },
        Read::Runs {
            filter: RunFilter::default(),
            page,
        },
        Read::Run {
            id: ItemId::new(1),
            attempt: None,
        },
        Read::Job(ItemId::new(1)),
        Read::JobLog(ItemId::new(1)),
        Read::Artifacts(ItemId::new(1)),
        Read::Notifications {
            filter: NotifyFilter::default(),
            page,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_refs_keep_nested_owners() {
        let repo = RepoRef::parse("orbit/team/orbit.git").unwrap();
        assert_eq!(
            (repo.owner.as_str(), repo.name.as_str()),
            ("orbit/team", "orbit")
        );
        assert!(RepoRef::parse("orbit").is_none());
    }

    /// `DUSTY_FORGE_DESCRIBE=<path>` writes every GitHub Read's plan for `forge-record --plan`.
    #[test]
    fn describe_plans() {
        let plans = describe(
            ProviderKind::GitHub,
            &HostKey::new("github.com"),
            &RepoRef::parse("orbit/orbit").unwrap(),
        );
        assert!(plans.iter().all(|(read, plan)| {
            !plan.requests.is_empty() || plan.note.is_some() || matches!(read, Read::Viewer)
        }));
        if let Some(path) = std::env::var_os("DUSTY_FORGE_DESCRIBE") {
            let value = plans
                .iter()
                .map(|(read, plan)| {
                    serde_json::json!({
                        "read": read,
                        "requests": plan.requests.iter().map(|request| serde_json::json!({
                            "method": request.method.as_str(),
                            "path": request.path_and_query(),
                            "graphql": request.graphql_operation(),
                            "accept": request.accept.header(ProviderKind::GitHub),
                            "body": request.body.as_ref().map(|body| String::from_utf8_lossy(&body.bytes()).into_owned()),
                        })).collect::<Vec<_>>(),
                        "note": plan.note,
                    })
                })
                .collect::<Vec<_>>();
            std::fs::write(path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        }
    }
}
