//! `:s/old/new/g`. Patterns are the regex crate's syntax, the same as `/` search, plus Vim's
//! `\<` and `\>`; replacements take `&`, `\1` to `\9`, `\n` and `\t`.

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::EditorMode;

use crate::editor::EditorOperation;

use crate::editor::EditorTransaction;

use crate::app::App;

use crate::editor::Precondition;

use crate::ui::PromptKind;

use crate::editor::text::TextEdit;

use crate::editor::input::substitution_pattern;
use crate::editor::text::TextBuffer;
use crate::editor::text::TextEditSet;
use unicode_segmentation::UnicodeSegmentation;

/// A replacement in regex-crate template form.
fn replacement_template(replacement: &str) -> String {
    let mut template = String::new();
    let mut characters = replacement.chars();
    while let Some(character) = characters.next() {
        match character {
            '&' => template.push_str("${0}"),
            '$' => template.push_str("$$"),
            '\\' => match characters.next() {
                Some(digit) if digit.is_ascii_digit() => {
                    template.push_str(&format!("${{{digit}}}"));
                }
                Some('n') => template.push('\n'),
                Some('t') => template.push('\t'),
                Some('$') => template.push_str("$$"),
                Some(other) => template.push(other),
                None => template.push('\\'),
            },
            other => template.push(other),
        }
    }
    template
}

/// A bounded, single-line sample for a replacement question.
fn substitute_preview(text: &str) -> String {
    let mut characters = text.chars();
    let mut preview: String = characters
        .by_ref()
        .take(80)
        .map(|character| match character {
            '\n' => '⏎',
            '\t' => '⇥',
            character if character.is_control() => '�',
            character => character,
        })
        .collect();
    if characters.next().is_some() {
        preview.push('…');
    }
    preview
}

/// The zero-based lines a range covers, in order. `current` is the cursor's line, `selection` the
/// Visual lines and `last` the buffer's final line.
fn range_lines(
    range: &str,
    current: usize,
    selection: Option<(usize, usize)>,
    last: usize,
) -> Result<(usize, usize), String> {
    let address = |text: &str| match text {
        "." => Ok(current),
        "$" => Ok(last),
        number => number
            .parse::<usize>()
            .ok()
            .and_then(|line| line.checked_sub(1))
            .filter(|line| *line <= last)
            .ok_or_else(|| format!("invalid range `{range}`")),
    };
    let (start, end) = match range {
        "" => (current, current),
        "%" => (0, last),
        "'<,'>" => selection.ok_or("no Visual selection")?,
        range => match range.split_once(',') {
            Some((start, end)) => (address(start)?, address(end)?),
            None => {
                let line = address(range)?;
                (line, line)
            }
        },
    };
    Ok((start.min(end), start.max(end)))
}

/// The edits for lines `start..=end`, and how many lines changed.
fn substitution_edits(
    text: &TextBuffer,
    (start, end): (usize, usize),
    pattern: &str,
    replacement: &str,
    flags: &str,
) -> Result<(Vec<TextEdit>, usize), String> {
    if pattern.is_empty() {
        return Err("no previous search pattern".into());
    }
    let regex = crate::editor::input::substitution_regex(pattern, flags)?;
    let template = replacement_template(replacement);
    let every = flags.contains('g');
    let mut edits = Vec::new();
    let mut changed = 0;
    for number in start..=end {
        let line = text.line(number).map_err(|error| error.to_string())?;
        let line_start = text
            .line_to_byte(number)
            .map_err(|error| error.to_string())?
            .get();
        let body = line.trim_end_matches(['\n', '\r']);
        let mut touched = false;
        for captures in regex.captures_iter(body) {
            let found = captures.get(0).expect("a match has its whole text");
            let mut replaced = String::new();
            captures.expand(&template, &mut replaced);
            let range = ByteRange::new(
                ByteOffset(line_start + found.start()),
                ByteOffset(line_start + found.end()),
            )
            .map_err(|error| error.to_string())?;
            edits.push(TextEdit::new(range, replaced));
            touched = true;
            if !every {
                break;
            }
        }
        changed += usize::from(touched);
    }
    Ok((edits, changed))
}

