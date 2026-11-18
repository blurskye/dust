//! Tests for buffers, views, registers and editor state.

use std::path::PathBuf;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::TextBuffer;

use crate::editor::text::TextEdit;

use crate::editor::text::TextEditSet;

use crate::editor::state::EditorState;

use crate::editor::CommandInvocation;
use crate::editor::InvocationSource;
use crate::editor::state::RegisterStore;

#[test]
fn cursor_belongs_to_view_not_shared_buffer() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, first) = state
        .open_text(TextBuffer::from_text("shared"), None)
        .unwrap();
    let second = state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    let mut transaction =
        crate::editor::transaction::EditorTransaction::new(crate::editor::state::TransactionId(1));
    transaction
        .operations
        .push(crate::editor::transaction::EditorOperation::SetCursor {
            view: first,
            position: ByteOffset(1),
        });
    state.apply_transaction(&transaction).unwrap();
    assert_eq!(state.buffers[buffer].text.to_string(), "shared");
    assert_eq!(state.views[first].cursor, ByteOffset(1));
    assert_eq!(state.views[second].cursor, ByteOffset(0));
}

#[test]
fn register_storage_disable_black_hole_and_mirroring_are_explicit() {
    let mut registers = RegisterStore::default();
    let old = crate::editor::state::RegisterValue {
        text: "old".into(),
        shape: crate::editor::state::RegisterShape::Character,
    };
    registers.set('"', old.clone(), true, true);
    registers.set(
        'a',
        crate::editor::state::RegisterValue {
            text: "named".into(),
            shape: crate::editor::state::RegisterShape::Character,
        },
        true,
        false,
    );
    assert_eq!(registers.get('"'), Some(&old));
    assert_eq!(
        registers.get('a').map(|value| value.text.as_str()),
        Some("named")
    );
    registers.set('_', old.clone(), true, true);
    registers.set('b', old, false, true);
    assert!(registers.get('_').is_none());
    assert!(registers.get('b').is_none());
}

#[test]
fn uppercase_block_register_appends_corresponding_rows() {
    for (left, right, expected) in [
        ("ab\ncd", "12\n34", "ab12\ncd34"),
        ("ab\ncd", "12", "ab12\ncd"),
        ("ab", "12\n34", "ab12\n34"),
    ] {
        let mut registers = RegisterStore::default();
        for (name, text) in [('a', left), ('A', right)] {
            registers.set(
                name,
                crate::editor::state::RegisterValue {
                    text: text.into(),
                    shape: crate::editor::state::RegisterShape::Block,
                },
                true,
                true,
            );
        }
        let expected = crate::editor::state::RegisterValue {
            text: expected.into(),
            shape: crate::editor::state::RegisterShape::Block,
        };
        assert_eq!(registers.get('a'), Some(&expected));
        assert_eq!(registers.get('"'), Some(&expected));
    }
}

#[test]
fn macro_registers_are_typed_and_uppercase_appends() {
    let mut registers = RegisterStore::default();
    let first =
        CommandInvocation::new("edit.delete_char", InvocationSource::Test).expect("invocation");
    let second =
        CommandInvocation::new("edit.toggle_case", InvocationSource::Test).expect("invocation");
    registers.set_macro('a', vec![first.clone()]);
    registers.set_macro('A', vec![second.clone()]);
    let expected = vec![first, second];
    assert_eq!(registers.get_macro('a').expect("macro"), expected);
    assert_eq!(registers.get_macro('A').expect("macro"), expected);
}

#[test]
fn workspace_root_changes_invalidate_window_titles() {
    let mut state = EditorState::new("/workspace".into());
    let before = state.window_version();
    let editor_before = state.version();
    state.set_workspace_root("/other".into());
    assert_ne!(state.window_version(), before);
    assert_ne!(state.version(), editor_before);
    let unchanged = state.window_version();
    state.set_workspace_root("/other".into());
    assert_eq!(state.window_version(), unchanged);
}

