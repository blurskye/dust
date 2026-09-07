use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn refresh_tools(&mut self) {
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "all".into(), |menu| menu.category.clone());
        let selected = self.selected_tool_id();
        self.tools = inspect_tools();
        self.tool_version_service = ToolVersionService::start();
        self.tool_versions.clear();
        self.tool_version_pending.clear();
        self.queue_tool_version_probes(&category);
        self.rebuild_tool_menu(&category, selected.as_deref());
    }

    pub(in crate::editor::session) fn process_tool_version_results(&mut self) {
        let rustup_key = self
            .tools
            .iter()
            .find(|entry| entry.definition.id == "rust-analyzer")
            .and_then(|entry| {
                let path = discover_executable(&entry.definition.executable)?;
                Self::rust_analyzer_is_rustup_proxy(&path).then(|| {
                    Self::tool_version_key(&entry.definition, LspRuntimeSource::Path, Some(&path))
                })
            });
        let mut changed = false;
        let mut unavailable_rustup = false;
        while let Some(result) = self.tool_version_service.try_result() {
            self.tool_version_pending.remove(&result.id);
            if rustup_key.as_ref() == Some(&result.id) && result.result.is_err() {
                unavailable_rustup = true;
            }
            self.tool_versions.insert(result.id, result.result);
            changed = true;
        }
        if changed {
            self.lsp_autostart_observation = None;
            let category = self.ui.tool_menu.as_ref().map(|menu| menu.category.clone());
            let selected = self.selected_tool_id();
            if let Some(category) = category {
                self.rebuild_tool_menu(&category, selected.as_deref());
            }
        }
        if unavailable_rustup {
            self.show_error(Self::unavailable_rust_analyzer_message());
        }
    }

    pub(in crate::editor::session) fn activate_selected_tool(&mut self) {
        if self
            .ui
            .tool_menu
            .as_ref()
            .is_some_and(|menu| menu.runtime_picker.is_some())
        {
            self.accept_selected_lsp_runtime();
            return;
        }
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.activate_tool_by_id(&id);
    }

    pub(in crate::editor::session) fn activate_tool_by_id(&mut self, id: &str) {
        let Some(index) = self
            .tools
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let definition = self.tools[index].definition.clone();
        let path_available = matches!(self.tools[index].status, ToolStatus::Installed(_));
        if definition.kind == ToolKind::Lsp {
            if self.selected_lsp_runtime_available(&definition) {
                if let Some((_, document)) = self.active_lsp_document()
                    && !definition.supports_language(&document.language_id)
                {
                    self.ui.notification = Some(format!(
                        "{} is installed. Open a supported file to start it.",
                        definition.name
                    ));
                    self.ui.notification_error = false;
                } else {
                    self.start_lsp(Some(id.into()));
                }
            } else if definition.managed_versions().is_empty() {
                self.show_error(format!(
                    "{} is not installed and Dust has no installer for it",
                    definition.name
                ));
            } else {
                let source = self.selected_lsp_runtime_source(&definition);
                if source == LspRuntimeSource::Path
                    && definition.id == "rust-analyzer"
                    && let Some(path) = discover_executable(&definition.executable)
                    && Self::rust_analyzer_is_rustup_proxy(&path)
                {
                    let key = Self::tool_version_key(&definition, source, Some(&path));
                    if matches!(self.tool_versions.get(&key), Some(Err(_))) {
                        self.show_error(Self::unavailable_rust_analyzer_message());
                        return;
                    }
                }
                self.install_tool_by_id(id, Some(source));
            }
            return;
        }
        if path_available {
            match definition.kind {
                ToolKind::Lsp => unreachable!("handled before installed-tool dispatch"),
                ToolKind::Dap => self.start_dap(Some(id.into()), None),
                ToolKind::Formatter | ToolKind::Linter => {
                    self.ui.notification = Some(self.message(
                        "tool_install_succeeded",
                        &[("tool", definition.name.clone())],
                    ));
                    self.ui.notification_error = false;
                }
            }
            return;
        }
        if let ToolStatus::Unavailable(diagnostic) = &self.tools[index].status {
            self.show_error(diagnostic.clone());
            return;
        }
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::InstallTool {
                id: id.into(),
                source: None,
            });
            return;
        }
        if definition.install.is_none() {
            self.show_error(self.message("tool_no_installer", &[("tool", definition.name)]));
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::ProjectLocal)
        {
            self.show_error(error);
            return;
        }
        match self.tool_installer.install(definition.clone()) {
            Ok(()) => {
                self.tools[index].status = ToolStatus::Installing {
                    phase: "preparing".into(),
                    completed: 0,
                    total: None,
                };
                self.ui.notification =
                    Some(self.message("tool_install_started", &[("tool", definition.name)]));
                self.ui.notification_error = false;
                let category = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map_or("all", |menu| &menu.category)
                    .to_owned();
                self.rebuild_tool_menu(&category, Some(id));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn install_selected_tool(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let focused = self
            .tools
            .iter()
            .find(|entry| entry.definition.id == id)
            .filter(|entry| entry.definition.kind == ToolKind::Lsp)
            .map(|entry| {
                (
                    self.focused_runtime_source(&entry.definition),
                    self.focused_managed_version(&entry.definition),
                )
            });
        let source = focused.as_ref().map(|(source, _)| *source);
        if let Some((source, Some(version))) = focused
            && matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
        {
            self.lsp_managed_versions.insert(id.clone(), version);
        }
        self.install_tool_by_id(&id, source);
    }

    pub(in crate::editor::session) fn install_tool_by_id(
        &mut self,
        id: &str,
        source: Option<LspRuntimeSource>,
    ) {
        let Some(index) = self
            .tools
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let base_definition = self.tools[index].definition.clone();
        let source = source.unwrap_or(LspRuntimeSource::Managed);
        if base_definition.kind == ToolKind::Lsp
            && !lsp_runtime_source_allowed_on_host(source, is_nixos_host())
        {
            self.show_error(if is_nixos_host() {
                "NixOS language servers are installed only through Dust's private Nix compatibility runtime"
            } else {
                "non-NixOS language servers are installed only as verified Dust downloads"
            });
            return;
        }
        let definition = if base_definition.kind == ToolKind::Lsp
            && matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
        {
            self.selected_managed_definition(&base_definition)
        } else {
            base_definition
        };
        let rustup_install = if definition.id == "rust-analyzer" && source == LspRuntimeSource::Path
        {
            discover_executable(&definition.executable).and_then(|proxy| {
                if !Self::rust_analyzer_is_rustup_proxy(&proxy) {
                    return None;
                }
                let key = Self::tool_version_key(&definition, source, Some(&proxy));
                let rustup = proxy.with_file_name("rustup");
                (matches!(self.tool_versions.get(&key), Some(Err(_))) && rustup.is_file())
                    .then_some((rustup, proxy))
            })
        } else {
            None
        };
        if definition.kind == ToolKind::Lsp {
            match source {
                LspRuntimeSource::Path if rustup_install.is_none() => {
                    if discover_executable(&definition.executable).is_some() {
                        self.ui.notification = Some(format!(
                            "{} is already installed on PATH; Dust leaves external software unchanged",
                            definition.name
                        ));
                        self.ui.notification_error = false;
                    } else {
                        self.show_error(format!(
                            "{} is not installed and has no Dust-managed build",
                            definition.name
                        ));
                    }
                    return;
                }
                LspRuntimeSource::Path => {}
                LspRuntimeSource::Managed
                    if self
                        .managed_variant_installed(&self.tools[index].definition, &definition) =>
                {
                    self.ui.notification = Some(format!(
                        "{} {} is already installed",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    self.ui.notification_error = false;
                    return;
                }
                LspRuntimeSource::Nix
                    if nix_lsp_profile_installed(&definition)
                        && self.managed_variant_installed(
                            &self.tools[index].definition,
                            &definition,
                        ) =>
                {
                    self.ui.notification = Some(format!(
                        "{} {} is already installed through Dust's Nix compatibility environment",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    self.ui.notification_error = false;
                    return;
                }
                LspRuntimeSource::Managed | LspRuntimeSource::Nix => {}
            }
        }
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::InstallTool {
                id: id.into(),
                source: (definition.kind == ToolKind::Lsp).then_some(source),
            });
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::ProjectLocal)
        {
            self.show_error(error);
            return;
        }
        let operation = if let Some((rustup, proxy)) = rustup_install {
            let Some(cwd) = self.workspace.active_root().and_then(|root| {
                self.workspace
                    .roots()
                    .iter()
                    .find(|candidate| candidate.id == root)
                    .map(|root| root.identity.canonical_path.clone())
            }) else {
                self.show_error("rustup installation requires an active workspace root");
                return;
            };
            self.tool_installer.install_rustup_component(
                definition.clone(),
                rustup,
                proxy,
                cwd,
                "rust-analyzer".into(),
            )
        } else if definition.kind == ToolKind::Lsp && source == LspRuntimeSource::Nix {
            let Some(nix) = discover_executable("nix") else {
                self.show_error("Dust via Nix is unavailable: nix was not found");
                return;
            };
            if !self.nix_workspace_available_for(&definition) {
                self.show_error("no verified upstream artifact or Dust Nix runtime is available for this language server");
                return;
            }
            self.tool_installer.install_nix(definition.clone(), nix)
        } else {
            if let ToolStatus::Unavailable(diagnostic) = &self.tools[index].status {
                self.show_error(diagnostic.clone());
                return;
            }
            if definition.install.is_none() {
                self.show_error(self.message("tool_no_installer", &[("tool", definition.name)]));
                return;
            }
            self.tool_installer.install(definition.clone())
        };
        match operation {
            Ok(()) => {
                self.tools[index].status = ToolStatus::Installing {
                    phase: "preparing".into(),
                    completed: 0,
                    total: None,
                };
                self.ui.notification = Some(format!(
                    "installing {} {} {}…",
                    definition.name,
                    source.label(),
                    if source == LspRuntimeSource::Path {
                        "component"
                    } else if matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix) {
                        definition.version.as_deref().unwrap_or("unknown")
                    } else {
                        "runtime"
                    }
                ));
                self.ui.notification_error = false;
                let category = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map_or("all", |menu| &menu.category)
                    .to_owned();
                self.rebuild_tool_menu(&category, Some(id));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn uninstall_selected_tool(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let Some(index) = self
            .tools
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let base_definition = self.tools[index].definition.clone();
        let source = if base_definition.kind == ToolKind::Lsp {
            self.focused_runtime_source(&base_definition)
        } else {
            LspRuntimeSource::Managed
        };
        if base_definition.kind == ToolKind::Lsp
            && !lsp_runtime_source_allowed_on_host(source, is_nixos_host())
        {
            self.show_error("that installation mechanism is not used on this host");
            return;
        }
        let definition = if base_definition.kind == ToolKind::Lsp
            && matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
        {
            self.focused_managed_version(&base_definition)
                .as_deref()
                .and_then(|version| base_definition.managed_version(version))
                .unwrap_or_else(|| base_definition.clone())
        } else {
            base_definition.clone()
        };
        let operation = match source {
            LspRuntimeSource::Path => {
                self.ui.notification = Some(
                    "This server is owned by PATH, not Dust; remove it with its package manager"
                        .into(),
                );
                self.ui.notification_error = false;
                return;
            }
            LspRuntimeSource::Managed => {
                if !self.managed_variant_installed(&base_definition, &definition) {
                    self.show_error(format!(
                        "Dust-download version {} is not installed",
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                }
                self.stop_lsp_sessions_for_server(&id);
                self.tool_installer.uninstall(definition.clone())
            }
            LspRuntimeSource::Nix => {
                if !nix_lsp_profile_installed(&definition)
                    || !self.managed_variant_installed(&base_definition, &definition)
                {
                    self.show_error(format!(
                        "Dust via Nix version {} is not installed",
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                }
                self.stop_lsp_sessions_for_server(&id);
                self.tool_installer.uninstall_nix(definition.clone())
            }
        };
        match operation {
            Ok(()) => {
                self.tools[index].status = ToolStatus::Installing {
                    phase: "uninstalling".into(),
                    completed: 0,
                    total: None,
                };
                self.ui.notification = Some(format!(
                    "uninstalling {} {} runtime…",
                    definition.name,
                    source.label()
                ));
                self.ui.notification_error = false;
                let category = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map_or("all", |menu| &menu.category)
                    .to_owned();
                self.rebuild_tool_menu(&category, Some(&id));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn stop_selected_lsp(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.stop_lsp_sessions_for_server(&id);
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        self.rebuild_tool_menu(&category, Some(&id));
    }

    pub(in crate::editor::session) fn restart_selected_lsp(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.stop_lsp_sessions_for_server(&id);
        self.start_lsp(Some(id.clone()));
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        self.rebuild_tool_menu(&category, Some(&id));
    }

    pub(in crate::editor::session) fn process_tool_progress(&mut self) {
        let mut changed = None;
        while let Ok(Some(progress)) = self.tool_installer.try_progress() {
            let Some(entry) = self
                .tools
                .iter_mut()
                .find(|entry| entry.definition.id == progress.id)
            else {
                continue;
            };
            entry.status = ToolStatus::Installing {
                phase: progress.phase,
                completed: progress.completed,
                total: progress.total,
            };
            changed = Some(entry.definition.id.clone());
        }
        if let (Some(id), Some(menu)) = (changed, &self.ui.tool_menu) {
            self.rebuild_tool_menu(&menu.category.clone(), Some(&id));
        }
    }

    pub(in crate::editor::session) fn process_tool_results(&mut self) {
        while let Ok(Some(result)) = self.tool_installer.try_result() {
            let name = self
                .tools
                .iter()
                .find(|entry| entry.definition.id == result.id)
                .map_or_else(|| result.id.clone(), |entry| entry.definition.name.clone());
            let diagnostic = result.result.err();
            self.tools = inspect_tools();
            if let Some(diagnostic) = diagnostic {
                if let Some(entry) = self
                    .tools
                    .iter_mut()
                    .find(|entry| entry.definition.id == result.id)
                {
                    entry.status = ToolStatus::Failed(diagnostic.clone());
                }
                self.show_error(format!(
                    "{} {} failed: {diagnostic}",
                    match result.operation {
                        ToolOperation::InstallManaged
                        | ToolOperation::InstallRustup
                        | ToolOperation::InstallNix => "installing",
                        ToolOperation::UninstallManaged | ToolOperation::UninstallNix => {
                            "uninstalling"
                        }
                    },
                    name
                ));
            } else {
                self.ui.notification = Some(match result.operation {
                    ToolOperation::InstallRustup => {
                        format!("installed {name} in the active rustup toolchain")
                    }
                    ToolOperation::InstallManaged => {
                        format!("installed {name} Dust download runtime")
                    }
                    ToolOperation::InstallNix => {
                        format!("installed {name} Dust via Nix runtime")
                    }
                    ToolOperation::UninstallManaged => {
                        format!("uninstalled {name} Dust download runtime")
                    }
                    ToolOperation::UninstallNix => {
                        format!("uninstalled {name} Dust via Nix runtime")
                    }
                });
                self.ui.notification_error = false;
                self.lsp_last_errors.remove(&result.id);
            }
            self.lsp_autostart_observation = None;
            self.tool_version_service = ToolVersionService::start();
            let prefix = format!("{}:", result.id);
            self.tool_versions
                .retain(|key, _| !key.starts_with(&prefix));
            self.tool_version_pending
                .retain(|key| !key.starts_with(&prefix));
            if let Some(menu) = &self.ui.tool_menu {
                let category = menu.category.clone();
                self.queue_tool_version_probes(&category);
                self.rebuild_tool_menu(&category, Some(&result.id));
            }
        }
    }
}

pub(in crate::editor::session) fn tool_install_progress_label(
    phase: &str,
    completed: u64,
    total: Option<u64>,
) -> String {
    let bytes = |value: u64| {
        if value >= 1024 * 1024 {
            format!("{:.1} MiB", value as f64 / (1024.0 * 1024.0))
        } else {
            format!("{} KiB", value / 1024)
        }
    };
    match total.filter(|total| *total > 0) {
        Some(total) => format!(
            "{phase} {} / {} ({}%)",
            bytes(completed),
            bytes(total),
            completed.saturating_mul(100).min(total.saturating_mul(100)) / total
        ),
        None => phase.into(),
    }
}

pub(in crate::editor::session) const fn tool_kind_key(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Lsp => "lsp",
        ToolKind::Dap => "dap",
        ToolKind::Formatter => "formatter",
        ToolKind::Linter => "linter",
    }
}
