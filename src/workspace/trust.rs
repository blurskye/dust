//! Workspace trust records and background persistence.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;

use serde::Deserialize;

use serde::Serialize;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::files::VfsError;

use crate::files::RootIdentity;

#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    trusted: HashSet<RootIdentity>,
    seen: HashSet<RootIdentity>,
}

impl TrustStore {
    pub fn replace(&mut self, identities: impl IntoIterator<Item = RootIdentity>) {
        self.trusted = identities.into_iter().collect();
        self.seen.extend(self.trusted.iter().cloned());
    }

    pub fn replace_seen(&mut self, identities: impl IntoIterator<Item = RootIdentity>) {
        self.seen = identities.into_iter().collect();
        self.seen.extend(self.trusted.iter().cloned());
    }

    #[must_use]
    pub fn identities(&self) -> Vec<RootIdentity> {
        sorted_identities(&self.trusted)
    }

    #[must_use]
    pub fn seen_identities(&self) -> Vec<RootIdentity> {
        sorted_identities(&self.seen)
    }

    pub fn trust(&mut self, identity: RootIdentity) {
        self.mark_seen(identity.clone());
        if !self.seen.contains(&identity) {
            return;
        }
        self.trusted
            .retain(|candidate| candidate.canonical_path != identity.canonical_path);
        self.trusted.insert(identity);
    }

    pub fn mark_seen(&mut self, identity: RootIdentity) -> bool {
        self.trusted.retain(|candidate| {
            candidate.canonical_path != identity.canonical_path || candidate == &identity
        });
        if self.seen.contains(&identity) {
            return false;
        }
        self.seen
            .retain(|candidate| candidate.canonical_path != identity.canonical_path);
        if self.seen.len() >= TRUST_IDENTITY_LIMIT {
            let oldest = sorted_identities(&self.seen)
                .into_iter()
                .find(|candidate| !self.trusted.contains(candidate));
            if let Some(oldest) = oldest {
                self.seen.remove(&oldest);
            } else {
                return false;
            }
        }
        self.seen.insert(identity);
        true
    }

    pub fn revoke(&mut self, identity: &RootIdentity) {
        self.trusted.remove(identity);
    }

    /// Drop every record of a folder, trusted or seen, such as one that is being deleted.
    pub fn forget(&mut self, path: &std::path::Path) {
        self.trusted
            .retain(|identity| identity.canonical_path != path);
        self.seen.retain(|identity| identity.canonical_path != path);
    }

    #[must_use]
    pub fn is_trusted(&self, identity: &RootIdentity) -> bool {
        self.trusted.contains(identity)
    }

    #[must_use]
    pub fn is_seen(&self, identity: &RootIdentity) -> bool {
        self.seen.contains(identity)
    }
}

fn sorted_identities(identities: &HashSet<RootIdentity>) -> Vec<RootIdentity> {
    let mut identities = identities.iter().cloned().collect::<Vec<_>>();
    identities.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
    identities
}

pub const TRUST_STATE_VERSION: u32 = 2;
const LEGACY_TRUST_STATE_VERSION: u32 = 1;
pub(super) const TRUST_STATE_BYTE_LIMIT: u64 = 1024 * 1024;
pub(super) const TRUST_IDENTITY_LIMIT: usize = 256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustDatabase {
    pub version: u32,
    pub identities: Vec<RootIdentity>,
    #[serde(default)]
    pub seen: Vec<RootIdentity>,
}

