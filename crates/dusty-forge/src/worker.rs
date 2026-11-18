//! `ForgeJobs::request` / `ForgeJobs::write`: provider-layer work on the same
//! single, cancellable forge worker. Host, transport and credential are resolved on the worker
//! so the UI thread never runs a CLI or opens a socket.

use super::ForgeCommands;
use super::ForgeJobs;
use super::ForgeReply;
use crate::cache;
use crate::cli::redact;
use crate::detect;
use crate::provider;
use crate::provider::Conditional;
use crate::provider::Fetched;
use crate::provider::ForgeContext;
use crate::provider::Provider;
use crate::provider::RepoRef;
use crate::request::ForgeError;
use crate::request::Payload;
use crate::request::Read;
use crate::request::Receipt;
use crate::request::Write;
use crate::transport;
use crate::transport::ConnectOptions;
use crate::transport::HostKey;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Where a request goes. `host: None` means "the forge of the repository's remote" (the
/// first `Read::Repository`); `repo: None` with a host is a host-wide read (inbox search).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ForgeTarget {
    pub host: Option<String>,
    pub repo: Option<String>,
}

impl ForgeTarget {
    pub fn remote() -> Self {
        Self::default()
    }

    pub fn repository(host: &str, repo: &str) -> Self {
        Self {
            host: Some(host.into()),
            repo: Some(repo.into()),
        }
    }

    pub fn host(host: &str) -> Self {
        Self {
            host: Some(host.into()),
            repo: None,
        }
    }

    pub fn resolve(&self, root: &Path) -> Result<(HostKey, Option<RepoRef>), ForgeError> {
        match &self.host {
            Some(host) => {
                let host = super::valid_host(host).map_err(ForgeError::Failed)?;
                let repo =
                    match &self.repo {
                        Some(repo) => Some(RepoRef::parse(repo).ok_or_else(|| {
                            ForgeError::Failed(format!("invalid repository {repo}"))
                        })?),
                        None => None,
                    };
                Ok((HostKey::new(&host), repo))
            }
            None => {
                let remote = detect::choose_remote(root)?;
                Ok((remote.host, Some(remote.repo)))
            }
        }
    }
}

/// Provider, endpoints and transport for a target.
pub fn connect(
    root: &Path,
    target: &ForgeTarget,
    cancel: &AtomicBool,
) -> Result<(Arc<dyn Provider>, ForgeContext), ForgeError> {
    let (host, repo) = target.resolve(root)?;
    connect_to(root, &host, repo, cancel)
}

/// `connect` for a host and repository already resolved.
pub fn connect_to(
    root: &Path,
    host: &HostKey,
    repo: Option<RepoRef>,
    cancel: &AtomicBool,
) -> Result<(Arc<dyn Provider>, ForgeContext), ForgeError> {
    let options = ConnectOptions::from_env();
    let detected = detect::detect(host, &options, cancel)?;
    let provider = provider::build(detected.kind)?;
    let endpoints = detected.endpoints;
    let transport = transport::for_host(&endpoints, &options, cancel)?;
    // Detection already saw this instance refuse anonymous API calls: say so instead of
    // sending reads that can only fail.
    if detected.sign_in_required && transport.kind().anonymous() {
        let error = ForgeError::SignInRequired {
            host: endpoints.host.to_string(),
            hints: crate::credential::sign_in_hints(endpoints.kind, &endpoints.host),
        };
        crate::host_state::observe_error(&endpoints, &error);
        return Err(error);
    }
    Ok((
        provider,
        ForgeContext::new(root.to_path_buf(), endpoints, options, repo, transport),
    ))
}

/// Anonymous quotas are small (60/hour on github.com): cached reads stay fresh this many times
/// longer, so revalidation sweeps and warm-ups spend less of it.
pub const ANONYMOUS_TTL_FACTOR: u32 = 4;

/// The budget for a payload on this context.
fn budget(ctx: &ForgeContext, read: &Read, value: &Payload) -> std::time::Duration {
    let ttl = cache::ttl(read, Some(value));
    if ctx.anonymous() {
        ttl.saturating_mul(ANONYMOUS_TTL_FACTOR)
    } else {
        ttl
    }
}

fn failed(error: String) -> ForgeError {
    ForgeError::Failed(redact(&error))
}

