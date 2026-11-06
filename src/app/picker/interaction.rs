use crate::app::App;
use crate::app::commands::PickerKind;
use crate::app::picker::MatcherSource;
use crate::app::picker::PickerContent;
use crate::app::picker::PickerOrigin;
use crate::app::picker::PickerPayload;
use crate::app::search::MAXIMUM_PICKER_ITEMS;
use crate::app::search::SEARCH_MAXIMUM_FILES;
use crate::editor::text::ByteOffset;
use crate::editor::text::PositionEncoding;
use crate::editor::text::TextBuffer;
use crate::language::lsp::LspFeature;
use crate::language::lsp::LspMarkupContent;
use crate::language::lsp::path_from_file_uri;
use crate::persistence::PersistedLocation;
use crate::tools::RegistryRequest;
use crate::ui::DocumentationPopupKind;
use crate::ui::PickerSurfaceKind;
use crate::ui::render::SCRATCH_LABEL;
use crate::ui::render::editor::STATUS_DIRTY_MARKER;
use crate::ui::render::picker::PICKER_MAXIMUM_PREVIEW_BYTES;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::workspace::PickerState;
use std::path::PathBuf;

use crate::app::documents::coordination::RECOVERY_PREVIEW_LINES;

impl App {
    pub(in crate::app) fn install_picker(
        &mut self,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
    ) {
        self.install_surface_picker(PickerSurfaceKind::Generic, title, items);
    }
}

impl App {
    pub(in crate::app) fn install_surface_picker(
        &mut self,
        surface: PickerSurfaceKind,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
    ) {
        self.install_surface_picker_state(
            surface,
            title,
            items,
            String::new(),
            true,
            PickerOrigin::General,
        );
    }
}

impl App {
    pub(in crate::app) fn install_surface_picker_state(
        &mut self,
        surface: PickerSurfaceKind,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
        query: String,
        complete: bool,
        origin: PickerOrigin,
    ) {
        if let PickerOrigin::PluginProvider(request) = origin
            && !self.extension.provider_presentable(request)
        {
            self.show_error("plugin picker source retired");
            return;
        }
        let retired_preview = self
            .search
            .workspace_picker()
            .filter(|id| !matches!(origin, PickerOrigin::WorkspaceEdit(current) if current == *id));
        let previous_epoch = self.search.picker_epoch().cloned();
        let epoch = match self.search.install_picker(
            PickerContent {
                surface,
                origin,
                title,
                items,
                query,
                complete,
            },
            if matches!(origin, PickerOrigin::Files) {
                SEARCH_MAXIMUM_FILES
            } else {
                MAXIMUM_PICKER_ITEMS
            },
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(epoch) => epoch,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        // A stream may have refreshed the picker underneath help, not the visible page.
        if self.search.help_is_open() {
            return;
        }
        if self.search.theme_picker_key().is_none() {
            self.configuration.cancel_theme_picker();
        }
        self.extension.picker_installed(match origin {
            PickerOrigin::PluginProvider(request) => Some(request),
            _ => None,
        });
        if !previous_epoch.is_some_and(|epoch| self.search.owns_picker_epoch(&epoch)) {
            self.input.begin_picker();
        }
        // A refused replacement must not destroy the still-valid old preview.
        if let Some(id) = retired_preview {
            self.retire_workspace_picker(id);
        }
        self.language
            .retain_actions(self.search.picker_code_actions(&epoch));
        self.input.cancel_prompt_completions();
        self.clear_picker_preview();
        self.sync_picker_ui();
    }
}

impl App {
    pub(in crate::app) fn retire_stale_picker(&mut self) {
        if self.search.picker_is_stale(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            self.close_picker();
        }
    }
}

impl App {
    pub(in crate::app) fn selected_picker_payload(&self) -> Option<&PickerPayload> {
        self.search.selected_picker_payload()
    }
}

impl App {
    pub(in crate::app) fn open_lsp_locations_picker(
        &mut self,
        locations: Vec<crate::language::lsp::LspLocation>,
        encoding: PositionEncoding,
    ) {
        let excerpts = self.reference_excerpts(&locations);
        let items = locations
            .into_iter()
            .enumerate()
            .map(|(index, location)| {
                let line = location.range.start.line;
                let path = path_from_file_uri(&location.uri);
                let code = path
                    .as_deref()
                    .and_then(|path| excerpts.get(&(path.to_owned(), line)).cloned())
                    .unwrap_or_default();
                let path = path.map_or_else(
                    || location.uri.clone(),
                    |path| self.workspace_relative_label(&path),
                );
                let id = PickerItemId(format!("location:{index}:{}", location.uri));
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: format!("{path}:{}", line.saturating_add(1)),
                        detail: code,
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspLocation(location, encoding),
                )
            })
            .collect();
        self.install_picker(self.message("picker_references_title", &[]), items);
    }
}

