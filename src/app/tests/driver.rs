//! Restricted driver for migrated input/configuration workflows. It deliberately
//! offers no mutable application, editor, configuration, registry or UI access.
use crate::app::App;
use crate::app::persistence::STATE_FILE;
use crate::app::startup::StartupTarget;
use crate::clock::Clock;
use crate::clock::ManualClock;
use crate::config::LoadedConfig;
use crate::ui::KeyInputKind;
use crate::ui::UiEvent;
use crate::ui::UiState;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use std::time::UNIX_EPOCH;
use tempfile::TempDir;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct TerminalCloneState {
    pub(in crate::app) graph: crate::windows::WindowGraph,
    pub(in crate::app) editor: crate::editor::EditorWindowVersion,
    pub(in crate::app) descriptions: Vec<(u64, String, crate::windows::ContentLifecycle)>,
    pub(in crate::app) selected: Option<crate::process::PtyId>,
    pub(in crate::app) focused: Option<u64>,
    pub(in crate::app) dimensions: Option<(usize, usize)>,
    pub(in crate::app) screen: Vec<String>,
    pub(in crate::app) pending: usize,
    pub(in crate::app) starts: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PluginCloneState {
    pub(in crate::app) graph: String,
    pub(in crate::app) editor: crate::editor::EditorWindowVersion,
    pub(in crate::app) catalog: crate::plugins::PluginCatalogVersion,
    pub(in crate::app) host: String,
    pub(in crate::app) retained: Vec<crate::ui::PluginSurfaceUiState>,
    pub(in crate::app) private: Vec<crate::ui::PrivateSurfaceUiState>,
    pub(in crate::app) copies: (usize, usize, usize),
}

pub(in crate::app) struct TestSession {
    pub(in crate::app) application: App,
    pub(in crate::app) clock: Arc<ManualClock>,
    pub(in crate::app) _workspace: TempDir,
    pub(in crate::app) _state: TempDir,
}
impl TestSession {
    pub(in crate::app) fn new(loaded: LoadedConfig) -> Self {
        loaded
            .config
            .validate()
            .expect("valid initial fixture policy");
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(UNIX_EPOCH, Instant::now()));
        let mut application = App::new(
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
    pub(in crate::app) fn new_buffer(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::NewBuffer);
    }
    pub(in crate::app) fn new_tab(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::Layout(
                crate::app::windows::LayoutAction::NewTab,
            ));
    }
    pub(in crate::app) fn window_list(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::WindowList);
    }
    pub(in crate::app) fn focused_window(&self) -> Option<crate::windows::WindowId> {
        self.application.window_session.graph().focused()
    }
    pub(in crate::app) fn active_tab(&self) -> Option<crate::windows::WorkspaceTabId> {
        self.application.state.active_tab()
    }
    pub(in crate::app) fn reconcile_windows(&mut self) {
        self.application.reconcile_editor_windows();
    }
    pub(in crate::app) fn copy_counts(&self) -> (usize, usize, usize) {
        (
            crate::windows::WindowGraph::test_clone_count(),
            crate::app::plugins::Plugins::fixture_window_clone_count(),
            self.application.window_session.content_builds(),
        )
    }
    pub(in crate::app) fn graph_revision(&self) -> u64 {
        self.application.window_session.graph().revision().unwrap()
    }
    pub(in crate::app) fn window_identity(
        &self,
        window: crate::windows::WindowId,
    ) -> (
        crate::windows::WindowId,
        crate::windows::ViewId,
        crate::windows::ContentId,
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
    pub(in crate::app) fn install_plugin_fixture(
        &mut self,
        index: crate::plugins::PluginContributionIndex,
    ) {
        self.application
            .extension
            .fixture_contribution(index)
            .unwrap();
    }
    pub(in crate::app) fn remove_catalog_load(&mut self, owner: &str, load: u64) {
        assert!(
            self.application
                .extension
                .remove_contribution(owner, crate::plugins::PluginLoadId(load))
        );
    }
    pub(in crate::app) fn plugin_window_target(&self, owner: &str) -> String {
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
    pub(in crate::app) fn clone_plugin_window(&mut self, target: String) {
        self.application
            .process_effect(crate::app::commands::AppEffect::WindowClone {
                target,
                axis: crate::windows::TileAxis::Vertical,
            });
    }
    pub(in crate::app) fn attempt_host_plugin_window_operations(&mut self, target: &str) {
        use crate::app::commands::AppEffect;
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
    pub(in crate::app) fn plugin_request(
        &mut self,
        index: &crate::plugins::PluginContributionIndex,
        request: crate::plugins::PluginUiRequest,
    ) -> crate::plugins::PluginUiEvent {
        self.application.apply_plugin_ui_request(index, request)
    }
    pub(in crate::app) fn reject_catalog_contribution(
        &mut self,
        index: crate::plugins::PluginContributionIndex,
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
    pub(in crate::app) fn terminal_fixture(&mut self) {
        let app = &mut self.application;
        for id in [1, 2] {
            let mut screen = crate::ui::PtyScreen::new(80, 8, 100);
            screen.feed_bytes(format!("terminal {id} contents").as_bytes());
            app.terminal_session.fixture(
                crate::process::PtyId(id),
                crate::app::terminal::TerminalFixture {
                    profile: format!("fixture-{id}"),
                    screen,
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
    pub(in crate::app) fn terminal_snapshots(&self) -> Vec<(u64, Vec<String>)> {
        self.application
            .terminal_session
            .descriptions()
            .into_iter()
            .map(|(id, _, _)| {
                let lines = self
                    .application
                    .terminal_session
                    .fixture_lines(crate::process::PtyId(id))
                    .into_iter()
                    .filter(|line| !line.trim().is_empty())
                    .collect();
                (id, lines)
            })
            .collect()
    }
    pub(in crate::app) fn background_completion_fixture(&mut self) {
        self.application.ui.language_completion = Some(crate::ui::LanguageCompletionState {
            typed: 0,
            items: vec![crate::ui::CompletionItem {
                icon: None,
                color: None,
                label: "background editor suggestion".into(),
                detail: String::new(),
            }],
            selected: 0,
            scroll: 0,
        });
    }
    pub(in crate::app) fn select_terminal_without_focus(&mut self, id: u64) {
        self.application
            .terminal_session
            .select(crate::process::PtyId(id));
    }
    pub(in crate::app) fn selected_terminal(&self) -> Option<u64> {
        self.application.terminal_session.selected().map(|id| id.0)
    }
    pub(in crate::app) fn displayed_terminal(&self) -> Option<u64> {
        self.application.displayed_terminal().map(|id| id.0)
    }
    pub(in crate::app) fn keyboard_terminal(&self) -> Option<u64> {
        self.application.keyboard_terminal().map(|id| id.0)
    }
    pub(in crate::app) fn context(&self) -> String {
        self.application.active_input_context()
    }
    pub(in crate::app) fn focus_explorer(&mut self, focused: bool) {
        self.application.set_explorer_focus(focused);
        self.application.reconcile_editor_windows();
    }
    pub(in crate::app) fn open_command_prompt(&mut self) {
        self.application
            .open_prompt(crate::ui::PromptKind::Command, String::new());
    }
    pub(in crate::app) fn close_prompt(&mut self) {
        self.application.close_prompt();
    }
    pub(in crate::app) fn close_documentation(&mut self) {
        self.application.close_documentation();
    }
    pub(in crate::app) fn close_explorer(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::ExplorerClose);
    }
    pub(in crate::app) fn notice(&mut self, text: &str) {
        self.application.notify_message(text.into(), false);
    }
    pub(in crate::app) fn error(&mut self, text: &str) {
        self.application.notify_message(text.into(), true);
    }
    pub(in crate::app) fn resize(&mut self, width: u16, height: u16) {
        self.application
            .handle_event(UiEvent::Resize(width, height));
        self.application.reconcile_editor_windows();
    }
    pub(in crate::app) fn frame(&mut self) -> crate::ui::Frame {
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
    pub(in crate::app) fn explorer_fixture(&mut self) {
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
    pub(in crate::app) fn clone_explorer(&mut self) {
        self.clone_explorer_view(1).unwrap();
    }
    pub(in crate::app) fn clone_explorer_view(&mut self, view: u64) -> Result<(), String> {
        let ids = self
            .application
            .window_session
            .native_ids()
            .explorer_view(view)
            .ok_or_else(|| "explorer fixture view is unavailable".to_owned())?;
        self.application.clone_graph_window(
            &format!("window:{}", ids.window.0),
            crate::windows::TileAxis::Horizontal,
        )?;
        self.application.reconcile_editor_windows();
        Ok(())
    }
    pub(in crate::app) fn close_explorer_view(&mut self, view: u64) -> Result<(), String> {
        let ids = self
            .application
            .window_session
            .native_ids()
            .explorer_view(view)
            .ok_or_else(|| "explorer fixture view is unavailable".to_owned())?;
        self.application
            .close_graph_window(&format!("window:{}", ids.window.0))
    }
    pub(in crate::app) fn reset_window_runtime(&mut self) -> Result<(), String> {
        self.application
            .window_session
            .reset_runtime(&[], &mut self.application.state)?;
        self.application.reconcile_editor_windows();
        self.application.sync_explorer_ui();
        Ok(())
    }
    pub(in crate::app) fn reset_terminal_window_runtime(&mut self) -> Result<(), String> {
        self.application
            .window_session
            .reset_runtime(&[], &mut self.application.state)?;
        self.application.reconcile_editor_windows();
        self.application.sync_terminal_panel();
        self.application.resize_active_terminal_to_panel();
        Ok(())
    }
    pub(in crate::app) fn explorer_focus(&self) -> Option<u64> {
        self.application.window_session.focused_explorer()
    }
    pub(in crate::app) fn explorer_selections(
        &self,
    ) -> std::collections::BTreeMap<u64, Option<u64>> {
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
    pub(in crate::app) fn explorer_body(&self, view: u64) -> crate::windows::WindowRect {
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
        crate::ui::render::explorer::explorer_body_geometry(rect)
    }
    pub(in crate::app) fn explorer_graph_shape(&self) -> (usize, usize, usize) {
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
    pub(in crate::app) fn explorer_content_ids(
        &self,
    ) -> Vec<(u64, crate::windows::ContentId, crate::windows::ViewId)> {
        self.application
            .window_session
            .native_ids()
            .explorer_views()
            .map(|(view, ids)| (view, ids.content, ids.view))
            .collect()
    }
    pub(in crate::app) fn float_explorer(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::WindowFloat {
                target: "explorer".into(),
                x: 5,
                y: 5,
                width: 40,
                height: 8,
            });
        self.application.reconcile_editor_windows();
    }
    pub(in crate::app) fn explorer_window(&self, view: u64) -> crate::windows::WindowId {
        self.application
            .window_session
            .native_ids()
            .explorer_view(view)
            .expect("explorer fixture view")
            .window
    }
    pub(in crate::app) fn window_tab(
        &self,
        window: crate::windows::WindowId,
    ) -> crate::windows::WorkspaceTabId {
        self.application
            .window_session
            .graph()
            .window(window)
            .expect("fixture window")
            .tab
    }
    pub(in crate::app) fn active_window_tab(&self) -> crate::windows::WorkspaceTabId {
        self.application
            .window_session
            .graph()
            .active_tab()
            .expect("active fixture window tab")
    }
    pub(in crate::app) fn window_placement(
        &self,
        window: crate::windows::WindowId,
    ) -> crate::windows::WindowPlacement {
        self.application
            .window_session
            .graph()
            .window(window)
            .expect("fixture window")
            .placement
    }
    pub(in crate::app) fn place_window(
        &mut self,
        window: crate::windows::WindowId,
        placement: crate::windows::WindowPlacement,
    ) -> Result<(), String> {
        self.application
            .place_graph_window(&format!("window:{}", window.0), placement)
    }
    pub(in crate::app) fn show_window(
        &mut self,
        window: crate::windows::WindowId,
    ) -> Result<(), String> {
        self.application
            .show_graph_window(&format!("window:{}", window.0))
    }
    pub(in crate::app) fn page_explorer(&mut self, direction: isize) {
        self.application.page_explorer_selection(direction);
    }
    pub(in crate::app) fn click(&mut self, column: u16, row: u16) {
        self.application.handle_event(UiEvent::Mouse {
            key: crate::editor::input::KeyEvent::mouse(
                crate::editor::input::KeyCode::MouseLeftDown,
                Default::default(),
            ),
            column,
            row,
        });
    }
    pub(in crate::app) fn mouse(
        &mut self,
        code: crate::editor::input::KeyCode,
        column: u16,
        row: u16,
    ) {
        self.application.handle_event(UiEvent::Mouse {
            key: crate::editor::input::KeyEvent::mouse(code, Default::default()),
            column,
            row,
        });
    }
    pub(in crate::app) fn explorer_focused(&self) -> bool {
        self.application.window_session.focused_explorer().is_some()
    }
    /// Waits for the open a click started, and names the file it brought up.
    pub(in crate::app) fn wait_for_opened_file(&mut self) -> Option<String> {
        crate::app::tests::wait_vfs(&mut self.application);
        let buffer = self.application.state.active_buffer_id()?;
        let path = self.application.state.buffers[buffer].path.as_ref()?;
        Some(path.file_name()?.to_string_lossy().into_owned())
    }
    pub(in crate::app) fn grep_snapshot_fixture(&mut self) {
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
    pub(in crate::app) fn start_files_fixture(&mut self) {
        for name in ["jack-1.txt", "jack-2.txt", "other.txt"] {
            std::fs::write(self._workspace.path().join(name), "jack\n").unwrap();
        }
        self.application.start_workspace_file_picker();
    }
    pub(in crate::app) fn finish_search(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.application.search.files_pending() || self.application.search.pending() {
            self.application.process_workspace_file_events();
            self.application.process_workspace_search_events();
            assert!(Instant::now() < deadline, "search fixture timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        crate::app::tests::wait_vfs(&mut self.application);
    }
    pub(in crate::app) fn open_grep(&mut self) {
        self.application.open_live_workspace_search();
    }
    pub(in crate::app) fn paste(&mut self, text: &str) {
        self.application.handle_event(UiEvent::Paste(text.into()));
    }
    /// The application under test, so a test can ask the SESSION what it did rather than
    /// infer it from the driver's surface.
    pub(in crate::app) fn application(&self) -> &App {
        &self.application
    }
    pub(in crate::app) fn application_mut(&mut self) -> &mut App {
        &mut self.application
    }
    pub(in crate::app) fn editor_mode(&self) -> crate::editor::EditorMode {
        self.application.state.views[self.application.state.active_view().unwrap()].mode
    }
    pub(in crate::app) fn key(&mut self, key: &str) {
        self.application.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: KeyInputKind::Press,
        });
    }
    pub(in crate::app) fn advance_input(&mut self, elapsed: Duration) {
        self.clock.advance(elapsed);
        self.application.flush_keymap_timeout();
        self.application.update_key_hint_ui();
    }
    pub(in crate::app) fn focus_lost(&mut self) {
        self.application.handle_event(UiEvent::FocusLost);
    }
    pub(in crate::app) fn apply_candidate(&mut self, loaded: LoadedConfig) -> Result<(), String> {
        self.application
            .apply_configuration_candidate(loaded, false)
    }
    pub(in crate::app) fn reject_stale_contribution(&mut self) -> Result<(), String> {
        let candidate = self
            .application
            .configuration
            .prepare_builtin_rules(self.application.extension.contributions());
        self.application.configuration.toggle_inlay_hints();
        self.application.commit_contribution_candidate(candidate)
    }
    pub(in crate::app) fn window_policy_snapshot(
        &self,
    ) -> (
        crate::windows::WindowGraph,
        crate::config::WindowsConfig,
        u64,
    ) {
        (
            self.application.window_session.graph().clone(),
            self.application.configuration.settings().windows.clone(),
            self.application.configuration.binding_revision(),
        )
    }
    pub(in crate::app) fn time(&self) -> (Instant, std::time::SystemTime) {
        (self.clock.monotonic(), self.clock.system_time())
    }
    pub(in crate::app) fn quit_requested(&self) -> bool {
        self.application.should_quit
    }
    pub(in crate::app) fn projection(&self) -> &UiState {
        &self.application.ui
    }
    pub(in crate::app) fn prefix(&self) -> Option<&str> {
        self.application.input.mapping_prefix()
    }
    pub(in crate::app) fn documentation(&mut self, text: String) {
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
    pub(in crate::app) fn verify_open_lifetimes(&mut self) {
        use crate::app::tests::wait_vfs;
        use crate::editor::text::TextBuffer;
        use std::fs;
        let app = &mut self.application;
        let root = app.state.workspace_root.clone();
        let path = root.join("pending.txt");
        fs::write(&path, b"disk").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("disk"), Some(path.clone()))
            .unwrap();
        app.reconcile_editor_windows();
        let (worker, gate) = crate::files::VfsWorker::start_gated_for_test(2);
        app.documents = crate::app::documents::Documents::new(
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
        app.documents = crate::app::documents::Documents::new(
            worker,
            &StartupTarget::Dashboard { root: root.clone() },
            app.state.window_version(),
            app.workspace.root_revision(),
            2,
        );
        app.start_open(path.clone());
        let (other, view) = app
            .state
            .open_text(TextBuffer::from_text("other"), Some(root.join("other.txt")))
            .unwrap();
        app.reconcile_editor_windows();
        gate.send(()).unwrap();
        wait_vfs(app);
        assert_eq!(app.state.active_view(), Some(view), "late open stole focus");
        assert_eq!(app.state.active_buffer_id(), Some(other));
        assert!(
            app.state
                .buffers
                .values()
                .any(|b| b.path.as_ref() == Some(&path)),
            "valid background data discarded"
        );
    }
    pub(in crate::app) fn verify_save_collisions(&mut self) {
        use crate::app::tests::wait_vfs;
        use crate::editor::text::TextBuffer;
        use std::fs;
        let app = &mut self.application;
        let root = app.state.workspace_root.clone();
        let target = root.join("target.txt");
        fs::write(&target, b"target").unwrap();
        app.state
            .load_hidden_text(TextBuffer::from_text("target"), target.clone())
            .unwrap();
        let (source, _) = app
            .state
            .open_text(
                TextBuffer::from_text("source"),
                Some(root.join("source.txt")),
            )
            .unwrap();
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
    pub(in crate::app) fn verify_expired_shutdown_does_not_write(&mut self) {
        self.application.documents.stop_worker();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.application.documents.worker_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let result = self
            .application
            .finish_shutdown_before(Instant::now() - Duration::from_secs(1));
        // Other workers still running at the deadline (such as a Nix preparation) add their own
        // diagnostics, so the persistence failure may arrive inside a combined shutdown error.
        assert!(
            matches!(
                &result,
                Err(crate::app::InteractiveError::Vfs(
                    crate::files::VfsError::Io {
                        operation: "finish persistence shutdown" | "finish shutdown",
                        ..
                    }
                ))
            ) && result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("exit snapshot was not saved"),
            "unexpected shutdown result: {result:?}"
        );
        let accepted = self.application.persistence.pending();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.application.persistence.pending() {
            self.application.process_persistence_events();
            assert!(
                Instant::now() < deadline,
                "persistence worker did not drain"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!accepted, "expired shutdown accepted a persistence request");
        assert!(
            !self._state.path().join(STATE_FILE).exists(),
            "expired shutdown wrote an exit snapshot"
        );
    }
}

impl TestSession {
    pub(in crate::app) fn trust_owned_terminal_fixture(&mut self) {
        crate::app::tests::prepare_trust_identity(&mut self.application);
        for root in self.application.workspace.roots().to_vec() {
            self.application
                .workspace
                .fixture_trust_identity(root.identity);
        }
        self.application.reconcile_workspace_trust();
    }
    pub(in crate::app) fn terminal_request(&mut self, profile: &str) {
        self.application
            .process_effect(crate::app::commands::AppEffect::TerminalOpen(Some(
                profile.into(),
            )));
    }
    pub(in crate::app) fn terminal_open(&mut self, profile: &str) {
        self.terminal_request(profile);
    }
    pub(in crate::app) fn focus_only_terminal(&mut self) {
        let ids = self.application.terminal_session.descriptions();
        assert_eq!(ids.len(), 1);
        self.application
            .process_effect(crate::app::commands::AppEffect::TerminalFocus(ids[0].0));
    }
    pub(in crate::app) fn terminal_close(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::TerminalClose);
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
    pub(in crate::app) fn terminal_begin_close(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::TerminalClose);
        assert!(
            self.application
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::TerminalClose)
        );
    }
    pub(in crate::app) fn terminal_submit_close(&mut self) {
        self.application.submit_prompt();
    }
    pub(in crate::app) fn terminal_hide_and_show(&mut self) {
        self.terminal_begin_close();
        self.terminal_submit_close();
        self.application
            .process_effect(crate::app::commands::AppEffect::TerminalToggle);
    }
    pub(in crate::app) fn notification(&self) -> Option<&str> {
        self.application.ui.notification.as_deref()
    }
    pub(in crate::app) fn terminal_unfocus(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::TerminalUnfocus);
    }
    pub(in crate::app) fn terminal_send(&mut self, text: &str) {
        self.application
            .process_effect(crate::app::commands::AppEffect::TerminalSend(text.into()));
    }
    pub(in crate::app) fn clone_terminal(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::WindowClone {
                target: "terminal".into(),
                axis: crate::windows::TileAxis::Horizontal,
            });
    }
    pub(in crate::app) fn place_terminal(
        &mut self,
        placement: crate::windows::WindowPlacement,
    ) -> Result<(), String> {
        self.application.place_graph_window("terminal", placement)
    }
    pub(in crate::app) fn show_terminal(&mut self) -> Result<(), String> {
        self.application.show_graph_window("terminal")
    }
    pub(in crate::app) fn terminal_window_identity(
        &self,
    ) -> Option<(
        crate::windows::ContentId,
        crate::windows::ViewId,
        crate::windows::WindowId,
    )> {
        let id = self.application.displayed_terminal()?;
        let ids = self
            .application
            .window_session
            .native_ids()
            .terminal(id.0)?;
        Some((ids.content, ids.view, ids.window))
    }
    pub(in crate::app) fn terminal_geometry_source(
        &self,
    ) -> Option<crate::app::windows::TerminalGeometrySource> {
        let id = self.application.displayed_terminal()?;
        self.application
            .window_session
            .terminal_geometry_source(id.0)
    }
    pub(in crate::app) fn terminal_window_tab(&self) -> Option<crate::windows::WorkspaceTabId> {
        let (_, _, window) = self.terminal_window_identity()?;
        self.application
            .window_session
            .graph()
            .window(window)
            .map(|window| window.tab)
    }
    pub(in crate::app) fn terminal_mode(&self) -> Option<crate::ui::TerminalPanelMode> {
        let id = self.application.displayed_terminal()?;
        self.application.terminal_session.mode(id)
    }
    pub(in crate::app) fn terminal_cursor(&self) -> Option<(usize, usize)> {
        self.application
            .ui
            .terminal_panel
            .as_ref()
            .and_then(|panel| panel.cursor)
    }
    pub(in crate::app) fn terminal_clone_state(&self) -> TerminalCloneState {
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
    pub(in crate::app) fn pump_terminal(&mut self) {
        self.application.process_execution_preflight();
        self.application.process_pty_events();
    }
    pub(in crate::app) fn terminal_pending(&self) -> usize {
        self.application.terminal_session.pending()
    }
    pub(in crate::app) fn terminal_phase(&self) -> Option<crate::ui::TerminalPhase> {
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
    pub(in crate::app) fn terminal_exit_code(&self) -> Option<u32> {
        self.application
            .ui
            .terminal_panel
            .as_ref()
            .and_then(|panel| panel.exit_code)
    }
    pub(in crate::app) fn expire_shutdown_with_terminal(&mut self) -> Vec<String> {
        let _ = self
            .application
            .finish_shutdown_before(Instant::now() - Duration::from_millis(1));
        self.application
            .notices
            .history()
            .map(|(_, message)| message.clone())
            .collect()
    }
    pub(in crate::app) fn terminal_dimensions(&self) -> Option<(u16, u16)> {
        self.application.terminal_body_dimensions()
    }
    pub(in crate::app) fn terminal_screen_dimensions(&self) -> Option<(usize, usize)> {
        let id = self.application.displayed_terminal()?;
        Some(self.application.terminal_session.fixture_dimensions(id))
    }
    pub(in crate::app) fn stop_terminal_then_resize(&mut self) -> Result<(), String> {
        let id = self.application.displayed_terminal().expect("terminal");
        let source = self.terminal_geometry_source().expect("geometry");
        self.application.terminal_session.request_stop(id)?;
        self.application
            .terminal_session
            .resize(id, 70, 5, source, &self.application.workspace)
    }
    pub(in crate::app) fn terminal_pending_resizes(&self) -> Option<usize> {
        let id = self.application.displayed_terminal()?;
        Some(
            self.application
                .terminal_session
                .fixture_pending_resizes(id),
        )
    }
    pub(in crate::app) fn terminal_pending_resize_matches(
        &self,
        source: &crate::app::windows::TerminalGeometrySource,
    ) -> bool {
        self.application.displayed_terminal().is_some_and(|id| {
            self.application
                .terminal_session
                .fixture_pending_resize_matches(id, source)
        })
    }
    pub(in crate::app) fn terminal_screen_state(
        &self,
    ) -> Option<crate::app::terminal::TerminalScreenState> {
        let id = self.application.displayed_terminal()?;
        Some(self.application.terminal_session.fixture_screen_state(id))
    }
    pub(in crate::app) fn terminal_lines(&self) -> Vec<String> {
        self.application
            .displayed_terminal()
            .map(|id| self.application.terminal_session.fixture_lines(id))
            .unwrap_or_default()
    }
    pub(in crate::app) fn terminal_started_markers(&self) -> String {
        std::fs::read_to_string(self._workspace.path().join("starts")).unwrap_or_default()
    }
}

impl TestSession {
    #[cfg(unix)]
    pub(in crate::app) fn debug_program_fixture(&self) {
        use std::os::unix::fs::PermissionsExt;
        let path = self._workspace.path().join("debuggee");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    pub(in crate::app) fn debug_start_native_program_fixture(&mut self) {
        let source = self._workspace.path().join("main.rs");
        std::fs::write(&source, "fn main() {}\n").unwrap();
        self.application
            .state
            .open_text(
                crate::editor::text::TextBuffer::from_text("fn main() {}\n"),
                Some(source),
            )
            .unwrap();
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|d| d.id == "codelldb")
            .unwrap();
        self.application
            .tooling
            .fixture_retain(|entry| entry.definition.id != "codelldb");
        self.application
            .tooling
            .fixture_insert(crate::tools::ToolEntry {
                definition,
                status: crate::tools::ToolStatus::Installed(std::env::current_exe().unwrap()),
            });
        self.application.start_dap(
            Some("codelldb".into()),
            Some(self._workspace.path().join("debuggee")),
        );
    }
    pub(in crate::app) fn debug_program_pending(&self) -> bool {
        self.application.debugger_session.debug_program_pending()
    }
    pub(in crate::app) fn debug_program_wire_path(&self) -> bool {
        std::fs::read_to_string(self._workspace.path().join("launch-program"))
            .is_ok_and(|p| std::path::Path::new(&p) == self._workspace.path().join("debuggee"))
    }
    pub(in crate::app) fn debug_reverse_refusal(&self) -> bool {
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
    pub(in crate::app) fn debug_request(&mut self, name: &str) {
        self.application
            .process_effect(crate::app::commands::AppEffect::DapConfigurationStart(
                name.into(),
            ));
    }
    pub(in crate::app) fn debug_preflights(&self) -> usize {
        self.application.workspace.preflight_pending()
    }
    pub(in crate::app) fn debug_start(&mut self, name: &str) {
        self.application
            .process_effect(crate::app::commands::AppEffect::DapConfigurationStart(
                name.into(),
            ));
    }
    pub(in crate::app) fn debug_stop(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::DapStop);
    }
    pub(in crate::app) fn pump_debug(&mut self) {
        self.application.process_dap_project_root();
        self.application.process_execution_preflight();
        self.application.process_dap_events();
    }
    /// A stop fills the panes by itself; asking for the threads is what opens a picker.
    pub(in crate::app) fn open_debug_threads_after_stop(&mut self) {
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
    pub(in crate::app) fn debug_threads(&mut self) {
        self.application
            .process_effect(crate::app::commands::AppEffect::DapThreads);
    }
    pub(in crate::app) fn accept_debug_picker(&mut self) {
        self.application.accept_picker();
    }
    pub(in crate::app) fn debug_pending(&self) -> usize {
        self.application.debugger_session.pending()
    }
    pub(in crate::app) fn debug_source_pending(&self) -> bool {
        self.application.debugger_session.source_pending()
    }
    pub(in crate::app) fn debug_source_text(&self) -> Option<String> {
        let editor = &self.application.state;
        let buffer = &editor.buffers[editor.views[editor.active_view()?].buffer];
        buffer
            .is_virtual_read_only()
            .then(|| buffer.text.to_string())
    }
}
