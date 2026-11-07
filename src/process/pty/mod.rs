//! PTY sessions, capacity reservations and worker-owned I/O.
use crate::process::PtyError;
use crate::process::PtyEvent;
use crate::process::PtyId;
use crate::process::PtyRequest;
use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

struct Slot(Arc<AtomicUsize>);
impl Slot {
    fn reserve(count: &Arc<AtomicUsize>, maximum: usize) -> Result<Self, PtyError> {
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                if n < maximum { n.checked_add(1) } else { None }
            })
            .map_err(|_| PtyError::QueueFull)?;
        Ok(Self(count.clone()))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Input {
    bytes: Vec<u8>,
    _slot: Slot,
}
struct Handle {
    input: SyncSender<Input>,
    resize: SyncSender<(u64, u16, u16)>,
    cancel: Arc<AtomicBool>,
    queued: Arc<AtomicUsize>,
    maximum: usize,
    capacity: usize,
    finished: Arc<AtomicBool>,
}
pub struct PtyWorker {
    next: Option<u64>,
    capacity: usize,
    residents: Arc<AtomicUsize>,
    sessions: HashMap<PtyId, Handle>,
    sender: SyncSender<PtyEvent>,
    receiver: Receiver<PtyEvent>,
    closed: bool,
}
pub(crate) struct PtyReservation<'a> {
    service: &'a mut PtyWorker,
    slot: Slot,
}
pub(crate) struct PtyInputReservation<'a> {
    handle: &'a Handle,
    slot: Slot,
}
impl PtyInputReservation<'_> {
    pub(crate) fn submit(self, bytes: Vec<u8>) -> Result<(), PtyError> {
        if self.handle.cancel.load(Ordering::Acquire)
            || self.handle.finished.load(Ordering::Acquire)
        {
            return Err(PtyError::Stopped);
        }
        if bytes.len() > self.handle.maximum {
            return Err(PtyError::InvalidPolicy);
        }
        self.handle
            .input
            .try_send(Input {
                bytes,
                _slot: self.slot,
            })
            .map_err(queue)
    }
}
impl PtyReservation<'_> {
    pub(crate) fn submit(
        self,
        request: PtyRequest,
        root: Option<crate::files::RootIdentity>,
    ) -> Result<PtyId, PtyError> {
        validate(&request)?;
        let id = PtyId(self.service.next.ok_or(PtyError::Exhausted)?);
        self.service.next = id.0.checked_add(1);
        let capacity = request.policy.event_capacity.clamp(1, 256);
        let (input, input_rx) = mpsc::sync_channel(capacity);
        let (resize, resize_rx) = mpsc::sync_channel(capacity);
        let cancel = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let handle = Handle {
            input,
            resize,
            cancel: cancel.clone(),
            queued: Arc::new(AtomicUsize::new(0)),
            maximum: request.policy.maximum_input_bytes,
            capacity,
            finished: finished.clone(),
        };
        let events = self.service.sender.clone();
        // The lease lives until controller, reader AND writer have released it.
        let resident = Arc::new(self.slot);
        thread::Builder::new()
            .name(format!("dust-pty-{}", id.0))
            .spawn(move || {
                struct Finished {
                    finished: Arc<AtomicBool>,
                    cancel: Arc<AtomicBool>,
                }
                impl Drop for Finished {
                    fn drop(&mut self) {
                        self.cancel.store(true, Ordering::Release);
                        self.finished.store(true, Ordering::Release);
                    }
                }
                let _finished = Finished {
                    finished,
                    cancel: cancel.clone(),
                };
                if let Err(error) = run(
                    id,
                    request,
                    root,
                    &events,
                    cancel,
                    (input_rx, resize_rx),
                    resident,
                ) {
                    let _ = events.send(PtyEvent::Failed {
                        id,
                        message: error.to_string(),
                    });
                }
            })
            .map_err(|error| PtyError::Spawn(error.to_string()))?;
        self.service.sessions.insert(id, handle);
        Ok(id)
    }
}
impl PtyWorker {
    pub fn start(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, 256);
        let (sender, receiver) = mpsc::sync_channel(capacity);
        Self {
            next: Some(1),
            capacity,
            residents: Arc::new(AtomicUsize::new(0)),
            sessions: HashMap::new(),
            sender,
            receiver,
            closed: false,
        }
    }
    pub(crate) fn check_open(&self) -> Result<(), PtyError> {
        if self.closed {
            return Err(PtyError::Stopped);
        }
        if self.next.is_none() {
            return Err(PtyError::Exhausted);
        }
        if self.sessions.len() >= self.capacity
            || self.residents.load(Ordering::Acquire) >= self.capacity
        {
            return Err(PtyError::QueueFull);
        }
        Ok(())
    }
    pub(crate) fn reserve(&mut self) -> Result<PtyReservation<'_>, PtyError> {
        self.check_open()?;
        let slot = Slot::reserve(&self.residents, self.capacity)?;
        Ok(PtyReservation {
            service: self,
            slot,
        })
    }
    /// Queues the spawn; success does not mean the process has started.
    pub fn open(&mut self, request: PtyRequest) -> Result<PtyId, PtyError> {
        self.reserve()?.submit(request, None)
    }
    pub(crate) fn reserve_input(&self, id: PtyId) -> Result<PtyInputReservation<'_>, PtyError> {
        if self.closed {
            return Err(PtyError::Stopped);
        }
        let handle = self.sessions.get(&id).ok_or(PtyError::Stopped)?;
        if handle.cancel.load(Ordering::Acquire) || handle.finished.load(Ordering::Acquire) {
            return Err(PtyError::Stopped);
        }
        let slot = Slot::reserve(&handle.queued, handle.capacity)?;
        Ok(PtyInputReservation { handle, slot })
    }
    pub fn input(&self, id: PtyId, bytes: Vec<u8>, limit: usize) -> Result<(), PtyError> {
        let reservation = self.reserve_input(id)?;
        if bytes.len() > limit {
            return Err(PtyError::InvalidPolicy);
        }
        reservation.submit(bytes)
    }
    pub fn resize(&self, id: PtyId, columns: u16, rows: u16) -> Result<(), PtyError> {
        self.resize_checked(id, 0, columns, rows)
    }
    pub(crate) fn resize_checked(
        &self,
        id: PtyId,
        sequence: u64,
        columns: u16,
        rows: u16,
    ) -> Result<(), PtyError> {
        if self.closed || columns == 0 || rows == 0 {
            return Err(PtyError::InvalidPolicy);
        }
        let handle = self.sessions.get(&id).ok_or(PtyError::Stopped)?;
        if handle.cancel.load(Ordering::Acquire) || handle.finished.load(Ordering::Acquire) {
            return Err(PtyError::Stopped);
        }
        handle
            .resize
            .try_send((sequence, columns, rows))
            .map_err(queue)
    }
    /// Out-of-band cancellation cannot be lost behind a full command queue.
    pub fn close(&self, id: PtyId) -> Result<(), PtyError> {
        self.sessions
            .get(&id)
            .ok_or(PtyError::Stopped)?
            .cancel
            .store(true, Ordering::Release);
        Ok(())
    }
    pub fn begin_shutdown(&mut self) {
        self.closed = true;
        for handle in self.sessions.values() {
            handle.cancel.store(true, Ordering::Release);
        }
    }
    pub fn pending(&self) -> usize {
        self.sessions.len()
    }
    pub fn poll(&mut self) -> Result<Vec<PtyEvent>, PtyError> {
        Ok(self.drain_events())
    }
    pub(crate) fn drain_events(&mut self) -> Vec<PtyEvent> {
        let mut events = Vec::new();
        for _ in 0..256 {
            let Ok(event) = self.receiver.try_recv() else {
                break;
            };
            if let PtyEvent::IoComplete { id, .. }
            | PtyEvent::Failed { id, .. }
            | PtyEvent::Unknown { id, .. } = &event
            {
                self.sessions.remove(id);
            }
            events.push(event);
        }
        // A panic/startup failure must not leave an immortal logical session.
        // Finished is published after all controller sends; queued terminals win.
        if events.is_empty() {
            let ended = self
                .sessions
                .iter()
                .filter_map(|(id, h)| h.finished.load(Ordering::Acquire).then_some(*id))
                .collect::<Vec<_>>();
            // Acquire the finished flags BEFORE the second drain. A terminal
            // may have arrived between the first empty receive and that load.
            for _ in 0..256 {
                let Ok(event) = self.receiver.try_recv() else {
                    break;
                };
                if let PtyEvent::IoComplete { id, .. }
                | PtyEvent::Failed { id, .. }
                | PtyEvent::Unknown { id, .. } = &event
                {
                    self.sessions.remove(id);
                }
                events.push(event);
            }
            if !events.is_empty() {
                return events;
            }
            for id in ended {
                self.sessions.remove(&id);
                events.push(PtyEvent::Unknown {
                    id,
                    message: "PTY worker stopped without a terminal result".into(),
                });
            }
        }
        events
    }
}
impl Drop for PtyWorker {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}
fn queue<T>(error: mpsc::TrySendError<T>) -> PtyError {
    match error {
        mpsc::TrySendError::Full(_) => PtyError::QueueFull,
        mpsc::TrySendError::Disconnected(_) => PtyError::Stopped,
    }
}
fn validate(request: &PtyRequest) -> Result<(), PtyError> {
    let argv = request
        .arguments
        .iter()
        .fold(0usize, |n, s| n.saturating_add(s.len()));
    let env = request.environment.iter().fold(0usize, |n, (k, v)| {
        n.saturating_add(k.len()).saturating_add(v.len())
    });
    if !request.program.is_absolute()
        || !request.cwd.is_absolute()
        || request.columns == 0
        || request.rows == 0
        || request.arguments.len() > 4096
        || argv > 1024 * 1024
        || env > 1024 * 1024
        || request.policy.maximum_output_bytes > 256 * 1024 * 1024
        || request.policy.maximum_input_bytes == 0
        || request.policy.maximum_input_bytes > 16 * 1024 * 1024
        || request.policy.event_capacity == 0
    {
        return Err(PtyError::InvalidPolicy);
    }
    Ok(())
}
struct Child {
    process: Box<dyn portable_pty::Child + Send + Sync>,
    reaped: bool,
}
impl Child {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        let status = self.process.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }
    fn kill(&mut self) {
        if !self.reaped {
            #[cfg(unix)]
            if let Some(group) = self.process.process_id() {
                crate::process::terminate_process_group(group);
            }
            let _ = self.process.kill();
        }
    }
    fn terminate(&mut self, master: &dyn portable_pty::MasterPty) {
        if self.reaped {
            return;
        }
        #[cfg(unix)]
        {
            if let Some(group) = master.process_group_leader().filter(|group| *group > 0) {
                crate::process::terminate_process_group(group as u32);
            }
            if let Some(group) = self.process.process_id() {
                crate::process::terminate_process_group(group);
            }
        }
        self.kill();
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill();
            let _ = self.process.wait();
        }
    }
}
fn deliver(
    mut event: PtyEvent,
    events: &SyncSender<PtyEvent>,
    cancel: &AtomicBool,
    child: &mut Child,
) -> bool {
    loop {
        match events.try_send(event) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(returned)) => event = returned,
        }
        if cancel.load(Ordering::Acquire) {
            child.kill();
        }
        thread::sleep(Duration::from_millis(2));
    }
}
fn open_working_directory(
    path: &std::path::Path,
    root: Option<&crate::files::RootIdentity>,
) -> Result<std::fs::File, PtyError> {
    use rustix::fs::OFlags;
    let fail = |error: String| PtyError::Spawn(error);
    let canonical = path
        .canonicalize()
        .map_err(|e| fail(format!("PTY working directory is unavailable: {e}")))?;
    if let Some(root) = root {
        let current = crate::files::RootIdentity::capture(&root.canonical_path)
            .map_err(|e| fail(e.to_string()))?;
        if root != &current
            || !path.starts_with(&root.canonical_path)
            || !canonical.starts_with(&root.canonical_path)
        {
            return Err(fail(
                "PTY execution root filesystem identity changed".into(),
            ));
        }
    }
    crate::files::open_no_symlinks(&canonical, OFlags::PATH | OFlags::DIRECTORY)
        .map_err(|e| fail(format!("PTY working directory is unavailable: {e}")))
}