impl App {
    /// Read each reference file once and retain only the requested, bounded excerpts.
    pub(in crate::app) fn reference_excerpts(
        &self,
        locations: &[crate::language::lsp::LspLocation],
    ) -> std::collections::HashMap<(std::path::PathBuf, usize), String> {
        const MAXIMUM_FILE: u64 = 1024 * 1024;
        let mut remaining = 1024 * 1024usize;
        let mut requested = std::collections::BTreeMap::<_, std::collections::BTreeSet<_>>::new();
        for location in locations {
            if let Some(path) = path_from_file_uri(&location.uri) {
                requested
                    .entry(path)
                    .or_default()
                    .insert(location.range.start.line);
            }
        }
        let mut excerpts = std::collections::HashMap::new();
        for (path, lines) in requested {
            if remaining < 4096 {
                break;
            }
            let open = self
                .state
                .buffers
                .values()
                .find(|buffer| buffer.path.as_ref() == Some(&path));
            let disk = if open.is_none() {
                crate::files::read_file_with_ceiling(&path, MAXIMUM_FILE)
                    .ok()
                    .and_then(|bytes| crate::editor::text::decode_utf8(&bytes).ok())
            } else {
                None
            };
            // Walk a closed file once, rather than rescanning it for each reference.
            let mut disk_lines = disk
                .as_ref()
                .map(|text| text.text.lines().enumerate().peekable());
            for line in lines {
                if remaining < 4096 {
                    break;
                }
                let excerpt = if let Some(buffer) = open {
                    buffer.text.line(line).ok()
                } else if let Some(source) = &mut disk_lines {
                    while source.peek().is_some_and(|(index, _)| *index < line) {
                        source.next();
                    }
                    source
                        .next()
                        .filter(|(index, _)| *index == line)
                        .map(|(_, text)| text.to_owned())
                } else {
                    None
                };
                if let Some(excerpt) = excerpt {
                    let excerpt: String = excerpt.trim().chars().take(1024).collect();
                    remaining -= excerpt.len();
                    excerpts.insert((path.clone(), line), excerpt);
                }
            }
        }
        excerpts
    }
}

impl App {
    pub(in crate::app) fn open_lsp_symbols_picker(
        &mut self,
        server: &str,
        symbols: Vec<crate::language::lsp::LspSymbol>,
    ) {
        let encoding = self.language.server_encoding(server);
        let server = self.lsp_server_label(server);
        let items = symbols
            .into_iter()
            .enumerate()
            .map(|(index, symbol)| {
                let id = PickerItemId(format!("symbol:{index}:{}", symbol.name));
                let payload = symbol.location.map_or_else(
                    || PickerPayload::Command(String::new()),
                    |location| PickerPayload::LspLocation(location, encoding),
                );
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: symbol.name,
                        detail: symbol.kind.map_or_else(String::new, |kind| {
                            self.message(
                                "picker_symbol_kind",
                                &[(
                                    "kind",
                                    crate::language::lsp::lsp_symbol_kind_name(kind).to_owned(),
                                )],
                            )
                        }),
                        source: server.clone(),
                        action: PickerAction::None,
                    },
                    payload,
                )
            })
            .collect();
        self.install_picker(self.message("picker_symbols_title", &[]), items);
    }
}

impl App {
    pub(in crate::app) fn open_lsp_code_actions_picker(&mut self, server: &str) {
        let choices = match self.language.install_offered_actions(MAXIMUM_PICKER_ITEMS) {
            Ok(choices) => choices,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if choices.is_empty() {
            self.show_lsp_no_result(server);
            return;
        }
        let items = choices
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("code-action:{index}:{}", choice.title)),
                        label: choice.title,
                        detail: choice.detail,
                        source: self.lsp_server_label(&choice.server),
                        action: PickerAction::None,
                    },
                    PickerPayload::CodeAction(choice.key),
                )
            })
            .collect();
        self.install_picker(self.message("picker_code_actions_title", &[]), items);
    }
}

