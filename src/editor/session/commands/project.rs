use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, CommandValue,
    ContextRequirement, register, string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, effect) in [
        (
            "workspace.trust",
            "Trust workspace",
            AppEffect::WorkspaceTrust,
        ),
        (
            "workspace.untrust",
            "Revoke workspace trust",
            AppEffect::WorkspaceUntrust,
        ),
        (
            "workspace.trust.status",
            "Show workspace trust",
            AppEffect::WorkspaceStatus,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    for (id, title, effect) in [
        (
            "explorer.open",
            "Open file explorer",
            AppEffect::ExplorerOpen,
        ),
        (
            "explorer.close",
            "Close file explorer",
            AppEffect::ExplorerClose,
        ),
        (
            "explorer.panel.toggle",
            "Toggle file explorer",
            AppEffect::ExplorerPanelToggle,
        ),
        (
            "explorer.next",
            "Select next explorer row",
            AppEffect::ExplorerMove(1),
        ),
        (
            "explorer.previous",
            "Select previous explorer row",
            AppEffect::ExplorerMove(-1),
        ),
        (
            "explorer.page_up",
            "Page explorer selection up",
            AppEffect::ExplorerPage(-1),
        ),
        (
            "explorer.page_down",
            "Page explorer selection down",
            AppEffect::ExplorerPage(1),
        ),
        (
            "explorer.parent",
            "Select explorer parent",
            AppEffect::ExplorerParent,
        ),
        (
            "explorer.child",
            "Select first explorer child",
            AppEffect::ExplorerChild,
        ),
        (
            "explorer.toggle",
            "Expand or collapse explorer node",
            AppEffect::ExplorerToggle,
        ),
        (
            "explorer.refresh",
            "Refresh explorer",
            AppEffect::ExplorerRefresh,
        ),
        (
            "explorer.follow",
            "Follow active file in explorer",
            AppEffect::ExplorerFollow,
        ),
        (
            "explorer.hidden.toggle",
            "Toggle hidden explorer entries",
            AppEffect::ExplorerToggleHidden,
        ),
        (
            "explorer.ignored.toggle",
            "Toggle ignored explorer entries",
            AppEffect::ExplorerToggleIgnored,
        ),
        (
            "explorer.file.open",
            "Open explorer file",
            AppEffect::ExplorerFileOpen { split: false },
        ),
        (
            "explorer.file.split_open",
            "Split-open explorer file",
            AppEffect::ExplorerFileOpen { split: true },
        ),
        (
            "explorer.mouse.select",
            "Select explorer row at mouse",
            AppEffect::ExplorerMouseSelect,
        ),
        (
            "explorer.search",
            "Search the explorer",
            AppEffect::ExplorerSearch,
        ),
        (
            "explorer.search.delete",
            "Delete the last character of the explorer search",
            AppEffect::ExplorerSearchDelete,
        ),
        (
            "explorer.search.done",
            "Stop typing and keep the explorer filtered",
            AppEffect::ExplorerSearchStop { keep: true },
        ),
        (
            "explorer.search.cancel",
            "Clear the explorer search",
            AppEffect::ExplorerSearchStop { keep: false },
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "explorer.create",
        "Create explorer file or directory",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("path", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplorerCreate(
                string_arg(invocation, "path")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "explorer.search.insert",
        "Type into the explorer search",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplorerSearchInsert(
                string_arg(invocation, "text")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "explorer.rename.prompt",
        "Rename selected explorer path",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::ExplorerRenamePrompt,
    )?;
    register(
        registry,
        "explorer.rename",
        "Rename selected explorer path",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("target", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplorerRename(
                string_arg(invocation, "target")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "explorer.delete",
        "Delete selected explorer path",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::ExplorerDeleteRequest,
    )?;
    for (id, title, effect) in [
        (
            "explorer.copy",
            "Copy selected explorer path",
            AppEffect::ExplorerClipboard { cut: false },
        ),
        (
            "explorer.move",
            "Move selected explorer path",
            AppEffect::ExplorerClipboard { cut: true },
        ),
        (
            "explorer.paste",
            "Paste copied explorer path",
            AppEffect::ExplorerPaste,
        ),
        (
            "explorer.path.yank",
            "Yank selected explorer path",
            AppEffect::ExplorerYankPath,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "lsp.sessions",
        "List language-server sessions",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::LspSessions,
    )?;
    register(
        registry,
        "lsp.session.select",
        "Select language-server session",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("key", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LspSessionSelect(
                string_arg(invocation, "key")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "session.save",
        "Save session state",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::SessionSave,
    )?;
    register(
        registry,
        "session.load",
        "Load session state",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::SessionLoad,
    )?;
    register(
        registry,
        "history.clear",
        "Clear command and search history",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HistoryClear,
    )?;
    register(
        registry,
        "buffer.large_file.set",
        "Set large-file mode",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("mode", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LargeFileSet(
                string_arg(invocation, "mode")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "buffer.large_file.status",
        "Show large-file mode",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::LargeFileStatus,
    )?;
    register(
        registry,
        "task.run",
        "Run configured task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "task.stop",
        "Stop running task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskStop(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "task.output",
        "Show task output",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskOutput(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "crash.inspect",
        "Inspect previous crash",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CrashInspect,
    )?;
    register(
        registry,
        "crash.clear",
        "Clear previous crash",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CrashClear,
    )?;
    register(
        registry,
        "plugin.validate",
        "Validate configured plugins",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PluginValidate,
    )?;
    register(
        registry,
        "plugin.list",
        "List plugin status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PluginList,
    )?;
    register(
        registry,
        "picker.plugin.open",
        "Open a plugin picker source",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("provider", ArgumentType::String, true)
            .field("query", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::PluginPickerOpen {
                provider: string_arg(invocation, "provider")?.into(),
                query: invocation
                    .arguments
                    .get("query")
                    .map(CommandValue::as_str)
                    .transpose()?
                    .unwrap_or("")
                    .into(),
            })
        },
    )?;
    register(
        registry,
        "buffer.virtual.open",
        "Open a read-only plugin virtual document",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("source", ArgumentType::String, true)
            .field("key", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::VirtualDocumentOpen {
                source: string_arg(invocation, "source")?.into(),
                key: string_arg(invocation, "key")?.into(),
            })
        },
    )?;
    register(
        registry,
        "health.virtual.open",
        "Open read-only health document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HealthVirtualOpen,
    )?;
    register(
        registry,
        "health.virtual.refresh",
        "Refresh read-only health document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HealthVirtualRefresh,
    )?;
    register(
        registry,
        "buffer.virtual.refresh",
        "Refresh focused read-only virtual document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::VirtualDocumentRefresh,
    )?;
    Ok(())
}
