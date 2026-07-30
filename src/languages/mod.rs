#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    document::{ByteOffset, PositionCodec, PositionEncoding, TextBuffer},
    packages::{ToolDefinition, ToolKind},
    processes::{read_content_length_message, write_content_length_message},
};

const COMMAND_QUEUE: usize = 64;
const MESSAGE_QUEUE: usize = 128;
const EVENT_QUEUE: usize = 256;
const MAX_PENDING_REQUESTS: usize = 128;
const MAX_LOG_LINES: usize = 200;
const MAX_LOG_LINE_BYTES: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LspPolicy {
    pub request_timeout: Duration,
    pub maximum_log_lines: usize,
}

impl Default for LspPolicy {
    fn default() -> Self {
        Self {
            request_timeout: REQUEST_TIMEOUT,
            maximum_log_lines: MAX_LOG_LINES,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LspSessionKey {
    pub root_id: u64,
    pub server_id: String,
}
impl LspSessionKey {
    #[must_use]
    pub fn transport_key(&self) -> String {
        format!("root:{}:{}", self.root_id, self.server_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspDocument {
    pub path: PathBuf,
    pub language_id: String,
    pub version: u64,
    pub text: String,
}

impl LspDocument {
    #[must_use]
    pub fn uri(&self) -> String {
        file_uri(&self.path)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyncKind {
    None,
    #[default]
    Full,
    Incremental,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspCapabilities {
    pub position_encoding: PositionEncoding,
    pub sync_kind: SyncKind,
    pub completion: bool,
    pub completion_resolve: bool,
    pub hover: bool,
    pub signature_help: bool,
    pub definition: bool,
    pub declaration: bool,
    pub type_definition: bool,
    pub implementation: bool,
    pub references: bool,
    pub document_symbols: bool,
    pub workspace_symbols: bool,
    pub rename: bool,
    pub formatting: bool,
    pub range_formatting: bool,
    pub code_action: bool,
    pub semantic_tokens: bool,
    pub inlay_hints: bool,
    pub pull_diagnostics: bool,
    pub code_lens: bool,
    pub document_highlights: bool,
    pub folding_ranges: bool,
    pub selection_ranges: bool,
    pub semantic_token_types: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspPosition {
    pub line: usize,
    pub character: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspTextEdit {
    pub range: LspRange,
    pub new_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspDocumentEdit {
    pub uri: String,
    pub version: Option<u64>,
    pub edits: Vec<LspTextEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspResourceOperation {
    Create {
        uri: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Rename {
        old_uri: String,
        new_uri: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Delete {
        uri: String,
        recursive: bool,
        ignore_if_not_exists: bool,
    },
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspWorkspaceEdit {
    pub documents: Vec<LspDocumentEdit>,
    pub resources: Vec<LspResourceOperation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspDiagnostic {
    pub range: LspRange,
    pub severity: Option<u8>,
    pub code: Option<String>,
    pub source: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCompletionItem {
    pub label: String,
    pub detail: String,
    pub insert_text: String,
    pub text_edit: Option<LspTextEdit>,
    pub additional_edits: Vec<LspTextEdit>,
    pub kind: Option<u8>,
    pub snippet: bool,
    pub documentation: String,
    pub resolve_item: Option<Value>,
    pub resolved: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSymbol {
    pub name: String,
    pub kind: Option<u8>,
    pub location: Option<LspLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspInlayHint {
    pub position: LspPosition,
    pub label: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCodeLens {
    pub range: LspRange,
    pub title: String,
    pub command: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSemanticTokens {
    pub result_id: Option<String>,
    pub data: Vec<u32>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSemanticTokenEdit {
    pub start: usize,
    pub delete_count: usize,
    pub data: Vec<u32>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSemanticTokenDelta {
    pub result_id: Option<String>,
    pub edits: Vec<LspSemanticTokenEdit>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspPullDiagnostics {
    pub result_id: Option<String>,
    pub items: Vec<LspDiagnostic>,
    pub unchanged: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub preferred: bool,
    pub edit: Option<LspWorkspaceEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspFeature {
    Hover,
    Completion,
    CompletionResolve { item: Value },
    SignatureHelp,
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    References,
    DocumentSymbols,
    WorkspaceSymbols { query: String },
    Rename { new_name: String },
    Formatting,
    RangeFormatting { range: LspRange },
    CodeAction { range: LspRange },
    PullDiagnostics { previous_result_id: Option<String> },
    SemanticTokensFull,
    SemanticTokensDelta { previous_result_id: String },
    InlayHints { range: LspRange },
    CodeLens,
    DocumentHighlights,
    FoldingRanges,
    SelectionRanges,
}

impl LspFeature {
    fn method(&self) -> &'static str {
        match self {
            Self::Hover => "textDocument/hover",
            Self::Completion => "textDocument/completion",
            Self::CompletionResolve { .. } => "completionItem/resolve",
            Self::SignatureHelp => "textDocument/signatureHelp",
            Self::Definition => "textDocument/definition",
            Self::Declaration => "textDocument/declaration",
            Self::TypeDefinition => "textDocument/typeDefinition",
            Self::Implementation => "textDocument/implementation",
            Self::References => "textDocument/references",
            Self::DocumentSymbols => "textDocument/documentSymbol",
            Self::WorkspaceSymbols { .. } => "workspace/symbol",
            Self::Rename { .. } => "textDocument/rename",
            Self::Formatting => "textDocument/formatting",
            Self::RangeFormatting { .. } => "textDocument/rangeFormatting",
            Self::CodeAction { .. } => "textDocument/codeAction",
            Self::PullDiagnostics { .. } => "textDocument/diagnostic",
            Self::SemanticTokensFull => "textDocument/semanticTokens/full",
            Self::SemanticTokensDelta { .. } => "textDocument/semanticTokens/full/delta",
            Self::InlayHints { .. } => "textDocument/inlayHint",
            Self::CodeLens => "textDocument/codeLens",
            Self::DocumentHighlights => "textDocument/documentHighlight",
            Self::FoldingRanges => "textDocument/foldingRange",
            Self::SelectionRanges => "textDocument/selectionRange",
        }
    }

    fn capability(&self, capabilities: &LspCapabilities) -> bool {
        match self {
            Self::Hover => capabilities.hover,
            Self::Completion => capabilities.completion,
            Self::CompletionResolve { .. } => capabilities.completion_resolve,
            Self::SignatureHelp => capabilities.signature_help,
            Self::Definition => capabilities.definition,
            Self::Declaration => capabilities.declaration,
            Self::TypeDefinition => capabilities.type_definition,
            Self::Implementation => capabilities.implementation,
            Self::References => capabilities.references,
            Self::DocumentSymbols => capabilities.document_symbols,
            Self::WorkspaceSymbols { .. } => capabilities.workspace_symbols,
            Self::Rename { .. } => capabilities.rename,
            Self::Formatting => capabilities.formatting,
            Self::RangeFormatting { .. } => capabilities.range_formatting,
            Self::CodeAction { .. } => capabilities.code_action,
            Self::PullDiagnostics { .. } => capabilities.pull_diagnostics,
            Self::SemanticTokensFull | Self::SemanticTokensDelta { .. } => {
                capabilities.semantic_tokens
            }
            Self::InlayHints { .. } => capabilities.inlay_hints,
            Self::CodeLens => capabilities.code_lens,
            Self::DocumentHighlights => capabilities.document_highlights,
            Self::FoldingRanges => capabilities.folding_ranges,
            Self::SelectionRanges => capabilities.selection_ranges,
        }
    }

    fn replace_key(&self) -> Option<&'static str> {
        match self {
            Self::Hover => Some("hover"),
            Self::Completion | Self::CompletionResolve { .. } => Some("completion"),
            Self::SignatureHelp => Some("signature"),
            Self::Definition | Self::Declaration | Self::TypeDefinition | Self::Implementation => {
                Some("navigation")
            }
            Self::References => Some("references"),
            Self::CodeAction { .. } => Some("code-action"),
            Self::PullDiagnostics { .. } => Some("pull-diagnostics"),
            Self::SemanticTokensFull | Self::SemanticTokensDelta { .. } => Some("semantic-tokens"),
            Self::InlayHints { .. } => Some("inlay-hints"),
            Self::CodeLens => Some("code-lens"),
            Self::DocumentHighlights => Some("highlights"),
            Self::FoldingRanges => Some("folds"),
            Self::SelectionRanges => Some("selection-ranges"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspFeatureResult {
    Hover(String),
    Completion(Vec<LspCompletionItem>),
    CompletionResolved(LspCompletionItem),
    Signature(String),
    Locations(Vec<LspLocation>),
    Symbols(Vec<LspSymbol>),
    WorkspaceEdit(LspWorkspaceEdit),
    CodeActions(Vec<LspCodeAction>),
    PullDiagnostics(LspPullDiagnostics),
    SemanticTokens(LspSemanticTokens),
    SemanticTokenDelta(LspSemanticTokenDelta),
    InlayHints(Vec<LspInlayHint>),
    CodeLens(Vec<LspCodeLens>),
    Ranges(Vec<LspRange>),
    Empty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspHealth {
    pub root: PathBuf,
    pub executable: PathBuf,
    pub capabilities: LspCapabilities,
    pub attached_documents: usize,
    pub pending_requests: usize,
    pub recent_logs: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ServerRequestId {
    Number(i64),
    String(String),
}

impl ServerRequestId {
    fn from_value(value: &Value) -> Option<Self> {
        value
            .as_i64()
            .map(Self::Number)
            .or_else(|| value.as_str().map(|value| Self::String(value.into())))
    }

    fn value(&self) -> Value {
        match self {
            Self::Number(value) => json!(value),
            Self::String(value) => json!(value),
        }
    }
}

#[derive(Clone, Debug)]
pub enum LspEvent {
    Starting {
        server: String,
    },
    Running {
        server: String,
        capabilities: LspCapabilities,
    },
    CapabilitiesChanged {
        server: String,
        capabilities: LspCapabilities,
    },
    Diagnostics {
        server: String,
        uri: String,
        version: Option<u64>,
        diagnostics: Vec<LspDiagnostic>,
    },
    Result {
        server: String,
        generation: u64,
        method: String,
        uri: String,
        version: u64,
        result: LspFeatureResult,
    },
    ApplyEditRequest {
        server: String,
        request_id: ServerRequestId,
        label: Option<String>,
        edit: LspWorkspaceEdit,
    },
    Progress {
        server: String,
        title: String,
        message: String,
    },
    Log {
        server: String,
        message: String,
    },
    Health {
        server: String,
        health: LspHealth,
    },
    Unsupported {
        server: String,
        method: String,
    },
    RequestFailed {
        server: String,
        generation: u64,
        method: String,
        message: String,
    },
    Stopped {
        server: String,
        success: bool,
    },
    Failed {
        server: String,
        message: String,
    },
}

#[derive(Clone, Debug)]
enum SessionCommand {
    Sync(LspDocument),
    Save(LspDocument),
    Close(String),
    Request {
        feature: LspFeature,
        document: LspDocument,
        offset: ByteOffset,
        generation: u64,
    },
    Cancel(u64),
    Health,
    RespondApplyEdit {
        request_id: ServerRequestId,
        applied: bool,
        reason: Option<String>,
    },
    Stop,
}

struct SessionHandle {
    sender: SyncSender<SessionCommand>,
    thread: Option<JoinHandle<()>>,
}

pub struct LspService {
    events: SyncSender<LspEvent>,
    receiver: Receiver<LspEvent>,
    sessions: BTreeMap<String, SessionHandle>,
}

impl LspService {
    #[must_use]
    pub fn new() -> Self {
        let (events, receiver) = mpsc::sync_channel(EVENT_QUEUE);
        Self {
            events,
            receiver,
            sessions: BTreeMap::new(),
        }
    }

    pub fn start(
        &mut self,
        definition: &ToolDefinition,
        root: PathBuf,
        document: Option<LspDocument>,
    ) -> Result<(), LspError> {
        self.start_with_policy(definition, root, document, LspPolicy::default())
    }

    pub fn start_with_policy(
        &mut self,
        definition: &ToolDefinition,
        root: PathBuf,
        document: Option<LspDocument>,
        policy: LspPolicy,
    ) -> Result<(), LspError> {
        self.start_session_with_policy(&definition.id, definition, root, document, policy)
    }
    pub fn start_session_with_policy(
        &mut self,
        session_key: &str,
        definition: &ToolDefinition,
        root: PathBuf,
        document: Option<LspDocument>,
        policy: LspPolicy,
    ) -> Result<(), LspError> {
        let _ = (session_key, definition, root, document, policy);
        Err(LspError::PreflightRequired)
    }
    pub fn start_session_absolute_with_policy(
        &mut self,
        session_key: &str,
        definition: &ToolDefinition,
        executable: PathBuf,
        root: PathBuf,
        document: Option<LspDocument>,
        policy: LspPolicy,
    ) -> Result<(), LspError> {
        if definition.kind != ToolKind::Lsp {
            return Err(LspError::NotLanguageServer(definition.id.clone()));
        }
        if self.sessions.contains_key(session_key) {
            return Err(LspError::AlreadyRunning(session_key.into()));
        }
        if !executable.is_absolute() {
            return Err(LspError::ExecutableMissing(
                executable.display().to_string(),
            ));
        }
        let (sender, commands) = mpsc::sync_channel(COMMAND_QUEUE);
        let events = self.events.clone();
        let server = session_key.to_owned();
        let args = definition.launch_args.clone();
        let executable_name = definition.executable.clone();
        send_event(
            &events,
            LspEvent::Starting {
                server: server.clone(),
            },
        );
        let thread = thread::Builder::new()
            .name(format!("dust-lsp-{server}"))
            .spawn(move || {
                run_session(
                    server,
                    executable,
                    executable_name,
                    args,
                    root,
                    document,
                    policy,
                    commands,
                    events,
                );
            })
            .map_err(LspError::Thread)?;
        self.sessions.insert(
            session_key.to_owned(),
            SessionHandle {
                sender,
                thread: Some(thread),
            },
        );
        Ok(())
    }

    pub fn sync(&self, server: &str, document: LspDocument) -> Result<(), LspError> {
        self.send(server, SessionCommand::Sync(document))
    }

    pub fn change(&self, server: &str, document: LspDocument) -> Result<(), LspError> {
        self.sync(server, document)
    }

    pub fn save(&self, server: &str, document: LspDocument) -> Result<(), LspError> {
        self.send(server, SessionCommand::Save(document))
    }

    pub fn close(&self, server: &str, uri: String) -> Result<(), LspError> {
        self.send(server, SessionCommand::Close(uri))
    }

    pub fn request(
        &self,
        server: &str,
        feature: LspFeature,
        document: LspDocument,
        offset: ByteOffset,
        generation: u64,
    ) -> Result<(), LspError> {
        self.send(
            server,
            SessionCommand::Request {
                feature,
                document,
                offset,
                generation,
            },
        )
    }

    pub fn cancel(&self, server: &str, generation: u64) -> Result<(), LspError> {
        self.send(server, SessionCommand::Cancel(generation))
    }

    pub fn health(&self, server: &str) -> Result<(), LspError> {
        self.send(server, SessionCommand::Health)
    }

    pub fn respond_apply_edit(
        &self,
        server: &str,
        request_id: ServerRequestId,
        applied: bool,
        reason: Option<String>,
    ) -> Result<(), LspError> {
        self.send(
            server,
            SessionCommand::RespondApplyEdit {
                request_id,
                applied,
                reason,
            },
        )
    }

    pub fn stop(&mut self, server: &str) -> Result<(), LspError> {
        let mut session = self
            .sessions
            .remove(server)
            .ok_or_else(|| LspError::NotRunning(server.into()))?;
        let _ = session.sender.try_send(SessionCommand::Stop);
        if let Some(thread) = session.thread.take() {
            let _ = thread.join();
        }
        Ok(())
    }

    #[must_use]
    pub fn running_servers(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    pub fn try_event(&self) -> Option<LspEvent> {
        self.receiver.try_recv().ok()
    }

    fn send(&self, server: &str, command: SessionCommand) -> Result<(), LspError> {
        self.sessions
            .get(server)
            .ok_or_else(|| LspError::NotRunning(server.into()))?
            .sender
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => LspError::Busy(server.into()),
                TrySendError::Disconnected(_) => LspError::NotRunning(server.into()),
            })
    }
}

impl Default for LspService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LspService {
    fn drop(&mut self) {
        for server in self.sessions.keys().cloned().collect::<Vec<_>>() {
            let _ = self.stop(&server);
        }
    }
}

#[derive(Clone)]
struct PendingRequest {
    feature: LspFeature,
    document: LspDocument,
    generation: u64,
    deadline: Instant,
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    server: String,
    executable: PathBuf,
    executable_name: String,
    args: Vec<String>,
    root: PathBuf,
    initial_document: Option<LspDocument>,
    policy: LspPolicy,
    commands: Receiver<SessionCommand>,
    events: SyncSender<LspEvent>,
) {
    let mut command = Command::new(&executable);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg0(executable_name);
    }
    let mut child = match command
        .args(args)
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            send_event(
                &events,
                LspEvent::Failed {
                    server,
                    message: error.to_string(),
                },
            );
            return;
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        fail_child(
            &mut child,
            &events,
            &server,
            "language server stdin was unavailable",
        );
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        fail_child(
            &mut child,
            &events,
            &server,
            "language server stdout was unavailable",
        );
        return;
    };
    let stderr = child.stderr.take();
    let (messages, message_receiver) = mpsc::sync_channel(MESSAGE_QUEUE);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let message = match read_content_length_message(&mut reader) {
                Ok(Some(message)) => Ok(message),
                Ok(None) => break,
                Err(error) => Err(error.to_string()),
            };
            if messages.try_send(message).is_err() {
                break;
            }
        }
    });
    let stderr_thread = stderr.map(|stderr| {
        let events = events.clone();
        let server = server.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr)
                .lines()
                .take(policy.maximum_log_lines.min(MAX_LOG_LINES))
            {
                let Ok(line) = line else {
                    break;
                };
                let message = sanitize_log(&line, MAX_LOG_LINE_BYTES);
                send_event(
                    &events,
                    LspEvent::Log {
                        server: server.clone(),
                        message,
                    },
                );
            }
        })
    });

    let initialize = initialize_message(&root);
    if let Err(error) = write_content_length_message(&mut stdin, &initialize) {
        send_event(
            &events,
            LspEvent::Failed {
                server: server.clone(),
                message: error.to_string(),
            },
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        if let Some(thread) = stderr_thread {
            let _ = thread.join();
        }
        return;
    }

    let mut initialized = false;
    let mut capabilities = LspCapabilities::default();
    let mut base_capabilities = LspCapabilities::default();
    let mut dynamic_registrations = HashMap::<String, String>::new();
    let mut documents = HashMap::<String, LspDocument>::new();
    if let Some(document) = initial_document {
        documents.insert(document.uri(), document);
    }
    let mut opened_documents = HashSet::new();
    let mut pending = HashMap::<i64, PendingRequest>::new();
    let mut next_request_id = 2_i64;
    let mut logs = VecDeque::<String>::with_capacity(MAX_LOG_LINES);
    let mut stopping = false;

    while !stopping {
        loop {
            match commands.try_recv() {
                Ok(command) => handle_command(
                    command,
                    &server,
                    &root,
                    &executable,
                    &mut stdin,
                    initialized,
                    &capabilities,
                    &mut documents,
                    &mut opened_documents,
                    &mut pending,
                    &mut next_request_id,
                    policy.request_timeout.min(Duration::from_secs(60)),
                    logs.len(),
                    &events,
                    &mut stopping,
                ),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stopping = true;
                    break;
                }
            }
        }
        match message_receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(message)) => {
                if message.get("id").and_then(Value::as_i64) == Some(1) && !initialized {
                    match initialize_capabilities(message.get("result")) {
                        Ok(negotiated) => {
                            base_capabilities = negotiated.clone();
                            capabilities = negotiated;
                        }
                        Err(error) => {
                            send_event(
                                &events,
                                LspEvent::Failed {
                                    server: server.clone(),
                                    message: error,
                                },
                            );
                            break;
                        }
                    }
                    initialized = true;
                    let _ = write_content_length_message(
                        &mut stdin,
                        &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
                    );
                    for document in documents.values() {
                        send_open(&mut stdin, document);
                        opened_documents.insert(document.uri());
                    }
                    send_event(
                        &events,
                        LspEvent::Running {
                            server: server.clone(),
                            capabilities: capabilities.clone(),
                        },
                    );
                } else if message.get("method").is_some() {
                    handle_server_message(
                        &server,
                        &mut stdin,
                        &message,
                        &events,
                        &mut logs,
                        &base_capabilities,
                        &mut capabilities,
                        &mut dynamic_registrations,
                    );
                } else if let Some(id) = message.get("id").and_then(Value::as_i64)
                    && let Some(request) = pending.remove(&id)
                {
                    handle_response(&server, request, message, &events);
                }
            }
            Ok(Err(error)) => {
                send_event(
                    &events,
                    LspEvent::Failed {
                        server: server.clone(),
                        message: error,
                    },
                );
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        expire_requests(&server, &mut stdin, &mut pending, &events);
        match child.try_wait() {
            Ok(Some(status)) => {
                send_event(
                    &events,
                    LspEvent::Stopped {
                        server: server.clone(),
                        success: status.success(),
                    },
                );
                stopping = true;
            }
            Ok(None) => {}
            Err(error) => {
                send_event(
                    &events,
                    LspEvent::Failed {
                        server: server.clone(),
                        message: error.to_string(),
                    },
                );
                stopping = true;
            }
        }
    }
    for id in pending.keys() {
        send_cancel(&mut stdin, *id);
    }
    for uri in opened_documents {
        send_close(&mut stdin, &uri);
    }
    if initialized {
        let _ = write_content_length_message(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":next_request_id,"method":"shutdown","params":null}),
        );
        let _ = write_content_length_message(
            &mut stdin,
            &json!({"jsonrpc":"2.0","method":"exit","params":null}),
        );
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    if let Some(thread) = stderr_thread {
        let _ = thread.join();
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    command: SessionCommand,
    server: &str,
    root: &Path,
    executable: &Path,
    stdin: &mut ChildStdin,
    initialized: bool,
    capabilities: &LspCapabilities,
    documents: &mut HashMap<String, LspDocument>,
    opened_documents: &mut HashSet<String>,
    pending: &mut HashMap<i64, PendingRequest>,
    next_request_id: &mut i64,
    request_timeout: Duration,
    recent_logs: usize,
    events: &SyncSender<LspEvent>,
    stopping: &mut bool,
) {
    match command {
        SessionCommand::Sync(document) => {
            let uri = document.uri();
            let previous = documents.insert(uri.clone(), document.clone());
            if initialized {
                if opened_documents.insert(uri) {
                    send_open(stdin, &document);
                } else {
                    send_change(stdin, previous.as_ref(), &document, capabilities);
                }
            }
        }
        SessionCommand::Save(document) => {
            let uri = document.uri();
            let previous = documents.insert(uri.clone(), document.clone());
            if initialized {
                if opened_documents.insert(uri) {
                    send_open(stdin, &document);
                } else if previous
                    .as_ref()
                    .is_some_and(|old| old.version != document.version)
                {
                    send_change(stdin, previous.as_ref(), &document, capabilities);
                }
                send_save(stdin, &document);
            }
        }
        SessionCommand::Close(uri) => {
            documents.remove(&uri);
            if initialized && opened_documents.remove(&uri) {
                send_close(stdin, &uri);
            }
        }
        SessionCommand::Request {
            feature,
            document,
            offset,
            generation,
        } => {
            if !initialized {
                send_event(
                    events,
                    LspEvent::RequestFailed {
                        server: server.into(),
                        generation,
                        method: feature.method().into(),
                        message: "language server is still initializing".into(),
                    },
                );
                return;
            }
            if !feature.capability(capabilities) {
                send_event(
                    events,
                    LspEvent::Unsupported {
                        server: server.into(),
                        method: feature.method().into(),
                    },
                );
                return;
            }
            if pending.len() >= MAX_PENDING_REQUESTS {
                send_event(
                    events,
                    LspEvent::RequestFailed {
                        server: server.into(),
                        generation,
                        method: feature.method().into(),
                        message: "language-server pending request limit reached".into(),
                    },
                );
                return;
            }
            if let Some(key) = feature.replace_key() {
                let obsolete = pending
                    .iter()
                    .filter_map(|(id, request)| {
                        (request.document.uri() == document.uri()
                            && request.feature.replace_key() == Some(key))
                        .then_some(*id)
                    })
                    .collect::<Vec<_>>();
                for id in obsolete {
                    pending.remove(&id);
                    send_cancel(stdin, id);
                }
            }
            let buffer = TextBuffer::from_text(&document.text);
            let Ok((line, character)) =
                PositionCodec::new(&buffer).byte_to_lsp(offset, capabilities.position_encoding)
            else {
                send_event(
                    events,
                    LspEvent::RequestFailed {
                        server: server.into(),
                        generation,
                        method: feature.method().into(),
                        message: "request position is invalid for the captured document".into(),
                    },
                );
                return;
            };
            let id = *next_request_id;
            *next_request_id = next_request_id.saturating_add(1);
            let params = feature_params(&feature, &document.uri(), line, character);
            let message = json!({
                "jsonrpc":"2.0", "id":id, "method":feature.method(), "params":params
            });
            if write_content_length_message(stdin, &message).is_ok() {
                pending.insert(
                    id,
                    PendingRequest {
                        feature,
                        document,
                        generation,
                        deadline: Instant::now() + request_timeout,
                    },
                );
            }
        }
        SessionCommand::Cancel(generation) => {
            let ids = pending
                .iter()
                .filter_map(|(id, request)| (request.generation == generation).then_some(*id))
                .collect::<Vec<_>>();
            for id in ids {
                pending.remove(&id);
                send_cancel(stdin, id);
            }
        }
        SessionCommand::RespondApplyEdit {
            request_id,
            applied,
            reason,
        } => {
            let result = if applied {
                json!({"applied":true})
            } else {
                json!({"applied":false,"failureReason":reason.unwrap_or_else(||"edit rejected".into())})
            };
            let _ = write_content_length_message(
                stdin,
                &json!({"jsonrpc":"2.0","id":request_id.value(),"result":result}),
            );
        }
        SessionCommand::Health => send_event(
            events,
            LspEvent::Health {
                server: server.into(),
                health: LspHealth {
                    root: root.to_path_buf(),
                    executable: executable.to_path_buf(),
                    capabilities: capabilities.clone(),
                    attached_documents: opened_documents.len(),
                    pending_requests: pending.len(),
                    recent_logs,
                },
            },
        ),
        SessionCommand::Stop => *stopping = true,
    }
}

fn initialize_message(root: &Path) -> Value {
    json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{
            "processId":std::process::id(),
            "clientInfo":{"name":"dust","version":env!("CARGO_PKG_VERSION")},
            "rootUri":file_uri(root),
            "workspaceFolders":[{"uri":file_uri(root),"name":root.file_name().and_then(|v|v.to_str()).unwrap_or("workspace")}],
            "capabilities":{
                "general":{"positionEncodings":["utf-8","utf-16","utf-32"]},
                "workspace":{"workspaceFolders":true,"configuration":true,"applyEdit":true,"symbol":{"dynamicRegistration":true}},
                "window":{"workDoneProgress":true},
                "textDocument":{
                    "synchronization":{"didSave":true,"dynamicRegistration":true},
                    "completion":{"dynamicRegistration":true,"completionItem":{"snippetSupport":true,"documentationFormat":["markdown","plaintext"]}},
                    "hover":{"dynamicRegistration":true,"contentFormat":["markdown","plaintext"]},
                    "signatureHelp":{"dynamicRegistration":true},
                    "definition":{"dynamicRegistration":true,"linkSupport":true},
                    "declaration":{"dynamicRegistration":true,"linkSupport":true},
                    "typeDefinition":{"dynamicRegistration":true,"linkSupport":true},
                    "implementation":{"dynamicRegistration":true,"linkSupport":true},
                    "references":{"dynamicRegistration":true},
                    "documentSymbol":{"dynamicRegistration":true,"hierarchicalDocumentSymbolSupport":true},
                    "publishDiagnostics":{"relatedInformation":true,"versionSupport":true},
                    "rename":{"dynamicRegistration":true,"prepareSupport":false},
                    "formatting":{"dynamicRegistration":true},
                    "rangeFormatting":{"dynamicRegistration":true},
                    "codeAction":{"dynamicRegistration":true,"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","refactor","source"]}}}
                }
            }
        }
    })
}

fn initialize_capabilities(result: Option<&Value>) -> Result<LspCapabilities, String> {
    let capabilities = result
        .and_then(|value| value.get("capabilities"))
        .ok_or_else(|| "initialize response has no capabilities".to_owned())?;
    let encoding = capabilities
        .get("positionEncoding")
        .and_then(Value::as_str)
        .and_then(PositionEncoding::parse_lsp)
        .unwrap_or_default();
    let sync = capabilities.get("textDocumentSync");
    let sync_value = sync
        .and_then(Value::as_u64)
        .or_else(|| {
            sync.and_then(|value| value.get("change"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(1);
    let sync_kind = match sync_value {
        0 => SyncKind::None,
        2 => SyncKind::Incremental,
        _ => SyncKind::Full,
    };
    Ok(LspCapabilities {
        position_encoding: encoding,
        sync_kind,
        completion: capability(capabilities, "completionProvider"),
        completion_resolve: capabilities
            .get("completionProvider")
            .and_then(|value| value.get("resolveProvider"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        hover: capability(capabilities, "hoverProvider"),
        signature_help: capability(capabilities, "signatureHelpProvider"),
        definition: capability(capabilities, "definitionProvider"),
        declaration: capability(capabilities, "declarationProvider"),
        type_definition: capability(capabilities, "typeDefinitionProvider"),
        implementation: capability(capabilities, "implementationProvider"),
        references: capability(capabilities, "referencesProvider"),
        document_symbols: capability(capabilities, "documentSymbolProvider"),
        workspace_symbols: capability(capabilities, "workspaceSymbolProvider"),
        rename: capability(capabilities, "renameProvider"),
        formatting: capability(capabilities, "documentFormattingProvider"),
        range_formatting: capability(capabilities, "documentRangeFormattingProvider"),
        code_action: capability(capabilities, "codeActionProvider"),
        semantic_tokens: capability(capabilities, "semanticTokensProvider"),
        inlay_hints: capability(capabilities, "inlayHintProvider"),
        pull_diagnostics: capability(capabilities, "diagnosticProvider"),
        code_lens: capability(capabilities, "codeLensProvider"),
        document_highlights: capability(capabilities, "documentHighlightProvider"),
        folding_ranges: capability(capabilities, "foldingRangeProvider"),
        selection_ranges: capability(capabilities, "selectionRangeProvider"),
        semantic_token_types: capabilities
            .pointer("/semanticTokensProvider/legend/tokenTypes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(256)
            .filter_map(Value::as_str)
            .map(|value| value.chars().take(128).collect())
            .collect(),
    })
}

fn capability(capabilities: &Value, key: &str) -> bool {
    match capabilities.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::Object(_)) => true,
        _ => false,
    }
}

fn send_open(stdin: &mut ChildStdin, document: &LspDocument) {
    let _ = write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":document.uri(),"languageId":document.language_id,"version":document.version,"text":document.text}}
        }),
    );
}

fn send_change(
    stdin: &mut ChildStdin,
    previous: Option<&LspDocument>,
    document: &LspDocument,
    capabilities: &LspCapabilities,
) {
    if capabilities.sync_kind == SyncKind::None {
        return;
    }
    let content_changes = if capabilities.sync_kind == SyncKind::Incremental {
        previous
            .and_then(|old| incremental_change(old, document, capabilities.position_encoding))
            .map_or_else(|| json!([{"text":document.text}]), |change| json!([change]))
    } else {
        json!([{"text":document.text}])
    };
    let _ = write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didChange",
            "params":{"textDocument":{"uri":document.uri(),"version":document.version},"contentChanges":content_changes}
        }),
    );
}

fn incremental_change(
    old: &LspDocument,
    new: &LspDocument,
    encoding: PositionEncoding,
) -> Option<Value> {
    let prefix = old
        .text
        .bytes()
        .zip(new.text.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let mut prefix = prefix;
    while prefix > 0 && (!old.text.is_char_boundary(prefix) || !new.text.is_char_boundary(prefix)) {
        prefix -= 1;
    }
    let old_tail = &old.text[prefix..];
    let new_tail = &new.text[prefix..];
    let suffix = old_tail
        .bytes()
        .rev()
        .zip(new_tail.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(old_tail.len())
        .min(new_tail.len());
    let mut old_end = old.text.len().saturating_sub(suffix);
    let mut new_end = new.text.len().saturating_sub(suffix);
    while old_end < old.text.len() && !old.text.is_char_boundary(old_end) {
        old_end += 1;
    }
    while new_end < new.text.len() && !new.text.is_char_boundary(new_end) {
        new_end += 1;
    }
    if old_end < prefix || new_end < prefix {
        return None;
    }
    let old_buffer = TextBuffer::from_text(&old.text);
    let codec = PositionCodec::new(&old_buffer);
    let (start_line, start_character) = codec.byte_to_lsp(ByteOffset(prefix), encoding).ok()?;
    let (end_line, end_character) = codec.byte_to_lsp(ByteOffset(old_end), encoding).ok()?;
    Some(json!({
        "range":{"start":{"line":start_line,"character":start_character},"end":{"line":end_line,"character":end_character}},
        "rangeLength": protocol_units(&old.text[prefix..old_end], encoding),
        "text":new.text[prefix..new_end]
    }))
}

fn protocol_units(text: &str, encoding: PositionEncoding) -> usize {
    match encoding {
        PositionEncoding::Utf8 => text.len(),
        PositionEncoding::Utf16 => text.encode_utf16().count(),
        PositionEncoding::Utf32 => text.chars().count(),
    }
}

fn send_save(stdin: &mut ChildStdin, document: &LspDocument) {
    let _ = write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didSave",
            "params":{"textDocument":{"uri":document.uri()},"text":document.text}
        }),
    );
}

fn send_close(stdin: &mut ChildStdin, uri: &str) {
    let _ = write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri}}
        }),
    );
}

fn send_cancel(stdin: &mut ChildStdin, id: i64) {
    let _ = write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}
        }),
    );
}

fn feature_params(feature: &LspFeature, uri: &str, line: usize, character: usize) -> Value {
    let document = json!({"uri":uri});
    let position = json!({"line":line,"character":character});
    match feature {
        LspFeature::CompletionResolve { item } => item.clone(),
        LspFeature::WorkspaceSymbols { query } => json!({"query":query}),
        LspFeature::DocumentSymbols | LspFeature::Formatting => json!({
            "textDocument":document,"options":{"tabSize":4,"insertSpaces":true}
        }),
        LspFeature::References => json!({
            "textDocument":document,"position":position,"context":{"includeDeclaration":true}
        }),
        LspFeature::Rename { new_name } => json!({
            "textDocument":document,"position":position,"newName":new_name
        }),
        LspFeature::RangeFormatting { range } => json!({
            "textDocument":document,"range":range_json(range),"options":{"tabSize":4,"insertSpaces":true}
        }),
        LspFeature::CodeAction { range } => json!({
            "textDocument":document,"range":range_json(range),"context":{"diagnostics":[]}
        }),
        LspFeature::PullDiagnostics { previous_result_id } => json!({
            "textDocument":document,"previousResultId":previous_result_id
        }),
        LspFeature::SemanticTokensDelta { previous_result_id } => {
            json!({"textDocument":document,"previousResultId":previous_result_id})
        }
        LspFeature::SemanticTokensFull | LspFeature::CodeLens | LspFeature::FoldingRanges => {
            json!({"textDocument":document})
        }
        LspFeature::InlayHints { range } => {
            json!({"textDocument":document,"range":range_json(range)})
        }
        LspFeature::SelectionRanges => json!({"textDocument":document,"positions":[position]}),
        _ => json!({"textDocument":document,"position":position}),
    }
}

fn range_json(range: &LspRange) -> Value {
    json!({"start":{"line":range.start.line,"character":range.start.character},
        "end":{"line":range.end.line,"character":range.end.character}})
}

fn handle_response(
    server: &str,
    request: PendingRequest,
    message: Value,
    events: &SyncSender<LspEvent>,
) {
    if let Some(error) = message.get("error") {
        send_event(
            events,
            LspEvent::RequestFailed {
                server: server.into(),
                generation: request.generation,
                method: request.feature.method().into(),
                message: error.to_string(),
            },
        );
        return;
    }
    let result = message.get("result").unwrap_or(&Value::Null);
    let normalized = normalize_result(&request.feature, result);
    match normalized {
        Ok(result) => send_event(
            events,
            LspEvent::Result {
                server: server.into(),
                generation: request.generation,
                method: request.feature.method().into(),
                uri: request.document.uri(),
                version: request.document.version,
                result,
            },
        ),
        Err(message) => send_event(
            events,
            LspEvent::RequestFailed {
                server: server.into(),
                generation: request.generation,
                method: request.feature.method().into(),
                message,
            },
        ),
    }
}

fn normalize_result(feature: &LspFeature, result: &Value) -> Result<LspFeatureResult, String> {
    if result.is_null() {
        return Ok(LspFeatureResult::Empty);
    }
    match feature {
        LspFeature::Hover => Ok(LspFeatureResult::Hover(hover_text(Some(result)))),
        LspFeature::Completion => Ok(LspFeatureResult::Completion(completion_items(result)?)),
        LspFeature::CompletionResolve { .. } => Ok(LspFeatureResult::CompletionResolved(
            completion_item(result, true)?,
        )),
        LspFeature::SignatureHelp => Ok(LspFeatureResult::Signature(signature_text(result))),
        LspFeature::Definition
        | LspFeature::Declaration
        | LspFeature::TypeDefinition
        | LspFeature::Implementation
        | LspFeature::References => Ok(LspFeatureResult::Locations(locations(result)?)),
        LspFeature::DocumentSymbols | LspFeature::WorkspaceSymbols { .. } => {
            Ok(LspFeatureResult::Symbols(symbols(result)?))
        }
        LspFeature::Rename { .. } => Ok(LspFeatureResult::WorkspaceEdit(workspace_edit(result)?)),
        LspFeature::Formatting | LspFeature::RangeFormatting { .. } => {
            Ok(LspFeatureResult::WorkspaceEdit(LspWorkspaceEdit {
                resources: Vec::new(),
                documents: vec![LspDocumentEdit {
                    uri: String::new(),
                    version: None,
                    edits: text_edits(result)?,
                }],
            }))
        }
        LspFeature::CodeAction { .. } => Ok(LspFeatureResult::CodeActions(code_actions(result)?)),
        LspFeature::PullDiagnostics { .. } => {
            Ok(LspFeatureResult::PullDiagnostics(LspPullDiagnostics {
                result_id: result
                    .get("resultId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                items: diagnostics(result.get("items").unwrap_or(&Value::Null)),
                unchanged: result.get("kind").and_then(Value::as_str) == Some("unchanged"),
            }))
        }
        LspFeature::SemanticTokensFull => {
            let values = result
                .get("data")
                .and_then(Value::as_array)
                .ok_or("semantic token data is not an array")?;
            if values.len() > 500_000 || values.len() % 5 != 0 {
                return Err("semantic token data has invalid tuple count".into());
            }
            let data = values
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or("semantic token value is invalid")
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(LspFeatureResult::SemanticTokens(LspSemanticTokens {
                result_id: result
                    .get("resultId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                data,
            }))
        }
        LspFeature::SemanticTokensDelta { .. } => {
            let edits = result
                .get("edits")
                .and_then(Value::as_array)
                .ok_or("semantic token delta edits are not an array")?;
            if edits.len() > 10_000 {
                return Err("semantic token delta exceeds edit ceiling".into());
            }
            let mut normalized = Vec::new();
            for edit in edits {
                let start = usize::try_from(
                    edit.get("start")
                        .and_then(Value::as_u64)
                        .ok_or("semantic delta start missing")?,
                )
                .map_err(|_| "semantic delta start invalid")?;
                let delete_count = usize::try_from(
                    edit.get("deleteCount")
                        .and_then(Value::as_u64)
                        .ok_or("semantic delta delete count missing")?,
                )
                .map_err(|_| "semantic delta delete count invalid")?;
                let data = edit
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|value| {
                        value
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .ok_or("semantic delta value invalid")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if data.len() % 5 != 0 {
                    return Err("semantic delta insert is not token tuples".into());
                }
                normalized.push(LspSemanticTokenEdit {
                    start,
                    delete_count,
                    data,
                });
            }
            Ok(LspFeatureResult::SemanticTokenDelta(
                LspSemanticTokenDelta {
                    result_id: result
                        .get("resultId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    edits: normalized,
                },
            ))
        }
        LspFeature::InlayHints { .. } => Ok(LspFeatureResult::InlayHints(inlay_hints(result))),
        LspFeature::CodeLens => Ok(LspFeatureResult::CodeLens(code_lenses(result))),
        LspFeature::DocumentHighlights => Ok(LspFeatureResult::Ranges(result_ranges(result))),
        LspFeature::FoldingRanges => Ok(LspFeatureResult::Ranges(folding_ranges(result))),
        LspFeature::SelectionRanges => Ok(LspFeatureResult::Ranges(selection_ranges(result))),
    }
}

fn result_ranges(value: &Value) -> Vec<LspRange> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| parse_range(item.get("range").unwrap_or(item)))
        .collect()
}
fn folding_ranges(value: &Value) -> Vec<LspRange> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| {
            Some(LspRange {
                start: LspPosition {
                    line: usize::try_from(item.get("startLine")?.as_u64()?).ok()?,
                    character: usize::try_from(
                        item.get("startCharacter")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .ok()?,
                },
                end: LspPosition {
                    line: usize::try_from(item.get("endLine")?.as_u64()?).ok()?,
                    character: usize::try_from(
                        item.get("endCharacter")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .ok()?,
                },
            })
        })
        .collect()
}
fn selection_ranges(value: &Value) -> Vec<LspRange> {
    let mut out = Vec::new();
    for item in value.as_array().into_iter().flatten().take(10_000) {
        let mut current = Some(item);
        for _ in 0..64 {
            let Some(value) = current else { break };
            if let Some(range) = value.get("range").and_then(parse_range) {
                out.push(range);
                if out.len() >= 10_000 {
                    return out;
                }
            }
            current = value.get("parent");
        }
    }
    out
}
fn inlay_hints(value: &Value) -> Vec<LspInlayHint> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| {
            let value = item.get("label")?;
            let label = if let Some(text) = value.as_str() {
                text.to_owned()
            } else {
                value
                    .as_array()?
                    .iter()
                    .filter_map(|part| part.get("value").and_then(Value::as_str))
                    .collect()
            };
            Some(LspInlayHint {
                position: parse_position(item.get("position")?)?,
                label: sanitize_log(&label, 4096),
            })
        })
        .collect()
}
fn code_lenses(value: &Value) -> Vec<LspCodeLens> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| {
            let command = item.get("command");
            Some(LspCodeLens {
                range: parse_range(item.get("range")?)?,
                title: command
                    .and_then(|value| value.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .chars()
                    .take(4096)
                    .collect(),
                command: command
                    .and_then(|value| value.get("command"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

fn completion_items(result: &Value) -> Result<Vec<LspCompletionItem>, String> {
    let items = result
        .as_array()
        .or_else(|| result.get("items").and_then(Value::as_array))
        .ok_or_else(|| "completion response is not an item list".to_owned())?;
    items
        .iter()
        .take(512)
        .map(|item| completion_item(item, false))
        .collect()
}
fn completion_item(item: &Value, resolved: bool) -> Result<LspCompletionItem, String> {
    let encoded = serde_json::to_vec(item).map_err(|_| "completion item is not serializable")?;
    if encoded.len() > 64 * 1024 {
        return Err("completion item exceeds byte ceiling".into());
    }
    let label = item
        .get("label")
        .and_then(Value::as_str)
        .ok_or("completion item has no label")?
        .chars()
        .take(4096)
        .collect::<String>();
    let insert_text = item
        .get("insertText")
        .and_then(Value::as_str)
        .unwrap_or(&label)
        .chars()
        .take(1024 * 1024)
        .collect();
    let documentation = item
        .get("documentation")
        .and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("value").and_then(Value::as_str))
        })
        .map(|source| sanitize_markdown(source, 64 * 1024))
        .unwrap_or_default()
        .chars()
        .take(64 * 1024)
        .collect();
    Ok(LspCompletionItem {
        label,
        detail: item
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(4096)
            .collect(),
        insert_text,
        text_edit: item.get("textEdit").and_then(parse_text_edit),
        additional_edits: item
            .get("additionalTextEdits")
            .map(text_edits)
            .transpose()?
            .unwrap_or_default(),
        snippet: item.get("insertTextFormat").and_then(Value::as_u64) == Some(2),
        kind: item
            .get("kind")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok()),
        documentation,
        resolve_item: (!resolved).then(|| item.clone()),
        resolved,
    })
}

fn locations(result: &Value) -> Result<Vec<LspLocation>, String> {
    let values = result
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(result));
    values
        .iter()
        .take(2048)
        .map(|value| {
            let target = value.get("targetUri").map_or_else(
                || value.clone(),
                |uri| json!({
                    "uri":uri,"range":value.get("targetSelectionRange").or_else(||value.get("targetRange"))
                }),
            );
            let uri = target
                .get("uri")
                .and_then(Value::as_str)
                .ok_or("location has no URI")?
                .to_owned();
            let range = parse_range(target.get("range").ok_or("location has no range")?)
                .ok_or("location range is malformed")?;
            Ok(LspLocation { uri, range })
        })
        .collect()
}

fn symbols(result: &Value) -> Result<Vec<LspSymbol>, String> {
    let values = result
        .as_array()
        .ok_or_else(|| "symbol response is not a list".to_owned())?;
    values
        .iter()
        .take(4096)
        .map(|value| {
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .ok_or("symbol has no name")?
                .to_owned();
            let location = value
                .get("location")
                .and_then(|location| locations(location).ok()?.into_iter().next());
            Ok(LspSymbol {
                name,
                kind: value
                    .get("kind")
                    .and_then(Value::as_u64)
                    .and_then(|v| u8::try_from(v).ok()),
                location,
            })
        })
        .collect()
}

fn workspace_edit(value: &Value) -> Result<LspWorkspaceEdit, String> {
    let mut documents = Vec::new();
    let mut resources = Vec::new();
    if let Some(changes) = value.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes.iter().take(256) {
            documents.push(LspDocumentEdit {
                uri: uri.clone(),
                version: None,
                edits: text_edits(edits)?,
            });
        }
    }
    if let Some(changes) = value.get("documentChanges").and_then(Value::as_array) {
        for change in changes.iter().take(256) {
            let Some(text_document) = change.get("textDocument") else {
                let kind = change
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let options = change.get("options").unwrap_or(&Value::Null);
                let operation = match kind {
                    "create" => change.get("uri").and_then(Value::as_str).map(|uri| {
                        LspResourceOperation::Create {
                            uri: uri.into(),
                            overwrite: options
                                .get("overwrite")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            ignore_if_exists: options
                                .get("ignoreIfExists")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        }
                    }),
                    "rename" => Some(LspResourceOperation::Rename {
                        old_uri: change
                            .get("oldUri")
                            .and_then(Value::as_str)
                            .ok_or("rename has no oldUri")?
                            .into(),
                        new_uri: change
                            .get("newUri")
                            .and_then(Value::as_str)
                            .ok_or("rename has no newUri")?
                            .into(),
                        overwrite: options
                            .get("overwrite")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        ignore_if_exists: options
                            .get("ignoreIfExists")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }),
                    "delete" => change.get("uri").and_then(Value::as_str).map(|uri| {
                        LspResourceOperation::Delete {
                            uri: uri.into(),
                            recursive: options
                                .get("recursive")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            ignore_if_not_exists: options
                                .get("ignoreIfNotExists")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        }
                    }),
                    _ => None,
                };
                if let Some(operation) = operation {
                    resources.push(operation);
                }
                continue;
            };
            let uri = text_document
                .get("uri")
                .and_then(Value::as_str)
                .ok_or("document edit has no URI")?;
            documents.push(LspDocumentEdit {
                uri: uri.into(),
                version: text_document.get("version").and_then(Value::as_u64),
                edits: text_edits(change.get("edits").unwrap_or(&Value::Null))?,
            });
        }
    }
    Ok(LspWorkspaceEdit {
        documents,
        resources,
    })
}

fn code_actions(value: &Value) -> Result<Vec<LspCodeAction>, String> {
    let actions = value
        .as_array()
        .ok_or_else(|| "code-action response is not a list".to_owned())?;
    let mut normalized = Vec::new();
    for action in actions.iter().take(256) {
        let Some(title) = action.get("title").and_then(Value::as_str) else {
            continue;
        };
        normalized.push(LspCodeAction {
            title: title.to_owned(),
            kind: action
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_owned),
            preferred: action
                .get("isPreferred")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            edit: action.get("edit").map(workspace_edit).transpose()?,
        });
    }
    Ok(normalized)
}

fn text_edits(value: &Value) -> Result<Vec<LspTextEdit>, String> {
    let values = value
        .as_array()
        .ok_or_else(|| "text edits are not a list".to_owned())?;
    values
        .iter()
        .take(4096)
        .map(|value| parse_text_edit(value).ok_or_else(|| "text edit is malformed".to_owned()))
        .collect()
}

fn parse_text_edit(value: &Value) -> Option<LspTextEdit> {
    Some(LspTextEdit {
        range: parse_range(value.get("range").or_else(|| value.get("replace"))?)?,
        new_text: value.get("newText")?.as_str()?.to_owned(),
    })
}

fn parse_range(value: &Value) -> Option<LspRange> {
    Some(LspRange {
        start: parse_position(value.get("start")?)?,
        end: parse_position(value.get("end")?)?,
    })
}

fn parse_position(value: &Value) -> Option<LspPosition> {
    Some(LspPosition {
        line: usize::try_from(value.get("line")?.as_u64()?).ok()?,
        character: usize::try_from(value.get("character")?.as_u64()?).ok()?,
    })
}

fn diagnostics(value: &Value) -> Vec<LspDiagnostic> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(4096)
        .filter_map(|diagnostic| {
            Some(LspDiagnostic {
                range: parse_range(diagnostic.get("range")?)?,
                severity: diagnostic
                    .get("severity")
                    .and_then(Value::as_u64)
                    .and_then(|v| u8::try_from(v).ok()),
                code: diagnostic
                    .get("code")
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)),
                source: diagnostic
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: sanitize_log(
                    diagnostic
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    MAX_LOG_LINE_BYTES,
                ),
            })
        })
        .collect()
}

fn handle_server_message(
    server: &str,
    stdin: &mut ChildStdin,
    message: &Value,
    events: &SyncSender<LspEvent>,
    logs: &mut VecDeque<String>,
    base_capabilities: &LspCapabilities,
    capabilities: &mut LspCapabilities,
    registrations: &mut HashMap<String, String>,
) {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "textDocument/publishDiagnostics" => {
            let uri = message
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let version = message.pointer("/params/version").and_then(Value::as_u64);
            let diagnostics = diagnostics(
                message
                    .pointer("/params/diagnostics")
                    .unwrap_or(&Value::Null),
            );
            send_event(
                events,
                LspEvent::Diagnostics {
                    server: server.into(),
                    uri: uri.into(),
                    version,
                    diagnostics,
                },
            );
        }
        "$/progress" => {
            let value = message.pointer("/params/value").unwrap_or(&Value::Null);
            send_event(
                events,
                LspEvent::Progress {
                    server: server.into(),
                    title: value
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    message: value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                },
            );
        }
        "window/logMessage" | "window/showMessage" => {
            let text = sanitize_log(
                message
                    .pointer("/params/message")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                MAX_LOG_LINE_BYTES,
            );
            if logs.len() == MAX_LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(text.clone());
            send_event(
                events,
                LspEvent::Log {
                    server: server.into(),
                    message: text,
                },
            );
        }
        _ => {}
    }
    if method == "client/registerCapability" || method == "client/unregisterCapability" {
        let parsed = if method == "client/registerCapability" {
            message
                .pointer("/params/registrations")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .take(256)
                        .filter_map(|item| {
                            Some((
                                item.get("id")?.as_str()?.to_owned(),
                                item.get("method")?.as_str()?.to_owned(),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|items| !items.is_empty())
                .map(|items| {
                    for (id, method) in items {
                        registrations.insert(id, method);
                    }
                })
        } else {
            message
                .pointer("/params/unregisterations")
                .or_else(|| message.pointer("/params/unregistrations"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .take(256)
                        .filter_map(|item| item.get("id").and_then(Value::as_str))
                        .for_each(|id| {
                            registrations.remove(id);
                        })
                })
        };
        if let Some(id) = message.get("id") {
            let response = if parsed.is_some() {
                *capabilities = effective_capabilities(
                    base_capabilities,
                    registrations.values().map(String::as_str),
                );
                send_event(
                    events,
                    LspEvent::CapabilitiesChanged {
                        server: server.into(),
                        capabilities: capabilities.clone(),
                    },
                );
                json!({"jsonrpc":"2.0","id":id,"result":null})
            } else {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"malformed dynamic registration"}})
            };
            let _ = write_content_length_message(stdin, &response);
        }
        return;
    }
    if method == "workspace/applyEdit" {
        let request_id = message.get("id").and_then(ServerRequestId::from_value);
        let edit = message
            .pointer("/params/edit")
            .map(workspace_edit)
            .transpose();
        match (request_id, edit) {
            (Some(request_id), Ok(Some(edit))) => send_event(
                events,
                LspEvent::ApplyEditRequest {
                    server: server.into(),
                    request_id,
                    label: message
                        .pointer("/params/label")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    edit,
                },
            ),
            _ => {
                if let Some(id) = message.get("id") {
                    let _ = write_content_length_message(
                        stdin,
                        &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"malformed workspace edit"}}),
                    );
                }
            }
        }
        return;
    }
    if let Some(id) = message.get("id") {
        let response = match method {
            "workspace/configuration" => json!([]),
            "workspace/workspaceFolders" => Value::Null,
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability" => Value::Null,
            _ => {
                let _ = write_content_length_message(
                    stdin,
                    &json!({
                        "jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not implemented by Dust"}
                    }),
                );
                return;
            }
        };
        let _ = write_content_length_message(
            stdin,
            &json!({"jsonrpc":"2.0","id":id,"result":response}),
        );
    }
}

