//! Conflict provenance, read from the repository on demand.
//!
//! Conflict views show base/ours/theirs/result; choosing a side edits the result buffer through
//! ordinary transactions. Saving and staging are separate steps.
//!
//! Nothing here is cached. A conflict is a state of the repository that changes underneath the
//! editor, so every read goes back to the index and to the repository's own operation state.
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitConflictError {
    #[error("repository could not be opened")]
    NotRepository,
    #[error("conflict state could not be read")]
    Backend,
}

/// One side of a conflict, as the index really holds it: a blob and its mode. `None` at the
/// `Option` level means the side DOES NOT EXIST for this path, which is different from an empty
/// blob and is why absence is a type-level fact rather than a zero OID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitConflictSide {
    pub oid: git2::Oid,
    pub mode: u32,
}

impl GitConflictSide {
    fn from_entry(entry: &git2::IndexEntry) -> Self {
        Self {
            oid: entry.id,
            mode: entry.mode,
        }
    }

    #[must_use]
    pub fn short(&self) -> String {
        self.oid.to_string()[..8].to_owned()
    }
}

/// The three sides of one conflicted path. `ours` is this branch, `theirs` is what is being
/// merged in, and `base` is the merge base the contract asks for ON DEMAND.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitConflictEntry {
    pub path: PathBuf,
    pub base: Option<GitConflictSide>,
    pub ours: Option<GitConflictSide>,
    pub theirs: Option<GitConflictSide>,
}

impl GitConflictEntry {
    /// What each side contributes, for a surface that must not show a missing side as content.
    #[must_use]
    pub fn summary(&self) -> String {
        fn name(side: &Option<GitConflictSide>) -> String {
            match side {
                Some(side) => side.short(),
                None => "absent".to_owned(),
            }
        }
        format!(
            "base {} · ours {} · theirs {}",
            name(&self.base),
            name(&self.ours),
            name(&self.theirs)
        )
    }
}

/// The repository's paused operation and its conflicts. `operation` is `None` when no operation
/// with a lifecycle is active; a state the model cannot name is reported as the repository's own
/// name for it rather than as completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitConflictState {
    pub operation: Option<String>,
    pub conflicts: Vec<GitConflictEntry>,
}

impl GitConflictState {
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.operation.is_some()
    }

    /// The operation bar's phase line: what is happening, and how much of it is unresolved.
    #[must_use]
    pub fn phase(&self) -> String {
        match (&self.operation, self.conflicts.len()) {
            (Some(operation), 0) => format!("{operation}: no textual conflicts remain"),
            (Some(operation), count) => format!("{operation}: {count} conflicted path(s)"),
            (None, count) => format!("no operation in progress; {count} conflicted path(s)"),
        }
    }
}

/// An action the bar may offer. Only the actions VALID in the current state are ever listed, so
/// a surface cannot invite the user to do something the repository would refuse.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GitOperationAction {
    /// Resolve the remaining conflicts. Valid while any conflict remains.
    Resolve,
    /// Finish the operation. Valid only when nothing is left conflicted.
    Continue,
    /// Drop the current replayed commit and resume a rebase.
    Skip,
    /// Abandon the operation. Always valid while an operation is paused.
    Abort,
}

impl GitOperationAction {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Continue => "continue",
            Self::Skip => "skip",
            Self::Abort => "abort",
        }
    }
}

/// The operation bar: what is happening, what is in the way, and what may be done about it.
///
/// `ui.md` requires the bar to name the operation, its phase, its blockers and ONLY the actions
/// valid in that state, to appear even when a paused operation has no textual conflict, and never
/// to present an unknown state as completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitOperationBar {
    pub operation: String,
    pub phase: String,
    pub blockers: Vec<String>,
    pub actions: Vec<GitOperationAction>,
}

impl GitOperationBar {
    /// Build the bar from the repository's REAL state. Nothing here is invented: the blockers are
    /// the conflicted paths actually in the index and the valid actions follow from whether any
    /// remain.
    #[must_use]
    pub fn of(state: &GitConflictState) -> Option<Self> {
        let operation = state.operation.clone()?;
        let blockers = state
            .conflicts
            .iter()
            .map(|conflict| conflict.path.display().to_string())
            .collect::<Vec<_>>();
        let mut actions = Vec::new();
        if blockers.is_empty() {
            if continues(&operation) {
                actions.push(GitOperationAction::Continue);
            }
        } else {
            actions.push(GitOperationAction::Resolve);
        }
        // Git skips a replayed commit; a merge has no commit to skip.
        if skips(&operation) {
            actions.push(GitOperationAction::Skip);
        }
        actions.push(GitOperationAction::Abort);
        Some(Self {
            operation,
            phase: state.phase(),
            blockers,
            actions,
        })
    }

