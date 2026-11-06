use crate::ui::render::SCRATCH_LABEL;
use crate::app::configuration::configured_text_width;
use crate::app::AdapterContext;
use crate::app::AppEffect;
use crate::app::BTreeMap;
use crate::app::ByteOffset;
use crate::app::ByteRange;
use crate::app::DiagnosticOwner;
use crate::app::DockEdge;
use crate::app::DocumentationPopupKind;
use crate::app::EditorCloseSave;
use crate::app::EditorMode;
use crate::app::EditorOperation;
use crate::app::EditorTransaction;
use crate::app::ExecutionPreflightEvent;
use crate::app::ExecutionSource;
use crate::app::HashSet;
use crate::app::Instant;
use crate::app::App;
use crate::app::LayoutAction;
use crate::app::LogLevel;
use crate::app::LspAutostartObservation;
use crate::app::LspDocument;
use crate::app::LspFeature;
use crate::app::LspLaunchRequest;
use crate::app::LspLaunchSource;
use crate::app::LspMarkupContent;
use crate::app::LspMarkupKind;
use crate::app::LspRuntimeSource;
use crate::app::PackageOperation;
use crate::app::Path;
use crate::app::PathBuf;
use crate::app::PendingLspProjectRoot;
use crate::app::PickerAction;
use crate::app::PickerItem;
use crate::app::PickerItemId;
use crate::app::PickerPayload;
use crate::app::PluginUiInteraction;
use crate::app::PositionCodec;
use crate::app::Precondition;
use crate::app::PromptKind;
use crate::app::PromptMove;
use crate::app::RegistryRequest;
use crate::app::RegistryResult;
use crate::app::TextBuffer;
use crate::app::TextEdit;
use crate::app::ToolDefinition;
use crate::app::ToolInstall;
use crate::app::ToolKind;
use crate::app::ToolMenuRow;
use crate::app::ToolMenuState;
use crate::app::ToolOperation;
use crate::app::ToolRuntimeOption;
use crate::app::ToolRuntimePickerState;
use crate::app::ToolStatus;
use crate::app::TrustedAction;
use crate::app::UndoPolicy;
use crate::app::WindowPlacement;
use crate::app::WindowRect;
use crate::app::WorkspaceTrust;
use crate::app::discover_executable;
use crate::app::is_nixos_host;
use crate::app::language_id_for_path;
use crate::app::lsp_runtime_source_allowed_on_host;
use crate::app::managed_formatter;
use crate::app::managed_tool_executable;
use crate::app::nix_lsp_launch_environment;
use crate::app::nix_lsp_profile_installed;
use crate::app::nix_workspace_shell_for_host;
use crate::app::tool_kind_key;
use crate::app::workspace::leaves_root;
use crate::ui::render::tools::TOOL_CATEGORY_ORDER;
use crate::ui::render::tools::TOOL_MANAGER_MINIMUM_HEIGHT;
use crate::ui::CompletionItem;
use crate::ui::LanguageCompletionState;
use crate::app::BufferRevision;
use crate::app::Duration;
use crate::app::TextEditSet;
use crate::app::command;
use crate::windows::WindowPlacement;
use crate::app::ViewId;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;



impl App {


    pub(in crate::app) fn pending_preflight_count(&self) -> usize {
        self.tasks.preflight_count()
            + self.terminal_session.preflight_count()
            + self.language.preflight_count()
    }
}

impl App {

