//! The on-disk forge cache (FORGE.md §7.1), shared by the IDE, the standalone `dusty-git` and
//! `dusty-git forge`. One JSON `Envelope` per entry under
//! `<root>/<host>/<owner-path>/<repo>/<kind>/<sha256(key)[..16]>.json`; host-wide entries
//! (inbox searches, notifications, viewer) under `<root>/<host>/_host/`. Each directory keeps an
//! `index.json` for LRU eviction.
//!
//! Readers take no lock: files are only ever replaced by `rename`, so a reader sees a whole old
//! file or a whole new one. Writers race benignly per key; the index is rewritten from a fresh
//! re-read under a non-blocking `flock` on `index.lock` and simply skipped when the lock is held
//! (the next writer, or a rebuild by scanning, catches up). Anything unreadable is a miss: the
//! file is unlinked best-effort and counted, and nothing here panics on file content.

use crate::forge::model::Fingerprint;
use crate::forge::model::Lifecycle;
use crate::forge::model::RunStatus;
use crate::forge::provider::RepoRef;
use crate::forge::request::Payload;
use crate::forge::request::Read;
use crate::forge::transport::HostKey;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Bumped whenever `Envelope` or `Payload` stops round-tripping; older files become misses.
pub const CACHE_SCHEMA: u32 = 1;
pub const REPO_CAP: u64 = 64 * 1024 * 1024;
pub const HOST_CAP: u64 = 256 * 1024 * 1024;
/// Logs and diffs above this are served from the network only.
pub const LARGE_ENTRY: u64 = 8 * 1024 * 1024;
/// Kinds that are tiny and needed for the first frame.
const NEVER_EVICT: [&str; 3] = ["repo", "viewer", "labels"];
const INDEX_FLUSH: Duration = Duration::from_secs(5);
const INDEX: &str = "index.json";
const LOCK: &str = "index.lock";
/// A `ttl` that never expires (diffs by sha, logs of finished jobs).
pub const FOREVER: Duration = Duration::from_secs(u64::MAX / 4);

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The cache-file family of a read (§7.1).
pub fn kind(read: &Read) -> &'static str {
    match read {
        Read::Repository | Read::RepositoryNamed(_) => "repo",
        Read::Viewer => "viewer",
        Read::Labels => "labels",
        Read::Milestones => "milestones",
        Read::Collaborators => "users",
        Read::List { .. } => "list",
        Read::Search { .. } => "search",
        Read::PullRequest(_) => "pr",
        Read::PullRequests(_) => "prs",
        Read::Fingerprints(_) => "fingerprints",
        Read::ArtifactFiles(_) => "artifact_files",
        Read::ArtifactFile { .. } => "artifact_file",
        Read::Issue(_) => "issue",
        Read::Fingerprint(_) => "fingerprint",
        Read::Timeline(_) => "timeline",
        Read::Threads(_) => "threads",
        Read::Diff(_) => "diff",
        Read::CommitDiff(_) => "commit_diff",
        Read::Viewed(_) => "viewed",
        Read::PullRequestForBranch(_) => "pr_for_branch",
        Read::Workflows => "workflows",
        Read::WorkflowSource { .. } => "workflow_src",
        Read::Runs { .. } => "runs",
        Read::Run { .. } => "run",
        Read::Job(_) => "job",
        Read::JobLog(_) => "log",
        Read::Artifacts(_) => "artifacts",
        Read::Notifications { .. } => "notifications",
        Read::PullRequestTemplate => "template",
    }
}

fn host_wide(read: &Read) -> bool {
    matches!(
        read,
        Read::Search { .. } | Read::Viewer | Read::Notifications { .. }
    )
}

/// Seconds since the epoch of an RFC 3339 UTC-or-offset timestamp; `None` when unparsable.
pub fn unix_seconds(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = of_era * 365 + of_era / 4 - of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let mut seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    let rest = text.get(19..).unwrap_or("");
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-')
        && rest.len() >= 6
    {
        let offset = number_at(rest, 1..3)? * 3600 + number_at(rest, 4..6)? * 60;
        seconds -= if sign == '+' { offset } else { -offset };
    }
    Some(seconds)
}

