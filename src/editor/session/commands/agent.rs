use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    integer_arg, register, string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    #[allow(clippy::type_complexity)]
    let commands: [(&str, &str, fn() -> AppEffect); 15] = [
        (
            "agent.ask",
            "Ask the agent, with the selection if there is one",
            || AppEffect::AgentAsk,
        ),
        ("agent.toggle", "Show or hide the agent panel", || {
            AppEffect::AgentToggle
        }),
        (
            "agent.add_file",
            "Ask the agent about the current file",
            || AppEffect::AgentAddFile,
        ),
        (
            "agent.add_diagnostics",
            "Ask the agent about the current file's diagnostics",
            || AppEffect::AgentAddDiagnostics,
        ),
        (
            "agent.new",
            "Pick an agent and start it in a new tab",
            || AppEffect::AgentNew,
        ),
        ("agent.sessions", "List the open agent sessions", || {
            AppEffect::AgentSessions
        }),
        ("agent.session.next", "Show the next agent session", || {
            AppEffect::AgentNextSession
        }),
        (
            "agent.session.previous",
            "Show the previous agent session",
            || AppEffect::AgentPreviousSession,
        ),
        (
            "agent.session.close",
            "Stop the shown agent session and close its tab",
            || AppEffect::AgentCloseSession,
        ),
        (
            "agent.resume",
            "Resume a saved agent conversation in a tab",
            || AppEffect::AgentResume,
        ),
        ("agent.cancel", "Stop the agent's current turn", || {
            AppEffect::AgentCancel
        }),
        ("agent.model", "Choose the agent's model", || {
            AppEffect::AgentModel
        }),
        ("agent.mode", "Choose the agent's mode", || {
            AppEffect::AgentMode
        }),
        ("agent.thinking", "Choose how hard the agent thinks", || {
            AppEffect::AgentThinking
        }),
        (
            "agent.options",
            "Change any option the agent offers",
            || AppEffect::AgentOptions,
        ),
    ];
    for (id, description, effect) in commands {
        register(
            registry,
            id,
            description,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect(),
        )?;
    }
    register(
        registry,
        "agent.input.insert",
        "Type text into the agent panel's input",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::AgentInput(
                string_arg(invocation, "text")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "agent.input.edit",
        "Edit the agent panel's input",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("action", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::AgentEdit(
                string_arg(invocation, "action")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "agent.scroll",
        "Scroll the agent transcript up (positive) or down",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("rows", ArgumentType::Integer, true),
        |invocation| Ok(AppEffect::AgentScroll(integer_arg(invocation, "rows")?)),
    )?;
    register(
        registry,
        "agent.escape",
        "Stop the agent's turn, or go back to the code",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::AgentEscape,
    )?;
    register(
        registry,
        "agent.tab.new",
        "Start another session of the shown agent in a new tab",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::AgentNewTab,
    )?;
    Ok(())
}
