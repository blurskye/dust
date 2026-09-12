use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    PathBuf, bool_arg, optional_bool_arg, optional_string_arg, register, string_arg,
};

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
    register(
        registry,
        "file.open",
        "Open file",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("path", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::OpenPath(PathBuf::from(string_arg(
                invocation, "path",
            )?)))
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
            move |_| AppEffect::CycleBuffer(offset),
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
    Ok(())
}
