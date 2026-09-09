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
    pub(super) fn reconcile_windows(&mut self) {
        self.application.reconcile_editor_windows();
    }
    pub(super) fn copy_counts(&self) -> (usize, usize, usize) {
        (
            crate::ui::window::WindowGraph::test_clone_count(),
            super::super::PluginWindowHost::test_clone_count(),
            self.application.window_session.content_builds(),
        )
    }
    pub(super) fn graph_revision(&self) -> u64 {
        self.application.window_session.graph().revision().unwrap()
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
            self.application.terminal_width,
            self.application.terminal_height,
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
    pub(super) fn terminal_finish_validation(&mut self) {
        super::wait_execution_validation(&mut self.application);
    }
    pub(super) fn terminal_open(&mut self, profile: &str) {
        self.terminal_request(profile);
        self.terminal_finish_validation();
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
    }
    pub(super) fn terminal_unfocus(&mut self) {
        self.application
            .process_effect(crate::editor::AppEffect::TerminalUnfocus);
    }
    pub(super) fn terminal_send(&mut self, text: &str) {
        self.application
            .process_effect(crate::editor::AppEffect::TerminalSend(text.into()));
    }
    pub(super) fn pump_terminal(&mut self) {
        self.application.process_execution_preflight();
        self.application.process_pty_events();
    }
    pub(super) fn terminal_pending(&self) -> usize {
        self.application.terminal_session.pending()
    }
    pub(super) fn terminal_dimensions(&self) -> Option<(u16, u16)> {
        self.application.terminal_body_dimensions()
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
        super::wait_execution_validation(&mut self.application);
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
