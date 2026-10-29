//! Git runtime, accepted snapshots and hunk actions. No mutable editor/UI authority.
use crate::{
    editor::{BufferId, EditorState},
    ui::{UiGitSign, UiGitSignKind},
    workspace::git::{
        GitChangePlanRequest, GitDiffLayer, GitDiffLineKind, GitFileStatus, GitHeadExpectation,
        GitHunk, GitHunkBufferInput, GitHunkGeneration, GitHunkId, GitHunkPolicy, GitHunkService,
        GitHunkSignKind, GitHunkSnapshot, GitMutationGeneration, GitMutationKind,
        GitMutationError, GitMutationRequest, GitMutationService, GitRepositoryId, GitResetHunkRequest,
        GitStatusGeneration, GitStatusPolicy, GitStatusService, GitStatusSnapshot,
        GitCommitGeneration, GitCommitRequest, GitCommitResult, GitCommitService,
        GitBranchGeneration, GitBranchList, GitBranchListRequest, GitBranchRequest,
        GitBranchService,
        GitRemoteEvent, GitRemoteGeneration, GitRemoteJournal, GitRemoteOutcome, GitRemoteService,
        GitUpdateReview,
        GitLogGeneration, GitLogPage, GitLogPageRequest, GitLogPatch, GitLogPatchRequest,
        GitLogRequest, GitLogService,
        GitUnsavedDocument, aggregate_git_status, git_head_id, git_hunk_signs,
        git_index_fingerprint, git_tip_message, worktree_hash,
    },
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

type SignSource = (u64, u64, GitRepositoryId, PathBuf, BufferId, u64);

#[derive(Default)]
pub(super) struct GitSession {
    status: GitStatusService,
    status_generation: Option<GitStatusGeneration>,
    status_root_revision: Option<u64>,
    status_snapshot: Option<GitStatusSnapshot>,
    decorations: BTreeMap<PathBuf, GitFileStatus>,
    silent: bool,
    silent_attempted: Option<(u64, PathBuf)>,
    hunks: GitHunkService,
    hunk_generation: Option<GitHunkGeneration>,
    hunk_expected: Option<(u64, u64, GitRepositoryId, PathBuf, GitDiffLayer)>,
    hunk_action: Option<i8>,
    hunk_snapshot: Option<GitHunkSnapshot>,
    sign_hunks: GitHunkService,
    sign_generation: Option<GitHunkGeneration>,
    sign_source: Option<SignSource>,
    selected: Option<(GitHunkSnapshot, GitHunk, PathBuf)>,
    /// The commit the history surface had selected, so a toggle restores the row the user was
    /// on rather than jumping back to the top.
    history_selection: Option<String>,
    /// The commit message the composer last held.
    ///
    /// Kept after the composer closes so the change surface can show what is about to be
    /// committed, and so cancelling the composer does not throw away what the user typed. It
    /// is cleared when a commit actually lands.
    commit_draft: String,
    mutation: GitMutationService,
    mutation_generation: Option<GitMutationGeneration>,
    commit: GitCommitService,
    commit_generation: Option<GitCommitGeneration>,
    log: GitLogService,
    log_generation: Option<GitLogGeneration>,
    log_page: Option<GitLogPage>,
    log_patch: Option<GitLogPatch>,
    /// Explicit user choice of history presentation; survives re-opening history.
    history_graph: bool,
    branches: GitBranchService,
    branch_generation: Option<GitBranchGeneration>,
    branch_list: Option<GitBranchList>,
    remote: GitRemoteService,
    remote_generation: Option<GitRemoteGeneration>,
    /// The reviewed update waiting for its single keystroke. It carries the tips it was built
    /// from, so an apply refuses a branch that moved after the review.
    remote_review: Option<GitUpdateReview>,
    remote_journal: GitRemoteJournal,
    /// The previewed whole-file replacement waiting for its confirmation. The plan is rebuilt
    /// when the answer arrives, so a plan previewed against one repository state is never applied
    /// to another.
    pending_replacement: Option<crate::workspace::git::GitReplacementPlan>,
    /// The revision a rewrite preview named, carried to the apply so the two cannot disagree.
    previewed_rewrite: Option<crate::workspace::git::GitHeadId>,
    /// A previewed cherry-pick or revert waiting for its answer. The preview carries the HEAD it
    /// was computed against, so the apply is checked against THAT value.
    pending_pick: Option<crate::workspace::git::GitPickPreview>,
    pending_destructive: Option<GitDestructiveAction>,
    /// Which of the two lists the last request asked for, so one poll can install it.
    branch_view_branches: bool,
    /// Path the outstanding plan was built for, so a refusal can name it.
    plan_path: Option<PathBuf>,
    /// Set when the last mutation was refused for unsaved editor content.
    unsaved_refusal: Option<PathBuf>,
}

/// Terminal outcome of one accepted mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GitMutationOutcome {
    /// Worktree changes were discarded.
    Reset,
    /// Index changes were applied.
    Staged,
    /// Refused before any write because the path has unsaved editor changes.
    UnsavedDocument,
}

/// Whichever log result arrived, so no outcome is dropped.
pub(super) enum GitLogOutcome {
    Page(GitLogPage),
    Patch(GitLogPatch),
}

/// A destructive Git action awaiting confirmation. It names which operation and which
/// target, so an answer can never be applied to a different one than was shown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum GitDestructiveAction {
    DeleteBranch(String),
    RemoveWorktree(String),
}

/// Whichever branch-list outcome arrived, so no outcome is dropped.
pub(super) enum GitBranchOutcome {
    List(GitBranchList),
    Changed,
}

pub(super) struct HunkReady {
    pub path: PathBuf,
    pub action: Option<i8>,
}