impl App {
    pub(in crate::app) fn open_builtin_picker(&mut self, kind: PickerKind) {
        match kind {
            PickerKind::Diagnostics => {
                let mut items: Vec<_> = self
                    .diagnostics
                    .choices()
                    .enumerate()
                    .map(|(index, (key, problem, _))| {
                        let id = PickerItemId(format!("diagnostic:{index}:{}", problem.uri));
                        let path = path_from_file_uri(&problem.uri);
                        let line = path.as_ref().and_then(|path| {
                            let open = self
                                .state
                                .buffers
                                .iter()
                                .find(|(_, buffer)| buffer.path.as_ref() == Some(path));
                            open?.1.text.byte_to_line(problem.range.start).ok()
                        });
                        let place = path.map_or_else(
                            || problem.uri.clone(),
                            |path| self.workspace_relative_label(&path),
                        );
                        let place =
                            line.map_or(place.clone(), |line| format!("{place}:{}", line + 1));
                        // Where comes first: at any width the row says which file to look at.
                        let message = problem.message.lines().map(str::trim).collect::<Vec<_>>();
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id,
                                label: format!("{place}  {}", message.join(" ")),
                                detail: String::new(),
                                source: problem.source.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Diagnostic(key),
                        )
                    })
                    .collect();
                items.extend(self.tasks.failed_tests().into_iter().map(
                    |(path, line, summary, key)| {
                        let place = self.workspace_relative_label(&path);
                        let summary = summary.lines().map(str::trim).collect::<Vec<_>>();
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("test:{}:{}", key.name, key.index)),
                                label: format!("{place}:{}  {}", line + 1, summary.join(" ")),
                                detail: String::new(),
                                source: key.name.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::TaskTest(key),
                        )
                    },
                ));
                self.install_surface_picker(
                    PickerSurfaceKind::Problems,
                    self.message("picker_diagnostics_title", &[]),
                    items,
                );
            }
            PickerKind::Buffers => {
                let items = self
                    .state
                    .buffers
                    .iter()
                    .enumerate()
                    .map(|(index, (buffer, state))| {
                        let id = PickerItemId(format!("buffer:{index}"));
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id,
                                label: state.path.as_ref().map_or_else(
                                    || SCRATCH_LABEL.to_owned(),
                                    |path| self.workspace_relative_label(path),
                                ),
                                detail: if state.dirty {
                                    STATUS_DIRTY_MARKER.to_owned()
                                } else {
                                    String::new()
                                },
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Buffer(buffer),
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_buffers_title", &[]), items);
            }
            PickerKind::Roots => {
                // Every workspace Dust has opened on this machine; choosing one moves the
                // editor there like a worktree switch.
                let current = self.state.workspace_root.clone();
                let mut projects = self
                    .workspace
                    .seen_roots()
                    .into_iter()
                    .chain(
                        self.workspace
                            .roots()
                            .iter()
                            .map(|root| root.identity.canonical_path.clone()),
                    )
                    .filter(|path| path.is_dir())
                    .collect::<Vec<_>>();
                projects.sort();
                projects.dedup();
                let open_roots = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect::<Vec<_>>();
                let home = std::env::var_os("HOME").map(PathBuf::from);
                let items = projects
                    .into_iter()
                    .map(|path| {
                        let name = path.file_name().map_or_else(
                            || path.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                        let shown = match home
                            .as_deref()
                            .and_then(|home| path.strip_prefix(home).ok())
                        {
                            Some(rest) => format!("~/{}", rest.display()),
                            None => path.display().to_string(),
                        };
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("project:{}", path.display())),
                                label: name,
                                detail: shown,
                                source: if path == current {
                                    "open here".into()
                                } else if open_roots.contains(&path) {
                                    "workspace root".into()
                                } else {
                                    String::new()
                                },
                                action: PickerAction::None,
                            },
                            PickerPayload::Project(path),
                        )
                    })
                    .collect();
                self.install_picker("Projects".to_owned(), items);
                self.search
                    .set_picker_footer("Enter Switch · Esc Esc Close".to_owned());
                self.sync_picker_ui();
            }
            PickerKind::Recents => {
                let items = self
                    .persistence
                    .recent_files()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, path)| {
                        let location = PersistedLocation {
                            path: path.clone(),
                            byte: 0,
                            anchor: 0,
                            scroll_line: 0,
                            scroll_column: 0,
                        };
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("recent:{index}")),
                                label: self.workspace_relative_label(&path),
                                detail: String::new(),
                                source: self.message("navigation_recent_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_recents_title", &[]), items);
            }
            PickerKind::Marks => {
                self.persistence.reconcile_marks(&self.state);
                let items = self
                    .persistence
                    .marks()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, mark)| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("mark:{index}:{}", mark.name)),
                                label: mark.name,
                                detail: self.persisted_location_label(&mark.location),
                                source: self.message("navigation_mark_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(mark.location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_marks_title", &[]), items);
            }
            PickerKind::Jumps | PickerKind::Changes => {
                let (source, title, rows) = if matches!(kind, PickerKind::Jumps) {
                    (
                        self.message("navigation_jump_source", &[]),
                        self.message("navigation_jumps_title", &[]),
                        self.persistence.jumps().to_vec(),
                    )
                } else {
                    (
                        self.message("navigation_change_source", &[]),
                        self.message("navigation_changes_title", &[]),
                        self.persistence.changes().to_vec(),
                    )
                };
                let items = rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, location)| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("navigation:{index}")),
                                label: self.persisted_location_label(&location),
                                detail: String::new(),
                                source: source.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(title, items);
            }
            PickerKind::Commands => self.open_command_palette(),
        }
    }
}