#[test]
fn opening_a_file_removes_only_an_unhosted_empty_scratch() {
    let mut state = EditorState::new("/workspace".into());
    let (scratch, _) = state.open_text(TextBuffer::from_text(""), None).unwrap();
    state
        .open_text(TextBuffer::from_text("a"), Some("/workspace/a".into()))
        .unwrap();
    assert!(!state.buffers.contains_key(scratch));
    let (shared, _) = state.open_text(TextBuffer::from_text(""), None).unwrap();
    state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    state
        .open_text(TextBuffer::from_text("b"), Some("/workspace/b".into()))
        .unwrap();
    assert!(state.buffers.contains_key(shared));
    state.validate_layout().unwrap();
}

#[test]
fn canonical_path_identity_reuses_buffer_and_view() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let path = PathBuf::from("/workspace/file.rs");
    let first = state
        .open_text(TextBuffer::from_text("first"), Some(path.clone()))
        .unwrap();
    let second = state
        .open_text(TextBuffer::from_text("ignored"), Some(path))
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(state.buffers.len(), 1);
    assert_eq!(state.views.len(), 1);
    assert_eq!(state.buffers[first.0].text.to_string(), "first");
}

#[test]
fn replacing_a_pane_keeps_hidden_buffers_without_orphan_views() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (first_buffer, first_view) = state.open_text(TextBuffer::from_text("one"), None).unwrap();
    state.buffers[first_buffer].dirty = true;
    let (_, second_view) = state.open_text(TextBuffer::from_text("two"), None).unwrap();
    assert!(!state.views.contains_key(first_view));
    assert!(state.buffers.contains_key(first_buffer));
    assert_eq!(state.views.len(), 1);
    assert_eq!(state.active_view(), Some(second_view));
    state
        .cycle_buffer(&[], -1, true)
        .expect("reopen hidden dirty buffer");
    assert_eq!(state.active_buffer_id(), Some(first_buffer));
    assert!(state.buffers[first_buffer].dirty);
    state.validate_layout().expect("valid reopened layout");
}

#[test]
fn rejected_edit_after_a_split_preserves_the_live_graph_and_selected_view() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
    let before = state.graph.clone();
    let transaction = crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
        .with_operation(crate::editor::EditorOperation::SplitPane {
            axis: crate::windows::TileAxis::Horizontal,
            ratio: 500,
        })
        .with_operation(crate::editor::EditorOperation::SetCursor {
            view,
            position: crate::editor::text::ByteOffset(usize::MAX),
        });
    assert!(state.apply_transaction(&transaction).is_err());
    assert_eq!(state.graph, before);
    assert_eq!(state.views.len(), 1);
    assert_eq!(state.active_view(), Some(view));
    state.validate_layout().unwrap();
}

#[test]
fn split_and_tab_layouts_keep_independent_views_and_focus() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, first) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
    let second = state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split");
    assert_eq!(state.views[first].buffer, buffer);
    assert_eq!(state.views[second].buffer, buffer);
    state.views[second].cursor = crate::editor::text::ByteOffset(2);
    state.views[second].scroll_column = 12;
    state.resize_active_pane(50).expect("grow second pane");
    assert!(matches!(
        state.tab_layout(state.active_tab().expect("tab")).unwrap(),
        crate::windows::TiledLayoutNode::Split { ratio: 450, .. }
    ));
    state.focus_pane(-1, true).expect("first pane");
    assert_eq!(state.active_view(), Some(first));
    assert_eq!(
        state.views[first].cursor,
        crate::editor::text::ByteOffset(0)
    );
    assert_eq!(state.views[first].scroll_column, 0);
    assert_eq!(state.views[second].scroll_column, 12);
    state.focus_pane(-1, false).expect("clamped focus");
    assert_eq!(state.active_view(), Some(first));
    let first_tab = state.active_tab().expect("tab");
    let second_tab = state.new_tab().expect("new tab");
    assert_ne!(first_tab, second_tab);
    state.focus_tab(-1, true).expect("previous tab");
    assert_eq!(state.active_tab(), Some(first_tab));
    state.close_active_pane().expect("close split");
    assert_eq!(
        state.tab_layout(first_tab).unwrap(),
        crate::windows::TiledLayoutNode::Window(state.tab_focused_window(first_tab).unwrap())
    );
}

