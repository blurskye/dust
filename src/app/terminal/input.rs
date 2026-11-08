//! Terminal byte encoding, modal cursor/selection, scrollback and derived screen projection.
use crate::app::terminal::TerminalRuntime;
use crate::app::terminal::Terminals;
use crate::app::workspace::Workspace;
use crate::process::PtyId;
use crate::ui::PtyScreen;
use crate::ui::TerminalPanelMode;
use crate::ui::TerminalPanelState;
use crate::ui::TerminalTabState;
use std::sync::Arc;
impl Terminals {
    pub(in crate::app) fn send(
        &self,
        id: PtyId,
        input: TerminalInput<'_>,
        workspace: &Workspace,
    ) -> Result<bool, String> {
        let tab = self.tabs.get(&id).ok_or("terminal closed")?;
        if tab.scope != workspace.version() || !Arc::ptr_eq(&tab.epoch, &self.epoch) {
            return Err("terminal input source is stale".into());
        }
        if tab.stopping {
            return Err("terminal stop is pending".into());
        }
        if tab.mode != TerminalPanelMode::Input
            && !matches!(input, TerminalInput::Text(_) | TerminalInput::Focus(_))
        {
            return Ok(false);
        }
        let byte_limit = crate::app::terminal::TERMINAL_MAXIMUM_INPUT_BYTES;
        let bytes = match input {
            TerminalInput::Text(text) => {
                if text.len() > byte_limit {
                    return Err("terminal input exceeds limit".into());
                }
                Some(text.as_bytes().to_vec())
            }
            TerminalInput::Paste(text) => {
                crate::ui::encode_paste(text, tab.screen.bracketed_paste(), byte_limit)
            }
            TerminalInput::Focus(focused) => {
                crate::ui::encode_focus(focused, tab.screen.focus_reporting()).map(<[u8]>::to_vec)
            }
            TerminalInput::Key(key) => {
                Some(crate::ui::encode_key(key, tab.screen.application_cursor()).to_vec())
            }
            TerminalInput::Mouse(event, modifiers, column, row, rect) => {
                crate::ui::encode_terminal_mouse_event(crate::ui::TerminalMouseEncoding {
                    event,
                    modifiers,
                    column,
                    row,
                    left: rect.x,
                    top: rect.y,
                    width: rect.width,
                    height: rect.height,
                    mode: tab.screen.mouse_mode(),
                    sgr: tab.screen.sgr_mouse(),
                    byte_limit,
                })
            }
        };
        let Some(bytes) = bytes else {
            return Ok(false);
        };
        let pty = tab.pty.ok_or("terminal has not started")?;
        let reservation = self.service.reserve_input(pty).map_err(|e| e.to_string())?;
        reservation.submit(bytes).map_err(|e| e.to_string())?;
        Ok(true)
    }
    pub(in crate::app) fn enter_terminal_normal_mode(&mut self, id: PtyId) {
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
        terminal.normal_cursor = Some(clamp_terminal_cursor(&terminal.screen, cursor));
        terminal.visual_anchor = None;
        terminal.visual_linewise = false;
        keep_terminal_cursor_visible(terminal);
    }

    pub(in crate::app) fn enter_terminal_input_mode(&mut self, id: PtyId) {
        let Some(terminal) = self.tabs.get_mut(&id) else {
            return;
        };
        terminal.mode = TerminalPanelMode::Input;
        terminal.normal_cursor = None;
        terminal.visual_anchor = None;
        terminal.visual_linewise = false;
        terminal.scroll_offset = 0;
    }

