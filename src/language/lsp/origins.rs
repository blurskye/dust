//! Private event origins and consuming replies bound to the producing session.
use super::*;
use crate::editor::text::TextVersion;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct EditContext {
    pub(super) session: Arc<()>,
    pub(super) server: Arc<str>,
    pub(super) encoding: PositionEncoding,
    pub(super) diagnostic_ambiguous: bool,
    pub(super) sources: Arc<HashMap<String, (crate::editor::BufferId, TextVersion)>>,
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
}
impl EventSender {
    pub fn send(&self, event: LspEvent) {
        let _ = self.channel.try_send(OwnedLspEvent {
            event,
            context: self.context.clone(),
            reply: None,
        });
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

// The wire protocol has no incarnation identifier. Once a URI is reused, push
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
            if first.0 != source.0 || !first.1.same_incarnation(&source.1) {
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
        self.exhausted || self.ambiguous.contains(uri)
    }
}
