
use crate::app::ExecutionSource;
use crate::app::Instant;
use crate::app::App;
use crate::app::LspRuntimeSource;
use crate::app::Path;
use crate::app::PathBuf;
use crate::app::ToolDefinition;
use crate::app::ToolInstall;
use crate::app::ToolKind;
use crate::app::ToolMenuRow;
use crate::app::ToolMenuState;
use crate::app::ToolOperation;
use crate::app::ToolRuntimeOption;
use crate::app::ToolRuntimePickerState;
use crate::app::ToolStatus;
use crate::app::discover_executable;
use crate::app::is_nixos_host;
use crate::app::lsp_runtime_source_allowed_on_host;
use crate::app::managed_tool_executable;
use crate::app::nix_lsp_profile_installed;
use crate::app::tool_kind_key;
use crate::ui::render::tools::TOOL_CATEGORY_ORDER;
use crate::ui::render::tools::TOOL_MANAGER_MINIMUM_HEIGHT;

impl App {

    pub(in crate::app) fn nix_workspace_available_for(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        self.tooling.nix_workspace_available_for(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }
}

impl App {


    pub(in crate::app) fn selected_lsp_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> LspRuntimeSource {
        self.tooling.selected_lsp_runtime_source(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }
}

impl App {


    pub(in crate::app) fn selected_managed_definition(
        &self,
        definition: &ToolDefinition,
    ) -> ToolDefinition {
        self.tooling.selected_managed_definition(definition)
    }
}

impl App {


    pub(in crate::app) fn managed_variant_installed(
        &self,
        base: &ToolDefinition,
        variant: &ToolDefinition,
    ) -> bool {
        self.tooling.managed_variant_installed(base, variant)
    }
}

impl App {


    pub(in crate::app) fn selected_managed_executable(
        &self,
        definition: &ToolDefinition,
    ) -> Option<PathBuf> {
        self.tooling.selected_managed_executable(definition)
    }
}

impl App {


    pub(in crate::app) fn tool_probe_directory(&self) -> Option<PathBuf> {
        self.active_lsp_document_without_text()
            .and_then(|(_, document)| document.path.parent().map(Path::to_path_buf))
            .or_else(|| {
                self.workspace
                    .roots()
                    .first()
                    .map(|root| root.identity.canonical_path.clone())
            })
    }
}

impl App {


    fn set_tool_probe_directory(&self) {
        self.tooling
            .set_probe_directory(self.tool_probe_directory());
    }
}

impl App {


    pub(in crate::app) fn selected_lsp_runtime_available(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        self.set_tool_probe_directory();
        self.tooling.selected_lsp_runtime_available(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }
}

impl App {


    pub(in crate::app) fn tool_version_key(
        &self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> String {
        self.set_tool_probe_directory();
        self.tooling
            .tool_version_key(definition, source, executable)
    }
}

impl App {


    pub(in crate::app) fn rust_analyzer_is_rustup_proxy(path: &Path) -> bool {
        crate::app::tools::ToolingSession::rust_analyzer_is_rustup_proxy(path)
    }
}

impl App {


    pub(in crate::app) fn unavailable_rust_analyzer_message() -> String {
        crate::app::tools::ToolingSession::unavailable_rust_analyzer_message()
    }
}

impl App {


    pub(in crate::app) fn rust_analyzer_path_version(
        version: &str,
        rustup_bundled: bool,
    ) -> (String, String) {
        crate::app::tools::ToolingSession::rust_analyzer_path_version(
            version,
            rustup_bundled,
        )
    }
}

impl App {


    pub(in crate::app) fn observed_tool_version(
        &self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> (String, Option<String>) {
        self.set_tool_probe_directory();
        self.tooling
            .observed_tool_version(definition, source, executable)
    }
}

impl App {


    pub(in crate::app) fn queue_tool_version_probe(
        &mut self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: PathBuf,
    ) {
        let Some(cancelled) = self.workspace.probe_cancellation() else {
            return;
        };
        self.tooling.set_probe_cancellation(cancelled);
        self.set_tool_probe_directory();
        self.tooling
            .queue_tool_version_probe(definition, source, executable)
    }
}

impl App {


    pub(in crate::app) fn queue_tool_version_probes(&mut self, category: &str) {
        let Some(cancelled) = self.workspace.probe_cancellation() else {
            return;
        };
        self.tooling.set_probe_cancellation(cancelled);
        self.set_tool_probe_directory();
        self.tooling.queue_tool_version_probes(category)
    }
}

impl App {

    fn tool_menu_height(&self) -> u16 {
        let height = (u32::from(self.window_session.height())
            * u32::from(crate::ui::render::tools::TOOL_MANAGER_HEIGHT_PERCENT)
            / 100)
            .try_into()
            .unwrap_or(self.window_session.height());
        height
            .max(
                crate::ui::render::tools::TOOL_MANAGER_MINIMUM_HEIGHT
                    .min(self.window_session.height()),
            )
            .min(self.window_session.height())
    }
}

impl App {


    fn tool_menu_list_visible_rows(&self) -> usize {
        // Rendering reserves the border, up to two wrapped help rows, tabs,
        // heading, separator, and ten detail rows. Using the two-help-row
        // bound keeps navigation safe when the help text wraps after resize.
        usize::from(self.tool_menu_height().saturating_sub(18)).max(1)
    }
}

impl App {


    fn tool_runtime_picker_visible_rows(&self) -> usize {
        let option_count = self
            .ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.runtime_picker.as_ref())
            .map_or(1, |picker| picker.options.len());
        let compact_height = u16::try_from(option_count)
            .unwrap_or(u16::MAX)
            .saturating_mul(2)
            .saturating_add(11)
            .max(TOOL_MANAGER_MINIMUM_HEIGHT);
        usize::from(
            self.tool_menu_height()
                .min(compact_height)
                .saturating_sub(9)
                / 2,
        )
        .max(1)
    }
}

impl App {


