#![forbid(unsafe_code)]
mod registry;
pub use registry::{
    OFFICIAL_REGISTRY, RegistryError, RegistryEvent, RegistryPackageMetadata,
    RegistryPackageSummary, RegistryRequest, RegistryResult, RegistrySearchResponse,
    RegistryService,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::plugins::{PluginLimits, PluginManifest, activate_candidate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageSource {
    Registry {
        registry: String,
        version: String,
        archive_sha256: String,
    },
    Git {
        url: String,
        commit: String,
        tree_sha256: String,
    },
    Path {
        path: PathBuf,
    },
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDependency {
    pub name: String,
    pub source: PackageSource,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub component: String,
    pub source: PackageSource,
    #[serde(default)]
    pub dependencies: Vec<PackageDependency>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: PackageSource,
    pub content_sha256: String,
    pub dependencies: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageLock {
    pub version: u32,
    pub packages: Vec<LockedPackage>,
    pub reproducible: bool,
}
impl PackageManifest {
    fn source_digest(&self) -> &str {
        match &self.source {
            PackageSource::Registry { archive_sha256, .. } => archive_sha256,
            PackageSource::Git { tree_sha256, .. } => tree_sha256,
            PackageSource::Path { .. } => "",
        }
    }
}
fn read_bounded_regular(path: &Path, maximum: u64) -> Result<Vec<u8>, PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(PackageError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum
    {
        return Err(PackageError::InvalidManifest);
    }
    fs::read(path).map_err(PackageError::Io)
}
static NEXT_PACKAGE_TEMP: AtomicU64 = AtomicU64::new(1);
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn valid_source(source: &PackageSource) -> bool {
    match source {
        PackageSource::Registry {
            version,
            archive_sha256,
            ..
        } => !version.is_empty() && version != "*" && valid_digest(archive_sha256),
        PackageSource::Git {
            commit,
            tree_sha256,
            ..
        } => {
            (commit.len() == 40 || commit.len() == 64)
                && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
                && valid_digest(tree_sha256)
        }
        PackageSource::Path { path } => path.is_absolute(),
    }
}

pub fn resolve_manifests(
    manifests: &BTreeMap<String, PackageManifest>,
) -> Result<PackageLock, PackageError> {
    for (name, manifest) in manifests {
        if name != &manifest.name
            || manifest.name.is_empty()
            || !valid_source(&manifest.source)
            || manifest
                .dependencies
                .iter()
                .any(|dependency| !manifests.contains_key(&dependency.name))
        {
            return Err(PackageError::InvalidManifest);
        }
        for dependency in &manifest.dependencies {
            if &manifests[&dependency.name].source != &dependency.source {
                return Err(PackageError::SourceConflict(dependency.name.clone()));
            }
        }
    }
    fn visit(
        name: &str,
        manifests: &BTreeMap<String, PackageManifest>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) -> Result<(), PackageError> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name.into()) {
            return Err(PackageError::Cycle(name.into()));
        }
        for dependency in &manifests[name].dependencies {
            visit(&dependency.name, manifests, visiting, visited, out)?;
        }
        visiting.remove(name);
        visited.insert(name.into());
        out.push(name.into());
        Ok(())
    }
    let mut order = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for name in manifests.keys() {
        visit(name, manifests, &mut visiting, &mut visited, &mut order)?;
    }
    let reproducible = order
        .iter()
        .all(|name| !matches!(manifests[name].source, PackageSource::Path { .. }));
    let packages = order
        .into_iter()
        .map(|name| {
            let manifest = &manifests[&name];
            let content_sha256 = match &manifest.source {
                PackageSource::Registry { archive_sha256, .. } => archive_sha256.clone(),
                PackageSource::Git { tree_sha256, .. } => tree_sha256.clone(),
                PackageSource::Path { .. } => String::new(),
            };
            LockedPackage {
                name,
                version: manifest.version.clone(),
                source: manifest.source.clone(),
                content_sha256,
                dependencies: manifest
                    .dependencies
                    .iter()
                    .map(|dependency| dependency.name.clone())
                    .collect(),
            }
        })
        .collect();
    Ok(PackageLock {
        version: 1,
        packages,
        reproducible,
    })
}
pub fn write_lock_atomic(
    path: &Path,
    lock: &PackageLock,
    maximum_bytes: usize,
) -> Result<(), PackageError> {
    let bytes = serde_json::to_vec_pretty(lock).map_err(PackageError::Json)?;
    if bytes.len() > maximum_bytes {
        return Err(PackageError::LockTooLarge);
    }
    let parent = path.parent().ok_or(PackageError::InvalidManifest)?;
    fs::create_dir_all(parent).map_err(PackageError::Io)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(PackageError::InvalidManifest);
    }
    let temp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(PackageError::Io)?;
    file.write_all(&bytes).map_err(PackageError::Io)?;
    file.sync_all().map_err(PackageError::Io)?;
    fs::rename(temp, path).map_err(PackageError::Io)?;
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(PackageError::Io)
}
fn write_protected_atomic(path: &Path, bytes: &[u8], maximum: usize) -> Result<(), PackageError> {
    if bytes.len() > maximum {
        return Err(PackageError::LockTooLarge);
    }
    let parent = path.parent().ok_or(PackageError::InvalidManifest)?;
    fs::create_dir_all(parent).map_err(PackageError::Io)?;
    if fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_file() || metadata.file_type().is_symlink())
    {
        return Err(PackageError::InvalidManifest);
    }
    let temp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(PackageError::Io)?;
    file.write_all(bytes).map_err(PackageError::Io)?;
    file.sync_all().map_err(PackageError::Io)?;
    fs::rename(temp, path).map_err(PackageError::Io)?;
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(PackageError::Io)
}
pub fn verify_lock_store(
    lock_path: &Path,
    store: &Path,
    maximum_bytes: usize,
) -> Result<usize, PackageError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let metadata = fs::metadata(lock_path).map_err(PackageError::Io)?;
    if metadata.len() > maximum_bytes as u64 {
        return Err(PackageError::LockTooLarge);
    }
    let mut bytes = Vec::new();
    fs::File::open(lock_path)
        .map_err(PackageError::Io)?
        .take(maximum_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(PackageError::Io)?;
    let lock: PackageLock = serde_json::from_slice(&bytes).map_err(PackageError::Json)?;
    if lock.version != 1 {
        return Err(PackageError::InvalidManifest);
    }
    let mut names = BTreeSet::new();
    for package in &lock.packages {
        if !names.insert(&package.name)
            || !valid_source(&package.source)
            || !valid_digest(&package.content_sha256)
        {
            return Err(PackageError::InvalidManifest);
        }
        let object = store.join(&package.content_sha256);
        let data = fs::read(&object).map_err(PackageError::Io)?;
        if format!("{:x}", Sha256::digest(&data)) != package.content_sha256.to_ascii_lowercase() {
            return Err(PackageError::ChecksumMismatch);
        }
        if data.starts_with(b"\0asm") {
            let mut config = wasmtime::Config::new();
            config.wasm_component_model(true);
            let engine =
                wasmtime::Engine::new(&config).map_err(|_| PackageError::InvalidManifest)?;
            wasmtime::component::Component::from_binary(&engine, &data)
                .map_err(|_| PackageError::InvalidManifest)?;
        }
    }
    Ok(lock.packages.len())
}

