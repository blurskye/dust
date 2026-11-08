//! Reverse requests, one-shot reply mailboxes and pipe-write completion.
use crate::debugger::DapControl;
use crate::debugger::DapError;
use crate::debugger::session::Writer;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DapReplyOutcome {
    AwaitingResponse,
    Ready,
    WriterQueued,
    Written,
    Failed,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct DapTerminalReply {
    inner: Arc<ReplySender>,
}
#[derive(Clone, Debug)]
pub(crate) struct DapReplyDelivery {
    live: Arc<AtomicBool>,
    outcome: Arc<AtomicU8>,
}
impl DapReplyDelivery {
    pub(crate) fn outcome(&self) -> DapReplyOutcome {
        outcome(&self.live, &self.outcome)
    }
}
fn outcome(live: &AtomicBool, state: &AtomicU8) -> DapReplyOutcome {
    match state.load(Ordering::Acquire) {
        0 if live.load(Ordering::Acquire) => DapReplyOutcome::AwaitingResponse,
        1 if live.load(Ordering::Acquire) => DapReplyOutcome::Ready,
        2 if live.load(Ordering::Acquire) => DapReplyOutcome::WriterQueued,
        3 => DapReplyOutcome::Written,
        4 => DapReplyOutcome::Failed,
        _ => DapReplyOutcome::Unknown,
    }
}
#[derive(Debug)]
struct ReplySender {
    sender: SyncSender<DapControl>,
    sequence: i64,
    live: Arc<AtomicBool>,
    used: Arc<AtomicBool>,
    claimed: AtomicBool,
    outcome: Arc<AtomicU8>,
}
impl ReplySender {
    fn submit(&self, success: bool, message: Option<String>) -> Result<(), DapError> {
        if message.as_ref().is_some_and(|m| m.len() > 64 * 1024) {
            return Err(DapError::InvalidConfiguration);
        }
        if !self.live.load(Ordering::Acquire) || self.used.swap(true, Ordering::AcqRel) {
            return Err(DapError::StaleContext);
        }
        // Exactly one producer wins used. The original mailbox reserves one slot;
        // it cannot be full from another request or from the writer's backpressure.
        self.outcome
            .store(DapReplyOutcome::Ready as u8, Ordering::Release);
        self.sender
            .try_send(DapControl::ReverseResponse {
                request_seq: self.sequence,
                success,
                message,
                process_id: None,
            })
            .map_err(|_| {
                self.outcome
                    .store(DapReplyOutcome::Unknown as u8, Ordering::Release);
                DapError::StaleContext
            })
    }
}
impl Drop for ReplySender {
    fn drop(&mut self) {
        if self.claimed.load(Ordering::Acquire) && !self.used.load(Ordering::Acquire) {
            let _ = self.submit(
                false,
                Some("terminal request scope discarded before a response".into()),
            );
        }
    }
}
impl DapTerminalReply {
    pub(crate) fn claim(self) -> Self {
        self.inner.claimed.store(true, Ordering::Release);
        self
    }
    pub(crate) fn current(&self) -> bool {
        self.inner.live.load(Ordering::Acquire) && !self.inner.used.load(Ordering::Acquire)
    }
    pub(crate) fn respond(&self, success: bool, message: Option<String>) -> Result<(), DapError> {
        self.inner.submit(success, message)
    }
    pub(crate) fn delivery(&self) -> DapReplyDelivery {
        DapReplyDelivery {
            live: self.inner.live.clone(),
            outcome: self.inner.outcome.clone(),
        }
    }
    /// Written means write+flush completed on the original pipe, not peer acknowledgement.
    pub fn outcome(&self) -> DapReplyOutcome {
        outcome(&self.inner.live, &self.inner.outcome)
    }
}
pub(super) struct ReverseRequest {
    pub sequence: i64,
    receiver: Receiver<DapControl>,
    used: Arc<AtomicBool>,
    outcome: Arc<AtomicU8>,
    ready: Option<Value>,
}
impl ReverseRequest {
    pub fn reserve(sequence: i64, live: Arc<AtomicBool>) -> (Self, DapTerminalReply) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let used = Arc::new(AtomicBool::new(false));
        let outcome = Arc::new(AtomicU8::new(0));
        (
            Self {
                sequence,
                receiver,
                used: used.clone(),
                outcome: outcome.clone(),
                ready: None,
            },
            DapTerminalReply {
                inner: Arc::new(ReplySender {
                    sender,
                    sequence,
                    live,
                    used,
                    claimed: AtomicBool::new(false),
                    outcome,
                }),
            },
        )
    }
    // Raw public compatibility controls have no editor provenance. They may only
    // complete an existing request, once; they cannot manufacture a reverse RPC.
    pub fn raw(&mut self, control: DapControl) {
        if !self.used.swap(true, Ordering::AcqRel) {
            self.prepare(control);
        }
    }
    fn prepare(&mut self, control: DapControl) {
        if let DapControl::ReverseResponse {
            request_seq,
            success,
            message,
            process_id,
        } = control
        {
            let body = process_id.map_or_else(|| json!({}), |id| json!({"processId":id}));
            let mut value = json!({"seq":0,"type":"response","request_seq":request_seq,"success":success,"command":"runInTerminal","body":body});
            if let Some(message) = message {
                value["message"] = Value::String(message);
            }
            self.ready = Some(value);
            self.outcome
                .store(DapReplyOutcome::Ready as u8, Ordering::Release);
        }
    }
    pub fn flush(&mut self, writer: &mut Writer, sequence: &mut i64) -> bool {
        if self.ready.is_none()
            && let Ok(control) = self.receiver.try_recv()
        {
            self.prepare(control);
        }
        if let Some(mut value) = self.ready.take() {
            // Assign the actual wire ID at enqueue, not while waiting for space:
            // intervening requests and other ready replies must keep wire order.
            value["seq"] = Value::from(*sequence);
            match writer.reply(value, self.outcome.clone()) {
                Ok(()) => *sequence += 1,
                Err(value) => self.ready = Some(value),
            }
        }
        matches!(self.outcome.load(Ordering::Acquire), 3 | 4)
    }
}
impl Drop for ReverseRequest {
    fn drop(&mut self) {
        self.used.store(true, Ordering::Release);
        // Closed original mailboxes refuse even while the actor is still alive.
        // A queued writer retains its delivery and may still finish later.
        let _ = self
            .outcome
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state <= DapReplyOutcome::Ready as u8).then_some(DapReplyOutcome::Unknown as u8)
            });
    }
}

