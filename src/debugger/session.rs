//! Bounded adapter reservation, original actor contexts and worker-owned pipe I/O.
use crate::debugger::DapControl;
use crate::debugger::DapError;
use crate::debugger::DapEvent;
use crate::debugger::DapLaunch;
use crate::debugger::run_adapter;
use crate::process::write_content_length_message;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

const CAPACITY: usize = 32;
const RESIDENT: usize = 8;

#[derive(Debug)]
pub(super) struct Actor {
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) supports_terminate: AtomicBool,
    launch: bool,
    next_request: AtomicU64,
    closed: AtomicBool,
    done: AtomicBool,
    pub(super) stop_id: AtomicU64,
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
    stop_id: u64,
}
pub(super) struct Control {
    pub(super) value: DapControl,
    pub(super) request_id: u64,
    pub(super) stop_id: u64,
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
            stop_id: self.actor.stop_id.load(Ordering::Acquire),
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
pub struct DapClient {
    session: Option<Session>,
    residents: Vec<Weak<()>>,
    closed: bool,
}
impl DapClient {
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
            stop_id: session.actor.stop_id.load(Ordering::Acquire),
        })
    }
    pub(crate) fn current(&self, source: &DapContext) -> bool {
        self.session.as_ref().is_some_and(|s| {
            Arc::ptr_eq(&s.actor, &source.actor)
                && !s.actor.closed.load(Ordering::Acquire)
                && !s.actor.cancel.load(Ordering::Acquire)
                && !s.actor.done.load(Ordering::Acquire)
                && s.actor.live.load(Ordering::Acquire)
                && source.stop_id == s.actor.stop_id.load(Ordering::Acquire)
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
        self.session.is_none() && self.restart_available()
    }
    pub(crate) fn restart_available(&self) -> bool {
        !self.closed && self.pending() < RESIDENT
    }
    pub(crate) fn reservation(&mut self) -> Result<DapReservation<'_>, DapError> {
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
        Ok(DapReservation {
            service: self,
            resident,
        })
    }
    pub fn start_absolute(&mut self, launch: DapLaunch) -> Result<(), DapError> {
        self.reservation()?.submit(launch, None)
    }
    fn submit(
        &mut self,
        launch: DapLaunch,
        scope: Option<ExecutionScope>,
        resident: Arc<()>,
    ) -> Result<(), DapError> {
        if launch.adapter.contains('\0')
            || !launch.executable.is_absolute()
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
            closed: AtomicBool::new(false),
            done: AtomicBool::new(false),
            stop_id: AtomicU64::new(0),
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
            return Err(DapError::StaleContext);
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
                stop_id: source.stop_id,
            })
            .map(|()| request_id)
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => DapError::Full,
                mpsc::TrySendError::Disconnected(_) => DapError::NotRunning,
            })
    }
    pub fn stop(&mut self) -> Result<(), DapError> {
        let session = self.session.take().ok_or(DapError::NotRunning)?;
        session.actor.closed.store(true, Ordering::Release);
        session.actor.live.store(false, Ordering::Release);
        session.actor.cancel.store(true, Ordering::Release);
        // Dropping the receiver discards this actor's events, never the replacement.
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
            let done = result.is_err() && session.actor.done.load(Ordering::Acquire);
            let result = if done {
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
                            response: crate::debugger::DapResponse::Breakpoints(_)
                                | crate::debugger::DapResponse::Disconnected { .. }
                                | crate::debugger::DapResponse::BreakpointChanged { .. },
                            ..
                        }
                    ) || matches!(&event, DapEvent::RequestFailed { command, .. } if command == "setBreakpoints" || command == "disconnect");
                    if session_scoped {
                        source.stop_id = session.actor.stop_id.load(Ordering::Acquire);
                    }
                    if matches!(
                        event,
                        DapEvent::Response { .. }
                            | DapEvent::RequestFailed { .. }
                            | DapEvent::Stopped { .. }
                    ) && !session_scoped
                        && source.stop_id != session.actor.stop_id.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    if let DapEvent::Failed { message, .. } = &mut event {
                        append_stderr(message, &session.actor);
                    }
                    if matches!(
                        event,
                        DapEvent::Terminated { .. }
                            | DapEvent::Failed { .. }
                            | DapEvent::Response {
                                response: crate::debugger::DapResponse::Disconnected { .. },
                                ..
                            }
                    ) {
                        session.reported = true;
                    }
                    return Some((source, event));
                }
                Err(_) if done && !session.reported => {
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
                            stop_id: session.actor.stop_id.load(Ordering::Acquire),
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
        let detail = crate::debugger::sanitize_output(&String::from_utf8_lossy(&stderr));
        if !detail.trim().is_empty() {
            message.push_str("\nadapter stderr: ");
            message.push_str(detail.trim());
        }
    }
}

