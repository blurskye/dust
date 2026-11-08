//! Git actions, accepted repository results and editor integration.
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::text::TextVersion;
use crate::git::GitBranchList;
use crate::git::GitBranchListRequest;
use crate::git::GitBranchRequest;
use crate::git::GitBranchRequestId;
use crate::git::GitBranchWorker;
use crate::git::GitChangePlanRequest;
use crate::git::GitDiffLayer;
use crate::git::GitDiffLineKind;
use crate::git::GitFileStatus;
use crate::git::GitHeadExpectation;
use crate::git::GitHunk;
use crate::git::GitHunkBufferInput;
use crate::git::GitHunkId;
use crate::git::GitHunkPolicy;
use crate::git::GitHunkRequestId;
use crate::git::GitHunkSignKind;
use crate::git::GitHunkSnapshot;
use crate::git::GitHunkWorker;
use crate::git::GitMutationError;
use crate::git::GitMutationKind;
use crate::git::GitMutationRequest;
use crate::git::GitMutationRequestId;
use crate::git::GitMutationWorker;
use crate::git::GitRepositoryId;
use crate::git::GitResetHunkRequest;
use crate::git::GitStatusPolicy;
use crate::git::GitStatusRequestId;
use crate::git::GitStatusSnapshot;
use crate::git::GitStatusWorker;
use crate::git::GitUnsavedDocument;
use crate::git::aggregate_git_status;
use crate::git::git_branch_name;
use crate::git::git_head_id;
use crate::git::git_hunk_signs;
use crate::git::git_index_fingerprint;
use crate::git::git_staged_signs;
use crate::git::index_line_in_buffer;
use crate::ui::UiGitSign;
use crate::ui::UiGitSignKind;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

type SignSource = (
    u64,
    u64,
    GitRepositoryId,
    PathBuf,
    BufferId,
    u64,
    TextVersion,
);

#[derive(Default)]
pub(super) struct Git {
    status: GitStatusWorker,
    status_request_id: Option<GitStatusRequestId>,
    status_root_revision: Option<u64>,
    status_snapshot: Option<GitStatusSnapshot>,
    branch: Option<String>,
    decorations: BTreeMap<PathBuf, GitFileStatus>,
    silent: bool,
    silent_attempted: Option<(u64, PathBuf)>,
    hunks: GitHunkWorker,
    hunk_request_id: Option<GitHunkRequestId>,
    hunk_expected: Option<(u64, u64, GitRepositoryId, PathBuf, GitDiffLayer)>,
    hunk_action: Option<GitHunkAction>,
    hunk_source: Option<(BufferId, TextVersion)>,
    hunk_selection_line: Option<u32>,
    hunk_snapshot: Option<GitHunkSnapshot>,
    reset_target: Option<(u64, GitHunkId, PathBuf)>,
    sign_hunks: GitHunkWorker,
    sign_request_id: Option<GitHunkRequestId>,
    /// The buffer's unstaged diff, held until the staged one is ready to go with it.
    unstaged_signs: Option<GitHunkSnapshot>,
    /// The active buffer's last unstaged diff, which places staged lines in the buffer.
    last_unstaged: Option<GitHunkSnapshot>,
    staged_sign_hunks: GitHunkWorker,
    staged_sign_request_id: Option<GitHunkRequestId>,
    /// What the staged diff was read for. It doesn't change as the buffer is typed in.
    staged_sign_source: Option<(u64, u64, GitRepositoryId, PathBuf)>,
    staged_signs: Option<GitHunkSnapshot>,
    sign_source: Option<SignSource>,
    sign_marks: Vec<(crate::editor::decorations::MarkId, UiGitSign)>,
    selected: Option<(GitHunkSnapshot, GitHunk, PathBuf)>,
    /// One change inside the selected hunk, when the cursor picked one out of several.
    selected_change: Vec<u32>,
    mutation: GitMutationWorker,
    mutation_request_id: Option<GitMutationRequestId>,
    branches: GitBranchWorker,
    branch_request_id: Option<GitBranchRequestId>,
    branch_root: Option<(u64, PathBuf)>,
    branch_list_scope: Option<(PathBuf, u64)>,
    show_worktrees: bool,
    /// The previewed whole-file replacement waiting for its confirmation. The plan is rebuilt
    /// when the answer arrives, so a plan previewed against one repository state is never applied
    /// to another.
    pending_replacement: Option<crate::git::GitReplacementPlan>,
    pending_destructive: Option<GitDestructiveAction>,
    /// Path the outstanding plan was built for, so a refusal can name it.
    plan_path: Option<PathBuf>,
    /// Set when the last mutation was refused for unsaved editor content.
    unsaved_refusal: Option<PathBuf>,
    /// The changes picker was asked for before status arrived; open it when it does.
    pub(super) open_changes_when_ready: bool,
}

/// Terminal outcome of one accepted mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GitMutationOutcome {
    /// Worktree changes were discarded.
    Reset,
    /// Index changes were applied; says which.
    Staged(&'static str),
    /// Refused before any write because the path has unsaved editor changes.
    UnsavedDocument,
}

/// A destructive Git action awaiting confirmation. It names which operation and which
/// target, so an answer can never be applied to a different one than was shown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum GitDestructiveAction {
    RemoveWorktree(String),
}

/// Whichever branch-list outcome arrived, so no outcome is dropped.
pub(super) enum GitBranchOutcome {
    List(GitBranchList),
    Changed,
}

/// What a key asked for while the hunks were still being read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GitHunkAction {
    Move(i8),
    Preview,
    Stage(u32),
    Unstage,
    StageLines(u32, u32),
    Reset,
}

pub(super) struct HunkReady {
    pub path: PathBuf,
    pub action: Option<GitHunkAction>,
}

impl Git {
    pub(in crate::app) fn status(&self) -> Option<&GitStatusSnapshot> {
        self.status_snapshot.as_ref()
    }
    /// The checked-out branch as of the last status result.
    pub(in crate::app) fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    /// Hold the previewed replacement until its confirmation is answered.
    pub(in crate::app) fn set_pending_replacement(&mut self, plan: crate::git::GitReplacementPlan) {
        self.pending_destructive = None;
        self.pending_replacement = Some(plan);
    }
    /// Take the previewed replacement, so it can be answered exactly once.
    pub(in crate::app) fn take_pending_replacement(
        &mut self,
    ) -> Option<crate::git::GitReplacementPlan> {
        self.pending_replacement.take()
    }
    pub(in crate::app) fn cancel_destructive(&mut self) {
        self.pending_replacement = None;
        self.pending_destructive = None;
    }
    /// Whether a replacement is waiting for an answer.
    pub(in crate::app) fn has_pending_replacement(&self) -> bool {
        self.pending_replacement.is_some()
    }
    pub(in crate::app) fn remember_reset_target(&mut self) {
        self.reset_target = self
            .selected
            .as_ref()
            .map(|(snapshot, hunk, path)| (snapshot.revision, hunk.id, path.clone()));
    }
    pub(in crate::app) fn take_reset_target(&mut self) -> bool {
        self.reset_target.take().is_some_and(|expected| {
            self.selected
                .as_ref()
                .is_some_and(|(snapshot, hunk, path)| {
                    expected == (snapshot.revision, hunk.id, path.clone())
                })
        })
    }
    pub(in crate::app) fn hunks_match(
        &self,
        editor: &EditorState,
        path: &Path,
        layer: GitDiffLayer,
        revision: u64,
    ) -> bool {
        self.hunk_source.as_ref().is_none_or(|(id, version)| {
            editor
                .buffers
                .get(*id)
                .is_some_and(|buffer| buffer.text.source_version() == *version)
        }) && self
            .hunk_snapshot
            .as_ref()
            .zip(self.status_snapshot.as_ref())
            .is_some_and(|(hunks, status)| {
                hunks.layer == layer
                    && hunks.root_revision == revision
                    && hunks.status_revision == status.revision
                    && hunks.repository_id == status.repository_id
                    && status.scope_root.join(&hunks.file) == path
            })
    }
    pub(in crate::app) fn hunks(&self) -> Option<&GitHunkSnapshot> {
        self.hunk_snapshot.as_ref()
    }
    pub(in crate::app) fn decorations(&self) -> &BTreeMap<PathBuf, GitFileStatus> {
        &self.decorations
    }
    pub(in crate::app) fn has_selection(&self) -> bool {
        self.selected.is_some()
    }
    pub(in crate::app) fn retry_status(&mut self) {
        self.silent_attempted = None;
    }

    pub(super) fn clear_signs(&mut self) {
        self.sign_hunks.cancel();
        self.sign_request_id = None;
        self.sign_source = None;
        self.unstaged_signs = None;
        self.last_unstaged = None;
    }
    /// Where a hunk's line sits in the buffer: a staged hunk counts the index's lines.
    pub(in crate::app) fn line_in_buffer(&self, layer: GitDiffLayer, line: usize) -> usize {
        self.last_unstaged
            .as_ref()
            .filter(|_| layer == GitDiffLayer::Staged)
            .and_then(|unstaged| index_line_in_buffer(line, &unstaged.hunks))
            .unwrap_or(line)
    }
    fn clear_hunks(&mut self) {
        self.hunks.cancel();
        self.hunk_request_id = None;
        self.hunk_expected = None;
        self.hunk_action = None;
        self.hunk_source = None;
        self.hunk_selection_line = None;
        self.hunk_snapshot = None;
        self.reset_target = None;
        self.selected = None;
        self.selected_change.clear();
    }
    pub(in crate::app) fn clear(&mut self) {
        if self.branch_list_scope.take().is_some() {
            self.branches.cancel();
            self.branch_request_id = None;
            self.branch_root = None;
            self.show_worktrees = false;
        }
        self.clear_status();
    }

    fn clear_status(&mut self) {
        self.clear_hunks();
        self.cancel_destructive();
        self.clear_signs();
        self.staged_sign_hunks.cancel();
        self.staged_sign_request_id = None;
        self.staged_sign_source = None;
        self.staged_signs = None;
        self.status.cancel();
        self.status_request_id = None;
        self.status_root_revision = None;
        self.status_snapshot = None;
        self.branch = None;
        self.decorations.clear();
        self.silent = false;
    }

