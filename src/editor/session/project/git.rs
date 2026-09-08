use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn lsp_position_encoding(&self) -> PositionEncoding {
        self.language
            .capabilities()
            .map_or(PositionEncoding::Utf16, |c| c.position_encoding)
    }

    pub(in crate::editor::session) fn clear_git_status(&mut self) {
        self.git.clear();
        self.ui.git_signs.clear();
    }

    pub(in crate::editor::session) fn reconcile_git_signs(&mut self) {
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

    pub(in crate::editor::session) fn process_git_sign_events(&mut self) {
        if let Some(signs) = self.git.poll_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        ) {
            self.ui.git_signs = signs;
        }
    }

    pub(in crate::editor::session) fn start_git_hunks(&mut self) {
        self.start_git_hunks_with_action(None);
    }
    pub(in crate::editor::session) fn start_git_hunks_with_action(&mut self, action: Option<i8>) {
        let path = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.as_deref());
        if let Err(error) = self
            .git
            .start_hunks(path, self.workspace.root_revision(), action)
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_git_hunk_events(&mut self) {
        match self.git.poll_hunks() {
            Ok(Some(ready)) => {
                if let Some(action) = ready.action {
                    if action == 0 {
                        self.preview_selected_git_hunk();
                    } else {
                        self.navigate_git_hunk(action);
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
                                path: ready.path.clone(),
                                status_revision: snapshot.status_revision,
                                repository: snapshot.repository_id,
                                hunk: hunk.id,
                                new_line: hunk.new_start.saturating_sub(1) as usize,
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
    pub(in crate::editor::session) fn navigate_git_hunk(&mut self, direction: i8) {
        if self.git.hunks().is_none() {
            self.start_git_hunks_with_action(Some(direction.signum()));
            return;
        }
        let line = self
            .state
            .active_view
            .and_then(|id| {
                let view = &self.state.views[id];
                self.state.buffers[view.buffer]
                    .text
                    .byte_to_line(view.cursor)
                    .ok()
            })
            .unwrap_or(0) as u32
            + 1;
        match self.git.navigate(direction, line) {
            Ok((path, line)) => self.navigate_matcher_source(MatcherSource {
                path,
                line,
                column: 0,
            }),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn preview_selected_git_hunk(&mut self) {
        if self.git.hunks().is_none() {
            self.start_git_hunks_with_action(Some(0));
            return;
        }
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
    pub(in crate::editor::session) fn request_reset_selected_git_hunk(&mut self) {
        if self.workspace.trust() != WorkspaceTrust::Trusted {
            self.show_error("Git reset requires a trusted workspace");
            return;
        }
        if !self.git.has_selection() {
            self.show_error("select a current Git hunk first");
            return;
        }
        self.open_prompt(PromptKind::GitReset, String::new());
        self.prompt_help("Discards worktree changes. Type `reset this hunk`.".into());
    }
    pub(in crate::editor::session) fn confirm_reset_selected_git_hunk(&mut self, phrase: String) {
        if phrase != "reset this hunk" {
            self.show_error("Git hunk reset confirmation did not match");
            return;
        }
        if let Err(error) = self.git.mutate(
            true,
            self.workspace.trust() == WorkspaceTrust::Trusted,
            self.workspace.root_revision(),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn stage_selected_git_hunk(&mut self) {
        if let Err(error) = self.git.mutate(
            false,
            self.workspace.trust() == WorkspaceTrust::Trusted,
            self.workspace.root_revision(),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_git_mutation_events(&mut self) {
        match self.git.poll_mutation() {
            Ok(Some(reset)) => {
                self.ui.git_signs.clear();
                self.start_git_status_refresh();
                self.notify_message(
                    if reset {
                        "Git hunk reset"
                    } else {
                        "Git hunk staged"
                    }
                    .into(),
                    false,
                );
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn start_git_status_refresh(&mut self) {
        self.start_git_status_refresh_with_feedback(true);
    }
    pub(in crate::editor::session) fn start_git_status_refresh_silent(&mut self) {
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| r.identity.canonical_path.as_path());
        if self.git.needs_status(self.workspace.root_revision(), root) {
            self.start_git_status_refresh_with_feedback(false);
        }
    }
    pub(in crate::editor::session) fn start_git_status_refresh_with_feedback(
        &mut self,
        report_errors: bool,
    ) {
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| r.identity.canonical_path.as_path());
        self.ui.git_signs.clear();
        if let Err(error) = self
            .git
            .refresh(root, self.workspace.root_revision(), report_errors)
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_git_status_events(&mut self) {
        match self.git.poll_status(self.workspace.root_revision()) {
            Ok(false) => return,
            Ok(true) => {}
            Err(error) => self.show_error(error),
        }
        if self.git.status().is_none() {
            self.ui.git_signs.clear();
        }
        if self.ui.explorer.is_some() {
            self.sync_explorer_ui();
        }
    }
    pub(in crate::editor::session) fn open_git_status_picker(&mut self) {
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| &r.identity.canonical_path);
        let Some(snapshot) = self.git.status().filter(|s| {
            s.root_revision == self.workspace.root_revision() && root == Some(&s.scope_root)
        }) else {
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
        self.install_picker("Git status".into(), items);
    }
}
