use super::*;

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
        self.seen.insert(identity);
        true
    }

    pub fn revoke(&mut self, identity: &RootIdentity) {
        self.trusted.remove(identity);
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
    #[error("trust state `{}` exceeds the {} byte safety ceiling", .0.display(), TRUST_STATE_BYTE_LIMIT)]
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

#[derive(Debug)]
pub(super) enum TrustRequest {
    Load {
        generation: u64,
        path: PathBuf,
    },
    InspectRoot {
        generation: u64,
        path: PathBuf,
    },
    Persist {
        generation: u64,
        path: PathBuf,
        database: TrustDatabase,
    },
}

#[derive(Debug)]
pub enum TrustEvent {
    Loaded {
        generation: u64,
        result: Result<TrustDatabase, TrustStateError>,
    },
    RootInspected {
        generation: u64,
        result: Result<RootIdentity, WorkspaceError>,
    },
    Persisted {
        generation: u64,
        result: Result<(), TrustStateError>,
    },
}

pub struct WorkspaceTrustService {
    sender: SyncSender<TrustRequest>,
    receiver: Receiver<TrustEvent>,
    next_generation: u64,
}

impl WorkspaceTrustService {
    pub fn start(queue_capacity: usize) -> Self {
        let (request_sender, request_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let (event_sender, event_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        thread::Builder::new()
            .name("dust-workspace-trust".into())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    let event = match request {
                        TrustRequest::Load { generation, path } => TrustEvent::Loaded {
                            generation,
                            result: load_trust_database(&path),
                        },
                        TrustRequest::InspectRoot { generation, path } => {
                            TrustEvent::RootInspected {
                                generation,
                                result: RootIdentity::capture(&path),
                            }
                        }
                        TrustRequest::Persist {
                            generation,
                            path,
                            database,
                        } => TrustEvent::Persisted {
                            generation,
                            result: persist_trust_database(&path, &database),
                        },
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
            next_generation: 0,
        }
    }

    fn next_generation(&mut self) -> Result<u64, TrustStateError> {
        self.next_generation =
            self.next_generation
                .checked_add(1)
                .ok_or_else(|| TrustStateError::Invalid {
                    path: PathBuf::new(),
                    reason: "trust request generation exhausted".into(),
                })?;
        Ok(self.next_generation)
    }

    pub fn load(&mut self, path: PathBuf) -> Result<u64, TrustStateError> {
        let generation = self.next_generation()?;
        self.sender
            .try_send(TrustRequest::Load { generation, path })
            .map_err(map_trust_send_error)?;
        Ok(generation)
    }

    pub fn inspect_root(&mut self, path: PathBuf) -> Result<u64, TrustStateError> {
        let generation = self.next_generation()?;
        self.sender
            .try_send(TrustRequest::InspectRoot { generation, path })
            .map_err(map_trust_send_error)?;
        Ok(generation)
    }

    pub fn persist(
        &mut self,
        path: PathBuf,
        identities: Vec<RootIdentity>,
        seen: Vec<RootIdentity>,
    ) -> Result<u64, TrustStateError> {
        let generation = self.next_generation()?;
        self.sender
            .try_send(TrustRequest::Persist {
                generation,
                path,
                database: TrustDatabase {
                    version: TRUST_STATE_VERSION,
                    identities,
                    seen,
                },
            })
            .map_err(map_trust_send_error)?;
        Ok(generation)
    }

    pub fn try_event(&self) -> Result<Option<TrustEvent>, TrustStateError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
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
mod generation_tests {
    use super::*;
    #[test]
    fn exhausted_generation_never_reuses_an_admitted_identity() {
        let mut service = WorkspaceTrustService::start(1);
        service.next_generation = u64::MAX;
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
