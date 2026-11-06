use crate::app::DiagnosticOwner;
use crate::app::App;
use crate::app::PickerAction;
use crate::app::PickerItem;
use crate::app::PickerItemId;
use crate::app::PickerOrigin;
use crate::app::PickerPayload;
use crate::app::PickerSurfaceKind;
use crate::app::TextBuffer;
use crate::app::LoadedConfig;
use crate::ui::render::TERMINAL_PANEL_MAXIMUM_HEIGHT;
use crate::ui::render::TERMINAL_PANEL_MINIMUM_HEIGHT;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;



impl App {

    pub(in crate::app) fn process_theme_results(&mut self) {
        if let Some(Err(error)) = self.configuration.poll_theme_restore() {
            self.show_error(error);
        }
        self.retire_stale_picker();
        if let Some(rows) = self
            .configuration
            .poll_theme_picker(self.search.theme_picker_key())
        {
            if !rows.loading
                && let Some(wanted) = self.configuration.take_wanted_theme()
            {
                self.apply_wanted_theme(&wanted, &rows);
                return;
            }
            let previous = self.search.picker_bookmark();
            self.present_theme_picker(rows);
            self.search.restore_picker_view(previous, false);
            self.sync_picker_ui();
        }
    }
}

impl App {

    fn apply_wanted_theme(
        &mut self,
        wanted: &str,
        rows: &crate::app::configuration::ThemeRows,
    ) {
        // "Rosé Pine", "rose pine" and "rose-pine" all name the same theme.
        let plain = |name: &str| name.to_lowercase().replace('é', "e").replace(' ', "-");
        let key = rows
            .items
            .iter()
            .find(|(_, name, _)| plain(name) == plain(wanted))
            .map(|(key, _, _)| key);
        let applied = key.map(|key| self.configuration.apply_theme(key));
        self.close_picker();
        match applied {
            Some(Ok(name)) => self.finish_theme_choice(&name),
            Some(Err(error)) => self.show_error(error),
            None => self.show_error(format!("no theme named {wanted}: :colorscheme lists them")),
        }
    }
}

impl App {

    pub(in crate::app) fn finish_theme_choice(&mut self, name: &str) {
        // The choice goes into config.toml like any other setting; once the reload lands, the
        // configured theme replaces the applied one. Without a config file it lives in the
        // editor state instead.
        let saved = self.configuration.user_settings_path().is_some_and(|path| {
            crate::config::write_user_setting(
                &path,
                "ui.theme",
                Some(toml::Value::String(name.into())),
            )
            .and_then(|()| {
                self.configuration
                    .request_reload()
                    .map_err(|error| error.to_string())
            })
            .map_err(|error| self.show_error(error))
            .is_ok()
        });
        let saving = saved || self.configuration.settings().persistence.enabled;
        if saving {
            self.persistence.request_save();
        }
        let message = format!(
            "Theme {}{}",
            crate::config::theme_display_name(name),
            if saving { "" } else { " (not saved)" }
        );
        if self.ui.settings.is_some() {
            self.refresh_settings();
            self.settings_status(message, false);
        } else {
            self.notify_message(message, false);
        }
    }
}

impl App {

    pub(in crate::app) fn present_theme_picker(&mut self, rows: crate::app::configuration::ThemeRows) {
        let items = rows
            .items
            .into_iter()
            .enumerate()
            .map(|(index, (key, name, origin))| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("theme:{index}")),
                        label: name,
                        detail: if index == 0 {
                            "Current".into()
                        } else {
                            String::new()
                        },
                        source: origin,
                        action: PickerAction::None,
                    },
                    PickerPayload::Theme(key),
                )
            })
            .collect();
        self.install_surface_picker_state(
            PickerSurfaceKind::Theme,
            "Dust themes".into(),
            items,
            String::new(),
            !rows.loading,
            PickerOrigin::General,
        );
        if !rows.diagnostics.is_empty() {
            self.show_error(rows.diagnostics.join("; "));
        }
    }
}

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
            self.window_session
                .accept_current(window_candidate, &self.state.window_version())?;
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
        let receipt = self
            .configuration
            .commit(candidate, self.extension.contributions())?;
        let (rules, _) = self.extension.prepare_window_rules(&window_contribution)?;
        rules.finish(receipt)?;
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.sync_ui_diagnostics();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.retire_workspace_resources("configuration changed");
        self.retire_plugin_edit_sources();
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
