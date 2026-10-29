use super::{AppEffect, ArgumentSchema, CommandError, CommandRegistry, ContextRequirement, register};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    let commands: [(&str, &str, fn() -> AppEffect); 14] = [
        ("agent.ask", "Ask the agent, with the selection if there is one", || AppEffect::AgentAsk),
        ("agent.toggle", "Show or hide the agent panel", || AppEffect::AgentToggle),
        ("agent.add_file", "Ask the agent about the current file", || AppEffect::AgentAddFile),
        ("agent.add_diagnostics", "Ask the agent about the current file's diagnostics", || {
            AppEffect::AgentAddDiagnostics
        }),
        ("agent.new", "Pick an agent and start it in a new tab", || AppEffect::AgentNew),
        ("agent.sessions", "List the open agent sessions", || AppEffect::AgentSessions),
        ("agent.session.next", "Show the next agent session", || AppEffect::AgentNextSession),
        ("agent.session.previous", "Show the previous agent session", || {
            AppEffect::AgentPreviousSession
        }),
        ("agent.session.close", "Stop the shown agent session and close its tab", || {
            AppEffect::AgentCloseSession
        }),
        ("agent.cancel", "Stop the agent's current turn", || AppEffect::AgentCancel),
        ("agent.model", "Choose the agent's model", || AppEffect::AgentModel),
        ("agent.mode", "Choose the agent's mode", || AppEffect::AgentMode),
        ("agent.thinking", "Choose how hard the agent thinks", || AppEffect::AgentThinking),
        ("agent.options", "Change any option the agent offers", || AppEffect::AgentOptions),
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
    Ok(())
}
