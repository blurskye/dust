//! Conflict provenance, read from the repository on demand.
//!
//! Conflict views show base/ours/theirs/result; choosing a side edits the result buffer through
//! ordinary transactions. Saving and staging are separate steps.
//!
//! Nothing here is cached. A conflict is a state of the repository that changes underneath the
//! editor, so every read goes back to the index and to the repository's own operation state.
use std::path::{Path, PathBuf};

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
    /// Abandon the operation. Always valid while an operation is paused.
    Abort,
}

impl GitOperationAction {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Continue => "continue",
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
            if operation == "merge" {
                actions.push(GitOperationAction::Continue);
            }
        } else {
            actions.push(GitOperationAction::Resolve);
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

/// Read the paused operation and every conflicted path from the repository, on demand.
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
            .map(|entry| PathBuf::from(String::from_utf8_lossy(&entry.path).into_owned()));
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
    let repo =
        git2::Repository::open(repository).map_err(|_| GitConflictResolveError::NotRepository)?;
    // Index conflict paths are RELATIVE to the worktree root, while a caller holds an absolute
    // buffer path, so the caller's path is made relative before comparison.
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo.workdir().unwrap_or(repository).join(path)
    };
    let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let wanted = match repo.workdir() {
        Some(workdir) => {
            let workdir = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
            canonical
                .strip_prefix(&workdir)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| path.to_path_buf())
        }
        None => path.to_path_buf(),
    };
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
            if Path::new(&String::from_utf8_lossy(&entry.path).into_owned()) == wanted.as_path() {
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
    let blob = repo
        .find_blob(side.id)
        .map_err(|error| GitConflictResolveError::Backend(error.message().to_owned()))?;
    String::from_utf8(blob.content().to_vec())
        .map_err(|_| GitConflictResolveError::NotText(wanted.display().to_string()))
}

/// Why a recovery action was refused. Every reason is decided BEFORE the first write.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitRecoveryRefusal {
    #[error("repository could not be opened")]
    NotRepository,
    #[error("no operation is paused, so there is nothing to {0}")]
    NotPaused(&'static str),
    #[error("{0} conflicted path(s) remain; resolve and stage them first")]
    ConflictsRemain(usize),
    #[error(
        "aborting would discard uncommitted changes in {0}; commit or stash them first, because \
         abort never throws work away"
    )]
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
}

/// Finish a paused operation by writing the merge commit its recorded parents describe.
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
    if operation != "merge" {
        return Err(GitRecoveryRefusal::Backend(format!(
            "continuing {operation} is not supported"
        )));
    }
    let remaining = conflict_count(&repo)?;
    if remaining > 0 {
        return Err(GitRecoveryRefusal::ConflictsRemain(remaining));
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
    // A merge records the operation's other head as a second parent; without it the commit would
    // silently be an ordinary commit and the merge would be lost.
    let merge_head = repo
        .find_reference("MERGE_HEAD")
        .and_then(|reference| {
            reference
                .target()
                .ok_or_else(|| git2::Error::from_str("MERGE_HEAD has no target"))
        })
        .and_then(|oid| repo.find_commit(oid))
        .map_err(|_| GitRecoveryRefusal::NoRecordedStart)?;
    let signature = repo
        .signature()
        .or_else(|_| git2::Signature::now("Dust", "dust@example.invalid"))
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let message = if message.trim().is_empty() {
        format!("Merge {}", &merge_head.id().to_string()[..8])
    } else {
        message.to_owned()
    };
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &[&head, &merge_head],
        )
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    drop(tree);
    drop(head);
    drop(merge_head);
    index
        .clear()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    repo.cleanup_state()
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    Ok(GitRecoveryOutcome {
        operation,
        commit: Some(oid),
        restored_to: None,
    })
}

/// Abandon a paused operation and return the branch to the revision git recorded for it.
///
/// There is deliberately NO hard-reset fallback: when the working tree holds changes that
/// returning to the recorded revision would destroy, the abort is REFUSED and names them.
pub fn abort_operation(repository: &Path) -> Result<GitRecoveryOutcome, GitRecoveryRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitRecoveryRefusal::NotRepository)?;
    let operation = operation_name(repo.state()).ok_or(GitRecoveryRefusal::NotPaused("abort"))?;
    let recorded = repo
        .find_reference("ORIG_HEAD")
        .and_then(|reference| {
            reference
                .target()
                .ok_or_else(|| git2::Error::from_str("ORIG_HEAD has no target"))
        })
        .map_err(|_| GitRecoveryRefusal::NoRecordedStart)?;
    // What the operation itself touched: the conflicted paths and the paths whose content the
    // merge changed. A change OUTSIDE that set is the user's, and abort must not discard it.
    let touched = merge_touched_paths(&repo, recorded)?;
    let risky = risky_changes(&repo, &touched)?;
    if !risky.is_empty() {
        return Err(GitRecoveryRefusal::WouldDiscard(risky.join(", ")));
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
    })
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

/// The paths the operation itself touched: conflicted paths plus paths that differ between the
/// recorded starting point and the current index.
fn merge_touched_paths(
    repo: &git2::Repository,
    recorded: git2::Oid,
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
            touched.insert(PathBuf::from(
                String::from_utf8_lossy(&entry.path).into_owned(),
            ));
        }
    }
    drop(conflicts);
    // Paths whose INDEX content differs from the recorded revision were changed by the
    // operation, not by the user.
    let recorded_tree = repo
        .find_commit(recorded)
        .and_then(|commit| commit.tree())
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let diff = repo
        .diff_tree_to_index(Some(&recorded_tree), Some(&index), None)
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    for delta in diff.deltas() {
        for path in [delta.old_file().path(), delta.new_file().path()]
            .into_iter()
            .flatten()
        {
            touched.insert(path.to_path_buf());
        }
    }
    Ok(touched)
}

/// Changes the user made that aborting would destroy: modified or untracked paths the operation
/// itself did not touch.
fn risky_changes(
    repo: &git2::Repository,
    touched: &std::collections::BTreeSet<PathBuf>,
) -> Result<Vec<String>, GitRecoveryRefusal> {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo
        .statuses(Some(&mut options))
        .map_err(|error| GitRecoveryRefusal::Backend(error.message().to_owned()))?;
    let mut risky = Vec::new();
    for entry in statuses.iter() {
        let Some(path) = entry.path() else {
            continue;
        };
        let path = PathBuf::from(path);
        let status = entry.status();
        // A conflicted entry belongs to the operation; so does a change the operation made.
        if status.contains(git2::Status::CONFLICTED) || touched.contains(&path) {
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
    /// Every path the replacement would write, with the content it would write.
    pub files: Vec<(PathBuf, String)>,
}

impl GitReplacementPlan {
    /// The list a confirmation must show: the files and the side they would take.
    #[must_use]
    pub fn preview(&self) -> String {
        let paths = self
            .files
            .iter()
            .map(|(path, _)| path.display().to_string())
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
    for path in paths {
        let content = read_side_content(repository, path, choice)?;
        files.push((path.clone(), content));
    }
    Ok(GitReplacementPlan { choice, files })
}

/// Apply a plan that was already previewed. Writes EXACTLY the files the plan names — the plan is
/// the authority, so nothing outside it is touched.
pub fn apply_replacement(
    plan: &GitReplacementPlan,
) -> Result<Vec<PathBuf>, GitConflictResolveError> {
    let mut written = Vec::new();
    for (path, content) in &plan.files {
        crate::files::atomic_write(path, content.as_bytes())
            .map_err(|error| GitConflictResolveError::Backend(error.to_string()))?;
        written.push(path.clone());
    }
    Ok(written)
}
