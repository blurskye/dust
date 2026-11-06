
use crate::ui::render::SCRATCH_LABEL;
use crate::app::commands::SyntaxFoldAction;
use crate::app::AppEffect;
use crate::app::DiagnosticOwner;
use crate::app::App;
use crate::app::PickerAction;
use crate::app::PickerItem;
use crate::app::PickerItemId;
use crate::app::PickerPayload;
use crate::app::TrustedAction;
use crate::app::builtin_languages;
use crate::app::plugins::VirtualContext;
use crate::app::command;
use crate::app::language_for_document;







use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::app::commands::optional_bool_arg;
use crate::app::commands::bool_arg;
use crate::app::ViewToggle;
use crate::app::commands::one_character_arg;

pub(in crate::app) fn register_commands(registry: &mut CommandRegistry<AppEffect>) -> Result<(), CommandError> {

    register(
        registry,
        "editor.quit",
        "Quit Dust",
        ContextRequirement::Application,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::Quit {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    register(
        registry,
        "input.mapping.cancel",
        "Cancel pending mapping",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CancelPendingMapping,
    )?;
    register(
        registry,
        "editor.flash",
        "Jump to a visible match by its label",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::FlashBegin,
    )?;
    register(
        registry,
        "edit.repeat_last",
        "Repeat last change",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |invocation| AppEffect::RepeatLast(invocation.count),
    )?;
    register(
        registry,
        "macro.record.start",
        "Start macro recording",
        ContextRequirement::View,
        ArgumentSchema::default().field("register", ArgumentType::String, true),
        |invocation| {
            let name = one_character_arg(invocation, "register")?;
            Ok(AppEffect::MacroRecordStart(name))
        },
    )?;
    register(
        registry,
        "macro.record.stop",
        "Stop macro recording",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::MacroRecordStop,
    )?;
    register(
        registry,
        "macro.replay",
        "Replay macro",
        ContextRequirement::View,
        ArgumentSchema::default().field("register", ArgumentType::String, true),
        |invocation| {
            let name = one_character_arg(invocation, "register")?;
            Ok(AppEffect::MacroReplay {
                register: name,
                count: invocation.count.map_or(1, |count| count.get()),
            })
        },
    )?;
    register(
        registry,
        "view.scroll",
        "Scroll view",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("kind", ArgumentType::String, true)
            .field("at_mouse", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::Scroll {
                kind: string_arg(invocation, "kind")?.into(),
                count: invocation.count.map_or(1, |count| count.get()),
                at_mouse: optional_bool_arg(invocation, "at_mouse").unwrap_or(false),
            })
        },
    )?;
    register(
        registry,
        "view.cursor.place_mouse",
        "Place cursor at mouse position",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::PlaceMouseCursor,
    )?;
    register(
        registry,
        "view.selection.extend_mouse",
        "Extend selection to mouse position",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::ExtendMouseSelection,
    )?;
    for (id, title, toggle) in [
        ("view.wrap.toggle", "Toggle line wrap", ViewToggle::Wrap),
        (
            "view.line_numbers.toggle",
            "Toggle line numbers",
            ViewToggle::LineNumbers,
        ),
        (
            "view.relative_numbers.toggle",
            "Toggle relative line numbers",
            ViewToggle::RelativeNumbers,
        ),
        (
            "view.indent_guides.toggle",
            "Toggle indent guides",
            ViewToggle::IndentGuides,
        ),
        (
            "view.git_signs.toggle",
            "Toggle git signs",
            ViewToggle::GitSigns,
        ),
        (
            "view.format_on_save.toggle",
            "Toggle format on save",
            ViewToggle::FormatOnSave,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::ViewToggle(toggle),
        )?;
    }
    register(
        registry,
        "view.zen.toggle",
        "Toggle zen mode",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::Zen,
    )?;
    register(
        registry,
        "edit.substitute_rest",
        "Replace every remaining match",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| Ok(AppEffect::SubstituteAll),
    )?;
Ok(())
}

use crate::app::PluginUiInteraction;

impl App {
    pub(in crate::app) fn process_editing_effect(&mut self, effect: AppEffect) -> Result<(), AppEffect> {
        match effect {
            AppEffect::OpenWorkspaceFilePicker => self.start_workspace_file_picker(),
            AppEffect::WalkJumps(back) => self.walk_jumps(back),
            AppEffect::FlashBegin => self.begin_flash(),
            AppEffect::OpenPrompt { kind, initial } => self.open_prompt(kind, initial),
            AppEffect::NotificationHistory => {
                let items = self
                    .notices
                    .history()
                    .rev()
                    .enumerate()
                    .map(|(index, (error, message))| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("notification:{index}")),
                                label: message.clone(),
                                detail: if *error {
                                    "error".into()
                                } else {
                                    "info".into()
                                },
                                source: "application".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Notice {
                                title: "Notification".into(),
                                text: message.clone(),
                            },
                        )
                    })
                    .collect();
                self.clear_notice();
                self.install_picker("Notifications".into(), items);
            }
            AppEffect::NotificationDismiss => {
                self.clear_notice();
            }
            AppEffect::SubstituteAll => {
                if self.substitute_confirm.is_some() {
                    self.answer_substitution(true, true);
                }
            }
            AppEffect::Scroll {
                kind,
                count,
                at_mouse,
            } => {
                if at_mouse {
                    self.focus_mouse_pane();
                }
                self.scroll_view(&kind, count);
            }
            AppEffect::PlaceMouseCursor => self.apply_mouse_cursor(false),
            AppEffect::ExtendMouseSelection => self.apply_mouse_cursor(true),
            AppEffect::ViewToggle(toggle) => {
                let enabled = self.configuration.toggle_view(toggle);
                let name = match toggle {
                    crate::app::ViewToggle::Wrap => "wrap",
                    crate::app::ViewToggle::LineNumbers => "line numbers",
                    crate::app::ViewToggle::RelativeNumbers => "relative numbers",
                    crate::app::ViewToggle::IndentGuides => "indent guides",
                    crate::app::ViewToggle::GitSigns => "git signs",
                    crate::app::ViewToggle::FormatOnSave => "format on save",
                };
                self.ensure_cursor_visible();
                self.notify_message(
                    format!("{name} {}", if enabled { "enabled" } else { "disabled" }),
                    false,
                );
            }
            AppEffect::RetainedUiFocus(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Focus(delta));
            }
            AppEffect::RetainedUiScroll(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Scroll(delta));
            }
            AppEffect::RetainedUiActivate => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Activate);
            }
            AppEffect::RetainedUiMouseSelect => self.select_retained_ui_at_mouse(),
            AppEffect::PrivateUiActivate { at_mouse } => {
                self.activate_private_surface_action(at_mouse);
            }
            AppEffect::RepeatLast(count) => self.repeat_last_change(count),
            AppEffect::MacroRecordStart(register) => {
                self.input.start_macro_recording(register);
                self.notify_message(
                    self.message("macro_recording", &[("register", register.to_string())]),
                    false,
                );
            }
            AppEffect::MacroRecordStop => {
                if let Some((register, invocations)) = self.input.finish_macro_recording() {
                    self.state.registers.set_macro(register, invocations);
                    self.state.mark_editor_changed();
                    self.notify_message(
                        self.message("macro_recorded", &[("register", register.to_string())]),
                        false,
                    );
                } else {
                    self.show_error(self.message("no_macro_recording", &[]));
                }
            }
            AppEffect::MacroReplay { .. } => {
                unreachable!("macro effects return their status through process_effect_checked")
            }
            AppEffect::Grammar(input) => self.process_grammar_input(input),
            AppEffect::CancelPendingMapping => self.cancel_pending_mapping(),
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
                let conflicts = self
                    .configuration
                    .rejected_keymaps()
                    .into_iter()
                    .chain(
                        self.configuration
                            .keymaps()
                            .conflicts
                            .iter()
                            .map(String::as_str),
                    )
                    .collect::<Vec<_>>();
                if conflicts.is_empty() {
                    self.notify_message(self.message("no_keymap_conflicts", &[]), false);
                } else {
                    self.show_error(conflicts.join("; "));
                }
            }
            AppEffect::RecoveryInspect => self.open_recovery_picker(),
            AppEffect::HistoryClear => {
                self.input.clear_history();
                self.notify_message(self.message("history_cleared", &[]), false);
            }
            AppEffect::SyntaxFold(action) => {
                if let Some(view) = self.state.active_view {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    let folds = self
                        .ui
                        .syntax_folds
                        .iter_mut()
                        .filter(|fold| fold.view == view);
                    match action {
                        SyntaxFoldAction::OpenAll => folds.for_each(|fold| fold.collapsed = false),
                        SyntaxFoldAction::CloseAll => folds.for_each(|fold| fold.collapsed = true),
                        SyntaxFoldAction::Toggle
                        | SyntaxFoldAction::Open
                        | SyntaxFoldAction::Close => {
                            // The innermost fold around the cursor, the one Vim's `za` acts on.
                            if let Some(fold) = folds
                                .filter(|fold| {
                                    line.is_some_and(|line| {
                                        line >= fold.start_line && line <= fold.end_line
                                    })
                                })
                                .max_by_key(|fold| fold.start_line)
                            {
                                fold.collapsed = match action {
                                    SyntaxFoldAction::Open => false,
                                    SyntaxFoldAction::Close => true,
                                    _ => !fold.collapsed,
                                };
                            } else {
                                self.notify_message(self.message("syntax_no_fold", &[]), false);
                            }
                        }
                    }
                }
            }
            AppEffect::SyntaxIndentLine => self.apply_syntax_indent_line(),
            AppEffect::SyntaxStatus => {
                let active = self.state.active_buffer_id();
                let active_language = active
                    .and_then(|buffer| self.state.buffers.get(buffer))
                    .and_then(|buffer| {
                        crate::language::syntax::language_for_document(
                            buffer.path.as_deref(),
                            buffer.text.chunks().next().unwrap_or_default(),
                        )
                    });
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
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("syntax:{index}:{}", info.id)),
                                label: info.language.name().to_owned(),
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
            AppEffect::LintRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("linter_matcher_missing", &[]));
                    return Ok(());
                };
                self.run_trusted(TrustedAction::RunLinter { name, matcher });
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
                match crate::app::tasks::FormatterSource::capture(&self.state, buffer) {
                    Ok(source) => self.run_trusted(TrustedAction::RunFormatter { name, source }),
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::VirtualDocumentOpen { source, key } => {
                let result = self.extension.open_virtual(&source, key, &self.state);
                self.route_virtual_open(result);
            }
            kind @ (AppEffect::HealthVirtualOpen | AppEffect::HealthVirtualRefresh) => {
                let result = self
                    .extension
                    .prepare_health(
                        matches!(kind, AppEffect::HealthVirtualRefresh),
                        self.commands.specs().count(),
                        VirtualContext {
                            editor: &self.state,
                            windows: &self.window_session,
                            scratch: SCRATCH_LABEL,
                        },
                    )
                    .and_then(|staged| {
                        Self::accept_virtual_publication(&mut self.window_session, staged)
                    });
                match result {
                    Ok(state) => {
                        *self.state = state;
                        self.set_dashboard_visibility(false);
                        self.reconcile_editor_windows();
                    }
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::VirtualDocumentRefresh => {
                let result = self
                    .state
                    .active_buffer_id()
                    .ok_or_else(|| "no active virtual document".to_owned())
                    .and_then(|buffer| self.extension.refresh_virtual(buffer, &self.state));
                self.route_virtual_open(result);
            }
            AppEffect::Substitute {
                range,
                pattern,
                replacement,
                flags,
            } => self.run_substitute(&range, &pattern, &replacement, &flags),
            AppEffect::GlobalDelete {
                range,
                pattern,
                invert,
            } => self.run_global_delete(&range, &pattern, invert),
            effect => return Err(effect),
        }
        Ok(())
    }
}
