use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    editor::text::{ByteOffset, PositionCodec, PositionEncoding, TextBuffer},
    process::{read_content_length_message, write_content_length_message},
    tools::{ToolDefinition, ToolKind},
};

use super::{
    COMMAND_QUEUE, EVENT_QUEUE, MAX_LOG_LINE_BYTES, MAX_LOG_LINES, MAX_PENDING_REQUESTS,
    MESSAGE_QUEUE,
    error::LspError,
    model::{
        LspCapabilities, LspDocument, LspEvent, LspFeature, LspHealth, LspPolicy,
        LspServerConfiguration, ServerRequestId,
    },
    origins::{EditContext, EventSender, OwnedLspEvent},
    protocol::{
        fail_child, feature_params, handle_response, initialize_capabilities, initialize_message,
        sanitize_log, send_cancel, send_change, send_close, send_event, send_open, send_save,
    },
    server_message::{ServerMessageContext, expire_requests, handle_server_message},
};

#[derive(Clone, Debug)]
pub(super) enum SessionCommand {
    Sync(LspDocument),
    SyncOwned {
        document: LspDocument,
        source: (crate::editor::BufferId, crate::editor::text::TextVersion),
    },
    RespondOwnedApplyEdit {
        request_id: ServerRequestId,
        key: std::sync::Arc<()>,
        applied: bool,
        reason: Option<String>,
        completed: SyncSender<Result<(), String>>,
    },
    Save(LspDocument),
    Close(String),
    Request {
        feature: LspFeature,
        document: LspDocument,
        offset: ByteOffset,
        generation: u64,
    },
    Cancel(u64),
    Configure(LspServerConfiguration),
    Health,
    RespondApplyEdit {
        request_id: ServerRequestId,
        applied: bool,
        reason: Option<String>,
    },
}

