use super::{
    AppEffect, BufferRevision, ByteOffset, ByteRange, CommandInvocation, CommandValue,
    DiagnosticOwner, DocumentEvent, DocumentationMarkupKind, DocumentationPopupKind, Duration,
    EditorMode, EditorOperation, EditorTransaction, ExplorerMutation, HashSet, IndentError,
    Instant, InteractiveApplication, InteractiveError, KeyEvent, KeyInputKind, LoadedConfig,
    LspEvent, LspFeature, LspFeatureResult, LspLaunch, LspMarkupContent, LspMarkupKind,
    LspProgressKind, LspRuntimeSource, LspWorkspaceEdit, MappingResolution, MatcherSource,
    NativeSurfaceTarget, PersistedLocation, PickerAction, PickerItem, PickerItemId, PickerPayload,
    PickerSurfaceKind, PositionCodec, PositionEncoding, Precondition, PromptKind, PtyId,
    ResourceIntentKind, RestoreContext, RestoreUpdate, SyntaxDocument, SyntaxSupport,
    TerminalInput, TerminalPanelMode, TextBuffer, TextEdit, TextEditSet, UiEvent, UiFold,
    UiInlayHint, UiSyntaxSpan, UndoPolicy, VfsResult, WindowRect, command, discover_executable,
    language_for_document, mode_name, path_from_file_uri, rect_contains_point,
    terminal_tab_at_column,
};
use crate::editor::session::diagnostics::MAXIMUM_DIAGNOSTICS_PER_URI;
use crate::editor::session::lsp::{LSP_MAXIMUM_LOG_LINES, ProjectionChange};
use crate::ui::render::{TERMINAL_PANEL_MAXIMUM_HEIGHT, TERMINAL_PANEL_MINIMUM_HEIGHT};
use crate::ui::window::{
    PANE_DEFAULT_RATIO, PANE_MINIMUM_HEIGHT, PANE_MINIMUM_WIDTH, WindowPlacement,
};

impl InteractiveApplication {
    pub(super) fn begin_flash(&mut self) {
        self.reconcile_editor_windows();
        let Some(view) = self.state.active_view else {
            return;
        };
        self.flash = Some(super::super::flash::FlashState {
            view,
            buffer: self.state.views[view].buffer,
            source: self.state.buffers[self.state.views[view].buffer]
                .text
                .source_version(),
            query: String::new(),
            targets: Vec::new(),
        });
        self.sync_flash_ui();
    }

    /// While flash is active every key goes here: characters extend the query or pick a
    /// label, Backspace edits, and anything else cancels.
    fn handle_flash_key(&mut self, key: &KeyEvent) {
        let Some(mut flash) = self.flash.take() else {
            return;
        };
        if self.state.active_view != Some(flash.view)
            || !self
            .state
            .views
            .get(flash.view)
            .is_some_and(|view| view.buffer == flash.buffer)
            || !self
                .state
                .buffers
                .get(flash.buffer)
                .is_some_and(|buffer| buffer.text.source_version() == flash.source)
        {
            self.sync_flash_ui();
            return;
        }
        match key.code {
            crate::editor::input::KeyCode::Char(character)
                if !key.modifiers.control && !key.modifiers.alt =>
            {
                if let Some(target) = flash
                    .targets
                    .iter()
                    .find(|target| target.label == Some(character))
                {
                    self.flash_jump(flash.view, target.range.start);
                    return;
                }
                flash.query.push(character);
            }
            crate::editor::input::KeyCode::Backspace if !flash.query.is_empty() => {
                flash.query.pop();
            }
            _ => {
                self.sync_flash_ui();
                return;
            }
        }
        let Some(view) = self.state.views.get(flash.view) else {
            self.sync_flash_ui();
            return;
        };
        let visible = crate::ui::active_editor_pane_viewport(&self.state, &self.ui)
            .filter(|viewport| viewport.view == flash.view && crate::ui::editor_state_content_width(
                &self.state, &self.state.buffers[view.buffer].text,
                &self.configuration.settings().editor, viewport.width,
            ) > 0)
            .map(|viewport| {
                let text = &self.state.buffers[view.buffer].text;
                let wrap = self.configuration.settings().editor.wrap
                    && !self.ui.large_file_buffers.contains(&view.buffer);
                let rows = crate::ui::VisualRowModel::new_for_state(
                    &self.state, text, flash.view, &self.ui, self.configuration.settings(),
                    viewport.width, wrap,
                );
                rows.visible_text_ranges(
                    crate::ui::VisualRowAnchor { line: view.scroll_line, wrap_row: view.scroll_wrap_row },
                    usize::from(viewport.height), view.scroll_column,
                )
            }).unwrap_or_default();
        flash.targets = super::super::flash::flash_targets(
            &self.state.buffers[view.buffer].text,
            &visible,
            view.cursor,
            &flash.query,
        );
        match flash.targets.as_slice() {
            [] if !flash.query.is_empty() => {
                self.sync_flash_ui();
                self.notify_message(format!("no match for {}", flash.query), false);
            }
            [only] => self.flash_jump(flash.view, only.range.start),
            _ => {
                self.flash = Some(flash);
                self.sync_flash_ui();
            }
        }
    }

