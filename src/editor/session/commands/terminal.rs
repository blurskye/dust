use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    integer_arg, optional_bool_arg, optional_integer_arg, optional_string_arg, register, string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "terminal.open",
        "Open terminal profile",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("profile", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::TerminalOpen(
                optional_string_arg(invocation, "profile").map(str::to_owned),
            ))
        },
    )?;
    register(
        registry,
        "terminal.toggle",
        "Open or toggle focus for the bottom terminal",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalToggle,
    )?;
    register(
        registry,
        "terminal.list",
        "List terminals",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalList,
    )?;
    register(
        registry,
        "terminal.focus",
        "Focus terminal",
        ContextRequirement::Application,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::TerminalFocus(
                u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                    CommandError::InvalidArguments("terminal id must be non-negative".into())
                })?,
            ))
        },
    )?;
    register(
        registry,
        "terminal.focus_active",
        "Focus the visible terminal panel",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalFocusActive,
    )?;
    register(
        registry,
        "terminal.cycle",
        "Cycle the selected terminal",
        ContextRequirement::Application,
        ArgumentSchema::default().field("direction", ArgumentType::Integer, true),
        |invocation| {
            let direction = i8::try_from(integer_arg(invocation, "direction")?).map_err(|_| {
                CommandError::InvalidArguments("terminal direction is outside i8 bounds".into())
            })?;
            Ok(AppEffect::TerminalCycle(direction))
        },
    )?;
    register(
        registry,
        "terminal.resize",
        "Resize the bottom terminal panel by rows",
        ContextRequirement::Application,
        ArgumentSchema::default().field("rows", ArgumentType::Integer, true),
        |invocation| {
            let rows = i16::try_from(integer_arg(invocation, "rows")?).map_err(|_| {
                CommandError::InvalidArguments("terminal resize is outside i16 bounds".into())
            })?;
            Ok(AppEffect::TerminalResize(rows))
        },
    )?;
    register(
        registry,
        "terminal.dock",
        "Dock the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("edge", ArgumentType::String, false)
            .field("size", ArgumentType::Integer, false),
        |invocation| {
            let size = u16::try_from(optional_integer_arg(invocation, "size").unwrap_or(12))
                .map_err(|_| {
                    CommandError::InvalidArguments(
                        "terminal dock size is outside u16 bounds".into(),
                    )
                })?;
            Ok(AppEffect::TerminalDock {
                edge: optional_string_arg(invocation, "edge").unwrap_or("bottom").to_owned(),
                size,
            })
        },
    )?;
    register(
        registry,
        "terminal.float",
        "Float the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("x", ArgumentType::Integer, false)
            .field("y", ArgumentType::Integer, false)
            .field("width", ArgumentType::Integer, false)
            .field("height", ArgumentType::Integer, false),
        |invocation| {
            if optional_integer_arg(invocation, "width").is_none() {
                return Ok(AppEffect::TerminalFloat(None));
            }
            let bounded = |name| {
                u16::try_from(integer_arg(invocation, name)?).map_err(|_| {
                    CommandError::InvalidArguments(format!(
                        "terminal float {name} is outside u16 bounds"
                    ))
                })
            };
            Ok(AppEffect::TerminalFloat(Some(crate::ui::window::WindowRect {
                x: bounded("x")?,
                y: bounded("y")?,
                width: bounded("width")?,
                height: bounded("height")?,
            })))
        },
    )?;
    register(
        registry,
        "terminal.fullscreen",
        "Fullscreen the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalFullscreen,
    )?;
    register(
        registry,
        "terminal.hide",
        "Hide the selected terminal window without closing its process",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalHide,
    )?;
    register(
        registry,
        "terminal.send",
        "Send terminal text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TerminalSend(
                string_arg(invocation, "text")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "terminal.mode.normal",
        "Enter terminal normal mode",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalModeNormal,
    )?;
    register(
        registry,
        "terminal.mode.input",
        "Return to terminal input mode",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalModeInput,
    )?;
    register(
        registry,
        "terminal.normal.move",
        "Move the terminal normal-mode cursor",
        ContextRequirement::Application,
        ArgumentSchema::default().field("motion", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TerminalNormalMove(
                string_arg(invocation, "motion")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "terminal.visual.start",
        "Start a terminal visual selection",
        ContextRequirement::Application,
        ArgumentSchema::default().field("linewise", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::TerminalVisualStart(
                optional_bool_arg(invocation, "linewise").unwrap_or(false),
            ))
        },
    )?;
    register(
        registry,
        "terminal.yank",
        "Yank terminal text into Dust registers",
        ContextRequirement::Application,
        ArgumentSchema::default().field("line", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::TerminalYank {
                line: optional_bool_arg(invocation, "line").unwrap_or(false),
            })
        },
    )?;
    register(
        registry,
        "terminal.unfocus",
        "Return focus from terminal to editor",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalUnfocus,
    )?;
    register(
        registry,
        "terminal.close",
        "Close focused terminal",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalClose,
    )?;
    register(
        registry,
        "terminal.quit",
        "Stop the focused terminal without asking",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalQuit,
    )?;
    Ok(())
}
