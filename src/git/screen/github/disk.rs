//! Disk cache reads off the UI thread (FORGE.md §7.2): while `j`/`k` pass over rows, the
//! details they show are read from disk by a worker and painted when they land, so a slow disk
//! (a cold page cache, a busy machine) never holds up a key.

use super::Key;
use crate::forge::cache::{Cache, CacheKey, Envelope};
use crate::forge::request::Payload;
use crate::forge::request::Read;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

/// What the disk holds for a key.
pub(super) enum Found {
    Payload(Envelope<Payload>),
    /// A "not found" the forge gave last time.
    Missing(Envelope<String>),
    Nothing,
}

/// Read `key`'s entry, or its "not found" marker where one is kept.
pub(super) fn read(cache: &Cache, key: &Key, cache_key: &CacheKey) -> Found {
    if let Some(envelope) = cache.get::<Payload>(cache_key) {
        return Found::Payload(envelope);
    }
    // A 404 for the repository, a list or the inbox means the scope is wrong: asked again.
    if matches!(key, Key::Repository | Key::List(..) | Key::Inbox(..) | Key::RowChecks(_)) {
        return Found::Nothing;
    }
    match cache.get::<String>(&cache_key.missing()) {
        Some(envelope) => Found::Missing(envelope),
        None => Found::Nothing,
    }
}

pub(super) struct Loaded {
    pub key: Key,
    pub read: Read,
    pub epoch: u64,
    pub found: Found,
}

type Job = (Key, Read, CacheKey, u64);

#[derive(Default)]
pub(super) struct Disk {
    jobs: Option<Sender<Job>>,
    replies: Option<Receiver<Loaded>>,
    /// Keys asked of the disk and not answered yet.
    pub(super) loading: HashSet<Key>,
    /// Answers that landed while `wait` waited for another key.
    early: Vec<Loaded>,
}

impl Disk {
    /// Ask the worker for `key`'s entry; the answer comes back through `replies`.
    pub(super) fn load(&mut self, cache: &Arc<Cache>, key: Key, what: Read, cache_key: CacheKey, epoch: u64) {
        if self.loading.contains(&key) {
            return;
        }
        if self.jobs.is_none() {
            let (jobs, inbox) = mpsc::channel::<Job>();
            let (sender, replies) = mpsc::channel();
            let cache = cache.clone();
            let spawned = std::thread::Builder::new()
                .name("dust-forge-disk".into())
                .spawn(move || {
                    while let Ok((key, asked, cache_key, epoch)) = inbox.recv() {
                        let found = read(&cache, &key, &cache_key);
                        let answer = Loaded { key, read: asked, epoch, found };
                        if sender.send(answer).is_err() {
                            break;
                        }
                    }
                });
            if spawned.is_err() {
                return;
            }
            self.jobs = Some(jobs);
            self.replies = Some(replies);
        }
        if let Some(jobs) = &self.jobs
            && jobs.send((key.clone(), what, cache_key, epoch)).is_ok()
        {
            self.loading.insert(key);
        }
    }

    /// `key`'s answer, if it lands before `until`. Others that land meanwhile are kept for
    /// `replies`.
    pub(super) fn wait(&mut self, key: &Key, until: Instant) -> Option<Loaded> {
        let replies = self.replies.as_ref()?;
        loop {
            let left = until.checked_duration_since(Instant::now())?;
            let answer = replies.recv_timeout(left).ok()?;
            self.loading.remove(&answer.key);
            if answer.key == *key {
                return Some(answer);
            }
            self.early.push(answer);
        }
    }

    /// Answers that landed since the last call.
    pub(super) fn replies(&mut self) -> Vec<Loaded> {
        let Some(replies) = &self.replies else {
            return Vec::new();
        };
        let mut loaded = std::mem::take(&mut self.early);
        let landed = loaded.len();
        loaded.extend(replies.try_iter());
        for answer in &loaded[landed..] {
            self.loading.remove(&answer.key);
        }
        loaded
    }
}