#[test]
fn minimum_resize_delta_clamps_both_sides_without_overflow() {
    for on_axis in [false, true] {
        for first in [false, true] {
            let mut state = EditorState::new(PathBuf::from("/resize"));
            state.open_text(TextBuffer::from_text("abc"), None).unwrap();
            state
                .split_active(crate::windows::TileAxis::Horizontal, 500)
                .unwrap();
            if first {
                state.focus_pane(-1, true).unwrap();
            }
            if on_axis {
                state
                    .resize_active_pane_on_axis(crate::windows::TileAxis::Horizontal, i16::MIN)
                    .unwrap();
            } else {
                state.resize_active_pane(i16::MIN).unwrap();
            }
            let crate::windows::TiledLayoutNode::Split { ratio, .. } =
                state.tab_layout(state.active_tab().unwrap()).unwrap()
            else {
                panic!("split");
            };
            assert_eq!(ratio, if first { 100 } else { 900 });
            state.validate_layout().unwrap();
        }
    }
}

#[test]
fn split_rejects_before_exceeding_the_universal_layout_depth_limit() {
    let mut state = EditorState::new(PathBuf::from("/bounded"));
    state
        .open_text(TextBuffer::from_text("seed"), None)
        .unwrap();
    for _ in 1..crate::windows::MAX_SPLIT_DEPTH {
        state
            .split_active(crate::windows::TileAxis::Horizontal, 500)
            .expect("split within depth limit");
    }
    assert_eq!(
        state.split_active(crate::windows::TileAxis::Horizontal, 500),
        Err("layout depth limit reached")
    );
    assert_eq!(
        state.editor_windows().count(),
        crate::windows::MAX_SPLIT_DEPTH
    );
    state
        .validate_layout()
        .expect("bounded layout remains valid");
}

#[test]
fn exhausted_window_revision_fails_open_instead_of_hiding_changes() {
    let mut state = EditorState::new(PathBuf::from("/revision"));
    state.window_revision = u64::MAX;
    state.mark_window_changed();
    assert_eq!(state.window_revision(), None);
    state.mark_window_changed();
    assert_eq!(state.window_revision(), None);
}

#[test]
fn layout_validation_rejects_ratios_outside_editor_source_policy() {
    let mut state = EditorState::new(PathBuf::from("/ratios"));
    state
        .open_text(TextBuffer::from_text("seed"), None)
        .unwrap();
    state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split");
    let tab = state.active_tab().expect("tab");
    let before = state.graph.clone();
    let mut root = state.tab_layout(tab).unwrap();
    let crate::windows::TiledLayoutNode::Split { ratio, .. } = &mut root else {
        panic!("expected split");
    };
    *ratio = 99;
    assert_eq!(
        state.graph.replace_layout(tab, Some(root)),
        Err(crate::windows::WindowError::InvalidPlacement)
    );
    assert_eq!(state.graph, before);
    state.validate_layout().unwrap();
}

#[test]
fn new_tabs_share_the_global_layout_pane_limit_without_partial_mutation() {
    let mut state = EditorState::new(PathBuf::from("/bounded-tabs"));
    state
        .open_text(TextBuffer::from_text("seed"), None)
        .unwrap();
    for _ in 1..crate::windows::MAX_WINDOWS {
        state.new_tab().expect("tab within pane limit");
    }
    let revision = state.window_revision();
    assert_eq!(state.new_tab(), Err("layout pane limit reached"));
    assert_eq!(state.window_revision(), revision);
    assert_eq!(state.editor_windows().count(), crate::windows::MAX_WINDOWS);
    state.validate_layout().expect("bounded tabs remain valid");
}

