use super::*;

impl InteractiveApplication {
    fn tool_menu_height(&self) -> u16 {
        let configured = &self.configuration.settings().ui.tool_manager;
        let height = (u32::from(self.terminal_height) * u32::from(configured.height_percent) / 100)
            .try_into()
            .unwrap_or(self.terminal_height);
        height
            .max(configured.minimum_height.min(self.terminal_height))
            .min(self.terminal_height)
    }

    fn tool_menu_list_visible_rows(&self) -> usize {
        // Rendering reserves the border, up to two wrapped help rows, tabs,
        // heading, separator, and ten detail rows. Using the two-help-row
        // bound keeps navigation safe when the help text wraps after resize.
        usize::from(self.tool_menu_height().saturating_sub(18)).max(1)
    }

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
            .max(self.configuration.settings().ui.tool_manager.minimum_height);
        usize::from(
            self.tool_menu_height()
                .min(compact_height)
                .saturating_sub(9)
                / 2,
        )
        .max(1)
    }

    pub(in crate::editor::session) fn open_tool_menu(&mut self, category: String) {
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.close_prompt();
        // `:lsp` is normally opened from a source buffer. Put the matching
        // server under the cursor so Enter and runtime selection act on the
        // server the user is actually editing, not an alphabetically earlier
        // unrelated adapter.
        let selected = (category == "lsp")
            .then(|| {
                self.active_lsp_document()
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

    pub(in crate::editor::session) fn lsp_menu_lifecycle(&self, id: &str) -> (String, Vec<String>) {
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
                    "failed"
                } else if session.restart_due.is_some() {
                    backoff = true;
                    "backoff"
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
        starting = starting
            || self
                .pending_lsp_project_root
                .as_ref()
                .is_some_and(|(_, pending)| pending.definition.id == id)
            || self
                .pending_nix_lsp_workspace
                .as_ref()
                .is_some_and(|(_, pending)| pending.definition.id == id)
            || self.pending_execution_preflight.values().any(|pending| {
                matches!(pending, PendingExecutionPreflight::Lsp { definition, .. } if definition.id == id)
            });
        let lifecycle = if running > 0 {
            if running == 1 {
                "running".into()
            } else {
                format!("running ×{running}")
            }
        } else if starting {
            "starting".into()
        } else if backoff {
            "backoff".into()
        } else if failed || self.language.errors().contains_key(id) {
            "failed".into()
        } else {
            "stopped".into()
        };
        (lifecycle, sessions)
    }

    pub(in crate::editor::session) fn runtime_options_for(
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
            location: path.as_ref().map_or_else(
                || "not installed on PATH".into(),
                |path| path.display().to_string(),
            ),
            status: if path.is_none() {
                "not installed"
            } else if path_error.is_some() {
                "unavailable"
            } else if selected_source == LspRuntimeSource::Path {
                "in use"
            } else {
                "available"
            }
            .into(),
            note: if path.is_none() {
                "No external executable is installed on PATH; Dust cannot install or uninstall PATH-owned software."
                    .into()
            } else if rustup_installable {
                "The rustup proxy has no rust-analyzer component for the active toolchain; press i to install it with rustup."
                    .into()
            } else if path_version == "version unavailable" {
                "Installed on PATH; this compatibility entry has no qualified version command."
                    .into()
            } else {
                path_error.unwrap_or_else(|| {
                    if definition.id == "rust-analyzer" {
                        if rustup_bundled {
                            format!(
                                "The PATH executable is a rustup proxy reporting `{path_version}`. VERSION identifies it as rustup bundled; BUILD / RELEASE shows its reported date."
                            )
                        } else {
                            format!(
                                "The PATH executable reports `{path_version}`. VERSION shows its executable version and BUILD / RELEASE shows its reported date."
                            )
                        }
                    } else {
                        format!(
                            "The external PATH executable reports `{path_version}`; Dust can select it but never replaces or removes it."
                        )
                    }
                })
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
                let (observed_version, probe_error) = if installed {
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
                release: variant.release.clone().unwrap_or_else(|| "catalog build".into()),
                location: if installed {
                    managed_path.as_ref().map_or_else(
                        || "installed path unavailable".into(),
                        |path| path.display().to_string(),
                    )
                } else {
                    "not installed".into()
                },
                status: if installed && selected_source == LspRuntimeSource::Managed
                    && selected_managed.version.as_deref() == Some(version.as_str())
                {
                    "in use"
                } else if installed {
                    "installed copy"
                } else if selected_source == LspRuntimeSource::Managed
                    && selected_managed.version.as_deref() == Some(version.as_str())
                {
                    "selected, not installed"
                } else {
                    "available to install"
                }
                .into(),
                note: probe_error.unwrap_or_else(|| {
                    if installed && !observed_version.is_empty() && observed_version != "probing…" {
                        format!(
                            "{} {version} is an installed Dust download; the executable reports {observed_version}.",
                            definition.name
                        )
                    } else if installed {
                        format!(
                            "{} {version} is installed as a separate Dust-owned copy.",
                            definition.name
                        )
                    } else if managed_path.is_some() {
                        format!(
                            "{} {version} is available; i replaces the other Dust-download version only. PATH is untouched.",
                            definition.name
                        )
                    } else {
                        format!(
                            "{} {version} is available as a verified Dust download; press i to install it.",
                            definition.name
                        )
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
                    location: "no Dust download available".into(),
                    status: "unavailable".into(),
                    note: "No portable managed artifact is published for this server.".into(),
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
                let (observed_version, probe_error) = if installed {
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
                    release: variant.release.clone().unwrap_or_else(|| "catalog build".into()),
                    location: if installed {
                        managed_path.as_ref().map_or_else(
                            || "installed artifact path unavailable".into(),
                            |path| {
                                format!(
                                    "{} · compatibility {}",
                                    path.display(),
                                    nix_lsp_workspace_root()
                                        .join("profiles")
                                        .join(&definition.id)
                                        .display()
                                )
                            },
                        )
                    } else {
                        "not installed".into()
                    },
                    status: if installed
                        && selected_source == LspRuntimeSource::Nix
                        && selected_managed.version.as_deref() == Some(version.as_str())
                    {
                        "in use"
                    } else if installed {
                        "installed copy"
                    } else if nix_available
                        && selected_source == LspRuntimeSource::Nix
                        && selected_managed.version.as_deref() == Some(version.as_str())
                    {
                        "selected, not installed"
                    } else if nix_available {
                        "available to install"
                    } else {
                        "unavailable"
                    }
                    .into(),
                    note: probe_error.unwrap_or_else(|| {
                        if installed && !observed_version.is_empty() && observed_version != "probing…" {
                            format!(
                                "Verified upstream {} {version} runs through Dust's private Nix compatibility environment and reports {observed_version}.",
                                definition.name
                            )
                        } else {
                            format!(
                                "The same checksum-pinned upstream {} {version} is processed by Dust's private Nix runtime at snapshot {}.",
                                definition.name,
                                &NIX_LSP_NIXPKGS_REVISION[..8]
                            )
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

    pub(in crate::editor::session) fn rebuild_tool_menu(
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
        let category = if configured.categories.contains_key(category) {
            category.to_owned()
        } else {
            "all".into()
        };
        let active_language = self
            .active_lsp_document()
            .map(|(_, document)| document.language_id);
        let mut entries = self
            .tooling
            .entries()
            .iter()
            .filter(|entry| category == "all" || tool_kind_key(entry.definition.kind) == category)
            .filter(|entry| {
                entry.definition.kind != ToolKind::Lsp
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
                let observed_definition = if matches!(
                    source,
                    LspRuntimeSource::Managed | LspRuntimeSource::Nix
                ) {
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
                    .then(|| {
                        Self::rust_analyzer_path_version(&reported_version, rustup_bundled)
                    });
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
                let (mut lifecycle, sessions) = if is_lsp {
                    self.lsp_menu_lifecycle(&definition.id)
                } else {
                    ("idle".into(), Vec::new())
                };
                if let ToolStatus::Installing {
                    phase,
                    completed,
                    total,
                } = &entry.status
                {
                    lifecycle = tool_install_progress_label(phase, *completed, *total);
                }
                let source_installed = match source {
                    LspRuntimeSource::Path => path.is_some(),
                    LspRuntimeSource::Managed => managed.is_some(),
                    LspRuntimeSource::Nix => {
                        nix_lsp_profile_installed(definition)
                            && self.managed_variant_installed(definition, &managed_definition)
                    },
                };
                if source == LspRuntimeSource::Path
                    && probe_error.is_some()
                    && lifecycle == "stopped"
                {
                    lifecycle = "unavailable".into();
                }
                let runtime = if source_installed {
                    source.label().into()
                } else {
                    match source {
                        LspRuntimeSource::Path => {
                            version = "—".into();
                            release = "—".into();
                            if lifecycle == "stopped" {
                                lifecycle = "not installed".into();
                            }
                            "Not installed".into()
                        }
                        LspRuntimeSource::Managed | LspRuntimeSource::Nix => {
                            if lifecycle == "stopped" {
                                lifecycle = "not installed".into();
                            }
                            source.label().into()
                        }
                    }
                };
                let executable = match source {
                    LspRuntimeSource::Path => path.as_ref().map_or_else(
                        || "not installed on PATH".into(),
                        |path| path.display().to_string(),
                    ),
                    LspRuntimeSource::Managed => managed.as_ref().map_or_else(
                        || "not installed in Dust state".into(),
                        |path| path.display().to_string(),
                    ),
                    LspRuntimeSource::Nix => managed.as_ref().map_or_else(
                        || "verified upstream artifact is not installed".into(),
                        |path| {
                            format!(
                                "{} · Nix compatibility {}",
                                path.display(),
                                nix_lsp_workspace_root()
                                    .join("profiles")
                                    .join(&definition.id)
                                    .display()
                            )
                        },
                    ),
                };
                let installed_managed_version = managed_tool_installed_version(definition)
                    .map(|installed| {
                        definition
                            .managed_version(&installed)
                            .and_then(|version| version.version)
                            .unwrap_or(installed)
                    })
                    .or_else(|| {
                        managed_tool_executable(definition).and_then(|_| definition.version.clone())
                    });
                let managed_status = if is_lsp && nixos_host {
                    String::new()
                } else {
                    installed_managed_version.map_or_else(
                        || {
                            format!(
                                "{} · selected · not installed · {} available version(s)",
                                managed_definition
                                    .version
                                    .as_deref()
                                    .unwrap_or("version unknown"),
                                definition.managed_versions().len()
                            )
                        },
                        |version| {
                            format!(
                                "{version} · installed copy · {} available version(s)",
                                definition.managed_versions().len()
                            )
                        },
                    )
                };
                let nix_status = if !is_lsp || !nixos_host {
                    String::new()
                } else if !self.nix_workspace_available_for(definition) {
                    "unavailable on this host".into()
                } else {
                    format!(
                        "{} · release {} · {} · compatibility {}",
                        managed_definition.version.as_deref().unwrap_or("unknown"),
                        managed_definition.release.as_deref().unwrap_or("catalog"),
                        if nix_lsp_profile_installed(definition)
                            && self.managed_variant_installed(definition, &managed_definition)
                        {
                            "installed upstream artifact"
                        } else {
                            "not installed"
                        },
                        &NIX_LSP_NIXPKGS_REVISION[..8]
                    )
                };
                let note = self.language.errors()
                    .get(&definition.id)
                    .cloned()
                    .or(probe_error)
                    .or_else(|| match &entry.status {
                        ToolStatus::Failed(error) | ToolStatus::Unavailable(error) => {
                            Some(error.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| match source {
                        LspRuntimeSource::Path
                            if path.is_some() && reported_version == "version unavailable" =>
                        {
                            "Installed on PATH; this compatibility entry has no qualified version command."
                                .into()
                        }
                        LspRuntimeSource::Path if path.is_some() => {
                            "PATH has priority; Dust will not modify or uninstall this executable."
                                .into()
                        }
                        LspRuntimeSource::Path => {
                            "Not installed. This compatibility entry appears only when an external executable or Dust installer is available."
                                .into()
                        }
                        LspRuntimeSource::Managed if managed.is_some() => format!(
                            "Dust download {} is ACTIVE; u removes this private copy.",
                            managed_definition.version.as_deref().unwrap_or("unknown")
                        ),
                        LspRuntimeSource::Managed => format!(
                            "Dust download {} is ACTIVE but not installed; press i.",
                            managed_definition.version.as_deref().unwrap_or("unknown")
                        ),
                        LspRuntimeSource::Nix
                            if nix_lsp_profile_installed(definition)
                                && self.managed_variant_installed(
                                    definition,
                                    &managed_definition,
                                ) =>
                        {
                            "The verified upstream artifact is ACTIVE through Dust's private Nix compatibility environment.".into()
                        }
                        LspRuntimeSource::Nix => {
                            "The Dust via Nix artifact is ACTIVE but not installed; press i."
                                .into()
                        }
                    });
                ToolMenuRow {
                    id: definition.id.clone(),
                    name: definition.name.clone(),
                    kind: configured
                        .categories
                        .get(tool_kind_key(definition.kind))
                        .cloned()
                        .unwrap_or_else(|| definition.kind.label().into()),
                    languages: definition.languages.join(&configured.language_separator),
                    version,
                    release,
                    runtime,
                    lifecycle,
                    executable,
                    note,
                    sessions,
                    managed_status,
                    nix_status,
                    installed: source_installed,
                    can_start: source_installed
                        && (!is_lsp
                            || active_language.as_ref().is_none_or(|language| {
                                definition.supports_language(language)
                            })),
                    can_install: if is_lsp && nixos_host {
                        self.nix_workspace_available_for(definition)
                    } else {
                        !definition.managed_versions().is_empty()
                    },
                    can_uninstall: if is_lsp && nixos_host {
                        nix_lsp_profile_installed(definition)
                            && self.managed_variant_installed(definition, &managed_definition)
                    } else {
                        managed.is_some()
                    },
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
        });
    }

    pub(in crate::editor::session) fn move_tool_menu(&mut self, direction: i8) {
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

    pub(in crate::editor::session) fn cycle_tool_category(&mut self, direction: i8) {
        let Some(menu) = &self.ui.tool_menu else {
            return;
        };
        if menu.runtime_picker.is_some() {
            return;
        }
        let order = &self.configuration.settings().ui.tool_manager.category_order;
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
        let category = order[next].clone();
        self.rebuild_tool_menu(&category, None);
    }

    pub(in crate::editor::session) fn selected_tool_id(&self) -> Option<String> {
        let menu = self.ui.tool_menu.as_ref()?;
        menu.runtime_picker
            .as_ref()
            .map(|picker| picker.server_id.clone())
            .or_else(|| menu.rows.get(menu.selected).map(|row| row.id.clone()))
    }

    pub(in crate::editor::session) fn focused_runtime_source(
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

    pub(in crate::editor::session) fn focused_managed_version(
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

    pub(in crate::editor::session) fn toggle_selected_lsp_runtime(&mut self) {
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

    pub(in crate::editor::session) fn accept_selected_lsp_runtime(&mut self) {
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
                    "{id} ACTIVE build: Dust download {}",
                    self.tooling
                        .managed_version(&id)
                        .map(String::as_str)
                        .unwrap_or("unknown")
                ),
                _ => format!("{id} ACTIVE installation: {}", source.label()),
            },
            false,
        );
        self.rebuild_tool_menu(&category, Some(&id));
    }

    pub(in crate::editor::session) fn close_tool_menu(&mut self) {
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

    pub(in crate::editor::session) fn stop_lsp_sessions_for_server(&mut self, id: &str) {
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
