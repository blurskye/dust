//! Read-only, file-by-file commit inspection. Large commits do not load every patch at once.
use super::*;
use crate::ui::{Frame, Style};
use crossterm::cursor::SetCursorStyle;

/// Generated log rows prepared once on arrival, with per-commit attribution.
/// Resizing never changes this projection or the native cursor offsets.
#[derive(Clone)]
pub(super) struct RefLogEntry {
    pub id: git2::Oid,
    pub subject: String,
    pub attribution: Option<(String, String)>,
}
impl RefLogEntry {
    pub fn parse(text: &str) -> Vec<Self> {
        text.lines()
            .filter_map(|line| {
                let mut fields = line.splitn(4, '\0');
                let id = fields.next()?;
                let author = fields.next()?;
                let date = fields.next()?;
                let subject = fields.next()?;
                if id.len() != 40 {
                    return None;
                }
                Some(Self {
                    id: git2::Oid::from_str(id).ok()?,
                    subject: subject.to_owned(),
                    attribution: valid_ref_attribution(author, date)
                        .then(|| (author.to_owned(), date.to_owned())),
                })
            })
            .collect()
    }
    /// One row per commit, `hash  subject`, which find and the cursor read; the reader paints
    /// it as History does, with its author and date (`shell::reader`).
    pub fn preview(entries: &[Self]) -> Preview {
        let mut preview = Preview::default();
        for entry in entries {
            let row: Preview = format!("{}  {}", &entry.id.to_string()[..7], entry.subject).into();
            preview.rows.extend(row.rows.into_iter().take(1));
        }
        preview
    }
}

fn valid_ref_attribution(author: &str, date: &str) -> bool {
    let bytes = date.as_bytes();
    if author.trim().is_empty()
        || author.len() > 4096
        || author.chars().any(char::is_control)
        || bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return false;
    }
    let Ok(year) = date[..4].parse::<u32>() else {
        return false;
    };
    let Ok(month) = date[5..7].parse::<usize>() else {
        return false;
    };
    let Ok(day) = date[8..].parse::<u32>() else {
        return false;
    };
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (1..=12).contains(&month) && day > 0 && day <= days[month - 1]
}

pub(super) struct ChangedFile {
    pub path: PathBuf,
    pub added: String,
    pub removed: String,
}
pub(super) struct Commit {
    pub id: String,
    pub author: String,
    pub date: String,
    /// Compact tip identities captured with the commit read, not during rendering.
    pub decorations: String,
    pub message: String,
    pub files: Vec<ChangedFile>,
    pub selected: usize,
    pub(super) cursor: usize,
    pub(super) filter: filter::ListFilter,
    collapsed: BTreeSet<PathBuf>,
    /// `rows()`, which a 2,000-file commit would otherwise rebuild on every key and paint.
    rows_memo: memo::CommitMemo,
}
fn compact_decorations(text: &str, selected_ref: Option<&str>) -> String {
    let mut names = text
        .split(", ")
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    // HEAD/current first, local branches next, tags last. Keep useful tip identity
    // without turning a patch header into an inventory of every ref at that tip.
    names.sort_by_key(|name| {
        let rank = if *name == "HEAD" || name.starts_with("HEAD -> ") {
            0
        } else if selected_ref.is_some_and(|selected| {
            *name == selected || name.strip_prefix("tag: ") == Some(selected)
        }) {
            1
        } else if name.starts_with("tag: ") {
            3
        } else {
            2
        };
        (rank, *name)
    });
    let extra = names.len().saturating_sub(3);
    let mut labels = names
        .into_iter()
        .take(3)
        .map(|name| {
            if let Some(branch) = name.strip_prefix("HEAD -> ") {
                format!("HEAD→{branch}")
            } else if let Some(tag) = name.strip_prefix("tag: ") {
                format!("tag:{tag}")
            } else {
                name.to_owned()
            }
        })
        .collect::<Vec<_>>();
    if extra > 0 {
        labels.push(format!("+{extra}"));
    }
    labels.join(", ")
}