fn run(
    id: PtyId,
    request: PtyRequest,
    root: Option<crate::files::RootIdentity>,
    events: &SyncSender<PtyEvent>,
    cancel: Arc<AtomicBool>,
    controls: (Receiver<Input>, Receiver<(u64, u16, u16)>),
    resident: Arc<Slot>,
) -> Result<(), PtyError> {
    let (input, resize) = controls;
    use portable_pty::CommandBuilder;
    use portable_pty::PtySize;
    use portable_pty::native_pty_system;
    let fail = |error: String| PtyError::Spawn(error);
    if cancel.load(Ordering::Acquire) {
        return Err(fail("PTY cancelled before spawn".into()));
    }
    let cwd = open_working_directory(&request.cwd, root.as_ref())?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: request.rows,
            cols: request.columns,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| fail(e.to_string()))?;
    let program = request.argv0.as_ref().unwrap_or(&request.program);
    let program = if program.is_absolute() {
        program.clone()
    } else {
        std::env::current_dir()
            .map_err(|error| fail(error.to_string()))?
            .join(program)
    };
    if program
        .canonicalize()
        .map_err(|error| fail(error.to_string()))?
        != request
            .program
            .canonicalize()
            .map_err(|error| fail(error.to_string()))?
    {
        return Err(fail("PTY executable target changed".into()));
    }
    let mut command = CommandBuilder::new(program);
    command.args(&request.arguments);
    // portable-pty substitutes HOME for a missing cwd. This descriptor-backed
    // path stays valid through spawn, even if the original name is removed.
    use std::os::fd::AsRawFd;
    command.cwd(format!("/proc/self/fd/{}", cwd.as_raw_fd()));
    for (key, value) in &request.environment {
        command.env(key, value);
    }
    if cancel.load(Ordering::Acquire) {
        return Err(fail("PTY cancelled before spawn".into()));
    }
    let mut child = Child {
        process: pair
            .slave
            .spawn_command(command)
            .map_err(|e| fail(e.to_string()))?,
        reaped: false,
    };
    drop(cwd);
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| fail(e.to_string()))?;
    #[cfg(target_os = "linux")]
    let readiness = {
        use nix::sys::epoll::Epoll;
        use nix::sys::epoll::EpollCreateFlags;
        use nix::sys::epoll::EpollEvent;
        use nix::sys::epoll::EpollFlags;
        use nix::sys::epoll::EpollOp;
        use std::os::fd::AsRawFd;
        let readiness =
            Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC).map_err(|e| fail(e.to_string()))?;
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| fail("PTY master descriptor unavailable".into()))?;
        // portable-pty exposes a raw descriptor, not an AsFd implementation.
        // The master and its cloned reader outlive this registration.
        #[expect(deprecated)]
        nix::sys::epoll::epoll_ctl(
            readiness.0.as_raw_fd(),
            EpollOp::EpollCtlAdd,
            fd,
            &mut EpollEvent::new(EpollFlags::EPOLLIN, 0),
        )
        .map_err(|e| fail(e.to_string()))?;
        readiness
    };
    let mut writer = pair.master.take_writer().map_err(|e| fail(e.to_string()))?;
    if !deliver(
        PtyEvent::Started {
            id,
            root_revision: request.root_revision,
        },
        events,
        &cancel,
        &mut child,
    ) {
        return Ok(());
    }
    let (read_tx, read_rx) = mpsc::sync_channel(1);
    let output = events.clone();
    let output_order = Arc::new(Mutex::new(()));
    let read_order = output_order.clone();
    let read_resident = resident.clone();
    let read_cancel = cancel.clone();
    let maximum = request.policy.maximum_output_bytes;
    thread::Builder::new()
        .name(format!("dust-pty-read-{}", id.0))
        .spawn(move || {
            let _resident = read_resident;
            let result = (|| -> Result<(), String> {
                let mut total = 0usize;
                let mut bytes = [0u8; 8192];
                loop {
                    if read_cancel.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    #[cfg(target_os = "linux")]
                    match readiness.wait(&mut [nix::sys::epoll::EpollEvent::empty()], 10u16) {
                        Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
                        Ok(_) => {}
                        Err(error) => return Err(error.to_string()),
                    }
                    let read = match reader.read(&mut bytes) {
                        Ok(0) => return Ok(()),
                        Ok(n) => n,
                        // Linux PTY masters report EIO when the last slave closes.
                        #[cfg(target_os = "linux")]
                        Err(e) if e.raw_os_error() == Some(5) => return Ok(()),
                        Err(e) => return Err(e.to_string()),
                    };
                    if maximum != 0 && read > maximum.saturating_sub(total) {
                        return Err("PTY output limit exceeded; stream is incomplete".into());
                    }
                    total = total.saturating_add(read);
                    let _order = read_order
                        .lock()
                        .map_err(|_| "PTY output ordering failed")?;
                    let mut event = PtyEvent::Output {
                        id,
                        bytes: bytes[..read].to_vec(),
                        truncated: false,
                    };
                    loop {
                        match output.try_send(event) {
                            Ok(()) => break,
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                return Err("PTY output receiver stopped".into());
                            }
                            Err(mpsc::TrySendError::Full(returned)) => event = returned,
                        }
                        if read_cancel.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            })();
            let _ = read_tx.send(result);
        })
        .map_err(|e| fail(e.to_string()))?;
    let (write_tx, write_rx) = mpsc::sync_channel(1);
    let write_cancel = cancel.clone();
    let write_resident = resident.clone();
    let writer_stop = Arc::new(AtomicBool::new(false));
    let stop_writer = writer_stop.clone();
    thread::Builder::new()
        .name(format!("dust-pty-write-{}", id.0))
        .spawn(move || {
            let _resident = write_resident;
            let result = (|| -> Result<(), String> {
                while !write_cancel.load(Ordering::Acquire) && !stop_writer.load(Ordering::Acquire)
                {
                    match input.recv_timeout(Duration::from_millis(10)) {
                        Ok(packet) => {
                            let Input { bytes, _slot } = packet;
                            drop(_slot);
                            writer
                                .write_all(&bytes)
                                .and_then(|_| writer.flush())
                                .map_err(|e| e.to_string())?;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
                if input.try_recv().is_ok() {
                    return Err("PTY stopped with undelivered accepted input".into());
                }
                Ok(())
            })();
            let _ = write_tx.send(result);
        })
        .map_err(|e| fail(e.to_string()))?;
    let mut exit = None;
    let mut exit_delivered = false;
    let mut read_done = false;
    let mut write_done = false;
    let mut failure = None;
    let mut terminated = false;
    loop {
        if cancel.load(Ordering::Acquire) || failure.is_some() {
            cancel.store(true, Ordering::Release);
            if !terminated {
                child.terminate(pair.master.as_ref());
                terminated = true;
            }
            writer_stop.store(true, Ordering::Release);
        }
        for _ in 0..256 {
            let Ok((sequence, columns, rows)) = resize.try_recv() else {
                break;
            };
            let _order = output_order
                .lock()
                .map_err(|_| fail("PTY output ordering failed".into()))?;
            pair.master
                .resize(PtySize {
                    rows,
                    cols: columns,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| fail(e.to_string()))?;
            if !deliver(
                PtyEvent::Resized {
                    id,
                    sequence,
                    columns,
                    rows,
                },
                events,
                &cancel,
                &mut child,
            ) {
                return Ok(());
            }
        }
        if exit.is_none()
            && let Some(status) = child.try_wait().map_err(|e| fail(e.to_string()))?
        {
            exit = Some(status.exit_code());
            writer_stop.store(true, Ordering::Release);
        }
        for (rx, done) in [(&read_rx, &mut read_done), (&write_rx, &mut write_done)] {
            if !*done {
                match rx.try_recv() {
                    Ok(result) => {
                        *done = true;
                        if let Err(e) = result {
                            failure.get_or_insert(e);
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        *done = true;
                        failure.get_or_insert("PTY I/O worker stopped without completion".into());
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }
        if let Some(code) = exit {
            if !exit_delivered {
                if !deliver(
                    PtyEvent::Exited {
                        id,
                        code: Some(code),
                    },
                    events,
                    &cancel,
                    &mut child,
                ) {
                    return Ok(());
                }
                exit_delivered = true;
            }
            if read_done && write_done {
                deliver(
                    PtyEvent::IoComplete {
                        id,
                        result: failure.map_or(Ok(()), Err),
                    },
                    events,
                    &cancel,
                    &mut child,
                );
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests;