#[test]
fn clone_pane_refuses_the_global_limit_without_partial_mutation() {
    let mut state = EditorState::new(PathBuf::from("/bounded-clone"));
    state
        .open_text(TextBuffer::from_text("seed"), None)
        .unwrap();
    for _ in 1..crate::windows::MAX_WINDOWS {
        state.new_tab().expect("tab within pane limit");
    }
    let tab = state.active_tab().expect("active tab");
    let pane = state.tab_focused_window(tab).unwrap();
    let revision = state.window_revision();
    let active = state.active_view();
    let views = state.views.len();
    assert_eq!(
        state.clone_pane(pane, crate::windows::TileAxis::Horizontal, 500),
        Err("layout pane limit reached")
    );
    assert_eq!(state.window_revision(), revision);
    assert_eq!(state.active_view(), active);
    assert_eq!(state.editor_windows().count(), crate::windows::MAX_WINDOWS);
    assert_eq!(state.views.len(), views);
    state.validate_layout().expect("bounded clone refusal");
}

#[test]
fn opening_beside_a_kept_pane_propagates_split_failure_without_replacing_it() {
    let mut state = EditorState::new("/workspace".into());
    let (hidden, _) = state
        .open_text(TextBuffer::from_text("file"), Some("/workspace/a".into()))
        .unwrap();
    let (kept, _) = state
        .open_text(TextBuffer::from_text("panel"), None)
        .unwrap();
    state.kept_buffer = Some(kept);
    for _ in 1..crate::windows::MAX_WINDOWS {
        state.new_tab().unwrap();
    }
    let active = state.active_view();
    let before = state.window_version();
    let buffers = state.buffers.len();
    let views = state.views.len();
    assert_eq!(
        state.open_text(TextBuffer::from_text("new"), Some("/workspace/b".into())),
        Err("layout pane limit reached")
    );
    assert_eq!(
        state.open_text(
            TextBuffer::from_text("ignored"),
            Some("/workspace/a".into())
        ),
        Err("layout pane limit reached")
    );
    assert_eq!(state.show_buffer(hidden), Err("layout pane limit reached"));
    assert_eq!(state.active_view(), active);
    assert_eq!(state.active_buffer_id(), Some(kept));
    assert_eq!(state.window_version(), before);
    assert_eq!(state.buffers.len(), buffers);
    assert_eq!(state.views.len(), views);
    state.validate_layout().unwrap();
}

#[test]
fn directional_focus_selects_spatial_neighbors_in_a_two_by_two_layout() {
    let mut state = EditorState::new(PathBuf::from("/directional"));
    state
        .open_text(TextBuffer::from_text("seed"), None)
        .unwrap();
    let tab = state.active_tab().expect("tab");
    let top_left = state.tab_focused_window(tab).unwrap();
    state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("right split");
    let top_right = state.tab_focused_window(tab).unwrap();
    state.focus_pane_id(top_left).expect("focus top left");
    state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .expect("left lower split");
    let bottom_left = state.tab_focused_window(tab).unwrap();
    state.focus_pane_id(top_right).expect("focus top right");
    state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .expect("right lower split");
    let bottom_right = state.tab_focused_window(tab).unwrap();

    state.focus_pane_id(top_left).expect("focus top left");
    assert_eq!(
        state.directional_pane(crate::windows::SpatialDirection::Right),
        Ok(Some(top_right))
    );
    assert_eq!(
        state.directional_pane(crate::windows::SpatialDirection::Down),
        Ok(Some(bottom_left))
    );
    assert_eq!(
        state.directional_pane(crate::windows::SpatialDirection::Left),
        Ok(None)
    );
    assert_eq!(
        state.directional_pane(crate::windows::SpatialDirection::Up),
        Ok(None)
    );

    state
        .focus_pane_id(bottom_right)
        .expect("focus bottom right");
    assert_eq!(
        state.directional_pane(crate::windows::SpatialDirection::Left),
        Ok(Some(bottom_left))
    );
    assert_eq!(
        state.directional_pane(crate::windows::SpatialDirection::Up),
        Ok(Some(top_right))
    );
}

