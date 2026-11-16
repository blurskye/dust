//! Vim-style modes on a modal screen, and the Git tab's leader chords.
//!
//! The editor is modal, so a screen its hosts open is too ([`GitScreen::configure_input`]):
//! a text box edits on the editor's engine (`textbox.rs`) starting in Insert mode, Esc goes to
//! Normal mode and a second Esc (or `q`) closes the box with its draft kept. The `:` prompt,
//! the reader's `/` find and a list's `/` filter are boxes too; Enter keeps, finds or runs
//! from either mode. A chip at the left of the footer says which mode is on, as the
//! editor's statusline does.
//!
//! The leader is the editor's (`input.keymaps.leader`) and the chords are the shared table's
//! (`github/keymap.rs`): on the Git tab `<leader> e` hides the rail, and the `lazygit` preset's
//! Space toggles staging. While the leader waits, the editor's which-key box lists what
//! finishes it; there is no timeout, and Esc or any other key cancels it.
use super::github::keymap::{Key, Named, Panel, Preset, Step, To};
use super::textbox::TextBox;
use super::*;
use crate::editor::EditorMode;
use crate::ui::Style;

/// What a modal screen's text box did with a form's key.
pub(super) enum BoxKey {
    /// The box took the key.
    Handled(Result<(), String>),
    /// Close the form as Esc does, keeping the draft (`q` or Esc in Normal mode).
    Close,
    /// The form handles the key as it always has.
    Pass,
}

/// The mode a modal screen is in, for its chip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    Normal,
    Insert,
    Visual,
}

/// `s` stages, `u` unstages and `-` toggles; the shared staging paths read this.
pub(super) fn stage_intent(code: &KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('s') => Some(true),
        KeyCode::Char('u') => Some(false),
        _ => None,
    }
}

/// The field a box edits: a free-text field that has focus, the `:` prompt and the `/` find
/// included.
fn box_field(form: &Form) -> Option<&Field> {
    let field = form.fields.get(form.selected)?;
    (!field.read_only && field.choices.is_empty()).then_some(field)
}

fn owner(form: &Form) -> (String, usize, usize) {
    (form.title.clone(), form.fields.len(), form.selected)
}

/// The owner title of the box over a list's filter query; no form has it.
const FILTER: &str = "\u{0}filter";

/// What a shared chord's footer word reads as in the keys sheet and the which-key box.
pub(super) fn chord_words(label: &str) -> String {
    match label {
        "sidebar" => "toggle sidebar".into(),
        "toggle" => "toggle: stage · unstage".into(),
        other => other.into(),
    }
}

fn mode_of(mode: EditorMode) -> InputMode {
    match mode {
        EditorMode::Insert | EditorMode::Replace => InputMode::Insert,
        EditorMode::VisualChar | EditorMode::VisualLine | EditorMode::VisualBlock => {
            InputMode::Visual
        }
        _ => InputMode::Normal,
    }
}

/// One key into a box. A one-line box keeps one line: a linewise put or `o` changes
/// nothing.
fn feed_box(text_box: &mut TextBox, key: &KeyEvent, multiline: bool) {
    let before = (text_box.text(), text_box.cursor());
    text_box.key(key);
    if !multiline && text_box.text().contains('\n') {
        let insert = mode_of(text_box.mode()) == InputMode::Insert;
        let registers = text_box.registers().clone();
        let owner = text_box.owner.clone();
        *text_box = TextBox::open(&before.0, before.1, insert, registers, owner);
    }
}

impl GitScreen {
    /// Modal or not: a modal screen edits its boxes with Vim keys and shows a mode chip.
    pub fn set_modal(&mut self, modal: bool) {
        self.modal = modal;
        if !modal {
            self.close_text_box();
        }
    }

