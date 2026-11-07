//! The agent panel: ACP sessions as tabs. Each session has its own agent process, transcript,
//! options (model, mode, thinking level) and permission requests. The panel is a split beside
//! the code with its own tab line, the active session's conversation with its Markdown
//! rendered, an input box and a footer; `ui::render::agent` draws it in place of the pane's
//! placeholder buffer. Questions go to the active session. A session keeps the worktree it
//! started in; switching worktrees does not move it.
//!
//! Each top-level session gets Dust's MCP server (`agent::mcp`), whose `agent` tool starts a
//! subagent: a session in its own tab that works on one task and whose final answer goes
//! back to the agent that asked. Agents' own subagent tools are turned off in the default
//! profiles, so every agent gets the same subagents.

use crate::ui::render::overlays::AGENT_ICON;

use crate::ui::render::overlays::SPINNER_FRAMES;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;

use serde_json::Value;

use serde_json::json;
use unicode_segmentation::UnicodeSegmentation;

use crate::agent::AgentMessage;
use crate::agent::AgentProcess;
use crate::agent::AuthMethod;
use crate::agent::PermissionOption;
use crate::agent::PromptPart;
use crate::agent::Rejoin;
use crate::agent::SessionOption;
use crate::agent::SessionUpdate;
use crate::agent::history;
use crate::agent::history::Conversation;
use crate::agent::history::Entry;
use crate::app::App;
use crate::app::commands::PromptMove;
use crate::app::picker::PickerPayload;
use crate::app::windows::LayoutAction;
use crate::config::Highlight;
use crate::editor::BufferId;
use crate::editor::EditorMode;
use crate::editor::text::TextBuffer;
use crate::remote::AgentRequest;
use crate::remote::Reply;
use crate::tools::ToolKind;
use crate::tools::ToolStatus;
use crate::ui::AgentListRow;
use crate::ui::AgentPanelUi;
use crate::ui::ConfirmationChoice;
use crate::ui::ContentTabEntry;
use crate::ui::ContentTabIdentity;
use crate::ui::ContentTabKind;
use crate::ui::PromptKind;
use crate::windows::ContentLifecycle;
use crate::windows::TileAxis;

use crate::ui::markdown::Styled;
use crate::windows::ContentId;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::workspace::PickerItemKind;

/// The owner of the placeholder buffer the panel's pane holds.
pub(in crate::app) const TRANSCRIPT_OWNER: &str = "Agent";
/// Messages handled per session and tick, so a chatty agent cannot stall input.
const MESSAGES_PER_TICK: usize = 256;
/// How long a question typed while the agent started waits once its session exists, so the
/// text some agents send just after, such as pi's list of its context and extensions, comes
/// before the question rather than inside the answer.
const STARTUP_TEXT_WAIT: std::time::Duration = std::time::Duration::from_millis(200);
/// Files larger than this are not read for the agent.
const READ_BYTE_LIMIT: u64 = 16 * 1024 * 1024;
/// The option category ACP uses for reasoning effort.
const THINKING: &str = "thought_level";
/// How much of a saved conversation goes to an agent that starts fresh, in characters.
const RECAP_CHARACTERS: usize = 60_000;
/// How long each spinner frame shows, in milliseconds.
const SPINNER_FRAME_MILLIS: u128 = 80;
/// Slash commands the panel runs itself; the agent's own commands follow them in the menu.
const PANEL_COMMANDS: [(&str, &str); 7] = [
    ("new", "Start an agent in a new tab"),
    ("resume", "Open a saved conversation"),
    ("model", "Pick the model"),
    ("mode", "Pick the mode"),
    ("thinking", "Pick the thinking level"),
    ("options", "All of the session's options"),
    ("close", "Stop this session and close its tab"),
];

/// What a row of one of the agent pickers stands for.
#[derive(Clone, Debug)]
pub(in crate::app) enum AgentChoice {
    /// Start a session of this profile in a new tab.
    Profile(String),
    /// Show the session with this tab number.
    Session(u64),
    /// List the values of this option of the active session.
    Option(String),
    /// Set an option of the active session.
    Value { option: String, value: String },
    /// Sign in to the active session's agent with this method.
    Login(String),
    /// Open a saved conversation in a tab.
    Resume(String),
}

#[derive(Default)]
pub(super) struct AgentPanel {
    directory: PathBuf,
    files: HashMap<crate::files::RequestId, AgentFileRequest>,
    sessions: Vec<AgentSession>,
    /// Index of the session the panel shows and questions go to.
    active: usize,
    /// Tab numbers are never reused, so a late permission answer cannot reach the wrong tab.
    started: u64,
    /// Agents whose `add_tools` command ran in this run of Dust.
    tools_added: HashSet<String>,
    /// What went wrong running those commands, shown by the next event pass.
    tool_failures: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    buffer: Option<BufferId>,
    /// Files, selections and diagnostics to send with the next question.
    context: Vec<PromptPart>,
    /// Permission requests waiting for the prompt line to be free.
    permissions: VecDeque<PermissionRequest>,
    /// The request the open permission prompt answers.
    asking: Option<PermissionRequest>,
    /// The question being typed, and the cursor in it as a byte offset.
    input: String,
    cursor: usize,
    /// Transcript rows scrolled up from the end, and how many rows there are.
    scroll: usize,
    lines: usize,
    /// The width the transcript was wrapped to.
    width: usize,
    /// Whether the input is in Vim's Normal mode; it starts in Insert.
    normal: bool,
    /// The selected row of the slash-command menu.
    selected: usize,
}

struct AgentFileRequest {
    session: u64,
    id: Value,
    method: String,
    params: Value,
    buffers: Vec<(BufferId, PathBuf)>,
    written: Option<PathBuf>,
}

impl AgentPanel {
    pub(in crate::app) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            ..Self::default()
        }
    }
}

struct AgentSession {
    number: u64,
    name: String,
    root: PathBuf,
    /// `None` once the agent exited; the transcript stays readable and the next question
    /// starts the agent again in the same tab.
    process: Option<AgentProcess>,
    phase: Phase,
    cancelling: bool,
    turn_result: Option<Result<(), String>>,
    id: Option<String>,
    /// Questions asked before the session was ready, sent in order.
    queued: VecDeque<Vec<PromptPart>>,
    /// When the agent created the session.
    created: Option<std::time::Instant>,
    /// The agent's last stderr line, shown if it exits.
    last_log: String,
    entries: Vec<Entry>,
    options: Vec<SessionOption>,
    auth_methods: Vec<AuthMethod>,
    /// Requests outside the session's own start and prompt flow.
    requests: Vec<(u64, Pending)>,
    /// Option values from the profile, set once the session exists.
    preferred: BTreeMap<String, String>,
    /// The history file this tab saves to.
    history: String,
    /// What the agent offered for picking up an earlier session.
    rejoin: Rejoin,
    /// An earlier agent session to pick up on the next start, and the one being picked up.
    earlier: Option<String>,
    /// The conversation so far goes with the next question, because the agent started
    /// fresh instead of picking up its earlier session.
    recap: bool,
    /// The tab number of the session that started this one as a subagent, the task it was
    /// given, and where its answer goes once it is done.
    parent: Option<u64>,
    task: String,
    reply: Option<Reply>,
    /// The agent's own slash commands, by name and description.
    commands: Vec<(String, String)>,
    /// The `mcpServers` and `_meta` of `session/new`.
    tools: Value,
    meta: Option<Value>,
}

/// A started agent process, its phase, and the profile's option values and `_meta`.
type Spawned = (AgentProcess, Phase, BTreeMap<String, String>, Option<Value>);

/// The preferred option a subagent's requested model waits under until the session says which
/// models it offers.
const SUBAGENT_MODEL: &str = "subagent model";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Initializing(u64),
    Creating(u64),
    /// Picking up `earlier` with `session/resume` or `session/load`.
    Rejoining(u64),
    /// The agent asked for a sign-in before it creates the session.
    SigningIn,
    Ready,
    Prompting(u64),
    /// The agent exited; a tab resumed from history has no process but is not stopped.
    Stopped,
}

enum Pending {
    SetOption {
        option: String,
        value: String,
        required: bool,
    },
    Authenticate,
}

struct PermissionRequest {
    session: u64,
    id: Value,
    action: &'static str,
    title: String,
    options: Vec<PermissionOption>,
}

impl AgentSession {
    pub(in crate::app) fn status(&self, waiting: bool) -> &'static str {
        match (self.process.is_some(), self.phase) {
            (false, Phase::Stopped) => "stopped",
            (false, _) => "saved",
            _ if waiting => "waiting for you",
            (true, Phase::Initializing(_) | Phase::Creating(_) | Phase::Rejoining(_)) => "starting",
            (true, Phase::SigningIn) => "sign-in needed",
            (true, Phase::Prompting(_)) => "working",
            (true, Phase::Ready | Phase::Stopped) => "ready",
        }
    }

    pub(in crate::app) fn option(&self, key: &str) -> Option<&SessionOption> {
        self.options
            .iter()
            .find(|option| option.id == key)
            .or_else(|| self.options.iter().find(|option| option.category == key))
    }

    pub(in crate::app) fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        self.process
            .as_mut()
            .ok_or_else(|| format!("{} is not running", self.name))?
            .request(method, params)
    }

    pub(in crate::app) fn send_next_prompt(&mut self) {
        if self.phase != Phase::Ready
            || !self.requests.is_empty()
            || self
                .created
                .is_some_and(|created| created.elapsed() < STARTUP_TEXT_WAIT)
        {
            return;
        }
        let Some(id) = self.id.clone() else {
            return;
        };
        let Some(mut parts) = self.queued.front().cloned() else {
            return;
        };
        if self.recap {
            parts.insert(
                0,
                PromptPart::Text(history::recap(&self.entries, RECAP_CHARACTERS)),
            );
        }
        match self.request("session/prompt", agent::prompt_params(&id, &parts)) {
            Ok(request) => {
                let mut question = self.queued.pop_front().expect("queued question");
                if let Some(PromptPart::Text(text)) = question.pop() {
                    self.entries.push(Entry::User {
                        text,
                        context: question.iter().map(context_label).collect(),
                        attachments: question,
                    });
                }
                self.recap = false;
                self.cancelling = false;
                self.turn_result = None;
                self.phase = Phase::Prompting(request);
            }
            Err(error) => {
                if !matches!(self.entries.last(), Some(Entry::Notice(previous)) if previous == &error)
                {
                    self.entries.push(Entry::Notice(error));
                }
            }
        }
    }

    pub(in crate::app) fn set_option(&mut self, key: &str, value: &str) -> Result<(), String> {
        let session = self.id.clone().ok_or("the session is still starting")?;
        let option = self
            .option(key)
            .ok_or_else(|| format!("{} has no option `{key}`", self.name))?
            .clone();
        if !option
            .values
            .iter()
            .any(|candidate| candidate.value == value)
        {
            return Err(format!("{} has no {} `{value}`", self.name, option.name));
        }
        let (method, params) = agent::set_option_request(&session, &option, value);
        let id = self.request(method, params)?;
        self.requests.push((
            id,
            Pending::SetOption {
                required: self.parent.is_some() && (key == "model" || option.category == "model"),
                option: option.id,
                value: value.to_owned(),
            },
        ));
        Ok(())
    }

    /// Ask for a session: the earlier one if the agent can pick it up, otherwise a new one
    /// that gets the conversation so far with the next question.
    pub(in crate::app) fn open_session(&mut self) {
        let method = match &self.earlier {
            Some(_) if self.rejoin.resume => "session/resume",
            Some(_) if self.rejoin.load => "session/load",
            Some(_) => {
                self.earlier = None;
                self.recap = true;
                "session/new"
            }
            None => "session/new",
        };
        let params = match &self.earlier {
            Some(earlier) => {
                agent::rejoin_params(earlier, &self.root, &self.tools, self.meta.as_ref())
            }
            None => agent::new_session_params(&self.root, &self.tools, self.meta.as_ref()),
        };
        let params = match params {
            Ok(params) => params,
            Err(error) => {
                self.stop(&error);
                return;
            }
        };
        match self.request(method, params) {
            Ok(id) if method == "session/new" => self.phase = Phase::Creating(id),
            Ok(id) => self.phase = Phase::Rejoining(id),
            Err(error) => self.entries.push(Entry::Notice(error)),
        }
    }

    pub(in crate::app) fn attach(&mut self, (process, phase, preferred, meta): Spawned) {
        self.entries.push(Entry::Started {
            root: self.root.clone(),
        });
        self.earlier = self.id.take().or_else(|| self.earlier.take());
        self.process = Some(process);
        self.phase = phase;
        self.options.clear();
        self.requests.clear();
        self.preferred = preferred;
        self.meta = meta;
    }

    pub(in crate::app) fn working(&self) -> bool {
        self.process.is_some()
            && matches!(
                self.phase,
                Phase::Initializing(_)
                    | Phase::Creating(_)
                    | Phase::Rejoining(_)
                    | Phase::Prompting(_)
            )
    }

    /// The tab's title: its number and agent, or a subagent's task.
    pub(in crate::app) fn title(&self) -> String {
        match self.parent {
            Some(_) => format!("↳ {}", self.task),
            None => format!("{} {}", self.number, self.name),
        }
    }

    pub(in crate::app) fn conversation(&self) -> Conversation {
        let title = self
            .entries
            .iter()
            .find_map(|entry| match entry {
                Entry::User { text, .. } => text.lines().next(),
                _ => None,
            })
            .unwrap_or_default();
        Conversation {
            id: self.history.clone(),
            agent: self.name.clone(),
            root: self.root.clone(),
            session: self.id.clone().or_else(|| self.earlier.clone()),
            title: title.chars().take(100).collect(),
            updated: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        }
    }

    /// Profile preferences such as `model = "…"`, applied once the session exists.
    /// A subagent asked for a model runs on that model or not at all, so the agent that asked
    /// never gets an answer from a model it did not pick. The model may be given by its id or,
    /// ignoring case, its name.
    pub(in crate::app) fn choose_subagent_model(&mut self) -> Result<(), String> {
        let Some(wanted) = self.preferred.remove(SUBAGENT_MODEL) else {
            return Ok(());
        };
        let Some(option) = self.option("model") else {
            return Err(format!("{} does not let Dust choose its model", self.name));
        };
        let Some(value) = option
            .values
            .iter()
            .find(|candidate| candidate.value == wanted)
            .or_else(|| {
                option
                    .values
                    .iter()
                    .find(|candidate| candidate.name.eq_ignore_ascii_case(&wanted))
            })
        else {
            let offered = option
                .values
                .iter()
                .map(|candidate| candidate.value.as_str())
                .collect::<Vec<_>>();
            return Err(format!(
                "{} has no model `{wanted}`; it offers: {}",
                self.name,
                offered.join(", ")
            ));
        };
        let (id, value) = (option.id.clone(), value.value.clone());
        self.preferred.remove("model");
        self.preferred.insert(id, value);
        Ok(())
    }

    pub(in crate::app) fn apply_preferred(&mut self) {
        for (key, value) in std::mem::take(&mut self.preferred) {
            if self
                .option(&key)
                .is_some_and(|option| option.current == value)
            {
                continue;
            }
            if let Err(error) = self.set_option(&key, &value) {
                if self.parent.is_some() {
                    self.stop(&error);
                    break;
                }
                self.entries.push(Entry::Notice(error));
            }
        }
    }

    pub(in crate::app) fn stop(&mut self, reason: &str) {
        self.queued.clear();
        self.requests.clear();
        self.process = None;
        self.phase = Phase::Stopped;
        self.turn_result = Some(Err(reason.to_owned()));
        self.entries.push(Entry::Notice(reason.to_owned()));
        if let Some(reply) = self.reply.take() {
            reply.send(Err(reason.to_owned()));
        }
    }
}

