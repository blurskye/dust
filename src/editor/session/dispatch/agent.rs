//! The agent panel: one ACP session at a time, its transcript in a read-only Markdown buffer
//! in a split beside the code, questions through a prompt and permission requests as yes/no
//! prompts. A session keeps the worktree it started in; switching worktrees does not move it.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use super::InteractiveApplication;
use crate::{
    agent::{self, AgentMessage, AgentProcess, PermissionOption, PromptPart, SessionUpdate},
    editor::{Axis, BufferId, EditorMode, LayoutAction, text::TextBuffer},
    ui::{ConfirmationChoice, PromptKind},
};

/// Shown where a file would show its name, and how the syntax session knows to use Markdown.
pub(super) const TRANSCRIPT_OWNER: &str = "Agent";
/// Messages handled per tick, so a chatty agent cannot stall input.
const MESSAGES_PER_TICK: usize = 256;
/// Files larger than this are not read for the agent.
const READ_BYTE_CEILING: u64 = 16 * 1024 * 1024;

#[derive(Default)]
pub(super) struct AgentPanel {
    /// The picked agent; `[agents] default` until one is picked.
    picked: Option<String>,
    session: Option<AgentSession>,
    /// What the transcript shows, kept when a session ends so it can still be read.
    entries: Vec<Entry>,
    buffer: Option<BufferId>,
    /// Files, selections and diagnostics to send with the next question.
    context: Vec<PromptPart>,
    /// Permission requests waiting for the prompt line to be free.
    permissions: VecDeque<PermissionRequest>,
    /// The request the open permission prompt answers.
    asking: Option<PermissionRequest>,
}

struct AgentSession {
    name: String,
    root: PathBuf,
    process: AgentProcess,
    phase: Phase,
    id: Option<String>,
    /// Questions asked before the session was ready, sent in order.
    queued: VecDeque<Vec<PromptPart>>,
    /// The agent's last stderr line, shown if it exits.
    last_log: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Initializing(u64),
    Creating(u64),
    Ready,
    Prompting(u64),
}

struct PermissionRequest {
    id: Value,
    title: String,
    options: Vec<PermissionOption>,
}

#[derive(Clone, Debug)]
enum Entry {
    Header { name: String, root: PathBuf },
    User { text: String, context: Vec<String> },
    Agent(String),
    Thought(String),
    Tool { id: String, title: String, status: String },
    Plan(Vec<(String, String)>),
    Notice(String),
}

impl InteractiveApplication {
    pub(super) fn agent_name(&self) -> String {
        self.agent
            .picked
            .clone()
            .unwrap_or_else(|| self.configuration.settings().agents.default.clone())
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
        self.open_prompt(PromptKind::AgentAsk, String::new());
    }

    pub(super) fn attach_agent_file(&mut self) {
        let Some(path) = self.active_path() else {
            self.show_error("the current buffer has no file");
            return;
        };
        self.agent.context.push(PromptPart::File { path, text: None });
        self.open_prompt(PromptKind::AgentAsk, String::new());
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
        self.open_prompt(PromptKind::AgentAsk, "Fix these diagnostics".into());
    }