    /// Reads what the editor's configuration says about input: its leader, the `ui.git.keys`
    /// preset and the statusline's mode words. The editor is modal, so the screen is too.
    pub fn configure_input(&mut self, config: &crate::config::Config) {
        self.set_key_preset(&config.input.keymaps.leader, config.ui.git.keys.name());
        // Forge tables are drawn with the editor's own table settings.
        super::github::configure_tables(&config.editor.markdown);
        let labels = &config.ui.status_line.mode_labels;
        for (slot, key) in ["normal", "insert", "visual_char"].iter().enumerate() {
            if let Some(label) = labels.get(*key) {
                self.mode_labels[slot] = label.clone();
            }
        }
        self.set_modal(true);
    }

    /// A screen that replaces this one (a tutor lesson) keeps its keys, modes and registers.
    pub(super) fn carry_input(&self, next: &mut GitScreen) {
        next.github.set_keys(self.github.key_table());
        next.modal = self.modal;
        next.mode_labels.clone_from(&self.mode_labels);
        next.box_registers = match &self.text_box {
            Some(text_box) => text_box.registers().clone(),
            None => self.box_registers.clone(),
        };
    }

    /// The mode the chip shows; `None` when the screen is not modal.
    pub fn input_mode(&self) -> Option<InputMode> {
        if !self.modal {
            return None;
        }
        if let Some(form) = &self.form {
            if box_field(form).is_none() {
                // A button, checkbox or choice row has focus.
                return Some(InputMode::Normal);
            }
            return Some(
                self.text_box
                    .as_ref()
                    .filter(|text_box| text_box.owner == owner(form))
                    .map_or(InputMode::Insert, |text_box| mode_of(text_box.mode())),
            );
        }
        if self
            .filter_target()
            .is_some_and(|target| self.list_filter(target).editing)
        {
            return Some(
                self.text_box
                    .as_ref()
                    .filter(|text_box| text_box.owner.0 == FILTER)
                    .map_or(InputMode::Insert, |text_box| mode_of(text_box.mode())),
            );
        }
        if self.github.actions_typing() {
            return Some(InputMode::Insert);
        }
        if self.page == 0 && (self.preview_anchor.is_some() || self.file_selection.active()) {
            return Some(InputMode::Visual);
        }
        Some(InputMode::Normal)
    }

    fn close_text_box(&mut self) {
        if let Some(text_box) = self.text_box.take() {
            self.box_registers = text_box.registers().clone();
        }
    }

