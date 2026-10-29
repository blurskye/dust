//! Disposable fixture sandboxes for the tutor surface.
//!
//! Practice is an explicitly entered surface, and its
//! exercises run against disposable fixtures isolated from the user's repositories, working
//! trees, credentials, hooks, network and any authenticated provider.
//!
//! A fixture is created under the system temporary directory, never inside a real workspace
//! root, and is removed by disposal. This module performs no authentication and registers no
//! credential callback, and the binary carries libgit2 without transport features, so a fixture
//! cannot reach a network. Creation uses libgit2 directly, which never executes a hook.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

/// Process-wide, so two services in one process can never mint the same fixture path. A
/// per-service counter collides the moment a second exercise surface opens.
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Correlates a fixture request with its outcome, so a stale event is never applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TutorGeneration(pub u64);

/// A created fixture. `path` is canonical, so a caller can compare it with a real root without
/// canonicalizing again in the foreground.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TutorFixture {
    pub generation: TutorGeneration,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TutorError {
    #[error("tutor fixture I/O failed: {0}")]
    Io(String),
    #[error("tutor fixture refused: `{0}` is inside a real workspace root")]
    InsideRealRoot(String),
    #[error("tutor fixture refused: `{0}` would contain a real workspace root")]
    ContainsRealRoot(String),
    #[error("tutor fixture destination is not usable: {0}")]
    Destination(String),
    #[error("tutor fixture backend failed")]
    Backend,
    #[error("tutor fixture deadline exceeded")]
    Deadline,
}

pub enum TutorRequest {
    Create {
        generation: TutorGeneration,
        real_roots: Vec<PathBuf>,
        deadline: Duration,
    },
    Dispose {
        generation: TutorGeneration,
        path: PathBuf,
        deadline: Duration,
    },
}

#[derive(Debug)]
pub enum TutorEvent {
    Created {
        generation: TutorGeneration,
        result: Result<PathBuf, TutorError>,
    },
    Disposed {
        generation: TutorGeneration,
        result: Result<(), TutorError>,
    },
}

pub struct TutorService {
    sender: SyncSender<TutorRequest>,
    receiver: Receiver<TutorEvent>,
    next_generation: u64,
    /// Set when the worker thread is gone, so a caller is told rather than left waiting.
    worker_gone: Arc<AtomicBool>,
}

impl TutorService {
    pub fn start(queue_capacity: usize) -> Self {
        let (request_sender, request_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let (event_sender, event_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let worker_gone = Arc::new(AtomicBool::new(false));
        let gone = Arc::clone(&worker_gone);
        thread::Builder::new()
            .name("dust-tutor".into())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    let event = match request {
                        TutorRequest::Create {
                            generation,
                            real_roots,
                            deadline,
                        } => {
                            let result = create_fixture(&real_roots, deadline);
                            TutorEvent::Created { generation, result }
                        }
                        TutorRequest::Dispose {
                            generation,
                            path,
                            deadline,
                        } => {
                            let result = dispose_fixture(&path, deadline);
                            TutorEvent::Disposed { generation, result }
                        }
                    };
                    if event_sender.send(event).is_err() {
                        break;
                    }
                }
                gone.store(true, Ordering::Release);
            })
            .expect("tutor worker thread");
        Self {
            sender: request_sender,
            receiver: event_receiver,
            next_generation: 1,
            worker_gone,
        }
    }

    /// Ask for a disposable fixture outside every real root.
    pub fn create(
        &mut self,
        real_roots: Vec<PathBuf>,
        deadline: Duration,
    ) -> Result<TutorGeneration, TutorError> {
        let generation = TutorGeneration(self.next_generation);
        self.next_generation += 1;
        self.sender
            .send(TutorRequest::Create {
                generation,
                real_roots,
                deadline,
            })
            .map_err(|_| TutorError::Backend)?;
        Ok(generation)
    }

    /// Ask for the fixture to be removed. Disposal is idempotent: an absent fixture is already
    /// disposed rather than an error.
    pub fn dispose(&mut self, path: PathBuf, deadline: Duration) -> Result<TutorGeneration, TutorError> {
        let generation = TutorGeneration(self.next_generation);
        self.next_generation += 1;
        self.sender
            .send(TutorRequest::Dispose {
                generation,
                path,
                deadline,
            })
            .map_err(|_| TutorError::Backend)?;
        Ok(generation)
    }

    /// Poll exactly once per call: a second poll can consume and discard the event this call
    /// was written to observe.
    pub fn poll(&mut self) -> Option<TutorEvent> {
        match self.receiver.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    #[must_use]
    pub fn worker_gone(&self) -> bool {
        self.worker_gone.load(Ordering::Acquire)
    }
}

