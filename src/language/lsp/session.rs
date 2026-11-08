//! Language-server sessions, requests and worker communication.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::BufReader;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use serde_json::json;

use crate::editor::text::ByteOffset;

use crate::editor::text::PositionCodec;

use crate::editor::text::PositionEncoding;

use crate::editor::text::TextBuffer;

use crate::process::read_content_length_message;

use crate::process::write_content_length_message;

use crate::tools::ToolDefinition;

use crate::tools::ToolKind;

use crate::language::lsp::COMMAND_QUEUE;

use crate::language::lsp::EVENT_QUEUE;

use crate::language::lsp::MAX_LOG_LINE_BYTES;

use crate::language::lsp::MAX_LOG_LINES;

use crate::language::lsp::MAX_PENDING_REQUESTS;

use crate::language::lsp::MESSAGE_QUEUE;

use crate::language::lsp::error::LspError;

use crate::language::lsp::server::LspCapabilities;

use crate::language::lsp::messages::LspDocument;

use crate::language::lsp::messages::LspEvent;

use crate::language::lsp::messages::LspFeature;

use crate::language::lsp::messages::LspHealth;

use crate::language::lsp::server::LspPolicy;

use crate::language::lsp::server::LspServerConfiguration;

use crate::language::lsp::messages::ServerRequestId;

use crate::language::lsp::origins::EditContext;

use crate::language::lsp::origins::EventSender;

use crate::language::lsp::origins::OwnedLspEvent;

use crate::language::lsp::protocol::fail_child;

use crate::language::lsp::protocol::feature_params;

use crate::language::lsp::protocol::handle_response;

use crate::language::lsp::protocol::initialize_capabilities;

use crate::language::lsp::protocol::initialize_message;

use crate::language::lsp::protocol::sanitize_log;

use crate::language::lsp::protocol::send_cancel;

use crate::language::lsp::protocol::send_change;

use crate::language::lsp::protocol::send_close;

use crate::language::lsp::protocol::send_event;

use crate::language::lsp::protocol::send_open;

use crate::language::lsp::protocol::send_save;

use crate::language::lsp::server_message::ServerMessageContext;

use crate::language::lsp::server_message::expire_requests;

use crate::language::lsp::server_message::handle_server_message;

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
        request_id: u64,
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

pub struct LspClient {
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

impl LspClient {
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
        request_id: u64,
    ) -> Result<(), LspError> {
        self.send(
            server,
            SessionCommand::Request {
                feature,
                document,
                offset,
                request_id,
            },
        )
    }