    #[must_use]
    pub fn offers(&self, action: GitOperationAction) -> bool {
        self.actions.contains(&action)
    }
}

/// Whether Git can continue `operation` once nothing is conflicted.
#[must_use]
pub fn continues(operation: &str) -> bool {
    matches!(operation, "merge" | "rebase" | "cherry-pick" | "revert")
}

/// Whether Git can skip the commit `operation` is applying: every replay, never a merge.
#[must_use]
pub fn skips(operation: &str) -> bool {
    matches!(operation, "rebase" | "cherry-pick" | "revert")
}

/// Read the paused operation and every conflicted path from the repository, on demand.
/// `read_conflict_state` for a repository whose status lists no conflicted path: the operation
/// in progress only, without loading the index (tens of milliseconds on a big repository).
pub fn read_operation_state(repository: &Path) -> Result<GitConflictState, GitConflictError> {
    let repo = git2::Repository::open(repository).map_err(|_| GitConflictError::NotRepository)?;
    Ok(GitConflictState {
        operation: operation_name(repo.state()),
        conflicts: Vec::new(),
    })
}

pub fn read_conflict_state(repository: &Path) -> Result<GitConflictState, GitConflictError> {
    let repo = git2::Repository::open(repository).map_err(|_| GitConflictError::NotRepository)?;
    let operation = operation_name(repo.state());
    let index = repo.index().map_err(|_| GitConflictError::Backend)?;
    let conflicts = index.conflicts().map_err(|_| GitConflictError::Backend)?;
    let mut entries = Vec::new();
    for conflict in conflicts {
        let conflict = conflict.map_err(|_| GitConflictError::Backend)?;
        // The path is whichever side the index actually carries: a conflict can lack a base,
        // and a delete/modify conflict can lack one of the others.
        let path = conflict
            .ancestor
            .as_ref()
            .or(conflict.our.as_ref())
            .or(conflict.their.as_ref())
            .map(|entry| git_path(&entry.path).ok_or(GitConflictError::Backend))
            .transpose()?;
        let Some(path) = path else {
            continue;
        };
        entries.push(GitConflictEntry {
            path,
            base: conflict.ancestor.as_ref().map(GitConflictSide::from_entry),
            ours: conflict.our.as_ref().map(GitConflictSide::from_entry),
            theirs: conflict.their.as_ref().map(GitConflictSide::from_entry),
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(GitConflictState {
        operation,
        conflicts: entries,
    })
}

/// The repository's own name for an operation with a lifecycle. A state this model cannot name
/// is reported by libgit2's own spelling rather than being folded into "clean".
fn operation_name(state: git2::RepositoryState) -> Option<String> {
    match state {
        git2::RepositoryState::Clean => None,
        git2::RepositoryState::Merge => Some("merge".into()),
        git2::RepositoryState::Revert | git2::RepositoryState::RevertSequence => {
            Some("revert".into())
        }
        git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
            Some("cherry-pick".into())
        }
        git2::RepositoryState::Bisect => Some("bisect".into()),
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseInteractive
        | git2::RepositoryState::RebaseMerge => Some("rebase".into()),
        git2::RepositoryState::ApplyMailbox | git2::RepositoryState::ApplyMailboxOrRebase => {
            Some("apply-mailbox".into())
        }
    }
}

/// Which side of a conflict the user is accepting. The contract's "explicit accepted source
/// versions": a choice names one of the three sides, never a merged guess.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitConflictChoice {
    Base,
    Ours,
    Theirs,
}

impl GitConflictChoice {
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "base" | "ancestor" => Some(Self::Base),
            "ours" | "our" | "local" => Some(Self::Ours),
            "theirs" | "their" | "remote" => Some(Self::Theirs),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Ours => "ours",
            Self::Theirs => "theirs",
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitConflictResolveError {
    #[error("repository could not be opened")]
    NotRepository,
    #[error("conflict state could not be read: {0}")]
    Backend(String),
    #[error("`{0}` is not a conflicted path")]
    NoConflict(String),
    #[error("`{path}` has no {side} side")]
    SideAbsent { path: String, side: &'static str },
    #[error("`{0}` is not text this editor can edit")]
    NotText(String),
}

/// Read ONE side of a conflicted path from the index, on demand, and return its content.
///
/// Refusals come before any content is produced: a path that is not conflicted, a side that
/// genuinely does not exist for this path, and a side that is not text are each refused rather
/// than turned into an empty string or a fabricated merge.
pub fn read_side_content(
    repository: &Path,
    path: &Path,
    choice: GitConflictChoice,
) -> Result<String, GitConflictResolveError> {
    read_side(repository, path, choice).map(|(content, _)| content)
}

fn read_side(
    repository: &Path,
    path: &Path,
    choice: GitConflictChoice,
) -> Result<(String, u32), GitConflictResolveError> {
    let repo =
        git2::Repository::open(repository).map_err(|_| GitConflictResolveError::NotRepository)?;
    let wanted = conflict_relative_path(&repo, path)?;
    let index = repo
        .index()
        .map_err(|error| GitConflictResolveError::Backend(error.message().to_owned()))?;
    let mut conflicts = index
        .conflicts()
        .map_err(|error| GitConflictResolveError::Backend(error.message().to_owned()))?;
    let mut found: Option<git2::IndexConflict> = None;
    for conflict in conflicts.by_ref() {
        let conflict = conflict
            .map_err(|error| GitConflictResolveError::Backend(error.message().to_owned()))?;
        let entry = conflict
            .ancestor
            .as_ref()
            .or(conflict.our.as_ref())
            .or(conflict.their.as_ref());
        if let Some(entry) = entry {
            // Index paths are bytes; compare against the caller's relativized path directly.
            if git_path(&entry.path).as_deref() == Some(wanted.as_path()) {
                found = Some(conflict);
                break;
            }
        }
    }
    drop(conflicts);
    let conflict =
        found.ok_or_else(|| GitConflictResolveError::NoConflict(wanted.display().to_string()))?;
    let side = match choice {
        GitConflictChoice::Base => conflict.ancestor,
        GitConflictChoice::Ours => conflict.our,
        GitConflictChoice::Theirs => conflict.their,
    };
    let Some(side) = side else {
        return Err(GitConflictResolveError::SideAbsent {
            path: wanted.display().to_string(),
            side: choice.name(),
        });
    };
    if !matches!(side.mode, 0o100644 | 0o100755) {
        return Err(GitConflictResolveError::NotText(
            wanted.display().to_string(),
        ));
    }
    let blob = repo
        .find_blob(side.id)
        .map_err(|error| GitConflictResolveError::Backend(error.message().to_owned()))?;
    String::from_utf8(blob.content().to_vec())
        .map(|content| (content, side.mode))
        .map_err(|_| GitConflictResolveError::NotText(wanted.display().to_string()))
}

pub(super) fn git_path(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(std::ffi::OsString::from_vec(bytes.to_vec()).into())
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

// Index names are lexical worktree-relative paths, not the targets of worktree symlinks.
fn conflict_relative_path(
    repo: &git2::Repository,
    path: &Path,
) -> Result<PathBuf, GitConflictResolveError> {
    let workdir = repo
        .workdir()
        .ok_or(GitConflictResolveError::NotRepository)?;
    let relative = if path.is_absolute() {
        path.strip_prefix(workdir)
            .map_err(|_| GitConflictResolveError::NoConflict(path.display().to_string()))?
    } else {
        path
    };
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(name) => normalized.push(name),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if normalized.pop() => {}
            _ => {
                return Err(GitConflictResolveError::NoConflict(
                    path.display().to_string(),
                ));
            }
        }
    }
    Ok(normalized)
}

/// Why a recovery action was refused or failed.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitRecoveryRefusal {
    #[error("repository could not be opened")]
    NotRepository,
    #[error("no operation is paused, so there is nothing to {0}")]
    NotPaused(&'static str),
    #[error("{0} conflicted path(s) remain; resolve and stage them first")]
    ConflictsRemain(usize),
    #[error("would discard unrelated changes: {0}")]
    WouldDiscard(String),
    #[error("the operation's recorded starting point could not be read")]
    NoRecordedStart,
    #[error("the recovery action failed: {0}")]
    Backend(String),
}

/// What a recovered operation left behind, so the caller can report the real revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRecoveryOutcome {
    pub operation: String,
    pub commit: Option<git2::Oid>,
    pub restored_to: Option<git2::Oid>,
    pub cleanup_warning: Option<String>,
    /// Set when the operation advanced but paused again instead of finishing.
    pub stopped: Option<GitOperationStop>,
}

/// Where a resumed replay paused again. Git exits non-zero when a continued or skipped
/// rebase stops on the next conflicting commit, but the step the user confirmed DID happen,
/// so this is progress to report, not a failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitOperationStop {
    /// The commit now being replayed, when Git records one.
    pub current: Option<git2::Oid>,
    /// Paths conflicted at the new stop, sorted; empty for an `edit` or `break` stop.
    pub conflicts: Vec<PathBuf>,
    /// `(step, total)` of the replay, when Git records it.
    pub progress: Option<(usize, usize)>,
}