/// One conditional read written through to the disk cache: a list this provider can
/// read incrementally asks only for what changed since the cached copy and merges into it; a
/// payload is stored with its freshness budget, a 304 or unchanged fingerprint renews the entry.
pub fn fetch_through(
    provider: &dyn Provider,
    ctx: &ForgeContext,
    read: &Read,
    conditional: Conditional,
    cancel: &AtomicBool,
) -> Result<Fetched, ForgeError> {
    let cache = cache::Cache::shared();
    let key = cache::CacheKey::new(ctx.host(), ctx.repo.as_ref(), read);
    let (conditional, base) = match &cache {
        Some(cache) => crate::incremental::prepare(provider, ctx, cache, &key, read, conditional),
        None => (conditional, None),
    };
    // GitLab numbers issues and merge requests apart and `Read::Timeline` names only the
    // number: after a restart the kind comes from the disk, not a guess.
    let gitlab = provider.kind() == crate::provider::ProviderKind::GitLab;
    if gitlab && let (Read::Timeline(number), Some(cache)) = (read, &cache) {
        crate::provider::gitlab::restore_kind(ctx, cache, *number);
    }
    let started = cache::now_ms();
    let result = crate::inflight::read(provider, ctx, read, &conditional, cancel);
    if gitlab
        && result.is_ok()
        && let (
            Read::Issue(number) | Read::PullRequest(number) | Read::Fingerprint(number),
            Some(cache),
        ) = (read, &cache)
    {
        crate::provider::gitlab::keep_kind(ctx, cache, *number);
    }
    let result = match (result, base) {
        (Ok(fetched), Some(base)) => Ok(crate::incremental::merge(read, base, fetched)),
        (result, _) => result,
    };
    if let (Ok(fetched), Some(cache)) = (&result, &cache) {
        match fetched {
            Fetched::Payload {
                value,
                etag,
                fingerprint,
            } => {
                cache.store_since(
                    &key,
                    value,
                    budget(ctx, read, value),
                    etag.clone(),
                    fingerprint.clone(),
                    started,
                );
                // A batch also fills each item's own entry, so opening one of them after a
                // prefetch is a cache hit.
                for (read, value, fingerprint) in cache::items_of(value) {
                    cache.store_since(
                        &cache::CacheKey::new(ctx.host(), ctx.repo.as_ref(), &read),
                        &value,
                        budget(ctx, &read, &value),
                        None,
                        fingerprint,
                        started,
                    );
                }
            }
            Fetched::NotModified => {
                cache.touch(&key);
            }
        }
        let missing = key.missing();
        if missing.file(cache.root()).exists() {
            cache.remove(&missing);
        }
    }
    // A durable "not found" (an expired log, a deleted run, an endpoint this forge lacks) is
    // remembered too, so a restart paints it from the cache like any answer.
    if let (Err(error @ ForgeError::NotFound(_)), Some(cache)) = (&result, &cache) {
        cache.store_since(
            &key.missing(),
            error.to_string(),
            cache::MISSING_TTL,
            None,
            None,
            started,
        );
    }
    result
}

/// Where a cached read's answer came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Served {
    /// A fresh cache entry; nothing was sent.
    Cache,
    /// The forge answered 304 (or an unchanged fingerprint); the cached copy was renewed.
    Revalidated,
    Network,
}

/// The cached entry for `read` when it is still fresh, without connecting.
pub fn cached_fresh(host: &HostKey, repo: Option<&RepoRef>, read: &Read) -> Option<Payload> {
    let cache = cache::Cache::shared()?;
    let envelope = cache.get::<Payload>(&cache::CacheKey::new(host, repo, read))?;
    envelope.fresh().then_some(envelope.data)
}

/// A read for a one-shot caller (`dusty-git forge`): a fresh cache entry answers without a
/// request; otherwise the read revalidates the cached copy with its ETag and writes through.
pub fn cached_read(
    provider: &dyn Provider,
    ctx: &ForgeContext,
    read: &Read,
    refresh: bool,
    cancel: &AtomicBool,
) -> Result<(Payload, Served), ForgeError> {
    let cache = cache::Cache::shared();
    let key = cache::CacheKey::new(ctx.host(), ctx.repo.as_ref(), read);
    let cached = cache.as_ref().and_then(|cache| cache.get::<Payload>(&key));
    if let Some(envelope) = &cached
        && envelope.fresh()
        && !refresh
    {
        return Ok((envelope.data.clone(), Served::Cache));
    }
    let conditional = Conditional {
        etag: cached.as_ref().and_then(|envelope| envelope.etag.clone()),
        fingerprint: cached
            .as_ref()
            .and_then(|envelope| envelope.fingerprint.clone()),
        ..Conditional::default()
    };
    match fetch_through(provider, ctx, read, conditional, cancel)? {
        Fetched::Payload { value, .. } => Ok((value, Served::Network)),
        Fetched::NotModified => match cached {
            Some(envelope) => Ok((envelope.data, Served::Revalidated)),
            // Only a cached copy can be revalidated; read it again in full.
            None => match fetch_through(provider, ctx, read, Conditional::default(), cancel)? {
                Fetched::Payload { value, .. } => Ok((value, Served::Network)),
                Fetched::NotModified => Err(ForgeError::BadResponse(
                    "the forge answered 304 to an unconditional read".into(),
                )),
            },
        },
    }
}