pub(super) struct SessionHandle {
    sender: SyncSender<SessionCommand>,
    origin: std::sync::Arc<()>,
    thread: Option<JoinHandle<()>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub struct LspService {
    events: SyncSender<OwnedLspEvent>,
    receiver: Receiver<OwnedLspEvent>,
    sessions: BTreeMap<String, SessionHandle>,
}

pub struct LspSessionLaunch<'a> {
    pub session_key: &'a str,
    pub definition: &'a ToolDefinition,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub root: PathBuf,
    pub document: Option<LspDocument>,
    pub policy: LspPolicy,
    pub configuration: LspServerConfiguration,
    pub environment: BTreeMap<String, std::ffi::OsString>,
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
        launch: LspSessionLaunch<'_>,
    ) -> Result<(), LspError> {
        self.start_session_owned(launch, None)
    }
    pub(crate) fn start_session_owned(
        &mut self,
        launch: LspSessionLaunch<'_>,
        initial_source: Option<(crate::editor::BufferId, crate::editor::text::TextVersion)>,
    ) -> Result<(), LspError> {
        let LspSessionLaunch {
            session_key,
            definition,
            executable,
            arguments,
            root,
            document,
            policy,
            configuration,
            environment,
        } = launch;
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
        let server = session_key.to_owned();
        let origin = std::sync::Arc::new(());
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_requested = Default::default();
        let events = EventSender {
            stop_requested: std::sync::Arc::clone(&stop_requested),
            cancelled: cancelled.clone(),
            channel: self.events.clone(),
            commands: sender.clone(),
            context: EditContext {
                session: origin.clone(),
                server: server.clone().into(),
                encoding: PositionEncoding::Utf16,
                diagnostic_ambiguous: false,
                disk: None,
                sources: std::sync::Arc::new(HashMap::new()),
            },
        };
        // Canonical preflight pins the executable identity, but multicall and
        // rustup proxy binaries still dispatch from the requested argv[0].
        let executable_name = requested_executable_name(definition);
        let thread = thread::Builder::new()
            .name(format!("dust-lsp-{server}"))
            .spawn(move || {
                send_event(
                    &events,
                    LspEvent::Starting {
                        server: server.clone(),
                    },
                );
                run_session(
                    server,
                    executable,
                    executable_name,
                    arguments,
                    root,
                    document,
                    policy,
                    configuration,
                    environment,
                    commands,
                    events,
                    initial_source,
                );
            })
            .map_err(LspError::Thread)?;
        self.sessions.insert(
            session_key.to_owned(),
            SessionHandle {
                sender,
                origin,
                thread: Some(thread),
                cancelled,
                stop_requested,
            },
        );
        Ok(())
    }

    pub(crate) fn diagnostic_producer(
        &self,
        server: &str,
        prefix: &str,
    ) -> Option<crate::workspace::DiagnosticProducer> {
        let session = self.sessions.get(server)?;
        Some(crate::workspace::DiagnosticProducer::from_runtime(
            crate::workspace::DiagnosticOwner::new(format!("{prefix}{server}")).ok()?,
            session.origin.clone(),
        ))
    }
    pub(crate) fn sync_owned(
        &self,
        server: &str,
        document: LspDocument,
        source: (crate::editor::BufferId, crate::editor::text::TextVersion),
    ) -> Result<(), LspError> {
        self.send(server, SessionCommand::SyncOwned { document, source })
    }
    pub(crate) fn owns_event_context(&self, context: &EditContext) -> bool {
        self.sessions
            .get(context.server.as_ref())
            .is_some_and(|session| std::sync::Arc::ptr_eq(&session.origin, &context.session))
    }
    pub(crate) fn edit_context_current(&self, context: &EditContext) -> bool {
        self.sessions
            .get(context.server.as_ref())
            .is_some_and(|session| {
                std::sync::Arc::ptr_eq(&session.origin, &context.session)
                    && session
                        .thread
                        .as_ref()
                        .is_none_or(|thread| !thread.is_finished())
            })
    }
    pub(crate) fn try_owned_event(&self) -> Option<OwnedLspEvent> {
        self.receiver.try_recv().ok()
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

    pub fn configure(
        &self,
        server: &str,
        configuration: LspServerConfiguration,
    ) -> Result<(), LspError> {
        self.send(server, SessionCommand::Configure(configuration))
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
        session
            .stop_requested
            .store(true, std::sync::atomic::Ordering::Release);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            session
                .cancelled
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = session.thread.take() {
                let _ = thread.join();
            }
        });
        Ok(())
    }

    pub(crate) fn begin_shutdown(&mut self) {
        for server in self.sessions.keys().cloned().collect::<Vec<_>>() {
            let _ = self.stop(&server);
        }
    }

    #[must_use]
    pub fn running_servers(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    pub fn try_event(&self) -> Option<LspEvent> {
        self.receiver.try_recv().ok().map(|event| event.event)
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
pub(super) struct PendingRequest {
    pub(super) feature: LspFeature,
    pub(super) document: LspDocument,
    pub(super) generation: u64,
    pub(super) deadline: Instant,
    pub(super) encoding: PositionEncoding,
    pub(super) sources: std::sync::Arc<
        HashMap<String, (crate::editor::BufferId, crate::editor::text::TextVersion)>,
    >,
}

pub(super) fn requested_executable_name(definition: &ToolDefinition) -> String {
    Path::new(&definition.executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&definition.executable)
        .to_owned()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_session(
    server: String,
    executable: PathBuf,
    executable_name: String,
    args: Vec<String>,
    root: PathBuf,
    initial_document: Option<LspDocument>,
    policy: LspPolicy,
    mut configuration: LspServerConfiguration,
    environment: BTreeMap<String, std::ffi::OsString>,
    commands: Receiver<SessionCommand>,
    mut events: EventSender,
    initial_source: Option<(crate::editor::BufferId, crate::editor::text::TextVersion)>,
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
        .envs(environment)
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
    let Some(stdin) = child.stdin.take() else {
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
    let cancelled = events.cancelled.clone();
    struct Cancel(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Cancel {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    let _cancel = Cancel(cancelled.clone());
    let pipes = (|| {
        Ok::<_, std::io::Error>((
            crate::process::CancellablePipe::new(stdin, cancelled.clone())?,
            crate::process::CancellablePipe::new(stdout, cancelled.clone())?,
            stderr
                .map(|pipe| crate::process::CancellablePipe::new(pipe, cancelled.clone()))
                .transpose()?,
        ))
    })();
    let (mut stdin, stdout, stderr) = match pipes {
        Ok(pipes) => pipes,
        Err(error) => {
            fail_child(&mut child, &events, &server, &error.to_string());
            return;
        }
    };
    let (messages, message_receiver) = mpsc::sync_channel(MESSAGE_QUEUE);
    let reader_cancelled = cancelled.clone();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut message = match read_content_length_message(&mut reader) {
                Ok(Some(message)) => Ok(message),
                Ok(None) => break,
                Err(error) => Err(error.to_string()),
            };
            loop {
                if reader_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                match messages.try_send(message) {
                    Ok(()) => break,
                    Err(TrySendError::Full(value)) => {
                        message = value;
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
        }
    });
    let log_reader = stderr.map(|stderr| {
        let events = events.clone();
        let server = server.clone();
        thread::spawn(move || {
            for (index, line) in BufReader::new(stderr).lines().enumerate() {
                let Ok(line) = line else {
                    break;
                };
                if index < policy.maximum_log_lines.min(MAX_LOG_LINES) {
                    send_event(
                        &events,
                        LspEvent::Log {
                            server: server.clone(),
                            message: sanitize_log(&line, MAX_LOG_LINE_BYTES),
                        },
                    );
                }
            }
        })
    });

    let initialize = initialize_message(&root, configuration.initialization_options.as_ref());
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
        return;
    }

    let mut initialized = false;
    let mut capabilities = LspCapabilities::default();
    let mut base_capabilities = LspCapabilities::default();
    let mut dynamic_registrations = HashMap::<String, (String, Value)>::new();
    let mut documents = HashMap::<String, LspDocument>::new();
    let mut document_sources = HashMap::new();
    let mut diagnostic_lineage = super::origins::DiagnosticLineage::default();
    if let Some(document) = initial_document {
        if let Some(source) = initial_source {
            diagnostic_lineage.observe(document.uri(), &source);
            document_sources.insert(document.uri(), source);
        }
        documents.insert(document.uri(), document);
    }
    let mut apply_requests = HashMap::<ServerRequestId, std::sync::Arc<()>>::new();
    let mut opened_documents = HashMap::new();
    let mut pending = HashMap::<i64, PendingRequest>::new();
    let mut next_request_id = 2_i64;
    let mut logs = VecDeque::<String>::with_capacity(MAX_LOG_LINES);
    let mut stopping = false;
    let initialize_deadline = Instant::now()
        + policy
            .request_timeout
            .clamp(Duration::from_secs(1), Duration::from_secs(60));

    while !stopping && !cancelled.load(std::sync::atomic::Ordering::Acquire) {
        loop {
            match commands.try_recv() {
                Ok(command) if !cancelled.load(std::sync::atomic::Ordering::Acquire) => {
                    handle_command(
                        command,
                        &server,
                        &root,
                        &executable,
                        &mut stdin,
                        initialized,
                        &capabilities,
                        &mut documents,
                        &mut document_sources,
                        &mut diagnostic_lineage,
                        &mut apply_requests,
                        &mut opened_documents,
                        &mut pending,
                        &mut next_request_id,
                        policy.request_timeout.min(Duration::from_secs(60)),
                        logs.len(),
                        &events,
                        &mut stopping,
                        &mut configuration,
                    )
                }
                Ok(_) | Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stopping = true;
                    break;
                }
            }
            if stopping {
                break;
            }
        }
        if stopping
            || events
                .stop_requested
                .load(std::sync::atomic::Ordering::Acquire)
        {
            break;
        }
        match message_receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(message)) => {
                if message.get("method").is_none()
                    && message.get("id").and_then(Value::as_i64) == Some(1)
                    && !initialized
                {
                    match initialize_capabilities(message.get("result")) {
                        Ok(negotiated) => {
                            base_capabilities = negotiated.clone();
                            capabilities = negotiated;
                            events.context.encoding = capabilities.position_encoding;
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
                    // Configuration-capable servers such as Pyright defer document
                    // work until the client publishes its initial settings snapshot.
                    // Dust has no adapter settings here, so the explicit empty object
                    // is the protocol-correct initial state rather than no state.
                    let _ = write_content_length_message(
                        &mut stdin,
                        &json!({
                            "jsonrpc":"2.0",
                            "method":"workspace/didChangeConfiguration",
                            "params":{"settings":configuration.settings.clone()}
                        }),
                    );
                    for document in documents.values() {
                        if let Err(error) = send_open(&mut stdin, document) {
                            send_event(&events, LspEvent::Failed {
                                server: server.clone(),
                                message: format!("language server synchronization write failed: {error}"),
                            });
                            stopping = true;
                            break;
                        }
                        opened_documents.insert(document.uri(), document.version);
                    }
                    if stopping {
                        break;
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
                        ServerMessageContext {
                            server: &server,
                            root: &root,
                            stdin: &mut stdin,
                            events: &events,
                            logs: &mut logs,
                            base_capabilities: &base_capabilities,
                            capabilities: &mut capabilities,
                            registrations: &mut dynamic_registrations,
                            configuration: &configuration,
                            sources: &document_sources,
                            diagnostic_lineage: &diagnostic_lineage,
                            documents: &documents,
                            apply_requests: &mut apply_requests,
                        },
                        &message,
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
            // Its output closed: it has exited or is about to, and the session must hear so.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let deadline = Instant::now() + Duration::from_secs(1);
                let status = loop {
                    match child.try_wait() {
                        Ok(None) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        status => break status.ok().flatten(),
                    }
                };
                send_event(
                    &events,
                    LspEvent::Stopped {
                        server: server.clone(),
                        success: status.is_some_and(|status| status.success()),
                    },
                );
                break;
            }
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
        if !stopping && !initialized && Instant::now() >= initialize_deadline {
            send_event(
                &events,
                LspEvent::Failed {
                    server: server.clone(),
                    message: "language server initialize request timed out".into(),
                },
            );
            stopping = true;
        }
    }
    if initialized && !cancelled.load(std::sync::atomic::Ordering::Acquire) {
        for (uri, _) in opened_documents {
            send_close(&mut stdin, &uri);
        }
        let _ = write_content_length_message(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":next_request_id,"method":"shutdown","params":null}),
        );
        let _ = write_content_length_message(
            &mut stdin,
            &json!({"jsonrpc":"2.0","method":"exit","params":null}),
        );
    }
    if events
        .stop_requested
        .load(std::sync::atomic::Ordering::Acquire)
    {
        let deadline = Instant::now() + Duration::from_millis(200);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    if let Some(reader) = log_reader {
        let _ = reader.join();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_command(
    command: SessionCommand,
    server: &str,
    root: &Path,
    executable: &Path,
    stdin: &mut impl std::io::Write,
    initialized: bool,
    capabilities: &LspCapabilities,
    documents: &mut HashMap<String, LspDocument>,
    document_sources: &mut HashMap<
        String,
        (crate::editor::BufferId, crate::editor::text::TextVersion),
    >,
    diagnostic_lineage: &mut super::origins::DiagnosticLineage,
    apply_requests: &mut HashMap<ServerRequestId, std::sync::Arc<()>>,
    opened_documents: &mut HashMap<String, u64>,
    pending: &mut HashMap<i64, PendingRequest>,
    next_request_id: &mut i64,
    request_timeout: Duration,
    recent_logs: usize,
    events: &EventSender,
    stopping: &mut bool,
    configuration: &mut LspServerConfiguration,
) {
    let command = match command {
        SessionCommand::SyncOwned { document, source } => {
            diagnostic_lineage.observe(document.uri(), &source);
            if diagnostic_lineage.ambiguous(&document.uri()) {
                let events = EventSender {
                    context: EditContext {
                        diagnostic_ambiguous: true,
                        ..events.context.clone()
                    },
                    ..events.clone()
                };
                send_event(
                    &events,
                    LspEvent::Stopped {
                        server: server.into(),
                        success: true,
                    },
                );
                *stopping = true;
                return;
            }
            document_sources.insert(document.uri(), source);
            SessionCommand::Sync(document)
        }
        SessionCommand::Sync(document) => {
            document_sources.remove(&document.uri());
            SessionCommand::Sync(document)
        }
        other => other,
    };
    match command {
        SessionCommand::SyncOwned { .. } => unreachable!("normalized owned sync"),
        SessionCommand::Sync(document) => {
            let uri = document.uri();
            if initialized {
                let result = if let Some(version) = opened_documents.get(&uri) {
                    // A disabled sync policy may have skipped the local previous snapshot.
                    let previous = documents.get(&uri).filter(|old| old.version == *version);
                    send_change(stdin, previous, &document, capabilities)
                } else {
                    send_open(stdin, &document)
                };
                if let Err(error) = result {
                    send_event(events, LspEvent::Failed {
                        server: server.into(),
                        message: format!("language server synchronization write failed: {error}"),
                    });
                    *stopping = true;
                    return;
                }
                if !opened_documents.contains_key(&uri)
                    || capabilities.sync_kind_for_document(&document) != super::model::SyncKind::None
                {
                    opened_documents.insert(uri.clone(), document.version);
                }
            }
            documents.insert(uri, document);
        }
        SessionCommand::Save(document) => {
            let uri = document.uri();
            if documents
                .get(&uri)
                .is_none_or(|old| old.version != document.version || old.text != document.text)
            {
                document_sources.remove(&uri);
            }
            if initialized {
                let previous = documents.get(&uri);
                let result = if let Some(version) = opened_documents.get(&uri) {
                    if *version != document.version || previous.is_some_and(|old| old.text != document.text) {
                        let previous = previous.filter(|old| old.version == *version);
                        send_change(stdin, previous, &document, capabilities)
                    } else {
                        Ok(())
                    }
                } else {
                    send_open(stdin, &document)
                };
                if let Err(error) = result {
                    send_event(events, LspEvent::Failed {
                        server: server.into(),
                        message: format!("language server synchronization write failed: {error}"),
                    });
                    *stopping = true;
                    return;
                }
                if !opened_documents.contains_key(&uri)
                    || capabilities.sync_kind_for_document(&document) != super::model::SyncKind::None
                {
                    opened_documents.insert(uri.clone(), document.version);
                }
                send_save(stdin, &document);
            }
            documents.insert(uri, document);
        }
        SessionCommand::Close(uri) => {
            document_sources.remove(&uri);
            documents.remove(&uri);
            if initialized && opened_documents.remove(&uri).is_some() {
                send_close(stdin, &uri);
            }
        }
        SessionCommand::Request {
            feature,
            document,
            offset,
            generation,
        } => {
            if documents
                .get(&document.uri())
                .is_none_or(|old| old.version != document.version || old.text != document.text)
            {
                document_sources.remove(&document.uri());
            }
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
            if !feature.capability_for_document(capabilities, &document) {
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
            if let Err(error) = write_content_length_message(stdin, &message) {
                send_event(
                    events,
                    LspEvent::RequestFailed {
                        server: server.into(),
                        generation,
                        method: feature.method().into(),
                        message: error.to_string(),
                    },
                );
                send_event(
                    events,
                    LspEvent::Failed {
                        server: server.into(),
                        message: format!("language server request write failed: {error}"),
                    },
                );
                *stopping = true;
            } else {
                let edit_sources = if matches!(
                    feature,
                    LspFeature::Rename { .. }
                        | LspFeature::Formatting { .. }
                        | LspFeature::RangeFormatting { .. }
                        | LspFeature::CodeAction { .. }
                        | LspFeature::CompletionResolve { .. }
                        | LspFeature::PullDiagnostics { .. }
                ) {
                    document_sources.clone()
                } else {
                    HashMap::new()
                };
                pending.insert(
                    id,
                    PendingRequest {
                        feature,
                        document,
                        generation,
                        deadline: Instant::now() + request_timeout,
                        encoding: capabilities.position_encoding,
                        sources: std::sync::Arc::new(edit_sources),
                    },
                );
            }
        }
        SessionCommand::Configure(updated) => {
            *configuration = updated;
            if initialized {
                let _ = write_content_length_message(
                    stdin,
                    &json!({
                        "jsonrpc":"2.0",
                        "method":"workspace/didChangeConfiguration",
                        "params":{"settings":configuration.settings.clone()}
                    }),
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
        SessionCommand::RespondOwnedApplyEdit {
            request_id,
            key,
            applied,
            reason,
            completed,
        } => {
            if apply_requests
                .get(&request_id)
                .is_some_and(|expected| std::sync::Arc::ptr_eq(expected, &key))
            {
                apply_requests.remove(&request_id);
                let _ =
                    completed.try_send(write_apply_response(stdin, request_id, applied, reason));
            } else {
                let _ = completed.try_send(Err(
                    "requesting edit identity retired before response".into()
                ));
            }
        }
        SessionCommand::RespondApplyEdit {
            request_id,
            applied,
            reason,
        } => {
            if apply_requests.remove(&request_id).is_some() {
                let _ = write_apply_response(stdin, request_id, applied, reason);
            }
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
    }
}

fn write_apply_response(
    stdin: &mut impl std::io::Write,
    request_id: ServerRequestId,
    applied: bool,
    reason: Option<String>,
) -> Result<(), String> {
    let result = if applied {
        json!({"applied":true})
    } else {
        json!({"applied":false,"failureReason":reason.unwrap_or_else(||"edit rejected".into())})
    };
    write_content_length_message(
        stdin,
        &json!({"jsonrpc":"2.0","id":request_id.value(),"result":result}),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
impl LspService {
    pub(crate) fn replace_edit_source_fixture(
        &mut self,
        uri: String,
        source: (crate::editor::BufferId, crate::editor::text::TextVersion),
        encoding: PositionEncoding,
    ) -> super::origins::EditSourceFixture {
        self.replace_edit_source_fixture_for("owned-edit-fixture", uri, source, encoding, 1)
    }
    pub(crate) fn replace_edit_source_fixture_for(
        &mut self,
        server: &str,
        uri: String,
        source: (crate::editor::BufferId, crate::editor::text::TextVersion),
        encoding: PositionEncoding,
        capacity: usize,
    ) -> super::origins::EditSourceFixture {
        let (sender, commands) = mpsc::sync_channel(capacity);
        let origin = std::sync::Arc::new(());
        let server = server.to_owned();
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_requested = Default::default();
        let events = EventSender {
            stop_requested: std::sync::Arc::clone(&stop_requested),
            cancelled: cancelled.clone(),
            channel: self.events.clone(),
            commands: sender.clone(),
            context: EditContext {
                session: origin.clone(),
                server: server.clone().into(),
                encoding,
                diagnostic_ambiguous: false,
                disk: None,
                sources: std::sync::Arc::new(HashMap::from([(uri, source)])),
            },
        };
        self.sessions.insert(
            server,
            SessionHandle {
                sender,
                origin,
                thread: None,
                cancelled,
                stop_requested,
            },
        );
        super::origins::EditSourceFixture {
            events,
            commands,
            pending: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;

    #[test]
    fn resumed_incremental_sync_replaces_snapshots_skipped_while_disabled() {
        use crate::editor::EditorState;
        for save in [false, true] {
            let mut editor = EditorState::new("/fixture".into());
            let text = TextBuffer::from_text("a");
            let (buffer, _) = editor.open_text(text.clone(), None);
            let mut service = LspService::new();
            let endpoint = service.replace_edit_source_fixture(
                "file:///fixture/main.rs".into(), (buffer, text.source_version()), PositionEncoding::Utf16,
            );
            let base = LspCapabilities { sync_kind: super::super::model::SyncKind::None, ..Default::default() };
            let options = json!({"documentSelector":[{"language":"rust"}],"syncKind":2});
            let active = super::super::server_message::effective_capabilities(
                &base, [("textDocument/didChange", &options)].into_iter(),
            );
            let mut documents = HashMap::new();
            let mut sources = HashMap::new();
            let mut lineage = super::super::origins::DiagnosticLineage::default();
            let mut apply = HashMap::new();
            let mut opened = HashMap::new();
            let mut pending = HashMap::new();
            let mut next = 2;
            let mut stopping = false;
            let mut config = LspServerConfiguration::standard();
            let mut output = Vec::new();
            for (version, text) in ["a", "longer", "longer!", "longer!!"].into_iter().enumerate() {
                let document = LspDocument {
                    path: "/fixture/main.rs".into(), language_id: "rust".into(),
                    version: version as u64, text: text.into(),
                };
                let command = if save && version >= 2 {
                    SessionCommand::Save(document)
                } else {
                    SessionCommand::Sync(document)
                };
                handle_command(
                    command, "owned-edit-fixture", Path::new("/fixture"), Path::new("/server"),
                    &mut output, true, if version >= 2 { &active } else { &base },
                    &mut documents, &mut sources, &mut lineage, &mut apply, &mut opened,
                    &mut pending, &mut next, Duration::from_secs(1), 0,
                    &endpoint.events, &mut stopping, &mut config,
                );
            }
            let mut input = std::io::Cursor::new(output);
            let changes = std::iter::from_fn(|| crate::process::read_content_length_message(&mut input).unwrap())
                .filter(|message| message["method"] == "textDocument/didChange").collect::<Vec<_>>();
            assert_eq!(changes.len(), 2);
            assert_eq!(changes[0]["params"]["textDocument"]["version"], 2);
            assert_eq!(changes[0]["params"]["contentChanges"], json!([{"text":"longer!"}]));
            assert_eq!(changes[1]["params"]["contentChanges"][0]["text"], "!");
            assert!(changes[1]["params"]["contentChanges"][0]["range"].is_object());
        }
    }

    #[test]
    fn reopening_a_document_retires_the_old_diagnostic_transport() {
        use crate::editor::{EditorState, text::TextBuffer};
        let mut editor = EditorState::new("/fixture".into());
        let first = TextBuffer::from_text("first");
        let (buffer, _) = editor.open_text(first.clone(), Some("/fixture/main.rs".into()));
        let document = LspDocument {
            path: "/fixture/main.rs".into(),
            language_id: "rust".into(),
            version: 0,
            text: "first".into(),
        };
        let mut service = LspService::new();
        let endpoint = service.replace_edit_source_fixture(
            document.uri(),
            (buffer, first.source_version()),
            PositionEncoding::Utf16,
        );
        let mut lineage = super::super::origins::DiagnosticLineage::default();
        let mut documents = HashMap::new();
        let mut sources = HashMap::new();
        let mut opened = HashMap::new();
        let mut pending = HashMap::new();
        let mut apply = HashMap::new();
        let mut stopping = false;
        let mut next = 2;
        let mut config = LspServerConfiguration::standard();
        let mut sink = Vec::new();
        for command in [
            SessionCommand::SyncOwned {
                document: document.clone(),
                source: (buffer, first.source_version()),
            },
            SessionCommand::Close(document.uri()),
            SessionCommand::SyncOwned {
                document,
                source: (buffer, TextBuffer::from_text("second").source_version()),
            },
        ] {
            handle_command(
                command,
                "owned-edit-fixture",
                Path::new("/fixture"),
                Path::new("/server"),
                &mut sink,
                true,
                &LspCapabilities::default(),
                &mut documents,
                &mut sources,
                &mut lineage,
                &mut apply,
                &mut opened,
                &mut pending,
                &mut next,
                Duration::from_secs(1),
                0,
                &endpoint.events,
                &mut stopping,
                &mut config,
            );
        }
        assert!(stopping);
        let event = service.try_owned_event().unwrap();
        assert!(matches!(
            event.event,
            LspEvent::Stopped { success: true, .. }
        ));
        assert!(event.context.diagnostic_ambiguous());
    }
}
