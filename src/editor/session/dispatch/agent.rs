//! The agent panel: ACP sessions as tabs. Each session has its own agent process, transcript,
//! options (model, mode, thinking level) and permission requests. The panel is a split beside
//! the code with its own tab line, the active session's conversation with its Markdown
//! rendered, an input box and a footer; `ui::render::agent` draws it in place of the pane's
//! placeholder buffer. Questions go to the active session. A session keeps the worktree it
//! started in; switching worktrees does not move it.

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use super::InteractiveApplication;
use crate::{
    agent::{
        self, AgentMessage, AgentProcess, AuthMethod, PermissionOption, PromptPart, Rejoin,
        SessionOption, SessionUpdate,
        history::{self, Conversation, Entry},
    },
    editor::{
        Axis, BufferId, EditorMode, LayoutAction, session::search::PickerPayload,
        text::TextBuffer,
    },
    language::syntax::SyntaxRole,
    tools::{ToolKind, ToolStatus},
    ui::{
        AgentPanelUi, ConfirmationChoice, PromptKind,
        markdown::{self, Styled},
    },
    workspace::{PickerAction, PickerItem, PickerItemId, PickerItemKind},
};

/// The owner of the placeholder buffer the panel's pane holds.
pub(in crate::editor::session) const TRANSCRIPT_OWNER: &str = "Agent";
/// Messages handled per session and tick, so a chatty agent cannot stall input.
const MESSAGES_PER_TICK: usize = 256;
/// Files larger than this are not read for the agent.
const READ_BYTE_CEILING: u64 = 16 * 1024 * 1024;
/// The option category ACP uses for reasoning effort.
const THINKING: &str = "thought_level";
/// How much of a saved conversation goes to an agent that starts fresh, in characters.
const RECAP_CHARACTERS: usize = 60_000;

/// What a row of one of the agent pickers stands for.
#[derive(Clone, Debug)]
pub(in crate::editor::session) enum AgentChoice {
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
    sessions: Vec<AgentSession>,
    /// Index of the session the panel shows and questions go to.
    active: usize,
    /// Tab numbers are never reused, so a late permission answer cannot reach the wrong tab.
    started: u64,
    buffer: Option<BufferId>,
    /// Files, selections and diagnostics to send with the next question.
    context: Vec<PromptPart>,
    /// Permission requests waiting for the prompt line to be free.
    permissions: VecDeque<PermissionRequest>,
    /// The request the open permission prompt answers.
    asking: Option<PermissionRequest>,
    /// Whether the tabs open when Dust last quit were brought back.
    restored: bool,
    /// The question being typed, and the cursor in it as a byte offset.
    input: String,
    cursor: usize,
    /// Transcript rows scrolled up from the end, and how many rows there are.
    scroll: usize,
    lines: usize,
    /// The width the transcript was wrapped to.
    width: usize,
    /// The pane Escape goes back to.
    back: Option<crate::editor::PaneId>,
}

struct AgentSession {
    number: u64,
    name: String,
    root: PathBuf,
    /// `None` once the agent exited; the transcript stays readable and the next question
    /// starts the agent again in the same tab.
    process: Option<AgentProcess>,
    phase: Phase,
    id: Option<String>,
    /// Questions asked before the session was ready, sent in order.
    queued: VecDeque<Vec<PromptPart>>,
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
}

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
    /// The agent exited; a tab restored from history has no process but is not stopped.
    Stopped,
}

enum Pending {
    SetOption { option: String, value: String },
    Authenticate,
}

struct PermissionRequest {
    session: u64,
    id: Value,
    title: String,
    options: Vec<PermissionOption>,
}

impl AgentSession {
    fn status(&self, waiting: bool) -> &'static str {
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

    fn option(&self, key: &str) -> Option<&SessionOption> {
        self.options
            .iter()
            .find(|option| option.id == key)
            .or_else(|| self.options.iter().find(|option| option.category == key))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        self.process
            .as_mut()
            .ok_or_else(|| format!("{} is not running", self.name))?
            .request(method, params)
    }

    fn respond(&mut self, id: Value, result: Result<Value, String>) {
        if let Some(process) = &mut self.process {
            let _ = process.respond(id, result);
        }
    }

    fn send_next_prompt(&mut self) {
        let Some(id) = self.id.clone() else {
            return;
        };
        let Some(mut parts) = self.queued.pop_front() else {
            return;
        };
        if std::mem::take(&mut self.recap) {
            let before = self
                .entries
                .iter()
                .rposition(|entry| matches!(entry, Entry::User { .. }))
                .unwrap_or(self.entries.len());
            parts.insert(0, PromptPart::Text(history::recap(&self.entries[..before], RECAP_CHARACTERS)));
        }
        match self.request("session/prompt", agent::prompt_params(&id, &parts)) {
            Ok(request) => self.phase = Phase::Prompting(request),
            Err(error) => self.entries.push(Entry::Notice(error)),
        }
    }

    fn set_option(&mut self, key: &str, value: &str) -> Result<(), String> {
        let session = self.id.clone().ok_or("the session is still starting")?;
        let option = self
            .option(key)
            .ok_or_else(|| format!("{} has no option `{key}`", self.name))?
            .clone();
        if !option.values.iter().any(|candidate| candidate.value == value) {
            return Err(format!("{} has no {} `{value}`", self.name, option.name));
        }
        let (method, params) = agent::set_option_request(&session, &option, value);
        let id = self.request(method, params)?;
        self.requests.push((
            id,
            Pending::SetOption {
                option: option.id,
                value: value.to_owned(),
            },
        ));
        Ok(())
    }

