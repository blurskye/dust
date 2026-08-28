use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_misc_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::MarkSet(name) => {
                if name.len() > 64 || name.chars().any(char::is_control) {
                    return Ok(());
                }
                if let Some(location) = self.current_persisted_location() {
                    self.marks.retain(|mark| mark.name != name);
                    self.marks.push(PersistedMark { name, location });
                    let maximum = self.configuration.settings.persistence.marks;
                    if self.marks.len() > maximum {
                        self.marks.drain(..self.marks.len() - maximum);
                    }
                }
            }
            AppEffect::SafeModeStatus => {
                self.ui.notification = Some(self.message(
                    "safe_mode_status",
                    &[(
                        "state",
                        if self.safe_mode {
                            "active".into()
                        } else {
                            "inactive".into()
                        },
                    )],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::ShowHealth => {
                self.ui.notification = Some(
                    self.message(
                        "healthy",
                        &[
                            ("buffers", self.state.buffers.len().to_string()),
                            ("workspace", self.state.workspace_root.display().to_string()),
                            ("terminal", self.terminal_capabilities.clone()),
                            ("jobs", self.job_tasks.len().to_string()),
                            (
                                "ptys",
                                self.terminals
                                    .values()
                                    .filter(|terminal| terminal.running)
                                    .count()
                                    .to_string(),
                            ),
                            (
                                "preflights",
                                self.pending_execution_preflight.len().to_string(),
                            ),
                            ("tasks", self.task_status.len().to_string()),
                            ("plugins", self.plugins.statuses.len().to_string()),
                            (
                                "plugin_failures",
                                self.plugins
                                    .statuses
                                    .values()
                                    .filter(|status| {
                                        matches!(status, crate::extensions::PluginStatus::Failed(_))
                                    })
                                    .count()
                                    .to_string(),
                            ),
                            ("packages", self.packages.pending_count().to_string()),
                            ("lsp", self.lsp_sessions.len().to_string()),
                            (
                                "persistence",
                                self.persistence_pending.is_some().to_string(),
                            ),
                            ("safe", self.safe_mode.to_string()),
                            ("dap", self.dap.is_running().to_string()),
                        ],
                    ),
                );
                self.ui.notification_error = false;
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
