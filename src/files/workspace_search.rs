use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use thiserror::Error;

use super::recovery::read_bounded_bytes;

pub(super) const ABSOLUTE_WORKSPACE_SEARCH_FILES: usize = 1_000_000;
pub(super) const ABSOLUTE_WORKSPACE_SEARCH_RESULTS: usize = 100_000;
pub(super) const ABSOLUTE_WORKSPACE_SEARCH_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub(super) const ABSOLUTE_WORKSPACE_SEARCH_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSearchPolicy {
    pub roots: Vec<PathBuf>,
    pub query: String,
    pub regex: bool,
    pub smart_case: bool,
    pub whole_word: bool,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub maximum_files: usize,
    pub maximum_results: usize,
    pub maximum_file_bytes: u64,
    pub maximum_total_bytes: u64,
    pub maximum_preview_bytes: usize,
    pub batch_size: usize,
    pub event_queue_capacity: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSearchMatch {
    pub id: String,
    pub canonical_path: PathBuf,
    pub relative_path: PathBuf,
    pub root: PathBuf,
    pub range: std::ops::Range<usize>,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub line_text: String,
    pub snapshot: Arc<[u8]>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceSearchSummary {
    pub files_considered: usize,
    pub files_searched: usize,
    pub bytes_searched: u64,
    pub skipped_binary: usize,
    pub skipped_too_large: usize,
    pub walk_errors: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceSearchEvent {
    Batch {
        generation: u64,
        matches: Vec<WorkspaceSearchMatch>,
    },
    Complete {
        generation: u64,
        summary: WorkspaceSearchSummary,
    },
    Cancelled {
        generation: u64,
    },
    Failed {
        generation: u64,
        error: String,
    },
}

#[derive(Default)]
pub struct WorkspaceSearchService {
    generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<WorkspaceSearchEvent>>,
}

impl WorkspaceSearchService {
    #[cfg(test)]
    pub(crate) fn events_for_test(events: Vec<WorkspaceSearchEvent>) -> Self {
        assert!(events.len() <= 256);
        let (sender, receiver) = mpsc::sync_channel(events.len().max(1));
        for event in events {
            sender.send(event).unwrap();
        }
        Self {
            generation: 1,
            cancellation: None,
            receiver: Some(receiver),
        }
    }
    pub fn start(&mut self, policy: WorkspaceSearchPolicy) -> Result<u64, WorkspaceSearchError> {
        validate_workspace_search_policy(&policy)?;
        let generation =
            self.generation
                .checked_add(1)
                .ok_or(WorkspaceSearchError::UnsafePolicy(
                    "request identity exhausted",
                ))?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::sync_channel(policy.event_queue_capacity);
        thread::Builder::new()
            .name(format!("dust-workspace-search-{generation}"))
            .spawn(move || run_workspace_search(generation, policy, worker_cancellation, sender))
            .map_err(WorkspaceSearchError::Spawn)?;
        self.cancel();
        self.generation = generation;
        self.cancellation = Some(cancellation);
        self.receiver = Some(receiver);
        Ok(generation)
    }

    pub fn cancel(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.store(true, Ordering::Release);
        }
        self.receiver = None;
    }

    pub fn try_event(&self) -> Option<WorkspaceSearchEvent> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    }

    pub(crate) fn poll_event(&self) -> Result<Option<WorkspaceSearchEvent>, &'static str> {
        match self.receiver.as_ref().map(|receiver| receiver.try_recv()) {
            Some(Ok(event)) => Ok(Some(event)),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err("workspace search worker stopped before completion")
            }
            _ => Ok(None),
        }
    }
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for WorkspaceSearchService {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceSearchError {
    #[error("workspace search requires a non-empty query")]
    EmptyQuery,
    #[error("workspace search requires at least one root")]
    NoRoots,
    #[error("workspace search policy exceeds a safety ceiling: {0}")]
    UnsafePolicy(&'static str),
    #[error("failed to start workspace search worker: {0}")]
    Spawn(io::Error),
}

pub(super) fn validate_workspace_search_policy(
    policy: &WorkspaceSearchPolicy,
) -> Result<(), WorkspaceSearchError> {
    if policy.query.is_empty() {
        return Err(WorkspaceSearchError::EmptyQuery);
    }
    if policy.query.len() > 65_536 {
        return Err(WorkspaceSearchError::UnsafePolicy("query length"));
    }
    if policy.roots.is_empty() {
        return Err(WorkspaceSearchError::NoRoots);
    }
    if policy.maximum_files == 0 || policy.maximum_files > ABSOLUTE_WORKSPACE_SEARCH_FILES {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_files"));
    }
    if policy.maximum_results == 0 || policy.maximum_results > ABSOLUTE_WORKSPACE_SEARCH_RESULTS {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_results"));
    }
    if policy.maximum_file_bytes == 0
        || policy.maximum_file_bytes > ABSOLUTE_WORKSPACE_SEARCH_FILE_BYTES
    {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_file_bytes"));
    }
    if policy.maximum_total_bytes == 0
        || policy.maximum_total_bytes > ABSOLUTE_WORKSPACE_SEARCH_TOTAL_BYTES
    {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_total_bytes"));
    }
    if !(1..=65_536).contains(&policy.maximum_preview_bytes) {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_preview_bytes"));
    }
    if !(1..=4_096).contains(&policy.batch_size) {
        return Err(WorkspaceSearchError::UnsafePolicy("batch_size"));
    }
    if !(1..=1_024).contains(&policy.event_queue_capacity) {
        return Err(WorkspaceSearchError::UnsafePolicy("event_queue_capacity"));
    }
    Ok(())
}

pub(super) fn compile_globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| error.to_string())?);
    }
    builder.build().map_err(|error| error.to_string())
}

