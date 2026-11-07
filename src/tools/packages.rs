//! Package storage, lock updates and background operations.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;

use sha2::Digest;

use sha2::Sha256;

use crate::plugins::PluginLimits;

use crate::plugins::PluginManifest;

use crate::plugins::activate_candidate;

use crate::tools::package_lock::LockedPackage;

use crate::tools::package_lock::PackageError;

use crate::tools::package_lock::PackageLock;

use crate::tools::package_lock::PackageManifest;

use crate::tools::package_lock::PackageSource;

use crate::tools::package_lock::commit_store_bytes;

use crate::tools::package_lock::read_bounded_regular;

use crate::tools::package_lock::valid_digest;

use crate::tools::package_lock::valid_source;

use crate::tools::package_lock::verify_lock_store;

use crate::tools::package_lock::write_lock_atomic;

use crate::tools::package_lock::write_protected_atomic;

use crate::tools::registry::RegistryPackageMetadata;

pub struct PackageStore {
    root: PathBuf,
    maximum_lock_bytes: usize,
    maximum_object_bytes: usize,
}
impl PackageStore {
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
    fn replace_lock(&self, lock: &PackageLock) -> Result<(), PackageError> {
        // Finish serialization and staging before changing rollback history.
        let staged = tempfile::NamedTempFile::new_in(&self.root).map_err(PackageError::Io)?;
        write_lock_atomic(staged.path(), lock, self.maximum_lock_bytes)?;
        let backup = self.backup_path();
        let previous = if backup.exists() {
            Some(read_bounded_regular(
                &backup,
                self.maximum_lock_bytes as u64,
            )?)
        } else {
            None
        };
        let current = if self.lock_path().exists() {
            Some(read_bounded_regular(
                &self.lock_path(),
                self.maximum_lock_bytes as u64,
            )?)
        } else {
            None
        };
        if let Some(bytes) = &current {
            write_protected_atomic(&backup, bytes, self.maximum_lock_bytes)?;
        }
        if let Err(error) = staged.persist(self.lock_path()) {
            if current.is_some() {
                if let Some(bytes) = previous {
                    write_protected_atomic(&backup, &bytes, self.maximum_lock_bytes)?;
                } else {
                    fs::remove_file(&backup).map_err(PackageError::Io)?;
                }
            }
            return Err(PackageError::Io(error.error));
        }
        fs::File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(PackageError::Io)
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
        // This compilation only validates the object; the plugin host compiles it for execution.
        config
            .wasm_component_model(true)
            .cranelift_opt_level(wasmtime::OptLevel::None);
        let engine = wasmtime::Engine::new(&config).map_err(|_| PackageError::InvalidManifest)?;
        wasmtime::component::Component::from_file(&engine, &object)
            .map_err(|_| PackageError::InvalidManifest)?;
        Ok(object)
    }
    pub fn store_root(&self) -> PathBuf {
        self.root.join("store")
    }
    fn check_plugin_owner(
        &self,
        manifest: &PackageManifest,
        plugin: &PluginManifest,
    ) -> Result<(), PackageError> {
        for package in self.read_lock()?.packages {
            let installed = self.activation_manifest(&package)?;
            if package.name == manifest.name && installed.id != plugin.id {
                return Err(PackageError::Activation(format!(
                    "package {} cannot change plugin ID from {} to {}; plugin {} belongs to package {}",
                    manifest.name, installed.id, plugin.id, installed.id, package.name
                )));
            }
            if package.name != manifest.name && installed.id == plugin.id {
                return Err(PackageError::Activation(format!(
                    "plugin {} is already owned by package {}; plugin {} belongs to package {}",
                    installed.id, package.name, installed.id, package.name
                )));
            }
        }
        Ok(())
    }
    pub fn commit_prepared(
        &self,
        manifest: &PackageManifest,
        plugin: &PluginManifest,
        object: &Path,
    ) -> Result<PackageLock, PackageError> {
        let _transaction = self.transaction()?;
        self.check_plugin_owner(manifest, plugin)?;
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
        self.replace_lock(&lock)?;
        Ok(lock)
    }
    fn activation_manifest(&self, package: &LockedPackage) -> Result<PluginManifest, PackageError> {
        if !valid_digest(&package.content_sha256)
            || package
                .activation_sha256
                .as_ref()
                .is_some_and(|digest| !valid_digest(digest))
        {
            return Err(PackageError::InvalidManifest);
        }
        let path = self.root.join("activations").join(format!(
            "{}.toml",
            package
                .activation_sha256
                .as_ref()
                .unwrap_or(&package.content_sha256)
        ));
        let bytes = read_bounded_regular(&path, 1024 * 1024)?;
        if let Some(expected) = &package.activation_sha256
            && format!("{:x}", Sha256::digest(&bytes)) != *expected
        {
            return Err(PackageError::ChecksumMismatch);
        }
        let plugin: PluginManifest =
            toml::from_str(std::str::from_utf8(&bytes).map_err(|_| PackageError::InvalidManifest)?)
                .map_err(|_| PackageError::InvalidManifest)?;
        if !plugin
            .component_sha256
            .eq_ignore_ascii_case(&package.content_sha256)
        {
            return Err(PackageError::ChecksumMismatch);
        }
        Ok(plugin)
    }
    fn activations_for_lock(
        &self,
        lock: &PackageLock,
    ) -> Result<Vec<(PluginManifest, PathBuf, Arc<fs::File>)>, PackageError> {
        let mut output = Vec::new();
        let mut config = wasmtime::Config::new();
        // Startup checks discard compiled code too; optimization belongs to the runtime loader.
        config
            .wasm_component_model(true)
            .cranelift_opt_level(wasmtime::OptLevel::None);
        let engine = wasmtime::Engine::new(&config).map_err(|_| PackageError::InvalidManifest)?;
        for package in &lock.packages {
            let plugin = self.activation_manifest(package)?;
            let object = self.root.join("store").join(&package.content_sha256);
            let component = read_bounded_regular(&object, self.maximum_object_bytes as u64)?;
            let object_lock = protect_object(&object)?;
            let actual = format!("{:x}", Sha256::digest(&component));
            if !actual.eq_ignore_ascii_case(&package.content_sha256) {
                return Err(PackageError::ChecksumMismatch);
            }
            wasmtime::component::Component::from_binary(&engine, &component)
                .map_err(|_| PackageError::InvalidManifest)?;
            output.push((plugin, object, object_lock));
        }
        Ok(output)
    }
    pub fn activations(
        &self,
    ) -> Result<Vec<(PluginManifest, PathBuf, Arc<fs::File>)>, PackageError> {
        let _transaction = self.transaction()?;
        let lock = self.read_lock()?;
        self.activations_for_lock(&lock)
    }
    pub fn remove(&self, name: &str) -> Result<PackageLock, PackageError> {
        let _transaction = self.transaction()?;
        let mut lock = self.read_lock()?;
        if !lock.packages.iter().any(|package| package.name == name) {
            return Err(PackageError::PackageNotFound(name.into()));
        }
        if lock.packages.iter().any(|package| {
            package.name != name
                && package
                    .dependencies
                    .iter()
                    .any(|dependency| dependency == name)
        }) {
            return Err(PackageError::PackageRequired(name.into()));
        }
        lock.packages.retain(|package| package.name != name);
        self.replace_lock(&lock)?;
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
        self.replace_lock(&lock)?;
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
        for entry in fs::read_dir(self.root.join("store")).map_err(PackageError::Io)? {
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
            for entry in fs::read_dir(self.root.join("activations")).map_err(PackageError::Io)? {
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
            self.maximum_object_bytes,
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
    _object_lock: Arc<fs::File>,
}

fn protect_object(object: &Path) -> Result<Arc<fs::File>, PackageError> {
    let file = fs::File::open(object).map_err(PackageError::Io)?;
    #[cfg(unix)]
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockShared)
        .map_err(|error| PackageError::Io(error.into()))?;
    Ok(Arc::new(file))
}
#[derive(Clone, Debug)]
pub struct PackageOperationResult {
    pub id: u64,
    pub operation: String,
    pub lock: Option<PackageLock>,
    pub verified: Option<usize>,
    pub removed: Option<usize>,
    pub prepared: Option<PreparedPackage>,
    pub activations: Vec<(PluginManifest, PathBuf, Arc<fs::File>)>,
    pub cleanup_error: Option<String>,
}
impl PackageOperationResult {
    fn collect_garbage(&mut self, manager: &PackageStore) {
        match manager.gc(256) {
            Ok(removed) => self.removed = Some(removed),
            Err(error) => self.cleanup_error = Some(error.to_string()),
        }
    }
}
#[derive(Debug)]
pub enum PackageEvent {
    Completed(Box<PackageOperationResult>),
    Failed {
        id: u64,
        operation: String,
        error: PackageError,
    },
}
pub struct PackageWorker {
    sender: SyncSender<(u64, PackageOperation)>,
    receiver: Receiver<PackageEvent>,
    ids: Arc<dyn crate::clock::IdSource>,
    pending: BTreeSet<u64>,
}
impl PackageWorker {
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
                        let manager = PackageStore::new(
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
                            cleanup_error: None,
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
                                if !prepared.is_empty() {
                                    return Err(PackageError::Busy);
                                }
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
                                manager.check_plugin_owner(&package, &plugin)?;
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
                                if !prepared.is_empty() {
                                    return Err(PackageError::Busy);
                                }
                                manager.check_plugin_owner(&metadata.package, &metadata.plugin)?;
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
                                output.collect_garbage(&manager);
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
                            PackageOperation::Rollback => {
                                if !prepared.is_empty() {
                                    return Err(PackageError::Busy);
                                }
                                output.lock = Some(manager.rollback()?);
                            }
                            PackageOperation::Remove(package) => {
                                if !prepared.is_empty() {
                                    return Err(PackageError::Busy);
                                }
                                output.lock = Some(manager.remove(&package)?);
                                output.collect_garbage(&manager);
                            }
                            PackageOperation::GarbageCollect { maximum } => {
                                output.removed = Some(manager.gc(maximum.min(4096))?)
                            }
                        }
                        Ok::<_, PackageError>(output)
                    })();
                    let event = result.map_or_else(
                        |error| PackageEvent::Failed {
                            id,
                            operation: name,
                            error,
                        },
                        |result| PackageEvent::Completed(Box::new(result)),
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
    pub fn poll(&mut self) -> Result<Vec<PackageEvent>, PackageError> {
        let mut events = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    let id = match &event {
                        PackageEvent::Completed(result) => result.id,
                        PackageEvent::Failed { id, .. } => *id,
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
    use crate::tools::packages::*;

    #[test]
    fn gc_reaches_garbage_after_a_retained_directory_prefix() {
        for directory in ["store", "activations"] {
            let root = tempfile::tempdir().unwrap();
            let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
            for index in 0..12 {
                let name = format!("{index:064x}");
                let name = if directory == "activations" {
                    format!("{name}.toml")
                } else {
                    name
                };
                fs::write(root.path().join(directory).join(name), "unused").unwrap();
            }
            let entries = fs::read_dir(root.path().join(directory))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            let mut lock = manager.read_lock().unwrap();
            for path in &entries[..entries.len() - 1] {
                let digest = path.file_stem().unwrap().to_str().unwrap().to_owned();
                lock.packages.push(LockedPackage {
                    name: digest.clone(),
                    version: "1".into(),
                    source: crate::tools::tests::package_fixture("unused").0.source,
                    content_sha256: digest.clone(),
                    activation_sha256: Some(digest),
                    dependencies: Vec::new(),
                });
            }
            write_lock_atomic(&manager.lock_path(), &lock, 1024 * 1024).unwrap();
            assert_eq!(manager.gc(1).unwrap(), 1, "{directory}");
            assert!(!entries.last().unwrap().exists(), "{directory}");
            assert!(
                entries[..entries.len() - 1]
                    .iter()
                    .all(|path| path.exists())
            );
        }
    }

    #[test]
    fn package_cleanup_failure_preserves_the_committed_result() {
        let root = tempfile::tempdir().unwrap();
        let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
        let (manifest, plugin, bytes) = crate::tools::tests::package_fixture("demo");
        let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
        manager
            .commit_prepared(&manifest, &plugin, &object)
            .unwrap();
        let lock = manager.remove("demo").unwrap();
        // An unrelated cleanup failure must not undo or hide the removal.
        fs::remove_dir_all(manager.store_root()).unwrap();
        fs::write(manager.store_root(), "not a directory").unwrap();
        let mut result = PackageOperationResult {
            id: 1,
            operation: "remove".into(),
            lock: Some(lock),
            verified: None,
            removed: None,
            prepared: None,
            activations: Vec::new(),
            cleanup_error: None,
        };
        result.collect_garbage(&manager);
        assert!(result.cleanup_error.is_some());
        assert!(result.lock.unwrap().packages.is_empty());
        assert!(manager.read_lock().unwrap().packages.is_empty());
    }

    #[test]
    fn preparation_rejects_plugin_id_conflicts_before_loading() {
        use std::time::Duration;
        use std::time::Instant;
        let root = tempfile::tempdir().unwrap();
        let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
        let (installed, plugin, bytes) = crate::tools::tests::package_fixture("publisher/alpha");
        let object = manager.stage_verified_object(&installed, &bytes).unwrap();
        manager
            .commit_prepared(&installed, &plugin, &object)
            .unwrap();
        let mut service = PackageWorker::start(root.path().into(), 1, 1024 * 1024, 1024);
        for registry in [false, true] {
            for (name, plugin_id, message) in [
                ("alpha", "renamed", "cannot change plugin ID"),
                ("beta", "publisher/alpha", "already owned"),
            ] {
                let (package, mut plugin, component) =
                    crate::tools::tests::package_fixture(&format!("publisher/{name}"));
                plugin.id = plugin_id.into();
                let operation = if registry {
                    PackageOperation::PrepareRegistry {
                        metadata: Box::new(RegistryPackageMetadata {
                            schema: 1,
                            publisher: "publisher".into(),
                            name: name.into(),
                            version: "1.0.0".into(),
                            yanked: false,
                            artifact_path: "plugin.wasm".into(),
                            artifact_sha256: plugin.component_sha256.clone(),
                            package,
                            plugin,
                        }),
                        component,
                        limits: PluginLimits::default(),
                    }
                } else {
                    let package_manifest = root.path().join("package.json");
                    let plugin_manifest = root.path().join("plugin.toml");
                    let component_path = root.path().join("plugin.wasm");
                    fs::write(&package_manifest, serde_json::to_vec(&package).unwrap()).unwrap();
                    fs::write(&plugin_manifest, toml::to_string(&plugin).unwrap()).unwrap();
                    fs::write(&component_path, component).unwrap();
                    PackageOperation::Install {
                        package_manifest,
                        plugin_manifest,
                        component: component_path,
                        roots: vec![root.path().canonicalize().unwrap()],
                        limits: PluginLimits::default(),
                        update: true,
                    }
                };
                let request = service.request(operation).unwrap();
                let deadline = Instant::now() + Duration::from_secs(3);
                let event = loop {
                    if let Some(event) = service.poll().unwrap().into_iter().next() {
                        break event;
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(2));
                };
                match event {
                    PackageEvent::Failed {
                        id,
                        error: PackageError::Activation(error),
                        ..
                    } => {
                        assert_eq!(id, request);
                        assert!(error.contains(message), "{error}");
                    }
                    event => panic!("conflicting plugin was prepared: {event:?}"),
                }
            }
        }
        assert_eq!(manager.activations().unwrap()[0].0.id, "publisher/alpha");
    }

    #[test]
    fn verification_waits_for_the_package_transaction() {
        use std::time::Duration;
        let root = tempfile::tempdir().unwrap();
        let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
        let (manifest, plugin, bytes) = crate::tools::tests::package_fixture("demo");
        let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
        manager
            .commit_prepared(&manifest, &plugin, &object)
            .unwrap();
        for direct in [false, true] {
            let transaction = manager.transaction().unwrap();
            let other = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
            let (started, ready) = mpsc::channel();
            let (done, result) = mpsc::channel();
            let worker = thread::spawn(move || {
                started.send(()).unwrap();
                let verified = if direct {
                    verify_lock_store(
                        &other.lock_path(),
                        &other.store_root(),
                        1024 * 1024,
                        1024 * 1024,
                    )
                } else {
                    other.verify()
                };
                done.send(verified).unwrap();
            });
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                result.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            drop(transaction);
            assert_eq!(
                result
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap(),
                1
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn package_validation_rejects_invalid_components_at_install_and_startup() {
        for bytes in [
            b"\0asm\x01\0\0\0".as_slice(),
            b"\0asm\x0d\0\x01\0\x01\x01\xff",
        ] {
            let root = tempfile::tempdir().unwrap();
            let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
            let (mut manifest, mut plugin, _) = crate::tools::tests::package_fixture("demo");
            let digest = format!("{:x}", Sha256::digest(bytes));
            if let PackageSource::Registry { archive_sha256, .. } = &mut manifest.source {
                *archive_sha256 = digest.clone();
            }
            plugin.component_sha256 = digest.clone();
            assert!(matches!(
                manager.stage_verified_object(&manifest, bytes),
                Err(PackageError::InvalidManifest)
            ));
            let object = manager.store_root().join(digest);
            manager
                .commit_prepared(&manifest, &plugin, &object)
                .unwrap();
            assert!(matches!(
                manager.activations(),
                Err(PackageError::InvalidManifest)
            ));
        }
    }

    #[test]
    fn startup_rejects_nonregular_objects_before_opening_them() {
        use nix::sys::stat::Mode;
        use nix::unistd::mkfifo;
        let root = tempfile::tempdir().unwrap();
        let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
        let (manifest, plugin, bytes) = crate::tools::tests::package_fixture("demo");
        let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
        manager
            .commit_prepared(&manifest, &plugin, &object)
            .unwrap();
        fs::remove_file(&object).unwrap();
        mkfifo(&object, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        assert!(matches!(
            manager.activations(),
            Err(PackageError::InvalidManifest)
        ));
    }

    #[test]
    fn startup_objects_stay_locked_through_indexing_and_loading() {
        use crate::plugins::PluginActivationBehavior;
        use crate::plugins::PluginCommandContext;
        use crate::plugins::PluginCommandDeclaration;
        use crate::plugins::PluginEvent;
        use crate::plugins::PluginHost;
        use std::time::Duration;
        use std::time::Instant;
        let root = tempfile::tempdir().unwrap();
        let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024 * 1024).unwrap();
        let (mut manifest, mut plugin, _) = crate::tools::tests::package_fixture("demo");
        let core = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/retained-guest/retained-guest.core.wasm"
        ));
        let bytes = wit_component::ComponentEncoder::default()
            .module(core)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if let PackageSource::Registry { archive_sha256, .. } = &mut manifest.source {
            *archive_sha256 = digest.clone();
        }
        plugin.component_sha256 = digest;
        plugin.commands = [
            "drive",
            "trap",
            "flood",
            "spin",
            "private",
            "events",
            "storage",
            "provider",
            "virtual",
            "confirmation",
            "confirmationprobe",
        ]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                PluginCommandDeclaration {
                    title: name.into(),
                    description: String::new(),
                    arguments: BTreeMap::new(),
                    context: PluginCommandContext::Application,
                    activation: PluginActivationBehavior::ActiveOnly,
                },
            )
        })
        .collect();
        let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
        manager
            .commit_prepared(&manifest, &plugin, &object)
            .unwrap();
        let mut activations = manager.activations().unwrap();
        let other = PackageStore::new(root.path().into(), 1024 * 1024, 1024 * 1024).unwrap();
        other.remove("demo").unwrap();
        let (mut replacement, mut replacement_plugin, mut bytes) =
            crate::tools::tests::package_fixture("other");
        bytes.extend_from_slice(&[0, 2, 1, b'x']); // Empty component with a custom section.
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if let PackageSource::Registry { archive_sha256, .. } = &mut replacement.source {
            *archive_sha256 = digest.clone();
        }
        replacement_plugin.component_sha256 = digest;
        let replacement_object = other.stage_verified_object(&replacement, &bytes).unwrap();
        other
            .commit_prepared(&replacement, &replacement_plugin, &replacement_object)
            .unwrap();
        other.gc(256).unwrap();
        assert!(object.exists());

        let (manifest, path, object_lock) = activations.pop().unwrap();
        let checksum = manifest.component_sha256.clone();
        let mut plugins = PluginHost::start(2);
        let load = plugins
            .load_protected(
                manager.store_root(),
                path,
                manifest,
                PluginLimits::default(),
                Some(object_lock),
            )
            .unwrap();
        let next_event = |plugins: &mut PluginHost| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(event) = plugins.poll().unwrap().into_iter().next() {
                    break event;
                }
                assert!(Instant::now() < deadline, "plugin load timed out");
                thread::sleep(Duration::from_millis(2));
            }
        };
        assert!(matches!(
            next_event(&mut plugins),
            PluginEvent::Indexed { .. }
        ));
        other.gc(256).unwrap();
        assert!(object.exists());
        plugins.activate_indexed("demo", load, checksum).unwrap();
        let event = next_event(&mut plugins);
        assert!(matches!(event, PluginEvent::Loaded { .. }), "{event:?}");
        assert_eq!(other.gc(256).unwrap(), 1);
        assert!(!object.exists());
    }

    #[test]
    fn gc_preserves_outstanding_prepared_objects_across_managers() {
        let root = tempfile::tempdir().unwrap();
        let manager = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
        let (manifest, plugin, bytes) = crate::tools::tests::package_fixture("prepared");
        let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
        let protected = protect_object(&object).unwrap();
        let other = PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
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

pub(crate) fn installed_plugins(root: &Path) -> BTreeSet<String> {
    use sha2::Digest;
    use sha2::Sha256;

    let read = |path: &Path| -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        crate::files::open_regular_file(path)
            .ok()?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= 1024 * 1024).then_some(bytes)
    };
    let Some(lock) = read(&root.join("dust-packages.lock.json"))
        .and_then(|bytes| serde_json::from_slice::<crate::tools::PackageLock>(&bytes).ok())
    else {
        return BTreeSet::new();
    };
    lock.packages
        .iter()
        .take(1024)
        .filter_map(|package| {
            let digest = package
                .activation_sha256
                .as_ref()
                .unwrap_or(&package.content_sha256);
            if [&package.content_sha256, digest]
                .into_iter()
                .any(|digest| digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return None;
            }
            let bytes = read(&root.join("activations").join(format!("{digest}.toml")))?;
            if package.activation_sha256.is_some()
                && !format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(digest)
            {
                return None;
            }
            let manifest: crate::plugins::PluginManifest =
                toml::from_str(std::str::from_utf8(&bytes).ok()?).ok()?;
            manifest
                .component_sha256
                .eq_ignore_ascii_case(&package.content_sha256)
                .then_some(manifest.id)
        })
        .collect()
}
use std::io::Read;