/// Validate the entire request before allocating an argv/environment projection.
/// Null environment entries (unset semantics) are deliberately unsupported. An empty `cwd`, as
/// CodeLLDB sends, leaves the directory to the client.
pub(super) fn terminal_arguments(
    message: &Value,
) -> Option<(
    std::path::PathBuf,
    Vec<String>,
    std::collections::HashMap<String, String>,
)> {
    if message
        .pointer("/arguments/kind")
        .is_some_and(|v| v.as_str() != Some("integrated"))
        || message
            .pointer("/arguments/argsCanBeInterpretedByShell")
            .is_some_and(|v| v.as_bool() != Some(false))
    {
        return None;
    }
    let cwd = message.pointer("/arguments/cwd")?.as_str()?;
    let args = message.pointer("/arguments/args")?.as_array()?;
    let env = message.pointer("/arguments/env").and_then(Value::as_object);
    if message.pointer("/arguments/env").is_some() && env.is_none() {
        return None;
    }
    let text = |s: &str| s.len() <= 64 * 1024 && !s.contains('\0');
    if !text(cwd)
        || !(cwd.is_empty() || std::path::Path::new(cwd).is_absolute())
        || args.is_empty()
        || args.len() > 4096
        || env.is_some_and(|e| e.len() > 4096)
    {
        return None;
    }
    let mut bytes = cwd.len();
    for a in args {
        let s = a.as_str()?;
        if !text(s) {
            return None;
        }
        bytes = bytes.checked_add(s.len())?;
    }
    if let Some(env) = env {
        for (k, v) in env {
            let v = v.as_str()?;
            if !text(k) || k.is_empty() || k.contains('=') || !text(v) {
                return None;
            }
            bytes = bytes.checked_add(k.len())?.checked_add(v.len())?;
        }
    }
    if bytes > 1024 * 1024 {
        return None;
    }
    Some((
        cwd.into(),
        args.iter()
            .map(|v| v.as_str().expect("validated arg").into())
            .collect(),
        env.into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_str().expect("validated env").into()))
            .collect(),
    ))
}

#[cfg(test)]
pub(crate) fn fixture() {
    let live = Arc::new(AtomicBool::new(true));
    let (mut original, reply) = ReverseRequest::reserve(7, live.clone());
    let reply = reply.claim();
    let duplicate = reply.clone();
    reply.respond(true, None).unwrap();
    assert!(duplicate.respond(false, None).is_err());
    assert_eq!(reply.outcome(), DapReplyOutcome::Ready);
    original.prepare(original.receiver.try_recv().unwrap());
    assert_eq!(original.ready.as_ref().unwrap()["request_seq"], 7);
    assert!(original.receiver.try_recv().is_err());
    let (unclaimed, token) = ReverseRequest::reserve(8, live.clone());
    drop(token);
    assert!(unclaimed.receiver.try_recv().is_err());
    let (claimed, token) = ReverseRequest::reserve(9, live.clone());
    drop(token.claim());
    assert!(matches!(
        claimed.receiver.try_recv(),
        Ok(DapControl::ReverseResponse { success: false, .. })
    ));
    let (_, closed) = ReverseRequest::reserve(7, live.clone());
    assert!(
        !closed.current(),
        "closed mailbox still offered reply authority"
    );
    assert_eq!(closed.outcome(), DapReplyOutcome::Unknown);
    live.store(false, Ordering::Release);
    assert!(!closed.current());
    assert!(closed.respond(true, None).is_err());
    // Reverse requests need an absolute cwd, which on Windows has a drive.
    let cwd = crate::files::absolute_test_path("/owned");
    let cwd = cwd.to_str().unwrap();
    let invalid = json!({"arguments":{"cwd":cwd,"args":["ok",12],"env":{}}});
    assert!(terminal_arguments(&invalid).is_none());
    let oversized = json!({"arguments":{"cwd":cwd,"args":vec!["x";4097],"env":{}}});
    assert!(terminal_arguments(&oversized).is_none());
    assert!(
        terminal_arguments(&json!({"arguments":{"cwd":cwd,"args":["ok"],"env":{"UNSET":null}}}))
            .is_none()
    );
    assert!(
        terminal_arguments(&json!({"arguments":{"cwd":cwd,"args":["ok"],"kind":"external"}}))
            .is_none()
    );
    assert!(
        terminal_arguments(
            &json!({"arguments":{"cwd":cwd,"args":["ok"],"argsCanBeInterpretedByShell":true}})
        )
        .is_none()
    );
    assert!(terminal_arguments(&json!({"arguments":{"cwd":cwd,"args":["ok"]}})).is_some());
}
