use std::path::PathBuf;

use crate::editor::text::{ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet};

use super::{EditorState, RegisterStore};
use crate::editor::{CommandInvocation, InvocationSource};

#[test]
fn cursor_belongs_to_view_not_shared_buffer() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, first) = state.open_text(TextBuffer::from_text("shared"), None);
    let second = state.views.insert(super::View::new(buffer));
    state.views[first].cursor = ByteOffset(1);
    state.views[second].cursor = ByteOffset(5);
    assert_eq!(state.buffers[buffer].text.to_string(), "shared");
    assert_ne!(state.views[first].cursor, state.views[second].cursor);
}

#[test]
fn register_storage_disable_black_hole_and_mirroring_are_explicit() {
    let mut registers = RegisterStore::default();
    let old = super::RegisterValue {
        text: "old".into(),
        shape: super::RegisterShape::Character,
    };
    registers.set('"', old.clone(), true, true);
    registers.set(
        'a',
        super::RegisterValue {
            text: "named".into(),
            shape: super::RegisterShape::Character,
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
fn macro_registers_are_typed_and_uppercase_appends() {
    let mut registers = RegisterStore::default();
    let first =
        CommandInvocation::new("edit.delete_char", InvocationSource::Test).expect("invocation");
    let second =
        CommandInvocation::new("edit.toggle_case", InvocationSource::Test).expect("invocation");
    registers.set_macro('a', vec![first]);
    registers.set_macro('A', vec![second]);
    assert_eq!(registers.get_macro('a').expect("macro").len(), 2);
    assert_eq!(registers.get_macro('A').expect("macro").len(), 2);
}

#[test]
fn canonical_path_identity_reuses_buffer_and_view() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let path = PathBuf::from("/workspace/file.rs");
    let first = state.open_text(TextBuffer::from_text("first"), Some(path.clone()));
    let second = state.open_text(TextBuffer::from_text("ignored"), Some(path));
    assert_eq!(first, second);
    assert_eq!(state.buffers.len(), 1);
    assert_eq!(state.views.len(), 1);
    assert_eq!(state.buffers[first.0].text.to_string(), "first");
}

#[test]
fn replacing_a_pane_keeps_hidden_buffers_without_orphan_views() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (first_buffer, first_view) = state.open_text(TextBuffer::from_text("one"), None);
    state.buffers[first_buffer].dirty = true;
    let (_, second_view) = state.open_text(TextBuffer::from_text("two"), None);
    assert!(!state.views.contains_key(first_view));
    assert!(state.buffers.contains_key(first_buffer));
    assert_eq!(state.views.len(), 1);
    assert_eq!(state.active_view, Some(second_view));
    state
        .cycle_buffer(-1, true)
        .expect("reopen hidden dirty buffer");
    assert_eq!(state.active_buffer_id(), Some(first_buffer));
    assert!(state.buffers[first_buffer].dirty);
    state.validate_layout().expect("valid reopened layout");
}

#[test]
fn split_and_tab_layouts_keep_independent_views_and_focus() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, first) = state.open_text(TextBuffer::from_text("abc"), None);
    let second = state
        .split_active(super::Axis::Horizontal, 500)
        .expect("split");
    assert_eq!(state.views[first].buffer, buffer);
    assert_eq!(state.views[second].buffer, buffer);
    state.views[second].cursor = crate::editor::text::ByteOffset(2);
    state.views[second].scroll_column = 12;
    state.resize_active_pane(50).expect("grow second pane");
    assert!(matches!(
        state.tabs[state.active_tab.expect("tab")].root,
        super::LayoutNode::Split { ratio: 450, .. }
    ));
    state.focus_pane(-1, true).expect("first pane");
    assert_eq!(state.active_view, Some(first));
    assert_eq!(
        state.views[first].cursor,
        crate::editor::text::ByteOffset(0)
    );
    assert_eq!(state.views[first].scroll_column, 0);
    assert_eq!(state.views[second].scroll_column, 12);
    state.focus_pane(-1, false).expect("clamped focus");
    assert_eq!(state.active_view, Some(first));
    let first_tab = state.active_tab.expect("tab");
    let second_tab = state.new_tab().expect("new tab");
    assert_ne!(first_tab, second_tab);
    state.focus_tab(-1, true).expect("previous tab");
    assert_eq!(state.active_tab, Some(first_tab));
    state.close_active_pane().expect("close split");
    assert_eq!(
        state.tabs[first_tab].root,
        super::LayoutNode::Pane(state.tabs[first_tab].focused)
    );
}

