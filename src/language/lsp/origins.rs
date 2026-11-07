//! Private event origins and consuming replies bound to the producing session.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;

use crate::editor::text::PositionEncoding;

use crate::editor::text::TextVersion;

#[cfg(test)]
use crate::language::lsp::messages::LspDiagnostic;
use crate::language::lsp::messages::LspEvent;
#[cfg(test)]
use crate::language::lsp::messages::LspWorkspaceEdit;
use crate::language::lsp::messages::ServerRequestId;
use crate::language::lsp::session::SessionCommand;

#[derive(Clone, Debug)]
pub(crate) struct EditContext {
    pub(super) session: Arc<()>,
    pub(super) server: Arc<str>,
    pub(super) encoding: PositionEncoding,
    pub(super) diagnostic_ambiguous: bool,
    pub(super) sources: Arc<HashMap<String, (crate::editor::BufferId, TextVersion)>>,
    pub(super) disk: Option<(String, crate::editor::text::TextBuffer)>,
}
impl EditContext {
    pub(crate) fn diagnostic_producer(
        &self,
        prefix: &str,
    ) -> Result<crate::workspace::DiagnosticProducer, crate::workspace::WorkspaceError> {
        Ok(crate::workspace::DiagnosticProducer::from_runtime(
            crate::workspace::DiagnosticOwner::new(format!("{prefix}{}", self.server))?,
            self.session.clone(),
        ))
    }
    pub(crate) fn diagnostic_ambiguous(&self) -> bool {
        self.diagnostic_ambiguous
    }
    pub(crate) fn disk_document(&self, uri: &str) -> Option<&crate::editor::text::TextBuffer> {
        self.disk
            .as_ref()
            .filter(|(path, _)| path == uri)
            .map(|(_, text)| text)
    }
    pub(crate) fn encoding(&self) -> PositionEncoding {
        self.encoding
    }
    pub(crate) fn document_source(
        &self,
        uri: &str,
    ) -> Option<&(crate::editor::BufferId, TextVersion)> {
        self.sources.get(uri)
    }
}
pub(crate) struct OwnedLspEvent {
    pub event: LspEvent,
    pub context: EditContext,
    pub reply: Option<ApplyEditReply>,
}
#[derive(Clone)]
pub(super) struct EventSender {
    pub channel: SyncSender<OwnedLspEvent>,
    pub context: EditContext,
    pub commands: SyncSender<SessionCommand>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub stop_requested: Arc<std::sync::atomic::AtomicBool>,
}
impl EventSender {
    pub fn send(&self, event: LspEvent) {
        let expendable = matches!(event, LspEvent::Log { .. });
        let mut event = OwnedLspEvent {
            event,
            context: self.context.clone(),
            reply: None,
        };
        loop {
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            match self.channel.try_send(event) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(_)) if expendable => return,
                Err(TrySendError::Full(value)) => {
                    event = value;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }
    pub fn apply(
        &self,
        event: LspEvent,
        request_id: ServerRequestId,
        key: Arc<()>,
        encoding: PositionEncoding,
        sources: HashMap<String, (crate::editor::BufferId, TextVersion)>,
    ) -> bool {
        let context = EditContext {
            encoding,
            sources: Arc::new(sources),
            ..self.context.clone()
        };
        let reply = ApplyEditReply {
            sender: self.commands.clone(),
            request_id,
            key,
            context: context.clone(),
        };
        self.channel
            .try_send(OwnedLspEvent {
                event,
                context,
                reply: Some(reply),
            })
            .is_ok()
    }
}
pub(crate) struct ApplyEditReply {
    sender: SyncSender<SessionCommand>,
    request_id: ServerRequestId,
    key: Arc<()>,
    context: EditContext,
}
impl ApplyEditReply {
    pub(crate) fn context(&self) -> &EditContext {
        &self.context
    }
    pub(crate) fn finish(self, applied: bool, reason: Option<String>) -> PendingEditReply {
        let (completed, completion) = mpsc::sync_channel(1);
        PendingEditReply {
            completion,
            sender: self.sender,
            server: self.context.server,
            command: Some(SessionCommand::RespondOwnedApplyEdit {
                request_id: self.request_id,
                key: self.key,
                applied,
                reason,
                completed,
            }),
        }
    }
}
pub(crate) struct PendingEditReply {
    sender: SyncSender<SessionCommand>,
    server: Arc<str>,
    command: Option<SessionCommand>,
    completion: Receiver<Result<(), String>>,
}
impl PendingEditReply {
    pub(crate) fn try_send(&mut self) -> Result<bool, String> {
        let Some(command) = self.command.take() else {
            return match self.completion.try_recv() {
                Ok(result) => result.map(|()| true),
                Err(TryRecvError::Empty) => Ok(false),
                Err(TryRecvError::Disconnected) => Err(format!(
                    "requesting language server {} stopped before acknowledging its edit response",
                    self.server
                )),
            };
        };
        match self.sender.try_send(command) {
            Ok(()) => Ok(false),
            Err(TrySendError::Full(command)) => {
                self.command = Some(command);
                Ok(false)
            }
            Err(TrySendError::Disconnected(_)) => Err(format!(
                "requesting language server {} stopped before its edit response could be delivered",
                self.server
            )),
        }
    }
}

#[cfg(test)]
pub(crate) struct EditSourceFixture {
    pub(super) events: EventSender,
    pub(super) commands: Receiver<SessionCommand>,
    pub(super) pending: HashMap<ServerRequestId, Arc<()>>,
}
#[cfg(test)]
impl EditSourceFixture {
    pub(crate) fn context(&self) -> EditContext {
        self.events.context.clone()
    }
    pub(crate) fn publish_result(
        &self,
        request_id: u64,
        method: &str,
        document: &crate::language::lsp::messages::LspDocument,
        result: crate::language::lsp::messages::LspFeatureResult,
    ) {
        self.events.send(LspEvent::Result {
            server: self.events.context.server.to_string(),
            request_id,
            method: method.into(),
            uri: document.uri(),
            version: document.version,
            result,
        });
    }
    pub(crate) fn synced_document(&self) -> Option<crate::language::lsp::messages::LspDocument> {
        match self.commands.try_recv().ok()? {
            SessionCommand::SyncOwned { document, .. } => Some(document),
            _ => panic!("expected owned document sync"),
        }
    }
    pub(crate) fn closed_uri(&self) -> Option<String> {
        while let Ok(command) = self.commands.try_recv() {
            if let SessionCommand::Close(uri) = command {
                return Some(uri);
            }
        }
        None
    }
    pub(crate) fn saved_uri(&self) -> Option<String> {
        self.saved_document().map(|document| document.uri())
    }
    pub(crate) fn saved_document(&self) -> Option<crate::language::lsp::messages::LspDocument> {
        while let Ok(command) = self.commands.try_recv() {
            if let SessionCommand::Save(document) = command {
                return Some(document);
            }
        }
        None
    }
    pub(crate) fn configured(
        &self,
    ) -> Option<crate::language::lsp::server::LspServerConfiguration> {
        match self.commands.try_recv().ok()? {
            SessionCommand::Configure(configuration) => Some(configuration),
            _ => None,
        }
    }
    pub(crate) fn requested_feature(&self) -> Option<crate::language::lsp::messages::LspFeature> {
        while let Ok(command) = self.commands.try_recv() {
            if let SessionCommand::Request { feature, .. } = command {
                return Some(feature);
            }
        }
        None
    }
    pub(crate) fn executed_command(&self) -> Option<String> {
        while let Ok(command) = self.commands.try_recv() {
            if let SessionCommand::Request {
                feature: crate::language::lsp::messages::LspFeature::ExecuteCommand(command),
                ..
            } = command
            {
                return Some(command.command);
            }
        }
        None
    }
    pub(crate) fn request(&mut self, id: ServerRequestId, edit: LspWorkspaceEdit) -> bool {
        if self.pending.contains_key(&id) {
            return false;
        }
        let key = Arc::new(());
        if !self.events.apply(
            LspEvent::ApplyEditRequest {
                server: self.events.context.server.to_string(),
                request_id: id.clone(),
                label: None,
                edit,
            },
            id.clone(),
            key.clone(),
            self.events.context.encoding,
            (*self.events.context.sources).clone(),
        ) {
            return false;
        }
        self.pending.insert(id, key);
        true
    }
    pub(crate) fn publish_diagnostics(
        &self,
        uri: &str,
        version: Option<u64>,
        diagnostics: Vec<LspDiagnostic>,
        lineage: &DiagnosticLineage,
    ) {
        let events = EventSender {
            context: EditContext {
                diagnostic_ambiguous: lineage.ambiguous(uri),
                ..self.events.context.clone()
            },
            ..self.events.clone()
        };
        events.send(LspEvent::Diagnostics {
            server: self.events.context.server.to_string(),
            uri: uri.into(),
            version,
            diagnostics,
        });
    }
    pub(crate) fn observe_diagnostic_document(
        &mut self,
        uri: &str,
        source: (crate::editor::BufferId, TextVersion),
        lineage: &mut DiagnosticLineage,
    ) {
        lineage.observe(uri.into(), &source);
        Arc::make_mut(&mut self.events.context.sources).insert(uri.into(), source);
    }
    pub(crate) fn take_synced_source(&self) -> Option<crate::editor::BufferId> {
        match self.commands.try_recv().ok()? {
            SessionCommand::SyncOwned { source, .. } => Some(source.0),
            _ => panic!("expected owned document sync"),
        }
    }
    pub(crate) fn publish_disk_diagnostics(
        &self,
        uri: &str,
        text: &str,
        diagnostics: Vec<LspDiagnostic>,
    ) {
        let events = EventSender {
            context: EditContext {
                sources: Arc::new(HashMap::new()),
                disk: Some((uri.into(), crate::editor::text::TextBuffer::from_text(text))),
                ..self.events.context.clone()
            },
            ..self.events.clone()
        };
        events.send(LspEvent::Diagnostics {
            server: self.events.context.server.to_string(),
            uri: uri.into(),
            version: None,
            diagnostics,
        });
    }
    pub(crate) fn block_responses(&self) {
        self.events
            .commands
            .try_send(SessionCommand::Health)
            .unwrap();
    }
    pub(crate) fn unblock_responses(&self) {
        assert!(matches!(
            self.commands.try_recv(),
            Ok(SessionCommand::Health)
        ));
    }
    pub(crate) fn response(&mut self) -> Option<(ServerRequestId, bool)> {
        match self.commands.try_recv().ok()? {
            SessionCommand::RespondOwnedApplyEdit {
                request_id,
                key,
                applied,
                completed,
                ..
            } => {
                assert!(
                    self.pending
                        .remove(&request_id)
                        .is_some_and(|expected| Arc::ptr_eq(&expected, &key))
                );
                completed.try_send(Ok(())).unwrap();
                Some((request_id, applied))
            }
            _ => panic!("unexpected command in bounded edit endpoint"),
        }
    }
}

// The wire protocol has no instance identifier. Once a URI is reused, push
// diagnostics are quarantined for this session rather than relabeled as fresh.
#[derive(Default)]
pub(crate) struct DiagnosticLineage {
    first: HashMap<String, (crate::editor::BufferId, TextVersion)>,
    ambiguous: std::collections::HashSet<String>,
    exhausted: bool,
}
impl DiagnosticLineage {
    pub(crate) fn observe(&mut self, uri: String, source: &(crate::editor::BufferId, TextVersion)) {
        if let Some(first) = self.first.get(&uri) {
            if first.0 != source.0 || !first.1.same_instance(&source.1) {
                self.ambiguous.insert(uri);
            }
        } else if self.first.len() < 4096 {
            self.first.insert(uri, source.clone());
        } else {
            self.exhausted = true;
        }
    }
    pub(crate) fn tracked(&self, uri: &str) -> bool {
        self.first.contains_key(uri)
    }
    pub(crate) fn ambiguous(&self, uri: &str) -> bool {
        self.ambiguous.contains(uri) || self.exhausted && !self.tracked(uri)
    }
}

#[cfg(test)]
mod tests {
    use crate::language::lsp::origins::*;
    #[test]
    fn full_event_queue_retains_lifecycle_and_result_events() {
        let (channel, receiver) = mpsc::sync_channel(1);
        let (commands, _) = mpsc::sync_channel(1);
        let sender = EventSender {
            channel,
            commands,
            cancelled: Default::default(),
            stop_requested: Default::default(),
            context: EditContext {
                session: Arc::new(()),
                server: "fixture".into(),
                encoding: PositionEncoding::Utf16,
                diagnostic_ambiguous: false,
                sources: Arc::new(HashMap::new()),
                disk: None,
            },
        };
        sender.send(LspEvent::Log {
            server: "fixture".into(),
            method: None,
            message: "full".into(),
        });
        let worker = std::thread::spawn(move || {
            sender.send(LspEvent::Running {
                server: "fixture".into(),
                capabilities: Default::default(),
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(matches!(
            receiver.recv().unwrap().event,
            LspEvent::Log { .. }
        ));
        assert!(matches!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
                .event,
            LspEvent::Running { .. }
        ));
        worker.join().unwrap();
    }
}
