use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

use crate::files::ProjectRootStrategy;

use super::{
    MAXIMUM_NIX_RUNTIME_ENVIRONMENT_BYTES, MAXIMUM_OS_RELEASE_BYTES, NIX_COMPATIBILITY_RUNTIME_ID,
    NIX_RUNTIME_ENVIRONMENT_FILE, NIXOS_HOST,
    compatibility::runtime_artifact,
    install_prepare::validate_tool_id,
    mason_catalog::mason_lsp_definitions,
    package_lock::BUNDLED_TOOL_CATALOG,
    runtimes::{fhs_glibc_loader_available, nix_profile_installed_at},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Lsp,
    Dap,
    Formatter,
    Linter,
    /// An ACP adapter for the agent panel.
    Agent,
}

impl ToolKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lsp => "LSP",
            Self::Dap => "DAP",
            Self::Formatter => "Formatter",
            Self::Linter => "Linter",
            Self::Agent => "Agent",
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
        /// The ZIP directory kept whole when the executable needs the files beside it.
        #[serde(default)]
        tree: Option<String>,
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
    /// How a formatter takes a file on stdin and writes it to stdout; `{path}` is the file.
    #[serde(default)]
    pub format_arguments: Vec<String>,
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
    #[must_use]
    pub fn supports_language(&self, language: &str) -> bool {
        self.languages
            .iter()
            .any(|candidate| candidate == language || candidate == "*")
    }

    #[must_use]
    pub fn automatically_supports_language(&self, language: &str) -> bool {
        self.languages.iter().any(|candidate| candidate == language)
    }

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
pub(super) struct ToolCatalogFile {
    tool: Vec<ToolDefinition>,
}

#[must_use]
pub fn bundled_tool_catalog() -> Vec<ToolDefinition> {
    let mut definitions = toml::from_str::<ToolCatalogFile>(BUNDLED_TOOL_CATALOG)
        .expect("bundled tool catalog must remain valid")
        .tool;
    let mut ids = definitions
        .iter()
        .map(|definition| definition.id.clone())
        .collect::<BTreeSet<_>>();
    let mut executables = definitions
        .iter()
        .map(|definition| definition.executable.clone())
        .collect::<BTreeSet<_>>();
    definitions.extend(mason_lsp_definitions().into_iter().filter(|definition| {
        ids.insert(definition.id.clone()) && executables.insert(definition.executable.clone())
    }));
    definitions
}

#[must_use]
pub fn discover_executable(executable: &str) -> Option<PathBuf> {
    let candidate = Path::new(executable);
    if candidate.components().count() > 1 {
        return executable_file(candidate).then(|| candidate.to_path_buf());
    }
    env::split_paths(&env::var_os("PATH")?).find_map(|directory| {
        let path = directory.join(executable);
        executable_file(&path).then_some(path)
    })
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// The executables on `PATH`, read with one listing per directory. Looking up every catalog
/// tool in every directory separately costs thousands of `stat` calls at startup.
struct PathExecutables(HashMap<OsString, PathBuf>);

impl PathExecutables {
    fn scan() -> Self {
        let mut found = HashMap::new();
        let mut listed = BTreeSet::new();
        for directory in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
            if !listed.insert(directory.clone()) {
                continue;
            }
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                found
                    .entry(entry.file_name())
                    .or_insert_with(|| entry.path());
            }
        }
        Self(found)
    }

    /// Same answer as [`discover_executable`] for the PATH that was scanned.
    fn find(&self, executable: &str) -> Option<PathBuf> {
        if executable.contains(std::path::MAIN_SEPARATOR) {
            return discover_executable(executable);
        }
        let path = self.0.get(OsStr::new(executable))?;
        // A directory of that name shadows nothing: keep searching like the plain lookup.
        if executable_file(path) {
            Some(path.clone())
        } else {
            discover_executable(executable)
        }
    }
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
        fs::metadata(rustup)
            .is_ok_and(|runtime| proxy.dev() == runtime.dev() && proxy.ino() == runtime.ino())
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
    crate::config::user_state_directory().join("tools")
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