    pub(in crate::app) fn process_execution_preflight(&mut self) {
        let events = self.workspace.preflight_events();
        for event in events {
            match event {
                ExecutionPreflightEvent::Ready(ready) => {
                    if self
                        .debugger_session
                        .owns_debug_preflight(ready.result().id)
                    {
                        let id = ready.result().id;
                        if let Err(error) = self.debugger_session.debug_preflight(ready) {
                            self.debugger_session.fail_debug_preflight(id);
                            self.show_error(error);
                        }
                        self.advance_debug_launch();
                        continue;
                    }
                    if self.language.owns_preflight(ready.result().id) {
                        match self.language.finish_preflight(
                            ready,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                            &self.configuration.settings().lsp,
                        ) {
                            Ok(Some(key)) => {
                                for prefix in ["lsp.", "lsp-pull:"] {
                                    if let Some(producer) =
                                        self.language.diagnostic_producer(&key, prefix)
                                    {
                                        self.diagnostics.activate(producer);
                                    }
                                }
                                self.sync_ui_diagnostics();
                            }
                            Ok(None) => {}
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    let result = ready.result();
                    let id = result.id;
                    if result.root_revision != self.workspace.root_revision()
                        || self
                            .workspace
                            .authorize_execution(ExecutionSource::ProjectLocal)
                            .is_err()
                    {
                        if let Some(step) = self.tasks.take_preflight(id) {
                            self.tasks.reject_step(&step);
                        }
                        if let Some(key) = self.terminal_session.take_preflight(id) {
                            if let Some(id) = self.terminal_session.resolve(&key)
                                && let Some(reply) = self.debugger_session.take_terminal(&id)
                            {
                                let _ =
                                    reply.respond(false, Some("terminal workspace retired".into()));
                            }
                            self.terminal_session.reject(&key);
                            self.sync_terminal_panel();
                        }
                        continue;
                    }
                    if let Some(step) = self.tasks.take_preflight(id) {
                        match self.tasks.launch(
                            step,
                            ready,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                        ) {
                            Ok(source) => {
                                if let Some(source) = source {
                                    self.diagnostics.activate(source);
                                    self.sync_ui_diagnostics();
                                }
                            }
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        let id = self.terminal_session.resolve(&key);
                        if id.is_some_and(|id| !self.debugger_session.terminal_current(&id)) {
                            self.terminal_session.reject(&key);
                            if let Some(id) = id {
                                self.debugger_session.take_terminal(&id);
                            }
                            continue;
                        }
                        if let Err(error) =
                            self.terminal_session
                                .launch(key.clone(), ready, &self.workspace)
                        {
                            self.terminal_session.reject(&key);
                            if let Some(reply) =
                                id.and_then(|id| self.debugger_session.take_terminal(&id))
                            {
                                let _ = reply.respond(false, Some(error.clone()));
                            }
                            self.show_error(error);
                        }
                        self.sync_terminal_panel();
                        continue;
                    }
                }
                ExecutionPreflightEvent::Failed {
                    id,
                    root_revision,
                    message,
                } => {
                    if self.debugger_session.owns_debug_preflight(id) {
                        self.debugger_session.fail_debug_preflight(id);
                        self.show_error(message);
                        continue;
                    }
                    if self.language.fail_preflight(id) {
                        self.show_error(message);
                        continue;
                    }
                    let mut owned = false;
                    if let Some(step) = self.tasks.take_preflight(id) {
                        self.tasks.reject_step(&step);
                        owned = true;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        if let Some(id) = self.terminal_session.resolve(&key)
                            && let Some(reply) = self.debugger_session.take_terminal(&id)
                        {
                            let _ = reply.respond(false, Some(message.clone()));
                        }
                        self.terminal_session.reject(&key);
                        self.sync_terminal_panel();
                        owned = true;
                    }
                    if owned && root_revision == self.workspace.root_revision() {
                        self.show_error(message)
                    }
                }
            }
        }
    }
}

impl App {

    pub(in crate::app) fn add_workspace_root(&mut self, path: PathBuf) {
        let requested_path = if path.is_absolute() {
            path
        } else {
            self.state.workspace_root.join(path)
        };
        match self.workspace.add_root_request(requested_path, None) {
            Ok(()) => self.notify_message(self.message("workspace_root_loading", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {


    pub(in crate::app) fn switch_workspace_root(&mut self, path: PathBuf) {
        let current = self.workspace.active_root().and_then(|id| {
            self.workspace
                .roots()
                .iter()
                .find(|root| root.id == id)
                .map(|root| (id, root.identity.canonical_path.clone()))
        });
        let Some((id, current_path)) = current else {
            self.add_workspace_root(path);
            return;
        };
        let target = path.canonicalize().unwrap_or_else(|_| path.clone());
        if target == current_path {
            self.close_picker();
            self.notify_message(format!("already in {}", target.display()), false);
            return;
        }
        // A folder that is already a workspace root is made active, not opened in place of this one.
        if let Some(existing) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.identity.canonical_path == target)
            .map(|root| root.id)
        {
            self.close_picker();
            self.activate_workspace_root(existing);
            return;
        }
        let unsaved = self
            .state
            .buffers
            .values()
            .filter(|buffer| {
                buffer.dirty
                    && buffer
                        .path
                        .as_ref()
                        .is_some_and(|file| leaves_root(file, &current_path, &target))
            })
            .count();
        if unsaved > 0 {
            self.show_error(format!(
                "{unsaved} file(s) in {} have unsaved changes; save or close them first",
                current_path.file_name().map_or_else(
                    || current_path.display().to_string(),
                    |name| name.to_string_lossy().into_owned()
                )
            ));
            return;
        }
        // Terminals belong to the folder they were opened in, so switching ends them, as
        // opening another folder does in VS Code; it takes asking twice when any are open.
        let terminals = self.terminal_session.running_in_root(&current_path);
        if terminals > 0 && self.switch_ends_terminals.as_ref() != Some(&target) {
            self.switch_ends_terminals = Some(target);
            self.show_error(format!(
                "switching ends {terminals} open terminal(s) and what runs in them; switch again \
                 to go ahead"
            ));
            return;
        }
        self.switch_ends_terminals = None;
        self.close_picker();
        match self.workspace.add_root_request(target, Some(id)) {
            Ok(()) => {
                self.switch_approved_terminals = self.terminal_session.ids_in_root(&current_path);
                self.notify_message("switching…".to_owned(), false);
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }

    /// After a worktree switch: close the old root's clean buffers, move the editor's root and
    /// reopen the file that was in front at the same place in the new worktree.
    pub(in crate::app) fn finish_root_switch(&mut self, path: PathBuf, old: &Path) {
        self.terminal_session.close_root(old);
        let leaving = |file: &Path| leaves_root(file, old, &path);
        let reopen = self.current_persisted_location().and_then(|mut location| {
            let relative = location.path.strip_prefix(old).ok()?;
            if !leaving(&location.path) {
                return None;
            }
            location.path = path.join(relative);
            location.path.is_file().then_some(location)
        });
        // Buffers in no window go first, or closing a window's buffer would put one of them on
        // screen, and a later save would land in the old checkout.
        let hidden = self
            .state
            .buffers
            .iter()
            .filter(|(id, buffer)| {
                !buffer.dirty
                    && buffer.path.as_deref().is_some_and(leaving)
                    && !self.state.views.values().any(|view| view.buffer == *id)
            })
            .map(|(id, buffer)| (id, buffer.path.clone()))
            .collect::<Vec<_>>();
        for (buffer, closed_path) in hidden {
            let mut candidate = self.state.draft();
            candidate.remove_hidden_document(buffer);
            match self.commit_editor_draft(candidate, None, false) {
                Ok(()) => self.retire_closed_buffer(buffer, closed_path.as_deref(), true),
                Err(error) => self.show_error(error),
            }
        }
        // Finish one buffer before choosing the next: otherwise fallback buffers can alternate
        // forever when several panes show the same leaving buffer.
        let leaving_buffers = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| !buffer.dirty && buffer.path.as_deref().is_some_and(leaving))
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for buffer in leaving_buffers {
            let views = self
                .state
                .views
                .iter()
                .filter(|(_, state)| state.buffer == buffer)
                .map(|(view, _)| view)
                .collect::<Vec<_>>();
            for view in views {
                if let Err(error) = self.state.focus_view(view) {
                    self.show_error(error);
                    break;
                }
                self.close_buffer_target(view, buffer);
            }
        }
        // Windows left with nothing to show go too, down to the one the file reopens in.
        let front = self.state.active_view;
        let empty = self
            .state
            .views
            .iter()
            .filter(|(view, state)| {
                Some(*view) != front
                    && self.state.buffers.get(state.buffer).is_some_and(|buffer| {
                        buffer.path.is_none() && !buffer.dirty && buffer.text.len_bytes() == 0
                    })
                    && Some(state.buffer) != self.state.kept_buffer
            })
            .map(|(view, state)| (view, state.buffer))
            .collect::<Vec<_>>();
        for (view, buffer) in empty {
            if self.state.close_view(view).is_ok() {
                let mut candidate = self.state.draft();
                candidate.remove_hidden_document(buffer);
                if let Err(error) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(error);
                }
            }
        }
        if let Some(front) = front {
            let _ = self.state.focus_view(front);
        }
        self.state.set_workspace_root(path.clone());
        self.invalidate_root_scoped_operations();
        self.start_git_status_refresh_silent();
        if let Some(location) = reopen {
            self.navigate_persisted_location(location);
        }
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.notify_message(format!("switched to {name}"), false);
    }
    pub(in crate::app) fn root_removal_blocker(
        &self,
        root: crate::workspace::RootId,
    ) -> Option<String> {
        let root = self
            .workspace
            .roots()
            .iter()
            .find(|candidate| candidate.id == root)?;
        if self.state.buffers.values().any(|buffer| {
            buffer.dirty
                && buffer
                    .path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&root.identity.canonical_path))
        }) {
            return Some(self.message("workspace_root_dirty_blocker", &[]));
        }
        if self
            .terminal_session
            .belongs_to_root(&root.identity.canonical_path)
        {
            return Some("Close the root’s terminals before removing it".into());
        }
        let busy = self.workspace.root_operation_pending()
            || self.language.active().is_some()
            || self
                .language
                .sessions()
                .values()
                .any(|state| state.restart_due.is_some())
            || self.debugger_session.is_running()
            || self
                .tooling
                .entries()
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Installing { .. }))
            || self.search.pending()
            || self.workspace_edits.preparing()
            || self.workspace_edits.proposal().is_some()
            || self.documents.legacy_pending()
            || self.workspace.persist_pending()
            || self.documents.has_startup_work()
            || self.documents.file_work_pending();
        busy.then(|| self.message("workspace_root_service_blocker", &[]))
    }

    pub(in crate::app) fn invalidate_root_scoped_operations(&mut self) {
        self.documents.retire_startup();
        self.documents.retire_workspace(&self.workspace.version());
        self.clear_git_status();
        self.terminal_session.retain_workspace(&self.workspace);
        self.reject_terminal_responses("workspace changed");
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.id)
            .collect::<HashSet<_>>();
        let stale = self
            .language
            .sessions()
            .iter()
            .filter_map(|(key, state)| (!roots.contains(&state.root)).then_some(key.clone()))
            .collect::<Vec<_>>();
        let removed_lsp = !stale.is_empty();
        for key in stale {
            let _ = self.language.stop(&key);
            self.clear_lsp_progress(&key);
            self.clear_lsp_server_ownership(&key);
            self.language.remove(&key);
            self.language.forget_synced(&key);
            if self.language.active().map(String::as_str) == Some(&key) {
                self.language.clear_active();

                self.clear_lsp_ui_projection();
            }
        }
        if removed_lsp {
            self.sync_ui_diagnostics();
        }
        self.language.retire_launches();
        for (plugin, load) in self.extension.retire_packages() {
            self.remove_plugin_contribution_generation(&plugin, load);
        }
        self.debugger_session.reset();
        self.input.clear_snippet();
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.tasks.retire_workspace();
        self.clear_git_status();
        self.search.cancel();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.retire_workspace_resources("workspace changed");
        self.retire_plugin_edit_sources();
        self.retire_explorer_intents();
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.ui.explorer = None;
        self.set_explorer_focus(false);
        self.reconcile_explorer_watch();
        self.close_picker();
        self.ensure_cursor_visible();
    }

    pub(in crate::app) fn remove_workspace_root(
        &mut self,
        id: crate::workspace::RootId,
    ) {
        if let Some(blocker) = self.root_removal_blocker(id) {
            self.show_error(blocker);
            return;
        }
        match self.workspace.remove_root(id) {
            Ok(root) => {
                if let Some(active) = self
                    .workspace
                    .active_root()
                    .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                {
                    self.state
                        .set_workspace_root(active.identity.canonical_path.clone());
                }
                self.invalidate_root_scoped_operations();
                self.notify_message(
                    self.message(
                        "workspace_root_removed",
                        &[("path", root.identity.canonical_path.display().to_string())],
                    ),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::app) fn activate_workspace_root(
        &mut self,
        id: crate::workspace::RootId,
    ) {
        let before = self.workspace.version();
        match self.workspace.set_active_root(id) {
            Ok(()) => {
                if before != self.workspace.version() {
                    self.documents.retire_startup();
                }
                self.documents.retire_workspace(&self.workspace.version());
                self.clear_git_status();
                let root = self
                    .workspace
                    .roots()
                    .iter()
                    .find(|root| root.id == id)
                    .map(|root| root.identity.canonical_path.clone());
                // The statusline and relative labels name the active root.
                if let Some(root) = &root {
                    self.state.set_workspace_root(root.clone());
                }
                let path = root
                    .map(|root| root.display().to_string())
                    .unwrap_or_default();
                self.notify_message(
                    self.message("workspace_root_activated", &[("path", path)]),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
}

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

    pub(in crate::app) fn selected_lsp_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> LspRuntimeSource {
        self.tooling.selected_lsp_runtime_source(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }

    pub(in crate::app) fn selected_managed_definition(
        &self,
        definition: &ToolDefinition,
    ) -> ToolDefinition {
        self.tooling.selected_managed_definition(definition)
    }

    pub(in crate::app) fn managed_variant_installed(
        &self,
        base: &ToolDefinition,
        variant: &ToolDefinition,
    ) -> bool {
        self.tooling.managed_variant_installed(base, variant)
    }

    pub(in crate::app) fn selected_managed_executable(
        &self,
        definition: &ToolDefinition,
    ) -> Option<PathBuf> {
        self.tooling.selected_managed_executable(definition)
    }

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

    fn set_tool_probe_directory(&self) {
        self.tooling
            .set_probe_directory(self.tool_probe_directory());
    }

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

    pub(in crate::app) fn rust_analyzer_is_rustup_proxy(path: &Path) -> bool {
        crate::app::tools::ToolingSession::rust_analyzer_is_rustup_proxy(path)
    }

    pub(in crate::app) fn unavailable_rust_analyzer_message() -> String {
        crate::app::tools::ToolingSession::unavailable_rust_analyzer_message()
    }

    pub(in crate::app) fn rust_analyzer_path_version(
        version: &str,
        rustup_bundled: bool,
    ) -> (String, String) {
        crate::app::tools::ToolingSession::rust_analyzer_path_version(
            version,
            rustup_bundled,
        )
    }

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
            .max(TOOL_MANAGER_MINIMUM_HEIGHT);
        usize::from(
            self.tool_menu_height()
                .min(compact_height)
                .saturating_sub(9)
                / 2,
        )
        .max(1)
    }

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

    pub(in crate::app) fn selected_tool_id(&self) -> Option<String> {
        let menu = self.ui.tool_menu.as_ref()?;
        menu.runtime_picker
            .as_ref()
            .map(|picker| picker.server_id.clone())
            .or_else(|| menu.rows.get(menu.selected).map(|row| row.id.clone()))
    }

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

    /// rustup and the rust-analyzer proxy on PATH, when rustup manages the toolchain.
    fn rustup_toolchain(&self, definition: &ToolDefinition) -> Option<(PathBuf, PathBuf)> {
        if definition.id != "rust-analyzer" {
            return None;
        }
        let (proxy, rustup_proxy) = self.tooling.path_executable(&definition.executable)?;
        let rustup = proxy.with_file_name("rustup");
        (rustup_proxy && rustup.is_file()).then_some((rustup, proxy))
    }

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

    /// Servers running from this tool, which a toolchain repair restarts.
    fn lsp_sessions_for_tool(&self, id: &str) -> Vec<String> {
        self.language
            .sessions()
            .iter()
            .filter(|(_, state)| state.definition.id == id && state.desired_running)
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(in crate::app) fn restart_lsp(&mut self, server: &str) {
        self.language.request_start(server, Some(Instant::now()));
        let _ = self.language.stop(server);
        self.language.forget_status(server);
        self.clear_lsp_progress(server);
    }

    pub(in crate::app) fn install_tool_by_id(
        &mut self,
        id: &str,
        source: Option<LspRuntimeSource>,
    ) {
        self.install_tool(id, source);
    }
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

    pub(in crate::app) fn process_tool_progress(&mut self) {
        let changed = self.tooling.poll_progress();
        if let (Some(_), Some(menu)) = (changed, &self.ui.tool_menu) {
            self.rebuild_tool_menu(&menu.category.clone(), None);
        }
    }

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

impl App {
    pub(super) fn install_package_after_validation(
        &mut self,
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    ) {
        let root = self.state.workspace_root.clone();
        let absolute = |path: PathBuf| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        };
        let operation = PackageOperation::Install {
            package_manifest: absolute(package_manifest),
            plugin_manifest: absolute(plugin_manifest),
            component: absolute(component),
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            limits: self.plugin_limits(),
            update,
        };
        if let Err(error) = self.extension.request_package(operation) {
            self.show_error(error)
        }
    }

    pub(in crate::app) fn request_registry(&mut self, request: RegistryRequest, install: bool) {
        let picker = if install {
            None
        } else {
            let title = match &request {
                RegistryRequest::Search { .. } => "package_registry_search_title",
                _ => "package_registry_show_title",
            };
            self.install_picker(self.message(title, &[]), Vec::new());
            self.search.picker_epoch().cloned()
        };
        if let Err(error) = self.extension.request_registry(request, install, picker) {
            self.show_error(error);
        }
    }
    pub(in crate::app) fn process_registry_events(&mut self) {
        use crate::app::plugins::RegistryUpdate;
        for update in self.extension.poll_registry() {
            match update {
                RegistryUpdate::Present(_, ref picker)
                    if !picker
                        .as_ref()
                        .is_some_and(|epoch| self.search.owns_picker_epoch(epoch)) => {}
                RegistryUpdate::Present(result, _) => match *result {
                    RegistryResult::Search(result) => {
                        let items = result
                            .packages
                            .into_iter()
                            .enumerate()
                            .map(|(index, package)| {
                                (
                                    PickerItem {
                                        badge: String::new(),
                                        kind: crate::workspace::PickerItemKind::Entry,
                                        id: PickerItemId(format!(
                                            "registry:{index}:{}/{}",
                                            package.publisher, package.name
                                        )),
                                        label: format!("{}/{}", package.publisher, package.name),
                                        detail: format!(
                                            "{}{}",
                                            package.version,
                                            if package.yanked { " · yanked" } else { "" }
                                        ),
                                        source: package.description.clone(),
                                        action: PickerAction::None,
                                    },
                                    PickerPayload::RegistrySummary(package),
                                )
                            })
                            .collect();
                        self.install_picker(
                            self.message("package_registry_search_title", &[]),
                            items,
                        );
                    }
                    RegistryResult::Show(metadata) => {
                        let label = format!("{}/{}", metadata.publisher, metadata.name);
                        let detail = format!("{} · {}", metadata.version, metadata.artifact_sha256);
                        self.install_picker(
                            self.message("package_registry_show_title", &[]),
                            vec![(
                                PickerItem {
                                    badge: String::new(),
                                    kind: crate::workspace::PickerItemKind::Entry,
                                    id: PickerItemId(format!("registry-show:{label}")),
                                    label,
                                    detail,
                                    source: metadata.artifact_path.clone(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::RegistryMetadata(Box::new(metadata)),
                            )],
                        );
                    }
                    RegistryResult::Artifact { .. } => self.show_error("unowned registry artifact"),
                },
                RegistryUpdate::ValidateArtifact => match self.workspace.execution_allowed() {
                    Ok(()) => self.run_trusted(TrustedAction::PrepareRegistryArtifact),
                    Err(failure) => {
                        self.show_workspace_failure(failure);
                        self.extension.discard_artifact();
                    }
                },
                RegistryUpdate::Error(error) => self
                    .show_error(self.message("package_registry_failed", &[("diagnostic", error)])),
            }
        }
    }
    pub(in crate::app) fn process_package_events(&mut self) {
        let updates = self.extension.poll_packages(
            self.plugin_limits(),
            &self.configuration.settings().plugins,
            self.workspace.safe_mode(),
        );
        self.apply_extension_updates(updates);
    }
    pub(in crate::app) fn apply_extension_updates(
        &mut self,
        updates: Vec<crate::app::plugins::ExtensionUpdate>,
    ) {
        use crate::app::plugins::ExtensionUpdate;
        for update in updates {
            match update {
                ExtensionUpdate::Remove(plugin, load) => {
                    self.remove_plugin_contribution_generation(&plugin, load)
                }
                ExtensionUpdate::Error(error) => self.show_error(error),
                ExtensionUpdate::Conflict(plugin) => {
                    self.show_error(self.message("package_plugin_conflict", &[("plugin", plugin)]))
                }
                ExtensionUpdate::Message(key, arguments) => {
                    self.notify_message(self.message(key, &arguments), false)
                }
                ExtensionUpdate::Lock(lock) => {
                    let items: Vec<_> = lock
                        .packages
                        .into_iter()
                        .enumerate()
                        .map(|(index, package)| {
                            (
                                PickerItem {
                                    badge: String::new(),
                                    kind: crate::workspace::PickerItemKind::Entry,
                                    id: PickerItemId(format!("package:{index}:{}", package.name)),
                                    label: package.name,
                                    detail: package.version,
                                    source: "package".into(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::Command(String::new()),
                            )
                        })
                        .collect();
                    let empty = items.is_empty();
                    self.install_picker(self.message("package_picker_title", &[]), items);
                    if empty {
                        self.search
                            .set_picker_footer(self.message("package_picker_empty", &[]));
                        self.sync_picker_ui();
                    }
                }
                ExtensionUpdate::Failed { operation, error } => {
                    self.health.record(
                        LogLevel::Error,
                        "packages",
                        "operation-failed",
                        BTreeMap::from([
                            ("operation".into(), operation.clone()),
                            ("error".into(), error.clone()),
                        ]),
                    );
                    self.show_error(self.message(
                        "package_operation_failed",
                        &[("operation", operation), ("diagnostic", error)],
                    ));
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn sync_lsp_status_ui(&mut self) {
        let running = self.language.running_servers();
        let server = self
            .language
            .active()
            .and_then(|server| {
                self.language
                    .sessions()
                    .get(server)
                    .map(|session| (server, session))
            })
            .map(|(server, session)| {
                if session.capabilities.is_some() {
                    session.server_id.clone()
                } else if session.circuit_open {
                    format!("failed {}", session.server_id)
                } else if session.restart_due.is_some() {
                    format!("backoff {}", session.server_id)
                } else if running.contains(server) {
                    format!("starting {}", session.server_id)
                } else {
                    format!("stopped {}", session.server_id)
                }
            });
        self.ui.set(server, |ui| &mut ui.lsp_server);
        let refresh = self.ui.tool_menu.as_ref().is_some_and(|menu| {
            menu.rows.iter().any(|row| {
                self.tooling
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.definition.id == row.id && entry.definition.kind == ToolKind::Lsp
                    })
                    .is_some_and(|entry| {
                        !matches!(entry.status, ToolStatus::Installing { .. })
                            && self.lsp_menu_lifecycle(&row.id).0 != row.session_state
                    })
            })
        });
        if refresh {
            let category = self
                .ui
                .tool_menu
                .as_ref()
                .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
            let selected = self.selected_tool_id();
            self.rebuild_tool_menu(&category, selected.as_deref());
        }
    }

    pub(in crate::app) fn reconcile_lsp_autostart(
        &mut self,
        buffer: crate::editor::BufferId,
        document: &LspDocument,
        matching_session: bool,
    ) {
        // A server runs for a workspace root; a file outside every root, such as the
        // tutorial's copy, gets none rather than an error.
        if self.workspace.require_path(&document.path).is_err() {
            return;
        }
        // Wait before recording an observation or copying a launch document: another
        // language's discovery can still own the shared lane.
        if !self
            .workspace
            .execution_lane(crate::app::workspace::DiscoveryKind::Language)
            .discovery_available()
        {
            return;
        }
        // Every matching server attaches, as in VS Code and Neovim: Tailwind runs
        // beside the HTML or TypeScript server. They start one at a time because
        // project-root discovery takes one launch at a time.
        let attached = self
            .language
            .sessions()
            .values()
            .filter(|state| state.documents.contains_key(&buffer))
            .map(|state| state.server_id.clone())
            .collect::<Vec<_>>();
        let mut launchable = self
            .tooling
            .entries()
            .iter()
            .filter(|entry| {
                entry.definition.kind == ToolKind::Lsp
                    && entry
                        .definition
                        .automatically_supports_language(&document.language_id)
                    && !matches!(entry.status, ToolStatus::Installing { .. })
                    && self.selected_lsp_runtime_available(&entry.definition)
            })
            .collect::<Vec<_>>();
        // The most specific server starts first and stays primary for hover and
        // definitions: typescript-language-server before Tailwind.
        launchable.sort_by_key(|entry| entry.definition.languages.len());
        let launchable = launchable
            .into_iter()
            .map(|entry| entry.definition.id.clone())
            .collect::<Vec<_>>();
        if !self
            .workspace
            .execution_lane(crate::app::workspace::DiscoveryKind::Language)
            .discovery_available()
            || launchable.iter().any(|id| self.language.launch_pending(id))
        {
            return;
        }
        let launchable_server = launchable
            .iter()
            .find(|id| {
                !attached.contains(id)
                    && !self.language.is_stopped(id)
                    && !self.language.explicitly_detached(id, buffer)
                    && !self.language.project_root_missing(
                        id,
                        &document.path,
                        &self.workspace.version(),
                        self.configuration.binding_revision(),
                    )
            })
            .cloned();
        // Like mason-lspconfig, install the server when none can start, or point at it when
        // automatic installs are off or this host has no way to install it.
        if launchable.is_empty() && !matching_session && self.configuration.settings().lsp.enabled {
            let installable = self
                .tooling
                .entries()
                .iter()
                .filter(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry
                            .definition
                            .automatically_supports_language(&document.language_id)
                        && (entry.status == ToolStatus::Available
                            && entry.definition.install.is_some()
                            || self
                                .rust_analyzer_rustup_install(&entry.definition)
                                .is_some())
                })
                // The most specific server: zls for Zig, not a spell checker that covers it.
                .min_by_key(|entry| entry.definition.languages.len())
                .map(|entry| {
                    let source = if self
                        .rust_analyzer_rustup_install(&entry.definition)
                        .is_some()
                    {
                        Some(LspRuntimeSource::Path)
                    } else {
                        Some(self.selected_lsp_runtime_source(&entry.definition)).filter(|source| {
                            matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
                        })
                    };
                    (
                        entry.definition.id.clone(),
                        entry.definition.name.clone(),
                        source,
                    )
                });
            if let Some((id, server, source)) = installable
                && self.language.hint_install_once(&document.language_id)
            {
                if source.is_some()
                    && self.configuration.settings().lsp.auto_install
                    && self.workspace.execution_allowed().is_ok()
                    && self
                        .workspace
                        .authorize_execution(ExecutionSource::ToolInstall)
                        .is_ok()
                {
                    self.install_tool(&id, source);
                    return;
                }
                self.notify_message(
                    self.message(
                        "lsp_install_hint",
                        &[
                            ("server", server),
                            ("language", document.language_id.clone()),
                        ],
                    ),
                    false,
                );
            }
        }
        let observation = LspAutostartObservation {
            buffer,
            path: document.path.clone(),
            language: document.language_id.clone(),
            root_revision: self.workspace.root_revision(),
            trust: self.workspace.trust(),
            enabled: self.configuration.settings().lsp.enabled,
            auto_start: self.configuration.settings().lsp.auto_start,
            nix_workspace_fallback: self.configuration.settings().lsp.nix_workspace_fallback,
            launchable_server,
        };
        if let Some(server) = self
            .language
            .observe(observation, self.workspace.safe_mode())
        {
            self.start_lsp(Some(server));
        }
    }

    pub(in crate::app) fn sync_lsp_document(&mut self) {
        let mut changed = self.language.retry_documents(&self.state);
        let Some(buffer) = self.state.active_buffer_id() else {
            self.language.reset_autostart();
            return;
        };
        // The text is copied only for a server that needs it, not on every pass.
        let Some((_, mut document)) = self.active_lsp_document_without_text() else {
            let stale = self
                .language
                .sessions()
                .iter()
                .filter_map(|(key, state)| {
                    state
                        .documents
                        .get(&buffer)
                        .map(|document| (key.clone(), document.uri()))
                })
                .collect::<Vec<_>>();
            self.detach_lsp_documents(buffer, stale);
            // Focusing a panel or terminal leaves the files beside it showing their hints.
            let mut hints = std::mem::take(&mut self.ui.lsp_inlay_hints);
            hints.retain(|hint| hint.buffer != buffer);
            self.clear_lsp_ui_projection();
            self.ui.lsp_inlay_hints = hints;
            self.language.clear_active();

            self.language.reset_autostart();
            return;
        };
        let revision = self.state.buffers[buffer].text.source_version();
        let mut sessions = Vec::new();
        let mut detach = Vec::new();
        for (key, state) in self.language.sessions() {
            // A lexical `starts_with` check would let a session for an outer
            // project absorb a nested, independently resolved project. Only
            // the asynchronous resolver may attach a new document to a server.
            let owns = state.documents.contains_key(&buffer);
            let supports = state.definition.supports_language(&document.language_id);
            if owns && supports {
                if let Some(previous) = state.documents.get(&buffer) {
                    if previous.path != document.path
                        || previous.language_id != document.language_id
                    {
                        detach.push((key.clone(), previous.uri()));
                    } else {
                        sessions.push(key.clone());
                    }
                }
            } else if let Some(previous) = state.documents.get(&buffer) {
                detach.push((key.clone(), previous.uri()));
            }
        }
        sessions.sort();
        if sessions.is_empty() {
            self.language.clear_active();

            self.clear_lsp_ui_projection();
        }
        if !sessions.is_empty()
            && !sessions
                .iter()
                .any(|key| self.language.active() == Some(key))
        {
            // A full language server rather than a linter such as ESLint names the file's server.
            let primary = sessions
                .iter()
                .find(|key| {
                    self.language
                        .sessions()
                        .get(*key)
                        .and_then(|session| session.capabilities.as_ref())
                        .is_some_and(|capabilities| capabilities.definition)
                })
                .unwrap_or(&sessions[0])
                .clone();
            self.select_lsp_session(&primary);
        }
        self.detach_lsp_documents(buffer, detach);
        let completion_open = self.ui.language_completion.is_some()
            && self.language.projection().is_some_and(|owner| {
                owner.buffer == buffer
                    && Some(owner.view) == self.state.active_view
                    && self.language.active() == Some(&owner.server)
                    && owner.uri == document.uri()
            });
        self.refresh_lsp_projection_owner();
        for server in &sessions {
            if self.language.synced(server, buffer, &revision) {
                continue;
            }
            if document.text.is_empty() {
                document.text = self.state.buffers[buffer].text.to_string();
            }
            if self
                .language
                .sync_owned(server, document.clone(), (buffer, revision.clone()))
                .is_ok()
            {
                changed = true;
            }
        }
        if changed {
            self.request_lsp_colors();
        }
        if changed
            && !(self.configuration.settings().lsp.inlay_hint_level
                == crate::config::InlayHintLevel::Max
                && self.ui.lsp_inlay_hints.is_empty()
                && self.language.inlay_pending())
        {
            self.schedule_lsp_inlay_refresh(false);
        }
        self.reconcile_lsp_autostart(buffer, &document, !sessions.is_empty());
        let typed = self.language.take_typed();
        let active_attached = self
            .language
            .active()
            .is_some_and(|active| sessions.contains(active));
        let trigger =
            typed.is_some_and(|typed| self.language.is_completion_trigger(&sessions, typed));
        if completion_open {
            if changed && trigger && active_attached {
                self.mark_lsp_completion_start(true);
                self.request_lsp(LspFeature::Completion);
            } else if changed && self.language.completion_incomplete() && active_attached {
                self.request_lsp(LspFeature::Completion);
            }
            self.narrow_lsp_completion();
        } else if changed
            && self.configuration.settings().lsp.auto_completion
            && !sessions.is_empty()
            && !self.ui.large_file_buffers.contains(&buffer)
            && self.lsp_completion_should_open(typed, trigger)
        {
            self.mark_lsp_completion_start(true);
            self.request_lsp(LspFeature::Completion);
        }
    }

    /// Whether a typed character opens the menu: a server's trigger character, or a letter that
    /// starts or continues a word. Numbers don't, and a request already out will show its answer.
    fn lsp_completion_should_open(&self, typed: Option<char>, trigger: bool) -> bool {
        if !self.lsp_completion_in_insert() {
            return false;
        }
        if trigger {
            return true;
        }
        typed.is_some_and(completion_word_char)
            && !self.language.request_pending(self.language.generation())
            && self
                .lsp_word_before_cursor()
                .and_then(|word| word.chars().next())
                .is_some_and(|first| !first.is_numeric())
    }

    fn lsp_completion_in_insert(&self) -> bool {
        self.state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .is_some_and(|view| view.mode == EditorMode::Insert)
    }

    /// The word the cursor is at the end of, with where it starts.
    fn lsp_word_before_cursor(&self) -> Option<String> {
        let start = self.lsp_word_start()?;
        let view = &self.state.views[self.state.active_view?];
        let range = ByteRange::new(start, view.cursor).ok()?;
        self.state.buffers.get(view.buffer)?.text.slice(range).ok()
    }

    fn lsp_word_start(&self) -> Option<ByteOffset> {
        let view = &self.state.views[self.state.active_view?];
        let text = &self.state.buffers.get(view.buffer)?.text;
        let line_start = text
            .line_to_byte(text.byte_to_line(view.cursor).ok()?)
            .ok()?;
        let before = text
            .slice(ByteRange::new(line_start, view.cursor).ok()?)
            .ok()?;
        let word = before
            .char_indices()
            .rev()
            .take_while(|(_, character)| completion_word_char(*character))
            .last()
            .map_or(before.len(), |(index, _)| index);
        Some(ByteOffset(line_start.get() + word))
    }

    /// Notes where this completion starts, which later typing is measured from.
    pub(in crate::app) fn mark_lsp_completion_start(&mut self, automatic: bool) {
        let start = self.lsp_word_start();
        self.language.begin_completion(start, automatic);
    }

    pub(in crate::app) fn cancel_lsp_completion(&mut self) {
        self.language.cancel_completion();
        self.ui.language_completion = None;
    }

    /// Shows the items matching what was typed since the request, best first. The menu closes
    /// when the cursor leaves the word or nothing matches.
    pub(in crate::app) fn narrow_lsp_completion(&mut self) {
        let Some(view) = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
        else {
            return;
        };
        let Some(buffer) = self.state.buffers.get(view.buffer) else {
            return;
        };
        let cursor = view.cursor;
        let revision = buffer.text.revision();
        let automatic_left_insert =
            self.language.completion_automatic() && view.mode != EditorMode::Insert;
        let start = self.language.completion_start();
        let same_line = |offset: ByteOffset| {
            buffer.text.byte_to_line(offset).ok() == buffer.text.byte_to_line(cursor).ok()
        };
        if automatic_left_insert || start.is_some_and(|start| start > cursor || !same_line(start)) {
            self.cancel_lsp_completion();
            return;
        }
        if !self.language.completion_needs_narrowing(cursor, revision) {
            return;
        }
        let codec = PositionCodec::new(&buffer.text);
        let typed = |from: ByteOffset| {
            ByteRange::new(from, cursor)
                .ok()
                .and_then(|range| buffer.text.slice(range).ok())
                .unwrap_or_default()
        };
        let from_start = start.map(typed).unwrap_or_default();
        let encodings = self.language.completion_encodings();
        let matches = self.language.score_completions(|server, item| {
            // A server's range says where its item begins; other items start at the word.
            let edit_start = item.text_edit.as_ref().and_then(|edit| {
                let encoding = encodings.get(server).copied()?;
                codec
                    .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
                    .ok()
                    .filter(|offset| *offset <= cursor && same_line(*offset))
            });
            edit_start.map_or_else(|| from_start.clone(), typed)
        });
        self.language.narrow_completions(matches, cursor, revision);
        self.project_lsp_completion();
    }

    /// Mirrors the current items into the menu the renderer draws.
    pub(in crate::app) fn project_lsp_completion(&mut self) {
        let icons = &self
            .configuration
            .settings()
            .ui
            .command_line
            .completion_kind_icons;
        let items = self
            .language
            .completion_items()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let kind = Self::lsp_completion_kind(item);
                CompletionItem {
                    icon: icons.get(kind).cloned(),
                    label: item.label.clone(),
                    detail: if kind == "Snippet" {
                        kind.to_owned()
                    } else {
                        self.lsp_server_label(
                            self.language.completion_server(index).unwrap_or_default(),
                        )
                    },
                    color: item.color(),
                }
            })
            .collect::<Vec<_>>();
        let typed = self.lsp_completion_typed_columns();
        let (selected, scroll) = self.language.completion_position();
        self.ui.language_completion = (!items.is_empty()).then_some(LanguageCompletionState {
            typed,
            items,
            selected,
            scroll,
        });
        if self.ui.language_completion.is_some() {
            self.ui.lsp_loading = None;
        }
        self.refresh_lsp_completion_documentation();
    }

    /// How much of the word being completed is already on the line.
    fn lsp_completion_typed_columns(&self) -> u16 {
        let Some(view) = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
        else {
            return 0;
        };
        let Some(start) = self.language.completion_start() else {
            return 0;
        };
        let text = &self.state.buffers[view.buffer].text;
        if start > view.cursor
            || text.byte_to_line(start).ok() != text.byte_to_line(view.cursor).ok()
        {
            return 0;
        }
        let Ok(line) = text.byte_to_line(start) else {
            return 0;
        };
        let (Ok(line_start), Ok(content)) = (text.line_to_byte(line), text.line(line)) else {
            return 0;
        };
        let from = start.get().saturating_sub(line_start.get());
        let to = view.cursor.get().saturating_sub(line_start.get());
        content.get(from..to).map_or(0, |typed| {
            u16::try_from(configured_text_width(
                typed,
                self.configuration.settings().editor.tab_width,
                self.configuration.settings().editor.ambiguous_width,
            ))
            .unwrap_or(u16::MAX)
        })
    }

    /// `<leader>cr` and F2: the command line, ready with the name under the cursor.
    fn word_at_cursor(&self) -> Option<String> {
        self.state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .and_then(|view| {
                let source = self.state.buffers.get(view.buffer)?.text.to_string();
                let cursor = view.cursor.get();
                let start = source[..cursor]
                    .char_indices()
                    .rev()
                    .take_while(|(_, character)| completion_word_char(*character))
                    .last()
                    .map_or(cursor, |(index, _)| index);
                let end = source[cursor..]
                    .char_indices()
                    .find(|(_, character)| !completion_word_char(*character))
                    .map_or(source.len(), |(relative, _)| cursor + relative);
                Some(source[start..end].to_owned())
            })
    }

    pub(in crate::app) fn open_rename_prompt(&mut self) {
        let word = self.word_at_cursor();
        self.open_prompt(
            PromptKind::Command,
            format!("rename {}", word.unwrap_or_default()),
        );
    }

    /// Servers list nothing for an empty query, so `<leader>sS` asks for one.
    pub(in crate::app) fn open_workspace_symbols_prompt(&mut self) {
        let word = self.word_at_cursor().unwrap_or_default();
        self.open_prompt(PromptKind::Command, format!("lsp.workspace_symbols {word}"));
    }
}

/// Where a conflicted file's first marker is, so opening it lands there.
fn first_conflict_line(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().position(|line| line.starts_with("<<<<<<<")))
        .unwrap_or(0)
}

/// A character of an identifier, for the word completion replaces and filters on.
/// Where a search from `cursor` lands, as a byte offset.
fn search_target(
    text: &TextBuffer,
    matches: &[std::ops::Range<usize>],
    cursor: ByteOffset,
    forward: bool,
    wrap: bool,
) -> Option<usize> {
    let offset = cursor.get().min(text.len_bytes());
    if forward {
        let start = if offset < text.len_bytes() {
            crate::editor::text::next_grapheme_boundary(text, cursor).get()
        } else {
            offset
        };
        matches
            .iter()
            .find(|matched| matched.start >= start)
            .or_else(|| wrap.then(|| matches.first()).flatten())
            .map(|matched| matched.start)
    } else {
        matches
            .iter()
            .rev()
            .find(|matched| matched.start < offset)
            .or_else(|| wrap.then(|| matches.last()).flatten())
            .map(|matched| matched.start)
    }
}

fn completion_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

impl App {
    pub(in crate::app) fn active_lsp_document(
        &self,
    ) -> Option<(crate::editor::BufferId, LspDocument)> {
        let (buffer_id, mut document) = self.active_lsp_document_without_text()?;
        document.text = self.state.buffers[buffer_id].text.to_string();
        Some((buffer_id, document))
    }

    /// The active document's path, language and version, with empty text.
    pub(in crate::app) fn active_lsp_document_without_text(
        &self,
    ) -> Option<(crate::editor::BufferId, LspDocument)> {
        let buffer_id = self.state.active_buffer_id()?;
        let buffer = self.state.buffers.get(buffer_id)?;
        let path = buffer.path.clone()?;
        let language_id = language_id_for_path(&path)?.to_owned();
        Some((
            buffer_id,
            LspDocument {
                path,
                language_id,
                version: buffer.text.revision().0,
                text: String::new(),
            },
        ))
    }

    pub(in crate::app) fn start_lsp(&mut self, requested: Option<String>) {
        self.launch_lsp(requested, false);
    }

    /// `asked` is a person starting this server, who is told when the project can't have it.
    fn launch_lsp(&mut self, requested: Option<String>, asked: bool) {
        if let Some(id) = requested.as_ref().filter(|_| asked) {
            self.language.clear_stopped(id);
        }
        if !self.configuration.settings().lsp.enabled {
            self.show_error(self.message("lsp_disabled", &[]));
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let entry = if let Some(id) = requested.as_ref() {
            let entry =
                self.tooling.entries().iter().find(|entry| {
                    entry.definition.id == *id && entry.definition.kind == ToolKind::Lsp
                });
            if !entry.is_some_and(|entry| entry.definition.supports_language(&document.language_id))
            {
                self.show_error(format!(
                    "language server `{id}` does not support {}",
                    document.language_id
                ));
                return;
            }
            entry
        } else {
            self.tooling
                .entries()
                .iter()
                .find(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry
                            .definition
                            .automatically_supports_language(&document.language_id)
                        && matches!(entry.status, ToolStatus::Installed(_))
                })
                .or_else(|| {
                    self.tooling.entries().iter().find(|entry| {
                        entry.definition.kind == ToolKind::Lsp
                            && entry
                                .definition
                                .automatically_supports_language(&document.language_id)
                    })
                })
        };
        let Some(entry) = entry else {
            self.show_error(self.message("lsp_no_server", &[]));
            return;
        };
        if matches!(entry.status, ToolStatus::Installing { .. }) {
            self.show_error("language-server runtime is being changed");
            return;
        }
        let base_definition = entry.definition.clone();
        let source = self.selected_lsp_runtime_source(&base_definition);
        let definition = if matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix) {
            self.selected_managed_definition(&base_definition)
        } else {
            base_definition.clone()
        };
        let launch_source = match source {
            LspRuntimeSource::Path => {
                let Some(executable) = discover_executable(&definition.executable) else {
                    self.show_error(format!(
                        "{} is not available on PATH; press p to choose another runtime",
                        definition.name
                    ));
                    return;
                };
                if definition.id == "rust-analyzer"
                    && Self::rust_analyzer_is_rustup_proxy(&executable)
                {
                    let key = self.tool_version_key(
                        &definition,
                        LspRuntimeSource::Path,
                        Some(&executable),
                    );
                    const CHECKING: &str = "checking rust-analyzer in the active rustup toolchain…";
                    match self.tooling.version(&key) {
                        Some(Ok(_)) => {
                            if self
                                .notices
                                .current()
                                .is_some_and(|(_, notice)| notice == CHECKING)
                            {
                                self.clear_notice();
                            }
                        }
                        Some(Err(_)) => {
                            self.show_error(Self::unavailable_rust_analyzer_message());
                            return;
                        }
                        None => {
                            self.queue_tool_version_probe(
                                &definition,
                                LspRuntimeSource::Path,
                                executable,
                            );
                            self.notify_message(CHECKING.into(), false);
                            return;
                        }
                    }
                }
                LspLaunchSource::Direct {
                    executable,
                    managed: false,
                }
            }
            LspRuntimeSource::Managed => {
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust-download version {} is not installed; press i to install it",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                LspLaunchSource::Direct {
                    executable,
                    managed: true,
                }
            }
            LspRuntimeSource::Nix => {
                let Some(shell) = nix_workspace_shell_for_host(
                    &definition,
                    self.configuration.settings().lsp.nix_workspace_fallback,
                    is_nixos_host(),
                ) else {
                    self.show_error("Dust via Nix is unavailable on this host");
                    return;
                };
                if !nix_lsp_profile_installed(&definition) {
                    self.show_error("Dust via Nix is not installed; press i to install it");
                    return;
                }
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust via Nix version {} is not installed; press i",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                if let Err(error) = nix_lsp_launch_environment(&definition) {
                    self.show_error(error);
                    return;
                }
                LspLaunchSource::NixWorkspace {
                    nix: executable,
                    shell: shell.into(),
                }
            }
        };
        self.language.clear_error(&definition.id);
        let Ok(root_id) = self.workspace.require_path(&document.path) else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let Some(root_path) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        if let Err(error) = self.workspace.authorize_execution(
            if matches!(launch_source, LspLaunchSource::NixWorkspace { .. }) {
                ExecutionSource::ProjectLocal
            } else {
                ExecutionSource::SystemTool
            },
        ) {
            self.show_error(error);
            return;
        }
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.discover_launch(
            PendingLspProjectRoot {
                asked,
                definition,
                launch_source,
                root_id,
                workspace_root: root_path,
                buffer,
                document,
            },
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::app) fn process_lsp_project_root(&mut self) {
        let Some(result) = self
            .workspace
            .project_result(crate::app::workspace::DiscoveryKind::Language)
        else {
            return;
        };
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.accept_project_root(
            result,
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::app) fn process_nix_lsp_workspace(&mut self) {
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.poll_nix_launch(
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::app) fn queue_lsp_preflight(&mut self, pending: LspLaunchRequest) {
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.queue_launch(
            pending,
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }

    pub(in crate::app) fn restart_lsp_session(&mut self, key: String) {
        self.language.retire_documents(&self.state);
        let Some(state) = self.language.sessions().get(&key) else {
            return;
        };
        if state.circuit_open || !state.desired_running {
            return;
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        let Some((&buffer, document)) = state.documents.iter().find(|(id, document)| {
            self.state
                .buffers
                .get(**id)
                .is_some_and(|buffer| buffer.path.as_ref() == Some(&document.path))
        }) else {
            return;
        };
        let (definition, launch, root, root_path, document) = (
            state.definition.clone(),
            state.launch.clone(),
            state.root,
            state.root_path.clone(),
            document.clone(),
        );
        self.queue_lsp_preflight(LspLaunchRequest {
            session_key: key,
            definition,
            launch,
            root_id: root,
            project_root: root_path,
            buffer,
            document,
            restart: true,
        });
    }

    pub(in crate::app) fn select_lsp_session(&mut self, key: &str) {
        if self.language.select(key) {
            self.ui.lsp_inlay_hints.clear();
            self.schedule_lsp_inlay_refresh(true);
        }
    }

    pub(in crate::app) fn show_lsp_sessions(&mut self) {
        let active = self.language.active().map(String::as_str);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let items = self
            .language
            .sessions()
            .iter()
            .map(|(key, session)| {
                let root = &session.root_path;
                let folder = root.file_name().map_or_else(
                    || root.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                // The folder and state come first so a narrow picker keeps them; the path last.
                let state = if session.circuit_open {
                    "crashed"
                } else if session.restart_due.is_some() {
                    "restarting"
                } else if session.desired_running {
                    "running"
                } else {
                    "stopped"
                };
                let mut detail = format!("{folder} · {state}");
                if active == Some(key) {
                    detail.push_str(" · active");
                }
                match session.restart_count {
                    0 => {}
                    1 => detail.push_str(" · 1 restart"),
                    count => detail.push_str(&format!(" · {count} restarts")),
                }
                let source = match home
                    .as_deref()
                    .and_then(|home| root.strip_prefix(home).ok())
                {
                    Some(rest) => format!("~/{}", rest.display()),
                    None => root.display().to_string(),
                };
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("lsp-session:{key}")),
                        label: session.server_id.clone(),
                        detail,
                        source,
                        action: PickerAction::None,
                    },
                    PickerPayload::LspSession(key.clone()),
                )
            })
            .collect();
        self.install_picker(self.message("picker_lsp_sessions_title", &[]), items);
    }

    pub(in crate::app) fn clear_lsp_server_ownership(&mut self, server: &str) {
        for prefix in ["lsp.", "lsp-pull:"] {
            if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.sync_ui_diagnostics();
        self.language.forget_synced(server);
        self.language.retire_projection_server(server);
    }

    pub(in crate::app) fn detach_lsp_documents(
        &mut self,
        buffer: crate::editor::BufferId,
        documents: Vec<(String, String)>,
    ) {
        let detached = !documents.is_empty();
        for (server, uri) in documents {
            let _ = self.language.close(&server, uri.clone());
            self.language.invalidate_document(&server, buffer);
            self.language.retire_projection_document(&server, &uri);
            for prefix in ["lsp.", "lsp-pull:"] {
                if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                    self.diagnostics.clear_owner_uri(&owner, &uri);
                }
            }
            self.language.detach(&server, buffer);
        }
        if detached {
            self.sync_ui_diagnostics();
            if self.state.active_buffer_id() == Some(buffer) {
                self.clear_lsp_ui_projection();
            }
        }
    }
}

impl App {
    pub(in crate::app) fn process_core_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        // Refuse before replacing the save-and-stage intent owned by the running mutation.
        if self.git.mutation_pending()
            && matches!(
                effect,
                AppEffect::GitFileStage
                    | AppEffect::GitFileUnstage
                    | AppEffect::GitHunkStage
                    | AppEffect::GitHunkUnstage
                    | AppEffect::GitLinesStage { .. }
                    | AppEffect::GitConflictStage
                    | AppEffect::GitHunkReset
            )
        {
            self.show_error("a Git mutation is already running");
            return Ok(());
        }
        match effect {
            AppEffect::Quit { force } => {
                if !force && self.state.buffers.iter().any(|(_, buffer)| buffer.dirty) {
                    self.show_error(self.message("unsaved_quit", &[]));
                } else {
                    if force {
                        self.queue_dirty_recovery_before_exit();
                    }
                    self.should_quit = true;
                }
            }
            AppEffect::Save {
                quit_after,
                path,
                force,
            } => {
                if path.is_some() || force || !self.begin_format_on_save(quit_after) {
                    self.start_save(quit_after, path, force);
                }
            }
            AppEffect::SaveAll { quit_after } => self.save_all(quit_after),
            AppEffect::OpenPath(path) => self.edit_path(path),
            AppEffect::ForceOpenPath(path) => {
                let wanted = self.state.workspace_root.join(&path).canonicalize().ok();
                let current = self
                    .state
                    .active_buffer_id()
                    .and_then(|id| self.state.buffers[id].path.as_ref())
                    .and_then(|file| file.canonicalize().ok());
                if wanted.is_some() && wanted == current {
                    self.start_reload(true);
                } else {
                    self.edit_path(path);
                }
            }
            // Settings files are optional, so the first open creates one to edit.
            AppEffect::OpenUserFile(path) => match create_user_file(&path) {
                Ok(()) => self.start_open(path),
                Err(error) => self.show_error(format!("{}: {error}", path.display())),
            },
            AppEffect::Reload { force } => self.start_reload(force),
            AppEffect::CloseBuffer { force } => self.close_active_buffer(force),
            AppEffect::CycleBuffer(offset) => {
                self.project_window_content_tabs();
                let order = self.window_session.content_tab_buffers();
                match self.state.cycle_buffer(&order, offset, true) {
                    Ok(_) => {
                        self.set_dashboard_visibility(false);
                    }
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::ToggleBufferPin => self.toggle_active_buffer_pin(),
            AppEffect::MoveBufferLine(direction) => self.move_active_buffer_line(direction),
            AppEffect::NewBuffer => {
                if let Err(error) = self.state.open_text(TextBuffer::from_text(""), None) {
                    self.show_error(error);
                    return Ok(());
                }
                self.set_dashboard_visibility(false);
                self.notify_message(self.message("new_scratch", &[]), false);
            }
            AppEffect::OpenWorkspaceFilePicker => self.start_workspace_file_picker(),
            AppEffect::NewTab => {
                // An untouched [No Name] tab is reused, so ctrl-t never piles up empty tabs;
                // the file picked from the finder replaces it.
                let unused = self.state.buffers.iter().find(|(_, buffer)| {
                    buffer.path.is_none()
                        && !buffer.dirty
                        && buffer.text.len_bytes() == 0
                        && buffer.origin == crate::editor::BufferOrigin::FileOrScratch
                });
                match unused.map(|(id, _)| id) {
                    Some(buffer) => {
                        if let Err(error) = self.state.show_buffer(buffer) {
                            self.show_error(error);
                        }
                    }
                    None => {
                        if let Err(error) = self.state.open_text(TextBuffer::from_text(""), None) {
                            self.show_error(error);
                            return Ok(());
                        }
                    }
                }
                self.set_dashboard_visibility(false);
                self.start_workspace_file_picker();
            }
            AppEffect::GitFileStage => {
                self.input.set_pending_stage(AppEffect::GitFileStage);
                // The plan itself refuses a path whose buffer has unsaved edits and reports
                // `UnsavedDocument`, which the mutation handler turns into the explicit
                // save-and-stage choice. The surface only has to name the right file.
                let file = self.changes_focused_relative();
                if let Err(error) = self.git.start_plan_for(
                    &self.state,
                    crate::git::GitMutationKind::StageFile,
                    self.workspace.root_revision(),
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    Vec::new(),
                    file,
                ) {
                    self.input.clear_pending_stage();
                    self.show_error(error);
                }
            }
            AppEffect::GitFileUnstage => {
                self.input.set_pending_stage(AppEffect::GitFileUnstage);
                let file = self.changes_focused_relative();
                if let Err(error) = self.git.start_plan_for(
                    &self.state,
                    crate::git::GitMutationKind::UnstageFile,
                    self.workspace.root_revision(),
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    Vec::new(),
                    file,
                ) {
                    self.input.clear_pending_stage();
                    self.show_error(error);
                }
            }
            AppEffect::GitLinesStage { start, end } => self.stage_git_lines(start, end),
            AppEffect::GitWorktreeCreate { branch, path } => {
                self.create_git_worktree(branch, path);
            }
            AppEffect::HelpForForm => self.open_form_help(),
            AppEffect::GitConflictsOpen => self.open_git_conflicts(),
            AppEffect::GitConflictAccept { side } => self.accept_conflict_block(&side),
            AppEffect::GitConflictChoose { side } => self.choose_git_conflict_side(&side),
            AppEffect::DebuggerBreakpointStates => self.open_breakpoint_states(),
            AppEffect::DebuggerBreakpointDisable => self.toggle_breakpoint_disabled(),
            AppEffect::DebuggerTerminate => self.terminate_debug_session(),
            AppEffect::DebuggerDetach => self.detach_debug_session(),
            AppEffect::GitConflictStage => self.stage_git_conflict(),
            AppEffect::GitConflictContinue => self.continue_git_conflict(),
            AppEffect::GitConflictAbort => self.abort_git_conflict(),
            AppEffect::GitConflictReplace { side, files } => {
                self.preview_git_replacement(&side, &files);
            }
            AppEffect::Tutor => self.open_tutor(),
            AppEffect::TutorNext => self.tutor_next(),
            AppEffect::WalkJumps(back) => self.walk_jumps(back),
            AppEffect::TutorBack => self.tutor_back(),
            AppEffect::TutorQuit => self.tutor_quit(),
            AppEffect::HelpContextual => self.open_contextual_help(),
            AppEffect::HelpCommandReference => self.open_command_reference(),
            AppEffect::GitWorktreeOpen => self.open_git_worktree(),
            AppEffect::GitWorktreeRemove => self.request_git_worktree_remove(),
            AppEffect::GitWorktreesOpen => self.start_git_worktrees(),
            AppEffect::GitHunksOpenStaged => {
                self.start_git_hunks_with_layer(None, crate::git::GitDiffLayer::Staged);
            }
            AppEffect::GitHunkUnstage => self.unstage_selected_git_hunk(),
            AppEffect::GitStatusRefresh => self.start_git_status_refresh(),
            AppEffect::GitStatusOpen => self.open_git_status_picker(),
            AppEffect::GitBrowse { copy } => self.browse_git_line(copy),
            AppEffect::FlashBegin => self.begin_flash(),
            AppEffect::TodoSearch => self.start_todo_search(),
            AppEffect::Agent(command) => self.run_agent_command(command),
            AppEffect::PullRequestList => self.list_pull_requests(),
            AppEffect::PullRequestComments => self.list_review_comments(),
            AppEffect::PullRequestComment => self.begin_review_comment(),
            AppEffect::GitChangesLayerSwitch => {
                // Only the change surface has a layer to switch; elsewhere this does nothing
                // rather than reporting an error for a key that has no meaning there.
                if self.search.switch_changes_layer().is_some() {
                    self.sync_picker_ui();
                }
            }
            AppEffect::GitHunksOpen => self.start_git_hunks(),
            AppEffect::GitConflictNavigate(direction) => self.navigate_git_conflict(direction),
            AppEffect::MarkdownPreview => {
                let Some(buffer) = self
                    .state
                    .active_buffer_id()
                    .map(|id| &self.state.buffers[id])
                else {
                    return Ok(());
                };
                let title = buffer.path.as_ref().map_or_else(
                    || "Markdown".to_owned(),
                    |path| self.workspace_relative_label(path),
                );
                let value = buffer.text.to_string();
                self.present_documentation_popup(
                    DocumentationPopupKind::Git,
                    title,
                    "Markdown".into(),
                    None,
                    vec![LspMarkupContent {
                        kind: LspMarkupKind::Markdown,
                        value,
                    }],
                );
            }
            AppEffect::GitHunkNavigate(direction) => self.navigate_git_hunk(direction),
            AppEffect::GitHunkPreview => self.preview_selected_git_hunk(),
            AppEffect::GitHunkStage => {
                self.input.set_pending_stage(AppEffect::GitHunkStage);
                self.stage_selected_git_hunk();
            }
            AppEffect::GitHunkReset => self.request_reset_selected_git_hunk(),
            AppEffect::OpenFilePrompt => {
                let alias = self
                    .configuration
                    .settings()
                    .input
                    .command_aliases
                    .iter()
                    .find(|(_, command)| command.as_str() == "file.open")
                    .map_or("file.open", |(alias, _)| alias.as_str());
                self.open_prompt(PromptKind::Command, format!("{alias} "));
            }
            AppEffect::OpenPrompt { kind, initial } => self.open_prompt(kind, initial),
            AppEffect::PromptInsert(text) => self.prompt_insert(&text),
            AppEffect::PromptBackspace => self.prompt_backspace(),
            AppEffect::PromptDeleteWord => self.prompt_delete(|input| input.prompt_delete_word()),
            AppEffect::PromptDeleteToStart => {
                self.prompt_delete(|input| input.prompt_delete_to_start());
            }
            AppEffect::PromptMove(movement) => self.prompt_move(movement),
            AppEffect::PromptHistory(direction) => self.prompt_history(direction),
            AppEffect::PromptComplete(direction) => self.prompt_complete(direction),
            // y and n answer at once, like Vim's own confirm prompts.
            AppEffect::PromptConfirmationSelect(choice) => {
                self.input.select_confirmation(choice);
                self.submit_prompt();
            }
            AppEffect::PromptConfirmationToggle => {
                self.input.toggle_confirmation();
                self.project_input_prompt();
            }
            AppEffect::PromptSubmit => self.submit_prompt(),
            AppEffect::PromptCancel => {
                let declined_trust = self
                    .input
                    .prompt()
                    .is_some_and(|prompt| prompt.kind == PromptKind::WorkspaceTrust);
                self.close_prompt();
                if declined_trust {
                    self.notify_message(self.message("workspace_trust_declined", &[]), false);
                }
            }
            AppEffect::NotificationHistory => {
                let items = self
                    .notices
                    .history()
                    .rev()
                    .enumerate()
                    .map(|(index, (error, message))| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("notification:{index}")),
                                label: message.clone(),
                                detail: if *error {
                                    "error".into()
                                } else {
                                    "info".into()
                                },
                                source: "application".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Notice {
                                title: "Notification".into(),
                                text: message.clone(),
                            },
                        )
                    })
                    .collect();
                self.clear_notice();
                self.install_picker("Notifications".into(), items);
            }
            AppEffect::NotificationDismiss => {
                self.clear_notice();
            }
            AppEffect::SearchRepeat { .. }
            | AppEffect::SearchWord { .. }
            | AppEffect::SearchSelectNext(_) => {
                unreachable!("search effects return their status through process_effect_checked")
            }
            AppEffect::SubstituteAll => {
                if self.substitute_confirm.is_some() {
                    self.answer_substitution(true, true);
                }
            }

            AppEffect::Scroll {
                kind,
                count,
                at_mouse,
            } => {
                if at_mouse {
                    self.focus_mouse_pane();
                }
                self.scroll_view(&kind, count);
            }
            AppEffect::PlaceMouseCursor => self.apply_mouse_cursor(false),
            AppEffect::ExtendMouseSelection => self.apply_mouse_cursor(true),
            AppEffect::Layout(action) => self.apply_layout_action(action),
            AppEffect::ViewToggle(toggle) => {
                let enabled = self.configuration.toggle_view(toggle);
                let name = match toggle {
                    crate::app::ViewToggle::Wrap => "wrap",
                    crate::app::ViewToggle::LineNumbers => "line numbers",
                    crate::app::ViewToggle::RelativeNumbers => "relative numbers",
                    crate::app::ViewToggle::IndentGuides => "indent guides",
                    crate::app::ViewToggle::GitSigns => "git signs",
                    crate::app::ViewToggle::FormatOnSave => "format on save",
                };
                self.ensure_cursor_visible();
                self.notify_message(
                    format!("{name} {}", if enabled { "enabled" } else { "disabled" }),
                    false,
                );
            }
            AppEffect::Zen => {
                let explorer_open = self.ui.explorer.is_some();
                match self
                    .configuration
                    .toggle_zen(explorer_open, self.state.is_zoomed())
                {
                    None => {
                        if explorer_open {
                            let _ = self.process_project_effect(AppEffect::ExplorerClose);
                        }
                        if !self.state.is_zoomed() {
                            self.apply_layout_action(LayoutAction::Zoom);
                        }
                    }
                    Some((reopen_explorer, undo_zoom)) => {
                        if undo_zoom && self.state.is_zoomed() {
                            self.apply_layout_action(LayoutAction::Zoom);
                        }
                        if reopen_explorer {
                            let _ = self.process_project_effect(AppEffect::ExplorerOpen);
                        }
                    }
                }
                self.ensure_cursor_visible();
            }
            AppEffect::SurfaceFocus(direction) => self.focus_surface(direction),
            AppEffect::DashboardSelect(step) => {
                let last = self.ui.dashboard_actions.len().saturating_sub(1);
                self.ui.dashboard_selected = if step < 0 {
                    self.ui
                        .dashboard_selected
                        .saturating_sub(usize::from(step.unsigned_abs()))
                } else {
                    self.ui
                        .dashboard_selected
                        .saturating_add(usize::from(step.unsigned_abs()))
                }
                .min(last);
            }
            AppEffect::DashboardOpen => {
                if let Some(invocation) = self
                    .configuration
                    .dashboard_invocation(self.ui.dashboard_selected)
                {
                    self.invoke_mapped(invocation);
                }
            }
            AppEffect::WindowClone { target, axis } => {
                if let Err(error) = self.clone_graph_window(&target, axis) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowList => {
                let graph = self.window_session.graph();
                let items = graph
                    .windows()
                    .filter_map(|window| {
                        let content = graph.content_for_window(window.id)?;
                        Some((
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("window:{}", window.id.0)),
                                label: content.title.clone(),
                                detail: format!(
                                    "{} · {:?} · {:?} · tab {}",
                                    content.content_type.as_str(),
                                    content.lifecycle,
                                    window.placement,
                                    window.tab.0
                                ),
                                source: content.owner.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Window(window.id),
                        ))
                    })
                    .collect();
                self.install_picker("Windows".into(), items);
            }
            AppEffect::RetainedUiFocus(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Focus(delta));
            }
            AppEffect::RetainedUiScroll(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Scroll(delta));
            }
            AppEffect::RetainedUiActivate => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Activate);
            }
            AppEffect::RetainedUiMouseSelect => self.select_retained_ui_at_mouse(),
            AppEffect::PrivateUiActivate { at_mouse } => {
                self.activate_private_surface_action(at_mouse);
            }
            AppEffect::WindowDock { target, edge, size } => {
                let edge = match edge.as_str() {
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    "top" => DockEdge::Top,
                    "bottom" => DockEdge::Bottom,
                    _ => {
                        self.show_error("window dock edge must be left, right, top, or bottom");
                        return Ok(());
                    }
                };
                let result =
                    self.place_graph_window(&target, WindowPlacement::Docked { edge, size });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFloat {
                target,
                x,
                y,
                width,
                height,
            } => {
                let result = self.place_graph_window(
                    &target,
                    WindowPlacement::Floating(WindowRect {
                        x,
                        y,
                        width,
                        height,
                    }),
                );
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFullscreen(target) => {
                let result = self.place_graph_window(&target, WindowPlacement::Fullscreen);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowHide(target) => {
                let result = self.place_graph_window(&target, WindowPlacement::Hidden);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowShow(target) => {
                let result = self.show_graph_window(&target);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowClose(target) => {
                if let Err(error) = self.close_graph_window(&target) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowResize { target, amount } => {
                if let Err(error) = self.resize_graph_window(&target, amount) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowResizeDirectional(direction, count) => {
                if let Err(error) = self.resize_focused_window(direction, count) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowMove { target, x, y } => {
                let result = self.move_graph_window(&target, x, y);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowSwap { first, second } => {
                if let Err(error) = self.swap_graph_windows(&first, &second) {
                    self.show_error(error);
                }
            }
            AppEffect::RepeatLast(count) => self.repeat_last_change(count),
            AppEffect::MacroRecordStart(register) => {
                self.input.start_macro_recording(register);
                self.notify_message(
                    self.message("macro_recording", &[("register", register.to_string())]),
                    false,
                );
            }
            AppEffect::MacroRecordStop => {
                if let Some((register, invocations)) = self.input.finish_macro_recording() {
                    self.state.registers.set_macro(register, invocations);
                    self.state.mark_editor_changed();
                    self.notify_message(
                        self.message("macro_recorded", &[("register", register.to_string())]),
                        false,
                    );
                } else {
                    self.show_error(self.message("no_macro_recording", &[]));
                }
            }
            AppEffect::MacroReplay { .. } => {
                unreachable!("macro effects return their status through process_effect_checked")
            }
            AppEffect::Grammar(input) => self.process_grammar_input(input),
            AppEffect::CancelPendingMapping => self.cancel_pending_mapping(),
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl App {
    pub(super) fn document_view(
        id: crate::editor::BufferId,
        buffer: &crate::editor::Buffer,
    ) -> crate::app::documents::DocumentView<'_> {
        crate::app::documents::DocumentView {
            id,
            path: buffer.path.as_deref(),
            text: &buffer.text,
            persisted: buffer.persisted_bytes(),
            new_file: buffer.new_file,
            dirty: buffer.dirty,
            virtual_read_only: buffer.is_virtual_read_only(),
            read_only: buffer.read_only,
        }
    }
    pub(in crate::app) fn show_document_failure(
        &mut self,
        failure: crate::app::documents::DocumentFailure,
    ) {
        match failure {
            crate::app::documents::DocumentFailure::Policy(key) => {
                self.show_error(self.message(key, &[]))
            }
            crate::app::documents::DocumentFailure::Error(e) => self.show_error(e),
        }
    }
    /// Open a path the user named. With nothing there it starts a new file, as in Vim, which
    /// the first save creates.
    pub(in crate::app) fn edit_path(&mut self, path: PathBuf) {
        let absolute = self.state.workspace_root.join(&path);
        let missing = matches!(
            std::fs::symlink_metadata(&absolute),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        if !missing {
            self.start_open(path);
            return;
        }
        let path = crate::app::startup::new_file_path(&absolute);
        self.record_jump();
        let mut candidate = self.state.draft();
        if let Err(error) = candidate.open_new_document(path.clone()) {
            self.show_error(error);
            return;
        }
        if let Err(error) = self.commit_editor_draft(candidate, None, true) {
            self.show_error(error);
            return;
        }
        let name = self.notice_file_label(&path);
        self.notify_message(self.message("new_file", &[("path", name)]), false);
    }
    /// The dashboard's Recover Unsaved Work: every journal with the file it was written for.
    /// Enter opens one, ctrl-x (`d` in Normal mode) discards it, Escape leaves them for later.
    pub(in crate::app) fn open_recovery_picker(&mut self) {
        let items = self
            .recovery
            .records()
            .iter()
            .rev()
            .map(|(journal, record)| {
                let label = record.path().as_deref().map_or_else(
                    || SCRATCH_LABEL.to_owned(),
                    |path| self.workspace_relative_label(path),
                );
                let lines = record.content.lines().count();
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("recovery:{}", journal.display())),
                        label,
                        detail: if lines == 1 {
                            "1 line".to_owned()
                        } else {
                            format!("{lines} lines")
                        },
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Recovery(journal.clone()),
                )
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            if self.recovery_picker_open() {
                self.close_picker();
            }
            self.notify_message(self.message("recovery_none", &[]), false);
            return;
        }
        self.install_picker("Recover Unsaved Work".to_owned(), items);
        self.search
            .set_picker_footer("Enter Recover · ctrl-x Discard · Esc Later".to_owned());
        self.sync_picker_ui();
    }

    pub(in crate::app) fn recovery_picker_open(&self) -> bool {
        matches!(
            self.search.selected_picker_payload(),
            Some(PickerPayload::Recovery(_))
        )
    }

    /// Recover a journal into its file, as an unsaved change on top of what is on disk, so undo
    /// goes back to the saved version. Work that never had a file, or whose file is already
    /// open, comes back in a new buffer.
    pub(in crate::app) fn recover_journal(&mut self, journal: &Path) {
        let Some(record) = self
            .recovery
            .records()
            .iter()
            .find(|(path, _)| path == journal)
            .map(|(_, record)| record.clone())
        else {
            self.show_error(self.message("recovery_none", &[]));
            return;
        };
        let original = record.path().filter(|path| {
            !self
                .state
                .buffers
                .values()
                .any(|buffer| buffer.path.as_ref() == Some(path))
        });
        if let Some(original) = original {
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect();
            match self.documents.read_scoped(original, roots) {
                Ok(id) => {
                    self.recovery.register_read(
                        id,
                        journal.to_path_buf(),
                        record.content,
                        self.workspace.version(),
                    );
                }
                Err(error) => self.show_error(error),
            }
            return;
        }
        let (buffer, _) = match self.state.open_text(TextBuffer::from_text(""), None) {
            Ok(opened) => opened,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.replace_with_recovered(buffer, record.content) {
            self.show_error(error);
            return;
        }
        self.finish_recovery(journal, "recovery_opened");
    }
    pub(in crate::app) fn replace_with_recovered(
        &mut self,
        buffer: crate::editor::BufferId,
        content: String,
    ) -> Result<(), String> {
        let text = &self.state.buffers[buffer].text;
        let revision = text.revision();
        let edits = crate::editor::text::TextEditSet::new(
            revision,
            vec![crate::editor::text::TextEdit::new(
                crate::editor::text::ByteRange::new(ByteOffset(0), ByteOffset(text.len_bytes()))
                    .expect("whole buffer"),
                content,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision { buffer, revision })
            .with_precondition(Precondition::BufferWritable(buffer))
            .with_operation(EditorOperation::ApplyEdits { buffer, edits });
        self.state
            .apply_transaction(&transaction)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    pub(in crate::app) fn finish_recovery(&mut self, journal: &Path, message: &str) {
        self.set_dashboard_visibility(false);
        self.ensure_cursor_visible();
        self.notify_message(self.message(message, &[]), false);
        if let Some(buffer) = self.state.active_buffer_id() {
            self.recovery.retain_recovered(buffer, journal.to_owned());
        }
    }

    pub(in crate::app) fn start_open(&mut self, path: PathBuf) {
        self.record_jump();
        match self.documents.request_open(
            path,
            &self.state.workspace_root,
            &self.state.window_version(),
            self.workspace.version(),
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view),
        ) {
            Ok(()) => self.notify_message(self.message("opening_file", &[]), false),
            Err(e) => self.show_document_failure(e),
        }
    }
    pub(in crate::app) fn start_reload(&mut self, force: bool) {
        let view = self.state.active_buffer_id().and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|b| Self::document_view(id, b))
        });
        match self.documents.request_reload(
            view,
            force,
            &self.state.window_version(),
            self.workspace.version(),
        ) {
            Ok(()) => self.notify_message(self.message("reloading", &[]), false),
            Err(e) => self.show_document_failure(e),
        }
    }
    /// The formatter from the tool manager for the active file, the way conform.nvim comes
    /// before the language server in LazyVim.
    pub(in crate::app) fn active_formatter(&self) -> Option<String> {
        let buffer = &self.state.buffers[self.state.active_buffer_id()?];
        let path = buffer.path.as_deref()?;
        let language = language_id_for_path(path)?;
        Some(managed_formatter(|tool| tool.supports_language(language), path)?.id)
    }
    /// Ask the language server to format, and save when it answers. False when there is nothing
    /// to wait for, so the caller saves right away.
    fn begin_format_on_save(&mut self, quit_after: bool) -> bool {
        if !self.configuration.settings().lsp.format_on_save {
            return false;
        }
        if let Some(pending) = self
            .state
            .active_buffer_id()
            .and_then(|buffer| self.format_on_save.get_mut(&buffer))
        {
            pending.quit_after |= quit_after;
            return true;
        }
        // An installed formatter runs as a process, which takes longer to start than a request.
        let managed =
            self.workspace.execution_allowed().is_ok() && self.active_formatter().is_some();
        let buffer = if managed {
            self.state.active_buffer_id()
        } else {
            self.active_lsp_document_without_text()
                .and_then(|(buffer, document)| {
                    self.language
                        .formatting_server(buffer, &document)
                        .map(|_| buffer)
                })
        };
        let Some(buffer) = buffer else {
            return false;
        };
        self.format_on_save.insert(
            buffer,
            super::FormatOnSave {
                request: None,
                buffer,
                source: self.state.buffers[buffer].text.source_version(),
                path: self.state.buffers[buffer].path.clone(),
                formatter: if managed {
                    self.active_formatter()
                } else {
                    None
                },
                started: !managed,
                quit_after,
                // VS Code gives formatters 750ms by default; a slow server must not hold a save back.
                deadline: std::time::Instant::now()
                    + std::time::Duration::from_millis(if managed { 3000 } else { 1000 }),
            },
        );
        if managed {
            self.start_pending_save_formatters();
        } else {
            self.request_lsp(LspFeature::Formatting {
                tab_size: self.state.policy.tab_width,
                insert_spaces: self.state.policy.expand_tabs,
            });
        }
        true
    }

    pub(in crate::app) fn formatting_save_for_request(
        &self,
        server: &str,
        generation: u64,
    ) -> Option<crate::editor::BufferId> {
        self.format_on_save.values().find_map(|pending| {
            pending
                .request
                .as_ref()
                .filter(|(key, request)| key == server && *request == generation)
                .map(|_| pending.buffer)
        })
    }

    fn start_pending_save_formatters(&mut self) {
        let waiting = self
            .format_on_save
            .values()
            .filter(|pending| !pending.started)
            .filter_map(|pending| Some((pending.buffer, pending.formatter.clone()?)))
            .collect::<Vec<_>>();
        for (buffer, name) in waiting {
            // Task names identify one running formatter. Saves using the same tool wait
            // their turn instead of bypassing formatting or replacing another run.
            if self.tasks.is_running(&name) {
                continue;
            }
            let pending = self.format_on_save.get_mut(&buffer).expect("pending save");
            pending.started = true;
            pending.deadline = Instant::now() + std::time::Duration::from_millis(3000);
            match crate::app::tasks::FormatterSource::capture(&self.state, buffer) {
                Ok(source) => self.run_trusted(TrustedAction::RunFormatter { name, source }),
                Err(error) => {
                    self.show_error(error);
                    self.finish_format_on_save(buffer);
                }
            }
        }
    }

    pub(in crate::app) fn finish_format_on_save(
        &mut self,
        buffer: crate::editor::BufferId,
    ) {
        let Some(pending) = self.format_on_save.remove(&buffer) else {
            return;
        };
        if pending.started
            && let Some(formatter) = &pending.formatter
        {
            self.tasks.stop_formatter(formatter, pending.buffer);
        }
        if let Some((server, generation)) = pending.request {
            let _ = self.language.cancel(&server, generation);
            self.language.retire_request(&server, generation);
        }
        if self
            .state
            .buffers
            .get(pending.buffer)
            .is_some_and(|buffer| {
                buffer.path == pending.path
                    && pending
                        .source
                        .same_incarnation(&buffer.text.source_version())
            })
        {
            match self.request_save_buffer(pending.buffer, pending.quit_after, None, false, None) {
                Ok(_) => self.notify_message(self.message("saving", &[]), false),
                Err(error) => self.show_document_failure(error),
            }
        } else {
            self.show_error(self.message("format_on_save_skipped", &[]));
        }
    }

    /// A commit or stage made outside Dust, such as in its terminal, moves the index or HEAD.
    fn follow_outside_git_changes(&mut self) {
        let Some(status) = self.git.status() else {
            return;
        };
        let Ok(head) = crate::git::git_head_id(&status.repository) else {
            return;
        };
        let metadata = ["index", "HEAD"].map(|name| {
            std::fs::metadata(status.git_dir.join(name))
                .and_then(|file| file.modified())
                .ok()
        });
        let stamp = (metadata, head);
        if self
            .git_stamp
            .replace(stamp)
            .is_some_and(|previous| previous != stamp)
        {
            self.start_git_status_refresh_with_feedback(false);
        }
    }

    /// Once a second, notice open files that changed outside Dust: lazygit, a hunk reset, a
    /// formatter. A clean buffer takes the new text; a changed one keeps its text and says so.
    pub(in crate::app) fn process_disk_changes(&mut self) {
        let now = self.clock.monotonic();
        if self.next_disk_check.is_some_and(|next| now < next) || self.documents.file_work_pending()
        {
            return;
        }
        self.next_disk_check = Some(now + std::time::Duration::from_secs(1));
        self.follow_outside_git_changes();
        let open: Vec<_> = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.virtual_owner().is_none())
            .filter_map(|(id, buffer)| Some((id, buffer.path.clone()?)))
            .collect();
        self.disk_stamps
            .retain(|id, _| open.iter().any(|(open, _)| open == id));
        for (id, path) in open {
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let stamp = (metadata.modified().ok(), metadata.len());
            if !metadata.is_file() {
                continue;
            }
            if self.disk_stamps.get(&id) == Some(&stamp) {
                continue;
            }
            if matches!(
                self.documents.check_admission(),
                Err(crate::files::VfsError::QueueFull)
            ) {
                break;
            }
            let editor = self.state.window_version();
            let workspace = self.workspace.version();
            let view = Self::document_view(id, &self.state.buffers[id]);
            match self.documents.request_disk_check(view, &editor, workspace) {
                Ok(()) => {
                    self.reload_stamps.insert(id, stamp);
                }
                Err(error) => self.show_document_failure(error),
            }
        }
    }

    pub(in crate::app) fn process_format_on_save_deadline(&mut self) {
        let expired = self
            .format_on_save
            .values()
            .filter(|pending| pending.started && Instant::now() >= pending.deadline)
            .map(|pending| pending.buffer)
            .collect::<Vec<_>>();
        for buffer in expired {
            self.finish_format_on_save(buffer);
        }
        self.start_pending_save_formatters();
    }

    pub(in crate::app) fn start_save(
        &mut self,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
    ) {
        let Some(buffer) = self.state.active_buffer_id() else {
            self.show_document_failure(crate::app::documents::DocumentFailure::Policy(
                "no_active_save",
            ));
            return;
        };
        // Said up front, because the write would only report the temporary file it could not make.
        if let Some(requested) = requested_path.as_deref() {
            let folder = requested
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty());
            if let Some(folder) = folder.filter(|f| !self.state.workspace_root.join(f).is_dir()) {
                let name = requested.file_name().unwrap_or_default().to_string_lossy();
                self.show_error(format!(
                    "can't save {name}: the folder {} doesn't exist",
                    folder.display()
                ));
                return;
            }
        }
        match self.request_save_buffer(buffer, quit_after, requested_path, force, None) {
            Ok(_) => self.notify_message(self.message("saving", &[]), false),
            Err(error) => self.show_document_failure(error),
        }
    }
    /// Every changed file, without waiting on formatters. Quitting happens when the last save
    /// lands, and only if nothing is left unsaved by then.
    fn save_all(&mut self, quit_after: bool) {
        let changed: Vec<_> = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| {
                buffer.dirty && buffer.path.is_some() && buffer.virtual_owner().is_none()
            })
            .map(|(id, _)| id)
            .collect();
        let Some(last) = changed.last().copied() else {
            if quit_after {
                self.process_effect(AppEffect::Quit { force: false });
            } else {
                self.notify_message(self.message("save_all_nothing", &[]), false);
            }
            return;
        };
        self.notify_message(self.message("saving", &[]), false);
        for buffer in changed {
            let quit = quit_after && buffer == last;
            if let Some((_, pending_quit)) =
                self.pending_saves.iter_mut().find(|(id, _)| *id == buffer)
            {
                *pending_quit |= quit;
            } else {
                self.pending_saves.push_back((buffer, quit));
            }
        }
        self.process_pending_saves();
    }

    pub(in crate::app) fn process_pending_saves(&mut self) {
        while !self.pending_saves.is_empty() {
            match self.documents.check_admission() {
                Ok(()) => {}
                Err(crate::files::VfsError::QueueFull) => break,
                Err(error) => {
                    self.pending_saves.clear();
                    self.show_error(error);
                    break;
                }
            }
            let (buffer, quit) = self.pending_saves.pop_front().expect("pending save");
            if let Err(error) = self.request_save_buffer(buffer, quit, None, false, None) {
                self.show_document_failure(error);
            }
        }
    }
    pub(in crate::app) fn request_save_buffer(
        &mut self,
        buffer: crate::editor::BufferId,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
        close: Option<EditorCloseSave>,
    ) -> Result<crate::files::RequestId, crate::app::documents::DocumentFailure> {
        let view = self
            .state
            .buffers
            .get(buffer)
            .map(|candidate| Self::document_view(buffer, candidate));
        let request = crate::app::documents::SaveRequest {
            editor: &self.state.window_version(),
            workspace: self.workspace.version(),
            base: &self.state.workspace_root,
            path: requested_path,
            policy: &self.configuration.settings().editor,
            force,
            quit: quit_after,
            close,
            other_open_paths: self
                .state
                .buffers
                .iter()
                .filter(|(id, _)| *id != buffer)
                .filter_map(|(_, candidate)| candidate.path.clone())
                .collect(),
        };
        self.documents.request_save(view, request)
    }
    pub(in crate::app) fn close_active_buffer(&mut self, force: bool) {
        let Some(view) = self.state.active_view else {
            self.set_dashboard_visibility(true);
            return;
        };
        let buffer = self.state.views[view].buffer;
        if !force {
            self.reconcile_editor_windows();
            match self
                .window_session
                .begin_buffer_close(&(&*self.state).into(), view, buffer)
            {
                Ok(step) => self.handle_editor_close_step(step),
                Err(error) => self.show_error(error),
            }
            return;
        }
        self.close_buffer_target(view, buffer);
    }
    pub(in crate::app) fn retire_closed_buffer(
        &mut self,
        buffer_id: crate::editor::BufferId,
        path: Option<&Path>,
        clean: bool,
    ) {
        self.recovery.retire_buffer(buffer_id, clean);
        self.documents.retire_buffer(buffer_id, path);
        if let Some(path) = path
            && let Some(id) = self.explorer.retire_buffer_path(path)
        {
            self.documents.cancel_request(id);
        }
        match self.extension.close_virtual_buffer(buffer_id) {
            Ok(Some(request)) => self.route_virtual_open(Ok(request)),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        let documents = self
            .language
            .sessions()
            .iter()
            .filter_map(|(server, state)| {
                state
                    .documents
                    .get(&buffer_id)
                    .map(|document| (server.clone(), document.uri()))
            })
            .collect::<Vec<_>>();
        self.detach_lsp_documents(buffer_id, documents);
    }
    pub(in crate::app) fn close_buffer_target(
        &mut self,
        view_id: crate::editor::ViewId,
        buffer_id: crate::editor::BufferId,
    ) {
        let clean = self
            .state
            .buffers
            .get(buffer_id)
            .is_some_and(|buffer| !buffer.dirty);
        let closed_path = self
            .state
            .buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.path.clone());
        if let Err(error) = self
            .window_session
            .check_editor_admission(&(&*self.state).into())
        {
            self.show_error(error);
            return;
        }
        let mut candidate = self.state.draft();
        let removed = match candidate.close_document(view_id, buffer_id) {
            Ok(removed) => removed,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.commit_editor_draft(candidate, None, false) {
            self.show_error(error);
            return;
        }
        if removed {
            self.retire_closed_buffer(buffer_id, closed_path.as_deref(), clean);
        }
        self.set_dashboard_visibility(self.state.active_view.is_none());
        if !self.window_session.dashboard_visible() {
            self.sync_lsp_document();
        } else {
            self.language.clear_active();

            self.clear_lsp_ui_projection();
        }
        self.notify_message(
            if self.window_session.dashboard_visible() {
                self.message("returned_dashboard", &[])
            } else {
                self.message("buffer_closed", &[])
            },
            false,
        );
    }
}

impl App {
    pub(in crate::app) fn process_file_reply(
        &mut self,
        reply: crate::app::documents::FileReply,
    ) {
        use crate::app::documents::DocumentChange;
        let stage = self.input.take_stage_save(reply.id());
        let close_save = reply.close_save().cloned();
        if let Err(error) = self
            .window_session
            .check_editor_admission(&(&*self.state).into())
        {
            if let Some(token) = &close_save {
                self.window_session.fail_editor_close_save(token);
            }
            self.show_error(error);
            return;
        }
        if let Err(error) = reply.check_save_destination(
            self.state
                .buffers
                .iter()
                .filter_map(|(id, b)| b.path.as_deref().map(|path| (id, path))),
        ) {
            if let Some(token) = &close_save {
                self.window_session.fail_editor_close_save(token);
            }
            self.show_document_failure(error);
            return;
        }
        let current = self.state.version();
        let view = reply.target().and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|b| Self::document_view(id, b))
        });
        let change = match self
            .documents
            .prepare_file(reply, &current, &self.workspace.version(), view)
            .and_then(|plan| plan.into_change(&current))
        {
            Ok(change) => change,
            Err(e) => {
                if let Some(token) = &close_save {
                    self.window_session.fail_editor_close_save(token);
                }
                self.show_document_failure(e);
                return;
            }
        };
        match change {
            DocumentChange::Open {
                text,
                path,
                line,
                navigation,
                activate,
            } => {
                let mut candidate = self.state.draft();
                let activate = activate
                    && self.window_session.navigation_current(
                        &navigation,
                        &self.state.window_version(),
                        self.state.active_view,
                    );
                if activate {
                    if let Err(error) = candidate.open_document(text, path.clone()) {
                        self.show_error(error);
                        return;
                    }
                } else {
                    candidate.load_document(text, path.clone());
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, activate) {
                    self.show_error(e);
                    return;
                }
                if activate && let (Some(line), Some(view)) = (line, self.state.active_view) {
                    let text = &self.state.buffers[self.state.views[view].buffer].text;
                    let position = text
                        .line_to_byte(line.min(text.visible_lines().saturating_sub(1)))
                        .unwrap_or(ByteOffset(0));
                    let transaction = EditorTransaction::new(self.transaction_id())
                        .with_operation(EditorOperation::SetCursor { view, position })
                        .with_undo_policy(UndoPolicy::Ignore);
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error);
                    }
                    self.ensure_cursor_visible();
                }
                self.record_recent_file(path.clone());
                // "Opening file…" is done once the file is there.
                if self.ui.notification.as_deref()
                    == Some(self.message("opening_file", &[]).as_str())
                {
                    self.clear_notice();
                }
            }
            DocumentChange::DiskChecked {
                buffer,
                path,
                changed,
            } => {
                if let Some(stamp) = self.reload_stamps.remove(&buffer) {
                    self.disk_stamps.insert(buffer, stamp);
                }
                if changed {
                    let shown = self.workspace_relative_label(&path);
                    self.notify_warning(self.message("file_changed_on_disk", &[("path", shown)]));
                }
            }
            DocumentChange::Reload { buffer, text, path } => {
                let mut candidate = self.state.draft();
                let stamp = self.reload_stamps.remove(&buffer);
                if !candidate.reload_document(buffer, text, path.clone()) {
                    self.show_error(self.message("reload_closed", &[]));
                    return;
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(e);
                    return;
                }
                if let Some(stamp) = stamp {
                    self.disk_stamps.insert(buffer, stamp);
                }
                self.git.forget_hunks();
                self.start_git_status_refresh_with_feedback(false);
                let views = self
                    .state
                    .views
                    .iter()
                    .filter(|(_, view)| view.buffer == buffer)
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>();
                for view in views {
                    self.keep_view_cursor_off_final_newline(view);
                }
                self.notify_message(
                    self.message("reloaded_file", &[("path", self.notice_file_label(&path))]),
                    false,
                );
            }
            DocumentChange::Saved {
                buffer,
                text,
                revision,
                bytes,
                snapshot,
                line_ending,
                target: path,
                quit: quit_after,
                close,
            } => {
                let mut candidate = self.state.draft();
                if !candidate.save_document(buffer, &text, revision, bytes, path.clone(), *snapshot)
                {
                    self.input.clear_pending_stage();
                    if let Some(token) = &close {
                        self.window_session.fail_editor_close_save(token);
                    }
                    return;
                }
                if let Some(ending) = line_ending {
                    candidate.set_document_line_ending(buffer, ending);
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, false) {
                    self.input.clear_pending_stage();
                    if let Some(token) = &close {
                        self.window_session.fail_editor_close_save(token);
                    }
                    self.show_error(e);
                    return;
                }
                // A staged save that was waiting on this completion replays now, against the
                // saved bytes, using the exact effect that was refused.
                let buffer_state = &self.state.buffers[buffer];
                let still_dirty = buffer_state.dirty;
                if let (Some(directory), false) = (&self.state.undo_directory, still_dirty) {
                    self.persistence.track_undo_save(
                        path.clone(),
                        crate::editor::save_undo_history(
                            directory.clone(),
                            path.clone(),
                            buffer_state.text.clone(),
                            buffer_state.undo.clone(),
                            self.state.undo_maximum_bytes,
                        ),
                    );
                }
                let lsp_document = buffer_state.path.clone().and_then(|path| {
                    Some(LspDocument {
                        language_id: language_id_for_path(&path)?.into(),
                        path,
                        version: revision.0,
                        text: crate::editor::text::decode_utf8(buffer_state.persisted_bytes())
                            .ok()?
                            .text,
                    })
                });
                self.git.forget_hunks();
                self.notify_message(
                    if still_dirty {
                        self.message(
                            "saved_dirty",
                            &[
                                ("revision", revision.0.to_string()),
                                ("path", self.notice_file_label(&path)),
                            ],
                        )
                    } else {
                        self.message("saved", &[("path", self.notice_file_label(&path))])
                    },
                    false,
                );
                if let Some(document) = lsp_document {
                    let current = LspDocument {
                        text: self.state.buffers[buffer].text.to_string(),
                        version: self.state.buffers[buffer].text.revision().0,
                        ..document.clone()
                    };
                    let source = self.state.buffers[buffer].text.source_version();
                    let servers = self
                        .language
                        .sessions()
                        .iter()
                        .filter_map(|(key, state)| {
                            state
                                .documents
                                .get(&buffer)
                                .filter(|owned| owned.uri() == document.uri())
                                .map(|_| key.clone())
                        })
                        .collect::<Vec<_>>();
                    for server in servers {
                        // Formatting and save policies can change text since the last sync.
                        // Publish the current buffer before didSave, never the older disk snapshot.
                        if !self.language.synced(&server, buffer, &source)
                            && let Err(error) = self.language.sync_owned(
                                &server,
                                current.clone(),
                                (buffer, source.clone()),
                            )
                        {
                            self.show_error(error);
                        }
                        if let Err(error) = self.language.save(&server, document.clone()) {
                            self.show_error(error);
                        }
                    }
                }
                if let Some((effect, line)) = stage.filter(|_| !still_dirty) {
                    self.retry_saved_stage(buffer, &path, effect, line);
                } else {
                    self.start_git_status_refresh_with_feedback(false);
                }
                if let Some(token) = close {
                    if still_dirty {
                        self.window_session.fail_editor_close_save(&token);
                        self.show_error(
                            "document changed while its close save was pending; close was cancelled",
                        );
                    } else {
                        match self
                            .window_session
                            .finish_editor_close_save(&token, &(&*self.state).into())
                        {
                            Ok(step) => self.handle_editor_close_step(step),
                            Err(error) => self.show_error(error),
                        }
                    }
                }
                if quit_after && !still_dirty {
                    if self
                        .state
                        .buffers
                        .iter()
                        .any(|(_, candidate)| candidate.dirty)
                    {
                        self.show_error(self.message("saved_others_dirty", &[]));
                    } else {
                        self.should_quit = true;
                    }
                }
            }
        }
    }
}

use crate::app::workspace::WorkspaceFailure;

use crate::app::workspace::WorkspaceUpdate;
impl App {
    pub(in crate::app) fn show_workspace_failure(&mut self, failure: WorkspaceFailure) {
        let message = match failure {
            WorkspaceFailure::Message(key) => self.message(key, &[]),
            WorkspaceFailure::Diagnostic(key, diagnostic) => {
                self.message(key, &[("diagnostic", diagnostic)])
            }
            WorkspaceFailure::Plain(message) => message,
        };
        self.show_error(message);
    }
    /// Run a project-defined command if the workspace is trusted.
    pub(in crate::app) fn run_trusted(&mut self, action: TrustedAction) {
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        match action {
            TrustedAction::StartDebug(intent) => self.authorize_debug_launch(intent),
            TrustedAction::InstallRegistry {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                true,
            ),
            TrustedAction::PrepareRegistryArtifact => {
                if let Err(error) = self.extension.prepare_artifact(self.plugin_limits()) {
                    self.show_error(error);
                }
            }
            TrustedAction::InstallPackage {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => self.install_package_after_validation(
                package_manifest,
                plugin_manifest,
                component,
                update,
            ),
            // A task with a matcher reports what it matched in Problems, as a VS Code task does.
            TrustedAction::RunTask(name) => {
                let adapter = match self.configuration.settings().tasks.get(&name) {
                    Some(task) => task
                        .matcher
                        .clone()
                        .map_or(AdapterContext::Plain, |matcher| AdapterContext::Linter {
                            matcher,
                            lint: task.kind == crate::config::TaskKind::Linter,
                        }),
                    None => AdapterContext::Plain,
                };
                self.start_task_plan(&name, adapter)
            }
            TrustedAction::OpenTerminal(intent) => self.open_terminal_with_response(intent, None),
            TrustedAction::OpenDapTerminal { intent, reply } => {
                self.open_terminal_with_response(intent, Some(reply))
            }
            TrustedAction::RunTest { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Test { matcher })
            }
            TrustedAction::RunLinter { name, matcher } => self.start_task_plan(
                &name,
                AdapterContext::Linter {
                    matcher,
                    lint: true,
                },
            ),
            TrustedAction::RunFormatter { name, source } => {
                self.start_task_plan(&name, AdapterContext::Formatter { source })
            }
        }
    }

    #[cfg(test)]
    pub(in crate::app) fn reconcile_workspace_trust(&mut self) {
        self.workspace.reconcile_trust();
    }
    pub(in crate::app) fn process_trust_events(&mut self) {
        if let Some((old, new)) = self.workspace.pending_root_switch() {
            let dirty = self.state.buffers.values().any(|buffer| {
                buffer.dirty
                    && buffer
                        .path
                        .as_deref()
                        .is_some_and(|file| leaves_root(file, old, new))
            });
            let new_terminal = self
                .terminal_session
                .ids_in_root(old)
                .iter()
                .any(|id| !self.switch_approved_terminals.contains(id));
            if dirty || new_terminal {
                self.workspace.cancel_root_switch();
                self.switch_approved_terminals.clear();
                self.show_error(if dirty {
                    "Workspace switch cancelled: save or close the unsaved files first"
                } else {
                    "Workspace switch cancelled: new terminals opened; switch again to confirm"
                });
            }
        }
        let before = self.workspace.version();
        let updates = self.workspace.poll(&self.state);
        if before != self.workspace.version() {
            self.documents.retire_workspace(&self.workspace.version());
        }
        for event in updates {
            match event {
                WorkspaceUpdate::Failure(e) => self.show_workspace_failure(e),
                WorkspaceUpdate::Message(key) => self.notify_message(self.message(key, &[]), false),
                WorkspaceUpdate::RootAdded(path) => {
                    self.invalidate_root_scoped_operations();
                    // A new root becomes the active one; the statusline and relative paths follow.
                    self.clear_git_status();
                    self.state.set_workspace_root(path.clone());
                    self.notify_message(
                        self.message(
                            "workspace_root_added",
                            &[("path", path.display().to_string())],
                        ),
                        false,
                    );
                }
                WorkspaceUpdate::RootSwitched { path, old } => self.finish_root_switch(path, &old),
            }
        }
    }
    pub(in crate::app) fn maybe_prompt_for_unseen_workspace(&mut self) {
        // Never open under the user's fingers: keys typed a moment ago would answer it.
        let typing = self
            .last_key_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(800));
        if self.workspace.should_prompt(
            typing
                || self.input.prompt().is_some()
                || self.ui.picker.is_some()
                || self.ui.tool_menu.is_some()
                || self.ui.settings.is_some(),
        ) {
            self.request_workspace_trust();
        }
    }
    pub(in crate::app) fn request_workspace_trust(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        match self.workspace.request_confirmation() {
            Ok(()) => {
                self.clear_notice();
                self.open_confirmation_actions(
                    PromptKind::WorkspaceTrust,
                    self.configuration
                        .settings()
                        .ui
                        .command_line
                        .text
                        .workspace_trust_title
                        .clone(),
                    self.message("workspace_trust_confirmation", &[]),
                    crate::ui::ConfirmationChoice::Yes,
                    "Trust",
                    "Not now",
                );
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
    pub(in crate::app) fn confirm_workspace_trust(&mut self) {
        match self.workspace.confirm() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
    pub(in crate::app) fn revoke_workspace_trust(&mut self) {
        match self.workspace.revoke() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

/// Create a user settings or keymaps file if it is missing or empty. A new file starts with a
/// few commented examples rather than nothing.
pub(super) fn create_user_file(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    let starter = if path.file_name().is_some_and(|name| name == "keymaps.toml") {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/defaults/keymaps-starter.toml"))
    } else {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/defaults/settings-starter.toml"))
    };
    if std::fs::metadata(path)?.len() == 0 {
        std::fs::write(path, starter)?;
    }
    Ok(())

}

impl App {


    pub(in crate::app) fn workspace_relative_label(&self, path: &std::path::Path) -> String {
        self.workspace
            .roots()
            .iter()
            .find_map(|root| path.strip_prefix(&root.identity.canonical_path).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    }
}