impl GitOperationStop {
    /// `stopped on conflict in a.txt (3/5)`: what a receipt says after the action's verb.
    #[must_use]
    pub fn summary(&self) -> String {
        let progress = self
            .progress
            .map(|(step, total)| format!(" ({step}/{total})"))
            .unwrap_or_default();
        match self.conflicts.as_slice() {
            [] => match self.current {
                Some(current) => format!("paused at {}{progress}", &current.to_string()[..8]),
                None => format!("paused{progress}"),
            },
            [path] => format!("stopped on conflict in {}{progress}", path.display()),
            [first, rest @ ..] => format!(
                "stopped on conflicts in {} and {} more{progress}",
                first.display(),
                rest.len()
            ),
        }
    }
}

/// Finish a merge or resume a rebase. The optional message is used for merge commits.
///
/// Refuses while ANY conflict remains: a half-resolved tree is not a merge, and committing one
/// would record a state the user never chose.
pub fn continue_operation(
    repository: &Path,
    message: &str,
) -> Result<GitRecoveryOutcome, GitRecoveryRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitRecoveryRefusal::NotRepository)?;
    let operation =
        operation_name(repo.state()).ok_or(GitRecoveryRefusal::NotPaused("continue"))?;
    if !continues(&operation) {
        return Err(GitRecoveryRefusal::Backend(format!(
            "continuing {operation} is not supported"
        )));
    }
    let remaining = conflict_count(&repo)?;
    if remaining > 0 {
        return Err(GitRecoveryRefusal::ConflictsRemain(remaining));
    }
    if matches!(operation.as_str(), "cherry-pick" | "revert") {
        // Git commits the whole index: unrelated staged changes would ride along.
        let start = pick_start(&repo).ok_or(GitRecoveryRefusal::NoRecordedStart)?;
        let touched = operation_touched_paths(&repo, start, &operation, None)?;
        let risky = risky_changes(&repo, &touched, true)?;
        if !risky.is_empty() {
            return Err(GitRecoveryRefusal::WouldDiscard(risky.join(", ")));
        }
        let before = replay_position(&repo);
        let result = run_operation_git(&repo, &[&operation, "--continue"]);
        return replay_outcome(repository, operation, before, result);
    }
    if operation == "rebase" {
        let (original, current) = rebase_info(&repo)?;
        let touched = operation_touched_paths(&repo, original, &operation, current)?;
        let risky = risky_changes(&repo, &touched, true)?;
        if !risky.is_empty() {
            return Err(GitRecoveryRefusal::WouldDiscard(risky.join(", ")));
        }
        let before = replay_position(&repo);
        let result = run_operation_git(&repo, &["rebase", "--continue"]);
        return replay_outcome(repository, operation, before, result);
    }
    let mut index = repo
        .index()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let tree_id = index
        .write_tree()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let head = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let merge_heads = recorded_merge_heads(&repo)?
        .into_iter()
        .map(|oid| {
            repo.find_commit(oid)
                .map_err(|_| GitRecoveryRefusal::NoRecordedStart)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // A previous attempt may have committed successfully but failed to remove MERGE_HEAD.
    // In that case retry only cleanup, never create another merge commit.
    let already_committed = head.parent_count() == merge_heads.len() + 1
        && repo.refname_to_id("ORIG_HEAD").ok() == head.parent_id(0).ok()
        && merge_heads
            .iter()
            .enumerate()
            .all(|(index, parent)| head.parent_id(index + 1).ok() == Some(parent.id()));
    let oid = if already_committed {
        head.id()
    } else {
        let signature = repo.signature().map_err(|error| {
            GitRecoveryRefusal::Backend(format!(
                "configure Git user.name and user.email before continuing: {}",
                error.message()
            ))
        })?;
        let message = if message.trim().is_empty() {
            format!(
                "Merge {}",
                merge_heads
                    .iter()
                    .map(|head| head.id().to_string()[..8].to_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            message.to_owned()
        };
        let parents = std::iter::once(&head)
            .chain(merge_heads.iter())
            .collect::<Vec<_>>();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &parents,
        )
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?
    };
    let cleanup_warning = repo.cleanup_state().err().map(|error| {
        format!("commit {oid} was created, but operation cleanup failed: {}; retry continue to clean up", error.message())
    });
    Ok(GitRecoveryOutcome {
        operation,
        commit: Some(oid),
        restored_to: None,
        cleanup_warning,
        stopped: None,
    })
}

/// Abort a paused operation without discarding unrelated user work.
///
/// Git's merge abort preserves working drafts, but can delete unrelated newly staged files.
/// Refuse those staged changes first. Other operations keep their conservative working-change check.
pub fn abort_operation(repository: &Path) -> Result<GitRecoveryOutcome, GitRecoveryRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitRecoveryRefusal::NotRepository)?;
    let operation = operation_name(repo.state()).ok_or(GitRecoveryRefusal::NotPaused("abort"))?;
    let rebase = if operation == "rebase" {
        Some(rebase_info(&repo)?)
    } else {
        None
    };
    let recorded = match operation.as_str() {
        "merge" => repo.refname_to_id("ORIG_HEAD").ok(),
        "cherry-pick" | "revert" => pick_start(&repo),
        "rebase" => rebase.as_ref().map(|(original, _)| *original),
        _ => {
            return Err(GitRecoveryRefusal::Backend(format!(
                "aborting {operation} is not supported"
            )));
        }
    }
    .ok_or(GitRecoveryRefusal::NoRecordedStart)?;
    let rebase_current = rebase.and_then(|(_, current)| current);
    let touched = operation_touched_paths(&repo, recorded, &operation, rebase_current)?;
    let risky = risky_changes(&repo, &touched, operation == "merge")?;
    if !risky.is_empty() {
        return Err(GitRecoveryRefusal::WouldDiscard(risky.join(", ")));
    }
    if matches!(operation.as_str(), "merge" | "rebase") {
        run_operation_git(&repo, &[&operation, "--abort"])?;
        return Ok(GitRecoveryOutcome {
            operation,
            commit: None,
            restored_to: repo
                .head()
                .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?
                .target(),
            cleanup_warning: None,
            stopped: None,
        });
    }
    let object = repo
        .find_object(recorded, None)
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(&object, Some(&mut checkout))
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let mut head = repo
        .find_reference("HEAD")
        .and_then(|reference| reference.resolve())
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    head.set_target(recorded, "dust: abort the paused operation")
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let mut index = repo
        .index()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    // ORIG_HEAD is a COMMIT, so its tree is reached through the commit rather than directly.
    let recorded_tree = repo
        .find_commit(recorded)
        .and_then(|commit| commit.tree())
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    index
        .read_tree(&recorded_tree)
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    index
        .write()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    repo.cleanup_state()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    Ok(GitRecoveryOutcome {
        operation,
        commit: None,
        restored_to: Some(recorded),
        cleanup_warning: None,
        stopped: None,
    })
}

