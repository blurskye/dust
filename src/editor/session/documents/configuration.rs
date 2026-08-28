use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_configuration_result(&mut self) {
        let result = match self.configuration_worker.try_result() {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if !configuration_generation_is_newer(self.configuration_generation, result.generation) {
            return;
        }
        self.configuration_generation = result.generation;
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
        let _ = self.configuration_worker.configure_watch(
            self.configuration.settings.config_watch.clone(),
            self.configuration.settings.ui.theme.clone(),
        );
    }

    pub(in crate::editor::session) fn apply_configuration_candidate(
        &mut self,
        loaded: LoadedConfig,
        automatic: bool,
    ) -> Result<(), String> {
        let mut candidate = match resolve_configuration(&self.commands, loaded, true) {
            Ok((candidate, None)) => candidate,
            Ok((_, Some(diagnostic))) | Err(diagnostic) => return Err(diagnostic),
        };
        let plugins_changed = candidate.settings.plugins != self.configuration.settings.plugins;
        let replacement_explorer_watch = if candidate.settings.ui.explorer.watch
            != self.configuration.settings.ui.explorer.watch
        {
            Some(
                ExplorerWatchService::start(explorer_watch_policy(
                    &candidate.settings.ui.explorer.watch,
                ))
                .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        if plugins_changed {
            return Err(
                "live plugin topology reload is not yet staged; previous configuration retained"
                    .into(),
            );
        }
        if !plugins_changed {
            candidate.keymaps = compile_keymaps_with_plugin_catalog(
                &self.commands,
                &candidate.raw_keymaps,
                &self.plugin_contributions,
                &candidate.settings.input.keymaps,
            )
            .map_err(|errors| errors.join("; "))?;
        }
        let window_candidate = (candidate.settings.windows != self.configuration.settings.windows)
            .then(|| self.prepare_configured_window_rules(&candidate.settings.windows))
            .transpose()?;
        self.configuration_worker
            .configure_watch(
                candidate.settings.config_watch.clone(),
                candidate.settings.ui.theme.clone(),
            )
            .map_err(|error| error.to_string())?;
        let restart_required = candidate.settings.runtime.vfs_queue_capacity
            != self.configuration.settings.runtime.vfs_queue_capacity
            || candidate.settings.ui.alternate_screen
                != self.configuration.settings.ui.alternate_screen
            || candidate.settings.ui.bracketed_paste
                != self.configuration.settings.ui.bracketed_paste
            || candidate.settings.ui.focus_events != self.configuration.settings.ui.focus_events;
        self.cancel_pending_mapping();
        self.input
            .set_count_limit(candidate.settings.input.count_limit);
        self.ui.dashboard_actions = dashboard_actions(&candidate);
        self.state.policy = crate::editor::EditingPolicy {
            tab_width: candidate.settings.editor.tab_width,
            indent_width: candidate.settings.editor.indent_width,
            expand_tabs: candidate.settings.editor.expand_tabs,
            undo_break_on_text_commit: candidate.settings.editor.undo_break_on_text_commit,
            registers_enabled: candidate.settings.editor.registers.storage
                == crate::config::RegisterStorage::Internal,
            mirror_named_to_unnamed: candidate.settings.editor.registers.mirror_named_to_unnamed,
            ambiguous_width_wide: candidate.settings.editor.ambiguous_width
                == crate::config::AmbiguousWidth::Wide,
        };
        if candidate.settings.observability != self.configuration.settings.observability {
            self.logs = make_logs(&candidate.settings, true);
            self.logs.record(
                LogLevel::Info,
                "configuration",
                "logging-reconfigured",
                BTreeMap::new(),
            );
        }
        if candidate.settings.recovery != self.configuration.settings.recovery {
            self.recovery_discovery_queued = false;
            self.recovery_cleanup_queued = false;
            self.recovery_pruned_revisions.clear();
        }
        if !candidate.settings.recovery.enabled {
            self.recovery_scheduled.clear();
        }
        if !candidate.settings.history.command.enabled {
            self.command_history.clear();
        } else {
            let excess = self
                .command_history
                .len()
                .saturating_sub(candidate.settings.history.command.maximum_entries);
            self.command_history.drain(..excess);
        }
        if !candidate.settings.history.search.enabled {
            self.search_history.clear();
        } else {
            let excess = self
                .search_history
                .len()
                .saturating_sub(candidate.settings.history.search.maximum_entries);
            self.search_history.drain(..excess);
        }
        self.diagnostics.set_limits(
            candidate.settings.workspace.maximum_diagnostics,
            candidate
                .settings
                .workspace
                .maximum_diagnostics_per_owner_uri,
        );
        self.sync_ui_diagnostics();
        if self.ui.dashboard && !candidate.settings.startup.show_dashboard {
            self.state.open_text(TextBuffer::from_text(""), None);
            self.ui.dashboard = false;
        }
        if plugins_changed {
            for (plugin, (load, token)) in self.package_prepared.drain().collect::<Vec<_>>() {
                let _ = self.plugins.unload_generation(&plugin, load);
                self.remove_plugin_contribution_generation(&plugin, load);
                let _ = self.packages.request(PackageOperation::Abort { token });
            }
            self.package_startup_pending.clear();
            self.package_managed_plugins.clear();
            let ids = self.plugins.statuses.keys().cloned().collect::<Vec<_>>();
            for id in ids {
                let load = self.plugins.load_id(&id);
                let _ = self.plugins.disable(&id);
                if let Some(load) = load {
                    self.remove_plugin_contribution_generation(&id, load);
                }
            }
            self.plugin_contributions = PluginContributionCatalog::default();
            self.plugin_host_events = PluginHostEventQueue::default();
            self.plugin_host_event_observation = None;
        }
        self.ui.documentation_popup = None;
        self.configuration = candidate;
        let maximum_inlays =
            self.configuration.settings.lsp.inlay_hint_level == crate::config::InlayHintLevel::Max;
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
        if let Some(watch) = replacement_explorer_watch {
            self.explorer_watch = watch;
            self.explorer_watch_generation = None;
            self.explorer_watch_observation = None;
            self.explorer_watch_reload_queue.clear();
        }
        if let Some((graph, mut surfaces)) = window_candidate {
            surfaces.invalidate();
            self.windows = graph;
            self.surface_windows = surfaces;
        }
        if !self.configuration.settings.ui.explorer.bundled_enabled {
            self.ui.explorer = None;
            self.ui.explorer_focused = false;
        }
        self.explorer_watch_observation = None;
        self.reconcile_explorer_watch();
        self.terminal_panel_height = self.terminal_panel_height.clamp(
            self.configuration.settings.ui.terminal_panel.minimum_height,
            self.configuration.settings.ui.terminal_panel.maximum_height,
        );
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        if plugins_changed {
            self.load_configured_plugins();
            self.package_startup_rollback_attempted = false;
            match self.packages.request(PackageOperation::Activations) {
                Ok(id) => self.package_startup_request = Some(id),
                Err(error) => self.show_error(error),
            }
        }
        self.rebuild_matcher_cache();
        if let Some(menu) = &self.ui.tool_menu {
            let category = menu.category.clone();
            self.rebuild_tool_menu(&category, None);
        }
        self.ui.config_diagnostic = None;
        self.ui.notification = Some(if restart_required {
            self.message("reloaded_restart", &[])
        } else if automatic {
            self.message("auto_reloaded", &[])
        } else {
            self.message("reloaded", &[])
        });
        self.ui.notification_error = false;
        self.ensure_cursor_visible();
        Ok(())
    }
}
