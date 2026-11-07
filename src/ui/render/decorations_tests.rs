//! Decorations share source positioning, wrapping and clipping with native inlays.

use crate::editor::decorations::{
    Decoration, Gravity, StyledText, VirtualText, VirtualTextPosition,
};
use crate::editor::text::{ByteOffset, ByteRange, TextBuffer};
use crate::editor::{EditorMode, EditorState};
use crate::ui::Style;
use crate::ui::render::composition_tests::{render, ui_with_editor_layout};
use crate::ui::render::{CompletionItem, LanguageCompletionState, UiInlayHint};

#[test]
fn inline_decorations_shift_source_and_clearing_restores_the_original_grid() {
    let mut state = EditorState::new("/workspace".into());
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("a界e\u{301}bc"), None)
        .unwrap();
    state.views[view].cursor = ByteOffset(4);
    let ui = ui_with_editor_layout(&state);
    let loaded = crate::config::load_bundled_config();
    let before = render(&state, &ui, &loaded.config, &loaded.dashboard, 50, 12);
    let buffer_state = &mut state.buffers[buffer];
    let mark = buffer_state
        .decorations
        .insert(
            &buffer_state.text,
            "test",
            ByteRange::new(ByteOffset(4), ByteOffset(7)).unwrap(),
            Gravity::Right,
            Gravity::Left,
        )
        .unwrap();
    buffer_state.decorations.decorate(
        mark,
        Decoration {
            highlight: Some("@keyword".into()),
            virtual_text: Some(VirtualText {
                text: StyledText {
                    text: "λ ".into(),
                    style: "Comment".into(),
                },
                position: VirtualTextPosition::Inline,
            }),
            sign: None,
        },
    );
    let after = render(&state, &ui, &loaded.config, &loaded.dashboard, 50, 12);
    let rows = (0..12)
        .map(|row| after.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rows.contains("a界λ e\u{301}bc"), "{rows}");
    assert_eq!(after.cursor.unwrap().0, before.cursor.unwrap().0 + 2);
    state.buffers[buffer].decorations.clear("test");
    let cleared = render(&state, &ui, &loaded.config, &loaded.dashboard, 50, 12);
    assert_eq!(cleared.grid, before.grid);
    assert_eq!(cleared.cursor, before.cursor);
}

#[test]
fn ghost_text_shifts_source_but_not_cursor_in_wrapped_and_unwrapped_views() {
    for wrap in [false, true] {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text("abcd"), None)
            .unwrap();
        state.views[view].mode = EditorMode::Insert;
        state.views[view].cursor = ByteOffset(1);
        let mut ui = ui_with_editor_layout(&state);
        let mut loaded = crate::config::load_bundled_config();
        loaded.config.editor.wrap = wrap;
        let before = render(&state, &ui, &loaded.config, &loaded.dashboard, 50, 12);
        ui.inline_completion = Some((
            view,
            UiInlayHint {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                position: ByteOffset(1),
                label: "ghost".into(),
            },
        ));
        ui.language_completion = Some(LanguageCompletionState {
            typed: 0,
            items: vec![CompletionItem {
                label: "ghost".into(),
                detail: String::new(),
                icon: None,
                color: None,
            }],
            selected: 0,
            scroll: 0,
        });
        let after = render(&state, &ui, &loaded.config, &loaded.dashboard, 50, 12);
        assert_eq!(before.cursor, after.cursor);
        let row = (0..12)
            .find(|row| after.grid.row_text(*row).contains("aghostbcd"))
            .expect("inline source text");
        let line = after.grid.row_text(row);
        let column = line.find("ghost").unwrap() as u16;
        assert_eq!(after.grid.get(column, row).unwrap().style, Style::Muted);
        ui.language_completion = None;
        let dismissed = render(&state, &ui, &loaded.config, &loaded.dashboard, 50, 12);
        assert_eq!(dismissed.grid, before.grid);
    }
}