fn effective_capabilities<'a>(
    base: &LspCapabilities,
    methods: impl Iterator<Item = &'a str>,
) -> LspCapabilities {
    let mut value = base.clone();
    for method in methods {
        match method {
            "textDocument/diagnostic" => value.pull_diagnostics = true,
            "textDocument/semanticTokens" | "textDocument/semanticTokens/full" => {
                value.semantic_tokens = true
            }
            "textDocument/inlayHint" => value.inlay_hints = true,
            "textDocument/codeLens" => value.code_lens = true,
            "textDocument/documentHighlight" => value.document_highlights = true,
            "textDocument/foldingRange" => value.folding_ranges = true,
            "textDocument/selectionRange" => value.selection_ranges = true,
            "textDocument/completion" => value.completion = true,
            "textDocument/hover" => value.hover = true,
            _ => {}
        }
    }
    value
}

fn expire_requests(
    server: &str,
    stdin: &mut ChildStdin,
    pending: &mut HashMap<i64, PendingRequest>,
    events: &SyncSender<LspEvent>,
) {
    let now = Instant::now();
    let expired = pending
        .iter()
        .filter_map(|(id, request)| (request.deadline <= now).then_some(*id))
        .collect::<Vec<_>>();
    for id in expired {
        if let Some(request) = pending.remove(&id) {
            send_cancel(stdin, id);
            send_event(
                events,
                LspEvent::RequestFailed {
                    server: server.into(),
                    generation: request.generation,
                    method: request.feature.method().into(),
                    message: "language-server request timed out".into(),
                },
            );
        }
    }
}