/// The edits a `:s` makes in `view`, and how many lines they change.
fn view_substitution(
    view: &crate::editor::View,
    text: &TextBuffer,
    (range, pattern, replacement, flags): (&str, &str, &str, &str),
) -> Result<(Vec<TextEdit>, usize), String> {
    let line_of = |offset| text.byte_to_line(offset).unwrap_or(0);
    let selection = view
        .selection
        .map(|selection| (line_of(selection.anchor), line_of(selection.head)));
    let last = text.visible_lines().saturating_sub(1);
    let lines = range_lines(range, line_of(view.cursor), selection, last)?;
    substitution_edits(text, lines, pattern, replacement, flags)
}

/// The zero-based lines of `range` that match `pattern`, or with `invert` that don't.
fn global_lines(
    view: &crate::editor::View,
    text: &TextBuffer,
    range: &str,
    pattern: &str,
    invert: bool,
) -> Result<Vec<usize>, String> {
    let line_of = |offset| text.byte_to_line(offset).unwrap_or(0);
    let selection = view
        .selection
        .map(|selection| (line_of(selection.anchor), line_of(selection.head)));
    let last = text.visible_lines().saturating_sub(1);
    let (start, end) = range_lines(range, line_of(view.cursor), selection, last)?;
    let pattern = substitution_pattern(pattern);
    let regex = regex::Regex::new(&pattern).map_err(|error| error.to_string())?;
    let mut lines = Vec::new();
    for number in start..=end {
        let line = text.line(number).map_err(|error| error.to_string())?;
        if regex.is_match(line.trim_end_matches(['\n', '\r'])) != invert {
            lines.push(number);
        }
    }
    Ok(lines)
}

impl App {
    /// Shows what a `:s` being typed puts in place of each match, as with Vim's inccommand.
    pub(in crate::app) fn preview_substitute(&mut self) {
        let preview = self.typed_substitute_hints().unwrap_or_default();
        self.ui.set(preview, |ui| &mut ui.substitute_preview);
    }

    fn typed_substitute_hints(&self) -> Option<Vec<crate::ui::UiInlayHint>> {
        let prompt = self
            .input
            .prompt()
            .filter(|prompt| prompt.kind == PromptKind::Command)?;
        let (range, pattern, replacement, flags) =
            crate::editor::input::typed_substitution(&prompt.input)?;
        let view = self.state.views.get(self.state.active_view()?)?;
        if self.ui.large_file_buffers.contains(&view.buffer) {
            return None;
        }
        let text = &self.state.buffers[view.buffer].text;
        let pattern = if pattern.is_empty() {
            self.ui.search_query.as_deref()?
        } else {
            &pattern
        };
        if flags.contains('n') {
            return Some(Vec::new());
        }
        let parts = (
            range.as_str(),
            pattern,
            replacement.as_str(),
            flags.as_str(),
        );
        let (edits, _) = view_substitution(view, text, parts).ok()?;
        Some(
            edits
                .iter()
                .map(|edit| crate::ui::UiInlayHint {
                    buffer: view.buffer,
                    revision: text.revision(),
                    position: edit.range().end,
                    label: format!("→{}", edit.replacement().replace('\n', "⏎")),
                })
                .collect(),
        )
    }

