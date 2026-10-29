//! Bounded commit history: a page of commits and the real patch for one commit or a range.
//! A filtered or truncated page states what it hid and never implies adjacency across it.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

use super::GitHeadId;

/// One page of history is bounded by the same patch ceiling the rest of the Git owner uses.
const PATCH_CEILING: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitLogGeneration(pub u64);
#[derive(Clone, Debug)]
pub struct GitLogPageRequest {
    pub repository: PathBuf,
    /// Case-insensitive substring matched against the commit subject.
    pub filter: Option<String>,
    /// Commits to skip before this page starts, so a next page continues the walk.
    pub offset: usize,
    /// Maximum commits in this page.
    pub maximum_commits: usize,
    pub deadline: Duration,
}
#[derive(Clone, Debug)]
pub enum GitLogPatchRequest {
    /// The commit's own patch, taken against its first parent.
    Commit {
        repository: PathBuf,
        commit: GitHeadId,
        maximum_bytes: usize,
        deadline: Duration,
    },
    /// A merge commit's combined patch: the change from the merge base of its parents.
    Merged {
        repository: PathBuf,
        commit: GitHeadId,
        maximum_bytes: usize,
        deadline: Duration,
    },
    /// The change spanning a range, from the base tree to the tip tree.
    Range {
        repository: PathBuf,
        base: GitHeadId,
        tip: GitHeadId,
        maximum_bytes: usize,
        deadline: Duration,
    },
}
#[derive(Clone, Debug)]
pub enum GitLogRequest {
    Page(GitLogPageRequest),
    Patch(GitLogPatchRequest),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLogEntry {
    pub id: GitHeadId,
    pub summary: String,
    pub author: String,
    pub authored_seconds: i64,
    /// True when the commit has more than one parent, so callers never flatten it.
    pub merge: bool,
    pub parents: Vec<GitHeadId>,
    /// Decoration: the local or remote refs that point exactly here.
    pub refs: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLogPage {
    pub generation: GitLogGeneration,
    pub entries: Vec<GitLogEntry>,
    /// Exact number of commits in the walk, established by finishing the walk.
    pub total: usize,
    /// Commits actually returned in this page.
    pub loaded: usize,
    /// Commits the filter hid, stated rather than silently dropped.
    pub filtered: usize,
    /// Commits the page bound withheld; the caller continues from `next_offset`.
    pub remaining: usize,
    pub next_offset: usize,
    pub filter: Option<String>,
}
impl GitLogPage {
    /// A view is complete only when it hid nothing and withheld nothing.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.filtered == 0 && self.remaining == 0
    }
    /// Plain-text note for a row or footer, or `None` when nothing is hidden.
    #[must_use]
    pub fn omission_note(&self) -> Option<String> {
        if self.complete() {
            return None;
        }
        Some(match (&self.filter, self.filtered, self.remaining) {
            (Some(filter), filtered, 0) => {
                format!("{filtered} hidden by `{filter}` of {} commits", self.total)
            }
            (Some(filter), filtered, remaining) => format!(
                "{filtered} hidden by `{filter}`, {remaining} more of {} commits",
                self.total
            ),
            (None, _, remaining) => format!("{remaining} more of {} commits", self.total),
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLogPatch {
    pub generation: GitLogGeneration,
    pub commit: GitHeadId,
    pub text: String,
    /// Set when the patch was cut at the ceiling rather than fully read.
    pub truncated: bool,
    /// True when the diff had to fall back because binary content cannot be shown as text.
    pub binary: bool,
}
#[derive(Clone, Debug)]
pub enum GitLogResult {
    Page(GitLogPage),
    Patch(GitLogPatch),
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum GitLogError {
    #[error("invalid Git log request")]
    Invalid,
    #[error("not a Git repository")]
    NotRepository,
    #[error("the requested commit does not exist")]
    UnknownCommit,
    #[error("a merge commit needs two parents to form a combined patch")]
    NotAMerge,
    #[error("Git log deadline exceeded")]
    Deadline,
    #[error("Git log was cancelled")]
    Cancelled,
    #[error("Git log worker stopped")]
    Stopped,
    #[error("Git log backend failed")]
    Backend,
}
pub(super) enum GitLogEvent {
    Complete(Result<GitLogResult, GitLogError>),
}
#[derive(Default)]
pub struct GitLogService {
    pub(super) next: u64,
    active: Option<GitLogGeneration>,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<GitLogEvent>>,
    accepted: Option<GitLogResult>,
}
impl GitLogService {
    pub fn start(&mut self, request: GitLogRequest) -> Result<GitLogGeneration, GitLogError> {
        match &request {
            GitLogRequest::Page(page) => {
                if page.repository.as_os_str().is_empty()
                    || page.maximum_commits == 0
                    || page.maximum_commits > 10_000
                    || page.offset > 1_000_000
                    || page.deadline.is_zero()
                    || page.deadline > Duration::from_secs(30)
                    || page.filter.as_ref().is_some_and(|filter| filter.len() > 4096)
                {
                    return Err(GitLogError::Invalid);
                }
            }
            GitLogRequest::Patch(patch) => {
                let (repository, maximum_bytes, deadline) = match patch {
                    GitLogPatchRequest::Commit {
                        repository,
                        maximum_bytes,
                        deadline,
                        ..
                    }
                    | GitLogPatchRequest::Merged {
                        repository,
                        maximum_bytes,
                        deadline,
                        ..
                    }
                    | GitLogPatchRequest::Range {
                        repository,
                        maximum_bytes,
                        deadline,
                        ..
                    } => (repository, maximum_bytes, deadline),
                };
                if repository.as_os_str().is_empty()
                    || *maximum_bytes == 0
                    || *maximum_bytes > PATCH_CEILING
                    || deadline.is_zero()
                    || *deadline > Duration::from_secs(30)
                {
                    return Err(GitLogError::Invalid);
                }
            }
        }
        self.cancel();
        self.next = self.next.saturating_add(1);
        let generation = GitLogGeneration(self.next);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-log-{}", generation.0))
            .spawn(move || {
                let result = match &request {
                    GitLogRequest::Page(page) => read_page(generation, page, &worker),
                    GitLogRequest::Patch(patch) => read_patch(generation, patch, &worker),
                };
                let _ = tx.send(GitLogEvent::Complete(result));
            })
            .map_err(|_| GitLogError::Stopped)?;
        self.active = Some(generation);
        self.cancel = Some(cancel);
        self.receiver = Some(rx);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release)
        }
        self.active = None;
        self.receiver = None
    }
    pub fn poll(&mut self) -> Result<Option<&GitLogResult>, GitLogError> {
        let event = match self.receiver.as_ref() {
            Some(receiver) => receiver.try_recv(),
            None => return Ok(None),
        };
        match event {
            Ok(GitLogEvent::Complete(result)) => {
                let expected = self.active.take();
                self.receiver = None;
                self.cancel = None;
                let result = result?;
                let generation = match &result {
                    GitLogResult::Page(page) => page.generation,
                    GitLogResult::Patch(patch) => patch.generation,
                };
                if expected != Some(generation) {
                    return Ok(None);
                }
                self.accepted = Some(result);
                Ok(self.accepted.as_ref())
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.cancel();
                Err(GitLogError::Stopped)
            }
        }
    }
}
impl Drop for GitLogService {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn open(repository: &Path) -> Result<git2::Repository, GitLogError> {
    git2::Repository::open(repository).map_err(|_| GitLogError::NotRepository)
}
fn bounded(text: &str, maximum: usize) -> String {
    text.chars().take(maximum).collect()
}
fn oid_of(id: GitHeadId) -> git2::Oid {
    git2::Oid::from_bytes(&id.0).unwrap_or_else(|_| git2::Oid::zero())
}

fn read_page(
    generation: GitLogGeneration,
    request: &GitLogPageRequest,
    cancel: &AtomicBool,
) -> Result<GitLogResult, GitLogError> {
    let repository = open(&request.repository)?;
    let started = Instant::now();
    let mut walk = repository.revwalk().map_err(|_| GitLogError::Backend)?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .map_err(|_| GitLogError::Backend)?;
    walk.push_head().map_err(|_| GitLogError::UnknownCommit)?;
    let filter = request
        .filter
        .as_ref()
        .map(|filter| filter.trim().to_lowercase())
        .filter(|filter| !filter.is_empty());
    let mut entries = Vec::new();
    let mut total = 0usize;
    let mut filtered = 0usize;
    let mut matched = 0usize;
    for oid in walk {
        if cancel.load(Ordering::Acquire) {
            return Err(GitLogError::Cancelled);
        }
        if started.elapsed() > request.deadline {
            return Err(GitLogError::Deadline);
        }
        let oid = oid.map_err(|_| GitLogError::Backend)?;
        let commit = repository
            .find_commit(oid)
            .map_err(|_| GitLogError::UnknownCommit)?;
        total += 1;
        let summary = commit.summary().unwrap_or_default().to_owned();
        if let Some(filter) = &filter
            && !summary.to_lowercase().contains(filter.as_str())
        {
            // The walk continues so the total stays exact; the omission is counted and stated.
            filtered += 1;
            continue;
        }
        if matched < request.offset {
            matched += 1;
            continue;
        }
        matched += 1;
        if entries.len() >= request.maximum_commits {
            continue;
        }
        let parents = commit
            .parent_ids()
            .map(|parent| GitHeadId(parent.as_bytes().try_into().unwrap_or([0; 20])))
            .collect::<Vec<_>>();
        entries.push(GitLogEntry {
            id: GitHeadId(commit.id().as_bytes().try_into().unwrap_or([0; 20])),
            summary: bounded(&summary, 4096),
            author: bounded(commit.author().name().unwrap_or_default(), 1024),
            authored_seconds: commit.time().seconds(),
            merge: parents.len() > 1,
            parents,
            refs: refs_at(&repository, commit.id()),
        });
    }
    let loaded = entries.len();
    let remaining = total
        .saturating_sub(filtered)
        .saturating_sub(request.offset)
        .saturating_sub(loaded);
    Ok(GitLogResult::Page(GitLogPage {
        generation,
        entries,
        total,
        loaded,
        filtered,
        remaining,
        next_offset: request.offset.saturating_add(loaded),
        filter,
    }))
}

fn refs_at(repository: &git2::Repository, target: git2::Oid) -> Vec<String> {
    let Ok(references) = repository.references() else {
        return Vec::new();
    };
    let mut refs = references
        .filter_map(Result::ok)
        .filter(|reference| reference.target() == Some(target))
        .filter_map(|reference| reference.shorthand().map(str::to_owned))
        .collect::<Vec<_>>();
    refs.sort();
    refs.truncate(16);
    refs
}

fn tree_of<'a>(
    repository: &'a git2::Repository,
    id: GitHeadId,
) -> Result<git2::Tree<'a>, GitLogError> {
    let commit = repository
        .find_commit(oid_of(id))
        .map_err(|_| GitLogError::UnknownCommit)?;
    commit.tree().map_err(|_| GitLogError::Backend)
}

fn patch_text(
    repository: &git2::Repository,
    old: Option<&git2::Tree<'_>>,
    new: &git2::Tree<'_>,
    maximum_bytes: usize,
    deadline: Duration,
    cancel: &AtomicBool,
) -> Result<(String, bool, bool), GitLogError> {
    let diff = repository
        .diff_tree_to_tree(old, Some(new), None)
        .map_err(|_| GitLogError::Backend)?;
    let binary = diff.deltas().any(|delta| delta.flags().is_binary());
    let started = Instant::now();
    let mut text = String::new();
    let mut truncated = false;
    let result = diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        if cancel.load(Ordering::Acquire) || started.elapsed() > deadline {
            truncated = true;
            return false;
        }
        // libgit2 hands the callback a content line WITHOUT its origin character, so the
        // origin has to be written back or the text is not a unified diff at all: an addition
        // and a deletion become indistinguishable. File headers, hunk headers and the
        // no-newline marker arrive already formatted and must not be prefixed.
        let origin = line.origin();
        let prefixed = matches!(origin, ' ' | '+' | '-');
        let length = line.content().len().saturating_add(usize::from(prefixed));
        if text.len().saturating_add(length) > maximum_bytes {
            truncated = true;
            return false;
        }
        if prefixed {
            text.push(origin);
        }
        text.push_str(&String::from_utf8_lossy(line.content()));
        true
    });
    result.map_err(|_| GitLogError::Backend)?;
    Ok((text, truncated, binary))
}

fn read_patch(
    generation: GitLogGeneration,
    request: &GitLogPatchRequest,
    cancel: &AtomicBool,
) -> Result<GitLogResult, GitLogError> {
    let (repository, commit, maximum_bytes, deadline) = match request {
        GitLogPatchRequest::Commit {
            repository,
            commit,
            maximum_bytes,
            deadline,
        }
        | GitLogPatchRequest::Merged {
            repository,
            commit,
            maximum_bytes,
            deadline,
        } => (repository, Some(*commit), *maximum_bytes, *deadline),
        GitLogPatchRequest::Range {
            repository,
            base,
            tip,
            maximum_bytes,
            deadline,
        } => {
            let repository = open(repository)?;
            let (text, truncated, binary) = patch_text(
                &repository,
                Some(&tree_of(&repository, *base)?),
                &tree_of(&repository, *tip)?,
                *maximum_bytes,
                *deadline,
                cancel,
            )?;
            return Ok(GitLogResult::Patch(GitLogPatch {
                generation,
                commit: *tip,
                text,
                truncated,
                binary,
            }));
        }
    };
    let repository = open(repository)?;
    let commit_id = commit.ok_or(GitLogError::UnknownCommit)?;
    let target = repository
        .find_commit(oid_of(commit_id))
        .map_err(|_| GitLogError::UnknownCommit)?;
    let target_tree = target.tree().map_err(|_| GitLogError::Backend)?;
    // A merge shows the change from the merge base of its parents, so both sides are included
    // rather than only the first parent's line of descent.
    let base_tree = match request {
        GitLogPatchRequest::Merged { .. } => {
            let first = target.parent(0).map_err(|_| GitLogError::NotAMerge)?;
            let second = target.parent(1).map_err(|_| GitLogError::NotAMerge)?;
            match repository.merge_base(first.id(), second.id()) {
                Ok(base) => Some(
                    repository
                        .find_commit(base)
                        .and_then(|commit| commit.tree())
                        .map_err(|_| GitLogError::Backend)?,
                ),
                Err(_) => Some(first.tree().map_err(|_| GitLogError::Backend)?),
            }
        }
        _ => target
            .parent(0)
            .ok()
            .and_then(|parent| parent.tree().ok()),
    };
    let (text, truncated, binary) = patch_text(
        &repository,
        base_tree.as_ref(),
        &target_tree,
        maximum_bytes,
        deadline,
        cancel,
    )?;
    Ok(GitLogResult::Patch(GitLogPatch {
        generation,
        commit: commit_id,
        text,
        truncated,
        binary,
    }))
}

/// What one column of a graph row is.
///
/// The legacy `history_lanes` prefix could only be a string, so it could not say WHERE an
/// edge leaves a lane or where it arrives. This can, which is what lets a merge be drawn as a
/// curve rather than as a straight line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphCellKind {
    /// No lane in this column on this row.
    Empty,
    /// The commit this row is about.
    Commit,
    /// The commit this row is about, and it has more than one parent.
    Merge,
    /// A lane passing through.
    Lane,
    /// An edge bending towards a lane further right.
    BendRight,
    /// An edge bending towards a lane further left.
    BendLeft,
    /// An edge closing into a commit from a lane to its right. A lane that converges must be
    /// SEEN to converge; without this the lane simply vanishes.
    BendRightClose,
    /// Ancestry continuing outside the loaded window.
    Continues,
}