    pub(in crate::app) fn open_tool_menu(&mut self, category: String) {
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.close_prompt();
        self.input.capture_changed();
        // `:lsp` is normally opened from a source buffer. Put the matching
        // server under the cursor so Enter and runtime selection act on the
        // server the user is actually editing, not an alphabetically earlier
        // unrelated adapter.
        let selected = (category == "lsp")
            .then(|| {
                self.active_lsp_document_without_text()
                    .map(|(_, document)| document.language_id)
            })
            .flatten()
            .and_then(|language| {
                self.tooling
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.definition.kind == ToolKind::Lsp
                            && entry.definition.automatically_supports_language(&language)
                            && matches!(entry.status, ToolStatus::Installed(_))
                    })
                    .or_else(|| {
                        self.tooling.entries().iter().find(|entry| {
                            entry.definition.kind == ToolKind::Lsp
                                && entry.definition.automatically_supports_language(&language)
                        })
                    })
                    .map(|entry| entry.definition.id.clone())
            });
        self.queue_tool_version_probes(&category);
        self.rebuild_tool_menu(&category, selected.as_deref());
    }
}

impl App {


    pub(in crate::app) fn lsp_menu_lifecycle(&self, id: &str) -> (String, Vec<String>) {
        let running_servers = self.language.running_servers();
        let mut running = 0usize;
        let mut starting = false;
        let mut backoff = false;
        let mut failed = false;
        let mut sessions = self
            .language
            .sessions()
            .iter()
            .filter(|(_, session)| session.server_id == id)
            .map(|(key, session)| {
                let state = if session.capabilities.is_some() {
                    running = running.saturating_add(1);
                    "running"
                } else if session.circuit_open {
                    failed = true;
                    "crashed"
                } else if session.restart_due.is_some() {
                    backoff = true;
                    "restarting"
                } else if running_servers.contains(key) {
                    starting = true;
                    "starting"
                } else {
                    "stopped"
                };
                format!("{} ({state})", session.root_path.display())
            })
            .collect::<Vec<_>>();
        sessions.sort();
        starting = starting || self.language.launch_pending(id);
        let lifecycle = if running > 0 {
            if running == 1 {
                "running".into()
            } else {
                format!("running ×{running}")
            }
        } else if starting {
            "starting".into()
        } else if backoff {
            "restarting".into()
        } else if failed || self.language.errors().contains_key(id) {
            "crashed".into()
        } else {
            "stopped".into()
        };
        (lifecycle, sessions)
    }
}

impl App {


    pub(in crate::app) fn runtime_options_for(
        &self,
        definition: &ToolDefinition,
    ) -> Vec<ToolRuntimeOption> {
        let nixos_host = is_nixos_host();
        let selected_source = self.selected_lsp_runtime_source(definition);
        let selected_managed = self.selected_managed_definition(definition);
        let path = discover_executable(&definition.executable);
        let managed_path = managed_tool_executable(definition);
        let nix_available = self.nix_workspace_available_for(definition);
        let nix_profile_installed = nix_lsp_profile_installed(definition);
        let (path_version, path_error) =
            self.observed_tool_version(definition, LspRuntimeSource::Path, path.as_ref());
        let path_version_reported = path_error.is_none()
            && !matches!(
                path_version.as_str(),
                "probing…" | "detecting…" | "version unavailable" | "probe failed"
            );
        let rustup_bundled = definition.id == "rust-analyzer"
            && path
                .as_deref()
                .is_some_and(Self::rust_analyzer_is_rustup_proxy);
        let rustup_installable = rustup_bundled
            && path_error.is_some()
            && path
                .as_ref()
                .is_some_and(|proxy| proxy.with_file_name("rustup").is_file());
        let path_usable = path.is_some() && path_error.is_none();
        let (path_display_version, path_release) = if rustup_bundled {
            if path_version_reported {
                Self::rust_analyzer_path_version(&path_version, true)
            } else {
                ("rustup bundled".into(), "toolchain build".into())
            }
        } else if definition.id == "rust-analyzer" && path_version_reported {
            Self::rust_analyzer_path_version(&path_version, false)
        } else {
            (
                path_version.clone(),
                if path_version_reported {
                    "detected PATH build".into()
                } else {
                    "—".into()
                },
            )
        };
        let mut options = vec![ToolRuntimeOption {
            source: LspRuntimeSource::Path.key().into(),
            version_pin: None,
            label: LspRuntimeSource::Path.label().into(),
            version: if path.is_some() {
                path_display_version
            } else {
                "—".into()
            },
            release: if path.is_some() {
                path_release
            } else {
                "—".into()
            },
            location: path
                .as_ref()
                .map_or_else(|| "not on PATH".into(), |path| path.display().to_string()),
            status: if path.is_none() {
                "not installed"
            } else if path_error.is_some() {
                "not working"
            } else if selected_source == LspRuntimeSource::Path {
                "in use"
            } else {
                "installed"
            }
            .into(),
            note: if path.is_none() {
                "Nothing on PATH. Install it with your package manager to use this build.".into()
            } else if rustup_installable {
                "rustup has no rust-analyzer for this toolchain; i installs it with rustup.".into()
            } else if let Some(error) = path_error {
                error
            } else if path_version_reported {
                format!("Found on PATH ({path_version}). Dust never changes or removes it.")
            } else {
                "Found on PATH. Dust never changes or removes it.".into()
            },
            selected: selected_source == LspRuntimeSource::Path,
            installed: path_usable,
            installable: rustup_installable,
            uninstallable: false,
        }];

        if lsp_runtime_source_allowed_on_host(LspRuntimeSource::Managed, nixos_host) {
            for variant in definition.managed_versions() {
                let version = variant.version.clone().unwrap_or_else(|| "unknown".into());
                let installed = self.managed_variant_installed(definition, &variant);
                let (_, probe_error) = if installed {
                    {
                        self.observed_tool_version(
                            &variant,
                            LspRuntimeSource::Managed,
                            managed_path.as_ref(),
                        )
                    }
                } else {
                    (String::new(), None)
                };
                options.push(ToolRuntimeOption {
                    source: LspRuntimeSource::Managed.key().into(),
                    version_pin: Some(version.clone()),
                    label: LspRuntimeSource::Managed.label().into(),
                    version: version.clone(),
                    release: variant
                        .release
                        .clone()
                        .unwrap_or_else(|| "catalog build".into()),
                    location: if installed {
                        managed_path.as_ref().map_or_else(
                            || "installed path unavailable".into(),
                            |path| path.display().to_string(),
                        )
                    } else {
                        "not installed".into()
                    },
                    status: if installed
                        && selected_source == LspRuntimeSource::Managed
                        && selected_managed.version.as_deref() == Some(version.as_str())
                    {
                        "in use"
                    } else if installed {
                        "installed"
                    } else {
                        "not installed"
                    }
                    .into(),
                    note: probe_error.unwrap_or_else(|| {
                        if installed {
                            format!("{} {version}, installed by Dust.", definition.name)
                        } else {
                            "A verified download from the Dust catalog.".to_owned()
                        }
                    }),
                    selected: selected_source == LspRuntimeSource::Managed
                        && selected_managed.version.as_deref() == Some(version.as_str()),
                    installed,
                    installable: variant.install.is_some(),
                    uninstallable: installed,
                });
            }

            if definition.managed_versions().is_empty() {
                options.push(ToolRuntimeOption {
                    source: LspRuntimeSource::Managed.key().into(),
                    version_pin: None,
                    label: LspRuntimeSource::Managed.label().into(),
                    version: "none".into(),
                    release: "none".into(),
                    location: "—".into(),
                    status: "unavailable".into(),
                    note: "Dust has no download for this server.".into(),
                    selected: selected_source == LspRuntimeSource::Managed,
                    installed: false,
                    installable: false,
                    uninstallable: false,
                });
            }
        }

        if lsp_runtime_source_allowed_on_host(LspRuntimeSource::Nix, nixos_host) {
            for variant in definition.managed_versions() {
                let version = variant.version.clone().unwrap_or_else(|| "unknown".into());
                let installed =
                    nix_profile_installed && self.managed_variant_installed(definition, &variant);
                let (_, probe_error) = if installed {
                    {
                        self.observed_tool_version(
                            &variant,
                            LspRuntimeSource::Nix,
                            managed_path.as_ref(),
                        )
                    }
                } else {
                    (String::new(), None)
                };
                options.push(ToolRuntimeOption {
                    source: LspRuntimeSource::Nix.key().into(),
                    version_pin: Some(version.clone()),
                    label: LspRuntimeSource::Nix.label().into(),
                    version: version.clone(),
                    release: variant
                        .release
                        .clone()
                        .unwrap_or_else(|| "catalog build".into()),
                    location: if installed {
                        managed_path
                            .as_ref()
                            .map_or_else(String::new, |path| path.display().to_string())
                    } else {
                        "not installed".into()
                    },
                    status: if installed
                        && selected_source == LspRuntimeSource::Nix
                        && selected_managed.version.as_deref() == Some(version.as_str())
                    {
                        "in use"
                    } else if installed {
                        "installed"
                    } else if nix_available {
                        "not installed"
                    } else {
                        "unavailable"
                    }
                    .into(),
                    note: probe_error.unwrap_or_else(|| {
                        if installed {
                            format!(
                                "{} {version}, installed by Dust through Nix.",
                                definition.name
                            )
                        } else {
                            "Built through Nix from the Dust catalog.".to_owned()
                        }
                    }),
                    selected: selected_source == LspRuntimeSource::Nix
                        && selected_managed.version.as_deref() == Some(version.as_str()),
                    installed,
                    installable: nix_available && variant.install.is_some(),
                    uninstallable: installed,
                });
            }
        }
        options
    }
}

impl App {