    pub(in crate::app) fn run_substitute(
        &mut self,
        range: &str,
        pattern: &str,
        replacement: &str,
        flags: &str,
    ) {
        let pattern = if pattern.is_empty() {
            match self
                .ui
                .search_query
                .as_deref()
                .filter(|pattern| !pattern.is_empty())
            {
                Some(pattern) => pattern.to_owned(),
                None => {
                    self.show_error("no previous search pattern");
                    return;
                }
            }
        } else {
            pattern.to_owned()
        };
        let pattern = pattern.as_str();
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let text = &self.state.buffers[buffer_id].text;
        let revision = text.revision();
        let selection = view.selection.is_some();
        let result = view_substitution(view, text, (range, pattern, replacement, flags));
        let (edits, lines) = match result {
            Ok(found) => found,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if edits.is_empty() {
            self.show_error(format!("pattern not found: {pattern}"));
            return;
        }
        if flags.contains('n') {
            self.notify_message(format!("{} matches on {lines} lines", edits.len()), false);
            return;
        }
        if flags.contains('c') {
            self.substitute_confirm = Some(SubstituteConfirm {
                view: view_id,
                buffer: buffer_id,
                revision,
                source: text.source_version(),
                edits,
                next: 0,
                accepted: Vec::new(),
            });
            self.ask_next_substitution();
            return;
        }
        let count = edits.len();
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(revision, edits),
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return;
        }
        if selection {
            self.set_mode(EditorMode::Normal);
        }
        self.normalize_substitute_cursor(view_id);
        self.ensure_cursor_visible();
        self.push_change_location();
        self.notify_message(format!("{count} substitutions on {lines} lines"), false);
    }