#[test]
fn layout_model_sequences_preserve_all_id_and_focus_invariants() {
    let mut state = EditorState::new(PathBuf::from("/model"));
    state
        .open_text(TextBuffer::from_text("seed"), None)
        .unwrap();
    for step in 0..120 {
        match step % 8 {
            0 if state.editor_windows().count() < 8 => {
                let _ = state.split_active(crate::windows::TileAxis::Horizontal, 500);
            }
            1 if state.editor_tabs().count() < 4 => {
                let _ = state.new_tab();
            }
            2 => {
                let _ = state.focus_pane(1, true);
            }
            3 => {
                let _ = state.focus_tab(-1, true);
            }
            4 => {
                let _ = state.resize_active_pane(50);
            }
            5 => {
                state
                    .open_text(TextBuffer::from_text(&format!("buffer-{step}")), None)
                    .unwrap();
            }
            6 if state.editor_windows().count() > state.editor_tabs().count() => {
                let _ = state.close_active_pane();
            }
            7 if state.editor_tabs().count() > 1 => {
                let _ = state.close_active_tab();
            }
            _ => {}
        }
        state.validate_layout().expect("layout invariant");
    }
    let expected = state
        .buffers
        .keys()
        .collect::<std::collections::BTreeSet<_>>();
    let mut reachable = std::collections::BTreeSet::new();
    for _ in 0..expected.len() {
        state
            .cycle_buffer(&[], 1, true)
            .expect("cycle hidden buffer");
        reachable.insert(state.active_buffer_id().expect("active buffer"));
        state.validate_layout().expect("cycled layout invariant");
    }
    assert_eq!(reachable, expected, "every hidden buffer remains reachable");
}

#[test]
fn changed_line_endings_remain_dirty_when_normalized_text_matches() {
    let original = b"a\r\nb\r\nc\n";
    let mut buffer =
        crate::editor::state::Buffer::new(TextBuffer::from_bytes(original).unwrap(), None);
    for (range, replacement) in [(5..6, ""), (5..5, "\n")] {
        let edits = TextEditSet::new(
            buffer.text.revision(),
            vec![TextEdit::new(
                ByteRange::new(ByteOffset(range.start), ByteOffset(range.end)).unwrap(),
                replacement,
            )],
        );
        buffer.text.apply_edits(&edits).unwrap();
        buffer.refresh_dirty();
        assert!(buffer.dirty);
    }
    assert_eq!(buffer.text.to_string(), "a\nb\nc\n");
    assert_eq!(buffer.text.to_bytes(), b"a\r\nb\r\nc\r\n");
    assert_eq!(buffer.persisted_bytes(), original);
    buffer.mark_saved(buffer.text.revision());
    assert!(!buffer.dirty);
}

#[test]
fn save_completion_for_old_revision_does_not_clear_newer_dirty_edit() {
    let mut buffer = crate::editor::state::Buffer::new(TextBuffer::from_text("a"), None);
    let first = TextEditSet::new(
        buffer.text.revision(),
        vec![TextEdit::new(ByteRange::empty(ByteOffset(1)), "b")],
    );
    buffer.text.apply_edits(&first).expect("first edit");
    buffer.refresh_dirty();
    let persisted = buffer.text.revision();
    let saved_bytes = buffer.text.to_bytes();
    let second = TextEditSet::new(
        buffer.text.revision(),
        vec![TextEdit::new(ByteRange::empty(ByteOffset(2)), "c")],
    );
    buffer.text.apply_edits(&second).expect("second edit");
    buffer.refresh_dirty();
    buffer.mark_saved_as(persisted, saved_bytes.clone(), None);
    assert!(buffer.dirty);
    assert_eq!(buffer.text.to_string(), "abc");
    assert_eq!(buffer.saved_revision, persisted);
    assert_eq!(buffer.persisted_bytes(), saved_bytes);
    buffer.text.restore_text("ab").unwrap();
    buffer.refresh_dirty();
    assert!(
        !buffer.dirty,
        "dirty checks use the bytes that actually reached disk"
    );
}

