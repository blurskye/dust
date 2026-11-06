use crate::editor::CommandValue;
use crate::app::AppEffect;
use crate::app::CommandInvocation;
use crate::app::DockEdge;
use crate::app::EditorMode;
use crate::app::Instant;
use crate::app::App;
use crate::app::NativeSurfaceTarget;
use crate::app::PromptKind;
use crate::app::TerminalInput;
use crate::app::TransactionId;
use crate::app::VisualRowModel;
use crate::app::WindowPlacement;
use crate::app::WindowRect;
use crate::app::WorkspaceTrust;
use crate::app::editor_pane_viewport_at;
use crate::app::editor_state_content_width;
use crate::app::editor_state_gutter_width;
use slotmap::Key;
// Remaining editor/window routing; terminal data and algorithms live in the owner.
use crate::app::PtyId;
use crate::app::CommandValue;
use crate::app::Duration;
use crate::app::KeyEvent;
use crate::app::KeyInputKind;
use crate::app::MappingResolution;
use crate::app::PtyId;
use crate::app::TerminalPanelMode;
use crate::app::UiEvent;
use crate::app::command;
use crate::app::mode_name;
use crate::app::rect_contains_point;
use crate::app::terminal_tab_at_column;
use crate::ui::render::TERMINAL_PANEL_MINIMUM_HEIGHT;
use crate::windows::PANE_MINIMUM_HEIGHT;
use crate::windows::PANE_MINIMUM_WIDTH;
use crate::windows::WindowPlacement;
use crate::app::windows::rect_contains_point;
use crate::app::workspace_top_rows;



impl App {


    pub(in crate::app) fn mouse_pane_geometry(
        &self,
    ) -> Option<(crate::editor::PaneId, u16, u16, u16, u16)> {
        let (global_column, global_row) = self.input.mouse_position()?;
        editor_pane_viewport_at(&self.state, &self.ui, global_column, global_row).map(|viewport| {
            (
                viewport.pane,
                global_column.saturating_sub(viewport.x),
                global_row.saturating_sub(viewport.y),
                viewport.width,
                viewport.height,
            )
        })
    }
}

impl App {


    pub(in crate::app) fn focus_mouse_pane(&mut self) {
        self.reconcile_editor_windows();
        let Some((pane, ..)) = self.mouse_pane_geometry() else {
            return;
        };
        let Some(window) = self.window_session.editor_ids().window_for(pane) else {
            return;
        };
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
        } else {
            self.ensure_cursor_visible();
        }
    }
}

impl App {


    pub(in crate::app) fn apply_mouse_cursor(&mut self, extend_selection: bool) {
        self.reconcile_editor_windows();
        let Some((pane_id, column, row, pane_width, pane_height)) = self.mouse_pane_geometry()
        else {
            return;
        };
        let Some(view_id) = self.state.panes.get(pane_id).map(|pane| pane.view) else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let buffer = &self.state.buffers[buffer_id];
        let body_height = pane_height;
        if row >= body_height {
            return;
        }
        let gutter = editor_state_gutter_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        if available == 0 {
            return;
        }
        let wrap_enabled = self.configuration.settings().editor.wrap
            && !(self.ui.large_file_buffers.contains(&buffer_id));
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            pane_width,
            wrap_enabled,
        );
        let source = self.state.version();
        let transaction_id = TransactionId(self.next_transaction);
        let plan = self.input.mouse_plan(
            crate::app::input::MouseDocument {
                editor: &source,
                view_id,
                pane_id,
                view,
                text: &buffer.text,
            },
            crate::app::input::MouseGeometry {
                column,
                row,
                body_height,
                gutter,
                rows: &visual_rows,
            },
            transaction_id,
            extend_selection,
        );
        let Some(plan) = plan else {
            return;
        };
        self.diagnostics.dismiss_expansion();
        self.ui.expanded_diagnostic = None;
        self.transaction_id();
        let transaction = match self.input.mouse_transaction(&plan, &self.state.version()) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.apply_authoritative_editor_transaction(transaction) {
            self.show_error(error);
        } else {
            let source = self.state.window_version();
            let current = self.state.active_view.and_then(|id| {
                self.state.views.get(id).and_then(|view| {
                    self.state
                        .buffers
                        .get(view.buffer)
                        .map(|buffer| (id, view.buffer, buffer.text.revision()))
                })
            });
            self.input.accept_mouse_plan(plan, &source, current);
            self.ensure_cursor_visible();
        }
    }
}

