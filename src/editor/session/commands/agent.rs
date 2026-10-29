use super::{AppEffect, ArgumentSchema, CommandError, CommandRegistry, ContextRequirement, register};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    let commands: [(&str, &str, fn() -> AppEffect); 7] = [
        ("agent.ask", "Ask the agent, with the selection if there is one", || AppEffect::AgentAsk),
        ("agent.toggle", "Show or hide the agent panel", || AppEffect::AgentToggle),
        ("agent.add_file", "Ask the agent about the current file", || AppEffect::AgentAddFile),
        ("agent.add_diagnostics", "Ask the agent about the current file's diagnostics", || {
            AppEffect::AgentAddDiagnostics
        }),
        ("agent.new", "Start a new agent session in the current worktree", || AppEffect::AgentNew),
        ("agent.cancel", "Stop the agent's current turn", || AppEffect::AgentCancel),
        ("agent.pick", "Choose which agent to use", || AppEffect::AgentPick),
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