    /// `:g/pattern/d`, or with `invert` `:v/pattern/d`: deletes the lines of `range` (the whole
    /// file without one) that match, or that don't.
    pub(in crate::app) fn run_global_delete(&mut self, range: &str, pattern: &str, invert: bool) {
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let text = &self.state.buffers[buffer_id].text;
        let revision = text.revision();
        let result = global_lines(
            view,
            text,
            if range.is_empty() { "%" } else { range },
            pattern,
            invert,
        );
        let lines = match result {
            Ok(lines) => lines,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if lines.is_empty() {
            self.show_error(format!("pattern not found: {pattern}"));
            return;
        }
        let mut edits: Vec<TextEdit> = Vec::new();
        let mut deleted = String::new();
        for line in &lines {
            deleted.push_str(&text.line(*line).expect("a matched line exists"));
            let start = text.line_to_byte(*line).map_or(0, ByteOffset::get);
            let end = text
                .line_to_byte(line + 1)
                .map_or(text.len_bytes(), ByteOffset::get);
            match edits.last_mut() {
                Some(last) if last.range().end.get() == start => {
                    let merged_start = last.range().start;
                    *last = TextEdit::new(
                        ByteRange::new(merged_start, ByteOffset(end)).expect("lines are in order"),
                        "",
                    );
                }
                _ => edits.push(TextEdit::new(
                    ByteRange::new(ByteOffset(start), ByteOffset(end)).expect("a line"),
                    "",
                )),
            }
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(revision, edits),
            })
            .with_operation(EditorOperation::SetRegister {
                name: '"',
                text: deleted,
                shape: crate::editor::RegisterShape::Line,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return;
        }
        if self.state.views[view_id].mode != EditorMode::Normal {
            self.set_mode(EditorMode::Normal);
        }
        self.keep_cursor_off_final_newline();
        self.ensure_cursor_visible();
        self.push_change_location();
        self.notify_message(format!("{} lines deleted", lines.len()), false);
    }

    /// Selects the next match of a `:s///c` and asks whether to replace it.
    fn ask_next_substitution(&mut self) {
        let Some(confirm) = &self.substitute_confirm else {
            return;
        };
        let Some(edit) = confirm.edits.get(confirm.next) else {
            self.finish_substitution();
            return;
        };
        let text = self
            .state
            .buffers
            .get(confirm.buffer)
            .map(|buffer| &buffer.text);
        if !text.is_some_and(|text| text.source_version() == confirm.source) {
            self.substitute_confirm = None;
            self.show_error("the file changed, so replacing stopped");
            return;
        }
        let found = text
            .and_then(|text| text.slice(edit.range()).ok())
            .unwrap_or_default();
        let message = format!(
            "Replace `{}` with `{}`? Match {} of {}; a replaces the rest, esc stops.",
            substitute_preview(&found),
            substitute_preview(edit.replacement()),
            confirm.next + 1,
            confirm.edits.len()
        );
        let (view, range) = (confirm.view, edit.range());
        if range.is_empty() {
            self.set_specific_mode(view, EditorMode::Normal);
            let Some(target) = self.state.views.get_mut(view) else {
                self.substitute_confirm = None;
                return;
            };
            target.cursor = range.start;
            self.ensure_cursor_visible();
        } else if !self.select_text(view, range.start.get(), range.end.get()) {
            self.substitute_confirm = None;
            return;
        }
        self.open_confirmation_actions(
            PromptKind::SubstituteConfirm,
            "Replace".into(),
            message,
            crate::ui::ConfirmationChoice::Yes,
            "Replace",
            "Skip",
        );
        self.scroll_above_substitute_question(view);
    }

    /// Keep the match in the visible rows above the actual question.
    fn scroll_above_substitute_question(&mut self, view: crate::editor::ViewId) {
        let Some(viewport) = crate::ui::view_pane_viewport(&self.state, &self.ui, view) else {
            return;
        };
        let Some(prompt) = self.input.prompt() else {
            return;
        };
        let (width, height) = crate::ui::prompt_surface_dimensions(
            &self.state,
            &self.ui,
            self.window_session.width(),
            self.window_session.height(),
        );
        let question = crate::ui::prompt_geometry_for_state(
            width,
            height,
            self.configuration.settings(),
            prompt,
        );
        let height =
            usize::from(viewport.height.min(question.top.saturating_sub(viewport.y))).max(1);
        let Some(target) = self.state.views.get(view) else {
            return;
        };
        let text = &self.state.buffers[target.buffer].text;
        let settings = self.configuration.settings();
        let wrap = settings.editor.wrap && !self.ui.large_file_buffers.contains(&target.buffer);
        let rows = crate::ui::VisualRowModel::new_for_state(
            &self.state,
            text,
            view,
            &self.ui,
            settings,
            viewport.width,
            wrap,
        );
        let start = rows.normalize(crate::ui::VisualRowAnchor {
            line: target.scroll_line,
            wrap_row: target.scroll_wrap_row,
        });
        let cursor = rows.cursor_anchor(target.cursor);
        if rows.screen_row(start, cursor, height).is_some() {
            return;
        }
        let first = rows.backward(cursor, height / 3);
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view,
                first_line: first.line,
            })
            .with_operation(EditorOperation::SetScrollWrapRow {
                view,
                first_wrap_row: first.wrap_row,
            })
            .with_undo_policy(crate::editor::UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    /// Replace (or skip) the match being asked about, or with `all` every one left, then ask
    /// about the next.
    pub(in crate::app) fn answer_substitution(&mut self, replace: bool, all: bool) {
        let Some(mut confirm) = self.substitute_confirm.take() else {
            return;
        };
        let end = if all {
            confirm.edits.len()
        } else {
            confirm.next + 1
        };
        if replace {
            let next = confirm.next;
            confirm
                .accepted
                .extend(confirm.edits[next..end].iter().cloned());
        }
        confirm.next = end;
        // Taken out, so closing this question doesn't count as stopping.
        self.close_prompt();
        self.substitute_confirm = Some(confirm);
        self.ask_next_substitution();
    }

    /// Makes the replacements said yes to, as one change to undo.
    pub(in crate::app) fn finish_substitution(&mut self) {
        let Some(confirm) = self.substitute_confirm.take() else {
            return;
        };
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::SubstituteConfirm)
        {
            self.close_prompt();
        }
        if self.state.views.contains_key(confirm.view) {
            self.set_specific_mode(confirm.view, EditorMode::Normal);
        }
        if !self
            .state
            .buffers
            .get(confirm.buffer)
            .is_some_and(|buffer| buffer.text.source_version() == confirm.source)
        {
            self.show_error("the file changed, so replacing stopped");
            return;
        }
        let count = confirm.accepted.len();
        if count == 0 {
            self.notify_message("nothing was replaced".into(), false);
            return;
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: confirm.buffer,
                revision: confirm.revision,
            })
            .with_precondition(Precondition::BufferWritable(confirm.buffer))
            .with_operation(EditorOperation::ApplyEdits {
                buffer: confirm.buffer,
                edits: TextEditSet::new(confirm.revision, confirm.accepted),
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return;
        }
        self.normalize_substitute_cursor(confirm.view);
        self.ensure_cursor_visible();
        self.push_change_location();
        self.notify_message(format!("{count} substitutions"), false);
    }

