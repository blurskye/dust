//! Configuration reload validation, publication and application updates.

use crate::app::App;
use crate::config::LoadedConfig;
use crate::editor::text::TextBuffer;
use crate::ui::render::TERMINAL_PANEL_MAXIMUM_HEIGHT;
use crate::ui::render::TERMINAL_PANEL_MINIMUM_HEIGHT;
use crate::workspace::DiagnosticOwner;

impl App {
    pub(in crate::app) fn process_configuration_result(&mut self) {
        let result = match self.configuration.next_result() {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let automatic = result.automatic;
        let outcome = match result.candidate {
            Ok(loaded) => self.apply_configuration_candidate(loaded, automatic),
            Err(diagnostic) => Err(diagnostic),
        };
        match outcome {
            Ok(()) => self.configuration.note_rejected_keymaps(None),
            Err(diagnostic) => {
                self.retain_configuration_watch();
                self.configuration.note_rejected_keymaps(
                    diagnostic.contains("keymap").then(|| diagnostic.clone()),
                );
                let message = if automatic {
                    "auto_reload_rejected"
                } else {
                    "reload_rejected"
                };
                let message = self.message(message, &[("diagnostic", diagnostic)]);
                self.settings_status(message.clone(), true);
                self.show_error(message);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn retain_configuration_watch(&self) {
        self.configuration.retain_watch();
    }
}

impl App {
    pub(in crate::app) fn apply_configuration_candidate(
        &mut self,
        loaded: LoadedConfig,
        automatic: bool,
    ) -> Result<(), String> {
        let mut warning = loaded.warning.clone();
        let candidate =
            self.configuration
                .prepare(loaded, &self.commands, self.extension.contributions())?;
        let window_candidate = (candidate.view().settings.windows
            != self.configuration.settings().windows)
            .then(|| self.prepare_configured_window_rules(&candidate.view().settings.windows))
            .transpose()?;
        self.configuration
            .stage_watch(&candidate, self.extension.contributions())?;
        if let Some(window_candidate) = window_candidate {
            self.window_session.accept_current(
                window_candidate,
                &self.state.window_version(),
                &mut self.state,
            )?;
        }
        self.state.policy = candidate.view().settings.editor.editing_policy();
        self.state.mark_editor_changed();
        self.recovery.configure(
            &self.configuration.settings().recovery,
            &candidate.view().settings.recovery,
        );
        if self.window_session.dashboard_visible()
            && !candidate.view().settings.startup.show_dashboard
        {
            self.state.open_text(TextBuffer::from_text(""), None)?;
            self.set_dashboard_visibility(false);
        }
        let window_contribution = candidate.window_contribution();
        let accepted = self
            .configuration
            .commit(candidate, self.extension.contributions())?;
        let (rules, _) = self.extension.prepare_window_rules(&window_contribution)?;
        rules.finish(accepted)?;
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.sync_ui_diagnostics();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.cancel_workspace_resources("configuration changed");
        self.discard_stale_plugin_edits();
        self.input.reconfigure();
        self.terminal_session
            .configure(&self.configuration.settings().terminals);
        self.restore_input_focus();
        self.project_configuration();
        let persistence = &self.configuration.settings().persistence;
        self.state.undo_directory = (persistence.enabled && persistence.undo_history)
            .then(|| self.persistence.undo_directory());
        self.project_input_hints();
        self.project_input_documentation();
        self.refresh_completions();
        let maximum_inlays = self.configuration.settings().lsp.inlay_hint_level
            == crate::config::InlayHintLevel::Max;
        let sessions = self
            .language
            .sessions()
            .iter()
            .map(|(key, state)| {
                (
                    key.clone(),
                    state.server_id.clone(),
                    state.launch.executable().clone(),
                )
            })
            .collect::<Vec<_>>();
        for (key, server_id, executable) in sessions {
            let mut configuration =
                crate::language::lsp::LspServerConfiguration::for_server(&server_id, &executable);
            if maximum_inlays {
                configuration.overlay(
                    crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(&server_id),
                );
            }
            if let Err(error) = self.language.configure(&key, configuration) {
                let diagnostic = format!("language-server configuration update failed: {error}");
                if let Some(warning) = &mut warning {
                    warning.push_str(&format!("; {diagnostic}"));
                } else {
                    warning = Some(diagnostic);
                }
            }
        }
        self.schedule_lsp_inlay_refresh(true);
        self.explorer.reset_watch_observation();
        self.reconcile_explorer_watch();
        self.window_session.constrain_terminal_height(
            TERMINAL_PANEL_MINIMUM_HEIGHT,
            TERMINAL_PANEL_MAXIMUM_HEIGHT,
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
        if let Some(warning) = warning {
            self.notify_warning(warning);
            if self.ui.settings.is_some() {
                self.refresh_settings();
            }
            self.ensure_cursor_visible();
            return Ok(());
        }
        // The Settings page shows the new values itself; a toast for each change is noise.
        if self.ui.settings.is_some() {
            self.refresh_settings();
            self.ensure_cursor_visible();
            return Ok(());
        }
        self.notify_message(
            if automatic {
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

impl App {
    pub(in crate::app) fn project_configuration(&mut self) {
        self.ui.dashboard_actions = self.configuration.dashboard_actions();
        self.ui.dashboard_selected = self
            .ui
            .dashboard_selected
            .min(self.ui.dashboard_actions.len().saturating_sub(1));
    }
}

impl App {
    pub(in crate::app) fn message(&self, key: &str, replacements: &[(&str, String)]) -> String {
        let mut message = self
            .configuration
            .settings()
            .messages
            .get(key)
            .expect("resolved configuration validates required messages")
            .clone();
        for (name, value) in replacements {
            message = message.replace(&format!("{{{name}}}"), value);
        }
        message
    }
}
