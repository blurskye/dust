//! Read-only, file-by-file commit inspection. Large commits do not load every patch at once.
use super::*;
use crate::ui::{Frame, Style};
use crossterm::cursor::SetCursorStyle;

pub(super) struct ChangedFile {
    pub path: PathBuf,
    pub added: String,
    pub removed: String,
}
pub(super) struct Commit {
    pub id: String,
    pub author: String,
    pub date: String,
    pub title: String,
    pub files: Vec<ChangedFile>,
    pub selected: usize,
}
impl Commit {
    fn loading(id: String) -> Self {
        Self {
            id,
            author: String::new(),
            date: String::new(),
            title: "Loading commit…".into(),
            files: Vec::new(),
            selected: 0,
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('\0');
        let id = parts.next()?.to_owned();
        git2::Oid::from_str(&id).ok()?;
        let author = parts.next()?.into();
        let date = parts.next()?.into();
        let title = parts.next()?.into();
        let files = parts
            .filter_map(|record| {
                let mut fields = record.trim_start_matches('\n').splitn(3, '\t');
                Some(ChangedFile {
                    added: fields.next()?.into(),
                    removed: fields.next()?.into(),
                    path: fields.next()?.into(),
                })
            })
            .collect();
        Some(Self {
            id,
            author,
            date,
            title,
            files,
            selected: 0,
        })
    }
}
impl GitScreen {
    pub(super) fn load_commit(&mut self, id: String) {
        if self.commit.as_ref().is_some_and(|commit| commit.id == id) {
            return;
        }
        self.jobs.cancel_preview();
        self.commit = Some(Commit::loading(id.clone()));
        self.preview = "Loading commit…".into();
        let result = self.run(
            Request::CommitInfo,
            vec![
                "show".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--format=%H%x00%an%x00%as%x00%s%x00".into(),
                "--numstat".into(),
                "-z".into(),
                "--first-parent".into(),
                "--no-renames".into(),
                id.into(),
                "--".into(),
            ],
        );
        self.report(result);
    }
    pub(super) fn load_commit_file(&mut self) {
        self.jobs.cancel_preview();
        self.preview_line = 0;
        self.preview_column = 0;
        let Some(commit) = &self.commit else { return };
        let Some(file) = commit.files.get(commit.selected) else {
            self.preview = "This commit has no file changes".into();
            return;
        };
        self.preview = "Loading file diff…".into();
        let result = self.run(
            Request::Preview,
            vec![
                "show".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--format=".into(),
                "--first-parent".into(),
                "--no-renames".into(),
                "--patch".into(),
                commit.id.clone().into(),
                "--".into(),
                file.path.clone().into_os_string(),
            ],
        );
        self.report(result);
    }
    pub(super) fn commit_key(&mut self, key: &KeyEvent) -> bool {
        if !self.commit_open {
            return false;
        }
        match key.code {
            KeyCode::Escape | KeyCode::Char('q') => {
                self.commit_open = false;
                self.focus = 2;
            }
            KeyCode::Char('[' | ']' | ':') => {
                self.commit_open = false;
                return false;
            }
            KeyCode::Tab => self.focus = if self.focus == 4 { 2 } else { 4 },
            KeyCode::Char('h') if key.modifiers.control => self.focus = 2,
            KeyCode::Char('l') if key.modifiers.control => self.focus = 4,
            KeyCode::Enter => self.focus = 4,
            KeyCode::Char('s') => {
                self.split_diff = !self.split_diff;
                self.preview_line = 0;
            }
            KeyCode::Char('J' | 'K') | KeyCode::Down | KeyCode::Up | KeyCode::Char('j' | 'k') => {
                let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j' | 'J'));
                if self.focus == 4 && !matches!(key.code, KeyCode::Char('J' | 'K')) {
                    self.move_selection(if down { 1 } else { -1 });
                } else if let Some(commit) = &mut self.commit {
                    let old = commit.selected;
                    commit.selected = commit
                        .selected
                        .saturating_add_signed(if down { 1 } else { -1 })
                        .min(commit.files.len().saturating_sub(1));
                    if old != commit.selected {
                        self.load_commit_file();
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.preview_column = self.preview_column.saturating_sub(8)
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.preview_column = self.preview_column.saturating_add(8)
            }
            KeyCode::PageDown => {
                self.preview_line =
                    (self.preview_line + 20).min(self.preview_rows.saturating_sub(1))
            }
            KeyCode::PageUp => self.preview_line = self.preview_line.saturating_sub(20),
            KeyCode::Home => self.preview_line = 0,
            KeyCode::End => self.preview_line = self.preview_rows.saturating_sub(1),
            KeyCode::Char('r') => {
                self.commit = None;
                self.load_preview();
            }
            _ => {}
        }
        true
    }
}

pub(super) fn files(frame: &mut Frame, app: &GitScreen, left: u16, body: u16) {
    super::render::section(
        &mut frame.grid,
        0,
        1,
        left,
        body,
        "Changed files",
        app.focus != 4,
    );
    let Some(commit) = &app.commit else { return };
    let visible = usize::from(body.saturating_sub(2) / 2).max(1);
    let offset = commit.selected.saturating_sub(visible - 1);
    for (index, file) in commit.files.iter().enumerate().skip(offset).take(visible) {
        let y = 2 + (index - offset) as u16 * 2;
        let selected = index == commit.selected;
        let style = if selected {
            if app.focus == 4 {
                Style::GitInactiveSelection(None)
            } else {
                Style::GitSelected(None)
            }
        } else {
            Style::GitSurface(None)
        };
        if selected {
            for yy in y..y + 2 {
                for x in 1..left - 1 {
                    frame.grid.set_cell(x, yy, " ", style);
                }
            }
        }
        let name = file.path.file_name().unwrap_or_default().to_string_lossy();
        frame.grid.put_text_clipped(
            2,
            y,
            &super::render::ellipsize(&name, usize::from(left - 4)),
            style,
            left - 2,
        );
        let parent = file
            .path
            .parent()
            .unwrap_or(Path::new(""))
            .display()
            .to_string();
        let x = if file.added == "-" {
            frame
                .grid
                .put_text_clipped(2, y + 1, "binary", Style::Muted, left - 2)
        } else {
            let x = frame.grid.put_text_clipped(
                2,
                y + 1,
                &format!("+{} ", file.added),
                Style::GitSignAdded,
                left - 2,
            );
            frame.grid.put_text_clipped(
                x,
                y + 1,
                &format!("−{}", file.removed),
                Style::GitSignDeleted,
                left - 2,
            )
        };
        frame.grid.put_text_clipped(
            x + 1,
            y + 1,
            &super::render::ellipsize(&parent, usize::from((left - 2).saturating_sub(x + 1))),
            Style::Muted,
            left - 2,
        );
        if selected && app.focus != 4 {
            frame.cursor = Some((2, y, SetCursorStyle::SteadyBlock));
        }
    }
    if commit.files.is_empty() {
        frame
            .grid
            .put_text_clipped(2, 2, "No changed files", Style::Muted, left - 2);
        frame.cursor = Some((2, 2, SetCursorStyle::SteadyBlock));
    }
    super::render::viewport(frame, left - 2, body, offset, visible, commit.files.len());
}
