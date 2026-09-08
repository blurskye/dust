use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn nix_workspace_available_for(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        nix_workspace_shell_for_host(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
            is_nixos_host(),
        )
        .is_some()
            && discover_executable("nix").is_some()
    }

    pub(in crate::editor::session) fn explicit_lsp_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> Option<LspRuntimeSource> {
        let nixos_host = is_nixos_host();
        if self.lsp_nix_workspace_preferred.contains(&definition.id)
            && lsp_runtime_source_allowed_on_host(LspRuntimeSource::Nix, nixos_host)
        {
            Some(LspRuntimeSource::Nix)
        } else if self.lsp_managed_preferred.contains(&definition.id)
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
    ) -> LspRuntimeSource {
        self.explicit_lsp_runtime_source(definition)
            .unwrap_or_else(|| {
                if discover_executable(&definition.executable).is_some() {
                    LspRuntimeSource::Path
                } else if is_nixos_host() {
                    if self.nix_workspace_available_for(definition) {
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
            .lsp_managed_versions
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

    pub(in crate::editor::session) fn selected_lsp_runtime_available(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        match self.selected_lsp_runtime_source(definition) {
            LspRuntimeSource::Path => {
                discover_executable(&definition.executable).is_some_and(|path| {
                    if definition.id != "rust-analyzer"
                        || !Self::rust_analyzer_is_rustup_proxy(&path)
                    {
                        return true;
                    }
                    let key =
                        Self::tool_version_key(definition, LspRuntimeSource::Path, Some(&path));
                    !matches!(self.tool_versions.get(&key), Some(Err(_)))
                })
            }
            LspRuntimeSource::Managed => self.selected_managed_executable(definition).is_some(),
            LspRuntimeSource::Nix => {
                nix_lsp_tool_installed(definition)
                    && self.selected_managed_executable(definition).is_some()
            }
        }
    }

    pub(in crate::editor::session) fn tool_version_key(
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> String {
        executable.map_or_else(
            || format!("{}:{}", definition.id, source.key()),
            |path| format!("{}:{}:{}", definition.id, source.key(), path.display()),
        )
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
        let key = Self::tool_version_key(definition, source, executable);
        if self.tool_version_pending.contains(&key) {
            return ("probing…".into(), None);
        }
        match self.tool_versions.get(&key) {
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
        let key = Self::tool_version_key(definition, source, Some(&executable));
        if self.tool_version_pending.contains(&key) || self.tool_versions.contains_key(&key) {
            return;
        }
        let (arguments, environment, current_directory, timeout) = (
            definition.probe_args.clone(),
            match source {
                LspRuntimeSource::Managed => tool_launch_environment(definition),
                LspRuntimeSource::Nix => match nix_lsp_launch_environment(definition) {
                    Ok(environment) => environment,
                    Err(error) => {
                        self.tool_versions.insert(key, Err(error));
                        return;
                    }
                },
                LspRuntimeSource::Path => BTreeMap::new(),
            },
            None,
            Duration::from_secs(2),
        );
        match self.tool_version_service.probe(ToolVersionRequest {
            id: key.clone(),
            executable,
            arguments,
            environment,
            current_directory,
            timeout,
        }) {
            Ok(()) => {
                self.tool_version_pending.insert(key);
            }
            Err(error) => {
                self.tool_versions.insert(key, Err(error));
            }
        }
    }

    pub(in crate::editor::session) fn queue_tool_version_probes(&mut self, category: &str) {
        let nixos_host = is_nixos_host();
        let definitions = self
            .tools
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