    fn normalize_substitute_cursor(&mut self, view: crate::editor::ViewId) {
        let Some(target) = self.state.views.get(view) else {
            return;
        };
        if target.mode != EditorMode::Normal {
            return;
        }
        let text = &self.state.buffers[target.buffer].text;
        let line = text
            .byte_to_line(target.cursor)
            .unwrap_or(0)
            .min(text.visible_lines().saturating_sub(1));
        let start = text.line_to_byte(line).unwrap_or(ByteOffset(0));
        let content = text.line(line).unwrap_or_default();
        let content = content.trim_end_matches('\n');
        let relative = target.cursor.get().saturating_sub(start.get());
        let cursor = ByteOffset(
            start.get()
                + content
                    .grapheme_indices(true)
                    .map(|(offset, _)| offset)
                    .take_while(|offset| *offset <= relative)
                    .last()
                    .unwrap_or(0),
        );
        self.state.views[view].cursor = cursor;
    }
}

/// A `:s///c` in progress: its matches, found before anything changed, and which to replace.
pub(in crate::app) struct SubstituteConfirm {
    view: crate::editor::ViewId,
    buffer: crate::editor::BufferId,
    revision: crate::editor::text::BufferRevision,
    source: crate::editor::text::TextVersion,
    edits: Vec<TextEdit>,
    next: usize,
    accepted: Vec<TextEdit>,
}

#[cfg(test)]
mod tests {
    use crate::app::editing::substitute::*;

    fn applied(
        text: &str,
        lines: (usize, usize),
        pattern: &str,
        with: &str,
        flags: &str,
    ) -> String {
        let buffer = TextBuffer::from_text(text);
        let (edits, _) = substitution_edits(&buffer, lines, pattern, with, flags).unwrap();
        let mut out = text.to_owned();
        for edit in edits.into_iter().rev() {
            let range = edit.range();
            out.replace_range(range.start.get()..range.end.get(), edit.replacement());
        }
        out
    }

    #[test]
    fn escaped_word_boundaries_match_literal_backslashes_in_preview_and_execution() {
        use crate::editor::input::parse_ex;
        use crate::editor::input::search_matches;
        use crate::editor::input::substitute_pattern;
        for (command, source, expected) in [
            (r"s/\\</x/g", r"\< \b", r"x \b"),
            (r"s/\\>/x/g", r"\> \b", r"x \b"),
            (r"s/\\\<cat/x/g", r"\cat \scatter", r"x \scatter"),
            ("s/a/b ", "a", "b "),
        ] {
            let invocation = parse_ex(command, &Default::default()).unwrap();
            let string = |name| match &invocation.arguments[name] {
                crate::editor::CommandValue::String(value) => value.as_str(),
                _ => panic!("expected string"),
            };
            assert_eq!(
                applied(
                    source,
                    (0, 0),
                    string("pattern"),
                    string("replacement"),
                    string("flags")
                ),
                expected
            );
            let preview = substitute_pattern(command).unwrap();
            let buffer = TextBuffer::from_text(source);
            let (edits, _) = substitution_edits(
                &buffer,
                (0, 0),
                string("pattern"),
                string("replacement"),
                string("flags"),
            )
            .unwrap();
            let ranges: Vec<_> = edits
                .iter()
                .map(|edit| edit.range().start.get()..edit.range().end.get())
                .collect();
            assert_eq!(
                search_matches(source, &preview, false, true).unwrap(),
                ranges
            );
        }
    }

