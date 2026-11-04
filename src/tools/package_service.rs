use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

use sha2::{Digest, Sha256};

use crate::extensions::{PluginLimits, PluginManifest, activate_candidate};

use super::{
    package_lock::{
        LockedPackage, PackageError, PackageLock, PackageManifest, PackageSource,
        commit_store_bytes, read_bounded_regular, valid_digest, valid_source, verify_lock_store,
        write_lock_atomic, write_protected_atomic,
    },
    registry::RegistryPackageMetadata,
};

pub struct PackageManager {
    root: PathBuf,
    maximum_lock_bytes: usize,
    maximum_object_bytes: usize,
}
impl PackageManager {
    pub fn new(
        root: PathBuf,
        maximum_lock_bytes: usize,
        maximum_object_bytes: usize,
    ) -> Result<Self, PackageError> {
        if !root.is_absolute() || maximum_lock_bytes == 0 || maximum_object_bytes == 0 {
            return Err(PackageError::InvalidManifest);
        }
        fs::create_dir_all(root.join("store")).map_err(PackageError::Io)?;
        fs::create_dir_all(root.join("activations")).map_err(PackageError::Io)?;
        Ok(Self {
            root,
            maximum_lock_bytes,
            maximum_object_bytes,
        })
    }
    fn transaction(&self) -> Result<impl Drop, PackageError> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("dust-packages.lock"))
            .map_err(PackageError::Io)?;
        #[cfg(unix)]
        {
            nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
                .map_err(|(_, error)| PackageError::Io(error.into()))
        }
        #[cfg(not(unix))]
        {
            Ok(file)
        }
    }
    fn lock_path(&self) -> PathBuf {
        self.root.join("dust-packages.lock.json")
    }
    fn backup_path(&self) -> PathBuf {
        self.root.join("dust-packages.lock.previous.json")
    }
    pub fn read_lock(&self) -> Result<PackageLock, PackageError> {
        let path = self.lock_path();
        if !path.exists() {
            return Ok(PackageLock {
                version: 1,
                packages: Vec::new(),
                reproducible: true,
            });
        }
        let metadata = fs::metadata(&path).map_err(PackageError::Io)?;
        if metadata.len() > self.maximum_lock_bytes as u64 {
            return Err(PackageError::LockTooLarge);
        }
        let lock: PackageLock = serde_json::from_slice(&fs::read(path).map_err(PackageError::Io)?)
            .map_err(PackageError::Json)?;
        if lock.version != 1 {
            return Err(PackageError::InvalidManifest);
        }
        Ok(lock)
    }
    fn backup(&self) -> Result<(), PackageError> {
        let lock = self.lock_path();
        if lock.exists() {
            let bytes = fs::read(lock).map_err(PackageError::Io)?;
            let backup = self.backup_path();
            let _ = fs::remove_file(&backup);
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(backup).map_err(PackageError::Io)?;
            file.write_all(&bytes).map_err(PackageError::Io)?;
            file.sync_all().map_err(PackageError::Io)?;
        }
        Ok(())
    }
    pub fn stage_verified_object(
        &self,
        manifest: &PackageManifest,
        component_bytes: &[u8],
    ) -> Result<PathBuf, PackageError> {
        if !valid_source(&manifest.source) || matches!(manifest.source, PackageSource::Path { .. })
        {
            return Err(PackageError::NonReproducibleSource);
        }
        let expected = match &manifest.source {
            PackageSource::Registry { archive_sha256, .. } => archive_sha256,
            PackageSource::Git { tree_sha256, .. } => tree_sha256,
            PackageSource::Path { .. } => unreachable!(),
        };
        let object = commit_store_bytes(
            &self.root.join("store"),
            expected,
            component_bytes,
            self.maximum_object_bytes,
        )?;
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        let engine = wasmtime::Engine::new(&config).map_err(|_| PackageError::InvalidManifest)?;
        wasmtime::component::Component::from_file(&engine, &object)
            .map_err(|_| PackageError::InvalidManifest)?;
        Ok(object)
    }
    pub fn store_root(&self) -> PathBuf {
        self.root.join("store")
    }
    pub fn commit_prepared(
        &self,
        manifest: &PackageManifest,
        plugin: &PluginManifest,
        object: &Path,
    ) -> Result<PackageLock, PackageError> {
        let _transaction = self.transaction()?;
        let expected = manifest.source_digest().to_ascii_lowercase();
        if !valid_digest(&expected)
            || object.file_name().and_then(|value| value.to_str()) != Some(expected.as_str())
        {
            return Err(PackageError::ChecksumMismatch);
        }
        let bytes = read_bounded_regular(object, self.maximum_object_bytes as u64)?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if !actual.eq_ignore_ascii_case(&expected)
            || !plugin.component_sha256.eq_ignore_ascii_case(&expected)
        {
            return Err(PackageError::ChecksumMismatch);
        }
        let activation_bytes =
            toml::to_string(plugin).map_err(|_| PackageError::InvalidManifest)?;
        let activation_sha256 = format!("{:x}", Sha256::digest(activation_bytes.as_bytes()));
        let activation = self
            .root
            .join("activations")
            .join(format!("{activation_sha256}.toml"));
        write_protected_atomic(&activation, activation_bytes.as_bytes(), 1024 * 1024)?;
        let mut lock = self.read_lock()?;
        self.backup()?;
        lock.packages
            .retain(|package| package.name != manifest.name);
        let mut source = manifest.source.clone();
        match &mut source {
            PackageSource::Registry { archive_sha256, .. } => *archive_sha256 = expected.clone(),
            PackageSource::Git { tree_sha256, .. } => *tree_sha256 = expected.clone(),
            PackageSource::Path { .. } => {}
        }
        lock.packages.push(LockedPackage {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            source,
            content_sha256: expected,
            activation_sha256: Some(activation_sha256),
            dependencies: manifest
                .dependencies
                .iter()
                .map(|dependency| dependency.name.clone())
                .collect(),
        });
        lock.packages.sort_by(|a, b| a.name.cmp(&b.name));
        lock.reproducible = true;
        write_lock_atomic(&self.lock_path(), &lock, self.maximum_lock_bytes)?;
        Ok(lock)
    }
    fn activations_for_lock(
        &self,
        lock: &PackageLock,
    ) -> Result<Vec<(PluginManifest, PathBuf)>, PackageError> {
        let mut output = Vec::new();
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        let engine = wasmtime::Engine::new(&config).map_err(|_| PackageError::InvalidManifest)?;
        for package in &lock.packages {
            if !valid_digest(&package.content_sha256)
                || package
                    .activation_sha256
                    .as_ref()
                    .is_some_and(|digest| !valid_digest(digest))
            {
                return Err(PackageError::InvalidManifest);
            }
            let object = self.root.join("store").join(&package.content_sha256);
            let manifest_path = self.root.join("activations").join(format!(
                "{}.toml",
                package
                    .activation_sha256
                    .as_ref()
                    .unwrap_or(&package.content_sha256)
            ));
            let bytes = read_bounded_regular(&manifest_path, 1024 * 1024)?;
            if let Some(expected) = &package.activation_sha256
                && format!("{:x}", Sha256::digest(&bytes)) != *expected
            {
                return Err(PackageError::ChecksumMismatch);
            }
            let plugin: PluginManifest = toml::from_str(
                std::str::from_utf8(&bytes).map_err(|_| PackageError::InvalidManifest)?,
            )
            .map_err(|_| PackageError::InvalidManifest)?;
            if !plugin
                .component_sha256
                .eq_ignore_ascii_case(&package.content_sha256)
            {
                return Err(PackageError::ChecksumMismatch);
            }
            let component = read_bounded_regular(&object, self.maximum_object_bytes as u64)?;
            let actual = format!("{:x}", Sha256::digest(&component));
            if !actual.eq_ignore_ascii_case(&package.content_sha256) {
                return Err(PackageError::ChecksumMismatch);
            }
            wasmtime::component::Component::from_binary(&engine, &component)
                .map_err(|_| PackageError::InvalidManifest)?;
            output.push((plugin, object));
        }
        Ok(output)
    }
    pub fn activations(&self) -> Result<Vec<(PluginManifest, PathBuf)>, PackageError> {
        let _transaction = self.transaction()?;
        let lock = self.read_lock()?;
        self.activations_for_lock(&lock)
    }
    pub fn remove(&self, name: &str) -> Result<PackageLock, PackageError> {
        let _transaction = self.transaction()?;
        let mut lock = self.read_lock()?;
        if lock.packages.iter().any(|package| {
            package
                .dependencies
                .iter()
                .any(|dependency| dependency == name)
        }) {
            return Err(PackageError::PackageRequired(name.into()));
        }
        self.backup()?;
        lock.packages.retain(|package| package.name != name);
        write_lock_atomic(&self.lock_path(), &lock, self.maximum_lock_bytes)?;
        Ok(lock)
    }
    pub fn rollback(&self) -> Result<PackageLock, PackageError> {
        let _transaction = self.transaction()?;
        let backup = self.backup_path();
        if !backup.exists() {
            return Err(PackageError::NoRollback);
        }
        let bytes = read_bounded_regular(&backup, self.maximum_lock_bytes as u64)?;
        let lock: PackageLock = serde_json::from_slice(&bytes).map_err(PackageError::Json)?;
        if lock.version != 1 {
            return Err(PackageError::InvalidManifest);
        }
        self.activations_for_lock(&lock)?;
        let current = self.read_lock()?;
        write_lock_atomic(&self.backup_path(), &current, self.maximum_lock_bytes)?;
        write_lock_atomic(&self.lock_path(), &lock, self.maximum_lock_bytes)?;
        Ok(lock)
    }
    pub fn gc(&self, maximum_removals: usize) -> Result<usize, PackageError> {
        let _transaction = self.transaction()?;
        let mut locks = vec![self.read_lock()?];
        if self.backup_path().exists() {
            let bytes = read_bounded_regular(&self.backup_path(), self.maximum_lock_bytes as u64)?;
            locks.push(serde_json::from_slice::<PackageLock>(&bytes).map_err(PackageError::Json)?);
        }
        let mut retained = BTreeSet::new();
        let mut retained_activations = BTreeSet::new();
        for package in locks.into_iter().flat_map(|lock| lock.packages) {
            retained_activations.insert(
                package
                    .activation_sha256
                    .unwrap_or_else(|| package.content_sha256.clone()),
            );
            retained.insert(package.content_sha256);
        }
        let mut removed = 0;
        for entry in fs::read_dir(self.root.join("store"))
            .map_err(PackageError::Io)?
            .take(maximum_removals.saturating_mul(4).max(1))
        {
            let entry = entry.map_err(PackageError::Io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = entry.metadata().map_err(PackageError::Io)?;
            if metadata.is_file()
                && valid_digest(&name)
                && !retained.contains(&name)
                && removed < maximum_removals
            {
                #[cfg(unix)]
                let _object_lock = {
                    let file = fs::File::open(entry.path()).map_err(PackageError::Io)?;
                    match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
                    {
                        Ok(lock) => lock,
                        Err((_, nix::errno::Errno::EAGAIN)) => continue,
                        Err((_, error)) => return Err(PackageError::Io(error.into())),
                    }
                };
                fs::remove_file(entry.path()).map_err(PackageError::Io)?;
                removed += 1
            }
        }
        if removed < maximum_removals {
            for entry in fs::read_dir(self.root.join("activations"))
                .map_err(PackageError::Io)?
                .take(maximum_removals.saturating_mul(4).max(1))
            {
                let entry = entry.map_err(PackageError::Io)?;
                let path = entry.path();
                let stem = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                let metadata = entry.metadata().map_err(PackageError::Io)?;
                if metadata.is_file()
                    && valid_digest(stem)
                    && !retained_activations.contains(stem)
                    && removed < maximum_removals
                {
                    fs::remove_file(path).map_err(PackageError::Io)?;
                    removed += 1
                }
            }
        }
        Ok(removed)
    }
    pub fn verify(&self) -> Result<usize, PackageError> {
        // No lock file means nothing was ever installed.
        if !self.lock_path().exists() {
            return Ok(0);
        }
        verify_lock_store(
            &self.lock_path(),
            &self.root.join("store"),
            self.maximum_lock_bytes,
        )
    }
}

