//! Commands for settings, keymap inspection, themes and view toggles.

use crate::app::App;
use crate::app::commands::AppEffect;

use crate::app::commands::PromptMove;
use crate::app::commands::optional_string_arg;
use crate::app::commands::register;
use crate::app::commands::register_grammar_commands;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use crate::ui::PromptKind;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "theme.select",
        "Select Color Theme (Colorscheme)",
        ContextRequirement::Application,
        ArgumentSchema::default().field("name", ArgumentType::String, false),
        |invocation| {
            Ok(match optional_string_arg(invocation, "name") {
                Some(name) => AppEffect::SetTheme(name.to_owned()),
                None => AppEffect::SelectTheme,
            })
        },
    )?;
    register(
        registry,
        "config.reload",
        "Reload Settings",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ReloadConfiguration,
    )?;
    register(
        registry,
        "config.open",
        "Open Settings File (config.toml)",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenUserFile(crate::config::user_config_path()),
    )?;
    register(
        registry,
        "keymap.open",
        "Open Keymaps (keymaps.toml)",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenUserFile(crate::config::user_keymaps_path()),
    )?;
    register(
        registry,
        "config.explain",
        "Explain a resolved configuration value",
        ContextRequirement::Application,
        ArgumentSchema::default().field("key", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplainConfig(
                string_arg(invocation, "key")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "keymap.inspect",
        "Inspect a resolved keymap binding",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("id", ArgumentType::String, false)
            .field("context", ArgumentType::String, false)
            .field("key", ArgumentType::String, false)
            .field("command", ArgumentType::String, false),
        |invocation| AppEffect::InspectKeymap {
            id: optional_string_arg(invocation, "id").map(str::to_owned),
            context: optional_string_arg(invocation, "context").map(str::to_owned),
            key: optional_string_arg(invocation, "key").map(str::to_owned),
            command: optional_string_arg(invocation, "command").map(str::to_owned),
        },
    )?;
    register(
        registry,
        "keymap.conflicts",
        "Show keymap conflicts",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ShowKeymapConflicts,
    )?;
    register(
        registry,
        "recovery.inspect",
        "Recover unsaved work",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::RecoveryInspect,
    )?;
    register(
        registry,
        "application.safe_mode.status",
        "Show safe-mode status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::SafeModeStatus,
    )?;
    register(
        registry,
        "application.notifications.open",
        "Open notification history",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::NotificationHistory,
    )?;
    register(
        registry,
        "application.notification.dismiss",
        "Dismiss current notification",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::NotificationDismiss,
    )?;
    register(
        registry,
        "command_line.open",
        "Open command line",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("kind", ArgumentType::String, true)
            .field("initial", ArgumentType::String, false),
        |invocation| {
            let kind = match string_arg(invocation, "kind")? {
                "command" => PromptKind::Command,
                "search_forward" => PromptKind::SearchForward,
                "search_backward" => PromptKind::SearchBackward,
                value => {
                    return Err(CommandError::InvalidArguments(format!(
                        "unknown prompt kind `{value}`"
                    )));
                }
            };
            Ok(AppEffect::OpenPrompt {
                kind,
                initial: optional_string_arg(invocation, "initial")
                    .unwrap_or_default()
                    .to_owned(),
            })
        },
    )?;
    register(
        registry,
        "command_line.insert",
        "Insert prompt text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PromptInsert(
                string_arg(invocation, "text")?.into(),
            ))
        },
    )?;
    for (id, title, effect) in [
        (
            "command_line.backspace",
            "Prompt backspace",
            AppEffect::PromptBackspace,
        ),
        (
            "command_line.delete_word",
            "Prompt delete word",
            AppEffect::PromptDeleteWord,
        ),
        (
            "command_line.delete_to_start",
            "Prompt delete to start",
            AppEffect::PromptDeleteToStart,
        ),
        (
            "command_line.move_left",
            "Prompt left",
            AppEffect::PromptMove(PromptMove::Left),
        ),
        (
            "command_line.move_right",
            "Prompt right",
            AppEffect::PromptMove(PromptMove::Right),
        ),
        (
            "command_line.move_word_left",
            "Prompt word left",
            AppEffect::PromptMove(PromptMove::WordLeft),
        ),
        (
            "command_line.move_word_right",
            "Prompt word right",
            AppEffect::PromptMove(PromptMove::WordRight),
        ),
        (
            "command_line.move_home",
            "Prompt home",
            AppEffect::PromptMove(PromptMove::Home),
        ),
        (
            "command_line.move_end",
            "Prompt end",
            AppEffect::PromptMove(PromptMove::End),
        ),
        (
            "command_line.history.previous",
            "Previous prompt history",
            AppEffect::PromptHistory(-1),
        ),
        (
            "command_line.history.next",
            "Next prompt history",
            AppEffect::PromptHistory(1),
        ),
        (
            "command_line.complete.next",
            "Next prompt completion",
            AppEffect::PromptComplete(1),
        ),
        (
            "command_line.complete.previous",
            "Previous prompt completion",
            AppEffect::PromptComplete(-1),
        ),
        (
            "command_line.submit",
            "Submit prompt",
            AppEffect::PromptSubmit,
        ),
        (
            "command_line.cancel",
            "Cancel prompt",
            AppEffect::PromptCancel,
        ),
    ] {
        let effect = effect.clone();
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register_grammar_commands(registry)?;
    Ok(())
}

use crate::app::windows::LayoutAction;

impl App {
    pub(in crate::app) fn process_configuration_effect(
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
            AppEffect::Zen => {
                let explorer_open = self.ui.explorer.is_some();
                match self
                    .configuration
                    .toggle_zen(explorer_open, self.state.is_zoomed())
                {
                    None => {
                        if explorer_open {
                            let _ = self.process_explorer_effect(AppEffect::ExplorerClose);
                        }
                        if !self.state.is_zoomed() {
                            self.apply_layout_action(LayoutAction::Zoom);
                        }
                    }
                    Some((reopen_explorer, undo_zoom)) => {
                        if undo_zoom && self.state.is_zoomed() {
                            self.apply_layout_action(LayoutAction::Zoom);
                        }
                        if reopen_explorer {
                            let _ = self.process_explorer_effect(AppEffect::ExplorerOpen);
                        }
                    }
                }
                self.ensure_cursor_visible();
            }
            AppEffect::SelectTheme | AppEffect::SetTheme(_) => {
                self.close_picker();
                match self.configuration.begin_theme_picker() {
                    Ok(rows) => self.present_theme_picker(rows),
                    Err(error) => self.show_error(error),
                }
                self.configuration.want_theme(match effect {
                    AppEffect::SetTheme(name) => Some(name),
                    _ => None,
                });
            }
            AppEffect::ViewToggle(toggle) => {
                let enabled = self.configuration.toggle_view(toggle);
                let name = match toggle {
                    crate::app::configuration::ViewToggle::Wrap => "wrap",
                    crate::app::configuration::ViewToggle::LineNumbers => "line numbers",
                    crate::app::configuration::ViewToggle::RelativeNumbers => "relative numbers",
                    crate::app::configuration::ViewToggle::IndentGuides => "indent guides",
                    crate::app::configuration::ViewToggle::GitSigns => "git signs",
                    crate::app::configuration::ViewToggle::FormatOnSave => "format on save",
                };
                self.ensure_cursor_visible();
                self.notify_message(
                    format!("{name} {}", if enabled { "enabled" } else { "disabled" }),
                    false,
                );
            }
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
            effect => return Err(effect),
        }
        Ok(())
    }
}