    pub(super) fn agent_ask(&mut self, question: String) {
        let question = question.trim().to_owned();
        if question.is_empty() {
            self.agent.context.clear();
            return;
        }
        if self.agent.session.is_none() {
            let name = self.agent_name();
            if let Err(error) = self.start_agent(&name) {
                self.show_error(error);
                return;
            }
        }
        let mut parts = std::mem::take(&mut self.agent.context);
        let context = parts.iter().map(context_label).collect();
        parts.push(PromptPart::Text(question.clone()));
        self.agent.entries.push(Entry::User {
            text: question,
            context,
        });
        if let Some(session) = &mut self.agent.session {
            session.queued.push_back(parts);
            if session.phase == Phase::Ready {
                send_next_prompt(session, &mut self.agent.entries);
            }
        }
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    pub(super) fn cancel_agent_turn(&mut self) {
        let Some(session) = &mut self.agent.session else {
            self.notify_message("no agent session".into(), false);
            return;
        };
        session.queued.clear();
        if let (Phase::Prompting(_), Some(id)) = (session.phase, &session.id) {
            let _ = session
                .process
                .notify("session/cancel", json!({"sessionId": id}));
            // The protocol asks the client to answer open permission requests as cancelled.
            for request in self.agent.permissions.drain(..).chain(self.agent.asking.take()) {
                let _ = session
                    .process
                    .respond(request.id, Ok(agent::permission_cancelled()));
            }
            if self
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == PromptKind::AgentPermission)
            {
                self.close_prompt();
            }
            self.notify_message("stopping the agent…".into(), false);
        }
    }

