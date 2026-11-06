use crate::app::search::MAXIMUM_PICKER_ITEMS;
use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::app::DockEdge;
use crate::app::ExplorerKind;
use crate::app::ExplorerMutation;
use crate::app::App;
use crate::app::NativeSurfaceTarget;
use crate::app::Path;
use crate::app::PathBuf;
use crate::app::PromptKind;
use crate::app::TextBuffer;
use crate::app::WindowRect;
use crate::app::RegisterShape;
use crate::app::RegisterValue;
use crate::app::rect_contains_point;
use crate::windows::PANE_DEFAULT_RATIO;
use crate::app::windows::rect_contains_point;
use crate::editor::RegisterShape;
use crate::editor::RegisterValue;
use crate::ui::ExplorerUiState;
use crate::app::explorer::tree::ExplorerKind;
use crate::app::explorer::tree::ExplorerLoadState;
use crate::app::explorer::tree::ExplorerNodeId;



impl App {

    pub(in crate::app) fn explorer_open_file(&mut self, split: bool) {
        if self
            .explorer
            .tree()
            .selected_node()
            .is_some_and(|node| matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory))
        {
            self.explorer_toggle();
            return;
        }
        if let Err(error) = self.documents.check_admission() {
            self.show_error(error);
            return;
        }
        let request = self.explorer.prepare_open(
            split,
            (
                self.workspace.version(),
                self.configuration.binding_revision(),
            ),
            self.state.window_version(),
            self.workspace.roots(),
        );
        let (retired, request) = match request {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Some(id) = retired {
            self.documents.cancel_request(id);
        }
        match self.documents.open_explorer(request.path, request.roots) {
            Ok(id) => {
                self.set_explorer_focus(false);
                // Finish the requested focus change before capturing its async
                // navigation guard; a redraw must not look like new navigation.
                self.reconcile_editor_windows();
                let navigation = self
                    .window_session
                    .navigation_stamp(self.state.window_version(), self.state.active_view);
                if !self.explorer.admit_open(request.ticket, id, navigation) {
                    self.documents.cancel_request(id);
                }
            }
            Err(error) => {
                self.explorer.fail_open(request.ticket);
                self.show_error(error);
            }
        }
    }
}

impl App {