impl GitSession {
    pub(super) fn status(&self) -> Option<&GitStatusSnapshot> {
        self.status_snapshot.as_ref()
    }
    /// Hold the previewed replacement until its confirmation is answered.
    pub(super) fn set_pending_replacement(
        &mut self,
        plan: crate::workspace::git::GitReplacementPlan,
    ) {
        self.pending_replacement = Some(plan);
    }
    /// Take the previewed replacement, so it can be answered exactly once.
    pub(super) fn take_pending_replacement(
        &mut self,
    ) -> Option<crate::workspace::git::GitReplacementPlan> {
        self.pending_replacement.take()
    }
    /// Whether a replacement is waiting for an answer.
    pub(super) fn has_pending_replacement(&self) -> bool {
        self.pending_replacement.is_some()
    }
    /// Test-facing: whether a remote request is still in flight. The production path learns the
    /// end of a request from the event it polls, so this exists to let a test wait on it.
    #[cfg(test)]
    pub(super) fn remote_pending(&self) -> bool {
        self.remote_generation.is_some()
    }
    pub(super) fn remote_review(&self) -> Option<&GitUpdateReview> {
        self.remote_review.as_ref()
    }
    /// Test-facing: the unknown-outcome journal, so a test can assert nothing was journaled.
    #[cfg(test)]
    pub(super) fn remote_journal(&self) -> &GitRemoteJournal {
        &self.remote_journal
    }
    /// Ask for an explicit fetch. The remote is named, never guessed here.
    pub(super) fn start_fetch(
        &mut self,
        repository: &Path,
        remote: &str,
        deadline: Duration,
    ) -> Result<(), String> {
        self.remote_generation = Some(
            self.remote
                .fetch(repository.to_path_buf(), remote.to_owned(), deadline)
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }
    /// Ask what a fast-forward would do. Nothing moves.
    pub(super) fn start_update_review(
        &mut self,
        repository: &Path,
        remote: &str,
        branch: &str,
        deadline: Duration,
    ) -> Result<(), String> {
        self.remote_generation = Some(
            self.remote
                .review_update(
                    repository.to_path_buf(),
                    remote.to_owned(),
                    branch.to_owned(),
                    deadline,
                )
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }
    /// Apply the reviewed plan with the single keystroke the contract asks for.
    pub(super) fn apply_update(&mut self, deadline: Duration) -> Result<(), String> {
        let Some(review) = self.remote_review.clone() else {
            return Err("review the update before applying it".into());
        };
        self.remote_generation = Some(
            self.remote
                .apply_update(review, deadline)
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }
    /// Push a local ref to a named destination. The refspec is non-forced and the remote value
    /// the push was based on is checked first.
    pub(super) fn start_push(
        &mut self,
        repository: &Path,
        remote: &str,
        source: &str,
        destination: &str,
        lease: Option<git2::Oid>,
        deadline: Duration,
    ) -> Result<(), String> {
        self.remote_generation = Some(
            self.remote
                .push(
                    repository.to_path_buf(),
                    remote.to_owned(),
                    source.to_owned(),
                    destination.to_owned(),
                    lease,
                    deadline,
                )
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }
    /// One poll per call, exactly once.
    pub(super) fn poll_remote(&mut self) -> Option<GitRemoteEvent> {
        let event = self.remote.poll()?;
        self.remote_generation = None;
        {
            let GitRemoteEvent::Fetched { outcome, .. } = &event;
            match outcome {
                GitRemoteOutcome::Reviewed(review) => self.remote_review = Some(review.clone()),
                GitRemoteOutcome::Updated { .. }
                | GitRemoteOutcome::Pushed { .. }
                | GitRemoteOutcome::Merged { .. }
                | GitRemoteOutcome::MergeConflicted { .. } => {
                    self.remote_review = None;
                }
                GitRemoteOutcome::Refused(_) | GitRemoteOutcome::Failed(_) => {
                    self.remote_review = None;
                }
                GitRemoteOutcome::Unknown(error) => {
                    // An unknown outcome is journaled and NEVER retried.
                    let _ = self
                        .remote_journal
                        .record_unknown("remote", &error.to_string());
                    self.remote_review = None;
                }
                GitRemoteOutcome::Fetched { .. } => {}
            }
        }
        Some(event)
    }
    pub(super) fn hunks(&self) -> Option<&GitHunkSnapshot> {
        self.hunk_snapshot.as_ref()
    }
    pub(super) fn decorations(&self) -> &BTreeMap<PathBuf, GitFileStatus> {
        &self.decorations
    }
    pub(super) fn has_selection(&self) -> bool {
        self.selected.is_some()
    }
    pub(super) fn retry_status(&mut self) {
        self.silent_attempted = None;
    }

    pub(super) fn clear_signs(&mut self) {
        self.sign_hunks.cancel();
        self.sign_generation = None;
        self.sign_source = None;
    }
    fn clear_hunks(&mut self) {
        self.hunks.cancel();
        self.hunk_generation = None;
        self.hunk_expected = None;
        self.hunk_action = None;
        self.hunk_snapshot = None;
        self.selected = None;
        self.mutation.cancel();
        self.mutation_generation = None;
        self.plan_path = None;
        self.unsaved_refusal = None;
        self.commit.cancel();
        self.commit_generation = None;
        self.log.cancel();
        self.log_generation = None;
        self.log_page = None;
        self.log_patch = None;
        self.branches.cancel();
        self.branch_generation = None;
        self.pending_destructive = None;
    }
    pub(super) fn clear(&mut self) {
        self.clear_hunks();
        self.clear_signs();
        self.status.cancel();
        self.status_generation = None;
        self.status_root_revision = None;
        self.status_snapshot = None;
        self.decorations.clear();
        self.silent = false;
    }

    pub(super) fn needs_status(&mut self, revision: u64, root: Option<&Path>) -> bool {
        let current = self
            .status_snapshot
            .as_ref()
            .is_some_and(|s| s.root_revision == revision && root == Some(s.scope_root.as_path()));
        if current || self.status_generation.is_some() {
            return false;
        }
        let Some(root) = root else {
            return false;
        };
        if self
            .silent_attempted
            .as_ref()
            .is_some_and(|(old, path)| *old == revision && path == root)
        {
            return false;
        }
        self.silent_attempted = Some((revision, root.to_owned()));
        true
    }
    pub(super) fn refresh(
        &mut self,
        editor: &EditorState,
        root: Option<&Path>,
        revision: u64,
        report: bool,
    ) -> Result<(), String> {
        self.clear();
        self.silent = !report;
        let result = match root {
            Some(root) => {
                let unsaved = Self::unsaved_documents(editor, root);
                self.status
                    .start_with_unsaved(
                        GitStatusPolicy {
                            root: root.to_owned(),
                            root_revision: revision,
                            maximum_files: 100_000,
                            maximum_path_bytes: 16 * 1024 * 1024,
                            deadline: Duration::from_secs(5),
                        },
                        unsaved,
                    )
                    .map_err(|e| e.to_string())
            }
            None => Err("workspace root is unavailable".into()),
        };
        match result {
            Ok(generation) => {
                self.status_generation = Some(generation);
                self.status_root_revision = Some(revision);
                Ok(())
            }
            Err(error) => {
                self.silent = false;
                if report { Err(error) } else { Ok(()) }
            }
        }
    }
    pub(super) fn poll_status(&mut self, revision: u64) -> Result<bool, String> {
        if self.status_generation.is_some() && self.status_root_revision != Some(revision) {
            self.clear();
            return Ok(true);
        }
        match self.status.poll() {
            Ok(Some(snapshot))
                if Some(snapshot.generation) == self.status_generation
                    && Some(snapshot.root_revision) == self.status_root_revision =>
            {
                self.decorations = aggregate_git_status(&snapshot.scope_root, &snapshot.entries);
                self.status_snapshot = Some(snapshot.clone());
                self.status_generation = None;
                self.status_root_revision = None;
                self.silent = false;
                Ok(true)
            }
            Ok(_) => Ok(false),
            Err(error) => {
                let report = !self.silent;
                self.clear();
                if report {
                    Err(error.to_string())
                } else {
                    Ok(true)
                }
            }
        }
    }

    // GIT_SIGN_FOREGROUND_BEGIN: immutable ownership and worker queue coordination only.
    fn sign_source(
        &self,
        editor: &EditorState,
        revision: u64,
        enabled: bool,
    ) -> Option<SignSource> {
        if !enabled {
            return None;
        }
        let status = self
            .status_snapshot
            .as_ref()
            .filter(|s| s.root_revision == revision)?;
        let id = editor.active_buffer_id()?;
        let buffer = editor.buffers.get(id)?;
        let path = buffer.path.as_ref()?;
        let relative = path.strip_prefix(&status.scope_root).ok()?;
        if !buffer.dirty
            && !status
                .entries
                .iter()
                .any(|entry| entry.relative_path == relative)
        {
            return None;
        }
        Some((
            status.root_revision,
            status.revision,
            status.repository_id,
            relative.to_owned(),
            id,
            buffer.text.revision().0,
        ))
    }
    /// True means the previous derived signs must be cleared.
    pub(super) fn reconcile_signs(
        &mut self,
        editor: &EditorState,
        revision: u64,
        enabled: bool,
    ) -> bool {
        let source = self.sign_source(editor, revision, enabled);
        if source == self.sign_source {
            return false;
        }
        self.clear_signs();
        let Some(source) = source else {
            return true;
        };
        let Some(status) = self.status_snapshot.as_ref() else {
            return true;
        };
        let policy = Self::policy(status, source.3.clone(), GitDiffLayer::Unstaged);
        let Some(buffer) = editor
            .buffers
            .get(source.4)
            .filter(|b| b.text.revision().0 == source.5 && b.path.is_some())
        else {
            return true;
        };
        if let Ok(generation) = self.sign_hunks.start_buffer(
            policy,
            GitHunkBufferInput {
                revision: source.5,
                bytes: buffer.text.to_bytes(),
            },
        ) {
            self.sign_generation = Some(generation);
            self.sign_source = Some(source);
        }
        true
    }
    pub(super) fn poll_signs(
        &mut self,
        editor: &EditorState,
        revision: u64,
        enabled: bool,
    ) -> Option<Vec<UiGitSign>> {
        let source = self.sign_source.as_ref()?;
        if self.sign_source(editor, revision, enabled).as_ref() != Some(source) {
            self.clear_signs();
            return Some(Vec::new());
        }
        match self.sign_hunks.poll() {
            Ok(Some(snapshot)) => {
                if Some(snapshot.generation) != self.sign_generation
                    || snapshot.root_revision != source.0
                    || snapshot.status_revision != source.1
                    || snapshot.repository_id != source.2
                    || snapshot.file != source.3
                    || snapshot.buffer_revision != Some(source.5)
                {
                    self.clear_signs();
                    return Some(Vec::new());
                }
                self.sign_generation = None;
                Some(
                    git_hunk_signs(snapshot)
                        .into_iter()
                        .map(|sign| UiGitSign {
                            buffer: source.4,
                            buffer_revision: source.5,
                            root_revision: source.0,
                            status_revision: source.1,
                            repository_id: source.2,
                            line: sign.line,
                            kind: match sign.kind {
                                GitHunkSignKind::Added => UiGitSignKind::Added,
                                GitHunkSignKind::Changed => UiGitSignKind::Changed,
                                GitHunkSignKind::Deleted => UiGitSignKind::Deleted,
                            },
                        })
                        .collect(),
                )
            }
            Ok(None) => None,
            Err(_) => {
                self.clear_signs();
                Some(Vec::new())
            }
        }
    }
    // GIT_SIGN_FOREGROUND_END

    pub(in crate::editor::session) fn policy(
        status: &GitStatusSnapshot,
        file: PathBuf,
        layer: GitDiffLayer,
    ) -> GitHunkPolicy {
        GitHunkPolicy {
            scope_root: status.scope_root.clone(),
            repository_id: status.repository_id,
            status_revision: status.revision,
            root_revision: status.root_revision,
            file,
            layer,
            maximum_hunks: 10_000,
            maximum_lines: 100_000,
            maximum_bytes: 8 * 1024 * 1024,
            deadline: Duration::from_secs(5),
        }
    }
    pub(super) fn start_hunks(
        &mut self,
        path: Option<&Path>,
        revision: u64,
        action: Option<i8>,
        layer: GitDiffLayer,
    ) -> Result<(), String> {
        self.clear_hunks();
        let status = self
            .status_snapshot
            .as_ref()
            .filter(|s| s.root_revision == revision)
            .ok_or("Git status is unavailable; refresh first")?;
        let path = path.ok_or("active buffer has no file path")?;
        let relative = path
            .strip_prefix(&status.scope_root)
            .map_err(|_| "active file is outside Git scope")?;
        let generation = self
            .hunks
            .start(Self::policy(status, relative.to_owned(), layer))
            .map_err(|e| e.to_string())?;
        self.hunk_generation = Some(generation);
        self.hunk_expected = Some((
            status.root_revision,
            status.revision,
            status.repository_id,
            path.to_owned(),
            layer,
        ));
        self.hunk_action = action;
        Ok(())
    }
    pub(super) fn poll_hunks(&mut self) -> Result<Option<HunkReady>, String> {
        let Some(expected) = self.hunk_expected.as_ref() else {
            return Ok(None);
        };
        if self.status_snapshot.as_ref().is_none_or(|s| {
            s.root_revision != expected.0
                || s.revision != expected.1
                || s.repository_id != expected.2
        }) {
            self.clear_hunks();
            return Ok(None);
        }
        match self.hunks.poll() {
            Ok(Some(snapshot))
                if Some(snapshot.generation) == self.hunk_generation
                    && snapshot.layer == expected.4 =>
            {
                self.hunk_snapshot = Some(snapshot.clone());
                let path = expected.3.clone();
                self.hunk_generation = None;
                self.hunk_expected = None;
                Ok(Some(HunkReady {
                    path,
                    action: self.hunk_action.take(),
                }))
            }
            Ok(Some(_)) => {
                // A read that came back for another layer is stale, not a candidate.
                self.clear_hunks();
                Ok(None)
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.clear_hunks();
                Err(error.to_string())
            }
        }
    }
    pub(super) fn select(
        &mut self,
        revision: u64,
        repository: GitRepositoryId,
        root_revision: u64,
        path: &Path,
        id: GitHunkId,
    ) -> bool {
        let Some(status) = self.status_snapshot.as_ref().filter(|s| {
            s.revision == revision
                && s.repository_id == repository
                && s.root_revision == root_revision
        }) else {
            return false;
        };
        let Some(snapshot) = self.hunk_snapshot.as_ref().filter(|s| {
            s.status_revision == revision
                && s.repository_id == repository
                && s.root_revision == root_revision
                && status.scope_root.join(&s.file) == path
        }) else {
            return false;
        };
        let Some(hunk) = snapshot.hunks.iter().find(|h| h.id == id) else {
            return false;
        };
        self.selected = Some((snapshot.clone(), hunk.clone(), path.to_owned()));
        true
    }
    pub(super) fn navigate(
        &mut self,
        direction: i8,
        line: u32,
    ) -> Result<(PathBuf, usize), String> {
        let snapshot = self
            .hunk_snapshot
            .as_ref()
            .ok_or("Git hunks are unavailable")?;
        let hunk = if direction < 0 {
            snapshot
                .hunks
                .iter()
                .rev()
                .find(|h| h.new_start < line)
                .or_else(|| snapshot.hunks.last())
        } else {
            snapshot
                .hunks
                .iter()
                .find(|h| h.new_start > line)
                .or_else(|| snapshot.hunks.first())
        }
        .ok_or("no Git hunk is available in this buffer")?;
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is unavailable; refresh first")?;
        let path = status.scope_root.join(&snapshot.file);
        let line = hunk.new_start.saturating_sub(1) as usize;
        self.selected = Some((snapshot.clone(), hunk.clone(), path.clone()));
        Ok((path, line))
    }
    pub(super) fn preview(&mut self) -> Result<(String, String), String> {
        let snapshot = self
            .hunk_snapshot
            .as_ref()
            .ok_or("Git hunks are unavailable")?;
        let hunk = self
            .selected
            .as_ref()
            .filter(|(s, _, _)| s.revision == snapshot.revision)
            .map(|(_, h, _)| h)
            .or_else(|| snapshot.hunks.first())
            .ok_or("no Git hunk is available in this buffer")?;
        let text = hunk
            .lines
            .iter()
            .map(|line| {
                let prefix = match line.kind {
                    GitDiffLineKind::Context => ' ',
                    GitDiffLineKind::Add => '+',
                    GitDiffLineKind::Delete => '-',
                };
                format!("{prefix}{}", line.text.trim_end_matches('\n'))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let header = hunk.header.trim().to_owned();
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is unavailable; refresh first")?;
        self.selected = Some((
            snapshot.clone(),
            hunk.clone(),
            status.scope_root.join(&snapshot.file),
        ));
        Ok((header, text))
    }
    /// Bounded, sorted unsaved editor content for one root, taken from real buffer state.
    /// The Git owner holds no editor authority: it receives this as data and only attributes it.
    fn unsaved_documents(editor: &EditorState, root: &Path) -> Vec<GitUnsavedDocument> {
        let mut documents = editor
            .buffers
            .values()
            .filter(|buffer| buffer.dirty)
            .filter_map(|buffer| {
                let relative = buffer.path.as_deref()?.strip_prefix(root).ok()?;
                // Revision zero is the never-edited identity and carries no source. Emitting it
                // would make the whole status read fail validation, which would take Git status
                // down over one buffer instead of omitting one entry.
                let document_revision = buffer.text.revision().0;
                if document_revision == 0 || relative.as_os_str().is_empty() {
                    return None;
                }
                Some(GitUnsavedDocument::new(
                    relative.to_owned(),
                    document_revision,
                ))
            })
            .collect::<Vec<_>>();
        documents.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        documents.dedup_by(|a, b| a.relative_path == b.relative_path);
        documents
    }
    fn captured_head(status: &GitStatusSnapshot) -> Result<GitHeadExpectation, String> {
        Ok(match git_head_id(&status.repository).map_err(|e| e.to_string())? {
            Some(id) => GitHeadExpectation::At(id),
            None => GitHeadExpectation::Unborn,
        })
    }
    /// Locate the active buffer inside the current status snapshot.
    fn active_relative(
        &self,
        editor: &EditorState,
        revision: u64,
    ) -> Result<(PathBuf, PathBuf), String> {
        let status = self
            .status_snapshot
            .as_ref()
            .filter(|s| s.root_revision == revision)
            .ok_or("Git status is unavailable; refresh first")?;
        let id = editor.active_buffer_id().ok_or("no active buffer")?;
        let path = editor.buffers[id]
            .path
            .as_deref()
            .ok_or("active buffer has no file path")?;
        let relative = path
            .strip_prefix(&status.scope_root)
            .map_err(|_| "active file is outside Git scope")?
            .to_owned();
        Ok((relative, status.scope_root.clone()))
    }
    /// Start a whole-file or line-scoped plan for the active buffer. Every plan carries the
    /// captured index, worktree, HEAD and unsaved revisions.
    pub(super) fn start_plan(
        &mut self,
        editor: &EditorState,
        kind: GitMutationKind,
        revision: u64,
        trusted: bool,
        selected_lines: Vec<u32>,
    ) -> Result<(), String> {
        self.start_plan_for(editor, kind, revision, trusted, selected_lines, None)
    }

    /// The same plan for an EXPLICIT file, given relative to the scope root.
    ///
    /// The change surface labels `s` and `u` by focus, so it stages the row under the cursor
    /// rather than the active buffer's file. A caller that names no file keeps exactly the
    /// behaviour it had, which is the active buffer's path.
    pub(super) fn start_plan_for(
        &mut self,
        editor: &EditorState,
        kind: GitMutationKind,
        revision: u64,
        trusted: bool,
        selected_lines: Vec<u32>,
        file: Option<PathBuf>,
    ) -> Result<(), String> {
        if !trusted {
            return Err("Git staging requires a trusted workspace".into());
        }
        let (relative, scope_root) = match file {
            Some(relative) => {
                let status = self
                    .status_snapshot
                    .as_ref()
                    .filter(|status| status.root_revision == revision)
                    .ok_or("Git status is unavailable; refresh first")?;
                (relative, status.scope_root.clone())
            }
            None => self.active_relative(editor, revision)?,
        };
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is unavailable; refresh first")?;
        let from_worktree = matches!(
            kind,
            GitMutationKind::StageFile | GitMutationKind::StageHunk | GitMutationKind::StageLines
        );
        let layer = if matches!(
            kind,
            GitMutationKind::UnstageFile | GitMutationKind::UnstageHunk
        ) {
            GitDiffLayer::Staged
        } else {
            GitDiffLayer::Unstaged
        };
        let policy = Self::policy(status, relative.clone(), layer);
        let mut plan = GitChangePlanRequest {
            kind,
            policy,
            hunk_snapshot_revision: 0,
            hunk_id: None,
            selected: None,
            selected_lines,
            expected_index: Some(git_index_fingerprint(&status.repository).map_err(|e| e.to_string())?),
            expected_worktree: None,
            expected_head: Self::captured_head(status)?,
            expected_document_revision: None,
            unsaved_documents: Self::unsaved_documents(editor, &scope_root),
            maximum_patch_bytes: 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        if from_worktree {
            plan.hunk_snapshot_revision = 1;
            plan.expected_worktree = Some(
                worktree_hash(&scope_root.join(&relative))
                    .map_err(|e| e.to_string())?
                    .0,
            );
        }
        if matches!(
            kind,
            GitMutationKind::StageHunk | GitMutationKind::UnstageHunk | GitMutationKind::StageLines
        ) {
            let (snapshot, hunk, _) = self
                .selected
                .as_ref()
                .ok_or("select a current Git hunk first")?;
            // Unstaging acts on the index, so it must be driven by a staged-layer selection
            // rather than by whatever hunk happens to be selected.
            if kind == GitMutationKind::UnstageHunk && snapshot.layer != GitDiffLayer::Staged {
                return Err("select a staged Git hunk first".into());
            }
            plan.hunk_snapshot_revision = snapshot.revision;
            plan.hunk_id = Some(hunk.id);
            plan.selected = Some(hunk.clone());
            plan.expected_index = Some(snapshot.index_fingerprint);
            plan.expected_worktree = snapshot.worktree_sha256;
        }
        self.plan_path = Some(relative);
        self.unsaved_refusal = None;
        self.mutation_generation =
            Some(self.mutation.start(GitMutationRequest::Plan(plan)).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Read the branch and worktree list. Both views come from one request, so a single poll
    /// installs whichever was asked for and no result can be consumed twice.
    pub(super) fn start_branches(
        &mut self,
        repository: &Path,
        branches: bool,
    ) -> Result<(), String> {
        self.branch_view_branches = branches;
        self.branch_list = None;
        let request = GitBranchRequest::List(GitBranchListRequest {
            repository: repository.to_path_buf(),
            deadline: Duration::from_secs(10),
        });
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    pub(super) fn set_pending_destructive(&mut self, action: GitDestructiveAction) {
        self.pending_destructive = Some(action);
    }
    pub(super) fn take_pending_destructive(&mut self) -> Option<GitDestructiveAction> {
        self.pending_destructive.take()
    }
    /// Delete a local branch. Every refusal is the model's.
    pub(super) fn start_branch_delete(&mut self, repository: &Path, name: &str) -> Result<(), String> {
        let request = GitBranchRequest::Delete {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            deadline: Duration::from_secs(10),
        };
        self.branch_list = None;
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Remove a worktree and its directory. This never deletes a branch.
    pub(super) fn start_worktree_remove(&mut self, repository: &Path, name: &str) -> Result<(), String> {
        let request = GitBranchRequest::RemoveWorktree {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            deadline: Duration::from_secs(10),
        };
        self.branch_list = None;
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Create a branch, optionally from a start point.
    pub(super) fn start_branch_create(
        &mut self,
        repository: &Path,
        name: &str,
        start: Option<String>,
    ) -> Result<(), String> {
        let request = GitBranchRequest::Create {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            start,
            deadline: Duration::from_secs(10),
        };
        self.branch_list = None;
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Create a local branch tracking a remote branch.
    pub(super) fn start_branch_track(
        &mut self,
        repository: &Path,
        name: &str,
        remote_branch: &str,
    ) -> Result<(), String> {
        let request = GitBranchRequest::TrackRemote {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            remote_branch: remote_branch.to_owned(),
            deadline: Duration::from_secs(10),
        };
        self.branch_list = None;
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    pub(super) fn start_branch_rename(
        &mut self,
        repository: &Path,
        name: &str,
        to: &str,
    ) -> Result<(), String> {
        let request = GitBranchRequest::Rename {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            to: to.to_owned(),
            deadline: Duration::from_secs(10),
        };
        self.branch_list = None;
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Create a worktree for a branch. The destination and occupancy checks are the model's.
    pub(super) fn start_worktree_create(
        &mut self,
        repository: &Path,
        name: &str,
        path: &Path,
        reference: &str,
    ) -> Result<(), String> {
        let request = GitBranchRequest::CreateWorktree {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            path: path.to_path_buf(),
            reference: reference.to_owned(),
            deadline: Duration::from_secs(10),
        };
        self.branch_list = None;
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// One poll reporting whichever branch outcome arrived.
    pub(super) fn poll_branch_outcome(&mut self) -> Result<Option<GitBranchOutcome>, String> {
        if self.branch_generation.is_none() {
            return Ok(None);
        }
        match self.branches.poll() {
            Ok(Some(crate::workspace::git::GitBranchResult::List(list))) => {
                if Some(list.generation) != self.branch_generation {
                    return Ok(None);
                }
                self.branch_generation = None;
                self.branch_list = Some(list.clone());
                Ok(Some(GitBranchOutcome::List(list.clone())))
            }
            Ok(Some(crate::workspace::git::GitBranchResult::Changed)) => {
                self.branch_generation = None;
                Ok(Some(GitBranchOutcome::Changed))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.branch_generation = None;
                Err(error.to_string())
            }
        }
    }
    pub(super) fn branch_view_is_branches(&self) -> bool {
        self.branch_view_branches
    }
    /// Read a bounded page of history for the current repository.
    pub(super) fn start_log(
        &mut self,
        repository: &Path,
        filter: Option<String>,
        offset: usize,
        maximum_commits: usize,
    ) -> Result<(), String> {
        let request = GitLogPageRequest {
            repository: repository.to_path_buf(),
            filter,
            offset,
            maximum_commits,
            deadline: Duration::from_secs(10),
        };
        self.log_page = None;
        self.log_generation =
            Some(self.log.start(GitLogRequest::Page(request)).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Read the real patch for one commit, honouring whether it is a merge.
    pub(super) fn start_log_patch(
        &mut self,
        repository: &Path,
        commit: crate::workspace::git::GitHeadId,
        merge: bool,
    ) -> Result<(), String> {
        let deadline = Duration::from_secs(10);
        let maximum_bytes = 1024 * 1024;
        let request = if merge {
            GitLogPatchRequest::Merged {
                repository: repository.to_path_buf(),
                commit,
                maximum_bytes,
                deadline,
            }
        } else {
            GitLogPatchRequest::Commit {
                repository: repository.to_path_buf(),
                commit,
                maximum_bytes,
                deadline,
            }
        };
        self.log_patch = None;
        self.log_generation =
            Some(self.log.start(GitLogRequest::Patch(request)).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Read the combined patch for a range: an anchored commit up to a tip.
    pub(super) fn start_log_range(
        &mut self,
        repository: &Path,
        base: crate::workspace::git::GitHeadId,
        tip: crate::workspace::git::GitHeadId,
    ) -> Result<(), String> {
        self.log_patch = None;
        let request = GitLogPatchRequest::Range {
            repository: repository.to_path_buf(),
            base,
            tip,
            maximum_bytes: 1024 * 1024,
            deadline: Duration::from_secs(10),
        };
        self.log_generation =
            Some(self.log.start(GitLogRequest::Patch(request)).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Poll the log service once and report whichever outcome arrived. A single poll is
    /// required: the service is one-shot, so two polls in one handler can consume and drop
    /// the other outcome.
    pub(super) fn poll_log_outcome(&mut self) -> Result<Option<GitLogOutcome>, String> {
        if self.log_generation.is_none() {
            return Ok(None);
        }
        match self.log.poll() {
            Ok(Some(crate::workspace::git::GitLogResult::Page(page))) => {
                if Some(page.generation) != self.log_generation {
                    return Ok(None);
                }
                self.log_generation = None;
                self.log_page = Some(page.clone());
                Ok(Some(GitLogOutcome::Page(page.clone())))
            }
            Ok(Some(crate::workspace::git::GitLogResult::Patch(patch))) => {
                if Some(patch.generation) != self.log_generation {
                    return Ok(None);
                }
                self.log_generation = None;
                self.log_patch = Some(patch.clone());
                Ok(Some(GitLogOutcome::Patch(patch.clone())))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.log_generation = None;
                Err(error.to_string())
            }
        }
    }
    pub(super) fn history_graph(&self) -> bool {
        self.history_graph
    }
    pub(super) fn set_history_graph(&mut self, graph: bool) {
        self.history_graph = graph;
    }
    pub(super) fn history(&self) -> Option<&GitLogPage> {
        self.log_page.as_ref()
    }
    /// The tip commit, used as the far end of an anchored range.
    pub(super) fn tip_commit(&self) -> Option<crate::workspace::git::GitHeadId> {
        let status = self.status_snapshot.as_ref()?;
        git_head_id(&status.repository).ok().flatten()
    }
    /// The tip message, so an amend composer can show what it replaces.
    pub(super) fn tip_message(&self) -> Option<String> {
        let status = self.status_snapshot.as_ref()?;
        git_tip_message(&status.repository).ok().flatten()
    }
    /// Compose a commit from exactly the staged set, pinned to the captured HEAD and index.
    /// Set the revision a rewrite preview NAMED, so the apply is pinned to what the user was
    /// actually shown rather than to whatever HEAD happens to be when they answer.
    pub(super) fn set_pending_pick(&mut self, preview: crate::workspace::git::GitPickPreview) {
        self.pending_pick = Some(preview);
    }
    pub(super) fn has_pending_pick(&self) -> bool {
        self.pending_pick.is_some()
    }
    /// Answer the previewed pick. The preview is consumed exactly once.
    pub(super) fn take_pending_pick(&mut self) -> Option<crate::workspace::git::GitPickPreview> {
        self.pending_pick.take()
    }
    pub(super) fn set_previewed_rewrite(&mut self, head: Option<crate::workspace::git::GitHeadId>) {
        self.previewed_rewrite = head;
    }
    pub(super) fn start_commit(
        &mut self,
        message: String,
        amend: bool,
        revision: u64,
        trusted: bool,
    ) -> Result<(), String> {
        if !trusted {
            return Err("Git commit requires a trusted workspace".into());
        }
        let status = self
            .status_snapshot
            .as_ref()
            .filter(|s| s.root_revision == revision)
            .ok_or("Git status is unavailable; refresh first")?;
        // An AMEND is pinned to the revision the preview NAMED when there was one, because the
        // user answered a question about THAT revision; only a fresh commit re-derives HEAD.
        let previewed = self.previewed_rewrite.take();
        let expected_head = match (amend, previewed) {
            (true, Some(previewed)) => GitHeadExpectation::At(previewed),
            _ => Self::captured_head(status)?,
        };
        let request = GitCommitRequest {
            repository: status.repository.clone(),
            message,
            amend,
            expected_head,
            expected_index: git_index_fingerprint(&status.repository).map_err(|e| e.to_string())?,
            deadline: Duration::from_secs(5),
        };
        self.commit_generation = Some(self.commit.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    pub(super) fn poll_commit(&mut self) -> Result<Option<GitCommitResult>, String> {
        if self.commit_generation.is_none() {
            return Ok(None);
        }
        match self.commit.poll() {
            Ok(Some(result)) => {
                if Some(result.generation) != self.commit_generation {
                    return Ok(None);
                }
                let result = result.clone();
                self.clear();
                Ok(Some(result))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.commit_generation = None;
                Err(error.to_string())
            }
        }
    }
    pub(super) fn mutate(
        &mut self,
        editor: &EditorState,
        reset: bool,
        trusted: bool,
        revision: u64,
    ) -> Result<(), String> {
        if !trusted {
            return Err(if reset {
                "Git reset requires a trusted workspace"
            } else {
                "Git staging requires a trusted workspace"
            }
            .into());
        }
        let (snapshot, hunk, _) = self
            .selected
            .as_ref()
            .ok_or("select a current Git hunk first")?;
        let status = self
            .status_snapshot
            .as_ref()
            .filter(|s| {
                s.repository_id == snapshot.repository_id
                    && s.revision == snapshot.status_revision
                    && s.root_revision == revision
            })
            .ok_or("Git hunk is stale")?;
        if !reset && snapshot.layer == GitDiffLayer::Staged {
            return Err("that hunk is already staged; unstage it instead".into());
        }
        let policy = Self::policy(status, snapshot.file.clone(), GitDiffLayer::Unstaged);
        // Discarding worktree changes has no plan equivalent, so it keeps the legacy request
        // and its index-plus-worktree capture. Hunk staging migrated to the plan type, which
        // additionally captures HEAD and the unsaved revisions.
        if reset {
            let request = GitMutationRequest::ResetUnstagedHunk(GitResetHunkRequest {
                policy,
                hunk_snapshot_revision: snapshot.revision,
                hunk_id: hunk.id,
                selected: hunk.clone(),
                expected_index: Some(snapshot.index_fingerprint),
                expected_worktree: snapshot.worktree_sha256,
                maximum_patch_bytes: 1024 * 1024,
                deadline: Duration::from_secs(5),
            });
            self.mutation_generation =
                Some(self.mutation.start(request).map_err(|e| e.to_string())?);
            return Ok(());
        }
        let plan = GitChangePlanRequest {
            kind: GitMutationKind::StageHunk,
            policy,
            hunk_snapshot_revision: snapshot.revision,
            hunk_id: Some(hunk.id),
            selected: Some(hunk.clone()),
            selected_lines: Vec::new(),
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            expected_head: Self::captured_head(status)?,
            expected_document_revision: None,
            unsaved_documents: Self::unsaved_documents(editor, &status.scope_root),
            maximum_patch_bytes: 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        self.plan_path = Some(snapshot.file.clone());
        self.unsaved_refusal = None;
        self.mutation_generation =
            Some(self.mutation.start(GitMutationRequest::Plan(plan)).map_err(|e| e.to_string())?);
        Ok(())
    }
    pub(super) fn poll_mutation(&mut self) -> Result<Option<GitMutationOutcome>, String> {
        if self.mutation_generation.is_none() {
            return Ok(None);
        }
        match self.mutation.poll() {
            Ok(Some(result)) => {
                if Some(result.generation) != self.mutation_generation {
                    return Ok(None);
                }
                let outcome = if matches!(result.kind, GitMutationKind::ResetUnstagedHunk) {
                    GitMutationOutcome::Reset
                } else {
                    GitMutationOutcome::Staged
                };
                self.clear();
                Ok(Some(outcome))
            }
            Ok(None) => Ok(None),
            // A refusal for unsaved content is a decision for the user, not a failure: it is
            // reported as an outcome so the caller can offer the save-and-stage choice.
            Err(GitMutationError::UnsavedDocument) => {
                self.mutation_generation = None;
                self.unsaved_refusal = self.plan_path.clone();
                Ok(Some(GitMutationOutcome::UnsavedDocument))
            }
            Err(error) => {
                self.mutation_generation = None;
                self.plan_path = None;
                Err(error.to_string())
            }
        }
    }
    /// The path whose staging was refused for unsaved editor content.
    /// The commit message the composer last held.
    /// Remember the history selection across a re-listing, and take it back out when the new
    /// page is installed.
    pub(super) fn set_history_selection(&mut self, identity: Option<String>) {
        self.history_selection = identity;
    }

    pub(super) fn take_history_selection(&mut self) -> Option<String> {
        self.history_selection.take()
    }

    pub(super) fn commit_draft(&self) -> &str {
        &self.commit_draft
    }

    /// Remember the composer's message, so the change surface can show it and cancelling the
    /// composer does not discard it.
    pub(super) fn set_commit_draft(&mut self, draft: String) {
        self.commit_draft = draft;
    }

    #[allow(dead_code)]
    pub(super) fn unsaved_refusal(&self) -> Option<&Path> {
        self.unsaved_refusal.as_deref()
    }
}

#[cfg(test)]
impl GitSession {
    pub(super) fn status_pending(&self) -> bool {
        self.status_generation.is_some()
    }
    pub(super) fn signs_pending(&self) -> bool {
        self.sign_generation.is_some()
    }
    pub(super) fn hunks_pending(&self) -> bool {
        self.hunk_generation.is_some()
    }
    pub(super) fn branch_list(&self) -> Option<&GitBranchList> {
        self.branch_list.as_ref()
    }
    pub(super) fn branch_pending(&self) -> bool {
        self.branch_generation.is_some()
    }
    pub(super) fn log_pending(&self) -> bool {
        self.log_generation.is_some()
    }
    pub(super) fn commit_pending(&self) -> bool {
        self.commit_generation.is_some()
    }
    pub(super) fn mutation_pending(&self) -> bool {
        self.mutation_generation.is_some()
    }
    pub(super) fn silent(&self) -> bool {
        self.silent
    }
    pub(super) fn status_generation(&self) -> u64 {
        self.status.generation()
    }
    pub(super) fn sign_generation(&self) -> u64 {
        self.sign_hunks.generation()
    }
}

/// The diff pane's structure for one file, built from the hunks the product already reads.
///
/// The structured read carries both line numbers exactly, so this does not format them back
/// to patch text and re-parse: it maps them straight across and reuses the shared span
/// computation, so a diff built this way and a diff parsed from patch text agree about what
/// changed inside a line.
pub(in crate::editor::session) fn diff_preview_from_hunks(
    hunks: &[crate::workspace::git::GitHunk],
    provenance: &str,
    truncated: bool,
    total_lines: usize,
) -> crate::ui::DiffPreview {
    use crate::ui::{DiffHunk, DiffLine, DiffLineKind, DiffPreview};
    let mut previous_end: Option<u32> = None;
    let mut converted = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        let lines = hunk
            .lines
            .iter()
            .map(|line| DiffLine {
                kind: match line.kind {
                    crate::workspace::git::GitDiffLineKind::Context => DiffLineKind::Context,
                    crate::workspace::git::GitDiffLineKind::Add => DiffLineKind::Added,
                    crate::workspace::git::GitDiffLineKind::Delete => DiffLineKind::Removed,
                },
                old_number: line.old_line,
                new_number: line.new_line,
                text: line.text.trim_end_matches('\n').to_owned(),
                spans: Vec::new(),
            })
            .collect::<Vec<_>>();
        // The collapsed count comes from the hunk's own range, so it states the real number of
        // unchanged lines the view is not showing.
        let collapsed_before = match previous_end {
            Some(end) => hunk.old_start.saturating_sub(end),
            None => hunk.old_start.saturating_sub(1),
        };
        previous_end = Some(hunk.old_start.saturating_add(hunk.old_lines));
        converted.push(DiffHunk {
            // The hunk header arrives with its own line ending, which would render as a
            // replacement glyph inside the pane.
            header: hunk.header.trim_end().to_owned(),
            lines,
            collapsed_before: usize::try_from(collapsed_before).unwrap_or(usize::MAX),
        });
    }
    let mut preview = DiffPreview {
        hunks: converted,
        provenance: provenance.to_owned(),
        truncated,
        total_lines,
        layout: crate::ui::DiffLayout::Unified,
    };
    // The shared computation decides which spans changed, exactly as the parser path does.
    for hunk in &mut preview.hunks {
        crate::ui::emphasise_changed_spans(&mut hunk.lines);
    }
    preview
}

/// The colours a lane cycles through, in order.
///
/// These are the theme-backed styles the renderer already has rather than a new palette: a
/// dedicated graph palette would be a theme-schema change and nothing in the document asks for
/// one. The cycle is bounded, so beyond its length two lanes share a colour; what never
/// happens is a lane CHANGING colour, because a lane's colour is a function of its index.
const LANE_STYLES: [crate::ui::Style; 6] = [
    crate::ui::Style::Accent,
    crate::ui::Style::StatusAccent,
    crate::ui::Style::Warning,
    crate::ui::Style::Status,
    crate::ui::Style::Error,
    crate::ui::Style::Search,
];

impl crate::workspace::git::HistoryGraphRow {
    /// The colour of a lane, assigned by its index so it is stable for the life of the view.
    #[must_use]
    pub fn lane_style(lane: usize) -> crate::ui::Style {
        LANE_STYLES[lane % LANE_STYLES.len()]
    }

    /// This row's columns as cells: one per cell, each carrying its own glyph and the colour
    /// of the lane it belongs to.
    ///
    /// Every cell carries a GLYPH as well as a colour, so a terminal without colour keeps the
    /// structure. A bend takes the colour of the lane it bends into rather than of the column
    /// it happens to be drawn in.
    #[must_use]
    pub fn cells(&self) -> Vec<crate::ui::PickerCell> {
        // The edges past the first are the parents that bent out of this commit's lane, in the
        // order the bends are drawn.
        let bends: Vec<usize> = self.edges.iter().skip(1).map(|edge| edge.to).collect();
        let mut columns: Vec<crate::ui::PickerCell> = self
            .cells
            .iter()
            .enumerate()
            .map(|(column, cell)| {
            let lane = match cell {
                // The bends follow the lane area, so a bend's column is the lane area's width
                // plus its position in that run - not the commit's lane plus one, which only
                // coincides for a single-lane row.
                crate::workspace::git::GraphCellKind::BendRight => bends
                    .get(column.saturating_sub(self.lane_width))
                    .copied()
                    .unwrap_or(column),
                // The cells before the bends are one per lane, so the column IS the lane.
                _ => column,
            };
            crate::ui::PickerCell::glyph(
                cell.drawn_glyph().to_string(),
                Self::lane_style(lane),
            )
        })
        .collect();
        // A lane that closes into this commit is DRAWN closing into it, in the closing lane's
        // own column, rather than vanishing. The curve carries the closing lane's colour and
        // its glyph, so a terminal without colour keeps the structure.
        for edge in &self.converges {
            if edge.from == edge.to {
                continue;
            }
            let kind = if edge.from > edge.to {
                crate::workspace::git::GraphCellKind::BendRightClose
            } else {
                crate::workspace::git::GraphCellKind::BendLeft
            };
            if let Some(cell) = columns.get_mut(edge.from) {
                *cell = crate::ui::PickerCell::glyph(
                    kind.drawn_glyph().to_string(),
                    Self::lane_style(edge.from),
                );
            }
        }
        columns
    }
}

/// The ref and tag badges a history entry carries, as their own cells rather than text
/// appended to the subject, so they are drawn as badges and are never read as part of the
/// message.
///
#[must_use]
pub(in crate::editor::session) fn graph_badge_cells(
    entry: &crate::workspace::git::GitLogEntry,
) -> Vec<crate::ui::PickerCell> {
    entry
        .refs
        .iter()
        .map(|name| {
            let text = format!(" {name} ");
            // A badge takes its OWN width. Zero means "take the rest of the row" to the
            // renderer, so a zero-width badge swallows the subject beside it.
            let width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
            crate::ui::PickerCell::new(text, crate::ui::Style::StatusAccent, width)
        })
        .collect()
}

/// A commit's age, relative and readable: `2h`, `1d`, and so on.
///
/// Stated from the AUTHORED time rather than the committer time, because that is the time the
/// change was written and the one a reader is reasoning about.
#[must_use]
pub(in crate::editor::session) fn relative_age(authored_seconds: i64, now_seconds: i64) -> String {
    let delta = now_seconds.saturating_sub(authored_seconds).max(0);
    match delta {
        0..=59 => "now".to_owned(),
        60..=3599 => format!("{}m", delta / 60),
        3_600..=86_399 => format!("{}h", delta / 3_600),
        86_400..=2_591_999 => format!("{}d", delta / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", delta / 2_592_000),
        _ => format!("{}y", delta / 31_536_000),
    }
}

/// The columns a history row draws: the graph when it is on, then the short hash, the ref and
/// tag badges, the subject, the relative age and the author - each as its own column rather
/// than concatenated into the subject line.
///
/// The subject is the flexible column, so a narrow surface truncates the message rather than
/// pushing the age and author off the row. Badges are drawn with the graph, which is the
/// drawing that shows them.
#[must_use]
pub(in crate::editor::session) fn history_row_cells(
    entry: &crate::workspace::git::GitLogEntry,
    graph: Option<Vec<crate::ui::PickerCell>>,
    now_seconds: i64,
) -> Vec<crate::ui::PickerCell> {
    let identity = entry
        .id
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let short = identity.get(..7).unwrap_or(&identity).to_owned();
    let mut cells = graph.unwrap_or_default();
    // One column wider than the hash, so the gap before the badges or the subject is part of
    // the column rather than something the next cell has to remember to add.
    cells.push(crate::ui::PickerCell::new(
        short,
        crate::ui::Style::Muted,
        8,
    ));
    if !cells.is_empty() && !entry.refs.is_empty() {
        cells.extend(graph_badge_cells(entry));
    }
    cells.push(crate::ui::PickerCell::flex(
        entry.summary.clone(),
        crate::ui::Style::Default,
    ));

    // The leading space is the gap that keeps the right-aligned age from running straight
    // into the author.
    cells.push(crate::ui::PickerCell::new(
        format!(" {}", entry.author),
        crate::ui::Style::Muted,
        9,
    ));
    cells
}
