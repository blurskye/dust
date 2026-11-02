//! Restricted driver for migrated input/configuration workflows. It deliberately
//! offers no mutable application, editor, configuration, registry or UI access.
use super::InteractiveApplication;
use crate::{
    clock::{Clock, ManualClock},
    config::LoadedConfig,
    editor::StartupTarget,
    ui::{KeyInputKind, UiEvent, UiState},
};
use std::{
    sync::Arc,
    time::{Duration, Instant, UNIX_EPOCH},
};
use tempfile::TempDir;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TerminalCloneState {
    graph: crate::ui::window::WindowGraph,
    editor: crate::editor::EditorWindowVersion,
    descriptions: Vec<(u64, String, crate::ui::window::ContentLifecycle)>,
    selected: Option<crate::process::PtyId>,
    focused: Option<u64>,
    dimensions: Option<(usize, usize)>,
    screen: Vec<String>,
    pending: usize,
    starts: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PluginCloneState {
    graph: String,
    editor: crate::editor::EditorWindowVersion,
    catalog: crate::extensions::PluginCatalogVersion,
    host: String,
    retained: Vec<crate::ui::PluginSurfaceUiState>,
    private: Vec<crate::ui::PrivateSurfaceUiState>,
    copies: (usize, usize, usize),
}

pub(super) struct TestSession {
    application: InteractiveApplication,
    clock: Arc<ManualClock>,
    _workspace: TempDir,
    _state: TempDir,
}
impl TestSession {
    pub(super) fn new(mut loaded: LoadedConfig) -> Self {
        loaded.config.runtime.vfs_queue_capacity = 2;
        loaded
            .config
            .validate()
            .expect("valid initial fixture policy");
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(UNIX_EPOCH, Instant::now()));
        let mut application = InteractiveApplication::new(
            StartupTarget::Dashboard {
                root: workspace.path().to_path_buf(),
            },
            loaded,
            false,
            state.path().to_path_buf(),
            false,
            false,
            clock.clone(),
        )
        .unwrap();
        application.handle_event(UiEvent::Resize(80, 24));
        Self {
            application,
            clock,
            _workspace: workspace,
            _state: state,
        }
    }
    pub(super) fn new_buffer(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::NewBuffer);
    }
    pub(super) fn new_tab(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::Layout(
                crate::editor::LayoutAction::NewTab,
            ));
    }
    pub(super) fn window_list(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::WindowList);
    }
    pub(super) fn focused_window(&self) -> Option<crate::ui::window::WindowId> {
        self.application.window_session.graph().focused()
    }
    pub(super) fn active_tab(&self) -> Option<crate::editor::TabId> {
        self.application.state.active_tab
    }
    pub(super) fn reconcile_windows(&mut self) {
        self.application.reconcile_editor_windows();
    }
    pub(super) fn copy_counts(&self) -> (usize, usize, usize) {
        (
            crate::ui::window::WindowGraph::test_clone_count(),
            crate::editor::session::extensions::ExtensionSession::fixture_window_clone_count(),
            self.application.window_session.content_builds(),
        )
    }
    pub(super) fn graph_revision(&self) -> u64 {
        self.application.window_session.graph().revision().unwrap()
    }
    pub(super) fn window_identity(
        &self,
        window: crate::ui::window::WindowId,
    ) -> (
        crate::ui::window::WindowId,
        crate::ui::window::ViewId,
        crate::ui::window::ContentId,
    ) {
        let descriptor = self
            .application
            .window_session
            .graph()
            .window(window)
            .expect("fixture window");
        let content = self
            .application
            .window_session
            .graph()
            .view(descriptor.view)
            .expect("fixture view")
            .content;
        (window, descriptor.view, content)
    }
    pub(super) fn install_plugin_fixture(
        &mut self,
        index: crate::extensions::PluginContributionIndex,
    ) {
        self.application
            .extension
            .fixture_contribution(index)
            .unwrap();
    }
    pub(super) fn remove_catalog_load(&mut self, owner: &str, load: u64) {
        assert!(
            self.application
                .extension
                .remove_contribution(owner, crate::extensions::PluginLoadId(load))
        );
    }
    pub(super) fn plugin_window_target(&self, owner: &str) -> String {
        let window = self
            .application
            .window_session
            .graph()
            .windows()
            .into_iter()
            .find(|window| {
                self.application
                    .window_session
                    .graph()
                    .content_for_window(window.id)
                    .is_some_and(|content| content.owner == owner)
            })
            .map(|window| window.id)
            .expect("plugin window");
        format!("window:{}", window.0)
    }
    pub(super) fn clone_plugin_window(&mut self, target: String) {
        self.application
            .process_effect(crate::editor::AppEffect::WindowClone {
                target,
                axis: crate::editor::Axis::Vertical,
            });
    }
    pub(super) fn attempt_host_plugin_window_operations(&mut self, target: &str) {
        use crate::editor::AppEffect;
        for effect in [
            AppEffect::WindowDock {
                target: target.into(),
                edge: "left".into(),
                size: 10,
            },
            AppEffect::WindowFloat {
                target: target.into(),
                x: 2,
                y: 2,
                width: 20,
                height: 6,
            },
            AppEffect::WindowFullscreen(target.into()),
            AppEffect::WindowHide(target.into()),
            AppEffect::WindowShow(target.into()),
            AppEffect::WindowMove {
                target: target.into(),
                x: 4,
                y: 4,
            },
            AppEffect::WindowResize {
                target: target.into(),
                amount: 2,
            },
            AppEffect::WindowClose(target.into()),
        ] {
            self.application.process_effect(effect);
        }
    }
    pub(super) fn plugin_clone_state(&self) -> PluginCloneState {
        PluginCloneState {
            graph: format!("{:?}", self.application.window_session.graph()),
            editor: self.application.state.window_version(),
            catalog: self.application.extension.contributions().version(),
            host: self.application.extension.fixture_window_state(),
            retained: self.application.ui.plugin_surfaces.clone(),
            private: self.application.ui.private_surfaces.clone(),
            copies: self.copy_counts(),
        }
    }
    pub(super) fn plugin_request(
        &mut self,
        index: &crate::extensions::PluginContributionIndex,
        request: crate::extensions::PluginUiRequest,
    ) -> crate::extensions::PluginUiEvent {
        self.application.apply_plugin_ui_request(index, request)
    }
    pub(super) fn reject_catalog_contribution(
        &mut self,
        index: crate::extensions::PluginContributionIndex,
    ) -> Result<(), String> {
        let candidate = self
            .application
            .configuration
            .prepare_builtin_rules(self.application.extension.contributions());
        self.application
            .extension
            .fixture_contribution(index)
            .unwrap();
        self.application.commit_contribution_candidate(candidate)
    }
    pub(super) fn terminal_fixture(&mut self) {
        let app = &mut self.application;
        for id in [1, 2] {
            app.terminal_session.fixture(
                crate::process::PtyId(id),
                crate::editor::session::terminal::TerminalFixture {
                    profile: format!("fixture-{id}"),
                    screen: crate::ui::PtyScreen::new(80, 8, 100),
                    running: true,
                    exit_code: None,
                    truncated: false,
                },
                app.workspace.version(),
            );
        }
        app.terminal_session.select(crate::process::PtyId(1));
        app.set_terminal_focus(true);
        app.sync_terminal_panel();
    }
    pub(super) fn background_completion_fixture(&mut self) {
        self.application.ui.language_completion = Some(crate::ui::LanguageCompletionState {
            items: vec![crate::ui::CompletionItem {
                color: None,
                label: "background editor suggestion".into(),
                detail: String::new(),
            }],
            selected: 0,
            scroll: 0,
        });
    }
    pub(super) fn select_terminal_without_focus(&mut self, id: u64) {
        self.application
            .terminal_session
            .select(crate::process::PtyId(id));
    }
    pub(super) fn selected_terminal(&self) -> Option<u64> {
        self.application.terminal_session.selected().map(|id| id.0)
    }
    pub(super) fn displayed_terminal(&self) -> Option<u64> {
        self.application.displayed_terminal().map(|id| id.0)
    }
    pub(super) fn keyboard_terminal(&self) -> Option<u64> {
        self.application.keyboard_terminal().map(|id| id.0)
    }
    pub(super) fn context(&self) -> String {
        self.application.active_input_context()
    }
    pub(super) fn focus_explorer(&mut self, focused: bool) {
        self.application.set_explorer_focus(focused);
        self.application.reconcile_editor_windows();
    }
    pub(super) fn open_command_prompt(&mut self) {
        self.application
            .open_prompt(crate::ui::PromptKind::Command, String::new());
    }
    pub(super) fn close_prompt(&mut self) {
        self.application.close_prompt();
    }
    pub(super) fn close_documentation(&mut self) {
        self.application.close_documentation();
    }
    pub(super) fn close_explorer(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::ExplorerClose);
    }
    pub(super) fn notice(&mut self, text: &str) {
        self.application.notify_message(text.into(), false);
    }
    pub(super) fn error(&mut self, text: &str) {
        self.application.notify_message(text.into(), true);
    }
    pub(super) fn resize(&mut self, width: u16, height: u16) {
        self.application
            .handle_event(UiEvent::Resize(width, height));
        self.application.reconcile_editor_windows();
    }
    pub(super) fn frame(&mut self) -> crate::ui::Frame {
        self.application.reconcile_editor_windows();
        crate::ui::render(
            &self.application.state,
            &self.application.ui,
            self.application.configuration.settings(),
            self.application.configuration.dashboard(),
            self.application.window_session.width(),
            self.application.window_session.height(),
        )
    }
    pub(super) fn explorer_fixture(&mut self) {
        for index in 0..40 {
            std::fs::write(
                self._workspace.path().join(format!("file{index:02}.txt")),
                "fixture",
            )
            .unwrap();
        }
        self.application.open_explorer();
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.application.explorer.pending() {
            self.application.process_explorer_result();
            assert!(Instant::now() < deadline, "explorer fixture timed out");
            std::thread::yield_now();
        }
        self.application.reconcile_editor_windows();
    }
    pub(super) fn clone_explorer(&mut self) {
        self.clone_explorer_view(1).unwrap();
    }
    pub(super) fn clone_explorer_view(&mut self, view: u64) -> Result<(), String> {
        let ids = self
            .application
            .window_session
            .native_ids()
            .explorer_view(view)
            .ok_or_else(|| "explorer fixture view is unavailable".to_owned())?;
        self.application.clone_graph_window(
            &format!("window:{}", ids.window.0),
            crate::editor::Axis::Horizontal,
        )?;
        self.application.reconcile_editor_windows();
        Ok(())
    }
    pub(super) fn close_explorer_view(&mut self, view: u64) -> Result<(), String> {
        let ids = self
            .application
            .window_session
            .native_ids()
            .explorer_view(view)
            .ok_or_else(|| "explorer fixture view is unavailable".to_owned())?;
        self.application
            .close_graph_window(&format!("window:{}", ids.window.0))
    }
    pub(super) fn reset_window_runtime(&mut self) -> Result<(), String> {
        self.application.window_session.reset_runtime(&[])?;
        self.application.reconcile_editor_windows();
        self.application.sync_explorer_ui();
        Ok(())
    }
    pub(super) fn reset_terminal_window_runtime(&mut self) -> Result<(), String> {
        self.application.window_session.reset_runtime(&[])?;
        self.application.reconcile_editor_windows();
        self.application.sync_terminal_panel();
        self.application.resize_active_terminal_to_panel();
        Ok(())
    }
    pub(super) fn explorer_focus(&self) -> Option<u64> {
        self.application.window_session.focused_explorer()
    }
    pub(super) fn explorer_selections(&self) -> std::collections::BTreeMap<u64, Option<u64>> {
        let mut selections = std::collections::BTreeMap::new();
        selections.insert(
            1,
            self.application
                .ui
                .explorer
                .as_ref()
                .and_then(|view| view.rows.iter().find(|row| row.selected))
                .map(|row| row.node_id),
        );
        selections.extend(self.application.ui.explorer_views.iter().map(|view| {
            (
                view.view,
                view.content
                    .rows
                    .iter()
                    .find(|row| row.selected)
                    .map(|row| row.node_id),
            )
        }));
        selections
    }
    pub(super) fn explorer_body(&self, view: u64) -> crate::ui::window::WindowRect {
        let ids = self
            .application
            .window_session
            .native_ids()
            .explorer_view(view)
            .expect("explorer fixture view");
        let rect = self
            .application
            .compositor_projection()
            .and_then(|projection| projection.window(ids.window))
            .expect("projected explorer fixture")
            .rect;
        crate::ui::geometry::explorer_body_geometry(
            rect,
            self.application.configuration.settings(),
        )
    }
    pub(super) fn explorer_graph_shape(&self) -> (usize, usize, usize) {
        let ids = self.explorer_content_ids();
        let contents = ids
            .iter()
            .map(|entry| entry.1)
            .collect::<std::collections::BTreeSet<_>>();
        let views = self
            .application
            .window_session
            .graph()
            .content_snapshots()
            .into_iter()
            .filter(|content| contents.contains(&content.id))
            .map(|content| content.view_count)
            .sum();
        (contents.len(), views, ids.len())
    }
    pub(super) fn explorer_content_ids(
        &self,
    ) -> Vec<(u64, crate::ui::window::ContentId, crate::ui::window::ViewId)> {
        self.application
            .window_session
            .native_ids()
            .explorer_views()
            .map(|(view, ids)| (view, ids.content, ids.view))
            .collect()
    }
    pub(super) fn float_explorer(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::WindowFloat {
                target: "explorer".into(),
                x: 5,
                y: 5,
                width: 40,
                height: 8,
            });
        self.application.reconcile_editor_windows();
    }
    pub(super) fn explorer_window(&self, view: u64) -> crate::ui::window::WindowId {
        self.application
            .window_session
            .native_ids()
            .explorer_view(view)
            .expect("explorer fixture view")
            .window
    }
    pub(super) fn window_tab(
        &self,
        window: crate::ui::window::WindowId,
    ) -> crate::ui::window::WorkspaceTabId {
        self.application
            .window_session
            .graph()
            .window(window)
            .expect("fixture window")
            .tab
    }
    pub(super) fn active_window_tab(&self) -> crate::ui::window::WorkspaceTabId {
        self.application
            .window_session
            .graph()
            .active_tab()
            .expect("active fixture window tab")
    }
    pub(super) fn window_placement(
        &self,
        window: crate::ui::window::WindowId,
    ) -> crate::ui::window::WindowPlacement {
        self.application
            .window_session
            .graph()
            .window(window)
            .expect("fixture window")
            .placement
    }
    pub(super) fn place_window(
        &mut self,
        window: crate::ui::window::WindowId,
        placement: crate::ui::window::WindowPlacement,
    ) -> Result<(), String> {
        self.application
            .place_graph_window(&format!("window:{}", window.0), placement)
    }
    pub(super) fn show_window(
        &mut self,
        window: crate::ui::window::WindowId,
    ) -> Result<(), String> {
        self.application
            .show_graph_window(&format!("window:{}", window.0))
    }
    pub(super) fn page_explorer(&mut self, direction: isize) {
        self.application.page_explorer_selection(direction);
    }
    pub(super) fn click(&mut self, column: u16, row: u16) {
        self.application.handle_event(UiEvent::Mouse {
            key: crate::editor::input::KeyEvent::mouse(
                crate::editor::input::KeyCode::MouseLeftDown,
                Default::default(),
            ),
            column,
            row,
        });
    }
    pub(super) fn grep_snapshot_fixture(&mut self) {
        // Exercise the real bounded worker/store instead of fabricating rich picker payloads.
        let text = (0..1200)
            .map(|line| format!("line{line:04} {}\n", "context ".repeat(12)))
            .collect::<String>();
        for index in 0..45 {
            std::fs::write(
                self._workspace.path().join(format!("row-{index:02}.txt")),
                &text,
            )
            .unwrap();
        }
        self.application.open_live_workspace_search();
        self.application
            .restart_live_workspace_search("line1087".into());
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.application.search.pending() {
            self.application.process_workspace_search_events();
            assert!(Instant::now() < deadline, "grep fixture timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(self.application.search.matches().len(), 45);
        self.application.reconcile_editor_windows();
    }
    pub(super) fn start_files_fixture(&mut self) {
        for name in ["jack-1.txt", "jack-2.txt", "other.txt"] {
            std::fs::write(self._workspace.path().join(name), "jack\n").unwrap();
        }
        self.application.start_workspace_file_picker();
    }
    pub(super) fn finish_search(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.application.search.files_pending() || self.application.search.pending() {
            self.application.process_workspace_file_events();
            self.application.process_workspace_search_events();
            assert!(Instant::now() < deadline, "search fixture timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        super::wait_vfs(&mut self.application);
    }
    pub(super) fn open_grep(&mut self) {
        self.application.open_live_workspace_search();
    }
    pub(super) fn paste(&mut self, text: &str) {
        self.application.handle_event(UiEvent::Paste(text.into()));
    }
    /// The application under test, so a test can ask the SESSION what it did rather than
    /// infer it from the driver's surface.
    pub(super) fn application(&self) -> &InteractiveApplication {
        &self.application
    }
    pub(super) fn application_mut(&mut self) -> &mut InteractiveApplication {
        &mut self.application
    }
    pub(super) fn editor_mode(&self) -> crate::editor::EditorMode {
        self.application.state.views[self.application.state.active_view.unwrap()].mode
    }
    pub(super) fn key(&mut self, key: &str) {
        self.application.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: KeyInputKind::Press,
        });
    }
    pub(super) fn advance_input(&mut self, elapsed: Duration) {
        self.clock.advance(elapsed);
        self.application.flush_keymap_timeout();
        self.application.update_key_hint_ui();
    }
    pub(super) fn focus_lost(&mut self) {
        self.application.handle_event(UiEvent::FocusLost);
    }
    pub(super) fn apply_candidate(&mut self, loaded: LoadedConfig) -> Result<(), String> {
        self.application
            .apply_configuration_candidate(loaded, false)
    }
    pub(super) fn reject_stale_contribution(&mut self) -> Result<(), String> {
        let candidate = self
            .application
            .configuration
            .prepare_builtin_rules(self.application.extension.contributions());
        self.application.configuration.toggle_inlay_hints();
        self.application.commit_contribution_candidate(candidate)
    }
    pub(super) fn window_policy_snapshot(
        &self,
    ) -> (
        crate::ui::window::WindowGraph,
        crate::config::WindowsConfig,
        u64,
    ) {
        (
            self.application.window_session.graph().clone(),
            self.application.configuration.settings().windows.clone(),
            self.application.configuration.binding_revision(),
        )
    }
    pub(super) fn time(&self) -> (Instant, std::time::SystemTime) {
        (self.clock.monotonic(), self.clock.system_time())
    }
    pub(super) fn quit_requested(&self) -> bool {
        self.application.should_quit
    }
    pub(super) fn projection(&self) -> &UiState {
        &self.application.ui
    }
    pub(super) fn prefix(&self) -> Option<&str> {
        self.application.input.mapping_prefix()
    }
    pub(super) fn documentation(&mut self, text: String) {
        self.application.present_documentation_popup(
            crate::ui::DocumentationPopupKind::Hover,
            "Hover".into(),
            "fixture".into(),
            None,
            vec![crate::language::lsp::LspMarkupContent {
                kind: crate::language::lsp::LspMarkupKind::PlainText,
                value: text,
            }],
        );
    }
}

impl TestSession {
    pub(super) fn verify_open_lifetimes(&mut self) {
        use super::wait_vfs;
        use crate::editor::text::TextBuffer;
        use std::fs;
        let app = &mut self.application;
        let root = app.state.workspace_root.clone();
        let path = root.join("pending.txt");
        fs::write(&path, b"disk").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("disk"), Some(path.clone()));
        app.reconcile_editor_windows();
        let (worker, gate) = crate::files::VfsWorker::start_gated_for_test(2);
        app.documents = crate::editor::session::documents::DocumentSession::new(
            worker,
            &StartupTarget::Dashboard { root: root.clone() },
            app.state.window_version(),
            app.workspace.root_revision(),
            2,
        );
        app.start_open(path.clone());
        app.close_active_buffer(true);
        gate.send(()).unwrap();
        wait_vfs(app);
        assert!(!app.state.buffers.contains_key(buffer));
        assert!(
            app.state
                .buffers
                .values()
                .all(|b| b.path.as_ref() != Some(&path)),
            "closed path resurrected"
        );
        let (worker, gate) = crate::files::VfsWorker::start_gated_for_test(2);
        app.documents = crate::editor::session::documents::DocumentSession::new(
            worker,
            &StartupTarget::Dashboard { root: root.clone() },
            app.state.window_version(),
            app.workspace.root_revision(),
            2,
        );
        app.start_open(path.clone());
        let (other, view) = app
            .state
            .open_text(TextBuffer::from_text("other"), Some(root.join("other.txt")));
        app.reconcile_editor_windows();
        gate.send(()).unwrap();
        wait_vfs(app);
        assert_eq!(app.state.active_view, Some(view), "late open stole focus");
        assert_eq!(app.state.active_buffer_id(), Some(other));
        assert!(
            app.state
                .buffers
                .values()
                .any(|b| b.path.as_ref() == Some(&path)),
            "valid background data discarded"
        );
    }
    pub(super) fn verify_save_collisions(&mut self) {
        use super::wait_vfs;
        use crate::editor::text::TextBuffer;
        use std::fs;
        let app = &mut self.application;
        let root = app.state.workspace_root.clone();
        let target = root.join("target.txt");
        fs::write(&target, b"target").unwrap();
        app.state
            .load_hidden_text(TextBuffer::from_text("target"), target.clone());
        let (source, _) = app.state.open_text(
            TextBuffer::from_text("source"),
            Some(root.join("source.txt")),
        );
        app.reconcile_editor_windows();
        app.start_save(false, Some(target.clone()), true);
        wait_vfs(app);
        assert_eq!(fs::read(&target).unwrap(), b"target");
        assert!(
            app.state.buffers[source]
                .path
                .as_ref()
                .unwrap()
                .ends_with("source.txt")
        );
        #[cfg(unix)]
        {
            let alias = root.join("alias.txt");
            std::os::unix::fs::symlink(&target, &alias).unwrap();
            app.start_save(false, Some(alias), true);
            wait_vfs(app);
            assert_eq!(fs::read(&target).unwrap(), b"target");
        }
    }
}