    pub(in crate::app) fn cancel_explorer_confirmation(&mut self) {
        if let Some(id) = self.explorer.cancel_confirmation() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {

    pub(in crate::app) fn retire_stale_explorer_intents(&mut self) {
        let ids = self.explorer.retire_stale_intents(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        for id in ids {
            self.documents.cancel_request(id);
        }
        if self
            .input
            .prompt()
            .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
            && !self.explorer.rename_current(
                &self.workspace.version(),
                self.configuration.binding_revision(),
                &self.state.window_version(),
            )
        {
            self.close_prompt();
        }
        if !self.explorer.confirmation_pending()
            && self
                .input
                .prompt()
                .is_some_and(|p| p.kind == PromptKind::ExplorerDelete)
        {
            self.close_prompt();
        }
    }
}

impl App {

    pub(in crate::app) fn retire_explorer_intents(&mut self) {
        for id in self.explorer.retire_intents() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {

    fn queue_explorer_mutation(
        &mut self,
        operation: Result<ExplorerMutation, String>,
        confirmation: bool,
        rename_prompt: bool,
    ) -> Result<(), String> {
        let operation = operation?;
        if self.explorer.mutation_pending() {
            return Err("an explorer operation is already pending".into());
        }
        self.documents
            .check_admission()
            .map_err(|error| error.to_string())?;
        let change = match &operation {
            ExplorerMutation::Rename { source, .. } | ExplorerMutation::Delete { path: source } => {
                Some(self.state.capture_path_change(source)?)
            }
            _ => None,
        };
        let context = crate::app::explorer::MutationContext {
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
            editor: self.state.window_version(),
            change,
        };
        let request = self.explorer.prepare_mutation(
            operation,
            context,
            self.workspace.roots(),
            SEARCH_MAXIMUM_FILE_BYTES as u64,
            confirmation,
        )?;
        let result = if rename_prompt {
            let source = self.explorer.submitted_rename_source();
            let target = request
                .operation
                .target()
                .expect("validated rename target")
                .into();
            self.documents.prepare_explorer_rename(source, target)
        } else {
            self.documents
                .prepare_explorer(request.operation, request.roots, request.maximum)
        }
        .map_err(|e| e.to_string());
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.admit_mutation(request.ticket, result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            return Err(error);
        }
        self.notify_message("checking explorer operation…".into(), false);
        Ok(())
    }
}

impl App {

    pub(in crate::app) fn create_explorer_path(&mut self, input: String) {
        if let Err(error) = self.queue_explorer_mutation(self.explorer.create(&input), false, false)
        {
            self.show_error(error);
        }
    }
}

impl App {

    pub(in crate::app) fn rename_explorer_path(&mut self, target: String) {
        if let Err(error) =
            self.queue_explorer_mutation(self.explorer.rename(&target), false, false)
        {
            self.show_error(error);
        }
    }
}

impl App {

    pub(in crate::app) fn open_explorer_rename_prompt(&mut self) {
        self.close_prompt();
        let request = match self.explorer.begin_rename(
            self.workspace.version(),
            self.configuration.binding_revision(),
            self.state.window_version(),
            self.workspace.roots(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let result = self.explorer.rename_inspection().and_then(|inspection| {
            let id = self
                .documents
                .inspect_explorer_rename(
                    request.path,
                    request.roots,
                    SEARCH_MAXIMUM_FILE_BYTES as u64,
                )
                .map_err(|error| error.to_string())?;
            inspection.register(id);
            Ok(())
        });
        if let Err(error) = result {
            self.explorer.cancel_rename();
            self.show_error(error);
            return;
        }
        self.open_prompt(PromptKind::ExplorerRename, request.initial);
        self.prompt_help(format!(
            "Relative to `{}`. Checking source… Enter renames; Esc cancels.",
            request.root.display()
        ));
    }
}

impl App {

    pub(in crate::app) fn submit_explorer_rename_prompt(&mut self, value: &str) {
        let operation = self.explorer.rename_value(
            value,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        if let Err(error) = self.queue_explorer_mutation(operation, false, true) {
            self.prompt_error(error);
        } else {
            self.prompt_help("Checking rename target… Esc cancels before application.".into());
        }
    }
}

impl App {

    pub(in crate::app) fn cancel_explorer_rename_prompt(&mut self) {
        for id in self.explorer.cancel_rename() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {

    pub(in crate::app) fn request_explorer_delete(&mut self) {
        self.close_prompt();
        if let Err(error) = self.queue_explorer_mutation(self.explorer.delete(), true, false) {
            self.show_error(error);
        }
    }
}

impl App {

    pub(in crate::app) fn confirm_explorer_delete(&mut self) {
        match self.explorer.confirm_delete(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) {
            Ok(prepared) => self.apply_prepared_explorer(prepared),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {

    pub(in crate::app) fn apply_prepared_explorer(
        &mut self,
        prepared: crate::files::PreparedExplorerMutation,
    ) {
        let result = (|| {
            if let ExplorerMutation::Rename { source, target } = prepared.operation() {
                self.state.check_rename_destination(source, target)?;
            }
            self.documents
                .apply_explorer(prepared)
                .map_err(|e| e.to_string())
        })();
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.admit_apply(result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            self.show_error(error);
        }
    }
}

impl App {

    pub(in crate::app) fn set_explorer_clipboard(&mut self, cut: bool) {
        match self.explorer.clipboard(cut, self.workspace.version()) {
            Ok(message) => self.notify_message(message, false),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {

    pub(in crate::app) fn paste_explorer_clipboard(&mut self) {
        if let Err(error) = self.queue_explorer_mutation(
            self.explorer.paste(&self.workspace.version()),
            false,
            false,
        ) {
            self.show_error(error);
        }
    }
}

impl App {

    pub(in crate::app) fn process_explorer_io(
        &mut self,
        reply: crate::files::ExplorerIoReply,
    ) {
        use crate::app::explorer::MutationUpdate;
        if let crate::files::ExplorerIoReply::RenameInspected { id, result } = reply {
            match self.explorer.accept_rename_inspection(
                id,
                result,
                self.workspace.version(),
                self.configuration.binding_revision(),
                &self.state.window_version(),
            ) {
                Some(Ok(root)) => self.prompt_help(format!(
                    "Relative to `{}`. Enter renames; Esc cancels.",
                    root.display()
                )),
                Some(Err(error)) => self.prompt_error(error),
                None => {}
            }
            return;
        }
        if let crate::files::ExplorerIoReply::Opened { id, path, result } = reply {
            let Some(reply) = self.explorer.accept_open(
                id,
                &path,
                (
                    self.workspace.version(),
                    self.configuration.binding_revision(),
                ),
                &self.state.window_version(),
            ) else {
                return;
            };
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            if let Err(error) = self
                .window_session
                .check_editor_admission(&(&*self.state).into())
            {
                self.show_error(error);
                return;
            }
            let text = match result
                .map_err(|e| e.to_string())
                .and_then(|bytes| TextBuffer::from_bytes(&bytes).map_err(|e| e.to_string()))
            {
                Ok(text) => text,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            let activate = self.window_session.navigation_current(
                &reply.navigation,
                &self.state.window_version(),
                self.state.active_view,
            );
            let mut draft = self.state.draft();
            if activate {
                if reply.split
                    && self.state.active_view.is_some()
                    && let Err(error) = draft.split_document(PANE_DEFAULT_RATIO)
                {
                    self.show_error(error);
                    return;
                }
                if let Err(error) = draft.open_document(text, path) {
                    self.show_error(error);
                    return;
                }
            } else {
                draft.load_document(text, path);
            }
            if let Err(error) = self.commit_editor_draft(draft, None, activate) {
                self.show_error(error);
                return;
            }
            if activate {
                self.set_dashboard_visibility(false);
                self.ensure_cursor_visible();
            }
            return;
        }
        let Some(update) = self.explorer.accept_mutation(
            reply,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) else {
            return;
        };
        match update {
            MutationUpdate::Confirmation(path) => {
                self.open_confirmation(
                    PromptKind::ExplorerDelete,
                    self.configuration
                        .settings()
                        .ui
                        .command_line
                        .text
                        .explorer_delete_title
                        .clone(),
                    explorer_delete_message(&path),
                    crate::ui::ConfirmationChoice::No,
                );
            }
            MutationUpdate::Apply(prepared) => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.apply_prepared_explorer(prepared);
            }
            MutationUpdate::Failed(error) => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.prompt_error(error);
                } else {
                    self.show_error(error);
                }
            }
            MutationUpdate::Complete { operation, change } => {
                let reconciliation = (|| -> Result<(), String> {
                    self.window_session
                        .check_editor_admission(&(&*self.state).into())?;
                    if let Some(change) = change {
                        let target = match &operation {
                            ExplorerMutation::Rename { target, .. } => Some(target.as_path()),
                            _ => None,
                        };
                        let (draft, recent) = self.state.reconcile_paths(change, target)?;
                        if let Some(draft) = draft {
                            self.commit_editor_draft(draft, None, false)?;
                        }
                        for path in recent {
                            self.record_recent_file(path);
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = reconciliation {
                    self.show_error(error);
                } else {
                    self.notify_message(
                        format!(
                            "explorer operation complete: {}",
                            operation
                                .target()
                                .or_else(|| operation.source())
                                .map_or_else(
                                    || "path".into(),
                                    |path| self.workspace_relative_label(path)
                                )
                        ),
                        false,
                    );
                }
                self.refresh_explorer();
                self.start_git_status_refresh_silent();
            }
        }
    }
}



fn explorer_delete_message(path: &std::path::Path) -> String {
    format!("Permanently delete {path:?}?")
}


#[cfg(all(test, unix))]
#[test]
fn explorer_delete_message_uses_truthful_path_debug_escaping() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    assert_eq!(
        explorer_delete_message(std::path::Path::new("a  界\n\\b")),
        "Permanently delete \"a  界\\n\\\\b\"?"
    );
    let raw = PathBuf::from(OsString::from_vec(b"raw-\xff".to_vec()));
    assert_eq!(
        explorer_delete_message(&raw),
        "Permanently delete \"raw-\\xFF\"?"
    );
}
impl App {

    pub(in crate::app) fn move_explorer_selection(&mut self, delta: isize) {
        self.explorer.navigate(delta);
        self.sync_explorer_ui();
    }
}

impl App {


    pub(in crate::app) fn page_explorer_selection(&mut self, direction: isize) {
        let viewport = self
            .window_session
            .focused_explorer()
            .and_then(|view| self.window_session.native_ids().explorer_view(view))
            .and_then(|ids| self.window_session.layout()?.projection.window(ids.window))
            .map(|window| crate::ui::render::explorer::explorer_body_geometry(window.rect))
            .or(self.window_session.content().explorer)
            .map_or(0, |body| usize::from(body.height));
        self.move_explorer_selection(
            direction.saturating_mul(isize::try_from(viewport).unwrap_or(isize::MAX)),
        );
    }
}

impl App {


    pub(in crate::app) fn explorer_collapse_or_parent(&mut self) {
        self.explorer.parent();
        self.sync_explorer_ui();
    }
}

impl App {


    // EXPLORER_WATCH_FOREGROUND_BEGIN: typed event coordination only.

    pub(in crate::app) fn reconcile_explorer_watch(&mut self) {
        if self.ui.explorer.is_none() {
            self.retire_explorer_intents();
        }
        if let Err(error) = self.explorer.reconcile_watch(self.ui.explorer.is_some()) {
            self.show_error(error);
        }
    }
}

impl App {


    #[cfg(test)]
    pub(in crate::app) fn accept_explorer_watch_batch(
        &mut self,
        batch: crate::files::ExplorerWatchBatch,
    ) {
        self.explorer
            .accept_watch(batch, self.ui.explorer.is_some());
    }
}

impl App {


    pub(in crate::app) fn process_explorer_watch_events(&mut self) {
        if let Err(error) = self.explorer.poll_watch(self.ui.explorer.is_some()) {
            self.show_error(error);
        }
        self.start_next_explorer_watch_reload();
    }
}

impl App {


    pub(in crate::app) fn start_next_explorer_watch_reload(&mut self) {
        if let Some(request) = self.explorer.next_reload() {
            self.load_explorer_request(request);
        }
    }
}

impl App {


    // EXPLORER_WATCH_FOREGROUND_END

    pub(in crate::app) fn sync_explorer_ui(&mut self) {
        self.refresh_window_content();
        if self.ui.explorer.is_none() {
            return;
        }
        let title = self.message("explorer_title", &[]);
        let root_label = crate::ui::workspace_title(&self.state);
        let explorer_windows = self
            .window_session
            .native_ids()
            .explorer_views()
            .filter_map(|(view, ids)| {
                let rect = self
                    .window_session
                    .layout()?
                    .projection
                    .window(ids.window)?
                    .rect;
                Some((view, ids.window, rect))
            })
            .collect::<Vec<_>>();
        let focused = self.window_session.focused_explorer();
        let policy = &self.configuration.settings().ui.explorer;
        let decorations = self.git.decorations();
        let mut primary = None;
        let mut clones = Vec::new();
        for (view, window, rect) in explorer_windows {
            let body = if view == 1 {
                self.window_session
                    .content()
                    .explorer
                    .unwrap_or_else(|| crate::ui::render::explorer::explorer_body_geometry(rect))
            } else {
                crate::ui::render::explorer::explorer_body_geometry(rect)
            };
            let Ok(projection) = self.explorer.projection_for(
                crate::app::explorer::ExplorerViewId(view),
                policy,
                body,
                decorations,
                title.clone(),
                &root_label,
            ) else {
                continue;
            };
            if view == 1 {
                primary = Some(projection);
            } else {
                clones.push(crate::ui::ExplorerWindowUiState {
                    view,
                    window,
                    content: projection,
                    focused: focused == Some(view),
                });
            }
        }
        if primary.is_none() {
            let body = self.ui.surface_content.explorer.unwrap_or(WindowRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            });
            primary = self
                .explorer
                .projection_for(
                    crate::app::explorer::PRIMARY_EXPLORER_VIEW,
                    policy,
                    body,
                    decorations,
                    title,
                    &root_label,
                )
                .ok();
        }
        self.ui.explorer = primary;
        self.ui.explorer_views = clones;
        self.reconcile_explorer_watch();
        self.reconcile_editor_windows();
    }
}

impl App {


    pub(in crate::app) fn open_explorer(&mut self) {
        self.window_session.allow_explorer();
        if self.explorer.tree().root_revision() != self.workspace.root_revision()
            || self.explorer.tree().visible_ids().is_empty()
        {
            self.explorer
                .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        }
        self.ui.explorer = Some(ExplorerUiState {
            title: String::new(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            matches: 0,
            query: String::new(),
            searching: false,
        });
        self.set_explorer_focus(true);
        self.sync_explorer_ui();
        self.git.retry_status();
        self.start_git_status_refresh_silent();
        let root_to_load = self.explorer.tree().selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root_to_load {
            self.load_explorer_node(root);
        }
        self.ensure_cursor_visible();
    }
}

impl App {


    pub(in crate::app) fn load_explorer_node(&mut self, node: ExplorerNodeId) {
        match self.explorer.load(
            node,
            self.workspace.version(),
            self.configuration.binding_revision(),
            MAXIMUM_PICKER_ITEMS,
        ) {
            Ok(()) => self.sync_explorer_ui(),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    fn load_explorer_request(&mut self, request: crate::app::explorer::DirectoryLoad) {
        match self.explorer.load_request(
            request,
            self.workspace.version(),
            self.configuration.binding_revision(),
            MAXIMUM_PICKER_ITEMS,
        ) {
            Ok(()) => self.sync_explorer_ui(),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    pub(in crate::app) fn follow_active_file_in_explorer(&mut self) {
        let Some(path) = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.clone())
        else {
            self.show_error("active buffer has no file path");
            return;
        };
        if self.workspace.root_for_canonical_path(&path).is_none() {
            self.show_error("active file is outside workspace roots");
            return;
        }
        self.open_explorer();
        if self.ui.explorer.is_some() {
            self.explorer.follow(path);
            self.continue_explorer_follow();
        }
    }
}

impl App {


    pub(in crate::app) fn continue_explorer_follow(&mut self) {
        match self.explorer.continue_follow() {
            Ok(Some(request)) => self.load_explorer_request(request),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        self.sync_explorer_ui();
    }
}

impl App {


    /// Filter the tree by `query`; the folders not read yet are read in the background so
    /// the whole project is searched.
    pub(in crate::app) fn search_explorer(&mut self, query: String) {
        self.explorer.set_search(query);
        self.start_next_explorer_watch_reload();
        self.sync_explorer_ui();
    }
}

impl App {


    pub(in crate::app) fn explorer_toggle(&mut self) {
        let Some(node) = self.explorer.tree().selected_node().cloned() else {
            return;
        };
        match node.kind {
            ExplorerKind::Root | ExplorerKind::Directory => match node.load {
                ExplorerLoadState::Loaded => {
                    self.explorer.toggle_loaded(node.id);
                    self.sync_explorer_ui();
                }
                _ => self.load_explorer_node(node.id),
            },
            ExplorerKind::File | ExplorerKind::Symlink => self.explorer_open_file(false),
        }
    }
}

impl App {


    pub(in crate::app) fn process_explorer_result(&mut self) {
        let Some(result) = self.explorer.poll(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        if let Err(error) = result {
            self.show_error(error);
        }
        self.explorer.select_search_match();
        self.sync_explorer_ui();
        self.continue_explorer_follow();
        self.start_next_explorer_watch_reload();
    }
}

impl App {


    pub(in crate::app) fn yank_explorer_path(&mut self) {
        let Some(node) = self.explorer.tree().selected_node() else {
            self.show_error("select an explorer path first");
            return;
        };
        let node_path = node.path.clone();
        let text = node_path.display().to_string();
        let policy = self.state.policy;
        self.state.registers.set(
            '"',
            RegisterValue {
                text: text.clone(),
                shape: RegisterShape::Character,
            },
            policy.registers_enabled,
            policy.mirror_named_to_unnamed,
        );
        self.state.mark_editor_changed();
        let label = self.workspace_relative_label(&node_path);
        self.notify_message(format!("yanked `{label}`"), false);
    }
}

impl App {


    pub(in crate::app) fn explorer_mouse_select(&mut self) {
        let Some(id) = self.explorer.take_clicked() else {
            return;
        };
        self.explorer.select(id);
        if self.window_session.focused_explorer().unwrap_or(1) == 1 {
            self.set_explorer_focus(true);
        }
        self.sync_explorer_ui();
        // As in neo-tree and VS Code, a click opens a file and toggles a folder.
        self.explorer_open_file(false);
    }
}

impl App {


    /// The tree row under the pointer in the layout now on screen.
    pub(in crate::app) fn explorer_row_at(
        &self,
        window: crate::windows::WindowId,
        column: u16,
        row: u16,
    ) -> Option<ExplorerNodeId> {
        let view = match self.window_session.native_ids().target_for_window(window)? {
            NativeSurfaceTarget::Explorer => 1,
            NativeSurfaceTarget::ExplorerView(view) => view,
            _ => return None,
        };
        let rect = self.compositor_projection()?.window(window)?.rect;
        let body = crate::ui::render::explorer::explorer_body_geometry(rect);
        if !rect_contains_point(body, column, row) {
            return None;
        }
        let explorer_ui = if view == 1 {
            self.ui.explorer.as_ref()
        } else {
            self.ui
                .explorer_views
                .iter()
                .find(|candidate| candidate.view == view)
                .map(|candidate| &candidate.content)
        };
        let index = usize::from(row.saturating_sub(body.y));
        explorer_ui?
            .rows
            .get(index)
            .map(|row| ExplorerNodeId(row.node_id))
    }
}

impl App {


    pub(in crate::app) fn refresh_explorer(&mut self) {
        self.retire_explorer_intents();
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.sync_explorer_ui();
        let root = self.explorer.tree().selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root {
            self.load_explorer_node(root);
        }
    }
}

impl App {

    pub(in crate::app) fn explorer_edge(&self) -> DockEdge {
        match self.configuration.settings().ui.explorer.side {
            crate::config::PanelSide::Left => DockEdge::Left,
            crate::config::PanelSide::Right => DockEdge::Right,
        }
    }
}