/// Where a cherry-pick or revert started. Single picks and reverts leave HEAD at their
/// starting commit; sequences record it.
fn pick_start(repo: &git2::Repository) -> Option<git2::Oid> {
    if repo.path().join("sequencer").exists() {
        std::fs::read_to_string(repo.path().join("sequencer/head"))
            .ok()
            .and_then(|head| git2::Oid::from_str(head.trim()).ok())
    } else {
        repo.head().ok().and_then(|head| head.target())
    }
}

/// Read-only snapshot of unrelated paths that prevent a rebase action.
/// Submission always recomputes these checks before invoking Git.
pub(super) fn rebase_action_blockers(
    repository: &Path,
    action: GitOperationAction,
) -> Result<Vec<String>, GitRecoveryRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitRecoveryRefusal::NotRepository)?;
    if operation_name(repo.state()).as_deref() != Some("rebase") {
        return Ok(Vec::new());
    }
    let (original, current) = rebase_info(&repo)?;
    let touched = operation_touched_paths(&repo, original, "rebase", current)?;
    risky_changes(&repo, &touched, action == GitOperationAction::Continue)
}

/// Drop the commit a rebase, cherry-pick or revert is applying, never unrelated changes.
pub fn skip_operation(repository: &Path) -> Result<GitRecoveryOutcome, GitRecoveryRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitRecoveryRefusal::NotRepository)?;
    let operation = operation_name(repo.state()).ok_or(GitRecoveryRefusal::NotPaused("skip"))?;
    if !skips(&operation) {
        return Err(GitRecoveryRefusal::Backend(format!(
            "skipping {operation} is not supported"
        )));
    }
    let (start, current) = if operation == "rebase" {
        rebase_info(&repo)?
    } else {
        (pick_start(&repo).ok_or(GitRecoveryRefusal::NoRecordedStart)?, None)
    };
    let touched = operation_touched_paths(&repo, start, &operation, current)?;
    let risky = risky_changes(&repo, &touched, false)?;
    if !risky.is_empty() {
        return Err(GitRecoveryRefusal::WouldDiscard(risky.join(", ")));
    }
    let before = replay_position(&repo);
    let result = run_operation_git(&repo, &[&operation, "--skip"]);
    replay_outcome(repository, operation, before, result)
}