impl TestSession {
    pub(super) fn verify_expired_shutdown_does_not_write(&mut self) {
        self.application.documents.stop_worker();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.application.documents.worker_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let result = self
            .application
            .finish_shutdown_before(Instant::now() - Duration::from_secs(1));
        assert!(matches!(
            result,
            Err(super::super::InteractiveError::Vfs(
                crate::files::VfsError::Io {
                    operation: "finish persistence shutdown",
                    ..
                }
            ))
        ));
        assert!(
            !self
                ._state
                .path()
                .join(
                    &self
                        .application
                        .configuration
                        .settings()
                        .persistence
                        .state_file
                )
                .exists(),
            "expired shutdown wrote an exit snapshot"
        );
    }
}

impl TestSession {
    pub(super) fn trust_owned_terminal_fixture(&mut self) {
        super::prepare_trust_identity(&mut self.application);
        for root in self.application.workspace.roots().to_vec() {
            self.application
                .workspace
                .fixture_trust_identity(root.identity);
        }
        self.application.reconcile_workspace_trust();
    }
    pub(super) fn terminal_request(&mut self, profile: &str) {
        self.application
            .process_effect(crate::editor::AppEffect::TerminalOpen(Some(profile.into())));
    }
    pub(super) fn terminal_open(&mut self, profile: &str) {
        self.terminal_request(profile);
    }
    pub(super) fn focus_only_terminal(&mut self) {
        let ids = self.application.terminal_session.descriptions();
        assert_eq!(ids.len(), 1);
        self.application
            .process_effect(crate::editor::AppEffect::TerminalFocus(ids[0].0));
    }
    pub(super) fn terminal_close(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::TerminalClose);
        if self
            .application
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::TerminalClose)
        {
            self.application
                .input
                .select_confirmation(crate::ui::ConfirmationChoice::Yes);
            self.application.submit_prompt();
        }
    }
    pub(super) fn terminal_begin_close(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::TerminalClose);
        assert!(
            self.application
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::TerminalClose)
        );
    }
    pub(super) fn terminal_submit_close(&mut self) {
        self.application.submit_prompt();
    }
    pub(super) fn terminal_hide_and_show(&mut self) {
        self.terminal_begin_close();
        self.terminal_submit_close();
        self.application
            .process_effect(crate::editor::AppEffect::TerminalToggle);
    }
    pub(super) fn notification(&self) -> Option<&str> {
        self.application.ui.notification.as_deref()
    }
    pub(super) fn terminal_unfocus(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::TerminalUnfocus);
    }
    pub(super) fn terminal_send(&mut self, text: &str) {
        self.application
            .process_effect(crate::editor::AppEffect::TerminalSend(text.into()));
    }
    pub(super) fn clone_terminal(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::WindowClone {
                target: "terminal".into(),
                axis: crate::editor::Axis::Horizontal,
            });
    }
    pub(super) fn place_terminal(
        &mut self,
        placement: crate::ui::window::WindowPlacement,
    ) -> Result<(), String> {
        self.application.place_graph_window("terminal", placement)
    }
    pub(super) fn show_terminal(&mut self) -> Result<(), String> {
        self.application.show_graph_window("terminal")
    }
    pub(super) fn terminal_window_identity(
        &self,
    ) -> Option<(
        crate::ui::window::ContentId,
        crate::ui::window::ViewId,
        crate::ui::window::WindowId,
    )> {
        let id = self.application.displayed_terminal()?;
        let ids = self
            .application
            .window_session
            .native_ids()
            .terminal(id.0)?;
        Some((ids.content, ids.view, ids.window))
    }
    pub(super) fn terminal_geometry_source(
        &self,
    ) -> Option<crate::editor::session::windows::TerminalGeometrySource> {
        let id = self.application.displayed_terminal()?;
        self.application
            .window_session
            .terminal_geometry_source(id.0)
    }
    pub(super) fn terminal_window_tab(&self) -> Option<crate::ui::window::WorkspaceTabId> {
        let (_, _, window) = self.terminal_window_identity()?;
        self.application
            .window_session
            .graph()
            .window(window)
            .map(|window| window.tab)
    }
    pub(super) fn terminal_mode(&self) -> Option<crate::ui::TerminalPanelMode> {
        let id = self.application.displayed_terminal()?;
        self.application.terminal_session.mode(id)
    }
    pub(super) fn terminal_cursor(&self) -> Option<(usize, usize)> {
        self.application
            .ui
            .terminal_panel
            .as_ref()
            .and_then(|panel| panel.cursor)
    }
    pub(super) fn terminal_clone_state(&self) -> TerminalCloneState {
        let selected = self.application.terminal_session.selected();
        TerminalCloneState {
            graph: self.application.window_session.graph().clone(),
            editor: self.application.state.window_version(),
            descriptions: self.application.terminal_session.descriptions(),
            selected,
            focused: self.application.window_session.focused_terminal(),
            dimensions: selected.map(|id| self.application.terminal_session.fixture_dimensions(id)),
            screen: selected
                .map(|id| self.application.terminal_session.fixture_lines(id))
                .unwrap_or_default(),
            pending: self.application.terminal_session.pending(),
            starts: self.terminal_started_markers(),
        }
    }
    pub(super) fn pump_terminal(&mut self) {
        self.application.process_execution_preflight();
        self.application.process_pty_events();
    }
    pub(super) fn terminal_pending(&self) -> usize {
        self.application.terminal_session.pending()
    }
    pub(super) fn terminal_phase(&self) -> Option<crate::ui::TerminalPhase> {
        let selected = self.application.terminal_session.selected()?;
        self.application
            .ui
            .terminal_panel
            .as_ref()?
            .tabs
            .iter()
            .find(|tab| tab.id == selected.0)
            .map(|tab| tab.phase)
    }
    pub(super) fn terminal_exit_code(&self) -> Option<u32> {
        self.application
            .ui
            .terminal_panel
            .as_ref()
            .and_then(|panel| panel.exit_code)
    }
    pub(super) fn expire_shutdown_with_terminal(&mut self) -> Vec<String> {
        let _ = self
            .application
            .finish_shutdown_before(Instant::now() - Duration::from_millis(1));
        self.application
            .notices
            .history()
            .map(|(_, message)| message.clone())
            .collect()
    }
    pub(super) fn terminal_dimensions(&self) -> Option<(u16, u16)> {
        self.application.terminal_body_dimensions()
    }
    pub(super) fn terminal_screen_dimensions(&self) -> Option<(usize, usize)> {
        let id = self.application.displayed_terminal()?;
        Some(self.application.terminal_session.fixture_dimensions(id))
    }
    pub(super) fn stop_terminal_then_resize(&mut self) -> Result<(), String> {
        let id = self.application.displayed_terminal().expect("terminal");
        let source = self.terminal_geometry_source().expect("geometry");
        self.application.terminal_session.request_stop(id)?;
        self.application.terminal_session.resize(
            id,
            70,
            5,
            source,
            &self.application.workspace,
        )
    }
    pub(super) fn terminal_pending_resizes(&self) -> Option<usize> {
        let id = self.application.displayed_terminal()?;
        Some(
            self.application
                .terminal_session
                .fixture_pending_resizes(id),
        )
    }
    pub(super) fn terminal_pending_resize_matches(
        &self,
        source: &crate::editor::session::windows::TerminalGeometrySource,
    ) -> bool {
        self.application.displayed_terminal().is_some_and(|id| {
            self.application
                .terminal_session
                .fixture_pending_resize_matches(id, source)
        })
    }
    pub(super) fn terminal_screen_state(
        &self,
    ) -> Option<crate::editor::session::terminal::TerminalScreenState> {
        let id = self.application.displayed_terminal()?;
        Some(self.application.terminal_session.fixture_screen_state(id))
    }
    pub(super) fn terminal_lines(&self) -> Vec<String> {
        self.application
            .displayed_terminal()
            .map(|id| self.application.terminal_session.fixture_lines(id))
            .unwrap_or_default()
    }
    pub(super) fn terminal_started_markers(&self) -> String {
        std::fs::read_to_string(self._workspace.path().join("starts")).unwrap_or_default()
    }
}