impl App {


    /// Dragging a docked terminal's inner edge resizes it: the border left of a right-hand
    /// terminal, or the top row of the bottom panel.
    fn handle_dock_drag(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        use crate::editor::input::TerminalMouseInput;
        use crate::windows::DockEdge;
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
}

impl App {


    /// A click on a terminal shown beside the active one, such as a docked agent, gives it the
    /// keys; other mouse input over it goes nowhere.
    fn handle_other_terminal_mouse(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        if self.ui.picker.is_some() || self.input.prompt().is_some() {
            return false;
        }
        let active = self.displayed_terminal();
        let Some(id) = self
            .compositor_projection()
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
}

impl App {


    /// Dragging the border between two editor windows resizes them, as in Neovim.
    fn handle_border_drag(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        use crate::editor::input::TerminalMouseInput;
        use crate::windows::TileAxis;
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
}

impl App {


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
        let region = crate::windows::WindowRect {
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
}

impl App {


    pub(in crate::app) fn handle_content_tab_mouse(
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
}

impl App {


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
}

impl App {


    pub(in crate::app) fn handle_event(&mut self, event: UiEvent) {
        if self.clipboard.waiting() && !matches!(event, UiEvent::Resize(_, _)) {
            self.clipboard
                .defer(crate::app::editing::clipboard::DeferredInput::Event(event));
            return;
        }
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
                    KeyInputKind::Press | KeyInputKind::Repeat
                        if self.record_shortcut_key(&key) => {}
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
                    projection
                        .hit_test(column, row)
                        .and_then(|window| projection.window(window))
                        .is_some_and(|window| {
                            matches!(
                                window.layer,
                                crate::windows::CompositorLayer::Floating
                                    | crate::windows::CompositorLayer::Fullscreen
                            )
                        })
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
                let hit = self
                    .compositor_projection()
                    .and_then(|projection| projection.hit_test(column, row));
                let over_terminal = hit.is_some_and(|window| {
                    matches!(
                        self.window_session.native_ids().target_for_window(window),
                        Some(NativeSurfaceTarget::Terminal(_))
                    )
                });
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
                        (!over_floating
                            && !over_terminal
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
                    && let Some(layout) = self.window_session.layout()
                    && let Some((_, window)) = layout.surfaces.terminal
                    && let Some(window) = self
                        .compositor_projection()
                        .and_then(|projection| projection.window(window))
                    && let Some(geometry) = crate::ui::terminal_window_geometry(window)
                    && row == geometry.top
                    && let Some(id) = self.ui.terminal_panel.as_ref().and_then(|terminal| {
                        terminal_tab_at_column(
                            terminal,
                            self.configuration.settings(),
                            geometry,
                            column,
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
                        TerminalInput::Mouse(event, key.modifiers, column, row, rect),
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
}

impl App {


    pub(in crate::app) fn keyboard_terminal(&self) -> Option<PtyId> {
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
}

impl App {

    pub(in crate::app) fn active_input_context(&self) -> String {
        if self.search.help_is_open() {
            return if self.input.picker_normal() {
                "picker_normal"
            } else {
                "picker"
            }
            .into();
        }
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
            if page.setup.is_some() {
                return "setup".into();
            }
            return match &page.edit {
                Some(crate::ui::SettingsEdit::Text { .. }) => "settings_edit",
                Some(crate::ui::SettingsEdit::Choice { .. }) => "settings_choice",
                Some(crate::ui::SettingsEdit::Keys(_)) => "settings",
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
            && self
                .input
                .mapping_context()
                .is_none_or(|context| context == "snippet")
        {
            return "snippet".into();
        }
        format!("editor.{}", mode_name(mode))
    }
}

impl App {


    pub(super) fn send_terminal_focus_report(&mut self, focused: bool) {
        if let Some(id) = self.keyboard_terminal()
            && let Err(error) =
                self.terminal_session
                    .send(id, TerminalInput::Focus(focused), &self.workspace)
        {
            self.show_error(error);
        }
    }
}

impl App {


    pub(in crate::app) fn handle_configured_key(&mut self, key: KeyEvent) {
        if self.clipboard.waiting() {
            self.clipboard
                .defer(crate::app::editing::clipboard::DeferredInput::Event(
                    UiEvent::Key {
                        key,
                        kind: KeyInputKind::Press,
                    },
                ));
            return;
        }
        let context = self.active_input_context();
        self.handle_key_in_context(&context, key);
    }
}

impl App {


    pub(in crate::app) fn handle_key_in_context(&mut self, context: &str, key: KeyEvent) {
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
            MappingResolution::DisabledAndReplay(replay) => {
                for key in replay {
                    self.handle_configured_key(key);
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
}

impl App {


    fn typing(&self) -> bool {
        self.state.active_view.is_some_and(|view| {
            matches!(
                self.state.views[view].mode,
                EditorMode::Insert | EditorMode::Replace
            )
        })
    }
}

impl App {


    /// Types `keys` as text where `context` takes text, without looking them up again.
    /// Returns false when the context takes no text or a key has none.
    fn type_unmapped(&mut self, context: &str, keys: &[KeyEvent]) -> bool {
        let input = &self.configuration.settings().input;
        let context = if context == "snippet" {
            "editor.insert"
        } else {
            context
        };
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
}

impl App {


    pub(in crate::app) fn handle_configured_paste(&mut self, text: String) {
        self.close_documentation();
        if self.window_session.terminal_focused()
            && let Some(id) = self.keyboard_terminal()
            && self.terminal_session.mode(id) == Some(TerminalPanelMode::Input)
        {
            match self
                .terminal_session
                .send(id, TerminalInput::Paste(&text), &self.workspace)
            {
                Ok(true) => {}
                Ok(false) => self.show_error("terminal paste exceeds the input limit"),
                Err(error) => self.show_error(error),
            }
            return;
        }
        let context = self.active_input_context();
        if self.typing()
            && matches!(
                context.as_str(),
                "editor.insert" | "editor.replace" | "snippet"
            )
        {
            match self
                .input
                .finish_mapping(&context, self.configuration.binding_revision())
            {
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
        let context = if context == "snippet" {
            "editor.insert"
        } else {
            &context
        };
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
}

impl App {


    pub(in crate::app) fn invoke_mapped_command(&mut self, command_id: &str) {
        match command(command_id) {
            Ok(invocation) => self.invoke_mapped(invocation),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    pub(in crate::app) fn invoke_text_command(&mut self, command_id: &str, text: String) {
        match command(command_id) {
            Ok(invocation) => {
                self.invoke(invocation.with_argument("text", CommandValue::String(text)))
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    #[cfg(test)]
    pub(in crate::app) fn invoke_insert_text(&mut self, text: String) {
        self.invoke_text_command("edit.insert_text", text);
    }
}

impl App {


    pub(in crate::app) fn invoke_mapped(&mut self, mut invocation: CommandInvocation) {
        if !invocation.id.as_str().starts_with("input.") {
            self.input.decorate_invocation(&mut invocation);
        }
        self.invoke(invocation);
    }
}

impl App {


    pub(in crate::app) fn cancel_pending_mapping(&mut self) {
        self.input.cancel_mapping();
        self.project_input_hints();
    }
}

impl App {

    pub(in crate::app) fn flush_keymap_timeout(&mut self) {
        if self.clipboard.waiting() {
            return;
        }
        let context = self.active_input_context();
        let resolution = self.input.flush_timeout(
            &context,
            self.configuration.binding_revision(),
            self.clock.monotonic(),
        );
        self.project_input_hints();
        match resolution {
            MappingResolution::Ready(invocation) => self.invoke_mapped(invocation),
            MappingResolution::DisabledAndReplay(replay) => {
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
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
}

impl App {

    pub(in crate::app) fn update_key_hint_ui(&mut self) {
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
}

impl App {

    pub(in crate::app) fn restore_input_focus(&mut self) {
        let Some(capture) = self.input.released_focus() else {
            return;
        };
        self.reconcile_editor_windows();
        if let Some(plan) = self.window_session.restore_focus_plan(
            capture,
            &self.state.window_version(),
            self.extension.contributions().version(),
        ) {
            let _ = self.apply_window_plan(plan);
        }
        self.project_window_activation();
    }
}

impl App {

    pub(in crate::app) fn project_input_hints(&mut self) {
        let hints = self.input.hints().cloned();
        self.ui.set(hints, |ui| &mut ui.key_hints);
    }
}
