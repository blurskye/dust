//! Command registration and execution for settings pages and first-start setup.

use crate::app::App;
use crate::app::commands::AppEffect;

use crate::app::commands::PromptMove;
use crate::app::commands::SettingsAction;
use crate::app::commands::SetupAction;
use crate::app::commands::integer_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, action) in [
        ("settings.open", "Open Settings", SettingsAction::Open),
        (
            "settings.shortcuts",
            "Open Keyboard Shortcuts",
            SettingsAction::Shortcuts,
        ),
        (
            "settings.page.close",
            "Close Settings",
            SettingsAction::Close,
        ),
        (
            "settings.page.activate",
            "Change the selected setting",
            SettingsAction::Activate,
        ),
        (
            "settings.page.toggle",
            "Toggle the selected setting",
            SettingsAction::Toggle,
        ),
        (
            "settings.page.reset",
            "Reset the selected setting to its default",
            SettingsAction::Reset,
        ),
        (
            "settings.page.search",
            "Search settings",
            SettingsAction::Search,
        ),
        (
            "settings.page.modified",
            "Show only modified settings",
            SettingsAction::Modified,
        ),
        (
            "settings.page.open_file",
            "Open config.toml",
            SettingsAction::OpenFile,
        ),
        (
            "settings.page.cancel",
            "Cancel or close",
            SettingsAction::Cancel,
        ),
        (
            "settings.page.backspace",
            "Delete a character",
            SettingsAction::Backspace,
        ),
        (
            "settings.page.clear",
            "Clear the text",
            SettingsAction::Clear,
        ),
        (
            "settings.page.left",
            "Cursor left",
            SettingsAction::Cursor(PromptMove::Left),
        ),
        (
            "settings.page.right",
            "Cursor right",
            SettingsAction::Cursor(PromptMove::Right),
        ),
        (
            "settings.page.word_left",
            "Cursor a word left",
            SettingsAction::Cursor(PromptMove::WordLeft),
        ),
        (
            "settings.page.word_right",
            "Cursor a word right",
            SettingsAction::Cursor(PromptMove::WordRight),
        ),
        (
            "settings.page.home",
            "Cursor to start",
            SettingsAction::Cursor(PromptMove::Home),
        ),
        (
            "settings.page.end",
            "Cursor to end",
            SettingsAction::Cursor(PromptMove::End),
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::Settings(action.clone()),
        )?;
    }
    register(
        registry,
        "settings.page.move",
        "Move through settings",
        ContextRequirement::Application,
        ArgumentSchema::default().field("by", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::Settings(SettingsAction::Move(integer_arg(
                invocation, "by",
            )?)))
        },
    )?;
    for (id, title, action) in [
        ("settings.setup", "Run First-Start Setup", SetupAction::Open),
        (
            "settings.setup.next",
            "Take the choice and continue",
            SetupAction::Next,
        ),
        (
            "settings.setup.back",
            "Back to the previous question",
            SetupAction::Back,
        ),
        ("settings.setup.skip", "Skip setup", SetupAction::Skip),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::Setup(action),
        )?;
    }
    register(
        registry,
        "settings.setup.move",
        "Move through setup choices",
        ContextRequirement::Application,
        ArgumentSchema::default().field("by", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::Setup(SetupAction::Move(integer_arg(
                invocation, "by",
            )?)))
        },
    )?;
    register(
        registry,
        "settings.page.section",
        "Jump to another settings section",
        ContextRequirement::Application,
        ArgumentSchema::default().field("by", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::Settings(SettingsAction::Section(integer_arg(
                invocation, "by",
            )?)))
        },
    )?;
    register(
        registry,
        "settings.page.insert",
        "Type into the settings page",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::Settings(SettingsAction::Insert(
                string_arg(invocation, "text")?.into(),
            )))
        },
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_settings_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Settings(action) => self.settings_action(action),
            AppEffect::Setup(action) => self.setup_action(action),
            effect => return Err(effect),
        }
        Ok(())
    }
}
