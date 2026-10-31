//! Git runtime, accepted snapshots and hunk actions. No mutable editor/UI authority.
use crate::{
    editor::{BufferId, EditorState},
    ui::{UiGitSign, UiGitSignKind},
    workspace::git::{
        GitBranchGeneration, GitBranchList, GitBranchListRequest, GitBranchRequest,
        GitBranchService, GitChangePlanRequest, GitDiffLayer, GitDiffLineKind, GitFileStatus,
        GitHeadExpectation, GitHunk, GitHunkBufferInput, GitHunkGeneration, GitHunkId,
        GitHunkPolicy, GitHunkService, GitHunkSignKind, GitHunkSnapshot, GitMutationError,
        GitMutationGeneration, GitMutationKind, GitMutationRequest, GitMutationService,
        GitRepositoryId, GitResetHunkRequest, GitStatusGeneration, GitStatusPolicy,
        GitStatusService, GitStatusSnapshot, GitUnsavedDocument, aggregate_git_status,
        git_head_id, git_hunk_signs, git_index_fingerprint, worktree_hash,
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
    mutation: GitMutationService,
    mutation_generation: Option<GitMutationGeneration>,
    branches: GitBranchService,
    branch_generation: Option<GitBranchGeneration>,
    show_worktrees: bool,
    /// The previewed whole-file replacement waiting for its confirmation. The plan is rebuilt
    /// when the answer arrives, so a plan previewed against one repository state is never applied
    /// to another.
    pending_replacement: Option<crate::workspace::git::GitReplacementPlan>,
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
    /// Index changes were applied.
    Staged,
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
            .ok_or("Git status is not ready; is this a Git repository?")?;
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
            .ok_or("Git status is not ready; is this a Git repository?")?;
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
                    .ok_or("Git status is not ready; is this a Git repository?")?;
                (relative, status.scope_root.clone())
            }
            None => self.active_relative(editor, revision)?,
        };
        let status = self
            .status_snapshot
            .as_ref()
            .ok_or("Git status is not ready; is this a Git repository?")?;
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
    /// Read the worktree list.
    pub(super) fn start_branches(&mut self, repository: &Path) -> Result<(), String> {
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
    /// Remove a worktree and its directory. This never deletes a branch.
    pub(super) fn start_worktree_remove(&mut self, repository: &Path, name: &str) -> Result<(), String> {
        let request = GitBranchRequest::RemoveWorktree {
            repository: repository.to_path_buf(),
            name: name.to_owned(),
            deadline: Duration::from_secs(10),
        };
        self.branch_generation =
            Some(self.branches.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    /// Create a worktree for a branch. The destination and occupancy checks are the model's.
    /// The worktree list reopens when the running change finishes.
    pub(super) fn show_worktrees_after_change(&mut self) {
        self.show_worktrees = true;
    }
    pub(super) fn take_show_worktrees(&mut self) -> bool {
        std::mem::take(&mut self.show_worktrees)
    }
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




    #[allow(dead_code)]
    pub(super) fn unsaved_refusal(&self) -> Option<&Path> {
        self.unsaved_refusal.as_deref()
    }
    pub(super) fn status_pending(&self) -> bool {
        self.status_generation.is_some()
    }
}

#[cfg(test)]
impl GitSession {
    pub(super) fn signs_pending(&self) -> bool {
        self.sign_generation.is_some()
    }
    pub(super) fn hunks_pending(&self) -> bool {
        self.hunk_generation.is_some()
    }
    pub(super) fn branch_pending(&self) -> bool {
        self.branch_generation.is_some()
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
/// The diff between two texts that aren't in Git, such as a proposed workspace edit.
pub(in crate::editor::session) fn text_diff_preview(
    old: &[u8],
    new: &[u8],
    provenance: &str,
) -> crate::ui::DiffPreview {
    use crate::ui::{DiffHunk, DiffLine, DiffLineKind};
    let mut hunks = Vec::new();
    if let Ok(patch) = git2::Patch::from_buffers(old, None, new, None, None) {
        let mut previous_end = 1;
        for index in 0..patch.num_hunks() {
            let Ok((hunk, count)) = patch.hunk(index) else {
                continue;
            };
            let mut lines = (0..count)
                .filter_map(|line| patch.line_in_hunk(index, line).ok())
                .filter_map(|line| {
                    let kind = match line.origin() {
                        '+' => DiffLineKind::Added,
                        '-' => DiffLineKind::Removed,
                        ' ' => DiffLineKind::Context,
                        _ => return None,
                    };
                    Some(DiffLine {
                        kind,
                        old_number: line.old_lineno(),
                        new_number: line.new_lineno(),
                        text: String::from_utf8_lossy(line.content()).trim_end().to_owned(),
                        spans: Vec::new(),
                    })
                })
                .collect::<Vec<_>>();
            crate::ui::emphasise_changed_spans(&mut lines);
            hunks.push(DiffHunk {
                header: String::from_utf8_lossy(hunk.header()).trim_end().to_owned(),
                lines,
                collapsed_before: usize::try_from(hunk.old_start().saturating_sub(previous_end))
                    .unwrap_or(usize::MAX),
            });
            previous_end = hunk.old_start().saturating_add(hunk.old_lines());
        }
    }
    crate::ui::DiffPreview {
        hunks,
        provenance: provenance.to_owned(),
        truncated: false,
        total_lines: 0,
        layout: crate::ui::DiffLayout::Unified,
    }
}

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