impl App {
    /// A saved location as `file:line`; its byte offset means nothing to a person. A file that
    /// isn't open is read only for a nearby offset, because this runs for every row.
    pub(in crate::app) fn persisted_location_label(&self, location: &PersistedLocation) -> String {
        const READ_CEILING: usize = 1024 * 1024;
        let name = self.workspace_relative_label(&location.path);
        let open = self
            .state
            .buffers
            .values()
            .find(|buffer| buffer.path.as_ref() == Some(&location.path));
        let line = match open {
            Some(buffer) => buffer.text.byte_to_line(ByteOffset(location.byte)).ok(),
            None if location.byte <= READ_CEILING => {
                use std::io::Read;
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
                    .open(&location.path)
                    .ok()
                    .and_then(|file| {
                        if !file.metadata().ok()?.is_file() {
                            return None;
                        }
                        let mut bytes = Vec::new();
                        // CRLF takes two disk bytes for one normalized byte; allow a BOM
                        // and a partial final UTF-8 character as well.
                        file.take((location.byte * 2 + 7) as u64)
                            .read_to_end(&mut bytes)
                            .ok()?;
                        if let Err(error) = std::str::from_utf8(&bytes) {
                            if error.error_len().is_some() {
                                return None;
                            }
                            bytes.truncate(error.valid_up_to());
                        }
                        TextBuffer::from_bytes(&bytes)
                            .ok()?
                            .byte_to_line(ByteOffset(location.byte))
                            .ok()
                    })
            }
            None => None,
        };
        match line {
            Some(line) => format!("{name}:{}", line + 1),
            None => name,
        }
    }
}