pub fn commit_store_bytes(
    store: &Path,
    expected_sha256: &str,
    bytes: &[u8],
    maximum_bytes: usize,
) -> Result<PathBuf, PackageError> {
    use sha2::{Digest, Sha256};
    if bytes.len() > maximum_bytes || !valid_digest(expected_sha256) {
        return Err(PackageError::InvalidManifest);
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err(PackageError::ChecksumMismatch);
    }
    fs::create_dir_all(store).map_err(PackageError::Io)?;
    let target = store.join(&actual);
    if target.exists() {
        return Ok(target);
    }
    let temp = store.join(format!(
        ".stage-{}-{}",
        std::process::id(),
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(PackageError::Io)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(PackageError::Io(error));
    }
    fs::rename(&temp, &target).map_err(PackageError::Io)?;
    fs::File::open(store)
        .and_then(|file| file.sync_all())
        .map_err(PackageError::Io)?;
    Ok(target)
}
#[derive(Debug, Error)]
pub enum PackageError {
    #[error("invalid package manifest")]
    InvalidManifest,
    #[error("package dependency cycle at {0}")]
    Cycle(String),
    #[error("conflicting source for package {0}")]
    SourceConflict(String),
    #[error("package source is development-only and nonreproducible")]
    NonReproducibleSource,
    #[error("package {0} is required by another package")]
    PackageRequired(String),
    #[error("no package lock rollback is available")]
    NoRollback,
    #[error("prepared package token is missing or expired")]
    MissingPrepared,
    #[error("package source file is outside workspace scope")]
    OutsideScope,
    #[error("package activation failed: {0}")]
    Activation(String),
    #[error("package service queue is full")]
    QueueFull,
    #[error("package service stopped")]
    ServiceStopped,
    #[error("package content checksum mismatch")]
    ChecksumMismatch,
    #[error("package lock exceeds its ceiling")]
    LockTooLarge,
    #[error("package lock JSON failed: {0}")]
    Json(#[source] serde_json::Error),
    #[error("package I/O failed: {0}")]
    Io(#[source] std::io::Error),
}

const BUNDLED_TOOL_CATALOG: &str = include_str!("../../assets/tools/catalog.toml");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Lsp,
    Dap,
    Formatter,
    Linter,
}

impl ToolKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lsp => "LSP",
            Self::Dap => "DAP",
            Self::Formatter => "Formatter",
            Self::Linter => "Linter",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    pub id: String,
    pub name: String,
    pub kind: ToolKind,
    pub languages: Vec<String>,
    pub executable: String,
    #[serde(default)]
    pub launch_args: Vec<String>,
    pub install_program: Option<String>,
    #[serde(default)]
    pub install_args: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCatalogFile {
    tool: Vec<ToolDefinition>,
}

#[must_use]
pub fn bundled_tool_catalog() -> Vec<ToolDefinition> {
    toml::from_str::<ToolCatalogFile>(BUNDLED_TOOL_CATALOG)
        .expect("bundled tool catalog must remain valid")
        .tool
}

#[must_use]
pub fn discover_executable(executable: &str) -> Option<PathBuf> {
    let candidate = Path::new(executable);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    env::split_paths(&env::var_os("PATH")?).find_map(|directory| {
        let path = directory.join(executable);
        path.is_file().then_some(path)
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolStatus {
    Available,
    Installed(PathBuf),
    Installing,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct ToolEntry {
    pub definition: ToolDefinition,
    pub status: ToolStatus,
}

#[must_use]
pub fn inspect_tools() -> Vec<ToolEntry> {
    bundled_tool_catalog()
        .into_iter()
        .map(|definition| {
            let status = discover_executable(&definition.executable)
                .map_or(ToolStatus::Available, ToolStatus::Installed);
            ToolEntry { definition, status }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct ToolInstallResult {
    pub id: String,
    pub result: Result<Option<PathBuf>, String>,
}

enum InstallerRequest {
    Install {
        definition: ToolDefinition,
        program: PathBuf,
    },
    Shutdown,
}

pub struct ToolInstaller {
    sender: SyncSender<InstallerRequest>,
    receiver: Receiver<ToolInstallResult>,
    cancel: Sender<()>,
    busy: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ToolInstaller {
    #[must_use]
    pub fn start() -> Self {
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::channel();
        let (cancel, cancellations) = mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        let thread = thread::Builder::new()
            .name("dust-tool-installer".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let InstallerRequest::Install {
                        definition,
                        program,
                    } = request
                    else {
                        break;
                    };
                    let id = definition.id.clone();
                    let result = install_tool(&definition, &program, &cancellations);
                    worker_busy.store(false, Ordering::Release);
                    if results.send(ToolInstallResult { id, result }).is_err() {
                        break;
                    }
                }
            })
            .expect("tool installer thread must start");
        Self {
            sender,
            receiver,
            cancel,
            busy,
            thread: Some(thread),
        }
    }

    pub fn install_absolute(
        &self,
        definition: ToolDefinition,
        program: PathBuf,
    ) -> Result<(), ToolInstallerError> {
        if !program.is_absolute() {
            return Err(ToolInstallerError::InvalidProgram);
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ToolInstallerError::Busy);
        }
        self.sender
            .try_send(InstallerRequest::Install {
                definition,
                program,
            })
            .map_err(|error| {
                self.busy.store(false, Ordering::Release);
                match error {
                    TrySendError::Full(_) => ToolInstallerError::Busy,
                    TrySendError::Disconnected(_) => ToolInstallerError::Stopped,
                }
            })
    }

    pub fn try_result(&self) -> Result<Option<ToolInstallResult>, ToolInstallerError> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ToolInstallerError::Stopped),
        }
    }
}

impl Drop for ToolInstaller {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        let _ = self.sender.send(InstallerRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn install_tool(
    definition: &ToolDefinition,
    program: &Path,
    cancellations: &Receiver<()>,
) -> Result<Option<PathBuf>, String> {
    if !program.is_absolute() {
        return Err("installer executable was not preflighted".into());
    }
    let mut child = Command::new(program)
        .args(&definition.install_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .spawn()
        .map_err(|error| format!("failed to launch {}: {error}", program.display()))?;
    let deadline = Instant::now() + Duration::from_secs(900);
    let status = loop {
        if cancellations.try_recv().is_ok() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("tool installation cancelled".into());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("tool installation exceeded the 15-minute source ceiling".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("failed to wait for {}: {error}", program.display())),
        }
    };
    if !status.success() {
        return Err(format!(
            "installer `{}` exited with {}",
            program.display(),
            status
                .code()
                .map_or_else(|| "signal".into(), |code| code.to_string())
        ));
    }
    Ok(discover_executable(&definition.executable))
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolInstallerError {
    #[error("another tool installation is already running")]
    Busy,
    #[error("tool installer stopped")]
    Stopped,
    #[error("tool installer program must be an absolute preflighted path")]
    InvalidProgram,
}

#[cfg(test)]
mod tests {
    use super::{ToolKind, bundled_tool_catalog, discover_executable};

    #[test]
    fn bundled_catalog_is_typed_and_has_real_language_and_debug_tools() {
        let catalog = bundled_tool_catalog();
        assert!(catalog.iter().any(|tool| tool.id == "rust-analyzer"));
        assert!(catalog.iter().any(|tool| tool.kind == ToolKind::Dap));
        assert!(catalog.iter().all(|tool| !tool.languages.is_empty()));
        assert!(
            catalog
                .iter()
                .all(|tool| { tool.install_program.is_some() || tool.install_args.is_empty() })
        );
    }

    #[test]
    fn current_test_process_is_discoverable_without_a_shell() {
        let executable = std::env::current_exe().expect("current executable");
        assert_eq!(
            discover_executable(executable.to_str().expect("UTF-8 path")),
            Some(executable)
        );
    }
}

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
        let expected = manifest.source_digest();
        if !valid_digest(expected)
            || object.file_name().and_then(|value| value.to_str()) != Some(expected)
        {
            return Err(PackageError::ChecksumMismatch);
        }
        let bytes = read_bounded_regular(object, self.maximum_object_bytes as u64)?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if !actual.eq_ignore_ascii_case(expected)
            || !plugin.component_sha256.eq_ignore_ascii_case(expected)
        {
            return Err(PackageError::ChecksumMismatch);
        }
        let activation = self
            .root
            .join("activations")
            .join(format!("{expected}.toml"));
        write_protected_atomic(
            &activation,
            toml::to_string(plugin)
                .map_err(|_| PackageError::InvalidManifest)?
                .as_bytes(),
            1024 * 1024,
        )?;
        let mut lock = self.read_lock()?;
        self.backup()?;
        lock.packages
            .retain(|package| package.name != manifest.name);
        lock.packages.push(LockedPackage {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            source: manifest.source.clone(),
            content_sha256: expected.into(),
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
            let object = self.root.join("store").join(&package.content_sha256);
            let manifest_path = self
                .root
                .join("activations")
                .join(format!("{}.toml", package.content_sha256));
            let bytes = read_bounded_regular(&manifest_path, 1024 * 1024)?;
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
        let lock = self.read_lock()?;
        self.activations_for_lock(&lock)
    }
    pub fn remove(&self, name: &str) -> Result<PackageLock, PackageError> {
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
        let lock = self.read_lock()?;
        let retained = lock
            .packages
            .into_iter()
            .map(|package| package.content_sha256)
            .collect::<BTreeSet<_>>();
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
                    && !retained.contains(stem)
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
        metadata: RegistryPackageMetadata,
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
#[derive(Clone, Debug)]
struct PreparedInstall {
    manifest: PackageManifest,
    plugin: PluginManifest,
    object: PathBuf,
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
    Completed(PackageOperationResult),
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
                        PackageServiceEvent::Completed,
                    );
                    let _ = etx.try_send(event);
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