#[test]
fn independently_changed_branches_have_distinct_window_sources() {
    let mut state = EditorState::new("/workspace".into());
    let (buffer, _) = state
        .open_text(TextBuffer::from_text("same"), None)
        .unwrap();
    let mut left = state.clone();
    let mut right = state.clone();
    left.set_buffer_path_and_dirty(buffer, Some("left.rs".into()), false);
    right.set_buffer_path_and_dirty(buffer, Some("right.rs".into()), false);
    assert_eq!(left.window_revision(), right.window_revision());
    assert!(left.window_version().same_editor(&right.window_version()));
    assert_ne!(left.window_version(), right.window_version());
}

#[test]
fn presentation_mutators_mint_sources_and_rejected_preconditions_do_not_clone() {
    fn changed(state: &mut EditorState, mutate: impl FnOnce(&mut EditorState)) {
        let before = state.window_version();
        mutate(state);
        assert_ne!(state.window_version(), before);
        assert!(state.window_version().same_editor(&before));
    }
    let mut state = EditorState::new("/workspace".into());
    changed(&mut state, |s| {
        s.open_text(TextBuffer::from_text("text"), Some("/one".into()))
            .unwrap();
    });
    let buffer = state.active_buffer_id().unwrap();
    changed(&mut state, |s| {
        s.set_buffer_dirty(buffer, true);
    });
    changed(&mut state, |s| {
        s.set_buffer_path_and_dirty(buffer, Some("/two".into()), false);
    });
    changed(&mut state, |s| {
        s.replace_buffer_from_disk(buffer, TextBuffer::from_text("new"), "/two".into());
    });
    changed(&mut state, |s| {
        s.load_hidden_text(TextBuffer::from_text("hidden"), "/hidden".into())
            .unwrap();
    });
    changed(&mut state, |s| {
        s.split_active(crate::windows::TileAxis::Vertical, 500)
            .unwrap();
    });
    changed(&mut state, |s| {
        s.focus_pane(-1, true).unwrap();
    });
    changed(&mut state, |s| {
        s.resize_active_pane(20).unwrap();
    });
    let panes = state
        .editor_windows()
        .map(|window| window.id)
        .collect::<Vec<_>>();
    changed(&mut state, |s| {
        s.swap_panes(panes[0], panes[1]).unwrap();
    });
    changed(&mut state, |s| {
        s.close_active_pane().unwrap();
    });
    changed(&mut state, |s| {
        s.new_tab().unwrap();
    });
    changed(&mut state, |s| {
        s.focus_tab(-1, true).unwrap();
    });
    changed(&mut state, |s| {
        s.close_active_tab().unwrap();
    });
    let before = state.version();
    let copies = EditorState::test_clone_count();
    let invalid = crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
        .with_precondition(crate::editor::Precondition::BufferRevision {
            buffer,
            revision: crate::editor::text::BufferRevision(u64::MAX),
        });
    assert!(state.prepare_transaction(&invalid).is_err());
    assert_eq!(state.version(), before);
    assert_eq!(EditorState::test_clone_count(), copies);
}

#[test]
fn reload_clears_stale_editing_positions() {
    let mut state = EditorState::new("/workspace".into());
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("abcde"), None)
        .unwrap();
    state.views[view].block_insert = vec![ByteOffset(0), ByteOffset(4)];
    state.views[view].multi_marks = vec![ByteRange::new(ByteOffset(3), ByteOffset(5)).unwrap()];
    state.views[view]
        .replace_history
        .push(crate::editor::state::ReplaceRecord {
            start: ByteOffset(4),
            original: "e".into(),
            inserted: "X".into(),
        });
    state.views[view].preferred_column = Some(4);
    assert!(state.replace_buffer_from_disk(buffer, TextBuffer::from_text("x"), "/file".into()));
    let view = &state.views[view];
    assert!(view.block_insert.is_empty());
    assert!(view.multi_marks.is_empty());
    assert!(view.replace_history.is_empty());
    assert_eq!(view.preferred_column, None);
}

#[test]
fn uppercase_registers_append_and_read_through_lowercase() {
    let mut registers = RegisterStore::default();
    for (name, text) in [('a', "one\n"), ('A', "two\n")] {
        registers.set(
            name,
            crate::editor::state::RegisterValue {
                text: text.into(),
                shape: crate::editor::state::RegisterShape::Line,
            },
            true,
            true,
        );
    }
    assert_eq!(registers.get('a').unwrap().text, "one\ntwo\n");
    assert_eq!(registers.get('A'), registers.get('a'));
    assert_eq!(registers.get('"'), registers.get('a'));
}

