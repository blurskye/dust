use crate::app::commands::AppEffect;
use crate::app::commands::ArgumentSchema;
use crate::app::commands::ArgumentType;
use crate::app::commands::CommandError;
use crate::app::commands::CommandRegistry;
use crate::app::commands::ContextRequirement;
use crate::app::commands::PathBuf;
use crate::app::commands::bool_arg;
use crate::app::commands::optional_bool_arg;
use crate::app::commands::optional_string_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;

pub(super) fn register_commands(
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
        "edit.substitute_lines",
        "Substitute in lines",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("range", ArgumentType::String, true)
            .field("pattern", ArgumentType::String, true)
            .field("replacement", ArgumentType::String, true)
            .field("flags", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::Substitute {
                range: string_arg(invocation, "range")?.to_owned(),
                pattern: string_arg(invocation, "pattern")?.to_owned(),
                replacement: string_arg(invocation, "replacement")?.to_owned(),
                flags: string_arg(invocation, "flags")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "edit.global_delete",
        "Delete matching lines",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("range", ArgumentType::String, true)
            .field("pattern", ArgumentType::String, true)
            .field("invert", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::GlobalDelete {
                range: string_arg(invocation, "range")?.to_owned(),
                pattern: string_arg(invocation, "pattern")?.to_owned(),
                invert: bool_arg(invocation, "invert")?,
            })
        },
    )?;
    register(
        registry,
        "file.save",
        "Save file",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("path", ArgumentType::String, false)
            .field("force", ArgumentType::Bool, false),
        |invocation| AppEffect::Save {
            quit_after: false,
            path: optional_string_arg(invocation, "path").map(PathBuf::from),
            force: optional_bool_arg(invocation, "force").unwrap_or(false),
        },
    )?;
    register(
        registry,
        "file.save_quit",
        "Save and quit",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| AppEffect::Save {
            quit_after: true,
            path: None,
            force: optional_bool_arg(invocation, "force").unwrap_or(false),
        },
    )?;
    for (id, title, quit_after) in [
        ("file.save_all", "Save all files", false),
        ("file.save_all_quit", "Save all files and quit", true),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::SaveAll { quit_after },
        )?;
    }
    register(
        registry,
        "file.markdown_preview",
        "Preview Markdown",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::MarkdownPreview,
    )?;
    register(
        registry,
        "file.open",
        "Open file by path",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("path", ArgumentType::String, true)
            .field("force", ArgumentType::Bool, false),
        |invocation| {
            let path = PathBuf::from(string_arg(invocation, "path")?);
            Ok(if optional_bool_arg(invocation, "force") == Some(true) {
                AppEffect::ForceOpenPath(path)
            } else {
                AppEffect::OpenPath(path)
            })
        },
    )?;
    register(
        registry,
        "file.reload",
        "Reload file",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::Reload {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    register(
        registry,
        "buffer.close",
        "Close buffer",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::CloseBuffer {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    for (id, title, offset) in [
        ("buffer.next", "Show next buffer", 1),
        ("buffer.previous", "Show previous buffer", -1),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |invocation| {
                AppEffect::CycleBuffer(
                    offset
                        * isize::try_from(invocation.count.map_or(1, |count| count.get()))
                            .unwrap_or(isize::MAX),
                )
            },
        )?;
    }
    register(
        registry,
        "buffer.pin.toggle",
        "Toggle active buffer pin",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::ToggleBufferPin,
    )?;
    for (id, title, direction) in [
        ("buffer.move_left", "Move buffer left", -1),
        ("buffer.move_right", "Move buffer right", 1),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Buffer,
            ArgumentSchema::default(),
            move |_| AppEffect::MoveBufferLine(direction),
        )?;
    }
    register(
        registry,
        "file.new",
        "New buffer",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::NewBuffer,
    )?;
    register(
        registry,
        "buffer.new_tab",
        "New tab: an empty buffer with the file finder over it",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::NewTab,
    )?;
    Ok(())
}
