//! Caching and freshness in the GitHub workspace: paint from the disk cache at
//! once, revalidate in the background, merge without moving the reader, warm what the user is
//! likely to open next on a low-priority worker, search locally while the remote search runs,
//! and show writes before the forge confirms them.
//!
//! All of it is on only when the disk cache is (`forge::cache::Cache::shared`); without it the
//! workspace behaves exactly as before: memory only, explicit refreshes, no warm worker.
use super::*;
use crate::forge::ForgeJobs;
use crate::forge::cache::{self, Cache, CacheKey};
use crate::forge::merge;
use crate::forge::model::Fingerprint;
use crate::forge::provider::{Conditional, Fetched};
use crate::forge::records;
use crate::forge::request::Payload;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Interactive work must have been idle this long before the warm worker starts a request.
const WARM_IDLE: Duration = Duration::from_millis(150);
/// The cursor rests this long on a row before its neighbours' details are prefetched.
const PREFETCH_REST: Duration = Duration::from_millis(120);
/// How long one turn (a key, a poll) may wait for the disk in all (`load_disk_soon`).
const DISK_BUDGET: Duration = Duration::from_millis(8);
/// The cursor rests this long on a row before the forge is asked for its details: `j`/`k`
/// through the list paint cached details and send nothing for the rows passed over.
const DWELL: Duration = Duration::from_millis(150);
/// Live search waits for typing to pause.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Visible entries are checked against their budget this often.
const SWEEP: Duration = Duration::from_secs(1);
/// An entry loaded from disk is confirmed once per session unless it is younger than this.
const CONFIRM_AFTER: Duration = Duration::from_secs(10);
/// Local hits shown while the remote search runs.
const LOCAL_HITS: usize = 50;

pub(super) struct Meta {
    pub(super) fetched: SystemTime,
    /// When this session last asked the forge (answered or not).
    pub(super) checked: Option<Instant>,
    pub(super) ttl: Duration,
    pub(super) etag: Option<String>,
    pub(super) fingerprint: Option<Fingerprint>,
    /// The last background check failed; the entry is shown as cached.
    pub(super) offline: bool,
    /// The forge has confirmed this entry during this session.
    pub(super) verified: bool,
}

impl Meta {
    fn age(&self) -> Duration {
        self.fetched.elapsed().unwrap_or_default()
    }
}

pub(super) struct Live {
    pub(super) page: usize,
    pub(super) query: String,
    pub(super) state: String,
    changed: Instant,
    sent: bool,
    corpus: Option<Vec<Value>>,
}

pub(super) struct LiveRows {
    pub(super) local: Vec<Value>,
    pub(super) remote: Vec<Value>,
    pub(super) divider: String,
}

type Scope = (String, String, u64);
/// One optimistic change to a cached value.
type Edit = Box<dyn Fn(&mut Value)>;

pub(super) struct Fresh {
    cache: Option<Arc<Cache>>,
    pub(super) meta: HashMap<Key, Meta>,
    /// Background revalidations in flight: merged quietly, failures only mark the entry.
    pub(super) implicit: HashSet<Key>,
    warm: ForgeJobs,
    warm_next: u64,
    warm_active: Option<(Request, Scope)>,
    warm_queue: VecDeque<Request>,
    warmed: Option<Scope>,
    idle_since: Option<Instant>,
    sweep: Instant,
    /// Row identities that changed in the last merge; highlighted for one repaint.
    pub(super) flash: HashSet<String>,
    prefetched: HashSet<Key>,
    rest: (usize, usize, Instant),
    /// The cursor moved to the row in `rest` from another row of the same list.
    moving: bool,
    /// `ensure` runs for a row the cursor is passing over: details are read from disk only.
    pub(super) holding: bool,
    /// The freshness chips last painted: their ages change with no reply to repaint them.
    chips: Vec<Option<String>>,
    remote: Option<Option<(String, String)>>,
    pub(super) live: Option<Live>,
    pub(super) live_key: Option<Key>,
    /// Values replaced by an optimistic write, restored if the forge refuses it.
    undo: Vec<(Key, Option<Value>)>,
    /// Gitea's and Forgejo's list check marks, asked for the rows on screen.
    pub(super) rows: super::row_checks::Rows,
    /// Disk reads for the rows the cursor passes over, off the UI thread.
    disk: super::disk::Disk,
    /// Until when this turn (a key, a poll) may wait for the disk (`load_disk_soon`).
    pub(super) disk_until: Option<Instant>,
    /// Keys the disk had nothing for, in scope `.0`: not read again each time `ensure`
    /// asks (the badges ask on every key), until the forge answers them.
    disk_absent: (u64, HashSet<Key>),
    /// The warm-up's keys still being read from disk: queued for the forge once the disk says
    /// whether it holds them, fresh.
    warm_disk: Vec<Request>,
}

impl Fresh {
    pub(super) fn new() -> Self {
        Self::with(Cache::shared())
    }

    pub(super) fn with(cache: Option<Arc<Cache>>) -> Self {
        Self {
            cache,
            meta: HashMap::new(),
            implicit: HashSet::new(),
            warm: ForgeJobs::warm(),
            warm_next: 0,
            warm_active: None,
            warm_queue: VecDeque::new(),
            warmed: None,
            idle_since: None,
            sweep: Instant::now(),
            flash: HashSet::new(),
            prefetched: HashSet::new(),
            rest: (0, usize::MAX, Instant::now()),
            moving: false,
            holding: false,
            chips: Vec::new(),
            remote: None,
            live: None,
            live_key: None,
            undo: Vec::new(),
            rows: Default::default(),
            disk: Default::default(),
            disk_until: None,
            disk_absent: Default::default(),
            warm_disk: Vec::new(),
        }
    }