/// Refresh just display metadata when refs move but the selected commit ID does not.
/// This leaves the commit/file identity guards and the loaded patch untouched.
pub(super) fn tip_decorations(
    repository: &git2::Repository,
    id: &str,
    selected_ref: Option<&str>,
) -> String {
    let Ok(id) = git2::Oid::from_str(id) else {
        return String::new();
    };
    let mut names = Vec::new();
    let mut current = None;
    if let Ok(head) = repository.head()
        && head.peel_to_commit().is_ok_and(|commit| commit.id() == id)
    {
        current = head
            .name()
            .and_then(|name| name.strip_prefix("refs/heads/"))
            .map(str::to_owned);
        names.push(
            current
                .as_ref()
                .map_or_else(|| "HEAD".into(), |name| format!("HEAD -> {name}")),
        );
    }
    if let Ok(references) = repository.references() {
        for reference in references.flatten() {
            let Some(name) = reference.name() else {
                continue;
            };
            let label = if let Some(branch) = name.strip_prefix("refs/heads/") {
                if current.as_deref() == Some(branch) {
                    continue;
                }
                branch.to_owned()
            } else if let Some(tag) = name.strip_prefix("refs/tags/") {
                format!("tag: {tag}")
            } else {
                continue;
            };
            if reference
                .peel_to_commit()
                .is_ok_and(|commit| commit.id() == id)
            {
                names.push(label);
            }
        }
    }
    compact_decorations(&names.join(", "), selected_ref)
}