    pub(in crate::app) fn reconcile_status_documents(&mut self, editor: &EditorState) {
        if let Some(snapshot) = &mut self.status_snapshot {
            let documents = Self::unsaved_documents(editor, &snapshot.scope_root);
            snapshot.reconcile_unsaved_documents(&documents, 100_000, 16 * 1024 * 1024);
        }
    }
    pub(in crate::app) fn needs_status(&mut self, revision: u64, root: Option<&Path>) -> bool {
        let current = self
            .status_snapshot
            .as_ref()
            .is_some_and(|s| s.root_revision == revision && root == Some(s.scope_root.as_path()));
        if current || self.status_request_id.is_some() {
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
    pub(in crate::app) fn refresh(
        &mut self,
        editor: &EditorState,
        root: Option<&Path>,
        revision: u64,
        report: bool,
    ) -> Result<(), String> {
        self.clear_status();
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
            Ok(request_id) => {
                self.status_request_id = Some(request_id);
                self.status_root_revision = Some(revision);
                Ok(())
            }
            Err(error) => {
                self.silent = false;
                if report { Err(error) } else { Ok(()) }
            }
        }
    }
    pub(in crate::app) fn poll_status(&mut self, revision: u64) -> Result<bool, String> {
        if self.status_request_id.is_some() && self.status_root_revision != Some(revision) {
            self.clear();
            return Ok(true);
        }
        match self.status.poll() {
            Ok(Some(snapshot))
                if Some(snapshot.request_id) == self.status_request_id
                    && Some(snapshot.root_revision) == self.status_root_revision =>
            {
                self.decorations = aggregate_git_status(&snapshot.scope_root, &snapshot.entries);
                self.branch = git_branch_name(&snapshot.repository).ok().flatten();
                self.status_snapshot = Some(snapshot.clone());
                self.status_request_id = None;
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
            buffer.text.source_version(),
        ))
    }
    /// True means the diff source changed. Anchored signs can stay while it refreshes.
    pub(in crate::app) fn reconcile_signs(
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
        let staged_policy = Self::policy(status, source.3.clone(), GitDiffLayer::Staged);
        let Some(buffer) = editor
            .buffers
            .get(source.4)
            .filter(|b| b.text.source_version() == source.6 && b.path.is_some())
        else {
            return true;
        };
        // Remember failures too; retry only when the source changes.
        self.sign_source = Some(source.clone());
        if buffer.text.len_bytes() > policy.maximum_bytes {
            return true;
        }
        if let Ok(request_id) = self.sign_hunks.start_buffer(
            policy,
            GitHunkBufferInput {
                revision: source.5,
                bytes: buffer.text.to_bytes(),
            },
        ) {
            self.sign_request_id = Some(request_id);
            let staged_source = (source.0, source.1, source.2, source.3.clone());
            if self.staged_sign_source.as_ref() != Some(&staged_source) {
                // Without a staged diff the unstaged signs still show.
                self.staged_signs = None;
                self.staged_sign_request_id = self.staged_sign_hunks.start(staged_policy).ok();
                self.staged_sign_source = Some(staged_source);
            }
        }
        true
    }
    pub(in crate::app) fn poll_signs(
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
        match self.staged_sign_hunks.poll() {
            Ok(Some(snapshot)) => {
                if Some(snapshot.request_id) == self.staged_sign_request_id {
                    self.staged_signs = Some(snapshot.clone());
                    self.staged_sign_request_id = None;
                }
            }
            Ok(None) => {}
            Err(_) => self.staged_sign_request_id = None,
        }
        match self.sign_hunks.poll() {
            Ok(Some(snapshot)) => {
                if Some(snapshot.request_id) != self.sign_request_id
                    || snapshot.root_revision != source.0
                    || snapshot.status_revision != source.1
                    || snapshot.repository_id != source.2
                    || snapshot.file != source.3
                    || snapshot.buffer_revision != Some(source.5)
                {
                    self.clear_signs();
                    return Some(Vec::new());
                }
                self.sign_request_id = None;
                self.unstaged_signs = Some(snapshot.clone());
                self.last_unstaged = Some(snapshot.clone());
            }
            Ok(None) => {}
            Err(crate::git::GitHunkError::Unmerged) => {
                let source = source.clone();
                self.clear_signs();
                // An unmerged index has no baseline; retry only when the source changes.
                self.sign_source = Some(source);
                return Some(Vec::new());
            }
            Err(_) => {
                self.sign_request_id = None;
                self.unstaged_signs = None;
                self.last_unstaged = None;
                return Some(Vec::new());
            }
        }
        if self.staged_sign_request_id.is_some() {
            return None;
        }
        let unstaged = self.unstaged_signs.take()?;
        let staged = self
            .staged_signs
            .as_ref()
            .map_or_else(Vec::new, |staged| git_staged_signs(staged, &unstaged));
        let sign = |sign: crate::git::GitHunkSign, staged: bool| UiGitSign {
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
            staged,
        };
        Some(
            git_hunk_signs(&unstaged)
                .into_iter()
                .map(|unstaged| sign(unstaged, false))
                .chain(staged.into_iter().map(|staged| sign(staged, true)))
                .collect(),
        )
    }
    // GIT_SIGN_FOREGROUND_END

    pub(in crate::app) fn policy(
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
    pub(in crate::app) fn start_hunks(
        &mut self,
        editor: &EditorState,
        path: Option<&Path>,
        revision: u64,
        action: Option<GitHunkAction>,
        layer: GitDiffLayer,
    ) -> Result<(), String> {
        let selection_line = self
            .selected
            .as_ref()
            .filter(|(snapshot, _, selected)| {
                Some(selected.as_path()) == path && snapshot.layer == layer
            })
            .and_then(|_| self.selected_line());
        self.clear_hunks();
        let status = self
            .status_snapshot
            .as_ref()
            .filter(|s| s.root_revision == revision)
            .ok_or("Git status is not ready; is this a Git repository?")?;
        let path = path.ok_or("active buffer has no file path")?;
        let relative = path
            .strip_prefix(&status.scope_root)
            .map_err(|_| "active file is outside Git scope")?;
        let policy = Self::policy(status, relative.to_owned(), layer);
        let request_id = if layer == GitDiffLayer::Unstaged {
            let (id, buffer) = editor
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_deref() == Some(path))
                .ok_or("Git hunk buffer is no longer open")?;
            if buffer.text.len_bytes() > policy.maximum_bytes {
                return Err("Git hunk buffer exceeds the size limit".into());
            }
            // Clean buffers use the disk snapshot needed by stage/reset. Unsaved buffers
            // use their displayed text, so navigation and previews follow their line numbers.
            let request_id = if buffer.dirty {
                self.hunks.start_buffer(
                    policy,
                    GitHunkBufferInput {
                        revision: buffer.text.revision().0,
                        bytes: buffer.text.to_bytes(),
                    },
                )
            } else {
                self.hunks.start(policy)
            }
            .map_err(|e| e.to_string())?;
            self.hunk_source = Some((id, buffer.text.source_version()));
            request_id
        } else {
            self.hunks.start(policy).map_err(|e| e.to_string())?
        };
        self.hunk_request_id = Some(request_id);
        self.hunk_expected = Some((
            status.root_revision,
            status.revision,
            status.repository_id,
            path.to_owned(),
            layer,
        ));
        self.hunk_action = action;
        self.hunk_selection_line = selection_line;
        Ok(())
    }
    pub(in crate::app) fn poll_hunks(
        &mut self,
        editor: &EditorState,
    ) -> Result<Option<HunkReady>, String> {
        if self.hunk_source.as_ref().is_some_and(|(id, version)| {
            editor
                .buffers
                .get(*id)
                .is_none_or(|buffer| buffer.text.source_version() != *version)
        }) {
            self.clear_hunks();
            return Ok(None);
        }
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
                if Some(snapshot.request_id) == self.hunk_request_id
                    && snapshot.layer == expected.4 =>
            {
                self.hunk_snapshot = Some(snapshot.clone());
                let path = expected.3.clone();
                self.hunk_request_id = None;
                self.hunk_expected = None;
                // Keep the earlier selection when a refreshed cursor is outside every hunk.
                if let Some(line) = self.hunk_selection_line.take() {
                    self.select_at(line);
                }
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
    pub(in crate::app) fn select(
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
    pub(in crate::app) fn reconcile_hunk_path(&mut self, editor: &EditorState) {
        let active = editor
            .active_buffer_id()
            .and_then(|id| editor.buffers[id].path.as_deref());
        let expected = self.hunk_snapshot.as_ref().and_then(|snapshot| {
            self.status_snapshot
                .as_ref()
                .map(|status| status.scope_root.join(&snapshot.file))
        });
        if expected.as_deref().is_some_and(|path| Some(path) != active) {
            self.clear_hunks();
        }
    }
    pub(in crate::app) fn navigate(
        &mut self,
        direction: i8,
        line: u32,
    ) -> Result<(PathBuf, usize), String> {
        let snapshot = self
            .hunk_snapshot
            .as_ref()
            .ok_or("Git hunks are unavailable")?;
        let changes = snapshot
            .hunks
            .iter()
            .flat_map(|hunk| hunk.change_lines().into_iter().map(move |at| (at, hunk)))
            .collect::<Vec<_>>();
        let (target, hunk) = if direction < 0 {
            changes
                .iter()
                .rev()
                .find(|(at, _)| *at < line)
                .or_else(|| changes.last())
        } else {
            changes
                .iter()
                .find(|(at, _)| *at > line)
                .or_else(|| changes.first())
        }
        .copied()
        .ok_or("no Git hunk is available in this buffer")?;
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is not ready; is this a Git repository?")?;
        let path = status.scope_root.join(&snapshot.file);
        let line = target.saturating_sub(1) as usize;
        self.selected_change = hunk.change_at(target);
        self.selected = Some((snapshot.clone(), hunk.clone(), path.clone()));
        Ok((path, line))
    }
    pub(in crate::app) fn selected_line(&self) -> Option<u32> {
        let (_, hunk, _) = self.selected.as_ref()?;
        hunk.change_lines()
            .into_iter()
            .find(|line| hunk.change_at(*line) == self.selected_change)
            .or(Some(hunk.new_start))
    }
    /// Select the hunk under the cursor, as gitsigns acts on the hunk you are in. Elsewhere the
    /// earlier selection stands.
    pub(in crate::app) fn select_at(&mut self, line: u32) {
        let Some(snapshot) = self.hunk_snapshot.as_ref() else {
            return;
        };
        // A staged hunk counts index lines, which unstaged changes above it shift in the buffer.
        let staged_line = |hunk: &GitHunk| {
            (hunk.new_start..hunk.new_start + hunk.new_lines.max(1)).find(|at| {
                self.line_in_buffer(GitDiffLayer::Staged, at.saturating_sub(1) as usize) as u32 + 1
                    == line
            })
        };
        let Some((hunk, line)) = snapshot.hunks.iter().find_map(|hunk| {
            if snapshot.layer == GitDiffLayer::Staged {
                staged_line(hunk).map(|at| (hunk, at))
            } else {
                hunk.covers_line(line).then_some((hunk, line))
            }
        }) else {
            return;
        };
        let Some(status) = self.status_snapshot.as_ref() else {
            return;
        };
        let path = status.scope_root.join(&snapshot.file);
        self.selected_change = hunk.change_at(line);
        self.selected = Some((snapshot.clone(), hunk.clone(), path));
    }
    /// Select the hunk with changes on these one-based lines and return those changes.
    pub(in crate::app) fn select_lines(
        &mut self,
        start: u32,
        end: u32,
    ) -> Result<Vec<u32>, String> {
        let snapshot = self
            .hunk_snapshot
            .as_ref()
            .ok_or("Git hunks are unavailable")?;
        let mut matches = snapshot.hunks.iter().filter_map(|hunk| {
            let lines = hunk.changes_on_lines(start, end);
            (!lines.is_empty()).then_some((hunk, lines))
        });
        let (hunk, lines) = matches
            .next()
            .ok_or("no unstaged Git change on those lines")?;
        if matches.next().is_some() {
            return Err("stage lines from one Git hunk at a time".into());
        }
        let path = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is unavailable")?
            .scope_root
            .join(&snapshot.file);
        self.selected_change.clear();
        self.selected = Some((snapshot.clone(), hunk.clone(), path));
        Ok(lines)
    }
    /// The file changed on disk, so the hunks have to be read again.
    pub(in crate::app) fn forget_hunks(&mut self) {
        if self.mutation_request_id.is_none() {
            self.clear_hunks();
        }
    }
    pub(in crate::app) fn preview(&mut self) -> Result<(String, String), String> {
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
        let change = (hunk.lines.iter().enumerate())
            .filter(|(index, _)| {
                self.selected_change.is_empty() || self.selected_change.contains(&(*index as u32))
            })
            .map(|(_, line)| line);
        let text = change
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
            .ok_or("Git status is not ready; is this a Git repository?")?;
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
        Ok(
            match git_head_id(&status.repository).map_err(|e| e.to_string())? {
                Some(id) => GitHeadExpectation::At(id),
                None => GitHeadExpectation::Unborn,
            },
        )
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
            .ok_or("Git status is not ready; is this a Git repository?")?;
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
    pub(in crate::app) fn start_plan(
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
    pub(in crate::app) fn start_plan_for(
        &mut self,
        editor: &EditorState,
        kind: GitMutationKind,
        revision: u64,
        trusted: bool,
        selected_lines: Vec<u32>,
        file: Option<PathBuf>,
    ) -> Result<(), String> {
        if self.mutation_request_id.is_some() {
            return Err("a Git mutation is already running".into());
        }
        if !trusted {
            return Err("Git staging requires a trusted workspace".into());
        }
        let (relative, scope_root) = match file {
            Some(relative) => {
                let status = self
                    .status_snapshot
                    .as_ref()
                    .filter(|status| status.root_revision == revision)
                    .ok_or("Git status is not ready; is this a Git repository?")?;
                (relative, status.scope_root.clone())
            }
            None => self.active_relative(editor, revision)?,
        };
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is not ready; is this a Git repository?")?;
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
            expected_index: Some(
                git_index_fingerprint(&status.repository).map_err(|e| e.to_string())?,
            ),
            expected_worktree: None,
            expected_head: Self::captured_head(status)?,
            expected_document_revision: None,
            unsaved_documents: Self::unsaved_documents(editor, &scope_root),
            maximum_patch_bytes: 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        if matches!(
            kind,
            GitMutationKind::StageHunk | GitMutationKind::UnstageHunk | GitMutationKind::StageLines
        ) {
            let (snapshot, hunk, _) = self
                .selected
                .as_ref()
                .ok_or("select a current Git hunk first")?;
            if snapshot.file != relative {
                return Err("Git hunk belongs to another file".into());
            }
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
        self.mutation_request_id = Some(
            self.mutation
                .start(GitMutationRequest::Plan(plan))
                .map_err(|e| e.to_string())?,
        );
        Ok(())
    }
    /// Read the worktree list.
    pub(in crate::app) fn start_branches(
        &mut self,
        repository: &Path,
        revision: u64,
    ) -> Result<(), String> {
        // A running list will supply the picker; mutations already reopen it on completion.
        if self.branch_request_id.is_some() {
            return Ok(());
        }
        let request = GitBranchRequest::List(GitBranchListRequest {
            repository: repository.to_path_buf(),
            deadline: Duration::from_secs(10),
        });
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is not ready")?;
        self.branch_request_id = Some(self.branches.start(request).map_err(|e| e.to_string())?);
        self.branch_root = Some((revision, status.scope_root.clone()));
        self.branch_list_scope = Some((repository.to_path_buf(), revision));
        Ok(())
    }
    pub(in crate::app) fn set_pending_destructive(&mut self, action: GitDestructiveAction) {
        self.pending_replacement = None;
        self.pending_destructive = Some(action);
    }
    pub(in crate::app) fn take_pending_destructive(&mut self) -> Option<GitDestructiveAction> {
        self.pending_destructive.take()
    }
    /// Remove a worktree and its directory. This never deletes a branch.
    pub(in crate::app) fn start_worktree_remove(
        &mut self,
        editor: &EditorState,
        repository: &Path,
        name: &str,
    ) -> Result<(), String> {
        if self.branch_request_id.is_some() {
            return Err("a Git worktree request is already running".into());
        }
        let repo = git2::Repository::open(repository).map_err(|e| e.to_string())?;
        let worktree = repo.find_worktree(name).map_err(|e| e.to_string())?;
        let path = crate::files::canonicalize(worktree.path()).map_err(|e| e.to_string())?;
        if editor.buffers.values().any(|buffer| {
            buffer.dirty
                && buffer
                    .path
                    .as_ref()
                    .is_some_and(|file| file.starts_with(&path))
        }) {
            return Err("worktree has unsaved editor changes".into());
        }
        let request = GitBranchRequest::RemoveWorktree {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            deadline: Duration::from_secs(10),
        };
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is not ready")?;
        self.branch_request_id = Some(self.branches.start(request).map_err(|e| e.to_string())?);
        self.branch_root = Some((status.root_revision, status.scope_root.clone()));
        Ok(())
    }
    /// Create a worktree for a branch. The destination and occupancy checks are the model's.
    /// The worktree list reopens when the running change finishes.
    pub(in crate::app) fn show_worktrees_after_change(&mut self) {
        self.show_worktrees = true;
    }
    pub(in crate::app) fn take_show_worktrees(&mut self) -> bool {
        std::mem::take(&mut self.show_worktrees)
    }
    pub(in crate::app) fn start_worktree_create(
        &mut self,
        repository: &Path,
        name: &str,
        path: &Path,
        reference: &str,
    ) -> Result<(), String> {
        if self.branch_request_id.is_some() {
            return Err("a Git worktree request is already running".into());
        }
        let request = GitBranchRequest::CreateWorktree {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            path: path.to_path_buf(),
            reference: reference.to_owned(),
            deadline: Duration::from_secs(10),
        };
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is not ready")?;
        self.branch_request_id = Some(self.branches.start(request).map_err(|e| e.to_string())?);
        self.branch_root = Some((status.root_revision, status.scope_root.clone()));
        Ok(())
    }
    /// One poll reporting whichever branch outcome arrived.
    pub(in crate::app) fn poll_branch_outcome(
        &mut self,
        root_revision: u64,
        root: &Path,
    ) -> Result<Option<GitBranchOutcome>, String> {
        if self
            .branch_list_scope
            .as_ref()
            .is_some_and(|(repository, expected)| {
                *expected != root_revision
                    || self
                        .status()
                        .is_some_and(|status| status.repository != *repository)
            })
        {
            self.branches.cancel();
            self.branch_request_id = None;
            self.branch_list_scope = None;
            self.branch_root = None;
            self.show_worktrees = false;
        }
        if self.branch_request_id.is_none() {
            return Ok(None);
        }
        if !self
            .branch_root
            .as_ref()
            .is_some_and(|(revision, path)| *revision == root_revision && path == root)
        {
            self.branches.cancel();
            self.branch_request_id = None;
            self.branch_root = None;
            self.show_worktrees = false;
            return Ok(None);
        }
        match self.branches.poll() {
            Ok(Some(crate::git::GitBranchResult::List(list))) => {
                if Some(list.request_id) != self.branch_request_id {
                    return Ok(None);
                }
                self.branch_request_id = None;
                self.branch_list_scope = None;
                Ok(Some(GitBranchOutcome::List(list.clone())))
            }
            Ok(Some(crate::git::GitBranchResult::Changed)) => {
                self.branch_request_id = None;
                Ok(Some(GitBranchOutcome::Changed))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.branch_request_id = None;
                self.show_worktrees = false;
                Err(error.to_string())
            }
        }
    }
    pub(in crate::app) fn mutate(
        &mut self,
        editor: &EditorState,
        reset: bool,
        trusted: bool,
        revision: u64,
    ) -> Result<(), String> {
        self.mutate_for(editor, reset, trusted, revision, None)
    }

    fn mutate_for(
        &mut self,
        editor: &EditorState,
        reset: bool,
        trusted: bool,
        revision: u64,
        file: Option<PathBuf>,
    ) -> Result<(), String> {
        if self.mutation_request_id.is_some() {
            return Err("a Git mutation is already running".into());
        }
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
        let active = match file {
            Some(file) => file,
            None => self.active_relative(editor, revision)?.0,
        };
        if snapshot.file != active {
            return Err("Git hunk belongs to another file".into());
        }
        let policy = Self::policy(status, snapshot.file.clone(), GitDiffLayer::Unstaged);
        // Discarding worktree changes has no plan equivalent, so it keeps the legacy request
        // and its index-plus-worktree capture. Hunk staging migrated to the plan type, which
        // additionally captures HEAD and the unsaved revisions.
        if reset {
            if snapshot.layer != GitDiffLayer::Unstaged {
                return Err("select an unstaged Git hunk to discard".into());
            }
            let path = status.scope_root.join(&snapshot.file);
            if editor
                .buffers
                .values()
                .any(|buffer| buffer.dirty && buffer.path.as_ref() == Some(&path))
            {
                return Err(
                    "save or close the unsaved buffer before discarding a disk hunk".into(),
                );
            }
            let request = GitMutationRequest::ResetUnstagedHunk(GitResetHunkRequest {
                policy,
                hunk_snapshot_revision: snapshot.revision,
                hunk_id: hunk.id,
                selected: hunk.clone(),
                selected_lines: self.selected_change.clone(),
                expected_index: Some(snapshot.index_fingerprint),
                expected_worktree: snapshot.worktree_sha256,
                maximum_patch_bytes: 1024 * 1024,
                deadline: Duration::from_secs(5),
            });
            self.mutation_request_id =
                Some(self.mutation.start(request).map_err(|e| e.to_string())?);
            return Ok(());
        }
        let plan = GitChangePlanRequest {
            kind: if self.selected_change.is_empty() {
                GitMutationKind::StageHunk
            } else {
                GitMutationKind::StageLines
            },
            policy,
            hunk_snapshot_revision: snapshot.revision,
            hunk_id: Some(hunk.id),
            selected: Some(hunk.clone()),
            selected_lines: self.selected_change.clone(),
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
        self.mutation_request_id = Some(
            self.mutation
                .start(GitMutationRequest::Plan(plan))
                .map_err(|e| e.to_string())?,
        );
        Ok(())
    }
    pub(in crate::app) fn poll_mutation(&mut self) -> Result<Option<GitMutationOutcome>, String> {
        if self.mutation_request_id.is_none() {
            return Ok(None);
        }
        match self.mutation.poll() {
            Ok(Some(result)) => {
                if Some(result.request_id) != self.mutation_request_id {
                    return Ok(None);
                }
                let outcome = match result.kind {
                    GitMutationKind::ResetUnstagedHunk => GitMutationOutcome::Reset,
                    GitMutationKind::StageFile => GitMutationOutcome::Staged("Git file staged"),
                    GitMutationKind::UnstageFile => GitMutationOutcome::Staged("Git file unstaged"),
                    GitMutationKind::UnstageHunk => GitMutationOutcome::Staged("Git hunk unstaged"),
                    _ => GitMutationOutcome::Staged("Git hunk staged"),
                };
                self.mutation_request_id = None;
                self.plan_path = None;
                self.unsaved_refusal = None;
                self.clear();
                Ok(Some(outcome))
            }
            Ok(None) => Ok(None),
            // A refusal for unsaved content is a decision for the user, not a failure: it is
            // reported as an outcome so the caller can offer the save-and-stage choice.
            Err(GitMutationError::UnsavedDocument) => {
                self.mutation_request_id = None;
                self.unsaved_refusal = self.plan_path.clone();
                Ok(Some(GitMutationOutcome::UnsavedDocument))
            }
            Err(error) => {
                self.mutation_request_id = None;
                self.plan_path = None;
                Err(error.to_string())
            }
        }
    }

    #[allow(dead_code)]
    pub(in crate::app) fn unsaved_refusal(&self) -> Option<&Path> {
        self.unsaved_refusal.as_deref()
    }
    pub(in crate::app) fn status_pending(&self) -> bool {
        self.status_request_id.is_some()
    }
    pub(in crate::app) fn mutation_pending(&self) -> bool {
        self.mutation_request_id.is_some()
    }
}

#[cfg(test)]
impl Git {
    pub(in crate::app) fn signs_pending(&self) -> bool {
        self.sign_request_id.is_some() || self.staged_sign_request_id.is_some()
    }
    pub(in crate::app) fn hunks_pending(&self) -> bool {
        self.hunk_request_id.is_some()
    }
    pub(in crate::app) fn branch_pending(&self) -> bool {
        self.branch_request_id.is_some()
    }
    pub(in crate::app) fn silent(&self) -> bool {
        self.silent
    }
    pub(in crate::app) fn status_request_id(&self) -> u64 {
        self.status.request_id()
    }
    pub(in crate::app) fn sign_request_id(&self) -> u64 {
        self.sign_hunks.request_id()
    }
}

/// The diff pane's structure for one file, built from the hunks the product already reads.
///
/// The structured read carries both line numbers exactly, so this does not format them back
/// to patch text and re-parse: it maps them straight across and reuses the shared span
/// computation, so a diff built this way and a diff parsed from patch text agree about what
/// changed inside a line.
/// The diff between two texts that aren't in Git, such as a proposed workspace edit.
pub(in crate::app) fn text_diff_preview(
    old: &[u8],
    new: &[u8],
    provenance: &str,
) -> crate::ui::DiffPreview {
    use crate::ui::DiffHunk;
    use crate::ui::DiffLine;
    use crate::ui::DiffLineKind;
    let mut preview = crate::ui::DiffPreview {
        hunks: Vec::new(),
        provenance: provenance.to_owned(),
        truncated: false,
        total_lines: 0,
        layout: crate::ui::DiffLayout::Unified,
    };
    let mut options = git2::DiffOptions::new();
    options.force_text(true);
    let result = (|| -> Result<(), git2::Error> {
        let patch = git2::Patch::from_buffers(old, None, new, None, Some(&mut options))?;
        let mut previous_end = 1;
        for index in 0..patch.num_hunks() {
            let (hunk, count) = patch.hunk(index)?;
            let mut lines = Vec::with_capacity(count);
            for index_in_hunk in 0..count {
                let line = patch.line_in_hunk(index, index_in_hunk)?;
                let kind = match line.origin() {
                    '+' => DiffLineKind::Added,
                    '-' => DiffLineKind::Removed,
                    ' ' => DiffLineKind::Context,
                    _ => continue,
                };
                let text = String::from_utf8_lossy(line.content());
                let text = text
                    .strip_suffix("\r\n")
                    .or_else(|| text.strip_suffix('\n'))
                    .unwrap_or(&text);
                lines.push(DiffLine {
                    kind,
                    old_number: line.old_lineno(),
                    new_number: line.new_lineno(),
                    text: text.to_owned(),
                    spans: Vec::new(),
                });
            }
            crate::ui::emphasise_changed_spans(&mut lines);
            preview.hunks.push(DiffHunk {
                header: String::from_utf8_lossy(hunk.header()).trim_end().to_owned(),
                lines,
                collapsed_before: usize::try_from(hunk.old_start().saturating_sub(previous_end))
                    .unwrap_or(usize::MAX),
            });
            previous_end = hunk.old_start().saturating_add(hunk.old_lines());
        }
        Ok(())
    })();
    if let Err(error) = result {
        preview.hunks.clear();
        preview.provenance = format!("{provenance}: diff unavailable: {error}");
    }
    preview
}

pub(in crate::app) fn diff_preview_from_hunks(
    hunks: &[crate::git::GitHunk],
    provenance: &str,
    truncated: bool,
    total_lines: usize,
) -> crate::ui::DiffPreview {
    use crate::ui::DiffHunk;
    use crate::ui::DiffLine;
    use crate::ui::DiffLineKind;
    use crate::ui::DiffPreview;
    let mut previous_end: Option<u32> = None;
    let mut converted = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        let lines = hunk
            .lines
            .iter()
            .map(|line| DiffLine {
                kind: match line.kind {
                    crate::git::GitDiffLineKind::Context => DiffLineKind::Context,
                    crate::git::GitDiffLineKind::Add => DiffLineKind::Added,
                    crate::git::GitDiffLineKind::Delete => DiffLineKind::Removed,
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

#[cfg(test)]
mod regression_tests {
    use crate::app::git::*;
    use crate::editor::text::TextBuffer;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn proposal_diff_previews_include_text_containing_nul() {
        let preview = text_diff_preview(b"a\0b\n", b"a\0c\n", "proposal");
        assert_eq!(preview.provenance, "proposal");
        let lines = &preview.hunks[0].lines;
        assert!(
            lines
                .iter()
                .any(|line| line.kind == crate::ui::DiffLineKind::Removed && line.text == "a\0b")
        );
        assert!(
            lines
                .iter()
                .any(|line| line.kind == crate::ui::DiffLineKind::Added && line.text == "a\0c")
        );
    }

    #[test]
    fn proposal_diff_previews_preserve_trailing_whitespace() {
        let preview = text_diff_preview(b"text  \r\n", b"text\r\n", "proposal");
        let lines = &preview.hunks[0].lines;
        let removed = lines
            .iter()
            .find(|line| line.kind == crate::ui::DiffLineKind::Removed)
            .unwrap();
        let added = lines
            .iter()
            .find(|line| line.kind == crate::ui::DiffLineKind::Added)
            .unwrap();
        assert_eq!(removed.text, "text  ");
        assert_eq!(added.text, "text");
        assert!(!removed.spans.is_empty());
    }

    pub(in crate::app) fn fixture() -> (tempfile::TempDir, git2::Repository, EditorState, Git) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
        std::fs::write(dir.path().join("a.txt"), &original).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap();
        drop(tree);
        let changed = original
            .replace("line 2\n", "changed 2\n")
            .replace("line 30\n", "changed 30\n");
        std::fs::write(dir.path().join("a.txt"), &changed).unwrap();
        let mut editor = EditorState::new(dir.path().to_owned());
        editor
            .open_text(
                TextBuffer::from_text(&changed),
                Some(dir.path().join("a.txt")),
            )
            .unwrap();
        let mut session = Git::default();
        refresh(&mut session, &editor, dir.path());
        let status = session.status_snapshot.as_ref().unwrap();
        let mut snapshot =
            crate::git::read_hunks(&Git::policy(status, "a.txt".into(), GitDiffLayer::Unstaged))
                .unwrap();
        snapshot.revision = 1;
        session.hunk_snapshot = Some(snapshot);
        session.select_at(2);
        (dir, repo, editor, session)
    }

    // Each wait runs Git, which can take seconds on a loaded Windows runner.
    pub(in crate::app) fn refresh(session: &mut Git, editor: &EditorState, root: &Path) {
        session.refresh(editor, Some(root), 1, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.status_pending() {
            session.poll_status(1).unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    fn read_signs(session: &mut Git, editor: &EditorState) -> Vec<UiGitSign> {
        session.reconcile_signs(editor, 1, true);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut signs = Vec::new();
        while session.signs_pending() {
            if let Some(result) = session.poll_signs(editor, 1, true) {
                signs = result;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        signs
    }

    pub(in crate::app) fn read_hunks(session: &mut Git, editor: &EditorState, path: &Path) {
        session
            .start_hunks(editor, Some(path), 1, None, GitDiffLayer::Unstaged)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.hunks_pending() {
            session.poll_hunks(editor).unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn git_sign_marks_follow_edits_and_undo_while_the_diff_refreshes() {
        let (dir, _repo, editor, git) = fixture();
        let (_dirs, mut app) = crate::app::tests::app();
        *app.state = editor;
        app.git = git;
        let revision = app.workspace.root_revision();
        app.git
            .refresh(&app.state, Some(dir.path()), revision, true)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git.status_pending() {
            app.git.poll_status(revision).unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        loop {
            app.process_git_sign_events();
            if !app.ui.git_signs.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(app.ui.git_signs.iter().any(|sign| sign.line == 1));
        let buffer = app.state.active_buffer_id().unwrap();
        let revision = app.state.buffers[buffer].text.revision();
        app.state
            .apply_transaction(
                &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                    .with_operation(crate::editor::EditorOperation::ApplyEdits {
                        buffer,
                        edits: crate::editor::text::TextEditSet::new(
                            revision,
                            vec![crate::editor::text::TextEdit::new(
                                crate::editor::text::ByteRange::empty(
                                    crate::editor::text::ByteOffset(0),
                                ),
                                "new\n",
                            )],
                        ),
                    }),
            )
            .unwrap();
        app.project_git_signs();
        assert!(app.ui.git_signs.iter().any(|sign| sign.line == 2));
        app.state
            .apply_transaction(
                &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                    .with_operation(crate::editor::EditorOperation::Undo { buffer }),
            )
            .unwrap();
        app.project_git_signs();
        assert!(app.ui.git_signs.iter().any(|sign| sign.line == 1));
    }

    #[test]
    fn reloaded_text_invalidates_signs_even_at_the_same_revision() {
        let (_dir, _, mut editor, mut session) = fixture();
        let before = read_signs(&mut session, &editor);
        assert!(before.iter().any(|sign| sign.line == 1));
        let id = editor.active_buffer_id().unwrap();
        assert_eq!(editor.buffers[id].text.revision().0, 0);
        let replacement = (1..=40)
            .map(|n| format!("line {n}\n"))
            .collect::<String>()
            .replace("line 20\n", "changed 20\n");
        editor.buffers[id].text = TextBuffer::from_text(&replacement);
        assert_eq!(editor.buffers[id].text.revision().0, 0);
        // An old in-flight reply cannot publish against the reloaded buffer either.
        assert!(session.poll_signs(&editor, 1, true).unwrap().is_empty());
        let after = read_signs(&mut session, &editor);
        assert_eq!(
            after
                .iter()
                .filter(|sign| !sign.staged)
                .map(|sign| sign.line)
                .collect::<Vec<_>>(),
            vec![19]
        );
    }

    #[test]
    fn failed_sign_sources_are_not_retried_until_the_text_changes() {
        let (_dir, _, mut editor, mut session) = fixture();
        let id = editor.active_buffer_id().unwrap();
        editor.buffers[id].text = TextBuffer::from_text(&"x".repeat(8 * 1024 * 1024 + 1));
        let materializations = TextBuffer::test_materializations();
        assert!(session.reconcile_signs(&editor, 1, true));
        assert!(!session.signs_pending());
        assert!(!session.reconcile_signs(&editor, 1, true));
        assert_eq!(TextBuffer::test_materializations(), materializations);

        // A worker failure, such as unsupported binary text, is remembered as well.
        editor.buffers[id].text = TextBuffer::from_text("binary\0text");
        read_signs(&mut session, &editor);
        assert!(!session.reconcile_signs(&editor, 1, true));
        editor.buffers[id].text = TextBuffer::from_text("changed\n");
        assert!(session.reconcile_signs(&editor, 1, true));
    }

    #[test]
    fn interactive_hunks_follow_unsaved_buffer_lines_and_reject_stale_text() {
        let (dir, _, mut editor, mut session) = fixture();
        let id = editor.active_buffer_id().unwrap();
        let path = dir.path().join("a.txt");
        let saved = editor.buffers[id].text.to_string();
        editor.buffers[id].text =
            TextBuffer::from_text(&format!("{}{}", "inserted\n".repeat(10), saved));
        editor.buffers[id].text.bump_revision().unwrap();
        editor.buffers[id].dirty = true;
        read_hunks(&mut session, &editor, &path);
        assert_eq!(session.navigate(1, 15).unwrap().1, 39);
        session.select_at(40);
        assert!(session.preview().unwrap().1.contains("+changed 30"));
        assert!(session.hunks_match(&editor, &path, GitDiffLayer::Unstaged, 1));
        // Staging this buffer-only hunk must offer the unsaved refusal, not apply it to disk.
        session.mutate(&editor, false, true, 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(outcome) = session.poll_mutation().unwrap() {
                assert_eq!(outcome, GitMutationOutcome::UnsavedDocument);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        session
            .start_hunks(&editor, Some(&path), 1, None, GitDiffLayer::Unstaged)
            .unwrap();
        editor.buffers[id].text = TextBuffer::from_text("reloaded\n");
        assert!(session.poll_hunks(&editor).unwrap().is_none());
        assert!(!session.hunks_pending());
        assert!(!session.hunks_match(&editor, &path, GitDiffLayer::Unstaged, 1));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
    }

    #[test]
    fn hunk_refresh_does_not_reuse_selection_from_another_layer() {
        let (dir, repo, editor, mut session) = fixture();
        let path = dir.path().join("a.txt");
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        session
            .start_hunks(&editor, Some(&path), 1, None, GitDiffLayer::Staged)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.hunks_pending() {
            session.poll_hunks(&editor).unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        session.select_at(20);
        assert!(
            !session.has_selection(),
            "an unstaged selection cannot select a staged hunk"
        );
    }

    #[test]
    fn navigating_hunks_replaces_the_selected_change_indices() {
        let (_, _, _, mut session) = fixture();
        let snapshot = session.hunk_snapshot.as_mut().unwrap();
        let mut extra = snapshot.hunks[0].lines.last().unwrap().clone();
        extra.kind = GitDiffLineKind::Add;
        extra.new_line = Some(4);
        extra.text = "extra change\n".into();
        snapshot.hunks[0].lines.push(extra);
        session.select_at(2);
        assert!(!session.selected_change.is_empty());
        assert_eq!(session.navigate(1, 10).unwrap().1, 29);
        let (_, hunk, _) = session.selected.as_ref().unwrap();
        assert_eq!(session.selected_change, hunk.change_at(30));
        session.select_at(20); // Outside the destination hunk, keep the navigation selection.
        let preview = session.preview().unwrap().1;
        assert!(
            preview.contains("-line 30") && preview.contains("+changed 30"),
            "{preview}"
        );
    }

    #[test]
    fn whole_file_deletion_is_selectable_from_the_empty_buffer() {
        let (dir, _, mut editor, mut session) = fixture();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "").unwrap();
        let id = editor.active_buffer_id().unwrap();
        editor.buffers[id].text = TextBuffer::from_text("");
        read_hunks(&mut session, &editor, &path);
        session.select_at(1);
        assert!(session.has_selection());
        assert!(session.preview().unwrap().1.contains("-line 1"));
        session
            .start_plan(&editor, GitMutationKind::StageHunk, 1, true, vec![])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.mutation_pending() {
            session.poll_mutation().unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        let repo = git2::Repository::open(dir.path()).unwrap();
        let entry = repo
            .index()
            .unwrap()
            .get_path(Path::new("a.txt"), 0)
            .unwrap();
        assert!(repo.find_blob(entry.id).unwrap().content().is_empty());
    }

    // Device files and FIFOs are Unix special files.
    #[cfg(unix)]
    #[test]
    fn stage_requests_reject_special_files_on_the_worker() {
        let (dir, _, editor, mut session) = fixture();
        std::os::unix::fs::symlink("/dev/zero", dir.path().join("device")).unwrap();
        session
            .start_plan_for(
                &editor,
                GitMutationKind::StageFile,
                1,
                true,
                vec![],
                Some("device".into()),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match session.poll_mutation() {
                Err(error) => {
                    assert!(
                        error.contains("outside the canonical workspace scope"),
                        "{error}"
                    );
                    break;
                }
                Ok(None) => {}
                Ok(Some(_)) => panic!("special file was staged"),
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(
            git2::Repository::open(dir.path())
                .unwrap()
                .index()
                .unwrap()
                .get_path(Path::new("device"), 0)
                .is_none()
        );
    }

    #[test]
    fn concurrent_mutations_are_refused_without_losing_the_first_result() {
        let (_dir, _, editor, mut session) = fixture();
        session
            .start_plan(&editor, GitMutationKind::StageFile, 1, true, vec![])
            .unwrap();
        let request_id = session.mutation_request_id;
        assert!(
            session
                .start_plan(&editor, GitMutationKind::StageFile, 1, true, vec![])
                .unwrap_err()
                .contains("already running")
        );
        assert!(
            session
                .mutate(&editor, true, true, 1)
                .unwrap_err()
                .contains("already running")
        );
        assert_eq!(session.mutation_request_id, request_id);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(outcome) = session.poll_mutation().unwrap() {
                assert_eq!(outcome, GitMutationOutcome::Staged("Git file staged"));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn worktree_results_are_ignored_after_a_root_change() {
        let (dir, _, _, mut session) = fixture();
        let other = tempfile::tempdir().unwrap();
        session.start_branches(dir.path(), 1).unwrap();
        session.show_worktrees_after_change();
        session.clear();
        // Activating another root does not increment the workspace's root revision.
        assert!(
            session
                .poll_branch_outcome(1, other.path())
                .unwrap()
                .is_none()
        );
        assert!(!session.branch_pending());
        assert!(!session.take_show_worktrees());
        assert!(
            session
                .poll_branch_outcome(1, other.path())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn failed_worktree_changes_do_not_reopen_the_picker() {
        let (dir, repo, _, mut session) = fixture();
        let branch = repo.head().unwrap().shorthand().unwrap().to_owned();
        session
            .start_worktree_create(dir.path(), "review", &dir.path().join("review"), &branch)
            .unwrap();
        session.show_worktrees_after_change();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Err(error) = session.poll_branch_outcome(1, dir.path()) {
                assert!(error.contains("checked out"), "{error}");
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(!session.branch_pending());
        assert!(!session.take_show_worktrees());
    }

    // Creating symlinks on Windows needs extra privileges.
    #[cfg(unix)]
    #[test]
    fn worktree_removal_refuses_unsaved_buffers_including_new_files() {
        let (dir, _, mut editor, mut session) = fixture();
        let path = dir.path().join("review");
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        session
            .start_worktree_create(dir.path(), "review", &alias.join("review"), "review")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.branch_pending() {
            session.poll_branch_outcome(1, dir.path()).unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        let (id, _) = editor
            .open_text(
                TextBuffer::from_text("unsaved\n"),
                Some(path.join("new.txt")),
            )
            .unwrap();
        editor.buffers[id].dirty = true;
        assert!(
            session
                .start_worktree_remove(&editor, dir.path(), "review")
                .unwrap_err()
                .contains("unsaved")
        );
        assert!(!session.branch_pending());
        assert!(path.join("a.txt").is_file());
        editor.buffers[id].dirty = false;
        session
            .start_worktree_remove(&editor, dir.path(), "review")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.branch_pending() {
            session.poll_branch_outcome(1, dir.path()).unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(!path.exists());
    }

    #[test]
    fn cached_hunks_cannot_mutate_another_buffers_file() {
        let (dir, _, mut editor, mut session) = fixture();
        editor
            .open_text(
                TextBuffer::from_text("other\n"),
                Some(dir.path().join("b.txt")),
            )
            .unwrap();
        assert!(
            session
                .mutate(&editor, true, true, 1)
                .unwrap_err()
                .contains("another file")
        );
        assert!(
            session
                .start_plan(&editor, GitMutationKind::StageHunk, 1, true, vec![])
                .is_err()
        );
        session.reconcile_hunk_path(&editor);
        assert!(session.hunk_snapshot.is_none());
        assert!(session.selected.is_none());
        assert!(!session.mutation_pending());
        assert!(
            std::fs::read_to_string(dir.path().join("a.txt"))
                .unwrap()
                .contains("changed 2")
        );
    }

    #[test]
    fn multi_hunk_line_selections_are_explicitly_refused() {
        let (_, _, _, mut session) = fixture();
        assert!(
            session
                .select_lines(2, 30)
                .unwrap_err()
                .contains("one Git hunk")
        );
        assert!(!session.select_lines(2, 2).unwrap().is_empty());
    }

    #[test]
    fn deleted_files_reach_the_stage_worker() {
        let (dir, _, editor, mut session) = fixture();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        session
            .start_plan(&editor, GitMutationKind::StageFile, 1, true, vec![])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.mutation_pending() {
            session.poll_mutation().unwrap();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        let index = git2::Repository::open(dir.path()).unwrap().index().unwrap();
        assert!(index.get_path(Path::new("a.txt"), 0).is_none());
    }

    #[test]
    fn deleted_file_hunks_are_selectable_for_staging_and_reset() {
        for reset in [false, true] {
            let (dir, _, editor, mut session) = fixture();
            let path = dir.path().join("a.txt");
            std::fs::remove_file(&path).unwrap();
            refresh(&mut session, &editor, dir.path());
            let status = session.status_snapshot.as_ref().unwrap();
            let mut snapshot = crate::git::read_hunks(&Git::policy(
                status,
                "a.txt".into(),
                GitDiffLayer::Unstaged,
            ))
            .unwrap();
            snapshot.revision = 2;
            assert_eq!(snapshot.hunks[0].new_start, 0);
            session.selected = None;
            session.hunk_snapshot = Some(snapshot);
            session.select_at(1);
            assert!(session.has_selection());
            if reset {
                session.mutate(&editor, true, true, 1).unwrap();
            } else {
                session
                    .start_plan(&editor, GitMutationKind::StageHunk, 1, true, vec![])
                    .unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while session.mutation_pending() {
                session.poll_mutation().unwrap();
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            let index = git2::Repository::open(dir.path()).unwrap().index().unwrap();
            assert_eq!(index.get_path(Path::new("a.txt"), 0).is_some(), reset);
            assert_eq!(path.exists(), reset);
            if reset {
                let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
                assert_eq!(std::fs::read_to_string(path).unwrap(), original);
            }
        }
    }

    #[test]
    fn worktree_lists_are_discarded_after_their_workspace_changes() {
        let (dir, _, _, mut session) = fixture();
        session.start_branches(dir.path(), 1).unwrap();
        assert!(
            session
                .poll_branch_outcome(2, dir.path())
                .unwrap()
                .is_none()
        );
        assert!(!session.branch_pending());
        session.start_branches(dir.path(), 2).unwrap();
        session.clear();
        assert!(!session.branch_pending());
        assert!(
            session
                .poll_branch_outcome(2, dir.path())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn status_refresh_does_not_cancel_a_current_worktree_list() {
        let (dir, _, editor, mut session) = fixture();
        session.start_branches(dir.path(), 1).unwrap();
        session
            .refresh(&editor, Some(dir.path()), 1, false)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(outcome) = session.poll_branch_outcome(1, dir.path()).unwrap() {
                assert!(matches!(outcome, GitBranchOutcome::List(_)));
                break;
            }
            assert!(
                session.branch_pending(),
                "status refresh cancelled the list"
            );
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn hunk_reads_preserve_worktree_mutation_outcomes() {
        let (dir, _, editor, mut session) = fixture();
        let path = dir.path().join("review");
        session
            .start_worktree_create(dir.path(), "review", &path, "review")
            .unwrap();
        session.show_worktrees_after_change();
        let request_id = session.branch_request_id;
        session.start_branches(dir.path(), 1).unwrap();
        assert_eq!(session.branch_request_id, request_id);
        assert!(session.take_show_worktrees());
        session
            .start_hunks(
                &editor,
                Some(&dir.path().join("a.txt")),
                1,
                None,
                GitDiffLayer::Unstaged,
            )
            .unwrap();
        assert!(session.branch_pending());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(outcome) = session.poll_branch_outcome(1, dir.path()).unwrap() {
                assert!(matches!(outcome, GitBranchOutcome::Changed));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(path.join("a.txt").exists());
    }

    #[test]
    fn unmerged_signs_stay_empty_without_restarting_the_same_read() {
        let (dir, repo, editor, mut session) = fixture();
        let mut index = repo.index().unwrap();
        let mut entry = index.get_path(Path::new("a.txt"), 0).unwrap();
        index.remove(Path::new("a.txt"), 0).unwrap();
        entry.flags = (entry.flags & !0x3000) | 0x2000;
        index.add(&entry).unwrap();
        index.write().unwrap();
        assert!(index.has_conflicts());
        refresh(&mut session, &editor, dir.path());
        assert!(session.reconcile_signs(&editor, 1, true));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut cleared = false;
        while session.signs_pending() {
            if let Some(signs) = session.poll_signs(&editor, 1, true) {
                assert!(signs.is_empty());
                cleared = true;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(cleared);
        let request_id = session.sign_hunks.request_id();
        assert!(!session.reconcile_signs(&editor, 1, true));
        assert_eq!(session.sign_hunks.request_id(), request_id);

        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        assert!(!index.has_conflicts());
        refresh(&mut session, &editor, dir.path());
        assert!(session.reconcile_signs(&editor, 1, true));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut signs = Vec::new();
        while session.signs_pending() {
            if let Some(result) = session.poll_signs(&editor, 1, true) {
                signs = result;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(signs.iter().any(|sign| sign.staged));
    }

    #[test]
    fn refresh_reloads_staged_signs_when_status_flags_are_unchanged() {
        let (dir, repo, editor, mut session) = fixture();
        let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
        let stage = |text: &str| {
            let mut index = repo.index().unwrap();
            index.read(true).unwrap();
            let entry = index.get_path(Path::new("a.txt"), 0).unwrap();
            index.add_frombuffer(&entry, text.as_bytes()).unwrap();
            index.write().unwrap();
        };
        let read = |session: &mut Git| {
            session.reconcile_signs(&editor, 1, true);
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut signs = Vec::new();
            while session.signs_pending() {
                if let Some(result) = session.poll_signs(&editor, 1, true) {
                    signs = result;
                }
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            signs
                .into_iter()
                .filter(|sign| sign.staged)
                .map(|sign| sign.line)
                .collect::<Vec<_>>()
        };
        stage(&original.replace("line 2\n", "changed 2\n"));
        refresh(&mut session, &editor, dir.path());
        let before = read(&mut session);
        let entries = session.status_snapshot.as_ref().unwrap().entries.clone();
        stage(&original.replace("line 30\n", "changed 30\n"));
        refresh(&mut session, &editor, dir.path());
        assert_eq!(session.status_snapshot.as_ref().unwrap().entries, entries);
        let after = read(&mut session);
        assert_ne!(before, after);
        assert_eq!(before, vec![1]);
        assert_eq!(after, vec![29]);
    }
}

mod conflict_markers;
pub(in crate::app) mod pull_requests;
mod review;
#[cfg(test)]
mod review_tests;

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::picker::MatcherSource;
use crate::app::picker::PickerPayload;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::Precondition;
use crate::editor::RegisterShape;
use crate::editor::RegisterValue;
use crate::editor::text::ByteOffset;
use crate::editor::text::PositionEncoding;
use crate::language::lsp::LspMarkupContent;
use crate::language::lsp::LspMarkupKind;
use crate::ui::DocumentationPopupKind;
use crate::ui::PickerSurfaceKind;
use crate::ui::PromptKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::workspace::WorkspaceTrust;

impl App {
    pub(in crate::app) fn clear_git_status(&mut self) {
        self.git.clear();
        self.git.sign_marks.clear();
        for buffer in self.state.buffers.values_mut() {
            buffer.decorations.clear("git");
        }
        self.ui.git_signs.clear();
    }
}

impl App {
    pub(in crate::app) fn reconcile_git_signs(&mut self) {
        if self.git.status().is_none()
            && self
                .state
                .active_buffer_id()
                .and_then(|id| self.state.buffers[id].path.as_ref())
                .is_some()
        {
            self.start_git_status_refresh_silent();
        }
        self.git.reconcile_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        );
        self.project_git_signs();
    }
}

impl App {
    pub(in crate::app) fn process_git_sign_events(&mut self) {
        self.git.reconcile_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        );
        if let Some(signs) = self.git.poll_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        ) {
            self.git.sign_marks.clear();
            for buffer in self.state.buffers.values_mut() {
                buffer.decorations.clear("git");
            }
            for sign in signs {
                let Some(buffer) = self.state.buffers.get_mut(sign.buffer) else {
                    continue;
                };
                let Ok(position) = buffer.text.line_to_byte(sign.line) else {
                    continue;
                };
                if let Some(mark) = buffer.decorations.insert(
                    &buffer.text,
                    "git",
                    crate::editor::text::ByteRange::empty(position),
                    crate::editor::decorations::Gravity::Right,
                    crate::editor::decorations::Gravity::Right,
                ) {
                    self.git.sign_marks.push((mark, sign));
                }
            }
        }
        self.project_git_signs();
    }

    fn project_git_signs(&mut self) {
        let source = self.git.sign_source(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        );
        let signs = self
            .git
            .sign_marks
            .iter()
            .filter_map(|(mark, sign)| {
                let source = source.as_ref()?;
                if (
                    sign.root_revision,
                    sign.status_revision,
                    sign.repository_id,
                    sign.buffer,
                ) != (source.0, source.1, source.2, source.4)
                {
                    return None;
                }
                let buffer = self.state.buffers.get(sign.buffer)?;
                let position = buffer.decorations.range(*mark)?.start;
                Some(UiGitSign {
                    line: buffer.text.byte_to_line(position).ok()?,
                    buffer_revision: buffer.text.revision().0,
                    ..*sign
                })
            })
            .collect();
        self.ui.set(signs, |ui| &mut ui.git_signs);
    }
}

impl App {
    pub(in crate::app) fn start_git_hunks(&mut self) {
        self.start_git_hunks_with_action(None);
    }
}

impl App {
    pub(in crate::app) fn start_git_hunks_with_action(&mut self, action: Option<GitHunkAction>) {
        self.start_git_hunks_with_layer(action, crate::git::GitDiffLayer::Unstaged);
    }
}

impl App {
    pub(in crate::app) fn start_git_hunks_with_layer(
        &mut self,
        action: Option<GitHunkAction>,
        layer: crate::git::GitDiffLayer,
    ) {
        let path = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.as_deref());
        if let Err(error) = self.git.start_hunks(
            &self.state,
            path,
            self.workspace.root_revision(),
            action,
            layer,
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn start_git_worktrees(&mut self) {
        let Some(status) = self.git.status().cloned() else {
            if self.git.status_pending() {
                self.git.show_worktrees_after_change();
            } else {
                self.show_error("Git status is not ready; is this a Git repository?");
            }
            return;
        };
        if let Err(error) = self
            .git
            .start_branches(&status.repository, self.workspace.root_revision())
        {
            self.show_error(error);
        }
    }
}

impl App {
    /// Remove the selected worktree and its directory. This never deletes a branch.
    pub(in crate::app) fn request_git_worktree_remove(&mut self) {
        let Some(crate::app::picker::PickerPayload::GitWorktree { name, main, .. }) =
            self.search.selected_picker_payload().cloned()
        else {
            self.show_error("select a worktree first");
            return;
        };
        if main {
            self.show_error("the primary worktree (main checkout) cannot be removed");
            return;
        }
        self.git
            .set_pending_destructive(crate::app::git::GitDestructiveAction::RemoveWorktree(
                name.clone(),
            ));
        self.open_confirmation(
            PromptKind::GitDestructive,
            "Remove worktree".into(),
            format!("Remove worktree `{name}` and its directory? No branch is deleted."),
            crate::ui::ConfirmationChoice::No,
        );
    }
}

impl App {
    fn git_repository(&mut self) -> Option<std::path::PathBuf> {
        match self.git.status() {
            Some(status) => Some(status.repository.clone()),
            None => {
                self.show_error("Git status is not ready; is this a Git repository?");
                None
            }
        }
    }
}

impl App {
    fn report_branch_request(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn create_git_worktree(
        &mut self,
        branch: String,
        path: Option<std::path::PathBuf>,
    ) {
        let Some(repository) = self.git_repository() else {
            return;
        };
        // Git names a worktree with one path segment; branches may have several.
        let name = branch.replace('/', "-");
        let path = path.unwrap_or_else(|| std::path::Path::new(".worktree").join(&name));
        let result = self
            .git
            .start_worktree_create(&repository, &name, &path, &branch);
        if result.is_ok() {
            self.git.show_worktrees_after_change();
        }
        self.report_branch_request(result);
    }
}

impl App {
    pub(in crate::app) fn confirm_git_destructive(
        &mut self,
        action: Option<crate::app::git::GitDestructiveAction>,
    ) {
        let Some(action) = action else {
            return;
        };
        let Some(status) = self.git.status().cloned() else {
            self.show_error("Git status is not ready; is this a Git repository?");
            return;
        };
        let result = match action {
            crate::app::git::GitDestructiveAction::RemoveWorktree(name) => self
                .git
                .start_worktree_remove(&self.state, &status.repository, &name),
        };
        match result {
            Ok(()) => self.git.show_worktrees_after_change(),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn process_git_branch_events(&mut self) {
        // One poll per handler call: the service is one shot.
        let outcome = match self
            .git
            .poll_branch_outcome(self.workspace.root_revision(), &self.state.workspace_root)
        {
            Ok(Some(outcome)) => Some(outcome),
            Ok(None) => None,
            Err(error) => {
                self.show_error(error);
                None
            }
        };
        let list = match outcome {
            Some(crate::app::git::GitBranchOutcome::List(list)) => list,
            Some(crate::app::git::GitBranchOutcome::Changed) => {
                self.start_git_status_refresh();
                self.notify_message("Git branch or worktree updated".into(), false);
                if self.git.take_show_worktrees() {
                    self.start_git_worktrees();
                }
                return;
            }
            None => return,
        };
        let new_worktree = (
            PickerItem {
                badge: String::new(),
                kind: crate::workspace::PickerItemKind::Entry,
                id: PickerItemId("git-worktree-new".into()),
                label: "New worktree…".into(),
                detail: "check out a branch in its own directory".into(),
                source: String::new(),
                action: PickerAction::None,
            },
            crate::app::picker::PickerPayload::Command("workspace.git.worktree.create".into()),
        );
        let items = std::iter::once(new_worktree)
            .chain(list.worktrees.iter().map(|worktree| {
                let mut detail = match &worktree.branch {
                    Some(branch) => format!("branch {branch}"),
                    None => "detached".to_string(),
                };
                if worktree.main {
                    detail.push_str(" · main");
                }
                if crate::files::canonicalize(&worktree.path)
                    .is_ok_and(|path| path == self.state.workspace_root)
                {
                    detail.push_str(" · open here");
                }
                if worktree.dirty {
                    detail.push_str(" · dirty");
                }
                if let Some(operation) = &worktree.operation {
                    detail.push_str(&format!(" · {operation} in progress"));
                }
                if worktree.locked {
                    detail.push_str(" · locked");
                }
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(if worktree.main {
                            "git-worktree:primary".into()
                        } else {
                            format!("git-worktree:linked:{}", worktree.name)
                        }),
                        label: worktree.name.clone(),
                        detail,
                        source: "worktrees".into(),
                        action: PickerAction::None,
                    },
                    crate::app::picker::PickerPayload::GitWorktree {
                        name: worktree.name.clone(),
                        main: worktree.main,
                        path: worktree.path.clone(),
                    },
                )
            }))
            .collect::<Vec<_>>();
        let title = format!("Git worktrees · {}", list.worktrees.len());
        self.install_picker(title, items);
        self.search
            .set_picker_footer("Enter Switch or Create · ctrl-x Remove · Esc Esc Close".to_owned());
        self.sync_picker_ui();
    }
}

impl App {
    pub(in crate::app) fn unstage_selected_git_hunk(&mut self) {
        self.git.reconcile_hunk_path(&self.state);
        use crate::git::GitDiffLayer;
        if !self.active_git_hunks(GitDiffLayer::Staged) {
            self.start_git_hunks_with_layer(Some(GitHunkAction::Unstage), GitDiffLayer::Staged);
            return;
        }
        self.git.select_at(self.cursor_line_number());
        if !self.git.has_selection() {
            // The staged list isn't what ]h and the signs should keep following.
            self.git.forget_hunks();
            self.show_error("no staged Git hunk on this line");
            return;
        }
        if let Err(error) = self.git.start_plan(
            &self.state,
            crate::git::GitMutationKind::UnstageHunk,
            self.workspace.root_revision(),
            self.workspace.trust() == WorkspaceTrust::Trusted,
            Vec::new(),
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_git_hunk_events(&mut self) {
        if self.search.poll_diff_preview(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            self.sync_picker_ui();
        }
        match self.git.poll_hunks(&self.state) {
            Ok(Some(ready)) => {
                if let Some(action) = ready.action {
                    if let GitHunkAction::Stage(line) | GitHunkAction::StageLines(line, _) = action
                    {
                        let result = if let GitHunkAction::StageLines(start, end) = action {
                            if let Ok(lines) = self.git.select_lines(start, end) {
                                let file = self.git.hunks().map(|snapshot| snapshot.file.clone());
                                self.git.start_plan_for(
                                    &self.state,
                                    crate::git::GitMutationKind::StageLines,
                                    self.workspace.root_revision(),
                                    self.workspace.trust() == WorkspaceTrust::Trusted,
                                    lines,
                                    file,
                                )
                            } else {
                                Err("no unstaged Git change on those lines".into())
                            }
                        } else {
                            self.git.select_at(line);
                            self.git.mutate(
                                &self.state,
                                false,
                                self.workspace.trust() == WorkspaceTrust::Trusted,
                                self.workspace.root_revision(),
                            )
                        };
                        if let Err(error) = result {
                            self.input.clear_pending_stage();
                            self.show_error(error);
                        }
                        return;
                    }
                    if self
                        .state
                        .active_buffer_id()
                        .and_then(|id| self.state.buffers[id].path.as_ref())
                        != Some(&ready.path)
                    {
                        return;
                    }
                    match action {
                        GitHunkAction::Move(direction) => self.navigate_git_hunk(direction),
                        GitHunkAction::Preview => self.preview_selected_git_hunk(),
                        GitHunkAction::Stage(_) | GitHunkAction::StageLines(_, _) => {
                            unreachable!("staging handled above")
                        }
                        GitHunkAction::Unstage => self.unstage_selected_git_hunk(),
                        GitHunkAction::Reset => self.request_reset_selected_git_hunk(),
                    }
                    return;
                }
                let Some(snapshot) = self.git.hunks() else {
                    return;
                };
                let items = snapshot
                    .hunks
                    .iter()
                    .map(|hunk| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("git-hunk:{}", hunk.id.0)),
                                label: hunk.header.trim().to_owned(),
                                // The layer is part of the identity: a staged hunk must never
                                // be presented as an unstaged one.
                                detail: format!(
                                    "-{},{} +{},{} · {}",
                                    hunk.old_start,
                                    hunk.old_lines,
                                    hunk.new_start,
                                    hunk.new_lines,
                                    snapshot.source().source_label()
                                ),
                                source: "git-hunk".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::GitHunk {
                                path: ready.path.clone(),
                                status_revision: snapshot.status_revision,
                                repository: snapshot.repository_id,
                                hunk: hunk.id,
                                new_line: self.git.line_in_buffer(
                                    snapshot.layer,
                                    hunk.change_lines()
                                        .first()
                                        .map_or(hunk.new_start, |line| *line)
                                        .saturating_sub(1)
                                        as usize,
                                ),
                            },
                        )
                    })
                    .collect();
                self.install_picker("Git hunks".into(), items);
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    /// Conflict markers are in the text itself, so this works in any file, staged or not.
    pub(in crate::app) fn navigate_git_conflict(&mut self, direction: i8) {
        let Some(view) = self.state.active_view().map(|id| &self.state.views[id]) else {
            return;
        };
        let buffer = &self.state.buffers[view.buffer];
        let current = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let source = buffer.text.to_string();
        let markers: Vec<usize> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.starts_with("<<<<<<< "))
            .map(|(index, _)| index)
            .collect();
        let target = if direction > 0 {
            markers
                .iter()
                .find(|line| **line > current)
                .or(markers.first())
        } else {
            markers
                .iter()
                .rev()
                .find(|line| **line < current)
                .or(markers.last())
        };
        if let Some(line) = target {
            let position = buffer.text.line_to_byte(*line).unwrap_or(ByteOffset(0));
            self.move_search_cursor(self.state.active_view().expect("active view"), position);
        } else {
            self.notify_message(self.message("conflict_none", &[]), false);
        }
    }
}

impl App {
    fn active_git_hunks(&self, layer: crate::git::GitDiffLayer) -> bool {
        self.state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.as_deref())
            .is_some_and(|path| {
                self.git
                    .hunks_match(&self.state, path, layer, self.workspace.root_revision())
            })
    }
}

impl App {
    pub(in crate::app) fn navigate_git_hunk(&mut self, direction: i8) {
        self.git.reconcile_hunk_path(&self.state);
        if !self.active_git_hunks(crate::git::GitDiffLayer::Unstaged) {
            self.start_git_hunks_with_action(Some(GitHunkAction::Move(direction.signum())));
            return;
        }
        match self.git.navigate(direction, self.cursor_line_number()) {
            Ok((path, line)) => self.navigate_matcher_source(MatcherSource {
                path,
                line,
                column: 0,
                encoding: PositionEncoding::Utf8,
            }),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    /// The one-based line under the cursor in the active window.
    fn cursor_line_number(&self) -> u32 {
        self.state
            .active_view()
            .and_then(|id| {
                let view = &self.state.views[id];
                self.state.buffers[view.buffer]
                    .text
                    .byte_to_line(view.cursor)
                    .ok()
            })
            .unwrap_or(0) as u32
            + 1
    }
}

impl App {
    pub(in crate::app) fn preview_selected_git_hunk(&mut self) {
        self.git.reconcile_hunk_path(&self.state);
        if !self.active_git_hunks(crate::git::GitDiffLayer::Unstaged) {
            self.start_git_hunks_with_action(Some(GitHunkAction::Preview));
            return;
        }
        self.git.select_at(self.cursor_line_number());
        match self.git.preview() {
            Ok((header, text)) => self.present_documentation_popup(
                DocumentationPopupKind::Git,
                header,
                "Git".into(),
                None,
                vec![LspMarkupContent {
                    kind: LspMarkupKind::PlainText,
                    value: text,
                }],
            ),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn request_reset_selected_git_hunk(&mut self) {
        self.git.reconcile_hunk_path(&self.state);
        if self.workspace.trust() != WorkspaceTrust::Trusted {
            self.show_error("Git reset requires a trusted workspace");
            return;
        }
        if !self.active_git_hunks(crate::git::GitDiffLayer::Unstaged) {
            self.start_git_hunks_with_action(Some(GitHunkAction::Reset));
            return;
        }
        self.git.select_at(self.cursor_line_number());
        if !self.git.has_selection() {
            self.show_error("select a current Git hunk first");
            return;
        }
        self.git.remember_reset_target();
        self.open_prompt(PromptKind::GitReset, String::new());
        self.prompt_help("Discards worktree changes. Type `reset this hunk`.".into());
    }
}

impl App {
    pub(in crate::app) fn confirm_reset_selected_git_hunk(&mut self, phrase: String) {
        if self.search.picker_surface() == PickerSurfaceKind::Changes {
            self.confirm_reset_git_review_hunk(phrase);
            return;
        }
        let same_target = self.git.take_reset_target();
        if !same_target || !self.active_git_hunks(crate::git::GitDiffLayer::Unstaged) {
            self.show_error("Git hunk target changed");
            return;
        }
        if phrase != "reset this hunk" {
            self.show_error("Git hunk reset confirmation did not match");
            return;
        }
        if let Err(error) = self.git.mutate(
            &self.state,
            true,
            self.workspace.trust() == WorkspaceTrust::Trusted,
            self.workspace.root_revision(),
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    /// Stage these one-based file lines. With no lines given, the selection or the cursor's line.
    pub(in crate::app) fn stage_git_lines(&mut self, start: i64, end: i64) {
        self.git.reconcile_hunk_path(&self.state);
        let (start, end) = if start < 0 && end < 0 {
            let Some((start, end, selecting)) = self.active_selected_lines() else {
                return;
            };
            if selecting {
                self.invoke_mapped_command("input.cancel");
            }
            (i64::from(start), i64::from(end))
        } else {
            (start, end)
        };
        self.input
            .set_pending_stage(AppEffect::GitLinesStage { start, end });
        let range = u32::try_from(start)
            .ok()
            .zip(u32::try_from(end).ok())
            .filter(|(start, end)| end >= start);
        let Some((start, end)) = range else {
            self.input.clear_pending_stage();
            self.show_error("line range must be two non-negative integers with end >= start");
            return;
        };
        let unstaged = crate::git::GitDiffLayer::Unstaged;
        if !self.active_git_hunks(unstaged) {
            self.start_git_hunks_with_action(Some(GitHunkAction::StageLines(start, end)));
            return;
        }
        let lines = match self.git.select_lines(start, end) {
            Ok(lines) => lines,
            Err(error) => {
                self.input.clear_pending_stage();
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.git.start_plan(
            &self.state,
            crate::git::GitMutationKind::StageLines,
            self.workspace.root_revision(),
            self.workspace.trust() == WorkspaceTrust::Trusted,
            lines,
        ) {
            self.input.clear_pending_stage();
            self.show_error(error);
        }
    }
}

impl App {
    /// The one-based lines of the active selection, or of the cursor, and whether it's a selection.
    fn active_selected_lines(&self) -> Option<(u32, u32, bool)> {
        let view = self.state.views.get(self.state.active_view()?)?;
        let text = &self.state.buffers.get(view.buffer)?.text;
        let (anchor, head) = view
            .selection
            .map_or((view.cursor, view.cursor), |selection| {
                (selection.anchor, selection.head)
            });
        let line = |at| text.byte_to_line(at).ok().map(|line| line as u32 + 1);
        Some((
            line(anchor.min(head))?,
            line(anchor.max(head))?,
            view.selection.is_some(),
        ))
    }
}

impl App {
    pub(in crate::app) fn stage_selected_git_hunk(&mut self) {
        self.git.reconcile_hunk_path(&self.state);
        if !self.active_git_hunks(crate::git::GitDiffLayer::Unstaged) {
            self.start_git_hunks_with_action(Some(GitHunkAction::Stage(self.cursor_line_number())));
            return;
        }
        self.git.select_at(self.cursor_line_number());
        if let Err(error) = self.git.mutate(
            &self.state,
            false,
            self.workspace.trust() == WorkspaceTrust::Trusted,
            self.workspace.root_revision(),
        ) {
            self.input.clear_pending_stage();
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_git_mutation_events(&mut self) {
        match self.git.poll_mutation() {
            Ok(Some(crate::app::git::GitMutationOutcome::Reset)) => {
                self.input.clear_pending_stage();
                self.ui.git_signs.clear();
                self.start_git_status_refresh();
                self.notify_message("Git hunk reset".into(), false);
            }
            Ok(Some(crate::app::git::GitMutationOutcome::Staged(message))) => {
                self.input.clear_pending_stage();
                self.ui.git_signs.clear();
                self.start_git_status_refresh();
                self.notify_message(message.into(), false);
            }
            Ok(Some(crate::app::git::GitMutationOutcome::UnsavedDocument)) => {
                // A refusal, not a failure: the plan never wrote the index, so offer the
                // explicit choice instead of reporting an error the user cannot act on.
                self.input.set_pending_stage_line(self.git.selected_line());
                let path = self
                    .git
                    .unsaved_refusal()
                    .map_or_else(|| "the path".into(), |path| path.display().to_string());
                self.open_confirmation(
                    PromptKind::GitStageUnsaved,
                    "Stage unsaved changes".into(),
                    format!(
                        "{path} has unsaved changes, so the index would record the version on disk rather than the version in the buffer. Save the buffer, then stage the saved revision?"
                    ),
                    crate::ui::ConfirmationChoice::No,
                );
            }
            Ok(None) => {}
            Err(error) => {
                self.input.clear_pending_stage();
                self.show_error(error);
            }
        }
    }
}

impl App {
    /// Answer the save-and-stage choice. The staging effect is replayed only after the save
    /// completes, so the plan is rebuilt against the saved bytes.
    pub(in crate::app) fn retry_saved_stage(
        &mut self,
        buffer: crate::editor::BufferId,
        path: &Path,
        effect: AppEffect,
        selected_line: Option<u32>,
    ) {
        use crate::git::GitDiffLayer;
        use crate::git::GitMutationKind;
        self.input.set_pending_stage(effect.clone());
        let result = match effect {
            AppEffect::GitFileStage | AppEffect::GitConflictStage | AppEffect::GitFileUnstage => {
                let kind = if matches!(effect, AppEffect::GitFileUnstage) {
                    GitMutationKind::UnstageFile
                } else {
                    GitMutationKind::StageFile
                };
                let file = self
                    .git
                    .status()
                    .and_then(|status| path.strip_prefix(&status.scope_root).ok())
                    .map(Path::to_owned);
                if file.is_none() {
                    self.input.clear_pending_stage();
                    return;
                }
                self.git.start_plan_for(
                    &self.state,
                    kind,
                    self.workspace.root_revision(),
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    Vec::new(),
                    file,
                )
            }
            AppEffect::GitHunkStage | AppEffect::GitLinesStage { .. } => {
                let line = selected_line.unwrap_or_else(|| {
                    self.state
                        .views
                        .values()
                        .find(|view| view.buffer == buffer)
                        .and_then(|view| {
                            self.state.buffers[buffer]
                                .text
                                .byte_to_line(view.cursor)
                                .ok()
                        })
                        .unwrap_or(0) as u32
                        + 1
                });
                let action = match effect {
                    AppEffect::GitLinesStage { start, end } => {
                        GitHunkAction::StageLines(start as u32, end as u32)
                    }
                    _ => GitHunkAction::Stage(line),
                };
                self.git.start_hunks(
                    &self.state,
                    Some(path),
                    self.workspace.root_revision(),
                    Some(action),
                    GitDiffLayer::Unstaged,
                )
            }
            _ => {
                self.input.clear_pending_stage();
                return;
            }
        };
        if let Err(error) = result {
            self.input.clear_pending_stage();
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn confirm_stage_unsaved(&mut self) {
        let path = self
            .git
            .unsaved_refusal()
            .zip(self.git.status())
            .map(|(relative, status)| status.scope_root.join(relative));
        let target = path.and_then(|path| {
            self.state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                .map(|(id, _)| (id, path))
        });
        let Some((buffer, _path)) = target else {
            self.input.clear_pending_stage();
            self.show_error("The unsaved Git document is no longer open");
            return;
        };
        match self.request_save_buffer(buffer, false, None, false, None) {
            Ok(Some(id)) => self.input.bind_stage_save(id),
            Ok(None) => self.documents.stage_after_save_work(buffer),
            Err(error) => {
                self.input.clear_pending_stage();
                self.show_document_failure(error);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn start_git_status_refresh(&mut self) {
        self.start_git_status_refresh_with_feedback(true);
    }
}

impl App {
    pub(in crate::app) fn start_git_status_refresh_silent(&mut self) {
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| r.identity.canonical_path.as_path());
        if self.git.needs_status(self.workspace.root_revision(), root) {
            self.start_git_status_refresh_with_feedback(false);
        }
    }
}

impl App {
    pub(in crate::app) fn start_git_status_refresh_with_feedback(&mut self, report_errors: bool) {
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| r.identity.canonical_path.as_path());
        self.ui.git_signs.clear();
        if let Err(error) = self.git.refresh(
            &self.state,
            root,
            self.workspace.root_revision(),
            report_errors,
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_git_status_events(&mut self) {
        match self.git.poll_status(self.workspace.root_revision()) {
            Ok(false) => return,
            Ok(true) => {}
            Err(error) => self.show_error(error),
        }
        self.state.workspace_repository = self
            .git
            .status()
            .filter(|snapshot| snapshot.scope_root == self.state.workspace_root)
            .and_then(|snapshot| snapshot.worktree_repository.clone());
        if self.git.status().is_none() {
            self.ui.git_signs.clear();
            self.git.open_changes_when_ready = false;
        } else if self.git.open_changes_when_ready {
            self.open_git_status_picker();
        } else if self.git.take_show_worktrees() {
            self.start_git_worktrees();
        } else if self.search.picker_surface() == PickerSurfaceKind::Changes {
            // Staging from the list moves the row between its sections.
            let normal = self.input.picker_normal();
            self.open_git_status_picker();
            self.input.set_picker_normal(normal);
            self.sync_picker_ui();
        }
        if self.ui.explorer.is_some() {
            self.sync_explorer_ui();
        }
        // A refreshed status is the bounded moment to re-read the operation bar, so a paused
        // operation appears and disappears with the repository rather than with a guess.
        self.project_operation_bar();
        self.project_debug_panes();
    }
}

impl App {
    pub(in crate::app) fn open_git_status_picker(&mut self) {
        self.git.reconcile_status_documents(&self.state);
        self.git.open_changes_when_ready = false;
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| &r.identity.canonical_path);
        let Some(snapshot) = self.git.status().filter(|s| {
            s.root_revision == self.workspace.root_revision() && root == Some(&s.scope_root)
        }) else {
            // Status loads in the background; open the picker as soon as it arrives.
            self.git.open_changes_when_ready = true;
            self.start_git_status_refresh();
            return;
        };
        // The three sections, in the order the surface draws them. A conflicted path is listed
        // only under Conflicts, so it never appears twice.
        let conflicted: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|entry| entry.status == crate::git::GitFileStatus::Conflicted)
            .collect();
        // The Unstaged section holds what is not staged. A path whose buffer has unsaved
        // edits but nothing on disk is listed here too, because the document says such a path
        // shows a badge and a path that is not listed cannot show one; its OWN provenance
        // says `buffer -> disk`, so it is never read as an unstaged change.
        let unstaged: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|entry| {
                entry.status != crate::git::GitFileStatus::Conflicted
                    && (entry.sources.unstaged || entry.sources.unsaved_document_revision.is_some())
            })
            .collect();
        let staged: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|entry| {
                entry.status != crate::git::GitFileStatus::Conflicted && entry.sources.staged
            })
            .collect();
        let total = conflicted.len() + unstaged.len() + staged.len();
        let mut items: Vec<(PickerItem, PickerPayload)> = Vec::new();
        for (title, entries, provenance) in [
            ("Conflicts", &conflicted, "both changed"),
            ("Unstaged", &unstaged, "index -> disk"),
            ("Staged", &staged, "HEAD -> index"),
        ] {
            if entries.is_empty() {
                continue;
            }
            items.push((
                PickerItem {
                    // Each header needs its own id: duplicate ids are rejected, so two
                    // headers sharing one would silently drop the second section.
                    id: PickerItemId(format!("section:{title}")),
                    label: format!("{title} {}", entries.len()),
                    detail: String::new(),
                    source: String::new(),
                    badge: String::new(),
                    action: PickerAction::None,
                    kind: crate::workspace::PickerItemKind::Section,
                },
                PickerPayload::Section,
            ));
            for entry in entries {
                items.push((
                    PickerItem {
                        // The id names the section as well, because one path can be both
                        // staged and further modified and the two rows are different changes.
                        id: PickerItemId(format!("git:{title}:{}", entry.relative_path.display())),
                        label: entry.relative_path.display().to_string(),
                        // The provenance column names the comparison this row belongs to, so
                        // an unstaged row is never read as unsaved and an unsaved buffer is
                        // never read as an unstaged change.
                        detail: if title == "Unstaged" && !entry.sources.unstaged {
                            "buffer -> disk".to_owned()
                        } else {
                            provenance.to_owned()
                        },
                        // The status column carries the code: the glyph that keeps the meaning
                        // readable when colour is unavailable.
                        source: Self::change_code(entry),
                        // A path whose buffer has unsaved edits carries a badge, and staging it
                        // is blocked with an explicit choice rather than silently saving.
                        badge: if entry.sources.unsaved_document_revision.is_some() {
                            "unsaved".to_owned()
                        } else {
                            String::new()
                        },
                        action: PickerAction::None,
                        kind: crate::workspace::PickerItemKind::Entry,
                    },
                    PickerPayload::MatcherSource(MatcherSource {
                        path: snapshot.scope_root.join(&entry.relative_path),
                        line: 0,
                        column: 0,
                        encoding: PositionEncoding::Utf8,
                    }),
                ));
            }
        }
        // Re-listing must not move the cursor: the row the user was on is remembered by its
        // identity and restored, so a refresh never reorders the list under them.
        let previous = self.search.picker_bookmark();
        // The title names the branch the changes are against, as the document draws it. A
        // detached HEAD has no branch name, so the segment is simply absent rather than
        // filled with something invented.
        let branch = crate::git::git_branch_name(&snapshot.repository)
            .ok()
            .flatten();
        let title = match branch {
            Some(branch) => format!("Changes · {branch} ±{total}"),
            None => format!("Changes · ±{total}"),
        };
        self.install_surface_picker(PickerSurfaceKind::Changes, title, items);
        self.search
            .set_picker_footer(self.message("git_review_footer", &[]));
        // The list is for acting on files, so its keys work from the start; i searches it.
        self.input.set_picker_normal(true);
        self.search.restore_picker_view(previous, false);
        self.sync_picker_ui();
    }
}

impl App {
    /// Open the current line on the forge, or copy its URL, as LazyVim's `<leader>gB`/`gY`.
    pub(in crate::app) fn browse_git_line(&mut self, copy: bool) {
        let file = self.state.active_view().and_then(|view| {
            let view = &self.state.views[view];
            let buffer = &self.state.buffers[view.buffer];
            let line = buffer
                .text
                .byte_to_line(view.cursor)
                .map_or(1, |line| line + 1);
            buffer.path.clone().map(|path| (path, line))
        });
        let Some((path, line)) = file else {
            self.show_error("this needs a file buffer");
            return;
        };
        let url = match crate::git::git_browse_url(&path, line) {
            Ok(url) => url,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if copy {
            let policy = self.state.policy;
            self.state.registers.set(
                '+',
                RegisterValue {
                    text: url.clone(),
                    shape: RegisterShape::Character,
                },
                policy.registers_enabled,
                true,
            );
            self.notify_message(format!("copied {url}"), false);
            return;
        }
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        match std::process::Command::new(opener)
            .arg(&url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.notify_message(format!("opened {url}"), false);
            }
            Err(error) => self.show_error(format!("{opener}: {error}")),
        }
    }
}

impl App {
    /// The two-character index/worktree code a change row shows. Unmerged and untracked have
    /// their own codes; everything else names which side differs.
    fn change_code(entry: &crate::git::GitStatusEntry) -> String {
        entry.change_code()
    }
}

impl App {
    /// Accept one side of the active conflict into the RESULT BUFFER, through an ordinary
    /// transaction so the change is undoable by the editor's own undo.
    ///
    /// Saving and staging are deliberately NOT part of this: the buffer is left dirty and the
    /// file on disk and the index are untouched until their own explicit actions run.
    pub(in crate::app) fn choose_git_conflict_side(&mut self, side: &str) {
        let Some(choice) = crate::git::GitConflictChoice::parse(side) else {
            self.show_error("choose base, ours or theirs");
            return;
        };
        let Some(status) = self.git.status() else {
            self.show_error("Git status is not ready; is this a Git repository?");
            return;
        };
        let repository = status.repository.clone();
        let Some(view_id) = self.state.active_view() else {
            self.show_error("no view is active");
            return;
        };
        let Some(view) = self.state.views.get(view_id) else {
            self.show_error("no view is active");
            return;
        };
        let buffer_id = view.buffer;
        let Some(path) = self
            .state
            .buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.path.clone())
        else {
            self.show_error("active buffer has no file path");
            return;
        };
        // The content comes from the repository, and every refusal is decided before the edit.
        let content = match crate::git::read_side_content(&repository, &path, choice) {
            Ok(content) => content,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        let content = match crate::editor::text::decode_utf8(content.as_bytes()) {
            Ok(decoded) => decoded.text,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            self.show_error("the buffer is gone");
            return;
        };
        let revision = buffer.text.revision();
        let length = ByteOffset(buffer.text.len_bytes());
        let edits = crate::editor::text::TextEditSet::new(
            revision,
            vec![crate::editor::text::TextEdit::new(
                crate::editor::text::ByteRange::new(ByteOffset(0), length).expect("whole buffer"),
                content,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error.to_string());
            return;
        }
        self.notify_message(
            format!(
                "Accepted the {} side of {}; save and stage are separate steps",
                choice.name(),
                path.display()
            ),
            false,
        );
    }
}

impl App {
    /// Preview replacing whole files with one side, then ask. Nothing is written until the
    /// confirmation is answered, and the plan is REBUILT when it is, so a preview taken against
    /// one repository state is never applied to another.
    pub(in crate::app) fn preview_git_replacement(&mut self, side: &str, files: &str) {
        let Some(choice) = crate::git::GitConflictChoice::parse(side) else {
            self.show_error("choose base, ours or theirs");
            return;
        };
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        let paths = files
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| {
                let path = std::path::Path::new(name);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    repository.join(path)
                }
            })
            .collect::<Vec<_>>();
        if paths.is_empty() {
            self.show_error("name at least one file to replace");
            return;
        }
        let plan = match crate::git::plan_replacement(&repository, &paths, choice) {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        let preview = plan.preview();
        self.git.set_pending_replacement(plan);
        self.open_confirmation(
            crate::ui::PromptKind::GitDestructive,
            "Replace whole files".into(),
            preview,
            crate::ui::ConfirmationChoice::No,
        );
        self.project_input_prompt();
    }
}

impl App {
    /// Apply the previewed replacement once it is confirmed. The plan is rebuilt from the same
    /// list and side, so the write is decided by current repository state rather than by a plan
    /// captured before the question was asked.
    pub(in crate::app) fn apply_git_replacement(
        &mut self,
        previewed: crate::git::GitReplacementPlan,
        answer: crate::ui::ConfirmationChoice,
    ) {
        if answer != crate::ui::ConfirmationChoice::Yes {
            self.notify_message("Replacement cancelled; nothing was written".into(), false);
            return;
        }
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        let paths = previewed
            .files
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect::<Vec<_>>();
        let plan = match crate::git::plan_replacement(&repository, &paths, previewed.choice) {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        match crate::git::apply_replacement(&plan) {
            Ok(written) => self.notify_message(
                format!(
                    "Replaced {} file(s); saving and staging are separate steps",
                    written.len()
                ),
                false,
            ),
            Err(error) => self.show_error(error.to_string()),
        }
    }
}

impl App {
    /// Stage the resolved conflict in the active buffer, through the SAME file-staging plan the
    /// ordinary staging surface uses rather than a second staging path.
    pub(in crate::app) fn stage_git_conflict(&mut self) {
        self.input.set_pending_stage(AppEffect::GitConflictStage);
        if let Err(error) = self.git.start_plan(
            &self.state,
            crate::git::GitMutationKind::StageFile,
            self.workspace.root_revision(),
            self.workspace.trust() == WorkspaceTrust::Trusted,
            Vec::new(),
        ) {
            self.input.clear_pending_stage();
            self.show_error(error);
        }
    }
}

impl App {
    /// Continue the paused operation. Refuses while any conflict remains, and refuses when no
    /// operation is paused, both before any write.
    pub(in crate::app) fn continue_git_conflict(&mut self) {
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        match crate::git::continue_operation(&repository, "") {
            Ok(outcome) => {
                self.notify_message(
                    format!(
                        "{} completed as {}",
                        outcome.operation,
                        outcome
                            .commit
                            .map(|oid| oid.to_string()[..8].to_owned())
                            .unwrap_or_else(|| "a commit".to_owned())
                    ),
                    false,
                );
                if let Some(warning) = outcome.cleanup_warning {
                    self.show_error(warning);
                }
                self.after_conflict_recovery();
            }
            Err(refusal) => self.show_error(refusal.to_string()),
        }
    }
}

impl App {
    /// Abandon the paused operation. There is no hard-reset fallback: when the working tree holds
    /// changes the abort would destroy, it is refused and names them.
    pub(in crate::app) fn abort_git_conflict(&mut self) {
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        match crate::git::abort_operation(&repository) {
            Ok(outcome) => {
                self.notify_message(
                    format!(
                        "{} abandoned; the branch is back at {}",
                        outcome.operation,
                        outcome
                            .restored_to
                            .map(|oid| oid.to_string()[..8].to_owned())
                            .unwrap_or_else(|| "where it started".to_owned())
                    ),
                    false,
                );
                self.after_conflict_recovery();
            }
            Err(refusal) => self.show_error(refusal.to_string()),
        }
    }
}

impl App {
    fn conflict_repository(&mut self) -> Option<std::path::PathBuf> {
        match self.git.status() {
            Some(status) => Some(status.repository.clone()),
            None => {
                self.show_error("Git status is not ready; is this a Git repository?");
                None
            }
        }
    }
}

impl App {
    /// After a recovery the repository state changed underneath every open buffer, so the
    /// status is refreshed rather than left stale.
    fn after_conflict_recovery(&mut self) {
        self.start_git_status_refresh();
        // The operation may have finished, so the bar is re-read from the repository rather than
        // left showing a state that no longer exists.
        self.project_operation_bar();
        self.project_debug_panes();
    }
}

impl App {
    /// The conflict surface. The operation bar names the paused operation and its phase, and
    /// each conflicted path reports its three sides with a side that does not exist shown as
    /// ABSENT rather than as content.
    pub(in crate::app) fn open_git_conflicts(&mut self) {
        let Some(status) = self.git.status() else {
            self.show_error("Git status is not ready; is this a Git repository?");
            return;
        };
        let repository = status.repository.clone();
        let scope_root = status.scope_root.clone();
        let state = match crate::git::read_conflict_state(&repository) {
            Ok(state) => state,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        let title = format!("Conflicts · {}", state.phase());
        let mut items = Vec::new();
        for (index, conflict) in state.conflicts.iter().enumerate() {
            if !repository.join(&conflict.path).starts_with(&scope_root) {
                continue;
            }
            items.push((
                PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                    id: PickerItemId(format!("conflict:{index}")),
                    label: conflict.path.display().to_string(),
                    detail: conflict.summary(),
                    source: "conflict".into(),
                    action: PickerAction::None,
                },
                PickerPayload::MatcherSource(MatcherSource {
                    line: first_conflict_line(&repository.join(&conflict.path)),
                    path: repository.join(&conflict.path),
                    column: 0,
                    encoding: PositionEncoding::Utf8,
                }),
            ));
        }
        self.install_picker(title, items);
        self.search.set_picker_footer(
            "Enter Open · then Space g m o/t/b takes ours, theirs or both · Esc Esc Close"
                .to_owned(),
        );
        self.sync_picker_ui();
    }
}

impl App {
    /// Move the editor into the selected worktree: it replaces the current root, so the file
    /// tree, pickers, LSP servers and git signs follow. The worktree gets its own trust
    /// decision. Files with unsaved changes in the current root block the switch.
    pub(in crate::app) fn open_git_worktree(&mut self) {
        let Some(crate::app::picker::PickerPayload::GitWorktree { path, .. }) =
            self.search.selected_picker_payload().cloned()
        else {
            self.show_error("select a worktree first");
            return;
        };
        self.switch_workspace_root(path);
    }
}

fn first_conflict_line(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().position(|line| line.starts_with("<<<<<<<")))
        .unwrap_or(0)
}

#[cfg(test)]
pub(in crate::app) mod tests;