/// Create the fixture. Every refusal is decided before the first write.
fn create_fixture(real_roots: &[PathBuf], deadline: Duration) -> Result<PathBuf, TutorError> {
    let started = Instant::now();
    let base = std::env::temp_dir();
    let base = std::fs::canonicalize(&base)
        .map_err(|error| TutorError::Destination(error.to_string()))?;
    if !base.is_dir() {
        return Err(TutorError::Destination(base.display().to_string()));
    }
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let candidate = base.join(format!("dust-tutor-{}-{sequence}", std::process::id()));

    // A fixture lives outside every real root, and never contains one.
    for root in real_roots {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if candidate.starts_with(&root) {
            return Err(TutorError::InsideRealRoot(root.display().to_string()));
        }
        if root.starts_with(&candidate) {
            return Err(TutorError::ContainsRealRoot(root.display().to_string()));
        }
    }
    if std::fs::create_dir_all(&candidate).is_err() {
        return Err(TutorError::Destination(candidate.display().to_string()));
    }
    let deadline_elapsed = || started.elapsed() > deadline;
    let repository = match git2::Repository::init(&candidate) {
        Ok(repository) => repository,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&candidate);
            return Err(TutorError::Backend);
        }
    };
    let outcome = seed_fixture(&repository, deadline_elapsed);
    drop(repository);
    if let Err(error) = outcome {
        let _ = std::fs::remove_dir_all(&candidate);
        return Err(error);
    }
    std::fs::canonicalize(&candidate).map_err(|error| TutorError::Io(error.to_string()))
}

/// One commit, so the fixture is a repository with history to practise on.
fn seed_fixture(
    repository: &git2::Repository,
    deadline_elapsed: impl Fn() -> bool,
) -> Result<(), TutorError> {
    let path = repository
        .workdir()
        .ok_or(TutorError::Backend)?
        .join("practice.txt");
    std::fs::write(&path, "practice\n").map_err(|error| TutorError::Io(error.to_string()))?;
    if deadline_elapsed() {
        return Err(TutorError::Deadline);
    }
    let mut index = repository.index().map_err(|_| TutorError::Backend)?;
    index
        .add_path(Path::new("practice.txt"))
        .map_err(|_| TutorError::Backend)?;
    index.write().map_err(|_| TutorError::Backend)?;
    let tree = repository
        .find_tree(index.write_tree().map_err(|_| TutorError::Backend)?)
        .map_err(|_| TutorError::Backend)?;
    let signature = git2::Signature::now("Dust Tutor", "tutor@dust.invalid")
        .map_err(|_| TutorError::Backend)?;
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Begin the exercise",
            &tree,
            &[],
        )
        .map_err(|_| TutorError::Backend)?;
    Ok(())
}

/// Remove the fixture. An absent fixture is already disposed, so disposal is idempotent.
fn dispose_fixture(path: &Path, deadline: Duration) -> Result<(), TutorError> {
    let started = Instant::now();
    match std::fs::remove_dir_all(path) {
        Ok(()) => {
            if started.elapsed() > deadline {
                return Err(TutorError::Deadline);
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TutorError::Io(error.to_string())),
    }
}