    /// A form's key on a modal screen: the focused text box edits with it, unless it is one
    /// the form keeps (Tab, Ctrl+Enter, Alt options, a choice or checkbox, Enter on one line).
    pub(super) fn modal_form_key(&mut self, key: &KeyEvent) -> BoxKey {
        if !self.modal {
            return BoxKey::Pass;
        }
        self.sync_text_box();
        let Some(form) = self.form.as_mut() else {
            return BoxKey::Pass;
        };
        let Some(field) = box_field(form) else {
            return BoxKey::Pass;
        };
        let multiline = field.multiline;
        let control = key.modifiers.control;
        let typed = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Enter
                | KeyCode::Escape
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
        );
        let kept = !typed
            || key.modifiers.alt
            || (key.code == KeyCode::Enter && (control || !multiline))
            || (control && matches!(key.code, KeyCode::Char('a' | 'e' | 'u')));
        let Some(text_box) = self.text_box.as_mut() else {
            return BoxKey::Pass;
        };
        let normal = text_box.mode() == EditorMode::Normal && text_box.idle();
        if kept || (normal && key.code == KeyCode::Escape) {
            return BoxKey::Pass;
        }
        if normal && key.code == KeyCode::Char('q') && !control {
            return BoxKey::Close;
        }
        // `?` in Normal mode opens the keys sheet the footer offers; in Insert it is text.
        if normal && key.code == KeyCode::Char('?') && !control {
            self.help = true;
            return BoxKey::Handled(Ok(()));
        }
        feed_box(text_box, key, multiline);
        self.write_box();
        BoxKey::Handled(Ok(()))
    }

    /// A key while a list's filter is typed, on a modal screen: the query edits on the
    /// editor's engine as a text box does. Enter keeps the filter and ↑/↓ walk the matches in
    /// either mode; Esc in Normal mode (or `q`) clears and closes it, as Esc always has.
    pub(super) fn modal_filter_key(&mut self, target: usize, key: &KeyEvent) -> BoxKey {
        if !self.modal {
            return BoxKey::Pass;
        }
        self.sync_text_box();
        let control = key.modifiers.control;
        let typed = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Escape
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
        );
        let kept = !typed || key.modifiers.alt || (control && key.code == KeyCode::Char('u'));
        let Some(text_box) = self
            .text_box
            .as_ref()
            .filter(|text_box| text_box.owner.0 == FILTER)
        else {
            return BoxKey::Pass;
        };
        let normal = text_box.mode() == EditorMode::Normal && text_box.idle();
        if kept || (normal && key.code == KeyCode::Escape) {
            return BoxKey::Pass;
        }
        if normal && key.code == KeyCode::Char('q') && !control {
            return BoxKey::Close;
        }
        let previous = self.filter_identity(target);
        self.remember_filter_match(target);
        let text_box = self.text_box.as_mut().expect("checked filter box");
        let before = text_box.text();
        feed_box(text_box, key, false);
        let changed = text_box.text() != before;
        self.write_box();
        if changed {
            self.update_filter(target, previous);
        }
        BoxKey::Handled(Ok(()))
    }

    /// What the filter's line paints for a modal screen's box: its Visual selection and
    /// whether the cursor is a block (Normal and Visual modes).
    pub(super) fn filter_box_marks(&self) -> (Option<std::ops::Range<usize>>, bool) {
        match self.text_box.as_ref() {
            Some(text_box) if self.form.is_none() && text_box.owner.0 == FILTER => (
                text_box.selection(),
                !matches!(text_box.mode(), EditorMode::Insert | EditorMode::Replace),
            ),
            _ => (None, false),
        }
    }

    /// Where the box's text lives now: the focused free-text field of the form, or the
    /// query of the list filter being typed. Its owner, text and cursor.
    fn box_source(&self) -> Option<((String, usize, usize), String, usize)> {
        if let Some(form) = &self.form {
            let field = box_field(form)?;
            return Some((owner(form), field.input.input.clone(), field.input.cursor));
        }
        let target = self.filter_target()?;
        let filter = self.list_filter(target);
        filter.editing.then(|| {
            (
                (FILTER.to_owned(), target, 0),
                filter.input.input.clone(),
                filter.input.cursor,
            )
        })
    }

    /// The box's text, cursor, selection and cursor shape into its field, or into the
    /// filter's query (whose line asks [`Self::filter_box_marks`] for the rest).
    fn write_box(&mut self) {
        let Some(text_box) = self.text_box.as_mut() else {
            return;
        };
        let text = text_box.text();
        let cursor = text_box.cursor().min(text.len());
        let highlight = text_box.selection();
        let block_cursor = !matches!(text_box.mode(), EditorMode::Insert | EditorMode::Replace);
        text_box.synced.clone_from(&text);
        if let Some(form) = self.form.as_mut() {
            let Some(field) = form.fields.get_mut(form.selected) else {
                return;
            };
            field.input.cursor = cursor;
            field.input.input = text;
            field.highlight = highlight;
            field.block_cursor = block_cursor;
        } else if let Some(target) = self.filter_target()
            && self.list_filter(target).editing
        {
            let input = &mut self.list_filter_mut(target).input;
            input.input = text;
            input.cursor = cursor;
        }
    }

    /// After every event: the text box follows the form's focused field (a new form opens
    /// it in Insert mode, another field of the same form keeps the mode, a closed form drops
    /// it and keeps its registers for the next), and a list taking focus shows the rail again.
    pub(super) fn sync_text_box(&mut self) {
        if self.rail_hidden && self.focus < 4 {
            self.rail_hidden = false;
        }
        if !self.modal {
            return;
        }
        let Some((wanted, text, cursor)) = self.box_source() else {
            if let Some(form) = self.form.as_mut() {
                // A button, checkbox or choice: the box waits for a text field to come back.
                if let Some(field) = form.fields.get_mut(form.selected) {
                    field.highlight = None;
                    field.block_cursor = false;
                }
            } else {
                self.close_text_box();
            }
            return;
        };
        match self.text_box.as_mut() {
            Some(text_box) if text_box.owner == wanted && text_box.synced == text => {
                if text_box.cursor() != cursor && mode_of(text_box.mode()) == InputMode::Insert {
                    text_box.set_cursor(cursor);
                }
                return;
            }
            _ => {}
        }
        // Same form (title and fields) keeps the mode; a new one starts typing.
        let insert = self.text_box.as_ref().is_none_or(|text_box| {
            text_box.owner.0 != wanted.0
                || text_box.owner.1 != wanted.1
                || mode_of(text_box.mode()) == InputMode::Insert
        });
        let registers = match self.text_box.take() {
            Some(text_box) => text_box.registers().clone(),
            None => self.box_registers.clone(),
        };
        // Another field of this form: the old one loses its box's marks.
        if let Some(form) = self.form.as_mut() {
            for field in &mut form.fields {
                field.highlight = None;
                field.block_cursor = false;
            }
        }
        self.text_box = Some(TextBox::open(&text, cursor, insert, registers, wanted));
        self.write_box();
    }

    /// The Git tab's chords: the leader, what finishes it, and the `lazygit` preset's Space.
    /// `None` when the chord took the key.
    pub(super) fn git_chord(&mut self, key: KeyEvent) -> Option<KeyEvent> {
        if self.page != 0
            || self.form.is_some()
            || self
                .filter_target()
                .is_some_and(|target| self.list_filter(target).editing)
        {
            return Some(key);
        }
        match self.github.chord_key(&key) {
            Step::Pass => match Key::of(&key) {
                // A key the preset leaves alone goes on as it came, modifiers and all.
                Some(plain) if self.github.key_table().preset == Preset::Lazygit => {
                    match self.lazygit_key(plain) {
                        Some(event) if Key::of(&event) == Some(plain) => Some(key),
                        other => other,
                    }
                }
                _ => Some(key),
            },
            Step::Wait | Step::Cancel => None,
            Step::Run(Named::ToggleSidebar) => {
                self.rail_hidden = !self.rail_hidden;
                if self.rail_hidden {
                    self.focus = 4;
                } else if self.focus == 4 {
                    self.focus = self.last_list.min(3);
                }
                self.notice.clear();
                None
            }
            // The lazygit preset's Space: whatever the panel's table gives it.
            Step::Run(Named::Toggle) => self.lazygit_key(Key::Char(' ')),
            #[allow(unreachable_patterns)]
            Step::Run(_) => None,
        }
    }

    /// Which of the Git tab's lists (or the reader) the keys act on.
    pub(super) fn key_panel(&self) -> Panel {
        if self.commit_open || (self.focus == 4 && self.last_list == 2) {
            return Panel::CommitFiles;
        }
        match (self.focus, self.files_view, self.subview, self.history_view) {
            (4, ..) if self.last_list == 0 && self.files_view == 0 => Panel::Staging,
            (4, ..) => Panel::CommitFiles,
            (0, 1, ..) => Panel::Worktrees,
            (0, 2, ..) => Panel::Submodules,
            (0, ..) => Panel::Files,
            (1, _, 2, _) => Panel::Remotes,
            (1, _, 3, _) => Panel::Tags,
            (1, ..) => Panel::Branches,
            (2, _, _, 1) => Panel::Reflog,
            (2, ..) => Panel::Commits,
            _ => Panel::Stash,
        }
    }

    /// A plain key through the lazygit preset (`github/keymap.rs`): the Dusty Git key for
    /// the same action, or `None` when the key ran an action here.
    fn lazygit_key(&mut self, key: Key) -> Option<KeyEvent> {
        // A conflict's merge-view keys and the operation menu (`conflicts.rs`) come first.
        if let Some(event) = self.conflict_lazygit(key) {
            return Some(event);
        }
        let panel = self.key_panel();
        match self.github.key_table().translate(panel, key) {
            None => Some(key.event()),
            // Enter on a folder (or the folded Changes header) still folds, as in lazygit.
            Some(To::Key(Key::Char('v')))
                if panel == Panel::Files
                    && key == Key::Enter
                    && (self.changes_collapsed
                        || self.selected_row().is_some_and(|row| row.directory)) =>
            {
                Some(key.event())
            }
            Some(To::Key(to)) => Some(to.event()),
            Some(To::Pull) => {
                let result = self.pull_form();
                self.report(result);
                None
            }
            Some(To::Stash) => {
                self.form(
                    "Save stash (includes untracked files)",
                    FormAction::Stash,
                    vec![("Message", String::new())],
                );
                None
            }
        }
    }

    /// The chip's text, styles and edge glyph for the mode in force, or `None` when the
    /// screen is not modal.
    fn mode_chip(&self) -> Option<(String, Style, Style, Option<&'static str>)> {
        let (label, style, edge_style) = match self.input_mode()? {
            InputMode::Normal => (
                &self.mode_labels[0],
                Style::StatusAccent,
                Style::StatusEdgeNormal,
            ),
            InputMode::Insert => (
                &self.mode_labels[1],
                Style::StatusInsert,
                Style::StatusEdgeInsert,
            ),
            InputMode::Visual => (
                &self.mode_labels[2],
                Style::StatusVisual,
                Style::StatusEdgeVisual,
            ),
        };
        let edge =
            (self.icon_mode == crate::config::ExplorerIconMode::NerdFont).then_some("\u{e0b0}");
        Some((format!(" {label} "), style, edge_style, edge))
    }

    /// The columns the chip takes at the left of the footer, gap included: every footer
    /// lays its hints out after them (`chrome::footer_rect`), so hints drop by priority
    /// instead of running under the chip.
    pub(super) fn mode_chip_width(&self, width: u16) -> u16 {
        if width < 40 {
            return 0;
        }
        self.mode_chip().map_or(0, |(chip, _, _, edge)| {
            super::shell::chrome::width(&chip) + u16::from(edge.is_some()) + 1
        })
    }

    /// The footer's mode chip, at the far left, styled as the editor's statusline chip, in
    /// the columns the footers left for it.
    pub(super) fn paint_mode_chip(&self, frame: &mut Frame) {
        let grid = &mut frame.grid;
        let (width, height) = (grid.width(), grid.height());
        if self.mode_chip_width(width) == 0 || height == 0 {
            return;
        }
        let Some((chip, style, edge_style, edge)) = self.mode_chip() else {
            return;
        };
        let y = height - 1;
        let x = grid.put_text_clipped(0, y, &chip, style, width);
        if let Some(edge) = edge {
            grid.set_cell(x, y, edge, edge_style);
        }
    }

    /// The editor's which-key box while the leader waits for the rest of a chord.
    pub(super) fn paint_leader_hints(&self, frame: &mut Frame) {
        if !self.github.leader_pending() || self.help {
            return;
        }
        let keys = self.github.key_table();
        let mut rows = keys
            .after_leader()
            .into_iter()
            .map(|(key, label)| crate::ui::KeyHintUiRow {
                next_key: key,
                label: chord_words(label),
                icon: None,
                group: false,
            })
            .collect::<Vec<_>>();
        rows.push(crate::ui::KeyHintUiRow {
            next_key: "Esc".into(),
            label: "cancel".into(),
            icon: None,
            group: false,
        });
        let hints = crate::ui::KeyHintUiState {
            prefix: keys.leader_name(),
            rows,
        };
        let bounds = crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: frame.grid.width(),
            height: frame.grid.height(),
        };
        crate::ui::render::terminal::render_key_hints(
            frame,
            &hints,
            bounds,
            crate::config::AmbiguousWidth::Narrow,
        );
    }
}
