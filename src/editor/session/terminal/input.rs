//! Terminal byte encoding, modal cursor/selection, scrollback and derived screen projection.
use super::{
    Arc, PtyId, PtyScreen, TerminalPanelMode, TerminalPanelState, TerminalRuntime, TerminalSession,
    TerminalTabState, WorkspaceSession,
};
impl TerminalSession {
    pub(in crate::editor::session) fn send(
        &self,
        id: PtyId,
        input: TerminalInput<'_>,
        workspace: &WorkspaceSession,
    ) -> Result<bool, String> {
        let tab = self.tabs.get(&id).ok_or("terminal closed")?;
        if tab.scope != workspace.version() || !Arc::ptr_eq(&tab.epoch, &self.epoch) {
            return Err("terminal input source retired".into());
        }
        if tab.stopping {
            return Err("terminal stop is pending".into());
        }
        if tab.mode != TerminalPanelMode::Input
            && !matches!(input, TerminalInput::Text(_) | TerminalInput::Focus(_))
        {
            return Ok(false);
        }
        let pty = tab.pty.ok_or("terminal has not started")?;
        let admission = self.service.reserve_input(pty).map_err(|e| e.to_string())?;
        let ceiling = super::TERMINAL_MAXIMUM_INPUT_BYTES;
        let bytes = match input {
            TerminalInput::Text(text) => {
                if text.len() > ceiling {
                    return Err("terminal input exceeds limit".into());
                }
                Some(text.as_bytes().to_vec())
            }
            TerminalInput::Paste(text) => {
                crate::ui::encode_paste(text, tab.screen.bracketed_paste(), ceiling)
            }
            TerminalInput::Focus(focused) => {
                crate::ui::encode_focus(focused, tab.screen.focus_reporting()).map(<[u8]>::to_vec)
            }
            TerminalInput::Key(key) => {
                Some(crate::ui::encode_key(key, tab.screen.application_cursor()).to_vec())
            }
            TerminalInput::Mouse(event, column, row, rect) => {
                crate::ui::encode_terminal_mouse_event(crate::ui::TerminalMouseEncoding {
                    event,
                    column,
                    row,
                    left: rect.x,
                    top: rect.y,
                    width: rect.width,
                    height: rect.height,
                    mode: tab.screen.mouse_mode(),
                    sgr: tab.screen.sgr_mouse(),
                    ceiling,
                })
            }
        };
        let Some(bytes) = bytes else {
            return Ok(false);
        };
        admission.submit(bytes).map_err(|e| e.to_string())?;
        Ok(true)
    }
    pub(in crate::editor::session) fn enter_terminal_normal_mode(&mut self, id: PtyId) {
        let Some(terminal) = self.tabs.get_mut(&id) else {
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
    }

    pub(in crate::editor::session) fn enter_terminal_input_mode(&mut self, id: PtyId) {
        let Some(terminal) = self.tabs.get_mut(&id) else {
            return;
        };
        terminal.mode = TerminalPanelMode::Input;
        terminal.normal_cursor = None;
        terminal.visual_anchor = None;
        terminal.visual_linewise = false;
        terminal.scroll_offset = 0;
    }

    pub(in crate::editor::session) fn move_terminal_normal_cursor(
        &mut self,
        id: PtyId,
        motion: &str,
    ) -> Result<(), String> {
        let Some(terminal) = self.tabs.get_mut(&id) else {
            return Ok(());
        };
        if terminal.mode == TerminalPanelMode::Input {
            return Ok(());
        }
        let history_len = terminal.screen.history_len();
        if history_len == 0 {
            return Ok(());
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
                return Err("unknown terminal normal-mode motion".into());
            }
        }
        cursor = clamp_terminal_cursor(&terminal.screen, cursor);
        terminal.normal_cursor = Some(cursor);
        keep_terminal_cursor_visible(terminal);
        Ok(())
    }