impl GraphCellKind {
    /// The glyph this cell is DRAWN with, which distinguishes a merge from an ordinary
    /// commit. The legacy prefix form uses `glyph` instead, because it never distinguished
    /// one, and that form is kept byte-identical for the callers that already read it.
    #[must_use]
    pub const fn drawn_glyph(self) -> char {
        match self {
            Self::Merge => '\u{25ce}',
            other => other.glyph(),
        }
    }

    /// The glyph the legacy prefix form uses.
    #[must_use]
    pub const fn glyph(self) -> char {
        match self {
            Self::Empty => ' ',
            Self::Commit | Self::Merge => '\u{25cf}',
            Self::Lane => '\u{2502}',
            Self::BendRight => '\u{256e}',
            Self::BendLeft => '\u{256d}',
            Self::BendRightClose => '\u{256f}',
            Self::Continues => '\u{2193}',
        }
    }
}

/// An edge of the graph: the lane it leaves and the lane it arrives at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
}

/// One row of the ancestry graph.
///
/// `cells` reproduces the legacy prefix exactly, cell for cell, so the string form can be
/// derived from it rather than computed a second time. `edges` carries what the string could
/// not: where each parent edge actually goes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryGraphRow {
    /// The lane the commit sits on.
    pub lane: usize,
    /// The lanes that CLOSED into this commit, as edges from the closing lane to the commit's
    /// lane.
    ///
    /// Additive information for the renderer: the lane layout itself is untouched, so the
    /// derived prefix stays byte-identical to what it was.
    pub converges: Vec<GraphEdge>,
    /// How many columns the lane area occupies. The bend cells follow it, so this is what maps
    /// a bend's column to the lane it belongs to.
    pub lane_width: usize,
    pub cells: Vec<GraphCellKind>,
    pub edges: Vec<GraphEdge>,
    pub merge: bool,
}

