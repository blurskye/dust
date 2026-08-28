use super::*;

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
    register(
        registry,
        "syntax.fold.toggle",
        "Toggle built-in syntax fold",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxFoldToggle,
    )?;
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