    pub fn cancel(&self, server: &str, request_id: u64) -> Result<(), LspError> {
        self.send(server, SessionCommand::Cancel(request_id))
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
            thread::sleep(Duration::from_millis(500));
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

impl Default for LspClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LspClient {
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
    pub(super) request_id: u64,
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
    // Unix passes it as argv[0]; Windows has no separate program name.
    #[cfg_attr(not(unix), allow(unused_variables))] executable_name: String,
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
        command.arg0(executable_name).process_group(0);
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
    stdin.set_write_timeout(
        policy
            .request_timeout
            .clamp(Duration::from_secs(1), Duration::from_secs(60)),
    );
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
            drain_stderr(stderr, |line| {
                if policy.maximum_log_lines > 0 {
                    send_event(
                        &events,
                        LspEvent::Log {
                            server: server.clone(),
                            method: None,
                            message: sanitize_log(
                                &String::from_utf8_lossy(line),
                                MAX_LOG_LINE_BYTES,
                            ),
                        },
                    );
                }
            });
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
        crate::language::lsp::protocol::terminate_server(&mut child);
        return;
    }

    let mut initialized = false;
    let mut capabilities = LspCapabilities::default();
    let mut base_capabilities = LspCapabilities::default();
    let mut dynamic_registrations = HashMap::<String, (String, Value)>::new();
    let mut documents = HashMap::<String, LspDocument>::new();
    let mut document_sources = HashMap::new();
    let mut diagnostic_lineage = crate::language::lsp::origins::DiagnosticLineage::default();
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
        for _ in 0..COMMAND_QUEUE {
            match commands.try_recv() {
                Ok(command) if !cancelled.load(std::sync::atomic::Ordering::Acquire) => {
                    if let Err(error) = handle_command(
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
                    ) && !stopping
                    {
                        send_event(
                            &events,
                            LspEvent::Failed {
                                server: server.clone(),
                                message: format!("language server synchronization failed: {error}"),
                            },
                        );
                        stopping = true;
                    }
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
                    let setup = (|| {
                        write_content_length_message(
                            &mut stdin,
                            &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
                        )?;
                        // Configuration-capable servers such as Pyright defer document
                        // work until the client publishes its initial settings snapshot.
                        // Dust has no adapter settings here, so the explicit empty object
                        // is the protocol-correct initial state rather than no state.
                        write_content_length_message(
                            &mut stdin,
                            &json!({
                                "jsonrpc":"2.0",
                                "method":"workspace/didChangeConfiguration",
                                "params":{"settings":configuration.settings.clone()}
                            }),
                        )?;
                        for document in documents.values() {
                            send_open(&mut stdin, document)?;
                            opened_documents.insert(document.uri(), document.version);
                        }
                        Ok::<_, crate::process::ProtocolError>(())
                    })();
                    if let Err(error) = setup {
                        send_event(
                            &events,
                            LspEvent::Failed {
                                server: server.clone(),
                                message: format!(
                                    "language server initialization write failed: {error}"
                                ),
                            },
                        );
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
                    match server_exit_status(&mut child) {
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
                        success: status == Some(true),
                    },
                );
                break;
            }
        }
        expire_requests(&server, &mut stdin, &mut pending, &events);
        match server_exit_status(&mut child) {
            Ok(Some(success)) => {
                send_event(
                    &events,
                    LspEvent::Stopped {
                        server: server.clone(),
                        success,
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
        stdin.set_write_timeout(Duration::from_millis(100));
        shutdown_server(&mut stdin, &message_receiver, next_request_id);
    }
    if events
        .stop_requested
        .load(std::sync::atomic::Ordering::Acquire)
    {
        let deadline = Instant::now() + Duration::from_millis(200);
        while matches!(server_exit_status(&mut child), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    crate::language::lsp::protocol::terminate_server(&mut child);
    let _ = reader.join();
    if let Some(reader) = log_reader {
        let _ = reader.join();
    }
}

fn drain_stderr(mut stderr: impl Read, mut publish: impl FnMut(&[u8])) {
    let mut bytes = [0; MAX_LOG_LINE_BYTES];
    let mut line = Vec::with_capacity(MAX_LOG_LINE_BYTES);
    loop {
        match stderr.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                for byte in &bytes[..count] {
                    if *byte == b'\n' {
                        publish(&line);
                        line.clear();
                    } else if line.len() < MAX_LOG_LINE_BYTES {
                        line.push(*byte);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if !line.is_empty() {
        publish(&line);
    }
}

fn shutdown_server(
    stdin: &mut impl std::io::Write,
    messages: &Receiver<Result<Value, String>>,
    id: i64,
) {
    if write_content_length_message(
        stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":"shutdown","params":null}),
    )
    .is_err()
    {
        return;
    }
    // As long as VS Code's client waits; servers such as rust-analyzer can take a moment.
    let deadline = Instant::now() + Duration::from_secs(2);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match messages.recv_timeout(remaining) {
            Ok(Ok(message))
                if message.get("method").is_none()
                    && message.get("id").and_then(Value::as_i64) == Some(id) =>
            {
                let _ = write_content_length_message(
                    stdin,
                    &json!({"jsonrpc":"2.0","method":"exit","params":null}),
                );
                break;
            }
            Ok(Ok(_)) => {}
            _ => break,
        }
    }
}

// Observe exit without reaping, so the process group cannot be recycled before
// teardown has terminated its remaining members.
fn server_exit_status(child: &mut std::process::Child) -> std::io::Result<Option<bool>> {
    crate::process::child_exit_status(child).map(|status| status.map(|code| code == Some(0)))
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
    diagnostic_lineage: &mut crate::language::lsp::origins::DiagnosticLineage,
    apply_requests: &mut HashMap<ServerRequestId, std::sync::Arc<()>>,
    opened_documents: &mut HashMap<String, u64>,
    pending: &mut HashMap<i64, PendingRequest>,
    next_request_id: &mut i64,
    request_timeout: Duration,
    recent_logs: usize,
    events: &EventSender,
    stopping: &mut bool,
    configuration: &mut LspServerConfiguration,
) -> Result<(), crate::process::ProtocolError> {
    let command = match command {
        SessionCommand::SyncOwned { document, source } => {
            diagnostic_lineage.observe(document.uri(), &source);
            if diagnostic_lineage.tracked(&document.uri())
                && diagnostic_lineage.ambiguous(&document.uri())
            {
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
                return Ok(());
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
            if let Err(error) = sync_document(
                stdin,
                &document,
                initialized,
                capabilities,
                documents,
                opened_documents,
            ) {
                send_event(
                    events,
                    LspEvent::Failed {
                        server: server.into(),
                        message: format!("language server synchronization write failed: {error}"),
                    },
                );
                *stopping = true;
                return Err(error);
            }
        }
        SessionCommand::Save(document) => {
            // An older disk snapshot must not rewind already synchronized edits.
            let uri = document.uri();
            if documents
                .get(&uri)
                .is_none_or(|old| old.version <= document.version)
            {
                if documents
                    .get(&uri)
                    .is_none_or(|old| old.version != document.version || old.text != document.text)
                {
                    document_sources.remove(&uri);
                }
                if let Err(error) = sync_document(
                    stdin,
                    &document,
                    initialized,
                    capabilities,
                    documents,
                    opened_documents,
                ) {
                    send_event(
                        events,
                        LspEvent::Failed {
                            server: server.into(),
                            message: format!(
                                "language server synchronization write failed: {error}"
                            ),
                        },
                    );
                    *stopping = true;
                    return Err(error);
                }
            }
            if initialized {
                send_save(stdin, &document)?;
            }
        }
        SessionCommand::Close(uri) => {
            document_sources.remove(&uri);
            documents.remove(&uri);
            if initialized && opened_documents.remove(&uri).is_some() {
                send_close(stdin, &uri)?;
            }
        }
        SessionCommand::Request {
            feature,
            document,
            offset,
            request_id,
        } => {
            // Workspace requests use a document only for routing; their caller may
            // hold an older snapshot, which must not roll back server text.
            let needs_document = !matches!(
                feature,
                LspFeature::ExecuteCommand(_) | LspFeature::WorkspaceSymbols { .. }
            );
            if needs_document
                && documents.get(&document.uri()).is_some_and(|old| {
                    old.version > document.version
                        || old.version == document.version && old.text != document.text
                })
            {
                send_event(events, LspEvent::RequestFailed {
                    server: server.into(),
                    request_id,
                    method: feature.method().into(),
                    message: "request document is older than or conflicts with the synchronized document".into(),
                });
                return Ok(());
            }
            if needs_document
                && documents
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
                        request_id,
                        method: feature.method().into(),
                        message: "language server is still initializing".into(),
                    },
                );
                return Ok(());
            }
            if !feature.capability_for_document(capabilities, &document) {
                send_event(
                    events,
                    LspEvent::Unsupported {
                        server: server.into(),
                        method: feature.method().into(),
                    },
                );
                return Ok(());
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
            if pending.len() >= MAX_PENDING_REQUESTS {
                send_event(
                    events,
                    LspEvent::RequestFailed {
                        server: server.into(),
                        request_id,
                        method: feature.method().into(),
                        message: "language-server pending request limit reached".into(),
                    },
                );
                return Ok(());
            }
            let buffer = TextBuffer::from_text(&document.text);
            let Ok((line, character)) =
                PositionCodec::new(&buffer).byte_to_lsp(offset, capabilities.position_encoding)
            else {
                send_event(
                    events,
                    LspEvent::RequestFailed {
                        server: server.into(),
                        request_id,
                        method: feature.method().into(),
                        message: "request position is invalid for the captured document".into(),
                    },
                );
                return Ok(());
            };
            if needs_document
                && let Err(error) = sync_document(
                    stdin,
                    &document,
                    initialized,
                    capabilities,
                    documents,
                    opened_documents,
                )
            {
                send_event(
                    events,
                    LspEvent::RequestFailed {
                        server: server.into(),
                        request_id,
                        method: feature.method().into(),
                        message: format!("document synchronization failed: {error}"),
                    },
                );
                return Err(error);
            }
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
                        request_id,
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
                        request_id,
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
                write_content_length_message(
                    stdin,
                    &json!({
                        "jsonrpc":"2.0",
                        "method":"workspace/didChangeConfiguration",
                        "params":{"settings":configuration.settings.clone()}
                    }),
                )?;
            }
        }
        SessionCommand::Cancel(request_id) => {
            let ids = pending
                .iter()
                .filter_map(|(id, request)| (request.request_id == request_id).then_some(*id))
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
                    "requesting edit identity changed before response".into()
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
    Ok(())
}

fn sync_document(
    stdin: &mut impl std::io::Write,
    document: &LspDocument,
    initialized: bool,
    capabilities: &LspCapabilities,
    documents: &mut HashMap<String, LspDocument>,
    opened_documents: &mut HashMap<String, u64>,
) -> Result<(), crate::process::ProtocolError> {
    let uri = document.uri();
    if initialized {
        if let Some(version) = opened_documents.get(&uri) {
            let previous = documents
                .get(&uri)
                .filter(|previous| previous.version == *version);
            if previous.is_none_or(|previous| {
                previous.version != document.version || previous.text != document.text
            }) {
                send_change(stdin, previous, document, capabilities)?;
                if capabilities.sync_kind_for_document(document)
                    != crate::language::lsp::server::SyncKind::None
                {
                    opened_documents.insert(uri.clone(), document.version);
                }
            }
        } else {
            send_open(stdin, document)?;
            opened_documents.insert(uri.clone(), document.version);
        }
    }
    // Only advance the snapshot after its notification was written successfully.
    documents.insert(uri, document.clone());
    Ok(())
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
impl LspClient {
    pub(crate) fn replace_edit_source_fixture(
        &mut self,
        uri: String,
        source: (crate::editor::BufferId, crate::editor::text::TextVersion),
        encoding: PositionEncoding,
    ) -> crate::language::lsp::origins::EditSourceFixture {
        self.replace_edit_source_fixture_for("owned-edit-fixture", uri, source, encoding, 1)
    }
    pub(crate) fn replace_edit_source_fixture_for(
        &mut self,
        server: &str,
        uri: String,
        source: (crate::editor::BufferId, crate::editor::text::TextVersion),
        encoding: PositionEncoding,
        capacity: usize,
    ) -> crate::language::lsp::origins::EditSourceFixture {
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
        crate::language::lsp::origins::EditSourceFixture {
            events,
            commands,
            pending: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::language::lsp::session::*;

    #[test]
    fn continuous_document_commands_do_not_starve_server_responses() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::Ordering;
        let script = crate::language::lsp::tests::BOUNDARY_SERVER
            .split("m=read()\nsend(")
            .next()
            .unwrap()
            .to_owned()
            + r#"
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{'hoverProvider':True}}})
while True:
 m=read()
 if m is None: break
 if m.get('method')=='textDocument/hover':
  time.sleep(.05)
  send({'jsonrpc':'2.0','id':m['id'],'result':{'contents':'alive'}})
"#;
        let (_root, mut service, document) = crate::language::lsp::tests::boundary_service(&script);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(service.try_event(), Some(LspEvent::Running { .. })) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        service
            .request(
                "boundary",
                LspFeature::Hover,
                document.clone(),
                ByteOffset(0),
                42,
            )
            .unwrap();
        let sender = service.sessions["boundary"].sender.clone();
        let done = Arc::new(AtomicBool::new(false));
        let producer_done = done.clone();
        let producer = thread::spawn(move || {
            while !producer_done.load(Ordering::Acquire) {
                if matches!(
                    sender.try_send(SessionCommand::Sync(document.clone())),
                    Err(TrySendError::Disconnected(_))
                ) {
                    break;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut received = false;
        while Instant::now() < deadline {
            if matches!(
                service.try_event(),
                Some(LspEvent::Result { request_id: 42, .. })
            ) {
                received = true;
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        done.store(true, Ordering::Release);
        producer.join().unwrap();
        service.stop("boundary").unwrap();
        assert!(received, "server response was starved by document commands");
    }

    #[test]
    fn stderr_is_bounded_lossy_and_keeps_recent_lines() {
        let mut bytes = vec![b'x'; MAX_LOG_LINE_BYTES * 3];
        bytes.extend_from_slice(b"\ninvalid \xff\n");
        for _ in 0..MAX_LOG_LINES {
            bytes.extend_from_slice(b"startup\n");
        }
        bytes.extend_from_slice(b"runtime error\nlast line");
        let mut lines = Vec::new();
        drain_stderr(bytes.as_slice(), |line| {
            assert!(line.len() <= MAX_LOG_LINE_BYTES);
            lines.push(String::from_utf8_lossy(line).into_owned());
        });
        assert_eq!(lines[0], "x".repeat(MAX_LOG_LINE_BYTES));
        assert_eq!(lines[1], "invalid \u{fffd}");
        assert_eq!(lines[lines.len() - 2], "runtime error");
        assert_eq!(lines.last().unwrap(), "last line");
    }

    #[test]
    fn failed_sync_does_not_advance_snapshot_and_unchanged_sync_writes_nothing() {
        let mut document = LspDocument {
            path: "/fixture/main.rs".into(),
            language_id: "rust".into(),
            version: 0,
            text: "old".into(),
        };
        let mut documents = HashMap::new();
        let mut opened = HashMap::new();
        let capabilities = LspCapabilities {
            sync_kind: crate::language::lsp::SyncKind::Incremental,
            ..Default::default()
        };
        let mut wire = Vec::new();
        sync_document(
            &mut wire,
            &document,
            true,
            &capabilities,
            &mut documents,
            &mut opened,
        )
        .unwrap();
        wire.clear();
        sync_document(
            &mut wire,
            &document,
            true,
            &capabilities,
            &mut documents,
            &mut opened,
        )
        .unwrap();
        assert!(wire.is_empty());
        document.version = 1;
        document.text = "x".repeat(crate::process::MAX_PROTOCOL_MESSAGE_BYTES);
        assert!(
            sync_document(
                &mut wire,
                &document,
                true,
                &capabilities,
                &mut documents,
                &mut opened
            )
            .is_err()
        );
        assert_eq!(documents[&document.uri()].text, "old");
        assert_eq!(documents[&document.uri()].version, 0);
        document.text = "new".into();
        sync_document(
            &mut wire,
            &document,
            true,
            &capabilities,
            &mut documents,
            &mut opened,
        )
        .unwrap();
        let change = read_content_length_message(&mut std::io::Cursor::new(wire))
            .unwrap()
            .unwrap();
        assert_eq!(change["params"]["contentChanges"][0]["text"], "new");
        assert_eq!(
            change["params"]["contentChanges"][0]["range"]["end"]["character"],
            3
        );
        assert_eq!(documents[&document.uri()].version, 1);

        let mut documents = HashMap::new();
        let mut opened = HashMap::new();
        assert!(
            sync_document(
                &mut std::io::sink(),
                &LspDocument {
                    text: "x".repeat(crate::process::MAX_PROTOCOL_MESSAGE_BYTES),
                    ..document
                },
                true,
                &capabilities,
                &mut documents,
                &mut opened
            )
            .is_err()
        );
        assert!(documents.is_empty() && opened.is_empty());
    }

    #[test]
    fn request_replaces_at_capacity_and_syncs_its_captured_document_first() {
        let text = TextBuffer::from_text("old");
        let mut editor = crate::editor::EditorState::new("/fixture".into());
        let (buffer, _) = editor
            .open_text(text.clone(), Some("/fixture/main.rs".into()))
            .unwrap();
        let mut document = LspDocument {
            path: "/fixture/main.rs".into(),
            language_id: "rust".into(),
            version: 0,
            text: "old".into(),
        };
        let mut service = LspClient::new();
        let endpoint = service.replace_edit_source_fixture(
            document.uri(),
            (buffer, text.source_version()),
            PositionEncoding::Utf16,
        );
        let mut documents = HashMap::from([(document.uri(), document.clone())]);
        let mut opened = HashMap::from([(document.uri(), document.version)]);
        let mut sources = HashMap::new();
        let mut lineage = crate::language::lsp::origins::DiagnosticLineage::default();
        let mut apply = HashMap::new();
        let mut pending = HashMap::new();
        for id in 0..MAX_PENDING_REQUESTS as i64 {
            let mut captured = document.clone();
            if id > 0 {
                captured.path = format!("/fixture/{id}.rs").into();
            }
            pending.insert(
                id,
                PendingRequest {
                    feature: LspFeature::Hover,
                    document: captured,
                    request_id: id as u64,
                    deadline: Instant::now() + Duration::from_secs(1),
                    encoding: PositionEncoding::Utf16,
                    sources: Default::default(),
                },
            );
        }
        document.text = "new".into();
        document.version = 1;
        let mut wire = Vec::new();
        let mut next = MAX_PENDING_REQUESTS as i64;
        let mut stopping = false;
        handle_command(
            SessionCommand::Request {
                feature: LspFeature::Hover,
                document: document.clone(),
                offset: ByteOffset(1),
                request_id: 999,
            },
            "owned-edit-fixture",
            Path::new("/fixture"),
            Path::new("/server"),
            &mut wire,
            true,
            &LspCapabilities {
                hover: true,
                ..Default::default()
            },
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
            &mut LspServerConfiguration::standard(),
        )
        .unwrap();
        assert!(!stopping);
        assert_eq!(pending.len(), MAX_PENDING_REQUESTS);
        assert!(!pending.contains_key(&0));
        assert_eq!(pending[&(MAX_PENDING_REQUESTS as i64)].document.text, "new");
        assert!(service.try_event().is_none());
        let mut wire = std::io::Cursor::new(wire);
        let cancel = read_content_length_message(&mut wire).unwrap().unwrap();
        let change = read_content_length_message(&mut wire).unwrap().unwrap();
        let request = read_content_length_message(&mut wire).unwrap().unwrap();
        assert_eq!(cancel["method"], "$/cancelRequest");
        assert_eq!(change["method"], "textDocument/didChange");
        assert_eq!(change["params"]["textDocument"]["version"], 1);
        assert_eq!(change["params"]["contentChanges"][0]["text"], "new");
        assert_eq!(request["method"], "textDocument/hover");

        // Cached document snapshots must neither rewind text nor prevent
        // documentless code-action commands from running.
        sources.insert(document.uri(), (buffer, text.source_version()));
        let stale = LspDocument {
            version: 0,
            text: "old".into(),
            ..document.clone()
        };
        for (feature, method) in [
            (LspFeature::Hover, "textDocument/hover"),
            (
                LspFeature::WorkspaceSymbols {
                    query: "name".into(),
                },
                "workspace/symbol",
            ),
            (
                LspFeature::ExecuteCommand(crate::language::lsp::LspCommand {
                    command: "fix".into(),
                    arguments: Vec::new(),
                }),
                "workspace/executeCommand",
            ),
        ] {
            let rejected = matches!(feature, LspFeature::Hover);
            if !rejected {
                pending.clear();
            }
            let mut wire = Vec::new();
            handle_command(
                SessionCommand::Request {
                    feature,
                    document: stale.clone(),
                    offset: ByteOffset(0),
                    request_id: 1000,
                },
                "owned-edit-fixture",
                Path::new("/fixture"),
                Path::new("/server"),
                &mut wire,
                true,
                &LspCapabilities {
                    hover: true,
                    workspace_symbols: true,
                    execute_command: true,
                    ..Default::default()
                },
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
                &mut LspServerConfiguration::standard(),
            )
            .unwrap();
            assert_eq!(documents[&document.uri()].version, 1);
            assert_eq!(documents[&document.uri()].text, "new");
            assert!(sources.contains_key(&document.uri()));
            if rejected {
                assert!(wire.is_empty());
                assert_eq!(pending.len(), MAX_PENDING_REQUESTS);
                assert!(pending.contains_key(&(MAX_PENDING_REQUESTS as i64)));
                assert!(matches!(
                    service.try_event(),
                    Some(LspEvent::RequestFailed {
                        request_id: 1000,
                        ..
                    })
                ));
            } else {
                let mut wire = std::io::Cursor::new(wire);
                let request = read_content_length_message(&mut wire).unwrap().unwrap();
                assert_eq!(request["method"], method);
                assert!(read_content_length_message(&mut wire).unwrap().is_none());
                assert!(service.try_event().is_none());
            }
        }
    }

    #[test]
    fn diagnostic_tracking_exhaustion_does_not_restart_or_invalidate_tracked_documents() {
        let text = TextBuffer::from_text("text");
        let mut editor = crate::editor::EditorState::new("/fixture".into());
        let (buffer, _) = editor
            .open_text(text.clone(), Some("/fixture/main.rs".into()))
            .unwrap();
        let mut service = LspClient::new();
        let endpoint = service.replace_edit_source_fixture(
            "file:///fixture/main.rs".into(),
            (buffer, text.source_version()),
            PositionEncoding::Utf16,
        );
        let mut documents = HashMap::new();
        let mut sources = HashMap::new();
        let mut opened = HashMap::new();
        let mut pending = HashMap::new();
        let mut apply = HashMap::new();
        let mut lineage = crate::language::lsp::origins::DiagnosticLineage::default();
        let mut stopping = false;
        for index in (0..4097).chain(std::iter::once(0)) {
            let document = LspDocument {
                path: format!("/fixture/{index}.rs").into(),
                language_id: "rust".into(),
                version: 0,
                text: "text".into(),
            };
            handle_command(
                SessionCommand::SyncOwned {
                    document,
                    source: (buffer, text.source_version()),
                },
                "owned-edit-fixture",
                Path::new("/fixture"),
                Path::new("/server"),
                &mut std::io::sink(),
                true,
                &LspCapabilities::default(),
                &mut documents,
                &mut sources,
                &mut lineage,
                &mut apply,
                &mut opened,
                &mut pending,
                &mut 2,
                Duration::from_secs(1),
                0,
                &endpoint.events,
                &mut stopping,
                &mut LspServerConfiguration::standard(),
            )
            .unwrap();
            assert!(!stopping);
        }
        assert_eq!(opened.len(), 4097);
        assert!(!lineage.ambiguous("file:///fixture/0.rs"));
        assert!(lineage.ambiguous("file:///fixture/4096.rs"));
        assert!(service.try_event().is_none());
    }

    #[test]
    fn saving_an_older_snapshot_does_not_replace_synchronized_text_or_sources() {
        let mut editor = crate::editor::EditorState::new("/fixture".into());
        let text = TextBuffer::from_text("newer unsaved text");
        let (buffer, _) = editor
            .open_text(text.clone(), Some("/fixture/main.rs".into()))
            .unwrap();
        let document = LspDocument {
            path: "/fixture/main.rs".into(),
            language_id: "rust".into(),
            version: 2,
            text: "newer unsaved text".into(),
        };
        let source = (buffer, text.source_version());
        let mut service = LspClient::new();
        let endpoint = service.replace_edit_source_fixture(
            document.uri(),
            source.clone(),
            PositionEncoding::Utf16,
        );
        let mut documents = HashMap::new();
        let mut sources = HashMap::new();
        let mut opened = HashMap::new();
        let mut pending = HashMap::new();
        let mut apply = HashMap::new();
        let mut lineage = crate::language::lsp::origins::DiagnosticLineage::default();
        let mut stopping = false;
        let mut next = 2;
        let mut config = LspServerConfiguration::standard();
        let mut sink = Vec::new();
        for command in [
            SessionCommand::SyncOwned {
                document: document.clone(),
                source: source.clone(),
            },
            SessionCommand::Save(LspDocument {
                version: 1,
                text: "written".into(),
                ..document.clone()
            }),
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
            )
            .unwrap();
        }
        assert_eq!(documents[&document.uri()].text, "newer unsaved text");
        assert_eq!(documents[&document.uri()].version, 2);
        assert_eq!(sources[&document.uri()], source);
        let mut output = sink.as_slice();
        let opened = read_content_length_message(&mut output).unwrap().unwrap();
        assert_eq!(opened["method"], "textDocument/didOpen");
        let saved = read_content_length_message(&mut output).unwrap().unwrap();
        assert_eq!(saved["method"], "textDocument/didSave");
        assert_eq!(saved["params"]["text"], "written");
        assert!(read_content_length_message(&mut output).unwrap().is_none());
    }

    #[test]
    fn resumed_incremental_sync_replaces_snapshots_skipped_while_disabled() {
        use crate::editor::EditorState;
        for save in [false, true] {
            let mut editor = EditorState::new("/fixture".into());
            let text = TextBuffer::from_text("a");
            let (buffer, _) = editor.open_text(text.clone(), None).unwrap();
            let mut service = LspClient::new();
            let endpoint = service.replace_edit_source_fixture(
                "file:///fixture/main.rs".into(),
                (buffer, text.source_version()),
                PositionEncoding::Utf16,
            );
            let base = LspCapabilities {
                sync_kind: crate::language::lsp::server::SyncKind::None,
                ..Default::default()
            };
            let options = json!({"documentSelector":[{"language":"rust"}],"syncKind":2});
            let active = crate::language::lsp::server_message::effective_capabilities(
                &base,
                [("textDocument/didChange", &options)].into_iter(),
            );
            let mut documents = HashMap::new();
            let mut sources = HashMap::new();
            let mut lineage = crate::language::lsp::origins::DiagnosticLineage::default();
            let mut apply = HashMap::new();
            let mut opened = HashMap::new();
            let mut pending = HashMap::new();
            let mut next = 2;
            let mut stopping = false;
            let mut config = LspServerConfiguration::standard();
            let mut output = Vec::new();
            for (version, text) in ["a", "longer", "longer!", "longer!!"]
                .into_iter()
                .enumerate()
            {
                let document = LspDocument {
                    path: "/fixture/main.rs".into(),
                    language_id: "rust".into(),
                    version: version as u64,
                    text: text.into(),
                };
                let command = if save && version >= 2 {
                    SessionCommand::Save(document)
                } else {
                    SessionCommand::Sync(document)
                };
                handle_command(
                    command,
                    "owned-edit-fixture",
                    Path::new("/fixture"),
                    Path::new("/server"),
                    &mut output,
                    true,
                    if version >= 2 { &active } else { &base },
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
                )
                .unwrap();
            }
            let mut input = std::io::Cursor::new(output);
            let changes = std::iter::from_fn(|| {
                crate::process::read_content_length_message(&mut input).unwrap()
            })
            .filter(|message| message["method"] == "textDocument/didChange")
            .collect::<Vec<_>>();
            assert_eq!(changes.len(), 2);
            assert_eq!(changes[0]["params"]["textDocument"]["version"], 2);
            assert_eq!(
                changes[0]["params"]["contentChanges"],
                json!([{"text":"longer!"}])
            );
            assert_eq!(changes[1]["params"]["contentChanges"][0]["text"], "!");
            assert!(changes[1]["params"]["contentChanges"][0]["range"].is_object());
        }
    }

    #[test]
    fn reopening_a_document_invalidates_the_old_diagnostic_transport() {
        use crate::editor::EditorState;
        use crate::editor::text::TextBuffer;
        let mut editor = EditorState::new("/fixture".into());
        let first = TextBuffer::from_text("first");
        let (buffer, _) = editor
            .open_text(first.clone(), Some("/fixture/main.rs".into()))
            .unwrap();
        let document = LspDocument {
            path: "/fixture/main.rs".into(),
            language_id: "rust".into(),
            version: 0,
            text: "first".into(),
        };
        let mut service = LspClient::new();
        let endpoint = service.replace_edit_source_fixture(
            document.uri(),
            (buffer, first.source_version()),
            PositionEncoding::Utf16,
        );
        let mut lineage = crate::language::lsp::origins::DiagnosticLineage::default();
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
            )
            .unwrap();
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
