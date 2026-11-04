//! Bounded adapter admission, original actor contexts and worker-owned pipe I/O.
use super::{DapControl, DapError, DapEvent, DapLaunch, run_adapter};
use crate::process::write_content_length_message;
use serde_json::Value;
use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

const CAPACITY: usize = 32;
const RESIDENT: usize = 8;

#[derive(Debug)]
pub(super) struct Actor {
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) supports_terminate: AtomicBool,
    launch: bool,
    next_request: AtomicU64,
    retired: AtomicBool,
    done: AtomicBool,
    pub(super) generation: AtomicU64,
    pub(super) live: Arc<AtomicBool>,
    failure: Mutex<Option<String>>,
    pub(super) stderr: Mutex<Vec<u8>>,
    #[cfg(test)]
    pub(super) writer_paused: AtomicBool,
    #[cfg(test)]
    pub(super) writer_waiting: AtomicBool,
    #[cfg(test)]
    pub(super) writes_queued: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    reverse_retries: std::sync::atomic::AtomicUsize,
}
#[derive(Clone, Debug)]
pub(crate) struct DapContext {
    actor: Arc<Actor>,
    generation: u64,
}
pub(super) struct Control {
    pub(super) value: DapControl,
    pub(super) request_id: u64,
    pub(super) generation: u64,
}
#[derive(Clone)]
pub(super) struct Events {
    sender: SyncSender<(DapContext, DapEvent)>,
    pub(super) actor: Arc<Actor>,
    _resident: Arc<()>,
}
impl Events {
    // Report worker failures after cleanup, when stderr has finished draining.
    pub(super) fn fail(&self, message: String) {
        if let Ok(mut failure) = self.actor.failure.lock() {
            *failure = Some(message);
        }
    }
    pub(super) fn send(&self, event: DapEvent) -> Result<(), ()> {
        let context = DapContext {
            actor: self.actor.clone(),
            generation: self.actor.generation.load(Ordering::Acquire),
        };
        deliver(&self.sender, (context, event), &self.actor.cancel)
            .then_some(())
            .ok_or(())
    }
}
pub(super) fn deliver<T>(sender: &SyncSender<T>, mut value: T, cancel: &AtomicBool) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(value) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(returned)) => value = returned,
        }
        thread::sleep(Duration::from_millis(2));
    }
}
struct Session {
    name: String,
    actor: Arc<Actor>,
    sender: SyncSender<Control>,
    receiver: Receiver<(DapContext, DapEvent)>,
    reported: bool,
}
pub struct DapService {
    session: Option<Session>,
    residents: Vec<Weak<()>>,
    closed: bool,
}
impl DapService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: None,
            residents: Vec::new(),
            closed: false,
        }
    }
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| !s.actor.done.load(Ordering::Acquire))
    }
    #[must_use]
    pub fn active_adapter(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.name.as_str())
    }
    pub(crate) fn pending(&self) -> usize {
        self.residents
            .iter()
            .filter(|r| r.strong_count() != 0)
            .count()
    }
    pub(crate) fn context(&self) -> Option<DapContext> {
        let session = self.session.as_ref()?;
        Some(DapContext {
            actor: session.actor.clone(),
            generation: session.actor.generation.load(Ordering::Acquire),
        })
    }
    pub(crate) fn current(&self, source: &DapContext) -> bool {
        self.session.as_ref().is_some_and(|s| {
            Arc::ptr_eq(&s.actor, &source.actor)
                && !s.actor.retired.load(Ordering::Acquire)
                && !s.actor.cancel.load(Ordering::Acquire)
                && !s.actor.done.load(Ordering::Acquire)
                && s.actor.live.load(Ordering::Acquire)
                && source.generation == s.actor.generation.load(Ordering::Acquire)
        })
    }
    #[cfg(test)]
    pub(super) fn writer_gate(&self, paused: bool) {
        self.session
            .as_ref()
            .unwrap()
            .actor
            .writer_paused
            .store(paused, Ordering::Release);
    }
    #[cfg(test)]
    pub(super) fn writer_waiting(&self) -> bool {
        self.session
            .as_ref()
            .unwrap()
            .actor
            .writer_waiting
            .load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(super) fn writes_queued(&self) -> usize {
        self.session
            .as_ref()
            .unwrap()
            .actor
            .writes_queued
            .load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(super) fn reverse_retries(&self) -> usize {
        self.session
            .as_ref()
            .unwrap()
            .actor
            .reverse_retries
            .load(Ordering::Acquire)
    }
    pub(crate) fn available(&self) -> bool {
        !self.closed && self.session.is_none() && self.pending() < RESIDENT
    }
    pub(crate) fn admission(&mut self) -> Result<DapAdmission<'_>, DapError> {
        if self.closed {
            return Err(DapError::NotRunning);
        }
        if self.session.is_some() {
            return Err(DapError::AlreadyRunning);
        }
        self.residents.retain(|r| r.strong_count() != 0);
        if self.residents.len() >= RESIDENT {
            return Err(DapError::Full);
        }
        let resident = Arc::new(());
        self.residents.push(Arc::downgrade(&resident));
        Ok(DapAdmission {
            service: self,
            resident,
        })
    }
    pub fn start_absolute(&mut self, launch: DapLaunch) -> Result<(), DapError> {
        self.admission()?.submit(launch, None)
    }
    fn submit(
        &mut self,
        launch: DapLaunch,
        scope: Option<ExecutionScope>,
        resident: Arc<()>,
    ) -> Result<(), DapError> {
        if !launch.executable.is_absolute()
            || !launch.root.is_absolute()
            || !matches!(launch.request.as_str(), "launch" | "attach")
            || launch.arguments.len() > 4096
            || launch.environment.len() > 4096
            || launch.path_maps.len() > 4096
            || !json_bounded(&launch.request_arguments)
            || launch.arguments.iter().map(String::len).sum::<usize>() > 1024 * 1024
            || launch
                .environment
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                > 1024 * 1024
        {
            return Err(DapError::InvalidConfiguration);
        }
        let actor = Arc::new(Actor {
            cancel: Arc::new(AtomicBool::new(false)),
            supports_terminate: AtomicBool::new(false),
            launch: launch.request == "launch",
            next_request: AtomicU64::new(1),
            retired: AtomicBool::new(false),
            done: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            live: Arc::new(AtomicBool::new(true)),
            failure: Mutex::new(None),
            stderr: Mutex::new(Vec::new()),
            #[cfg(test)]
            writer_paused: AtomicBool::new(false),
            #[cfg(test)]
            writer_waiting: AtomicBool::new(false),
            #[cfg(test)]
            writes_queued: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            reverse_retries: std::sync::atomic::AtomicUsize::new(0),
        });
        let (sender, controls) = mpsc::sync_channel(CAPACITY);
        let (events, receiver) = mpsc::sync_channel(CAPACITY);
        let events = Events {
            sender: events,
            actor: actor.clone(),
            _resident: resident.clone(),
        };
        let name = launch.adapter.clone();
        events
            .send(DapEvent::Starting {
                adapter: name.clone(),
            })
            .ok();
        let finish = actor.clone();
        thread::Builder::new()
            .name(format!("dust-dap-{name}"))
            .spawn(move || {
                struct Finish(Arc<Actor>);
                impl Drop for Finish {
                    fn drop(&mut self) {
                        self.0.live.store(false, Ordering::Release);
                        self.0.cancel.store(true, Ordering::Release);
                        self.0.done.store(true, Ordering::Release);
                    }
                }
                let _finish = Finish(finish);
                run_adapter(launch, controls, events, scope);
            })
            .map_err(DapError::Thread)?;
        self.session = Some(Session {
            name,
            actor,
            sender,
            receiver,
            reported: false,
        });
        Ok(())
    }
    pub fn control(&self, control: DapControl) -> Result<(), DapError> {
        let source = self.context().ok_or(DapError::NotRunning)?;
        self.control_owned(&source, control)
    }
    pub(crate) fn control_owned(
        &self,
        source: &DapContext,
        control: DapControl,
    ) -> Result<(), DapError> {
        self.control_tracked(source, control).map(|_| ())
    }
    pub(crate) fn control_tracked(
        &self,
        source: &DapContext,
        control: DapControl,
    ) -> Result<u64, DapError> {
        if !self.current(source) {
            return Err(DapError::Retired);
        }
        let session = self.session.as_ref().ok_or(DapError::NotRunning)?;
        if matches!(control, DapControl::Stop) {
            session.actor.cancel.store(true, Ordering::Release);
            session.actor.live.store(false, Ordering::Release);
            return Ok(0);
        }
        if session.actor.cancel.load(Ordering::Acquire) {
            return Err(DapError::NotRunning);
        }
        if let DapControl::Disconnect { terminate_debuggee } = &control
            && *terminate_debuggee != session.actor.launch
            && !session.actor.supports_terminate.load(Ordering::Acquire)
        {
            return Err(DapError::InvalidConfiguration);
        }
        if !control_bounded(&control) {
            return Err(DapError::InvalidConfiguration);
        }
        let request_id = session.actor.next_request.fetch_add(1, Ordering::AcqRel);
        if request_id == u64::MAX {
            return Err(DapError::InvalidConfiguration);
        }
        session
            .sender
            .try_send(Control {
                request_id,
                value: control,
                generation: source.generation,
            })
            .map(|()| request_id)
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => DapError::Full,
                mpsc::TrySendError::Disconnected(_) => DapError::NotRunning,
            })
    }
    pub fn stop(&mut self) -> Result<(), DapError> {
        let session = self.session.take().ok_or(DapError::NotRunning)?;
        session.actor.retired.store(true, Ordering::Release);
        session.actor.live.store(false, Ordering::Release);
        session.actor.cancel.store(true, Ordering::Release);
        // Dropping the receiver retires this actor's events, never the replacement.
        Ok(())
    }
    pub(crate) fn begin_shutdown(&mut self) {
        self.closed = true;
        let _ = self.stop();
    }
    pub fn try_event(&mut self) -> Option<DapEvent> {
        self.try_owned_event().map(|(_, event)| event)
    }
    pub(crate) fn try_owned_event(&mut self) -> Option<(DapContext, DapEvent)> {
        let session = self.session.as_mut()?;
        for _ in 0..CAPACITY {
            let result = session.receiver.try_recv();
            let result = if result.is_err() && session.actor.done.load(Ordering::Acquire) {
                // Acquire completion, then recheck the queue before synthesizing a terminal.
                session.receiver.try_recv()
            } else {
                result
            };
            match result {
                Ok((mut source, mut event)) => {
                    let session_scoped = matches!(
                        &event,
                        DapEvent::Response {
                            response: super::DapResponse::Breakpoints(_)
                                | super::DapResponse::Disconnected { .. }
                                | super::DapResponse::BreakpointChanged { .. },
                            ..
                        }
                    ) || matches!(&event, DapEvent::RequestFailed { command, .. } if command == "setBreakpoints" || command == "disconnect");
                    if session_scoped {
                        source.generation = session.actor.generation.load(Ordering::Acquire);
                    }
                    if matches!(
                        event,
                        DapEvent::Response { .. }
                            | DapEvent::RequestFailed { .. }
                            | DapEvent::Stopped { .. }
                    ) && source.generation != session.actor.generation.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    if let DapEvent::Failed { message, .. } = &mut event {
                        append_stderr(message, &session.actor);
                    }
                    if matches!(event, DapEvent::Terminated { .. } | DapEvent::Failed { .. }
                        | DapEvent::Response { response: super::DapResponse::Disconnected { .. }, .. }) {
                        session.reported = true;
                    }
                    return Some((source, event));
                }
                Err(_) if session.actor.done.load(Ordering::Acquire) && !session.reported => {
                    session.reported = true;
                    let mut message = session
                        .actor
                        .failure
                        .lock()
                        .ok()
                        .and_then(|mut f| f.take())
                        .unwrap_or_else(|| {
                            "DAP worker stopped; outstanding request/write outcomes unknown".into()
                        });
                    append_stderr(&mut message, &session.actor);
                    return Some((
                        DapContext {
                            actor: session.actor.clone(),
                            generation: session.actor.generation.load(Ordering::Acquire),
                        },
                        DapEvent::Failed {
                            adapter: session.name.clone(),
                            message,
                        },
                    ));
                }
                Err(_) => return None,
            }
        }
        None
    }
}
fn append_stderr(message: &mut String, actor: &Actor) {
    if let Ok(stderr) = actor.stderr.lock() {
        let detail = super::sanitize_output(&String::from_utf8_lossy(&stderr));
        if !detail.trim().is_empty() {
            message.push_str("\nadapter stderr: ");
            message.push_str(detail.trim());
        }
    }
}