fn signature_text(result: &Value) -> String {
    result
        .pointer("/signatures/0/label")
        .and_then(Value::as_str)
        .unwrap_or("no signature information")
        .to_owned()
}

fn hover_text(result: Option<&Value>) -> String {
    let Some(result) = result else {
        return "no hover information".into();
    };
    let contents = result.get("contents").unwrap_or(result);
    if let Some(text) = contents.as_str() {
        return sanitize_log(text, MAX_LOG_LINE_BYTES);
    }
    if let Some(text) = contents.get("value").and_then(Value::as_str) {
        return sanitize_log(text, MAX_LOG_LINE_BYTES);
    }
    if let Some(parts) = contents.as_array() {
        return parts
            .iter()
            .filter_map(|part| {
                part.as_str()
                    .or_else(|| part.get("value").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    contents.to_string()
}

fn sanitize_log(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(limit)
        .collect()
}

fn send_event(events: &SyncSender<LspEvent>, event: LspEvent) {
    let _ = events.try_send(event);
}

fn fail_child(
    child: &mut std::process::Child,
    events: &SyncSender<LspEvent>,
    server: &str,
    message: &str,
) {
    send_event(
        events,
        LspEvent::Failed {
            server: server.into(),
            message: message.into(),
        },
    );
    let _ = child.kill();
    let _ = child.wait();
}

#[must_use]
pub fn language_id_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some("rust"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("typescriptreact"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("javascriptreact"),
        "py" => Some("python"),
        "go" => Some("go"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" => Some("cpp"),
        "json" | "jsonc" => Some("json"),
        "toml" => Some("toml"),
        "html" => Some("html"),
        "css" => Some("css"),
        "md" | "markdown" => Some("markdown"),
        _ => None,
    }
}

#[must_use]
pub fn file_uri(path: &Path) -> String {
    let mut encoded = String::new();
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("file://{encoded}")
}

#[must_use]
pub fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let source = uri.strip_prefix("file://")?;
    let bytes = source.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let value = u8::from_str_radix(
                std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?,
                16,
            )
            .ok()?;
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok().map(PathBuf::from)
}

#[derive(Debug, Error)]
pub enum LspError {
    #[error("tool `{0}` is not a language server")]
    NotLanguageServer(String),
    #[error("language server `{0}` is already running")]
    AlreadyRunning(String),
    #[error("language server `{0}` is not running")]
    NotRunning(String),
    #[error("language server `{0}` is busy")]
    Busy(String),
    #[error("language server executable `{0}` is not installed")]
    ExecutableMissing(String),
    #[error(
        "language server launch requires an absolute executable from ExecutionPreflightService"
    )]
    PreflightRequired,
    #[error("failed to create language-server thread: {0}")]
    Thread(#[source] std::io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetTabstop {
    pub index: u32,
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSnippet {
    pub text: String,
    pub first_tabstop: Option<usize>,
    pub tabstops: Vec<SnippetTabstop>,
}
pub fn parse_snippet(source: &str, maximum_bytes: usize) -> Result<ParsedSnippet, String> {
    if source.len() > maximum_bytes {
        return Err("snippet exceeds byte ceiling".into());
    }
    let bytes = source.as_bytes();
    let mut index = 0usize;
    let mut text = String::new();
    let mut stops = Vec::new();
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 1 < bytes.len()
            && matches!(bytes[index + 1], b'$' | b'}' | b'\\')
        {
            text.push(bytes[index + 1] as char);
            index += 2;
            continue;
        }
        if bytes[index] != b'$' {
            let character = source[index..]
                .chars()
                .next()
                .ok_or("snippet UTF-8 boundary")?;
            text.push(character);
            index += character.len_utf8();
            continue;
        }
        index += 1;
        if index >= bytes.len() {
            text.push('$');
            break;
        }
        if bytes[index].is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1
            }
            let number = source[start..index]
                .parse::<u32>()
                .map_err(|_| "invalid tabstop")?;
            stops.push((number, text.len(), text.len()));
            continue;
        }
        if bytes[index] == b'{' {
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1
            }
            if start == index {
                return Err("snippet placeholder has no index".into());
            }
            let number = source[start..index]
                .parse::<u32>()
                .map_err(|_| "invalid placeholder")?;
            let stop_position = stops.len();
            stops.push((number, text.len(), text.len()));
            if index >= bytes.len() {
                return Err("unterminated snippet placeholder".into());
            }
            match bytes[index] {
                b'}' => index += 1,
                b':' => {
                    index += 1;
                    let start = index;
                    while index < bytes.len() && bytes[index] != b'}' {
                        index += 1
                    }
                    if index >= bytes.len() {
                        return Err("unterminated snippet default".into());
                    }
                    text.push_str(&source[start..index]);
                    index += 1
                }
                b'|' => {
                    index += 1;
                    let start = index;
                    while index + 1 < bytes.len()
                        && !(bytes[index] == b'|' && bytes[index + 1] == b'}')
                    {
                        index += 1
                    }
                    if index + 1 >= bytes.len() {
                        return Err("unterminated snippet choice".into());
                    }
                    let choice = source[start..index].split(',').next().unwrap_or_default();
                    text.push_str(choice);
                    index += 2
                }
                _ => return Err("unsupported snippet placeholder".into()),
            }
            stops[stop_position].2 = text.len();
            continue;
        }
        text.push('$');
    }
    stops.sort_by_key(|(number, start, _)| ((*number == 0), *number, *start));
    let first = stops.first().map(|(_, start, _)| *start);
    Ok(ParsedSnippet {
        text,
        first_tabstop: first,
        tabstops: stops
            .into_iter()
            .map(|(index, start, end)| SnippetTabstop { index, start, end })
            .collect(),
    })
}
pub fn sanitize_markdown(source: &str, maximum_chars: usize) -> String {
    let mut out = String::new();
    let mut escape = false;
    for character in source.chars() {
        if out.chars().count() >= maximum_chars {
            break;
        }
        if escape {
            if ('@'..='~').contains(&character) {
                escape = false
            }
            continue;
        }
        if character == '\u{1b}' {
            escape = true;
            continue;
        }
        if character.is_control() && !matches!(character, '\n' | '\t') {
            continue;
        }
        out.push(character);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        thread,
        time::{Duration, Instant},
    };

    use serde_json::{Value, json};

    use super::{
        LspDocument, LspEvent, LspFeature, LspFeatureResult, LspPolicy, LspService,
        PositionEncoding, SyncKind, completion_items, file_uri, hover_text, incremental_change,
        initialize_capabilities, language_id_for_path, path_from_file_uri, workspace_edit,
    };
    use crate::packages::{ToolDefinition, ToolKind};

    #[test]
    fn language_and_uri_mapping_are_deterministic() {
        assert_eq!(language_id_for_path(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(
            language_id_for_path(Path::new("ui.tsx")),
            Some("typescriptreact")
        );
        assert_eq!(
            language_id_for_path(Path::new("ui.jsx")),
            Some("javascriptreact")
        );
        assert_eq!(language_id_for_path(Path::new("main.go")), Some("go"));
        assert_eq!(language_id_for_path(Path::new("README")), None);
        let path = Path::new("/tmp/a b#%.rs");
        let uri = file_uri(path);
        assert!(uri.ends_with("/tmp/a%20b%23%25.rs"));
        assert_eq!(path_from_file_uri(&uri).as_deref(), Some(path));
    }

    #[test]
    fn hover_markup_is_safely_reduced_to_text() {
        assert_eq!(
            hover_text(Some(
                &json!({"contents":{"kind":"markdown","value":"**type**"}})
            )),
            "**type**"
        );
        assert_eq!(hover_text(Some(&Value::Null)), "null");
    }

    #[test]
    fn initialization_negotiates_utf32_and_incremental_sync() {
        let capabilities = initialize_capabilities(Some(&json!({"capabilities":{
            "positionEncoding":"utf-32","textDocumentSync":{"change":2},
            "hoverProvider":true,"completionProvider":{"triggerCharacters":["."]}
        }})))
        .expect("capabilities");
        assert_eq!(capabilities.position_encoding, PositionEncoding::Utf32);
        assert_eq!(capabilities.sync_kind, SyncKind::Incremental);
        assert!(capabilities.hover && capabilities.completion);
    }

    #[test]
    fn incremental_sync_uses_the_negotiated_unicode_units() {
        let old = LspDocument {
            path: "/tmp/a.rs".into(),
            language_id: "rust".into(),
            version: 1,
            text: "a😀x".into(),
        };
        let new = LspDocument {
            version: 2,
            text: "a😀yz".into(),
            ..old.clone()
        };
        let change = incremental_change(&old, &new, PositionEncoding::Utf16).expect("change");
        assert_eq!(change.pointer("/range/start/character"), Some(&json!(3)));
        assert_eq!(change.pointer("/range/end/character"), Some(&json!(4)));
        assert_eq!(change.get("text"), Some(&json!("yz")));
    }

    #[test]
    fn completion_and_workspace_edits_are_bounded_typed_data() {
        let items = completion_items(
            &json!([{"label":"println","detail":"macro","insertText":"println!"}]),
        )
        .expect("completion");
        assert_eq!(items[0].insert_text, "println!");
        let edit = workspace_edit(&json!({"changes":{"file:///tmp/a.rs":[{
            "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":"b"
        }]}}))
        .expect("workspace edit");
        assert_eq!(edit.documents.len(), 1);
        assert_eq!(edit.documents[0].edits[0].new_text, "b");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deterministic_fake_server_proves_lifecycle_sync_and_features() {
        let Some(python) = crate::packages::discover_executable("python3") else {
            return;
        };
        let workspace = tempfile::tempdir().expect("workspace");
        let script = workspace.path().join("fake_lsp.py");
        let trace_path = workspace.path().join("trace.jsonl");
        std::fs::write(
            &script,
            r#"
import json, sys
trace = open(sys.argv[1], 'w', buffering=1)
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line: return None
        if line in (b'\r\n', b'\n'): break
        if line.lower().startswith(b'content-length:'): length = int(line.split(b':',1)[1])
    return json.loads(sys.stdin.buffer.read(length))
def send(value):
    data=json.dumps(value,separators=(',',':')).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(data)).encode()+data); sys.stdout.buffer.flush()
while True:
    message=read()
    if message is None: break
    trace.write(json.dumps(message,separators=(',',':'))+'\n')
    method=message.get('method')
    if method=='initialize':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'capabilities':{
          'positionEncoding':'utf-8','textDocumentSync':{'change':2},'hoverProvider':True,
          'completionProvider':{},'definitionProvider':True,'referencesProvider':True,
          'documentFormattingProvider':True,'renameProvider':True,'codeActionProvider':True}}})
    elif method=='textDocument/didOpen':
        d=message['params']['textDocument']; send({'jsonrpc':'2.0','method':'textDocument/publishDiagnostics','params':{
          'uri':d['uri'],'version':d['version'],'diagnostics':[{'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':1}},'severity':2,'message':'fake warning'}]}})
        send({'jsonrpc':'2.0','id':99,'method':'workspace/applyEdit','params':{'label':'fake edit','edit':{'changes':{d['uri']:[{
          'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':1}},'newText':'b'}]}}}})
    elif method=='textDocument/hover':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'contents':{'kind':'markdown','value':'fake hover'}}})
    elif method=='textDocument/completion':
        send({'jsonrpc':'2.0','id':message['id'],'result':[{'label':'fakeItem','insertText':'fakeItem'}]})
    elif method=='shutdown': send({'jsonrpc':'2.0','id':message['id'],'result':None})
    elif method=='exit': break
