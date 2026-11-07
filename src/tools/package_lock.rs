//! Package manifests, locks and content-addressed objects.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use serde::Deserialize;

use serde::Serialize;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_sha256: Option<String>,
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
    pub(super) fn source_digest(&self) -> &str {
        match &self.source {
            PackageSource::Registry { archive_sha256, .. } => archive_sha256,
            PackageSource::Git { tree_sha256, .. } => tree_sha256,
            PackageSource::Path { .. } => "",
        }
    }
}
pub(super) fn read_bounded_regular(path: &Path, maximum: u64) -> Result<Vec<u8>, PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(PackageError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum
    {
        return Err(PackageError::InvalidManifest);
    }
    let file = crate::files::open_read_no_symlinks(path).map_err(PackageError::Io)?;
    if !file.metadata().map_err(PackageError::Io)?.is_file() {
        return Err(PackageError::InvalidManifest);
    }
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(PackageError::Io)?;
    if bytes.len() as u64 > maximum {
        return Err(PackageError::InvalidManifest);
    }
    Ok(bytes)
}
static NEXT_PACKAGE_TEMP: AtomicU64 = AtomicU64::new(1);

/// A unique suffix for temporary names in the tool store. The process id keeps a name left
/// behind by an interrupted install from colliding with a later run's.
pub(super) fn next_temp_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    )
}
pub(super) fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
pub(super) fn valid_source(source: &PackageSource) -> bool {
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
            if manifests[&dependency.name].source != dependency.source {
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
            let mut source = manifest.source.clone();
            match &mut source {
                PackageSource::Registry { archive_sha256, .. } => {
                    archive_sha256.make_ascii_lowercase()
                }
                PackageSource::Git { tree_sha256, .. } => tree_sha256.make_ascii_lowercase(),
                PackageSource::Path { .. } => {}
            }
            let content_sha256 = manifest.source_digest().to_ascii_lowercase();
            LockedPackage {
                name,
                version: manifest.version.clone(),
                source,
                content_sha256,
                activation_sha256: None,
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
    write_protected_atomic(path, &bytes, maximum_bytes)
}
pub(super) fn write_protected_atomic(
    path: &Path,
    bytes: &[u8],
    maximum: usize,
) -> Result<(), PackageError> {
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
    // NamedTempFile removes staging files on every error path and creates them with mode 0600.
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(PackageError::Io)?;
    file.write_all(bytes).map_err(PackageError::Io)?;
    file.as_file().sync_all().map_err(PackageError::Io)?;
    file.persist(path)
        .map_err(|error| PackageError::Io(error.error))?;
    crate::files::sync_directory(parent).map_err(PackageError::Io)
}
pub fn verify_lock_store(
    lock_path: &Path,
    store: &Path,
    maximum_bytes: usize,
    maximum_object_bytes: usize,
) -> Result<usize, PackageError> {
    use sha2::Digest;
    use sha2::Sha256;
    let parent = lock_path.parent().ok_or(PackageError::InvalidManifest)?;
    let transaction = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("dust-packages.lock"))
        .map_err(PackageError::Io)?;
    let _transaction = crate::files::lock_exclusive(transaction, false)
        .map_err(|(_, error)| PackageError::Io(error))?;
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
        let source_digest = match &package.source {
            PackageSource::Registry { archive_sha256, .. } => Some(archive_sha256),
            PackageSource::Git { tree_sha256, .. } => Some(tree_sha256),
            PackageSource::Path { .. } => None,
        };
        if source_digest.is_some_and(|digest| !digest.eq_ignore_ascii_case(&package.content_sha256))
        {
            return Err(PackageError::ChecksumMismatch);
        }
        let object = store.join(package.content_sha256.to_ascii_lowercase());
        let data = read_bounded_regular(&object, maximum_object_bytes as u64)?;
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
    use sha2::Digest;
    use sha2::Sha256;
    if bytes.len() > maximum_bytes || !valid_digest(expected_sha256) {
        return Err(PackageError::InvalidManifest);
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err(PackageError::ChecksumMismatch);
    }
    fs::create_dir_all(store).map_err(PackageError::Io)?;
    let target = store.join(&actual);
    if read_bounded_regular(&target, maximum_bytes as u64)
        .is_ok_and(|data| format!("{:x}", Sha256::digest(&data)) == actual)
    {
        return Ok(target);
    }
    let mut file = tempfile::NamedTempFile::new_in(store).map_err(PackageError::Io)?;
    file.write_all(bytes).map_err(PackageError::Io)?;
    file.as_file().sync_all().map_err(PackageError::Io)?;
    file.persist(&target)
        .map_err(|error| PackageError::Io(error.error))?;
    crate::files::sync_directory(store).map_err(PackageError::Io)?;
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
    #[error("package {0} is not installed")]
    PackageNotFound(String),
    #[error("no package lock rollback is available")]
    NoRollback,
    #[error("prepared package token is missing or expired")]
    MissingPrepared,
    #[error("package source file is outside workspace scope")]
    OutsideScope,
    #[error("package activation failed: {0}")]
    Activation(String),
    #[error("another package operation is still in progress")]
    Busy,
    #[error("package service queue is full")]
    QueueFull,
    #[error("package service stopped")]
    ServiceStopped,
    #[error("package content checksum mismatch")]
    ChecksumMismatch,
    #[error("package lock exceeds its size limit")]
    LockTooLarge,
    #[error("package lock JSON failed: {0}")]
    Json(#[source] serde_json::Error),
    #[error("package I/O failed: {0}")]
    Io(#[source] std::io::Error),
}

pub(super) const BUNDLED_TOOL_CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/tools/catalog.toml"
));