/// Run one write against its provider and drop what it made stale from the cache.
/// Porcelain acts locally; a PR creation pushes first when asked and remembers its base
/// (`branch.<head>.gh-merge-base`).
pub fn write_through(
    commands: &ForgeCommands<'_>,
    root: &Path,
    target: &ForgeTarget,
    write: &Write,
) -> Result<Receipt, ForgeError> {
    if write.porcelain() {
        return commands.porcelain(root, target, write);
    }
    let target = match write {
        Write::CreatePullRequest(request) => {
            ForgeTarget::repository(&request.host, &request.base_repository)
        }
        _ => target.clone(),
    };
    let (provider, ctx) = connect(root, &target, commands.cancelled)?;
    // Anonymous: refused before anything is sent, and before a PR's branch is pushed.
    ctx.require_credential(write, local_write(provider.kind(), write))?;
    let pushed = match write {
        Write::CreatePullRequest(request) => {
            super::pull_request::push_reviewed(commands, root, request).map_err(failed)?;
            request.push.is_some()
        }
        _ => false,
    };
    let written = provider.write(&ctx, write, commands.cancelled);
    if let Ok(receipt) = &written {
        invalidate(&ctx, &receipt.invalidates);
        if let Write::CreatePullRequest(request) = write {
            crate::template::set_merge_base(root, &request.head_branch, &request.base_branch);
        }
    }
    written.map_err(|error| match error {
        _ if pushed => {
            ForgeError::Failed("Branch pushed, but the pull request wasn't created".into())
        }
        error => error,
    })
}

/// Writes a provider serves without the forge's write API: viewed files kept on this computer
/// (GitLab, Gitea) and artifact downloads of public projects (GitLab, Gitea).
pub fn local_write(kind: provider::ProviderKind, write: &Write) -> bool {
    kind != provider::ProviderKind::GitHub
        && matches!(
            write,
            Write::SetViewed { .. } | Write::DownloadArtifact { .. }
        )
}

/// What a confirmed write changed is revalidated by the next reader.
pub fn invalidate(ctx: &ForgeContext, reads: &[Read]) {
    if let Some(cache) = cache::Cache::shared() {
        for read in reads {
            cache.invalidate(&cache::CacheKey::new(ctx.host(), ctx.repo.as_ref(), read));
        }
    }
}

impl ForgeJobs {
    /// Run one `Read` against the target's provider.
    pub fn request(
        &mut self,
        root: PathBuf,
        request_id: u64,
        target: ForgeTarget,
        read: Read,
    ) -> Result<(), String> {
        self.fetch(root, request_id, target, read, Conditional::default())
    }

    /// Run one conditional `Read` and write the answer through to the disk cache.
    pub fn fetch(
        &mut self,
        root: PathBuf,
        request_id: u64,
        target: ForgeTarget,
        read: Read,
        conditional: Conditional,
    ) -> Result<(), String> {
        let background = self.warming();
        self.fetch_as(
            root,
            request_id,
            target,
            read,
            conditional,
            background,
            None,
        )
    }

    /// `fetch`, as background work when nobody waits for the answer (a cached entry being
    /// checked): its requests then yield to the ones somebody does wait for. `kind` is what
    /// the asker knows the numbered item is (GitLab numbers issues and merge requests apart,
    /// and `Read::Timeline` names only the number).
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_as(
        &mut self,
        root: PathBuf,
        request_id: u64,
        target: ForgeTarget,
        read: Read,
        conditional: Conditional,
        background: bool,
        kind: Option<crate::model::ItemKind>,
    ) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            let result = connect(root, &target, commands.cancelled).and_then(|(provider, ctx)| {
                ctx.set_background(background);
                if let (Some(kind), Read::Timeline(number)) = (kind, &read)
                    && provider.kind() == crate::provider::ProviderKind::GitLab
                {
                    crate::provider::gitlab::note_kind(&ctx, *number, kind);
                }
                fetch_through(
                    provider.as_ref(),
                    &ctx,
                    &read,
                    conditional,
                    commands.cancelled,
                )
            });
            ForgeReply::Fetched(read, result)
        })
    }

    /// Run one `Write`; porcelain acts locally, a PR creation pushes first when asked.
    pub fn write(
        &mut self,
        root: PathBuf,
        request_id: u64,
        target: ForgeTarget,
        write: Write,
    ) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            let result = write_through(commands, root, &target, &write);
            ForgeReply::Written(write, result)
        })
    }
}