    pub(in crate::app) fn rebuild_tool_menu(
        &mut self,
        category: &str,
        selected_id: Option<&str>,
    ) {
        let nixos_host = is_nixos_host();
        let configured = self.configuration.settings().ui.tool_manager.clone();
        let previous_picker = self.ui.tool_menu.as_ref().and_then(|menu| {
            menu.runtime_picker.as_ref().map(|picker| {
                (
                    picker.server_id.clone(),
                    picker
                        .options
                        .get(picker.selected)
                        .map(|option| (option.source.clone(), option.version_pin.clone())),
                    picker.scroll,
                )
            })
        });
        let previous_scroll = self
            .ui
            .tool_menu
            .as_ref()
            .filter(|menu| menu.category == category)
            .map_or(0, |menu| menu.scroll);
        let (query, searching) = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(Default::default, |menu| {
                (menu.query.clone(), menu.searching)
            });
        let needle = query.to_lowercase();
        let category = if configured.categories.contains_key(category) {
            category.to_owned()
        } else {
            "all".into()
        };
        let active_language = self
            .active_lsp_document_without_text()
            .map(|(_, document)| document.language_id);
        let mut entries = self
            .tooling
            .entries()
            .iter()
            .filter(|entry| category == "all" || tool_kind_key(entry.definition.kind) == category)
            .filter(|entry| {
                let definition = &entry.definition;
                needle.is_empty()
                    || definition.name.to_lowercase().contains(&needle)
                    || definition.id.to_lowercase().contains(&needle)
                    || definition
                        .languages
                        .iter()
                        .any(|language| language.contains(&needle))
            })
            // A search reaches every known server, as Mason lists them all; without one the
            // list keeps to servers that are installed or that Dust can install.
            .filter(|entry| {
                !needle.is_empty()
                    || entry.definition.kind != ToolKind::Lsp
                    || discover_executable(&entry.definition.executable).is_some()
                    || managed_tool_executable(&entry.definition).is_some()
                    || !entry.definition.managed_versions().is_empty()
                    || !self.lsp_menu_lifecycle(&entry.definition.id).1.is_empty()
            })
            .collect::<Vec<_>>();
        // Stable ordering is deliberate: installation, runtime selection, and
        // lifecycle changes must never make the focused row jump elsewhere.
        entries.sort_by_key(|entry| entry.definition.name.to_lowercase());
        let rows = entries
            .into_iter()
            .map(|entry| {
                let definition = &entry.definition;
                let is_lsp = definition.kind == ToolKind::Lsp;
                let source = if is_lsp {
                    self.selected_lsp_runtime_source(definition)
                } else if discover_executable(&definition.executable).is_some() {
                    LspRuntimeSource::Path
                } else {
                    LspRuntimeSource::Managed
                };
                let path = discover_executable(&definition.executable);
                let managed_definition = if is_lsp {
                    self.selected_managed_definition(definition)
                } else {
                    definition.clone()
                };
                let managed = if is_lsp {
                    self.selected_managed_executable(definition)
                } else {
                    managed_tool_executable(definition)
                };
                let source_executable = match source {
                    LspRuntimeSource::Path => path.as_ref(),
                    LspRuntimeSource::Managed | LspRuntimeSource::Nix => managed.as_ref(),
                };
                let observed_definition =
                    if matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix) {
                        &managed_definition
                    } else {
                        definition
                    };
                let (reported_version, probe_error) =
                    self.observed_tool_version(observed_definition, source, source_executable);
                let rustup_bundled = source == LspRuntimeSource::Path
                    && definition.id == "rust-analyzer"
                    && path
                        .as_deref()
                        .is_some_and(Self::rust_analyzer_is_rustup_proxy);
                let path_version_reported = probe_error.is_none()
                    && !matches!(
                        reported_version.as_str(),
                        "probing…" | "detecting…" | "version unavailable" | "probe failed"
                    );
                let path_presentation = (source == LspRuntimeSource::Path
                    && definition.id == "rust-analyzer"
                    && (rustup_bundled || path_version_reported))
                    .then(|| Self::rust_analyzer_path_version(&reported_version, rustup_bundled));
                let mut version = match source {
                    LspRuntimeSource::Path => path_presentation
                        .as_ref()
                        .map_or_else(|| reported_version.clone(), |(version, _)| version.clone()),
                    LspRuntimeSource::Managed => managed_definition
                        .version
                        .clone()
                        .unwrap_or_else(|| reported_version.clone()),
                    LspRuntimeSource::Nix => managed_definition
                        .version
                        .clone()
                        .unwrap_or_else(|| reported_version.clone()),
                };
                let mut release = match source {
                    LspRuntimeSource::Path => path_presentation.map_or_else(
                        || {
                            if path_version_reported {
                                "reported by executable".into()
                            } else {
                                "—".into()
                            }
                        },
                        |(_, release)| release,
                    ),
                    LspRuntimeSource::Managed => managed_definition
                        .release
                        .clone()
                        .unwrap_or_else(|| "catalog build".into()),
                    LspRuntimeSource::Nix => managed_definition
                        .release
                        .clone()
                        .unwrap_or_else(|| "catalog build".into()),
                };
                let (session_state, sessions) = if is_lsp {
                    self.lsp_menu_lifecycle(&definition.id)
                } else {
                    (String::new(), Vec::new())
                };
                let installed = match source {
                    LspRuntimeSource::Path => path.is_some(),
                    LspRuntimeSource::Managed => managed.is_some(),
                    LspRuntimeSource::Nix => {
                        nix_lsp_profile_installed(definition)
                            && self.managed_variant_installed(definition, &managed_definition)
                    }
                };
                let can_install = if is_lsp && nixos_host {
                    self.nix_workspace_available_for(definition)
                } else {
                    !definition.managed_versions().is_empty()
                };
                let can_uninstall = if is_lsp && nixos_host {
                    installed
                } else {
                    managed.is_some()
                };
                // A Dust-installed copy older than the one this release ships can be updated.
                let installed_version = (installed && source != LspRuntimeSource::Path)
                    .then(|| crate::tools::managed_tool_installed_version(definition))
                    .flatten();
                let update = definition.version.clone().filter(|current| {
                    installed_version.as_ref().is_some_and(|installed| {
                        installed != current && definition.release.as_ref() != Some(installed)
                    })
                });
                let error = self
                    .language
                    .errors()
                    .get(&definition.id)
                    .cloned()
                    .or_else(|| probe_error.clone())
                    .or_else(|| match &entry.status {
                        ToolStatus::Failed(error) | ToolStatus::Unavailable(error) => {
                            Some(error.clone())
                        }
                        _ => None,
                    });
                // One state per row, from a fixed set, so the column always reads the same way.
                let lifecycle = if let ToolStatus::Installing {
                    phase,
                    completed,
                    total,
                } = &entry.status
                {
                    tool_install_progress_label(phase, *completed, *total)
                } else if !installed {
                    if matches!(entry.status, ToolStatus::Failed(_)) {
                        "install failed".into()
                    } else {
                        "not installed".into()
                    }
                } else if source == LspRuntimeSource::Path && probe_error.is_some() {
                    "not working".into()
                } else if is_lsp && session_state != "stopped" {
                    session_state.clone()
                } else {
                    "installed".into()
                };
                if !installed && source == LspRuntimeSource::Path {
                    version = "—".into();
                    release = "—".into();
                }
                let version = short_tool_version(&version);
                let runtime = if installed || can_install {
                    source.label().into()
                } else {
                    "—".into()
                };
                let executable = match (&source, installed) {
                    (_, false) => String::new(),
                    (LspRuntimeSource::Path, true) => path
                        .as_ref()
                        .map_or_else(String::new, |path| path.display().to_string()),
                    (LspRuntimeSource::Managed | LspRuntimeSource::Nix, true) => managed
                        .as_ref()
                        .map_or_else(String::new, |path| path.display().to_string()),
                };
                let note = if let Some(error) = error {
                    error
                } else if matches!(entry.status, ToolStatus::Installing { .. }) {
                    format!("Installing {} {version}.", definition.name)
                } else if installed {
                    match source {
                        LspRuntimeSource::Path => {
                            "Found on PATH. Dust uses it as is and never removes it.".into()
                        }
                        LspRuntimeSource::Managed | LspRuntimeSource::Nix => {
                            let how = if source == LspRuntimeSource::Nix {
                                "Installed by Dust through Nix"
                            } else {
                                "Installed by Dust"
                            };
                            match (&update, &installed_version) {
                                (Some(current), Some(installed)) => format!(
                                    "{how}: {installed}. This Dust release ships {current}; \
                                     press i to update."
                                ),
                                _ => format!(
                                    "{how}. This is the version this Dust release ships; \
                                     a newer Dust brings newer tools."
                                ),
                            }
                        }
                    }
                } else if can_install {
                    format!("Not installed. Press i to install {version}.")
                } else {
                    "Dust has no installer for this tool. Install it with your package manager \
                     so it is on PATH."
                        .into()
                };
                ToolMenuRow {
                    id: definition.id.clone(),
                    name: definition.name.clone(),
                    kind: configured
                        .categories
                        .get(tool_kind_key(definition.kind))
                        .cloned()
                        .unwrap_or_else(|| definition.kind.label().into()),
                    languages: definition
                        .languages
                        .join(crate::ui::render::tools::TOOL_LANGUAGE_SEPARATOR),
                    version,
                    release,
                    runtime,
                    lifecycle,
                    session_state,
                    executable,
                    note,
                    sessions,
                    lsp: is_lsp,
                    installed,
                    can_start: installed
                        && (!is_lsp
                            || active_language
                                .as_ref()
                                .is_none_or(|language| definition.supports_language(language))),
                    can_install,
                    can_uninstall,
                    update,
                }
            })
            .collect::<Vec<_>>();
        let selected = selected_id
            .and_then(|id| rows.iter().position(|row| row.id == id))
            .or_else(|| {
                self.ui
                    .tool_menu
                    .as_ref()
                    .and_then(|menu| menu.rows.get(menu.selected))
                    .and_then(|previous| rows.iter().position(|row| row.id == previous.id))
            })
            .unwrap_or(0)
            .min(rows.len().saturating_sub(1));
        let runtime_picker = previous_picker.and_then(|(server_id, focused, scroll)| {
            let definition = self
                .tooling
                .entries()
                .iter()
                .find(|entry| {
                    entry.definition.id == server_id && entry.definition.kind == ToolKind::Lsp
                })
                .map(|entry| entry.definition.clone())?;
            let options = self.runtime_options_for(&definition);
            let selected = focused
                .and_then(|(source, version_pin)| {
                    options.iter().position(|option| {
                        option.source == source && option.version_pin == version_pin
                    })
                })
                .unwrap_or_else(|| {
                    options
                        .iter()
                        .position(|option| option.selected)
                        .unwrap_or(0)
                });
            Some(ToolRuntimePickerState {
                server_id,
                selected,
                scroll: scroll.min(options.len().saturating_sub(1)),
                options,
            })
        });
        let visible_rows = self.tool_menu_list_visible_rows();
        let mut scroll = previous_scroll.min(rows.len().saturating_sub(1));
        if selected < scroll {
            scroll = selected;
        } else if selected >= scroll.saturating_add(visible_rows) {
            scroll = selected.saturating_add(1).saturating_sub(visible_rows);
        }
        self.ui.tool_menu = Some(ToolMenuState {
            category,
            selected,
            scroll,
            rows,
            runtime_picker,
            query,
            searching,
        });
    }
}

impl App {


