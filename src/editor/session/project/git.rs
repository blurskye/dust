use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn lsp_position_encoding(&self) -> PositionEncoding {
        self.lsp_capabilities
            .as_ref()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            })
    }

    // GIT_SIGN_FOREGROUND_BEGIN: immutable ownership and worker queue coordination only.

    pub(in crate::editor::session) fn current_git_sign_owner(
        &self,
    ) -> Option<(
        u64,
        u64,
        crate::workspace::git::GitRepositoryId,
        PathBuf,
        crate::editor::BufferId,
        u64,
    )> {
        if !self.configuration.settings.editor.git_signs.enabled {
            return None;
        }
        let status = self
            .git_status_snapshot
            .as_ref()
            .filter(|status| status.root_revision == self.workspace.root_revision())?;
        let buffer_id = self.state.active_buffer_id()?;
        let buffer = self.state.buffers.get(buffer_id)?;
        let path = buffer.path.as_ref()?;
        let relative = path.strip_prefix(&status.scope_root).ok()?.to_path_buf();
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
            relative,
            buffer_id,
            buffer.text.revision().0,
        ))
    }

    pub(in crate::editor::session) fn clear_git_signs(&mut self) {
        self.git_sign_hunks.cancel();
        self.git_sign_generation = None;
        self.git_sign_owner = None;
        self.ui.git_signs.clear();
    }

    pub(in crate::editor::session) fn reconcile_git_signs(&mut self) {
        use crate::workspace::git::{GitDiffLayer, GitHunkBufferInput, GitHunkPolicy};
        let Some(owner) = self.current_git_sign_owner() else {
            if self.git_status_snapshot.is_none()
                && self
                    .state
                    .active_buffer_id()
                    .and_then(|buffer| self.state.buffers.get(buffer))
                    .and_then(|buffer| buffer.path.as_ref())
                    .is_some()
            {
                self.start_git_status_refresh_silent();
            }
            if self.git_sign_owner.is_some() || !self.ui.git_signs.is_empty() {
                self.clear_git_signs();
            }
            return;
        };
        if self.git_sign_owner.as_ref() == Some(&owner) {
            return;
        }
        self.clear_git_signs();
        let Some(status) = self.git_status_snapshot.as_ref() else {
            return;
        };
        let policy = GitHunkPolicy {
            scope_root: status.scope_root.clone(),
            repository_id: owner.2,
            status_revision: owner.1,
            root_revision: owner.0,
            file: owner.3.clone(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10_000,
            maximum_lines: 100_000,
            maximum_bytes: 8 * 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        let Some(buffer) = self
            .state
            .buffers
            .get(owner.4)
            .filter(|buffer| buffer.text.revision().0 == owner.5 && buffer.path.is_some())
        else {
            return;
        };
        let input = GitHunkBufferInput {
            revision: owner.5,
            bytes: buffer.text.to_bytes(),
        };
        match self.git_sign_hunks.start_buffer(policy, input) {
            Ok(generation) => {
                self.git_sign_generation = Some(generation);
                self.git_sign_owner = Some(owner);
            }
            Err(_) => self.clear_git_signs(),
        }
    }

    pub(in crate::editor::session) fn process_git_sign_events(&mut self) {
        use crate::ui::{UiGitSign, UiGitSignKind};
        use crate::workspace::git::{GitHunkSignKind, git_hunk_signs};
        let Some(owner) = self.git_sign_owner.clone() else {
            return;
        };
        if self.current_git_sign_owner().as_ref() != Some(&owner) {
            self.clear_git_signs();
            return;
        }
        match self.git_sign_hunks.poll() {
            Ok(Some(snapshot)) => {
                let snapshot = snapshot.clone();
                if Some(snapshot.generation) != self.git_sign_generation
                    || snapshot.root_revision != owner.0
                    || snapshot.status_revision != owner.1
                    || snapshot.repository_id != owner.2
                    || snapshot.file != owner.3
                    || snapshot.buffer_revision != Some(owner.5)
                {
                    self.clear_git_signs();
                    return;
                }
                self.git_sign_generation = None;
                self.ui.git_signs = git_hunk_signs(&snapshot)
                    .into_iter()
                    .map(|sign| UiGitSign {
                        buffer: owner.4,
                        buffer_revision: owner.5,
                        root_revision: owner.0,
                        status_revision: owner.1,
                        repository_id: owner.2,
                        line: sign.line,
                        kind: match sign.kind {
                            GitHunkSignKind::Added => UiGitSignKind::Added,
                            GitHunkSignKind::Changed => UiGitSignKind::Changed,
                            GitHunkSignKind::Deleted => UiGitSignKind::Deleted,
                        },
                    })
                    .collect();
            }
            Ok(None) => {}
            Err(_) => self.clear_git_signs(),
        }
    }

    // GIT_SIGN_FOREGROUND_END

    pub(in crate::editor::session) fn clear_git_hunks(&mut self) {
        self.git_hunks.cancel();
        self.git_hunk_generation = None;
        self.git_hunk_expected = None;
        self.git_hunk_snapshot = None;
        self.selected_git_hunk = None;
        self.git_mutation.cancel();
        self.git_mutation_generation = None;
    }

    pub(in crate::editor::session) fn clear_git_status(&mut self) {
        self.clear_git_hunks();
        self.clear_git_signs();
        self.git_status.cancel();
        self.git_status_generation = None;
        self.git_status_root_revision = None;
        self.git_status_snapshot = None;
        self.git_status_decorations.clear();
        self.git_status_silent = false;
    }

    pub(in crate::editor::session) fn start_git_hunks(&mut self) {
        use crate::workspace::git::*;
        self.clear_git_hunks();
        let Some(snapshot) = self
            .git_status_snapshot
            .clone()
            .filter(|s| s.root_revision == self.workspace.root_revision())
        else {
            self.show_error("Git status is unavailable; refresh first");
            return;
        };
        let Some(path) = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.clone())
        else {
            self.show_error("active buffer has no file path");
            return;
        };
        let Ok(relative) = path.strip_prefix(&snapshot.scope_root).map(PathBuf::from) else {
            self.show_error("active file is outside Git scope");
            return;
        };
        let policy = GitHunkPolicy {
            scope_root: snapshot.scope_root.clone(),
            repository_id: snapshot.repository_id,
            status_revision: snapshot.revision,
            root_revision: snapshot.root_revision,
            file: relative.clone(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10_000,
            maximum_lines: 100_000,
            maximum_bytes: 8 * 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        match self.git_hunks.start(policy) {
            Ok(generation) => {
                self.git_hunk_generation = Some(generation);
                self.git_hunk_expected = Some((
                    snapshot.root_revision,
                    snapshot.revision,
                    snapshot.repository_id,
                    path,
                ))
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn process_git_hunk_events(&mut self) {
        let Some(expected) = self.git_hunk_expected.clone() else {
            return;
        };
        if self.git_status_snapshot.as_ref().is_none_or(|s| {
            s.root_revision != expected.0
                || s.revision != expected.1
                || s.repository_id != expected.2
        }) {
            self.clear_git_hunks();
            return;
        }
        match self.git_hunks.poll() {
            Ok(Some(snapshot)) => {
                let snapshot = snapshot.clone();
                if Some(snapshot.generation) != self.git_hunk_generation {
                    return;
                }
                self.git_hunk_generation = None;
                self.git_hunk_expected = None;
                self.git_hunk_snapshot = Some(snapshot.clone());
                let items = snapshot
                    .hunks
                    .iter()
                    .map(|hunk| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("git-hunk:{}", hunk.id.0)),
                                label: hunk.header.trim().to_owned(),
                                detail: format!(
                                    "-{},{} +{},{}",
                                    hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
                                ),
                                source: "git-hunk".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::GitHunk {
                                path: expected.3.clone(),
                                status_revision: expected.1,
                                repository: expected.2,
                                hunk: hunk.id,
                                new_line: hunk.new_start.saturating_sub(1) as usize,
                            },
                        )
                    })
                    .collect();
                self.install_picker("Git hunks".into(), items)
            }
            Ok(None) => {}
            Err(error) => {
                self.clear_git_hunks();
                self.show_error(error)
            }
        }
    }

    pub(in crate::editor::session) fn request_reset_selected_git_hunk(&mut self) {
        if self.workspace.trust() != WorkspaceTrust::Trusted {
            self.show_error("Git reset requires a trusted workspace");
            return;
        }
        if self.selected_git_hunk.is_none() {
            self.show_error("select a current Git hunk first");
            return;
        }
        self.open_prompt(PromptKind::GitReset, String::new());
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.error =
                Some("Type `reset this hunk` to confirm destructive worktree reset".into())
        }
    }

    pub(in crate::editor::session) fn confirm_reset_selected_git_hunk(&mut self, phrase: String) {
        use crate::workspace::git::*;
        if phrase != "reset this hunk" {
            self.show_error("Git hunk reset confirmation did not match");
            return;
        }
        if self.workspace.trust() != WorkspaceTrust::Trusted {
            self.show_error("Git reset requires a trusted workspace");
            return;
        }
        let Some((snapshot, hunk, _)) = self.selected_git_hunk.clone() else {
            self.show_error("Git hunk is stale");
            return;
        };
        let Some(status) = self.git_status_snapshot.as_ref().filter(|s| {
            s.repository_id == snapshot.repository_id
                && s.revision == snapshot.status_revision
                && s.root_revision == self.workspace.root_revision()
        }) else {
            self.show_error("Git hunk is stale");
            return;
        };
        let request = GitResetHunkRequest {
            policy: GitHunkPolicy {
                scope_root: status.scope_root.clone(),
                repository_id: status.repository_id,
                status_revision: status.revision,
                root_revision: status.root_revision,
                file: snapshot.file.clone(),
                layer: GitDiffLayer::Unstaged,
                maximum_hunks: 10_000,
                maximum_lines: 100_000,
                maximum_bytes: 8 * 1024 * 1024,
                deadline: Duration::from_secs(5),
            },
            hunk_snapshot_revision: snapshot.revision,
            hunk_id: hunk.id,
            selected: hunk,
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            maximum_patch_bytes: 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        match self
            .git_mutation
            .start(GitMutationRequest::ResetUnstagedHunk(request))
        {
            Ok(generation) => self.git_mutation_generation = Some(generation),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn stage_selected_git_hunk(&mut self) {
        use crate::workspace::git::*;
        if self.workspace.trust() != WorkspaceTrust::Trusted {
            self.show_error("Git staging requires a trusted workspace");
            return;
        }
        let Some((snapshot, hunk, _path)) = self.selected_git_hunk.clone() else {
            self.show_error("select a current Git hunk first");
            return;
        };
        let Some(status) = self.git_status_snapshot.as_ref().filter(|s| {
            s.repository_id == snapshot.repository_id
                && s.revision == snapshot.status_revision
                && s.root_revision == self.workspace.root_revision()
        }) else {
            self.show_error("Git hunk is stale");
            return;
        };
        let request = GitStageHunkRequest {
            policy: GitHunkPolicy {
                scope_root: status.scope_root.clone(),
                repository_id: status.repository_id,
                status_revision: status.revision,
                root_revision: status.root_revision,
                file: snapshot.file.clone(),
                layer: GitDiffLayer::Unstaged,
                maximum_hunks: 10_000,
                maximum_lines: 100_000,
                maximum_bytes: 8 * 1024 * 1024,
                deadline: Duration::from_secs(5),
            },
            hunk_snapshot_revision: snapshot.revision,
            hunk_id: hunk.id,
            selected: hunk,
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            maximum_patch_bytes: 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        match self
            .git_mutation
            .start(GitMutationRequest::StageHunk(request))
        {
            Ok(generation) => self.git_mutation_generation = Some(generation),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn process_git_mutation_events(&mut self) {
        if self.git_mutation_generation.is_none() {
            return;
        }
        match self.git_mutation.poll() {
            Ok(Some(result)) => {
                let was_reset = matches!(
                    result.kind,
                    crate::workspace::git::GitMutationKind::ResetUnstagedHunk
                );
                self.git_mutation_generation = None;
                self.clear_git_status();
                self.start_git_status_refresh();
                self.ui.notification = Some(
                    if was_reset {
                        "Git hunk reset"
                    } else {
                        "Git hunk staged"
                    }
                    .into(),
                );
                self.ui.notification_error = false
            }
            Ok(None) => {}
            Err(error) => {
                self.git_mutation_generation = None;
                self.show_error(error)
            }
        }
    }

    pub(in crate::editor::session) fn start_git_status_refresh(&mut self) {
        self.start_git_status_refresh_with_feedback(true);
    }

    pub(in crate::editor::session) fn start_git_status_refresh_silent(&mut self) {
        let revision = self.workspace.root_revision();
        let active_root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
            .map(|root| root.identity.canonical_path.clone());
        let current = self.git_status_snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.root_revision == revision && active_root.as_ref() == Some(&snapshot.scope_root)
        });
        let attempt = active_root.map(|root| (revision, root));
        if !current
            && self.git_status_generation.is_none()
            && attempt.is_some()
            && self.git_status_silent_attempted != attempt
        {
            self.git_status_silent_attempted = attempt;
            self.start_git_status_refresh_with_feedback(false);
        }
    }

    pub(in crate::editor::session) fn start_git_status_refresh_with_feedback(
        &mut self,
        report_errors: bool,
    ) {
        use crate::workspace::git::*;
        self.clear_git_status();
        self.git_status_silent = !report_errors;
        let revision = self.workspace.root_revision();
        let Some(root) = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| r.identity.canonical_path.clone())
        else {
            self.git_status_silent = false;
            if report_errors {
                self.show_error("workspace root is unavailable");
            }
            return;
        };
        match self.git_status.start(GitStatusPolicy {
            root,
            root_revision: revision,
            maximum_files: 100_000,
            maximum_path_bytes: 16 * 1024 * 1024,
            deadline: Duration::from_secs(5),
        }) {
            Ok(generation) => {
                self.git_status_generation = Some(generation);
                self.git_status_root_revision = Some(revision);
                self.git_status_snapshot = None
            }
            Err(error) => {
                self.git_status_silent = false;
                if report_errors {
                    self.show_error(error)
                }
            }
        }
    }

    pub(in crate::editor::session) fn process_git_status_events(&mut self) {
        if self.git_status_generation.is_some()
            && self.git_status_root_revision != Some(self.workspace.root_revision())
        {
            self.clear_git_status();
            return;
        }
        match self.git_status.poll() {
            Ok(Some(snapshot)) => {
                let snapshot = snapshot.clone();
                if Some(snapshot.generation) == self.git_status_generation
                    && Some(snapshot.root_revision) == self.git_status_root_revision
                {
                    self.git_status_generation = None;
                    self.git_status_root_revision = None;
                    self.git_status_decorations = crate::workspace::git::aggregate_git_status(
                        &snapshot.scope_root,
                        &snapshot.entries,
                    );
                    self.git_status_snapshot = Some(snapshot);
                    self.git_status_silent = false;
                    if self.ui.explorer.is_some() {
                        self.sync_explorer_ui();
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                let report_error = !self.git_status_silent;
                self.clear_git_status();
                if self.ui.explorer.is_some() {
                    self.sync_explorer_ui();
                }
                if report_error {
                    self.show_error(error)
                }
            }
        }
    }

    pub(in crate::editor::session) fn open_git_status_picker(&mut self) {
        let active_root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
            .map(|root| &root.identity.canonical_path);
        let Some(snapshot) = self
            .git_status_snapshot
            .as_ref()
            .filter(|s| {
                s.root_revision == self.workspace.root_revision()
                    && active_root == Some(&s.scope_root)
            })
            .cloned()
        else {
            self.show_error("Git status is unavailable; refresh first");
            return;
        };
        let items = snapshot
            .entries
            .iter()
            .map(|entry| {
                (
                    PickerItem {
                        id: PickerItemId(format!("git:{}", entry.relative_path.display())),
                        label: entry.relative_path.display().to_string(),
                        detail: format!("{:?}", entry.status),
                        source: "git".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::MatcherSource(MatcherSource {
                        path: snapshot.scope_root.join(&entry.relative_path),
                        line: 0,
                        column: 0,
                    }),
                )
            })
            .collect();
        self.install_picker("Git status".into(), items)
    }
}
