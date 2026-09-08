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
                    let maximum = self.configuration.settings().persistence.marks;
                    if self.marks.len() > maximum {
                        self.marks.drain(..self.marks.len() - maximum);
                    }
                }
            }
            AppEffect::SafeModeStatus => {
                self.notify_message(
                    self.message(
                        "safe_mode_status",
                        &[(
                            "state",
                            if self.workspace.safe_mode() {
                                "active".into()
                            } else {
                                "inactive".into()
                            },
                        )],
                    ),
                    false,
                );
            }
            AppEffect::ShowHealth => {
                self.notify_message(
                    self.message(
                        "healthy",
                        &[
                            ("buffers", self.state.buffers.len().to_string()),
                            ("workspace", self.state.workspace_root.display().to_string()),
                            ("terminal", self.notices.terminal_capabilities().to_owned()),
                            ("jobs", self.tasks.running_count().to_string()),
                            ("ptys", self.terminal_session.running_count().to_string()),
                            (
                                "preflights",
                                self.pending_execution_preflight.len().to_string(),
                            ),
                            ("tasks", self.tasks.status_count().to_string()),
                            ("plugins", self.extension.statuses().len().to_string()),
                            (
                                "plugin_failures",
                                self.extension
                                    .statuses()
                                    .values()
                                    .filter(|status| {
                                        matches!(status, crate::extensions::PluginStatus::Failed(_))
                                    })
                                    .count()
                                    .to_string(),
                            ),
                            ("packages", self.packages.pending_count().to_string()),
                            ("lsp", self.language.sessions().len().to_string()),
                            ("persistence", self.persistence.pending().to_string()),
                            ("safe", self.workspace.safe_mode().to_string()),
                            ("dap", self.debugger_session.is_running().to_string()),
                        ],
                    ),
                    false,
                );
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