fn number_at(text: &str, range: std::ops::Range<usize>) -> Option<i64> {
    text.get(range)?.parse().ok()
}

/// The §7.3 freshness budget once the payload is known: closed things live long, running
/// things short, and an item nobody touched for a day is polled every 15 minutes.
pub fn ttl(read: &Read, payload: Option<&Payload>) -> Duration {
    let base = read.ttl();
    let quiet = |updated: &str| {
        unix_seconds(updated).is_some_and(|at| (now_ms() / 1000) as i64 - at > 24 * 3600)
    };
    match (read, payload) {
        (_, Some(Payload::PullRequest(pull))) => match pull.row.lifecycle {
            Lifecycle::Merged | Lifecycle::Closed | Lifecycle::ClosedNotPlanned => {
                Duration::from_secs(24 * 3600)
            }
            _ if quiet(pull.row.updated.as_str()) => Duration::from_secs(15 * 60),
            _ if pull
                .checks
                .iter()
                .any(|check| check.state == crate::forge::model::CiState::Pending) =>
            {
                Duration::from_secs(45)
            }
            _ => Duration::from_secs(60),
        },
        (_, Some(Payload::Issue(issue))) => match issue.row.lifecycle {
            lifecycle if !lifecycle.is_open() => Duration::from_secs(24 * 3600),
            _ if quiet(issue.row.updated.as_str()) => Duration::from_secs(15 * 60),
            _ => base,
        },
        (Read::Runs { .. }, Some(Payload::Runs(runs))) => {
            if runs
                .items
                .iter()
                .any(|run| run.status != RunStatus::Completed)
            {
                Duration::from_secs(20)
            } else {
                base
            }
        }
        (Read::Run { .. }, Some(Payload::Run(detail))) => {
            if detail.run.status == RunStatus::Completed {
                Duration::from_secs(24 * 3600)
            } else {
                base
            }
        }
        (Read::Job(_), Some(Payload::Job(job))) => {
            if job.status == RunStatus::Completed {
                Duration::from_secs(24 * 3600)
            } else {
                base
            }
        }
        (Read::CommitDiff(_), _) => FOREVER,
        _ => base,
    }
}