    /// A background read for the warm worker, ahead of the startup warm-up.
    pub(super) fn push_warm(&mut self, request: Request) {
        self.warm_queue.push_front(request);
    }

    /// Whether the warm worker holds (queued or running) a read whose key matches.
    pub(super) fn warm_has(&self, matches: impl Fn(&Key) -> bool) -> bool {
        self.warm_active
            .as_ref()
            .is_some_and(|(request, _)| matches(&request.key))
            || self.warm_queue.iter().any(|request| matches(&request.key))
    }

    pub(super) fn enabled(&self) -> bool {
        self.cache.is_some()
    }

    /// A different repository: nothing known about the old one applies.
    pub(super) fn forget(&mut self) {
        self.meta.clear();
        self.implicit.clear();
        self.warm_queue.clear();
        self.prefetched.clear();
        self.flash.clear();
        self.live = None;
        self.live_key = None;
        self.undo.clear();
    }

    pub(super) fn warm_pending(&self, key: &Key) -> bool {
        self.warm_active
            .as_ref()
            .is_some_and(|(request, _)| &request.key == key)
    }

    pub(super) fn drop_warm(&mut self, key: &Key) {
        self.warm_queue.retain(|request| &request.key != key);
    }
}

fn kind_of(page: usize) -> ItemKind {
    if page == 1 {
        ItemKind::Issue
    } else {
        ItemKind::PullRequest
    }
}

