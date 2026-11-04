use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    PathBuf, PickerKind, PromptKind, integer_arg, register, string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "workspace.root.add",
        "Add workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("path", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceRootAdd(PathBuf::from(string_arg(
                invocation, "path",
            )?)))
        },
    )?;
    register(
        registry,
        "workspace.root.activate",
        "Activate workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            let id = u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                CommandError::InvalidArguments("root id must be non-negative".into())
            })?;
            Ok(AppEffect::WorkspaceRootActivate(id))
        },
    )?;
    register(
        registry,
        "workspace.root.remove",
        "Remove workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            let id = u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                CommandError::InvalidArguments("root id must be non-negative".into())
            })?;
            Ok(AppEffect::WorkspaceRootRemove(id))
        },
    )?;
    register(
        registry,
        "workspace.root.status",
        "Show active workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceRootStatus,
    )?;
    register(
        registry,
        "workspace.search.prompt",
        "Open workspace search prompt",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceSearchPrompt,
    )?;
    register(
        registry,
        "workspace.replace.prompt",
        "Open workspace replacement prompt",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenPrompt {
            kind: PromptKind::ReplaceFind,
            initial: String::new(),
        },
    )?;
    register(
        registry,
        "workspace.search",
        "Search workspace",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("query", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceSearch(
                string_arg(invocation, "query")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "workspace.git.pr.list",
        "Pick a pull request and check it out in its own worktree",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::PullRequestList,
    )?;
    register(
        registry,
        "workspace.git.pr.comments",
        "List the review comments of this branch's pull request",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::PullRequestComments,
    )?;
    register(
        registry,
        "workspace.git.pr.comment",
        "Comment on the current line in this branch's pull request",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::PullRequestComment,
    )?;
    register(
        registry,
        "workspace.search.todo",
        "Search TODO, FIXME, HACK and NOTE comments",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TodoSearch,
    )?;
    register(
        registry,
        "workspace.search.cancel",
        "Cancel workspace search",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceSearchCancel,
    )?;
    register(
        registry,
        "workspace.replace",
        "Replace across workspace",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("query", ArgumentType::String, true)
            .field("replacement", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceReplace {
                query: string_arg(invocation, "query")?.into(),
                replacement: string_arg(invocation, "replacement")?.into(),
            })
        },
    )?;
    for (id, title, kind) in [
        (
            "diagnostic.open",
            "Open workspace problems",
            PickerKind::Diagnostics,
        ),
        (
            "buffer.picker.open",
            "Open buffer picker",
            PickerKind::Buffers,
        ),
        (
            "command_palette.open",
            "Open command picker",
            PickerKind::Commands,
        ),
        ("workspace.root.list", "List projects", PickerKind::Roots),
        (
            "navigation.recents",
            "Open recent files",
            PickerKind::Recents,
        ),
        ("navigation.marks", "Open marks", PickerKind::Marks),
        ("navigation.jumps", "Open jump list", PickerKind::Jumps),
        (
            "navigation.changes",
            "Open change list",
            PickerKind::Changes,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::OpenPicker(kind),
        )?;
    }
    for (id, title, effect) in [
        (
            "picker.backspace",
            "Delete picker query character",
            AppEffect::PickerBackspace,
        ),
        (
            "picker.next",
            "Select next picker item",
            AppEffect::PickerMove(1),
        ),
        (
            "picker.previous",
            "Select previous picker item",
            AppEffect::PickerMove(-1),
        ),
        (
            "picker.preview.down",
            "Scroll the picker's diff down",
            AppEffect::PickerPreviewPage(1),
        ),
        (
            "picker.preview.up",
            "Scroll the picker's diff up",
            AppEffect::PickerPreviewPage(-1),
        ),
        (
            "picker.results",
            "Send these results to the shared Results surface",
            AppEffect::PickerResults,
        ),
        (
            "picker.remove",
            "Remove the selected row",
            AppEffect::PickerRemove,
        ),
        (
            "picker.accept",
            "Accept picker item",
            AppEffect::PickerAccept,
        ),
        ("picker.cancel", "Close picker", AppEffect::PickerCancel),
        (
            "picker.mode.normal",
            "Navigate picker results",
            AppEffect::PickerNormal,
        ),
        (
            "picker.mode.insert",
            "Edit picker query",
            AppEffect::PickerInsertMode,
        ),
        (
            "workspace.edit.toggle",
            "Toggle workspace edit file",
            AppEffect::WorkspaceEditToggle,
        ),
        (
            "workspace.edit.accept",
            "Accept workspace edit",
            AppEffect::WorkspaceEditAccept,
        ),
        (
            "workspace.resource.apply",
            "Apply staged resource operations",
            AppEffect::WorkspaceResourceApply,
        ),
        (
            "workspace.edit.reject",
            "Reject workspace edit",
            AppEffect::WorkspaceEditReject,
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
        "picker.insert",
        "Insert picker query text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PickerInsert(
                string_arg(invocation, "text")?.into(),
            ))
        },
    )?;
    Ok(())
}
