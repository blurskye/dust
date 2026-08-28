use super::*;

#[derive(Clone, Debug)]
pub(super) enum SessionCommand {
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
    Configure(LspServerConfiguration),
    Health,
    RespondApplyEdit {
        request_id: ServerRequestId,
        applied: bool,
        reason: Option<String>,
    },
    Stop,
}

pub(super) struct SessionHandle {
    sender: SyncSender<SessionCommand>,
    thread: Option<JoinHandle<()>>,
}

pub struct LspService {
    events: SyncSender<LspEvent>,
    receiver: Receiver<LspEvent>,
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
    pub environment: BTreeMap<String, String>,
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
        let events = self.events.clone();
        let server = session_key.to_owned();
        let executable_name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&definition.executable)
            .to_owned();
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
                    arguments,
                    root,
                    document,
                    policy,
                    configuration,
                    environment,
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
pub(super) struct PendingRequest {
    pub(super) feature: LspFeature,
    pub(super) document: LspDocument,
    pub(super) generation: u64,
    pub(super) deadline: Instant,
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
    environment: BTreeMap<String, String>,
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
                    &mut configuration,
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
                        ServerMessageContext {
                            server: &server,
                            stdin: &mut stdin,
                            events: &events,
                            logs: &mut logs,
                            base_capabilities: &base_capabilities,
                            capabilities: &mut capabilities,
                            registrations: &mut dynamic_registrations,
                            configuration: &configuration,
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
pub(super) fn handle_command(
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
    configuration: &mut LspServerConfiguration,
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
