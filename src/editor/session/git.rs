//! Git runtime, accepted snapshots and hunk actions. No mutable editor/UI authority.
use crate::{
    editor::{BufferId, EditorState},
    ui::{UiGitSign, UiGitSignKind},
    workspace::git::{
        GitDiffLayer, GitDiffLineKind, GitFileStatus, GitHunk, GitHunkBufferInput,
        GitHunkGeneration, GitHunkId, GitHunkPolicy, GitHunkService, GitHunkSignKind,
        GitHunkSnapshot, GitMutationGeneration, GitMutationKind, GitMutationRequest,
        GitMutationService, GitRepositoryId, GitResetHunkRequest, GitStageHunkRequest,
        GitStatusGeneration, GitStatusPolicy, GitStatusService, GitStatusSnapshot,
        aggregate_git_status, git_hunk_signs,
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
    hunk_expected: Option<(u64, u64, GitRepositoryId, PathBuf)>,
    hunk_action: Option<i8>,
    hunk_snapshot: Option<GitHunkSnapshot>,
    sign_hunks: GitHunkService,
    sign_generation: Option<GitHunkGeneration>,
    sign_source: Option<SignSource>,
    selected: Option<(GitHunkSnapshot, GitHunk, PathBuf)>,
    mutation: GitMutationService,
    mutation_generation: Option<GitMutationGeneration>,
}

pub(super) struct HunkReady {
    pub path: PathBuf,
    pub action: Option<i8>,
}

impl GitSession {
    pub(super) fn status(&self) -> Option<&GitStatusSnapshot> {
        self.status_snapshot.as_ref()
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
        root: Option<&Path>,
        revision: u64,
        report: bool,
    ) -> Result<(), String> {
        self.clear();
        self.silent = !report;
        let result = match root {
            Some(root) => self
                .status
                .start(GitStatusPolicy {
                    root: root.to_owned(),
                    root_revision: revision,
                    maximum_files: 100_000,
                    maximum_path_bytes: 16 * 1024 * 1024,
                    deadline: Duration::from_secs(5),
                })
                .map_err(|e| e.to_string()),
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
        let policy = Self::policy(status, source.3.clone());
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

    fn policy(status: &GitStatusSnapshot, file: PathBuf) -> GitHunkPolicy {
        GitHunkPolicy {
            scope_root: status.scope_root.clone(),
            repository_id: status.repository_id,
            status_revision: status.revision,
            root_revision: status.root_revision,
            file,
            layer: GitDiffLayer::Unstaged,
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
            .start(Self::policy(status, relative.to_owned()))
            .map_err(|e| e.to_string())?;
        self.hunk_generation = Some(generation);
        self.hunk_expected = Some((
            status.root_revision,
            status.revision,
            status.repository_id,
            path.to_owned(),
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
            Ok(Some(snapshot)) if Some(snapshot.generation) == self.hunk_generation => {
                self.hunk_snapshot = Some(snapshot.clone());
                let path = expected.3.clone();
                self.hunk_generation = None;
                self.hunk_expected = None;
                Ok(Some(HunkReady {
                    path,
                    action: self.hunk_action.take(),
                }))
            }
            Ok(_) => Ok(None),
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
    pub(super) fn mutate(
        &mut self,
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
        let policy = Self::policy(status, snapshot.file.clone());
        let request = if reset {
            GitMutationRequest::ResetUnstagedHunk(GitResetHunkRequest {
                policy,
                hunk_snapshot_revision: snapshot.revision,
                hunk_id: hunk.id,
                selected: hunk.clone(),
                expected_index: Some(snapshot.index_fingerprint),
                expected_worktree: snapshot.worktree_sha256,
                maximum_patch_bytes: 1024 * 1024,
                deadline: Duration::from_secs(5),
            })
        } else {
            GitMutationRequest::StageHunk(GitStageHunkRequest {
                policy,
                hunk_snapshot_revision: snapshot.revision,
                hunk_id: hunk.id,
                selected: hunk.clone(),
                expected_index: Some(snapshot.index_fingerprint),
                expected_worktree: snapshot.worktree_sha256,
                maximum_patch_bytes: 1024 * 1024,
                deadline: Duration::from_secs(5),
            })
        };
        self.mutation_generation = Some(self.mutation.start(request).map_err(|e| e.to_string())?);
        Ok(())
    }
    pub(super) fn poll_mutation(&mut self) -> Result<Option<bool>, String> {
        if self.mutation_generation.is_none() {
            return Ok(None);
        }
        match self.mutation.poll() {
            Ok(Some(result)) => {
                if Some(result.generation) != self.mutation_generation {
                    return Ok(None);
                }
                let reset = matches!(result.kind, GitMutationKind::ResetUnstagedHunk);
                self.clear();
                Ok(Some(reset))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.mutation_generation = None;
                Err(error.to_string())
            }
        }
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
