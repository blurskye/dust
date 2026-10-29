use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    PromptKind, PromptMove, optional_string_arg, register, register_grammar_commands, string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "theme.select",
        "Select theme (live preview; remember in editor state)",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::SelectTheme,
    )?;
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
        "workspace.git.commit",
        "Compose and create a commit from the staged set",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitCommit,
    )?;
    register(
        registry,
        "workspace.git.commit.amend",
        "Compose an amend of the current commit",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitCommitAmend,
    )?;
    register(
        registry,
        "workspace.git.history.open",
        "Open Git history as a bounded commit list",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHistoryOpen,
    )?;
    register(
        registry,
        "workspace.git.branches.open",
        "Open the Git branch list",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitBranchesOpen,
    )?;
    register(
        registry,
        "workspace.git.worktrees.open",
        "Open the Git worktree list",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitWorktreesOpen,
    )?;
    register(
        registry,
        "workspace.git.branch.delete",
        "Delete the selected local branch",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitBranchDelete,
    )?;
    register(
        registry,
        "workspace.git.worktree.remove",
        "Remove the selected worktree and its directory",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitWorktreeRemove,
    )?;
    register(
        registry,
        "workspace.git.branch.create",
        "Create a branch, optionally from a start point",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("name", ArgumentType::String, true)
            .field("start", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::GitBranchCreate {
                name: string_arg(invocation, "name")?.into(),
                start: optional_string_arg(invocation, "start").map(str::to_owned),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.branch.track",
        "Create a local branch tracking a remote branch",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("name", ArgumentType::String, true)
            .field("remote_branch", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitBranchTrack {
                name: string_arg(invocation, "name")?.into(),
                remote_branch: string_arg(invocation, "remote_branch")?.into(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.branch.rename",
        "Rename the selected branch",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("to", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitBranchRename {
                to: string_arg(invocation, "to")?.into(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.worktree.create",
        "Create a worktree for a branch",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("name", ArgumentType::String, true)
            .field("path", ArgumentType::String, true)
            .field("reference", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitWorktreeCreate {
                name: string_arg(invocation, "name")?.into(),
                path: std::path::PathBuf::from(string_arg(invocation, "path")?),
                reference: string_arg(invocation, "reference")?.into(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.worktree.open",
        "Open the selected worktree as a workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitWorktreeOpen),
    )?;
    register(
        registry,
        "workspace.git.worktree.close",
        "Close the selected worktree as a workspace root without removing it",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitWorktreeClose),
    )?;
    register(
        registry,
        "workspace.git.remote.fetch",
        "Fetch the named remote and report what moved",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("remote", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::GitRemoteFetch {
                remote: optional_string_arg(invocation, "remote").map(str::to_owned),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.remote.update",
        "Review a fast-forward update from the remote",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitRemoteUpdate),
    )?;
    register(
        registry,
        "workspace.git.remote.update.apply",
        "Apply the reviewed fast-forward update",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitRemoteApply),
    )?;
    register(
        registry,
        "workspace.git.remote.push",
        "Push the current branch to a named remote and destination, without forcing",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("remote", ArgumentType::String, false)
            .field("destination", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::GitRemotePush {
                remote: optional_string_arg(invocation, "remote").map(str::to_owned),
                destination: optional_string_arg(invocation, "destination").map(str::to_owned),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.conflicts.open",
        "Show the paused operation and each conflicted path's base, ours and theirs",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictsOpen),
    )?;
    register(
        registry,
        "workspace.git.conflict.choose",
        "Accept one side of the active conflict into the result buffer",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("side", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitConflictChoose {
                side: string_arg(invocation, "side")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.conflict.stage",
        "Stage the resolved conflict in the active buffer",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictStage),
    )?;
    register(
        registry,
        "workspace.git.conflict.continue",
        "Finish the paused operation by recording its merge commit",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictContinue),
    )?;
    register(
        registry,
        "workspace.git.conflict.abort",
        "Abandon the paused operation and return to where it started",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictAbort),
    )?;
    register(
        registry,
        "workspace.git.conflict.replace",
        "Preview replacing whole files with one side, then confirm it",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("side", ArgumentType::String, true)
            .field("files", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitConflictReplace {
                side: string_arg(invocation, "side")?.to_owned(),
                files: string_arg(invocation, "files")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.cherry.pick",
        "Cherry-pick a named commit onto the current branch",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("commit", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitPick {
                commit: string_arg(invocation, "commit")?.to_owned(),
                kind: "cherry-pick",
            })
        },
    )?;
    register(
        registry,
        "workspace.git.revert",
        "Revert a named commit on the current branch",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("commit", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitPick {
                commit: string_arg(invocation, "commit")?.to_owned(),
                kind: "revert",
            })
        },
    )?;
    register(
        registry,
        "workspace.git.reset",
        "Reset HEAD to a named revision, in the named mode",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("mode", ArgumentType::String, true)
            .field("target", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitReset {
                mode: string_arg(invocation, "mode")?.to_owned(),
                target: string_arg(invocation, "target")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.handoff",
        "Show why an operation is not native, and the command to run yourself",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("operation", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitHandoff {
                operation: string_arg(invocation, "operation")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "dap.breakpoints.states",
        "Show what is true about each breakpoint rather than what was asked for",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerBreakpointStates),
    )?;
    register(
        registry,
        "dap.breakpoint.disable",
        "Enable or disable the breakpoint on the cursor line",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerBreakpointDisable),
    )?;
    register(
        registry,
        "dap.terminate",
        "End the debug session and ask the debuggee to end with it",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerTerminate),
    )?;
    register(
        registry,
        "dap.detach",
        "Leave the debug session without asking the debuggee to stop",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerDetach),
    )?;
    register(
        registry,
        "session.tutor.start",
        "Open the practice sandbox: a disposable fixture isolated from your work",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::TutorStart),
    )?;
    register(
        registry,
        "session.tutor.lessons",
        "Show the practice chapters: what each teaches and what you have done",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::TutorLessons),
    )?;
    register(
        registry,
        "session.tutor.stop",
        "Leave the practice sandbox and remove its disposable fixture",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::TutorStop),
    )?;
    register(
        registry,
        "overlay.help.contextual",
        "Show what is available on the focused surface",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::HelpContextual),
    )?;
    register(
        registry,
        "overlay.help.form",
        "Show help for the form or confirmation in front of you",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::HelpForForm),
    )?;
    register(
        registry,
        "overlay.help.reference",
        "Show the reference for the selected command",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::HelpCommandReference),
    )?;
    register(
        registry,
        "workspace.git.history.toggle",
        "Toggle Git history between the list and the ancestry graph",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHistoryToggle,
    )?;
    register(
        registry,
        "workspace.git.history.range",
        "Show the combined patch from the selected commit to the tip",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHistoryRange,
    )?;
    register(
        registry,
        "workspace.git.commit.submit",
        "Create the commit from the composed message",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitCommitSubmit,
    )?;
    register(
        registry,
        "workspace.git.hunks.open_staged",
        "Open staged Git hunks",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunksOpenStaged,
    )?;
    register(
        registry,
        "workspace.git.hunk.unstage",
        "Unstage selected staged Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkUnstage,
    )?;
    register(
        registry,
        "workspace.git.file.stage",
        "Stage the whole active file",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitFileStage,
    )?;
    register(
        registry,
        "workspace.git.file.unstage",
        "Unstage the whole active file",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitFileUnstage,
    )?;
    register(
        registry,
        "workspace.git.lines.stage",
        "Stage a line range inside the current Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("start", ArgumentType::Integer, true)
            .field("end", ArgumentType::Integer, true),
        |invocation| {
            let integer = |name: &str| match invocation.arguments.get(name) {
                Some(crate::editor::CommandValue::Integer(value)) => *value,
                _ => -1,
            };
            AppEffect::GitLinesStage {
                start: integer("start"),
                end: integer("end"),
            }
        },
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
        "workspace.git.diff.layer.switch",
        "Switch the diff between the unstaged and staged comparison",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitChangesLayerSwitch,
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
    for (id, title, effect) in [
        (
            "surface.confirmation.select_yes",
            "Select Yes",
            AppEffect::PromptConfirmationSelect(crate::ui::ConfirmationChoice::Yes),
        ),
        (
            "surface.confirmation.select_no",
            "Select No",
            AppEffect::PromptConfirmationSelect(crate::ui::ConfirmationChoice::No),
        ),
        (
            "surface.confirmation.toggle",
            "Toggle confirmation choice",
            AppEffect::PromptConfirmationToggle,
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
    register_grammar_commands(registry)?;
    Ok(())
}