impl TestSession {
    #[cfg(unix)]
    pub(super) fn debug_program_fixture(&self) {
        use std::os::unix::fs::PermissionsExt;
        let path = self._workspace.path().join("debuggee");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    pub(super) fn debug_program_pending(&self) -> bool {
        self.application.debugger_session.debug_program_pending()
    }
    pub(super) fn debug_program_wire_path(&self) -> bool {
        std::fs::read_to_string(self._workspace.path().join("launch-program"))
            .is_ok_and(|p| std::path::Path::new(&p) == self._workspace.path().join("debuggee"))
    }
    pub(super) fn debug_reverse_refusal(&self) -> bool {
        std::fs::read(self._workspace.path().join("reverse-reply"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|v| {
                v["success"] == false
                    && v["message"]
                        .as_str()
                        .is_some_and(|s| s.contains("original debug configuration"))
            })
    }
    pub(super) fn debug_request(&mut self, name: &str) {
        self.application
            .process_effect(crate::editor::AppEffect::DapConfigurationStart(name.into()));
    }
    pub(super) fn debug_preflights(&self) -> usize {
        self.application.workspace.preflight_pending()
    }
    pub(super) fn debug_start(&mut self, name: &str) {
        self.application
            .process_effect(crate::editor::AppEffect::DapConfigurationStart(name.into()));
    }
    pub(super) fn debug_stop(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::DapStop);
    }
    pub(super) fn pump_debug(&mut self) {
        self.application.process_dap_project_root();
        self.application.process_execution_preflight();
        self.application.process_dap_events();
    }
    /// A stop fills the panes by itself; asking for the threads is what opens a picker.
    pub(super) fn open_debug_threads_after_stop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(6);
        while self.application.debugger_session.threads().is_empty() {
            self.pump_debug();
            assert!(
                Instant::now() < deadline,
                "the session never stopped: {:?}",
                self.projection().notification
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        self.debug_threads();
        while self.projection().picker.is_none() {
            self.pump_debug();
            assert!(Instant::now() < deadline, "the threads picker never opened");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pub(super) fn debug_threads(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::DapThreads);
    }
    pub(super) fn accept_debug_picker(&mut self) {
        self.application.accept_picker();
    }
    pub(super) fn debug_pending(&self) -> usize {
        self.application.debugger_session.pending()
    }
    pub(super) fn debug_source_pending(&self) -> bool {
        self.application.debugger_session.source_pending()
    }
    pub(super) fn debug_source_text(&self) -> Option<String> {
        let editor = &self.application.state;
        let buffer = &editor.buffers[editor.views[editor.active_view?].buffer];
        buffer
            .is_virtual_read_only()
            .then(|| buffer.text.to_string())
    }
}