impl App {
    fn respond_to_agent(&mut self, index: usize, id: Value, result: Result<Value, String>) {
        if let Some(process) = &mut self.agent.sessions[index].process
            && let Err(error) = process.respond(id, result)
        {
            self.stop_agent_session(index, &error);
        }
    }

    fn stop_agent_session(&mut self, index: usize, reason: &str) {
        let number = self.agent.sessions[index].number;
        self.agent.sessions[index].stop(reason);
        self.cancel_agent_permissions(&[number]);
        self.stop_agent_children(number);
    }

    fn active_agent(&self) -> Option<&AgentSession> {
        self.agent.sessions.get(self.agent.active)
    }

    fn active_agent_mut(&mut self) -> Option<&mut AgentSession> {
        self.agent.sessions.get_mut(self.agent.active)
    }

    /// `<leader>aa` in Visual mode: show the agent with the selection going along with the
    /// next question.
    pub(in crate::app) fn begin_agent_question(&mut self) {
        if self.configuration.settings().agents.connection
            == crate::config::AgentConnection::Terminal
        {
            self.ask_terminal_agent();
            return;
        }
        if let Some(view) = self.state.active_view()
            && self.state.views[view].selection.is_some()
        {
            let buffer = &self.state.buffers[self.state.views[view].buffer];
            let text = selected_text(&buffer.text, &self.state.views[view], self.state.policy);
            let part = match &buffer.path {
                Some(path) => PromptPart::File {
                    path: path.clone(),
                    text: Some(text),
                },
                None => PromptPart::Text(format!("```\n{text}\n```")),
            };
            self.agent.context.push(part);
            self.state.views[view].selection = None;
            self.set_mode(EditorMode::Normal);
        }
        self.show_agent_panel();
        self.start_default_agent();
        self.refresh_agent_transcript();
    }

    /// Opening the panel starts the default agent, so it is ready by the time the first
    /// question is typed. With no default, or its program missing, the panel says so instead.
    fn start_default_agent(&mut self) {
        let agents = &self.configuration.settings().agents;
        if !self.agent.sessions.is_empty()
            || !agents
                .profiles
                .get(&agents.default)
                .is_some_and(|profile| self.agent_program(&profile.program).is_some())
        {
            return;
        }
        let name = agents.default.clone();
        if let Err(error) = self.start_agent_session(&name) {
            self.show_error(error);
        }
    }

