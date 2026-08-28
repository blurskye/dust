use super::*;

impl InteractiveApplication {
    pub(super) fn retire_terminal(&mut self, id: PtyId, request_close: bool) {
        if request_close {
            let _ = self.ptys.close(id);
        }
        let was_active = self.active_terminal == Some(id);
        let was_focused = was_active && self.terminal_focused;
        if self.terminals.remove(&id).is_some() {
            self.terminal_set_revision = self.terminal_set_revision.saturating_add(1);
        }
        self.dap_terminal_responses.remove(&id);
        if was_active {
            let mut remaining = self.terminals.keys().copied().collect::<Vec<_>>();
            remaining.sort_unstable_by_key(|candidate| candidate.0);
            self.active_terminal = remaining.first().copied();
            self.terminal_focused = was_focused && self.active_terminal.is_some();
        }
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.ensure_cursor_visible();
    }

    pub(super) fn enter_terminal_normal_mode(&mut self) {
        let Some(id) = self.active_terminal else {
            return;
        };
        let Some(terminal) = self.terminals.get_mut(&id) else {
            return;
        };
        let (column, row) = terminal.screen.cursor();
        let cursor = if terminal.mode == TerminalPanelMode::Input {
            (terminal.screen.scrollback_len().saturating_add(row), column)
        } else {
            terminal
                .normal_cursor
                .unwrap_or((terminal.screen.scrollback_len().saturating_add(row), column))
        };
        terminal.mode = TerminalPanelMode::Normal;
        terminal.normal_cursor = Some(cursor);
        terminal.visual_anchor = None;
        terminal.visual_linewise = false;
        keep_terminal_cursor_visible(terminal);
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }

    pub(super) fn enter_terminal_input_mode(&mut self) {
        let Some(id) = self.active_terminal else {
            return;
        };
        let Some(terminal) = self.terminals.get_mut(&id) else {
            return;
        };
        terminal.mode = TerminalPanelMode::Input;
        terminal.normal_cursor = None;
        terminal.visual_anchor = None;
        terminal.visual_linewise = false;
        terminal.scroll_offset = 0;
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }

    pub(super) fn move_terminal_normal_cursor(&mut self, motion: &str) {
        let Some(id) = self.active_terminal else {
            return;
        };
        let Some(terminal) = self.terminals.get_mut(&id) else {
            return;
        };
        if terminal.mode == TerminalPanelMode::Input {
            return;
        }
        let history_len = terminal.screen.history_len();
        if history_len == 0 {
            return;
        }
        let mut cursor = clamp_terminal_cursor(
            &terminal.screen,
            terminal.normal_cursor.unwrap_or((history_len - 1, 0)),
        );
        match motion {
            "left" => cursor.1 = cursor.1.saturating_sub(1),
            "right" => {
                cursor.1 = cursor.1.saturating_add(1).min(
                    terminal
                        .screen
                        .history_row(cursor.0)
                        .map_or(0, |row| row.len().saturating_sub(1)),
                );
            }
            "up" => cursor.0 = cursor.0.saturating_sub(1),
            "down" => cursor.0 = cursor.0.saturating_add(1).min(history_len - 1),
            "line_start" => cursor.1 = 0,
            "line_end" => {
                cursor.1 = terminal
                    .screen
                    .history_row(cursor.0)
                    .map_or(0, terminal_row_end);
            }
            "first" => cursor = (0, 0),
            "last" => {
                cursor.0 = history_len - 1;
                cursor.1 = terminal
                    .screen
                    .history_row(cursor.0)
                    .map_or(0, terminal_row_end);
            }
            _ => {
                self.show_error("unknown terminal normal-mode motion");
                return;
            }
        }
        cursor = clamp_terminal_cursor(&terminal.screen, cursor);
        terminal.normal_cursor = Some(cursor);
        keep_terminal_cursor_visible(terminal);
        self.sync_terminal_panel();
    }

    pub(super) fn start_terminal_visual(&mut self, linewise: bool) {
        let Some(id) = self.active_terminal else {
            return;
        };
        let Some(terminal) = self.terminals.get_mut(&id) else {
            return;
        };
        if terminal.mode == TerminalPanelMode::Input {
            return;
        }
        let cursor = terminal.normal_cursor.unwrap_or((
            terminal.screen.scrollback_len() + terminal.screen.cursor().1,
            terminal.screen.cursor().0,
        ));
        terminal.mode = TerminalPanelMode::Visual;
        terminal.visual_anchor = Some(cursor);
        terminal.visual_linewise = linewise;
        keep_terminal_cursor_visible(terminal);
        self.sync_terminal_panel();
    }