impl App {
    pub(in crate::app) fn sync_picker_ui(&mut self) {
        self.retire_stale_picker();
        let explorer = &self.configuration.settings().ui.explorer;
        let icon = (explorer.icon_mode == crate::config::ExplorerIconMode::NerdFont)
            .then(|| explorer.active_glyphs().file.clone());
        self.search.set_file_icon(icon);
        self.ui.picker = self.search.picker_projection();
        let theme = match self.search.selected_picker_payload() {
            Some(PickerPayload::Theme(key)) => Some(key),
            _ => None,
        };
        self.configuration.preview_theme(theme);
        let context = self.active_input_context();
        let theme_picker = self.search.theme_picker_key().is_some();
        if let Some(picker) = &mut self.ui.picker {
            let (mode, commands): (&str, &[(&str, &str)]) =
                if self.search.workspace_picker().is_some() {
                    (
                        "Review",
                        &[
                            ("workspace.edit.toggle", "toggle"),
                            ("workspace.edit.accept", "apply"),
                            ("workspace.edit.reject", "reject"),
                        ],
                    )
                } else if theme_picker {
                    ("", &[("picker.cancel", "cancel")])
                } else if self.input.picker_normal() {
                    (
                        "Normal",
                        &[
                            ("picker.mode.insert", "edit"),
                            ("picker.accept", "accept"),
                            ("picker.cancel", "close"),
                        ],
                    )
                } else {
                    (
                        "Insert",
                        &[
                            ("picker.mode.normal", "normal"),
                            ("picker.accept", "accept"),
                            ("picker.cancel", "close"),
                        ],
                    )
                };
            let hints = commands
                .iter()
                .filter_map(|(command, action)| {
                    self.configuration
                        .keymaps()
                        .inspect_key(&context, "escape")
                        .filter(|binding| {
                            *command == "picker.cancel"
                                && binding.command.as_deref() == Some(*command)
                        })
                        .or_else(|| self.configuration.keymaps().inspect(&context, command))
                        .map(|binding| {
                            if theme_picker {
                                let key = if binding.keys == "escape" {
                                    "Escape"
                                } else {
                                    &binding.keys
                                };
                                format!("Press {key} to {action}")
                            } else {
                                format!("{} {action}", binding.keys)
                            }
                        })
                })
                .collect::<Vec<_>>()
                .join(" · ");
            picker.input_hint = if mode.is_empty() {
                hints
            } else {
                format!("{mode} · {hints}")
            };
        }
        if !self.search.help_is_open() {
            self.sync_picker_preview();
        }
        if let Some(picker) = &mut self.ui.picker {
            if self.search.help_is_open() {
                picker.preview = None;
            } else {
                picker.preview.clone_from(self.search.preview());
            }
        }
        self.refresh_window_content();
    }
}

impl App {
    pub(in crate::app) fn picker_insert(&mut self, text: &str) {
        self.retire_stale_picker();
        let query = self.search.picker_insert(text);
        self.sync_picker_ui();
        if self.search.live()
            && let Some(query) = query
        {
            self.restart_live_workspace_search(query);
        }
    }
}

impl App {
    pub(in crate::app) fn picker_backspace(&mut self) {
        self.retire_stale_picker();
        let query = self.search.picker_backspace();
        self.sync_picker_ui();
        if self.search.live()
            && let Some(query) = query
        {
            self.restart_live_workspace_search(query);
        }
    }
}

impl App {
    pub(in crate::app) fn picker_move(&mut self, direction: i8) {
        self.retire_stale_picker();
        self.search.picker_move(direction, true);
        self.sync_picker_ui();
    }
}

