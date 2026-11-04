//! Tool catalog, runtime choices, installer/probe state and result reconciliation.
use crate::tools::{
    ToolDefinition, ToolEntry, ToolInstallResult, ToolInstaller, ToolKind, ToolStatus,
    ToolVersionRequest, ToolVersionService, discover_executable, inspect_tools, is_nixos_host,
    managed_tool_executable, managed_tool_installed_version, nix_lsp_launch_environment,
    nix_lsp_shell, nix_lsp_tool_installed, tool_launch_environment,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct ToolingSession {
    entries: Vec<ToolEntry>,
    /// Each executable's PATH location and whether it is rustup's proxy, kept until the tool
    /// list is inspected again. The language server autostart check asks on every main loop
    /// pass, and a PATH search costs dozens of `stat` calls.
    path_lookups: RefCell<HashMap<String, Option<(PathBuf, bool)>>>,
    nix_preferred: BTreeSet<String>,
    managed_preferred: BTreeSet<String>,
    managed_versions: BTreeMap<String, String>,
    installer: ToolInstaller,
    version_service: ToolVersionService,
    probe_directory: RefCell<Option<PathBuf>>,
    version_pending: BTreeSet<String>,
    versions: BTreeMap<String, Result<String, String>>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum LspRuntimeSource {
    Path,
    Managed,
    Nix,
}

impl LspRuntimeSource {
    pub(in crate::editor::session) const fn key(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Managed => "managed",
            Self::Nix => "nix",
        }
    }

    pub(in crate::editor::session) const fn label(self) -> &'static str {
        match self {
            Self::Path => "PATH",
            Self::Managed => "Dust",
            Self::Nix => "Dust (Nix)",
        }
    }
}

pub(in crate::editor::session) const fn lsp_runtime_source_allowed_on_host(
    source: LspRuntimeSource,
    nixos_host: bool,
) -> bool {
    match source {
        LspRuntimeSource::Path => true,
        LspRuntimeSource::Managed => !nixos_host,
        LspRuntimeSource::Nix => nixos_host,
    }
}

pub(in crate::editor::session) fn nix_workspace_shell_for_host(
    definition: &ToolDefinition,
    fallback_enabled: bool,
    nixos_host: bool,
) -> Option<&str> {
    fallback_enabled
        .then_some(())
        .filter(|()| nixos_host)
        .and_then(|()| nix_lsp_shell(definition))
}

pub(in crate::editor::session) const fn tool_kind_key(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Lsp => "lsp",
        ToolKind::Dap => "dap",
        ToolKind::Formatter => "formatter",
        ToolKind::Linter => "linter",
        ToolKind::Agent => "agent",
    }
}

#[cfg(test)]
impl ToolingSession {
    pub(super) fn fixture_retain(&mut self, keep: impl FnMut(&ToolEntry) -> bool) {
        self.entries.retain(keep);
    }
    pub(super) fn fixture_insert(&mut self, entry: ToolEntry) {
        self.entries.push(entry);
    }
    pub(super) fn fixture_version(&mut self, key: String, value: Result<String, String>) {
        self.versions.insert(key, value);
    }
    pub(super) fn check_probe_worker_replacement(
        &mut self,
        definition: &ToolDefinition,
        executable: PathBuf,
    ) {
        let key = self.tool_version_key(definition, LspRuntimeSource::Path, Some(&executable));
        self.forget_probe(&key);
        self.queue_tool_version_probe(definition, LspRuntimeSource::Path, executable.clone());
        assert!(self.version_pending.contains(&key));
        assert_eq!(
            self.observed_tool_version(definition, LspRuntimeSource::Path, Some(&executable))
                .0,
            "probing…"
        );
        self.replace_probe_worker();
        assert!(!self.version_pending.contains(&key));
        assert_eq!(
            self.observed_tool_version(definition, LspRuntimeSource::Path, Some(&executable))
                .0,
            "detecting…"
        );
    }
}

pub(in crate::editor::session) enum ToolInstall {
    Managed(ToolDefinition),
    Nix(ToolDefinition, PathBuf),
    Rustup {
        definition: ToolDefinition,
        rustup: PathBuf,
        proxy: PathBuf,
        cwd: PathBuf,
    },
    Uninstall(ToolDefinition, bool),
}

