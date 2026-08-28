use super::*;

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "dap.watch.add",
        "Add debug watch",
        ContextRequirement::Application,
        ArgumentSchema::default().field("expression", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapWatchAdd(
                string_arg(invocation, "expression")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.watch.remove",
        "Remove debug watch",
        ContextRequirement::Application,
        ArgumentSchema::default().field("expression", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapWatchRemove(
                string_arg(invocation, "expression")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.watch.list",
        "List debug watches",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::DapWatchList,
    )?;
    register(
        registry,
        "dap.configuration.start",
        "Start configured debug session",
        ContextRequirement::Application,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapConfigurationStart(
                string_arg(invocation, "name")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.threads",
        "Show debug threads",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::DapThreads,
    )?;
    register(
        registry,
        "dap.stack",
        "Show debug stack",
        ContextRequirement::Application,
        ArgumentSchema::default().field("thread", ArgumentType::Integer, true),
        |invocation| Ok(AppEffect::DapStack(integer_arg(invocation, "thread")?)),
    )?;
    register(
        registry,
        "dap.scopes",
        "Show debug scopes",
        ContextRequirement::Application,
        ArgumentSchema::default().field("frame", ArgumentType::Integer, true),
        |invocation| Ok(AppEffect::DapScopes(integer_arg(invocation, "frame")?)),
    )?;
    register(
        registry,
        "dap.variables",
        "Show debug variables",
        ContextRequirement::Application,
        ArgumentSchema::default().field("reference", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::DapVariables(integer_arg(
                invocation,
                "reference",
            )?))
        },
    )?;
    register(
        registry,
        "dap.evaluate",
        "Evaluate debug expression",
        ContextRequirement::Application,
        ArgumentSchema::default().field("expression", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapEvaluate(
                string_arg(invocation, "expression")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.breakpoint.configure",
        "Configure source breakpoint",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("condition", ArgumentType::String, false)
            .field("hit_condition", ArgumentType::String, false)
            .field("log_message", ArgumentType::String, false),
        |invocation| AppEffect::DapBreakpointConfigure {
            condition: optional_string_arg(invocation, "condition").map(str::to_owned),
            hit_condition: optional_string_arg(invocation, "hit_condition").map(str::to_owned),
            log_message: optional_string_arg(invocation, "log_message").map(str::to_owned),
        },
    )?;
    register(
        registry,
        "dap.session.restart",
        "Restart configured debug session",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::DapRestart,
    )?;
    register(
        registry,
        "dap.breakpoint.toggle",
        "Toggle source breakpoint",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::DapBreakpointToggle,
    )?;
    register(
        registry,
        "dap.session.start",
        "Start debug session",
        ContextRequirement::Application,
        ArgumentSchema::default().field("program", ArgumentType::String, false),
        |invocation| {
            AppEffect::DapStart(optional_string_arg(invocation, "program").map(PathBuf::from))
        },
    )?;
    Ok(())
}