    pub(in crate::editor::session) fn start_terminal_visual(&mut self, id: PtyId, linewise: bool) {
        let Some(terminal) = self.tabs.get_mut(&id) else {
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
    }

    pub(in crate::editor::session) fn yank_terminal_text(
        &mut self,
        id: PtyId,
        line: bool,
    ) -> Option<(String, bool)> {
        let terminal = self.tabs.get_mut(&id)?;
        if terminal.mode == TerminalPanelMode::Input {
            return None;
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
            return None;
        }
        terminal.mode = TerminalPanelMode::Normal;
        terminal.visual_anchor = None;
        terminal.visual_linewise = false;
        Some((text, linewise))
    }
    pub(in crate::editor::session) fn projection(
        &self,
        focused: Option<u64>,
        requested_height: u16,
    ) -> Option<TerminalPanelState> {
        let id = focused
            .map(PtyId)
            .filter(|id| self.contains(*id))
            .or(self.displayed())?;
        let terminal = self.tabs.get(&id)?;
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
            .tabs
            .iter()
            .map(|(tab_id, tab)| TerminalTabState {
                phase: tab.phase(),
                id: tab_id.0,
                profile: tab.profile.clone(),
                running: tab.running,
                exit_code: tab.exit_code,
            })
            .collect::<Vec<_>>();
        tabs.sort_by_key(|tab| tab.id);
        Some(TerminalPanelState {
            id: id.0,
            profile: terminal.profile.clone(),
            tabs,
            mode: terminal.mode,
            running: terminal.running,
            exit_code: terminal.exit_code,
            truncated: terminal.truncated,
            unsupported_sequences: terminal.screen.unsupported_sequences(),
            focused: focused == Some(id.0),
            columns: u16::try_from(columns).unwrap_or(u16::MAX),
            rows: u16::try_from(rows).unwrap_or(u16::MAX),
            cells,
            cells_start,
            cursor,
            selection,
            scrollback_lines: terminal.screen.scrollback_len(),
            scroll_offset: terminal.scroll_offset,
            requested_height,
        })
    }
}

pub(in crate::editor::session) enum TerminalInput<'a> {
    Text(&'a str),
    Paste(&'a str),
    Focus(bool),
    Key(crate::ui::PtyInputKey),
    Mouse(
        crate::editor::input::TerminalMouseInput,
        u16,
        u16,
        crate::ui::window::WindowRect,
    ),
}
fn terminal_row_end(row: &[crate::ui::PtyCell]) -> usize {
    row.iter()
        .rposition(|cell| !cell.continuation && cell.text != " ")
        .unwrap_or(0)
}

pub(super) fn clamp_terminal_cursor(screen: &PtyScreen, cursor: (usize, usize)) -> (usize, usize) {
    let row = cursor.0.min(screen.history_len().saturating_sub(1));
    let column = cursor.1.min(
        screen
            .history_row(row)
            .map_or(0, |cells| cells.len().saturating_sub(1)),
    );
    (row, column)
}

fn terminal_yank_text(
    screen: &PtyScreen,
    first: (usize, usize),
    second: (usize, usize),
    linewise: bool,
) -> String {
    if screen.history_len() == 0 {
        return String::new();
    }
    let (start, end) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    let mut output = String::new();
    for row_index in start.0..=end.0.min(screen.history_len().saturating_sub(1)) {
        let Some(row) = screen.history_row(row_index) else {
            continue;
        };
        let first_column = if linewise || row_index > start.0 {
            0
        } else {
            start.1
        };
        let last_column = if linewise || row_index < end.0 {
            terminal_row_end(row)
        } else {
            end.1.min(row.len().saturating_sub(1))
        };
        if first_column <= last_column {
            for cell in row
                .iter()
                .take(last_column.saturating_add(1))
                .skip(first_column)
            {
                if !cell.continuation {
                    output.push_str(&cell.text);
                }
            }
        }
        if row_index < end.0 || linewise {
            output.push('\n');
        }
    }
    output
}

pub(super) fn keep_terminal_cursor_visible(terminal: &mut TerminalRuntime) {
    let total = terminal.screen.scrollback_len() + terminal.screen.dimensions().1;
    let visible = terminal.screen.dimensions().1.max(1);
    let Some((row, _)) = terminal.normal_cursor else {
        terminal.scroll_offset = total.saturating_sub(visible);
        return;
    };
    if row < terminal.scroll_offset {
        terminal.scroll_offset = row;
    } else if row >= terminal.scroll_offset.saturating_add(visible) {
        terminal.scroll_offset = row.saturating_add(1).saturating_sub(visible);
    }
    terminal.scroll_offset = terminal.scroll_offset.min(total.saturating_sub(visible));
}
