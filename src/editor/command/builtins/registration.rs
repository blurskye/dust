use super::*;

pub fn register_builtin_commands<E: 'static>(
    registry: &mut CommandRegistry<E>,
) -> Result<(), CommandError> {
    register(
        registry,
        "view.cursor.move",
        "Move cursor",
        ContextRequirement::View,
        ArgumentSchema::default().field("motion", ArgumentType::String, true),
    )?;
    register(
        registry,
        "mode.insert",
        "Enter Insert mode",
        ContextRequirement::View,
        ArgumentSchema::default().field("placement", ArgumentType::String, true),
    )?;
    for (id, title) in [
        ("mode.normal", "Enter Normal mode"),
        ("mode.replace", "Enter Replace mode"),
        ("mode.visual_char", "Enter character Visual mode"),
        ("mode.visual_line", "Enter line Visual mode"),
        ("mode.visual_block", "Enter block Visual mode"),
        ("edit.delete_char", "Delete character"),
        (
            "edit.delete_backward",
            "Delete previous character in Normal mode",
        ),
        ("edit.substitute", "Substitute characters"),
        ("edit.join_lines", "Join lines"),
        ("edit.toggle_case", "Toggle character case"),
        ("edit.backspace", "Delete previous character"),
        (
            "edit.replace_backspace",
            "Restore previous Replace-mode text",
        ),
        ("history.undo", "Undo"),
        ("history.redo", "Redo"),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::View,
            ArgumentSchema::default(),
        )?;
    }
    register(
        registry,
        "edit.replace",
        "Replace characters",
        ContextRequirement::View,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
    )?;
    register(
        registry,
        "edit.insert_text",
        "Insert text",
        ContextRequirement::View,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
    )?;
    register(
        registry,
        "edit.replace_text",
        "Replace text in Replace mode",
        ContextRequirement::View,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
    )?;
    register(
        registry,
        "edit.operator",
        "Apply operator",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("operator", ArgumentType::String, true)
            .field("motion", ArgumentType::String, true)
            .field("linewise", ArgumentType::Bool, true),
    )?;
    register(
        registry,
        "edit.repeat_visual",
        "Repeat normalized Visual change",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("operator", ArgumentType::String, true)
            .field("shape", ArgumentType::String, true)
            .field("width", ArgumentType::Integer, true)
            .field("height", ArgumentType::Integer, true),
    )?;
    register(
        registry,
        "edit.put",
        "Put register",
        ContextRequirement::View,
        ArgumentSchema::default().field("before", ArgumentType::Bool, true),
    )?;
    Ok(())
}

pub(super) fn register<E: 'static>(
    registry: &mut CommandRegistry<E>,
    id: &str,
    title: &str,
    context: ContextRequirement,
    arguments: ArgumentSchema,
) -> Result<(), CommandError> {
    let mut spec = CommandSpec::builtin(id, title)?;
    spec.context = context;
    spec.arguments = arguments;
    registry.register(spec, execute_builtin::<E>)
}
