use super::*;

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "config.reload",
        "Reload configuration transactionally",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ReloadConfiguration,
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
    for (id, title, effect) in [
        (
            "recovery.inspect",
            "Inspect recovery journals",
            AppEffect::RecoveryInspect,
        ),
        (
            "recovery.recover_latest",
            "Recover latest journal as scratch buffer",
            AppEffect::RecoveryRecoverLatest,
        ),
        (
            "recovery.discard_latest",
            "Discard latest recovery journal",
            AppEffect::RecoveryDiscardLatest,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "navigation.mark.set",
        "Set navigation mark",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| Ok(AppEffect::MarkSet(string_arg(invocation, "name")?.into())),
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
        "workspace.git.status.refresh",
        "Refresh Git status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitStatusRefresh,
    )?;
    register(
        registry,
        "workspace.git.hunk.next",
        "Go to next Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkNavigate(1),
    )?;
    register(
        registry,
        "workspace.git.hunk.previous",
        "Go to previous Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkNavigate(-1),
    )?;
    register(
        registry,
        "workspace.git.hunk.preview",
        "Preview selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkPreview,
    )?;
    register(
        registry,
        "workspace.git.hunk.reset",
        "Reset selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkReset,
    )?;
    register(
        registry,
        "workspace.git.hunk.stage",
        "Stage selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkStage,
    )?;
    register(
        registry,
        "workspace.git.hunks.open",
        "Open Git hunks",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunksOpen,
    )?;
    register(
        registry,
        "workspace.git.status.open",
        "Open Git status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitStatusOpen,
    )?;
    register(
        registry,
        "health.open",
        "Show health",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ShowHealth,
    )?;
    register(
        registry,
        "workspace.file.find",
        "Open file",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenWorkspaceFilePicker,
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