    /// Set the tool search and whether keys still type into it, then filter the list again.
    pub(in crate::app) fn search_tool_menu(
        &mut self,
        query: Option<String>,
        searching: bool,
    ) {
        let Some(menu) = self.ui.tool_menu.as_mut() else {
            return;
        };
        menu.query = query.unwrap_or_default();
        menu.searching = searching;
        let category = menu.category.clone();
        self.rebuild_tool_menu(&category, None);
    }
}

impl App {


    pub(in crate::app) fn move_tool_menu(&mut self, direction: i8) {
        let list_visible = self.tool_menu_list_visible_rows();
        let picker_visible = self.tool_runtime_picker_visible_rows();
        let Some(menu) = &mut self.ui.tool_menu else {
            return;
        };
        if let Some(picker) = &mut menu.runtime_picker {
            if picker.options.is_empty() {
                return;
            }
            picker.selected = if direction < 0 {
                picker.selected.saturating_sub(1)
            } else {
                picker
                    .selected
                    .saturating_add(1)
                    .min(picker.options.len() - 1)
            };
            let visible = picker_visible;
            if picker.selected < picker.scroll {
                picker.scroll = picker.selected;
            } else if picker.selected >= picker.scroll.saturating_add(visible) {
                picker.scroll = picker.selected.saturating_add(1).saturating_sub(visible);
            }
            return;
        }
        if menu.rows.is_empty() {
            return;
        }
        menu.selected = if direction < 0 {
            menu.selected.saturating_sub(1)
        } else {
            menu.selected.saturating_add(1).min(menu.rows.len() - 1)
        };
        let visible = list_visible;
        if menu.selected < menu.scroll {
            menu.scroll = menu.selected;
        } else if menu.selected >= menu.scroll.saturating_add(visible) {
            menu.scroll = menu.selected.saturating_add(1).saturating_sub(visible);
        }
    }
}

impl App {


