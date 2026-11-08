//! Built-in editing command declarations.

use crate::editor::command::ArgumentSchema;
use crate::editor::command::ArgumentType;
use crate::editor::command::CommandError;
use crate::editor::command::CommandRegistry;
use crate::editor::command::CommandSpec;
use crate::editor::command::ContextRequirement;
use crate::editor::command::builtins::execute::execute_builtin;

pub fn register_builtin_commands<E: 'static>(
    registry: &mut CommandRegistry<E>,
) -> Result<(), CommandError> {
    register(
        registry,
        "view.cursor.move",
        "Move cursor",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("motion", ArgumentType::String, true)
            .field("character", ArgumentType::String, false),
    )?;
    register(
        registry,
        "mode.insert",
        "Enter Insert mode",
        ContextRequirement::View,
        ArgumentSchema::default().field("placement", ArgumentType::String, true),
    )?;
    register(
        registry,
        "multi.insert",
        "Type at every cursor",
        ContextRequirement::View,
        ArgumentSchema::default().field("placement", ArgumentType::String, true),
    )?;
    register(
        registry,
        "multi.move",
        "Move every cursor",
        ContextRequirement::View,
        ArgumentSchema::default().field("motion", ArgumentType::String, true),
    )?;
    for (id, title) in [
        ("mode.normal", "Enter Normal mode"),
        ("multi.add_next", "Add a cursor at the next match"),
        ("multi.add_below", "Add a cursor below"),
        ("multi.add_above", "Add a cursor above"),
        ("multi.delete", "Delete at every cursor"),
        ("multi.add_all", "Add a cursor at every match"),
        ("multi.next", "Go to the next cursor"),
        ("multi.previous", "Go to the previous cursor"),
        ("multi.skip", "Skip this match and add the next"),
        ("multi.remove", "Remove this cursor"),
        ("multi.delete_char", "Delete the character at every cursor"),
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
        (
            "view.selection.swap",
            "Go to the other end of the selection",
        ),
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
        "edit.apply",
        "Apply a checked edit and cursor",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("revision", ArgumentType::Integer, true)
            .field("start", ArgumentType::Integer, true)
            .field("end", ArgumentType::Integer, true)
            .field("text", ArgumentType::String, true)
            .field("cursor", ArgumentType::Integer, true),
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
            .field("linewise", ArgumentType::Bool, true)
            .field("character", ArgumentType::String, false)
            .field("find_character", ArgumentType::String, false)
            .field("comment", ArgumentType::String, false)
            .field("revision", ArgumentType::Integer, false)
            .field("start", ArgumentType::Integer, false)
            .field("end", ArgumentType::Integer, false),
    )?;
    register(
        registry,
        "edit.surround",
        "Delete or replace surrounding pair",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("action", ArgumentType::String, true)
            .field("character", ArgumentType::String, true)
            .field("replacement", ArgumentType::String, false),
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
            .field("height", ArgumentType::Integer, true)
            .field("character", ArgumentType::String, false)
            .field("comment", ArgumentType::String, false),
    )?;
    register(
        registry,
        "edit.put",
        "Put register",
        ContextRequirement::View,
        ArgumentSchema::default().field("before", ArgumentType::Bool, true),
    )?;
    for id in [
        "syntax.selection.grow",
        "syntax.selection.shrink",
        "syntax.function.next",
        "syntax.function.previous",
        "syntax.class.next",
        "syntax.class.previous",
        "syntax.argument.swap_next",
        "syntax.argument.swap_previous",
        "syntax.select.inner_function",
        "syntax.select.a_function",
        "syntax.select.inner_class",
        "syntax.select.a_class",
        "syntax.select.inner_argument",
        "syntax.select.a_argument",
        "syntax.select.inner_comment",
        "syntax.select.a_comment",
    ] {
        // User-facing labels are supplied by the bundled configuration.
        register(
            registry,
            id,
            id,
            ContextRequirement::View,
            ArgumentSchema::default(),
        )?;
    }
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
    spec.access = std::collections::BTreeSet::from([crate::editor::command::CommandAccess::Edit]);
    registry.register(spec, execute_builtin::<E>)
}