impl Default for TrustDatabase {
    fn default() -> Self {
        Self {
            version: TRUST_STATE_VERSION,
            identities: Vec::new(),
            seen: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum TrustStateError {
    #[error("failed to {operation} trust state `{}`: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("trust state `{}` exceeds the {} byte safety limit", .0.display(), TRUST_STATE_BYTE_LIMIT)]
    TooLarge(PathBuf),
    #[error("invalid trust state `{}`: {reason}", path.display())]
    Invalid { path: PathBuf, reason: String },
    #[error("unsupported trust state version {0}")]
    UnsupportedVersion(u32),
    #[error("trust state contains too many identities")]
    TooManyIdentities,
    #[error("persisted trust identity lacks filesystem identity: `{}`", .0.display())]
    MissingFilesystemIdentity(PathBuf),
    #[error("workspace trust worker queue is full")]
    QueueFull,
    #[error("workspace trust worker stopped")]
    WorkerStopped,
}

pub(super) fn validate_trust_database(
    path: &Path,
    database: &TrustDatabase,
) -> Result<(), TrustStateError> {
    if database.version != TRUST_STATE_VERSION {
        return Err(TrustStateError::UnsupportedVersion(database.version));
    }
    if database.identities.len() > TRUST_IDENTITY_LIMIT
        || database.seen.len() > TRUST_IDENTITY_LIMIT
    {
        return Err(TrustStateError::TooManyIdentities);
    }
    validate_identities(path, &database.identities)?;
    validate_identities(path, &database.seen)?;
    if database
        .identities
        .iter()
        .any(|identity| !database.seen.contains(identity))
    {
        return Err(TrustStateError::Invalid {
            path: path.to_path_buf(),
            reason: "every trusted identity must also be recorded as seen".into(),
        });
    }
    Ok(())
}

fn validate_identities(path: &Path, identities: &[RootIdentity]) -> Result<(), TrustStateError> {
    let mut unique = HashSet::new();
    let mut paths = HashSet::new();
    for identity in identities {
        if identity.filesystem_identity.is_none() {
            return Err(TrustStateError::MissingFilesystemIdentity(
                identity.canonical_path.clone(),
            ));
        }
        if !identity.canonical_path.is_absolute()
            || !unique.insert(identity.clone())
            || !paths.insert(identity.canonical_path.clone())
        {
            return Err(TrustStateError::Invalid {
                path: path.to_path_buf(),
                reason: "identities must be unique absolute canonical paths".into(),
            });
        }
    }
    Ok(())
}

pub(super) fn load_trust_database(path: &Path) -> Result<TrustDatabase, TrustStateError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(TrustDatabase::default());
        }
        Err(source) => {
            return Err(TrustStateError::Io {
                operation: "open",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut bytes = Vec::new();
    file.take(TRUST_STATE_BYTE_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| TrustStateError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > TRUST_STATE_BYTE_LIMIT {
        return Err(TrustStateError::TooLarge(path.to_path_buf()));
    }
    let source = String::from_utf8(bytes).map_err(|error| TrustStateError::Invalid {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let mut database =
        toml::from_str::<TrustDatabase>(&source).map_err(|error| TrustStateError::Invalid {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    if database.version == LEGACY_TRUST_STATE_VERSION {
        database.version = TRUST_STATE_VERSION;
        database.seen = database.identities.clone();
    }
    validate_trust_database(path, &database)?;
    Ok(database)
}

pub(super) fn persist_trust_database(
    path: &Path,
    database: &TrustDatabase,
) -> Result<(), TrustStateError> {
    validate_trust_database(path, database)?;
    let source = toml::to_string(database).map_err(|error| TrustStateError::Invalid {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if source.len() as u64 > TRUST_STATE_BYTE_LIMIT {
        return Err(TrustStateError::TooLarge(path.to_path_buf()));
    }
    let parent = path.parent().ok_or_else(|| TrustStateError::Invalid {
        path: path.to_path_buf(),
        reason: "trust state path has no parent".into(),
    })?;
    fs::create_dir_all(parent).map_err(|source| TrustStateError::Io {
        operation: "create parent directory for",
        path: path.to_path_buf(),
        source,
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| TrustStateError::Io {
        operation: "create temporary",
        path: path.to_path_buf(),
        source,
    })?;
    temporary
        .write_all(source.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| TrustStateError::Io {
            operation: "write",
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| TrustStateError::Io {
            operation: "replace",
            path: path.to_path_buf(),
            source: error.error,
        })?;
    Ok(())
}

/// Apply only this instance's changes to the current database, under a process-shared lock.
fn update_trust_database(
    path: &Path,
    previous: &TrustDatabase,
    desired: &TrustDatabase,
    revoked: &[RootIdentity],
) -> Result<(), TrustStateError> {
    validate_trust_database(path, desired)?;
    let parent = path.parent().ok_or_else(|| TrustStateError::Invalid {
        path: path.to_path_buf(),
        reason: "trust state path has no parent".into(),
    })?;
    let io_error = |source| TrustStateError::Io {
        operation: "lock",
        path: path.to_path_buf(),
        source,
    };
    fs::create_dir_all(parent).map_err(io_error)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(io_error)?;
    let _lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, error)| io_error(io::Error::from_raw_os_error(error as i32)))?;
    let mut current = load_trust_database(path)?;
    for (entries, before, after) in [
        (
            &mut current.identities,
            &previous.identities,
            &desired.identities,
        ),
        (&mut current.seen, &previous.seen, &desired.seen),
    ] {
        entries.retain(|identity| !before.contains(identity) || after.contains(identity));
        for identity in after.iter().filter(|identity| !before.contains(identity)) {
            entries.retain(|candidate| candidate.canonical_path != identity.canonical_path);
            entries.push(identity.clone());
        }
    }
    current.identities.retain(|identity| {
        !revoked.contains(identity)
            && !desired.seen.iter().any(|observed| {
                !previous.seen.contains(observed)
                    && observed.canonical_path == identity.canonical_path
                    && observed != identity
            })
    });
    // Seen history may be evicted locally; keep every identity another instance still trusts.
    for identity in &current.identities {
        if !current.seen.contains(identity) {
            current.seen.push(identity.clone());
        }
    }
    while current.seen.len() > TRUST_IDENTITY_LIMIT {
        let Some(position) = current
            .seen
            .iter()
            .position(|identity| !current.identities.contains(identity))
        else {
            return Err(TrustStateError::TooManyIdentities);
        };
        current.seen.remove(position);
    }
    persist_trust_database(path, &current)
}

#[derive(Debug)]
pub(super) enum TrustRequest {
    Load {
        request_id: u64,
        path: PathBuf,
    },
    InspectRoot {
        request_id: u64,
        path: PathBuf,
    },
    Persist {
        request_id: u64,
        path: PathBuf,
        database: TrustDatabase,
        revoked: Vec<RootIdentity>,
    },
}

#[derive(Debug)]
pub enum TrustEvent {
    Loaded {
        request_id: u64,
        result: Result<TrustDatabase, TrustStateError>,
    },
    RootInspected {
        request_id: u64,
        result: Result<RootIdentity, VfsError>,
    },
    Persisted {
        request_id: u64,
        result: Result<(), TrustStateError>,
    },
}

pub struct WorkspaceTrustWorker {
    sender: SyncSender<TrustRequest>,
    receiver: Receiver<TrustEvent>,
    next_request_id: u64,
    pending_writes: usize,
    write_error: Option<String>,
}

impl WorkspaceTrustWorker {
    pub fn start(queue_capacity: usize) -> Self {
        let (request_sender, request_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let (event_sender, event_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        thread::Builder::new()
            .name("dust-workspace-trust".into())
            .spawn(move || {
                let mut previous = TrustDatabase::default();
                while let Ok(request) = request_receiver.recv() {
                    let event = match request {
                        TrustRequest::Load { request_id, path } => {
                            let result = load_trust_database(&path);
                            if let Ok(database) = &result {
                                previous = database.clone();
                            }
                            TrustEvent::Loaded { request_id, result }
                        }
                        TrustRequest::InspectRoot { request_id, path } => {
                            TrustEvent::RootInspected {
                                request_id,
                                result: RootIdentity::capture(&path),
                            }
                        }
                        TrustRequest::Persist {
                            request_id,
                            path,
                            database,
                            revoked,
                        } => {
                            let result =
                                update_trust_database(&path, &previous, &database, &revoked);
                            if result.is_ok() {
                                previous = database;
                            }
                            TrustEvent::Persisted { request_id, result }
                        }
                    };
                    if event_sender.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("workspace trust worker thread");
        Self {
            sender: request_sender,
            receiver: event_receiver,
            next_request_id: 0,
            pending_writes: 0,
            write_error: None,
        }
    }

    fn next_request_id(&mut self) -> Result<u64, TrustStateError> {
        self.next_request_id =
            self.next_request_id
                .checked_add(1)
                .ok_or_else(|| TrustStateError::Invalid {
                    path: PathBuf::new(),
                    reason: "trust request identity exhausted".into(),
                })?;
        Ok(self.next_request_id)
    }

    pub fn load(&mut self, path: PathBuf) -> Result<u64, TrustStateError> {
        let request_id = self.next_request_id()?;
        self.sender
            .try_send(TrustRequest::Load { request_id, path })
            .map_err(map_trust_send_error)?;
        Ok(request_id)
    }

    pub fn inspect_root(&mut self, path: PathBuf) -> Result<u64, TrustStateError> {
        let request_id = self.next_request_id()?;
        self.sender
            .try_send(TrustRequest::InspectRoot { request_id, path })
            .map_err(map_trust_send_error)?;
        Ok(request_id)
    }

    pub fn persist(
        &mut self,
        path: PathBuf,
        identities: Vec<RootIdentity>,
        seen: Vec<RootIdentity>,
        revoked: Vec<RootIdentity>,
    ) -> Result<u64, TrustStateError> {
        let request_id = self.next_request_id()?;
        self.sender
            .try_send(TrustRequest::Persist {
                request_id,
                path,
                database: TrustDatabase {
                    version: TRUST_STATE_VERSION,
                    identities,
                    seen,
                },
                revoked,
            })
            .map_err(map_trust_send_error)?;
        self.pending_writes += 1;
        Ok(request_id)
    }

    pub fn pending_writes(&self) -> bool {
        self.pending_writes != 0
    }

    pub fn shutdown_result(&self) -> Result<(), String> {
        if self.pending_writes() {
            return Err(
                "workspace trust writes did not finish before the shared shutdown deadline".into(),
            );
        }
        self.write_error.clone().map_or(Ok(()), Err)
    }

    pub fn try_event(&mut self) -> Result<Option<TrustEvent>, TrustStateError> {
        match self.receiver.try_recv() {
            Ok(event) => {
                if let TrustEvent::Persisted { result, .. } = &event {
                    self.pending_writes -= 1;
                    if let Err(error) = result {
                        self.write_error = Some(error.to_string());
                    }
                }
                Ok(Some(event))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TrustStateError::WorkerStopped),
        }
    }
}

pub(super) fn map_trust_send_error(error: TrySendError<TrustRequest>) -> TrustStateError {
    match error {
        TrySendError::Full(_) => TrustStateError::QueueFull,
        TrySendError::Disconnected(_) => TrustStateError::WorkerStopped,
    }
}

#[cfg(test)]
mod request_id_tests {
    use crate::workspace::trust::*;
    #[test]
    fn exhausted_request_id_never_reuses_an_accepted_identity() {
        let mut service = WorkspaceTrustWorker::start(1);
        service.next_request_id = u64::MAX;
        assert!(matches!(
            service.inspect_root(PathBuf::new()),
            Err(TrustStateError::Invalid { .. })
        ));
        assert!(matches!(
            service.load(PathBuf::new()),
            Err(TrustStateError::Invalid { .. })
        ));
    }
}

#[cfg(test)]
mod persistence_tests {
    use crate::workspace::trust::*;

    fn wait_for_event(service: &mut WorkspaceTrustWorker) -> TrustEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(event) = service.try_event().unwrap() {
                return event;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn non_utf8_trust_paths_round_trip_without_changing_utf8_records() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir
            .path()
            .join(std::ffi::OsString::from_vec(b"root-\xff".to_vec()));
        let utf8 = RootIdentity::capture(dir.path()).unwrap();
        let identity = RootIdentity {
            canonical_path: root,
            filesystem_identity: utf8.filesystem_identity,
        };
        let database = TrustDatabase {
            identities: vec![identity.clone(), utf8.clone()],
            seen: vec![identity, utf8],
            ..TrustDatabase::default()
        };
        let path = dir.path().join("trust.toml");
        persist_trust_database(&path, &database).unwrap();
        assert_eq!(load_trust_database(&path).unwrap(), database);
        assert!(fs::read_to_string(&path).unwrap().contains(&format!(
            "canonical_path = {:?}",
            dir.path().to_str().unwrap()
        )));
    }

    #[test]
    fn explicit_revocation_removes_a_grant_missing_from_the_loaded_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let identity = RootIdentity::capture(dir.path()).unwrap();
        let path = dir.path().join("trust.toml");
        let mut service = WorkspaceTrustWorker::start(2);
        service.load(path.clone()).unwrap();
        assert!(matches!(
            wait_for_event(&mut service),
            TrustEvent::Loaded { result: Ok(_), .. }
        ));
        persist_trust_database(
            &path,
            &TrustDatabase {
                identities: vec![identity.clone()],
                seen: vec![identity.clone()],
                ..TrustDatabase::default()
            },
        )
        .unwrap();
        service
            .persist(
                path.clone(),
                vec![],
                vec![identity.clone()],
                vec![identity.clone()],
            )
            .unwrap();
        assert!(service.pending_writes());
        assert!(matches!(
            wait_for_event(&mut service),
            TrustEvent::Persisted { result: Ok(()), .. }
        ));
        assert!(!service.pending_writes());
        let loaded = load_trust_database(&path).unwrap();
        assert!(loaded.identities.is_empty());
        assert_eq!(loaded.seen, vec![identity]);
    }

    #[test]
    fn observing_a_replaced_root_invalidates_concurrent_trust_of_the_old_identity() {
        let dir = tempfile::tempdir().unwrap();
        let old = RootIdentity::capture(dir.path()).unwrap();
        let mut replacement = old.clone();
        replacement.filesystem_identity = Some((123, 456));
        let path = dir.path().join("trust.toml");
        persist_trust_database(
            &path,
            &TrustDatabase {
                identities: vec![old.clone()],
                seen: vec![old],
                ..TrustDatabase::default()
            },
        )
        .unwrap();
        let desired = TrustDatabase {
            seen: vec![replacement.clone()],
            ..TrustDatabase::default()
        };
        update_trust_database(&path, &TrustDatabase::default(), &desired, &[]).unwrap();
        let loaded = load_trust_database(&path).unwrap();
        assert!(loaded.identities.is_empty());
        assert_eq!(loaded.seen, vec![replacement]);
    }

    #[test]
    fn shutdown_tracks_all_accepted_writes_and_retains_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.toml");
        let mut service = WorkspaceTrustWorker::start(2);
        service
            .persist(dir.path().to_path_buf(), vec![], vec![], vec![])
            .unwrap();
        service.persist(path, vec![], vec![], vec![]).unwrap();
        assert!(service.shutdown_result().is_err());
        assert!(matches!(
            wait_for_event(&mut service),
            TrustEvent::Persisted { result: Err(_), .. }
        ));
        assert!(service.pending_writes());
        assert!(matches!(
            wait_for_event(&mut service),
            TrustEvent::Persisted { result: Ok(()), .. }
        ));
        assert!(!service.pending_writes());
        assert!(
            service
                .shutdown_result()
                .unwrap_err()
                .contains("trust state")
        );
    }

    #[test]
    fn stale_trust_snapshots_cannot_restore_revocations() {
        let dir = tempfile::tempdir().unwrap();
        let a = RootIdentity::capture(dir.path()).unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let b = RootIdentity::capture(b_dir.path()).unwrap();
        let path = dir.path().join("trust.toml");
        let initial = TrustDatabase {
            identities: vec![a.clone()],
            seen: vec![a.clone()],
            ..TrustDatabase::default()
        };
        persist_trust_database(&path, &initial).unwrap();
        let revoked = TrustDatabase {
            identities: vec![],
            ..initial.clone()
        };
        update_trust_database(&path, &initial, &revoked, &[]).unwrap();
        let stale = TrustDatabase {
            identities: vec![a.clone(), b.clone()],
            seen: vec![a.clone(), b.clone()],
            ..TrustDatabase::default()
        };
        update_trust_database(&path, &initial, &stale, &[]).unwrap();
        let loaded = load_trust_database(&path).unwrap();
        assert_eq!(loaded.identities, vec![b]);
        assert!(loaded.seen.contains(&a));
    }

    #[test]
    fn seen_history_stays_within_the_persistence_limit() {
        let mut store = TrustStore::default();
        let identities = (0..=TRUST_IDENTITY_LIMIT)
            .map(|n| RootIdentity {
                canonical_path: PathBuf::from(format!("/workspace/{n}")),
                filesystem_identity: Some((1, n as u64)),
            })
            .collect::<Vec<_>>();
        store.trust(identities[0].clone());
        for identity in &identities[1..] {
            assert!(store.mark_seen(identity.clone()));
        }
        assert!(store.is_trusted(&identities[0]));
        assert_eq!(store.seen_identities().len(), TRUST_IDENTITY_LIMIT);
        let database = TrustDatabase {
            identities: store.identities(),
            seen: store.seen_identities(),
            ..TrustDatabase::default()
        };
        let dir = tempfile::tempdir().unwrap();
        persist_trust_database(&dir.path().join("trust.toml"), &database).unwrap();
        store.revoke(&identities[0]);
        persist_trust_database(
            &dir.path().join("trust.toml"),
            &TrustDatabase {
                identities: store.identities(),
                seen: store.seen_identities(),
                ..TrustDatabase::default()
            },
        )
        .unwrap();
    }
}