    #[test]
    fn substitutes_first_per_line_or_every_match_with_captures() {
        assert_eq!(applied("a a\na a\n", (0, 1), "a", "b", ""), "b a\nb a\n");
        assert_eq!(applied("a a\na a\n", (0, 0), "a", "b", "g"), "b b\na a\n");
        assert_eq!(
            applied("foo bar\n", (0, 0), r"(\w+) (\w+)", r"\2 \1 &", ""),
            "bar foo foo bar\n"
        );
        assert_eq!(
            applied("cat scatter\n", (0, 0), r"\<cat\>", "dog", "g"),
            "dog scatter\n"
        );
        assert_eq!(applied("A\n", (0, 0), "a", "$1", "i"), "$1\n");
    }

    #[test]
    fn escaped_dollars_stay_literal_in_replacements() {
        assert_eq!(applied("foo", (0, 0), "(foo)", r"\$1", ""), "$1");
        assert_eq!(applied("foo", (0, 0), "(foo)", r"\$\1", ""), "$foo");
    }

    #[test]
    fn word_boundaries_are_directional_and_preserve_escaped_backslashes() {
        let text = TextBuffer::from_text("foo\\< foo\\>\nfoo\n");
        let view = crate::editor::View::new(crate::editor::BufferId::default());
        for (pattern, lines) in [
            (r"foo\<", vec![]),
            (r"\>foo", vec![]),
            (r"\<foo\>", vec![0, 1]),
            (r"\\<", vec![0]),
            (r"\\>", vec![0]),
        ] {
            assert_eq!(global_lines(&view, &text, "%", pattern, false), Ok(lines));
        }
        assert_eq!(applied("foo", (0, 0), r"foo\<", "X", ""), "foo");
        assert_eq!(applied("foo", (0, 0), r"\>foo", "X", ""), "foo");
        assert_eq!(applied(r"foo\<", (0, 0), r"\\<", "X", ""), "fooX");
        assert_eq!(applied(r"foo\>", (0, 0), r"\\>", "X", ""), "fooX");
    }

    #[test]
    fn a_substitution_being_typed_has_parts_once_its_replacement_begins() {
        use crate::editor::input::typed_substitution as typed;
        assert_eq!(typed("%s/foo"), None);
        assert_eq!(
            typed("s//x"),
            Some((String::new(), String::new(), "x".into(), String::new()))
        );
        assert_eq!(
            typed("%s/foo/b"),
            Some(("%".into(), "foo".into(), "b".into(), String::new()))
        );
        assert_eq!(
            typed("2,3s/a/b/g"),
            Some(("2,3".into(), "a".into(), "b".into(), "g".into()))
        );
        assert_eq!(typed("set wrap"), None);
    }

    #[test]
    fn global_takes_the_lines_that_match_or_with_v_the_others() {
        let text = TextBuffer::from_text("keep\n// note\nkeep too\n// more\n");
        let view = crate::editor::View::new(crate::editor::BufferId::default());
        assert_eq!(
            global_lines(&view, &text, "%", "^//", false),
            Ok(vec![1, 3])
        );
        assert_eq!(global_lines(&view, &text, "%", "^//", true), Ok(vec![0, 2]));
        assert_eq!(
            global_lines(&view, &text, "1,2", r"\<keep\>", false),
            Ok(vec![0])
        );
    }

    #[test]
    fn ranges_name_lines_from_one_and_the_cursor_and_the_end() {
        assert_eq!(range_lines("", 4, None, 9), Ok((4, 4)));
        assert_eq!(range_lines("%", 4, None, 9), Ok((0, 9)));
        assert_eq!(range_lines("2,3", 4, None, 9), Ok((1, 2)));
        assert_eq!(range_lines(".,$", 4, None, 9), Ok((4, 9)));
        assert_eq!(range_lines("'<,'>", 4, Some((6, 2)), 9), Ok((2, 6)));
        assert!(range_lines("'<,'>", 4, None, 9).is_err());
        for range in ["0", "11", "999", "1,999", "999,1", "18446744073709551616"] {
            assert!(range_lines(range, 4, None, 9).is_err(), "{range}");
        }
        assert_eq!(range_lines("10", 4, None, 9), Ok((9, 9)));
    }
}
