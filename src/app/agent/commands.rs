//! The agent's commands: their names and arguments, and what each one runs.
//!
//! A new agent command is a variant here, its registration in `register_commands`, its
//! arm in `run_agent_command`, a label in `assets/defaults/config.toml` and, if it has a
//! default key, a binding in `assets/defaults/keymaps.toml`.

use crate::app::commands::AppEffect;

use crate::app::commands::integer_arg;

use crate::app::commands::register;

use crate::app::App;
use crate::app::agent::THINKING;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

#[derive(Clone, Debug)]
pub enum AgentCommand {
    Ask,
    Toggle,
    AddFile,
    AddDiagnostics,
    New,
    Sessions,
    NextSession,
    PreviousSession,
    CloseSession,
    Resume,
    Cancel,
    Model,
    Mode,
    Thinking,
    Options,
    Input(String),
    Edit(String),
    Scroll(i64),
    Escape,
    NewTab,
}

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    use AgentCommand::*;
    let commands = [
        (
            "agent.ask",
            "Ask the agent, with the selection if there is one",
            Ask,
        ),
        ("agent.toggle", "Show or hide the agent panel", Toggle),
        (
            "agent.add_file",
            "Ask the agent about the current file",
            AddFile,
        ),
        (
            "agent.add_diagnostics",
            "Ask the agent about the current file's diagnostics",
            AddDiagnostics,
        ),
        ("agent.new", "Pick an agent and start it in a new tab", New),
        ("agent.sessions", "List the open agent sessions", Sessions),
        (
            "agent.session.next",
            "Show the next agent session",
            NextSession,
        ),
        (
            "agent.session.previous",
            "Show the previous agent session",
            PreviousSession,
        ),
        (
            "agent.session.close",
            "Stop the shown agent session and close its tab",
            CloseSession,
        ),
        (
            "agent.resume",
            "Resume a saved agent conversation in a tab",
            Resume,
        ),
        ("agent.cancel", "Stop the agent's current turn", Cancel),
        ("agent.model", "Choose the agent's model", Model),
        ("agent.mode", "Choose the agent's mode", Mode),
        (
            "agent.thinking",
            "Choose how hard the agent thinks",
            Thinking,
        ),
        (
            "agent.options",
            "Change any option the agent offers",
            Options,
        ),
        (
            "agent.escape",
            "Stop the agent's turn, or go back to the code",
            Escape,
        ),
        (
            "agent.tab.new",
            "Start another session of the shown agent in a new tab",
            NewTab,
        ),
    ];
    for (id, description, command) in commands {
        register(
            registry,
            id,
            description,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| AppEffect::Agent(command.clone()),
        )?;
    }
    register(
        registry,
        "agent.input.insert",
        "Type text into the agent panel's input",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            let text = string_arg(invocation, "text")?.to_owned();
            Ok(AppEffect::Agent(Input(text)))
        },
    )?;
    register(
        registry,
        "agent.input.edit",
        "Edit the agent panel's input",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("action", ArgumentType::String, true),
        |invocation| {
            let action = string_arg(invocation, "action")?.to_owned();
            Ok(AppEffect::Agent(Edit(action)))
        },
    )?;
    register(
        registry,
        "agent.scroll",
        "Scroll the agent transcript up (positive) or down",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("rows", ArgumentType::Integer, true),
        |invocation| Ok(AppEffect::Agent(Scroll(integer_arg(invocation, "rows")?))),
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn run_agent_command(&mut self, command: AgentCommand) {
        match command {
            AgentCommand::Ask => self.begin_agent_question(),
            AgentCommand::Toggle => self.toggle_agent_panel(),
            AgentCommand::AddFile => self.attach_agent_file(),
            AgentCommand::AddDiagnostics => self.attach_agent_diagnostics(),
            AgentCommand::New => self.open_agent_picker(),
            AgentCommand::Sessions => self.open_agent_sessions_picker(),
            AgentCommand::NextSession => self.cycle_agent_session(1),
            AgentCommand::PreviousSession => self.cycle_agent_session(-1),
            AgentCommand::CloseSession => self.close_agent_session(),
            AgentCommand::Resume => self.open_agent_resume_picker(),
            AgentCommand::Cancel => self.cancel_agent_turn(),
            AgentCommand::Model => self.open_agent_option_picker(Some("model")),
            AgentCommand::Mode => self.open_agent_option_picker(Some("mode")),
            AgentCommand::Thinking => self.open_agent_option_picker(Some(THINKING)),
            AgentCommand::Options => self.open_agent_option_picker(None),
            AgentCommand::Input(text) => self.agent_input_insert(&text),
            AgentCommand::Edit(action) => self.agent_input_edit(&action),
            AgentCommand::Scroll(rows) => self.scroll_agent(rows),
            AgentCommand::Escape => self.agent_escape(),
            AgentCommand::NewTab => self.new_agent_tab(),
        }
    }
}

impl App {
    pub(in crate::app) fn process_agent_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Agent(command) => self.run_agent_command(command),
            effect => return Err(effect),
        }
        Ok(())
    }
}