    /// Ask for a session: the earlier one if the agent can pick it up, otherwise a new one
    /// that gets the conversation so far with the next question.
    fn open_session(&mut self) {
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
            Some(earlier) => agent::rejoin_params(earlier, &self.root),
            None => agent::new_session_params(&self.root),
        };
        match self.request(method, params) {
            Ok(id) if method == "session/new" => self.phase = Phase::Creating(id),
            Ok(id) => self.phase = Phase::Rejoining(id),
            Err(error) => self.entries.push(Entry::Notice(error)),
        }
    }

    fn attach(&mut self, (process, phase, preferred): (AgentProcess, Phase, BTreeMap<String, String>)) {
        self.entries.push(Entry::Started { root: self.root.clone() });
        self.earlier = self.id.take().or_else(|| self.earlier.take());
        self.process = Some(process);
        self.phase = phase;
        self.options.clear();
        self.requests.clear();
        self.preferred = preferred;
        self.recap = false;
    }

    fn conversation(&self, open: bool) -> Conversation {
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
            open,
        }
    }

    /// Profile preferences such as `model = "…"`, applied once the session exists.
    fn apply_preferred(&mut self) {
        for (key, value) in std::mem::take(&mut self.preferred) {
            if self.option(&key).is_some_and(|option| option.current != value)
                && let Err(error) = self.set_option(&key, &value)
            {
                self.entries.push(Entry::Notice(error));
            }
        }
    }
}

impl InteractiveApplication {
    fn active_agent(&self) -> Option<&AgentSession> {
        self.agent.sessions.get(self.agent.active)
    }

    fn active_agent_mut(&mut self) -> Option<&mut AgentSession> {
        self.agent.sessions.get_mut(self.agent.active)
    }

