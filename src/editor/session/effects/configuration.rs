use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_configuration_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::ReloadConfiguration => match self.configuration.request_reload() {
                Ok(()) => {
                    self.notify_message(self.message("config_reload_queued", &[]), false);
                }
                Err(error) => self.show_error(error),
            },
            AppEffect::ExplainConfig(key) => {
                if let Some(explanation) = self.configuration.explain(&key) {
                    self.notify_message(
                        self.message(
                            "config_explanation",
                            &[
                                ("key", explanation.key),
                                ("value", explanation.value),
                                ("source", explanation.source),
                            ],
                        ),
                        false,
                    );
                } else {
                    self.show_error(self.message("unknown_config_key", &[("key", key)]));
                }
            }
            AppEffect::InspectKeymap {
                id,
                context,
                key,
                command,
            } => {
                let context = context.unwrap_or_else(|| self.active_input_context());
                let inspection = id
                    .as_deref()
                    .and_then(|id| self.configuration.keymaps().inspect_id(id))
                    .or_else(|| {
                        key.as_deref().and_then(|keys| {
                            self.configuration.keymaps().inspect_key(&context, keys)
                        })
                    })
                    .or_else(|| {
                        command.as_deref().and_then(|command| {
                            self.configuration.keymaps().inspect(&context, command)
                        })
                    });
                if let Some(inspection) = inspection {
                    self.notify_message(
                        self.message(
                            "keymap_inspection",
                            &[
                                ("id", inspection.id),
                                ("context", inspection.context),
                                ("source_keys", inspection.source_keys),
                                ("keys", inspection.keys),
                                ("operation", inspection.operation.as_str().into()),
                                ("command", inspection.command.unwrap_or_default()),
                                ("arguments", format!("{:?}", inspection.arguments)),
                                ("source", inspection.source),
                            ],
                        ),
                        false,
                    );
                } else if id.is_none() && command.is_none() && key.is_none() {
                    let count = self
                        .configuration
                        .keymaps()
                        .reverse
                        .keys()
                        .filter(|(mapped_context, _)| mapped_context == &context)
                        .count();
                    self.notify_message(
                        self.message(
                            "keymap_context_summary",
                            &[("context", context), ("count", count.to_string())],
                        ),
                        false,
                    );
                } else {
                    self.show_error(self.message("no_keymap_binding", &[("context", context)]));
                }
            }
            AppEffect::ShowKeymapConflicts => {
                if self.configuration.keymaps().conflicts.is_empty() {
                    self.notify_message(self.message("no_keymap_conflicts", &[]), false);
                } else {
                    self.show_error(self.configuration.keymaps().conflicts.join("; "));
                }
            }
            AppEffect::RecoveryInspect => {
                self.notify_message(
                    self.message(
                        "recovery_summary",
                        &[("count", self.recovery.records().len().to_string())],
                    ),
                    false,
                );
            }
            AppEffect::RecoveryRecoverLatest => {
                if let Some((_, record)) = self.recovery.records().last() {
                    self.state
                        .open_text(TextBuffer::from_text(&record.content), None);
                    self.set_dashboard_visibility(false);
                    self.notify_message(self.message("recovery_opened", &[]), false);
                } else {
                    self.show_error(self.message("recovery_none", &[]));
                }
            }
            AppEffect::RecoveryDiscardLatest => {
                match self.recovery.discard_latest(self.documents.recovery_io()) {
                    Ok(true) => {}
                    Ok(false) => self.show_error(self.message("recovery_none", &[])),
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::SessionSave => {
                self.save_persistence();
                self.notify_message(self.message("session_save_queued", &[]), false);
            }
            AppEffect::SessionLoad => {
                self.load_persistence();
                self.notify_message(self.message("session_load_queued", &[]), false);
            }
            AppEffect::HistoryClear => {
                self.input.clear_history();
                self.notify_message(self.message("history_cleared", &[]), false);
            }
            AppEffect::LargeFileSet(mode) => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let mode = match mode.as_str() {
                    "auto" => LargeFileOverride::Auto,
                    "on" => LargeFileOverride::On,
                    "off" => LargeFileOverride::Off,
                    _ => {
                        self.show_error(self.message("large_file_invalid", &[]));
                        return Ok(());
                    }
                };
                self.documents.set_large_file_mode(
                    buffer,
                    self.state.buffers[buffer].path.as_deref(),
                    mode,
                );
                self.process_large_file_modes();
                self.notify_message(
                    self.message(
                        "large_file_changed",
                        &[("mode", mode_name_large(mode).into())],
                    ),
                    false,
                );
            }
            AppEffect::LargeFileStatus => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let configured = self.documents.configured_mode(buffer);
                let effective = if self.effective_large_file(buffer) {
                    "on"
                } else {
                    "off"
                };
                self.notify_message(
                    self.message(
                        "large_file_status",
                        &[
                            ("mode", mode_name_large(configured).into()),
                            ("effective", effective.into()),
                        ],
                    ),
                    false,
                );
            }
            AppEffect::SyntaxFoldToggle => {
                if let Some(view) = self.state.active_view {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    if let Some(fold) = self.ui.syntax_folds.iter_mut().find(|fold| {
                        fold.view == view
                            && line.is_some_and(|line| {
                                line >= fold.start_line && line <= fold.end_line
                            })
                    }) {
                        fold.collapsed = !fold.collapsed;
                    }
                }
            }
            AppEffect::SyntaxIndentLine => self.apply_syntax_indent_line(),
            AppEffect::SyntaxStatus => {
                let active = self.state.active_buffer_id();
                let active_language = active
                    .and_then(|buffer| self.state.buffers.get(buffer))
                    .and_then(|buffer| buffer.path.as_deref())
                    .and_then(language_for_path);
                let items = builtin_languages()
                    .enumerate()
                    .map(|(index, info)| {
                        let state = if Some(info.language) == active_language {
                            if let Some(buffer) = active {
                                if let Some(error) = self.syntax.failure(buffer) {
                                    self.message(
                                        "syntax_state_failed",
                                        &[("error", error.to_owned())],
                                    )
                                } else if self.syntax.result(buffer).is_some_and(|result| {
                                    self.state.buffers.get(buffer).is_some_and(|state| {
                                        state.text.revision().0 == result.revision
                                    }) && result.language == info.language
                                }) {
                                    self.message(
                                        if self
                                            .syntax
                                            .result(buffer)
                                            .is_some_and(|result| result.truncated)
                                        {
                                            "syntax_state_active_truncated"
                                        } else {
                                            "syntax_state_active"
                                        },
                                        &[],
                                    )
                                } else {
                                    self.message("syntax_state_pending", &[])
                                }
                            } else {
                                self.message("syntax_state_available", &[])
                            }
                        } else {
                            self.message("syntax_state_available", &[])
                        };
                        (
                            PickerItem {
                                id: PickerItemId(format!("syntax:{index}:{}", info.id)),
                                label: self.syntax_language_label(info.language),
                                detail: self.message(
                                    "syntax_capabilities",
                                    &[
                                        (
                                            "highlights",
                                            self.syntax_support_label(info.capabilities.highlights),
                                        ),
                                        (
                                            "folds",
                                            self.syntax_support_label(info.capabilities.folds),
                                        ),
                                        (
                                            "indents",
                                            self.syntax_support_label(info.capabilities.indents),
                                        ),
                                        ("state", state),
                                    ],
                                ),
                                source: self.message("syntax_builtin_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("syntax_status_title", &[]), items);
            }
            AppEffect::TestRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("test_matcher_missing", &[]));
                    return Ok(());
                };
                self.defer_trusted_execution(DeferredExecution::RunTest { name, matcher });
            }
            AppEffect::TestStop(name) => self.stop_task(&name),
            AppEffect::TestList(name) => self.show_test_picker(&name),
            AppEffect::TaskList => self.show_task_picker(),
            AppEffect::TaskRestart(name) => {
                self.stop_task(&name);
                self.defer_trusted_execution(DeferredExecution::RunTask(name));
            }
            AppEffect::LintRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("linter_matcher_missing", &[]));
                    return Ok(());
                };
                self.defer_trusted_execution(DeferredExecution::RunLinter { name, matcher });
            }
            AppEffect::LintClear(name) => {
                if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                    self.diagnostics.clear_owner(&owner);
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::FormatRun(name) => {
                let Some(view_id) = self.state.active_view else {
                    return Ok(());
                };
                let Some(view) = self.state.views.get(view_id) else {
                    return Ok(());
                };
                let buffer = view.buffer;
                match crate::editor::session::tasks::FormatterSource::capture(&self.state, buffer) {
                    Ok(source) => self
                        .defer_trusted_execution(DeferredExecution::RunFormatter { name, source }),
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::TaskRun(name) => {
                self.defer_trusted_execution(DeferredExecution::RunTask(name))
            }
            AppEffect::TaskStop(name) => self.stop_task(&name),
            AppEffect::TaskOutput(name) => self.show_output_picker(&name),
            AppEffect::CrashInspect => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                self.notify_message(
                    self.message("crash_location", &[("path", path.display().to_string())]),
                    path.is_file(),
                );
            }
            AppEffect::CrashClear => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                match self.documents.delete(path) {
                    Ok(id) => {
                        self.pending_vfs.insert(id, PendingVfs::CrashDelete);
                    }
                    Err(error) => self.show_error(error),
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