    pub(in crate::app) fn cycle_tool_category(&mut self, direction: i8) {
        let Some(menu) = &self.ui.tool_menu else {
            return;
        };
        if menu.runtime_picker.is_some() {
            return;
        }
        let order = &TOOL_CATEGORY_ORDER;
        let current = order
            .iter()
            .position(|category| category == &menu.category)
            .unwrap_or(0);
        let next = if direction < 0 {
            if current == 0 {
                order.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % order.len()
        };
        let category = order[next];
        self.queue_tool_version_probes(category);
        self.rebuild_tool_menu(category, None);
    }
}

impl App {


    pub(in crate::app) fn selected_tool_id(&self) -> Option<String> {
        let menu = self.ui.tool_menu.as_ref()?;
        menu.runtime_picker
            .as_ref()
            .map(|picker| picker.server_id.clone())
            .or_else(|| menu.rows.get(menu.selected).map(|row| row.id.clone()))
    }
}

impl App {


    pub(in crate::app) fn focused_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> LspRuntimeSource {
        self.ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.runtime_picker.as_ref())
            .filter(|picker| picker.server_id == definition.id)
            .and_then(|picker| picker.options.get(picker.selected))
            .and_then(|option| match option.source.as_str() {
                "path" => Some(LspRuntimeSource::Path),
                "managed" => Some(LspRuntimeSource::Managed),
                "nix" => Some(LspRuntimeSource::Nix),
                _ => None,
            })
            .unwrap_or_else(|| self.selected_lsp_runtime_source(definition))
    }
}

impl App {


    pub(in crate::app) fn focused_managed_version(
        &self,
        definition: &ToolDefinition,
    ) -> Option<String> {
        self.ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.runtime_picker.as_ref())
            .filter(|picker| picker.server_id == definition.id)
            .and_then(|picker| picker.options.get(picker.selected))
            .filter(|option| {
                matches!(
                    option.source.as_str(),
                    source if source == LspRuntimeSource::Managed.key()
                        || source == LspRuntimeSource::Nix.key()
                )
            })
            .and_then(|option| option.version_pin.clone())
            .or_else(|| self.selected_managed_definition(definition).version)
    }
}

impl App {