pub(super) fn nixos_os_release_at(path: &Path) -> bool {
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

pub(super) fn tool_launch_environment_at(
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

pub(super) fn managed_executable(root: &Path, definition: &ToolDefinition) -> Option<PathBuf> {
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

/// A formatter installed from the tool manager: its id, executable and arguments for `path`.
pub struct ManagedFormatter {
    pub id: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

/// The installed formatter `select` picks, if any. Only the tool manager's count here: a
/// formatter that merely happens to be on PATH was not chosen for format on save.
pub fn managed_formatter(
    select: impl Fn(&ToolDefinition) -> bool,
    path: &Path,
) -> Option<ManagedFormatter> {
    static FORMATTERS: std::sync::OnceLock<Vec<ToolDefinition>> = std::sync::OnceLock::new();
    let shown = path.display().to_string();
    FORMATTERS
        .get_or_init(|| {
            bundled_tool_catalog()
                .into_iter()
                .filter(|tool| !tool.format_arguments.is_empty())
                .collect()
        })
        .iter()
        .filter(|tool| select(tool))
        .find_map(|tool| {
            Some(ManagedFormatter {
                id: tool.id.clone(),
                executable: managed_tool_executable(tool)?,
                arguments: tool
                    .format_arguments
                    .iter()
                    .map(|argument| argument.replace("{path}", &shown))
                    .collect(),
                environment: tool_launch_environment(tool),
            })
        })
}

pub(super) const MANAGED_TOOL_VERSION_FILE: &str = ".dust-version";
pub(super) const MAXIMUM_MANAGED_TOOL_VERSION_BYTES: u64 = 256;

pub(super) fn managed_tool_installed_version_at(
    root: &Path,
    definition: &ToolDefinition,
) -> Option<String> {
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

pub(super) fn write_managed_tool_version(
    path: &Path,
    definition: &ToolDefinition,
) -> Result<(), String> {
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

pub(super) fn nix_lsp_profile_path_at(root: &Path, definition: &ToolDefinition) -> Option<PathBuf> {
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
pub(super) struct NixCompatibilityRuntime {
    pub(super) loader: PathBuf,
    pub(super) nix_ld: PathBuf,
    pub(super) patchelf: PathBuf,
    pub(super) library_path: String,
    pub(super) runtime_path: String,
}

impl NixCompatibilityRuntime {
    pub(super) fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("NIX_LD".into(), self.loader.display().to_string()),
            ("NIX_LD_LIBRARY_PATH".into(), self.library_path.clone()),
            ("LD_LIBRARY_PATH".into(), self.library_path.clone()),
        ])
    }

    pub(super) fn executable(&self, name: &str) -> Option<PathBuf> {
        validate_tool_id(name).ok()?;
        env::split_paths(&self.runtime_path)
            .map(|directory| directory.join(name))
            .find(|path| valid_nix_store_path(path) && path.is_file())
    }
}

pub(super) fn valid_nix_store_path(path: &Path) -> bool {
    path.is_absolute()
        && path.starts_with("/nix/store")
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

pub(super) fn parse_nix_runtime_environment(
    profile: &Path,
) -> Result<NixCompatibilityRuntime, String> {
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

pub(super) fn nix_lsp_runtime_at(
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
pub fn nix_lsp_launch_environment(
    definition: &ToolDefinition,
) -> Result<BTreeMap<String, String>, String> {
    nix_lsp_launch_environment_at(&tool_store_root(), definition)
}

fn install_readiness(definition: &ToolDefinition, on_path: &PathExecutables) -> Option<String> {
    match definition.install.as_ref()? {
        ToolInstall::NpmPackage { .. }
            if on_path.find("bun").is_none()
                && !(definition.kind == ToolKind::Lsp
                    && is_nixos_host()
                    && on_path.find("nix").is_some())
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
    let on_path = PathExecutables::scan();
    bundled_tool_catalog()
        .into_iter()
        .map(|definition| {
            // PATH is intentionally first priority. A Dust-managed installation
            // remains independently selectable in the interactive runtime picker.
            let status = on_path
                .find(&definition.executable)
                .or_else(|| managed_executable(root, &definition))
                .map_or_else(
                    || {
                        install_readiness(&definition, &on_path)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn path_lookup_skips_nonexecutable_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("clangd");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(discover_executable(path.to_str().unwrap()).is_none());
        assert!(
            PathExecutables(HashMap::from([("clangd".into(), path.clone())]))
                .find("clangd")
                .as_ref()
                != Some(&path)
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(discover_executable(path.to_str().unwrap()), Some(path));
    }
}
