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
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    filesystem::ProjectRootStrategy,
    plugins::{PluginLimits, PluginManifest, activate_candidate},
};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const BUNDLED_NIX_LSP_FLAKE: &str = include_str!("../../assets/tools/nix-workspace/flake.nix");
const BUNDLED_NIX_LSP_LOCK: &str = include_str!("../../assets/tools/nix-workspace/flake.lock");
/// Exact nixpkgs revision locked by Dust's bundled Linux compatibility runtime.
pub const NIX_LSP_NIXPKGS_REVISION: &str = "b6018f87da91d19d0ab4cf979885689b469cdd41";
const NIX_COMPATIBILITY_FLAKE_PACKAGE: &str = "compat-runtime";
const NIX_COMPATIBILITY_RUNTIME_ID: &str =
    "v1-b6018f87da91d19d0ab4cf979885689b469cdd41-common-libraries-1";
const NIX_RUNTIME_ENVIRONMENT_FILE: &str = "share/dust/runtime.env";
const MAXIMUM_NIX_RUNTIME_ENVIRONMENT_BYTES: u64 = 32 * 1024;
const MAXIMUM_OS_RELEASE_BYTES: u64 = 16 * 1024;
static NIXOS_HOST: OnceLock<bool> = OnceLock::new();

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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DebugProgramPolicy {
    /// The adapter receives a preflighted compiled/debuggable program path.
    #[default]
    Executable,
    /// The adapter debugs the selected source document directly.
    Source,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArchiveFormat {
    Raw,
    Gzip,
    TarGzip,
    Zip,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolInstall {
    /// A checked executable in an upstream release archive.
    Archive {
        url: String,
        sha256: String,
        archive: ToolArchiveFormat,
        executable_path: String,
    },
    /// Exact npm-registry packages in a Dust-owned Bun prefix. Bun is either a
    /// checked Dust-managed runtime or a directly runnable user-provided binary
    /// (including one supplied by a Nix development shell). The package installer
    /// never invokes npm, Node, or a distribution package manager.
    NpmPackage { packages: Vec<String> },
    /// Python packages in a Dust-owned uv environment.
    UvTool { packages: Vec<String> },
    /// A Go module built by a Dust-owned Go toolchain.
    GoPackage { package: String },
    /// A Rust component added by a Dust-owned rustup installation.
    RustupComponent { component: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolVersionPin {
    /// Exact executable version exposed by the tool manager.
    pub version: String,
    /// Upstream release/build identifier when it differs from the executable version.
    #[serde(default)]
    pub release: Option<String>,
    /// Immutable source used when this version is installed.
    pub install: ToolInstall,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    pub id: String,
    pub name: String,
    pub kind: ToolKind,
    pub languages: Vec<String>,
    pub executable: String,
    /// Exact executable/package version for the default Dust-managed artifact.
    #[serde(default)]
    pub version: Option<String>,
    /// Upstream release/build identifier when it differs from `version`.
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub launch_args: Vec<String>,
    /// A bounded invocation that verifies a downloaded native artifact before activation.
    #[serde(default)]
    pub probe_args: Vec<String>,
    #[serde(default)]
    pub debug_program_policy: DebugProgramPolicy,
    /// Bounded marker evidence for an adapter-defined project root.
    #[serde(default)]
    pub project_root_markers: Vec<String>,
    /// The catalog-selected validator; an unproven candidate never launches a server.
    #[serde(default)]
    pub project_root_strategy: ProjectRootStrategy,
    #[serde(default)]
    pub install: Option<ToolInstall>,
    /// Additional exact, checksum/package-pinned versions that Dust can install.
    #[serde(default)]
    pub pins: Vec<ToolVersionPin>,
}

impl ToolDefinition {
    /// Returns the default catalog definition followed by every exact supported pin.
    #[must_use]
    pub fn managed_versions(&self) -> Vec<Self> {
        let mut versions = Vec::with_capacity(self.pins.len().saturating_add(1));
        if self.version.is_some() && self.install.is_some() {
            let mut current = self.clone();
            current.pins.clear();
            versions.push(current);
        }
        versions.extend(self.pins.iter().map(|pin| {
            let mut definition = self.clone();
            definition.version = Some(pin.version.clone());
            definition.release = pin.release.clone();
            definition.install = Some(pin.install.clone());
            definition.pins.clear();
            definition
        }));
        versions
    }

    /// Resolves one exact supported managed version without accepting an arbitrary package string.
    #[must_use]
    pub fn managed_version(&self, version: &str) -> Option<Self> {
        self.managed_versions().into_iter().find(|definition| {
            definition.version.as_deref() == Some(version)
                || definition.release.as_deref() == Some(version)
        })
    }
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

#[must_use]
pub fn rust_analyzer_is_rustup_proxy(path: &Path) -> bool {
    if fs::canonicalize(path)
        .ok()
        .and_then(|target| target.file_name().map(ToOwned::to_owned))
        .is_some_and(|name| name == "rustup")
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let Ok(proxy) = fs::metadata(path) else {
            return false;
        };
        let rustup = path.with_file_name("rustup");
        return fs::metadata(rustup)
            .is_ok_and(|runtime| proxy.dev() == runtime.dev() && proxy.ino() == runtime.ino());
    }
    #[cfg(not(unix))]
    false
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolStatus {
    Available,
    /// The catalog has a method, but this host cannot satisfy its runtime policy.
    Unavailable(String),
    Installed(PathBuf),
    Installing {
        phase: String,
        completed: u64,
        total: Option<u64>,
    },
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct ToolEntry {
    pub definition: ToolDefinition,
    pub status: ToolStatus,
}

#[must_use]
pub fn tool_store_root() -> PathBuf {
    crate::configuration::user_state_directory().join("tools")
}

/// Whether this Linux host is NixOS, determined once from the standard OS identity file.
///
/// The result selects how Dust runs the same verified upstream artifact: directly on
/// ordinary Linux, or through Dust's locked private Nix compatibility runtime on NixOS.
/// An unreadable or malformed file fails closed to the portable direct-artifact policy.
#[must_use]
pub fn is_nixos_host() -> bool {
    *NIXOS_HOST.get_or_init(|| nixos_os_release_at(Path::new("/etc/os-release")))
}

fn nixos_os_release_at(path: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut text = String::new();
    if Read::by_ref(&mut file)
        .take(MAXIMUM_OS_RELEASE_BYTES.saturating_add(1))
        .read_to_string(&mut text)
        .is_err()
        || text.len() > MAXIMUM_OS_RELEASE_BYTES as usize
    {
        return false;
    }
    text.lines().any(|line| {
        let Some((key, raw_value)) = line.split_once('=') else {
            return false;
        };
        if key.trim() != "ID" {
            return false;
        }
        let value = raw_value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value);
        value == "nixos"
    })
}

/// Dust owns this flake and its lock file; it is never sourced from a project.
#[must_use]
pub fn nix_lsp_workspace_root() -> PathBuf {
    tool_store_root().join("nix-lsp-workspace")
}

/// A profile identity for a catalog LSP that has a verified upstream installer.
/// Every profile realizes the same generic compatibility package; release versions
/// remain catalog/upstream data and are never encoded in the Nix flake.
#[must_use]
pub fn nix_lsp_shell(definition: &ToolDefinition) -> Option<&str> {
    (definition.kind == ToolKind::Lsp
        && definition.install.is_some()
        && validate_tool_id(&definition.id).is_ok())
    .then_some(definition.id.as_str())
}

/// Environment required to launch an installed JavaScript tool. The tool-local
/// `node` entry is Bun on ordinary Linux or pinned Nix Node on NixOS, so a
/// `#!/usr/bin/env node` package executable never depends on global Node/npm.
#[must_use]
pub fn tool_launch_environment(definition: &ToolDefinition) -> BTreeMap<String, String> {
    tool_launch_environment_at(&tool_store_root(), definition)
}

fn tool_launch_environment_at(
    root: &Path,
    definition: &ToolDefinition,
) -> BTreeMap<String, String> {
    if !matches!(definition.install, Some(ToolInstall::NpmPackage { .. })) {
        return BTreeMap::new();
    }
    let bin = root.join(&definition.id).join("bin");
    if !bin.join("node").is_file() {
        return BTreeMap::new();
    }
    let mut paths = vec![bin];
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    let Ok(path) = env::join_paths(paths) else {
        return BTreeMap::new();
    };
    BTreeMap::from([("PATH".into(), path.to_string_lossy().into_owned())])
}

fn managed_executable(root: &Path, definition: &ToolDefinition) -> Option<PathBuf> {
    let candidate = root
        .join(&definition.id)
        .join("bin")
        .join(&definition.executable);
    candidate.is_file().then_some(candidate)
}

/// Returns the executable installed in Dust's private tool store, independently
/// from a same-named executable discovered on PATH.
#[must_use]
pub fn managed_tool_executable(definition: &ToolDefinition) -> Option<PathBuf> {
    managed_executable(&tool_store_root(), definition)
}

const MANAGED_TOOL_VERSION_FILE: &str = ".dust-version";
const MAXIMUM_MANAGED_TOOL_VERSION_BYTES: u64 = 256;

fn managed_tool_installed_version_at(root: &Path, definition: &ToolDefinition) -> Option<String> {
    validate_tool_id(&definition.id).ok()?;
    let path = root.join(&definition.id).join(MANAGED_TOOL_VERSION_FILE);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAXIMUM_MANAGED_TOOL_VERSION_BYTES
    {
        return None;
    }
    let version = fs::read_to_string(path).ok()?;
    let version = version.trim();
    (!version.is_empty()
        && version.len() <= MAXIMUM_MANAGED_TOOL_VERSION_BYTES as usize
        && !version.chars().any(char::is_control))
    .then(|| version.to_owned())
}

/// Exact catalog version recorded inside the atomically activated private tool directory.
#[must_use]
pub fn managed_tool_installed_version(definition: &ToolDefinition) -> Option<String> {
    managed_tool_installed_version_at(&tool_store_root(), definition)
}

fn write_managed_tool_version(path: &Path, definition: &ToolDefinition) -> Result<(), String> {
    let version = definition
        .version
        .as_deref()
        .ok_or("managed tool version is missing from the catalog")?;
    if version.is_empty()
        || version.len() > MAXIMUM_MANAGED_TOOL_VERSION_BYTES as usize
        || version.chars().any(char::is_control)
    {
        return Err("managed tool version is invalid".into());
    }
    let target = path.join(MANAGED_TOOL_VERSION_FILE);
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(target)
        .map_err(|error| format!("cannot record managed tool version: {error}"))?;
    file.write_all(version.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot record managed tool version: {error}"))
}

fn nix_lsp_profile_path_at(root: &Path, definition: &ToolDefinition) -> Option<PathBuf> {
    let profile = nix_lsp_shell(definition)?;
    Some(root.join("nix-lsp-workspace/profiles").join(profile))
}

/// Returns whether the private Nix compatibility profile for this catalog LSP is realized.
#[must_use]
pub fn nix_lsp_profile_installed(definition: &ToolDefinition) -> bool {
    nix_lsp_profile_path_at(&tool_store_root(), definition)
        .is_some_and(|profile| parse_nix_runtime_environment(&profile).is_ok())
}

/// A NixOS LSP installation is complete only when both the compatibility profile and
/// the exact verified upstream artifact exist. A profile alone is never launchable.
#[must_use]
pub fn nix_lsp_tool_installed(definition: &ToolDefinition) -> bool {
    nix_lsp_profile_installed(definition) && managed_tool_executable(definition).is_some()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NixCompatibilityRuntime {
    loader: PathBuf,
    nix_ld: PathBuf,
    patchelf: PathBuf,
    library_path: String,
    runtime_path: String,
}

impl NixCompatibilityRuntime {
    fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("NIX_LD".into(), self.loader.display().to_string()),
            ("NIX_LD_LIBRARY_PATH".into(), self.library_path.clone()),
            ("LD_LIBRARY_PATH".into(), self.library_path.clone()),
        ])
    }

    fn executable(&self, name: &str) -> Option<PathBuf> {
        validate_tool_id(name).ok()?;
        env::split_paths(&self.runtime_path)
            .map(|directory| directory.join(name))
            .find(|path| valid_nix_store_path(path) && path.is_file())
    }
}

fn valid_nix_store_path(path: &Path) -> bool {
    path.is_absolute()
        && path.starts_with("/nix/store")
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn parse_nix_runtime_environment(profile: &Path) -> Result<NixCompatibilityRuntime, String> {
    if !nix_profile_installed_at(profile) {
        return Err("Dust Nix compatibility profile is not installed".into());
    }
    let path = profile.join(NIX_RUNTIME_ENVIRONMENT_FILE);
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("cannot inspect Dust Nix runtime metadata: {error}"))?;
    if !metadata.file_type().is_file() || metadata.len() > MAXIMUM_NIX_RUNTIME_ENVIRONMENT_BYTES {
        return Err("Dust Nix runtime metadata is not a bounded regular file".into());
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read Dust Nix runtime metadata: {error}"))?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or("Dust Nix runtime metadata is malformed")?;
        if !matches!(
            key,
            "DUST_NIX_RUNTIME_ID"
                | "DUST_NIX_LOADER"
                | "DUST_NIX_LD"
                | "DUST_NIX_PATCHELF"
                | "DUST_NIX_LIBRARY_PATH"
                | "DUST_NIX_RUNTIME_PATH"
        ) || value.is_empty()
            || value.chars().any(char::is_control)
            || values.insert(key, value).is_some()
        {
            return Err("Dust Nix runtime metadata is invalid".into());
        }
    }
    if values.get("DUST_NIX_RUNTIME_ID").copied() != Some(NIX_COMPATIBILITY_RUNTIME_ID) {
        return Err("Dust Nix runtime profile is from an incompatible revision".into());
    }
    let path_value = |key: &str| -> Result<PathBuf, String> {
        let value = PathBuf::from(
            *values
                .get(key)
                .ok_or("Dust Nix runtime metadata is incomplete")?,
        );
        if !valid_nix_store_path(&value) || !value.is_file() {
            return Err(format!("Dust Nix runtime {key} path is unavailable"));
        }
        Ok(value)
    };
    let list_value = |key: &str| -> Result<String, String> {
        let value = *values
            .get(key)
            .ok_or("Dust Nix runtime metadata is incomplete")?;
        if value.split(':').any(|entry| {
            let path = Path::new(entry);
            entry.is_empty() || !valid_nix_store_path(path) || !path.is_dir()
        }) {
            return Err(format!("Dust Nix runtime {key} contains an invalid path"));
        }
        Ok(value.into())
    };
    Ok(NixCompatibilityRuntime {
        loader: path_value("DUST_NIX_LOADER")?,
        nix_ld: path_value("DUST_NIX_LD")?,
        patchelf: path_value("DUST_NIX_PATCHELF")?,
        library_path: list_value("DUST_NIX_LIBRARY_PATH")?,
        runtime_path: list_value("DUST_NIX_RUNTIME_PATH")?,
    })
}

fn nix_lsp_runtime_at(
    root: &Path,
    definition: &ToolDefinition,
) -> Result<NixCompatibilityRuntime, String> {
    let profile = nix_lsp_profile_path_at(root, definition)
        .ok_or("this LSP has no Dust Nix compatibility profile")?;
    parse_nix_runtime_environment(&profile)
}

pub(crate) fn nix_lsp_launch_environment_at(
    root: &Path,
    definition: &ToolDefinition,
) -> Result<BTreeMap<String, String>, String> {
    let runtime = nix_lsp_runtime_at(root, definition)?;
    let mut environment = runtime.environment();
    let mut paths = Vec::new();
    if matches!(definition.install, Some(ToolInstall::NpmPackage { .. })) {
        paths.push(root.join(&definition.id).join("bin"));
    }
    paths.extend(env::split_paths(&runtime.runtime_path));
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    let path = env::join_paths(paths).map_err(|_| "Dust Nix runtime PATH is invalid")?;
    environment.insert("PATH".into(), path.to_string_lossy().into_owned());
    Ok(environment)
}

pub(crate) fn nix_lsp_launch_environment_for_workspace_at(
    workspace: &Path,
    definition: &ToolDefinition,
) -> Result<BTreeMap<String, String>, String> {
    if !workspace.is_absolute()
        || workspace.file_name().and_then(|name| name.to_str()) != Some("nix-lsp-workspace")
    {
        return Err("Dust Nix compatibility workspace path is invalid".into());
    }
    let root = workspace
        .parent()
        .ok_or("Dust Nix compatibility workspace has no private tool root")?;
    nix_lsp_launch_environment_at(root, definition)
}

/// Environment for a verified upstream LSP processed by Dust's private Nix runtime.
#[must_use]
pub fn nix_lsp_launch_environment(
    definition: &ToolDefinition,
) -> Result<BTreeMap<String, String>, String> {
    nix_lsp_launch_environment_at(&tool_store_root(), definition)
}

fn install_readiness(definition: &ToolDefinition) -> Option<String> {
    match definition.install.as_ref()? {
        ToolInstall::NpmPackage { .. }
            if discover_executable("bun").is_none()
                && !(definition.kind == ToolKind::Lsp
                    && is_nixos_host()
                    && discover_executable("nix").is_some())
                && (!fhs_glibc_loader_available() || runtime_artifact("bun").is_err()) =>
        {
            Some(
                "requires a directly runnable Bun runtime, Dust's private Nix runtime on NixOS, or an FHS glibc Linux host"
                    .into(),
            )
        }
        ToolInstall::UvTool { .. } if runtime_artifact("uv").is_err() => Some(format!(
            "no managed static uv runtime for Linux {}",
            env::consts::ARCH
        )),
        ToolInstall::GoPackage { .. } if runtime_artifact("go").is_err() => Some(format!(
            "no managed Go runtime for Linux {}",
            env::consts::ARCH
        )),
        ToolInstall::RustupComponent { .. } if runtime_artifact("rustup").is_err() => Some(
            format!("no managed rustup runtime for Linux {}", env::consts::ARCH),
        ),
        _ => None,
    }
}

#[must_use]
pub fn inspect_tools_at(root: &Path) -> Vec<ToolEntry> {
    bundled_tool_catalog()
        .into_iter()
        .map(|definition| {
            // PATH is intentionally first priority. A Dust-managed installation
            // remains independently selectable in the interactive runtime picker.
            let status = discover_executable(&definition.executable)
                .or_else(|| managed_executable(root, &definition))
                .map_or_else(
                    || {
                        install_readiness(&definition)
                            .map_or(ToolStatus::Available, ToolStatus::Unavailable)
                    },
                    ToolStatus::Installed,
                );
            ToolEntry { definition, status }
        })
        .collect()
}

#[must_use]
pub fn inspect_tools() -> Vec<ToolEntry> {
    inspect_tools_at(&tool_store_root())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolOperation {
    InstallManaged,
    UninstallManaged,
    InstallNix,
    UninstallNix,
}

#[derive(Clone, Debug)]
pub struct ToolInstallResult {
    pub id: String,
    pub operation: ToolOperation,
    pub result: Result<Option<PathBuf>, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolInstallProgress {
    pub id: String,
    pub operation: ToolOperation,
    pub phase: String,
    pub completed: u64,
    pub total: Option<u64>,
}

enum InstallerRequest {
    Install {
        definition: ToolDefinition,
    },
    Uninstall {
        definition: ToolDefinition,
    },
    InstallNix {
        definition: ToolDefinition,
        nix: PathBuf,
    },
    UninstallNix {
        definition: ToolDefinition,
    },
    Shutdown,
}

pub struct ToolInstaller {
    sender: SyncSender<InstallerRequest>,
    receiver: Receiver<ToolInstallResult>,
    progress_receiver: Receiver<ToolInstallProgress>,
    cancel: Sender<()>,
    busy: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ToolInstaller {
    #[must_use]
    pub fn start() -> Self {
        Self::start_at(tool_store_root())
    }

    #[must_use]
    pub fn start_at(root: PathBuf) -> Self {
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::channel();
        let (progress, progress_receiver) = mpsc::sync_channel(32);
        let (cancel, cancellations) = mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        let thread = thread::Builder::new()
            .name("dust-tool-installer".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let (definition, operation, nix) = match request {
                        InstallerRequest::Install { definition } => {
                            (definition, ToolOperation::InstallManaged, None)
                        }
                        InstallerRequest::Uninstall { definition } => {
                            (definition, ToolOperation::UninstallManaged, None)
                        }
                        InstallerRequest::InstallNix { definition, nix } => {
                            (definition, ToolOperation::InstallNix, Some(nix))
                        }
                        InstallerRequest::UninstallNix { definition } => {
                            (definition, ToolOperation::UninstallNix, None)
                        }
                        InstallerRequest::Shutdown => break,
                    };
                    let id = definition.id.clone();
                    let result_id = id.clone();
                    let progress = progress.clone();
                    let mut report = move |phase: &str, completed: u64, total: Option<u64>| {
                        let _ = progress.try_send(ToolInstallProgress {
                            id: id.clone(),
                            operation,
                            phase: phase.into(),
                            completed,
                            total,
                        });
                    };
                    report(
                        match operation {
                            ToolOperation::InstallManaged | ToolOperation::InstallNix => {
                                "preparing"
                            }
                            ToolOperation::UninstallManaged | ToolOperation::UninstallNix => {
                                "uninstalling"
                            }
                        },
                        0,
                        None,
                    );
                    let result = match operation {
                        ToolOperation::InstallManaged => {
                            install_tool(&definition, &root, &cancellations, &mut report, None)
                        }
                        ToolOperation::UninstallManaged => {
                            uninstall_managed_tool(&definition, &root).map(|()| None)
                        }
                        ToolOperation::InstallNix => install_nix_lsp_profile(
                            &definition,
                            &root,
                            nix.as_deref().expect("Nix install request has executable"),
                            &cancellations,
                            &mut report,
                        ),
                        ToolOperation::UninstallNix => {
                            uninstall_nix_lsp_tool(&definition, &root).map(|()| None)
                        }
                    };
                    worker_busy.store(false, Ordering::Release);
                    if results
                        .send(ToolInstallResult {
                            id: result_id,
                            operation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("tool installer thread must start");
        Self {
            sender,
            receiver,
            progress_receiver,
            cancel,
            busy,
            thread: Some(thread),
        }
    }

    pub fn install(&self, definition: ToolDefinition) -> Result<(), ToolInstallerError> {
        if definition.install.is_none() {
            return Err(ToolInstallerError::Unavailable);
        }
        self.submit(InstallerRequest::Install { definition })
    }

    pub fn uninstall(&self, definition: ToolDefinition) -> Result<(), ToolInstallerError> {
        self.submit(InstallerRequest::Uninstall { definition })
    }

    pub fn install_nix(
        &self,
        definition: ToolDefinition,
        nix: PathBuf,
    ) -> Result<(), ToolInstallerError> {
        if nix_lsp_shell(&definition).is_none() {
            return Err(ToolInstallerError::Unavailable);
        }
        self.submit(InstallerRequest::InstallNix { definition, nix })
    }

    pub fn uninstall_nix(&self, definition: ToolDefinition) -> Result<(), ToolInstallerError> {
        if nix_lsp_shell(&definition).is_none() {
            return Err(ToolInstallerError::Unavailable);
        }
        self.submit(InstallerRequest::UninstallNix { definition })
    }

    fn submit(&self, request: InstallerRequest) -> Result<(), ToolInstallerError> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ToolInstallerError::Busy);
        }
        self.sender.try_send(request).map_err(|error| {
            self.busy.store(false, Ordering::Release);
            match error {
                TrySendError::Full(_) => ToolInstallerError::Busy,
                TrySendError::Disconnected(_) => ToolInstallerError::Stopped,
            }
        })
    }

    pub fn try_progress(&self) -> Result<Option<ToolInstallProgress>, ToolInstallerError> {
        match self.progress_receiver.try_recv() {
            Ok(progress) => Ok(Some(progress)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ToolInstallerError::Stopped),
        }
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

const MAXIMUM_TOOL_VERSION_BYTES: u64 = 1024;
const MAXIMUM_TOOL_VERSION_CHARS: usize = 160;

#[derive(Clone, Debug)]
pub struct ToolVersionRequest {
    pub id: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub current_directory: Option<PathBuf>,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct ToolVersionResult {
    pub id: String,
    pub result: Result<String, String>,
}

enum ToolVersionCommand {
    Probe(ToolVersionRequest),
    Shutdown,
}

/// Bounded, shell-free version inspection for the tool-manager presentation.
/// It never decides whether a tool is executable; launch still uses canonical
/// preflight immediately before the protocol process starts.
pub struct ToolVersionService {
    sender: SyncSender<ToolVersionCommand>,
    receiver: Receiver<ToolVersionResult>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Default for ToolVersionService {
    fn default() -> Self {
        Self::start()
    }
}

impl ToolVersionService {
    #[must_use]
    pub fn start() -> Self {
        let (sender, requests) = mpsc::sync_channel(32);
        let (results, receiver) = mpsc::sync_channel(32);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("dust-tool-version-probe".into())
            .spawn(move || {
                while !worker_cancel.load(Ordering::Acquire) {
                    let Ok(command) = requests.recv() else {
                        break;
                    };
                    let ToolVersionCommand::Probe(request) = command else {
                        break;
                    };
                    let id = request.id.clone();
                    let result = probe_tool_version(request, &worker_cancel);
                    if worker_cancel.load(Ordering::Acquire)
                        || results.send(ToolVersionResult { id, result }).is_err()
                    {
                        break;
                    }
                }
            })
            .expect("tool-version worker must start");
        Self {
            sender,
            receiver,
            cancel,
            thread: Some(thread),
        }
    }

    pub fn probe(&self, request: ToolVersionRequest) -> Result<(), String> {
        self.sender
            .try_send(ToolVersionCommand::Probe(request))
            .map_err(|error| match error {
                TrySendError::Full(_) => "tool-version probe queue is full".into(),
                TrySendError::Disconnected(_) => "tool-version probe worker stopped".into(),
            })
    }

    pub fn try_result(&self) -> Option<ToolVersionResult> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for ToolVersionService {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        let _ = self.sender.try_send(ToolVersionCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
fn drain_version_pipe<Fd: std::os::fd::AsFd>(
    pipe: &Fd,
    output: &mut Vec<u8>,
    oversized: &mut bool,
) -> Result<bool, String> {
    let mut buffer = [0_u8; 512];
    loop {
        match nix::unistd::read(pipe, &mut buffer) {
            Ok(0) => return Ok(true),
            Ok(read) => {
                let remaining = (MAXIMUM_TOOL_VERSION_BYTES as usize)
                    .saturating_add(1)
                    .saturating_sub(output.len());
                output.extend_from_slice(&buffer[..read.min(remaining)]);
                if read > remaining || output.len() > MAXIMUM_TOOL_VERSION_BYTES as usize {
                    *oversized = true;
                }
            }
            Err(nix::errno::Errno::EAGAIN) => return Ok(false),
            Err(nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(format!("cannot read version probe output: {error}")),
        }
    }
}

#[cfg(unix)]
fn probe_tool_version(
    request: ToolVersionRequest,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    let mut command = Command::new(&request.executable);
    command
        .args(&request.arguments)
        .envs(&request.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = &request.current_directory {
        command.current_dir(directory);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start version probe: {error}"))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("version probe stdout was unavailable".into());
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("version probe stderr was unavailable".into());
    };
    for pipe in [
        &stdout as &dyn std::os::fd::AsFd,
        &stderr as &dyn std::os::fd::AsFd,
    ] {
        let configured = fcntl(pipe, FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .map_err(|error| format!("cannot inspect version probe pipe: {error}"))
            .and_then(|flags| {
                fcntl(pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
                    .map(|_| ())
                    .map_err(|error| format!("cannot bound version probe pipe: {error}"))
            });
        if let Err(error) = configured {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut oversized = false;
    let deadline = Instant::now() + request.timeout.min(Duration::from_secs(30));
    let status = loop {
        if !stdout_closed {
            match drain_version_pipe(&stdout, &mut stdout_bytes, &mut oversized) {
                Ok(closed) => stdout_closed = closed,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        if !stderr_closed {
            match drain_version_pipe(&stderr, &mut stderr_bytes, &mut oversized) {
                Ok(closed) => stderr_closed = closed,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        if cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("version probe cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("version probe timed out".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("version probe failed: {error}"));
            }
        }
    };
    let close_deadline = Instant::now() + Duration::from_millis(125);
    while !(stdout_closed && stderr_closed) && Instant::now() < close_deadline {
        if !stdout_closed {
            stdout_closed = drain_version_pipe(&stdout, &mut stdout_bytes, &mut oversized)?;
        }
        if !stderr_closed {
            stderr_closed = drain_version_pipe(&stderr, &mut stderr_bytes, &mut oversized)?;
        }
        if !(stdout_closed && stderr_closed) {
            thread::sleep(Duration::from_millis(5));
        }
    }
    if !(stdout_closed && stderr_closed) {
        return Err("version probe output remained open after process exit".into());
    }
    let bytes = if stdout_bytes.is_empty() {
        &stderr_bytes
    } else {
        &stdout_bytes
    };
    if !status.success() {
        let diagnostic =
            String::from_utf8_lossy(&bytes[..bytes.len().min(MAXIMUM_TOOL_VERSION_BYTES as usize)])
                .split_whitespace()
                .take(24)
                .collect::<Vec<_>>()
                .join(" ");
        return Err(if diagnostic.is_empty() {
            format!("version probe exited with {status}")
        } else {
            format!(
                "version probe exited with {status}: {diagnostic}{}",
                if oversized { " (output truncated)" } else { "" }
            )
        });
    }
    if oversized {
        return Err("version probe output exceeded its limit".into());
    }
    let version = String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if version.is_empty() {
        return Err("version probe produced no output".into());
    }
    Ok(version.chars().take(MAXIMUM_TOOL_VERSION_CHARS).collect())
}

#[cfg(not(unix))]
fn probe_tool_version(
    _request: ToolVersionRequest,
    _cancelled: &AtomicBool,
) -> Result<String, String> {
    Err("bounded tool-version probes are not qualified on this platform".into())
}

#[derive(Clone, Debug)]
pub struct NixWorkspaceResult {
    pub generation: u64,
    pub result: Result<PathBuf, String>,
}

enum NixWorkspaceRequest {
    Prepare { generation: u64 },
    Shutdown,
}

/// Writes Dust's pinned generic compatibility flake in the private tool store
/// without ever evaluating a workspace-owned flake. Nix owns its cache/store closure.
pub struct NixWorkspaceService {
    next_generation: u64,
    sender: SyncSender<NixWorkspaceRequest>,
    receiver: Receiver<NixWorkspaceResult>,
    thread: Option<JoinHandle<()>>,
}

impl Default for NixWorkspaceService {
    fn default() -> Self {
        Self::start()
    }
}

impl NixWorkspaceService {
    #[must_use]
    pub fn start() -> Self {
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("dust-nix-lsp-workspace".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let NixWorkspaceRequest::Prepare { generation } = request else {
                        break;
                    };
                    let result = prepare_nix_lsp_workspace();
                    if results
                        .send(NixWorkspaceResult { generation, result })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("Nix workspace worker must start");
        Self {
            next_generation: 1,
            sender,
            receiver,
            thread: Some(thread),
        }
    }

    pub fn prepare(&mut self) -> Result<u64, String> {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        self.sender
            .try_send(NixWorkspaceRequest::Prepare { generation })
            .map_err(|error| -> String {
                match error {
                    TrySendError::Full(_) => {
                        "Nix LSP workspace preparation is already pending".into()
                    }
                    TrySendError::Disconnected(_) => "Nix LSP workspace worker stopped".into(),
                }
            })?;
        Ok(generation)
    }

    #[must_use]
    pub fn try_result(&self) -> Option<NixWorkspaceResult> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for NixWorkspaceService {
    fn drop(&mut self) {
        let _ = self.sender.send(NixWorkspaceRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const MAXIMUM_TOOL_DOWNLOAD_BYTES: usize = 512 * 1024 * 1024;
const MAXIMUM_TOOL_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
const MAXIMUM_TOOL_ARCHIVE_ENTRIES: usize = 16_384;
const TOOL_INSTALL_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Clone, Copy)]
struct BootstrapArtifact {
    url: &'static str,
    sha256: &'static str,
    archive: ToolArchiveFormat,
    executable_path: &'static str,
}

fn relative_catalog_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("tool catalog contains an unsafe archive path".into());
    }
    Ok(path.to_path_buf())
}

fn validate_tool_id(value: &str) -> Result<(), String> {
    let path = relative_catalog_path(value)?;
    if path.components().count() != 1
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("tool ID is not a safe private-store directory name".into());
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Dust tool store must be absolute".into());
    }
    fs::create_dir_all(path).map_err(|error| format!("cannot create tool store: {error}"))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect tool store: {error}"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("Dust tool store is not a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot protect tool store: {error}"))?;
    }
    Ok(())
}

fn write_private_workspace_file(root: &Path, name: &str, content: &str) -> Result<(), String> {
    let destination = root.join(name);
    if destination.exists() {
        let metadata = fs::symlink_metadata(&destination)
            .map_err(|error| format!("cannot inspect managed Nix workspace file: {error}"))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err("managed Nix workspace file is not a regular file".into());
        }
        if metadata.len() > 1024 * 1024 {
            return Err("managed Nix workspace file exceeds its byte ceiling".into());
        }
        let existing = fs::read(&destination)
            .map_err(|error| format!("cannot read managed Nix workspace file: {error}"))?;
        if existing == content.as_bytes() {
            return Ok(());
        }
    }
    let staging = root.join(format!(
        ".{name}-{}",
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| format!("cannot stage managed Nix workspace file: {error}"))?;
    let result = (|| {
        file.write_all(content.as_bytes())
            .map_err(|error| format!("cannot write managed Nix workspace file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync managed Nix workspace file: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("cannot protect managed Nix workspace file: {error}"))?;
        }
        fs::rename(&staging, &destination)
            .map_err(|error| format!("cannot activate managed Nix workspace file: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staging);
    }
    result
}

fn prepare_nix_lsp_workspace() -> Result<PathBuf, String> {
    prepare_nix_lsp_workspace_at(&nix_lsp_workspace_root())
}

fn prepare_nix_lsp_workspace_at(root: &Path) -> Result<PathBuf, String> {
    ensure_private_directory(root)?;
    ensure_private_directory(&root.join("profiles"))?;
    write_private_workspace_file(root, "flake.nix", BUNDLED_NIX_LSP_FLAKE)?;
    write_private_workspace_file(root, "flake.lock", BUNDLED_NIX_LSP_LOCK)?;
    Ok(root.to_path_buf())
}

fn checked_https_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "tool catalog has an invalid artifact URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("tool artifact URLs must be anonymous HTTPS URLs".into());
    }
    Ok(url)
}

type ToolProgressReporter = dyn FnMut(&str, u64, Option<u64>);

fn download_checked(
    url: &str,
    expected_sha256: &str,
    report: &mut ToolProgressReporter,
) -> Result<Vec<u8>, String> {
    if !valid_digest(expected_sha256) {
        return Err("tool catalog has an invalid artifact checksum".into());
    }
    let url = checked_https_url(url)?;
    let client = Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(Policy::limited(3))
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| format!("cannot create HTTPS client: {error}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("tool artifact download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "tool artifact download returned {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAXIMUM_TOOL_DOWNLOAD_BYTES as u64)
    {
        return Err("tool artifact exceeds the download ceiling".into());
    }
    let total = response.content_length();
    report("downloading", 0, total);
    let mut response = response.take(MAXIMUM_TOOL_DOWNLOAD_BYTES.saturating_add(1) as u64);
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("cannot read tool artifact: {error}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        report(
            "downloading",
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            total,
        );
    }
    if bytes.len() > MAXIMUM_TOOL_DOWNLOAD_BYTES {
        return Err("tool artifact exceeds the download ceiling".into());
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err("tool artifact checksum did not match the bundled catalog".into());
    }
    Ok(bytes)
}

fn copy_limited_with_budget(
    mut reader: impl Read,
    mut writer: impl Write,
    remaining: &mut u64,
) -> Result<(), String> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot read tool archive: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        let read = u64::try_from(read).unwrap_or(u64::MAX);
        if read > *remaining {
            return Err("tool archive exceeds the unpacked-size ceiling".into());
        }
        writer
            .write_all(&buffer[..usize::try_from(read).unwrap_or(0)])
            .map_err(|error| format!("cannot write installed tool: {error}"))?;
        *remaining = remaining.saturating_sub(read);
    }
}

fn copy_limited(reader: impl Read, writer: impl Write) -> Result<(), String> {
    let mut remaining = MAXIMUM_TOOL_UNPACKED_BYTES;
    copy_limited_with_budget(reader, writer, &mut remaining)
}

fn prepare_executable(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect installed tool: {error}"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("tool archive did not contain a regular executable".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot mark installed tool executable: {error}"))?;
    }
    Ok(())
}

fn extract_one(
    bytes: &[u8],
    archive: ToolArchiveFormat,
    expected_path: &str,
    output: &Path,
) -> Result<(), String> {
    let expected_path = relative_catalog_path(expected_path)?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|error| format!("cannot create installed tool: {error}"))?;
    let found = match archive {
        ToolArchiveFormat::Raw => {
            if expected_path.components().count() != 1 {
                return Err("a raw tool artifact must contain one executable name".into());
            }
            copy_limited(Cursor::new(bytes), &mut file)?;
            true
        }
        ToolArchiveFormat::Gzip => {
            if expected_path.components().count() != 1 {
                return Err("a gzip tool artifact must contain one executable name".into());
            }
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            copy_limited(decoder, &mut file)?;
            true
        }
        ToolArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
                .map_err(|error| format!("cannot open ZIP tool artifact: {error}"))?;
            if zip.len() > MAXIMUM_TOOL_ARCHIVE_ENTRIES {
                return Err("tool archive has too many entries".into());
            }
            let mut entry = zip
                .by_name(expected_path.to_str().unwrap_or_default())
                .map_err(|_| "tool archive is missing its expected executable")?;
            if entry.is_dir() {
                return Err("tool archive executable is a directory".into());
            }
            copy_limited(&mut entry, &mut file)?;
            true
        }
        ToolArchiveFormat::TarGzip => {
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            let mut tar = tar::Archive::new(decoder);
            let mut found = false;
            for entry in tar
                .entries()
                .map_err(|error| format!("cannot read tar tool artifact: {error}"))?
                .take(MAXIMUM_TOOL_ARCHIVE_ENTRIES.saturating_add(1))
            {
                let mut entry = entry.map_err(|error| format!("cannot read tar entry: {error}"))?;
                let path = entry
                    .path()
                    .map_err(|error| format!("cannot read tar path: {error}"))?;
                if path == expected_path {
                    if !entry.header().entry_type().is_file() {
                        return Err("tool archive executable is not a regular file".into());
                    }
                    copy_limited(&mut entry, &mut file)?;
                    found = true;
                    break;
                }
            }
            found
        }
    };
    if !found {
        return Err("tool archive is missing its expected executable".into());
    }
    drop(file);
    prepare_executable(output)
}

fn extract_tar_gzip_tree(bytes: &[u8], prefix: &str, output: &Path) -> Result<(), String> {
    let prefix = relative_catalog_path(prefix)?;
    if prefix.components().count() != 1 {
        return Err("runtime archive prefix must contain one directory".into());
    }
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut tar = tar::Archive::new(decoder);
    let mut remaining = MAXIMUM_TOOL_UNPACKED_BYTES;
    let mut count = 0usize;
    for entry in tar
        .entries()
        .map_err(|error| format!("cannot read runtime tar archive: {error}"))?
    {
        count = count.saturating_add(1);
        if count > MAXIMUM_TOOL_ARCHIVE_ENTRIES {
            return Err("runtime archive has too many entries".into());
        }
        let mut entry = entry.map_err(|error| format!("cannot read runtime tar entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("cannot read runtime tar path: {error}"))?;
        let Ok(relative) = path.strip_prefix(&prefix) else {
            return Err("runtime archive contains an entry outside its expected prefix".into());
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let relative_text = relative
            .to_str()
            .ok_or("runtime archive path is not UTF-8")?;
        let relative = relative_catalog_path(relative_text)?;
        let destination = output.join(relative);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("cannot create runtime directory: {error}"))?;
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err("runtime archive contains a non-regular entry".into());
        }
        let parent = destination
            .parent()
            .ok_or("runtime archive file has no parent")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create runtime directory: {error}"))?;
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let executable = entry.header().mode().unwrap_or(0) & 0o111 != 0;
            options.mode(if executable { 0o700 } else { 0o600 });
        }
        let mut file = options
            .open(&destination)
            .map_err(|error| format!("cannot create runtime file: {error}"))?;
        copy_limited_with_budget(&mut entry, &mut file, &mut remaining)?;
    }
    Ok(())
}

fn replace_managed_directory(staging: &Path, destination: &Path) -> Result<(), String> {
    let backup = destination.with_file_name(format!(
        ".replace-{}-{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("tool"),
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let had_previous = if destination.exists() {
        let metadata = fs::symlink_metadata(destination)
            .map_err(|error| format!("cannot inspect existing tool: {error}"))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("managed tool destination is not a real directory".into());
        }
        fs::rename(destination, &backup)
            .map_err(|error| format!("cannot stage previous tool for replacement: {error}"))?;
        true
    } else {
        false
    };
    if let Err(error) = fs::rename(staging, destination) {
        if had_previous {
            let _ = fs::rename(&backup, destination);
        }
        return Err(format!("cannot activate tool: {error}"));
    }
    if had_previous {
        fs::remove_dir_all(&backup).map_err(|error| {
            format!("tool activated but previous version cleanup failed: {error}")
        })?;
    }
    Ok(())
}

const MANAGED_RUNTIME_MARKER: &str = ".dust-runtime-sha256";
const MAXIMUM_ELF_INTERPRETER_BYTES: u64 = 4096;
const MAXIMUM_ELF_PROGRAM_HEADERS: u16 = 256;

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

/// Reads PT_INTERP without executing an untrusted artifact or loading the whole file.
fn elf_interpreter(path: &Path) -> Result<Option<String>, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("cannot inspect ELF artifact: {error}"))?;
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot open ELF artifact: {error}"))?;
    let mut header = [0_u8; 64];
    let read = file
        .read(&mut header)
        .map_err(|error| format!("cannot read ELF header: {error}"))?;
    if read < 4 || &header[..4] != b"\x7fELF" {
        return Ok(None);
    }
    if read < 52 || header[5] != 1 {
        return Err("managed ELF uses an unsupported class or byte order".into());
    }
    let (program_offset, entry_size, entry_count, offset_field, size_field) = match header[4] {
        1 => (
            u64::from(read_u32_le(&header, 28).ok_or("invalid ELF32 header")?),
            read_u16_le(&header, 42).ok_or("invalid ELF32 header")?,
            read_u16_le(&header, 44).ok_or("invalid ELF32 header")?,
            4_usize,
            16_usize,
        ),
        2 if read >= 64 => (
            read_u64_le(&header, 32).ok_or("invalid ELF64 header")?,
            read_u16_le(&header, 54).ok_or("invalid ELF64 header")?,
            read_u16_le(&header, 56).ok_or("invalid ELF64 header")?,
            8_usize,
            32_usize,
        ),
        _ => return Err("managed ELF uses an unsupported class or truncated header".into()),
    };
    if entry_count > MAXIMUM_ELF_PROGRAM_HEADERS
        || entry_size < u16::try_from(size_field + 8).unwrap_or(u16::MAX)
    {
        return Err("managed ELF program-header table is invalid".into());
    }
    let table_bytes = u64::from(entry_size)
        .checked_mul(u64::from(entry_count))
        .and_then(|size| program_offset.checked_add(size))
        .ok_or("managed ELF program-header table overflows")?;
    if table_bytes > metadata.len() {
        return Err("managed ELF program-header table exceeds the file".into());
    }
    let mut entry = vec![0_u8; usize::from(entry_size)];
    for index in 0..entry_count {
        let position = program_offset + u64::from(index) * u64::from(entry_size);
        file.seek(SeekFrom::Start(position))
            .and_then(|_| file.read_exact(&mut entry))
            .map_err(|error| format!("cannot read ELF program header: {error}"))?;
        if read_u32_le(&entry, 0) != Some(3) {
            continue;
        }
        let (offset, size) = if header[4] == 1 {
            (
                u64::from(read_u32_le(&entry, offset_field).ok_or("invalid ELF interpreter")?),
                u64::from(read_u32_le(&entry, size_field).ok_or("invalid ELF interpreter")?),
            )
        } else {
            (
                read_u64_le(&entry, offset_field).ok_or("invalid ELF interpreter")?,
                read_u64_le(&entry, size_field).ok_or("invalid ELF interpreter")?,
            )
        };
        if size == 0
            || size > MAXIMUM_ELF_INTERPRETER_BYTES
            || offset
                .checked_add(size)
                .is_none_or(|end| end > metadata.len())
        {
            return Err("managed ELF interpreter is invalid".into());
        }
        let mut bytes = vec![0_u8; usize::try_from(size).map_err(|_| "invalid ELF interpreter")?];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut bytes))
            .map_err(|error| format!("cannot read ELF interpreter: {error}"))?;
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        let interpreter =
            String::from_utf8(bytes).map_err(|_| "managed ELF interpreter is not UTF-8")?;
        if interpreter.is_empty() || interpreter.chars().any(char::is_control) {
            return Err("managed ELF interpreter is invalid".into());
        }
        return Ok(Some(interpreter));
    }
    Ok(None)
}

fn patch_nix_compatible_executable(
    path: &Path,
    runtime: &NixCompatibilityRuntime,
    cwd: &Path,
    cancellations: &Receiver<()>,
) -> Result<(), String> {
    let Some(interpreter) = elf_interpreter(path)? else {
        return Ok(());
    };
    if Path::new(&interpreter) == runtime.loader {
        return Ok(());
    }
    if interpreter.contains("ld-musl") {
        return Err(format!(
            "{} is dynamically linked against musl; Dust requires the verified GNU artifact on NixOS",
            path.display()
        ));
    }
    if interpreter.starts_with("/nix/store/") {
        return Err(format!(
            "{} references a Nix interpreter outside Dust's compatibility profile",
            path.display()
        ));
    }
    if !interpreter.contains("ld-linux") && !interpreter.ends_with("/ld.so") {
        return Err(format!(
            "{} uses unsupported ELF interpreter {interpreter}",
            path.display()
        ));
    }
    let arguments = vec![
        "--set-interpreter".into(),
        runtime.loader.display().to_string(),
        "--add-rpath".into(),
        runtime.library_path.clone(),
        path.display().to_string(),
    ];
    run_managed(
        &runtime.patchelf,
        &arguments,
        cwd,
        &BTreeMap::new(),
        cancellations,
        None,
    )
    .map_err(|error| {
        format!(
            "cannot apply Dust Nix compatibility to {}: {error}",
            path.display()
        )
    })?;
    if elf_interpreter(path)?.as_deref() != Some(runtime.loader.to_string_lossy().as_ref()) {
        return Err(format!(
            "Dust Nix compatibility did not update {}",
            path.display()
        ));
    }
    Ok(())
}

fn patch_nix_compatibility_tree(
    root: &Path,
    runtime: &NixCompatibilityRuntime,
    cancellations: &Receiver<()>,
) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = 0usize;
    let mut bytes = 0u64;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("cannot inspect managed tool tree: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("cannot inspect managed tool entry: {error}"))?;
            entries = entries.saturating_add(1);
            if entries > MAXIMUM_TOOL_ARCHIVE_ENTRIES {
                return Err("managed tool tree has too many entries".into());
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| format!("cannot inspect managed tool entry: {error}"))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.file_type().is_dir() {
                stack.push(entry.path());
            } else if metadata.file_type().is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > MAXIMUM_TOOL_UNPACKED_BYTES {
                    return Err("managed tool tree exceeds its byte ceiling".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 != 0 {
                        patch_nix_compatible_executable(
                            &entry.path(),
                            runtime,
                            root,
                            cancellations,
                        )?;
                    }
                }
            } else {
                return Err("managed tool tree contains a special file".into());
            }
        }
    }
    Ok(())
}

fn runtime_artifact(name: &str) -> Result<BootstrapArtifact, String> {
    match (name, std::env::consts::ARCH) {
        ("uv", "x86_64") => Ok(BootstrapArtifact {
            url: "https://github.com/astral-sh/uv/releases/download/0.12.5/uv-x86_64-unknown-linux-musl.tar.gz",
            sha256: "a4742988791c9aeae68c78150d6cba762062ad2a47e53738c2779d2b596bfcdb",
            archive: ToolArchiveFormat::TarGzip,
            executable_path: "uv-x86_64-unknown-linux-musl/uv",
        }),
        ("bun", "x86_64") => Ok(BootstrapArtifact {
            url: "https://github.com/oven-sh/bun/releases/download/bun-v1.3.14/bun-linux-x64-baseline.zip",
            sha256: "a063908ae08b7852ca10939bbdc6ceed3ddabce8fb9402dce83d65d73b36e6c7",
            archive: ToolArchiveFormat::Zip,
            executable_path: "bun-linux-x64-baseline/bun",
        }),
        ("go", "x86_64") => Ok(BootstrapArtifact {
            url: "https://go.dev/dl/go1.24.12.linux-amd64.tar.gz",
            sha256: "bddf8e653c82429aea7aec2520774e79925d4bb929fe20e67ecc00dd5af44c50",
            archive: ToolArchiveFormat::TarGzip,
            executable_path: "go/bin/go",
        }),
        ("rustup", "x86_64") => Ok(BootstrapArtifact {
            url: "https://static.rust-lang.org/rustup/archive/1.28.0/x86_64-unknown-linux-gnu/rustup-init",
            sha256: "c8d03f559a2335693379e1d3eaee76622b2a6580807e63bcd61faea709b9f664",
            archive: ToolArchiveFormat::Raw,
            executable_path: "rustup-init",
        }),
        _ => Err(format!(
            "Dust has no managed {name} runtime for Linux {}",
            std::env::consts::ARCH
        )),
    }
}

fn fhs_glibc_loader_available() -> bool {
    [
        "/lib64/ld-linux-x86-64.so.2",
        "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    ]
    .iter()
    .any(|path| Path::new(path).is_file())
}

fn managed_runtime(
    root: &Path,
    name: &str,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<PathBuf, String> {
    if cancellations.try_recv().is_ok() {
        return Err("tool installation cancelled".into());
    }
    let artifact = runtime_artifact(name)?;
    let runtime = root.join(".runtimes").join(name);
    let executable = runtime.join("bin").join(name);
    let marker = runtime.join(MANAGED_RUNTIME_MARKER);
    if executable.is_file()
        && fs::read_to_string(&marker)
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case(artifact.sha256))
    {
        if let Some(compatibility) = compatibility {
            patch_nix_compatible_executable(&executable, compatibility, &runtime, cancellations)?;
        }
        return Ok(executable);
    }
    fs::create_dir_all(runtime.parent().ok_or("runtime has no parent")?)
        .map_err(|error| format!("cannot create runtime directory: {error}"))?;
    let staging = root.join(format!(
        ".runtime-{name}-{}",
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&staging).map_err(|error| format!("cannot stage runtime: {error}"))?;
    let result = (|| {
        let bytes = download_checked(artifact.url, artifact.sha256, report)?;
        report("extracting runtime", 0, None);
        let bin = staging.join("bin");
        let staged_executable = bin.join(name);
        if name == "go" && artifact.archive == ToolArchiveFormat::TarGzip {
            extract_tar_gzip_tree(&bytes, "go", &staging)?;
            prepare_executable(&staged_executable)?;
        } else {
            fs::create_dir(&bin).map_err(|error| format!("cannot create runtime bin: {error}"))?;
            extract_one(
                &bytes,
                artifact.archive,
                artifact.executable_path,
                &staged_executable,
            )?;
        }
        if let Some(compatibility) = compatibility {
            patch_nix_compatibility_tree(&staging, compatibility, cancellations)?;
        }
        fs::write(staging.join(MANAGED_RUNTIME_MARKER), artifact.sha256)
            .map_err(|error| format!("cannot record managed runtime identity: {error}"))?;
        replace_managed_directory(&staging, &runtime)?;
        Ok(runtime.join("bin").join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn managed_bun_runtime(
    root: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<PathBuf, String> {
    // NixOS uses Dust's exact checked GNU Bun artifact so PATH cannot silently
    // change the runtime processed by the private compatibility environment.
    if compatibility.is_none()
        && let Some(bun) = discover_executable("bun")
    {
        let bun = fs::canonicalize(&bun)
            .map_err(|error| format!("cannot canonicalize Bun runtime: {error}"))?;
        if bun.is_file() && bun.is_absolute() {
            report("verifying Bun runtime", 0, None);
            run_managed(
                &bun,
                &["--version".into()],
                root,
                &BTreeMap::from([("HOME".into(), root.display().to_string())]),
                cancellations,
                None,
            )
            .map_err(|error| format!("configured Bun runtime is not runnable: {error}"))?;
            return Ok(bun);
        }
    }
    if compatibility.is_none() && !fhs_glibc_loader_available() {
        return Err(
            "no directly runnable Bun runtime and this Linux host lacks the FHS glibc loader"
                .into(),
        );
    }
    managed_runtime(root, "bun", cancellations, report, compatibility)
}

fn run_managed(
    program: &Path,
    arguments: &[String],
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    cancellations: &Receiver<()>,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<(), String> {
    let mut effective_environment = environment.clone();
    if let Some(runtime) = compatibility {
        effective_environment.extend(runtime.environment());
        effective_environment.insert("DUST_NIX_LD".into(), runtime.nix_ld.display().to_string());
        let mut paths = env::split_paths(&runtime.runtime_path).collect::<Vec<_>>();
        if let Some(existing) = effective_environment.get("PATH") {
            paths.extend(env::split_paths(existing));
        }
        let path = env::join_paths(paths).map_err(|_| "Dust Nix compatibility PATH is invalid")?;
        effective_environment.insert("PATH".into(), path.to_string_lossy().into_owned());
    }
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .envs(&effective_environment)
        .spawn()
        .map_err(|error| format!("failed to launch managed {}: {error}", program.display()))?;
    let deadline = Instant::now() + TOOL_INSTALL_TIMEOUT;
    loop {
        if cancellations.try_recv().is_ok() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("tool installation cancelled".into());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("tool installation exceeded the 15-minute ceiling".into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "managed installer `{}` exited with {}",
                    program.display(),
                    status
                        .code()
                        .map_or_else(|| "signal".into(), |code| code.to_string())
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("failed to wait for managed installer: {error}")),
        }
    }
}

fn install_javascript_package(
    root: &Path,
    packages: &[String],
    staging: &Path,
    output: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<(), String> {
    let bun = managed_bun_runtime(root, cancellations, report, compatibility)?;
    let bin = staging.join("bin");
    let node = compatibility
        .and_then(|runtime| runtime.executable("node"))
        .unwrap_or_else(|| bun.clone());
    #[cfg(unix)]
    std::os::unix::fs::symlink(&node, bin.join("node"))
        .map_err(|error| format!("cannot provide private JavaScript runtime: {error}"))?;
    let arguments = std::iter::once("install".into())
        .chain(std::iter::once("--global".into()))
        .chain(std::iter::once("--ignore-scripts".into()))
        .chain(std::iter::once("--no-progress".into()))
        .chain(std::iter::once("--no-summary".into()))
        .chain(packages.iter().cloned())
        .collect::<Vec<_>>();
    let environment = BTreeMap::from([
        ("HOME".into(), staging.display().to_string()),
        ("BUN_INSTALL".into(), staging.display().to_string()),
        (
            "BUN_INSTALL_CACHE_DIR".into(),
            staging.join("bun-cache").display().to_string(),
        ),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]);
    report("resolving JavaScript packages", 0, None);
    run_managed(
        &bun,
        &arguments,
        staging,
        &environment,
        cancellations,
        compatibility,
    )?;
    if !output.is_file() {
        return Err("managed Bun installation did not produce its executable".into());
    }
    Ok(())
}

fn install_uv_tool(
    definition: &ToolDefinition,
    root: &Path,
    packages: &[String],
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<Option<PathBuf>, String> {
    let uv = managed_runtime(root, "uv", cancellations, report, compatibility)?;
    let destination = root.join(&definition.id);
    if destination.exists() {
        let metadata = fs::symlink_metadata(&destination)
            .map_err(|error| format!("cannot inspect managed tool: {error}"))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("managed tool destination is not a real directory".into());
        }
        fs::remove_dir_all(&destination)
            .map_err(|error| format!("cannot replace tool: {error}"))?;
    }
    fs::create_dir(&destination).map_err(|error| format!("cannot create managed tool: {error}"))?;
    let result = (|| {
        let bin = destination.join("bin");
        let tools = destination.join("uv-tools");
        fs::create_dir(&bin).map_err(|error| format!("cannot create tool bin: {error}"))?;
        let arguments = std::iter::once("tool".into())
            .chain(std::iter::once("install".into()))
            .chain(std::iter::once("--reinstall".into()))
            .chain(packages.iter().cloned())
            .collect::<Vec<_>>();
        let environment = BTreeMap::from([
            ("HOME".into(), destination.display().to_string()),
            ("UV_TOOL_DIR".into(), tools.display().to_string()),
            ("UV_TOOL_BIN_DIR".into(), bin.display().to_string()),
            (
                "UV_CACHE_DIR".into(),
                destination.join("uv-cache").display().to_string(),
            ),
            (
                "UV_PYTHON_INSTALL_DIR".into(),
                destination.join("python").display().to_string(),
            ),
            ("UV_NO_PROGRESS".into(), "1".into()),
        ]);
        report("resolving Python tool", 0, None);
        run_managed(
            &uv,
            &arguments,
            &destination,
            &environment,
            cancellations,
            compatibility,
        )?;
        if let Some(compatibility) = compatibility {
            patch_nix_compatibility_tree(&destination, compatibility, cancellations)?;
        }
        let executable = bin.join(&definition.executable);
        if !executable.is_file() {
            return Err("managed uv installation did not produce its executable".into());
        }
        write_managed_tool_version(&destination, definition)?;
        Ok(Some(executable))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&destination);
    }
    result
}

fn install_rustup_component(
    definition: &ToolDefinition,
    root: &Path,
    component: &str,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<Option<PathBuf>, String> {
    if component.is_empty()
        || component.len() > 128
        || !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("tool catalog has an invalid rustup component".into());
    }
    let initializer = managed_runtime(root, "rustup", cancellations, report, compatibility)?;
    let runtime = root.join(".runtimes/rustup");
    let rustup_home = runtime.join("rustup-home");
    let cargo_home = runtime.join("cargo-home");
    let environment = BTreeMap::from([
        ("HOME".into(), runtime.display().to_string()),
        ("RUSTUP_HOME".into(), rustup_home.display().to_string()),
        ("CARGO_HOME".into(), cargo_home.display().to_string()),
        ("RUSTUP_INIT_SKIP_PATH_CHECK".into(), "yes".into()),
    ]);
    let rustup = cargo_home.join("bin/rustup");
    if !rustup.is_file() {
        let arguments = vec![
            "-y".into(),
            "--no-modify-path".into(),
            "--profile".into(),
            "minimal".into(),
            "--default-toolchain".into(),
            "stable".into(),
        ];
        report("installing Rust toolchain", 0, None);
        run_managed(
            &initializer,
            &arguments,
            &runtime,
            &environment,
            cancellations,
            compatibility,
        )?;
    }
    if !rustup.is_file() {
        return Err("managed rustup initialization did not produce rustup".into());
    }
    let arguments = vec![
        "component".into(),
        "add".into(),
        component.into(),
        "--toolchain".into(),
        "stable".into(),
    ];
    report("installing Rust component", 0, None);
    run_managed(
        &rustup,
        &arguments,
        &runtime,
        &environment,
        cancellations,
        compatibility,
    )?;
    if let Some(compatibility) = compatibility {
        patch_nix_compatibility_tree(&runtime, compatibility, cancellations)?;
    }
    let target = rustup_home
        .join("toolchains/stable-x86_64-unknown-linux-gnu/bin")
        .join(&definition.executable);
    prepare_executable(&target)?;
    let staging = root.join(format!(
        ".tool-{}-{}",
        definition.id,
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&staging).map_err(|error| format!("cannot stage rust component: {error}"))?;
    let result = (|| {
        let bin = staging.join("bin");
        fs::create_dir(&bin).map_err(|error| format!("cannot create tool bin: {error}"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, bin.join(&definition.executable))
            .map_err(|error| format!("cannot link managed Rust component: {error}"))?;
        #[cfg(not(unix))]
        fs::copy(&target, bin.join(&definition.executable))
            .map_err(|error| format!("cannot copy managed Rust component: {error}"))?;
        write_managed_tool_version(&staging, definition)?;
        let destination = root.join(&definition.id);
        replace_managed_directory(&staging, &destination)?;
        Ok(Some(destination.join("bin").join(&definition.executable)))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn probe_downloaded_tool(
    executable: &Path,
    arguments: &[String],
    workspace: &Path,
    cancellations: &Receiver<()>,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<(), String> {
    if arguments.is_empty() {
        return Ok(());
    }
    run_managed(
        executable,
        arguments,
        workspace,
        &BTreeMap::from([("HOME".into(), workspace.display().to_string())]),
        cancellations,
        compatibility,
    )
    .map_err(|error| format!("downloaded artifact is not runnable on this Linux host: {error}"))
}

fn uninstall_managed_tool(definition: &ToolDefinition, root: &Path) -> Result<(), String> {
    validate_tool_id(&definition.id)?;
    ensure_private_directory(root)?;
    let destination = root.join(&definition.id);
    let Ok(metadata) = fs::symlink_metadata(&destination) else {
        return Ok(());
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("managed tool destination is not a real directory".into());
    }
    let removed = root.join(format!(
        ".removed-{}-{}",
        definition.id,
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    fs::rename(&destination, &removed)
        .map_err(|error| format!("cannot deactivate managed tool: {error}"))?;
    fs::remove_dir_all(&removed)
        .map_err(|error| format!("tool deactivated but cleanup failed: {error}"))?;
    Ok(())
}

fn nix_installer_environment() -> BTreeMap<String, String> {
    [
        "HOME",
        "PATH",
        "USER",
        "LOGNAME",
        "NIX_REMOTE",
        "SSL_CERT_FILE",
        "NIX_SSL_CERT_FILE",
    ]
    .into_iter()
    .filter_map(|key| env::var(key).ok().map(|value| (key.into(), value)))
    .collect()
}

fn install_nix_lsp_profile(
    definition: &ToolDefinition,
    root: &Path,
    nix: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
) -> Result<Option<PathBuf>, String> {
    let profile_id = nix_lsp_shell(definition)
        .ok_or("this LSP has no verified artifact for the Dust Nix runtime")?;
    let workspace = prepare_nix_lsp_workspace_at(&root.join("nix-lsp-workspace"))?;
    let profile = workspace.join("profiles").join(profile_id);
    let compatibility = match parse_nix_runtime_environment(&profile) {
        Ok(compatibility) => compatibility,
        Err(_) => {
            // Migrate an old per-server nixpkgs/dev-shell profile before creating
            // the generic runtime out-link. The verified artifact is left untouched
            // until its replacement has passed extraction and probing.
            uninstall_nix_lsp_profile(definition, root)?;
            report("realizing Dust Nix compatibility", 0, None);
            let arguments = vec![
                "build".into(),
                "--no-update-lock-file".into(),
                "--no-write-lock-file".into(),
                "--out-link".into(),
                profile.display().to_string(),
                format!(
                    "path:{}#{NIX_COMPATIBILITY_FLAKE_PACKAGE}",
                    workspace.display()
                ),
            ];
            run_managed(
                nix,
                &arguments,
                &workspace,
                &nix_installer_environment(),
                cancellations,
                None,
            )?;
            parse_nix_runtime_environment(&profile)?
        }
    };
    report("installing verified upstream artifact", 0, None);
    install_tool(
        definition,
        root,
        cancellations,
        report,
        Some(&compatibility),
    )
}

fn nix_profile_installed_at(profile: &Path) -> bool {
    fs::symlink_metadata(profile).is_ok_and(|metadata| metadata.file_type().is_symlink())
        && fs::canonicalize(profile).is_ok()
}

fn uninstall_nix_lsp_profile(definition: &ToolDefinition, root: &Path) -> Result<(), String> {
    let shell =
        nix_lsp_shell(definition).ok_or("this LSP has no Dust Nix compatibility runtime")?;
    let profiles = root.join("nix-lsp-workspace/profiles");
    let Ok(metadata) = fs::symlink_metadata(&profiles) else {
        return Ok(());
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("Dust Nix environment store is not a real directory".into());
    }
    let generation_prefix = format!("{shell}-");
    let mut removed = 0usize;
    for entry in fs::read_dir(&profiles)
        .map_err(|error| format!("cannot read Dust Nix environments: {error}"))?
        .take(1024)
    {
        let entry =
            entry.map_err(|error| format!("cannot inspect Dust Nix environment: {error}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != shell && !(name.starts_with(&generation_prefix) && name.ends_with("-link")) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("cannot inspect Dust Nix environment link: {error}"))?;
        if !metadata.file_type().is_symlink() {
            return Err("Dust Nix environment entry is not a symlink".into());
        }
        fs::remove_file(entry.path())
            .map_err(|error| format!("cannot remove Dust Nix environment: {error}"))?;
        removed = removed.saturating_add(1);
    }
    if removed == 0 && nix_profile_installed_at(&profiles.join(shell)) {
        return Err("Dust Nix environment could not be removed".into());
    }
    Ok(())
}

fn uninstall_nix_lsp_tool(definition: &ToolDefinition, root: &Path) -> Result<(), String> {
    // Deactivate the executable first so a profile cleanup failure cannot leave a
    // partially removed tool launchable.
    uninstall_managed_tool(definition, root)?;
    uninstall_nix_lsp_profile(definition, root)
}

fn install_tool(
    definition: &ToolDefinition,
    root: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<Option<PathBuf>, String> {
    validate_tool_id(&definition.id)?;
    ensure_private_directory(root)?;
    let install = definition
        .install
        .as_ref()
        .ok_or("this tool has no portable bundled installer")?;
    if let ToolInstall::UvTool { packages } = install {
        return install_uv_tool(
            definition,
            root,
            packages,
            cancellations,
            report,
            compatibility,
        );
    }
    if let ToolInstall::RustupComponent { component } = install {
        return install_rustup_component(
            definition,
            root,
            component,
            cancellations,
            report,
            compatibility,
        );
    }
    let staging = root.join(format!(
        ".tool-{}-{}",
        definition.id,
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&staging).map_err(|error| format!("cannot stage tool installation: {error}"))?;
    let result = (|| {
        let bin = staging.join("bin");
        fs::create_dir(&bin).map_err(|error| format!("cannot create tool bin: {error}"))?;
        let output = bin.join(&definition.executable);
        match install {
            ToolInstall::Archive {
                url,
                sha256,
                archive,
                executable_path,
            } => {
                let bytes = download_checked(url, sha256, report)?;
                report("extracting", 0, None);
                extract_one(&bytes, *archive, executable_path, &output)?;
            }
            ToolInstall::NpmPackage { packages } => {
                install_javascript_package(
                    root,
                    packages,
                    &staging,
                    &output,
                    cancellations,
                    report,
                    compatibility,
                )?;
            }
            ToolInstall::GoPackage { package } => {
                let go = managed_runtime(root, "go", cancellations, report, compatibility)?;
                let arguments = vec!["install".into(), package.clone()];
                let environment = BTreeMap::from([
                    ("HOME".into(), staging.display().to_string()),
                    ("GOBIN".into(), bin.display().to_string()),
                    (
                        "GOPATH".into(),
                        staging.join("gopath").display().to_string(),
                    ),
                    (
                        "GOCACHE".into(),
                        staging.join("gocache").display().to_string(),
                    ),
                    ("GOPROXY".into(), "https://proxy.golang.org,direct".into()),
                    ("GOSUMDB".into(), "sum.golang.org".into()),
                ]);
                report("building Go tool", 0, None);
                run_managed(
                    &go,
                    &arguments,
                    &staging,
                    &environment,
                    cancellations,
                    compatibility,
                )?;
                prepare_executable(&output)?;
            }
            ToolInstall::UvTool { .. } | ToolInstall::RustupComponent { .. } => {
                unreachable!("handled before staging")
            }
        }
        if let Some(compatibility) = compatibility {
            report("applying Nix compatibility", 0, None);
            patch_nix_compatibility_tree(&staging, compatibility, cancellations)?;
        }
        if !matches!(install, ToolInstall::NpmPackage { .. }) {
            report("verifying", 0, None);
            probe_downloaded_tool(
                &output,
                &definition.probe_args,
                &staging,
                cancellations,
                compatibility,
            )?;
        }
        write_managed_tool_version(&staging, definition)?;
        let destination = root.join(&definition.id);
        replace_managed_directory(&staging, &destination)?;
        Ok(Some(destination.join("bin").join(&definition.executable)))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolInstallerError {
    #[error("another tool installation is already running")]
    Busy,
    #[error("tool installer stopped")]
    Stopped,
    #[error("this tool has no portable bundled installer")]
    Unavailable,
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use reqwest::Url;

    use super::{
        ToolArchiveFormat, ToolInstall, ToolInstaller, ToolKind, ToolOperation, ToolVersionRequest,
        bundled_tool_catalog, discover_executable, elf_interpreter, install_nix_lsp_profile,
        install_tool, managed_tool_installed_version_at, nix_lsp_launch_environment_at,
        nix_lsp_runtime_at, nix_lsp_shell, nixos_os_release_at, prepare_nix_lsp_workspace_at,
        probe_tool_version, relative_catalog_path, runtime_artifact, tool_launch_environment_at,
        uninstall_managed_tool, uninstall_nix_lsp_profile, validate_tool_id,
        write_managed_tool_version,
    };

    #[test]
    fn bundled_catalog_is_typed_and_has_real_language_and_debug_tools() {
        let catalog = bundled_tool_catalog();
        assert!(catalog.iter().any(|tool| tool.id == "rust-analyzer"));
        assert!(catalog.iter().any(|tool| tool.kind == ToolKind::Dap));
        assert!(catalog.iter().all(|tool| !tool.languages.is_empty()));
        assert!(
            catalog
                .iter()
                .filter(|tool| tool.kind == ToolKind::Lsp)
                .all(|tool| nix_lsp_shell(tool).is_some() && tool.install.is_some())
        );
        assert!(catalog.iter().any(|tool| matches!(
            tool.install,
            Some(ToolInstall::Archive { .. } | ToolInstall::NpmPackage { .. })
        )));
        assert!(catalog.iter().all(|tool| {
            tool.install.is_some() || matches!(tool.id.as_str(), "lldb-dap" | "js-debug-adapter")
        }));
    }

    #[test]
    fn rust_analyzer_catalog_exposes_distinct_exact_installable_pins() {
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .expect("rust-analyzer catalog entry");
        let versions = definition.managed_versions();
        assert_eq!(
            versions
                .iter()
                .map(|version| (
                    version.version.as_deref().unwrap_or_default(),
                    version.release.as_deref().unwrap_or_default(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("0.3.3025", "2026-08-24"),
                ("0.3.3016", "2026-08-17.4"),
                ("0.3.3008", "2026-08-10.1"),
                ("0.3.2997", "2026-08-03"),
                ("0.3.2989", "2026-07-27"),
            ]
        );
        assert!(versions.iter().all(|version| matches!(
            version.install,
            Some(ToolInstall::Archive { ref url, ref sha256, .. })
                if url.contains(version.release.as_deref().unwrap_or_default())
                    && super::valid_digest(sha256)
        )));
    }

    #[test]
    fn managed_tool_records_the_exact_activated_catalog_version() {
        let root = tempfile::tempdir().expect("tool store");
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .expect("rust-analyzer catalog entry")
            .managed_version("2026-08-10.1")
            .expect("exact pin");
        let destination = root.path().join(&definition.id);
        std::fs::create_dir(&destination).expect("managed tool directory");
        write_managed_tool_version(&destination, &definition).expect("version marker");
        assert_eq!(definition.version.as_deref(), Some("0.3.3008"));
        assert_eq!(
            managed_tool_installed_version_at(root.path(), &definition).as_deref(),
            Some("0.3.3008")
        );
    }

    #[test]
    fn catalog_uses_dust_managed_installation_methods_not_system_programs() {
        let catalog = bundled_tool_catalog();
        assert!(catalog.iter().all(|tool| !matches!(
            tool.install,
            Some(ToolInstall::Archive { ref url, .. }) if !url.starts_with("https://")
        )));
        assert!(
            catalog
                .iter()
                .all(|tool| tool.install.as_ref().is_none_or(|install| !matches!(
                    install,
                    ToolInstall::Archive { .. }
                ) || !matches!(
                    tool.id.as_str(),
                    "rustfmt" | "clippy"
                )))
        );
    }

    #[test]
    fn javascript_tools_prepend_their_private_bun_backed_node_runtime() {
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "pyright")
            .expect("pyright catalog entry");
        let directory = tempfile::tempdir().expect("tool store");
        let bin = directory.path().join("pyright/bin");
        std::fs::create_dir_all(&bin).expect("tool bin");
        std::fs::write(bin.join("node"), b"test runtime").expect("private node runtime");
        let environment = tool_launch_environment_at(directory.path(), &definition);
        let path = environment.get("PATH").expect("JavaScript PATH");
        assert_eq!(
            std::env::split_paths(path).next(),
            Some(bin),
            "the package shebang must resolve node to Dust's Bun runtime"
        );
    }

    #[test]
    fn nixos_host_detection_accepts_only_the_exact_os_release_id() {
        let directory = tempfile::tempdir().expect("os release directory");
        let os_release = directory.path().join("os-release");
        std::fs::write(&os_release, "NAME=NixOS\nID=\"nixos\"\n").expect("NixOS release");
        assert!(nixos_os_release_at(&os_release));
        std::fs::write(&os_release, "ID_LIKE=nixos\nID=ubuntu\n").expect("other release");
        assert!(!nixos_os_release_at(&os_release));
    }

    #[test]
    fn private_nix_workspace_contains_only_dust_pinned_flake_inputs() {
        let directory = tempfile::tempdir().expect("tool store");
        let workspace = prepare_nix_lsp_workspace_at(&directory.path().join("nix-workspace"))
            .expect("Nix workspace");
        let flake = std::fs::read_to_string(workspace.join("flake.nix")).expect("flake source");
        let lock = std::fs::read_to_string(workspace.join("flake.lock")).expect("flake lock");
        assert!(workspace.join("profiles").is_dir());
        assert!(flake.contains("Dust-owned Linux executable compatibility runtime"));
        assert!(flake.contains("compat-runtime"));
        assert!(!flake.contains("rust-analyzer ="));
        assert!(lock.contains("b6018f87da91d19d0ab4cf979885689b469cdd41"));
    }

    #[test]
    fn bun_bootstrap_is_the_pinned_glibc_baseline_artifact() {
        let bun = runtime_artifact("bun").expect("Bun runtime artifact");
        assert_eq!(
            bun.url,
            "https://github.com/oven-sh/bun/releases/download/bun-v1.3.14/bun-linux-x64-baseline.zip"
        );
        assert_eq!(
            bun.sha256,
            "a063908ae08b7852ca10939bbdc6ceed3ddabce8fb9402dce83d65d73b36e6c7"
        );
        assert_eq!(bun.archive, ToolArchiveFormat::Zip);
        assert_eq!(bun.executable_path, "bun-linux-x64-baseline/bun");
    }

    #[test]
    #[ignore = "operator qualification: requires Nix plus upstream artifact network access"]
    fn nixos_runtime_installs_patches_and_runs_the_verified_upstream_artifact() {
        if !super::is_nixos_host() {
            return;
        }
        let nix = discover_executable("nix").expect("Nix executable");
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .expect("rust-analyzer catalog entry")
            .managed_version("0.3.3016")
            .expect("exact upstream pin");
        let directory = tempfile::tempdir().expect("tool store");
        let (_, cancellations) = std::sync::mpsc::channel();
        let mut report = |_: &str, _: u64, _: Option<u64>| {};
        let installed = install_nix_lsp_profile(
            &definition,
            directory.path(),
            &nix,
            &cancellations,
            &mut report,
        )
        .expect("Dust via Nix install")
        .expect("installed executable");
        let runtime = nix_lsp_runtime_at(directory.path(), &definition)
            .expect("private compatibility runtime");
        assert_eq!(
            elf_interpreter(&installed).expect("ELF interpreter"),
            Some(runtime.loader.display().to_string())
        );
        let output = std::process::Command::new(&installed)
            .arg("--version")
            .env_clear()
            .envs(runtime.environment())
            .output()
            .expect("launch patched upstream executable");
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("0.3.3016"));
    }

    #[test]
    #[ignore = "operator qualification: requires Nix and all catalog LSP upstream networks"]
    fn nixos_runtime_installs_and_probes_every_catalog_lsp() {
        if !super::is_nixos_host() {
            return;
        }
        let nix = discover_executable("nix").expect("Nix executable");
        let definitions = bundled_tool_catalog()
            .into_iter()
            .filter(|tool| tool.kind == ToolKind::Lsp)
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 5);
        let directory = tempfile::tempdir().expect("tool store");
        let (_, cancellations) = std::sync::mpsc::channel();
        let mut report = |_: &str, _: u64, _: Option<u64>| {};
        for definition in definitions {
            let installed = install_nix_lsp_profile(
                &definition,
                directory.path(),
                &nix,
                &cancellations,
                &mut report,
            )
            .unwrap_or_else(|error| panic!("install {}: {error}", definition.id))
            .expect("installed executable");
            let environment = nix_lsp_launch_environment_at(directory.path(), &definition)
                .expect("private launch environment");
            if definition.probe_args.is_empty() {
                let mut child = std::process::Command::new(&installed)
                    .args(&definition.launch_args)
                    .current_dir(directory.path())
                    .env_clear()
                    .envs(&environment)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap_or_else(|error| panic!("launch {}: {error}", definition.id));
                std::thread::sleep(std::time::Duration::from_millis(250));
                assert!(
                    child.try_wait().expect("launch status").is_none(),
                    "{} exited during launch smoke",
                    definition.id
                );
                child.kill().expect("stop launch smoke");
                child.wait().expect("reap launch smoke");
            } else {
                let version = probe_tool_version(
                    ToolVersionRequest {
                        id: definition.id.clone(),
                        executable: installed,
                        arguments: definition.probe_args.clone(),
                        environment,
                        current_directory: Some(directory.path().to_path_buf()),
                        timeout: std::time::Duration::from_secs(10),
                    },
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .unwrap_or_else(|error| panic!("probe {}: {error}", definition.id));
                assert!(!version.is_empty(), "{} version", definition.id);
            }
        }
    }

    fn write_lsp_message(
        stdin: &mut std::process::ChildStdin,
        message: &serde_json::Value,
    ) -> Result<(), String> {
        let body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len())
            .and_then(|()| stdin.write_all(&body))
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("write LSP frame: {error}"))
    }

    fn lsp_reader(
        stdout: std::process::ChildStdout,
        messages: std::sync::mpsc::Sender<Result<serde_json::Value, String>>,
    ) {
        let mut reader = std::io::BufReader::new(stdout);
        loop {
            let mut content_length = None;
            loop {
                let mut line = String::new();
                match std::io::BufRead::read_line(&mut reader, &mut line) {
                    Ok(0) => return,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {
                        if let Some(value) = line
                            .trim()
                            .strip_prefix("Content-Length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                        {
                            content_length = Some(value);
                        }
                    }
                    Err(error) => {
                        let _ = messages.send(Err(format!("read LSP header: {error}")));
                        return;
                    }
                }
            }
            let Some(length) = content_length.filter(|length| *length <= 8 * 1024 * 1024) else {
                let _ = messages.send(Err("invalid LSP Content-Length".into()));
                return;
            };
            let mut body = vec![0_u8; length];
            if let Err(error) = std::io::Read::read_exact(&mut reader, &mut body) {
                let _ = messages.send(Err(format!("read LSP body: {error}")));
                return;
            }
            let message =
                serde_json::from_slice(&body).map_err(|error| format!("decode LSP JSON: {error}"));
            if messages.send(message).is_err() {
                return;
            }
        }
    }

    fn await_lsp_response(
        id: u64,
        stdin: &mut std::process::ChildStdin,
        messages: &std::sync::mpsc::Receiver<Result<serde_json::Value, String>>,
    ) -> Result<serde_json::Value, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut observed = Vec::new();
        for _ in 0..2048 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(format!("LSP response {id} timed out after {observed:?}"));
            }
            let message = match messages.recv_timeout(remaining) {
                Ok(message) => message?,
                Err(error) => {
                    return Err(format!(
                        "LSP response {id} unavailable after {observed:?}: {error}"
                    ));
                }
            };
            if let Some(method) = message.get("method").and_then(serde_json::Value::as_str) {
                observed.push(method.to_owned());
            }
            if message.get("id").and_then(serde_json::Value::as_u64) == Some(id)
                && message.get("method").is_none()
            {
                return Ok(message);
            }
            if let (Some(request_id), Some(method)) = (
                message.get("id"),
                message.get("method").and_then(serde_json::Value::as_str),
            ) {
                let result = if method == "workspace/configuration" {
                    let count = message
                        .pointer("/params/items")
                        .and_then(serde_json::Value::as_array)
                        .map_or(0, Vec::len);
                    serde_json::Value::Array(vec![serde_json::Value::Null; count])
                } else {
                    serde_json::Value::Null
                };
                write_lsp_message(
                    stdin,
                    &serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": result,
                    }),
                )?;
            }
        }
        Err(format!("LSP response {id} exceeded the message ceiling"))
    }

    #[test]
    #[ignore = "operator qualification: installs all catalog LSPs and performs live protocol I/O"]
    fn nixos_runtime_completes_protocol_smoke_for_every_catalog_lsp() {
        if !super::is_nixos_host() {
            return;
        }
        let nix = discover_executable("nix").expect("Nix executable");
        let definitions = bundled_tool_catalog()
            .into_iter()
            .filter(|tool| tool.kind == ToolKind::Lsp)
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 5);
        let directory = tempfile::tempdir().expect("tool store");
        let (_, cancellations) = std::sync::mpsc::channel();
        let mut report = |_: &str, _: u64, _: Option<u64>| {};
        for definition in definitions {
            let installed = install_nix_lsp_profile(
                &definition,
                directory.path(),
                &nix,
                &cancellations,
                &mut report,
            )
            .unwrap_or_else(|error| panic!("install {}: {error}", definition.id))
            .expect("installed executable");
            let project = directory.path().join("projects").join(&definition.id);
            std::fs::create_dir_all(project.join("src")).expect("project directory");
            std::fs::write(
                project.join("Cargo.toml"),
                "[package]\nname='dust_protocol_smoke'\nversion='0.1.0'\nedition='2024'\n",
            )
            .expect("Cargo marker");
            std::fs::write(project.join("package.json"), "{}\n").expect("Node marker");
            std::fs::write(project.join("tsconfig.json"), "{}\n").expect("TS marker");
            std::fs::write(
                project.join("pyproject.toml"),
                "[project]\nname='smoke'\nversion='0.1.0'\n",
            )
            .expect("Python marker");
            std::fs::write(
                project.join("go.mod"),
                "module example.com/dust-smoke\ngo 1.24\n",
            )
            .expect("Go marker");
            std::fs::write(project.join(".clangd"), "CompileFlags: {}\n").expect("clangd marker");
            let (language, source_name, text) = match definition.id.as_str() {
                "rust-analyzer" => (
                    "rust",
                    "src/main.rs",
                    "fn main() { let value = 1; value; }\n",
                ),
                "typescript-language-server" => {
                    ("typescript", "main.ts", "const value = 1;\nvalue;\n")
                }
                "pyright" => ("python", "main.py", "value = 1\nvalue\n"),
                "clangd" => ("c", "main.c", "int main(void) { return 0; }\n"),
                "gopls" => (
                    "go",
                    "main.go",
                    "package main\nfunc main() { var value = 1; _ = value }\n",
                ),
                other => panic!("unexpected catalog LSP {other}"),
            };
            let source = project.join(source_name);
            if let Some(parent) = source.parent() {
                std::fs::create_dir_all(parent).expect("source parent");
            }
            std::fs::write(&source, text).expect("source fixture");
            let root_uri = Url::from_file_path(&project).expect("root URI").to_string();
            let source_uri = Url::from_file_path(&source)
                .expect("source URI")
                .to_string();
            let environment = nix_lsp_launch_environment_at(directory.path(), &definition)
                .expect("private launch environment");
            if matches!(definition.install, Some(ToolInstall::NpmPackage { .. })) {
                let node =
                    std::fs::canonicalize(directory.path().join(&definition.id).join("bin/node"))
                        .expect("private Node runtime");
                assert!(
                    node.starts_with("/nix/store") && node.to_string_lossy().contains("nodejs"),
                    "{} did not select pinned Nix Node: {}",
                    definition.id,
                    node.display()
                );
            }
            let mut child = std::process::Command::new(&installed)
                .args(&definition.launch_args)
                .current_dir(&project)
                .envs(environment)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap_or_else(|error| panic!("launch {}: {error}", definition.id));
            let mut stdin = child.stdin.take().expect("LSP stdin");
            let stdout = child.stdout.take().expect("LSP stdout");
            let (messages_tx, messages_rx) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || lsp_reader(stdout, messages_tx));
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "processId": null,
                        "rootUri": root_uri,
                        "capabilities": {
                            "general": { "positionEncodings": ["utf-16"] },
                            "textDocument": { "completion": { "completionItem": {} } },
                            "workspace": { "configuration": true, "workspaceFolders": true }
                        },
                        "workspaceFolders": [{ "uri": root_uri, "name": "smoke" }]
                    }
                }),
            )
            .expect("initialize frame");
            let initialize = await_lsp_response(1, &mut stdin, &messages_rx)
                .unwrap_or_else(|error| panic!("initialize {}: {error}", definition.id));
            assert!(
                initialize.get("error").is_none(),
                "{}: {initialize}",
                definition.id
            );
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
            )
            .expect("initialized frame");
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeConfiguration",
                    "params": { "settings": {} }
                }),
            )
            .expect("initial configuration frame");
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": { "textDocument": {
                        "uri": source_uri, "languageId": language, "version": 1, "text": text
                    }}
                }),
            )
            .expect("didOpen frame");
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "textDocument/completion",
                    "params": {
                        "textDocument": { "uri": source_uri },
                        "position": { "line": 0, "character": 0 },
                        "context": { "triggerKind": 1 }
                    }
                }),
            )
            .expect("completion frame");
            let completion = await_lsp_response(2, &mut stdin, &messages_rx)
                .unwrap_or_else(|error| panic!("completion {}: {error}", definition.id));
            assert!(
                completion.get("error").is_none(),
                "{}: {completion}",
                definition.id
            );
            assert!(
                completion.get("result").is_some(),
                "{}: {completion}",
                definition.id
            );
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
            )
            .expect("shutdown frame");
            let shutdown = await_lsp_response(3, &mut stdin, &messages_rx)
                .unwrap_or_else(|error| panic!("shutdown {}: {error}", definition.id));
            assert!(
                shutdown.get("error").is_none(),
                "{}: {shutdown}",
                definition.id
            );
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
            )
            .expect("exit frame");
            drop(stdin);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if child.try_wait().expect("LSP status").is_some() {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().expect("kill LSP after exit timeout");
                    child.wait().expect("reap LSP");
                    panic!("{} did not exit after shutdown", definition.id);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            reader.join().expect("LSP reader");
        }
    }

    #[test]
    #[ignore = "operator qualification: requires Bun and npm-registry network access"]
    fn managed_bun_installs_and_launches_an_exact_javascript_tool_without_node_or_npm() {
        if discover_executable("bun").is_none() {
            return;
        }
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "prettier")
            .expect("prettier catalog entry");
        let directory = tempfile::tempdir().expect("tool store");
        let (_, cancellations) = std::sync::mpsc::channel();
        let mut report = |_: &str, _: u64, _: Option<u64>| {};
        let installed = install_tool(
            &definition,
            directory.path(),
            &cancellations,
            &mut report,
            None,
        )
        .expect("managed Bun install")
        .expect("tool executable");
        let bin = installed.parent().expect("tool bin");
        assert!(bin.join("node").is_file());
        let status = std::process::Command::new(&installed)
            .arg("--version")
            .env_clear()
            .env("HOME", directory.path())
            .env("PATH", bin)
            .status()
            .expect("launch Bun-backed JavaScript executable");
        assert!(status.success());
    }

    #[test]
    fn archive_paths_reject_absolute_parent_and_platform_escape_forms() {
        for path in ["", "../tool", "/tool", "nested/../tool", "nested\\tool"] {
            assert!(relative_catalog_path(path).is_err(), "{path}");
        }
        assert_eq!(
            relative_catalog_path("release/bin/tool").unwrap(),
            std::path::PathBuf::from("release/bin/tool")
        );
        for id in [
            "",
            "../server",
            "nested/server",
            ".",
            "server.tool",
            "server tool",
        ] {
            assert!(validate_tool_id(id).is_err(), "unsafe tool ID: {id}");
        }
        assert!(validate_tool_id("typescript-language-server").is_ok());
    }

    #[test]
    fn installer_worker_routes_real_managed_uninstall_and_reports_its_operation() {
        let directory = tempfile::tempdir().expect("tool store");
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .expect("catalog tool");
        let selected = directory.path().join("rust-analyzer/bin");
        std::fs::create_dir_all(&selected).expect("selected tool");
        std::fs::write(selected.join("rust-analyzer"), b"tool").expect("selected executable");
        let installer = ToolInstaller::start_at(directory.path().to_path_buf());

        installer.uninstall(definition).expect("queue uninstall");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let result = loop {
            if let Some(result) = installer.try_result().expect("installer result") {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "uninstall worker timed out"
            );
            std::thread::yield_now();
        };

        assert_eq!(result.operation, ToolOperation::UninstallManaged);
        assert!(result.result.is_ok());
        assert!(!directory.path().join("rust-analyzer").exists());
    }

    #[test]
    fn managed_uninstall_atomically_removes_only_the_selected_tool() {
        let directory = tempfile::tempdir().expect("tool store");
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .expect("catalog tool");
        let selected = directory.path().join("rust-analyzer/bin");
        let sibling = directory.path().join("clangd/bin");
        std::fs::create_dir_all(&selected).expect("selected tool");
        std::fs::create_dir_all(&sibling).expect("sibling tool");
        std::fs::write(selected.join("rust-analyzer"), b"tool").expect("selected executable");
        std::fs::write(sibling.join("clangd"), b"sibling").expect("sibling executable");

        uninstall_managed_tool(&definition, directory.path()).expect("uninstall");

        assert!(!directory.path().join("rust-analyzer").exists());
        assert!(sibling.join("clangd").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn pinned_nix_uninstall_removes_only_its_profile_generations() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tool store");
        let profiles = directory.path().join("nix-lsp-workspace/profiles");
        std::fs::create_dir_all(&profiles).expect("profiles");
        let target = directory.path().join("target");
        std::fs::write(&target, b"profile").expect("target");
        for name in [
            "rust-analyzer",
            "rust-analyzer-1-link",
            "rust-analyzer-2-link",
            "clangd",
        ] {
            symlink(&target, profiles.join(name)).expect("profile link");
        }
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .expect("catalog tool");

        uninstall_nix_lsp_profile(&definition, directory.path()).expect("uninstall profile");

        assert!(!profiles.join("rust-analyzer").exists());
        assert!(!profiles.join("rust-analyzer-1-link").exists());
        assert!(!profiles.join("rust-analyzer-2-link").exists());
        assert!(profiles.join("clangd").exists());
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_is_bounded_when_a_descendant_keeps_output_open() {
        let shell = discover_executable("sh").expect("shell fixture");
        let started = std::time::Instant::now();
        let result = probe_tool_version(
            ToolVersionRequest {
                id: "hostile".into(),
                executable: shell,
                arguments: vec!["-c".into(), "(sleep 5) & printf 'server 1.2.3\\n'".into()],
                environment: std::collections::BTreeMap::new(),
                current_directory: None,
                timeout: std::time::Duration::from_secs(1),
            },
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("remained open")),
            "{result:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
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