/// Ancestry lanes for a window of history, newest first.
///
/// The lanes are computed from the real parent ids, never from list position. A lane stays
/// occupied while the commit it expects has not been seen, which is what makes a parent that
/// lies outside the window render as continuing rather than as an edge to a nearby commit.
/// A row ending in `\u{2193}` has ancestry outside the window; a root commit does not, so the two
/// are distinguishable.
#[must_use]
pub fn history_graph_rows(entries: &[GitLogEntry]) -> Vec<HistoryGraphRow> {
    let visible = entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
    let mut lanes: Vec<Option<GitHeadId>> = Vec::new();
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let lane = match lanes.iter().position(|slot| *slot == Some(entry.id)) {
            Some(lane) => lane,
            None => match lanes.iter().position(Option::is_none) {
                Some(index) => {
                    lanes[index] = Some(entry.id);
                    index
                }
                None => {
                    lanes.push(Some(entry.id));
                    lanes.len() - 1
                }
            },
        };
        // Any other lane waiting for this commit converges into it here. The clearing IS the
        // edge, so it is recorded before the slot is freed - otherwise the lane would simply
        // vanish and ancestry that merges could not be seen to merge.
        let mut converges = Vec::new();
        for (index, slot) in lanes.iter_mut().enumerate() {
            if index != lane && *slot == Some(entry.id) {
                converges.push(GraphEdge {
                    from: index,
                    to: lane,
                });
                *slot = None;
            }
        }
        let mut edges = Vec::new();
        lanes[lane] = entry.parents.first().copied();
        if let Some(parent) = entry.parents.first() {
            edges.push(GraphEdge {
                from: lane,
                to: lane,
            });
            let _ = parent;
        }
        // A parent that opens a new lane is an edge that bends out of this commit's lane and
        // arrives at the lane the parent was given.
        let mut opens = 0usize;
        for parent in entry.parents.iter().skip(1) {
            if lanes.contains(&Some(*parent)) {
                let to = lanes
                    .iter()
                    .position(|slot| *slot == Some(*parent))
                    .unwrap_or(lane);
                edges.push(GraphEdge { from: lane, to });
                opens += 1;
                continue;
            }
            let to = match lanes.iter().position(Option::is_none) {
                Some(index) => index,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            };
            lanes[to] = Some(*parent);
            edges.push(GraphEdge { from: lane, to });
            opens += 1;
        }
        let width = lanes.len().max(lane.saturating_add(1));
        let mut cells = Vec::with_capacity(width + opens + 1);
        for index in 0..width {
            if index == lane {
                cells.push(if entry.merge {
                    GraphCellKind::Merge
                } else {
                    GraphCellKind::Commit
                });
            } else if lanes.get(index).is_some_and(|slot| slot.is_some()) {
                cells.push(GraphCellKind::Lane);
            } else {
                cells.push(GraphCellKind::Empty);
            }
        }
        for _ in 0..opens {
            cells.push(GraphCellKind::BendRight);
        }
        let continues = entry
            .parents
            .iter()
            .any(|parent| !visible.contains(parent));
        if continues {
            cells.push(GraphCellKind::Continues);
        }
        out.push(HistoryGraphRow {
            lane,
            converges,
            lane_width: width,
            cells,
            edges,
            merge: entry.merge,
        });
    }
    out
}

/// The legacy prefix form of the ancestry lanes, derived from the structured rows so the two
/// can never disagree about what the graph looks like.
#[must_use]
pub fn history_lanes(entries: &[GitLogEntry]) -> Vec<String> {
    history_graph_rows(entries)
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| cell.glyph())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}
