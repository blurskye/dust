use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    PromptKind, SyntaxFoldAction, register, string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "format.run",
        "Run configured formatter",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::FormatRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "lint.run",
        "Run configured linter",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LintRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "lint.clear",
        "Clear linter diagnostics",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LintClear(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    for (id, description, initial) in [
        ("test.run.prompt", "Run test by name", "test.run name="),
        (
            "test.list.prompt",
            "Show test results by name",
            "test.list name=",
        ),
        ("test.stop.prompt", "Stop test by name", "test.stop name="),
    ] {
        register(
            registry,
            id,
            description,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| AppEffect::OpenPrompt {
                kind: PromptKind::Command,
                initial: initial.to_owned(),
            },
        )?;
    }
    register(
        registry,
        "test.run",
        "Run configured test",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "test.stop",
        "Stop test run",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestStop(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "test.list",
        "List test results",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestList(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "syntax.status",
        "Show built-in syntax language status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxStatus,
    )?;
    for (id, name, action) in [
        (
            "syntax.fold.toggle",
            "Toggle fold",
            SyntaxFoldAction::Toggle,
        ),
        ("syntax.fold.open", "Open fold", SyntaxFoldAction::Open),
        ("syntax.fold.close", "Close fold", SyntaxFoldAction::Close),
        (
            "syntax.fold.open_all",
            "Open all folds",
            SyntaxFoldAction::OpenAll,
        ),
        (
            "syntax.fold.close_all",
            "Close all folds",
            SyntaxFoldAction::CloseAll,
        ),
    ] {
        register(
            registry,
            id,
            name,
            ContextRequirement::View,
            ArgumentSchema::default(),
            move |_| AppEffect::SyntaxFold(action),
        )?;
    }
    register(
        registry,
        "syntax.indent.line",
        "Indent current line from built-in syntax structure",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxIndentLine,
    )?;
    register(
        registry,
        "task.list",
        "List task status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TaskList,
    )?;
    register(
        registry,
        "task.restart",
        "Restart task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskRestart(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{CommandInvocation, EditorState, InvocationSource};

    #[test]
    fn test_prompts_use_stable_command_ids() {
        let root = tempfile::tempdir().unwrap();
        let state = EditorState::new(root.path().into());
        let mut registry = CommandRegistry::default();
        register_commands(&mut registry).unwrap();
        for command in ["test.run", "test.list", "test.stop"] {
            let invocation = CommandInvocation::new(
                &format!("{command}.prompt"), InvocationSource::Test,
            ).unwrap();
            let outcome = registry.execute(&state, &invocation).unwrap();
            assert!(matches!(outcome.effects.as_slice(), [AppEffect::OpenPrompt { kind: PromptKind::Command, initial }] if initial == &format!("{command} name=")));
        }
    }
}