    fn flash_jump(&mut self, view: crate::editor::ViewId, target: ByteOffset) {
        self.record_jump();
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetCursor {
                view,
                position: target,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
        self.sync_flash_ui();
        self.ensure_cursor_visible();
    }

    pub(super) fn sync_flash_ui(&mut self) {
        if self.flash.as_ref().is_some_and(|flash| {
            self.state.active_view != Some(flash.view)
                || self.window_session.graph().focused()
                    .and_then(|window| self.window_session.editor_ids().pane_for_window(window))
                    .and_then(|pane| self.state.panes.get(pane))
                    .is_none_or(|pane| pane.view != flash.view)
        }) {
            self.flash = None;
        }
        self.ui.flash = self.flash.as_ref().map(|flash| crate::ui::FlashOverlay {
            view: flash.view,
            matches: flash.targets.iter().map(|target| target.range).collect(),
            labels: flash
                .targets
                .iter()
                .filter_map(|target| Some((target.range.start, target.label?)))
                .collect(),
        });
    }

    pub(super) fn sync_status_summary_ui(&mut self) {
        let branch = self.git.branch().map(str::to_owned);
        self.ui.set(branch, |ui| &mut ui.git_branch);
        let running = self.tasks.running_count();
        self.ui.set(running, |ui| &mut ui.running_tasks);
    }

    pub(super) fn toggle_buffer_pin(&mut self, buffer: crate::editor::BufferId) {
        if !self.state.buffers.contains_key(buffer) {
            return;
        }
        self.window_session.toggle_pin(buffer);
        self.reconcile_editor_windows();
    }

    pub(super) fn toggle_active_buffer_pin(&mut self) {
        if let Some(buffer) = self.state.active_buffer_id() {
            self.toggle_buffer_pin(buffer)
        }
    }

    pub(super) fn move_active_buffer_line(&mut self, direction: isize) {
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        if let Err(error) = self.window_session.move_content(buffer, direction) {
            self.show_error(error);
            return;
        }
        self.reconcile_editor_windows();
    }

    /// Dragging a docked terminal's inner edge resizes it: the border left of a right-hand
    /// terminal, or the top row of the bottom panel.
    fn handle_dock_drag(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        use crate::editor::input::TerminalMouseInput;
        use crate::ui::window::DockEdge;
        let docked = |this: &Self, id: PtyId| {
            let rect = this.terminal_rect(id)?;
            match this
                .current_native_placement(NativeSurfaceTarget::Terminal(id.0))
                .ok()?
            {
                WindowPlacement::Docked { edge, .. } => Some((edge, rect)),
                _ => None,
            }
        };
        match key.terminal_mouse() {
            Some(TerminalMouseInput::LeftDown) => {
                let edge_at = self
                    .shown_terminal_windows()
                    .into_iter()
                    .find_map(|(id, _)| {
                        let (edge, rect) = docked(self, id)?;
                        let on_edge = match edge {
                            DockEdge::Right => column == rect.x,
                            DockEdge::Left => column + 1 == rect.x.saturating_add(rect.width),
                            DockEdge::Bottom => row == rect.y,
                            DockEdge::Top => row + 1 == rect.y.saturating_add(rect.height),
                        } && rect_contains_point(rect, column, row);
                        on_edge.then_some((id, edge))
                    });
                self.dock_drag = edge_at.map(|(id, _)| id);
                // The bottom panel's top row holds its tabs, so a click there still picks one.
                edge_at.is_some_and(|(_, edge)| matches!(edge, DockEdge::Left | DockEdge::Right))
            }
            Some(TerminalMouseInput::LeftDrag) => {
                let Some(id) = self.dock_drag else {
                    return false;
                };
                let Some((edge, rect)) = docked(self, id) else {
                    self.dock_drag = None;
                    return false;
                };
                let (width, height) = (self.window_session.width(), self.window_session.height());
                let size = match edge {
                    DockEdge::Right => rect.x.saturating_add(rect.width).saturating_sub(column),
                    DockEdge::Left => column.saturating_add(1).saturating_sub(rect.x),
                    DockEdge::Bottom => rect.y.saturating_add(rect.height).saturating_sub(row),
                    DockEdge::Top => row.saturating_add(1).saturating_sub(rect.y),
                };
                let size = match edge {
                    DockEdge::Left | DockEdge::Right => {
                        size.clamp(20, width.saturating_sub(PANE_MINIMUM_WIDTH).max(20))
                    }
                    DockEdge::Top | DockEdge::Bottom => size.clamp(
                        crate::ui::render::TERMINAL_PANEL_MINIMUM_HEIGHT,
                        height.saturating_sub(PANE_MINIMUM_HEIGHT).max(4),
                    ),
                };
                if let Err(error) = self.apply_native_window_placement(
                    NativeSurfaceTarget::Terminal(id.0),
                    WindowPlacement::Docked { edge, size },
                ) {
                    self.show_error(error);
                }
                true
            }
            Some(TerminalMouseInput::LeftUp) => self.dock_drag.take().is_some(),
            _ => false,
        }
    }

    /// A click on a terminal shown beside the active one, such as a docked agent, gives it the
    /// keys; other mouse input over it goes nowhere.
    fn handle_other_terminal_mouse(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        if self.ui.picker.is_some() || self.input.prompt().is_some() {
            return false;
        }
        let active = self.displayed_terminal();
        let Some(id) = self.compositor_projection()
            .and_then(|projection| projection.hit_test(column, row))
            .and_then(|window| self.window_session.native_ids().target_for_window(window))
            .and_then(|target| match target {
                NativeSurfaceTarget::Terminal(id) => Some(PtyId(id)),
                _ => None,
            })
            .filter(|id| Some(*id) != active)
        else {
            return false;
        };
        if key.terminal_mouse() == Some(crate::editor::input::TerminalMouseInput::LeftDown) {
            self.process_effect(AppEffect::TerminalFocus(id.0));
        }
        true
    }

    /// Dragging the border between two editor windows resizes them, as in Neovim.
    fn handle_border_drag(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        use crate::editor::input::TerminalMouseInput;
        use crate::ui::window::TileAxis;
        let Some(projection) = self.compositor_projection() else {
            return false;
        };
        let pane_at = |x: u16, y: u16| {
            projection
                .windows
                .iter()
                .filter(|window| rect_contains_point(window.rect, x, y))
                .find_map(|window| {
                    self.window_session
                        .editor_ids()
                        .pane_for_window(window.window)
                })
        };
        match key.terminal_mouse() {
            Some(TerminalMouseInput::LeftDown) => {
                let sides = projection
                    .borders
                    .iter()
                    .find_map(|border| match border.axis {
                        TileAxis::Horizontal
                            if column == border.x
                                && (border.y..border.y.saturating_add(border.length))
                                    .contains(&row) =>
                        {
                            Some((
                                pane_at(column.wrapping_sub(1), row)?,
                                pane_at(column + 1, row)?,
                                true,
                            ))
                        }
                        TileAxis::Vertical
                            if row == border.y
                                && (border.x..border.x.saturating_add(border.length))
                                    .contains(&column) =>
                        {
                            Some((
                                pane_at(column, row.wrapping_sub(1))?,
                                pane_at(column, row + 1)?,
                                false,
                            ))
                        }
                        _ => None,
                    });
                self.border_drag = sides;
                sides.is_some()
            }
            Some(TerminalMouseInput::LeftDrag) => {
                let Some((first, second, side_by_side)) = self.border_drag else {
                    return false;
                };
                let Some(panes) = self.state.panes_of_split_between(first, second) else {
                    self.border_drag = None;
                    return false;
                };
                // Measure the split from the windows under it, then put the border at the mouse.
                let rects: Vec<WindowRect> = projection
                    .windows
                    .iter()
                    .filter(|window| {
                        self.window_session
                            .editor_ids()
                            .pane_for_window(window.window)
                            .is_some_and(|pane| panes.contains(&pane))
                    })
                    .map(|window| window.rect)
                    .collect();
                let (start, end, at) = if side_by_side {
                    (
                        rects.iter().map(|rect| rect.x).min().unwrap_or(0),
                        rects
                            .iter()
                            .map(|rect| rect.x + rect.width)
                            .max()
                            .unwrap_or(0),
                        column,
                    )
                } else {
                    (
                        rects.iter().map(|rect| rect.y).min().unwrap_or(0),
                        rects
                            .iter()
                            .map(|rect| rect.y + rect.height)
                            .max()
                            .unwrap_or(0),
                        row,
                    )
                };
                let available = u32::from(end.saturating_sub(start).saturating_sub(1)).max(1);
                let ratio = u32::from(at.saturating_sub(start)) * 1_000 / available;
                if self
                    .state
                    .set_split_ratio_between(first, second, u16::try_from(ratio).unwrap_or(900))
                    .is_ok()
                {
                    self.reconcile_editor_windows();
                }
                true
            }
            Some(TerminalMouseInput::LeftUp) => self.border_drag.take().is_some(),
            _ => false,
        }
    }

    fn handle_debug_tab_mouse(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        if key.terminal_mouse() != Some(crate::editor::input::TerminalMouseInput::LeftDown)
            || self.input.prompt().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
            || self.ui.settings.is_some()
            || self.ui.dashboard
            || self.state.active_view.is_none()
        {
            return false;
        }
        let (Some(bounds), Some(selected)) = (
            self.workspace_bounds(),
            self.ui.debug_panes.as_ref().map(|panes| panes.selected),
        ) else {
            return false;
        };
        let top =
            crate::ui::workspace_top_rows(&self.state, &self.ui, self.configuration.settings());
        let region = crate::ui::window::WindowRect {
            y: bounds.y.saturating_add(top),
            height: bounds.height.saturating_sub(top),
            ..bounds
        };
        let Some(pane) = crate::ui::debug_pane_tab_at(region, selected, column, row) else {
            return false;
        };
        if let Some(panes) = self.ui.debug_panes.as_mut() {
            panes.selected = pane;
        }
        true
    }

    pub(super) fn handle_content_tab_mouse(
        &mut self,
        key: &KeyEvent,
        column: u16,
        row: u16,
    ) -> bool {
        if self.input.prompt().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
            || self.ui.settings.is_some()
        {
            return false;
        }
        let Some(kind) = key.terminal_mouse() else {
            return false;
        };
        if kind == crate::editor::input::TerminalMouseInput::LeftDrag
            && let Some(last) = self.tab_drag
        {
            self.drag_content_tab(last, column);
            return true;
        }
        self.tab_drag = None;
        if !matches!(
            kind,
            crate::editor::input::TerminalMouseInput::LeftDown
                | crate::editor::input::TerminalMouseInput::RightDown
                | crate::editor::input::TerminalMouseInput::MiddleDown
        ) {
            return false;
        }
        let Some(bounds) = self.workspace_bounds() else {
            return false;
        };
        let tab_line = self.state.tabs.len() > 1;
        let buffer_line = self.configuration.settings().ui.buffer_line.enabled
            && (!self
                .configuration
                .settings()
                .ui
                .buffer_line
                .hide_when_single
                || self.ui.content_tabs.len() > 1);
        if !buffer_line
            || row != bounds.y.saturating_add(u16::from(tab_line))
            || column < bounds.x
            || column >= bounds.x.saturating_add(bounds.width)
        {
            return false;
        }
        let Some(identity) = crate::ui::content_tab_at_column(
            &self.ui.content_tabs,
            self.configuration.settings(),
            bounds.width,
            column - bounds.x,
        ) else {
            return false;
        };
        let entry = self
            .ui
            .content_tabs
            .iter()
            .find(|entry| entry.identity == identity)
            .cloned();
        let Some(buffer) = self
            .window_session
            .editor_ids()
            .buffer_for_content(identity.0)
        else {
            if kind == crate::editor::input::TerminalMouseInput::RightDown
                && entry.is_some_and(|entry| !entry.closeable)
            {
                self.show_error("content-line entry is not closeable");
                return true;
            }
            return false;
        };
        if kind == crate::editor::input::TerminalMouseInput::MiddleDown {
            self.toggle_buffer_pin(buffer);
            return true;
        }
        if self.state.show_buffer(buffer).is_err() {
            return false;
        }
        self.reconcile_editor_windows();
        if kind == crate::editor::input::TerminalMouseInput::LeftDown {
            self.tab_drag = Some(column);
        }
        if kind == crate::editor::input::TerminalMouseInput::RightDown {
            if entry.is_some_and(|entry| entry.closeable) {
                self.close_active_buffer(false)
            } else {
                self.show_error("content-line entry is not closeable")
            }
        }
        true
    }

    /// A dragged tab moves past the tab under the mouse. Only when the mouse is moving that
    /// way, because swapping tabs of different widths can leave it over the same tab.
    fn drag_content_tab(&mut self, last: u16, column: u16) {
        self.tab_drag = Some(column);
        let Some(bounds) = self.workspace_bounds() else {
            return;
        };
        let Some(over) = crate::ui::content_tab_at_column(
            &self.ui.content_tabs,
            self.configuration.settings(),
            bounds.width,
            column.saturating_sub(bounds.x),
        ) else {
            return;
        };
        let active = self.state.active_buffer_id();
        let ids = self.window_session.editor_ids();
        let tabs = &self.ui.content_tabs;
        let active = tabs
            .iter()
            .position(|entry| ids.buffer_for_content(entry.identity.0) == active);
        let over = tabs.iter().position(|entry| entry.identity == over);
        let (Some(active), Some(over)) = (active, over) else {
            return;
        };
        if over > active && column > last {
            self.move_active_buffer_line(1);
        } else if over < active && column < last {
            self.move_active_buffer_line(-1);
        }
    }

    pub(super) fn handle_event(&mut self, event: UiEvent) {
        if matches!(event, UiEvent::Key { .. }) {
            self.last_key_at = Some(Instant::now());
        }
        // Information has been read once the user moves on, and must not cover what comes next.
        // Errors and warnings keep their time, and everything stays in `:messages`.
        if matches!(
            event,
            UiEvent::Key {
                kind: KeyInputKind::Press | KeyInputKind::Repeat,
                ..
            }
        ) && self.ui.notification.is_some()
            && !self.ui.notification_error
            && !self.ui.notification_warning
        {
            self.clear_notice();
        }
        match event {
            UiEvent::Resize(width, height) => {
                self.window_session.observe_size((width, height));
                self.window_session.invalidate();
                self.project_window_geometry();
                self.window_session.invalidate_native();
                self.resize_active_terminal_to_panel();
                self.sync_terminal_panel();
                self.sync_explorer_ui();
                self.ensure_cursor_visible();
            }
            UiEvent::Key { key, kind } => {
                self.sync_flash_ui();
                match kind {
                    KeyInputKind::Press | KeyInputKind::Repeat if self.flash.is_some() => {
                        self.handle_flash_key(&key);
                    }
                    KeyInputKind::Press | KeyInputKind::Repeat => self.handle_configured_key(key),
                    KeyInputKind::Release => self.handle_key_in_context("key_release", key),
                }
            }
            UiEvent::Paste(text) => self.handle_configured_paste(text),
            UiEvent::FocusGained => {
                self.send_terminal_focus_report(true);
                self.handle_key_in_context("focus", KeyEvent::focus_gained())
            }
            UiEvent::FocusLost => {
                self.send_terminal_focus_report(false);
                if self.input.prompt().is_some_and(|prompt| {
                    matches!(
                        prompt.kind,
                        PromptKind::WorkspaceTrust
                            | PromptKind::GitReset
                            | PromptKind::ExplorerDelete
                            | PromptKind::ExplorerRename
                            | PromptKind::PluginConfirmation
                            | PromptKind::TerminalClose
                            | PromptKind::DirtyClose
                            | PromptKind::GitStageUnsaved
                            | PromptKind::GitDestructive
                    )
                }) {
                    self.close_prompt();
                }
                self.handle_key_in_context("focus", KeyEvent::focus_lost());
            }
            UiEvent::Mouse { key, column, row } => {
                self.input.observe_mouse(column, row);
                if self.ui.settings.is_some()
                    && self.ui.picker.is_none()
                    && self.input.prompt().is_none()
                {
                    self.settings_mouse(&key, column, row);
                    return;
                }
                let overlay = if self.ui.picker.is_some() {
                    Some("picker_mouse")
                } else if self.input.prompt().is_some() {
                    Some("prompt_mouse")
                } else if self.ui.tool_menu.is_some() {
                    Some("tool_manager_mouse")
                } else {
                    None
                };
                if let Some(context) = overlay {
                    self.handle_key_in_context(context, key);
                    return;
                }
                if self.input.documentation().is_some() {
                    match key.terminal_mouse() {
                        Some(crate::editor::input::TerminalMouseInput::ScrollUp) => {
                            self.process_effect(AppEffect::LspDocumentationScroll(-1));
                            return;
                        }
                        Some(crate::editor::input::TerminalMouseInput::ScrollDown) => {
                            self.process_effect(AppEffect::LspDocumentationScroll(1));
                            return;
                        }
                        _ => {}
                    }
                }
                let over_floating = self.compositor_projection().is_some_and(|projection| {
                    projection.hit_test(column, row)
                        .and_then(|window| projection.window(window))
                        .is_some_and(|window| matches!(window.layer,
                            crate::ui::window::CompositorLayer::Floating
                                | crate::ui::window::CompositorLayer::Fullscreen))
                });
                if (!over_floating
                    && (self.handle_debug_tab_mouse(&key, column, row)
                        || self.handle_content_tab_mouse(&key, column, row)
                        || self.handle_dock_drag(&key, column, row)
                        || self.handle_border_drag(&key, column, row)))
                    || self.handle_other_terminal_mouse(&key, column, row)
                {
                    return;
                }
                let hit = self.compositor_projection()
                    .and_then(|projection| projection.hit_test(column, row));
                let over_terminal = hit.is_some_and(|window| matches!(
                    self.window_session.native_ids().target_for_window(window),
                    Some(NativeSurfaceTarget::Terminal(_))
                ));
                let explorer_window = (!over_terminal)
                    .then(|| {
                        self.compositor_projection()
                            .and_then(|projection| projection.hit_test(column, row))
                    })
                    .flatten()
                    .filter(|window| {
                        matches!(
                            self.window_session.native_ids().target_for_window(*window),
                            Some(
                                NativeSurfaceTarget::Explorer
                                    | NativeSurfaceTarget::ExplorerView(_)
                            )
                        )
                    })
                    .or_else(|| {
                        (!over_floating && !over_terminal
                            && self
                                .explorer_window_rect()
                                .is_some_and(|rect| rect_contains_point(rect, column, row)))
                        .then(|| {
                            self.window_session
                                .native_ids()
                                .explorer()
                                .map(|ids| ids.window)
                        })
                        .flatten()
                    });
                let over_explorer = explorer_window.is_some();
                // Only a press moves focus. Terminals also report releases and plain pointer
                // motion, and those must not take the keyboard away from the user.
                let pressed = matches!(
                    key.code,
                    crate::editor::input::KeyCode::MouseLeftDown
                        | crate::editor::input::KeyCode::MouseRightDown
                        | crate::editor::input::KeyCode::MouseMiddleDown
                );
                if let Some(window) = explorer_window.filter(|_| pressed) {
                    // The click picks this row once focus has moved.
                    self.explorer
                        .set_clicked(self.explorer_row_at(window, column, row));
                    if let Ok(plan) = self.window_session.plan_focus(
                        window,
                        &self.state.window_version(),
                        self.extension.contributions().version(),
                    ) && self.apply_window_plan(plan).is_ok()
                    {
                        self.reconcile_editor_windows();
                    }
                }
                if over_terminal
                    && key.terminal_mouse()
                        == Some(crate::editor::input::TerminalMouseInput::LeftDown)
                    && let Some(rect) = self.terminal_window_rect()
                    && row == rect.y
                    && let Some(id) = self.ui.terminal_panel.as_ref().and_then(|terminal| {
                        terminal_tab_at_column(
                            terminal,
                            self.configuration.settings(),
                            rect.width,
                            column.saturating_sub(rect.x),
                        )
                    })
                {
                    self.process_effect(AppEffect::TerminalFocus(id));
                    return;
                }
                if over_explorer && let Some(mouse) = key.terminal_mouse() {
                    match mouse {
                        crate::editor::input::TerminalMouseInput::ScrollUp => {
                            self.move_explorer_selection(-3);
                            return;
                        }
                        crate::editor::input::TerminalMouseInput::ScrollDown => {
                            self.move_explorer_selection(3);
                            return;
                        }
                        _ => {}
                    }
                }
                if !over_terminal
                    && !over_explorer
                    && self.ui.picker.is_none()
                    && self.input.prompt().is_none()
                    && self.agent_panel_mouse(&key, column, row)
                {
                    return;
                }
                let plugin_context = self
                    .compositor_projection()
                    .and_then(|projection| projection.hit_test(column, row))
                    .and_then(|window| self.extension.window_context(window));
                if self.ui.picker.is_none()
                    && self.input.prompt().is_none()
                    && self.ui.tool_menu.is_none()
                    && let Some(context) = plugin_context
                {
                    if let Some(window) = self
                        .compositor_projection()
                        .and_then(|projection| projection.hit_test(column, row))
                        .filter(|_| pressed)
                        && self
                            .window_session
                            .plan_focus(
                                window,
                                &self.state.window_version(),
                                self.extension.contributions().version(),
                            )
                            .and_then(|plan| self.apply_window_plan(plan))
                            .is_ok()
                    {
                        self.reconcile_editor_windows();
                    }
                    self.handle_key_in_context(&context, key);
                    return;
                }
                let context = if self.ui.picker.is_some() {
                    "picker_mouse"
                } else if self.input.prompt().is_some() {
                    "prompt_mouse"
                } else if self.ui.tool_menu.is_some() {
                    "tool_manager_mouse"
                } else if over_terminal {
                    "terminal_mouse"
                } else if over_explorer {
                    "explorer_mouse"
                } else if self.window_session.dashboard_visible()
                    || self.state.active_view.is_none()
                {
                    "dashboard_mouse"
                } else {
                    if pressed {
                        self.set_explorer_focus(false);
                    }
                    "editor_mouse"
                };
                if context == "terminal_mouse"
                    && self.window_session.terminal_focused()
                    && let Some(id) = self.keyboard_terminal()
                    && self.terminal_session.mode(id) == Some(TerminalPanelMode::Input)
                    && let Some(window) = self
                        .window_session
                        .native_ids()
                        .terminal(id.0)
                        .and_then(|ids| self.compositor_projection()?.window(ids.window))
                    && let Some(geometry) = crate::ui::terminal_window_geometry(window)
                    && let Some(event) = key.terminal_mouse()
                {
                    let rect = WindowRect {
                        x: geometry.x,
                        y: geometry.body_top,
                        width: geometry.width,
                        height: geometry.body_height,
                    };
                    match self.terminal_session.send(
                        id,
                        TerminalInput::Mouse(event, column, row, rect),
                        &self.workspace,
                    ) {
                        Ok(true) => return,
                        Ok(false) => {}
                        Err(error) => {
                            self.show_error(error);
                            return;
                        }
                    }
                }
                self.handle_key_in_context(context, key);
            }
        }
    }

    pub(super) fn keyboard_terminal(&self) -> Option<PtyId> {
        if self.input.capture_context().is_some()
            || self.workspace_edits.proposal().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
            || self.ui.settings.is_some()
        {
            return None;
        }
        self.window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
    }
    pub(super) fn active_input_context(&self) -> String {
        if let Some(context) = self.input.capture_context() {
            return context.into();
        }
        if let Some(pending) = self.input.pending_context() {
            return match pending {
                crate::editor::input::PendingInputContext::Register => "register",
                crate::editor::input::PendingInputContext::Prefix => "prefix",
                crate::editor::input::PendingInputContext::Replace => "replace",
                crate::editor::input::PendingInputContext::Character => "character",
                crate::editor::input::PendingInputContext::MacroRecord => "macro_record",
                crate::editor::input::PendingInputContext::MacroReplay => "macro_replay",
            }
            .into();
        }
        if self.workspace_edits.proposal().is_some() {
            return "workspace_edit".into();
        }
        if self.ui.picker.is_some() {
            if self.search.theme_picker_key().is_some() {
                return "theme_picker".into();
            }
            return if self.input.picker_normal() {
                "picker_normal"
            } else {
                "picker"
            }
            .into();
        }
        if let Some(menu) = &self.ui.tool_menu {
            return if menu.searching {
                "tool_manager_search"
            } else {
                "tool_manager"
            }
            .into();
        }
        if let Some(page) = &self.ui.settings {
            return match &page.edit {
                Some(crate::ui::SettingsEdit::Text { .. }) => "settings_edit",
                Some(crate::ui::SettingsEdit::Choice { .. }) => "settings_choice",
                None if page.searching => "settings_search",
                None => "settings",
            }
            .into();
        }
        if self.keyboard_terminal().is_some() && self.terminal_window_rect().is_some() {
            return self
                .keyboard_terminal()
                .and_then(|id| self.terminal_session.mode(id))
                .map_or("terminal", |mode| match mode {
                    TerminalPanelMode::Input => "terminal",
                    TerminalPanelMode::Normal => "terminal_normal",
                    TerminalPanelMode::Visual => "terminal_visual",
                })
                .into();
        }
        if self.ui.language_completion.is_some() {
            return "completion".into();
        }
        if self.ui.explorer.is_some() && self.window_session.explorer_focused() {
            return if self.explorer.searching() {
                "explorer_search"
            } else {
                "explorer"
            }
            .into();
        }
        if let Some(context) = self
            .window_session
            .graph()
            .focused()
            .and_then(|window| self.extension.window_context(window))
        {
            return context;
        }
        if !self.window_session.dashboard_visible() && self.agent_input_focused() {
            return if self.agent_normal_mode() {
                "agent_normal"
            } else {
                "agent"
            }
            .into();
        }
        if self.window_session.dashboard_visible() || self.state.active_view.is_none() {
            return "dashboard".into();
        }
        let mode = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        if mode == EditorMode::Insert
            && self.input.snippet_buffer() == self.state.active_buffer_id()
            && self.input.snippet_buffer().is_some()
            && self.input.pending_context().is_none()
            && self.input.mapping_context().is_none_or(|context| context == "snippet")
        {
            return "snippet".into();
        }
        format!("editor.{}", mode_name(mode))
    }

    pub(super) fn send_terminal_focus_report(&mut self, focused: bool) {
        if let Some(id) = self.keyboard_terminal()
            && let Err(error) =
                self.terminal_session
                    .send(id, TerminalInput::Focus(focused), &self.workspace)
        {
            self.show_error(error);
        }
    }

    pub(super) fn handle_configured_key(&mut self, key: KeyEvent) {
        let context = self.active_input_context();
        self.handle_key_in_context(&context, key);
    }

    pub(super) fn handle_key_in_context(&mut self, context: &str, key: KeyEvent) {
        let scrolls_completion_documentation = context == "completion"
            && key.modifiers.control
            && matches!(key.code, crate::editor::input::KeyCode::Char('d' | 'u'));
        if context != "documentation" && !scrolls_completion_documentation {
            self.close_documentation();
        }
        self.ui.lsp_loading = None;
        self.lsp_asked = None;
        self.lsp_asked_generation = None;
        self.lsp_waiting = None;
        self.lsp_waiting_server = None;
        self.lsp_retry_at = None;
        self.lsp_retries = 0;
        if key.terminal_mouse().is_none() {
            self.diagnostics.dismiss_expansion();
            self.ui.expanded_diagnostic = None;
        }
        let context = context.to_owned();
        let resolution = self.input.resolve(
            &context,
            key.clone(),
            self.configuration.keymaps(),
            self.configuration.binding_revision(),
            self.clock.monotonic(),
            Duration::from_millis(self.configuration.settings().input.mapping_timeout_ms),
        );
        self.project_input_hints();
        match resolution {
            MappingResolution::Ready(invocation) => {
                self.invoke_mapped(invocation);
            }
            MappingResolution::ReadyAndReplay(invocation, replay) => {
                self.invoke_mapped(invocation);
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            // A key that finishes no sequence ends it, as in Vim: `]q` must not go on to run `q`,
            // and `<leader>gz` must not start an operator. While typing, all the keys are text,
            // so `ja` with `jk` mapped types `ja`.
            MappingResolution::Replay(prefix, replay) => {
                if context.starts_with("editor.") && !self.typing() {
                    self.input.cancel_pending();
                } else {
                    self.type_unmapped(&context, &prefix);
                    for key in replay {
                        self.handle_configured_key(key);
                    }
                }
            }
            MappingResolution::Disabled => {}
            MappingResolution::Pending | MappingResolution::Ambiguous => return,
            MappingResolution::Missing if context == "snippet" => {
                self.handle_key_in_context("editor.insert", key);
                return;
            }
            MappingResolution::Missing => {
                if !self.type_unmapped(&context, std::slice::from_ref(&key))
                    && let Some(command_id) = self
                        .configuration
                        .settings()
                        .input
                        .unmatched_commands
                        .get(&context)
                        .cloned()
                {
                    self.invoke_mapped_command(&command_id);
                    if self.active_input_context() != context {
                        self.handle_configured_key(key);
                    }
                }
            }
        }
        self.ensure_cursor_visible();
    }

    fn typing(&self) -> bool {
        self.state.active_view.is_some_and(|view| {
            matches!(
                self.state.views[view].mode,
                EditorMode::Insert | EditorMode::Replace
            )
        })
    }

    /// Types `keys` as text where `context` takes text, without looking them up again.
    /// Returns false when the context takes no text or a key has none.
    fn type_unmapped(&mut self, context: &str, keys: &[KeyEvent]) -> bool {
        let input = &self.configuration.settings().input;
        let context = if context == "snippet" { "editor.insert" } else { context };
        let Some(command_id) = input
            .character_fallback_commands
            .get(context)
            .or_else(|| input.text_commands.get(context))
            .cloned()
        else {
            return false;
        };
        let mut typed = true;
        for key in keys {
            match key.text_character() {
                Some(character) => self.invoke_text_command(&command_id, character.to_string()),
                None => typed = false,
            }
        }
        typed
    }

    pub(super) fn handle_configured_paste(&mut self, text: String) {
        self.close_documentation();
        if self.window_session.terminal_focused()
            && let Some(id) = self.keyboard_terminal()
            && self.terminal_session.mode(id) == Some(TerminalPanelMode::Input)
        {
            match self.terminal_session.send(id, TerminalInput::Paste(&text), &self.workspace) {
                Ok(true) => {}
                Ok(false) => self.show_error("terminal paste exceeds the input limit"),
                Err(error) => self.show_error(error),
            }
            return;
        }
        let context = self.active_input_context();
        if self.typing() && matches!(context.as_str(), "editor.insert" | "editor.replace" | "snippet") {
            match self.input.finish_mapping(&context, self.configuration.binding_revision()) {
                MappingResolution::Ready(invocation) => self.invoke_mapped(invocation),
                MappingResolution::ReadyAndReplay(invocation, replay) => {
                    self.invoke_mapped(invocation);
                    for key in replay {
                        self.handle_configured_key(key);
                    }
                }
                MappingResolution::Replay(prefix, replay) => {
                    self.type_unmapped(&context, &prefix);
                    self.type_unmapped(&context, &replay);
                }
                _ => {}
            }
        }
        self.cancel_pending_mapping();
        let context = self.active_input_context();
        let context = if context == "snippet" { "editor.insert" } else { &context };
        if let Some(command_id) = self
            .configuration
            .settings()
            .input
            .paste_commands
            .get(context)
            .cloned()
        {
            self.invoke_text_command(&command_id, text);
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn invoke_mapped_command(&mut self, command_id: &str) {
        match command(command_id) {
            Ok(invocation) => self.invoke_mapped(invocation),
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn invoke_text_command(&mut self, command_id: &str, text: String) {
        match command(command_id) {
            Ok(invocation) => {
                self.invoke(invocation.with_argument("text", CommandValue::String(text)))
            }
            Err(error) => self.show_error(error),
        }
    }

    #[cfg(test)]
    pub(super) fn invoke_insert_text(&mut self, text: String) {
        self.invoke_text_command("edit.insert_text", text);
    }

    pub(super) fn invoke_mapped(&mut self, mut invocation: CommandInvocation) {
        if !invocation.id.as_str().starts_with("input.") {
            self.input.decorate_invocation(&mut invocation);
        }
        self.invoke(invocation);
    }

    pub(super) fn cancel_pending_mapping(&mut self) {
        self.input.cancel_mapping();
        self.project_input_hints();
    }
    pub(super) fn flush_keymap_timeout(&mut self) {
        let context = self.active_input_context();
        let resolution = self.input.flush_timeout(
            &context,
            self.configuration.binding_revision(),
            self.clock.monotonic(),
        );
        self.project_input_hints();
        match resolution {
            MappingResolution::Ready(invocation) => self.invoke_mapped(invocation),
            MappingResolution::ReadyAndReplay(invocation, replay) => {
                self.invoke_mapped(invocation);
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            // A lone `j` with `jk` mapped is typed once the wait is over, as in Vim.
            MappingResolution::Replay(prefix, _)
                if !context.starts_with("editor.") || self.typing() =>
            {
                self.type_unmapped(&context, &prefix);
            }
            _ => return,
        }
        self.ensure_cursor_visible();
    }
    pub(super) fn update_key_hint_ui(&mut self) {
        let context = self.active_input_context();
        if self.input.prompt().is_some() || self.workspace_edits.proposal().is_some() {
            self.input.hide_hints();
        } else {
            self.input.update_hints(
                &context,
                (
                    self.configuration.keymaps(),
                    self.configuration.binding_revision(),
                ),
                &self.configuration.settings().ui.key_hints,
                &self.configuration.settings().command_labels,
                self.clock.monotonic(),
            );
        }
        self.project_input_hints();
    }

    /// Vim never leaves the Normal-mode cursor on the empty line after a final newline; `dd`
    /// on the last line lands on the line above instead.
    /// Where views stand on lines a formatter is replacing. The edit alone would send each to
    /// the end of what it replaced, so `restore_view_positions` puts them back.
    /// Each cursor as the count of non-blank characters before it, for views whose cursor line
    /// `replaced` says a reformat rewrites. Formatting mostly moves whitespace, so the cursor
    /// then stays on the same character, where a line and column would land on other code.
    pub(super) fn view_positions(
        &self,
        replaced: impl Fn(usize) -> bool,
    ) -> Vec<(
        crate::editor::ViewId,
        crate::editor::text::BufferRevision,
        usize,
        TextBuffer,
    )> {
        self.state
            .views
            .iter()
            .filter_map(|(id, view)| {
                let text = &self.state.buffers.get(view.buffer)?.text;
                if !replaced(text.byte_to_line(view.cursor).ok()?) {
                    return None;
                }
                let before = text
                    .slice(ByteRange::new(ByteOffset(0), view.cursor).ok()?)
                    .ok()?;
                let non_blank = before.chars().filter(|c| !c.is_whitespace()).count();
                Some((id, text.revision(), non_blank, text.clone()))
            })
            .collect()
    }
    pub(super) fn restore_view_positions(
        &mut self,
        positions: Vec<(
            crate::editor::ViewId,
            crate::editor::text::BufferRevision,
            usize,
            TextBuffer,
        )>,
    ) {
        for (id, revision, non_blank, content) in positions {
            let Some(view) = self.state.views.get(id) else {
                continue;
            };
            let text = &self.state.buffers[view.buffer].text;
            if text.revision() == revision {
                continue;
            }
            let text = text.to_string();
            // Punctuation and other code changes invalidate the ordinal. In that case keep
            // the cursor already mapped through the edit by the editor transaction.
            if !text.chars().filter(|c| !c.is_whitespace())
                .eq(content.to_string().chars().filter(|c| !c.is_whitespace()))
            {
                continue;
            }
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_operation(EditorOperation::SetCursor {
                    view: id,
                    position: ByteOffset(nth_non_blank(&text, non_blank)),
                })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            }
        }
        self.ensure_cursor_visible();
    }
    pub(super) fn keep_cursor_off_final_newline(&mut self) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let Some(view) = self.state.views.get(view_id) else {
            return;
        };
        if view.mode != EditorMode::Normal {
            return;
        }
        let text = &self.state.buffers[view.buffer].text;
        let last = text.visible_lines().saturating_sub(1);
        if text.byte_to_line(view.cursor).unwrap_or(0) <= last {
            return;
        }
        let Ok(start) = text.line_to_byte(last) else {
            return;
        };
        let indent = text.line(last).map_or(0, |line| {
            line.find(|character: char| !character.is_whitespace())
                .unwrap_or(0)
        });
        self.state.views[view_id].cursor = ByteOffset(start.get() + indent);
    }

    /// Whether yanks and pastes go through the system clipboard (`clipboard = "unnamedplus"`).
    pub(super) fn clipboard_shared(&self) -> bool {
        self.configuration.settings().editor.registers.clipboard
            == crate::config::ClipboardSync::Unnamedplus
    }

    pub(super) fn process_clipboard_pastes(&mut self) {
        let before = self.state.window_version();
        let pending = self.clipboard.pending();
        let registers_enabled = self.state.policy.registers_enabled;
        if let Some(invocation) = self.clipboard.finish_pull(
            &mut self.state.registers, registers_enabled, &before,
        ) {
            let succeeded = self.invoke_checked(invocation);
            self.clipboard.continue_pastes(&before, &self.state.window_version());
            if succeeded {
                self.continue_macro_replay();
            } else {
                self.cancel_macro_replay();
            }
        } else if pending && !self.clipboard.pending() {
            // The read was discarded because its original view changed.
            self.cancel_macro_replay();
        }
    }

    pub(super) fn invoke(&mut self, invocation: CommandInvocation) {
        self.invoke_checked(invocation);
    }
    pub(super) fn invoke_checked(&mut self, mut invocation: CommandInvocation) -> bool {
        if !invocation.id.as_str().starts_with("lsp.documentation.") {
            self.close_documentation();
        }
        if invocation.target.view.is_none() {
            invocation.target.view = self.state.active_view;
        }
        if invocation.target.buffer.is_none() {
            invocation.target.buffer = self.state.active_buffer_id();
        }
        if invocation.id.as_str() == "edit.put"
            && matches!(invocation.register, None | Some('"' | '+' | '*'))
            && self.clipboard_shared()
            && self
                .clipboard
                .pull(&invocation, self.state.window_version())
        {
            return true;
        }
        if invocation.id.as_str() == "edit.operator"
            && invocation.arguments.get("operator") == Some(&CommandValue::String("comment".into()))
            && !invocation.arguments.contains_key("comment")
        {
            let token = invocation
                .target
                .buffer
                .and_then(|id| self.state.buffers.get(id))
                .and_then(|buffer| buffer.path.as_deref())
                .and_then(|path| path.extension())
                .and_then(|extension| {
                    self.configuration
                        .settings()
                        .editor
                        .comments
                        .get(extension.to_string_lossy().as_ref())
                        .cloned()
                });
            if let Some(token) = token {
                invocation = invocation.with_argument("comment", CommandValue::String(token));
            }
        }
        // `cgn`, `dgn`: the operator acts on the next search match, selected as `gn` does, and
        // `.` repeats it on the match after that.
        let search_match = (invocation.id.as_str() == "edit.operator"
            && invocation.arguments.get("motion")
                == Some(&CommandValue::String("search_match".into())))
        .then(|| invocation.clone());
        if search_match.is_some() {
            if !self.select_next_search_match(1) {
                self.set_mode(EditorMode::Normal);
                return false;
            }
            invocation = invocation.with_argument("motion", CommandValue::String("visual".into()));
        }
        let visual_operator = invocation.id.as_str() == "edit.operator"
            && invocation
                .arguments
                .get("motion")
                .is_some_and(|motion| motion == &CommandValue::String("visual".into()));
        let suppress_repeat = visual_operator
            && invocation
                .arguments
                .get("operator")
                .is_some_and(|operator| operator == &CommandValue::String("yank".into()));
        let repeat_override = search_match.or_else(|| {
            visual_operator
                .then(|| self.normalized_visual_repeat(&invocation))
                .flatten()
        });
        // Long moves are jumps that `ctrl-o` comes back from, as in Vim.
        if invocation.id.as_str() == "view.cursor.move"
            && invocation.arguments.get("motion").is_some_and(|motion| {
                matches!(
                    motion,
                    CommandValue::String(motion) if matches!(
                        motion.as_str(),
                        "document_start" | "document_end" | "line_number" | "matching_pair"
                            | "paragraph_forward" | "paragraph_backward"
                    )
                )
            })
        {
            self.record_jump();
        }
        if invocation.id.as_str() == "edit.insert_text"
            && let Some(CommandValue::String(text)) = invocation.arguments.get("text")
        {
            self.language.note_typed(text);
        }
        match self.commands.execute(&self.state, &invocation) {
            Ok(outcome) => {
                let text_before = invocation.target.buffer.and_then(|buffer| {
                    self.state
                        .buffers
                        .get(buffer)
                        .map(|buffer| buffer.text.source_version())
                });
                if let Some(mut transaction) = outcome.transaction {
                    let source = self.state.window_version();
                    let snippet_document = self.input.snippet_buffer().and_then(|id| {
                        self.state.buffers.get(id).map(|buffer| {
                            crate::editor::session::input::SnippetDocument {
                                editor: &source,
                                buffer: id,
                                text: &buffer.text,
                                view: None,
                            }
                        })
                    });
                    let update = self
                        .input
                        .prepare_snippet_mirror(snippet_document, &mut transaction);
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error);
                        return false;
                    }
                    if let Some(update) = update {
                        let source = self.state.window_version();
                        let document = self.input.snippet_buffer().and_then(|id| {
                            self.state.buffers.get(id).map(|buffer| {
                                crate::editor::session::input::SnippetDocument {
                                    editor: &source,
                                    buffer: id,
                                    text: &buffer.text,
                                    view: None,
                                }
                            })
                        });
                        self.input.accept_snippet_edit(update, document);
                    }
                }

                self.keep_cursor_off_final_newline();
                if invocation.target.buffer.and_then(|buffer| {
                    self.state
                        .buffers
                        .get(buffer)
                        .map(|buffer| buffer.text.source_version())
                }) != text_before
                {
                    self.push_change_location();
                }
                self.input.record_successful_invocation(
                    &invocation,
                    repeat_override.as_ref(),
                    suppress_repeat,
                );
                self.tutor_observe(&invocation);
                for effect in outcome.effects {
                    self.process_effect(effect);
                }
                self.sync_flash_ui();
                true
            }
            Err(error) => {
                // A failed `dfz` or `di(` must not leave the view stuck in operator-pending.
                if self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view))
                    .is_some_and(|view| view.mode == EditorMode::OperatorPending)
                {
                    self.set_mode(EditorMode::Normal);
                }
                self.show_error(error);
                false
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_configuration_result(&mut self) {
        let result = match self.configuration.next_result() {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let automatic = result.automatic;
        let outcome = match result.candidate {
            Ok(loaded) => self.apply_configuration_candidate(loaded, automatic),
            Err(diagnostic) => Err(diagnostic),
        };
        match outcome {
            Ok(()) => self.configuration.note_rejected_keymaps(None),
            Err(diagnostic) => {
                self.retain_configuration_watch();
                self.configuration.note_rejected_keymaps(
                    diagnostic.contains("keymap").then(|| diagnostic.clone()),
                );
                let message = if automatic {
                    "auto_reload_rejected"
                } else {
                    "reload_rejected"
                };
                let message = self.message(message, &[("diagnostic", diagnostic)]);
                self.settings_status(message.clone(), true);
                self.show_error(message);
            }
        }
    }

    pub(in crate::editor::session) fn retain_configuration_watch(&self) {
        self.configuration.retain_watch();
    }

    pub(in crate::editor::session) fn apply_configuration_candidate(
        &mut self,
        loaded: LoadedConfig,
        automatic: bool,
    ) -> Result<(), String> {
        let mut warning = loaded.warning.clone();
        let candidate =
            self.configuration
                .prepare(loaded, &self.commands, self.extension.contributions())?;
        let window_candidate = (candidate.view().settings.windows
            != self.configuration.settings().windows)
            .then(|| self.prepare_configured_window_rules(&candidate.view().settings.windows))
            .transpose()?;
        self.configuration
            .stage_watch(&candidate, self.extension.contributions())?;
        if let Some(window_candidate) = window_candidate {
            self.window_session
                .accept_current(window_candidate, &self.state.window_version())?;
        }
        self.state.policy = candidate.view().settings.editor.editing_policy();
        self.state.mark_editor_changed();
        self.recovery.configure(
            &self.configuration.settings().recovery,
            &candidate.view().settings.recovery,
        );
        if self.window_session.dashboard_visible()
            && !candidate.view().settings.startup.show_dashboard
        {
            self.state.open_text(TextBuffer::from_text(""), None);
            self.set_dashboard_visibility(false);
        }
        let window_contribution = candidate.window_contribution();
        let receipt = self
            .configuration
            .commit(candidate, self.extension.contributions())?;
        let (rules, _) = self.extension.prepare_window_rules(&window_contribution)?;
        rules.finish(receipt)?;
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.sync_ui_diagnostics();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.retire_workspace_resources("configuration changed");
        self.retire_plugin_edit_sources();
        self.input.reconfigure();
        self.terminal_session
            .configure(&self.configuration.settings().terminals);
        self.restore_input_focus();
        self.project_configuration();
        let persistence = &self.configuration.settings().persistence;
        self.state.undo_directory = (persistence.enabled && persistence.undo_history)
            .then(|| self.persistence.undo_directory());
        self.project_input_hints();
        self.project_input_documentation();
        self.refresh_completions();
        let maximum_inlays = self.configuration.settings().lsp.inlay_hint_level
            == crate::config::InlayHintLevel::Max;
        let sessions = self
            .language
            .sessions()
            .iter()
            .map(|(key, state)| {
                (
                    key.clone(),
                    state.server_id.clone(),
                    state.launch.executable().clone(),
                )
            })
            .collect::<Vec<_>>();
        for (key, server_id, executable) in sessions {
            let mut configuration =
                crate::language::lsp::LspServerConfiguration::for_server(&server_id, &executable);
            if maximum_inlays {
                configuration.overlay(
                    crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(&server_id),
                );
            }
            if let Err(error) = self.language.configure(&key, configuration) {
                let diagnostic = format!("language-server configuration update failed: {error}");
                if let Some(warning) = &mut warning {
                    warning.push_str(&format!("; {diagnostic}"));
                } else {
                    warning = Some(diagnostic);
                }
            }
        }
        self.schedule_lsp_inlay_refresh(true);
        self.explorer.reset_watch_observation();
        self.reconcile_explorer_watch();
        self.window_session.constrain_terminal_height(
            TERMINAL_PANEL_MINIMUM_HEIGHT,
            TERMINAL_PANEL_MAXIMUM_HEIGHT,
        );
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.rebuild_matcher_cache();
        if let Some(menu) = &self.ui.tool_menu {
            let category = menu.category.clone();
            self.rebuild_tool_menu(&category, None);
        }
        self.ui.config_diagnostic = None;
        if let Some(warning) = warning {
            self.notify_warning(warning);
            if self.ui.settings.is_some() {
                self.refresh_settings();
            }
            self.ensure_cursor_visible();
            return Ok(());
        }
        // The Settings page shows the new values itself; a toast for each change is noise.
        if self.ui.settings.is_some() {
            self.refresh_settings();
            self.ensure_cursor_visible();
            return Ok(());
        }
        self.notify_message(
            if automatic {
                self.message("auto_reloaded", &[])
            } else {
                self.message("reloaded", &[])
            },
            false,
        );
        self.ensure_cursor_visible();
        Ok(())
    }
}

use std::ops::ControlFlow::{Break, Continue};

/// How often, and how far apart, a request answered "content modified" before its server
/// reported loading is sent again: rust-analyzer does this for a file opened a moment ago.
const LSP_CONTENT_MODIFIED_RETRIES: u8 = 10;
const LSP_CONTENT_MODIFIED_DELAY: Duration = Duration::from_millis(500);

enum NavigationRead {
    Matcher(MatcherSource),
    Persisted(PersistedLocation),
    Lsp(
        crate::language::lsp::LspRange,
        PositionEncoding,
        crate::editor::session::windows::NavigationStamp,
        crate::editor::session::workspace::WorkspaceVersion,
    ),
    WorkspaceSearch(std::ops::Range<usize>),
    /// A recovery journal's path and text, to put into the file once it is open.
    Recovered(std::path::PathBuf, String),
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn schedule_recovery_work(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            false,
            self.documents.recovery_io(),
        );
        if std::mem::take(&mut self.recovery.oversized_notice) {
            self.notify_warning(self.message("recovery_too_large", &[]));
        }
    }
    pub(in crate::editor::session) fn queue_dirty_recovery_before_exit(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            true,
            self.documents.recovery_io(),
        );
    }

    pub(in crate::editor::session) fn process_vfs_results(&mut self) {
        loop {
            let result = match self
                .documents
                .poll(&self.state.window_version(), self.workspace.root_revision())
            {
                Ok(Some(DocumentEvent::Legacy(result))) => result,
                Ok(Some(DocumentEvent::TaskRead(reply))) => {
                    if let Err(error) = self.tasks.accept_linter_read(
                        reply,
                        &self.state,
                        &self.workspace,
                        self.configuration.binding_revision(),
                        &self.diagnostics,
                    ) {
                        self.show_error(error);
                    }
                    self.drain_linter_sources();
                    continue;
                }
                Ok(Some(DocumentEvent::WorkspaceIo(reply))) => {
                    self.process_workspace_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::Explorer(reply))) => {
                    self.process_explorer_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::File(reply))) => {
                    self.process_file_reply(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::Stopped {
                    requests,
                    error,
                    task_reads,
                })) => {
                    for id in requests {
                        self.input.retire_document_request(id);
                        self.search.retire_document_request(id);
                        self.language.retire_document_request(id);
                        self.persistence.retire_document_request(id);
                        self.navigation_reads.remove(&id);
                        self.recovery.take_read(id, self.workspace.version());
                    }
                    self.recovery.stopped();
                    self.search.clear_preview();
                    self.explorer.retire_intents();
                    self.persistence.restore_transport_stopped();
                    if self.workspace_edits.proposal().is_some() || self.workspace_edits.preparing()
                    {
                        self.reject_workspace_proposal();
                    }
                    self.finish_workspace_resource_obligations("document worker stopped");
                    self.tasks.linter_worker_stopped(task_reads);
                    self.drain_linter_sources();
                    self.show_error(error);
                    break;
                }
                Ok(Some(DocumentEvent::Discarded)) => continue,
                Ok(Some(DocumentEvent::Startup {
                    receipt,
                    path,
                    text,
                })) => {
                    let buffer = match text
                        .and_then(|text| self.open_startup_document(text, path.clone()))
                    {
                        Ok(buffer) => Some(buffer),
                        Err(error) => {
                            self.show_error(error);
                            None
                        }
                    };
                    self.documents.finish_startup(
                        receipt,
                        buffer,
                        &self.state.window_version(),
                        self.workspace.root_revision(),
                    );
                    self.finish_startup_reads_if_complete();
                    continue;
                }
                Ok(None) => break,
                Err(error) => {
                    self.show_error(error);
                    break;
                }
            };
            let result = match self.persistence.accept_restore(
                result,
                RestoreContext {
                    editor: &self.state,
                    workspace: &self.workspace,
                    input: &self.input,
                    windows: &self.window_session,
                    configuration: self.configuration.binding_revision(),
                },
            ) {
                Break(RestoreUpdate::Waiting) => continue,
                Break(RestoreUpdate::Ready(ready)) => {
                    self.finish_session_restore(*ready);
                    continue;
                }
                Break(RestoreUpdate::Abandoned) => {
                    self.cancel_session_restore();
                    continue;
                }
                Break(RestoreUpdate::Failed(error)) => {
                    self.cancel_session_restore();
                    self.show_error(error);
                    continue;
                }
                Continue(result) => result,
            };
            let result = match self.recovery.accept(result) {
                Break(update) => {
                    use crate::editor::session::recovery::RecoveryUpdate;
                    match update {
                        RecoveryUpdate::Quiet | RecoveryUpdate::Pruned(0) => {}
                        RecoveryUpdate::Error(error) => self.show_error(error),
                        RecoveryUpdate::Pruned(count) => self.notify_message(
                            self.message("recovery_pruned", &[("count", count.to_string())]),
                            false,
                        ),
                        RecoveryUpdate::Discarded => {
                            if self.recovery_picker_open() {
                                self.open_recovery_picker();
                            }
                            self.notify_message(self.message("recovery_discarded", &[]), false);
                        }
                        RecoveryUpdate::CrashCleared => {
                            self.notify_message(self.message("crash_cleared", &[]), false)
                        }
                    }
                    continue;
                }
                Continue(result) => result,
            };
            match result {
                VfsResult::RecoveryDiscovered { .. }
                | VfsResult::RecoveryCleaned { .. }
                | VfsResult::RecoveryWritten { .. } => {
                    self.show_error(self.message("unexpected_completion", &[]))
                }
                VfsResult::Deleted { result, .. } => match result {
                    Err(error) => self.show_error(error),
                    Ok(()) => self.show_error(self.message("unexpected_completion", &[])),
                },
                VfsResult::ExplorerMutated { .. } => {
                    self.show_error("unowned explorer mutation result")
                }
                VfsResult::ResourceApplied { .. }
                | VfsResult::ResourceResolved { .. }
                | VfsResult::CreateTarget { .. } => self.show_error("unowned workspace I/O result"),
                VfsResult::Read { id, path, result } => {
                    if self.search.accept_preview(
                        id,
                        &result,
                        self.workspace.version(),
                        self.configuration.binding_revision(),
                    ) {
                        self.sync_picker_ui();
                        continue;
                    }
                    let pending = self
                        .search
                        .take_matcher_navigation(id)
                        .map(NavigationRead::Matcher)
                        .or_else(|| {
                            self.persistence
                                .take_navigation_read(id)
                                .map(NavigationRead::Persisted)
                        })
                        .or_else(|| {
                            self.language.take_navigation_read(id).map(
                                |(range, encoding, navigation, workspace)| {
                                    NavigationRead::Lsp(range, encoding, navigation, workspace)
                                },
                            )
                        })
                        .or_else(|| {
                            self.search
                                .take_workspace_navigation(id)
                                .map(NavigationRead::WorkspaceSearch)
                        })
                        .or_else(|| {
                            self.recovery.take_read(id, self.workspace.version()).map(
                                |(journal, content)| NavigationRead::Recovered(journal, content),
                            )
                        });
                    if let Some((navigation, capture)) = self.navigation_reads.remove(&id)
                        && (!self.window_session.navigation_current(
                            &navigation,
                            &self.state.window_version(),
                            self.state.active_view,
                        ) || !self.input.capture_current(&capture))
                    {
                        continue;
                    }
                    if let Some(NavigationRead::Recovered(journal, content)) = &pending {
                        // A read may finish after another command opened this file. Never
                        // replace that buffer, and never bind a rejected path to recovered text.
                        let already_open = self.state.buffers.values()
                            .any(|buffer| buffer.path.as_ref() == Some(&path));
                        let missing = matches!(&result, Err(crate::files::VfsError::Io {
                            operation: "open for reading", source, ..
                        }) if source.kind() == std::io::ErrorKind::NotFound);
                        let writable = self.workspace.require_path(&path).is_ok();
                        let (text, destination) = match result {
                            Ok(bytes) if !already_open && writable => {
                                match TextBuffer::from_bytes(&bytes) {
                                    Ok(text) => (text, Some(path)),
                                    Err(_) => (TextBuffer::from_text(""), None),
                                }
                            }
                            Err(_) if missing && !already_open && writable =>
                                (TextBuffer::from_text(""), Some(path)),
                            _ => (TextBuffer::from_text(""), None),
                        };
                        if let Some(path) = &destination {
                            self.record_recent_file(path.clone());
                        }
                        let (buffer, _) = self.state.open_text(text, destination);
                        if let Err(error) = self.replace_with_recovered(buffer, content.clone()) {
                            self.show_error(error);
                            continue;
                        }
                        self.finish_recovery(journal, "recovery_restored");
                        continue;
                    }
                    match result.map_err(InteractiveError::from).and_then(|bytes| {
                        TextBuffer::from_bytes(&bytes).map_err(InteractiveError::from)
                    }) {
                        Ok(text) => match pending {
                            Some(NavigationRead::Matcher(source)) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                self.state.buffers[buffer].read_only =
                                    self.workspace.require_path(&path).is_err();
                                let start = self.state.buffers[buffer]
                                    .text
                                    .line_to_byte(source.line)
                                    .ok()
                                    .map(|offset| {
                                        ByteOffset(
                                            (offset.get() + source.column)
                                                .min(self.state.buffers[buffer].text.len_bytes()),
                                        )
                                    })
                                    .filter(|offset| {
                                        self.state.buffers[buffer]
                                            .text
                                            .validate_offset(*offset)
                                            .is_ok()
                                    })
                                    .unwrap_or(ByteOffset(0));
                                self.state.views[view].cursor = start;
                                self.state.mark_editor_changed();
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Persisted(location)) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let length = self.state.buffers[buffer].text.len_bytes();
                                let clamp = |offset: usize| {
                                    let mut offset = offset.min(length);
                                    while !self.state.buffers[buffer]
                                        .text
                                        .is_char_boundary(ByteOffset(offset))
                                    {
                                        offset -= 1;
                                    }
                                    ByteOffset(offset)
                                };
                                let cursor = clamp(location.byte);
                                let anchor = clamp(location.anchor);
                                let selection =
                                    (anchor != cursor).then_some(crate::editor::Selection {
                                        anchor,
                                        head: cursor,
                                        kind: crate::editor::SelectionKind::Character,
                                    });
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: cursor,
                                    })
                                    .with_operation(EditorOperation::SetSelection {
                                        view,
                                        selection,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                    continue;
                                }
                                self.state.views[view].scroll_line = location.scroll_line.min(
                                    self.state.buffers[buffer]
                                        .text
                                        .len_lines()
                                        .saturating_sub(1),
                                );
                                self.state.views[view].scroll_wrap_row = 0;
                                self.state.views[view].scroll_column = location.scroll_column;
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Lsp(range, encoding, navigation, workspace)) => {
                                if workspace != self.workspace.version() {
                                    continue;
                                }
                                if !self.window_session.navigation_current(
                                    &navigation,
                                    &self.state.window_version(),
                                    self.state.active_view,
                                ) {
                                    let mut draft = self.state.draft();
                                    let buffer = draft.load_document(text, path.clone());
                                    if let Err(error) = self.commit_editor_draft(draft, None, false)
                                    {
                                        self.show_error(error);
                                    }
                                    if let Some(buffer) = self.state.buffers.get_mut(buffer) {
                                        buffer.read_only =
                                            self.workspace.require_path(&path).is_err();
                                    }
                                    continue;
                                }
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                self.state.buffers[buffer].read_only =
                                    self.workspace.require_path(&path).is_err();
                                let position = self.state.buffers.get(buffer).and_then(|buffer| {
                                    PositionCodec::new(&buffer.text)
                                        .lsp_to_byte(
                                            range.start.line,
                                            range.start.character,
                                            encoding,
                                        )
                                        .ok()
                                });
                                if let Some(position) = position {
                                    let transaction = EditorTransaction::new(self.transaction_id())
                                        .with_precondition(Precondition::ViewDisplays {
                                            view,
                                            buffer,
                                        })
                                        .with_operation(EditorOperation::SetCursor {
                                            view,
                                            position,
                                        })
                                        .with_undo_policy(UndoPolicy::Ignore);
                                    if let Err(error) = self.state.apply_transaction(&transaction) {
                                        self.show_error(error);
                                    }
                                } else {
                                    self.show_error(self.message("lsp_invalid_position", &[]));
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::WorkspaceSearch(range)) => {
                                let start = ByteOffset(range.start);
                                let end = ByteOffset(range.end);
                                if text.validate_offset(start).is_err()
                                    || text.validate_offset(end).is_err()
                                {
                                    self.show_error(
                                        self.message("workspace_search_stale_result", &[]),
                                    );
                                    continue;
                                }
                                let (buffer, view) = self.state.open_text(text, Some(path));
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_precondition(Precondition::ViewDisplays { view, buffer })
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: start,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            _ => self.show_error(self.message("unexpected_read", &[])),
                        },
                        Err(error) => self.show_error(error),
                    }
                }
                VfsResult::Directory {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.input.take_completion_read(id);
                    match (pending, result) {
                        (Some(key), _) if !self.input.completion_current(&key) => {
                            self.refresh_completions();
                        }
                        (Some(key), Ok(paths)) => {
                            self.input.accept_prompt_paths(
                                &key,
                                paths,
                                &self.state.workspace_root,
                                self.configuration.settings(),
                            );
                            self.project_input_prompt();
                        }
                        (Some(_), Err(_)) => {}
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Write { result, .. } => {
                    if let Err(error) = result {
                        self.show_error(error);
                    } else {
                        self.show_error(self.message("unexpected_completion", &[]));
                    }
                }
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_explorer_io(
        &mut self,
        reply: crate::files::ExplorerIoReply,
    ) {
        use crate::editor::session::explorer::MutationUpdate;
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
                draft.open_document(text, path);
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

impl InteractiveApplication {
    fn process_workspace_io(&mut self, reply: crate::editor::session::documents::WorkspaceIoReply) {
        if self.workspace_edits.owns_preparation_reply(&reply) {
            if let VfsResult::ResourceResolved {
                result: Ok(intent), ..
            } = reply.result()
            {
                let target = match &intent.kind {
                    ResourceIntentKind::Rename { target, .. } => Some(target.as_path()),
                    ResourceIntentKind::Delete => None,
                };
                if let Err(error) = self.state.check_resource_destination(target) {
                    self.abort_workspace_edit(error);
                    return;
                }
            }
            match self.workspace_edits.accept_preparation_reply(
                reply,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            ) {
                Ok(()) => {
                    if self.workspace_edits.files_ready() {
                        self.finish_workspace_proposal();
                    }
                }
                Err(error) => self.abort_workspace_edit(error),
            }
            return;
        }

        let Some(proposal) = self.workspace_edits.resource_proposal(&reply) else {
            return;
        };
        let Some(result) = self.workspace_edits.resource_finished(
            reply,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        let request = match result {
            Ok(request) => request,
            Err(error) => {
                self.language.cancel_deferred_edits(Some(proposal));
                self.show_error(error);
                return;
            }
        };
        let (change, target) = request.into_change();
        match self.state.reconcile_paths(change, target.as_deref()) {
            Ok((draft, recent)) => {
                if let Some(draft) = draft
                    && let Err(error) = self.commit_editor_draft(draft, None, false)
                {
                    self.language.cancel_deferred_edits(Some(proposal));
                    self.show_error(error);
                    return;
                }
                for path in recent {
                    self.record_recent_file(path);
                }
            }
            Err(error) => {
                self.language.cancel_deferred_edits(Some(proposal));
                self.show_error(error);
                return;
            }
        }
        if !self.workspace_edits.resources_pending(proposal)
            && let Err(error) = self.language.finish_deferred_edit(proposal, &self.state)
        {
            self.show_error(error);
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn retire_workspace_resources(&mut self, reason: &str) {
        self.language.cancel_deferred_edits(None);
        let (notices, requests) = self.workspace_edits.retire_resources(reason);
        for notice in notices {
            self.show_error(notice);
        }
        for id in requests {
            if let Some(cancelled) = self.documents.cancel_workspace_queued(id) {
                if let Some(notice) = self.workspace_edits.resource_cancelled(cancelled) {
                    self.show_error(notice);
                }
            } else {
                self.show_error(
                    "resource publication retired; its filesystem outcome is still pending",
                );
            }
        }
    }
    pub(in crate::editor::session) fn finish_workspace_resource_obligations(
        &mut self,
        reason: &str,
    ) {
        self.language.cancel_deferred_edits(None);
        for notice in self.workspace_edits.finish_resources(reason) {
            self.show_error(notice);
        }
    }
}

fn explorer_delete_message(path: &std::path::Path) -> String {
    format!("Permanently delete {path:?}?")
}

#[cfg(all(test, unix))]
#[test]
fn explorer_delete_message_uses_truthful_path_debug_escaping() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
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

impl InteractiveApplication {
    pub(super) fn syntax_support_label(&self, support: SyntaxSupport) -> String {
        self.message(
            match support {
                SyntaxSupport::Full => "syntax_support_full",
                SyntaxSupport::Partial => "syntax_support_partial",
                SyntaxSupport::Unsupported => "syntax_support_unsupported",
            },
            &[],
        )
    }

    pub(super) fn apply_syntax_indent_line(&mut self) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            return;
        };
        let Some(language) = language_for_document(
            buffer.path.as_deref(),
            buffer.text.chunks().next().unwrap_or_default(),
        ) else {
            self.show_error(self.message("syntax_indent_unavailable", &[]));
            return;
        };
        let revision = buffer.text.revision();
        let line = match buffer.text.byte_to_line(cursor) {
            Ok(line) => line,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let levels = match self.syntax.indentation(buffer_id, revision, language, line) {
            Ok(levels) => levels,
            Err(error) => {
                let message = match error {
                    IndentError::Pending => self.message("syntax_indent_pending", &[]),
                    IndentError::Unsupported => self.message(
                        "syntax_indent_unsupported",
                        &[("language", language.name().to_owned())],
                    ),
                    IndentError::Truncated => self.message("syntax_indent_truncated", &[]),
                };
                self.show_error(message);
                return;
            }
        };
        let desired = if self.state.policy.expand_tabs {
            " ".repeat(levels.saturating_mul(self.state.policy.indent_width))
        } else {
            "\t".repeat(levels)
        };
        let text = match buffer.text.line(line) {
            Ok(text) => text,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let whitespace = text
            .bytes()
            .take_while(|byte| matches!(*byte, b' ' | b'\t'))
            .count();
        if text.get(..whitespace) == Some(desired.as_str()) {
            self.notify_message(self.message("syntax_indent_unchanged", &[]), false);
            return;
        }
        let start = match buffer.text.line_to_byte(line) {
            Ok(start) => start,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let end = ByteOffset(start.get().saturating_add(whitespace));
        let edits = TextEditSet::new(
            revision,
            vec![TextEdit::new(
                ByteRange::new(start, end).expect("ordered indentation"),
                desired,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error)
        } else {
            self.notify_message(self.message("syntax_indent_applied", &[]), false);
        }
    }

    pub(super) fn process_syntax(&mut self) {
        // Move old fold rows with their text before matching them to a new parse.
        for (fold, source) in self.ui.syntax_folds.iter_mut()
            .zip(&mut self.syntax_fold_positions)
        {
            let Some((buffer_id, version, revision, range)) = source else { continue };
            let Some(buffer) = self.state.buffers.get(*buffer_id).filter(|buffer| {
                self.state.views.get(fold.view).is_some_and(|view| view.buffer == *buffer_id)
                    && version.same_incarnation(&buffer.text.source_version())
            }) else {
                fold.collapsed = false;
                continue;
            };
            if let Some(mapped) = buffer.text.map_range_since(*revision, *range) {
                fold.start_line = buffer.text.byte_to_line(mapped.start).unwrap_or(fold.start_line);
                fold.end_line = buffer.text.byte_to_line(mapped.end).unwrap_or(fold.end_line);
                *range = mapped;
                *version = buffer.text.source_version();
                *revision = buffer.text.revision();
            }
        }
        let documents = self
            .state
            .buffers
            .iter()
            .map(|(buffer, state)| SyntaxDocument {
                buffer,
                text: &state.text,
                language: language_for_document(
                    state.path.as_deref(),
                    state.text.chunks().next().unwrap_or_default(),
                ),
                enabled: !self.effective_large_file(buffer),
            })
            .collect::<Vec<_>>();
        let too_large = self.message("syntax_source_too_large", &[]);
        self.syntax.tick(
            &documents,
            &self.configuration.settings().syntax,
            self.clock.monotonic(),
            &too_large,
        );
        for (buffer, result) in self.syntax.results() {
            let revision = BufferRevision(result.revision);
            let stale = self.state.buffers.get(*buffer).is_some_and(|target| {
                self.syntax.source(*buffer) == Some(&target.text.source_version())
                    && !self.syntax_spans_source.0.contains(&(*buffer, result.generation, result.revision))
            });
            if stale {
                self.state.buffers[*buffer].syntax_objects =
                    Some((revision, result.objects.clone()));
            }
        }
        // Folds and spans are rebuilt only when a parse result or the views changed, not on
        // every tick.
        let source = (
            self.syntax
                .results()
                .map(|(buffer, result)| (*buffer, result.generation, result.revision))
                .collect::<Vec<_>>(),
            self.state
                .views
                .iter()
                .map(|(view_id, view)| (view_id, view.buffer))
                .collect::<Vec<_>>(),
        );
        if source != self.syntax_spans_source {
            self.syntax_spans_source = source;
            self.rebuild_syntax_rows();
        }
        // Until a new parse or token reply arrives, the old colours follow the edit instead of
        // dropping out, so typed text doesn't flash plain and back.
        move_spans_onto_current_text(&mut self.ui.syntax_spans, &self.state);
        move_spans_onto_current_text(&mut self.ui.lsp_semantic_spans, &self.state);
    }

    fn rebuild_syntax_rows(&mut self) {
        let collapsed = self
            .ui
            .syntax_folds
            .iter()
            .filter(|fold| fold.collapsed)
            .map(|fold| (fold.view, fold.start_line, fold.end_line))
            .collect::<HashSet<_>>();
        let mut syntax_folds = Vec::new();
        for (view_id, view) in self.state.views.iter() {
            if let Some(result) = self.syntax.result(view.buffer) {
                let text = &self.state.buffers[view.buffer].text;
                let Some(parsed) = text.text_at(BufferRevision(result.revision)) else { continue };
                for fold in &result.folds {
                    let range = parsed.line_to_byte(fold.start_row).ok().zip(
                        parsed.line_to_byte(fold.end_row).ok(),
                    ).and_then(|(start, end)| ByteRange::new(start, end).ok())
                        .and_then(|range| text.map_range_since(BufferRevision(result.revision), range));
                    let Some((start_line, end_line)) = range.and_then(|range| {
                        Some((text.byte_to_line(range.start).ok()?, text.byte_to_line(range.end).ok()?))
                    }) else { continue };
                    syntax_folds.push(UiFold {
                        view: view_id,
                        start_line,
                        end_line,
                        collapsed: collapsed.contains(&(view_id, start_line, end_line)),
                    });
                }
            }
        }
        self.syntax_fold_positions = syntax_folds.iter().map(|fold| {
            let buffer = self.state.buffers.get(self.state.views.get(fold.view)?.buffer)?;
            Some((self.state.views.get(fold.view)?.buffer, buffer.text.source_version(), buffer.text.revision(), ByteRange::new(
                buffer.text.line_to_byte(fold.start_line).ok()?,
                buffer.text.line_to_byte(fold.end_line).ok()?,
            ).ok()?))
        }).collect();
        self.ui.set(syntax_folds, |ui| &mut ui.syntax_folds);
        self.ui.syntax_spans = self
            .syntax
            .results()
            .flat_map(|(buffer, result)| {
                result.spans.iter().filter_map(|span| {
                    Some(UiSyntaxSpan {
                        buffer: *buffer,
                        revision: BufferRevision(result.revision),
                        range: ByteRange::new(ByteOffset(span.start), ByteOffset(span.end)).ok()?,
                        role: span.role,
                    })
                })
            })
            .collect();
        self.ui
            .syntax_spans
            .sort_unstable_by_key(|span| (span.range.start, span.range.end));
        self.ui.markdown_marks = self
            .syntax
            .results()
            .flat_map(|(buffer, result)| {
                result.markdown.iter().filter_map(|span| {
                    Some(crate::ui::UiMarkdownMark {
                        buffer: *buffer,
                        revision: BufferRevision(result.revision),
                        range: ByteRange::new(ByteOffset(span.start), ByteOffset(span.end)).ok()?,
                        mark: span.mark,
                    })
                })
            })
            .collect();
        self.ui
            .markdown_marks
            .sort_unstable_by_key(|mark| mark.range.start);
    }
}

/// Where the `count`th non-blank character (from zero) of `text` starts, or its end.
fn nth_non_blank(text: &str, count: usize) -> usize {
    text.char_indices()
        .filter(|(_, character)| !character.is_whitespace())
        .nth(count)
        .map_or(text.len(), |(at, _)| at)
}

fn move_spans_onto_current_text(spans: &mut Vec<UiSyntaxSpan>, state: &crate::editor::EditorState) {
    let current = |span: &UiSyntaxSpan| {
        state
            .buffers
            .get(span.buffer)
            .map(|buffer| buffer.text.revision())
    };
    if spans
        .iter()
        .all(|span| current(span).is_none_or(|at| at == span.revision))
    {
        return;
    }
    spans.retain_mut(|span| {
        let Some(buffer) = state.buffers.get(span.buffer) else {
            return true;
        };
        if span.revision == buffer.text.revision() {
            return true;
        }
        let Some(range) = buffer.text.map_range_since(span.revision, span.range) else {
            return false;
        };
        span.range = range;
        span.revision = buffer.text.revision();
        true
    });
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn sync_ui_diagnostics(&mut self) {
        if let Some(candidate) = self.diagnostics.projection(&self.state) {
            self.ui.diagnostics = candidate;
            self.window_session.diagnostics_changed();
        }
        self.ui.expanded_diagnostic = self.diagnostics.expanded(&self.state);
    }
    fn clear_lsp_ui_rows(&mut self) {
        if self.ui.language_completion.is_some() {
            self.ui.language_completion = None;
        }
        // A Git or Markdown preview doesn't come from a server, and this runs each frame in a file
        // without one.
        if self.input.documentation().is_some_and(|popup| {
            !matches!(
                popup.kind,
                DocumentationPopupKind::Git | DocumentationPopupKind::Notice
            )
        }) {
            self.close_documentation();
        }
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_semantic_spans);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_highlight_spans);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_inlay_hints);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_folds);
    }
    pub(in crate::editor::session) fn clear_lsp_ui_projection(&mut self) {
        self.language.clear_projection();
        self.clear_lsp_ui_rows();
    }
    pub(in crate::editor::session) fn refresh_lsp_projection_owner(&mut self) {
        let document = self.active_lsp_document_without_text();
        let change = self.language.refresh_projection(
            &self.state,
            document
                .as_ref()
                .map(|(buffer, document)| (*buffer, document)),
        );
        if change != ProjectionChange::Same {
            // Typing keeps the server's colours, moved along with the edit, until it sends new
            // ones; dropping them flashes the text back to tree-sitter's.
            let semantic = std::mem::take(&mut self.ui.lsp_semantic_spans);
            self.clear_lsp_ui_rows();
            if change == ProjectionChange::Typed {
                self.ui.lsp_semantic_spans = semantic;
            }
            if !self.language.completion_items().is_empty() {
                self.project_lsp_completion();
            }
            if self.language.projection().is_some() {
                self.schedule_lsp_inlay_refresh(true);
            }
        }
    }
    pub(in crate::editor::session) fn clear_lsp_session_state(&mut self) {
        if let Some(server) = self.language.active().cloned() {
            self.clear_lsp_server_ownership(&server);
        }
        self.language.clear_active();
        self.sync_ui_diagnostics();
        self.clear_lsp_ui_projection();
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_startup_refresh(&mut self) {
        self.language
            .startup_inlays(&self.configuration.settings().lsp, Instant::now());
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_warmup_retry(&mut self) -> bool {
        self.language
            .retry_inlays(&self.configuration.settings().lsp, Instant::now())
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_refresh(&mut self, immediate: bool) {
        self.language.schedule_inlays(
            &self.configuration.settings().lsp,
            immediate,
            Instant::now(),
        );
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
    pub(in crate::editor::session) fn process_lsp_inlay_refresh(&mut self) {
        if self
            .language
            .inlay_ready(&self.configuration.settings().lsp, Instant::now())
        {
            self.request_lsp_inlay_hints();
        }
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
}

impl InteractiveApplication {
    fn reprobe_failed_path_runtime(&mut self, server: &str) {
        let Some(definition) = self.language.sessions().get(server).and_then(|state| {
            matches!(&state.launch, LspLaunch::Direct { managed: false, .. })
                .then(|| state.definition.clone())
        }) else {
            return;
        };
        let Some(executable) = discover_executable(&definition.executable) else {
            return;
        };
        let key = self.tool_version_key(&definition, LspRuntimeSource::Path, Some(&executable));
        self.tooling.forget_probe(&key);
        self.queue_tool_version_probe(&definition, LspRuntimeSource::Path, executable);
    }

    pub(in crate::editor::session) fn append_lsp_log(&mut self, line: String) {
        self.language.append_log(line, LSP_MAXIMUM_LOG_LINES);
        if self.search.picker_surface() == PickerSurfaceKind::Logs && self.ui.picker.is_some() {
            self.show_lsp_logs();
        }
    }

    pub(in crate::editor::session) fn lsp_server_label(&self, server_key: &str) -> String {
        self.language.server_label(server_key)
    }

    pub(in crate::editor::session) fn update_lsp_progress(
        &mut self,
        server: String,
        token: String,
        kind: LspProgressKind,
        title: String,
        message: String,
        percentage: Option<u8>,
    ) {
        let was_loading = self.language.server_loading(&server);
        self.language.update_progress(
            (server.clone(), token, kind, title, message, percentage),
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        if was_loading && !self.language.server_loading(&server) {
            self.schedule_lsp_inlay_refresh(true);
        }
        self.project_lsp_progress();
    }
    pub(in crate::editor::session) fn clear_lsp_progress(&mut self, server: &str) {
        self.language.clear_progress(server);
        self.project_lsp_progress();
    }
    fn project_lsp_progress(&mut self) {
        let (rows, frame) = self.language.progress();
        if self.ui.lsp_progress != rows {
            self.ui.lsp_progress = rows.to_vec();
        }
        if self.lsp_waiting_server.as_ref()
            .is_some_and(|server| {
                self.language.sessions().get(server).is_some_and(|session| session.capabilities.is_some())
                    && !self.language.server_loading(server)
            })
        {
            self.lsp_waiting_server = None;
            self.lsp_retry_at = None;
            if let Some(feature) = self.lsp_waiting.take() {
                self.request_lsp(feature);
            }
        }
        self.ui.set(frame, |ui| &mut ui.lsp_progress_frame);
    }
    pub(in crate::editor::session) fn update_lsp_progress_animation(&mut self) {
        self.language.animate_progress(
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        self.project_lsp_progress();
    }
    /// Whether the server is still working through its project.
    /// Sends the waiting request again once a "content modified" retry is due.
    pub(super) fn process_lsp_retry(&mut self) {
        if self.lsp_retry_at.is_none_or(|due| Instant::now() < due) {
            return;
        }
        self.lsp_retry_at = None;
        self.lsp_waiting_server = None;
        if let Some(feature) = self.lsp_waiting.take() {
            self.request_lsp(feature);
        }
    }
    fn lsp_server_loading(&self, server: &str) -> bool {
        self.language.server_loading(server)
    }
    fn show_lsp_loading(&mut self) {
        self.ui.lsp_loading = Some(self.message("lsp_loading", &[]));
    }
    /// An empty answer from a server still loading its project says nothing about the code.
    pub(in crate::editor::session) fn show_lsp_no_result(&mut self, server: &str) {
        if self.lsp_server_loading(server) {
            self.show_lsp_loading();
            self.lsp_waiting = self.lsp_asked.clone();
            self.lsp_waiting_server = Some(server.into());
        } else {
            self.show_error(self.message("lsp_no_result", &[]));
        }
    }
    pub(in crate::editor::session) fn handle_lsp_request_failure(
        &mut self,
        server: String,
        generation: u64,
        method: String,
        message: String,
    ) {
        // The status line's own request: an empty trail is the whole result.
        if self.language.is_breadcrumb_request(generation) {
            self.language.retry_breadcrumbs();
            return;
        }
        // A formatter that fails must not cost the save.
        if method == "textDocument/formatting"
            && self.format_on_save.as_ref().is_some_and(|pending| {
                pending.request.as_ref() == Some(&(server.clone(), generation))
            })
        {
            self.append_lsp_log(format!("format on save failed: {message}"));
            self.finish_format_on_save();
            return;
        }
        if method == "textDocument/inlayHint" {
            if self.language.fail_inlay(&server, generation) {
                self.schedule_lsp_inlay_warmup_retry();
                let server = self.lsp_server_label(&server);
                self.append_lsp_log(format!("{server}: inlay hint refresh failed: {message}"));
            }
            // Inlay hints are background, revision-owned projections. A failure
            // for any superseded generation is stale by definition, while a
            // current failure is retried above. Neither is user-actionable.
            return;
        }
        let current_asked = self.lsp_asked_generation == Some(generation)
            && self.lsp_asked.as_ref().is_some_and(|feature| feature.method() == method);
        if current_asked {
            self.ui.lsp_loading = None;
        }
        // Colours are background work, and a second server failing a completion
        // must not hide the items the others returned.
        let secondary = method == "textDocument/completion"
            && (self.language.completion_automatic()
                || self
                    .language
                    .active()
                    .is_some_and(|active| *active != server));
        // A server that fails code actions must not hide what the others offered.
        if method == "textDocument/codeAction" {
            self.language.forget_request(&server, generation);
            let main = self
                .language
                .active()
                .is_some_and(|active| *active == server);
            let label = self.lsp_server_label(&server);
            if main {
                self.show_error(self.message(
                    "lsp_request_failed",
                    &[
                        ("server", label),
                        ("method", method),
                        ("diagnostic", message),
                    ],
                ));
            } else {
                self.append_lsp_log(format!("{label}: {method} failed: {message}"));
            }
            if !self.language.request_pending() && (!main || self.language.has_offered_actions()) {
                self.open_lsp_code_actions_picker(&server);
            }
            return;
        }
        if method == "textDocument/documentColor" || secondary {
            self.language.forget_request(&server, generation);
            let server = self.lsp_server_label(&server);
            self.append_lsp_log(format!("{server}: {method} failed: {message}"));
            return;
        }
        // A server still indexing answers "content modified"; the protocol says to ask again,
        // so the request waits for the load to finish, as an empty answer does.
        // Right after opening a file the server can answer that way before it reports loading;
        // then the request is sent again shortly, a bounded number of times.
        if message.contains("-32801") && current_asked {
            let loading = self.lsp_server_loading(&server);
            if loading || self.lsp_retries < LSP_CONTENT_MODIFIED_RETRIES {
                if !loading {
                    self.lsp_retries += 1;
                    self.lsp_retry_at = Some(Instant::now() + LSP_CONTENT_MODIFIED_DELAY);
                }
                self.show_lsp_loading();
                self.lsp_waiting = self.lsp_asked.clone();
                self.lsp_waiting_server = loading.then_some(server);
                return;
            }
        }
        if !current_asked && message.contains("-32801") {
            self.language.forget_request(&server, generation);
            return;
        }
        let server = self.lsp_server_label(&server);
        self.show_error(self.message(
            "lsp_request_failed",
            &[
                ("server", server),
                ("method", method),
                ("diagnostic", message),
            ],
        ));
    }

    pub(in crate::editor::session) fn process_lsp_events(&mut self) {
        for error in self.language.retry_configurations() {
            self.show_error(error);
        }
        for error in self.language.flush_edit_replies() {
            self.show_error(error);
        }
        while let Some(owned) = self.language.poll_event() {
            let crate::language::lsp::OwnedLspEvent {
                event,
                context,
                reply,
            } = owned;
            if !self.language.owns_event_context(&context) {
                self.language
                    .reject_reply(reply, "requesting language-server session retired".into());
                continue;
            }
            match event {
                LspEvent::Starting { .. } => {}
                LspEvent::Running {
                    server,
                    capabilities,
                } => {
                    self.ui.lsp_loading = None;
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_startup_refresh();
                    }
                    if self.lsp_waiting_server.as_ref().is_none_or(|waiting| waiting == &server)
                        && !self.language.server_loading(&server)
                    {
                        self.lsp_waiting_server = None;
                        if let Some(feature) = self.lsp_waiting.take() {
                            self.request_lsp(feature);
                        }
                    }
                    self.request_lsp_colors();
                    // Successful lifecycle transitions are reflected by the
                    // status line and bounded progress surface. They must not
                    // leave persistent bottom-left notifications.
                }
                LspEvent::CapabilitiesChanged {
                    server,
                    capabilities,
                } => {
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                    self.request_lsp_colors();
                }

                LspEvent::InlayHintsRefresh { server } => {
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }
                LspEvent::Diagnostics {
                    server,
                    uri,
                    version,
                    diagnostics,
                } => {
                    let result = self
                        .language
                        .diagnostic_batch(
                            &context,
                            &self.state,
                            (&uri, version, diagnostics),
                            (
                                false,
                                MAXIMUM_DIAGNOSTICS_PER_URI,
                                &self.lsp_server_label(&server),
                            ),
                        )
                        .and_then(|batch| self.diagnostics.publish(batch, &self.state));
                    match result {
                        Ok(()) => self.sync_ui_diagnostics(),
                        Err(error) => self.append_lsp_log(error),
                    }
                }

                LspEvent::Result {
                    server,
                    generation,
                    method,
                    uri,
                    version,
                    result,
                } => {
                    if context
                        .document_source(&uri)
                        .is_some_and(|(buffer, source)| {
                            self.state
                                .buffers
                                .get(*buffer)
                                .is_none_or(|buffer| buffer.text.source_version() != *source)
                        })
                    {
                        continue;
                    }
                    let save_format = method == "textDocument/formatting"
                        && self.format_on_save.as_ref().is_some_and(|pending| {
                            pending.request.as_ref() == Some(&(server.clone(), generation))
                        });
                    if save_format {
                        self.present_lsp_result_owned(
                            &server,
                            generation,
                            &method,
                            &uri,
                            result,
                            Some(context),
                        );
                        continue;
                    }
                    let current = self.active_lsp_document_without_text();
                    if !current.as_ref().is_some_and(|(_, document)| {
                        document.uri() == uri && document.version == version
                    }) {
                        continue;
                    }
                    if let LspFeatureResult::Colors(colors) = result {
                        self.install_lsp_colors(&server, &uri, colors, context.encoding());
                        continue;
                    }
                    if self.language.is_breadcrumb_request(generation) {
                        self.refresh_lsp_projection_owner();
                        if !self.language.accept_breadcrumbs(&server, &uri, version) {
                            continue;
                        }
                        if let LspFeatureResult::Symbols(symbols) = result {
                            self.install_lsp_breadcrumbs(symbols, context.encoding());
                        }
                        continue;
                    }
                    self.refresh_lsp_projection_owner();
                    if !self
                        .language
                        .accept_result(&server, generation, &method, &uri, version,
                            self.state.active_view.and_then(|id| self.state.views.get(id)).map_or(ByteOffset(0), |view| view.cursor))
                    {
                        continue;
                    }
                    self.present_lsp_result_owned(
                        &server,
                        generation,
                        &method,
                        &uri,
                        result,
                        Some(context),
                    );
                }
                LspEvent::ApplyEditRequest {
                    server: _,
                    request_id: _,
                    label,
                    edit,
                } => {
                    self.begin_workspace_proposal(
                        label.unwrap_or_else(|| self.message("workspace_edit_server_label", &[])),
                        edit,
                        reply,
                        context.encoding(),
                    );
                }
                LspEvent::Progress {
                    server,
                    token,
                    kind,
                    title,
                    message,
                    percentage,
                } => self.update_lsp_progress(server, token, kind, title, message, percentage),
                LspEvent::Log { server, message } => {
                    let server = self.lsp_server_label(&server);
                    self.append_lsp_log(format!("{server}: {message}"));
                }
                LspEvent::Health { server, health } => {
                    let server = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message(
                            "lsp_health",
                            &[
                                ("server", server),
                                ("documents", health.attached_documents.to_string()),
                                ("pending", health.pending_requests.to_string()),
                                (
                                    "encoding",
                                    health.capabilities.position_encoding.lsp_name().into(),
                                ),
                            ],
                        ),
                        false,
                    );
                }
                LspEvent::Unsupported { server, method } => {
                    let server = self.lsp_server_label(&server);
                    self.show_error(
                        self.message("lsp_unsupported", &[("server", server), ("method", method)]),
                    );
                }
                LspEvent::RequestFailed {
                    server,
                    generation,
                    method,
                    message,
                } => self.handle_lsp_request_failure(server, generation, method, message),
                LspEvent::Stopped { server, success } => {
                    if context.diagnostic_ambiguous() {
                        self.language.request_start(&server, None);
                    }
                    if !success {
                        self.reprobe_failed_path_runtime(&server);
                    }
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    self.ui.lsp_colors.retain(|span| span.server != server);
                    let server_label = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message("lsp_stopped", &[("server", server_label)]),
                        false,
                    );
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
                LspEvent::Failed { server, message } => {
                    self.reprobe_failed_path_runtime(&server);
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    self.ui.lsp_colors.retain(|span| span.server != server);
                    let server_label = self.lsp_server_label(&server);
                    self.language.failed(server_label.clone(), message.clone());
                    self.show_error(self.message(
                        "lsp_failed",
                        &[("server", server_label), ("diagnostic", message)],
                    ));
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
            }
        }
    }

    pub(in crate::editor::session) fn schedule_lsp_restart(&mut self, key: String) {
        if let Some(server) = self.language.schedule_restart(&key, Instant::now()) {
            self.show_error(self.message("lsp_restart_disabled", &[("server", server)]));
        }
    }

    pub(in crate::editor::session) fn restart_lsp_if_due(&mut self) {
        if let Some(key) = self.language.restart_due(Instant::now()) {
            self.restart_lsp_session(key);
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn request_lsp_inlay_hints(&mut self) {
        self.refresh_lsp_projection_owner();
        let Some(owner) = self.language.projection() else {
            return;
        };
        if self.ui.large_file_buffers.contains(&owner.buffer)
            || !self
                .language
                .sessions()
                .get(&owner.server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.inlay_hints)
        {
            self.ui.lsp_inlay_hints.clear();
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if buffer == owner.buffer {
            self.language.request_inlays(document);
        }
    }

    /// Keeps the symbols of the document on screen fresh for the status line.
    pub(in crate::editor::session) fn process_lsp_breadcrumbs(&mut self) {
        let settings = self.configuration.settings();
        let shown = settings.ui.status_line.enabled
            && settings
                .ui
                .status_line
                .components
                .iter()
                .any(|component| component == "breadcrumbs");
        let Some((buffer, document)) = self.active_lsp_document_without_text().filter(|_| shown)
        else {
            self.ui.set(None, |ui| &mut ui.breadcrumbs);
            return;
        };
        if self.ui.large_file_buffers.contains(&buffer) {
            return;
        }
        if let Some(server) = self
            .language
            .request_breadcrumbs(buffer, &document, Instant::now())
            && let Some((_, document)) = self.active_lsp_document()
        {
            self.language.send_breadcrumbs(&server, buffer, document);
        }
    }
    /// The symbols a cursor can be inside, in the server's order: outer ones before inner ones.
    fn install_lsp_breadcrumbs(
        &mut self,
        symbols: Vec<crate::language::lsp::LspSymbol>,
        encoding: PositionEncoding,
    ) {
        // As LazyVim's status line: Variables and such aren't places in the file.
        const PLACES: [u8; 12] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 23];
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        let codec = PositionCodec::new(&self.state.buffers[buffer].text);
        let symbols = symbols
            .into_iter()
            .filter(|symbol| symbol.kind.is_some_and(|kind| PLACES.contains(&kind)))
            .filter_map(|symbol| {
                let extent = symbol.extent?;
                let start = codec.lsp_to_byte(extent.start.line, extent.start.character, encoding);
                let end = codec.lsp_to_byte(extent.end.line, extent.end.character, encoding);
                Some(crate::ui::UiBreadcrumb {
                    name: symbol.name.trim_start().to_owned(),
                    range: ByteRange::new(start.ok()?, end.ok()?).ok()?,
                })
            })
            .collect();
        self.ui
            .set(Some((buffer, symbols)), |ui| &mut ui.breadcrumbs);
    }

    /// Asks every attached server with a colour provider for the active document's colours.
    pub(in crate::editor::session) fn request_lsp_colors(&mut self) {
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if !self.ui.large_file_buffers.contains(&buffer) {
            self.language.request_colors(buffer, &document);
        }
    }
    fn install_lsp_colors(
        &mut self,
        server: &str,
        uri: &str,
        colors: Vec<crate::language::lsp::LspColor>,
        encoding: PositionEncoding,
    ) {
        let Some(path) = path_from_file_uri(uri) else {
            return;
        };
        let Some((buffer_id, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            return;
        };
        let codec = PositionCodec::new(&buffer.text);
        let revision = buffer.text.revision();
        let spans = colors.into_iter().filter_map(|color| {
            let start = codec
                .lsp_to_byte(
                    color.range.start.line,
                    color.range.start.character,
                    encoding,
                )
                .ok()?;
            let end = codec
                .lsp_to_byte(color.range.end.line, color.range.end.character, encoding)
                .ok()?;
            Some(crate::ui::UiColorSpan {
                server: server.to_owned(),
                buffer: buffer_id,
                revision,
                range: ByteRange::new(start, end).ok()?,
                rgb: color.rgb,
            })
        });
        let mut next = self
            .ui
            .lsp_colors
            .iter()
            .filter(|span| {
                (span.server != server || span.buffer != buffer_id)
                    && self.state.buffers.get(span.buffer).is_some()
            })
            .cloned()
            .chain(spans)
            .collect::<Vec<_>>();
        next.sort_unstable_by_key(|span| (span.buffer, span.range.start, span.range.end));
        self.ui.set(next, |ui| &mut ui.lsp_colors);
    }

    pub(in crate::editor::session) fn request_lsp(&mut self, mut feature: LspFeature) {
        if matches!(feature, LspFeature::Formatting)
            && let Some(tool) = self.active_formatter()
        {
            self.process_effect(AppEffect::FormatRun(tool));
            return;
        }
        if matches!(feature, LspFeature::InlayHints { .. }) {
            self.schedule_lsp_inlay_refresh(true);
            self.process_lsp_inlay_refresh();
            return;
        }
        if matches!(&feature, LspFeature::WorkspaceSymbols { query } if query.is_empty()) {
            self.open_workspace_symbols_prompt();
            return;
        }
        self.close_documentation();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let offset = view.cursor;
        let selection = view.selection;
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        let server = if matches!(feature, LspFeature::Formatting) {
            // Not every server on a file formats it: Tailwind sits beside the TypeScript one.
            self.language
                .formatting_server(buffer, &document)
                .or_else(|| self.language.supporting_server(buffer, &document, &feature))
        } else {
            self.language.supporting_server(buffer, &document, &feature)
        };
        // Someone is waiting on these, unlike a save's format or what the editor asks by itself.
        let asked = !self.format_on_save.is_some()
            && !matches!(
                feature,
                LspFeature::SemanticTokensFull | LspFeature::SemanticTokensDelta { .. }
            )
            && !(matches!(feature, LspFeature::Completion) && self.language.completion_automatic());
        let Some(server) = server else {
            self.start_lsp(None);
            if asked
                && self
                    .language
                    .sessions()
                    .values()
                    .any(|session| session.capabilities.is_none())
            {
                self.show_lsp_loading();
                self.lsp_waiting = Some(feature);
                self.lsp_waiting_server = None;
            }
            return;
        };
        // Switching the server or the buffer's owner clears completion state, including where
        // this one started.
        let completion = matches!(feature, LspFeature::Completion).then(|| {
            (
                self.language.completion_start(),
                self.language.completion_automatic(),
            )
        });
        // A formatter beside the file's main server, such as Ruff by basedpyright, doesn't replace it.
        let formats = matches!(
            feature,
            LspFeature::Formatting | LspFeature::RangeFormatting { .. }
        );
        if !formats && self.language.active() != Some(&server) {
            self.select_lsp_session(&server);
        }
        // The application owns a session record while its process is still
        // initializing. Do not expose that record as a runnable server or
        // send a request that can race the worker's startup/disconnect path.
        // `Running` publishes capabilities before this becomes requestable.
        if self
            .language
            .sessions()
            .get(&server)
            .is_none_or(|session| session.capabilities.is_none())
        {
            if asked {
                self.show_lsp_loading();
                self.lsp_waiting = Some(feature);
                self.lsp_waiting_server = Some(server.clone());
            }
            return;
        }
        if asked && (matches!(feature, LspFeature::Completion) || self.lsp_server_loading(&server))
        {
            self.show_lsp_loading();
        }
        if asked {
            self.lsp_asked = Some(feature.clone());
            self.lsp_asked_generation = None;
        }
        if matches!(feature, LspFeature::CodeAction { .. }) {
            self.language.clear_offered_actions();
            let encoding = self
                .language
                .capabilities()
                .map_or(PositionEncoding::Utf16, |capabilities| {
                    capabilities.position_encoding
                });
            let buffer = TextBuffer::from_text(&document.text);
            let codec = PositionCodec::new(&buffer);
            let (start, end) = selection.map_or((offset, offset), |selection| {
                if selection.anchor <= selection.head {
                    (selection.anchor, selection.head)
                } else {
                    (selection.head, selection.anchor)
                }
            });
            let Ok((start_line, start_character)) = codec.byte_to_lsp(start, encoding) else {
                return;
            };
            let Ok((end_line, end_character)) = codec.byte_to_lsp(end, encoding) else {
                return;
            };
            // As LazyVim asks: everything on the cursor's lines, so a fix is offered from anywhere
            // on the line that has the problem.
            let line_end = buffer
                .line_to_byte(end_line.saturating_add(1))
                .unwrap_or_else(|_| ByteOffset(buffer.len_bytes()));
            let line_start = buffer.line_to_byte(start_line).unwrap_or(start);
            let position = |offset| {
                codec
                    .byte_to_lsp(offset, encoding)
                    .ok()
                    .map(|(line, character)| crate::language::lsp::LspPosition { line, character })
            };
            let diagnostics = self
                .diagnostics
                .within(&document.uri(), line_start, line_end)
                .into_iter()
                .filter_map(|problem| {
                    Some(crate::language::lsp::LspDiagnostic {
                        range: crate::language::lsp::LspRange {
                            start: position(problem.range.start)?,
                            end: position(problem.range.end)?,
                        },
                        severity: Some(match problem.severity {
                            crate::workspace::DiagnosticSeverity::Error => 1,
                            crate::workspace::DiagnosticSeverity::Warning => 2,
                            crate::workspace::DiagnosticSeverity::Information => 3,
                            crate::workspace::DiagnosticSeverity::Hint => 4,
                        }),
                        code: problem.code.clone(),
                        source: Some(problem.source.clone()),
                        message: problem.message.clone(),
                        data: problem.data.clone(),
                    })
                })
                .collect();
            feature = LspFeature::CodeAction {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: start_line,
                        character: start_character,
                    },
                    end: crate::language::lsp::LspPosition {
                        line: end_line,
                        character: end_character,
                    },
                },
                diagnostics,
            };
        }
        self.refresh_lsp_projection_owner();
        if let Some((start, automatic)) = completion {
            self.language.begin_completion(start, automatic);
        }
        match self
            .language
            .request_projected(&server, feature, document, offset)
        {
            Ok(()) => {
                if asked {
                    self.lsp_asked_generation = Some(self.language.generation());
                }
                if formats
                    && let Some(pending) = self
                        .format_on_save
                        .as_mut()
                        .filter(|pending| pending.buffer == buffer && pending.request.is_none())
                {
                    pending.request = Some((server.clone(), self.language.generation()));
                }
            }
            // The worker has already reported (or will imminently report) its
            // real startup/exit failure. Avoid replacing that diagnostic with
            // a misleading raw transport-key "not running" error.
            Err(crate::language::lsp::LspError::NotRunning(_)) => {
                self.language.stopped(&server);
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn present_documentation_popup(
        &mut self,
        kind: DocumentationPopupKind,
        title: String,
        provider: String,
        source_range: Option<ByteRange>,
        source_sections: Vec<LspMarkupContent>,
    ) {
        if kind != DocumentationPopupKind::Completion {
            self.reconcile_editor_windows();
            self.input.hold_focus(self.window_session.capture_focus());
        }
        let config = self.configuration.settings();
        let available = usize::from(self.window_session.width());
        let available = if kind == DocumentationPopupKind::Completion {
            available / 2
        } else {
            available
        };
        let text_width = available
            .min(usize::from(
                crate::ui::render::overlays::DOCUMENTATION_MAX_WIDTH,
            ))
            .saturating_sub(
                2 + 2 * usize::from(crate::ui::render::overlays::DOCUMENTATION_PADDING),
            );
        self.input.present_documentation(
            kind,
            title,
            provider,
            source_range,
            source_sections.into_iter().map(|source| {
                (
                    match source.kind {
                        LspMarkupKind::PlainText => DocumentationMarkupKind::PlainText,
                        LspMarkupKind::Markdown => DocumentationMarkupKind::Markdown,
                    },
                    source.value,
                )
            }),
            config,
            text_width,
        );
        self.restore_input_focus();
        self.project_input_documentation();
    }

    pub(in crate::editor::session) fn open_lsp_completion_documentation(&mut self, asked: bool) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        let (server, label, documentation) = match self
            .language
            .completion_documentation(&self.state, selected)
        {
            Ok(value) => value,
            Err(error) => {
                let stale = error == "completion documentation is stale";
                if stale {
                    self.close_documentation();
                }
                // Docs that open by themselves simply lag the menu; that is nothing to report.
                if !stale || asked {
                    self.show_error(error);
                }
                return;
            }
        };
        let server_label = self.lsp_server_label(&server);
        self.present_documentation_popup(
            DocumentationPopupKind::Completion,
            self.message(
                "lsp_completion_documentation_title",
                &[("server", server_label), ("label", label)],
            ),
            server,
            None,
            vec![documentation],
        );
    }

    pub(in crate::editor::session) fn present_lsp_result(
        &mut self,
        server: &str,
        generation: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
    ) {
        self.present_lsp_result_owned(server, generation, method, uri, result, None);
    }
    pub(in crate::editor::session) fn present_lsp_result_owned(
        &mut self,
        server: &str,
        generation: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
        context: Option<crate::language::lsp::EditContext>,
    ) {
        self.ui.lsp_loading = None;
        let server_label = self.lsp_server_label(server);
        // A null result has the same meaning as an empty list for these features.
        let result = match (method, result) {
            ("textDocument/codeAction", LspFeatureResult::Empty) => {
                LspFeatureResult::CodeActions(Vec::new())
            }
            (
                "textDocument/documentHighlight" | "textDocument/foldingRange"
                | "textDocument/selectionRange",
                LspFeatureResult::Empty,
            ) => LspFeatureResult::Ranges(Vec::new()),
            ("textDocument/semanticTokens/full" | "textDocument/semanticTokens/full/delta", LspFeatureResult::Empty) => {
                LspFeatureResult::SemanticTokens(crate::language::lsp::LspSemanticTokens {
                    result_id: None,
                    data: Vec::new(),
                })
            }
            ("textDocument/inlayHint", LspFeatureResult::Empty) => {
                LspFeatureResult::InlayHints(Vec::new())
            }
            (_, result) => result,
        };
        match result {
            // Another server on this file already answered.
            LspFeatureResult::Hover(_) if self.hover_is_showing() => {}
            LspFeatureResult::Hover(hover) => {
                let source_range = hover.range.and_then(|range| {
                    let buffer_id = self.state.active_buffer_id()?;
                    let buffer = self.state.buffers.get(buffer_id)?;
                    let codec = PositionCodec::new(&buffer.text);
                    let encoding = self.lsp_position_encoding();
                    let start = codec
                        .lsp_to_byte(range.start.line, range.start.character, encoding)
                        .ok()?;
                    let end = codec
                        .lsp_to_byte(range.end.line, range.end.character, encoding)
                        .ok()?;
                    ByteRange::new(start, end).ok()
                });
                self.present_documentation_popup(
                    DocumentationPopupKind::Hover,
                    self.message("lsp_hover_title", &[("server", server_label.clone())]),
                    server.into(),
                    source_range,
                    hover.sections,
                );
            }
            LspFeatureResult::Signature(content) => {
                self.present_documentation_popup(
                    DocumentationPopupKind::Signature,
                    self.message("lsp_signature_title", &[("server", server_label.clone())]),
                    server.into(),
                    None,
                    vec![content],
                );
            }
            LspFeatureResult::Completion { items, incomplete } => {
                self.language
                    .install_completions(server, items, incomplete, generation);
                self.narrow_lsp_completion();
                if self.ui.language_completion.is_none()
                    && !self.language.request_pending()
                    && !self.language.completion_automatic()
                {
                    self.show_lsp_no_result(server);
                }
            }
            LspFeatureResult::CompletionResolved(item) => {
                if self.language.accept_resolved(generation, item, &self.state) {
                    self.accept_lsp_completion();
                }
            }
            LspFeatureResult::Locations(locations) => {
                if locations.is_empty() {
                    self.show_lsp_no_result(server);
                } else if method == "textDocument/references" || locations.len() > 1 {
                    self.open_lsp_locations_picker(
                        locations,
                        self.language.server_encoding(server),
                    );
                } else if let Some(location) = locations.first() {
                    self.navigate_lsp_location_encoded(
                        location,
                        self.language.server_encoding(server),
                    );
                }
            }
            LspFeatureResult::Symbols(mut symbols) => {
                for location in symbols
                    .iter_mut()
                    .filter_map(|symbol| symbol.location.as_mut())
                {
                    if location.uri.is_empty() {
                        location.uri = uri.into();
                    }
                }
                self.open_lsp_symbols_picker(server, symbols);
            }
            LspFeatureResult::WorkspaceEdit(mut edit) => {
                for document in &mut edit.documents {
                    if document.uri.is_empty() {
                        document.uri = uri.into();
                    }
                }
                // Formatting was asked for and touches one file, so it applies without a review.
                if matches!(
                    method,
                    "textDocument/formatting" | "textDocument/rangeFormatting"
                ) {
                    // Typing while the server worked moved the text its edits point at.
                    let edit_is_current = context.as_ref().is_some_and(|context| {
                        self.language.edit_context_current(context)
                            && context
                                .document_source(uri)
                                .is_some_and(|(buffer, version)| {
                                    self.state.buffers.get(*buffer).is_some_and(|buffer| {
                                        buffer.text.source_version() == *version
                                    })
                                })
                    });
                    let has_edits = edit
                        .documents
                        .iter()
                        .any(|document| !document.edits.is_empty());
                    if edit_is_current && has_edits {
                        let encoding = self.language.server_encoding(server);
                        let replaced = edit
                            .documents
                            .iter()
                            .flat_map(|document| &document.edits)
                            .filter(|edit| edit.range.end.line > edit.range.start.line)
                            .map(|edit| edit.range.start.line..=edit.range.end.line)
                            .collect::<Vec<_>>();
                        let positions = self.view_positions(|line| {
                            replaced.iter().any(|lines| lines.contains(&line))
                        });
                        if let Err(error) = self.apply_lsp_workspace_edit(edit, encoding) {
                            self.show_error(error);
                        }
                        self.restore_view_positions(positions);
                    }
                    if self.format_on_save.as_ref().is_some_and(|pending| {
                        pending.request.as_ref() == Some(&(server.to_owned(), generation))
                    }) {
                        self.finish_format_on_save();
                    }
                    return;
                }
                self.begin_workspace_proposal_owned(
                    self.message("workspace_edit_lsp_label", &[]),
                    edit,
                    None,
                    self.lsp_position_encoding(),
                    context,
                );
            }
            LspFeatureResult::CodeActions(actions) => {
                self.language.offer_actions(server, actions, context);
                if !self.language.request_pending() {
                    self.open_lsp_code_actions_picker(server);
                }
            }
            LspFeatureResult::PullDiagnostics(result) => {
                if let Some(id) = result.result_id.clone() {
                    self.language.record_pull(server, uri, id);
                }
                if result.unchanged {
                    return;
                }
                let normalized = context
                    .as_ref()
                    .ok_or_else(|| "pull diagnostics lack a requesting context".to_string())
                    .and_then(|context| {
                        self.language.diagnostic_batch(
                            context,
                            &self.state,
                            (uri, None, result.items),
                            (true, MAXIMUM_DIAGNOSTICS_PER_URI, &server_label),
                        )
                    });
                match normalized.and_then(|batch| self.diagnostics.publish(batch, &self.state)) {
                    Ok(()) => self.sync_ui_diagnostics(),
                    Err(error) => self.append_lsp_log(error),
                }
            }
            LspFeatureResult::SemanticTokens(result) => {
                if !self.language.record_semantic(server, uri, &result) {
                    return;
                }
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let legend = self
                        .language
                        .capabilities()
                        .map(|value| value.semantic_token_types.as_slice())
                        .unwrap_or(&[]);
                    // As in Neovim, a token colours only when the theme gives its
                    // `@lsp.type.*` group a colour; otherwise tree-sitter's stays.
                    let theme = self.configuration.theme();
                    let groups: Vec<_> = legend
                        .iter()
                        .map(|name| {
                            crate::language::syntax::Highlight::for_semantic_token(name)
                                .filter(|group| theme.highlight(*group).is_some())
                        })
                        .collect();
                    let mut line = 0usize;
                    let mut character = 0usize;
                    let mut spans = Vec::new();
                    for token in result.data.chunks_exact(5).take(100_000) {
                        line = line.saturating_add(token[0] as usize);
                        character = if token[0] == 0 {
                            character.saturating_add(token[1] as usize)
                        } else {
                            token[1] as usize
                        };
                        let Some(role) = groups.get(token[3] as usize).copied().flatten() else {
                            continue;
                        };
                        let Ok(start) = codec.lsp_to_byte(line, character, encoding) else {
                            continue;
                        };
                        let Ok(end) = codec.lsp_to_byte(
                            line,
                            character.saturating_add(token[2] as usize),
                            encoding,
                        ) else {
                            continue;
                        };
                        let Ok(range) = ByteRange::new(start, end) else {
                            continue;
                        };
                        spans.push(UiSyntaxSpan {
                            buffer: buffer_id,
                            revision: buffer.text.revision(),
                            range,
                            role,
                        });
                    }
                    self.ui.lsp_semantic_spans = spans;
                }
            }
            LspFeatureResult::SemanticTokenDelta(delta) => {
                if let Some(full) = self.language.semantic_delta(server, uri, delta) {
                    self.present_lsp_result(
                        server,
                        generation,
                        method,
                        uri,
                        LspFeatureResult::SemanticTokens(full),
                    );
                } else {
                    self.request_lsp(LspFeature::SemanticTokensFull);
                }
            }
            LspFeatureResult::InlayHints(items) => {
                let provisional = items
                    .iter()
                    .any(|item| item.label.to_ascii_lowercase().contains("{unknown}"));
                // A server still loading answers with nothing; keep the hints on screen
                // until the retry brings real ones.
                let retrying = if items.is_empty() || provisional {
                    self.schedule_lsp_inlay_warmup_retry()
                } else {
                    self.language.inlays_received();
                    false
                };
                let keep_shown = retrying && !self.ui.lsp_inlay_hints.is_empty();
                if let Some(path) = path_from_file_uri(uri).filter(|_| !keep_shown)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    self.ui.lsp_inlay_hints = items
                        .into_iter()
                        .filter_map(|item| {
                            Some(UiInlayHint {
                                buffer: buffer_id,
                                revision: buffer.text.revision(),
                                position: codec
                                    .lsp_to_byte(
                                        item.position.line,
                                        item.position.character,
                                        encoding,
                                    )
                                    .ok()?,
                                label: item.label,
                            })
                        })
                        .collect();
                }
            }
            LspFeatureResult::CodeLens(items) => {
                let rows = items
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        let command = item.command?;
                        Some((
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("code-lens:{index}")),
                                label: item.title,
                                detail: command.command.clone(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::LspCommand(command),
                        ))
                    })
                    .collect::<Vec<_>>();
                if rows.is_empty() {
                    self.show_lsp_no_result(server);
                } else {
                    self.install_picker(self.message("picker_code_lens_title", &[]), rows);
                }
            }
            LspFeatureResult::Ranges(items) => {
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let ranges = items
                        .into_iter()
                        .filter_map(|item| {
                            ByteRange::new(
                                codec
                                    .lsp_to_byte(item.start.line, item.start.character, encoding)
                                    .ok()?,
                                codec
                                    .lsp_to_byte(item.end.line, item.end.character, encoding)
                                    .ok()?,
                            )
                            .ok()
                        })
                        .collect::<Vec<_>>();
                    match method {
                        "textDocument/documentHighlight" => {
                            self.ui.lsp_highlight_spans = ranges
                                .into_iter()
                                .map(|range| UiSyntaxSpan {
                                    buffer: buffer_id,
                                    revision: buffer.text.revision(),
                                    range,
                                    role: crate::language::syntax::Highlight::VARIABLE,
                                })
                                .collect()
                        }
                        "textDocument/foldingRange" => {
                            if let Some(view) = self.state.active_view {
                                self.ui.lsp_folds = ranges
                                    .into_iter()
                                    .filter_map(|range| {
                                        Some(UiFold {
                                            view,
                                            start_line: buffer
                                                .text
                                                .byte_to_line(range.start)
                                                .ok()?,
                                            end_line: buffer.text.byte_to_line(range.end).ok()?,
                                            collapsed: false,
                                        })
                                    })
                                    .collect()
                            }
                        }
                        "textDocument/selectionRange" => {
                            self.language.install_selection_ranges(ranges)
                        }
                        _ => {}
                    }
                }
            }
            LspFeatureResult::Colors(_) => {}
            // Nothing to change is a normal answer from a formatter.
            LspFeatureResult::Empty if matches!(method, "textDocument/formatting" | "textDocument/rangeFormatting") => {
                if self.format_on_save.as_ref().is_some_and(|pending| {
                    pending.request.as_ref() == Some(&(server.to_owned(), generation))
                }) {
                    self.finish_format_on_save();
                }
            }
            // Only the last of several servers can say that nobody knows.
            LspFeatureResult::Empty
                if method == "textDocument/hover"
                    && (self.language.request_pending() || self.hover_is_showing()) => {}
            // Completion asks by itself as you type, and nothing to offer is no news.
            LspFeatureResult::Empty
                if method == "textDocument/completion"
                    && (self.language.request_pending()
                        || self.language.completion_automatic()
                        || self.ui.language_completion.is_some()) => {}
            LspFeatureResult::Empty if method == "workspace/executeCommand" => {}
            LspFeatureResult::Empty => self.show_lsp_no_result(server),
        }
    }

    fn hover_is_showing(&self) -> bool {
        self.input
            .documentation()
            .is_some_and(|popup| popup.kind == DocumentationPopupKind::Hover)
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn begin_workspace_proposal(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<crate::language::lsp::ApplyEditReply>,
        encoding: PositionEncoding,
    ) {
        self.begin_workspace_proposal_owned(label, edit, response, encoding, None);
    }
    pub(in crate::editor::session) fn begin_workspace_proposal_owned(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<crate::language::lsp::ApplyEditReply>,
        encoding: PositionEncoding,
        context: Option<crate::language::lsp::EditContext>,
    ) {
        if let Err(error) = self.workspace_edits.check_begin().and_then(|()| {
            self.state
                .window_version()
                .revision()
                .map(|_| ())
                .ok_or_else(|| "editor source revision is exhausted".into())
        }) {
            self.language.reject_reply(response, error.clone());
            self.show_error(error);
            return;
        }
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::editor::session::lsp_edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                })
            })
            .collect::<Vec<_>>();
        let plan = match self
            .language
            .start_edit(edit, encoding, context, response, &documents)
        {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let ticket = match self.state.workspace_edit_sources().and_then(|source| {
            self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                source,
            )
        }) {
            Ok(ticket) => ticket,
            Err(error) => {
                let _ = self
                    .language
                    .finish_edit(&plan.key, false, Some(error.clone()), &self.state);
                self.show_error(error);
                return;
            }
        };
        let maximum = crate::editor::session::search::SEARCH_MAXIMUM_FILE_BYTES;
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        if let Err(error) = self.workspace_edits.start_files(
            ticket,
            label,
            Some(plan.key),
            plan.all_files,
            plan.files,
            (roots, maximum),
        ) {
            self.abort_workspace_edit(error);
            return;
        }
        while let Some((index, file)) = self.workspace_edits.next_file() {
            let snapshot =
                if let crate::editor::session::workspace_edits::InputFile::Read(path) = file {
                    self.state.workspace_snapshot(
                        self.workspace_edits
                            .preparing_sources()
                            .expect("admitted source"),
                        path,
                        maximum,
                    )
                } else {
                    Ok(None)
                };
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.abort_workspace_edit(error);
                    return;
                }
            };
            if let Some(snapshot) = snapshot {
                if let Err(error) = self.workspace_edits.open_snapshot(index, snapshot) {
                    self.abort_workspace_edit(error);
                    return;
                }
            } else {
                let admitted = self
                    .workspace_edits
                    .prepare_io(
                        index,
                        &self.workspace,
                        self.configuration.binding_revision_ref(),
                        &self.state,
                    )
                    .and_then(|permit| self.documents.submit_workspace_io(permit));
                if let Err(error) = admitted {
                    self.abort_workspace_edit(error);
                    return;
                }
            }
        }
        if self.workspace_edits.files_ready() {
            self.finish_workspace_proposal();
        } else {
            self.notify_message(self.message("workspace_edit_loading", &[]), false);
        }
    }
    fn check_workspace_edit_origin(&self) -> Result<(), String> {
        let Some(key) = self.workspace_edits.origin() else {
            return Ok(());
        };
        if self.extension.owns_edit(key) {
            return self.extension.check_edit(
                key,
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            );
        }
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::editor::session::lsp_edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                })
            })
            .collect::<Vec<_>>();
        self.language.check_edit(key, &documents)
    }
    pub(in crate::editor::session) fn finish_workspace_proposal(&mut self) {
        let result = (|| {
            self.workspace_edits.current_preparation(
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            self.check_workspace_edit_origin()?;
            let source = self
                .workspace_edits
                .preparing_sources()
                .ok_or("workspace preparation source missing")?;
            let files = self.workspace_edits.prepared_files()?;
            self.state
                .validate_workspace_sources(source, files.paths())?;
            let key = self
                .workspace_edits
                .preparing_origin()
                .ok_or("workspace source origin missing")?;
            let drafts = self.language.decode_edit(key, files)?;
            self.workspace_edits.finish_preparation(
                drafts,
                self.workspace.edit_scope(),
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )
        })();
        match result {
            Ok(()) => self.show_workspace_proposal(),
            Err(error) => self.abort_workspace_edit(error),
        }
    }
    pub(in crate::editor::session) fn retire_workspace_picker(
        &mut self,
        id: crate::workspace::ProposalId,
    ) {
        if self.workspace_edits.proposal().is_none_or(|p| p.id != id) {
            return;
        }
        let retired = self.workspace_edits.reject();
        for request in retired.requests {
            self.documents.cancel_request(request);
        }
        if let Some(key) = retired.origin {
            let _ = self.finish_workspace_edit_origin(
                &key,
                false,
                Some("workspace preview dismissed".into()),
            );
        }
    }
    fn finish_workspace_edit_origin(
        &mut self,
        key: &crate::editor::session::workspace_edits::OriginKey,
        applied: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        if self.extension.owns_edit(key) {
            self.extension.finish_edit(key)
        } else {
            self.language.finish_edit(key, applied, reason, &self.state)
        }
    }
    pub(in crate::editor::session) fn abort_workspace_edit(&mut self, reason: String) {
        let retired = self.workspace_edits.reject();
        for id in retired.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = retired.origin {
            let _ = self.finish_workspace_edit_origin(&origin, false, Some(reason.clone()));
        }
        self.close_picker();
        self.show_error(reason);
    }
    pub(in crate::editor::session) fn reject_workspace_proposal(&mut self) {
        let retired = self.workspace_edits.reject();
        for id in retired.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = retired.origin {
            let _ = self.finish_workspace_edit_origin(
                &origin,
                false,
                Some("workspace edit rejected".into()),
            );
        }
        self.close_picker();
        self.notify_message(self.message("workspace_edit_rejected", &[]), false);
    }
    pub(in crate::editor::session) fn accept_workspace_proposal(&mut self) {
        if let Err(error) = self.check_workspace_edit_origin() {
            self.abort_workspace_edit(error);
            return;
        }
        let transaction = self.transaction_id();
        let prepared = (|| {
            let (source, proposal) = self.workspace_edits.approval(
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            let changes = self
                .workspace_edits
                .resources()
                .iter()
                .map(|intent| {
                    let target = match &intent.kind {
                        ResourceIntentKind::Rename { target, .. } => Some(target.as_path()),
                        ResourceIntentKind::Delete => None,
                    };
                    self.state.check_resource_destination(target)?;
                    self.state.workspace_resource_change(source, &intent.source)
                })
                .collect::<Result<Vec<_>, String>>()?;
            let draft = self
                .state
                .prepare_workspace_edit(source, proposal, transaction)?;
            Ok::<_, String>((draft, changes))
        })();
        let (draft, changes) = match prepared {
            Ok(value) => value,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.workspace_edits.stage_approval(&draft, changes) {
            self.show_error(error);
            return;
        }
        if let Err(error) = self.commit_editor_draft(draft, None, false) {
            self.workspace_edits.abandon_approval();
            self.show_error(error);
            return;
        }
        let proposal = self.workspace_edits.proposal().expect("approved preview").id;
        let accepted = self.workspace_edits.accepted(self.state.publication());
        if accepted.is_ok() {
            self.close_picker();
        }
        match accepted {
            Ok(Some(origin)) => {
                let result = if !self.extension.owns_edit(&origin)
                    && self.workspace_edits.resources_pending(proposal)
                {
                    self.language.defer_edit(&origin, proposal)
                } else {
                    self.finish_workspace_edit_origin(&origin, true, None)
                };
                if let Err(error) = result {
                    self.show_error(error);
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.show_error(error);
                return;
            }
        }
        self.notify_message(self.message("workspace_edit_applied", &[]), false);
    }
    /// Applies an edit whose positions use `encoding`, the encoding of the server that sent it.
    pub(in crate::editor::session) fn apply_lsp_workspace_edit(
        &mut self,
        edit: LspWorkspaceEdit,
        encoding: PositionEncoding,
    ) -> Result<(), String> {
        let id = self.transaction_id();
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::editor::session::lsp_edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                })
            })
            .collect::<Vec<_>>();
        let transaction =
            crate::editor::session::lsp_edits::LspEditSources::completion_transaction(
                edit, encoding, &documents, id,
            )?;
        let draft = self
            .state
            .prepare_transaction(&transaction)
            .map_err(|e| e.to_string())?;
        self.commit_editor_draft(draft, None, false)?;
        Ok(())
    }
}

#[cfg(test)]
mod formatter_cursor_tests {
    #[test]
    fn a_formatted_file_keeps_the_cursor_on_its_word() {
        let before = "<html><head></head>\n<body><p>Hello World</p></body></html>\n";
        let after =
            "<html>\n  <head></head>\n  <body>\n    <p>Hello World</p>\n  </body>\n</html>\n";
        let cursor = before.find("World").unwrap();
        let non_blank = before[..cursor]
            .chars()
            .filter(|c| !c.is_whitespace())
            .count();
        let moved = super::nth_non_blank(after, non_blank);
        assert_eq!(&after[moved..moved + 5], "World");
        assert_eq!(super::nth_non_blank("a b", 9), 3);
    }
}