"#,
        )
        .expect("script");
        let definition = ToolDefinition {
            id: "fake".into(),
            name: "Fake".into(),
            kind: ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: python.display().to_string(),
            launch_args: vec![
                script.display().to_string(),
                trace_path.display().to_string(),
            ],
            install_program: None,
            install_args: Vec::new(),
        };
        let document = LspDocument {
            path: workspace.path().join("main.rs"),
            language_id: "rust".into(),
            version: 1,
            text: "a😀".into(),
        };
        let mut service = LspService::new();
        service
            .start_session_absolute_with_policy(
                &definition.id,
                &definition,
                python.clone(),
                workspace.path().into(),
                Some(document.clone()),
                LspPolicy {
                    request_timeout: Duration::from_millis(100),
                    maximum_log_lines: 20,
                },
            )
            .expect("start");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut running = false;
        let mut diagnostics_seen = false;
        let mut apply_edit_seen = false;
        while Instant::now() < deadline && !(running && diagnostics_seen && apply_edit_seen) {
            if let Some(event) = service.try_event() {
                match event {
                    LspEvent::Running { capabilities, .. } => {
                        assert_eq!(capabilities.position_encoding, PositionEncoding::Utf8);
                        assert_eq!(capabilities.sync_kind, SyncKind::Incremental);
                        running = true;
                    }
                    LspEvent::Diagnostics { diagnostics, .. } => {
                        assert_eq!(diagnostics[0].message, "fake warning");
                        diagnostics_seen = true;
                    }
                    LspEvent::ApplyEditRequest {
                        server,
                        request_id,
                        edit,
                        ..
                    } => {
                        assert_eq!(edit.documents.len(), 1);
                        service
                            .respond_apply_edit(
                                &server,
                                request_id,
                                false,
                                Some("test rejection".into()),
                            )
                            .expect("respond");
                        apply_edit_seen = true;
                    }
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(running && diagnostics_seen && apply_edit_seen);
        service
            .request(
                "fake",
                LspFeature::Hover,
                document.clone(),
                crate::document::ByteOffset(5),
                11,
            )
            .expect("hover");
        service
            .request(
                "fake",
                LspFeature::Completion,
                document.clone(),
                crate::document::ByteOffset(5),
                12,
            )
            .expect("completion");
        service
            .sync(
                "fake",
                LspDocument {
                    version: 2,
                    text: "a😀z".into(),
                    ..document.clone()
                },
            )
            .expect("sync");
        let mut hover = false;
        let mut completion = false;
        while Instant::now() < deadline && !(hover && completion) {
            if let Some(LspEvent::Result {
                generation, result, ..
            }) = service.try_event()
            {
                match (generation, result) {
                    (11, LspFeatureResult::Hover(text)) => hover = text == "fake hover",
                    (12, LspFeatureResult::Completion(items)) => {
                        completion = items[0].label == "fakeItem";
                    }
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(hover && completion);
        service
            .request(
                "fake",
                LspFeature::Definition,
                document.clone(),
                crate::document::ByteOffset(5),
                13,
            )
            .expect("definition");
        let timeout_deadline = Instant::now() + Duration::from_secs(2);
        let mut timed_out = false;
        while Instant::now() < timeout_deadline && !timed_out {
            if let Some(LspEvent::RequestFailed {
                generation: 13,
                message,
                ..
            }) = service.try_event()
            {
                timed_out = message.contains("timed out");
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(timed_out);
        service.close("fake", document.uri()).expect("close");
        service.stop("fake").expect("stop");
        let trace = std::fs::read_to_string(trace_path).expect("trace");
        assert!(trace.contains("textDocument/didClose"));
        assert!(trace.contains("test rejection"));
        let change = trace
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|message| message.get("method") == Some(&json!("textDocument/didChange")))
            .expect("change");
        assert_eq!(
            change.pointer("/params/contentChanges/0/range/start/character"),
            Some(&json!(5))
        );
        assert_eq!(
            change.pointer("/params/contentChanges/0/text"),
            Some(&json!("z"))
        );
    }
}