#[derive(Clone, Debug)]
pub enum PackageOperation {
    Install {
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        roots: Vec<PathBuf>,
        limits: PluginLimits,
        update: bool,
    },
    PrepareRegistry {
        metadata: Box<RegistryPackageMetadata>,
        component: Vec<u8>,
        limits: PluginLimits,
    },
    Commit {
        token: u64,
    },
    Abort {
        token: u64,
    },
    Activations,
    List,
    Verify,
    Rollback,
    Remove(String),
    GarbageCollect {
        maximum: usize,
    },
}
#[derive(Clone, Debug)]
pub struct PreparedPackage {
    pub token: u64,
    pub manifest: PackageManifest,
    pub plugin: PluginManifest,
    pub object: PathBuf,
}
#[derive(Debug)]
pub(super) struct PreparedInstall {
    manifest: PackageManifest,
    plugin: PluginManifest,
    object: PathBuf,
    #[cfg(unix)]
    _object_lock: nix::fcntl::Flock<fs::File>,
}

#[cfg(unix)]
fn protect_object(object: &Path) -> Result<nix::fcntl::Flock<fs::File>, PackageError> {
    let file = fs::File::open(object).map_err(PackageError::Io)?;
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockShared)
        .map_err(|(_, error)| PackageError::Io(error.into()))
}
#[derive(Clone, Debug)]
pub struct PackageOperationResult {
    pub id: u64,
    pub operation: String,
    pub lock: Option<PackageLock>,
    pub verified: Option<usize>,
    pub removed: Option<usize>,
    pub prepared: Option<PreparedPackage>,
    pub activations: Vec<(PluginManifest, PathBuf)>,
}
#[derive(Debug)]
pub enum PackageServiceEvent {
    Completed(Box<PackageOperationResult>),
    Failed {
        id: u64,
        operation: String,
        error: PackageError,
    },
}
pub struct PackageService {
    sender: SyncSender<(u64, PackageOperation)>,
    receiver: Receiver<PackageServiceEvent>,
    ids: Arc<dyn crate::clock::IdSource>,
    pending: BTreeSet<u64>,
}
impl PackageService {
    pub fn start(
        root: PathBuf,
        capacity: usize,
        maximum_lock_bytes: usize,
        maximum_object_bytes: usize,
    ) -> Self {
        Self::start_with_ids(
            root,
            capacity,
            maximum_lock_bytes,
            maximum_object_bytes,
            Arc::new(crate::clock::AtomicIds::new(1)),
        )
    }
    pub fn start_with_ids(
        root: PathBuf,
        capacity: usize,
        maximum_lock_bytes: usize,
        maximum_object_bytes: usize,
        ids: Arc<dyn crate::clock::IdSource>,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<(u64, PackageOperation)>(capacity.clamp(1, 64));
        let (etx, erx) = mpsc::sync_channel(capacity.clamp(1, 64));
        thread::Builder::new()
            .name("dust-packages".into())
            .spawn(move || {
                let mut prepared = BTreeMap::<u64, PreparedInstall>::new();
                while let Ok((id, operation)) = rx.recv() {
                    let name = match &operation {
                        PackageOperation::Install { update, .. } => {
                            if *update {
                                "update"
                            } else {
                                "install"
                            }
                        }
                        PackageOperation::PrepareRegistry { .. } => "registry_prepare",
                        PackageOperation::Commit { .. } => "commit",
                        PackageOperation::Abort { .. } => "abort",
                        PackageOperation::Activations => "activations",
                        PackageOperation::List => "list",
                        PackageOperation::Verify => "verify",
                        PackageOperation::Rollback => "rollback",
                        PackageOperation::Remove(_) => "remove",
                        PackageOperation::GarbageCollect { .. } => "gc",
                    }
                    .to_owned();
                    let result = (|| {
                        let manager = PackageManager::new(
                            root.clone(),
                            maximum_lock_bytes,
                            maximum_object_bytes,
                        )?;
                        let mut output = PackageOperationResult {
                            id,
                            operation: name.clone(),
                            lock: None,
                            verified: None,
                            removed: None,
                            prepared: None,
                            activations: Vec::new(),
                        };
                        match operation {
                            PackageOperation::Install {
                                package_manifest,
                                plugin_manifest,
                                component,
                                roots,
                                limits,
                                ..
                            } => {
                                let _transaction = manager.transaction()?;
                                let package_manifest =
                                    fs::canonicalize(package_manifest).map_err(PackageError::Io)?;
                                let plugin_manifest =
                                    fs::canonicalize(plugin_manifest).map_err(PackageError::Io)?;
                                let component =
                                    fs::canonicalize(component).map_err(PackageError::Io)?;
                                if [&package_manifest, &plugin_manifest, &component]
                                    .into_iter()
                                    .any(|path| !roots.iter().any(|root| path.starts_with(root)))
                                {
                                    return Err(PackageError::OutsideScope);
                                }
                                let package_bytes =
                                    read_bounded_regular(&package_manifest, 1024 * 1024)?;
                                let plugin_bytes =
                                    read_bounded_regular(&plugin_manifest, 1024 * 1024)?;
                                let component_bytes =
                                    read_bounded_regular(&component, maximum_object_bytes as u64)?;
                                let package: PackageManifest =
                                    serde_json::from_slice(&package_bytes)
                                        .map_err(PackageError::Json)?;
                                let plugin: PluginManifest = toml::from_str(
                                    std::str::from_utf8(&plugin_bytes)
                                        .map_err(|_| PackageError::InvalidManifest)?,
                                )
                                .map_err(|_| PackageError::InvalidManifest)?;
                                let object =
                                    manager.stage_verified_object(&package, &component_bytes)?;
                                if !plugin
                                    .component_sha256
                                    .eq_ignore_ascii_case(package.source_digest())
                                {
                                    return Err(PackageError::ChecksumMismatch);
                                }
                                activate_candidate(
                                    &manager.store_root(),
                                    &object,
                                    plugin.clone(),
                                    &limits,
                                )
                                .map_err(|error| PackageError::Activation(error.to_string()))?;
                                prepared.insert(
                                    id,
                                    PreparedInstall {
                                        manifest: package.clone(),
                                        plugin: plugin.clone(),
                                        object: object.clone(),
                                        #[cfg(unix)]
                                        _object_lock: protect_object(&object)?,
                                    },
                                );
                                output.prepared = Some(PreparedPackage {
                                    token: id,
                                    manifest: package,
                                    plugin,
                                    object,
                                });
                            }
                            PackageOperation::PrepareRegistry {
                                metadata,
                                component,
                                limits,
                            } => {
                                let _transaction = manager.transaction()?;
                                if metadata.package.name
                                    != format!("{}/{}", metadata.publisher, metadata.name)
                                    || !metadata
                                        .artifact_sha256
                                        .eq_ignore_ascii_case(metadata.package.source_digest())
                                    || !metadata
                                        .plugin
                                        .component_sha256
                                        .eq_ignore_ascii_case(&metadata.artifact_sha256)
                                {
                                    return Err(PackageError::InvalidManifest);
                                }
                                let object =
                                    manager.stage_verified_object(&metadata.package, &component)?;
                                activate_candidate(
                                    &manager.store_root(),
                                    &object,
                                    metadata.plugin.clone(),
                                    &limits,
                                )
                                .map_err(|error| PackageError::Activation(error.to_string()))?;
                                prepared.insert(
                                    id,
                                    PreparedInstall {
                                        manifest: metadata.package.clone(),
                                        plugin: metadata.plugin.clone(),
                                        object: object.clone(),
                                        #[cfg(unix)]
                                        _object_lock: protect_object(&object)?,
                                    },
                                );
                                output.prepared = Some(PreparedPackage {
                                    token: id,
                                    manifest: metadata.package,
                                    plugin: metadata.plugin,
                                    object,
                                });
                            }
                            PackageOperation::Commit { token } => {
                                let candidate = prepared
                                    .remove(&token)
                                    .ok_or(PackageError::MissingPrepared)?;
                                output.lock = Some(manager.commit_prepared(
                                    &candidate.manifest,
                                    &candidate.plugin,
                                    &candidate.object,
                                )?);
                                output.removed = Some(manager.gc(256)?);
                            }
                            PackageOperation::Abort { token } => {
                                prepared
                                    .remove(&token)
                                    .ok_or(PackageError::MissingPrepared)?;
                                output.removed = Some(manager.gc(256)?);
                            }
                            PackageOperation::Activations => {
                                output.activations = manager.activations()?
                            }
                            PackageOperation::List => output.lock = Some(manager.read_lock()?),
                            PackageOperation::Verify => output.verified = Some(manager.verify()?),
                            PackageOperation::Rollback => output.lock = Some(manager.rollback()?),
                            PackageOperation::Remove(package) => {
                                output.lock = Some(manager.remove(&package)?);
                                output.removed = Some(manager.gc(256)?);
                            }
                            PackageOperation::GarbageCollect { maximum } => {
                                output.removed = Some(manager.gc(maximum.min(4096))?)
                            }
                        }
                        Ok::<_, PackageError>(output)
                    })();
                    let event = result.map_or_else(
                        |error| PackageServiceEvent::Failed {
                            id,
                            operation: name,
                            error,
                        },
                        |result| PackageServiceEvent::Completed(Box::new(result)),
                    );
                    if etx.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("package service");
        Self {
            sender: tx,
            receiver: erx,
            ids,
            pending: BTreeSet::new(),
        }
    }
    pub fn request(&mut self, operation: PackageOperation) -> Result<u64, PackageError> {
        let id = self.ids.next();
        self.sender
            .try_send((id, operation))
            .map_err(|error| match error {
                TrySendError::Full(_) => PackageError::QueueFull,
                TrySendError::Disconnected(_) => PackageError::ServiceStopped,
            })?;
        self.pending.insert(id);
        Ok(id)
    }
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn poll(&mut self) -> Result<Vec<PackageServiceEvent>, PackageError> {
        let mut events = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    let id = match &event {
                        PackageServiceEvent::Completed(result) => result.id,
                        PackageServiceEvent::Failed { id, .. } => *id,
                    };
                    self.pending.remove(&id);
                    events.push(event)
                }
                Err(TryRecvError::Empty) => return Ok(events),
                Err(TryRecvError::Disconnected) => return Err(PackageError::ServiceStopped),
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn gc_preserves_outstanding_prepared_objects_across_managers() {
        let root = tempfile::tempdir().unwrap();
        let manager = PackageManager::new(root.path().into(), 1024 * 1024, 1024).unwrap();
        let (manifest, plugin, bytes) = super::super::tests::package_fixture("prepared");
        let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
        let protected = protect_object(&object).unwrap();
        let other = PackageManager::new(root.path().into(), 1024 * 1024, 1024).unwrap();
        assert_eq!(other.gc(256).unwrap(), 0);
        assert!(object.exists());
        manager
            .commit_prepared(&manifest, &plugin, &object)
            .unwrap();
        drop(protected);
        assert_eq!(other.gc(256).unwrap(), 0);
        assert!(object.exists());
    }
}
