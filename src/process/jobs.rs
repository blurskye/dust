use std::{
    collections::{BTreeMap, HashMap},
    io::{self, BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
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
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
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
    root: Option<crate::workspace::RootIdentity>,
    id: JobId,
    request: JobRequest,
    cancel: Arc<AtomicBool>,
    slot: Arc<JobSlot>,
}
/// Bounded admission only. Filesystem, spawn, process polling and teardown are worker-owned.
pub struct JobSupervisor {
    next: Option<u64>,
    jobs: HashMap<JobId, Arc<AtomicBool>>,
    sender: SyncSender<Submission>,
    receiver: Option<Receiver<JobEvent>>,
    stop: Arc<AtomicBool>,
    residents: Arc<AtomicUsize>,
    worker: thread::JoinHandle<()>,
}
pub(crate) struct JobAdmission<'a> {
    supervisor: &'a mut JobSupervisor,
    slot: Arc<JobSlot>,
}
impl JobSupervisor {
    pub fn new(event_capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Submission>(256);
        let (events, receiver) = mpsc::sync_channel(event_capacity.clamp(1, 4096));
        let stop = Arc::new(AtomicBool::new(false));
        let closing = stop.clone();
        let worker = thread::Builder::new()
            .name("dust-jobs".into())
            .spawn(move || {
                let mut engine = JobEngine::new(event_capacity);
                while !closing.load(Ordering::Acquire) {
                    match rx.try_recv() {
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
                    thread::sleep(Duration::from_millis(2));
                }
            })
            .expect("job worker");
        Self {
            next: Some(1),
            jobs: HashMap::new(),
            sender: tx,
            receiver: Some(receiver),
            stop,
            residents: Arc::new(AtomicUsize::new(0)),
            worker,
        }
    }
    pub(crate) fn reserve(&mut self) -> Result<JobAdmission<'_>, JobError> {
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
        Ok(JobAdmission {
            supervisor: self,
            slot,
        })
    }
    pub fn start(&mut self, request: JobRequest) -> Result<JobId, JobError> {
        self.reserve()?.submit(request)
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
    }
}
impl JobAdmission<'_> {
    pub(crate) fn submit(self, request: JobRequest) -> Result<JobId, JobError> {
        self.submit_inner(request, None)
    }
    pub(crate) fn submit_scoped(
        self,
        request: JobRequest,
        root: crate::workspace::RootIdentity,
    ) -> Result<JobId, JobError> {
        if !request.cwd.starts_with(&root.canonical_path) {
            return Err(JobError::InvalidPolicy);
        }
        self.submit_inner(request, Some(root))
    }
    fn submit_inner(
        self,
        request: JobRequest,
        root: Option<crate::workspace::RootIdentity>,
    ) -> Result<JobId, JobError> {
        check_request(&request)?;
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
fn check_request(request: &JobRequest) -> Result<(), JobError> {
    let argument_bytes = request.arguments.iter().map(String::len).sum::<usize>();
    let environment_bytes = request
        .environment
        .iter()
        .map(|(key, value)| key.len().saturating_add(value.len()))
        .sum::<usize>();
    if !request.program.is_absolute()
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
        } = submission;
        if let Some(expected) = root {
            if crate::workspace::RootIdentity::capture(&expected.canonical_path)
                .ok()
                .as_ref()
                != Some(&expected)
            {
                return Err(JobError::RootChanged);
            }
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
            .current_dir(&request.cwd)
            .env_clear()
            .envs(&request.environment)
            .stdin(if request.stdin.is_some() {
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
        }
        let mut child = command.spawn().map_err(JobError::Spawn)?;
        let readers = Arc::new(AtomicUsize::new(0));
        let io_failed = Arc::new(AtomicBool::new(false));
        if let Some(bytes) = request.stdin {
            if let Some(mut stdin) = child.stdin.take() {
                let lease = slot.clone();
                let pending = readers.clone();
                let failed = io_failed.clone();
                pending.fetch_add(1, Ordering::AcqRel);
                thread::spawn(move || {
                    let _lease = lease;
                    if stdin.write_all(&bytes).is_err() {
                        failed.store(true, Ordering::Release);
                    }
                    pending.fetch_sub(1, Ordering::AcqRel);
                });
            }
        }
        let total = Arc::new(AtomicUsize::new(0));
        if let Some(stdout) = child.stdout.take() {
            stream_reader(
                id,
                JobStream::Stdout,
                stdout,
                self.sender.clone(),
                total.clone(),
                request.policy.clone(),
                readers.clone(),
                io_failed.clone(),
                slot.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            stream_reader(
                id,
                JobStream::Stderr,
                stderr,
                self.sender.clone(),
                total,
                request.policy.clone(),
                readers.clone(),
                io_failed.clone(),
                slot.clone(),
            );
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
            running.timed_out |= running.started.elapsed() >= running.timeout;
            if running.status.is_none()
                && !running.killed
                && (running.timed_out || running.cancel.load(Ordering::Acquire))
            {
                terminate_tree(&mut running.child);
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
                match running.child.try_wait() {
                    Ok(Some(status)) => running.status = Some(status.code()),
                    Ok(None) => {}
                    Err(error) => {
                        events.push(JobEvent::Failed {
                            job: *id,
                            message: error.to_string(),
                        });
                        done.push(*id);
                        continue;
                    }
                }
            }
            if let Some(code) = running.status {
                if running.readers.load(Ordering::Acquire) == 0 {
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
                } else if running.timed_out || running.cancel.load(Ordering::Acquire) {
                    events.push(JobEvent::Failed {
                        job: *id,
                        message: "process exited but output collection did not finish".into(),
                    });
                    done.push(*id);
                }
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
            if job.status.is_none() {
                terminate_tree(&mut job.child);
                let _ = job.child.wait();
            }
        }
    }
}
fn stream_reader(
    id: JobId,
    stream: JobStream,
    reader: impl Read + Send + 'static,
    sender: SyncSender<JobEvent>,
    total: Arc<AtomicUsize>,
    policy: JobPolicy,
    readers: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    slot: Arc<JobSlot>,
) {
    readers.fetch_add(1, Ordering::AcqRel);
    thread::spawn(move || {
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
            let mut bytes = Vec::new();
            let read = match reader
                .by_ref()
                .take(policy.maximum_line_bytes.saturating_add(1) as u64)
                .read_until(b'\n', &mut bytes)
            {
                Ok(read) => read,
                Err(_) => {
                    failed.store(true, Ordering::Release);
                    break;
                }
            };
            if read == 0 {
                break;
            }
            let line_truncated = bytes.len() > policy.maximum_line_bytes;
            bytes.truncate(policy.maximum_line_bytes);
            let allowed = reserve_output(&total, policy.maximum_output_bytes, bytes.len());
            bytes.truncate(allowed);
            let truncated = line_truncated || bytes.len() < read || allowed < read;
            if !bytes.is_empty() || truncated {
                if sender
                    .send(JobEvent::Output {
                        job: id,
                        stream,
                        bytes,
                        truncated,
                    })
                    .is_err()
                {
                    break;
                }
            }
            if truncated && allowed == 0 {
                break;
            }
        }
    });
}
fn reserve_output(total: &AtomicUsize, ceiling: usize, requested: usize) -> usize {
    let mut current = total.load(Ordering::Relaxed);
    loop {
        let allowed = requested.min(ceiling.saturating_sub(current));
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

fn terminate_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    let _ = child.kill();
}
#[derive(Debug, Error)]
pub enum JobError {
    #[error("job policy or request is invalid")]
    InvalidPolicy,
    #[error("cannot spawn job: {0}")]
    Spawn(#[source] io::Error),
    #[error("unknown job {0:?}")]
    Unknown(JobId),
    #[error("job admission is full")]
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
    use super::*;
    fn request(root: &std::path::Path, command: &str) -> JobRequest {
        JobRequest {
            program: PathBuf::from("/bin/sh"),
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
    fn job_admission_is_worker_owned_bounded_and_drains_output_before_terminal() {
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
        let identity = crate::workspace::RootIdentity::capture(&target).unwrap();
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
}