/// The single-item entries a batch payload also fills (`Read::PullRequests` →
/// `Read::PullRequest(n)` for each, likewise fingerprints).
pub fn items_of(payload: &Payload) -> Vec<(Read, Payload, Option<Fingerprint>)> {
    match payload {
        Payload::PullRequests(pulls) => pulls
            .iter()
            .map(|pull| {
                (
                    Read::PullRequest(pull.row.number),
                    Payload::PullRequest(Box::new(pull.clone())),
                    Some(pull.fingerprint.clone()),
                )
            })
            .collect(),
        Payload::Fingerprints(prints) => prints
            .iter()
            .map(|(number, print)| {
                (
                    Read::Fingerprint(*number),
                    Payload::Fingerprint(print.clone()),
                    Some(print.clone()),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// How often a screen showing CI should poll (t3code cadence): every 45 s while any run, job
/// or check is still going, every 60 s once everything finished. `None` for payloads that are
/// not CI.
/// A job still to finish. A job that already has a conclusion (GitLab's manual jobs are
/// `Skipped` while they wait for someone to start them) does not keep the screen polling.
fn job_pending(job: &crate::forge::model::Job) -> bool {
    job.status != RunStatus::Completed && job.conclusion.is_none()
}

pub fn poll_interval(payload: &Payload) -> Option<Duration> {
    use crate::forge::model::CiState;
    let pending = match payload {
        Payload::Runs(runs) => runs
            .items
            .iter()
            .any(|run| run.status != RunStatus::Completed),
        Payload::Run(detail) => {
            detail.run.status != RunStatus::Completed || detail.jobs.iter().any(job_pending)
        }
        Payload::Job(job) => job_pending(job),
        Payload::PullRequest(pull) => pull
            .checks
            .iter()
            .any(|check| check.state == CiState::Pending),
        Payload::Fingerprint(fingerprint) => fingerprint.rollup == Some(CiState::Pending),
        _ => return None,
    };
    Some(Duration::from_secs(if pending { 45 } else { 60 }))
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CacheKey {
    pub host: String,
    /// `owner/name` (owner may be a nested group); `None` for host-wide entries.
    pub repo: Option<String>,
    pub kind: &'static str,
    pub key: String,
}

impl CacheKey {
    pub fn new(host: &HostKey, repo: Option<&RepoRef>, read: &Read) -> Self {
        Self::for_read(&host.to_string(), repo.map(RepoRef::full).as_deref(), read)
    }

    pub fn for_read(host: &str, repo: Option<&str>, read: &Read) -> Self {
        Self {
            host: host.trim().trim_end_matches('.').to_ascii_lowercase(),
            repo: if host_wide(read) {
                None
            } else {
                repo.filter(|repo| !repo.is_empty()).map(str::to_owned)
            },
            kind: kind(read),
            key: read.cache_key(),
        }
    }

    /// A key outside the `Read` vocabulary (tests, `dusty-git forge cache`).
    pub fn named(host: &str, repo: Option<&str>, kind: &'static str, key: &str) -> Self {
        Self {
            host: host.into(),
            repo: repo.map(str::to_owned),
            kind,
            key: key.into(),
        }
    }

    fn digest(&self) -> String {
        let hash = Sha256::digest(self.key.as_bytes());
        hash[..8].iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The entry's directory, below the cache root.
    pub fn directory(&self, root: &Path) -> PathBuf {
        let mut path = root.join(segment(&self.host));
        match &self.repo {
            Some(repo) => {
                for part in repo.split('/').filter(|part| !part.is_empty()) {
                    path.push(segment(part));
                }
            }
            None => path.push("_host"),
        }
        path
    }

    fn relative(&self) -> String {
        format!("{}/{}.json", self.kind, self.digest())
    }

    pub fn file(&self, root: &Path) -> PathBuf {
        self.directory(root).join(self.relative())
    }
}

/// A path segment that can neither escape the cache root nor collide with `_host`.
fn segment(text: &str) -> String {
    let cleaned = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '@' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    match cleaned.as_str() {
        "" | "." | ".." | "_host" => format!("_{cleaned}_"),
        _ => cleaned,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub schema: u32,
    pub key: String,
    /// Unix milliseconds.
    pub fetched_at: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub fingerprint: Option<Fingerprint>,
    pub server_version: Option<String>,
    /// Seconds.
    pub ttl: u64,
    pub bytes: u64,
    pub data: T,
}

impl<T> Envelope<T> {
    pub fn new(key: &CacheKey, data: T, ttl: Duration) -> Self {
        Self {
            schema: CACHE_SCHEMA,
            key: key.key.clone(),
            fetched_at: now_ms(),
            etag: None,
            last_modified: None,
            fingerprint: None,
            server_version: None,
            ttl: ttl.as_secs(),
            bytes: 0,
            data,
        }
    }

    pub fn age(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.fetched_at))
    }

    pub fn fresh(&self) -> bool {
        self.age() < Duration::from_secs(self.ttl)
    }

    pub fn fetched(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.fetched_at)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub key: String,
    pub kind: String,
    pub bytes: u64,
    pub fetched_at: u64,
    pub last_used: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub schema: u32,
    /// By path relative to the directory (`list/0123abcd….json`).
    pub entries: BTreeMap<String, IndexEntry>,
    pub total_bytes: u64,
}

impl Index {
    fn recount(&mut self) {
        self.total_bytes = self.entries.values().map(|entry| entry.bytes).sum();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub corrupt: u64,
    pub writes: u64,
    pub skipped: u64,
    pub evicted: u64,
}

#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    corrupt: AtomicU64,
    writes: AtomicU64,
    skipped: AtomicU64,
    evicted: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Caps {
    pub repo: u64,
    pub host: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            repo: REPO_CAP,
            host: HOST_CAP,
        }
    }
}

pub struct Cache {
    root: PathBuf,
    caps: Caps,
    counters: Counters,
    /// `last_used` stamps not yet written to an index, per directory.
    touched: Mutex<(Instant, Touched)>,
}

/// `last_used` stamps by directory, then by entry path.
type Touched = HashMap<PathBuf, BTreeMap<String, u64>>;

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

impl Cache {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            caps: Caps::default(),
            counters: Counters::default(),
            touched: Mutex::new((Instant::now(), HashMap::new())),
        }
    }

    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The process-wide cache: `DUSTY_FORGE_CACHE_DIR`, else `$XDG_CACHE_HOME/dusty/forge`.
    /// `None` (memory only) when the API base is overridden without a cache directory — a
    /// fake or redirected API must never seed the real host's entries — and in unit tests.
    /// `[forge].cache_mib` arrives as `DUSTY_FORGE_CACHE_MIB` (repo cap = a quarter).
    pub fn shared() -> Option<Arc<Cache>> {
        static SHARED: OnceLock<Option<Arc<Cache>>> = OnceLock::new();
        SHARED
            .get_or_init(|| {
                let root = default_root()?;
                let mut cache = Cache::at(root);
                if let Some(mib) = std::env::var("DUSTY_FORGE_CACHE_MIB")
                    .ok()
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .filter(|mib| *mib > 0)
                {
                    cache.caps = Caps {
                        host: mib * 1024 * 1024,
                        repo: (mib * 1024 * 1024 / 4).max(1024 * 1024),
                    };
                }
                Some(Arc::new(cache))
            })
            .clone()
    }

    pub fn stats(&self) -> CacheStats {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        CacheStats {
            hits: load(&self.counters.hits),
            misses: load(&self.counters.misses),
            corrupt: load(&self.counters.corrupt),
            writes: load(&self.counters.writes),
            skipped: load(&self.counters.skipped),
            evicted: load(&self.counters.evicted),
        }
    }

    /// Read an entry. Every failure but "absent" unlinks the file and counts as corrupt.
    pub fn get<T: DeserializeOwned>(&self, key: &CacheKey) -> Option<Envelope<T>> {
        let path = key.file(&self.root);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.counters.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            Err(_) => return self.corrupt(&path),
        };
        let envelope = match serde_json::from_slice::<Envelope<T>>(&bytes) {
            Ok(envelope) if envelope.schema == CACHE_SCHEMA && envelope.key == key.key => envelope,
            _ => return self.corrupt(&path),
        };
        self.counters.hits.fetch_add(1, Ordering::Relaxed);
        self.touch_used(key);
        Some(envelope)
    }

    fn corrupt<T>(&self, path: &Path) -> Option<T> {
        self.counters.corrupt.fetch_add(1, Ordering::Relaxed);
        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        }
        None
    }

    /// Store a payload; returns whether it was written (large logs/diffs are not).
    pub fn put<T: Serialize>(&self, key: &CacheKey, envelope: &mut Envelope<T>) -> bool {
        envelope.schema = CACHE_SCHEMA;
        envelope.key = key.key.clone();
        let Ok(mut bytes) = serde_json::to_vec(envelope) else {
            return false;
        };
        if matches!(key.kind, "log" | "diff" | "commit_diff") && bytes.len() as u64 > LARGE_ENTRY {
            self.counters.skipped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        envelope.bytes = bytes.len() as u64;
        if let Ok(sized) = serde_json::to_vec(envelope) {
            bytes = sized;
        }
        let path = key.file(&self.root);
        if !write_atomic(&path, &bytes) {
            return false;
        }
        self.counters.writes.fetch_add(1, Ordering::Relaxed);
        let directory = key.directory(&self.root);
        let entry = IndexEntry {
            key: key.key.clone(),
            kind: key.kind.into(),
            bytes: bytes.len() as u64,
            fetched_at: envelope.fetched_at,
            last_used: now_ms(),
        };
        let relative = key.relative();
        self.with_index(&directory, |index| {
            index.entries.insert(relative.clone(), entry.clone());
            index.recount();
        });
        self.evict(key, &directory);
        true
    }

    /// Store `data` fetched now.
    pub fn store<T: Serialize>(
        &self,
        key: &CacheKey,
        data: T,
        ttl: Duration,
        etag: Option<String>,
        fingerprint: Option<Fingerprint>,
    ) -> bool {
        let mut envelope = Envelope::new(key, data, ttl);
        envelope.etag = etag;
        envelope.fingerprint = fingerprint;
        self.put(key, &mut envelope)
    }

    /// A 304 or an unchanged fingerprint: the data is current as of now.
    pub fn touch(&self, key: &CacheKey) -> bool {
        self.rewrite(key, |envelope| {
            envelope.fetched_at = now_ms();
        })
    }

    /// A confirmed write made this entry stale (§7.7): the next reader revalidates.
    pub fn invalidate(&self, key: &CacheKey) -> bool {
        self.rewrite(key, |envelope| envelope.ttl = 0)
    }

    fn rewrite(
        &self,
        key: &CacheKey,
        change: impl FnOnce(&mut Envelope<serde_json::Value>),
    ) -> bool {
        let Some(mut envelope) = self.get::<serde_json::Value>(key) else {
            return false;
        };
        change(&mut envelope);
        self.put(key, &mut envelope)
    }

    pub fn remove(&self, key: &CacheKey) {
        let _ = std::fs::remove_file(key.file(&self.root));
        let relative = key.relative();
        self.with_index(&key.directory(&self.root), |index| {
            index.entries.remove(&relative);
            index.recount();
        });
    }

    /// Every readable entry of one kind in a repository (or host-wide) directory: the corpus
    /// for local search (§7.4).
    pub fn scan<T: DeserializeOwned>(
        &self,
        host: &str,
        repo: Option<&str>,
        kind: &'static str,
    ) -> Vec<Envelope<T>> {
        let directory = CacheKey::named(host, repo, kind, "")
            .directory(&self.root)
            .join(kind);
        let Ok(entries) = std::fs::read_dir(&directory) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .filter_map(|path| {
                let bytes = std::fs::read(&path).ok()?;
                match serde_json::from_slice::<Envelope<T>>(&bytes) {
                    Ok(envelope) if envelope.schema == CACHE_SCHEMA => Some(envelope),
                    _ => None,
                }
            })
            .collect()
    }

    /// Delete everything below the root.
    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(&self.root) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The index of a directory, rebuilt by scanning when missing or unreadable.
    pub fn index(&self, directory: &Path) -> Index {
        read_index(directory).unwrap_or_else(|| rebuild_index(directory))
    }

    fn touch_used(&self, key: &CacheKey) {
        let mut touched = self
            .touched
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        touched
            .1
            .entry(key.directory(&self.root))
            .or_default()
            .insert(key.relative(), now_ms());
        if touched.0.elapsed() >= INDEX_FLUSH {
            touched.0 = Instant::now();
            let pending = std::mem::take(&mut touched.1);
            drop(touched);
            self.write_used(pending);
        }
    }

    /// Write pending `last_used` stamps (also done at most every 5 s while reading).
    pub fn flush(&self) {
        let pending = {
            let mut touched = self
                .touched
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            touched.0 = Instant::now();
            std::mem::take(&mut touched.1)
        };
        self.write_used(pending);
    }

    fn write_used(&self, pending: Touched) {
        for (directory, used) in pending {
            self.with_index(&directory, |index| {
                for (relative, at) in &used {
                    if let Some(entry) = index.entries.get_mut(relative) {
                        entry.last_used = entry.last_used.max(*at);
                    }
                }
            });
        }
    }

    /// Re-read, change and atomically rewrite a directory's index under its lock; skipped
    /// when another process holds the lock (the index is advisory).
    fn with_index(&self, directory: &Path, change: impl FnOnce(&mut Index)) -> bool {
        let Some(_lock) = IndexLock::try_acquire(directory, 25) else {
            return false;
        };
        let mut index = self.index(directory);
        index.schema = CACHE_SCHEMA;
        change(&mut index);
        match serde_json::to_vec(&index) {
            Ok(bytes) => write_atomic(&directory.join(INDEX), &bytes),
            Err(_) => false,
        }
    }

    /// After a write: bring the repository directory under its cap and the host under its
    /// cap, to 90 %, least recently used first, never touching `NEVER_EVICT` kinds.
    fn evict(&self, key: &CacheKey, directory: &Path) {
        let index = self.index(directory);
        let host_directory = self.root.join(segment(&key.host));
        let host_total = host_total(&host_directory);
        let over_repo = index.total_bytes.saturating_sub(self.caps.repo);
        let over_host = host_total.saturating_sub(self.caps.host);
        if over_repo == 0 && over_host == 0 {
            return;
        }
        let Some(_lock) = IndexLock::try_acquire(directory, 0) else {
            return;
        };
        let mut index = self.index(directory);
        let target_repo = index.total_bytes.saturating_sub(self.caps.repo * 9 / 10);
        let target_host = host_total.saturating_sub(self.caps.host * 9 / 10);
        let mut need = if over_repo > 0 { target_repo } else { 0 }.max(if over_host > 0 {
            target_host
        } else {
            0
        });
        let mut victims = index
            .entries
            .iter()
            .filter(|(_, entry)| !NEVER_EVICT.contains(&entry.kind.as_str()))
            .map(|(relative, entry)| (entry.last_used, relative.clone(), entry.bytes))
            .collect::<Vec<_>>();
        victims.sort();
        for (_, relative, bytes) in victims {
            if need == 0 {
                break;
            }
            let _ = std::fs::remove_file(directory.join(&relative));
            index.entries.remove(&relative);
            need = need.saturating_sub(bytes);
            self.counters.evicted.fetch_add(1, Ordering::Relaxed);
        }
        index.recount();
        if let Ok(bytes) = serde_json::to_vec(&index) {
            write_atomic(&directory.join(INDEX), &bytes);
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.flush();
    }
}

fn default_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DUSTY_FORGE_CACHE_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(test)
        || std::env::var_os("DUSTY_FORGE_API_BASE").is_some_and(|base| !base.is_empty())
        || std::env::var("DUSTY_FORGE_CACHE").as_deref() == Ok("0")
    {
        return None;
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("dusty").join("forge"))
}

/// `<file>.tmp.<pid>.<n>` then `rename`; never leaves the temporary behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = parent.join(format!(
        "{name}.tmp.{}.{}",
        std::process::id(),
        TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return false;
    }
    true
}

