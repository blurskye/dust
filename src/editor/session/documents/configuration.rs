use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_configuration_result(&mut self) {
        let result = match self.configuration.next_result() {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let automatic = result.automatic;
        let loaded = match result.candidate {
            Ok(loaded) => loaded,
            Err(diagnostic) => {
                self.retain_configuration_watch();
                let message = if automatic {
                    "auto_reload_rejected"
                } else {
                    "reload_rejected"
                };
                self.show_error(self.message(message, &[("diagnostic", diagnostic)]));
                return;
            }
        };
        if let Err(diagnostic) = self.apply_configuration_candidate(loaded, automatic) {
            self.retain_configuration_watch();
            let message = if automatic {
                "auto_reload_rejected"
            } else {
                "reload_rejected"
            };
            self.show_error(self.message(message, &[("diagnostic", diagnostic)]));
        }
    }

    pub(in crate::editor::session) fn retain_configuration_watch(&self) {
        self.configuration.retain_watch();
    }

    pub(in crate::editor::session) fn apply_configuration_candidate(
        &mut self,
        loaded: LoadedConfig,
        automatic: bool,
    ) -> Result<(), String> {
        let candidate =
            self.configuration
                .prepare(loaded, &self.commands, &self.plugin_contributions)?;
        let replacement_explorer_watch = if candidate.view().settings.ui.explorer.watch
            != self.configuration.settings().ui.explorer.watch
        {
            Some(
                ExplorerWatchService::start(explorer_watch_policy(
                    &candidate.view().settings.ui.explorer.watch,
                ))
                .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let window_candidate = (candidate.view().settings.windows
            != self.configuration.settings().windows)
            .then(|| self.prepare_configured_window_rules(&candidate.view().settings.windows))
            .transpose()?;
        self.configuration
            .stage_watch(&candidate, &self.plugin_contributions)?;
        if let Some(window_candidate) = window_candidate {
            self.window_session
                .accept_current(window_candidate, &self.state.window_version())?;
        }
        let restart_required = candidate.view().settings.runtime.vfs_queue_capacity
            != self.configuration.settings().runtime.vfs_queue_capacity
            || candidate.view().settings.ui.alternate_screen
                != self.configuration.settings().ui.alternate_screen
            || candidate.view().settings.ui.bracketed_paste
                != self.configuration.settings().ui.bracketed_paste
            || candidate.view().settings.ui.focus_events
                != self.configuration.settings().ui.focus_events;
        self.state.policy = crate::editor::EditingPolicy {
            tab_width: candidate.view().settings.editor.tab_width,
            indent_width: candidate.view().settings.editor.indent_width,
            expand_tabs: candidate.view().settings.editor.expand_tabs,
            undo_break_on_text_commit: candidate.view().settings.editor.undo_break_on_text_commit,
            registers_enabled: candidate.view().settings.editor.registers.storage
                == crate::config::RegisterStorage::Internal,
            mirror_named_to_unnamed: candidate
                .view()
                .settings
                .editor
                .registers
                .mirror_named_to_unnamed,
            ambiguous_width_wide: candidate.view().settings.editor.ambiguous_width
                == crate::config::AmbiguousWidth::Wide,
        };
        self.state.mark_editor_changed();
        if candidate.view().settings.observability != self.configuration.settings().observability {
            self.notices
                .configure_logs(&candidate.view().settings, true);
            self.notices.record(
                LogLevel::Info,
                "configuration",
                "logging-reconfigured",
                BTreeMap::new(),
            );
        }
        if candidate.view().settings.recovery != self.configuration.settings().recovery {
            self.recovery_discovery_queued = false;
            self.recovery_cleanup_queued = false;
            self.recovery_pruned_revisions.clear();
        }
        if !candidate.view().settings.recovery.enabled {
            self.recovery_scheduled.clear();
        }
        if self.window_session.dashboard_visible()
            && !candidate.view().settings.startup.show_dashboard
        {
            self.state.open_text(TextBuffer::from_text(""), None);
            self.set_dashboard_visibility(false);
        }
        self.configuration
            .commit(candidate, &self.plugin_contributions)?;
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.diagnostics.set_limits(
            self.configuration.settings().workspace.maximum_diagnostics,
            self.configuration
                .settings()
                .workspace
                .maximum_diagnostics_per_owner_uri,
        );
        self.sync_ui_diagnostics();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.retire_workspace_resources("configuration changed");
        self.retire_plugin_edit_sources();
        self.input.reconfigure(self.configuration.settings());
        if self
            .terminal_session
            .configure(&self.configuration.settings().terminals)
        {
            self.reject_terminal_responses("terminal configuration changed");
        }
        self.restore_input_focus();
        self.project_configuration();
        self.project_input_hints();
        self.project_input_documentation();
        self.refresh_completions();
        let maximum_inlays = self.configuration.settings().lsp.inlay_hint_level
            == crate::config::InlayHintLevel::Max;
        let sessions = self
            .lsp_sessions
            .iter()
            .map(|(key, state)| (key.clone(), state.server_id.clone()))
            .collect::<Vec<_>>();
        for (key, server_id) in sessions {
            let configuration = if maximum_inlays {
                crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(&server_id)
            } else {
                crate::language::lsp::LspServerConfiguration::standard()
            };
            let _ = self.lsp.configure(&key, configuration);
        }
        self.schedule_lsp_inlay_refresh(true);
        self.explorer.reconfigure_watch(replacement_explorer_watch);
        if !self.configuration.settings().ui.explorer.bundled_enabled {
            self.ui.explorer = None;
            self.set_explorer_focus(false);
        }
        self.reconcile_explorer_watch();
        self.window_session.constrain_terminal_height(
            self.configuration
                .settings()
                .ui
                .terminal_panel
                .minimum_height,
            self.configuration
                .settings()
                .ui
                .terminal_panel
                .maximum_height,
        );
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.rebuild_matcher_cache();
        if let Some(menu) = &self.ui.tool_menu {
            let category = menu.category.clone();
            self.rebuild_tool_menu(&category, None);
        }
        self.ui.config_diagnostic = None;
        self.notify_message(
            if restart_required {
                self.message("reloaded_restart", &[])
            } else if automatic {
                self.message("auto_reloaded", &[])
            } else {
                self.message("reloaded", &[])
            },
            false,
        );
        self.ensure_cursor_visible();
        Ok(())
    }
}