/// Stage one resolved path with Git itself (`git add`), as `s` would.
pub(super) fn stage_resolved(repository: &Path, path: &Path) -> Result<(), GitRecoveryRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitRecoveryRefusal::NotRepository)?;
    let path = path
        .to_str()
        .ok_or_else(|| GitRecoveryRefusal::Backend("the path is not UTF-8; stage it with s".into()))?;
    run_operation_git(&repo, &["--literal-pathspecs", "add", "--", path])
}

/// The replayed commit and step a rebase (or the commit a cherry-pick or revert) is paused
/// at, to tell whether Git moved on.
fn replay_position(repo: &git2::Repository) -> (Option<git2::Oid>, Option<(usize, usize)>) {
    match operation_name(repo.state()).as_deref() {
        Some("cherry-pick") => (repo.refname_to_id("CHERRY_PICK_HEAD").ok(), None),
        Some("revert") => (repo.refname_to_id("REVERT_HEAD").ok(), None),
        _ => (
            rebase_info(repo).ok().and_then(|(_, current)| current),
            rebase_progress(repo),
        ),
    }
}

/// `(step, total)` from the rebase's own bookkeeping: `msgnum`/`end` for the merge backend,
/// `next`/`last` for the apply backend.
pub(super) fn rebase_progress(repo: &git2::Repository) -> Option<(usize, usize)> {
    let read = |path: PathBuf| -> Option<usize> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    };
    let merge = repo.path().join("rebase-merge");
    if merge.is_dir() {
        return Some((read(merge.join("msgnum"))?, read(merge.join("end"))?));
    }
    let apply = repo.path().join("rebase-apply");
    Some((read(apply.join("next"))?, read(apply.join("last"))?))
}