fn read_index(directory: &Path) -> Option<Index> {
    let bytes = std::fs::read(directory.join(INDEX)).ok()?;
    let index = serde_json::from_slice::<Index>(&bytes).ok()?;
    (index.schema == CACHE_SCHEMA).then_some(index)
}

/// Rebuild an index from the files present; unreadable files are removed.
fn rebuild_index(directory: &Path) -> Index {
    let mut index = Index {
        schema: CACHE_SCHEMA,
        ..Index::default()
    };
    let Ok(kinds) = std::fs::read_dir(directory) else {
        return index;
    };
    for kind in kinds.flatten() {
        let kind_path = kind.path();
        if !kind_path.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&kind_path) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let Some(name) = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
            else {
                continue;
            };
            if !name.ends_with(".json") {
                continue;
            }
            #[derive(Deserialize)]
            struct Head {
                schema: u32,
                key: String,
                fetched_at: u64,
            }
            let head = std::fs::read(&path).ok().and_then(|bytes| {
                serde_json::from_slice::<Head>(&bytes)
                    .ok()
                    .map(|head| (head, bytes.len()))
            });
            match head {
                Some((head, bytes)) if head.schema == CACHE_SCHEMA => {
                    index.entries.insert(
                        format!("{}/{name}", kind.file_name().to_string_lossy()),
                        IndexEntry {
                            key: head.key,
                            kind: kind.file_name().to_string_lossy().into_owned(),
                            bytes: bytes as u64,
                            fetched_at: head.fetched_at,
                            last_used: head.fetched_at,
                        },
                    );
                }
                _ => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    index.recount();
    index
}

/// Sum of every index below a host directory.
fn host_total(directory: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![(directory.to_path_buf(), 0)];
    while let Some((path, depth)) = stack.pop() {
        if let Some(index) = read_index(&path) {
            total += index.total_bytes;
        }
        if depth >= 8 {
            continue;
        }
        if let Ok(children) = std::fs::read_dir(&path) {
            for child in children.flatten() {
                let child = child.path();
                if child.is_dir() && child.join(INDEX).exists()
                    || child.is_dir() && !looks_like_kind(&child)
                {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    total
}

/// Kind directories hold entry files, never further indexes.
fn looks_like_kind(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| {
        entries.any(|entry| {
            entry.is_ok_and(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            })
        })
    })
}

/// A non-blocking `flock` on `<dir>/index.lock`, retried for at most `wait_ms`.
struct IndexLock(#[allow(dead_code)] std::fs::File);

impl IndexLock {
    fn try_acquire(directory: &Path, wait_ms: u64) -> Option<Self> {
        std::fs::create_dir_all(directory).ok()?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(LOCK))
            .ok()?;
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Some(Self(file)),
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return None,
            }
        }
    }
}

// The lock is released when the file closes.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::model::ItemKind;
    use crate::forge::request::Filter;
    use crate::forge::request::Page;
    use crate::forge::request::StateFilter;

    #[test]
    fn rfc3339_parses_to_unix_seconds() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2026-11-14T10:00:00Z"), Some(1_794_650_400));
        assert_eq!(
            unix_seconds("2026-11-14T12:00:00+02:00"),
            unix_seconds("2026-11-14T10:00:00Z")
        );
        assert_eq!(unix_seconds("yesterday"), None);
    }

    #[test]
    fn keys_place_host_wide_reads_apart_and_never_escape_the_root() {
        let host = HostKey::new("GitHub.com");
        let repo = RepoRef::parse("orbit/team/orbit").unwrap();
        let list = CacheKey::new(
            &host,
            Some(&repo),
            &Read::List {
                kind: ItemKind::PullRequest,
                filter: Filter::default(),
                page: Page::default(),
            },
        );
        let root = Path::new("/cache");
        assert!(
            list.file(root)
                .starts_with("/cache/github.com/orbit/team/orbit/list")
        );
        let search = CacheKey::new(
            &host,
            Some(&repo),
            &Read::Search {
                kind: ItemKind::Issue,
                filter: Filter {
                    state: StateFilter::Open,
                    query: "x".into(),
                },
                page: Page::default(),
            },
        );
        assert!(
            search
                .file(root)
                .starts_with("/cache/github.com/_host/search")
        );
        let hostile = CacheKey::named("..", Some("../../etc"), "list", "k");
        assert!(hostile.file(root).starts_with("/cache/_.._"));
        assert!(!hostile.file(root).to_string_lossy().contains("/../"));
    }

    #[test]
    fn ttl_follows_lifecycle() {
        let read = Read::PullRequest(1);
        let mut pull = crate::forge::model::PullRequest::default();
        pull.row.updated = crate::forge::model::Timestamp::new("2999-01-01T00:00:00Z");
        assert_eq!(
            ttl(&read, Some(&Payload::PullRequest(Box::new(pull.clone())))),
            Duration::from_secs(60)
        );
        pull.row.lifecycle = Lifecycle::Merged;
        assert_eq!(
            ttl(&read, Some(&Payload::PullRequest(Box::new(pull)))),
            Duration::from_secs(24 * 3600)
        );
        assert_eq!(ttl(&Read::CommitDiff("abc".into()), None), FOREVER);
    }
}
