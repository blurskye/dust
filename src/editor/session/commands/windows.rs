use super::*;

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "window.dock",
        "Dock an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("edge", ArgumentType::String, true)
            .field("size", ArgumentType::Integer, true),
        |invocation| {
            let size = u16::try_from(integer_arg(invocation, "size")?).map_err(|_| {
                CommandError::InvalidArguments("window dock size is outside u16 bounds".into())
            })?;
            Ok(AppEffect::WindowDock {
                target: string_arg(invocation, "target")?.to_owned(),
                edge: string_arg(invocation, "edge")?.to_owned(),
                size,
            })
        },
    )?;
    register(
        registry,
        "window.float",
        "Float an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("x", ArgumentType::Integer, true)
            .field("y", ArgumentType::Integer, true)
            .field("width", ArgumentType::Integer, true)
            .field("height", ArgumentType::Integer, true),
        |invocation| {
            let bounded = |name| {
                u16::try_from(integer_arg(invocation, name)?).map_err(|_| {
                    CommandError::InvalidArguments(format!(
                        "window float {name} is outside u16 bounds"
                    ))
                })
            };
            Ok(AppEffect::WindowFloat {
                target: string_arg(invocation, "target")?.to_owned(),
                x: bounded("x")?,
                y: bounded("y")?,
                width: bounded("width")?,
                height: bounded("height")?,
            })
        },
    )?;
    for (id, title, build) in [
        (
            "window.fullscreen",
            "Fullscreen an ordinary native window",
            AppEffect::WindowFullscreen as fn(String) -> AppEffect,
        ),
        (
            "window.hide",
            "Hide an ordinary native window",
            AppEffect::WindowHide,
        ),
        (
            "window.show",
            "Show an ordinary native window",
            AppEffect::WindowShow,
        ),
        (
            "window.close",
            "Close an ordinary native window",
            AppEffect::WindowClose,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default().field("target", ArgumentType::String, true),
            move |invocation| Ok(build(string_arg(invocation, "target")?.to_owned())),
        )?;
    }
    register(
        registry,
        "window.resize",
        "Resize an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("amount", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::WindowResize {
                target: string_arg(invocation, "target")?.to_owned(),
                amount: i16::try_from(integer_arg(invocation, "amount")?).map_err(|_| {
                    CommandError::InvalidArguments("window resize is outside i16 bounds".into())
                })?,
            })
        },
    )?;
    register(
        registry,
        "window.resize.directional",
        "Resize the focused window in one direction",
        ContextRequirement::Application,
        ArgumentSchema::default().field("direction", ArgumentType::String, true),
        |invocation| {
            let direction = match string_arg(invocation, "direction")? {
                "left" => FocusDirection::Left,
                "down" => FocusDirection::Down,
                "up" => FocusDirection::Up,
                "right" => FocusDirection::Right,
                _ => {
                    return Err(CommandError::InvalidArguments(
                        "window resize direction must be left, down, up, or right".into(),
                    ));
                }
            };
            Ok(AppEffect::WindowResizeDirectional(direction))
        },
    )?;
    register(
        registry,
        "window.move",
        "Move an ordinary floating native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("x", ArgumentType::Integer, true)
            .field("y", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::WindowMove {
                target: string_arg(invocation, "target")?.to_owned(),
                x: u16::try_from(integer_arg(invocation, "x")?).map_err(|_| {
                    CommandError::InvalidArguments("window x is outside u16 bounds".into())
                })?,
                y: u16::try_from(integer_arg(invocation, "y")?).map_err(|_| {
                    CommandError::InvalidArguments("window y is outside u16 bounds".into())
                })?,
            })
        },
    )?;
    register(
        registry,
        "window.swap",
        "Swap two ordinary native window placements",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("first", ArgumentType::String, true)
            .field("second", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WindowSwap {
                first: string_arg(invocation, "first")?.to_owned(),
                second: string_arg(invocation, "second")?.to_owned(),
            })
        },
    )?;
    Ok(())
}