impl App {
    /// Read the focused change's diff on the preview's own hunk worker.
    fn sync_changes_diff_preview(&mut self) {
        let Some(crate::app::picker::PickerPayload::MatcherSource(source)) =
            self.selected_picker_payload().cloned()
        else {
            self.clear_picker_preview();
            return;
        };
        let Some(status) = self.git.status().cloned() else {
            self.clear_picker_preview();
            return;
        };
        let layer = self.search.changes_layer();
        // A hunk policy takes a path RELATIVE to the scope root: it rejects an absolute one,
        // and the status row carries an absolute path.
        let Ok(relative) = source.path.strip_prefix(&status.scope_root) else {
            self.clear_picker_preview();
            return;
        };
        let policy = crate::app::git::GitSession::policy(&status, relative.to_owned(), layer);
        if let Some(id) = self.search.prepare_diff_preview(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
            relative.display().to_string(),
        ) {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    fn clear_picker_preview(&mut self) {
        if let Some(id) = self.search.clear_preview() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    /// The change surface's focused row as a path RELATIVE to the scope root, when that
    /// surface is open.
    ///
    /// `None` means the caller should act on the active buffer, which is what every other
    /// surface wants. The change surface labels `s` and `u` by focus, so it stages the row
    /// under the cursor rather than whatever buffer happens to be active.
    pub(in crate::app) fn changes_focused_relative(&self) -> Option<std::path::PathBuf> {
        if self.search.picker_surface() != PickerSurfaceKind::Changes {
            return None;
        }
        let crate::app::picker::PickerPayload::MatcherSource(source) =
            self.selected_picker_payload()?
        else {
            return None;
        };
        let status = self.git.status()?;
        source
            .path
            .strip_prefix(&status.scope_root)
            .ok()
            .map(std::path::Path::to_owned)
    }
}

impl App {
    fn sync_picker_preview(&mut self) {
        if self.search.picker_surface() == PickerSurfaceKind::Changes {
            self.sync_changes_diff_preview();
            return;
        }
        if let Some(PickerPayload::ProposalFile(index)) = self.selected_picker_payload() {
            let index = *index;
            let preview = self.workspace_edits.proposal().and_then(|proposal| {
                let file = proposal.files.get(index)?;
                Some(crate::ui::PickerPreview {
                    title: file
                        .canonical_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    body: crate::ui::PickerPreviewBody::Diff(crate::app::git::text_diff_preview(
                        &file.original_bytes,
                        &file.proposed_bytes,
                        "current -> proposed",
                    )),
                })
            });
            match preview {
                Some(preview) => {
                    if let Some(id) = self.search.set_preview(preview) {
                        self.documents.cancel_request(id);
                    }
                }
                None => self.clear_picker_preview(),
            }
            return;
        }
        let Some((label, payload)) = self
            .search
            .picker()
            .and_then(PickerState::selected_item)
            .and_then(|item| {
                self.selected_picker_payload()
                    .cloned()
                    .map(|payload| (item.label.clone(), payload))
            })
        else {
            self.clear_picker_preview();
            return;
        };
        let payload = if let PickerPayload::TaskTest(key) = payload {
            let Some(source) = self.tasks.resolve_test(&key) else {
                self.clear_picker_preview();
                return;
            };
            PickerPayload::MatcherSource(source)
        } else {
            payload
        };
        match payload {
            PickerPayload::Recovery(journal) => {
                let preview = self
                    .recovery
                    .records()
                    .iter()
                    .find(|(path, _)| *path == journal)
                    .map(|(_, record)| crate::ui::PickerPreview {
                        title: label,
                        body: crate::ui::PickerPreviewBody::Text(
                            crate::ui::PickerTextPreview::new(
                                record
                                    .content
                                    .lines()
                                    .take(RECOVERY_PREVIEW_LINES)
                                    .map(str::to_owned)
                                    .collect(),
                                0,
                                None,
                                record
                                    .path()
                                    .as_deref()
                                    .and_then(crate::language::syntax::language_for_path),
                            ),
                        ),
                    });
                match preview {
                    Some(preview) => {
                        if let Some(id) = self.search.set_preview(preview) {
                            self.documents.cancel_request(id);
                        }
                    }
                    None => self.clear_picker_preview(),
                }
            }
            PickerPayload::WorkspaceSearch(key) => {
                if let Some(id) = self
                    .search
                    .preview_match(&key, PICKER_MAXIMUM_PREVIEW_BYTES)
                {
                    self.documents.cancel_request(id);
                }
            }
            PickerPayload::MatcherSource(source) => {
                let (retired, ticket) = self.search.prepare_preview(
                    source.path.clone(),
                    Some((source.line, source.column)),
                    label,
                    "loading…".into(),
                    self.workspace.version(),
                    self.configuration.binding_revision(),
                );
                if let Some(id) = retired {
                    self.documents.cancel_request(id);
                }
                let Some(ticket) = ticket else {
                    return;
                };
                let roots = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect();
                let maximum = PICKER_MAXIMUM_PREVIEW_BYTES;
                match self
                    .documents
                    .read_scoped_bounded(source.path, roots, maximum as u64)
                {
                    Ok(id) => {
                        if !self.search.admit_preview(ticket, id, maximum) {
                            self.documents.cancel_request(id);
                        }
                    }
                    Err(error) => self.search.preview_failed(ticket, error),
                }
            }
            _ => self.clear_picker_preview(),
        }
    }
}

impl App {
    pub(in crate::app) fn close_picker(&mut self) {
        if self.search.help_is_open() {
            self.search.close_picker();
            if self.search.picker_projection().is_some() {
                self.sync_picker_ui();
                self.refresh_window_content();
                return;
            }
        }
        self.configuration.cancel_theme_picker();
        self.debugger_session.dismiss_choices();
        if self.ui.picker.is_some() {
            self.input.capture_changed();
        }
        if let Some(id) = self.search.workspace_picker() {
            self.retire_workspace_picker(id);
        }
        self.language.clear_actions();
        if let Some(id) = self.search.close_picker() {
            self.documents.cancel_request(id);
        }
        self.extension.picker_installed(None);
        self.tasks.select_output(None);
        self.ui.picker = None;
        self.refresh_window_content();
        // Restore graph focus before the accepted row captures a navigation target.
        self.reconcile_editor_windows();
    }
}

impl App {
    pub(in crate::app) fn accept_picker(&mut self) {
        // A help page documents; it does not act. Accepting inside help closes the page and
        // returns the surface it was opened over, exactly like Escape, instead of running the
        // row's command and resurrecting a surface underneath it.
        if self.search.help_is_open() {
            self.close_picker();
            return;
        }
        self.retire_stale_picker();
        if let Some(name) = self.search.new_worktree_name() {
            self.close_picker();
            self.create_git_worktree(name, None);
            return;
        }
        if let Some(PickerPayload::Theme(key)) = self.search.selected_picker_payload() {
            match self.configuration.apply_theme(key) {
                Ok(name) => {
                    self.close_picker();
                    self.finish_theme_choice(&name);
                }
                Err(error) => self.show_error(error),
            }
            return;
        }
        let payload = self.search.take_picker_payload();
        if let Some(PickerPayload::ProposalFile(index)) = &payload {
            if let Err(error) = self.workspace_edits.toggle(*index) {
                self.show_error(error);
            }
            self.show_workspace_proposal();
            return;
        }
        // Retain only the accepted source match before close retires live search data.
        let search_match = match &payload {
            Some(PickerPayload::WorkspaceSearch(id)) => self
                .search
                .matches()
                .iter()
                .find(|found| &found.id == id)
                .cloned(),
            _ => None,
        };
        let code_action = match &payload {
            Some(PickerPayload::CodeAction(key)) => {
                let documents = self
                    .state
                    .buffers
                    .iter()
                    .filter_map(|(id, buffer)| {
                        Some(crate::app::language::edits::EditDocument {
                            id,
                            path: buffer.path.as_deref()?,
                            text: &buffer.text,
                            dirty: buffer.dirty,
                        })
                    })
                    .collect::<Vec<_>>();
                Some(self.language.take_action(key, &documents))
            }
            _ => None,
        };
        let debug_selection = match &payload {
            Some(PickerPayload::Debug(key)) => Some(self.debugger_session.take_selection(key)),
            _ => None,
        };
        let provider_selection = match &payload {
            Some(PickerPayload::PluginProviderItem {
                plugin,
                load,
                request,
                item,
                action_command,
            }) => self.extension.provider_selection(
                plugin,
                *load,
                *request,
                item,
                action_command,
                &self.state,
            ),
            _ => None,
        };
        self.close_picker();
        match payload {
            Some(PickerPayload::Theme(_)) => {}
            Some(PickerPayload::WorkspaceSearch(_)) => {
                if let Some(found) = search_match {
                    self.navigate_workspace_search_match(found);
                }
            }
            Some(PickerPayload::LspLocation(location, encoding)) => {
                self.navigate_lsp_location_encoded(&location, encoding)
            }
            Some(PickerPayload::Diagnostic(key)) => {
                if let Some(row) = self.diagnostics.resolve(&key, &self.state) {
                    let (uri, range) = (row.uri.clone(), row.range);
                    self.navigate_byte_location(&uri, range);
                } else {
                    self.show_error("diagnostic source retired");
                }
            }
            Some(PickerPayload::CodeAction(_)) => {
                let offered = match code_action.expect("selected code action") {
                    Ok(offered) => offered,
                    Err(error) => {
                        self.show_error(error);
                        return;
                    }
                };
                let action = offered.action;
                if let Some(edit) = action.edit {
                    if self.workspace_edits.check_begin().is_err() {
                        self.show_error("another workspace edit is pending");
                        return;
                    }
                    self.begin_workspace_proposal_owned(
                        action.title,
                        edit,
                        None,
                        self.language.server_encoding(&offered.server),
                        offered.context,
                    );
                    if let Some(command) = action.command {
                        self.language.set_edit_command(offered.server, command);
                    }
                } else if let Some(command) = action.command {
                    self.select_lsp_session(&offered.server);
                    self.request_lsp(LspFeature::ExecuteCommand(command));
                }
            }
            Some(PickerPayload::LspCodeLens {
                server,
                context,
                lens,
            }) => {
                if !self.language.edit_context_current(&context) {
                    self.show_error("code-lens source retired");
                    return;
                }
                if let Some(command) = lens.command {
                    self.execute_lsp_command(&server, command);
                } else if let Some(item) = lens.resolve_item {
                    self.refresh_lsp_projection_owner();
                    if let Some((_, document)) = self.active_lsp_document()
                        && let Err(error) = self.language.request_projected(
                            &server,
                            LspFeature::CodeLensResolve { item },
                            document,
                            ByteOffset(0),
                        )
                    {
                        self.show_error(error);
                    }
                }
            }
            Some(PickerPayload::Buffer(buffer)) => {
                if let Err(error) = self.state.show_buffer(buffer) {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::GitWorktree { path, .. } | PickerPayload::Project(path)) => {
                self.switch_workspace_root(path)
            }
            Some(PickerPayload::Agent(choice)) => self.accept_agent_choice(choice),
            Some(PickerPayload::PullRequest {
                root,
                generation,
                number,
                local,
            }) => self.checkout_pull_request(root, generation, number, local),
            Some(PickerPayload::Terminal(key)) => {
                if let Some(id) = self.terminal_session.resolve(&key) {
                    self.terminal_session.select(id);
                    self.reveal_terminal(id);
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            Some(PickerPayload::Window(window)) => {
                self.reconcile_editor_windows();
                let result = self
                    .window_session
                    .plan_focus(
                        window,
                        &self.state.window_version(),
                        self.extension.contributions().version(),
                    )
                    .and_then(|plan| self.apply_window_plan(plan));
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::LspSession(key)) => self.select_lsp_session(&key),
            Some(PickerPayload::Debug(_)) => {
                self.debugger_session.refresh(
                    &self.configuration.settings().debug,
                    self.workspace.version(),
                );
                let target = crate::app::debugger::SourceTarget {
                    line: 1,
                    column: 1,
                    navigation: self
                        .window_session
                        .navigation_stamp(self.state.window_version(), self.state.active_view),
                    capture: self.input.capture_epoch(),
                };
                let debug_selection = debug_selection.expect("debug selection routed");
                // A value with nothing inside it opens to the whole of itself, unclipped.
                if let Some(value) = debug_selection.as_ref().ok().and_then(|s| s.value()) {
                    let value = value.to_owned();
                    self.debugger_session.record_inspected_value(value.clone());
                    self.notify_message(value, false);
                }
                match debug_selection
                    .and_then(|selection| self.debugger_session.activate(selection, target))
                {
                    Ok(Some(location)) => self.navigate_debug_location(location),
                    Ok(None) => {}
                    Err(error) => self.show_error(error),
                }
            }
            Some(PickerPayload::GitHunk {
                path,
                status_revision,
                repository,
                hunk,
                new_line,
            }) => {
                if self.git.select(
                    status_revision,
                    repository,
                    self.workspace.root_revision(),
                    &path,
                    hunk,
                ) {
                    self.navigate_matcher_source(MatcherSource {
                        path,
                        line: new_line,
                        column: 0,
                        encoding: PositionEncoding::Utf8,
                    })
                } else {
                    self.show_error("Git hunk is stale")
                }
            }
            Some(PickerPayload::TaskTest(key)) => {
                if let Some(source) = self.tasks.resolve_test(&key) {
                    self.navigate_matcher_source(source);
                } else {
                    self.show_error("test result source retired");
                }
            }
            Some(PickerPayload::MatcherSource(source)) => self.navigate_matcher_source(source),
            Some(PickerPayload::PersistedLocation(location)) => {
                self.navigate_persisted_location(location)
            }
            Some(PickerPayload::RegistrySummary(summary)) => self.request_registry(
                RegistryRequest::Show {
                    publisher: summary.publisher,
                    name: summary.name,
                    version: summary.version,
                },
                false,
            ),
            Some(PickerPayload::RegistryMetadata(metadata)) => {
                self.request_registry(RegistryRequest::Fetch { metadata }, true)
            }
            Some(PickerPayload::Command(command)) if !command.is_empty() => {
                self.run_palette_command(&command);
            }
            Some(PickerPayload::Recovery(journal)) => self.recover_journal(&journal),
            Some(PickerPayload::Tutor(start)) => self.start_tutor(start),
            Some(PickerPayload::Notice {
                title,
                text: message,
            }) => {
                self.present_documentation_popup(
                    DocumentationPopupKind::Notice,
                    title,
                    "application".into(),
                    None,
                    vec![LspMarkupContent {
                        kind: crate::language::lsp::LspMarkupKind::PlainText,
                        value: message,
                    }],
                );
            }
            Some(PickerPayload::PluginProviderItem { plugin, .. }) => {
                if let Some((command, arguments)) = provider_selection {
                    self.execute_plugin_command(plugin, command, arguments);
                }
            }
            _ => {}
        }
    }
}
