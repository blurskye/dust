//! Flash-style jump (`s`), as flash.nvim in LazyVim: type a few characters, every visible
//! match gets a one-key label, and typing that label jumps there.

use crate::editor::decorations::{Gravity, MarkId};
use unicode_segmentation::UnicodeSegmentation;

use crate::editor::ViewId;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::TextBuffer;

/// Home row first, like flash.nvim.
const LABELS: &str = "asdfghjklqwertyuiopzxcvbnm";

#[derive(Clone, Debug)]
pub(in crate::app) struct FlashState {
    pub(super) view: ViewId,
    pub(super) buffer: crate::editor::BufferId,
    pub(super) source: crate::editor::text::TextVersion,
    pub(super) query: String,
    pub(super) targets: Vec<(MarkId, Option<char>)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FlashTarget {
    pub(super) range: ByteRange,
    pub(super) label: Option<char>,
}

/// Every match of `query` in visible source ranges, labelled nearest to the cursor first.
///
/// Matching ignores case unless the query has a capital letter. A label is never a character
/// that continues some match, so typing more of the query is never read as a label.
pub(super) fn flash_targets(
    text: &TextBuffer,
    ranges: &[ByteRange],
    cursor: ByteOffset,
    query: &str,
) -> Vec<FlashTarget> {
    if query.is_empty() {
        return Vec::new();
    }
    let ignore_case = !query.chars().any(char::is_uppercase);
    let same = |a: char, b: char| {
        if ignore_case {
            a.to_lowercase().eq(b.to_lowercase())
        } else {
            a == b
        }
    };
    let mut targets = Vec::new();
    let mut continuations = Vec::new();
    for range in ranges {
        let Ok(content) = text.slice(*range) else {
            continue;
        };
        let start = range.start;
        for (index, _) in content.grapheme_indices(true) {
            let mut rest = content[index..].chars();
            if !query
                .chars()
                .all(|wanted| rest.next().is_some_and(|got| same(wanted, got)))
            {
                continue;
            }
            let begin = ByteOffset(start.get() + index);
            let end = ByteOffset(begin.get() + content[index..].len() - rest.as_str().len());
            if begin == cursor {
                continue;
            }
            if let Some(next) = rest.next() {
                continuations.extend(next.to_lowercase());
            }
            if let Ok(range) = ByteRange::new(begin, end) {
                targets.push(FlashTarget { range, label: None });
            }
        }
    }
    let mut order = (0..targets.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| targets[index].range.start.get().abs_diff(cursor.get()));
    let mut labels = LABELS
        .chars()
        .filter(|label| !continuations.contains(label));
    for index in order {
        let Some(label) = labels.next() else {
            break;
        };
        targets[index].label = Some(label);
    }
    targets
}

use crate::app::App;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::UndoPolicy;
use crate::editor::input::KeyEvent;

impl App {
    pub(in crate::app) fn begin_flash(&mut self) {
        self.reconcile_editor_windows();
        let Some(view) = self.state.active_view() else {
            return;
        };
        self.flash = Some(crate::app::editing::flash::FlashState {
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
}

impl App {
    /// While flash is active every key goes here: characters extend the query or pick a
    /// label, Backspace edits, and anything else cancels.
    pub(in crate::app) fn handle_flash_key(&mut self, key: &KeyEvent) {
        let Some(mut flash) = self.flash.take() else {
            return;
        };
        if self.state.active_view() != Some(flash.view)
            || !self
                .state
                .views
                .get(flash.view)
                .is_some_and(|view| view.buffer == flash.buffer)
            || !self
                .state
                .buffers
                .get(flash.buffer)
                .is_some_and(|buffer| buffer.text.source_version().same_instance(&flash.source))
        {
            self.sync_flash_ui();
            return;
        }
        match key.code {
            crate::editor::input::KeyCode::Char(character)
                if !key.modifiers.control && !key.modifiers.alt =>
            {
                if let Some((mark, _)) = flash
                    .targets
                    .iter()
                    .find(|(_, label)| *label == Some(character))
                    && let Some(range) = self.state.buffers[flash.buffer].decorations.range(*mark)
                {
                    self.flash_jump(flash.view, range.start);
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
        let Some(viewport) = crate::ui::active_editor_pane_viewport(&self.state, &self.ui)
            .filter(|viewport| viewport.view == flash.view)
        else {
            self.sync_flash_ui();
            return;
        };
        let text = &self.state.buffers[view.buffer].text;
        let width = crate::ui::editor_state_content_width(
            &self.state,
            text,
            &self.configuration.settings().editor,
            viewport.width,
        );
        let wrap = width > 0
            && self.configuration.settings().editor.wrap
            && !self.ui.large_file_buffers.contains(&view.buffer);
        let model = crate::ui::VisualRowModel::new_for_state(
            &self.state,
            text,
            flash.view,
            &self.ui,
            self.configuration.settings(),
            viewport.width,
            wrap,
        );
        let start = crate::ui::VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        };
        let horizontal = if wrap { 0 } else { view.scroll_column };
        let ranges = if width > 0 {
            model.visible_text_ranges(start, usize::from(viewport.height), horizontal)
        } else {
            Vec::new()
        };
        let targets = flash_targets(text, &ranges, view.cursor, &flash.query);
        match targets.as_slice() {
            [] if !flash.query.is_empty() => {
                self.sync_flash_ui();
                self.notify_message(format!("no match for {}", flash.query), false);
            }
            [only] => self.flash_jump(flash.view, only.range.start),
            _ => {
                let buffer = &mut self.state.buffers[flash.buffer];
                buffer.decorations.clear("flash");
                flash.targets = targets
                    .into_iter()
                    .filter_map(|target| {
                        let mark = buffer.decorations.insert(
                            &buffer.text,
                            "flash",
                            target.range,
                            Gravity::Right,
                            Gravity::Left,
                        )?;
                        Some((mark, target.label))
                    })
                    .collect();
                self.flash = Some(flash);
                self.sync_flash_ui();
            }
        }
    }
}

impl App {
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
}

impl App {
    pub(in crate::app) fn sync_flash_ui(&mut self) {
        if self.flash.as_ref().is_some_and(|flash| {
            self.state.active_view() != Some(flash.view)
                || self
                    .window_session
                    .graph()
                    .focused()
                    .and_then(|window| self.state.window_view(window))
                    .is_none_or(|view| view != flash.view)
        }) {
            self.flash = None;
        }
        if self.flash.is_none() {
            for buffer in self.state.buffers.values_mut() {
                buffer.decorations.clear("flash");
            }
        }
        self.ui.flash = self.flash.as_ref().and_then(|flash| {
            let buffer = self.state.buffers.get(flash.buffer)?;
            Some(crate::ui::FlashOverlay {
                view: flash.view,
                matches: flash
                    .targets
                    .iter()
                    .filter_map(|(mark, _)| buffer.decorations.range(*mark))
                    .collect(),
                labels: flash
                    .targets
                    .iter()
                    .filter_map(|(mark, label)| {
                        Some((buffer.decorations.range(*mark)?.start, (*label)?))
                    })
                    .collect(),
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::app::editing::flash::*;

    fn labels(text: &str, cursor: usize, query: &str) -> Vec<(usize, Option<char>)> {
        let text = TextBuffer::from_bytes(text.as_bytes()).unwrap();
        let visible = ByteRange::new(ByteOffset(0), ByteOffset(text.len_bytes())).unwrap();
        flash_targets(&text, &[visible], ByteOffset(cursor), query)
            .into_iter()
            .map(|target| (target.range.start.get(), target.label))
            .collect()
    }

    #[test]
    fn hidden_matches_do_not_consume_labels_or_exclude_continuations() {
        let text = TextBuffer::from_text("xa xs xa");
        let targets = flash_targets(
            &text,
            &[ByteRange::new(ByteOffset(3), ByteOffset(5)).unwrap()],
            ByteOffset(99),
            "x",
        );
        assert_eq!(
            targets,
            vec![FlashTarget {
                range: ByteRange::new(ByteOffset(3), ByteOffset(4)).unwrap(),
                label: Some('a'),
            }]
        );
    }

    #[test]
    fn matches_never_start_inside_joined_emoji() {
        assert!(labels("👩‍💻 👨‍💻", 0, "💻").is_empty());
        assert_eq!(labels("👩‍💻 x", 99, "👩‍💻")[0].0, 0);
    }

    #[test]
    fn matches_are_labelled_nearest_first_and_skip_continuations() {
        // "ab" is followed by 'c' and 'd', so neither can be a label.
        assert_eq!(
            labels("abc abd\nxab\n", 8, "ab"),
            vec![(0, Some('f')), (4, Some('s')), (9, Some('a'))]
        );
    }

    #[test]
    fn a_capital_makes_the_match_case_sensitive() {
        assert_eq!(labels("Foo foo", 3, "f").len(), 2);
        assert_eq!(labels("Foo foo", 3, "F"), vec![(0, Some('a'))]);
    }
}