    /// `<leader>an` and the agent picker: end the current session and start a new one in the
    /// current worktree.
    pub(super) fn restart_agent(&mut self, name: Option<String>) {
        if let Some(name) = name {
            self.agent.picked = Some(name);
        }
        self.agent.session = None;
        self.agent.permissions.clear();
        self.agent.asking = None;
        self.agent.entries.clear();
        let name = self.agent_name();
        if let Err(error) = self.start_agent(&name) {
            self.show_error(error);
        }
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    fn start_agent(&mut self, name: &str) -> Result<(), String> {
        let profile = self
            .configuration
            .settings()
            .agents
            .profiles
            .get(name)
            .ok_or_else(|| format!("no agent named `{name}` in [agents.profiles]"))?
            .clone();
        let root = self.state.workspace_root.clone();
        let mut process = AgentProcess::spawn(
            &profile.program,
            &profile.arguments,
            &profile.environment,
            &root,
        )
        .map_err(|error| format!("could not start agent {name}: {error}"))?;
        let id = process.request("initialize", agent::initialize_params())?;
        self.agent.entries.push(Entry::Header {
            name: name.to_owned(),
            root: root.clone(),
        });
        self.agent.session = Some(AgentSession {
            name: name.to_owned(),
            root,
            process,
            phase: Phase::Initializing(id),
            id: None,
            queued: VecDeque::new(),
            last_log: String::new(),
        });
        Ok(())
    }

    pub(super) fn process_agent_events(&mut self) {
        let mut changed = false;
        for _ in 0..MESSAGES_PER_TICK {
            let Some(message) = self
                .agent
                .session
                .as_ref()
                .and_then(|session| session.process.try_message())
            else {
                break;
            };
            changed = true;
            self.handle_agent_message(message);
        }
        if self.agent.asking.is_none()
            && self.input.prompt().is_none()
            && let Some(request) = self.agent.permissions.pop_front()
        {
            let name = self.agent_name();
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
        }
        if changed {
            self.refresh_agent_transcript();
        }
    }

    fn handle_agent_message(&mut self, message: AgentMessage) {
        let Some(session) = &mut self.agent.session else {
            return;
        };
        let entries = &mut self.agent.entries;
        match message {
            AgentMessage::Response { id, result } => match (session.phase, result) {
                (Phase::Initializing(expected), Ok(_)) if id == expected => {
                    match session
                        .process
                        .request("session/new", agent::new_session_params(&session.root))
                    {
                        Ok(id) => session.phase = Phase::Creating(id),
                        Err(error) => entries.push(Entry::Notice(error)),
                    }
                }
                (Phase::Creating(expected), Ok(result)) if id == expected => {
                    session.id = result["sessionId"].as_str().map(str::to_owned);
                    session.phase = Phase::Ready;
                    send_next_prompt(session, entries);
                }
                (Phase::Prompting(expected), Ok(result)) if id == expected => {
                    match result["stopReason"].as_str().unwrap_or_default() {
                        "cancelled" => entries.push(Entry::Notice("Stopped.".into())),
                        "refusal" => entries.push(Entry::Notice("The agent refused.".into())),
                        "max_tokens" | "max_turn_requests" => {
                            entries.push(Entry::Notice("The agent hit its turn limit.".into()))
                        }
                        _ => {}
                    }
                    session.phase = Phase::Ready;
                    send_next_prompt(session, entries);
                }
                (phase, Err(error)) => {
                    entries.push(Entry::Notice(error));
                    if matches!(phase, Phase::Prompting(expected) if expected == id) {
                        session.phase = Phase::Ready;
                        send_next_prompt(session, entries);
                    } else if matches!(phase, Phase::Initializing(_) | Phase::Creating(_)) {
                        self.agent.session = None;
                    }
                }
                _ => {}
            },
            AgentMessage::Notification { method, params } => {
                if method == "session/update" {
                    apply_update(entries, agent::session_update(&params));
                }
            }
            AgentMessage::Request { id, method, params } => match method.as_str() {
                "session/request_permission" => {
                    self.agent.permissions.push_back(PermissionRequest {
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
                    if let Some(session) = &mut self.agent.session {
                        let _ = session
                            .process
                            .respond(id, result.map(|content| json!({"content": content})));
                    }
                }
                "fs/write_text_file" => {
                    let result = self.agent_write_file(&params);
                    if let Some(session) = &mut self.agent.session {
                        let _ = session.process.respond(id, result.map(|()| Value::Null));
                    }
                }
                _ => {
                    let _ = session
                        .process
                        .respond(id, Err(format!("{method} is not supported by Dust")));
                }
            },
            AgentMessage::Log(line) => session.last_log = line,
            AgentMessage::Exited(reason) => {
                let detail = if session.last_log.is_empty() {
                    reason
                } else {
                    format!("{reason}: {}", session.last_log)
                };
                entries.push(Entry::Notice(format!("{} stopped ({detail}).", session.name)));
                self.agent.session = None;
                self.agent.permissions.clear();
            }
        }
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
    fn agent_write_file(&mut self, params: &Value) -> Result<(), String> {
        let root = self
            .agent
            .session
            .as_ref()
            .map(|session| session.root.clone())
            .ok_or("no agent session")?;
        let path = PathBuf::from(params["path"].as_str().ok_or("path is missing")?);
        let content = params["content"].as_str().ok_or("content is missing")?;
        if !path.is_absolute()
            || !path.starts_with(&root)
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
        let (Some(request), Some(session)) = (request, &mut self.agent.session) else {
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
        let _ = session.process.respond(request.id, Ok(result));
    }

    pub(super) fn cancel_agent_permission(&mut self) {
        if let (Some(request), Some(session)) = (self.agent.asking.take(), &mut self.agent.session)
        {
            let _ = session
                .process
                .respond(request.id, Ok(agent::permission_cancelled()));
        }
    }

    pub(super) fn toggle_agent_panel(&mut self) {
        if let Some(pane) = self.agent_pane() {
            let previous = self.active_tab_focus();
            if self.state.focus_pane_id(pane).is_ok() {
                self.apply_layout_action(LayoutAction::ClosePane);
            }
            if let Some(previous) = previous.filter(|previous| *previous != pane) {
                let _ = self.state.focus_pane_id(previous);
            }
            self.reconcile_editor_windows();
            return;
        }
        self.show_agent_panel();
        self.refresh_agent_transcript();
    }

    /// Show the transcript in a split on the right, keeping focus in the code.
    fn show_agent_panel(&mut self) {
        if self.agent_pane().is_some() {
            return;
        }
        let previous = self.active_tab_focus();
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
        if let Err(error) = shown {
            self.show_error(error);
        }
        if let Some(previous) = previous {
            let _ = self.state.focus_pane_id(previous);
        }
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

    /// Rewrite the transcript buffer. Views that were at the end follow the new end.
    fn refresh_agent_transcript(&mut self) {
        let Some(buffer) = self.agent.buffer.filter(|id| self.state.buffers.contains_key(*id))
        else {
            return;
        };
        let views = self
            .state
            .views
            .iter()
            .filter(|(_, view)| view.buffer == buffer)
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let width = views
            .iter()
            .filter_map(|view| crate::ui::view_pane_viewport(&self.state, &self.ui, *view))
            .map(|viewport| usize::from(viewport.width))
            .min()
            .unwrap_or(80)
            .saturating_sub(8)
            .max(20);
        let working = self
            .agent
            .session
            .as_ref()
            .map(|session| match session.phase {
                Phase::Initializing(_) | Phase::Creating(_) => "_Starting…_",
                Phase::Prompting(_) => "_Working…_",
                Phase::Ready => "",
            })
            .unwrap_or("");
        let text = transcript(&self.agent.entries, working, width);
        let (old_lines, revision) = {
            let target = &self.state.buffers[buffer];
            let revision = match &target.origin {
                crate::editor::BufferOrigin::VirtualReadOnly { revision, .. } => *revision,
                crate::editor::BufferOrigin::FileOrScratch => return,
            };
            (target.text.visible_lines(), revision)
        };
        let following = views
            .iter()
            .map(|view| {
                let cursor = self.state.views[*view].cursor;
                let line = self.state.buffers[buffer].text.byte_to_line(cursor).unwrap_or(0);
                (*view, line + 2 >= old_lines)
            })
            .collect::<Vec<_>>();
        if !self.state.replace_virtual_read_only(
            buffer,
            revision,
            revision.saturating_add(1),
            TextBuffer::from_text(&text),
        ) {
            return;
        }
        let new_text = &self.state.buffers[buffer].text;
        let last = new_text.visible_lines().saturating_sub(1);
        let last_start = new_text.line_to_byte(last).map_or(0, |offset| offset.get());
        let length = new_text.len_bytes();
        for (view, follow) in following {
            let height = crate::ui::view_pane_viewport(&self.state, &self.ui, view)
                .map_or(20, |viewport| usize::from(viewport.height));
            let target = &mut self.state.views[view];
            target.selection = None;
            if follow {
                target.cursor = crate::editor::text::ByteOffset(last_start);
                target.scroll_line = (last + 1).saturating_sub(height.saturating_sub(1));
                target.scroll_wrap_row = 0;
            } else if target.cursor.get() > length {
                target.cursor = crate::editor::text::ByteOffset(last_start);
            }
        }
    }
}

fn send_next_prompt(session: &mut AgentSession, entries: &mut Vec<Entry>) {
    let Some(id) = session.id.clone() else {
        return;
    };
    let Some(parts) = session.queued.pop_front() else {
        return;
    };
    match session
        .process
        .request("session/prompt", agent::prompt_params(&id, &parts))
    {
        Ok(request) => session.phase = Phase::Prompting(request),
        Err(error) => entries.push(Entry::Notice(error)),
    }
}

fn apply_update(entries: &mut Vec<Entry>, update: SessionUpdate) {
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

/// The Markdown the panel shows, hard-wrapped to `width` outside code blocks.
fn transcript(entries: &[Entry], working: &str, width: usize) -> String {
    let mut out = String::new();
    let mut agent_heading = None::<&str>;
    let mut answered = true;
    let push = |out: &mut String, text: &str| {
        let mut fenced = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
            }
            if fenced || line.chars().count() <= width {
                out.push_str(line);
                out.push('\n');
            } else {
                out.push_str(&wrap(line, width));
            }
        }
    };
    for (index, entry) in entries.iter().enumerate() {
        // Markdown needs a blank line between a tool list and the prose around it.
        let previous = index.checked_sub(1).map(|previous| &entries[previous]);
        if matches!(
            (previous, entry),
            (Some(Entry::Agent(_)), Entry::Tool { .. }) | (Some(Entry::Tool { .. }), Entry::Agent(_))
        ) {
            out.push('\n');
        }
        let needs_heading = matches!(
            entry,
            Entry::Agent(_) | Entry::Thought(_) | Entry::Tool { .. } | Entry::Plan(_)
        );
        if needs_heading && !answered {
            out.push_str(&format!("\n## {}\n\n", agent_heading.unwrap_or("Agent")));
            answered = true;
        }
        match entry {
            Entry::Header { name, root } => {
                agent_heading = Some(name.as_str());
                if !out.is_empty() {
                    out.push_str("\n---\n\n");
                }
                out.push_str(&format!("# {name}\n\n`{}`\n", shown_root(root, width)));
            }
            Entry::User { text, context } => {
                out.push_str("\n## You\n\n");
                push(&mut out, text);
                for label in context {
                    out.push_str(&format!("> + {label}\n"));
                }
                answered = false;
            }
            Entry::Agent(text) => push(&mut out, text),
            Entry::Thought(text) => {
                // Only the thought still being written is shown; finished ones are noise.
                if index + 1 == entries.len() {
                    for line in wrap(text, width.saturating_sub(2)).lines() {
                        out.push_str(&format!("> {line}\n"));
                    }
                }
            }
            Entry::Tool { title, status, .. } => {
                let mark = match status.as_str() {
                    "completed" => "✓",
                    "failed" => "✗",
                    "in_progress" => "…",
                    _ => "○",
                };
                out.push_str(&format!("- {mark} {title}\n"));
            }
            Entry::Plan(steps) => {
                out.push_str("\n**Plan**\n");
                for (status, step) in steps {
                    let mark = match status.as_str() {
                        "completed" => "x",
                        "in_progress" => "~",
                        _ => " ",
                    };
                    out.push_str(&format!("- [{mark}] {step}\n"));
                }
                out.push('\n');
            }
            Entry::Notice(text) => out.push_str(&format!("\n_{text}_\n")),
        }
    }
    if entries.is_empty() {
        out.push_str("# Agent\n\nNo session yet. Ask with <leader>aa.\n");
    }
    if !working.is_empty() {
        out.push_str(&format!("\n{working}\n"));
    }
    out
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

fn wrap(line: &str, width: usize) -> String {
    let mut out = String::new();
    let mut current = String::new();
    for word in line.split(' ') {
        let candidate = current.chars().count() + usize::from(!current.is_empty()) + word.chars().count();
        if !current.is_empty() && candidate > width {
            out.push_str(&current);
            out.push('\n');
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    out.push_str(&current);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_groups_a_turn_under_headings() {
        let entries = vec![
            Entry::Header {
                name: "claude".into(),
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
        assert_eq!(
            transcript(&entries, "", 80),
            "# claude\n\n`/work/repo`\n\n## You\n\nwhy\n> + main.rs\n\n## claude\n\n- ✓ Read main.rs\n\nBecause.\n"
        );
    }

    #[test]
    fn streamed_chunks_join_and_tool_updates_land_on_their_call() {
        let mut entries = Vec::new();
        apply_update(&mut entries, SessionUpdate::AgentText("Hel".into()));
        apply_update(&mut entries, SessionUpdate::AgentText("lo".into()));
        apply_update(
            &mut entries,
            SessionUpdate::ToolCall {
                id: "1".into(),
                title: "Edit".into(),
                status: "pending".into(),
            },
        );
        apply_update(
            &mut entries,
            SessionUpdate::ToolCallUpdate {
                id: "1".into(),
                title: None,
                status: Some("completed".into()),
            },
        );
        assert!(matches!(&entries[0], Entry::Agent(text) if text == "Hello"));
        assert!(matches!(&entries[1], Entry::Tool { status, .. } if status == "completed"));
    }

    #[test]
    fn long_lines_wrap_at_words() {
        assert_eq!(wrap("aaa bbb ccc", 7), "aaa bbb\nccc\n");
    }
}