    pub(super) fn yank_terminal_text(&mut self, line: bool) {
        let Some(id) = self.active_terminal else {
            return;
        };
        let Some(terminal) = self.terminals.get_mut(&id) else {
            return;
        };
        if terminal.mode == TerminalPanelMode::Input {
            return;
        }
        let cursor = clamp_terminal_cursor(
            &terminal.screen,
            terminal
                .normal_cursor
                .unwrap_or((terminal.screen.history_len().saturating_sub(1), 0)),
        );
        let (anchor, linewise) = if line {
            (cursor, true)
        } else if terminal.mode == TerminalPanelMode::Visual {
            (
                terminal.visual_anchor.unwrap_or(cursor),
                terminal.visual_linewise,
            )
        } else {
            (cursor, true)
        };
        let text = terminal_yank_text(&terminal.screen, anchor, cursor, linewise);
        if text.is_empty() {
            return;
        }
        terminal.mode = TerminalPanelMode::Normal;
        terminal.visual_anchor = None;
        terminal.visual_linewise = false;
        self.state.registers.set(
            '"',
            RegisterValue {
                text,
                shape: if linewise {
                    RegisterShape::Line
                } else {
                    RegisterShape::Character
                },
            },
            self.state.policy.registers_enabled,
            self.state.policy.mirror_named_to_unnamed,
        );
        self.ui.notification = Some(self.message("terminal_yanked", &[]));
        self.ui.notification_error = false;
        self.sync_terminal_panel();
    }

    pub(super) fn sync_terminal_panel(&mut self) {
        let Some(id) = self.active_terminal else {
            self.terminal_focused = false;
            self.ui.terminal_panel = None;
            self.reconcile_editor_windows();
            return;
        };
        let Some(terminal) = self.terminals.get(&id) else {
            self.active_terminal = None;
            self.terminal_focused = false;
            self.ui.terminal_panel = None;
            self.reconcile_editor_windows();
            return;
        };
        let (cursor_column, cursor_row) = terminal.screen.cursor();
        let (columns, rows) = terminal.screen.dimensions();
        let input_cursor = (
            cursor_column.min(columns.saturating_sub(1)),
            terminal
                .screen
                .scrollback_len()
                .saturating_add(cursor_row.min(rows.saturating_sub(1))),
        );
        let cursor = match terminal.mode {
            TerminalPanelMode::Input => terminal.screen.cursor_visible().then_some(input_cursor),
            TerminalPanelMode::Normal | TerminalPanelMode::Visual => {
                terminal.normal_cursor.map(|(row, column)| (column, row))
            }
        };
        let selection = (terminal.mode == TerminalPanelMode::Visual).then(|| {
            (
                terminal.visual_anchor.unwrap_or((
                    terminal.screen.scrollback_len().saturating_add(cursor_row),
                    cursor_column,
                )),
                terminal.normal_cursor.unwrap_or((
                    terminal.screen.scrollback_len().saturating_add(cursor_row),
                    cursor_column,
                )),
                terminal.visual_linewise,
            )
        });
        let history_rows = terminal.screen.history_len();
        let visible_rows = rows.max(1);
        let cells_start = if terminal.mode == TerminalPanelMode::Input {
            history_rows.saturating_sub(visible_rows)
        } else {
            terminal
                .scroll_offset
                .min(history_rows.saturating_sub(visible_rows))
        };
        let cells = terminal.screen.history_window(cells_start, visible_rows);
        let mut tabs = self
            .terminals
            .iter()
            .map(|(tab_id, tab)| TerminalTabState {
                id: tab_id.0,
                profile: tab.profile.clone(),
                running: tab.running,
                exit_code: tab.exit_code,
            })
            .collect::<Vec<_>>();
        tabs.sort_by_key(|tab| tab.id);
        self.ui.terminal_panel = Some(TerminalPanelState {
            id: id.0,
            profile: terminal.profile.clone(),
            tabs,
            mode: terminal.mode,
            running: terminal.running,
            exit_code: terminal.exit_code,
            truncated: terminal.truncated,
            unsupported_sequences: terminal.screen.unsupported_sequences(),
            focused: self.terminal_focused,
            columns: u16::try_from(columns).unwrap_or(u16::MAX),
            rows: u16::try_from(rows).unwrap_or(u16::MAX),
            cells,
            cells_start,
            cursor,
            selection,
            scrollback_lines: terminal.screen.scrollback_len(),
            scroll_offset: terminal.scroll_offset,
            requested_height: self.terminal_panel_height,
        });
        self.reconcile_editor_windows();
    }
}