pub(super) fn compile_workspace_pattern(policy: &WorkspaceSearchPolicy) -> Result<Regex, String> {
    let pattern = if policy.regex {
        policy.query.clone()
    } else {
        regex::escape(&policy.query)
    };
    let case_insensitive = policy.smart_case && !policy.query.chars().any(char::is_uppercase);
    RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .unicode(true)
        .build()
        .map_err(|error| error.to_string())
}

pub(super) fn send_search_event(
    sender: &SyncSender<WorkspaceSearchEvent>,
    cancellation: &AtomicBool,
    mut event: WorkspaceSearchEvent,
) -> bool {
    loop {
        if cancellation.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                thread::yield_now();
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

pub(super) fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

pub(super) fn has_word_boundaries(source: &str, start: usize, end: usize) -> bool {
    let before = source[..start].chars().next_back();
    let after = source[end..].chars().next();
    before.is_none_or(|character| !is_word_character(character))
        && after.is_none_or(|character| !is_word_character(character))
}

pub(super) fn bounded_line_preview(
    source: &str,
    start: usize,
    end: usize,
    maximum_bytes: usize,
) -> String {
    let mut preview_end = end.min(start.saturating_add(maximum_bytes));
    while preview_end > start && !source.is_char_boundary(preview_end) {
        preview_end = preview_end.saturating_sub(1);
    }
    source[start..preview_end].to_owned()
}

pub(super) fn owner_root<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
}

pub(super) fn run_workspace_search(
    generation: u64,
    policy: WorkspaceSearchPolicy,
    cancellation: Arc<AtomicBool>,
    sender: SyncSender<WorkspaceSearchEvent>,
) {
    let send_failure = |error: String| {
        let _ = send_search_event(
            &sender,
            &cancellation,
            WorkspaceSearchEvent::Failed { generation, error },
        );
    };
    let pattern = match compile_workspace_pattern(&policy) {
        Ok(pattern) => pattern,
        Err(error) => {
            send_failure(error);
            return;
        }
    };
    let include = match compile_globs(&policy.include_globs) {
        Ok(globs) => globs,
        Err(error) => {
            send_failure(error);
            return;
        }
    };
    let exclude = match compile_globs(&policy.exclude_globs) {
        Ok(globs) => globs,
        Err(error) => {
            send_failure(error);
            return;
        }
    };
    let mut roots = policy.roots.clone();
    roots.sort();
    roots.dedup();
    let mut builder = WalkBuilder::new(&roots[0]);
    for root in roots.iter().skip(1) {
        builder.add(root);
    }
    for worktree in roots.iter().flat_map(|root| super::directory::root_worktrees(root)) {
        builder.add(worktree);
    }
    builder
        .follow_links(false)
        .hidden(!policy.include_hidden)
        .ignore(!policy.include_ignored)
        .git_ignore(!policy.include_ignored)
        .git_global(!policy.include_ignored)
        .git_exclude(!policy.include_ignored)
        .parents(!policy.include_ignored);
    let mut summary = WorkspaceSearchSummary::default();
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for entry in builder.build() {
        if cancellation.load(Ordering::Acquire) {
            let _ = send_search_event(
                &sender,
                &cancellation,
                WorkspaceSearchEvent::Cancelled { generation },
            );
            return;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        summary.files_considered = summary.files_considered.saturating_add(1);
        if summary.files_considered > policy.maximum_files {
            summary.truncated = true;
            break;
        }
        let path = entry.into_path();
        let Some(root) = owner_root(&path, &roots) else {
            summary.walk_errors = summary.walk_errors.saturating_add(1);
            continue;
        };
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if (!policy.include_globs.is_empty() && !include.is_match(relative))
            || exclude.is_match(relative)
        {
            continue;
        }
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    }
    paths.sort();
    let mut matches = Vec::new();
    'files: for path in paths {
        if cancellation.load(Ordering::Acquire) {
            return;
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        if metadata.len() > policy.maximum_file_bytes {
            summary.skipped_too_large = summary.skipped_too_large.saturating_add(1);
            continue;
        }
        if summary.bytes_searched.saturating_add(metadata.len()) > policy.maximum_total_bytes {
            summary.truncated = true;
            break;
        }
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        let bytes = match read_bounded_bytes(file, policy.maximum_file_bytes) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                summary.skipped_too_large = summary.skipped_too_large.saturating_add(1);
                continue;
            }
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        summary.bytes_searched = summary
            .bytes_searched
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let bytes: Arc<[u8]> = bytes.into();
        if bytes.contains(&0) {
            summary.skipped_binary = summary.skipped_binary.saturating_add(1);
            continue;
        }
        let Ok(text) = crate::editor::text::TextBuffer::from_bytes(&bytes) else {
            summary.skipped_binary = summary.skipped_binary.saturating_add(1);
            continue;
        };
        let source = text.to_string();
        summary.files_searched = summary.files_searched.saturating_add(1);
        let Some(root) = owner_root(&path, &roots) else {
            continue;
        };
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        for found in pattern.find_iter(&source) {
            if policy.whole_word && !has_word_boundaries(&source, found.start(), found.end()) {
                continue;
            }
            if matches.len() >= policy.maximum_results {
                summary.truncated = true;
                break 'files;
            }
            let line_start = source[..found.start()]
                .rfind('\n')
                .map_or(0, |offset| offset.saturating_add(1));
            let line_end = source[found.end()..]
                .find('\n')
                .map_or(source.len(), |offset| found.end().saturating_add(offset));
            let line = source[..line_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            let column = found.start().saturating_sub(line_start);
            let end_line_start = source[..found.end()]
                .rfind('\n')
                .map_or(0, |offset| offset.saturating_add(1));
            let end_line = source[..end_line_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            let end_column = found.end().saturating_sub(end_line_start);
            matches.push(WorkspaceSearchMatch {
                id: format!("{}:{}:{}", path.display(), found.start(), found.end()),
                canonical_path: path.clone(),
                relative_path: relative.clone(),
                root: root.clone(),
                range: found.start()..found.end(),
                line,
                column,
                end_line,
                end_column,
                line_text: bounded_line_preview(
                    &source,
                    line_start,
                    line_end,
                    policy.maximum_preview_bytes,
                ),
                snapshot: Arc::clone(&bytes),
            });
        }
    }
    for batch in matches.chunks(policy.batch_size) {
        if !send_search_event(
            &sender,
            &cancellation,
            WorkspaceSearchEvent::Batch {
                generation,
                matches: batch.to_vec(),
            },
        ) {
            return;
        }
    }
    let _ = send_search_event(
        &sender,
        &cancellation,
        WorkspaceSearchEvent::Complete {
            generation,
            summary,
        },
    );
}