/// A list's age column in one relative format from minutes to years (`3w ago`, `2mo ago`,
/// `1y ago`), so a list never mixes `2d ago` with `Nov 6` and `Aug 20 2025`.
pub(super) fn relative(stamp: &str, now: i64) -> String {
    let Some(at) = records::epoch(stamp) else {
        return String::new();
    };
    let seconds = (now - at).max(0);
    let days = seconds / 86_400;
    if seconds < 60 {
        "just now".into()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else if days < 14 {
        format!("{days}d ago")
    } else if days < 60 {
        format!("{}w ago", days / 7)
    } else if days < 365 {
        format!("{}mo ago", days / 30)
    } else {
        format!("{}y ago", days / 365)
    }
}

fn ago(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < 45 {
        "just now".into()
    } else if seconds < 3600 {
        format!("{}m ago", (seconds / 60).max(1))
    } else if seconds < 48 * 3600 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

/// Whether a list row (legacy shape) matches a GitHub-style query and state, as far as the
/// cached row can tell. Qualifiers the row cannot answer are left to the remote search.
pub(super) fn matches(row: &Value, query: &str, state: &str) -> bool {
    let row_state = string(row, "state");
    let draft = row["isDraft"].as_bool() == Some(true);
    let state_ok = match state {
        "open" => row_state == "OPEN",
        "closed" => matches!(row_state, "CLOSED" | "MERGED"),
        "merged" => row_state == "MERGED",
        _ => true,
    };
    if !state_ok {
        return false;
    }
    let title = string(row, "title").to_lowercase();
    let author = login(row).to_lowercase();
    let labels = array(row, "labels")
        .iter()
        .map(|label| string(label, "name").to_lowercase())
        .collect::<Vec<_>>();
    query.split_whitespace().all(|term| {
        let term = term.to_lowercase();
        let (negated, term) = match term.strip_prefix('-') {
            Some(rest) if rest.contains(':') => (true, rest.to_owned()),
            _ => (false, term),
        };
        let verdict = match term.split_once(':') {
            Some(("is" | "state", value)) => match value {
                "open" => row_state == "OPEN",
                "closed" => matches!(row_state, "CLOSED" | "MERGED"),
                "merged" => row_state == "MERGED",
                "draft" => draft,
                _ => true,
            },
            Some(("draft", value)) => draft == (value == "true"),
            Some(("author", value)) => author == value.trim_start_matches('@'),
            Some(("label", value)) => {
                let value = value.trim_matches('"');
                labels.iter().any(|label| label == value)
            }
            Some(("no", "label")) => labels.is_empty(),
            Some((_, _)) => return true,
            None => {
                let digits = term.trim_start_matches('#');
                title.contains(term.as_str())
                    || !digits.is_empty()
                        && digits.chars().all(|c| c.is_ascii_digit())
                        && number(row, "number").to_string() == digits
                    || author == term
            }
        };
        verdict != negated
    })
}

impl Workspace {
    /// The cache entry a read lands in: the target's host and repository, or the remote's.
    fn cache_key(&mut self, op: &Op) -> Option<CacheKey> {
        let Op::Read(target, read) = op else {
            return None;
        };
        let (host, repo) = match &target.host {
            Some(host) => (host.clone(), target.repo.clone()),
            None => {
                let remote = self.fresh.remote.get_or_insert_with(|| {
                    crate::forge::detect::choose_remote(&self.root)
                        .ok()
                        .map(|remote| (remote.host.to_string(), remote.repo.full()))
                });
                let (host, repo) = remote.clone()?;
                (host, Some(repo))
            }
        };
        Some(CacheKey::for_read(&host, repo.as_deref(), read))
    }

    /// Fill a memory miss from disk within this turn's budget (`load_disk_soon`). Returns
    /// whether the key is cached now: `false` while the disk is still being read, and the
    /// key's handling runs again when it lands (`poll_disk`).
    pub(super) fn load_disk(&mut self, key: &Key, op: &Op) -> bool {
        self.load_disk_soon(key, op).unwrap_or(false)
    }

    /// Fill a memory miss from disk on this thread, however long the disk takes: a live
    /// search, which asks the forge at once for whatever the disk lacks.
    /// Returns whether the key is now cached.
    pub(super) fn load_disk_now(&mut self, key: &Key, op: &Op) -> bool {
        let Some((read, cache_key, cache)) = self.disk_entry(key, op) else {
            return self.cache.contains_key(key);
        };
        let found = super::disk::read(&cache, key, &cache_key);
        self.apply_disk(key, &read, found)
    }

    /// `load_disk` on the disk worker: the entry is painted when it lands (`poll_disk`), and
    /// the key's handling (`ensure`) runs again then. `None` while the disk is being read,
    /// else whether the key is cached now.
    pub(super) fn load_disk_later(&mut self, key: &Key, op: &Op) -> Option<bool> {
        let Some((read, cache_key, cache)) = self.disk_entry(key, op) else {
            return Some(self.cache.contains_key(key));
        };
        self.fresh
            .disk
            .load(&cache, key.clone(), read, cache_key, self.scope_epoch);
        None
    }

    /// `load_disk` through the disk worker, waiting for it while this turn's budget
    /// (`DISK_BUDGET`) lasts: a quick disk paints at once as a direct read would, and a slow
    /// one never holds up a key (its entry paints when it lands, `poll_disk`). `None` while
    /// the disk is still being read, else whether the key is cached now.
    pub(super) fn load_disk_soon(&mut self, key: &Key, op: &Op) -> Option<bool> {
        let Some((read, cache_key, cache)) = self.disk_entry(key, op) else {
            return Some(self.cache.contains_key(key));
        };
        let epoch = self.scope_epoch;
        self.fresh
            .disk
            .load(&cache, key.clone(), read.clone(), cache_key.clone(), epoch);
        let until = *self
            .fresh
            .disk_until
            .get_or_insert_with(|| Instant::now() + DISK_BUDGET);
        let loaded = self.fresh.disk.wait(key, until)?;
        if loaded.epoch != epoch {
            // An answer for the scope before: ask again for this one.
            self.fresh
                .disk
                .load(&cache, key.clone(), read, cache_key, epoch);
            return None;
        }
        Some(self.apply_disk(&loaded.key, &loaded.read, loaded.found))
    }

    /// Whether the disk is being read for `key`.
    pub(super) fn disk_loading(&self, key: &Key) -> bool {
        self.fresh.disk.loading.contains(key)
    }

    /// Entries the disk worker read; returns whether any was painted.
    pub(super) fn poll_disk(&mut self) -> bool {
        let mut painted = false;
        let mut answered = false;
        for loaded in self.fresh.disk.replies() {
            answered = true;
            if loaded.epoch != self.scope_epoch || self.cache.contains_key(&loaded.key) {
                continue;
            }
            painted |= self.apply_disk(&loaded.key, &loaded.read, loaded.found);
        }
        // The rows' handling waited for the disk: the forge is asked now if it must be.
        if painted || answered && !self.cursor_moving() {
            self.ensure(false);
        }
        painted
    }

    /// What a disk read of `key` needs, when the memory misses it and the cache is on.
    fn disk_entry(&mut self, key: &Key, op: &Op) -> Option<(Read, CacheKey, Arc<Cache>)> {
        if !self.fresh.enabled() || self.cache.contains_key(key) {
            return None;
        }
        if self.fresh.disk_absent.0 != self.scope_epoch {
            self.fresh.disk_absent = (self.scope_epoch, HashSet::new());
        }
        if self.fresh.disk_absent.1.contains(key) {
            return None;
        }
        let Op::Read(_, read) = op else {
            return None;
        };
        if matches!(read, Read::Fingerprint(_)) {
            return None;
        }
        let cache_key = self.cache_key(op)?;
        let cache = self.fresh.cache.clone()?;
        Some((read.clone(), cache_key, cache))
    }

    /// Put what the disk held for `key` in memory. Returns whether the key is now cached.
    fn apply_disk(&mut self, key: &Key, read: &Read, found: super::disk::Found) -> bool {
        let envelope = match found {
            super::disk::Found::Payload(envelope) => envelope,
            super::disk::Found::Missing(envelope) => {
                // A "not found" the forge gave last time, shown until it is asked again.
                self.fresh.meta.insert(
                    key.clone(),
                    Meta {
                        fetched: envelope.fetched(),
                        checked: None,
                        ttl: Duration::from_secs(envelope.ttl),
                        etag: None,
                        fingerprint: None,
                        offline: false,
                        verified: false,
                    },
                );
                self.cache.insert(key.clone(), Err(envelope.data));
                return true;
            }
            super::disk::Found::Nothing => {
                if self.fresh.disk_absent.0 == self.scope_epoch {
                    self.fresh.disk_absent.1.insert(key.clone());
                }
                return false;
            }
        };
        self.note_totals(key, &envelope.data);
        let (mut value, text) = crate::forge::legacy::encode(shape(key), &envelope.data);
        if let Key::Inbox(host, page, state, _) = key
            && inbox::validate_rows(&mut value, host, *page, state).is_err()
        {
            return false;
        }
        if *key == Key::Repository {
            let host = reqwest::Url::parse(string(&value, "url"))
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default();
            let name = string(&value, "nameWithOwner").to_owned();
            if name.is_empty() {
                return false;
            }
            self.change_repository(name, host);
        }
        if let Read::List { page, .. } | Read::Search { page, .. } = read {
            self.list_limits.insert(key.clone(), page.per_page as usize);
        }
        self.fresh.meta.insert(
            key.clone(),
            Meta {
                fetched: envelope.fetched(),
                checked: None,
                ttl: Duration::from_secs(envelope.ttl),
                etag: envelope.etag.clone(),
                fingerprint: envelope.fingerprint.clone(),
                offline: false,
                verified: false,
            },
        );
        self.cache.insert(key.clone(), Ok(Reply { text, value }));
        true
    }

    /// Stale-while-revalidate: whether a cached entry should be asked about again.
    pub(super) fn needs_check(&self, key: &Key) -> bool {
        if !self.fresh.enabled() {
            return false;
        }
        let Some(meta) = self.fresh.meta.get(key) else {
            return false;
        };
        // The last check failed (offline, rate limited, a server error): retry with backoff, a
        // quarter of how old the shown data is, from 10 s up to 5 min.
        if meta.offline {
            let wait = (meta.age() / 4).clamp(CONFIRM_AFTER, Duration::from_secs(300));
            return meta.checked.is_none_or(|checked| checked.elapsed() >= wait);
        }
        if let Some(checked) = meta.checked
            && checked.elapsed() < meta.ttl.clamp(CONFIRM_AFTER, Duration::from_secs(3600))
        {
            return false;
        }
        meta.age() >= meta.ttl || meta.checked.is_none() && meta.age() >= CONFIRM_AFTER
    }

    /// Queue a background check of a cached entry; the cached value stays on screen.
    pub(super) fn revalidate(&mut self, key: Key, op: Op) {
        if self.pending(&key) || self.fresh.warm_pending(&key) {
            return;
        }
        self.fresh.drop_warm(&key);
        // An opened pull request is checked by its fingerprint first.
        let op = match (&key, op) {
            (Key::Item(2, number), Op::Read(target, Read::PullRequest(_)))
                if self
                    .fresh
                    .meta
                    .get(&key)
                    .is_some_and(|meta| meta.fingerprint.is_some()) =>
            {
                Op::Read(target, Read::Fingerprint(*number))
            }
            (_, op) => op,
        };
        // Asked now: a failing forge is not asked again before the entry's budget runs out.
        if let Some(meta) = self.fresh.meta.get_mut(&key) {
            meta.checked = Some(Instant::now());
        }
        self.fresh.implicit.insert(key.clone());
        self.queue.push_back(Request { key, op });
    }

    /// What the forge is told it may answer 304 to.
    pub(super) fn conditional(&self, key: &Key, read: &Read) -> Conditional {
        let meta = self.fresh.meta.get(key);
        match read {
            Read::Repository | Read::RepositoryNamed(_) if self.cache.contains_key(key) => {
                Conditional {
                    etag: meta.and_then(|meta| meta.etag.clone()),
                    ..Conditional::default()
                }
            }
            Read::Fingerprint(_) => Conditional {
                fingerprint: meta.and_then(|meta| meta.fingerprint.clone()),
                ..Conditional::default()
            },
            _ => Conditional::default(),
        }
    }

    fn confirmed(&mut self, key: &Key) {
        if let Some(meta) = self.fresh.meta.get_mut(key) {
            meta.fetched = SystemTime::now();
            meta.checked = Some(Instant::now());
            meta.offline = false;
            meta.verified = true;
        }
    }

    /// Apply one read's answer: the interactive worker's, or the warm worker's (`background`).
    pub(super) fn apply(
        &mut self,
        request: Request,
        fetched: Result<Fetched, String>,
        background: bool,
    ) {
        let implicit = self.fresh.implicit.remove(&request.key) || background;
        let read = match &request.op {
            Op::Read(_, read) => Some(read.clone()),
            Op::Write(..) => None,
        };
        let cached = self.cache.get(&request.key).is_some_and(Result::is_ok);
        let (etag, fingerprint, ttl) = match &fetched {
            Ok(Fetched::Payload {
                value,
                etag,
                fingerprint,
            }) => (
                etag.clone(),
                fingerprint.clone(),
                read.as_ref().map_or(Duration::from_secs(60), |read| {
                    cache::ttl(read, Some(value))
                }),
            ),
            _ => (None, None, Duration::from_secs(60)),
        };
        let parsed = match fetched {
            Ok(Fetched::Payload {
                value: Payload::Fingerprint(fingerprint),
                ..
            }) => {
                let same = self
                    .fresh
                    .meta
                    .get(&request.key)
                    .and_then(|meta| meta.fingerprint.as_ref())
                    == Some(&fingerprint);
                if same {
                    self.confirmed(&request.key);
                } else if let Key::Item(page, number) = request.key {
                    let read = if page == 1 {
                        Read::Issue(number)
                    } else {
                        Read::PullRequest(number)
                    };
                    self.fresh.implicit.insert(request.key.clone());
                    self.queue.push_front(Request {
                        key: request.key,
                        op: self.read(read),
                    });
                }
                self.ensure(false);
                return;
            }
            Ok(Fetched::NotModified) if cached => {
                self.confirmed(&request.key);
                self.ensure(false);
                return;
            }
            Ok(Fetched::NotModified) => Err("Unexpected response · not modified".into()),
            Ok(Fetched::Payload { value, .. }) => {
                self.note_totals(&request.key, &value);
                let (mut value, text) = crate::forge::legacy::encode(shape(&request.key), &value);
                match &request.key {
                    Key::Inbox(host, page, state, _) => {
                        inbox::validate_rows(&mut value, host, *page, state)
                            .map(|()| Reply { value, text })
                    }
                    _ => Ok(Reply { value, text }),
                }
            }
            Err(error) => Err(error),
        };
        if request.key == Key::Repository
            && let Ok(reply) = &parsed
        {
            let host = reqwest::Url::parse(string(&reply.value, "url"))
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default();
            self.change_repository(string(&reply.value, "nameWithOwner").into(), host);
        }
        if background && parsed.is_err() && !cached {
            // A failed warm-up leaves the key to the interactive worker.
            return;
        }
        if parsed.is_ok()
            && let Some(limit) = request.list_limit()
        {
            self.list_limits.insert(request.key.clone(), limit);
        }
        let selected_item = if request.key == self.list_key() {
            self.items()
                .get(self.selected[self.page.saturating_sub(1).min(1)])
                .map(|item| self.row_identity(item))
                .or_else(|| {
                    self.list_selections
                        .get(&request.key)
                        .map(|(id, _)| id.clone())
                })
        } else {
            None
        };
        let selected_run = if request.key == Key::Runs(self.workflow_id()) {
            self.runs()
                .get(self.run_selected)
                .map(|run| number(run, "databaseId"))
        } else {
            None
        };
        match parsed {
            Err(error) if cached => {
                if implicit {
                    let meta = self.fresh.meta.get_mut(&request.key);
                    if let Some(meta) = meta {
                        meta.offline = true;
                        meta.checked = Some(Instant::now());
                    }
                } else if !matches!(request.key, Key::List(..) | Key::Inbox(..))
                    || matches!(self.page, 1 | 2) && request.key == self.list_key()
                {
                    // In words, with how old what stays on screen is.
                    let age = self.fresh.meta.get(&request.key).map(Meta::age);
                    self.notice = self.stale_notice(&error, age);
                    self.notice_error = true;
                }
            }
            Ok(mut reply) => {
                let unchanged = cached && self.value(&request.key) == Some(&reply.value);
                if !unchanged {
                    if implicit
                        && cached
                        && matches!(self.page, 1 | 2)
                        && request.key == self.list_key()
                    {
                        reply = self.merge_list(&request.key, reply);
                    }
                    self.cache.insert(request.key.clone(), Ok(reply));
                }
                // The worker wrote it to disk too.
                self.fresh.disk_absent.1.remove(&request.key);
                self.fresh.meta.insert(
                    request.key.clone(),
                    Meta {
                        fetched: SystemTime::now(),
                        checked: Some(Instant::now()),
                        ttl,
                        etag,
                        fingerprint,
                        offline: false,
                        verified: true,
                    },
                );
            }
            Err(error) => {
                self.cache.insert(request.key.clone(), Err(error));
            }
        }
        if let Some(id) = selected_item {
            let index = self.page.saturating_sub(1).min(1);
            self.selected[index] = self
                .items()
                .iter()
                .position(|item| self.row_identity(item) == id)
                .unwrap_or_else(|| self.selected[index].min(self.items().len().saturating_sub(1)));
            self.remember_list_selection();
        }
        if let Some(id) = selected_run
            && let Some(index) = self
                .runs()
                .iter()
                .position(|run| number(run, "databaseId") == id)
        {
            self.run_selected = index;
        }
        self.ensure(false);
    }

    /// The merge rules for a background list refresh.
    fn merge_list(&mut self, key: &Key, reply: Reply) -> Reply {
        let old = self
            .value(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let Some(fresh) = reply.value.as_array() else {
            return reply;
        };
        let cursor = old
            .get(self.selected[self.page.saturating_sub(1).min(1)])
            .map(|item| self.row_identity(item));
        let word = if self.states[self.page.saturating_sub(1).min(1)] == "open" {
            "closed"
        } else {
            "gone"
        };
        let merged = merge::rows(
            &old,
            fresh,
            |row| self.row_identity(row),
            cursor.as_deref(),
            0,
            true,
            |row| {
                let mut row = row.clone();
                if let Some(object) = row.as_object_mut() {
                    object
                        .entry("dustyGone")
                        .or_insert_with(|| Value::from(word));
                }
                row
            },
        );
        self.fresh.flash.extend(merged.changed);
        self.fresh.flash.extend(merged.inserted);
        let value = Value::Array(merged.rows);
        Reply {
            text: value.to_string(),
            value,
        }
    }

    /// Rows kept only to avoid a jump disappear once the reader leaves the list.
    pub(super) fn drop_ghosts(&mut self) {
        for reply in self.cache.values_mut().flatten() {
            if let Some(rows) = reply.value.as_array_mut()
                && rows.iter().any(|row| row.get("dustyGone").is_some())
            {
                rows.retain(|row| row.get("dustyGone").is_none());
                reply.text = reply.value.to_string();
            }
        }
    }

    /// The only freshness UI: `updated 3m ago · ⟳`.
    pub(super) fn freshness_chip(&self, key: &Key, compact: bool) -> Option<String> {
        if !self.fresh.enabled() || !self.cache.get(key).is_some_and(Result::is_ok) {
            return None;
        }
        let meta = self.fresh.meta.get(key)?;
        let busy = self.pending(key) || self.fresh.warm_pending(key);
        let age = ago(meta.age());
        // One string at every width: a border too narrow for it drops the label whole
        // rather than shortening `updated just now` to `now`.
        if compact {
            return None;
        }
        Some(if meta.offline {
            format!("updated {age} · {}", self.stale_word())
        } else {
            format!("updated {age}{}", if busy { " · ⟳" } else { "" })
        })
    }

    /// The list read for a page, state and query (the warm worker and live search use it).
    fn list_read_at(&self, page: usize, state: &str, query: &str, limit: usize) -> Op {
        let filter = Filter {
            state: StateFilter::parse(state),
            query: query.into(),
        };
        let page_of = Page::first(limit.min(LIST_CEILING) as u32);
        if self.inbox.enabled {
            return Op::Read(
                ForgeTarget::host(&self.host),
                Read::Search {
                    kind: kind_of(page),
                    filter,
                    page: page_of,
                },
            );
        }
        self.read(Read::List {
            kind: kind_of(page),
            filter,
            page: page_of,
        })
    }

    fn list_key_at(&self, page: usize, state: &str, query: &str) -> Key {
        if self.inbox.enabled {
            Key::Inbox(self.host.clone(), page, state.into(), query.into())
        } else {
            Key::List(page, state.into(), query.into())
        }
    }

    /// Whether `j`/`k` are moving the list cursor: it reached its row less than `DWELL` ago
    /// from another row of the same list. Records where the cursor rests.
    pub(super) fn cursor_moving(&mut self) -> bool {
        if !self.fresh.enabled()
            || !matches!(self.page, 1 | 2)
            || self.inbox.enabled
            || self.opened.is_some()
        {
            return false;
        }
        let selected = self.selected[self.page - 1];
        let (page, row, _) = self.fresh.rest;
        if (page, row) != (self.page, selected) {
            // The row a page first shows is not a move.
            self.fresh.moving = page == self.page && row != usize::MAX;
            self.fresh.rest = (self.page, selected, Instant::now());
        }
        self.fresh.moving && self.fresh.rest.2.elapsed() < DWELL
    }

    /// The warm worker, prefetching and the revalidation sweep; called from `poll`.
    pub(super) fn poll_fresh(&mut self) -> bool {
        if !self.fresh.enabled() {
            return false;
        }
        let mut changed = self.poll_disk();
        while let Some(event) = self.fresh.warm.try_reply() {
            if event.request_id != self.fresh.warm_next {
                continue;
            }
            let Some((request, scope)) = self.fresh.warm_active.take() else {
                continue;
            };
            if scope != (self.host.clone(), self.repo.clone(), self.scope_epoch) {
                continue;
            }
            let ForgeReply::Fetched(_, result) = event.reply else {
                continue;
            };
            changed = true;
            let key = request.key.clone();
            self.apply(request, result.map_err(|error| error.to_string()), true);
            self.ask_again(&key);
        }
        changed |= self.poll_live();
        if self.repo.is_empty() {
            return changed;
        }
        let scope = (self.host.clone(), self.repo.clone(), self.scope_epoch);
        if self.fresh.warmed.as_ref() != Some(&scope) {
            self.fresh.warmed = Some(scope.clone());
            self.fresh.warm_queue.clear();
            // Startup: everything a first keypress can show.
            let mut warm = vec![
                (
                    Key::List(1, "open".into(), String::new()),
                    self.read(Read::List {
                        kind: ItemKind::Issue,
                        filter: Filter::default(),
                        page: Page::first(LIST_BATCH as u32),
                    }),
                ),
                (
                    Key::List(2, "open".into(), String::new()),
                    self.read(Read::List {
                        kind: ItemKind::PullRequest,
                        filter: Filter::default(),
                        page: Page::first(LIST_BATCH as u32),
                    }),
                ),
                (Key::Workflows, self.read(Read::Workflows)),
                (Key::Runs(0), self.runs_read(0)),
            ];
            if self.inbox.enabled {
                warm.clear();
            }
            // Read by the disk worker within this poll's budget: a slow disk never holds up
            // the first keys.
            self.fresh.warm_disk.clear();
            for (key, op) in warm {
                match self.load_disk_soon(&key, &op) {
                    Some(cached) => {
                        if !cached || self.needs_check(&key) {
                            self.fresh.warm_queue.push_back(Request { key, op });
                        }
                    }
                    None => self.fresh.warm_disk.push(Request { key, op }),
                }
            }
            changed = true;
        }
        if !self.fresh.warm_disk.is_empty() {
            for request in std::mem::take(&mut self.fresh.warm_disk) {
                if self.disk_loading(&request.key) {
                    self.fresh.warm_disk.push(request);
                } else if !self.cache.contains_key(&request.key) || self.needs_check(&request.key) {
                    self.fresh.warm_queue.push_back(request);
                }
            }
        }
        // After the warm-up is queued (a new scope clears the queue): the list's check marks.
        changed |= self.poll_row_checks();
        // Prefetch the neighbours of a resting cursor.
        if matches!(self.page, 1 | 2) && !self.inbox.enabled && self.opened.is_none() {
            let at = self.page - 1;
            let selected = self.selected[at];
            if !self.cursor_moving() && self.fresh.moving {
                // The cursor dwelt on its row: ask the forge for what it shows.
                self.fresh.moving = false;
                self.ensure(false);
                changed = true;
            }
            if self.fresh.rest.2.elapsed() >= PREFETCH_REST {
                let neighbours = [selected + 1, selected.wrapping_sub(1)]
                    .into_iter()
                    .filter_map(|index| self.items().get(index).map(|item| number(item, "number")))
                    .collect::<Vec<_>>();
                for id in neighbours {
                    let key = Key::Item(self.page, id);
                    if self.fresh.prefetched.contains(&key)
                        || self.pending(&key)
                        || self.disk_loading(&key)
                    {
                        continue;
                    }
                    let op = self.read(if self.page == 1 {
                        Read::Issue(id)
                    } else {
                        Read::PullRequest(id)
                    });
                    // A slow disk is not waited for past the poll's budget: asked again once
                    // its answer has landed.
                    let Some(cached) = self.load_disk_soon(&key, &op) else {
                        continue;
                    };
                    self.fresh.prefetched.insert(key.clone());
                    changed |= cached;
                    // Signed out, one item is six REST reads of a 60-an-hour quota: neighbours
                    // come from disk only, and the forge is asked when one is opened.
                    if !self.read_only()
                        && (!self.cache.contains_key(&key) || self.needs_check(&key))
                    {
                        self.fresh.warm_queue.push_back(Request { key, op });
                    }
                }
            }
        }
        // The chips' ages (`updated just now` → `updated 1m ago`) repaint as they change.
        let chips = [self.list_key(), Key::Item(self.page, self.number())]
            .iter()
            .map(|key| self.freshness_chip(key, false))
            .collect::<Vec<_>>();
        if chips != self.fresh.chips {
            self.fresh.chips = chips;
            changed = true;
        }
        // Revalidate what is on screen as its budget runs out (and poll fingerprints).
        let idle = self.active.idle() && self.queue.is_empty();
        if idle && self.fresh.sweep.elapsed() >= SWEEP {
            self.fresh.sweep = Instant::now();
            self.ensure(false);
        }
        let idle = self.active.idle() && self.queue.is_empty();
        let since = if idle {
            *self.fresh.idle_since.get_or_insert_with(Instant::now)
        } else {
            self.fresh.idle_since = None;
            Instant::now()
        };
        if self.fresh.warm_active.is_none()
            && !self.fresh.warm.busy()
            && since.elapsed() >= WARM_IDLE
            && super::super::jobs::running() == 0
            && std::env::var("DUSTY_FORGE_WARM").as_deref() != Ok("0")
        {
            while let Some(request) = self.fresh.warm_queue.pop_front() {
                if self.pending(&request.key)
                    || self.cache.get(&request.key).is_some_and(Result::is_ok)
                        && !self.needs_check(&request.key)
                {
                    continue;
                }
                let Op::Read(target, read) = &request.op else {
                    continue;
                };
                self.fresh.warm_next += 1;
                let conditional = self.conditional(&request.key, read);
                if self
                    .fresh
                    .warm
                    .fetch(
                        self.root.clone(),
                        self.fresh.warm_next,
                        target.clone(),
                        read.clone(),
                        conditional,
                    )
                    .is_ok()
                {
                    if let Some(meta) = self.fresh.meta.get_mut(&request.key) {
                        meta.checked = Some(Instant::now());
                    }
                    self.fresh.warm_active = Some((request, scope));
                    changed = true;
                }
                break;
            }
        }
        changed
    }

    /// The filter form's text as the user types (`None` once it closes).
    pub(super) fn live_input(&mut self, input: Option<(String, String)>) {
        let Some((query, state)) = input else {
            if self.fresh.live.take().is_some() {
                self.fresh.live_key = None;
            }
            return;
        };
        if !self.fresh.enabled() || !matches!(self.page, 1 | 2) || self.inbox.enabled {
            return;
        }
        match &mut self.fresh.live {
            Some(live) if live.page == self.page => {
                if live.query != query || live.state != state {
                    live.query = query;
                    live.state = state;
                    live.changed = Instant::now();
                    live.sent = false;
                }
            }
            _ => {
                let unchanged = {
                    let at = self.page - 1;
                    self.queries[at] == query && self.states[at] == state
                };
                self.fresh.live = Some(Live {
                    page: self.page,
                    query,
                    state,
                    changed: Instant::now(),
                    sent: unchanged,
                    corpus: None,
                });
            }
        }
    }

    /// Debounced remote search for the live query; a newer query cancels an older one.
    fn poll_live(&mut self) -> bool {
        let Some(live) = &self.fresh.live else {
            return false;
        };
        if live.sent || live.changed.elapsed() < DEBOUNCE || live.page != self.page {
            return false;
        }
        let (page, query, state) = (live.page, live.query.clone(), live.state.clone());
        if let Some(live) = &mut self.fresh.live {
            live.sent = true;
        }
        let key = self.list_key_at(page, &state, &query);
        if let Some(previous) = self.fresh.live_key.take()
            && previous != key
            && previous != self.list_key()
        {
            // A live search the user typed past: its answer is dropped, not news.
            self.active.cancel(&previous);
            self.fresh.implicit.remove(&previous);
            self.queue.retain(|request| request.key != previous);
        }
        let op = self.list_read_at(page, &state, &query, LIST_BATCH);
        self.fresh.live_key = Some(key.clone());
        let cached = self.load_disk_now(&key, &op);
        if (!cached || self.needs_check(&key)) && !self.pending(&key) {
            if cached {
                self.fresh.implicit.insert(key.clone());
            }
            self.queue.push_front(Request { key, op });
        }
        true
    }

    fn corpus(&self, page: usize) -> Vec<Value> {
        let mut rows = BTreeMap::<u64, Value>::new();
        let mut add = |row: &Value| {
            let id = number(row, "number");
            if id == 0 || row.get("dustyGone").is_some() {
                return;
            }
            let newer = rows
                .get(&id)
                .is_none_or(|old| string(old, "updatedAt") < string(row, "updatedAt"));
            if newer {
                rows.insert(id, row.clone());
            }
        };
        for (key, reply) in &self.cache {
            if let (Key::List(at, ..), Ok(reply)) = (key, reply)
                && *at == page
            {
                reply
                    .value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .for_each(&mut add);
            }
        }
        if let Some(cache) = &self.fresh.cache {
            let kind = kind_of(page);
            for envelope in cache.scan::<Payload>(&self.host, Some(&self.repo), "list") {
                let listed = serde_json::from_str::<Read>(&envelope.key).is_ok_and(
                    |read| matches!(read, Read::List { kind: listed, .. } if listed == kind),
                );
                if listed {
                    let (value, _) = crate::forge::legacy::encode(
                        crate::forge::legacy::Shape::List(kind),
                        &envelope.data,
                    );
                    value.as_array().into_iter().flatten().for_each(&mut add);
                }
            }
        }
        rows.into_values().rev().collect()
    }

    /// Local hits for the live query, then the remote results below a divider.
    pub(super) fn live_rows(&mut self) -> Option<LiveRows> {
        let live = self.fresh.live.as_ref()?;
        if live.page != self.page {
            return None;
        }
        if live.corpus.is_none() {
            let corpus = self.corpus(self.page);
            if let Some(live) = &mut self.fresh.live {
                live.corpus = Some(corpus);
            }
        }
        let live = self.fresh.live.as_ref()?;
        let local = live
            .corpus
            .iter()
            .flatten()
            .filter(|row| matches(row, &live.query, &live.state))
            .take(LOCAL_HITS)
            .cloned()
            .collect::<Vec<_>>();
        let key = self.list_key_at(live.page, &live.state, &live.query);
        let seen = local
            .iter()
            .map(|row| number(row, "number"))
            .collect::<HashSet<_>>();
        let remote = self
            .value(&key)
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter(|row| !seen.contains(&number(row, "number")))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let host = if self.host.is_empty() {
            "GitHub"
        } else {
            self.host.as_str()
        };
        let divider = if !live.sent || self.pending(&key) {
            format!("From {host} · searching ⟳")
        } else {
            match self.cache.get(&key) {
                // Nothing new from the forge: no divider for an empty section.
                Some(Ok(_)) if remote.is_empty() => String::new(),
                Some(Ok(_)) => format!("From {host} · {} more", remote.len()),
                Some(Err(_)) => format!("From {host} · offline"),
                None => format!("From {host} · searching ⟳"),
            }
        };
        Some(LiveRows {
            local,
            remote,
            divider,
        })
    }

    /// Show a write before the forge confirms it; `settle` keeps or undoes it.
    pub(super) fn optimistic(&mut self, write: &Write) {
        self.fresh.undo.clear();
        if !self.fresh.enabled() {
            return;
        }
        let page_of = |kind: &ItemKind| if *kind == ItemKind::Issue { 1 } else { 2 };
        let mut edits: Vec<(Key, Edit)> = Vec::new();
        match write {
            Write::Comment { number, body, .. } => {
                let body = body.clone();
                edits.push((
                    Key::Timeline(*number),
                    Box::new(move |timeline| {
                        let event = serde_json::json!({
                            "event": "commented",
                            "user": {"login": "you"},
                            "body": body,
                            "created_at": "sending…",
                            "dustyPending": true,
                        });
                        match timeline.as_array_mut() {
                            Some(pages) if !pages.is_empty() => {
                                if let Some(last) = pages.last_mut().and_then(Value::as_array_mut) {
                                    last.push(event);
                                }
                            }
                            _ => *timeline = serde_json::json!([[event]]),
                        }
                    }),
                ));
            }
            Write::SetState {
                kind,
                number: id,
                open,
                ..
            } => {
                let word = if *open { "OPEN" } else { "CLOSED" };
                let page = page_of(kind);
                edits.push((
                    Key::Item(page, *id),
                    Box::new(move |item| item["state"] = word.into()),
                ));
                let id = *id;
                edits.push((
                    self.list_key_at(page, &self.states[page - 1], &self.queries[page - 1]),
                    Box::new(move |rows| {
                        for row in rows.as_array_mut().into_iter().flatten() {
                            if number(row, "number") == id {
                                row["state"] = word.into();
                            }
                        }
                    }),
                ));
            }
            Write::SetLabels {
                kind,
                number,
                add,
                remove,
            } => {
                let (add, remove) = (add.clone(), remove.clone());
                let relabel = move |item: &mut Value| {
                    let mut labels = array(item, "labels")
                        .iter()
                        .filter(|label| !remove.iter().any(|name| name == string(label, "name")))
                        .cloned()
                        .collect::<Vec<_>>();
                    for name in &add {
                        if !labels.iter().any(|label| string(label, "name") == name) {
                            labels.push(serde_json::json!({"name": name}));
                        }
                    }
                    item["labels"] = Value::Array(labels);
                };
                edits.push((Key::Item(page_of(kind), *number), Box::new(relabel)));
            }
            Write::Merge { number, .. } => {
                edits.push((
                    Key::Item(2, *number),
                    Box::new(|item| item["state"] = "MERGED".into()),
                ));
            }
            _ => {}
        }
        for (key, edit) in edits {
            let Some(Ok(reply)) = self.cache.get_mut(&key) else {
                continue;
            };
            self.fresh
                .undo
                .push((key.clone(), Some(reply.value.clone())));
            edit(&mut reply.value);
            reply.text = reply.value.to_string();
            // A read of it already on its way answers for the time before the write: dropped,
            // so it cannot paint over the edit; the write's own refresh reads it again.
            self.active.cancel(&key);
        }
    }

    /// The forge answered the write: keep the optimistic values (a refresh follows) or put the
    /// cached ones back.
    pub(super) fn settle(&mut self, confirmed: bool) {
        let undo = std::mem::take(&mut self.fresh.undo);
        if confirmed {
            return;
        }
        for (key, value) in undo.into_iter().rev() {
            match value {
                Some(value) => {
                    self.cache.insert(
                        key,
                        Ok(Reply {
                            text: value.to_string(),
                            value,
                        }),
                    );
                }
                None => {
                    self.cache.remove(&key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_search_understands_common_qualifiers() {
        let row = json!({"number": 12, "title": "Crash on resize", "state": "OPEN",
            "author": {"login": "ana"}, "labels": [{"name": "bug"}]});
        assert!(matches(&row, "crash", "open"));
        assert!(matches(&row, "author:ana label:bug", "all"));
        assert!(!matches(&row, "-label:bug", "all"));
        assert!(matches(&row, "#12", "open"));
        assert!(!matches(&row, "crash", "closed"));
        assert!(matches(&row, "milestone:v2 resize", "open"));
        assert!(!matches(&row, "scroll", "open"));
    }

    #[test]
    fn ages_read_like_the_chip() {
        assert_eq!(ago(Duration::from_secs(5)), "just now");
        assert_eq!(ago(Duration::from_secs(180)), "3m ago");
        assert_eq!(ago(Duration::from_secs(7200)), "2h ago");
    }
}