    pub(in crate::app) fn move_terminal_normal_cursor(
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
                let step = terminal
                    .screen
                    .history_row(cursor.0)
                    .and_then(|row| {
                        row.get(cursor.1 + 1)
                            .map(|cell| if cell.continuation { 2 } else { 1 })
                    })
                    .unwrap_or(1);
                cursor.1 = cursor.1.saturating_add(step).min(
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
                    .map_or(0, |row| terminal_row_end(&row));
            }
            "first" => cursor = (0, 0),
            "last" => {
                cursor.0 = history_len - 1;
                cursor.1 = terminal
                    .screen
                    .history_row(cursor.0)
                    .map_or(0, |row| terminal_row_end(&row));
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

    pub(in crate::app) fn start_terminal_visual(&mut self, id: PtyId, linewise: bool) {
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

    pub(in crate::app) fn yank_terminal_text(
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
    pub(in crate::app) fn projection(
        &self,
        id: PtyId,
        focused: bool,
        requested_height: u16,
    ) -> Option<TerminalPanelState> {
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
        // A side terminal's window shows only itself; the panel shows the other tabs.
        let mut tabs = self
            .tabs
            .iter()
            .filter(|(tab_id, tab)| {
                if terminal.side {
                    **tab_id == id
                } else {
                    !tab.side
                }
            })
            .map(|(tab_id, tab)| TerminalTabState {
                phase: tab.phase(),
                id: tab_id.0,
                profile: tab.title(),
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
            focused,
            input_hint: None,
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

impl TerminalRuntime {
    fn title(&self) -> String {
        if self.agent {
            return self.profile.clone();
        }
        if let Some(title) = self.screen.title() {
            return title.to_owned();
        }
        let program = self
            .program
            .file_name()
            .unwrap_or(self.program.as_os_str())
            .to_string_lossy();
        if self.cwd.as_os_str().is_empty() {
            return program.into_owned();
        }
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        format!(
            "{program} · {}",
            terminal_directory(&self.cwd, home.as_deref())
        )
    }
}

fn terminal_directory(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    let path = home
        .and_then(|home| path.strip_prefix(home).ok())
        .map_or_else(
            || path.display().to_string(),
            |relative| {
                if relative.as_os_str().is_empty() {
                    "~".into()
                } else {
                    format!("~/{}", relative.display())
                }
            },
        );
    let characters = path.chars().count();
    if characters > 48 {
        format!(
            "…{}",
            path.chars().skip(characters - 47).collect::<String>()
        )
    } else {
        path
    }
}

pub(in crate::app) enum TerminalInput<'a> {
    Text(&'a str),
    Paste(&'a str),
    Focus(bool),
    Key(crate::ui::PtyInputKey),
    Mouse(
        crate::editor::input::TerminalMouseInput,
        crate::editor::input::Modifiers,
        u16,
        u16,
        crate::windows::WindowRect,
    ),
}
fn terminal_row_end(row: &[crate::ui::PtyCell]) -> usize {
    row.iter()
        .rposition(|cell| !cell.continuation && cell.text != " ")
        .unwrap_or(0)
}

pub(super) fn clamp_terminal_cursor(screen: &PtyScreen, cursor: (usize, usize)) -> (usize, usize) {
    let row = cursor.0.min(screen.history_len().saturating_sub(1));
    let cells = screen.history_row(row).unwrap_or_default();
    let mut column = cursor.1.min(cells.len().saturating_sub(1));
    while column > 0 && cells.get(column).is_some_and(|cell| cell.continuation) {
        column -= 1;
    }
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
    let first = clamp_terminal_cursor(screen, first);
    let second = clamp_terminal_cursor(screen, second);
    let (start, end) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    let start = if linewise { (start.0, 0) } else { start };
    let end = if linewise {
        (end.0, screen.dimensions().0.saturating_sub(1))
    } else {
        end
    };
    let mut output = screen.selection_text(start, end);
    if linewise {
        output.push('\n');
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

#[cfg(test)]
mod tests {
    use crate::app::terminal::input::*;

    #[test]
    fn terminal_directories_use_home_and_keep_long_path_suffixes() {
        use std::path::Path;
        assert_eq!(
            terminal_directory(
                Path::new("/home/user/project"),
                Some(Path::new("/home/user"))
            ),
            "~/project"
        );
        assert_eq!(
            terminal_directory(Path::new("/home/user"), Some(Path::new("/home/user"))),
            "~"
        );
        let long = format!("/workspace/{}/project", "界".repeat(60));
        let title = terminal_directory(Path::new(&long), None);
        assert_eq!(title.chars().count(), 48);
        assert!(title.starts_with('…') && title.ends_with("/project"));
    }

    #[test]
    fn terminal_yanks_preserve_empty_rows_without_adding_spaces() {
        let mut screen = PtyScreen::new(10, 3, 4);
        screen.feed_bytes(b"\r\nx\r\n");
        assert_eq!(terminal_yank_text(&screen, (0, 0), (0, 0), true), "\n");
        assert_eq!(terminal_yank_text(&screen, (1, 0), (1, 0), true), "x\n");
        assert_eq!(terminal_yank_text(&screen, (0, 0), (2, 0), true), "\nx\n\n");
    }

    #[test]
    fn terminal_yanks_join_soft_wraps_but_keep_hard_newlines() {
        let mut screen = PtyScreen::new(4, 2, 8);
        screen.feed_bytes(b"abcdefgh\r\nijkl\r\nmn");
        assert!(screen.scrollback_len() > 0);
        assert_eq!(
            terminal_yank_text(&screen, (0, 0), (1, 3), false),
            "abcdefgh"
        );
        assert_eq!(
            terminal_yank_text(&screen, (0, 1), (2, 1), false),
            "bcdefgh\nij"
        );
        assert_eq!(
            terminal_yank_text(&screen, (2, 1), (0, 1), false),
            "bcdefgh\nij"
        );
        assert_eq!(
            terminal_yank_text(&screen, (0, 0), (1, 3), true),
            "abcdefgh\n"
        );
    }
}