impl Commit {
    fn loading(id: String) -> Self {
        Self {
            id,
            author: String::new(),
            date: String::new(),
            decorations: String::new(),
            message: "Loading commit…".into(),
            files: Vec::new(),
            selected: 0,
            cursor: 0,
            filter: filter::ListFilter::default(),
            collapsed: BTreeSet::new(),
            rows_memo: memo::CommitMemo::default(),
        }
    }
    pub fn parse(text: &str, selected_ref: Option<&str>) -> Option<Self> {
        let mut parts = text.split('\0');
        let id = parts.next()?.to_owned();
        git2::Oid::from_str(&id).ok()?;
        let author = parts.next()?.into();
        let date = parts.next()?.into();
        let decorations = compact_decorations(parts.next()?, selected_ref);
        let message = parts.next()?.trim_end().into();
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
        let mut commit = Self {
            id,
            author,
            date,
            decorations,
            message,
            files,
            selected: 0,
            cursor: 0,
            filter: filter::ListFilter::default(),
            collapsed: BTreeSet::new(),
            rows_memo: memo::CommitMemo::default(),
        };
        commit.next_file(0);
        Some(commit)
    }
    /// `path`'s entry in `files`.
    pub(super) fn file(&self, path: &Path) -> Option<&ChangedFile> {
        self.files.get(self.rows_memo.position(&self.files, path)?)
    }
    pub(super) fn rows(&self) -> memo::Rows {
        self.rows_memo.get(&self.filter, &self.collapsed, || {
            self.filter.tree(
                self.files
                    .iter()
                    .map(|file| (file.path.clone(), String::new()))
                    .collect(),
                &self.collapsed,
            )
        })
    }
    pub(super) fn select_row(&mut self) -> bool {
        let Some(row) = self.rows().get(self.cursor).cloned() else {
            return false;
        };
        let Some(index) = self.rows_memo.position(&self.files, &row.value) else {
            return false;
        };
        let changed = self.selected != index;
        self.selected = index;
        changed
    }
    fn next_file(&mut self, delta: isize) -> bool {
        let old = self.selected;
        let files = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| self.filter.matches(&file.path.to_string_lossy()))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let Some(position) = files
            .iter()
            .position(|&index| index == self.selected)
            .or((!files.is_empty()).then_some(0))
        else {
            return false;
        };
        self.selected = files[position.saturating_add_signed(delta).min(files.len() - 1)];
        if let Some(file) = self.files.get(self.selected) {
            self.collapsed
                .retain(|parent| !file.path.starts_with(parent));
            self.cursor = self
                .rows()
                .iter()
                .position(|row| row.value == file.path)
                .unwrap_or(0);
        }
        old != self.selected
    }
    fn fold(&mut self, close: bool) {
        if self.filter.active() {
            return;
        }
        let Some(row) = self.rows().get(self.cursor).cloned() else {
            return;
        };
        if row.directory && (!close || !self.collapsed.contains(&row.value)) {
            if close {
                self.collapsed.insert(row.value);
            } else {
                self.collapsed.remove(&row.value);
            }
        } else if close && let Some(parent) = tree::parent(&self.rows(), &row.value) {
            self.collapsed.insert(parent.clone());
            self.cursor = self
                .rows()
                .iter()
                .position(|row| row.value == parent)
                .unwrap_or(0);
        }
    }
}
impl GitScreen {
    pub(super) fn load_commit(&mut self, id: String) {
        if self.commit.as_ref().is_some_and(|commit| commit.id == id) {
            return;
        }
        self.jobs.cancel_preview();
        self.commit = Some(Commit::loading(id.clone()));
        self.preview_args = None;
        self.preview_cursor = None;
        self.preview_anchor = None;
        self.preview = "Loading commit…".into();
        let result = self.run(
            Request::CommitInfo,
            vec![
                "show".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                // Strict ISO with the hour, so the reader can say `2h` (`chrome::when`).
                "--format=%H%x00%an%x00%aI%x00%D%x00%B%x00".into(),
                "--decorate=short".into(),
                "--decorate-refs=HEAD".into(),
                "--decorate-refs=refs/heads/*".into(),
                "--decorate-refs=refs/tags/*".into(),
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
        self.clear_hunks();
        self.jobs.cancel_preview();
        self.preview_anchor = None;
        let Some(commit) = &self.commit else { return };
        let Some(file) = commit.files.get(commit.selected) else {
            self.preview = "This commit has no file changes".into();
            return;
        };
        self.load_git_preview(vec![
            "--literal-pathspecs".into(),
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
        ]);
    }
    pub(super) fn message_key(&mut self, key: &KeyEvent) -> bool {
        let Some(offset) = self.message_scroll.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Escape | KeyCode::Char('q' | 'm') => self.message_scroll = None,
            KeyCode::Down | KeyCode::Char('j') => *offset = offset.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => *offset = offset.saturating_sub(1),
            KeyCode::PageDown => *offset = offset.saturating_add(self.message_visible.max(1)),
            KeyCode::PageUp => *offset = offset.saturating_sub(self.message_visible.max(1)),
            KeyCode::Home => *offset = 0,
            KeyCode::End => *offset = usize::MAX,
            _ => {}
        }
        true
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
            KeyCode::Char('[' | ']' | '<' | '>' | ':' | '0'..='5') => {
                self.commit_open = false;
                return false;
            }
            KeyCode::Char('j' | 'k') if key.modifiers.control => {
                self.commit_open = false;
                return false;
            }
            KeyCode::Tab => self.focus = if self.focus == 4 { 0 } else { 4 },
            KeyCode::Char('h') if key.modifiers.control => self.focus = 0,
            KeyCode::Char('l') if key.modifiers.control => self.focus = 4,
            KeyCode::Enter => {
                if self.focus != 4
                    && let Some(commit) = &mut self.commit
                    && let Some(row) = commit.rows().get(commit.cursor)
                    && row.directory
                    && !commit.filter.active()
                {
                    if !commit.collapsed.remove(&row.value) {
                        commit.collapsed.insert(row.value.clone());
                    }
                } else {
                    self.focus = 4;
                }
            }
            // `e` in the commit reader: the line in the editor, as in the worktree reader.
            KeyCode::Char('e')
                if self.focus == 4 && !key.modifiers.control && !key.modifiers.alt =>
            {
                self.outcome = self.open_line();
            }
            KeyCode::Char('c') if !key.modifiers.control && !key.modifiers.alt => {
                self.commit_form()
            }
            KeyCode::Char('|') => {
                self.split_diff = !self.split_diff;
                self.preview_offset = 0;
                if self.preview.cursor_byte.is_none() {
                    self.preview_cursor = None;
                }
            }
            KeyCode::Char('J' | 'K') | KeyCode::Down | KeyCode::Up | KeyCode::Char('j' | 'k') => {
                let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j' | 'J'));
                if self.focus == 4 && !matches!(key.code, KeyCode::Char('J' | 'K')) {
                    self.move_selection(if down { 1 } else { -1 });
                } else if let Some(commit) = &mut self.commit {
                    let delta = if down { 1 } else { -1 };
                    let changed = if matches!(key.code, KeyCode::Char('J' | 'K')) {
                        commit.next_file(delta)
                    } else {
                        commit.cursor = commit
                            .cursor
                            .saturating_add_signed(delta)
                            .min(commit.rows().len().saturating_sub(1));
                        commit.select_row()
                    };
                    if changed {
                        self.load_commit_file();
                    }
                }
            }
            KeyCode::Char('h' | 'l') | KeyCode::Left | KeyCode::Right if self.focus != 4 => {
                if let Some(commit) = &mut self.commit {
                    commit.fold(matches!(key.code, KeyCode::Char('h') | KeyCode::Left));
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.preview.search.leave_match();
                self.preview.cursor_byte = None;
                self.preview_column = self.preview_column.saturating_sub(8)
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.preview.search.leave_match();
                self.preview.cursor_byte = None;
                self.preview_column = self.preview_column.saturating_add(8)
            }
            KeyCode::PageDown => self.move_preview(self.preview_visible.max(1) as isize),
            KeyCode::PageUp => self.move_preview(-(self.preview_visible.max(1) as isize)),
            KeyCode::Home => self.preview_edge(false),
            KeyCode::End => self.preview_edge(true),
            KeyCode::Char('r') => self.load_commit_file(),
            _ => {}
        }
        true
    }
}

pub(super) fn message(frame: &mut Frame, app: &mut GitScreen, width: u16, height: u16) {
    if width < 40 || height < 12 {
        return;
    }
    let Some(commit) = &app.commit else { return };
    let w = width;
    let h = height.saturating_sub(2);
    let x = 0;
    let y = 1;
    let lines = crate::ui::render::wrap_plain_text(
        &commit.message,
        usize::from(w - 4),
        crate::config::AmbiguousWidth::Narrow,
    );
    let visible = usize::from(h - 4);
    app.message_visible = visible;
    let offset = app
        .message_scroll
        .unwrap_or(0)
        .min(lines.len().saturating_sub(visible));
    app.message_scroll = Some(offset);
    for row in y..y + h {
        for col in x..x + w {
            frame.grid.set_cell(col, row, " ", Style::GitSurface(None));
        }
    }
    super::render::border(
        &mut frame.grid,
        x,
        y,
        w,
        h,
        &format!("Commit message · {}", &commit.id[..7]),
        true,
    );
    for (index, line) in lines.iter().skip(offset).take(visible).enumerate() {
        frame.grid.put_text_clipped(
            x + 2,
            y + 1 + index as u16,
            line,
            Style::GitSurface(None),
            x + w - 2,
        );
    }
    frame.grid.put_text_clipped(
        x + 2,
        y + h - 2,
        "Esc back · j/k scroll · PgUp/PgDn · Home/End",
        Style::Muted,
        x + w - 2,
    );
    super::render::viewport(frame, x + w - 2, y + h - 1, offset, visible, lines.len());
    frame.cursor = Some((x + 2, y + 1, SetCursorStyle::SteadyBlock));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorations_prioritize_actual_head_then_selected_ref_and_bound_other_tips() {
        assert_eq!(
            compact_decorations(
                "tag: release, topic/z, feature/source-search, HEAD -> main, aaa/unrelated",
                Some("feature/source-search"),
            ),
            "HEAD→main, feature/source-search, aaa/unrelated, +2"
        );
        assert_eq!(compact_decorations("", None), "");
        assert_eq!(
            compact_decorations(
                "main, HEAD, feature/source-search",
                Some("feature/source-search")
            ),
            "HEAD, feature/source-search, main"
        );
    }

    #[test]
    fn decorations_field_does_not_consume_message_or_numstat_records() {
        let parsed = Commit::parse(
            concat!(
                "0123456789012345678901234567890123456789\0",
                "Author\0",
                "2026-11-09\0",
                "HEAD -> main, feature/source-search\0",
                "Subject\n\nBody\n\0",
                "\n2\t1\tfile.rs\0",
            ),
            Some("feature/source-search"),
        )
        .unwrap();
        assert_eq!(parsed.decorations, "HEAD→main, feature/source-search");
        assert_eq!(parsed.message, "Subject\n\nBody");
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].path, PathBuf::from("file.rs"));
        assert_eq!(parsed.files[0].added, "2");
        assert_eq!(parsed.files[0].removed, "1");
    }
}