/// Turn the result of `rebase --continue`/`--skip` into an outcome. A non-zero exit that left
/// the rebase paused on a DIFFERENT step with fresh conflicts means the confirmed step
/// happened and Git stopped on the next conflict: that is reported as an outcome with
/// `stopped`, never as a failure, so the caller can close its confirmation.
fn replay_outcome(
    repository: &Path,
    operation: String,
    before: (Option<git2::Oid>, Option<(usize, usize)>),
    result: Result<(), GitRecoveryRefusal>,
) -> Result<GitRecoveryOutcome, GitRecoveryRefusal> {
    // Git changed the repository behind this process; read it afresh.
    let repo = git2::Repository::open(repository).map_err(|_| GitRecoveryRefusal::NotRepository)?;
    let stopped = if operation_name(repo.state()).as_deref() == Some(operation.as_str()) {
        let (current, progress) = replay_position(&repo);
        let conflicts = read_conflict_state(repository)
            .map(|state| {
                state
                    .conflicts
                    .into_iter()
                    .map(|conflict| conflict.path)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let advanced = current != before.0
            || matches!((progress, before.1), (Some((now, _)), Some((then, _))) if now > then);
        Some((
            GitOperationStop {
                current,
                conflicts,
                progress,
            },
            advanced,
        ))
    } else {
        None
    };
    let stopped = match (result, stopped) {
        (Ok(()), stopped) => stopped.map(|(stop, _)| stop),
        (Err(_), Some((stop, true))) if !stop.conflicts.is_empty() => Some(stop),
        (Err(error), _) => return Err(error),
    };
    Ok(GitRecoveryOutcome {
        operation,
        commit: repo
            .head()
            .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?
            .target(),
        restored_to: None,
        cleanup_warning: None,
        stopped,
    })
}

fn rebase_info(
    repo: &git2::Repository,
) -> Result<(git2::Oid, Option<git2::Oid>), GitRecoveryRefusal> {
    let directory = ["rebase-merge", "rebase-apply"]
        .into_iter()
        .map(|name| repo.path().join(name))
        .find(|path| path.is_dir())
        .ok_or(GitRecoveryRefusal::NoRecordedStart)?;
    let original = std::fs::read_to_string(directory.join("orig-head"))
        .ok()
        .and_then(|id| git2::Oid::from_str(id.trim()).ok())
        .ok_or(GitRecoveryRefusal::NoRecordedStart)?;
    // Git records REBASE_HEAD; libgit2 exposes its current pick through the rebase API.
    let current = repo.refname_to_id("REBASE_HEAD").ok().or_else(|| {
        let mut rebase = repo.open_rebase(None).ok()?;
        rebase
            .operation_current()
            .and_then(|index| rebase.nth(index))
            .map(|operation| operation.id())
    });
    Ok((original, current))
}

fn run_operation_git(
    repo: &git2::Repository,
    arguments: &[&str],
) -> Result<(), GitRecoveryRefusal> {
    use crate::process::{JobEvent, JobPolicy, JobRequest, JobSupervisor};
    use std::time::Duration;

    let workdir = repo.workdir().ok_or(GitRecoveryRefusal::NotRepository)?;
    let program = crate::process::discover_executable("git")
        .ok_or_else(|| GitRecoveryRefusal::Backend("Git is not installed or not on PATH".into()))?;
    let mut environment = super::command::environment();
    environment.insert("GIT_EDITOR".into(), "true".into());
    let mut jobs = JobSupervisor::try_new(16)
        .map_err(|error| GitRecoveryRefusal::Backend(error.to_string()))?;
    jobs.start(JobRequest {
        program,
        argv0: None,
        arguments: arguments
            .iter()
            .map(|argument| (*argument).into())
            .collect(),
        cwd: workdir.to_path_buf(),
        environment,
        root_revision: 0,
        stdin: None,
        policy: JobPolicy {
            maximum_output_bytes: 64 * 1024,
            maximum_line_bytes: 64 * 1024,
            timeout: Duration::from_secs(30),
            ..JobPolicy::default()
        },
    })
    .map_err(|error| GitRecoveryRefusal::Backend(error.to_string()))?;
    let mut message = Vec::new();
    loop {
        for event in jobs.poll() {
            match event {
                JobEvent::Output { bytes, .. } => {
                    message.extend(
                        bytes
                            .into_iter()
                            .take(4096usize.saturating_sub(message.len())),
                    );
                }
                JobEvent::Exited {
                    code, timed_out, ..
                } => {
                    if timed_out {
                        return Err(GitRecoveryRefusal::Backend(format!(
                            "Git {} timed out",
                            arguments.join(" ")
                        )));
                    }
                    if code != Some(0) {
                        let message = String::from_utf8_lossy(&message).trim().to_owned();
                        return Err(GitRecoveryRefusal::Backend(if message.is_empty() {
                            format!("Git {} failed (exit {code:?})", arguments.join(" "))
                        } else {
                            message
                        }));
                    }
                    return Ok(());
                }
                JobEvent::Failed { message, .. } => {
                    return Err(GitRecoveryRefusal::Backend(message));
                }
                JobEvent::Started { .. } => {}
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn conflict_count(repo: &git2::Repository) -> Result<usize, GitRecoveryRefusal> {
    let index = repo
        .index()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let conflicts = index
        .conflicts()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    Ok(conflicts.count())
}

pub(super) fn recorded_merge_heads(
    repo: &git2::Repository,
) -> Result<Vec<git2::Oid>, GitRecoveryRefusal> {
    let contents = std::fs::read_to_string(repo.path().join("MERGE_HEAD"))
        .map_err(|_| GitRecoveryRefusal::NoRecordedStart)?;
    let heads = contents
        .lines()
        .map(|line| {
            git2::Oid::from_str(line.trim()).map_err(|_| GitRecoveryRefusal::NoRecordedStart)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if heads.is_empty() {
        return Err(GitRecoveryRefusal::NoRecordedStart);
    }
    Ok(heads)
}

/// Determine operation paths from its commits, never from arbitrary user-staged changes.
fn operation_touched_paths(
    repo: &git2::Repository,
    recorded: git2::Oid,
    operation: &str,
    rebase_current: Option<git2::Oid>,
) -> Result<std::collections::BTreeSet<PathBuf>, GitRecoveryRefusal> {
    let mut touched = std::collections::BTreeSet::new();
    let index = repo
        .index()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let mut conflicts = index
        .conflicts()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    for conflict in conflicts.by_ref() {
        let conflict =
            conflict.map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
        if let Some(entry) = conflict
            .ancestor
            .as_ref()
            .or(conflict.our.as_ref())
            .or(conflict.their.as_ref())
        {
            touched.insert(git_path(&entry.path).ok_or_else(|| {
                GitRecoveryRefusal::Backend("unsupported index path encoding".into())
            })?);
        }
    }
    drop(conflicts);
    let incoming = match operation {
        "merge" => recorded_merge_heads(repo)?,
        "cherry-pick" => vec![
            repo.refname_to_id("CHERRY_PICK_HEAD")
                .map_err(|_| GitRecoveryRefusal::NoRecordedStart)?,
        ],
        "revert" => vec![
            repo.refname_to_id("REVERT_HEAD")
                .map_err(|_| GitRecoveryRefusal::NoRecordedStart)?,
        ],
        "rebase" => rebase_current.into_iter().collect(),
        _ => Vec::new(),
    };
    for incoming in incoming {
        let commit = repo
            .find_commit(incoming)
            .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
        let recorded_commit = repo
            .find_commit(recorded)
            .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
        // Recompute the merge rather than treating every incoming path as changed: a change
        // already present on our branch is not permission to discard later user edits there.
        let merged = if operation == "merge" {
            Some(
                repo.merge_commits(&recorded_commit, &commit, None)
                    .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?,
            )
        } else {
            None
        };
        let base = if operation == "merge" {
            Some(recorded)
        } else if commit.parent_count() > 0 {
            Some(
                commit
                    .parent_id(0)
                    .map_err(|_| GitRecoveryRefusal::NoRecordedStart)?,
            )
        } else {
            None
        };
        let base_tree = base
            .map(|base| repo.find_commit(base).and_then(|commit| commit.tree()))
            .transpose()
            .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
        let incoming_tree = commit
            .tree()
            .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
        let diff = if let Some(merged) = &merged {
            repo.diff_tree_to_index(base_tree.as_ref(), Some(merged), None)
        } else {
            repo.diff_tree_to_tree(base_tree.as_ref(), Some(&incoming_tree), None)
        }
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
        for delta in diff.deltas() {
            for path in [delta.old_file().path(), delta.new_file().path()]
                .into_iter()
                .flatten()
            {
                touched.insert(path.to_path_buf());
            }
        }
    }
    Ok(touched)
}

/// Unrelated changes an abort would overwrite. Git's merge abort preserves working-only changes.
fn risky_changes(
    repo: &git2::Repository,
    touched: &std::collections::BTreeSet<PathBuf>,
    index_only: bool,
) -> Result<Vec<String>, GitRecoveryRefusal> {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo
        .statuses(Some(&mut options))
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let mut risky = Vec::new();
    for entry in statuses.iter() {
        let path = git_path(entry.path_bytes()).ok_or_else(|| {
            GitRecoveryRefusal::Backend("unsupported status path encoding".into())
        })?;
        let status = entry.status();
        // A conflicted entry belongs to the operation; so does a change the operation made.
        if status.contains(git2::Status::CONFLICTED) || touched.contains(&path) {
            continue;
        }
        if index_only
            && !status.intersects(
                git2::Status::INDEX_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_DELETED
                    | git2::Status::INDEX_RENAMED
                    | git2::Status::INDEX_TYPECHANGE,
            )
        {
            continue;
        }
        risky.push(path.display().to_string());
    }
    risky.sort();
    Ok(risky)
}

/// What a whole-file replacement WOULD do, computed before anything is written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitReplacementPlan {
    pub choice: GitConflictChoice,
    /// Every path the replacement would write, with its content and Git file mode.
    pub files: Vec<(PathBuf, String, u32)>,
}

impl GitReplacementPlan {
    /// The list a confirmation must show: the files and the side they would take.
    #[must_use]
    pub fn preview(&self) -> String {
        let paths = self
            .files
            .iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "replace {} file(s) with the {} side: {paths}",
            self.files.len(),
            self.choice.name()
        )
    }
}

/// Build a replacement plan for the given paths. Nothing is written: every side's content is
/// read FIRST, so a refusal leaves the repository and the files untouched.
pub fn plan_replacement(
    repository: &Path,
    paths: &[PathBuf],
    choice: GitConflictChoice,
) -> Result<GitReplacementPlan, GitConflictResolveError> {
    if paths.is_empty() {
        return Err(GitConflictResolveError::NoConflict(
            "(no paths given)".into(),
        ));
    }
    let mut files = Vec::new();
    let repo =
        git2::Repository::open(repository).map_err(|_| GitConflictResolveError::NotRepository)?;
    let workdir = repo
        .workdir()
        .ok_or(GitConflictResolveError::NotRepository)?;
    for path in paths {
        let destination = workdir.join(conflict_relative_path(&repo, path)?);
        // Replacing a symlink would replace the alias, not the selected repository file.
        let mut parent = destination.as_path();
        while parent != workdir {
            if std::fs::symlink_metadata(parent).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(GitConflictResolveError::NoConflict(
                    path.display().to_string(),
                ));
            }
            parent = parent
                .parent()
                .ok_or(GitConflictResolveError::NotRepository)?;
        }
        let (content, mode) = read_side(repository, &destination, choice)?;
        files.push((destination, content, mode));
    }
    Ok(GitReplacementPlan { choice, files })
}

#[derive(Debug, Error)]
#[error("replacement failed at `{path}`: {error}; already replaced: {replaced}", path = .path.display(), replaced = .written.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "))]
pub struct GitReplacementError {
    pub path: PathBuf,
    pub written: Vec<PathBuf>,
    pub error: GitConflictResolveError,
}

/// Apply a previewed plan, reporting the paths already written if a later write fails.
pub fn apply_replacement(plan: &GitReplacementPlan) -> Result<Vec<PathBuf>, GitReplacementError> {
    let mut written = Vec::new();
    for (path, content, mode) in &plan.files {
        let result = if matches!(mode, 0o100644 | 0o100755) {
            crate::files::atomic_write_with_executable(path, content.as_bytes(), *mode == 0o100755)
                .map_err(|error| GitConflictResolveError::Backend(error.to_string()))
        } else {
            Err(GitConflictResolveError::NotText(path.display().to_string()))
        };
        if let Err(error) = result {
            return Err(GitReplacementError {
                path: path.clone(),
                written,
                error,
            });
        }
        written.push(path.clone());
    }
    Ok(written)
}