impl ToolingSession {
    pub(super) fn new(entries: Vec<ToolEntry>) -> Self {
        Self {
            entries,
            path_lookups: RefCell::default(),
            nix_preferred: BTreeSet::new(),
            managed_preferred: BTreeSet::new(),
            managed_versions: BTreeMap::new(),
            installer: ToolInstaller::start(),
            version_service: ToolVersionService::start(),
            probe_directory: RefCell::new(None),
            version_pending: BTreeSet::new(),
            versions: BTreeMap::new(),
        }
    }
    pub(super) fn entries(&self) -> &[ToolEntry] {
        &self.entries
    }
    pub(super) fn version(&self, key: &str) -> Option<&Result<String, String>> {
        self.versions.get(key)
    }
    pub(super) fn managed_version(&self, id: &str) -> Option<&String> {
        self.managed_versions.get(id)
    }
    pub(super) fn forget_probe(&mut self, key: &str) {
        self.versions.remove(key);
        self.version_pending.remove(key);
    }
    pub(super) fn refresh(&mut self) {
        self.reinspect(None);
        self.path_lookups.get_mut().clear();
        self.replace_probe_worker();
        self.versions.clear();
    }
    /// Re-reads the catalog and host, keeping the rows of installs still running or
    /// queued (other than `finished`).
    fn reinspect(&mut self, finished: Option<&str>) {
        let waiting: Vec<_> = self
            .entries
            .iter()
            .filter(|e| Some(e.definition.id.as_str()) != finished)
            .filter(|e| matches!(e.status, ToolStatus::Installing { .. }))
            .map(|e| (e.definition.id.clone(), e.status.clone()))
            .collect();
        self.entries = inspect_tools();
        for (id, status) in waiting {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.definition.id == id) {
                entry.status = status;
            }
        }
    }
    fn replace_probe_worker(&mut self) {
        self.version_service = ToolVersionService::start();
        // A replacement cannot complete requests admitted by the old worker.
        self.version_pending.clear();
    }
    pub(super) fn poll_versions(&mut self) -> (bool, bool) {
        let mut changed = false;
        let mut unavailable = false;
        let mut rustup_key = None;
        while let Some(result) = self.version_service.try_result() {
            // Looked up only when a result arrives: this runs every main loop pass.
            let rustup_key = rustup_key.get_or_insert_with(|| {
                self.entries
                    .iter()
                    .find(|e| e.definition.id == "rust-analyzer")
                    .and_then(|entry| {
                        let path = discover_executable(&entry.definition.executable)?;
                        Self::rust_analyzer_is_rustup_proxy(&path).then(|| {
                            self.tool_version_key(
                                &entry.definition,
                                LspRuntimeSource::Path,
                                Some(&path),
                            )
                        })
                    })
            });
            self.version_pending.remove(&result.id);
            unavailable |= rustup_key.as_ref() == Some(&result.id) && result.result.is_err();
            self.versions.insert(result.id, result.result);
            changed = true;
        }
        (changed, unavailable)
    }
    pub(super) fn select_version(&mut self, id: &str, version: String) -> Result<(), String> {
        if !self.entries.iter().any(|e| {
            e.definition.id == id
                && e.definition.kind == ToolKind::Lsp
                && e.definition.managed_version(&version).is_some()
        }) {
            return Err("this managed runtime has no installable version".into());
        }
        self.managed_versions.insert(id.into(), version);
        Ok(())
    }
    pub(super) fn select_runtime(
        &mut self,
        id: &str,
        source: LspRuntimeSource,
        version: Option<String>,
    ) -> Result<(), String> {
        if !lsp_runtime_source_allowed_on_host(source, is_nixos_host()) {
            return Err("that installation mechanism is not used on this host".into());
        }
        if !self
            .entries
            .iter()
            .any(|e| e.definition.id == id && e.definition.kind == ToolKind::Lsp)
        {
            return Err("language server is unavailable".into());
        }
        if source != LspRuntimeSource::Path {
            self.select_version(
                id,
                version.ok_or("this managed runtime has no installable version")?,
            )?;
        }
        self.nix_preferred.remove(id);
        self.managed_preferred.remove(id);
        match source {
            LspRuntimeSource::Path => {}
            LspRuntimeSource::Nix => {
                self.nix_preferred.insert(id.into());
            }
            LspRuntimeSource::Managed => {
                self.managed_preferred.insert(id.into());
            }
        }
        Ok(())
    }
    pub(super) fn metadata(&self) -> BTreeMap<String, String> {
        self.nix_preferred
            .iter()
            .map(|id| (format!("lsp.runtime.{id}"), "nix".into()))
            .chain(
                self.managed_preferred
                    .iter()
                    .map(|id| (format!("lsp.runtime.{id}"), "managed".into())),
            )
            .chain(
                self.managed_versions
                    .iter()
                    .map(|(id, version)| (format!("lsp.version.{id}"), version.clone())),
            )
            .collect()
    }
    pub(super) fn restore(&mut self, metadata: &BTreeMap<String, String>) {
        self.nix_preferred.clear();
        self.managed_preferred.clear();
        self.managed_versions.clear();
        for entry in &self.entries {
            let definition = &entry.definition;
            if definition.kind != ToolKind::Lsp {
                continue;
            }
            match metadata
                .get(&format!("lsp.runtime.{}", definition.id))
                .map(String::as_str)
            {
                Some("nix") => {
                    self.nix_preferred.insert(definition.id.clone());
                }
                Some("managed") => {
                    self.managed_preferred.insert(definition.id.clone());
                }
                _ => {}
            }
            if let Some(version) = metadata
                .get(&format!("lsp.version.{}", definition.id))
                .and_then(|v| definition.managed_version(v))
                .and_then(|d| d.version)
            {
                self.managed_versions.insert(definition.id.clone(), version);
            }
        }
    }
    pub(super) fn install(&mut self, request: ToolInstall) -> Result<(), String> {
        let (definition, removing) = match &request {
            ToolInstall::Managed(d)
            | ToolInstall::Nix(d, _)
            | ToolInstall::Rustup { definition: d, .. } => (d, false),
            ToolInstall::Uninstall(d, _) => (d, true),
        };
        let id = definition.id.clone();
        let Some(entry) = self.entries.iter().find(|e| e.definition.id == id) else {
            return Err("tool source retired".into());
        };
        if matches!(entry.status, ToolStatus::Installing { .. }) {
            return Err(format!(
                "{} is already being installed",
                entry.definition.name
            ));
        }
        let queued = self
            .entries
            .iter()
            .any(|e| matches!(e.status, ToolStatus::Installing { .. }));
        match request {
            ToolInstall::Managed(d) => self.installer.install(d),
            ToolInstall::Nix(d, nix) => self.installer.install_nix(d, nix),
            ToolInstall::Rustup {
                definition,
                rustup,
                proxy,
                cwd,
            } => self.installer.install_rustup_component(
                definition,
                rustup,
                proxy,
                cwd,
                "rust-analyzer".into(),
            ),
            ToolInstall::Uninstall(d, true) => self.installer.uninstall_nix(d),
            ToolInstall::Uninstall(d, false) => self.installer.uninstall(d),
        }
        .map_err(|error| error.to_string())?;
        if let Some(entry) = self.entries.iter_mut().find(|e| e.definition.id == id) {
            entry.status = ToolStatus::Installing {
                phase: if queued {
                    "queued"
                } else if removing {
                    "uninstalling"
                } else {
                    "preparing"
                }
                .into(),
                completed: 0,
                total: None,
            };
        }
        Ok(())
    }
    pub(super) fn poll_progress(&mut self) -> Option<String> {
        let mut changed = None;
        while let Ok(Some(progress)) = self.installer.try_progress() {
            if let Some(entry) = self
                .entries
                .iter_mut()
                .find(|e| e.definition.id == progress.id)
            {
                entry.status = ToolStatus::Installing {
                    phase: progress.phase,
                    completed: progress.completed,
                    total: progress.total,
                };
                changed = Some(entry.definition.id.clone());
            }
        }
        changed
    }
    pub(super) fn poll_install(&mut self) -> Option<(String, ToolInstallResult)> {
        let result = self.installer.try_result().ok()??;
        // Progress and completion use separate queues. Drain progress sent before this
        // result so a later poll cannot put the finished row back into Installing.
        let _ = self.poll_progress();
        let name = self
            .entries
            .iter()
            .find(|e| e.definition.id == result.id)
            .map_or_else(|| result.id.clone(), |e| e.definition.name.clone());
        self.reinspect(Some(&result.id));
        self.path_lookups.get_mut().clear();
        if let Err(error) = &result.result
            && let Some(entry) = self
                .entries
                .iter_mut()
                .find(|e| e.definition.id == result.id)
        {
            entry.status = ToolStatus::Failed(error.clone());
        }
        self.replace_probe_worker();
        let prefix = format!("{}:", result.id);
        self.versions.retain(|key, _| !key.starts_with(&prefix));
        Some((name, result))
    }

    pub(in crate::editor::session) fn nix_workspace_available_for(
        &self,
        definition: &ToolDefinition,
        nix_fallback: bool,
    ) -> bool {
        nix_workspace_shell_for_host(definition, nix_fallback, is_nixos_host()).is_some()
            && discover_executable("nix").is_some()
    }

    pub(in crate::editor::session) fn explicit_lsp_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> Option<LspRuntimeSource> {
        let nixos_host = is_nixos_host();
        if self.nix_preferred.contains(&definition.id)
            && lsp_runtime_source_allowed_on_host(LspRuntimeSource::Nix, nixos_host)
        {
            Some(LspRuntimeSource::Nix)
        } else if self.managed_preferred.contains(&definition.id)
            && lsp_runtime_source_allowed_on_host(LspRuntimeSource::Managed, nixos_host)
        {
            Some(LspRuntimeSource::Managed)
        } else {
            None
        }
    }

    pub(in crate::editor::session) fn selected_lsp_runtime_source(
        &self,
        definition: &ToolDefinition,
        nix_fallback: bool,
    ) -> LspRuntimeSource {
        self.explicit_lsp_runtime_source(definition)
            .unwrap_or_else(|| {
                if self.path_executable(&definition.executable).is_some() {
                    LspRuntimeSource::Path
                } else if is_nixos_host() {
                    if self.nix_workspace_available_for(definition, nix_fallback) {
                        LspRuntimeSource::Nix
                    } else {
                        LspRuntimeSource::Path
                    }
                } else if managed_tool_executable(definition).is_some()
                    || !definition.managed_versions().is_empty()
                {
                    LspRuntimeSource::Managed
                } else {
                    LspRuntimeSource::Path
                }
            })
    }

    pub(in crate::editor::session) fn selected_managed_definition(
        &self,
        definition: &ToolDefinition,
    ) -> ToolDefinition {
        let selected = self
            .managed_versions
            .get(&definition.id)
            .filter(|version| definition.managed_version(version).is_some())
            .cloned()
            .or_else(|| {
                managed_tool_installed_version(definition)
                    .filter(|version| definition.managed_version(version).is_some())
            })
            .or_else(|| definition.version.clone());
        selected
            .as_deref()
            .and_then(|version| definition.managed_version(version))
            .unwrap_or_else(|| definition.clone())
    }

    pub(in crate::editor::session) fn managed_variant_installed(
        &self,
        base: &ToolDefinition,
        variant: &ToolDefinition,
    ) -> bool {
        if managed_tool_executable(base).is_none() {
            return false;
        }
        match managed_tool_installed_version(base) {
            Some(installed) => {
                variant.version.as_deref() == Some(installed.as_str())
                    || variant.release.as_deref() == Some(installed.as_str())
            }
            None => variant.version == base.version,
        }
    }

    pub(in crate::editor::session) fn selected_managed_executable(
        &self,
        definition: &ToolDefinition,
    ) -> Option<PathBuf> {
        let selected = self.selected_managed_definition(definition);
        self.managed_variant_installed(definition, &selected)
            .then(|| managed_tool_executable(definition))
            .flatten()
    }

    pub(in crate::editor::session) fn path_executable(
        &self,
        executable: &str,
    ) -> Option<(PathBuf, bool)> {
        self.path_lookups
            .borrow_mut()
            .entry(executable.to_owned())
            .or_insert_with(|| {
                discover_executable(executable).map(|path| {
                    let rustup_proxy = Self::rust_analyzer_is_rustup_proxy(&path);
                    (path, rustup_proxy)
                })
            })
            .clone()
    }

    pub(in crate::editor::session) fn selected_lsp_runtime_available(
        &self,
        definition: &ToolDefinition,
        nix_fallback: bool,
    ) -> bool {
        match self.selected_lsp_runtime_source(definition, nix_fallback) {
            LspRuntimeSource::Path => {
                self.path_executable(&definition.executable)
                    .is_some_and(|(path, rustup_proxy)| {
                        if definition.id != "rust-analyzer" || !rustup_proxy {
                            return true;
                        }
                        let key =
                            self.tool_version_key(definition, LspRuntimeSource::Path, Some(&path));
                        !matches!(self.versions.get(&key), Some(Err(_)))
                    })
            }
            LspRuntimeSource::Managed => self.selected_managed_executable(definition).is_some(),
            LspRuntimeSource::Nix => {
                nix_lsp_tool_installed(definition)
                    && self.selected_managed_executable(definition).is_some()
            }
        }
    }

    pub(in crate::editor::session) fn set_probe_directory(&self, directory: Option<PathBuf>) {
        *self.probe_directory.borrow_mut() = directory;
    }

    pub(in crate::editor::session) fn tool_version_key(
        &self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> String {
        let key = executable.map_or_else(
            || format!("{}:{}", definition.id, source.key()),
            |path| format!("{}:{}:{}", definition.id, source.key(), path.display()),
        );
        if definition.id == "rust-analyzer"
            && source == LspRuntimeSource::Path
            && let Some(directory) = self.probe_directory.borrow().as_ref()
        {
            return format!("{key}:{}", directory.display());
        }
        key
    }

    pub(in crate::editor::session) fn compact_tool_version(
        definition: &ToolDefinition,
        version: &str,
    ) -> String {
        let mut value = version.trim();
        for prefix in [&definition.name, &definition.executable] {
            if let Some(rest) = value.strip_prefix(prefix)
                && rest.starts_with(char::is_whitespace)
            {
                value = rest.trim_start();
                break;
            }
        }
        let module_suffix = format!("/{} ", definition.name);
        if let Some(index) = value.find(&module_suffix) {
            value = value[index + module_suffix.len()..].trim_start();
        }
        value = value.strip_prefix("version ").unwrap_or(value);
        if value.is_empty() {
            version.into()
        } else {
            value.into()
        }
    }

    pub(in crate::editor::session) fn rust_analyzer_is_rustup_proxy(path: &Path) -> bool {
        crate::tools::rust_analyzer_is_rustup_proxy(path)
    }

    pub(in crate::editor::session) fn unavailable_rust_analyzer_message() -> String {
        "rust-analyzer missing. Press i in :lsp to install it via rustup.".into()
    }

    pub(in crate::editor::session) fn rust_analyzer_path_version(
        version: &str,
        rustup_bundled: bool,
    ) -> (String, String) {
        let reported = version.trim();
        let executable = reported
            .split_whitespace()
            .next()
            .unwrap_or(reported)
            .trim_end_matches("-standalone");
        let release = reported
            .split_whitespace()
            .map(|part| {
                part.trim_matches(|character: char| !character.is_ascii_digit() && character != '-')
            })
            .find(|part| {
                part.len() == 10
                    && part.as_bytes().get(4) == Some(&b'-')
                    && part.as_bytes().get(7) == Some(&b'-')
            })
            .map(str::to_owned);
        if rustup_bundled {
            return (
                "rustup bundled".into(),
                release.unwrap_or_else(|| "toolchain build".into()),
            );
        }
        if executable.starts_with("0.") {
            return (executable.into(), "standalone build".into());
        }
        (
            format!("Rust {executable}"),
            release.unwrap_or_else(|| "toolchain build".into()),
        )
    }

    pub(in crate::editor::session) fn observed_tool_version(
        &self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> (String, Option<String>) {
        let key = self.tool_version_key(definition, source, executable);
        if self.version_pending.contains(&key) {
            return ("probing…".into(), None);
        }
        match self.versions.get(&key) {
            Some(Ok(version)) => (Self::compact_tool_version(definition, version), None),
            Some(Err(error)) => ("probe failed".into(), Some(error.clone())),
            None if source == LspRuntimeSource::Managed => (
                definition
                    .version
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                None,
            ),
            None if source == LspRuntimeSource::Nix => (
                definition
                    .version
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                None,
            ),
            None if executable.is_some() && definition.probe_args.is_empty() => {
                ("version unavailable".into(), None)
            }
            None => ("detecting…".into(), None),
        }
    }

    pub(in crate::editor::session) fn queue_tool_version_probe(
        &mut self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: PathBuf,
    ) {
        if definition.probe_args.is_empty() {
            return;
        }
        let key = self.tool_version_key(definition, source, Some(&executable));
        if self.version_pending.contains(&key) || self.versions.contains_key(&key) {
            return;
        }
        let (arguments, environment, current_directory, timeout) = (
            definition.probe_args.clone(),
            match source {
                LspRuntimeSource::Managed => tool_launch_environment(definition),
                LspRuntimeSource::Nix => match nix_lsp_launch_environment(definition) {
                    Ok(environment) => environment,
                    Err(error) => {
                        self.versions.insert(key, Err(error));
                        return;
                    }
                },
                LspRuntimeSource::Path => BTreeMap::new(),
            },
            self.probe_directory.borrow().clone(),
            Duration::from_secs(2),
        );
        match self.version_service.probe(ToolVersionRequest {
            id: key.clone(),
            executable,
            arguments,
            environment,
            current_directory,
            timeout,
        }) {
            Ok(()) => {
                self.version_pending.insert(key);
            }
            Err(error) => {
                self.versions.insert(key, Err(error));
            }
        }
    }

    pub(in crate::editor::session) fn queue_tool_version_probes(&mut self, category: &str) {
        let nixos_host = is_nixos_host();
        let definitions = self
            .entries
            .iter()
            .filter(|entry| category == "all" || tool_kind_key(entry.definition.kind) == category)
            .map(|entry| entry.definition.clone())
            .collect::<Vec<_>>();
        for definition in definitions {
            if let Some(path) = discover_executable(&definition.executable) {
                self.queue_tool_version_probe(&definition, LspRuntimeSource::Path, path);
            }
            if definition.kind != ToolKind::Lsp
                || lsp_runtime_source_allowed_on_host(LspRuntimeSource::Managed, nixos_host)
            {
                let managed_definition = if definition.kind == ToolKind::Lsp {
                    self.selected_managed_definition(&definition)
                } else {
                    definition.clone()
                };
                let managed = if definition.kind == ToolKind::Lsp {
                    self.selected_managed_executable(&definition)
                } else {
                    managed_tool_executable(&definition)
                };
                if let Some(path) = managed {
                    self.queue_tool_version_probe(
                        &managed_definition,
                        LspRuntimeSource::Managed,
                        path,
                    );
                }
            }
            if definition.kind == ToolKind::Lsp
                && lsp_runtime_source_allowed_on_host(LspRuntimeSource::Nix, nixos_host)
                && nix_lsp_tool_installed(&definition)
                && let Some(executable) = self.selected_managed_executable(&definition)
            {
                let selected = self.selected_managed_definition(&definition);
                self.queue_tool_version_probe(&selected, LspRuntimeSource::Nix, executable);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn rustup_probes_use_project_directory_and_separate_cached_results() {
        use std::{fs, os::unix::fs::PermissionsExt, thread, time::Instant};
        let root = tempfile::tempdir().unwrap();
        let one = root.path().join("one");
        let two = root.path().join("two");
        fs::create_dir(&one).unwrap();
        fs::create_dir(&two).unwrap();
        fs::write(two.join("available"), "").unwrap();
        let executable = root.path().join("rust-analyzer");
        fs::write(&executable, "#!/bin/sh\nif [ -f available ]; then printf 'rust-analyzer project'; else exit 1; fi\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .unwrap();
        definition.probe_args = vec!["--version".into()];
        let mut tooling = ToolingSession::new(Vec::new());
        for (directory, success) in [(one, false), (two, true)] {
            tooling.set_probe_directory(Some(directory));
            let key =
                tooling.tool_version_key(&definition, LspRuntimeSource::Path, Some(&executable));
            assert!(!tooling.versions.contains_key(&key));
            tooling.queue_tool_version_probe(
                &definition,
                LspRuntimeSource::Path,
                executable.clone(),
            );
            let deadline = Instant::now() + Duration::from_secs(3);
            while !tooling.versions.contains_key(&key) {
                tooling.poll_versions();
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(tooling.versions[&key].is_ok(), success);
        }
        assert_eq!(tooling.versions.len(), 2);
    }
}