#[cfg(test)]
mod ref_log_tests {
    use super::*;

    #[test]
    fn ref_log_rejects_bad_optional_metadata_without_losing_literal_subjects() {
        let id = "0123456789012345678901234567890123456789";
        for (author, date) in [
            ("", "2026-01-01"),
            (" ", "2026-01-01"),
            ("bad\tname", "2026-01-01"),
            ("bad\u{1b}name", "2026-01-01"),
            ("Author", "2025-02-29"),
            ("Author", "2026-13-01"),
            ("Author", "2026-01-00"),
            ("Author", "2026-1-1"),
            ("Author", "日期日期日期日期日期"),
        ] {
            let entries = RefLogEntry::parse(&format!(
                "{id}\0{author}\0{date}\0Literal subject (HEAD -> other) 中文"
            ));
            assert_eq!(entries.len(), 1);
            assert!(entries[0].attribution.is_none());
            let preview = RefLogEntry::preview(&entries);
            assert_eq!(preview.rows.len(), 1);
            assert_eq!(
                preview.rows[0].text,
                "0123456  Literal subject (HEAD -> other) 中文"
            );
        }
        assert!(valid_ref_attribution("Zoë 中文", "2024-02-29"));
        assert!(!valid_ref_attribution("Author", "1900-02-29"));
        assert!(valid_ref_attribution("Author", "2000-02-29"));
        assert!(RefLogEntry::parse("0123456\0Author\x002026-01-01\0Subject").is_empty());
        assert!(RefLogEntry::parse(&format!("{id}\0old two-field protocol")).is_empty());
    }