    pub(in crate::app) fn toggle_selected_lsp_runtime(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let Some(definition) = self
            .tooling
            .entries()
            .iter()
            .find(|entry| entry.definition.id == id)
            .map(|entry| entry.definition.clone())
        else {
            return;
        };
        if definition.kind != ToolKind::Lsp {
            self.show_error("runtime selection is available only for language servers");
            return;
        }
        let options = self.runtime_options_for(&definition);
        let selected = options
            .iter()
            .position(|option| option.selected)
            .unwrap_or(0);
        if let Some(menu) = &mut self.ui.tool_menu {
            menu.runtime_picker = Some(ToolRuntimePickerState {
                server_id: id,
                selected,
                scroll: selected.saturating_sub(2),
                options,
            });
        }
    }
}

impl App {


    pub(in crate::app) fn accept_selected_lsp_runtime(&mut self) {
        let Some((id, source, version_pin, installed, installable)) =
            self.ui.tool_menu.as_ref().and_then(|menu| {
                let picker = menu.runtime_picker.as_ref()?;
                let option = picker.options.get(picker.selected)?;
                Some((
                    picker.server_id.clone(),
                    option.source.clone(),
                    option.version_pin.clone(),
                    option.installed,
                    option.installable,
                ))
            })
        else {
            return;
        };
        let source = match source.as_str() {
            "path" => LspRuntimeSource::Path,
            "managed" => LspRuntimeSource::Managed,
            "nix" => LspRuntimeSource::Nix,
            _ => return,
        };
        if !installed && !installable {
            self.show_error("this runtime is not installed or installable on the current host");
            return;
        }
        if let Err(error) = self.tooling.select_runtime(&id, source, version_pin) {
            self.show_error(error);
            return;
        }
        self.language.cancel_launches(&id);
        self.stop_lsp_sessions_for_server(&id);
        self.language.reset_autostart();
        if let Some(menu) = &mut self.ui.tool_menu {
            menu.runtime_picker = None;
        }
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        if !installed && installable {
            self.rebuild_tool_menu(&category, Some(&id));
            self.install_tool_by_id(&id, Some(source));
            return;
        }
        self.notify_message(
            match source {
                LspRuntimeSource::Managed => format!(
                    "{id} now uses Dust {}",
                    self.tooling
                        .managed_version(&id)
                        .map(String::as_str)
                        .unwrap_or("unknown")
                ),
                _ => format!("{id} now uses {}", source.label()),
            },
            false,
        );
        self.rebuild_tool_menu(&category, Some(&id));
    }
}

impl App {


    pub(in crate::app) fn close_tool_menu(&mut self) {
        let Some(menu) = &mut self.ui.tool_menu else {
            return;
        };
        if menu.runtime_picker.take().is_some() {
            let category = menu.category.clone();
            let selected = menu.rows.get(menu.selected).map(|row| row.id.clone());
            self.rebuild_tool_menu(&category, selected.as_deref());
        } else {
            self.ui.tool_menu = None;
        }
    }
}

impl App {


    pub(in crate::app) fn stop_lsp_sessions_for_server(&mut self, id: &str) {
        self.language.retire_server_launches(id);
        let sessions = self
            .language
            .sessions()
            .iter()
            .filter_map(|(key, session)| (session.server_id == id).then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in sessions {
            self.language.request_stop(&key);
            let _ = self.language.stop(&key);
            self.clear_lsp_progress(&key);
            self.clear_lsp_server_ownership(&key);
            self.language.remove(&key);
            self.language.forget_synced(&key);
            if self.language.active().map(String::as_str) == Some(&key) {
                self.clear_lsp_session_state();
            }
        }
    }
}

impl App {

    pub(in crate::app) fn refresh_tools(&mut self) {
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "all".into(), |menu| menu.category.clone());
        let selected = self.selected_tool_id();
        self.tooling.refresh();
        self.queue_tool_version_probes(&category);
        self.rebuild_tool_menu(&category, selected.as_deref());
    }
}

impl App {


    pub(in crate::app) fn process_tool_version_results(&mut self) {
        let (changed, unavailable_rustup) = self
            .tooling
            .poll_versions(self.workspace.execution_allowed().is_ok());
        if changed {
            self.language.reset_autostart();
            let category = self.ui.tool_menu.as_ref().map(|menu| menu.category.clone());
            let selected = self.selected_tool_id();
            if let Some(category) = category {
                self.rebuild_tool_menu(&category, selected.as_deref());
            }
        }
        // With automatic installs on, the next autostart pass adds it with rustup instead.
        if unavailable_rustup && !self.configuration.settings().lsp.auto_install {
            self.show_error(Self::unavailable_rust_analyzer_message());
        }
    }
}

impl App {


    pub(in crate::app) fn activate_selected_tool(&mut self) {
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
}

impl App {