    /// `<leader>aa`: in Visual mode the selection goes along with the question.
    pub(super) fn begin_agent_question(&mut self) {
        if let Some(view) = self.state.active_view
            && let Some(selection) = self.state.views[view].selection
        {
            let buffer = &self.state.buffers[self.state.views[view].buffer];
            let text = selected_text(&buffer.text, selection, self.state.views[view].cursor);
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
        self.refresh_agent_transcript();
    }

    pub(super) fn attach_agent_file(&mut self) {
        let Some(path) = self.active_path() else {
            self.show_error("the current buffer has no file");
            return;
        };
        self.agent.context.push(PromptPart::File { path, text: None });
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    pub(super) fn attach_agent_diagnostics(&mut self) {
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
                let line = text.byte_to_line(problem.range.start).map_or(0, |line| line + 1);
                format!("- line {line}: {:?}: {}", problem.severity, problem.message)
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            self.notify_message("no diagnostics in this file".into(), false);
            return;
        }
        self.agent.context.push(PromptPart::File { path: path.clone(), text: None });
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

    pub(super) fn agent_ask(&mut self, question: String) {
        self.restore_agent_tabs();
        let question = question.trim().to_owned();
        if question.is_empty() {
            self.agent.context.clear();
            return;
        }
        if self.agent.sessions.is_empty() {
            let name = self.configuration.settings().agents.default.clone();
            if let Err(error) = self.start_agent_session(&name) {
                self.show_error(error);
                return;
            }
        } else if self.active_agent().is_some_and(|session| session.process.is_none())
            && let Err(error) = self.restart_active_agent()
        {
            self.show_error(error);
            return;
        }
        let mut parts = std::mem::take(&mut self.agent.context);
        let context = parts.iter().map(context_label).collect();
        parts.push(PromptPart::Text(question.clone()));
        let signing_in = if let Some(session) = self.active_agent_mut() {
            session.entries.push(Entry::User {
                text: question,
                context,
            });
            session.queued.push_back(parts);
            if session.phase == Phase::Ready {
                session.send_next_prompt();
            }
            session.phase == Phase::SigningIn
        } else {
            false
        };
        self.show_agent_panel();
        self.refresh_agent_transcript();
        if signing_in {
            self.open_agent_login_picker();
        }
    }

    pub(super) fn cancel_agent_turn(&mut self) {
        let Some(session) = self.agent.sessions.get_mut(self.agent.active) else {
            self.notify_message("no agent session".into(), false);
            return;
        };
        session.queued.clear();
        let (Phase::Prompting(_), Some(id)) = (session.phase, session.id.clone()) else {
            return;
        };
        if let Some(process) = &mut session.process {
            let _ = process.notify("session/cancel", json!({"sessionId": id}));
        }
        // The protocol asks the client to answer open permission requests as cancelled.
        let number = session.number;
        let (cancelled, kept): (Vec<_>, Vec<_>) = self
            .agent
            .permissions
            .drain(..)
            .partition(|request| request.session == number);
        self.agent.permissions = kept.into();
        let asking = self.agent.asking.take_if(|request| request.session == number);
        let prompt_open = asking.is_some();
        for request in cancelled.into_iter().chain(asking) {
            self.agent.sessions[self.agent.active]
                .respond(request.id, Ok(agent::permission_cancelled()));
        }
        if prompt_open {
            self.close_prompt();
        }
        self.notify_message("stopping the agent…".into(), false);
    }

    /// `<leader>an`: pick an agent to start in a new tab.
    pub(super) fn open_agent_picker(&mut self) {
        let agents = &self.configuration.settings().agents;
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
        self.search
            .set_picker_footer("Enter Start in a new tab · Esc Close".to_owned());
        self.sync_picker_ui();
    }

    /// `<leader>as`: the open sessions.
    pub(super) fn open_agent_sessions_picker(&mut self) {
        self.restore_agent_tabs();
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
                        format!("{}{model} · {}", session.status(waiting), session.root.display()),
                        if index == self.agent.active { "shown".into() } else { String::new() },
                    ),
                    PickerPayload::Agent(AgentChoice::Session(session.number)),
                )
            })
            .collect();
        self.install_picker("Agent Sessions".to_owned(), items);
        self.search.set_picker_footer("Enter Show · Esc Close".to_owned());
        self.sync_picker_ui();
    }

    /// `<leader>am`, `<leader>aM`, `<leader>ar` and `<leader>ao`: the active session's
    /// model, mode, thinking level, or all of its options.
    pub(super) fn open_agent_option_picker(&mut self, key: Option<&str>) {
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
            self.search.set_picker_footer("Enter Change · Esc Close".to_owned());
            self.sync_picker_ui();
            return;
        };
        let Some(option) = session.option(key).cloned() else {
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
                        if value.value == option.current { "current".into() } else { String::new() },
                    ),
                    PickerPayload::Agent(AgentChoice::Value {
                        option: option.id.clone(),
                        value: value.value.clone(),
                    }),
                )
            })
            .collect();
        self.install_picker(format!("{} {}", session.name, option.name), items);
        self.search.set_picker_footer("Enter Use · Esc Close".to_owned());
        self.sync_picker_ui();
    }

    /// `<leader>aR`: saved conversations, newest first.
    pub(super) fn open_agent_resume_picker(&mut self) {
        self.restore_agent_tabs();
        let saved = history::list(&history::directory());
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
                let open = self.agent.sessions.iter().any(|session| session.history == conversation.id);
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
            .set_picker_footer("Enter Resume in a tab · Esc Close".to_owned());
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
        self.search.set_picker_footer("Enter Sign in · Esc Close".to_owned());
        self.sync_picker_ui();
    }

    pub(super) fn accept_agent_choice(&mut self, choice: AgentChoice) {
        let result = match choice {
            AgentChoice::Profile(name) => self.start_agent_session(&name).map(|()| {
                self.show_agent_panel();
            }),
            AgentChoice::Session(number) => {
                if let Some(index) = self.agent.sessions.iter().position(|s| s.number == number) {
                    self.agent.active = index;
                    self.agent.scroll = 0;
                    self.show_agent_panel();
                }
                Ok(())
            }
            AgentChoice::Resume(id) => match self.reopen_conversation(&id) {
                Some(index) if self.agent.sessions[index].process.is_none() => {
                    self.show_agent_panel();
                    self.restart_active_agent()
                }
                Some(_) => {
                    self.show_agent_panel();
                    Ok(())
                }
                None => Err("that conversation could not be read".to_owned()),
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
    pub(super) fn cycle_agent_session(&mut self, offset: isize) {
        self.restore_agent_tabs();
        let count = self.agent.sessions.len();
        if count == 0 {
            self.notify_message("no agent sessions; start one with <leader>an".into(), false);
            return;
        }
        self.agent.active = (self.agent.active as isize + offset).rem_euclid(count as isize) as usize;
        self.agent.scroll = 0;
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    /// `<leader>ax`: stop the active session and close its tab.
    pub(super) fn close_agent_session(&mut self) {
        if self.agent.sessions.is_empty() {
            self.notify_message("no agent session".into(), false);
            return;
        }
        // Closed tabs stay in the history and can be resumed with <leader>aR.
        self.save_agent_history(self.agent.active, false);
        let session = self.agent.sessions.remove(self.agent.active);
        self.agent.permissions.retain(|request| request.session != session.number);
        if self.agent.asking.take_if(|request| request.session == session.number).is_some() {
            self.close_prompt();
        }
        self.agent.active = self.agent.active.min(self.agent.sessions.len().saturating_sub(1));
        drop(session);
        if self.agent.sessions.is_empty() && self.agent_pane().is_some() {
            self.toggle_agent_panel();
        }
        self.refresh_agent_transcript();
    }

    /// Starting an installed adapter from the tool manager opens a session of the profile
    /// that runs it.
    pub(super) fn start_agent_for_program(&mut self, executable: &str) {
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

    fn start_agent_session(&mut self, name: &str) -> Result<(), String> {
        let root = self.state.workspace_root.clone();
        let spawned = self.spawn_agent(name, &root)?;
        let index = self.add_agent_tab(name.to_owned(), root, Vec::new(), None, None);
        self.agent.active = index;
        self.agent.sessions[index].attach(spawned);
        Ok(())
    }

    /// A stopped tab; its agent starts with `attach`.
    fn add_agent_tab(
        &mut self,
        name: String,
        root: PathBuf,
        entries: Vec<Entry>,
        earlier: Option<String>,
        history: Option<String>,
    ) -> usize {
        self.agent.started += 1;
        let number = self.agent.started;
        let history = history.unwrap_or_else(|| {
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis());
            format!("{millis}-{}-{number}", std::process::id())
        });
        self.agent.sessions.push(AgentSession {
            number,
            name,
            root,
            process: None,
            phase: Phase::Ready,
            id: None,
            queued: VecDeque::new(),
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
        });
        self.agent.sessions.len() - 1
    }

    /// Start the active tab's agent again after it exited, in the tab's own worktree. It
    /// picks up the tab's earlier session when it can.
    fn restart_active_agent(&mut self) -> Result<(), String> {
        let Some(session) = self.active_agent() else {
            return Ok(());
        };
        let (name, root) = (session.name.clone(), session.root.clone());
        let spawned = self.spawn_agent(&name, &root)?;
        self.agent.sessions[self.agent.active].attach(spawned);
        Ok(())
    }

    fn spawn_agent(
        &self,
        name: &str,
        root: &Path,
    ) -> Result<(AgentProcess, Phase, BTreeMap<String, String>), String> {
        let profile = self
            .configuration
            .settings()
            .agents
            .profiles
            .get(name)
            .ok_or_else(|| format!("no agent named `{name}` in [agents.profiles]"))?;
        let Some(program) = self.agent_program(&profile.program) else {
            let installable = self.tooling.entries().iter().any(|entry| {
                entry.definition.kind == ToolKind::Agent
                    && Path::new(&entry.definition.executable) == profile.program
            });
            return Err(if installable {
                format!(
                    "{} is not installed; install it in the tool manager (<leader>cm, Agent)",
                    profile.program.display()
                )
            } else {
                format!("{} is not installed", profile.program.display())
            });
        };
        let mut process =
            AgentProcess::spawn(&program, &profile.arguments, &profile.environment, root)
                .map_err(|error| format!("could not start agent {name}: {error}"))?;
        let id = process.request("initialize", agent::initialize_params())?;
        Ok((process, Phase::Initializing(id), profile.options.clone()))
    }

    /// The program to run: a path as given, a Dust-installed adapter, or one on `PATH`.
    fn agent_program(&self, program: &Path) -> Option<PathBuf> {
        if program.components().count() > 1 {
            return program.is_file().then(|| program.to_path_buf());
        }
        let installed = self.tooling.entries().iter().find_map(|entry| match &entry.status {
            ToolStatus::Installed(path)
                if entry.definition.kind == ToolKind::Agent
                    && Path::new(&entry.definition.executable) == program =>
            {
                Some(path.clone())
            }
            _ => None,
        });
        installed.or_else(|| self.tooling.path_executable(&program.to_string_lossy()).map(|(path, _)| path))
    }

    fn agent_waiting(&self, number: u64) -> bool {
        self.agent.asking.as_ref().is_some_and(|request| request.session == number)
            || self.agent.permissions.iter().any(|request| request.session == number)
    }

    pub(super) fn process_agent_events(&mut self) {
        let mut changed = false;
        for index in 0..self.agent.sessions.len() {
            for _ in 0..MESSAGES_PER_TICK {
                let Some(message) = self.agent.sessions[index]
                    .process
                    .as_ref()
                    .and_then(AgentProcess::try_message)
                else {
                    break;
                };
                changed = true;
                let save = matches!(message, AgentMessage::Response { .. } | AgentMessage::Exited(_));
                self.handle_agent_message(index, message);
                if save {
                    self.save_agent_history(index, true);
                }
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
                .map_or_else(|| "The agent".to_owned(), |session| {
                    format!("{} {}", session.number, session.name)
                });
            let title = request.title.clone();
            self.agent.asking = Some(request);
            self.open_confirmation_actions(
                PromptKind::AgentPermission,
                format!("{name} wants to run a tool"),
                title,
                ConfirmationChoice::No,
                "Allow",
                "Deny",
            );
            changed = true;
        }
        if changed {
            self.refresh_agent_transcript();
        }
    }

    /// Keep the tab's history file current; tabs nobody asked anything are not kept.
    fn save_agent_history(&self, index: usize, open: bool) {
        let session = &self.agent.sessions[index];
        if self.configuration.settings().persistence.enabled
            && session.entries.iter().any(|entry| matches!(entry, Entry::User { .. }))
        {
            history::save(history::directory(), session.conversation(open), session.entries.clone());
        }
    }

    /// Bring back the tabs that were open when Dust last quit, stopped until asked again.
    fn restore_agent_tabs(&mut self) {
        if std::mem::replace(&mut self.agent.restored, true)
            || !self.configuration.settings().persistence.enabled
        {
            return;
        }
        let mut open = history::list(&history::directory())
            .into_iter()
            .filter(|conversation| conversation.open)
            .collect::<Vec<_>>();
        // Oldest first, so the tabs keep their order.
        open.reverse();
        for conversation in open {
            self.reopen_conversation(&conversation.id);
        }
        self.agent.active = 0;
    }

    /// Put a saved conversation in a tab, or show the tab it is already in.
    fn reopen_conversation(&mut self, id: &str) -> Option<usize> {
        if let Some(index) = self.agent.sessions.iter().position(|session| session.history == id) {
            self.agent.active = index;
            return Some(index);
        }
        let (conversation, entries) = history::load(&history::directory(), id)?;
        let index = self.add_agent_tab(
            conversation.agent,
            conversation.root,
            entries,
            conversation.session,
            Some(conversation.id),
        );
        self.agent.active = index;
        Some(index)
    }

    fn handle_agent_message(&mut self, index: usize, message: AgentMessage) {
        let session = &mut self.agent.sessions[index];
        match message {
            AgentMessage::Response { id, result } => {
                if let Some(position) = session.requests.iter().position(|(request, _)| *request == id) {
                    let (_, pending) = session.requests.remove(position);
                    match (pending, result) {
                        (Pending::SetOption { option, value }, Ok(result)) => {
                            if result["configOptions"].is_array() {
                                session.options = agent::session_options(&result);
                            } else if let Some(option) =
                                session.options.iter_mut().find(|candidate| candidate.id == option)
                            {
                                option.current = value;
                            }
                        }
                        (Pending::Authenticate, Ok(_)) => session.open_session(),
                        (_, Err(error)) => session.entries.push(Entry::Notice(error)),
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
                if method == "session/update" && !matches!(session.phase, Phase::Rejoining(_)) {
                    apply_update(session, agent::session_update(&params));
                }
            }
            AgentMessage::Request { id, method, params } => match method.as_str() {
                "session/request_permission" => {
                    let number = session.number;
                    self.agent.permissions.push_back(PermissionRequest {
                        session: number,
                        id,
                        title: params["toolCall"]["title"]
                            .as_str()
                            .unwrap_or("a tool")
                            .to_owned(),
                        options: agent::permission_options(&params),
                    });
                }
                "fs/read_text_file" => {
                    let result = self.agent_read_file(&params);
                    self.agent.sessions[index]
                        .respond(id, result.map(|content| json!({"content": content})));
                }
                "fs/write_text_file" => {
                    let root = session.root.clone();
                    let result = self.agent_write_file(&root, &params);
                    self.agent.sessions[index].respond(id, result.map(|()| Value::Null));
                }
                _ => session.respond(id, Err(format!("{method} is not supported by Dust"))),
            },
            AgentMessage::Log(line) => session.last_log = line,
            AgentMessage::Exited(reason) => {
                let detail = if session.last_log.is_empty() {
                    reason
                } else {
                    format!("{reason}: {}", session.last_log)
                };
                session
                    .entries
                    .push(Entry::Notice(format!("{} stopped ({detail}).", session.name)));
                session.process = None;
                session.phase = Phase::Stopped;
                session.queued.clear();
                session.requests.clear();
                let number = session.number;
                self.agent.permissions.retain(|request| request.session != number);
                if self.agent.asking.take_if(|request| request.session == number).is_some() {
                    self.close_prompt();
                }
            }
        }
    }

    /// The reply to the session's start or prompt requests. Returns whether the agent now
    /// waits for a sign-in.
    fn handle_agent_response(&mut self, index: usize, id: u64, result: Result<Value, String>) -> bool {
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
                session
                    .entries
                    .push(Entry::Notice(format!("{} picked up its earlier session.", session.name)));
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
                match result["stopReason"].as_str().unwrap_or_default() {
                    "cancelled" => session.entries.push(Entry::Notice("Stopped.".into())),
                    "refusal" => session.entries.push(Entry::Notice("The agent refused.".into())),
                    "max_tokens" | "max_turn_requests" => session
                        .entries
                        .push(Entry::Notice("The agent hit its turn limit.".into())),
                    _ => {}
                }
                session.phase = Phase::Ready;
                session.send_next_prompt();
            }
            (phase, Err(error)) => {
                session.entries.push(Entry::Notice(error));
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

    /// Open buffers are read from memory, so the agent sees unsaved edits.
    fn agent_read_file(&self, params: &Value) -> Result<String, String> {
        let path = PathBuf::from(params["path"].as_str().ok_or("path is missing")?);
        let open = self
            .state
            .buffers
            .values()
            .find(|buffer| buffer.path.as_ref() == Some(&path));
        let text = match open {
            Some(buffer) => buffer.text.to_string(),
            None => {
                let size = std::fs::metadata(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?
                    .len();
                if size > READ_BYTE_CEILING {
                    return Err(format!("{} is too large to read", path.display()));
                }
                std::fs::read_to_string(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?
            }
        };
        Ok(agent::slice_lines(
            &text,
            params["line"].as_u64(),
            params["limit"].as_u64(),
        ))
    }

    /// Writes stay inside the session's worktree and never overwrite unsaved edits; an open,
    /// clean buffer is reloaded so it shows the agent's change.
    fn agent_write_file(&mut self, root: &Path, params: &Value) -> Result<(), String> {
        let path = PathBuf::from(params["path"].as_str().ok_or("path is missing")?);
        let content = params["content"].as_str().ok_or("content is missing")?;
        if !path.is_absolute()
            || !path.starts_with(root)
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(format!("{} is outside {}", path.display(), root.display()));
        }
        let open = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            .map(|(id, buffer)| (id, buffer.dirty));
        if let Some((_, true)) = open {
            return Err(format!("{} has unsaved changes in Dust", path.display()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&path, content).map_err(|error| format!("{}: {error}", path.display()))?;
        if let Some((id, _)) = open {
            let view = Self::document_view(id, &self.state.buffers[id]);
            if let Err(error) = self.documents.request_reload(
                Some(view),
                false,
                &self.state.window_version(),
                self.workspace.version(),
            ) {
                self.show_document_failure(error);
            }
        }
        Ok(())
    }

    pub(super) fn answer_agent_permission(&mut self, choice: ConfirmationChoice) {
        let request = self.agent.asking.take();
        self.close_prompt();
        let Some(request) = request else {
            return;
        };
        let Some(session) = self
            .agent
            .sessions
            .iter_mut()
            .find(|session| session.number == request.session)
        else {
            return;
        };
        let wanted = if choice == ConfirmationChoice::Yes {
            "allow_once"
        } else {
            "reject_once"
        };
        let prefix = &wanted[..wanted.find('_').unwrap_or(0)];
        let option = request
            .options
            .iter()
            .find(|option| option.kind == wanted)
            .or_else(|| request.options.iter().find(|option| option.kind.starts_with(prefix)));
        let result = option.map_or_else(agent::permission_cancelled, |option| {
            agent::permission_selected(&option.id)
        });
        session.respond(request.id, Ok(result));
    }

    pub(super) fn cancel_agent_permission(&mut self) {
        if let Some(request) = self.agent.asking.take()
            && let Some(session) = self
                .agent
                .sessions
                .iter_mut()
                .find(|session| session.number == request.session)
        {
            session.respond(request.id, Ok(agent::permission_cancelled()));
        }
    }

    pub(super) fn toggle_agent_panel(&mut self) {
        self.restore_agent_tabs();
        if let Some(pane) = self.agent_pane() {
            let previous = self.active_tab_focus();
            if self.state.focus_pane_id(pane).is_ok() {
                self.apply_layout_action(LayoutAction::ClosePane);
            }
            if let Some(previous) = previous.filter(|previous| *previous != pane) {
                let _ = self.state.focus_pane_id(previous);
            }
            self.reconcile_editor_windows();
            self.ui.agent_panel = None;
            return;
        }
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    /// Show the panel in a split on the right and put the cursor in its input.
    fn show_agent_panel(&mut self) {
        let previous = self.active_tab_focus();
        if self.agent_pane().is_none() {
            if self.state.active_view.is_some() {
                self.apply_layout_action(LayoutAction::Split(Axis::Horizontal));
            }
            let shown = match self.agent.buffer.filter(|id| self.state.buffers.contains_key(*id)) {
                Some(buffer) => self.state.show_buffer(buffer).map(|_| ()),
                None => {
                    let (buffer, _) = self.state.open_virtual_read_only(
                        TextBuffer::from_text(""),
                        TRANSCRIPT_OWNER.into(),
                        0,
                        0,
                        1,
                    );
                    self.agent.buffer = Some(buffer);
                    Ok(())
                }
            };
            match shown {
                Ok(()) => self.set_dashboard_visibility(false),
                Err(error) => self.show_error(error),
            }
        }
        let Some(pane) = self.agent_pane() else {
            return;
        };
        if let Some(previous) = previous.filter(|previous| *previous != pane) {
            self.agent.back = Some(previous);
        }
        self.set_explorer_focus(false);
        let _ = self.state.focus_pane_id(pane);
        self.reconcile_editor_windows();
    }

    fn agent_pane(&self) -> Option<crate::editor::PaneId> {
        let buffer = self.agent.buffer?;
        let shown = self.state.active_tab_views();
        self.state
            .panes
            .iter()
            .find(|(_, pane)| {
                shown.contains(&pane.view)
                    && self
                        .state
                        .views
                        .get(pane.view)
                        .is_some_and(|view| view.buffer == buffer)
            })
            .map(|(id, _)| id)
    }

    fn active_tab_focus(&self) -> Option<crate::editor::PaneId> {
        Some(self.state.tabs.get(self.state.active_tab?)?.focused)
    }

    fn active_path(&self) -> Option<PathBuf> {
        let buffer = self.state.active_buffer_id()?;
        self.state.buffers.get(buffer)?.path.clone()
    }

    fn agent_view(&self) -> Option<crate::editor::ViewId> {
        Some(self.state.panes.get(self.agent_pane()?)?.view)
    }

    /// Whether keys go to the panel's input.
    pub(super) fn agent_input_focused(&self) -> bool {
        self.ui
            .agent_panel
            .as_ref()
            .is_some_and(|panel| Some(panel.view) == self.state.active_view)
    }

    /// Build what the panel draws from the active session.
    fn refresh_agent_transcript(&mut self) {
        let Some(view) = self.agent_view() else {
            self.ui.agent_panel = None;
            return;
        };
        let width = crate::ui::view_pane_viewport(&self.state, &self.ui, view)
            .map_or(80, |viewport| usize::from(viewport.width))
            .saturating_sub(2)
            .max(10);
        self.agent.width = width;
        let mut text = Styled::default();
        let mut bands = Vec::new();
        let (tabs, status, settings) = match self.active_agent() {
            Some(session) => {
                let working = match (session.process.is_some(), session.phase) {
                    (true, Phase::Initializing(_) | Phase::Creating(_) | Phase::Rejoining(_)) => {
                        "Starting…"
                    }
                    (true, Phase::Prompting(_)) => "Working…",
                    _ => "",
                };
                transcript(&mut text, &mut bands, &session.name, &session.entries, working, width);
                let status = match working {
                    "Working…" => "Working…  esc to stop".to_owned(),
                    "" => shown_root(&session.root, width / 2),
                    starting => starting.to_owned(),
                };
                let settings = [("model", ""), ("mode", ""), (THINKING, "thinking ")]
                    .into_iter()
                    .filter_map(|(key, label)| {
                        Some(format!("{label}{}", value_name(session.option(key)?)))
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                let tabs = self
                    .agent
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        let mark = match tab.status(self.agent_waiting(tab.number)) {
                            "working" | "starting" => " …",
                            "waiting for you" | "sign-in needed" => " ?",
                            "stopped" => " ✗",
                            _ => "",
                        };
                        (format!("{} {}{mark}", tab.number, tab.name), index == self.agent.active)
                    })
                    .collect();
                (tabs, status, settings)
            }
            None => {
                let name = &self.configuration.settings().agents.default;
                text.wrapped(
                    &format!(
                        "No agent session yet. Type below to ask {name}, ctrl-n picks another \
                         agent and ctrl-r resumes a saved conversation."
                    ),
                    Some(SyntaxRole::Comment),
                    ("", None),
                    ("", None),
                    width,
                );
                (vec![("Agent".to_owned(), true)], String::new(), String::new())
            }
        };
        self.agent.lines = crate::ui::transcript_lines(&text.text).len();
        self.agent.scroll = self.agent.scroll.min(self.agent.lines);
        self.ui.agent_panel = Some(AgentPanelUi {
            view,
            tabs,
            transcript: text,
            bands,
            scroll: self.agent.scroll,
            context: Vec::new(),
            input: String::new(),
            cursor: 0,
            placeholder: String::new(),
            status,
            settings,
        });
        self.sync_agent_input();
    }

    /// Copy the input and its context to the panel; cheaper than a rebuild, for keystrokes.
    fn sync_agent_input(&mut self) {
        let name = self.active_agent().map_or_else(
            || self.configuration.settings().agents.default.clone(),
            |session| session.name.clone(),
        );
        let Some(panel) = self.ui.agent_panel.as_mut() else {
            return;
        };
        panel.input.clone_from(&self.agent.input);
        panel.cursor = self.agent.cursor;
        panel.scroll = self.agent.scroll;
        panel.context = self.agent.context.iter().map(context_label).collect();
        panel.placeholder = format!("Ask {name}…  enter sends, alt-enter adds a line");
    }

    /// The panel's width changed or it moved to another view: wrap the transcript again.
    pub(super) fn follow_agent_pane(&mut self) {
        let Some(view) = self.agent_view() else {
            if self.ui.agent_panel.is_some() {
                self.ui.agent_panel = None;
            }
            return;
        };
        let width = crate::ui::view_pane_viewport(&self.state, &self.ui, view)
            .map_or(80, |viewport| usize::from(viewport.width))
            .saturating_sub(2)
            .max(10);
        if self.ui.agent_panel.as_ref().is_none_or(|panel| panel.view != view)
            || width != self.agent.width
        {
            self.refresh_agent_transcript();
        }
    }

    pub(super) fn agent_input_insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.agent.input.insert_str(self.agent.cursor, &text);
        self.agent.cursor += text.len();
        self.sync_agent_input();
    }

    /// Editing keys in the input: `submit`, `newline`, `backspace`, `delete`, `left`, `right`,
    /// `home`, `end`, `up`, `down`, `delete_word`, `clear_line` and `clear`.
    pub(super) fn agent_input_edit(&mut self, action: &str) {
        let input = &mut self.agent.input;
        let cursor = self.agent.cursor.min(input.len());
        let line_start = input[..cursor].rfind('\n').map_or(0, |index| index + 1);
        let line_end = input[cursor..].find('\n').map_or(input.len(), |index| cursor + index);
        let before = input[..cursor].chars().next_back().map_or(0, char::len_utf8);
        let after = input[cursor..].chars().next().map_or(0, char::len_utf8);
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
            "backspace" => {
                input.replace_range(cursor - before..cursor, "");
                cursor - before
            }
            "delete" => {
                input.replace_range(cursor..cursor + after, "");
                cursor
            }
            "left" => cursor - before,
            "right" => cursor + after,
            "home" => line_start,
            "end" => line_end,
            "delete_word" => {
                let kept = input[..cursor].trim_end();
                let start = kept
                    .char_indices()
                    .rev()
                    .find(|(_, character)| character.is_whitespace())
                    .map_or(0, |(index, character)| index + character.len_utf8());
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
            "up" if input.is_empty() => {
                // Up in an empty input brings back the last question.
                let last = self.agent.sessions.get(self.agent.active).and_then(|session| {
                    session.entries.iter().rev().find_map(|entry| match entry {
                        Entry::User { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                });
                *input = last.unwrap_or_default();
                input.len()
            }
            "up" | "down" => {
                let column = input[line_start..cursor].chars().count();
                let target = if action == "up" {
                    line_start
                        .checked_sub(1)
                        .map(|end| input[..end].rfind('\n').map_or(0, |index| index + 1))
                } else {
                    (line_end < input.len()).then_some(line_end + 1)
                };
                match target {
                    Some(start) => {
                        let end = input[start..].find('\n').map_or(input.len(), |index| start + index);
                        input[start..end]
                            .char_indices()
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

    /// Scroll the transcript; positive is up, toward older output.
    pub(super) fn scroll_agent(&mut self, rows: i64) {
        let rows = isize::try_from(rows).unwrap_or(0);
        self.agent.scroll = self.agent.scroll.saturating_add_signed(rows).min(self.agent.lines);
        self.sync_agent_input();
    }

    /// Escape stops a turn that is running, and otherwise goes back to the code.
    pub(super) fn agent_escape(&mut self) {
        if self
            .active_agent()
            .is_some_and(|session| matches!(session.phase, Phase::Prompting(_)))
        {
            self.cancel_agent_turn();
            return;
        }
        let agent = self.agent_pane();
        let shown = self.state.active_tab_views();
        let back = self
            .agent
            .back
            .filter(|pane| self.state.panes.contains_key(*pane) && Some(*pane) != agent)
            .or_else(|| {
                self.state
                    .panes
                    .iter()
                    .find(|(id, pane)| Some(*id) != agent && shown.contains(&pane.view))
                    .map(|(id, _)| id)
            });
        if let Some(pane) = back {
            let _ = self.state.focus_pane_id(pane);
            self.reconcile_editor_windows();
        }
    }

    /// The wheel scrolls the transcript, a click focuses the input, and a click on the tab
    /// line shows that session. Returns whether the event was over the panel.
    pub(super) fn agent_panel_mouse(
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
            Some(TerminalMouseInput::LeftDown) => {
                self.show_agent_panel();
                if row == viewport.y {
                    let labels = self
                        .ui
                        .agent_panel
                        .as_ref()
                        .map(|panel| panel.tabs.iter().map(|(label, _)| label.clone()).collect())
                        .unwrap_or_else(Vec::new);
                    let mut right = usize::from(viewport.x);
                    for (index, label) in labels.iter().enumerate() {
                        right += crate::ui::geometry::text_width(label, self.configuration.settings().editor.ambiguous_width) + 2;
                        if usize::from(column) < right {
                            if index < self.agent.sessions.len() && index != self.agent.active {
                                self.agent.active = index;
                                self.agent.scroll = 0;
                            }
                            break;
                        }
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
            if let Some(option) = session.options.iter_mut().find(|option| option.category == "mode") {
                option.current = mode;
            }
        }
        SessionUpdate::Other => {}
    }
}

fn context_label(part: &PromptPart) -> String {
    match part {
        PromptPart::File { path, text: None } => file_name(path),
        PromptPart::File {
            path,
            text: Some(text),
        } => format!("{} ({} lines selected)", file_name(path), text.lines().count()),
        PromptPart::Text(text) => text.lines().next().unwrap_or_default().to_owned(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

fn selected_text(
    text: &TextBuffer,
    selection: crate::editor::Selection,
    cursor: crate::editor::text::ByteOffset,
) -> String {
    let (start, end) = if selection.anchor <= cursor {
        (selection.anchor, cursor)
    } else {
        (cursor, selection.anchor)
    };
    let (start, end) = if selection.kind == crate::editor::SelectionKind::Line {
        let first = text.byte_to_line(start).unwrap_or(0);
        let last = text.byte_to_line(end).unwrap_or(first);
        let start = text.line_to_byte(first).map_or(0, |offset| offset.get());
        let end = text
            .line_to_byte(last + 1)
            .map_or(text.len_bytes(), |offset| offset.get());
        (start, end)
    } else {
        let end = crate::editor::text::next_grapheme_boundary(text, end).get();
        (start.get(), end)
    };
    crate::editor::text::ByteRange::new(
        crate::editor::text::ByteOffset(start),
        crate::editor::text::ByteOffset(end.max(start)),
    )
    .ok()
    .and_then(|range| text.slice(range).ok())
    .unwrap_or_default()
}

/// The conversation, with the Markdown in questions and answers rendered and wrapped to
/// `width`. The byte ranges of the user's messages go to `bands`.
fn transcript(
    out: &mut Styled,
    bands: &mut Vec<std::ops::Range<usize>>,
    name: &str,
    entries: &[Entry],
    working: &str,
    width: usize,
) {
    let plain = ("", None);
    let quote = Some(SyntaxRole::Quote);
    for (index, entry) in entries.iter().enumerate() {
        // A blank line between tool lines and the prose around them.
        let previous = index.checked_sub(1).map(|previous| &entries[previous]);
        if matches!(
            (previous, entry),
            (Some(Entry::Agent(_)), Entry::Tool { .. }) | (Some(Entry::Tool { .. }), Entry::Agent(_))
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
            Entry::User { text, context } => {
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
            Entry::Tool { title, status, .. } => {
                let mark = match status.as_str() {
                    "completed" => "✓ ",
                    "failed" => "✗ ",
                    "in_progress" => "… ",
                    _ => "○ ",
                };
                out.wrapped(title, None, (mark, Some(SyntaxRole::Keyword)), ("  ", None), width);
            }
            Entry::Plan(steps) => {
                out.push("\n", None);
                out.push("Plan", Some(SyntaxRole::Strong));
                out.push("\n", None);
                for (status, step) in steps {
                    let mark = match status.as_str() {
                        "completed" => "✓ ",
                        "in_progress" => "… ",
                        _ => "○ ",
                    };
                    out.wrapped(step, None, (mark, Some(SyntaxRole::Keyword)), ("  ", None), width);
                }
                out.push("\n", None);
            }
            Entry::Notice(text) => {
                out.push("\n", None);
                out.wrapped(text, quote, plain, plain, width);
            }
        }
    }
    if !working.is_empty() {
        out.push("\n", None);
        out.push(working, quote);
        out.push("\n", None);
    }
}

/// The session's directory with the home directory as `~`, cut from the left to fit.
fn shown_root(root: &Path, width: usize) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shown = match home.as_deref().and_then(|home| root.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => root.display().to_string(),
    };
    let width = width.saturating_sub(2).max(8);
    let count = shown.chars().count();
    if count <= width {
        shown
    } else {
        format!("…{}", shown.chars().skip(count + 1 - width).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_users_message_is_a_band_and_the_answer_follows() {
        let entries = vec![
            Entry::Started {
                root: PathBuf::from("/work/repo"),
            },
            Entry::User {
                text: "why".into(),
                context: vec!["main.rs".into()],
            },
            Entry::Tool {
                id: "t".into(),
                title: "Read main.rs".into(),
                status: "completed".into(),
            },
            Entry::Agent("Because.".into()),
        ];
        let mut out = Styled::default();
        let mut bands = Vec::new();
        transcript(&mut out, &mut bands, "claude", &entries, "", 80);
        assert_eq!(out.text, "\nwhy\n+ main.rs\n\n\n✓ Read main.rs\n\nBecause.\n");
        let banded = bands.iter().map(|band| &out.text[band.clone()]).collect::<Vec<_>>();
        assert_eq!(banded, ["\nwhy\n+ main.rs\n\n"]);
    }

    #[test]
    fn streamed_chunks_join_and_option_updates_replace_the_list() {
        let mut session = AgentSession {
            number: 1,
            name: "claude".into(),
            root: PathBuf::from("/r"),
            process: None,
            phase: Phase::Ready,
            id: Some("s".into()),
            queued: VecDeque::new(),
            last_log: String::new(),
            entries: Vec::new(),
            options: agent::session_options(&json!({
                "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "Default"}, {"id": "plan", "name": "Plan"}]}
            })),
            auth_methods: Vec::new(),
            requests: Vec::new(),
            preferred: BTreeMap::new(),
            history: "1".into(),
            rejoin: Rejoin::default(),
            earlier: None,
            recap: false,
        };
        assert_eq!(session.status(false), "saved");
        session.phase = Phase::Stopped;
        assert_eq!(session.status(false), "stopped");
        apply_update(&mut session, SessionUpdate::AgentText("Hel".into()));
        apply_update(&mut session, SessionUpdate::AgentText("lo".into()));
        apply_update(
            &mut session,
            SessionUpdate::ToolCall {
                id: "1".into(),
                title: "Edit".into(),
                status: "pending".into(),
            },
        );
        apply_update(
            &mut session,
            SessionUpdate::ToolCallUpdate {
                id: "1".into(),
                title: None,
                status: Some("completed".into()),
            },
        );
        assert!(matches!(&session.entries[0], Entry::Agent(text) if text == "Hello"));
        assert!(matches!(&session.entries[1], Entry::Tool { status, .. } if status == "completed"));
        apply_update(&mut session, SessionUpdate::Mode("plan".into()));
        assert_eq!(value_name(session.option("mode").unwrap()), "Plan");
        assert!(session.set_option("mode", "missing").is_err());

        // An agent that cannot pick up its earlier session gets the conversation instead.
        session.earlier = Some("old".into());
        session.open_session();
        assert!(session.recap && session.earlier.is_none());
    }
}
