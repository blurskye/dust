//! Anchored positions for the language server's inlay text and color previews.

use crate::app::App;
use crate::editor::BufferId;
use crate::editor::decorations::{Gravity, MarkId};
use crate::editor::text::{ByteOffset, ByteRange};
use std::collections::BTreeMap;

type InlayKey = (BufferId, u64, ByteOffset, String);
type ColorKey = (BufferId, String, u64, ByteOffset, ByteOffset, (u8, u8, u8));

#[derive(Default)]
pub(super) struct LanguageMarks {
    inlays: BTreeMap<InlayKey, MarkId>,
    colors: BTreeMap<ColorKey, MarkId>,
}

impl App {
    pub(in crate::app) fn project_language_marks(&mut self) {
        let mut previous = std::mem::take(&mut self.language.marks.inlays);
        let mut hints = self.ui.lsp_inlay_hints.clone();
        hints.retain_mut(|hint| {
            let Some(buffer) = self.state.buffers.get(hint.buffer) else {
                return false;
            };
            let key = (
                hint.buffer,
                hint.revision.0,
                hint.position,
                hint.label.clone(),
            );
            let mark = if let Some(mark) = previous.remove(&key) {
                mark
            } else {
                let Some(position) = buffer.text.map_offset_since(hint.revision, hint.position)
                else {
                    return false;
                };
                let buffer = &mut self.state.buffers[hint.buffer];
                let Some(mark) = buffer.decorations.insert(
                    &buffer.text,
                    "inlay",
                    ByteRange::empty(position),
                    Gravity::Right,
                    Gravity::Right,
                ) else {
                    return false;
                };
                mark
            };
            let buffer = &self.state.buffers[hint.buffer];
            let Some(range) = buffer.decorations.range(mark) else {
                return false;
            };
            hint.position = range.start;
            hint.revision = buffer.text.revision();
            if let Some(replaced) = self.language.marks.inlays.insert(
                (
                    hint.buffer,
                    hint.revision.0,
                    hint.position,
                    hint.label.clone(),
                ),
                mark,
            ) {
                self.state.buffers[hint.buffer].decorations.remove(replaced);
            }
            true
        });
        self.ui.set(hints, |ui| &mut ui.lsp_inlay_hints);
        for ((id, ..), mark) in previous {
            if let Some(buffer) = self.state.buffers.get_mut(id) {
                buffer.decorations.remove(mark);
            }
        }

        let mut previous = std::mem::take(&mut self.language.marks.colors);
        let mut colors = self.ui.lsp_colors.clone();
        colors.retain_mut(|span| {
            let Some(buffer) = self.state.buffers.get(span.buffer) else {
                return false;
            };
            let key = (
                span.buffer,
                span.server.clone(),
                span.revision.0,
                span.range.start,
                span.range.end,
                span.rgb,
            );
            let mark = if let Some(mark) = previous.remove(&key) {
                mark
            } else {
                let Some(range) = buffer.text.map_range_since(span.revision, span.range) else {
                    return false;
                };
                let buffer = &mut self.state.buffers[span.buffer];
                let Some(mark) = buffer.decorations.insert(
                    &buffer.text,
                    "color",
                    range,
                    Gravity::Right,
                    Gravity::Left,
                ) else {
                    return false;
                };
                mark
            };
            let buffer = &self.state.buffers[span.buffer];
            let Some(range) = buffer.decorations.range(mark) else {
                return false;
            };
            span.range = range;
            span.revision = buffer.text.revision();
            if let Some(replaced) = self.language.marks.colors.insert(
                (
                    span.buffer,
                    span.server.clone(),
                    span.revision.0,
                    span.range.start,
                    span.range.end,
                    span.rgb,
                ),
                mark,
            ) {
                self.state.buffers[span.buffer].decorations.remove(replaced);
            }
            true
        });
        self.ui.set(colors, |ui| &mut ui.lsp_colors);
        for ((id, ..), mark) in previous {
            if let Some(buffer) = self.state.buffers.get_mut(id) {
                buffer.decorations.remove(mark);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::text::{ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet};
    use crate::editor::{EditorOperation, EditorTransaction, TransactionId};

    #[test]
    fn inlays_and_colors_survive_more_than_the_text_mapping_history() {
        let (_dirs, mut app) = crate::app::tests::app();
        let (id, _) = app
            .state
            .open_text(TextBuffer::from_text("#fff"), None)
            .unwrap();
        let revision = app.state.buffers[id].text.revision();
        app.ui.lsp_inlay_hints.push(crate::ui::UiInlayHint {
            buffer: id,
            revision,
            position: ByteOffset(4),
            label: "color".into(),
        });
        app.ui.lsp_colors.push(crate::ui::UiColorSpan {
            server: "test".into(),
            buffer: id,
            revision,
            range: ByteRange::new(ByteOffset(0), ByteOffset(4)).unwrap(),
            rgb: (255, 255, 255),
        });
        app.project_language_marks();
        let idle = (app.state.revision, app.ui.revision);
        app.project_language_marks();
        assert_eq!((app.state.revision, app.ui.revision), idle);
        for _ in 0..70 {
            let revision = app.state.buffers[id].text.revision();
            app.state
                .apply_transaction(&EditorTransaction::new(TransactionId(1)).with_operation(
                    EditorOperation::ApplyEdits {
                        buffer: id,
                        edits: TextEditSet::new(
                            revision,
                            vec![TextEdit::new(ByteRange::empty(ByteOffset(0)), "x")],
                        ),
                    },
                ))
                .unwrap();
        }
        app.project_language_marks();
        assert_eq!(app.ui.lsp_inlay_hints[0].position, ByteOffset(74));
        assert_eq!(
            app.ui.lsp_colors[0].range,
            ByteRange::new(ByteOffset(70), ByteOffset(74)).unwrap()
        );
        app.state
            .apply_transaction(
                &EditorTransaction::new(TransactionId(1))
                    .with_operation(EditorOperation::Undo { buffer: id }),
            )
            .unwrap();
        app.project_language_marks();
        assert_eq!(app.ui.lsp_inlay_hints[0].position, ByteOffset(73));
        assert_eq!(app.ui.lsp_colors[0].range.start, ByteOffset(69));
        app.state
            .replace_buffer_from_disk(id, TextBuffer::from_text("new"), "/file".into());
        app.project_language_marks();
        assert!(app.ui.lsp_inlay_hints.is_empty());
        assert!(app.ui.lsp_colors.is_empty());
    }
}