    pub(in crate::app) fn activate_tool_by_id(&mut self, id: &str) {
        let Some(index) = self
            .tooling
            .entries()
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let definition = self.tooling.entries()[index].definition.clone();
        let path_available = matches!(
            self.tooling.entries()[index].status,
            ToolStatus::Installed(_)
        );
        if definition.kind == ToolKind::Lsp {
            if self.selected_lsp_runtime_available(&definition) {
                if self
                    .active_lsp_document_without_text()
                    .is_some_and(|(_, document)| {
                        !definition.supports_language(&document.language_id)
                    })
                {
                    self.notify_message(
                        format!(
                            "{} is installed. Open a supported file to start it.",
                            definition.name
                        ),
                        false,
                    );
                } else {
                    self.launch_lsp(Some(id.into()), true);
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
                    let key = self.tool_version_key(&definition, source, Some(&path));
                    if matches!(self.tooling.version(&key), Some(Err(_))) {
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
                ToolKind::Agent => self.start_agent_for_program(&definition.executable),
                ToolKind::Formatter | ToolKind::Linter => {
                    self.notify_message(
                        self.message(
                            "tool_install_succeeded",
                            &[("tool", definition.name.clone())],
                        ),
                        false,
                    );
                }
            }
            return;
        }
        if let ToolStatus::Unavailable(diagnostic) = &self.tooling.entries()[index].status {
            self.show_error(diagnostic.clone());
            return;
        }
        if definition.managed_versions().is_empty() {
            self.show_error(format!(
                "{} is not installed and Dust has no installer for it",
                definition.name
            ));
            return;
        }
        self.install_tool_by_id(id, None);
    }
}

impl App {


    pub(in crate::app) fn install_selected_tool(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let focused = self
            .tooling
            .entries()
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
        let menu = self.ui.tool_menu.as_ref();
        let update = menu
            .filter(|menu| menu.runtime_picker.is_none())
            .and_then(|menu| menu.rows.iter().find(|row| row.id == id))
            .and_then(|row| row.update.clone());
        let focused = focused.map(|(source, version)| (source, update.or(version)));
        if let Some((source, Some(version))) = focused
            && matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
            && let Err(error) = self.tooling.select_version(&id, version)
        {
            self.show_error(error);
            return;
        }
        self.install_tool_by_id(&id, source);
    }
}

impl App {


    /// rustup and its rust-analyzer proxy, when the proxy on PATH fails because the toolchain
    /// lacks the component, so `rustup component add` can fix it.
    fn rust_analyzer_rustup_install(
        &self,
        definition: &ToolDefinition,
    ) -> Option<(PathBuf, PathBuf)> {
        let (rustup, proxy) = self.rustup_toolchain(definition)?;
        let key = self.tool_version_key(definition, LspRuntimeSource::Path, Some(&proxy));
        matches!(self.tooling.version(&key), Some(Err(_))).then_some((rustup, proxy))
    }
}

impl App {


    /// rustup and the rust-analyzer proxy on PATH, when rustup manages the toolchain.
    fn rustup_toolchain(&self, definition: &ToolDefinition) -> Option<(PathBuf, PathBuf)> {
        if definition.id != "rust-analyzer" {
            return None;
        }
        let (proxy, rustup_proxy) = self.tooling.path_executable(&definition.executable)?;
        let rustup = proxy.with_file_name("rustup");
        (rustup_proxy && rustup.is_file()).then_some((rustup, proxy))
    }
}

impl App {


    /// Shows a server's warning. When rust-analyzer can't run build scripts or proc macros
    /// on a rustup toolchain, the toolchain usually lacks a component, so Dust adds the
    /// components once and restarts the server instead.
    pub(in crate::app) fn report_lsp_status(&mut self, key: &str, message: &str) {
        let Some(definition) = self
            .language
            .sessions()
            .get(key)
            .map(|state| state.definition.clone())
        else {
            return;
        };
        let summary = message
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("the workspace loaded only in part")
            .to_owned();
        let label = self.lsp_server_label(key);
        let lower = message.to_lowercase();
        let toolchain_failure = ["build script", "proc-macro", "proc macro"]
            .iter()
            .any(|needle| lower.contains(needle));
        if toolchain_failure
            && self.configuration.settings().lsp.auto_install
            && let Some((rustup, proxy)) = self.rustup_toolchain(&definition)
            && let Some(cwd) = self.tool_probe_directory()
            && self.workspace.execution_allowed().is_ok()
            && self
                .workspace
                .authorize_execution(ExecutionSource::ToolInstall)
                .is_ok()
            && self.language.repair_once(&definition.id)
            && self
                .tooling
                .install(ToolInstall::Rustup {
                    definition,
                    rustup,
                    proxy,
                    cwd,
                })
                .is_ok()
        {
            self.notify_message(
                format!("{label}: {summary} Adding the missing toolchain parts with rustup…"),
                false,
            );
            return;
        }
        self.notify_message(format!("{label}: {summary}"), true);
    }
}

impl App {


    /// Servers running from this tool, which a toolchain repair restarts.
    fn lsp_sessions_for_tool(&self, id: &str) -> Vec<String> {
        self.language
            .sessions()
            .iter()
            .filter(|(_, state)| state.definition.id == id && state.desired_running)
            .map(|(key, _)| key.clone())
            .collect()
    }
}

impl App {


    pub(in crate::app) fn restart_lsp(&mut self, server: &str) {
        self.language.request_start(server, Some(Instant::now()));
        let _ = self.language.stop(server);
        self.language.forget_status(server);
        self.clear_lsp_progress(server);
    }
}

impl App {


    pub(in crate::app) fn install_tool_by_id(
        &mut self,
        id: &str,
        source: Option<LspRuntimeSource>,
    ) {
        self.install_tool(id, source);
    }
}

impl App {

    pub(in crate::app) fn install_tool(
        &mut self,
        id: &str,
        source: Option<LspRuntimeSource>,
    ) {
        let Some(index) = self
            .tooling
            .entries()
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let base_definition = self.tooling.entries()[index].definition.clone();
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
        let rustup_install = (source == LspRuntimeSource::Path)
            .then(|| self.rust_analyzer_rustup_install(&definition))
            .flatten();
        if definition.kind == ToolKind::Lsp {
            match source {
                LspRuntimeSource::Path if rustup_install.is_none() => {
                    if discover_executable(&definition.executable).is_some() {
                        self.notify_message(format!(
                            "{} is already installed on PATH; Dust leaves external software unchanged",
                            definition.name
                        ), false);
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
                    if self.managed_variant_installed(
                        &self.tooling.entries()[index].definition,
                        &definition,
                    ) =>
                {
                    self.notify_message(
                        format!(
                            "{} {} is already installed",
                            definition.name,
                            definition.version.as_deref().unwrap_or("unknown")
                        ),
                        false,
                    );
                    return;
                }
                LspRuntimeSource::Nix
                    if nix_lsp_profile_installed(&definition)
                        && self.managed_variant_installed(
                            &self.tooling.entries()[index].definition,
                            &definition,
                        ) =>
                {
                    self.notify_message(format!(
                        "{} {} is already installed through Dust's Nix compatibility environment",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ), false);
                    return;
                }
                LspRuntimeSource::Managed | LspRuntimeSource::Nix => {}
            }
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::ToolInstall)
        {
            self.show_error(error);
            return;
        }
        let operation = if let Some((rustup, proxy)) = rustup_install {
            let Some(cwd) = self.tool_probe_directory() else {
                self.show_error("rustup installation requires an active workspace root");
                return;
            };
            self.tooling.install(ToolInstall::Rustup {
                definition: definition.clone(),
                rustup,
                proxy,
                cwd,
            })
        } else if definition.kind == ToolKind::Lsp && source == LspRuntimeSource::Nix {
            let Some(nix) = discover_executable("nix") else {
                self.show_error("Dust via Nix is unavailable: nix was not found");
                return;
            };
            if !self.nix_workspace_available_for(&definition) {
                self.show_error("no verified upstream artifact or Dust Nix runtime is available for this language server");
                return;
            }
            self.tooling
                .install(ToolInstall::Nix(definition.clone(), nix))
        } else {
            if let ToolStatus::Unavailable(diagnostic) = &self.tooling.entries()[index].status {
                self.show_error(diagnostic.clone());
                return;
            }
            if definition.install.is_none() {
                self.show_error(self.message("tool_no_installer", &[("tool", definition.name)]));
                return;
            }
            self.tooling
                .install(ToolInstall::Managed(definition.clone()))
        };
        match operation {
            Ok(()) => {
                self.notify_message(
                    match (source, definition.version.as_deref()) {
                        (LspRuntimeSource::Path, _) => {
                            format!("installing {} with rustup…", definition.name)
                        }
                        (_, Some(version)) => format!("installing {} {version}…", definition.name),
                        (_, None) => format!("installing {}…", definition.name),
                    },
                    false,
                );
                // An automatic install only notifies; it doesn't open the tools menu.
                if let Some(menu) = &self.ui.tool_menu {
                    let category = menu.category.clone();
                    self.rebuild_tool_menu(&category, Some(id));
                }
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    pub(in crate::app) fn uninstall_selected_tool(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let Some(index) = self
            .tooling
            .entries()
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let base_definition = self.tooling.entries()[index].definition.clone();
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
        let operation =
            match source {
                LspRuntimeSource::Path => {
                    self.notify_message(
                    "This server is owned by PATH, not Dust; remove it with its package manager"
                        .into(), false);
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
                    self.language.cancel_launches(&id);
                    self.stop_lsp_sessions_for_server(&id);
                    self.tooling
                        .install(ToolInstall::Uninstall(definition.clone(), false))
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
                    self.language.cancel_launches(&id);
                    self.stop_lsp_sessions_for_server(&id);
                    self.tooling
                        .install(ToolInstall::Uninstall(definition.clone(), true))
                }
            };
        match operation {
            Ok(()) => {
                self.notify_message(
                    format!(
                        "uninstalling {} {} runtime…",
                        definition.name,
                        source.label()
                    ),
                    false,
                );
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
}

impl App {


    pub(in crate::app) fn stop_selected_lsp(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.language.cancel_launches(&id);
        self.stop_lsp_sessions_for_server(&id);
        self.language.mark_stopped(&id);
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        self.rebuild_tool_menu(&category, Some(&id));
    }
}

impl App {


    pub(in crate::app) fn restart_selected_lsp(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.language.cancel_launches(&id);
        self.stop_lsp_sessions_for_server(&id);
        self.launch_lsp(Some(id.clone()), true);
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        self.rebuild_tool_menu(&category, Some(&id));
    }
}

impl App {


    pub(in crate::app) fn process_tool_progress(&mut self) {
        let changed = self.tooling.poll_progress();
        if let (Some(_), Some(menu)) = (changed, &self.ui.tool_menu) {
            self.rebuild_tool_menu(&menu.category.clone(), None);
        }
    }
}

impl App {


    /// When rustup can't add rust-analyzer, install Dust's pinned standalone build instead, as
    /// mason does, so automatic installs still end with a working server.
    fn install_rust_analyzer_download(&mut self, id: &str, diagnostic: &str) -> bool {
        if !self.configuration.settings().lsp.auto_install {
            return false;
        }
        let Some(definition) = self
            .tooling
            .entries()
            .iter()
            .find(|entry| entry.definition.id == id)
            .map(|entry| entry.definition.clone())
        else {
            return false;
        };
        let source = if is_nixos_host() {
            LspRuntimeSource::Nix
        } else {
            LspRuntimeSource::Managed
        };
        if source == LspRuntimeSource::Nix && !self.nix_workspace_available_for(&definition)
            || self
                .tooling
                .select_runtime(id, source, definition.version.clone())
                .is_err()
        {
            return false;
        }
        self.install_tool(id, Some(source));
        let installing = self.tooling.entries().iter().any(|entry| {
            entry.definition.id == id && matches!(entry.status, ToolStatus::Installing { .. })
        });
        if installing {
            self.notify_message(
                format!(
                    "rustup could not add {} ({diagnostic}); installing Dust's build instead…",
                    definition.name
                ),
                false,
            );
        }
        installing
    }
}

impl App {


    pub(in crate::app) fn process_tool_results(&mut self) {
        while let Some((name, result)) = self.tooling.poll_install() {
            // With the server already running, rustup was repairing the toolchain, which a
            // different rust-analyzer build wouldn't fix.
            let running = if result.operation == ToolOperation::InstallRustup {
                self.lsp_sessions_for_tool(&result.id)
            } else {
                Vec::new()
            };
            if let Err(diagnostic) = &result.result
                && result.operation == ToolOperation::InstallRustup
                && running.is_empty()
                && self.install_rust_analyzer_download(&result.id, diagnostic)
            {
                continue;
            }
            if result.result.is_ok() {
                for server in &running {
                    self.restart_lsp(server);
                }
            }
            if let Err(diagnostic) = result.result {
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
                self.notify_message(
                    match result.operation {
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
                    },
                    false,
                );
                self.language.clear_error(&result.id);
            }
            self.language.reset_autostart();
            if let Some(menu) = &self.ui.tool_menu {
                let category = menu.category.clone();
                self.queue_tool_version_probes(&category);
                self.rebuild_tool_menu(&category, None);
            }
        }
    }
}



/// The first word of a reported version, so `0.1.99 (58cf8f9 2025-01-01)` reads `0.1.99`.
fn short_tool_version(version: &str) -> String {
    match version {
        "version unavailable" | "probe failed" | "unknown" | "" => "—".into(),
        _ => version.split_whitespace().next().unwrap_or("—").to_owned(),
    }
}


pub(in crate::app) fn tool_install_progress_label(
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
    // The percentage comes first so a narrow column still shows progress.
    match total.filter(|total| *total > 0) {
        Some(total) => format!(
            "{phase} {}% ({} / {})",
            completed.saturating_mul(100).min(total.saturating_mul(100)) / total,
            bytes(completed),
            bytes(total),
        ),
        None => phase.into(),
    }
}