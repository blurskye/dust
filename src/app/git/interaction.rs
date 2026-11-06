use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::git::GitHunkAction;
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
use std::path::Path;

impl App {
    pub(in crate::app) fn clear_git_status(&mut self) {
        self.git.clear();
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
        if self.git.reconcile_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        ) {
            self.ui.git_signs.clear();
        }
    }
}

impl App {
    pub(in crate::app) fn process_git_sign_events(&mut self) {
        if let Some(signs) = self.git.poll_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        ) {
            self.ui.git_signs = signs;
        }
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
                if worktree
                    .path
                    .canonicalize()
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
        let Some(view) = self.state.active_view.map(|id| &self.state.views[id]) else {
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
            self.move_search_cursor(self.state.active_view.expect("active view"), position);
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
            .active_view
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
        let view = self.state.views.get(self.state.active_view?)?;
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
            Ok(id) => self.input.bind_stage_save(id),
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
        self.search.set_picker_footer(
            "s Stage · u Unstage · Tab Diff · ctrl-d Scroll · Esc Close".to_owned(),
        );
        // The list is for acting on files, so its keys work from the start; i searches it.
        self.input.set_picker_normal(true);
        self.search.restore_picker_view(previous, false);
        self.sync_picker_ui();
    }
}

impl App {
    /// Open the current line on the forge, or copy its URL, as LazyVim's `<leader>gB`/`gY`.
    pub(in crate::app) fn browse_git_line(&mut self, copy: bool) {
        let file = self.state.active_view.and_then(|view| {
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
        let Some(view_id) = self.state.active_view else {
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