impl Default for DapClient {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for DapClient {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

pub(super) struct Writer {
    sender: SyncSender<(Value, Option<Arc<AtomicU8>>)>,
    pending: VecDeque<Value>,
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
                        Ok((value, delivery)) => {
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
                                if let Some(delivery) = &delivery {
                                    delivery.store(
                                        crate::debugger::DapReplyOutcome::Failed as u8,
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
                            if let Some(delivery) = delivery {
                                delivery.store(
                                    crate::debugger::DapReplyOutcome::Written as u8,
                                    Ordering::Release,
                                );
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok((
            Self {
                sender,
                pending: VecDeque::new(),
                actor,
            },
            worker,
        ))
    }
    pub(super) fn reply(&mut self, value: Value, delivery: Arc<AtomicU8>) -> Result<(), Value> {
        if self.flush().is_err() {
            delivery.store(
                crate::debugger::DapReplyOutcome::Unknown as u8,
                Ordering::Release,
            );
            return Err(value);
        }
        // Replies cannot overtake requests that already have wire sequence IDs.
        if !self.pending.is_empty() {
            #[cfg(test)]
            self.actor.reverse_retries.fetch_add(1, Ordering::AcqRel);
            return Err(value);
        }
        #[cfg(test)]
        self.actor.writes_queued.fetch_add(1, Ordering::AcqRel);
        match self.sender.try_send((value, Some(delivery.clone()))) {
            Ok(()) => {
                // Publish reservation only after enqueue; do not overwrite a fast
                // writer's already-completed delivery.
                let _ = delivery.compare_exchange(
                    crate::debugger::DapReplyOutcome::Ready as u8,
                    crate::debugger::DapReplyOutcome::WriterQueued as u8,
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
                        (value, crate::debugger::DapReplyOutcome::Ready)
                    }
                    mpsc::TrySendError::Disconnected((value, _)) => {
                        self.actor.live.store(false, Ordering::Release);
                        self.actor.cancel.store(true, Ordering::Release);
                        (value, crate::debugger::DapReplyOutcome::Unknown)
                    }
                };
                delivery.store(state as u8, Ordering::Release);
                Err(value)
            }
        }
    }
    pub(super) fn submit(&mut self, value: Value) -> Result<(), ()> {
        self.pending.push_back(value);
        self.flush()
    }
    pub(super) fn flush(&mut self) -> Result<(), ()> {
        while let Some(value) = self.pending.pop_front() {
            #[cfg(test)]
            self.actor.writes_queued.fetch_add(1, Ordering::AcqRel);
            if let Err(error) = self.sender.try_send((value, None)) {
                #[cfg(test)]
                self.actor.writes_queued.fetch_sub(1, Ordering::AcqRel);
                match error {
                    mpsc::TrySendError::Full((value, _)) => {
                        self.pending.push_front(value);
                        break;
                    }
                    mpsc::TrySendError::Disconnected(_) => {
                        if let Ok(mut failure) = self.actor.failure.lock() {
                            *failure =
                                Some("DAP writer stopped; accepted write outcomes unknown".into());
                        }
                        self.actor.live.store(false, Ordering::Release);
                        self.actor.cancel.store(true, Ordering::Release);
                        return Err(());
                    }
                }
            }
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
    pub(crate) fn terminal_reply(&self) -> (impl Drop, crate::debugger::DapTerminalReply) {
        crate::debugger::reverse::ReverseRequest::reserve(1, self.events.actor.live.clone())
    }
    pub(crate) fn event(&self, stop_id: u64, event: DapEvent) {
        self.events.actor.stop_id.store(stop_id, Ordering::Release);
        self.events.send(event).unwrap();
    }
    pub(crate) fn advance(&self, stop_id: u64) {
        self.events.actor.stop_id.store(stop_id, Ordering::Release);
    }
    pub(crate) fn control(&self) -> Option<DapControl> {
        self.controls.try_recv().ok().map(|c| c.value)
    }
    pub(crate) fn with_terminal_reply(&self, test: impl FnOnce(crate::debugger::DapTerminalReply)) {
        let (_request, reply) =
            crate::debugger::reverse::ReverseRequest::reserve(1, self.events.actor.live.clone());
        test(reply);
    }
}
#[cfg(test)]
pub(crate) fn fixture() -> (DapClient, TestAdapter) {
    let actor = Arc::new(Actor {
        cancel: Arc::new(AtomicBool::new(false)),
        supports_terminate: AtomicBool::new(true),
        launch: true,
        next_request: AtomicU64::new(1),
        closed: AtomicBool::new(false),
        done: AtomicBool::new(false),
        stop_id: AtomicU64::new(0),
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
        DapClient {
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
pub(crate) struct DapReservation<'a> {
    service: &'a mut DapClient,
    resident: Arc<()>,
}
impl DapReservation<'_> {
    pub(crate) fn submit(
        self,
        launch: DapLaunch,
        scope: Option<ExecutionScope>,
    ) -> Result<(), DapError> {
        self.service.submit(launch, scope, self.resident)
    }
}
pub(crate) struct ExecutionScope {
    pub(crate) roots: Vec<crate::files::RootIdentity>,
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
    use crate::debugger::DapResponse;
    use crate::debugger::DapSourceBreakpoint;
    use crate::debugger::session::*;
    #[test]
    fn restart_requires_a_free_resident_slot_before_stopping() {
        let (mut service, _adapter) = fixture();
        let residents: Vec<_> = (0..RESIDENT).map(|_| Arc::new(())).collect();
        service.residents = residents.iter().map(Arc::downgrade).collect();
        assert!(service.is_running());
        assert!(!service.restart_available());
        assert!(!service.available());
        drop(residents);
        assert!(service.restart_available());
        assert!(!service.available());
        service.begin_shutdown();
        assert!(!service.restart_available());
    }
    #[test]
    fn unsupported_detach_is_refused_before_enqueue() {
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
    fn queued_session_controls_survive_new_stops() {
        use crate::debugger::transport_tests::ADAPTER;
        use crate::debugger::transport_tests::python;
        use serde_json::json;
        use std::collections::BTreeMap;
        use std::collections::HashMap;
        use std::time::Instant;
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
        let mut service = DapClient::new();
        service
            .start_absolute(DapLaunch {
                adapter: "queued".into(),
                executable: python(),
                arguments: vec!["-u".into(), script.display().to_string()],
                environment: HashMap::new(),
                path_maps: BTreeMap::new(),
                root: root.path().into(),
                request: "launch".into(),
                request_arguments: json!({}),
                breakpoints: vec![],
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if matches!(service.try_event(), Some(DapEvent::Stopped { .. })) {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        // These controls were accepted before the stop advanced, unlike stopped-data queries.
        for (request_id, value) in [
            (
                100,
                DapControl::SetBreakpoints {
                    path: root.path().join("main.rs"),
                    breakpoints: vec![],
                },
            ),
            (
                101,
                DapControl::Disconnect {
                    terminate_debuggee: true,
                },
            ),
        ] {
            service
                .session
                .as_ref()
                .unwrap()
                .sender
                .try_send(Control {
                    request_id,
                    value,
                    stop_id: 0,
                })
                .unwrap();
        }
        let mut breakpoint_answer = false;
        let mut disconnected = false;
        while !breakpoint_answer || !disconnected {
            match service.try_event() {
                Some(DapEvent::Response {
                    request_id: 100,
                    response: DapResponse::Breakpoints(_),
                    ..
                }) => breakpoint_answer = true,
                Some(DapEvent::Response {
                    request_id: 101,
                    response: DapResponse::Disconnected { .. },
                    ..
                }) => disconnected = true,
                Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
                _ => {}
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        while !service
            .session
            .as_ref()
            .unwrap()
            .actor
            .done
            .load(Ordering::Acquire)
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(service.try_event().is_none());
        service.stop().unwrap();
    }

    #[test]
    fn successful_disconnect_is_a_terminal_outcome() {
        for completed_before_poll in [false, true] {
            let (mut service, adapter) = fixture();
            adapter.event(
                0,
                DapEvent::Response {
                    adapter: "same-name".into(),
                    stop_id: 0,
                    request_id: 1,
                    request: Some(DapControl::Disconnect {
                        terminate_debuggee: false,
                    }),
                    response: DapResponse::Disconnected {
                        terminate_debuggee: false,
                    },
                },
            );
            adapter.advance(1);
            adapter
                .events
                .actor
                .done
                .store(completed_before_poll, Ordering::Release);
            assert!(matches!(
                service.try_event(),
                Some(DapEvent::Response {
                    response: DapResponse::Disconnected {
                        terminate_debuggee: false
                    },
                    ..
                })
            ));
            adapter.events.actor.done.store(true, Ordering::Release);
            assert!(service.try_event().is_none());
            assert!(service.try_event().is_none());
        }
    }
    #[test]
    fn completion_drains_terminal_events_before_reporting_failure() {
        for _ in 0..128 {
            let (mut service, adapter) = fixture();
            assert!(service.try_event().is_none());
            thread::scope(|scope| {
                scope.spawn(move || {
                    adapter.event(
                        0,
                        DapEvent::Output {
                            adapter: "same-name".into(),
                            output: "done".into(),
                        },
                    );
                    adapter.event(
                        0,
                        DapEvent::Terminated {
                            adapter: "same-name".into(),
                        },
                    );
                    adapter.events.actor.done.store(true, Ordering::Release);
                });
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let mut output_seen = false;
                loop {
                    assert!(std::time::Instant::now() < deadline, "completion deadline");
                    match service.try_event() {
                        Some(DapEvent::Output { .. }) => output_seen = true,
                        Some(DapEvent::Terminated { .. }) => {
                            assert!(output_seen);
                            break;
                        }
                        Some(event) => panic!("unexpected completion: {event:?}"),
                        None => thread::yield_now(),
                    }
                }
            });
            assert!(service.try_event().is_none());
        }
    }
    #[test]
    fn completion_without_a_terminal_event_reports_failure_once() {
        let (mut service, adapter) = fixture();
        adapter.events.actor.done.store(true, Ordering::Release);
        assert!(matches!(service.try_event(), Some(DapEvent::Failed { .. })));
        assert!(service.try_event().is_none());
    }
    #[test]
    fn writer_retries_full_queue_in_wire_order() {
        let (service, _adapter) = fixture();
        let actor = service.session.as_ref().unwrap().actor.clone();
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        let mut writer = Writer {
            sender,
            pending: VecDeque::new(),
            actor: actor.clone(),
        };
        for sequence in 0..CAPACITY * 3 {
            writer.submit(serde_json::json!({"seq": sequence})).unwrap();
        }
        assert_eq!(writer.pending.len(), CAPACITY * 2);
        assert!(!actor.cancel.load(Ordering::Acquire));
        let delivery = Arc::new(AtomicU8::new(crate::debugger::DapReplyOutcome::Ready as u8));
        let reply = serde_json::json!({"seq": CAPACITY * 3});
        assert_eq!(
            writer.reply(reply.clone(), delivery.clone()),
            Err(reply.clone())
        );
        for sequence in 0..CAPACITY * 3 {
            let (value, state) = receiver.try_recv().unwrap();
            actor.writes_queued.fetch_sub(1, Ordering::AcqRel);
            assert_eq!(value["seq"], sequence);
            assert!(state.is_none());
            writer.flush().unwrap();
        }
        assert!(writer.pending.is_empty());
        writer.reply(reply.clone(), delivery).unwrap();
        assert_eq!(receiver.try_recv().unwrap().0, reply);
        assert!(!actor.cancel.load(Ordering::Acquire));
        drop(receiver);
        assert!(writer.submit(Value::Null).is_err());
        assert!(actor.cancel.load(Ordering::Acquire));
    }
    #[test]
    fn session_scoped_failures_survive_but_execution_responses_are_filtered() {
        let (mut service, adapter) = fixture();
        for command in ["stackTrace", "setBreakpoints", "disconnect"] {
            adapter.event(
                0,
                DapEvent::RequestFailed {
                    adapter: "same-name".into(),
                    stop_id: 0,
                    request_seq: 1,
                    request_id: 1,
                    request: None,
                    command: command.into(),
                    message: "rejected".into(),
                },
            );
        }
        adapter.advance(1);
        for expected in ["setBreakpoints", "disconnect"] {
            let (context, event) = service.try_owned_event().unwrap();
            assert_eq!(context.stop_id, 1);
            assert!(
                matches!(event, DapEvent::RequestFailed { command, .. } if command == expected)
            );
        }
        assert!(service.try_event().is_none());
    }
    #[test]
    fn queued_breakpoint_answers_survive_new_stops() {
        let (mut service, adapter) = fixture();
        let path = std::path::PathBuf::from("/source");
        adapter.event(
            0,
            DapEvent::Response {
                adapter: "same-name".into(),
                stop_id: 0,
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