    /// The default agent's own program in a terminal, for `agents.connection = "terminal"`:
    /// shows the one that is running, with the selection pasted into its prompt, or starts it.
    fn ask_terminal_agent(&mut self) {
        let name = self.configuration.settings().agents.default.clone();
        if name.is_empty() {
            self.show_error(
                "Choose the agent to run as Default Agent in Settings (ctrl-,)".to_owned(),
            );
            return;
        }
        self.add_agent_tools(&name);
        let Some(profile) = self.configuration.settings().agents.profiles.get(&name) else {
            self.show_error(format!("agents.default names no agent profile: {name}"));
            return;
        };
        if profile.cli.is_empty() {
            self.show_error(format!(
                "agent {name} has no cli command to run in a terminal"
            ));
            return;
        }
        // Through the shell, as the lazygit profile does, so the program is found on PATH and a
        // missing one says so in the terminal.
        let arguments = ["-c".to_owned(), "exec \"$0\" \"$@\"".to_owned()]
            .into_iter()
            .chain(profile.cli.iter().cloned())
            .collect();
        let environment = profile.environment.clone();
        let running = self
            .terminal_session
            .descriptions()
            .into_iter()
            .find(|(id, profile, lifecycle)| {
                self.agent_terminal(crate::process::PtyId(*id))
                    && *profile == name
                    && matches!(
                        lifecycle,
                        ContentLifecycle::Preflight
                            | ContentLifecycle::Starting
                            | ContentLifecycle::Running
                    )
            })
            .map(|(id, ..)| crate::process::PtyId(id));
        let source_view = self.state.active_view();
        let selection = source_view.and_then(|view| {
            self.state.views[view].selection?;
            let buffer = &self.state.buffers[self.state.views[view].buffer];
            let text = selected_text(&buffer.text, &self.state.views[view], self.state.policy);
            Some(match &buffer.path {
                Some(path) => format!(
                    "{}:\n```\n{text}\n```\n",
                    self.workspace_relative_label(path)
                ),
                None => format!("```\n{text}\n```\n"),
            })
        });
        if let Some(id) = running {
            self.reveal_terminal(id);
            self.terminal_session.enter_terminal_input_mode(id);
            if let Some(text) = selection {
                match self.terminal_session.send(
                    id,
                    crate::app::terminal::TerminalInput::Paste(&text),
                    &self.workspace,
                ) {
                    Ok(true) => {
                        if let Some(view) = source_view {
                            self.state.views[view].selection = None;
                        }
                        self.set_mode(EditorMode::Normal);
                    }
                    Ok(false) => self.show_error("the agent terminal did not accept the selection"),
                    Err(error) => self.show_error(error),
                }
            }
            self.sync_terminal_panel();
            return;
        }
        let program = crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments,
            cwd: None,
            environment,
            columns: 80,
            rows: 24,
            float: false,
            close_on_exit: true,
        };
        match self.terminal_session.open_program(
            &name,
            program,
            &self.workspace,
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view()),
            self.input.capture_epoch(),
            true,
        ) {
            Ok(intent) => {
                self.run_trusted(crate::app::TrustedAction::OpenTerminal(intent));
                if selection.is_some() {
                    self.notify_message(
                        format!("Starting {name}; press <leader>aa again to send the selection"),
                        false,
                    );
                }
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::app) fn attach_agent_file(&mut self) {
        let Some(path) = self.active_path() else {
            self.show_error("the current buffer has no file");
            return;
        };
        self.agent
            .context
            .push(PromptPart::File { path, text: None });
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    pub(in crate::app) fn attach_agent_diagnostics(&mut self) {
        let (Some(path), Some(buffer)) = (self.active_path(), self.state.active_buffer_id()) else {
            self.show_error("the current buffer has no file");
            return;
        };
        let uri = crate::language::lsp::file_uri(&path);
        let text = &self.state.buffers[buffer].text;
        let lines = self
            .diagnostics
            .choices()
            .filter(|(_, problem, _)| problem.uri == uri)
            .map(|(_, problem, _)| {
                let line = text
                    .byte_to_line(problem.range.start)
                    .map_or(0, |line| line + 1);
                format!("- line {line}: {:?}: {}", problem.severity, problem.message)
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            self.notify_message("no diagnostics in this file".into(), false);
            return;
        }
        self.agent.context.push(PromptPart::File {
            path: path.clone(),
            text: None,
        });
        self.agent.context.push(PromptPart::Text(format!(
            "Diagnostics in {}:\n{}",
            path.display(),
            lines.join("\n")
        )));
        if self.agent.input.is_empty() {
            self.agent.input = "Fix these diagnostics".into();
            self.agent.cursor = self.agent.input.len();
        }
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    pub(in crate::app) fn agent_ask(&mut self, question: String) {
        let question = question.trim().to_owned();
        if question.is_empty() {
            return;
        }
        if self.agent.sessions.is_empty() {
            let name = self.configuration.settings().agents.default.clone();
            // With no default, ask which agent; the question stays to send to it.
            if name.is_empty() {
                self.agent.input = question;
                self.agent.cursor = self.agent.input.len();
                self.refresh_agent_transcript();
                self.open_agent_picker();
                return;
            }
            if let Err(error) = self.start_agent_session(&name) {
                // The question stays in the input to send once the agent can start.
                self.agent.input = question;
                self.agent.cursor = self.agent.input.len();
                self.show_error(error);
                self.refresh_agent_transcript();
                return;
            }
        } else if self
            .active_agent()
            .is_some_and(|session| session.process.is_none())
            && let Err(error) = self.restart_active_agent()
        {
            self.agent.input = question;
            self.agent.cursor = self.agent.input.len();
            self.show_error(error);
            self.refresh_agent_transcript();
            return;
        }
        let mut parts = std::mem::take(&mut self.agent.context);
        parts.push(PromptPart::Text(question));
        let signing_in = if let Some(session) = self.active_agent_mut() {
            session.queued.push_back(parts);
            if session.phase == Phase::Ready {
                session.send_next_prompt();
            }
            session.phase == Phase::SigningIn
        } else {
            false
        };
        self.save_agent_history(self.agent.active);
        self.show_agent_panel();
        self.refresh_agent_transcript();
        if signing_in {
            self.open_agent_login_picker();
        }
    }

    pub(in crate::app) fn cancel_agent_turn(&mut self) {
        let Some(session) = self.agent.sessions.get_mut(self.agent.active) else {
            self.notify_message("no agent session".into(), false);
            return;
        };
        session.queued.clear();
        session.cancelling = true;
        let failure = if let (Phase::Prompting(_), Some(id), Some(process)) =
            (session.phase, session.id.clone(), session.process.as_mut())
        {
            process
                .notify("session/cancel", json!({"sessionId": id}))
                .err()
        } else {
            None
        };
        let number = session.number;
        if let Some(error) = failure {
            self.stop_agent_session(
                self.agent.active,
                &format!("could not send cancellation: {error}; agent stopped"),
            );
            self.show_error(format!(
                "could not send cancellation: {error}; agent stopped"
            ));
            return;
        }
        self.stop_agent_children(number);
        self.cancel_agent_permissions(&[number]);
        self.notify_message("stopping the agent…".into(), false);
    }

    fn stop_agent_children(&mut self, number: u64) {
        let mut children = Vec::new();
        for child in self
            .agent
            .sessions
            .iter_mut()
            .filter(|child| child.parent == Some(number))
        {
            children.push(child.number);
            if child.process.is_some() || !child.queued.is_empty() || child.reply.is_some() {
                child.stop("the parent agent stopped the subagent");
            }
        }
        self.cancel_agent_permissions(&children);
    }

    fn cancel_agent_permissions(&mut self, numbers: &[u64]) {
        // Permissions waiting for path resolution must be cancelled too, or their
        // completion would reopen the prompt after the turn was stopped.
        let pending: Vec<_> = self
            .agent
            .files
            .iter()
            .filter(|(_, request)| {
                request.method == "session/request_permission" && numbers.contains(&request.session)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in pending {
            self.documents.cancel_request(id);
            if let Some(request) = self.agent.files.remove(&id)
                && let Some(index) = self
                    .agent
                    .sessions
                    .iter()
                    .position(|session| session.number == request.session)
            {
                self.respond_to_agent(index, request.id, Ok(agent::permission_cancelled()));
            }
        }
        let (cancelled, kept): (Vec<_>, Vec<_>) = self
            .agent
            .permissions
            .drain(..)
            .partition(|request| numbers.contains(&request.session));
        self.agent.permissions = kept.into();
        let asking = self
            .agent
            .asking
            .take_if(|request| numbers.contains(&request.session));
        let prompt_open = asking.is_some();
        for request in cancelled.into_iter().chain(asking) {
            if let Some(index) = self
                .agent
                .sessions
                .iter()
                .position(|session| session.number == request.session)
            {
                self.respond_to_agent(index, request.id, Ok(agent::permission_cancelled()));
            }
        }
        if prompt_open {
            self.close_prompt();
        }
    }

    /// `<leader>an`: pick an agent to start in a new tab.
    pub(in crate::app) fn open_agent_picker(&mut self) {
        let agents = &self.configuration.settings().agents;
        let default = PickerItemId(format!("agent:{}", agents.default));
        let items = agents
            .profiles
            .iter()
            .map(|(name, profile)| {
                let command = std::iter::once(profile.program.display().to_string())
                    .chain(profile.arguments.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ");
                let badge = if self.agent_program(&profile.program).is_none() {
                    "not installed".into()
                } else if *name == agents.default {
                    "default".into()
                } else {
                    String::new()
                };
                (
                    picker_row(format!("agent:{name}"), name.clone(), command, badge),
                    PickerPayload::Agent(AgentChoice::Profile(name.clone())),
                )
            })
            .collect();
        self.install_picker("New Agent Session".to_owned(), items);
        // It starts on the default agent, not on whichever was picked last.
        self.search.select_picker_item(&default);
        self.search
            .set_picker_footer("Enter Start in a new tab · Esc Esc Close".to_owned());
        self.sync_picker_ui();
    }

    /// `<leader>as`: the open sessions.
    pub(in crate::app) fn open_agent_sessions_picker(&mut self) {
        if self.agent.sessions.is_empty() {
            self.notify_message("no agent sessions; start one with <leader>an".into(), false);
            return;
        }
        let items = self
            .agent
            .sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let waiting = self.agent_waiting(session.number);
                let model = session
                    .option("model")
                    .map(|option| format!(" · {}", value_name(option)))
                    .unwrap_or_default();
                (
                    picker_row(
                        format!("agent-session:{}", session.number),
                        format!("{} {}", session.number, session.name),
                        format!(
                            "{}{model} · {}",
                            session.status(waiting),
                            session.root.display()
                        ),
                        if index == self.agent.active {
                            "shown".into()
                        } else {
                            String::new()
                        },
                    ),
                    PickerPayload::Agent(AgentChoice::Session(session.number)),
                )
            })
            .collect();
        self.install_picker("Agent Sessions".to_owned(), items);
        self.search
            .set_picker_footer("Enter Show · Esc Esc Close".to_owned());
        self.sync_picker_ui();
    }

    /// `<leader>am`, `<leader>aM`, `<leader>ar` and `<leader>ao`: the active session's
    /// model, mode, thinking level, or all of its options.
    pub(in crate::app) fn open_agent_option_picker(&mut self, key: Option<&str>) {
        let Some(session) = self.active_agent() else {
            self.notify_message("no agent session; start one with <leader>an".into(), false);
            return;
        };
        if session.id.is_none() {
            self.notify_message(format!("{} is still starting", session.name), false);
            return;
        }
        let Some(key) = key else {
            if session.options.is_empty() {
                self.notify_message(format!("{} offers no options", session.name), false);
                return;
            }
            let items = session
                .options
                .iter()
                .map(|option| {
                    (
                        picker_row(
                            format!("agent-option:{}", option.id),
                            option.name.clone(),
                            value_name(option),
                            String::new(),
                        ),
                        PickerPayload::Agent(AgentChoice::Option(option.id.clone())),
                    )
                })
                .collect();
            let title = format!("{} Options", session.name);
            self.install_picker(title, items);
            self.search
                .set_picker_footer("Enter Change · Esc Esc Close".to_owned());
            self.sync_picker_ui();
            return;
        };
        // The model picker also lists the other agents, so one key changes agent and model.
        let mut others = Vec::new();
        if key == "model" {
            let current = session.name.clone();
            for (name, profile) in &self.configuration.settings().agents.profiles {
                if *name != current && self.agent_program(&profile.program).is_some() {
                    others.push((
                        picker_row(
                            format!("agent:{name}"),
                            format!("{name} ›"),
                            "another agent, in a new tab".to_owned(),
                            "agent".to_owned(),
                        ),
                        PickerPayload::Agent(AgentChoice::Profile(name.clone())),
                    ));
                }
            }
        }
        let Some(option) = session.option(key).cloned() else {
            if !others.is_empty() {
                let title = format!("{} Agent", session.name);
                self.install_picker(title, others);
                self.search
                    .set_picker_footer("Enter Use · Esc Esc Close".to_owned());
                self.sync_picker_ui();
                return;
            }
            let what = match key {
                THINKING => "thinking level",
                other => other,
            };
            self.notify_message(format!("{} offers no {what} choice", session.name), false);
            return;
        };
        let items = option
            .values
            .iter()
            .map(|value| {
                (
                    picker_row(
                        format!("agent-value:{}", value.value),
                        value.name.clone(),
                        value.description.clone(),
                        if value.value == option.current {
                            "current".into()
                        } else {
                            String::new()
                        },
                    ),
                    PickerPayload::Agent(AgentChoice::Value {
                        option: option.id.clone(),
                        value: value.value.clone(),
                    }),
                )
            })
            .chain(others)
            .collect();
        self.install_picker(format!("{} {}", session.name, option.name), items);
        self.search
            .set_picker_footer("Enter Use · Esc Esc Close".to_owned());
        self.sync_picker_ui();
    }

    /// `<leader>aR`: saved conversations, newest first.
    pub(in crate::app) fn open_agent_resume_picker(&mut self) {
        let saved = history::list(&self.agent.directory);
        if saved.is_empty() {
            self.notify_message("no saved agent conversations".into(), false);
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let items = saved
            .into_iter()
            .map(|conversation| {
                let open = self
                    .agent
                    .sessions
                    .iter()
                    .any(|session| session.history == conversation.id);
                (
                    picker_row(
                        format!("agent-history:{}", conversation.id),
                        conversation.title,
                        format!(
                            "{} · {} · {}",
                            conversation.agent,
                            age(now.saturating_sub(conversation.updated)),
                            shown_root(&conversation.root, 60)
                        ),
                        if open { "open".into() } else { String::new() },
                    ),
                    PickerPayload::Agent(AgentChoice::Resume(conversation.id)),
                )
            })
            .collect();
        self.install_picker("Resume Agent Conversation".to_owned(), items);
        self.search
            .set_picker_footer("Enter Resume in a tab · Esc Esc Close".to_owned());
        self.sync_picker_ui();
    }

    fn open_agent_login_picker(&mut self) {
        let Some(session) = self.active_agent() else {
            return;
        };
        let items = session
            .auth_methods
            .iter()
            .map(|method| {
                (
                    picker_row(
                        format!("agent-login:{}", method.id),
                        method.name.clone(),
                        method.description.clone(),
                        String::new(),
                    ),
                    PickerPayload::Agent(AgentChoice::Login(method.id.clone())),
                )
            })
            .collect();
        let title = format!("Sign In to {}", session.name);
        self.install_picker(title, items);
        self.search
            .set_picker_footer("Enter Sign in · Esc Esc Close".to_owned());
        self.sync_picker_ui();
    }

    pub(in crate::app) fn accept_agent_choice(&mut self, choice: AgentChoice) {
        let result = match choice {
            AgentChoice::Profile(name) => self.open_agent_tab(&name),
            AgentChoice::Session(number) => {
                if let Some(index) = self.agent.sessions.iter().position(|s| s.number == number) {
                    self.agent.active = index;
                    self.agent.scroll = 0;
                    self.show_agent_panel();
                }
                Ok(())
            }
            AgentChoice::Resume(id) => match self.reopen_conversation(&id) {
                Ok(index) if self.agent.sessions[index].process.is_none() => {
                    self.show_agent_panel();
                    self.agent.normal = false;
                    self.restart_active_agent()
                }
                Ok(_) => {
                    self.show_agent_panel();
                    self.agent.normal = false;
                    Ok(())
                }
                Err(error) => Err(error),
            },
            AgentChoice::Option(option) => {
                self.open_agent_option_picker(Some(&option));
                return;
            }
            AgentChoice::Value { option, value } => self
                .active_agent_mut()
                .ok_or_else(|| "no agent session".to_owned())
                .and_then(|session| session.set_option(&option, &value)),
            AgentChoice::Login(method) => self
                .active_agent_mut()
                .ok_or_else(|| "no agent session".to_owned())
                .and_then(|session| {
                    let id = session.request("authenticate", json!({"methodId": method}))?;
                    session.requests.push((id, Pending::Authenticate));
                    session.entries.push(Entry::Notice("Signing in…".into()));
                    Ok(())
                }),
        };
        if let Err(error) = result {
            self.show_error(error);
        }
        self.refresh_agent_transcript();
    }

    /// `<leader>a]` and `<leader>a[`.
    pub(in crate::app) fn cycle_agent_session(&mut self, offset: isize) {
        let count = self.agent.sessions.len();
        if count == 0 {
            self.notify_message("no agent sessions; start one with <leader>an".into(), false);
            return;
        }
        self.agent.active =
            (self.agent.active as isize + offset).rem_euclid(count as isize) as usize;
        self.agent.scroll = 0;
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    /// `<leader>ax`: stop the active session and close its tab.
    pub(in crate::app) fn close_agent_session(&mut self) {
        if self.agent.sessions.is_empty() {
            self.notify_message("no agent session".into(), false);
            return;
        }
        // Closed tabs stay in the history and can be resumed with <leader>aR.
        self.save_agent_history(self.agent.active);
        let number = self.agent.sessions[self.agent.active].number;
        self.cancel_agent_permissions(&[number]);
        self.stop_agent_children(number);
        let mut session = self.agent.sessions.remove(self.agent.active);
        if let Some(reply) = session.reply.take() {
            reply.send(Err("the user closed the subagent".into()));
        }
        self.agent.active = self
            .agent
            .active
            .min(self.agent.sessions.len().saturating_sub(1));
        drop(session);
        if self.agent.sessions.is_empty() && self.agent_pane().is_some() {
            self.toggle_agent_panel();
        }
        self.refresh_agent_transcript();
    }

    /// Close sessions before their disposable workspace is removed.
    pub(in crate::app) fn close_agent_sessions_in_root(&mut self, root: &Path) {
        let active = self
            .agent
            .sessions
            .get(self.agent.active)
            .map(|session| session.number);
        while let Some(index) = self
            .agent
            .sessions
            .iter()
            .position(|session| session.root.starts_with(root))
        {
            self.agent.active = index;
            self.close_agent_session();
        }
        if let Some(index) = self
            .agent
            .sessions
            .iter()
            .position(|session| Some(session.number) == active)
        {
            self.agent.active = index;
        }
        self.refresh_agent_transcript();
    }

    /// Starting an installed adapter from the tool manager opens a session of the profile
    /// that runs it.
    pub(in crate::app) fn start_agent_for_program(&mut self, executable: &str) {
        let profile = self
            .configuration
            .settings()
            .agents
            .profiles
            .iter()
            .find(|(_, profile)| profile.program == Path::new(executable))
            .map(|(name, _)| name.clone());
        let Some(name) = profile else {
            self.show_error(format!("no profile in [agents.profiles] runs {executable}"));
            return;
        };
        match self.start_agent_session(&name) {
            Ok(()) => {
                self.show_agent_panel();
                self.refresh_agent_transcript();
            }
            Err(error) => self.show_error(error),
        }
    }

    /// `ctrl-t` in the panel: a new tab running the default agent. With no default ("ask every
    /// time") it asks which, unless only one agent is installed.
    pub(in crate::app) fn new_agent_tab(&mut self) {
        let default = self.configuration.settings().agents.default.clone();
        if !default.is_empty() {
            self.accept_agent_choice(AgentChoice::Profile(default));
            return;
        }
        let installed = self
            .configuration
            .settings()
            .agents
            .profiles
            .iter()
            .filter(|(_, profile)| self.agent_program(&profile.program).is_some())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        match installed.as_slice() {
            [name] => self.accept_agent_choice(AgentChoice::Profile(name.clone())),
            _ => self.open_agent_picker(),
        }
    }

    /// Start `name` in a new tab, as many as are asked for, empty or not.
    pub(in crate::app) fn open_agent_tab(&mut self, name: &str) -> Result<(), String> {
        self.start_agent_session(name)?;
        self.agent.scroll = 0;
        // A new tab keeps the mode the panel is in, as a new tab does in the editor.
        self.show_agent_panel();
        Ok(())
    }

    /// An agent's `agent` tool call: start a subagent of the asking session in a new tab.
    /// The tab shown does not change; the subagent's answer goes back once it is done.
    pub(in crate::app) fn start_subagent(&mut self, request: AgentRequest) {
        if request.reply.is_cancelled() {
            return;
        }
        let Some(parent) = self
            .agent
            .sessions
            .iter()
            .find(|session| session.number == request.parent)
        else {
            request
                .reply
                .send(Err("the agent that asked is no longer open in Dust".into()));
            return;
        };
        if !matches!(parent.phase, Phase::Prompting(_))
            || parent.cancelling
            || parent.process.is_none()
        {
            request
                .reply
                .send(Err("the parent agent turn is no longer active".into()));
            return;
        }
        let name = request.agent.clone().unwrap_or_else(|| parent.name.clone());
        let root = parent.root.clone();
        let mut spawned = match self.spawn_agent(&name, &root, None) {
            Ok(spawned) => spawned,
            Err(error) => {
                request.reply.send(Err(error));
                return;
            }
        };
        let index = match self.add_agent_tab(name, root, Vec::new(), None, None) {
            Ok(index) => index,
            Err(error) => {
                request.reply.send(Err(error));
                return;
            }
        };
        let session = &mut self.agent.sessions[index];
        session.parent = Some(request.parent);
        session.task = match request.description.trim() {
            "" => request
                .prompt
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(40)
                .collect(),
            description => description.to_owned(),
        };
        // Subagents do not start subagents of their own.
        session.tools = json!([]);
        if let Some(model) = request.model {
            spawned.2.insert(SUBAGENT_MODEL.into(), model);
        }
        session.attach(spawned);

        session
            .queued
            .push_back(vec![PromptPart::Text(request.prompt)]);
        session.reply = Some(request.reply);
        self.refresh_agent_transcript();
    }

    /// Send a subagent's answer once its turn is over, and stop its agent.
    pub(in crate::app) fn settle_subagent(&mut self, index: usize) -> bool {
        let session = &mut self.agent.sessions[index];
        let finished = match session.phase {
            _ if session.reply.is_none() => false,
            _ if session.process.is_none() => true,
            Phase::Ready => session.queued.is_empty(),
            Phase::SigningIn => true,
            _ => false,
        };
        let Some(reply) = finished.then(|| session.reply.take()).flatten() else {
            return false;
        };
        let turn = session
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::User { .. }))
            .map_or(0, |user| user + 1);
        let turn = &session.entries[turn..];
        let answer = turn
            .iter()
            .filter_map(|entry| match entry {
                Entry::Agent(text) if !text.trim().is_empty() => Some(text.trim()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let notices = turn
            .iter()
            .filter_map(|entry| match entry {
                Entry::Notice(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        let failure = match &session.turn_result {
            Some(Ok(())) => None,
            Some(Err(error)) => Some(error.clone()),
            None => Some(if notices.is_empty() {
                format!("{} stopped without completing its turn", session.name)
            } else {
                notices.clone()
            }),
        };
        let failed = failure.is_some() || answer.is_empty();
        reply.send(if let Some(error) = failure {
            Err(error)
        } else if answer.is_empty() {
            Err(format!("{} stopped without an answer", session.name))
        } else {
            Ok(answer)
        });
        session.process = None;
        session.phase = if failed { Phase::Stopped } else { Phase::Ready };
        true
    }

    fn start_agent_session(&mut self, name: &str) -> Result<(), String> {
        let root = self.state.workspace_root.clone();
        self.add_agent_tools(name);
        let spawned = self.spawn_agent(name, &root, Some(self.agent.started + 1))?;
        let agents = &self.configuration.settings().agents;
        let note = agents
            .profiles
            .get(name)
            .and_then(add_tools_command)
            .filter(|_| !agents.add_tools)
            .map(|command| {
                let command = command
                    .iter()
                    .map(|word| word.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                Entry::Notice(format!(
                    "To give {name} Dust's subagents, turn on Add Dust's Tools in Settings \
                     (ctrl-,) or run this once: {command}"
                ))
            });
        let index = self.add_agent_tab(name.to_owned(), root, Vec::new(), None, None)?;
        self.agent.active = index;
        self.agent.sessions[index].attach(spawned);
        self.agent.sessions[index].entries.extend(note);
        Ok(())
    }

    /// A stopped tab; its agent starts with `attach`.
    pub(in crate::app) fn add_agent_tab(
        &mut self,
        name: String,
        root: PathBuf,
        entries: Vec<Entry>,
        earlier: Option<String>,
        history: Option<String>,
    ) -> Result<usize, String> {
        let number = self.agent.started + 1;
        let history = history.unwrap_or_else(|| {
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis());
            format!("{millis}-{}-{number}", std::process::id())
        });
        let tools = match (&self.remote, std::env::current_exe()) {
            (Some(server), Ok(dust)) => {
                let profiles = self.configuration.settings().agents.profiles.keys();
                let profiles = profiles.map(String::as_str).collect::<Vec<_>>().join(",");
                json!([agent::mcp::server_entry(
                    &dust,
                    server.path(),
                    number,
                    &profiles
                )?])
            }
            _ => json!([]),
        };
        self.agent.started = number;
        self.agent.sessions.push(AgentSession {
            number,
            name,
            root,
            process: None,
            phase: Phase::Ready,
            cancelling: false,
            turn_result: None,
            id: None,
            queued: VecDeque::new(),
            created: None,
            last_log: String::new(),
            entries,
            options: Vec::new(),
            auth_methods: Vec::new(),
            requests: Vec::new(),
            preferred: BTreeMap::new(),
            history,
            rejoin: Rejoin::default(),
            earlier,
            recap: false,
            parent: None,
            task: String::new(),
            reply: None,
            commands: Vec::new(),
            tools,
            meta: None,
        });
        Ok(self.agent.sessions.len() - 1)
    }

    /// Start the active tab's agent again after it exited, in the tab's own worktree. It
    /// picks up the tab's earlier session when it can.
    fn restart_active_agent(&mut self) -> Result<(), String> {
        let Some(session) = self.active_agent() else {
            return Ok(());
        };
        let (name, root, tab) = (session.name.clone(), session.root.clone(), session.number);
        let tab = session.parent.is_none().then_some(tab);
        self.add_agent_tools(&name);
        let spawned = self.spawn_agent(&name, &root, tab)?;
        // A restarted process can reuse JSON-RPC IDs, but must not receive replies
        // to filesystem requests made by the previous process in this tab.
        let pending: Vec<_> = self
            .agent
            .files
            .iter()
            .filter(|(_, request)| request.session == self.agent.sessions[self.agent.active].number)
            .map(|(id, _)| *id)
            .collect();
        for id in pending {
            self.agent.files.remove(&id);
            self.documents.cancel_request(id);
        }
        self.agent.sessions[self.agent.active].attach(spawned);
        Ok(())
    }

    /// Start the agent of profile `name`. `tab` is the session's tab number, for agents that
    /// start Dust's MCP server from their own config; subagents get none.
    /// Run the agent's `add_tools` command once per run of Dust when `agents.add_tools` is on, so
    /// an agent that reads MCP servers only from its own config gets Dust's tools before it
    /// starts. A failure is shown and not retried until Dust starts again.
    pub(in crate::app) fn add_agent_tools(&mut self, name: &str) {
        let agents = &self.configuration.settings().agents;
        let Some(command) = agents.profiles.get(name).and_then(add_tools_command) else {
            return;
        };
        if !agents.add_tools || !self.agent.tools_added.insert(name.to_owned()) {
            return;
        }
        let Some((program, arguments)) = command.split_first() else {
            return;
        };
        let (program, arguments) = (program.clone(), arguments.to_vec());
        let (name, failures) = (name.to_owned(), self.agent.tool_failures.clone());
        // In the background, so the agent starts at once. The command saves the tools in the
        // agent's own config, where every later start finds them.
        std::thread::spawn(move || {
            let output = std::process::Command::new(program)
                .args(arguments)
                .stdin(std::process::Stdio::null())
                .output();
            let error = match output {
                Ok(output) if output.status.success() => return,
                Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                Err(error) => error.to_string(),
            };
            if let Ok(mut failures) = failures.lock() {
                failures.push(format!("could not add Dust's tools to {name}: {error}"));
            }
        });
    }

    fn spawn_agent(&self, name: &str, root: &Path, tab: Option<u64>) -> Result<Spawned, String> {
        let profile = self
            .configuration
            .settings()
            .agents
            .profiles
            .get(name)
            .ok_or_else(|| format!("no agent named `{name}` in [agents.profiles]"))?;
        let Some(program) = self.agent_program(&profile.program) else {
            return Err(self.missing_agent_program(&profile.program));
        };
        let mut environment = profile
            .environment
            .iter()
            .chain(&profile.acp_environment)
            .map(|(key, value)| (key.clone(), std::ffi::OsString::from(value)))
            .collect::<BTreeMap<_, _>>();
        if let Some(entry) = self.tooling.entries().iter().find(|entry| {
            entry.definition.kind == ToolKind::Agent
                && matches!(&entry.status, ToolStatus::Installed(path) if path == &program)
        }) {
            environment.extend(crate::tools::tool_launch_environment(&entry.definition));
        }
        if let Some(server) = &self.remote {
            environment.insert(
                crate::remote::SERVER_VARIABLE.into(),
                server.path().as_os_str().to_owned(),
            );
        }
        if let Some(tab) = tab {
            environment.insert(agent::mcp::TAB_VARIABLE.into(), tab.to_string().into());
        }
        let profiles = self.configuration.settings().agents.profiles.keys();
        let profiles = profiles.map(String::as_str).collect::<Vec<_>>().join(",");
        environment.insert(agent::mcp::PROFILES_VARIABLE.into(), profiles.into());
        let mut process = AgentProcess::spawn(&program, &profile.arguments, &environment, root)
            .map_err(|error| format!("could not start agent {name}: {error}"))?;
        let id = process.request("initialize", agent::initialize_params())?;
        Ok((
            process,
            Phase::Initializing(id),
            profile.options.clone(),
            profile.meta.clone(),
        ))
    }

    fn missing_agent_program(&self, program: &Path) -> String {
        let installable = self.tooling.entries().iter().any(|entry| {
            entry.definition.kind == ToolKind::Agent
                && Path::new(&entry.definition.executable) == program
        });
        if installable {
            format!(
                "{} is not installed; install it in the tool manager (<leader>cm, Agent)",
                program.display()
            )
        } else {
            format!("{} is not installed", program.display())
        }
    }

    /// The program to run: a path as given, a Dust-installed adapter, or one on `PATH`.
    fn agent_program(&self, program: &Path) -> Option<PathBuf> {
        if program.components().count() > 1 {
            return program.is_file().then(|| program.to_path_buf());
        }
        let installed = self
            .tooling
            .entries()
            .iter()
            .find_map(|entry| match &entry.status {
                ToolStatus::Installed(path)
                    if entry.definition.kind == ToolKind::Agent
                        && Path::new(&entry.definition.executable) == program =>
                {
                    Some(path.clone())
                }
                _ => None,
            });
        installed.or_else(|| {
            self.tooling
                .path_executable(&program.to_string_lossy())
                .map(|(path, _)| path)
        })
    }

    fn agent_waiting(&self, number: u64) -> bool {
        self.agent
            .asking
            .as_ref()
            .is_some_and(|request| request.session == number)
            || self
                .agent
                .permissions
                .iter()
                .any(|request| request.session == number)
    }

    pub(in crate::app) fn process_agent_events(&mut self) {
        let failures = self
            .agent
            .tool_failures
            .lock()
            .map(|mut failures| std::mem::take(&mut *failures))
            .unwrap_or_default();
        for error in history::take_errors().into_iter().chain(failures) {
            self.show_error(error);
        }
        let mut changed = false;
        for index in 0..self.agent.sessions.len() {
            if self.agent.sessions[index]
                .reply
                .as_ref()
                .is_some_and(Reply::is_cancelled)
            {
                let number = self.agent.sessions[index].number;
                self.cancel_agent_permissions(&[number]);
                self.agent.sessions[index].stop("the MCP client cancelled the subagent");
                changed = true;
            }
            let mut save = false;
            if let Some(process) = &mut self.agent.sessions[index].process
                && let Err(error) = process.flush_responses()
            {
                self.stop_agent_session(index, &error);
                save = true;
            }
            let session = &mut self.agent.sessions[index];
            let phase = session.phase;
            let entries = session.entries.len();
            session.send_next_prompt();
            save |= phase != session.phase || entries != session.entries.len();
            changed |= save;
            for _ in 0..MESSAGES_PER_TICK {
                let Some(message) = self.agent.sessions[index]
                    .process
                    .as_ref()
                    .and_then(AgentProcess::try_message)
                else {
                    break;
                };
                changed = true;
                save |= !matches!(message, AgentMessage::Log(_));
                self.handle_agent_message(index, message);
            }
            changed |= self.settle_subagent(index);
            if save {
                self.save_agent_history(index);
            }
        }
        if self.agent.asking.is_none()
            && self.input.prompt().is_none()
            && let Some(request) = self.agent.permissions.pop_front()
        {
            let name = self
                .agent
                .sessions
                .iter()
                .find(|session| session.number == request.session)
                .map_or_else(
                    || "The agent".to_owned(),
                    |session| format!("{} {}", session.number, session.name),
                );
            let title = request.title.clone();
            let action = request.action;
            let allow = if !request
                .options
                .iter()
                .any(|option| option.kind == "allow_once")
                && request
                    .options
                    .iter()
                    .any(|option| option.kind == "allow_always")
            {
                "Allow always"
            } else {
                "Allow"
            };
            self.agent.asking = Some(request);
            self.open_confirmation_actions(
                PromptKind::AgentPermission,
                format!("{name} wants to {action}"),
                title,
                ConfirmationChoice::No,
                allow,
                "Deny",
            );
            changed = true;
        }
        if changed {
            self.refresh_agent_transcript();
        } else if self.agent.sessions.iter().any(AgentSession::working) {
            self.sync_agent_chrome();
        }
    }

    /// Keep the tab's history file current; tabs nobody asked anything are not kept, and
    /// subagents belong to the conversation that started them.
    fn save_agent_history(&self, index: usize) {
        let session = &self.agent.sessions[index];
        if self.configuration.settings().persistence.enabled
            && session.parent.is_none()
            && session
                .entries
                .iter()
                .any(|entry| matches!(entry, Entry::User { .. }))
        {
            history::save(
                self.agent.directory.clone(),
                session.conversation(),
                session.entries.clone(),
            );
        }
    }

    pub(in crate::app) fn finish_agent_history(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        for index in 0..self.agent.sessions.len() {
            self.save_agent_history(index);
        }
        history::flush(&self.agent.directory, deadline)
    }

    /// Put a saved conversation in a tab, or show the tab it is already in.
    pub(in crate::app) fn reopen_conversation(&mut self, id: &str) -> Result<usize, String> {
        if let Some(index) = self
            .agent
            .sessions
            .iter()
            .position(|session| session.history == id)
        {
            self.agent.active = index;
            return Ok(index);
        }
        let (conversation, entries) = history::load(&self.agent.directory, id)
            .ok_or_else(|| "that conversation could not be read".to_owned())?;
        let index = self.add_agent_tab(
            conversation.agent,
            conversation.root,
            entries,
            conversation.session,
            Some(conversation.id),
        )?;
        self.agent.active = index;
        Ok(index)
    }

    pub(in crate::app) fn handle_agent_message(&mut self, index: usize, message: AgentMessage) {
        let session = &mut self.agent.sessions[index];
        match message {
            AgentMessage::Response { id, result } => {
                if let Some(position) = session
                    .requests
                    .iter()
                    .position(|(request, _)| *request == id)
                {
                    let (_, pending) = session.requests.remove(position);
                    match (pending, result) {
                        (
                            Pending::SetOption {
                                option,
                                value,
                                required,
                            },
                            Ok(result),
                        ) => {
                            if result["configOptions"].is_array() {
                                session.options = agent::session_options(&result);
                            } else if let Some(option) = session
                                .options
                                .iter_mut()
                                .find(|candidate| candidate.id == option)
                            {
                                option.current = value.clone();
                            }
                            if required
                                && !session
                                    .option(&option)
                                    .is_some_and(|option| option.current == value)
                            {
                                session.stop("the agent did not apply the requested model");
                            } else {
                                session.send_next_prompt();
                            }
                        }
                        (Pending::SetOption { required: true, .. }, Err(error)) => {
                            session.stop(&error)
                        }
                        (Pending::Authenticate, Ok(_)) => session.open_session(),
                        (_, Err(error)) => {
                            session.entries.push(Entry::Notice(error));
                            session.send_next_prompt();
                        }
                    }
                    return;
                }
                let signing_in = self.handle_agent_response(index, id, result);
                if signing_in && index == self.agent.active && self.ui.picker.is_none() {
                    self.open_agent_login_picker();
                }
            }
            AgentMessage::Notification { method, params } => {
                // `session/load` replays the conversation, which the tab already shows.
                if method == "session/update" {
                    apply_update(session, agent::session_update(&params));
                }
            }
            AgentMessage::Request { id, method, params } => match method.as_str() {
                "session/request_permission" | "fs/read_text_file" | "fs/write_text_file" => {
                    let number = session.number;
                    self.queue_agent_file(AgentFileRequest {
                        session: number,
                        id,
                        method,
                        params,
                        buffers: Vec::new(),
                        written: None,
                    });
                }
                _ => self.respond_to_agent(
                    index,
                    id,
                    Err(format!("{method} is not supported by Dust")),
                ),
            },
            AgentMessage::Log(line) => session.last_log = line,
            AgentMessage::Exited(reason) => {
                if let Some(line) = session.process.as_ref().and_then(AgentProcess::take_log) {
                    session.last_log = line;
                }
                let detail = if session.last_log.is_empty() {
                    reason
                } else {
                    format!("{reason}: {}", session.last_log)
                };
                session.entries.push(Entry::Notice(format!(
                    "{} stopped ({detail}).",
                    session.name
                )));
                session.turn_result = Some(Err(detail));
                session.process = None;
                session.phase = Phase::Stopped;
                session.queued.clear();
                session.requests.clear();
                let number = session.number;
                self.cancel_agent_permissions(&[number]);
                self.stop_agent_children(number);
            }
        }
    }

    /// The reply to the session's start or prompt requests. Returns whether the agent now
    /// waits for a sign-in.
    pub(in crate::app) fn handle_agent_response(
        &mut self,
        index: usize,
        id: u64,
        result: Result<Value, String>,
    ) -> bool {
        let session = &mut self.agent.sessions[index];
        match (session.phase, result) {
            (Phase::Initializing(expected), Ok(result)) if id == expected => {
                session.auth_methods = agent::auth_methods(&result);
                session.rejoin = agent::rejoin_support(&result);
                session.open_session();
            }
            (Phase::Rejoining(expected), Ok(result)) if id == expected => {
                session.id = session.earlier.take();
                session.options = agent::session_options(&result);
                session.phase = Phase::Ready;
                session.entries.push(Entry::Notice(format!(
                    "{} picked up its earlier session.",
                    session.name
                )));
                session.send_next_prompt();
            }
            (Phase::Rejoining(expected), Err(error)) if id == expected => {
                session.entries.push(Entry::Notice(format!(
                    "{} could not pick up its earlier session ({error}); it starts fresh with the conversation so far.",
                    session.name
                )));
                session.earlier = None;
                session.open_session();
                session.recap = true;
            }
            (Phase::Creating(expected), Ok(result)) if id == expected => {
                session.id = result["sessionId"].as_str().map(str::to_owned);
                session.options = agent::session_options(&result);
                session.phase = Phase::Ready;
                session.created = Some(std::time::Instant::now());
                if let Err(error) = session.choose_subagent_model() {
                    session.stop(&error);
                    return false;
                }
                session.apply_preferred();
                session.send_next_prompt();
            }
            (Phase::Creating(expected), Err(error))
                if id == expected
                    && !session.auth_methods.is_empty()
                    && error.to_lowercase().contains("auth") =>
            {
                session.phase = Phase::SigningIn;
                session.entries.push(Entry::Notice(format!(
                    "{} needs you to sign in. Pick a method, or ask again later with <leader>aa.",
                    session.name
                )));
                return true;
            }
            (Phase::Prompting(expected), Ok(result)) if id == expected => {
                session.turn_result = Some(match result["stopReason"].as_str() {
                    Some("end_turn") => Ok(()),
                    Some(reason) => Err(format!("agent turn ended: {reason}")),
                    None => Ok(()),
                });
                match result["stopReason"].as_str().unwrap_or_default() {
                    "cancelled" => session.entries.push(Entry::Notice("Stopped.".into())),
                    "refusal" => session
                        .entries
                        .push(Entry::Notice("The agent refused.".into())),
                    "max_tokens" | "max_turn_requests" => session
                        .entries
                        .push(Entry::Notice("The agent hit its turn limit.".into())),
                    _ => {}
                }
                session.phase = Phase::Ready;
                session.send_next_prompt();
            }
            (phase, Err(error)) => {
                // With no sign-in method to offer here, the agent's own CLI is the way in.
                let hint = (session.auth_methods.is_empty()
                    && error.to_lowercase().contains("auth"))
                .then(|| {
                    format!(
                        "Sign in with {}'s own command line, then ask again.",
                        session.name
                    )
                });
                session.turn_result = Some(Err(error.clone()));
                session.entries.push(Entry::Notice(error));
                session.entries.extend(hint.map(Entry::Notice));
                if matches!(phase, Phase::Prompting(expected) if expected == id) {
                    session.phase = Phase::Ready;
                    session.send_next_prompt();
                } else if matches!(phase, Phase::Initializing(_) | Phase::Creating(_)) {
                    session.process = None;
                    session.queued.clear();
                }
            }
            _ => {}
        }
        false
    }

    fn agent_buffer_paths(&self) -> Vec<(BufferId, PathBuf)> {
        self.state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| buffer.path.clone().map(|path| (id, path)))
            .collect()
    }

    /// Resolve paths on the document worker, including aliases of not-yet-created files.
    fn queue_agent_file(&mut self, mut request: AgentFileRequest) {
        let Some(index) = self
            .agent
            .sessions
            .iter()
            .position(|s| s.number == request.session && s.process.is_some())
        else {
            return;
        };
        let root = self.agent.sessions[index].root.clone();
        request.buffers = self.agent_buffer_paths();
        let mut paths: Vec<_> = request
            .buffers
            .iter()
            .map(|(_, path)| path.clone())
            .collect();
        if request.method == "session/request_permission" {
            paths.extend(
                agent::permission_changes(&request.params)
                    .iter()
                    .map(|path| root.join(path)),
            );
        } else {
            let Some(path) = request.params["path"].as_str().map(PathBuf::from) else {
                self.respond_to_agent(index, request.id, Err("path is missing".into()));
                return;
            };
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                self.respond_to_agent(
                    index,
                    request.id,
                    Err("agent file path must be absolute without parent components".into()),
                );
                return;
            }
            if request.method == "fs/write_text_file" && !request.params["content"].is_string() {
                self.respond_to_agent(index, request.id, Err("content is missing".into()));
                return;
            }
            paths.push(request.written.clone().unwrap_or(path));
            paths.push(root);
        }
        match self.documents.resolve_agent_paths(paths) {
            Ok(id) => {
                self.agent.files.insert(id, request);
            }
            Err(error) => self.respond_to_agent(index, request.id, Err(error.to_string())),
        }
    }

    pub(in crate::app) fn unsaved_agent_changes(
        &self,
        paths: &[PathBuf],
        buffers: &[(BufferId, PathBuf)],
    ) -> String {
        paths
            .iter()
            .filter(|path| {
                buffers.iter().any(|(id, resolved)| {
                    resolved == *path
                        && self
                            .state
                            .buffers
                            .get(*id)
                            .is_some_and(|buffer| buffer.dirty)
                })
            })
            .map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy();
                format!("! {name} has unsaved changes; this changes the saved copy\n")
            })
            .collect()
    }

    /// All disk work is asynchronous; buffer checks and updates stay on the editor thread.
    pub(in crate::app) fn accept_agent_file(
        &mut self,
        result: crate::files::VfsResult,
    ) -> Option<crate::files::VfsResult> {
        use crate::files::VfsResult;
        let Some(mut request) = self.agent.files.remove(&result.request_id()) else {
            return Some(result);
        };
        let index = self
            .agent
            .sessions
            .iter()
            .position(|s| s.number == request.session && s.process.is_some())?;
        let response = match result {
            VfsResult::PathsResolved { paths, .. } => {
                if request.buffers != self.agent_buffer_paths() {
                    self.queue_agent_file(request);
                    return None;
                }
                let (buffer_paths, targets) = paths.split_at(request.buffers.len());
                let resolved: Vec<_> = request.buffers.iter().zip(buffer_paths).map(|((id, _), path)| (*id, path.clone())).collect();
                if request.method == "session/request_permission" {
                    let unsaved = self.unsaved_agent_changes(targets, &resolved);
                    self.agent.permissions.push_back(PermissionRequest {
                        session: request.session, id: request.id,
                        action: agent::permission_action(&request.params),
                        title: format!("{unsaved}{}", agent::permission_detail(&request.params)),
                        options: agent::permission_options(&request.params),
                    });
                    return None;
                }
                let path = targets[0].clone();
                let open: Vec<_> = resolved.iter().filter(|(_, resolved)| *resolved == path).map(|(id, _)| *id).collect();
                if request.written.is_some() {
                    self.finish_agent_write(&request, &open)
                } else if request.method == "fs/read_text_file" {
                    let open = open.iter().find(|id| self.state.buffers[**id].dirty).or_else(|| open.first());
                    if let Some(id) = open {
                        let text = self.state.buffers[*id].text.to_string();
                        self.respond_to_agent(index, request.id, Ok(json!({"content": agent::slice_lines(&text, request.params["line"].as_u64(), request.params["limit"].as_u64())})));
                        return None;
                    }
                    match self.documents.read_scoped_bounded(path.clone(), vec![path], READ_BYTE_LIMIT) {
                        Ok(id) => { self.agent.files.insert(id, request); return None; }
                        Err(error) => Err(error.to_string()),
                    }
                } else if !path.starts_with(&targets[1]) {
                    Err(format!("{} is outside {}", path.display(), targets[1].display()))
                } else if open.iter().any(|id| self.state.buffers[*id].dirty) {
                    Err(format!("{} has unsaved changes in Dust", path.display()))
                } else {
                    let bytes = request.params["content"].as_str().expect("validated content").as_bytes().to_vec();
                    match self.documents.write_agent_file(path, targets[1].clone(), bytes) {
                        Ok(id) => { self.agent.files.insert(id, request); return None; }
                        Err(error) => Err(error.to_string()),
                    }
                }
            }
            VfsResult::Read { result, .. } => result.map_err(|e| e.to_string()).and_then(|bytes| {
                String::from_utf8(bytes).map_err(|e| e.to_string()).map(|text| json!({"content": agent::slice_lines(&text, request.params["line"].as_u64(), request.params["limit"].as_u64())}))
            }),
            VfsResult::Write { path, result, .. } => match result {
                Ok(()) => {
                    // Buffers may have opened or changed paths while the write ran.
                    // Resolve the current set before publishing and acknowledging it.
                    request.written = Some(path);
                    self.queue_agent_file(request);
                    return None;
                }
                Err(error) => Err(error.to_string()),
            },
            _ => Err("unexpected agent file result".into()),
        };
        self.respond_to_agent(index, request.id, response);
        None
    }

    fn finish_agent_write(
        &mut self,
        request: &AgentFileRequest,
        open: &[BufferId],
    ) -> Result<Value, String> {
        let mut candidate = self.state.draft();
        let text = TextBuffer::from_bytes(
            request.params["content"]
                .as_str()
                .expect("validated content")
                .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
        for id in open {
            let buffer = &self.state.buffers[*id];
            if buffer.dirty {
                return Err(
                    "file written, but newer unsaved changes in Dust were preserved".into(),
                );
            }
            candidate.reload_document(
                *id,
                text.clone(),
                buffer.path.clone().expect("resolved buffer path"),
            );
        }
        self.commit_editor_draft(candidate, None, false)
            .map_err(|e| e.to_string())?;
        self.git.forget_hunks();
        self.keep_cursor_off_final_newline();
        Ok(Value::Null)
    }

    pub(in crate::app) fn fail_agent_files(&mut self, error: &str) {
        let files = std::mem::take(&mut self.agent.files);
        for (_, request) in files {
            if let Some(index) = self
                .agent
                .sessions
                .iter()
                .position(|s| s.number == request.session)
            {
                self.respond_to_agent(index, request.id, Err(error.to_owned()));
            }
        }
    }

    pub(in crate::app) fn answer_agent_permission(&mut self, choice: ConfirmationChoice) {
        let request = self.agent.asking.take();
        self.close_prompt();
        let Some(request) = request else {
            return;
        };
        let Some(index) = self
            .agent
            .sessions
            .iter()
            .position(|session| session.number == request.session)
        else {
            return;
        };
        let wanted = if choice == ConfirmationChoice::Yes {
            "allow_once"
        } else {
            "reject_once"
        };
        let fallback = if choice == ConfirmationChoice::Yes {
            "allow_always"
        } else {
            "reject_always"
        };
        let option = request
            .options
            .iter()
            .find(|option| option.kind == wanted)
            .or_else(|| {
                request
                    .options
                    .iter()
                    .find(|option| option.kind == fallback)
            });
        let result = option.map_or_else(agent::permission_cancelled, |option| {
            agent::permission_selected(&option.id)
        });
        self.respond_to_agent(index, request.id, Ok(result));
    }

    pub(in crate::app) fn cancel_agent_permission(&mut self) {
        if let Some(request) = self.agent.asking.take()
            && let Some(index) = self
                .agent
                .sessions
                .iter()
                .position(|session| session.number == request.session)
        {
            self.respond_to_agent(index, request.id, Ok(agent::permission_cancelled()));
        }
    }

    pub(in crate::app) fn toggle_agent_panel(&mut self) {
        if self.configuration.settings().agents.connection
            == crate::config::AgentConnection::Terminal
        {
            // Like the panel, a shown agent hides whether or not it has the keys.
            let shown = self
                .terminal_session
                .side_terminals()
                .into_iter()
                .find(|id| self.agent_terminal(*id) && self.terminal_rect(*id).is_some());
            match shown {
                Some(id) => {
                    if let Err(error) = self.apply_native_window_placement(
                        crate::app::windows::NativeSurfaceTarget::Terminal(id.0),
                        crate::windows::WindowPlacement::Hidden,
                    ) {
                        self.show_error(error);
                    }
                }
                None => self.ask_terminal_agent(),
            }
            return;
        }
        if let Some(pane) = self.agent_pane() {
            let previous = self.active_tab_focus();
            if self.state.focus_pane_id(pane).is_ok() {
                // The only window cannot close: an empty buffer and the dashboard take its
                // place instead, as when the last file is closed.
                if self.state.is_last_pane() {
                    if let Err(error) = self.state.replace_active_view(None) {
                        self.show_error(error);
                        return;
                    }
                    self.set_dashboard_visibility(true);
                } else {
                    self.apply_layout_action(LayoutAction::ClosePane);
                }
            }
            if let Some(previous) = previous.filter(|previous| *previous != pane) {
                let _ = self.state.focus_pane_id(previous);
            }
            self.reconcile_editor_windows();
            self.ui.agent_panel = None;
            return;
        }
        self.show_agent_panel();
        self.start_default_agent();
        self.refresh_agent_transcript();
    }

    /// Show the panel in a split on its side and put the cursor in its input.
    pub(in crate::app) fn show_agent_panel(&mut self) {
        let previous = self.active_tab_focus();
        if self.agent_pane().is_none() {
            let pane_width = crate::ui::active_editor_pane_viewport(&self.state, &self.ui)
                .map(|viewport| u32::from(viewport.width));
            if self.state.active_view().is_some() {
                self.apply_layout_action(LayoutAction::Split(TileAxis::Horizontal));
            }
            let shown = match self
                .agent
                .buffer
                .filter(|id| self.state.buffers.contains_key(*id))
            {
                Some(buffer) => self.state.replace_active_view(Some(buffer)),
                None => self
                    .state
                    .open_virtual_read_only(
                        TextBuffer::from_text(""),
                        TRANSCRIPT_OWNER.into(),
                        0,
                        0,
                        1,
                    )
                    .map(|(buffer, _)| {
                        self.agent.buffer = Some(buffer);
                        self.state.kept_buffer = Some(buffer);
                    }),
            };
            if let Err(error) = shown {
                self.show_error(error);
            }
            // A plain split would give the panel half the screen; it opens narrower.
            if let (Some(code), Some(panel)) = (previous, self.active_tab_focus()) {
                let share = u32::from(self.configuration.settings().agents.panel_width) * 10;
                // A conversation needs about 40 columns, however narrow the terminal is.
                let readable =
                    pane_width.map_or(share, |width| (40 * 1_000 / width.max(1)).min(700));
                let share = share.max(readable) as u16;
                // The split's ratio is its first pane's share; on the left the panel is first.
                let left =
                    self.configuration.settings().agents.side == crate::config::PanelSide::Left;
                let _ = self.state.set_split_ratio_between(
                    code,
                    panel,
                    if left {
                        share
                    } else {
                        1_000u16.saturating_sub(share)
                    },
                );
                if left && code != panel {
                    let _ = self.state.swap_panes(code, panel);
                }
            }
        }
        let Some(pane) = self.agent_pane() else {
            return;
        };
        // Coming from another window starts typing, as in a chat box.
        if previous != Some(pane) {
            self.agent.normal = false;
        }
        // The dashboard can be up beside a panel that already exists, as after its last tab closed.
        self.set_dashboard_visibility(false);
        self.set_explorer_focus(false);
        let _ = self.state.focus_pane_id(pane);
        self.reconcile_editor_windows();
    }

    pub(in crate::app) fn agent_pane(&self) -> Option<crate::windows::WindowId> {
        let buffer = self.agent.buffer?;
        let shown = self.state.active_tab_views();
        shown
            .into_iter()
            .find(|view| {
                self.state
                    .views
                    .get(*view)
                    .is_some_and(|view| view.buffer == buffer)
            })
            .and_then(|view| self.state.view_window(view))
    }

    fn active_tab_focus(&self) -> Option<crate::windows::WindowId> {
        self.state.focused_window()
    }

    fn active_path(&self) -> Option<PathBuf> {
        let buffer = self.state.active_buffer_id()?;
        self.state.buffers.get(buffer)?.path.clone()
    }

    pub(in crate::app) fn agent_view(&self) -> Option<crate::editor::ViewId> {
        self.state.window_view(self.agent_pane()?)
    }

    /// Whether keys go to the panel's input.
    pub(in crate::app) fn agent_input_focused(&self) -> bool {
        self.ui
            .agent_panel
            .as_ref()
            .is_some_and(|panel| Some(panel.view) == self.state.active_view())
    }

    /// Build what the panel draws from the active session.
    pub(in crate::app) fn refresh_agent_transcript(&mut self) {
        let Some(view) = self.agent_view() else {
            self.ui.agent_panel = None;
            return;
        };
        let width = crate::ui::view_pane_viewport(&self.state, &self.ui, view)
            .map_or(80, |viewport| usize::from(viewport.width))
            .saturating_sub(2)
            .max(1);
        let same_width = self.agent.width == width;
        self.agent.width = width;
        let mut text = Styled {
            ambiguous_width: self.configuration.settings().editor.ambiguous_width,
            ..Styled::default()
        };
        let mut bands = Vec::new();
        let (status, settings) = match self.active_agent() {
            Some(session) => {
                transcript(
                    &mut text,
                    &mut bands,
                    &session.name,
                    &session.entries,
                    width,
                );
                let settings = ["model", "mode", THINKING]
                    .into_iter()
                    .filter_map(|key| Some(value_name(session.option(key)?)))
                    .collect::<Vec<_>>()
                    .join(" · ");
                (shown_root(&session.root, width / 2), settings)
            }
            None => {
                let agents = &self.configuration.settings().agents;
                let name = &agents.default;
                let ask = if name.is_empty() {
                    "Type below and pick the agent to ask".to_owned()
                } else {
                    format!("Type below to ask {name}, ctrl-n picks another agent")
                };
                text.wrapped(
                    &format!(
                        "No agent session yet. {ask}, ctrl-r resumes a saved conversation and / \
                         lists the commands."
                    ),
                    Some(Highlight::COMMENT),
                    ("", None),
                    ("", None),
                    width,
                );
                // Said here as well as in the notice, which fades, so a first ask is not lost.
                if let Some(missing) = agents
                    .profiles
                    .get(name)
                    .filter(|profile| self.agent_program(&profile.program).is_none())
                    .map(|profile| self.missing_agent_program(&profile.program))
                {
                    text.push("\n\n", None);
                    text.wrapped(
                        &missing,
                        Some(Highlight::QUOTE),
                        ("", None),
                        ("", None),
                        width,
                    );
                }
                (String::new(), String::new())
            }
        };
        let lines = crate::ui::transcript_lines(&text.text, &bands).len();
        if self.agent.scroll > 0 && same_width {
            self.agent.scroll = self
                .agent
                .scroll
                .saturating_add(lines.saturating_sub(self.agent.lines));
        }
        self.agent.lines = lines;
        self.ui.agent_panel = Some(AgentPanelUi {
            view,
            tabs: Vec::new(),
            transcript: text,
            bands,
            scroll: self.agent.scroll,
            context: Vec::new(),
            family: Vec::new(),
            menu: Vec::new(),
            selected: 0,
            normal: false,
            busy: String::new(),
            input: String::new(),
            cursor: 0,
            placeholder: String::new(),
            status,
            settings,
        });
        self.sync_agent_chrome();
        self.sync_agent_input();
    }

    /// The tab line, the subagent list and the spinner, which change without the
    /// transcript changing.
    fn sync_agent_chrome(&mut self) {
        let frames = SPINNER_FRAMES;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let frame =
            usize::try_from(millis / SPINNER_FRAME_MILLIS).unwrap_or(0) % frames.len().max(1);
        let spinner = frames.get(frame).copied().unwrap_or("…");
        let entry = |index: usize, title: String, closeable: bool| ContentTabEntry {
            identity: ContentTabIdentity(ContentId(index as u64)),
            kind: ContentTabKind::VirtualDocument,
            title,
            active: index == self.agent.active,
            dirty: false,
            pinned: false,
            diagnostic: None,
            icon: AGENT_ICON.into(),
            closeable,
            segments: None,
        };
        let mut tabs = self
            .agent
            .sessions
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let mark = match tab.status(self.agent_waiting(tab.number)) {
                    "working" | "starting" => format!(" {spinner}"),
                    "waiting for you" | "sign-in needed" => " ?".into(),
                    "stopped" => " ✗".into(),
                    _ => String::new(),
                };
                entry(index, format!("{}{mark}", tab.title()), true)
            })
            .collect::<Vec<_>>();
        if tabs.is_empty() {
            tabs.push(entry(0, "Agent".into(), false));
        }
        let (busy, family) = match self.active_agent() {
            Some(session) => {
                let busy = match (session.working(), session.phase) {
                    (false, _) => String::new(),
                    (true, Phase::Prompting(_)) if self.agent.normal => {
                        format!("{spinner} Working · esc to stop")
                    }
                    (true, Phase::Prompting(_)) => format!("{spinner} Working · esc esc to stop"),
                    (true, _) => format!("{spinner} Starting"),
                };
                let main = session.parent.unwrap_or(session.number);
                let members = self
                    .agent
                    .sessions
                    .iter()
                    .filter(|member| member.number == main || member.parent == Some(main))
                    .collect::<Vec<_>>();
                let family = if members.len() < 2 {
                    Vec::new()
                } else {
                    members
                        .into_iter()
                        .map(|member| AgentListRow {
                            shown: member.number == session.number,
                            name: if member.parent.is_some() {
                                member.name.clone()
                            } else {
                                "main".into()
                            },
                            task: member.task.clone(),
                            status: match (member.working(), member.parent, member.phase) {
                                (true, ..) => spinner.to_owned(),
                                (false, None, _) => String::new(),
                                (false, Some(_), Phase::Stopped) => "✗".into(),
                                (false, Some(_), _) => "✓".into(),
                            },
                        })
                        .collect()
                };
                (busy, family)
            }
            None => (String::new(), Vec::new()),
        };
        let Some(panel) = self.ui.agent_panel.as_mut() else {
            return;
        };
        if panel.tabs != tabs {
            panel.tabs = tabs;
        }
        if panel.busy != busy {
            panel.busy = busy;
        }
        if panel.family != family {
            panel.family = family;
        }
    }

    /// Slash commands matching a `/word` being typed: the panel's own, then the agent's.
    fn slash_menu(&self) -> Vec<(String, String)> {
        let Some(word) = self.agent.input.strip_prefix('/') else {
            return Vec::new();
        };
        if word.contains(char::is_whitespace) {
            return Vec::new();
        }
        let own = PANEL_COMMANDS
            .iter()
            .map(|(name, description)| ((*name).to_owned(), (*description).to_owned()));
        let theirs = self
            .active_agent()
            .map(|session| session.commands.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|(name, _)| PANEL_COMMANDS.iter().all(|(own, _)| own != name))
            .cloned();
        slash_matches(word, own.chain(theirs))
    }

    /// Run one of the panel's own slash commands; false for any other name.
    fn run_panel_command(&mut self, name: &str) -> bool {
        if PANEL_COMMANDS.iter().all(|(own, _)| *own != name) {
            return false;
        }
        self.agent.input.clear();
        self.agent.cursor = 0;
        self.sync_agent_input();
        match name {
            "new" => self.open_agent_picker(),
            "resume" => self.open_agent_resume_picker(),
            "model" => self.open_agent_option_picker(Some("model")),
            "mode" => self.open_agent_option_picker(Some("mode")),
            "thinking" => self.open_agent_option_picker(Some(THINKING)),
            "options" => self.open_agent_option_picker(None),
            _ => self.close_agent_session(),
        }
        true
    }

    /// Copy the input and its context to the panel; cheaper than a rebuild, for keystrokes.
    fn sync_agent_input(&mut self) {
        if self.agent.normal {
            let cursor = self.agent.cursor.min(self.agent.input.len());
            let start = self.agent.input[..cursor]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            if cursor > start
                && (cursor == self.agent.input.len()
                    || self.agent.input[cursor..].starts_with('\n'))
            {
                self.agent.cursor = PromptMove::Left.apply(&self.agent.input, cursor);
            }
        }
        let name = self.active_agent().map_or_else(
            || self.configuration.settings().agents.default.clone(),
            |session| session.name.clone(),
        );
        let Some(panel) = self.ui.agent_panel.as_mut() else {
            return;
        };
        panel.input.clone_from(&self.agent.input);
        panel.cursor = self.agent.cursor;
        panel.normal = self.agent.normal;
        panel.scroll = self.agent.scroll;
        panel.context = self.agent.context.iter().map(context_label).collect();
        panel.placeholder = if name.is_empty() {
            "Ask an agent…  / for commands".to_owned()
        } else {
            format!("Ask {name}…  / for commands")
        };
        let menu = self.slash_menu();
        self.agent.selected = self.agent.selected.min(menu.len().saturating_sub(1));
        let viewport = self
            .agent_view()
            .and_then(|view| crate::ui::view_pane_viewport(&self.state, &self.ui, view));
        if let Some(panel) = self.ui.agent_panel.as_mut() {
            panel.selected = self.agent.selected;
            panel.menu = menu;
            if let Some(viewport) = viewport {
                let height = panel.transcript_height(
                    self.configuration.settings(),
                    viewport.width,
                    viewport.height,
                );
                self.agent.scroll = self
                    .agent
                    .scroll
                    .min(self.agent.lines.saturating_sub(height));
            }
            panel.scroll = self.agent.scroll;
        }
    }

    /// The panel's width changed or it moved to another view: wrap the transcript again.
    pub(in crate::app) fn follow_agent_pane(&mut self) {
        let Some(view) = self.agent_view() else {
            if self.ui.agent_panel.is_some() {
                self.ui.agent_panel = None;
            }
            return;
        };
        let width = crate::ui::view_pane_viewport(&self.state, &self.ui, view)
            .map_or(80, |viewport| usize::from(viewport.width))
            .saturating_sub(2)
            .max(1);
        if self
            .ui
            .agent_panel
            .as_ref()
            .is_none_or(|panel| panel.view != view)
            || width != self.agent.width
        {
            self.refresh_agent_transcript();
        } else {
            self.sync_agent_input();
        }
    }

    pub(in crate::app) fn agent_input_insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.agent.input.insert_str(self.agent.cursor, &text);
        self.agent.cursor += text.len();
        self.sync_agent_input();
    }

    /// Editing keys in the input: `submit`, `newline`, `backspace`, `delete`, `left`, `right`,
    /// `word_left`, `word_right`, `home`, `end`, `up`, `down`, `delete_word`, `clear_line`, `clear` and `complete`, and
    /// the Vim ones in `agent_input_vim`. While the slash-command menu is open, up and down
    /// move in it, and `complete` and `submit` take the selected command.
    pub(in crate::app) fn agent_input_edit(&mut self, action: &str) {
        if self.agent_input_vim(action) {
            self.sync_agent_input();
            return;
        }
        let menu = self.slash_menu();
        if !menu.is_empty() && matches!(action, "up" | "down" | "complete" | "submit") {
            let count = menu.len();
            let selected = self.agent.selected.min(count - 1);
            let name = menu[selected].0.clone();
            match action {
                "up" => self.agent.selected = (selected + count - 1) % count,
                "down" => self.agent.selected = (selected + 1) % count,
                "complete" => {
                    self.agent.input = format!("/{name} ");
                    self.agent.cursor = self.agent.input.len();
                }
                _ => {
                    if !self.run_panel_command(&name) {
                        self.agent.input.clear();
                        self.agent.cursor = 0;
                        self.agent.scroll = 0;
                        self.agent_ask(format!("/{name}"));
                    }
                    return;
                }
            }
            self.sync_agent_input();
            return;
        }
        if action == "submit" {
            let typed = self.agent.input.trim().strip_prefix('/').map(str::to_owned);
            if typed.is_some_and(|name| self.run_panel_command(&name)) {
                return;
            }
        }
        let input = &mut self.agent.input;
        let cursor = self.agent.cursor.min(input.len());
        let line_start = input[..cursor].rfind('\n').map_or(0, |index| index + 1);
        let line_end = input[cursor..]
            .find('\n')
            .map_or(input.len(), |index| cursor + index);
        let previous = PromptMove::Left.apply(input, cursor);
        let next = PromptMove::Right.apply(input, cursor);
        let normal = self.agent.normal;
        self.agent.cursor = match action {
            "submit" => {
                let question = std::mem::take(input);
                self.agent.cursor = 0;
                self.agent.scroll = 0;
                self.agent_ask(question);
                return;
            }
            "newline" => {
                input.insert(cursor, '\n');
                cursor + 1
            }
            "backspace" if normal && cursor == line_start => cursor,
            "backspace" => {
                input.replace_range(previous..cursor, "");
                previous
            }
            "delete" => {
                if !normal || cursor < line_end {
                    input.replace_range(cursor..next, "");
                }
                cursor
            }
            "left" if normal => previous.max(line_start),
            "left" => previous,
            "right" if normal => next.min(line_end),
            "right" => next,
            "word_left" => PromptMove::WordLeft.apply(input, cursor),
            "word_right" => PromptMove::WordRight.apply(input, cursor),
            "home" => line_start,
            "end" => line_end,
            "delete_word" => {
                let start = PromptMove::WordLeft.apply(input, cursor);
                input.replace_range(start..cursor, "");
                start
            }
            "clear_line" => {
                input.replace_range(line_start..cursor, "");
                line_start
            }
            "clear" => {
                input.clear();
                0
            }
            "complete" => cursor,
            "up" if input.is_empty() => {
                // Up in an empty input brings back the last question.
                let last = self
                    .agent
                    .sessions
                    .get(self.agent.active)
                    .and_then(|session| {
                        session.entries.iter().rev().find_map(|entry| match entry {
                            Entry::User { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                    });
                *input = last.unwrap_or_default();
                input.len()
            }
            "up" | "down" => {
                let column = input[line_start..cursor].graphemes(true).count();
                let target = if action == "up" {
                    line_start
                        .checked_sub(1)
                        .map(|end| input[..end].rfind('\n').map_or(0, |index| index + 1))
                } else {
                    (line_end < input.len()).then_some(line_end + 1)
                };
                match target {
                    Some(start) => {
                        let end = input[start..]
                            .find('\n')
                            .map_or(input.len(), |index| start + index);
                        input[start..end]
                            .grapheme_indices(true)
                            .nth(column)
                            .map_or(end, |(index, _)| start + index)
                    }
                    None if action == "up" => 0,
                    None => input.len(),
                }
            }
            other => {
                self.show_error(format!("unknown agent input action {other}"));
                return;
            }
        };
        self.sync_agent_input();
    }

    /// Vim's mode changes, motions and line edits in the input. Returns false for any
    /// other action.
    fn agent_input_vim(&mut self, action: &str) -> bool {
        let menu_open = !self.slash_menu().is_empty();
        let input = &mut self.agent.input;
        let cursor = self.agent.cursor.min(input.len());
        let line_start = input[..cursor].rfind('\n').map_or(0, |index| index + 1);
        let line_end = input[cursor..]
            .find('\n')
            .map_or(input.len(), |index| cursor + index);
        let previous = PromptMove::Left.apply(input, cursor);
        let next = PromptMove::Right.apply(input, cursor);
        let (cursor, insert) = match action {
            "normal" if menu_open => {
                input.clear();
                (0, false)
            }
            // Leaving Insert steps back onto the last typed character, as in Vim.
            "normal" => (
                if cursor > line_start {
                    previous
                } else {
                    cursor
                },
                false,
            ),
            "insert" => (cursor, true),
            "append" => (if cursor < line_end { next } else { cursor }, true),
            "insert_home" => (
                line_start + input[line_start..line_end].len()
                    - input[line_start..line_end].trim_start().len(),
                true,
            ),
            "append_end" => (line_end, true),
            "open_below" => {
                input.insert(line_end, '\n');
                (line_end + 1, true)
            }
            "open_above" => {
                input.insert(line_start, '\n');
                (line_start, true)
            }
            "slash" => {
                input.insert(cursor, '/');
                (cursor + 1, true)
            }
            "word_forward" => (PromptMove::WordRight.apply(input, cursor), false),
            "word_back" => (PromptMove::WordLeft.apply(input, cursor), false),
            "word_end" => {
                let start = input[next..]
                    .grapheme_indices(true)
                    .find(|(_, grapheme)| !grapheme.chars().all(char::is_whitespace))
                    .map_or(input.len(), |(index, _)| next + index);
                let end = PromptMove::WordRight.apply(input, start);
                let end = input[..end].trim_end().len();
                (PromptMove::Left.apply(input, end).max(cursor), false)
            }
            "delete_line" => {
                let (start, end) = if line_end < input.len() {
                    (line_start, line_end + 1)
                } else {
                    (line_start.saturating_sub(1), line_end)
                };
                input.replace_range(start..end, "");
                (start.min(input.len()), false)
            }
            "delete_to_end" | "change_to_end" => {
                input.replace_range(cursor..line_end, "");
                (cursor, action == "change_to_end")
            }
            "change_line" => {
                input.replace_range(line_start..line_end, "");
                (line_start, true)
            }
            _ => return false,
        };
        self.agent.cursor = cursor;
        self.agent.normal = !insert;
        true
    }

    /// Scroll the transcript; positive is up, toward older output.
    pub(in crate::app) fn scroll_agent(&mut self, rows: i64) {
        let rows = isize::try_from(rows).unwrap_or(0);
        self.agent.scroll = self.agent.scroll.saturating_add_signed(rows);
        self.sync_agent_input();
    }

    /// Escape in Normal mode: close the slash-command menu, or stop a turn that is running.
    /// Insert mode's Escape goes to Normal; `ctrl-h/l` leave the panel like any window.
    pub(in crate::app) fn agent_escape(&mut self) {
        if !self.slash_menu().is_empty() {
            self.agent.input.clear();
            self.agent.cursor = 0;
            self.sync_agent_input();
            return;
        }
        if self
            .active_agent()
            .is_some_and(|session| matches!(session.phase, Phase::Prompting(_)))
        {
            self.cancel_agent_turn();
        }
    }

    /// Whether the input is in Normal mode, for the `agent_normal` key context.
    pub(in crate::app) fn agent_normal_mode(&self) -> bool {
        self.agent.normal
    }

    /// The wheel scrolls the transcript, a click focuses the input, and a click on the tab
    /// line shows that session. Returns whether the event was over the panel.
    pub(in crate::app) fn agent_panel_mouse(
        &mut self,
        key: &crate::editor::input::KeyEvent,
        column: u16,
        row: u16,
    ) -> bool {
        use crate::editor::input::TerminalMouseInput;
        let Some(view) = self.ui.agent_panel.as_ref().map(|panel| panel.view) else {
            return false;
        };
        let Some(viewport) = crate::ui::editor_pane_viewport_at(&self.state, &self.ui, column, row)
            .filter(|viewport| viewport.view == view)
        else {
            return false;
        };
        match key.terminal_mouse() {
            Some(TerminalMouseInput::ScrollUp) => self.scroll_agent(3),
            Some(TerminalMouseInput::ScrollDown) => self.scroll_agent(-3),
            Some(input @ (TerminalMouseInput::LeftDown | TerminalMouseInput::MiddleDown)) => {
                self.show_agent_panel();
                let tab = (row == viewport.y)
                    .then(|| {
                        let panel = self.ui.agent_panel.as_ref()?;
                        crate::ui::content_tab_at_column(
                            &panel.tabs,
                            self.configuration.settings(),
                            viewport.width,
                            column.saturating_sub(viewport.x),
                        )
                    })
                    .flatten()
                    .map(|ContentTabIdentity(ContentId(index))| index as usize)
                    .filter(|index| *index < self.agent.sessions.len());
                if let Some(index) = tab {
                    // A middle click closes the tab, as on the buffer line.
                    self.agent.active = index;
                    self.agent.scroll = 0;
                    if matches!(input, TerminalMouseInput::MiddleDown) {
                        self.close_agent_session();
                    }
                }
                self.refresh_agent_transcript();
            }
            _ => {}
        }
        true
    }
}

/// How long ago, roughly.
fn age(seconds: u64) -> String {
    match seconds {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// The profile's `add_tools` command with `{dust}` replaced by this Dust program, or `None`
/// when it has none.
fn add_tools_command(
    profile: &crate::config::AgentProfileConfig,
) -> Option<Vec<std::ffi::OsString>> {
    if profile.add_tools.is_empty() {
        return None;
    }
    let dust = std::env::current_exe().map_or_else(|_| "dust".into(), PathBuf::into_os_string);
    Some(
        profile
            .add_tools
            .iter()
            .map(|word| match word.as_str() {
                "{dust}" => dust.clone(),
                word => word.into(),
            })
            .collect(),
    )
}

fn picker_row(id: String, label: String, detail: String, badge: String) -> PickerItem {
    PickerItem {
        badge,
        kind: PickerItemKind::Entry,
        id: PickerItemId(id),
        label,
        detail,
        source: String::new(),
        action: PickerAction::None,
    }
}

/// The display name of an option's current value.
fn value_name(option: &SessionOption) -> String {
    option
        .values
        .iter()
        .find(|value| value.value == option.current)
        .map_or_else(|| option.current.clone(), |value| value.name.clone())
}

fn apply_update(session: &mut AgentSession, update: SessionUpdate) {
    if matches!(session.phase, Phase::Rejoining(_))
        && !matches!(
            update,
            SessionUpdate::Options(_) | SessionUpdate::Mode(_) | SessionUpdate::Commands(_)
        )
    {
        return;
    }
    let entries = &mut session.entries;
    match update {
        SessionUpdate::AgentText(text) => {
            if let Some(Entry::Agent(current)) = entries.last_mut() {
                current.push_str(&text);
            } else {
                entries.push(Entry::Agent(text));
            }
        }
        SessionUpdate::Thought(text) => {
            if let Some(Entry::Thought(current)) = entries.last_mut() {
                current.push_str(&text);
            } else {
                entries.push(Entry::Thought(text));
            }
        }
        SessionUpdate::ToolCall { id, title, status } => {
            entries.push(Entry::Tool { id, title, status });
        }
        SessionUpdate::ToolCallUpdate { id, title, status } => {
            let tool = entries.iter_mut().rev().find_map(|entry| match entry {
                Entry::Tool {
                    id: candidate,
                    title,
                    status,
                } if *candidate == id => Some((title, status)),
                _ => None,
            });
            if let Some((current_title, current_status)) = tool {
                if let Some(title) = title {
                    *current_title = title;
                }
                if let Some(status) = status {
                    *current_status = status;
                }
            }
        }
        SessionUpdate::Plan(steps) => {
            if let Some(Entry::Plan(current)) = entries
                .iter_mut()
                .rev()
                .take_while(|entry| !matches!(entry, Entry::User { .. }))
                .find(|entry| matches!(entry, Entry::Plan(_)))
            {
                *current = steps;
            } else {
                entries.push(Entry::Plan(steps));
            }
        }
        SessionUpdate::Options(options) => session.options = options,
        SessionUpdate::Mode(mode) => {
            if let Some(option) = session
                .options
                .iter_mut()
                .find(|option| option.category == "mode")
            {
                option.current = mode;
            }
        }
        SessionUpdate::Commands(commands) => session.commands = commands,
        SessionUpdate::Other => {}
    }
}

fn context_label(part: &PromptPart) -> String {
    match part {
        PromptPart::File { path, text: None } => file_name(path),
        PromptPart::File {
            path,
            text: Some(text),
        } => format!(
            "{} ({} lines selected)",
            file_name(path),
            text.lines().count()
        ),
        PromptPart::Text(text) => text.lines().next().unwrap_or_default().to_owned(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn selected_text(
    text: &TextBuffer,
    view: &crate::editor::View,
    policy: crate::editor::EditingPolicy,
) -> String {
    crate::editor::visual_ranges(text, view, policy)
        .map(|(ranges, shape)| {
            ranges
                .into_iter()
                .map(|range| text.slice(range).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(if shape == crate::editor::RegisterShape::Block {
                    "\n"
                } else {
                    ""
                })
        })
        .unwrap_or_default()
}

/// The conversation, with the Markdown in questions and answers rendered and wrapped to
/// `width`. The byte ranges of the user's messages go to `bands`.
fn transcript(
    out: &mut Styled,
    bands: &mut Vec<std::ops::Range<usize>>,
    name: &str,
    entries: &[Entry],
    width: usize,
) {
    let plain = ("", None);
    let quote = Some(Highlight::QUOTE);
    for (index, entry) in entries.iter().enumerate() {
        // A blank line between tool lines and the prose around them.
        let previous = index.checked_sub(1).map(|previous| &entries[previous]);
        if matches!(
            (previous, entry),
            (Some(Entry::Agent(_)), Entry::Tool { .. })
                | (Some(Entry::Tool { .. }), Entry::Agent(_))
        ) {
            out.push("\n", None);
        }
        match entry {
            // The first start is described by the footer; later ones are restarts.
            Entry::Started { root } => {
                if index > 0 {
                    out.push("\n", None);
                    let text = format!("{name} restarted in {}.", shown_root(root, width));
                    out.wrapped(&text, quote, plain, plain, width);
                }
            }
            Entry::User { text, context, .. } => {
                if !out.text.is_empty() {
                    out.push("\n", None);
                }
                let start = out.text.len();
                out.push("\n", None);
                markdown::render(out, text, width);
                for label in context {
                    out.wrapped(&format!("+ {label}"), None, plain, plain, width);
                }
                out.push("\n", None);
                bands.push(start..out.text.len());
                out.push("\n", None);
            }
            Entry::Agent(text) => markdown::render(out, text, width),
            Entry::Thought(text) => {
                // Only the thought still being written is shown; finished ones are noise.
                if index + 1 == entries.len() {
                    let bar = ("▎ ", quote);
                    out.wrapped(text, quote, bar, bar, width);
                }
            }
            // A tool call can start without a title; it shows once it has one.
            Entry::Tool { title, .. } if title.trim().is_empty() => {}
            Entry::Tool { title, status, .. } => {
                let role = match status.as_str() {
                    "completed" => Highlight::STRING,
                    "failed" => Highlight::NUMBER,
                    "in_progress" => Highlight::TYPE,
                    _ => Highlight::COMMENT,
                };
                out.wrapped(title, None, ("● ", Some(role)), ("  ", None), width);
            }
            Entry::Plan(steps) => {
                out.push("\n", None);
                out.push("Plan", Some(Highlight::STRONG));
                out.push("\n", None);
                for (status, step) in steps {
                    let mark = match status.as_str() {
                        "completed" => "✓ ",
                        "in_progress" => "… ",
                        _ => "○ ",
                    };
                    out.wrapped(
                        step,
                        None,
                        (mark, Some(Highlight::KEYWORD)),
                        ("  ", None),
                        width,
                    );
                }
                out.push("\n", None);
            }
            Entry::Notice(text) => {
                out.push("\n", None);
                out.wrapped(text, quote, plain, plain, width);
            }
        }
    }
}

/// The session's directory with the home directory as `~`, cut from the left to fit.
fn shown_root(root: &Path, width: usize) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shown = match home
        .as_deref()
        .and_then(|home| root.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => root.display().to_string(),
    };
    let width = width.saturating_sub(2).max(8);
    let count = shown.chars().count();
    if count <= width {
        shown
    } else {
        format!(
            "…{}",
            shown.chars().skip(count + 1 - width).collect::<String>()
        )
    }
}

/// The commands starting with `word`, in order, but the one named exactly `word` first: `/mode`
/// typed in full is mode, not model.
fn slash_matches(
    word: &str,
    commands: impl Iterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let mut matches: Vec<_> = commands
        .filter(|(name, _)| name.starts_with(word))
        .collect();
    matches.sort_by_key(|(name, _)| name != word);
    matches
}

pub(in crate::app) mod commands;
#[cfg(test)]
mod tests;

use crate::agent;
use crate::ui::markdown;