impl Default for DapService {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for DapService {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

pub(super) struct Writer {
    sender: SyncSender<(Value, Option<Arc<AtomicU8>>)>,
    actor: Arc<Actor>,
}
impl Writer {
    pub(super) fn start(
        pipe: std::process::ChildStdin,
        events: Events,
    ) -> std::io::Result<(Self, thread::JoinHandle<()>)> {
        let mut pipe = crate::process::CancellablePipe::new(pipe, events.actor.cancel.clone())?;
        let (sender, receiver) = mpsc::sync_channel::<(Value, Option<Arc<AtomicU8>>)>(CAPACITY);
        let actor = events.actor.clone();
        let worker = thread::Builder::new()
            .name("dust-dap-writer".into())
            .spawn(move || {
                let _resident = events;
                while !_resident.actor.cancel.load(Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(10)) {
                        Ok((value, receipt)) => {
                            #[cfg(test)]
                            {
                                _resident.actor.writes_queued.fetch_sub(1, Ordering::AcqRel);
                                _resident
                                    .actor
                                    .writer_waiting
                                    .store(true, Ordering::Release);
                                while _resident.actor.writer_paused.load(Ordering::Acquire)
                                    && !_resident.actor.cancel.load(Ordering::Acquire)
                                {
                                    thread::sleep(Duration::from_millis(2));
                                }
                                _resident
                                    .actor
                                    .writer_waiting
                                    .store(false, Ordering::Release);
                            }
                            if _resident.actor.cancel.load(Ordering::Acquire) {
                                break;
                            }
                            if let Err(error) = write_content_length_message(&mut pipe, &value) {
                                if let Some(receipt) = &receipt {
                                    receipt.store(
                                        super::DapReplyOutcome::Failed as u8,
                                        Ordering::Release,
                                    );
                                }
                                if let Ok(mut failure) = _resident.actor.failure.lock() {
                                    *failure = Some(format!("DAP writer failed: {error}"));
                                }
                                _resident.actor.live.store(false, Ordering::Release);
                                _resident.actor.cancel.store(true, Ordering::Release);
                                break;
                            }
                            if let Some(receipt) = receipt {
                                receipt.store(
                                    super::DapReplyOutcome::Written as u8,
                                    Ordering::Release,
                                );
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok((Self { sender, actor }, worker))
    }
    pub(super) fn reply(&mut self, value: Value, receipt: Arc<AtomicU8>) -> Result<(), Value> {
        #[cfg(test)]
        self.actor.writes_queued.fetch_add(1, Ordering::AcqRel);
        match self.sender.try_send((value, Some(receipt.clone()))) {
            Ok(()) => {
                // Publish admission only after enqueue; do not overwrite a fast
                // writer's already-completed receipt.
                let _ = receipt.compare_exchange(
                    super::DapReplyOutcome::Ready as u8,
                    super::DapReplyOutcome::WriterQueued as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                Ok(())
            }
            Err(error) => {
                #[cfg(test)]
                self.actor.writes_queued.fetch_sub(1, Ordering::AcqRel);
                let (value, state) = match error {
                    mpsc::TrySendError::Full((value, _)) => {
                        #[cfg(test)]
                        self.actor.reverse_retries.fetch_add(1, Ordering::AcqRel);
                        (value, super::DapReplyOutcome::Ready)
                    }
                    mpsc::TrySendError::Disconnected((value, _)) => {
                        self.actor.live.store(false, Ordering::Release);
                        self.actor.cancel.store(true, Ordering::Release);
                        (value, super::DapReplyOutcome::Unknown)
                    }
                };
                receipt.store(state as u8, Ordering::Release);
                Err(value)
            }
        }
    }
    pub(super) fn submit(&mut self, value: Value) -> Result<(), ()> {
        // Full is an explicit failed session, not silent loss or a blocking write.
        #[cfg(test)]
        self.actor.writes_queued.fetch_add(1, Ordering::AcqRel);
        if self.sender.try_send((value, None)).is_err() {
            #[cfg(test)]
            self.actor.writes_queued.fetch_sub(1, Ordering::AcqRel);
            if let Ok(mut failure) = self.actor.failure.lock() {
                *failure = Some("DAP writer full/stopped; admitted write outcomes unknown".into());
            }
            self.actor.live.store(false, Ordering::Release);
            self.actor.cancel.store(true, Ordering::Release);
            return Err(());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct TestAdapter {
    events: Events,
    controls: Receiver<Control>,
}
#[cfg(test)]
impl TestAdapter {
    pub(crate) fn event(&self, generation: u64, event: DapEvent) {
        self.events
            .actor
            .generation
            .store(generation, Ordering::Release);
        self.events.send(event).unwrap();
    }
    pub(crate) fn advance(&self, generation: u64) {
        self.events
            .actor
            .generation
            .store(generation, Ordering::Release);
    }
    pub(crate) fn control(&self) -> Option<DapControl> {
        self.controls.try_recv().ok().map(|c| c.value)
    }
}
#[cfg(test)]
pub(crate) fn fixture() -> (DapService, TestAdapter) {
    let actor = Arc::new(Actor {
        cancel: Arc::new(AtomicBool::new(false)),
        supports_terminate: AtomicBool::new(true),
        launch: true,
        next_request: AtomicU64::new(1),
        retired: AtomicBool::new(false),
        done: AtomicBool::new(false),
        generation: AtomicU64::new(0),
        live: Arc::new(AtomicBool::new(true)),
        failure: Mutex::new(None),
        stderr: Mutex::new(Vec::new()),
        writer_paused: AtomicBool::new(false),
        writer_waiting: AtomicBool::new(false),
        writes_queued: std::sync::atomic::AtomicUsize::new(0),
        reverse_retries: std::sync::atomic::AtomicUsize::new(0),
    });
    let (sender, controls) = mpsc::sync_channel(CAPACITY);
    let (events, receiver) = mpsc::sync_channel(CAPACITY);
    let events = Events {
        sender: events,
        actor: actor.clone(),
        _resident: Arc::new(()),
    };
    (
        DapService {
            session: Some(Session {
                name: "same-name".into(),
                actor,
                sender,
                receiver,
                reported: false,
            }),
            residents: Vec::new(),
            closed: false,
        },
        TestAdapter { events, controls },
    )
}

fn control_bounded(control: &DapControl) -> bool {
    match control {
        DapControl::Evaluate {
            expression,
            context,
            ..
        } => expression.len() <= 64 * 1024 && context.len() <= 1024,
        DapControl::SetBreakpoints { path, breakpoints } => {
            path.as_os_str().len() <= 64 * 1024
                && breakpoints.len() <= 10_000
                && breakpoints
                    .iter()
                    .try_fold(0usize, |total, row| {
                        let mut total = total;
                        for value in [
                            row.condition.as_deref(),
                            row.hit_condition.as_deref(),
                            row.log_message.as_deref(),
                        ]
                        .into_iter()
                        .flatten()
                        {
                            if value.len() > 64 * 1024 {
                                return None;
                            }
                            total = total.checked_add(value.len())?;
                            if total > 1024 * 1024 {
                                return None;
                            }
                        }
                        Some(total)
                    })
                    .is_some()
        }
        DapControl::ReverseResponse { message, .. } => {
            message.as_ref().is_none_or(|s| s.len() <= 64 * 1024)
        }
        _ => true,
    }
}
fn json_bounded(value: &Value) -> bool {
    struct Limit(usize);
    impl std::io::Write for Limit {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= 16 * 1024 * 1024)
                .ok_or_else(|| std::io::Error::other("DAP launch JSON exceeds limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limit(0), value).is_ok()
}

/// Exclusive reservation in the actual resident ledger, retained through worker registration.
pub(crate) struct DapAdmission<'a> {
    service: &'a mut DapService,
    resident: Arc<()>,
}
impl DapAdmission<'_> {
    pub(crate) fn submit(
        self,
        launch: DapLaunch,
        scope: Option<ExecutionScope>,
    ) -> Result<(), DapError> {
        self.service.submit(launch, scope, self.resident)
    }
}
pub(crate) struct ExecutionScope {
    pub(crate) roots: Vec<crate::workspace::RootIdentity>,
    pub(crate) paths: Vec<std::path::PathBuf>,
}
impl ExecutionScope {
    pub(super) fn check(&self) -> Result<(), String> {
        let canonical = self
            .paths
            .iter()
            .map(std::fs::canonicalize)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let requested = self.paths.iter().map(|p| p.as_path()).collect::<Vec<_>>();
        let canonical = canonical.iter().map(|p| p.as_path()).collect::<Vec<_>>();
        let selected = crate::files::participating_roots(&requested, &canonical, &self.roots)
            .map_err(|e| e.to_string())?;
        crate::files::roots_current(&selected).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;
    use crate::debugger::{DapResponse, DapSourceBreakpoint};
    #[test]
    fn unsupported_detach_is_refused_before_admission() {
        let (service, adapter) = fixture();
        service
            .session
            .as_ref()
            .unwrap()
            .actor
            .supports_terminate
            .store(false, Ordering::Release);
        assert!(
            service
                .control(DapControl::Disconnect {
                    terminate_debuggee: false
                })
                .is_err()
        );
        assert!(adapter.control().is_none());
        service
            .control(DapControl::Disconnect {
                terminate_debuggee: true,
            })
            .unwrap();
        assert!(matches!(
            adapter.control(),
            Some(DapControl::Disconnect {
                terminate_debuggee: true
            })
        ));
    }
    #[test]
    fn queued_session_controls_survive_new_stop_generations() {
        use crate::debugger::transport_tests::{ADAPTER, python};
        use serde_json::json;
        use std::{collections::{BTreeMap, HashMap}, time::Instant};
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("queued.py");
        let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
        let body = r#"
while True:
 m=read()
 if m is None: break
 c=m['command']; body={}
 if c=='initialize': body={'supportsConfigurationDoneRequest':True}
 if c=='setBreakpoints': body={'breakpoints':[]}
 if c=='threads': body={'threads':[]}
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c=='launch': send({'type':'event','event':'initialized','body':{}})
 if c=='configurationDone': send({'type':'event','event':'stopped','body':{'reason':'entry','threadId':2}})
 if c=='disconnect': break
"#;
        std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
        let mut service = DapService::new();
        service.start_absolute(DapLaunch {
            adapter: "queued".into(), executable: python(),
            arguments: vec!["-u".into(), script.display().to_string()],
            environment: HashMap::new(), path_maps: BTreeMap::new(), root: root.path().into(),
            request: "launch".into(), request_arguments: json!({}), breakpoints: vec![],
        }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if matches!(service.try_event(), Some(DapEvent::Stopped { .. })) { break; }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        // These controls were admitted before the stop advanced, unlike stopped-data queries.
        for (request_id, value) in [
            (100, DapControl::SetBreakpoints { path: root.path().join("main.rs"), breakpoints: vec![] }),
            (101, DapControl::Disconnect { terminate_debuggee: true }),
        ] {
            service.session.as_ref().unwrap().sender.try_send(Control {
                request_id, value, generation: 0,
            }).unwrap();
        }
        let mut breakpoint_answer = false;
        let mut disconnected = false;
        while !breakpoint_answer || !disconnected {
            match service.try_event() {
                Some(DapEvent::Response { request_id: 100, response: DapResponse::Breakpoints(_), .. }) => breakpoint_answer = true,
                Some(DapEvent::Response { request_id: 101, response: DapResponse::Disconnected { .. }, .. }) => disconnected = true,
                Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
                _ => {}
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        while !service.session.as_ref().unwrap().actor.done.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(service.try_event().is_none());
        service.stop().unwrap();
    }

    #[test]
    fn queued_breakpoint_answers_survive_new_stop_generations() {
        let (mut service, adapter) = fixture();
        let path = std::path::PathBuf::from("/source");
        adapter.event(
            0,
            DapEvent::Response {
                adapter: "same-name".into(),
                stop_generation: 0,
                request_id: 4,
                request: Some(DapControl::SetBreakpoints {
                    path: path.clone(),
                    breakpoints: vec![DapSourceBreakpoint {
                        path,
                        line: 20,
                        column: None,
                        condition: None,
                        hit_condition: None,
                        log_message: None,
                    }],
                }),
                response: DapResponse::Breakpoints(vec![(true, Some(20), None)]),
            },
        );
        adapter.advance(1);
        assert!(matches!(
            service.try_event(),
            Some(DapEvent::Response { request_id: 4, .. })
        ));
    }
}