#[test]
fn split_rejects_before_exceeding_the_universal_layout_depth_ceiling() {
    let mut state = EditorState::new(PathBuf::from("/bounded"));
    state.open_text(TextBuffer::from_text("seed"), None);
    for _ in 1..super::MAXIMUM_LAYOUT_DEPTH {
        state
            .split_active(super::Axis::Horizontal, 500)
            .expect("split within depth ceiling");
    }
    assert_eq!(
        state.split_active(super::Axis::Horizontal, 500),
        Err("layout depth ceiling reached")
    );
    assert_eq!(state.panes.len(), super::MAXIMUM_LAYOUT_DEPTH);
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
    state.open_text(TextBuffer::from_text("seed"), None);
    state
        .split_active(super::Axis::Horizontal, 500)
        .expect("split");
    let tab = state.active_tab.expect("tab");
    let super::LayoutNode::Split { ratio, .. } = &mut state.tabs[tab].root else {
        panic!("expected split");
    };
    *ratio = 99;
    assert_eq!(
        state.validate_layout(),
        Err("layout split ratio is invalid")
    );
}

#[test]
fn new_tabs_share_the_global_layout_pane_ceiling_without_partial_mutation() {
    let mut state = EditorState::new(PathBuf::from("/bounded-tabs"));
    state.open_text(TextBuffer::from_text("seed"), None);
    for _ in 1..super::MAXIMUM_LAYOUT_PANES {
        state.new_tab().expect("tab within pane ceiling");
    }
    let revision = state.window_revision();
    assert_eq!(state.new_tab(), Err("layout pane ceiling reached"));
    assert_eq!(state.window_revision(), revision);
    assert_eq!(state.panes.len(), super::MAXIMUM_LAYOUT_PANES);
    state.validate_layout().expect("bounded tabs remain valid");
}

#[test]
fn directional_focus_selects_spatial_neighbors_in_a_two_by_two_layout() {
    let mut state = EditorState::new(PathBuf::from("/directional"));
    state.open_text(TextBuffer::from_text("seed"), None);
    let tab = state.active_tab.expect("tab");
    let top_left = state.tabs[tab].focused;
    state
        .split_active(super::Axis::Horizontal, 500)
        .expect("right split");
    let top_right = state.tabs[tab].focused;
    state.focus_pane_id(top_left).expect("focus top left");
    state
        .split_active(super::Axis::Vertical, 500)
        .expect("left lower split");
    let bottom_left = state.tabs[tab].focused;
    state.focus_pane_id(top_right).expect("focus top right");
    state
        .split_active(super::Axis::Vertical, 500)
        .expect("right lower split");
    let bottom_right = state.tabs[tab].focused;

    state.focus_pane_id(top_left).expect("focus top left");
    assert_eq!(
        state.directional_pane(super::FocusDirection::Right),
        Ok(Some(top_right))
    );
    assert_eq!(
        state.directional_pane(super::FocusDirection::Down),
        Ok(Some(bottom_left))
    );
    assert_eq!(
        state.directional_pane(super::FocusDirection::Left),
        Ok(None)
    );
    assert_eq!(state.directional_pane(super::FocusDirection::Up), Ok(None));

    state
        .focus_pane_id(bottom_right)
        .expect("focus bottom right");
    assert_eq!(
        state.directional_pane(super::FocusDirection::Left),
        Ok(Some(bottom_left))
    );
    assert_eq!(
        state.directional_pane(super::FocusDirection::Up),
        Ok(Some(top_right))
    );
}

#[test]
fn layout_model_sequences_preserve_all_id_and_focus_invariants() {
    let mut state = EditorState::new(PathBuf::from("/model"));
    state.open_text(TextBuffer::from_text("seed"), None);
    for step in 0..120 {
        match step % 8 {
            0 if state.panes.len() < 8 => {
                let _ = state.split_active(super::Axis::Horizontal, 500);
            }
            1 if state.tabs.len() < 4 => {
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
                state.open_text(TextBuffer::from_text(&format!("buffer-{step}")), None);
            }
            6 if state.panes.len() > state.tabs.len() => {
                let _ = state.close_active_pane();
            }
            7 if state.tabs.len() > 1 => {
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
        state.cycle_buffer(1, true).expect("cycle hidden buffer");
        reachable.insert(state.active_buffer_id().expect("active buffer"));
        state.validate_layout().expect("cycled layout invariant");
    }
    assert_eq!(reachable, expected, "every hidden buffer remains reachable");
}

#[test]
fn save_completion_for_old_revision_does_not_clear_newer_dirty_edit() {
    let mut buffer = super::Buffer::new(TextBuffer::from_text("a"), None);
    let first = TextEditSet::new(
        buffer.text.revision(),
        vec![TextEdit::new(ByteRange::empty(ByteOffset(1)), "b")],
    );
    buffer.text.apply_edits(&first).expect("first edit");
    buffer.refresh_dirty();
    let persisted = buffer.text.revision();
    let second = TextEditSet::new(
        buffer.text.revision(),
        vec![TextEdit::new(ByteRange::empty(ByteOffset(2)), "c")],
    );
    buffer.text.apply_edits(&second).expect("second edit");
    buffer.refresh_dirty();
    buffer.mark_saved(persisted);
    assert!(buffer.dirty);
    assert_eq!(buffer.text.to_string(), "abc");
}
