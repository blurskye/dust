//! Command registration and execution for editing commands, transactions and replay.

use crate::app::App;
use crate::app::commands::AppEffect;

use crate::app::commands::bool_arg;
use crate::app::commands::one_character_arg;
use crate::app::commands::optional_bool_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::app::configuration::ViewToggle;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
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

impl App {
    pub(in crate::app) fn process_editing_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::FlashBegin => self.begin_flash(),
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