    #[test]
    fn forty_ref_entries_keep_one_row_each_with_their_own_metadata_in_fixed_order() {
        let input = (0..40)
            .map(|i| format!("{i:040x}\0Author {i}\x002026-11-12\0Subject {i} 中文"))
            .collect::<Vec<_>>()
            .join("\n");
        let entries = RefLogEntry::parse(&input);
        let preview = RefLogEntry::preview(&entries);
        assert_eq!(preview.rows.len(), 40);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(
                preview.rows[i].text,
                format!("{}  Subject {i} 中文", &entry.id.to_string()[..7])
            );
            assert_eq!(
                entry.attribution,
                Some((format!("Author {i}"), "2026-11-12".into()))
            );
        }
        assert!(preview.same_content(&RefLogEntry::preview(&entries)));
        assert!(!preview.patch && preview.cursor_byte.is_none() && preview.split.is_empty());
    }

    #[test]
    fn prepared_ref_log_does_not_parse_subject_punctuation_as_ref_identity() {
        let entries = RefLogEntry::parse(concat!(
            "0123456789012345678901234567890123456789\0First Author\x002026-11-12\0Subject (HEAD -> main, other/ref)\n",
            "1234567890123456789012345678901234567890\0Older Author\x002025-06-08\0Second subject\n",
        ));
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[1].attribution,
            Some(("Older Author".into(), "2025-06-08".into()))
        );
        let preview = RefLogEntry::preview(&entries);
        assert_eq!(
            preview.rows[0].text,
            "0123456  Subject (HEAD -> main, other/ref)"
        );
        assert_eq!(preview.rows[1].text, "1234567  Second subject");
        assert_eq!(preview.rows.len(), 2);
        assert!(
            preview
                .rows
                .iter()
                .all(|row| row.path.is_none() && row.marker.is_none())
        );
        assert!(!preview.patch);
        assert!(RefLogEntry::parse("not-an-oid\0subject").is_empty());
    }
}
