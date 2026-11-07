//! Bounded child-process jobs, output and cancellation.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use thiserror::Error;
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JobId(pub u64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobStream {
    Stdout,
    Stderr,
}
#[derive(Clone, Debug)]
pub struct JobPolicy {
    pub maximum_output_bytes: usize,
    pub maximum_line_bytes: usize,
    pub maximum_stdin_bytes: usize,
    pub timeout: Duration,
}
impl Default for JobPolicy {
    fn default() -> Self {
        Self {
            maximum_output_bytes: 16 * 1024 * 1024,
            maximum_line_bytes: 1024 * 1024,
            maximum_stdin_bytes: 16 * 1024 * 1024,
            timeout: Duration::from_secs(300),
        }
    }
}
#[derive(Clone, Debug)]
pub struct JobRequest {
    pub program: PathBuf,
    pub argv0: Option<PathBuf>,
    pub arguments: Vec<std::ffi::OsString>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, std::ffi::OsString>,
    pub root_revision: u64,
    pub stdin: Option<Vec<u8>>,
    pub policy: JobPolicy,
}
#[derive(Clone, Debug)]
pub enum JobEvent {
    Started {
        job: JobId,
        root_revision: u64,
    },
    Output {
        job: JobId,
        stream: JobStream,
        bytes: Vec<u8>,
        truncated: bool,
    },
    Exited {
        job: JobId,
        code: Option<i32>,
        timed_out: bool,
    },
    Failed {
        job: JobId,
        message: String,
    },
}

struct JobSlot(Arc<AtomicUsize>);
impl Drop for JobSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Submission {
    root: Option<crate::files::RootIdentity>,
    id: JobId,
    request: JobRequest,
    cancel: Arc<AtomicBool>,
    slot: Arc<JobSlot>,
    input: Option<Receiver<Vec<u8>>>,
}
/// Reserves worker capacity. Filesystem access, spawning and process cleanup run on workers.
pub struct JobSupervisor {
    next: Option<u64>,
    jobs: HashMap<JobId, Arc<AtomicBool>>,
    inputs: HashMap<JobId, SyncSender<Vec<u8>>>,
    sender: SyncSender<Submission>,
    receiver: Option<Receiver<JobEvent>>,
    stop: Arc<AtomicBool>,
    residents: Arc<AtomicUsize>,
    worker: thread::JoinHandle<()>,
}
pub(crate) struct JobReservation<'a> {
    supervisor: &'a mut JobSupervisor,
    slot: Arc<JobSlot>,
}
impl JobSupervisor {
    pub fn new(event_capacity: usize) -> Self {
        Self::try_new(event_capacity).expect("job worker")
    }
    pub(crate) fn try_new(event_capacity: usize) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Submission>(256);
        let (events, receiver) = mpsc::sync_channel(event_capacity.clamp(1, 4096));
        let stop = Arc::new(AtomicBool::new(false));
        let closing = stop.clone();
        let worker = thread::Builder::new()
            .name("dust-jobs".into())
            .spawn(move || {
                let mut engine = JobEngine::new(event_capacity);
                while !closing.load(Ordering::Acquire) {
                    // Child processes are polled every 2 ms while any run; with none, the
                    // worker sleeps until a job arrives or the supervisor is dropped, so an
                    // idle editor stays idle.
                    let next = if engine.jobs.is_empty() {
                        rx.recv().map_err(|_| mpsc::TryRecvError::Disconnected)
                    } else {
                        rx.try_recv()
                    };
                    match next {
                        Ok(submission) => {
                            let id = submission.id;
                            let revision = submission.request.root_revision;
                            let event = if submission.cancel.load(Ordering::Acquire) {
                                JobEvent::Failed {
                                    job: id,
                                    message: "job cancelled before launch".into(),
                                }
                            } else {
                                match engine.start(submission) {
                                    Ok(()) => JobEvent::Started {
                                        job: id,
                                        root_revision: revision,
                                    },
                                    Err(error) => JobEvent::Failed {
                                        job: id,
                                        message: error.to_string(),
                                    },
                                }
                            };
                            if !deliver(&events, event, &closing, &mut engine) {
                                break;
                            }
                        }
                        Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                    for event in engine.poll() {
                        if !deliver(&events, event, &closing, &mut engine) {
                            return;
                        }
                    }
                    if !engine.jobs.is_empty() {
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            })?;
        Ok(Self {
            next: Some(1),
            jobs: HashMap::new(),
            inputs: HashMap::new(),
            sender: tx,
            receiver: Some(receiver),
            stop,
            residents: Arc::new(AtomicUsize::new(0)),
            worker,
        })
    }
    pub(crate) fn reserve(&mut self) -> Result<JobReservation<'_>, JobError> {
        if self.stop.load(Ordering::Acquire) || self.worker.is_finished() {
            return Err(JobError::Stopped);
        }
        if self.next.is_none() {
            return Err(JobError::Exhausted);
        }
        self.residents
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < 256).then_some(count + 1)
            })
            .map_err(|_| JobError::Full)?;
        let slot = Arc::new(JobSlot(self.residents.clone()));
        Ok(JobReservation {
            supervisor: self,
            slot,
        })
    }
    pub fn start(&mut self, request: JobRequest) -> Result<JobId, JobError> {
        self.reserve()?.submit(request)
    }
    /// Interactive jobs use raw output chunks and keep stdin open until explicitly closed.
    pub(crate) fn start_streaming(
        &mut self,
        request: JobRequest,
        root: crate::files::RootIdentity,
    ) -> Result<JobId, JobError> {
        let (sender, input) = mpsc::sync_channel(16);
        let id = self
            .reserve()?
            .submit_inner(request, Some(root), Some(input))?;
        self.inputs.insert(id, sender);
        Ok(id)
    }
    pub(crate) fn write_stdin(
        &mut self,
        id: JobId,
        bytes: Vec<u8>,
        close: bool,
    ) -> Result<(), JobError> {
        if bytes.len() > 1024 * 1024 {
            return Err(JobError::InvalidPolicy);
        }
        let sender = self.inputs.get(&id).ok_or(JobError::Unknown(id))?;
        if !bytes.is_empty() {
            sender.try_send(bytes).map_err(|error| match error {
                mpsc::TrySendError::Full(_) => JobError::Full,
                mpsc::TrySendError::Disconnected(_) => JobError::Unknown(id),
            })?;
        }
        if close {
            self.inputs.remove(&id);
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Vec<JobEvent> {
        let mut events = Vec::new();
        if let Some(receiver) = &self.receiver {
            for _ in 0..256 {
                let Ok(event) = receiver.try_recv() else {
                    break;
                };
                let id = event.id();
                if !self.jobs.contains_key(&id) {
                    continue;
                }
                if matches!(event, JobEvent::Failed { .. } | JobEvent::Exited { .. }) {
                    self.jobs.remove(&id);
                    self.inputs.remove(&id);
                }
                events.push(event);
            }
        }
        if self.worker.is_finished() && events.is_empty() {
            events.extend(self.jobs.drain().map(|(job, _)| JobEvent::Failed {
                job,
                message: "job worker stopped; process/output outcome may be incomplete".into(),
            }));
        }
        events
    }
    pub fn cancel(&mut self, id: JobId) -> Result<(), JobError> {
        self.jobs
            .get(&id)
            .ok_or(JobError::Unknown(id))?
            .store(true, Ordering::Release);
        Ok(())
    }
    pub fn is_running(&self, id: JobId) -> bool {
        self.jobs.contains_key(&id)
    }
    pub(crate) fn begin_shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        for cancel in self.jobs.values() {
            cancel.store(true, Ordering::Release);
        }
    }
    pub(crate) fn abandon(&mut self) {
        self.begin_shutdown();
        self.receiver.take();
        self.jobs.clear();
        self.inputs.clear();
    }
}
impl JobReservation<'_> {
    pub(crate) fn submit(self, request: JobRequest) -> Result<JobId, JobError> {
        self.submit_inner(request, None, None)
    }
    pub(crate) fn submit_scoped(
        self,
        request: JobRequest,
        root: crate::files::RootIdentity,
    ) -> Result<JobId, JobError> {
        if !request.cwd.starts_with(&root.canonical_path) {
            return Err(JobError::InvalidPolicy);
        }
        self.submit_inner(request, Some(root), None)
    }
    fn submit_inner(
        self,
        request: JobRequest,
        root: Option<crate::files::RootIdentity>,
        input: Option<Receiver<Vec<u8>>>,
    ) -> Result<JobId, JobError> {
        check_request(&request, input.is_some())?;
        let supervisor = self.supervisor;
        let next = supervisor.next.ok_or(JobError::Exhausted)?;
        supervisor.next = next.checked_add(1);
        let id = JobId(next);
        let cancel = Arc::new(AtomicBool::new(false));
        supervisor
            .sender
            .try_send(Submission {
                root,
                id,
                request,
                cancel: cancel.clone(),
                slot: self.slot,
                input,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => JobError::Full,
                mpsc::TrySendError::Disconnected(_) => JobError::Stopped,
            })?;
        supervisor.jobs.insert(id, cancel);
        Ok(id)
    }
}
impl Drop for JobSupervisor {
    fn drop(&mut self) {
        self.abandon();
    }
}
impl JobEvent {
    fn id(&self) -> JobId {
        match self {
            Self::Started { job, .. }
            | Self::Output { job, .. }
            | Self::Exited { job, .. }
            | Self::Failed { job, .. } => *job,
        }
    }
}
fn check_request(request: &JobRequest, streaming: bool) -> Result<(), JobError> {
    let argument_bytes = request
        .arguments
        .iter()
        .map(|argument| argument.len())
        .sum::<usize>();
    let environment_bytes = request
        .environment
        .iter()
        .map(|(key, value)| key.len().saturating_add(value.len()))
        .sum::<usize>();
    if (!streaming && !request.program.is_absolute())
        || request.program.as_os_str().is_empty()
        || !request.cwd.is_absolute()
        || request.arguments.len() > 4096
        || request.environment.len() > 4096
        || argument_bytes > 1024 * 1024
        || environment_bytes > 1024 * 1024
        || request.policy.maximum_output_bytes == 0
        || request.policy.maximum_output_bytes > 256 * 1024 * 1024
        || request.policy.maximum_line_bytes == 0
        || request.policy.maximum_line_bytes > 16 * 1024 * 1024
        || request.policy.maximum_stdin_bytes > 256 * 1024 * 1024
        || request
            .stdin
            .as_ref()
            .is_some_and(|bytes| bytes.len() > request.policy.maximum_stdin_bytes)
        || request.policy.timeout.is_zero()
        || request.policy.timeout > Duration::from_secs(86_400)
    {
        return Err(JobError::InvalidPolicy);
    }

    Ok(())
}
fn deliver(
    sender: &SyncSender<JobEvent>,
    mut event: JobEvent,
    stop: &AtomicBool,
    engine: &mut JobEngine,
) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(value)) => event = value,
        }
        engine.expire();
        thread::sleep(Duration::from_millis(2));
    }
}
struct RunningJob {
    child: Child,
    started: Instant,
    timeout: Duration,
    cancel: Arc<AtomicBool>,
    readers: Arc<AtomicUsize>,
    io_failed: Arc<AtomicBool>,
    status: Option<Option<i32>>,
    timed_out: bool,
    killed: bool,
    _slot: Arc<JobSlot>,
}
struct JobEngine {
    jobs: HashMap<JobId, RunningJob>,
    sender: SyncSender<JobEvent>,
    receiver: Receiver<JobEvent>,
    capacity: usize,
}
impl JobEngine {
    fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(capacity.clamp(1, 4096));
        Self {
            jobs: HashMap::new(),
            sender,
            receiver,
            capacity: capacity.clamp(1, 4096),
        }
    }
    fn start(&mut self, submission: Submission) -> Result<(), JobError> {
        let Submission {
            root,
            id,
            request,
            cancel,
            slot,
            input,
        } = submission;
        let streaming = input.is_some();
        let cwd = request
            .cwd
            .canonicalize()
            .map_err(|_| JobError::InvalidPolicy)?;
        if let Some(expected) = root
            && (crate::files::RootIdentity::capture(&expected.canonical_path)
                .ok()
                .as_ref()
                != Some(&expected)
                || !cwd.starts_with(&expected.canonical_path))
        {
            return Err(JobError::RootChanged);
        }
        if !request.cwd.is_dir() {
            return Err(JobError::InvalidPolicy);
        }
        if cancel.load(Ordering::Acquire) {
            return Err(JobError::Cancelled);
        }
        let mut command = Command::new(&request.program);
        command
            .args(&request.arguments)
            .current_dir(&cwd)
            .env_clear()
            .envs(&request.environment)
            .stdin(if request.stdin.is_some() || streaming {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
            if let Some(argv0) = &request.argv0 {
                command.arg0(argv0);
            }
        }
        let mut child = command.spawn().map_err(JobError::Spawn)?;
        // Configure all pipes before starting I/O threads, so setup failure still owns the child.
        let pipes = (|| -> io::Result<_> {
            Ok((
                child
                    .stdin
                    .take()
                    .map(|pipe| crate::process::CancellablePipe::new(pipe, cancel.clone()))
                    .transpose()?,
                child
                    .stdout
                    .take()
                    .map(|pipe| crate::process::CancellablePipe::new(pipe, cancel.clone()))
                    .transpose()?,
                child
                    .stderr
                    .take()
                    .map(|pipe| crate::process::CancellablePipe::new(pipe, cancel.clone()))
                    .transpose()?,
            ))
        })();
        let (stdin, stdout, stderr) = match pipes {
            Ok(pipes) => pipes,
            Err(error) => {
                terminate_tree(&mut child, false);
                let _ = child.wait();
                return Err(JobError::Spawn(error));
            }
        };
        let readers = Arc::new(AtomicUsize::new(0));
        let io_failed = Arc::new(AtomicBool::new(false));
        let setup_io = (|| -> io::Result<()> {
            if let Some(input) = input {
                let mut stdin = stdin.expect("streaming stdin is piped");
                let initial = request.stdin.unwrap_or_default();
                let cancelled = cancel.clone();
                let lease = slot.clone();
                thread::Builder::new()
                    .name("dust-job-stdin".into())
                    .spawn(move || {
                        let _lease = lease;
                        if stdin.write_all(&initial).is_err() {
                            return;
                        }
                        while !cancelled.load(Ordering::Acquire) {
                            match input.recv_timeout(Duration::from_millis(10)) {
                                Ok(bytes) => {
                                    if stdin.write_all(&bytes).is_err() {
                                        break;
                                    }
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    })?;
            } else if let Some(bytes) = request.stdin
                && let Some(mut stdin) = stdin
            {
                let lease = slot.clone();
                let pending = readers.clone();
                let failed = io_failed.clone();
                let cancelled = cancel.clone();
                pending.fetch_add(1, Ordering::AcqRel);
                thread::Builder::new()
                    .name("dust-job-stdin".into())
                    .spawn(move || {
                        let _lease = lease;
                        if stdin.write_all(&bytes).is_err() && !cancelled.load(Ordering::Acquire) {
                            failed.store(true, Ordering::Release);
                        }
                        pending.fetch_sub(1, Ordering::AcqRel);
                    })?;
            }
            let total = Arc::new(AtomicUsize::new(0));
            if let Some(stdout) = stdout {
                stream_reader(
                    id,
                    JobStream::Stdout,
                    stdout,
                    self.sender.clone(),
                    total.clone(),
                    (request.policy.clone(), streaming),
                    (
                        readers.clone(),
                        io_failed.clone(),
                        slot.clone(),
                        cancel.clone(),
                    ),
                )?;
            }
            if let Some(stderr) = stderr {
                stream_reader(
                    id,
                    JobStream::Stderr,
                    stderr,
                    self.sender.clone(),
                    total,
                    (request.policy.clone(), streaming),
                    (
                        readers.clone(),
                        io_failed.clone(),
                        slot.clone(),
                        cancel.clone(),
                    ),
                )?;
            }
            Ok(())
        })();
        if let Err(error) = setup_io {
            cancel.store(true, Ordering::Release);
            terminate_tree(&mut child, false);
            let _ = child.wait();
            return Err(JobError::Spawn(error));
        }
        self.jobs.insert(
            id,
            RunningJob {
                child,
                started: Instant::now(),
                timeout: request.policy.timeout,
                cancel,
                readers,
                io_failed,
                status: None,
                timed_out: false,
                killed: false,
                _slot: slot,
            },
        );
        Ok(())
    }

    fn expire(&mut self) {
        for running in self.jobs.values_mut() {
            if running.status.is_none()
                && let Ok(Some(status)) = job_exit_status(&mut running.child)
            {
                running.status = Some(status);
            }
            let expired = running.started.elapsed() >= running.timeout;
            running.timed_out |= running.status.is_none() && expired;
            // The leader may have exited while a descendant still holds its pipes open.
            if !running.killed && (expired || running.cancel.load(Ordering::Acquire)) {
                if running.timed_out {
                    running.cancel.store(true, Ordering::Release);
                }
                terminate_tree(&mut running.child, running.status.is_some());
                running.killed = true;
            }
        }
    }
    fn poll(&mut self) -> Vec<JobEvent> {
        self.expire();
        let mut events = Vec::new();
        for _ in 0..256 {
            match self.receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(_) => break,
            }
        }
        let mut done = Vec::new();
        let mut terminals = Vec::new();
        for (id, running) in &mut self.jobs {
            if running.status.is_none() {
                match job_exit_status(&mut running.child) {
                    Ok(Some(status)) => running.status = Some(status),
                    Ok(None) => {}
                    Err(error) => {
                        running.cancel.store(true, Ordering::Release);
                        terminate_tree(&mut running.child, false);
                        let _ = running.child.wait();
                        events.push(JobEvent::Failed {
                            job: *id,
                            message: error.to_string(),
                        });
                        done.push(*id);
                        continue;
                    }
                }
            }
            if let Some(code) = running.status
                && running.readers.load(Ordering::Acquire) == 0
            {
                running.cancel.store(true, Ordering::Release);
                // No later cleanup can signal this group once the leader is reaped.
                let _ = running.child.wait();
                terminals.push(if running.io_failed.load(Ordering::Acquire) {
                    JobEvent::Failed {
                        job: *id,
                        message: "job input/output transfer failed".into(),
                    }
                } else {
                    JobEvent::Exited {
                        job: *id,
                        code,
                        timed_out: running.timed_out,
                    }
                });
                done.push(*id);
            }
        }
        // Every selected terminal's readers had finished before this bounded drain.
        // At most capacity messages can precede their EOF in this FIFO.
        if !terminals.is_empty() {
            for _ in 0..self.capacity {
                match self.receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(_) => break,
                }
            }
        }
        events.extend(terminals);
        for id in done {
            self.jobs.remove(&id);
        }
        events
    }
}
impl Drop for JobEngine {
    fn drop(&mut self) {
        for job in self.jobs.values_mut() {
            job.cancel.store(true, Ordering::Release);
            if !job.killed {
                terminate_tree(&mut job.child, job.status.is_some());
            }
            let _ = job.child.wait();
        }
    }
}
fn stream_reader(
    id: JobId,
    stream: JobStream,
    reader: impl Read + Send + 'static,
    sender: SyncSender<JobEvent>,
    total: Arc<AtomicUsize>,
    (policy, streaming): (JobPolicy, bool),
    completion: (
        Arc<AtomicUsize>,
        Arc<AtomicBool>,
        Arc<JobSlot>,
        Arc<AtomicBool>,
    ),
) -> io::Result<()> {
    let (readers, failed, slot, cancel) = completion;
    readers.fetch_add(1, Ordering::AcqRel);
    thread::Builder::new()
        .name("dust-job-output".into())
        .spawn(move || {
            struct End(Arc<AtomicUsize>);
            impl Drop for End {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _end = End(readers);
            let _slot = slot;
            let mut reader = BufReader::new(reader);
            loop {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let mut bytes = Vec::new();
                let result = if streaming {
                    bytes.resize(8192.min(policy.maximum_line_bytes), 0);
                    reader
                        .read(&mut bytes)
                        .inspect(|read| bytes.truncate(*read))
                } else {
                    reader
                        .by_ref()
                        .take(policy.maximum_line_bytes.saturating_add(1) as u64)
                        .read_until(b'\n', &mut bytes)
                };
                let read = match result {
                    Ok(read) => read,
                    Err(_) => {
                        if !cancel.load(Ordering::Acquire) {
                            failed.store(true, Ordering::Release);
                        }
                        break;
                    }
                };
                if read == 0 {
                    break;
                }
                let line_truncated = bytes.len() > policy.maximum_line_bytes;
                let discard_tail = line_truncated && !bytes.ends_with(b"\n");
                bytes.truncate(policy.maximum_line_bytes);
                let allowed = reserve_output(&total, policy.maximum_output_bytes, bytes.len());
                bytes.truncate(allowed);
                let truncated = line_truncated || bytes.len() < read || allowed < read;
                if !bytes.is_empty() || truncated {
                    let mut event = JobEvent::Output {
                        job: id,
                        stream,
                        bytes,
                        truncated,
                    };
                    loop {
                        if cancel.load(Ordering::Acquire) {
                            return;
                        }
                        match sender.try_send(event) {
                            Ok(()) => break,
                            Err(mpsc::TrySendError::Disconnected(_)) => return,
                            Err(mpsc::TrySendError::Full(value)) => {
                                event = value;
                                thread::sleep(Duration::from_millis(2));
                            }
                        }
                    }
                }
                if truncated && allowed == 0 {
                    break;
                }
                if discard_tail {
                    // Discard the rest of this physical line without allocating for it.
                    loop {
                        match reader.fill_buf() {
                            Ok([]) => break,
                            Ok(tail) => {
                                let newline = tail.iter().position(|byte| *byte == b'\n');
                                let consumed = newline.map_or(tail.len(), |at| at + 1);
                                reader.consume(consumed);
                                if newline.is_some() || cancel.load(Ordering::Acquire) {
                                    break;
                                }
                            }
                            Err(_) => {
                                if !cancel.load(Ordering::Acquire) {
                                    failed.store(true, Ordering::Release);
                                }
                                return;
                            }
                        }
                    }
                }
            }
        })
        .map(|_| ())
}
fn reserve_output(total: &AtomicUsize, limit: usize, requested: usize) -> usize {
    let mut current = total.load(Ordering::Relaxed);
    loop {
        let allowed = requested.min(limit.saturating_sub(current));
        match total.compare_exchange_weak(
            current,
            current.saturating_add(allowed),
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return allowed,
            Err(actual) => current = actual,
        }
    }
}

fn job_exit_status(child: &mut Child) -> io::Result<Option<Option<i32>>> {
    #[cfg(unix)]
    {
        use nix::sys::wait::Id;
        use nix::sys::wait::WaitPidFlag;
        use nix::sys::wait::WaitStatus;
        use nix::sys::wait::waitid;
        // Keep the exited leader reserved while descendants can still hold pipes.
        match waitid(
            Id::Pid(nix::unistd::Pid::from_raw(child.id() as i32)),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
        )
        .map_err(io::Error::from)?
        {
            WaitStatus::Exited(_, code) => Ok(Some(Some(code))),
            WaitStatus::Signaled(..) => Ok(Some(None)),
            _ => Ok(None),
        }
    }
    #[cfg(not(unix))]
    {
        child
            .try_wait()
            .map(|status| status.map(|status| status.code()))
    }
}

fn terminate_tree(child: &mut Child, reaped: bool) {
    #[cfg(unix)]
    {
        crate::process::terminate_process_group(child.id());
    }
    if !reaped {
        let _ = child.kill();
    }
}
#[derive(Debug, Error)]
pub enum JobError {
    #[error("job policy or request is invalid")]
    InvalidPolicy,
    #[error("cannot spawn job: {0}")]
    Spawn(#[source] io::Error),
    #[error("unknown job {0:?}")]
    Unknown(JobId),
    #[error("job queue is full")]
    Full,
    #[error("job worker stopped")]
    Stopped,
    #[error("job identity exhausted")]
    Exhausted,
    #[error("job cancelled before spawn")]
    Cancelled,
    #[error("task execution root filesystem identity changed")]
    RootChanged,
}

#[cfg(all(test, unix))]
mod tests {
    use crate::process::jobs::*;
    fn request(root: &std::path::Path, command: &str) -> JobRequest {
        JobRequest {
            program: PathBuf::from("/bin/sh"),
            argv0: None,
            arguments: vec!["-c".into(), command.into()],
            cwd: root.into(),
            environment: BTreeMap::new(),
            root_revision: 1,
            stdin: None,
            policy: JobPolicy {
                timeout: Duration::from_secs(2),
                ..JobPolicy::default()
            },
        }
    }
    #[test]
    fn job_process_receives_native_arguments_and_environment() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        let native = std::ffi::OsString::from_vec(b"path-\xff".to_vec());
        let mut request = request(root.path(), "printf '%s' \"$1\"; printf '%s' \"$PATH\"");
        request.arguments.extend(["sh".into(), native.clone()]);
        request.environment.insert("PATH".into(), native);
        let mut jobs = JobSupervisor::new(16);
        let id = jobs.start(request).unwrap();
        let mut bytes = Vec::new();
        let mut exited = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !exited {
            for event in jobs.poll() {
                match event {
                    JobEvent::Output { bytes: output, .. } => bytes.extend(output),
                    JobEvent::Exited {
                        job,
                        code,
                        timed_out,
                    } => {
                        assert_eq!(job, id);
                        assert_eq!(code, Some(0));
                        assert!(!timed_out);
                        exited = true;
                    }
                    JobEvent::Failed { message, .. } => panic!("{message}"),
                    JobEvent::Started { .. } => {}
                }
            }
            assert!(
                Instant::now() < deadline,
                "native argument job did not finish"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(bytes, b"path-\xffpath-\xff");
    }

    #[test]
    fn job_capacity_is_bounded_and_output_drains_before_completion() {
        let root = tempfile::tempdir().unwrap();
        let mut jobs = JobSupervisor::new(1);
        let id = jobs
            .start(request(&root.path().join("missing"), "true"))
            .expect("foreground must not stat/spawn");
        let deadline = Instant::now() + Duration::from_secs(3);
        while jobs.is_running(id) && Instant::now() < deadline {
            for event in jobs.poll() {
                assert!(matches!(event, JobEvent::Failed { .. }));
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(!jobs.is_running(id));
        let id = jobs
            .start(request(
                root.path(),
                "i=0; while [ $i -lt 32 ]; do printf 'line\n'; i=$((i+1)); done",
            ))
            .unwrap();
        thread::sleep(Duration::from_millis(30));
        let mut bytes = Vec::new();
        let mut terminal = 0;
        let deadline = Instant::now() + Duration::from_secs(3);
        while jobs.is_running(id) && Instant::now() < deadline {
            for event in jobs.poll() {
                match event {
                    JobEvent::Output { bytes: value, .. } => {
                        assert_eq!(terminal, 0);
                        bytes.extend(value)
                    }
                    JobEvent::Exited { code, .. } => {
                        assert_eq!(code, Some(0));
                        terminal += 1;
                        assert_eq!(bytes.len(), 160);
                    }
                    JobEvent::Started { .. } => {}
                    event => panic!("{event:?}"),
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(terminal, 1);
        assert_eq!(bytes, b"line\n".repeat(32));
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let identity = crate::files::RootIdentity::capture(&target).unwrap();
        std::fs::rename(&target, root.path().join("old-target")).unwrap();
        std::fs::create_dir(&target).unwrap();
        let id = jobs
            .reserve()
            .unwrap()
            .submit_scoped(request(&target, "printf forbidden > executed"), identity)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut refused = false;
        while jobs.is_running(id) && Instant::now() < deadline {
            for event in jobs.poll() {
                if let JobEvent::Failed { message, .. } = event {
                    refused = message.contains("filesystem identity changed");
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(refused);
        assert!(!target.join("executed").exists());
        // Simulated exhausted resident capacity; no filesystem or worker action is needed for this refusal.
        jobs.residents.store(256, Ordering::Release);
        assert!(matches!(jobs.reserve(), Err(JobError::Full)));
        jobs.residents.store(0, Ordering::Release);
        let loan = jobs.reserve().unwrap();
        drop(loan);
        assert_eq!(jobs.residents.load(Ordering::Acquire), 0);
        jobs.next = None;
        assert!(matches!(jobs.reserve(), Err(JobError::Exhausted)));
        jobs.next = Some(100);
        let id = jobs
            .start(request(root.path(), "while :; do :; done"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut started = false;
        while !started && Instant::now() < deadline {
            started = jobs
                .poll()
                .iter()
                .any(|event| matches!(event,JobEvent::Started{job,..} if *job==id));
            thread::sleep(Duration::from_millis(2));
        }
        assert!(started);
        jobs.cancel(id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while jobs.is_running(id) && Instant::now() < deadline {
            jobs.poll();
            thread::sleep(Duration::from_millis(2));
        }
        assert!(!jobs.is_running(id));
        jobs.begin_shutdown();
        assert!(matches!(jobs.reserve(), Err(JobError::Stopped)));
        let start = Instant::now();
        drop(jobs);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    fn submission(root: &std::path::Path, script: &str) -> (Submission, Arc<AtomicUsize>) {
        let residents = Arc::new(AtomicUsize::new(1));
        let mut request = request(root, script);
        request
            .environment
            .insert("PATH".into(), std::env::var_os("PATH").unwrap());
        (
            Submission {
                root: None,
                input: None,
                id: JobId(1),
                request,
                cancel: Arc::new(AtomicBool::new(false)),
                slot: Arc::new(JobSlot(residents.clone())),
            },
            residents,
        )
    }

    #[test]
    fn queued_job_rechecks_cwd_symlink_containment() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cwd = root.path().join("sub");
        std::fs::create_dir(&cwd).unwrap();
        let (mut queued, _) = submission(&cwd, "printf forbidden > marker");
        queued.root = Some(crate::files::RootIdentity::capture(root.path()).unwrap());
        std::fs::remove_dir(&cwd).unwrap();
        std::os::unix::fs::symlink(outside.path(), &cwd).unwrap();
        let mut engine = JobEngine::new(16);
        assert!(matches!(engine.start(queued), Err(JobError::RootChanged)));
        assert!(!outside.path().join("marker").exists());
    }

    #[test]
    fn completed_job_is_not_timed_out_when_polling_is_late() {
        let root = tempfile::tempdir().unwrap();
        let (mut queued, _) = submission(root.path(), "exit 0");
        queued.request.policy.timeout = Duration::from_millis(1);
        let mut engine = JobEngine::new(16);
        engine.start(queued).unwrap();
        let running = engine.jobs.get_mut(&JobId(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while job_exit_status(&mut running.child).unwrap().is_none() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(job_exit_status(&mut running.child).unwrap(), Some(Some(0)));
        running.started = Instant::now() - Duration::from_secs(1);
        let mut exited = false;
        while !engine.jobs.is_empty() {
            for event in engine.poll() {
                if let JobEvent::Exited {
                    code, timed_out, ..
                } = event
                {
                    assert_eq!(code, Some(0));
                    assert!(!timed_out);
                    exited = true;
                }
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(exited);
    }

    #[test]
    fn oversized_output_lines_discard_the_tail_before_the_next_line() {
        let root = tempfile::tempdir().unwrap();
        let (mut queued, _) = submission(root.path(), "printf '123456789\\nok\\nabcdefghi'");
        queued.request.policy.maximum_line_bytes = 4;
        let mut engine = JobEngine::new(16);
        engine.start(queued).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut lines = Vec::new();
        while !engine.jobs.is_empty() {
            for event in engine.poll() {
                if let JobEvent::Output {
                    bytes, truncated, ..
                } = event
                {
                    lines.push((bytes, truncated));
                }
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(
            lines,
            vec![
                (b"1234".to_vec(), true),
                (b"ok\n".to_vec(), false),
                (b"abcd".to_vec(), true)
            ]
        );
    }

    #[test]
    fn job_cancellation_releases_escaped_descendants_pipe_readers() {
        struct Escaped(u32);
        impl Drop for Escaped {
            fn drop(&mut self) {
                crate::process::terminate_process_group(self.0);
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (queued, residents) = submission(
            root.path(),
            "exec python3 -c 'import os,time\nif os.fork() == 0:\n os.setsid()\n print(os.getpid(), flush=True)\n time.sleep(30)\n'",
        );
        let cancel = queued.cancel.clone();
        let mut engine = JobEngine::new(1);
        engine.start(queued).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let escaped = loop {
            let output = engine.poll().into_iter().find_map(|event| match event {
                JobEvent::Output {
                    stream: JobStream::Stdout,
                    bytes,
                    ..
                } => Some(bytes),
                JobEvent::Output { bytes, .. } => {
                    panic!("fixture stderr: {}", String::from_utf8_lossy(&bytes))
                }
                _ => None,
            });
            if let Some(bytes) = output {
                break Escaped(String::from_utf8(bytes).unwrap().trim().parse().unwrap());
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        cancel.store(true, Ordering::Release);
        while !engine.jobs.is_empty() || residents.load(Ordering::Acquire) != 0 {
            engine.poll();
            assert!(
                Instant::now() < deadline,
                "escaped descendant retained job readers"
            );
            thread::sleep(Duration::from_millis(2));
        }
        drop(escaped);
    }

    #[test]
    fn cancellation_releases_readers_without_draining_backpressured_output() {
        let root = tempfile::tempdir().unwrap();
        let (queued, _) = submission(root.path(), "printf 'one\ntwo\nthree\nfour\n'");
        let cancel = queued.cancel.clone();
        let mut engine = JobEngine::new(1);
        engine.start(queued).unwrap();
        assert!(matches!(
            engine
                .receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            JobEvent::Output { .. }
        ));
        cancel.store(true, Ordering::Release);
        engine.expire();
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.jobs[&JobId(1)].readers.load(Ordering::Acquire) != 0 {
            assert!(
                Instant::now() < deadline,
                "output delivery ignored cancellation"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn job_deadline_releases_descendant_pipes_after_parent_exit() {
        let root = tempfile::tempdir().unwrap();
        let (queued, residents) = submission(root.path(), "sleep 30 & printf spawned");
        let mut engine = JobEngine::new(16);
        engine.start(queued).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.jobs[&JobId(1)].status.is_none() {
            engine.poll();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        let running = engine.jobs.get_mut(&JobId(1)).unwrap();
        assert_ne!(running.readers.load(Ordering::Acquire), 0);
        running.started = Instant::now() - running.timeout;
        let mut exited = false;
        while !engine.jobs.is_empty() || residents.load(Ordering::Acquire) != 0 {
            for event in engine.poll() {
                if let JobEvent::Exited {
                    code, timed_out, ..
                } = event
                {
                    assert_eq!(code, Some(0));
                    assert!(!timed_out);
                    exited = true;
                }
            }
            assert!(
                Instant::now() < deadline,
                "descendant pipes outlived the job deadline"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert!(exited);
    }

    #[test]
    fn job_cancellation_releases_descendants_after_parent_exit() {
        let root = tempfile::tempdir().unwrap();
        let (queued, residents) = submission(root.path(), "sleep 30 & printf spawned");
        let cancel = queued.cancel.clone();
        let mut engine = JobEngine::new(16);
        engine.start(queued).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while engine.jobs[&JobId(1)].status.is_none() {
            engine.poll();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_ne!(engine.jobs[&JobId(1)].readers.load(Ordering::Acquire), 0);
        let running = engine.jobs.get_mut(&JobId(1)).unwrap();
        let pid = nix::unistd::Pid::from_raw(running.child.id() as i32);
        // Observation must not reap the leader while cleanup can still signal its group.
        assert_eq!(job_exit_status(&mut running.child).unwrap(), Some(Some(0)));
        cancel.store(true, Ordering::Release);
        while !engine.jobs.is_empty() || residents.load(Ordering::Acquire) != 0 {
            engine.poll();
            assert!(Instant::now() < deadline, "descendant retained job readers");
            thread::sleep(Duration::from_millis(2));
        }
        use nix::sys::wait::Id;
        use nix::sys::wait::WaitPidFlag;
        use nix::sys::wait::waitid;
        assert_eq!(
            waitid(Id::Pid(pid), WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG),
            Err(nix::errno::Errno::ECHILD)
        );
    }

    #[test]
    fn job_cancellation_kills_descendants_ignoring_sigterm() {
        let root = tempfile::tempdir().unwrap();
        let (queued, residents) = submission(
            root.path(),
            "(trap '' TERM; printf 'ready\\n'; exec sleep 30) & wait",
        );
        let cancel = queued.cancel.clone();
        let mut engine = JobEngine::new(16);
        engine.start(queued).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !engine
            .poll()
            .iter()
            .any(|event| matches!(event, JobEvent::Output { bytes, .. } if bytes == b"ready\n"))
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::Release);
        while !engine.jobs.is_empty() || residents.load(Ordering::Acquire) != 0 {
            engine.poll();
            assert!(
                Instant::now() < deadline,
                "SIGTERM-resistant descendant retained job readers"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn job_launch_preserves_original_executable_invocation() {
        let root = tempfile::tempdir().unwrap();
        let invocation = root.path().join("custom-sh");
        std::os::unix::fs::symlink("/bin/sh", &invocation).unwrap();
        let (mut queued, _) = submission(root.path(), "printf '%s' \"$0\"");
        queued.request.program = invocation.canonicalize().unwrap();
        queued.request.argv0 = Some(invocation.clone());
        let mut engine = JobEngine::new(16);
        engine.start(queued).unwrap();
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !engine.jobs.is_empty() {
            for event in engine.poll() {
                if let JobEvent::Output { bytes, .. } = event {
                    output.extend(bytes);
                }
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            String::from_utf8(output).unwrap(),
            invocation.to_str().unwrap()
        );
    }
}