#[test]
fn switching_buffers_restores_the_last_cursor_and_scroll() {
    let mut state = EditorState::new("/workspace".into());
    let (a, view) = state
        .open_text(
            TextBuffer::from_text("one\ntwo\nthree"),
            Some("/workspace/a".into()),
        )
        .unwrap();
    state.views[view].cursor = ByteOffset(8);
    state.views[view].scroll_line = 2;
    state
        .open_text(TextBuffer::from_text("other"), Some("/workspace/b".into()))
        .unwrap();
    let view = state.show_buffer(a).unwrap();
    assert_eq!(state.views[view].cursor, ByteOffset(8));
    assert_eq!(state.views[view].scroll_line, 2);
}

#[test]
fn closing_a_buffer_restores_the_hidden_replacements_navigation() {
    let mut state = EditorState::new("/workspace".into());
    let (a, first) = state
        .open_text(
            TextBuffer::from_text("one\ntwo\nthree"),
            Some("/workspace/a".into()),
        )
        .unwrap();
    state.views[first].cursor = ByteOffset(8);
    state.views[first].scroll_line = 2;
    let (b, view) = state
        .open_text(TextBuffer::from_text("b"), Some("/workspace/b".into()))
        .unwrap();
    let mut draft = state.draft();
    assert!(draft.close_document(view, b).unwrap());
    let state = draft.accept(&state.version()).unwrap();
    assert_eq!(state.active_buffer_id(), Some(a));
    let restored = &state.views[state.active_view().unwrap()];
    assert_eq!(restored.cursor, ByteOffset(8));
    assert_eq!(restored.scroll_line, 2);
}

#[test]
fn closing_last_views_preserves_navigation_without_editing_state() {
    for close_tab in [false, true] {
        let mut state = EditorState::new("/workspace".into());
        state
            .open_text(
                TextBuffer::from_text("other"),
                Some("/workspace/other".into()),
            )
            .unwrap();
        if close_tab {
            state.new_tab().unwrap();
        } else {
            state
                .split_active(crate::windows::TileAxis::Horizontal, 500)
                .unwrap();
        }
        let (a, view) = state
            .open_text(
                TextBuffer::from_text("one\ntwo\nthree"),
                Some("/workspace/a".into()),
            )
            .unwrap();
        state.views[view].cursor = ByteOffset(8);
        state.views[view].scroll_line = 2;
        state.views[view].scroll_column = 3;
        state.views[view].mode = crate::editor::state::EditorMode::Multi;
        state.views[view].multi_marks = vec![ByteRange::empty(ByteOffset(8))];
        if close_tab {
            state.close_active_tab().unwrap();
        } else {
            state.close_active_pane().unwrap();
        }
        let restored = state.show_buffer(a).unwrap();
        let view = &state.views[restored];
        assert_eq!(view.cursor, ByteOffset(8));
        assert_eq!(view.scroll_line, 2);
        assert_eq!(view.scroll_column, 3);
        assert_eq!(view.mode, crate::editor::state::EditorMode::Normal);
        assert!(view.multi_marks.is_empty());
        state.validate_layout().unwrap();
    }
}

#[test]
fn unzoom_allocates_a_view_when_its_buffer_changes() {
    let mut state = EditorState::new("/workspace".into());
    state
        .open_text(TextBuffer::from_text("a"), Some("/workspace/a".into()))
        .unwrap();
    state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .unwrap();
    let origin = state.active_view().unwrap();
    state.toggle_zoom().unwrap();
    let (b, _) = state
        .open_text(TextBuffer::from_text("b"), Some("/workspace/b".into()))
        .unwrap();
    state.toggle_zoom().unwrap();
    assert_eq!(state.active_buffer_id(), Some(b));
    assert_ne!(state.active_view(), Some(origin));
    state.validate_layout().unwrap();
}
