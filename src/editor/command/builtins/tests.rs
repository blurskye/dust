use std::path::PathBuf;

use crate::editor::text::TextBuffer;

use crate::editor::{
    CommandInvocation, CommandRegistry, CommandValue, EditorState, InvocationSource,
    register_builtin_commands,
};

#[test]
fn movement_and_edit_commands_apply_through_registry_transactions() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (_, view) = state.open_text(TextBuffer::from_text("one two"), None);
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).expect("builtins");
    let mut move_word = CommandInvocation::new("view.cursor.move", InvocationSource::Test)
        .expect("invocation")
        .with_argument("motion", CommandValue::String("word_forward".into()));
    move_word.target.view = Some(view);
    let outcome = registry.execute(&state, &move_word).expect("execute");
    state
        .apply_transaction(outcome.transaction.as_ref().expect("transaction"))
        .expect("apply");
    assert_eq!(state.views[view].cursor.get(), 4);

    state.policy = crate::editor::EditingPolicy {
        indent_width: 2,
        expand_tabs: false,
        ..crate::editor::EditingPolicy::default()
    };
    let mut indent = CommandInvocation::new("edit.operator", InvocationSource::Test)
        .expect("indent")
        .with_argument("operator", CommandValue::String("indent".into()))
        .with_argument("motion", CommandValue::String("line".into()))
        .with_argument("linewise", CommandValue::Bool(true));
    indent.target.view = Some(view);
    let outcome = registry
        .execute(&state, &indent)
        .expect("configured indent");
    state
        .apply_transaction(outcome.transaction.as_ref().expect("transaction"))
        .expect("apply");
    assert!(
        state.buffers[state.views[view].buffer]
            .text
            .to_string()
            .starts_with('\t')
    );
}

fn apply(state: &mut EditorState, id: &str, arguments: &[(&str, &str)], count: u64) {
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).unwrap();
    let mut invocation = CommandInvocation::new(id, InvocationSource::Test).unwrap();
    invocation.count = std::num::NonZeroU64::new(count);
    for (name, value) in arguments {
        invocation = invocation.with_argument(
            name,
            match *value {
                "true" => CommandValue::Bool(true),
                "false" => CommandValue::Bool(false),
                value => CommandValue::String(value.into()),
            },
        );
    }
    let outcome = registry.execute(state, &invocation).unwrap();
    if let Some(transaction) = outcome.transaction {
        state.apply_transaction(&transaction).unwrap();
    }
}

fn editing(
    text: &str,
    cursor: usize,
) -> (EditorState, crate::editor::BufferId, crate::editor::ViewId) {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state.open_text(TextBuffer::from_text(text), None);
    state.views[view].cursor = crate::editor::text::ByteOffset(cursor);
    (state, buffer, view)
}

#[test]
fn linewise_put_separates_an_unterminated_last_line() {
    use crate::editor::{RegisterShape, RegisterValue};
    let (mut state, buffer, view) = editing("one\ntwo", 4);
    state.registers.set(
        '"',
        RegisterValue {
            text: "two".into(),
            shape: RegisterShape::Line,
        },
        true,
        true,
    );
    apply(&mut state, "edit.put", &[("before", "false")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "one\ntwo\ntwo\n");
    assert_eq!(state.views[view].cursor.get(), 8);
}

#[test]
fn block_put_extends_eof_and_repeats_rows_horizontally() {
    use crate::editor::{RegisterShape, RegisterValue};
    for (text, cursor, expected) in [
        ("x", 0, "aax\nbb\ncc"),
        ("xx", 1, "xaax\n bb\n cc"),
        ("x\nx\nx\nx", 0, "aax\nbbx\nccx\nx"),
    ] {
        let (mut state, buffer, _) = editing(text, cursor);
        state.registers.set(
            '"',
            RegisterValue {
                text: "a\nb\nc".into(),
                shape: RegisterShape::Block,
            },
            true,
            true,
        );
        apply(&mut state, "edit.put", &[("before", "true")], 2);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn crlf_insert_and_replace_use_normalized_cursor_lengths() {
    for command in ["edit.insert_text", "edit.replace_text"] {
        let (mut state, buffer, view) = editing("", 0);
        state.views[view].mode = crate::editor::EditorMode::Insert;
        apply(&mut state, command, &[("text", "a\r\nb")], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), "a\nb");
        assert_eq!(state.views[view].cursor.get(), 3);
    }
}

#[test]
fn normal_horizontal_motion_stays_off_line_endings() {
    let (mut state, _, view) = editing("ab\ncd\n", 1);
    apply(&mut state, "view.cursor.move", &[("motion", "right")], 1);
    assert_eq!(state.views[view].cursor.get(), 1);
    state.views[view].mode = crate::editor::EditorMode::Insert;
    apply(&mut state, "view.cursor.move", &[("motion", "right")], 1);
    assert_eq!(state.views[view].cursor.get(), 2);
}

#[test]
fn counted_horizontal_and_word_motions_stop_at_boundaries() {
    let source = format!("{}\nlast", "e\u{301}".repeat(20_000));
    let (mut state, _, view) = editing(&source, 0);
    for motion in ["left", "word_backward", "big_word_backward"] {
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1_000_000);
        assert_eq!(state.views[view].cursor.get(), 0, "{motion}");
    }
    apply(&mut state, "view.cursor.move", &[("motion", "right")], 1_000_000);
    assert_eq!(state.views[view].cursor.get(), source.len() - 8);
    apply(&mut state, "view.cursor.move", &[("motion", "left")], 1_000_000);
    assert_eq!(state.views[view].cursor.get(), 0);
    for motion in ["word_forward", "big_word_forward", "word_end", "big_word_end"] {
        state.views[view].cursor = crate::editor::text::ByteOffset(0);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1_000_000);
        assert_eq!(state.views[view].cursor.get(), source.len() - 1, "{motion}");
    }
}

#[test]
fn counted_line_end_moves_and_deletes_through_the_requested_line() {
    for (count, cursor, remaining) in [(2, 7, "a\nlast\n"), (100, 12, "a\n")] {
        let (mut state, buffer, view) = editing("abc\ndefg\nlast\n", 1);
        apply(&mut state, "view.cursor.move", &[("motion", "line_end")], count);
        assert_eq!(state.views[view].cursor.get(), cursor);
        state.views[view].cursor = crate::editor::text::ByteOffset(1);
        apply(&mut state, "edit.operator", &[
            ("operator", "delete"), ("motion", "line_end"), ("linewise", "false")
        ], count);
        assert_eq!(state.buffers[buffer].text.to_string(), remaining);
    }
    for (source, cursor, count, expected) in [
        ("a\n\nb", 2, 1, "a\n\nb"),
        ("a\nb", 0, 1, "\nb"),
        ("a", 0, 1, ""),
        ("a\nb\nc", 0, 2, "\nc"),
        ("a\n\nb", 0, 2, "\nb"),
        ("e\u{301}\nb", 0, 1, "\nb"),
    ] {
        let (mut state, buffer, view) = editing(source, cursor);
        apply(&mut state, "edit.operator", &[
            ("operator", "delete"), ("motion", "line_end"), ("linewise", "false")
        ], count);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        if source == "a\n\nb" && cursor == 2 {
            assert_eq!(state.views[view].cursor.get(), 2);
        }
    }
}

#[test]
fn counted_percent_navigates_lines_and_document_start_skips_indent() {
    let source = "  line\n".repeat(100);
    let (mut state, _, view) = editing(&source, 70);
    for (count, line) in [(1, 0), (50, 49), (100, 99)] {
        apply(&mut state, "view.cursor.move", &[("motion", "matching_pair")], count);
        assert_eq!(state.views[view].cursor.get(), line * 7 + 2);
    }
    for count in [0, 1] {
        state.views[view].cursor = crate::editor::text::ByteOffset(70);
        apply(&mut state, "view.cursor.move", &[("motion", "document_start")], count);
        assert_eq!(state.views[view].cursor.get(), 2);
    }
}

#[test]
fn matching_pairs_ignore_quoted_and_escaped_delimiters_in_both_directions() {
    for source in [
        r#"( ")" )"#,
        r#"( '(' ')' )"#,
        r"( `)` )",
        r#"( "escaped \" )" )"#,
        r"( \) )",
        r"( \\() )",
        "(\n  \"quoted )\"\n)",
        r#""ignored (" ( ")" )"#,
    ] {
        let opening = if source.starts_with('"') { source.find(" ( ").unwrap() + 1 } else { 0 };
        let closing = source.len() - 1;
        let (mut state, _, view) = editing(source, opening);
        apply(&mut state, "view.cursor.move", &[("motion", "matching_pair")], 0);
        assert_eq!(state.views[view].cursor.get(), closing, "{source}");
        apply(&mut state, "view.cursor.move", &[("motion", "matching_pair")], 0);
        assert_eq!(state.views[view].cursor.get(), opening, "{source}");
    }
}

#[test]
fn counted_percent_operators_cover_whole_lines() {
    for cursor in [0, 8] {
        let (mut state, buffer, _) = editing("  one\ntwo\nthree\nfour\n", cursor);
        apply(&mut state, "edit.operator", &[
            ("operator", "delete"), ("motion", "matching_pair"), ("linewise", "false")
        ], 50);
        let expected = if cursor == 0 { "three\nfour\n" } else { "  one\nthree\nfour\n" };
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.registers.get('"').unwrap().shape, crate::editor::RegisterShape::Line);
    }
}

#[test]
fn replace_backspace_does_not_restore_across_cursor_movement() {
    let (mut state, buffer, view) = editing("abc\ndef", 0);
    apply(&mut state, "mode.replace", &[], 1);
    apply(&mut state, "edit.replace_text", &[("text", "X")], 1);
    apply(&mut state, "view.cursor.move", &[("motion", "down")], 1);
    let cursor = state.views[view].cursor;
    apply(&mut state, "edit.replace_backspace", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "Xbc\ndef");
    assert_eq!(state.views[view].cursor, cursor);
    assert!(state.views[view].replace_history.is_empty());
}

#[test]
fn backward_inclusive_operators_include_the_cursor_character() {
    for (text, cursor, motion, character, expected) in [
        ("(abc)", 4, "matching_pair", "(", ""),
        ("abc", 2, "find_backward", "a", ""),
        ("abc", 2, "till_backward", "a", "a"),
        ("abcb", 3, "till_backward_again", "a", "a"),
    ] {
        let (mut state, buffer, _) = editing(text, cursor);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", motion),
                ("character", character),
                ("linewise", "false"),
            ],
            0,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn word_motions_and_objects_keep_combining_graphemes_whole() {
    for (motion, cursor, expected) in [
        ("word_forward", 0, 5),
        ("word_backward", 5, 0),
        ("word_end", 0, 3),
    ] {
        let (mut state, _, view) = editing("e\u{301}x y", cursor);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1);
        assert_eq!(state.views[view].cursor.get(), expected, "{motion}");
    }
    let (mut state, buffer, _) = editing("e\u{301} test", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "delete"),
            ("motion", "inner_word"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), " test");
}

#[test]
fn repeated_till_skips_an_adjacent_target_after_a_combining_grapheme() {
    let (mut state, _, view) = editing("e\u{301}x ax", 0);
    apply(&mut state, "view.cursor.move", &[("motion", "till_forward"), ("character", "x")], 1);
    assert_eq!(state.views[view].cursor.get(), 0);
    apply(&mut state, "view.cursor.move", &[("motion", "till_forward_again"), ("character", "x")], 1);
    assert_eq!(state.views[view].cursor.get(), 5);
    let (mut state, _, view) = editing("xax e\u{301}z", 4);
    apply(&mut state, "view.cursor.move", &[("motion", "till_backward"), ("character", "x")], 1);
    assert_eq!(state.views[view].cursor.get(), 3);
    apply(&mut state, "view.cursor.move", &[("motion", "till_backward_again"), ("character", "x")], 1);
    assert_eq!(state.views[view].cursor.get(), 1);
}

#[test]
fn word_motions_stop_on_empty_lines_in_both_directions() {
    for (motion, cursor, count, expected) in [
        ("word_forward", 0, 1, 4), ("word_forward", 0, 2, 5),
        ("big_word_forward", 0, 1, 4), ("big_word_forward", 4, 1, 5),
        ("word_backward", 5, 1, 4), ("word_backward", 5, 2, 0),
        ("big_word_backward", 5, 1, 4), ("big_word_backward", 4, 1, 0),
    ] {
        let (mut state, _, view) = editing("one\n\ntwo", cursor);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], count);
        assert_eq!(state.views[view].cursor.get(), expected, "{motion}");
    }
}

#[test]
fn word_forward_operators_keep_the_final_words_newline() {
    for motion in ["word_forward", "big_word_forward"] {
        for (source, cursor, count, expected) in [
            ("one\ntwo", 0, 1, "\ntwo"),
            ("\ntwo", 0, 1, "two"),
            ("one  \n  two", 0, 1, "\n  two"),
            ("one two\nthree", 0, 2, "\nthree"),
            ("one\ntwo three", 0, 2, "three"),
            ("one\n  \ntwo", 0, 1, "\n  \ntwo"),
            ("one\ntwo\nthree", 0, 2, "three"),
            ("one\ntwo\nthree", 1, 2, "o\nthree"),
            ("one\n\ntwo", 0, 2, "two"),
            ("one\n\ntwo", 1, 2, "o\ntwo"),
            ("  one\ntwo\nthree", 2, 2, "three"),
            ("one\ntwo\n", 0, 100, ""),
        ] {
            let (mut state, buffer, _) = editing(source, cursor);
            apply(&mut state, "edit.operator", &[
                ("operator", "delete"), ("motion", motion), ("linewise", "false")
            ], count);
            assert_eq!(state.buffers[buffer].text.to_string(), expected, "{motion}");
        }
    }
}

#[test]
fn repeated_word_end_advances_but_change_word_keeps_its_space() {
    for motion in ["word_end", "big_word_end"] {
        let (mut state, _, view) = editing("one two", 0);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1);
        assert_eq!(state.views[view].cursor.get(), 2);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1);
        assert_eq!(state.views[view].cursor.get(), 6);
    }
    let (mut state, buffer, _) = editing("a two", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "change"),
            ("motion", "word_forward"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), " two");
}

#[test]
fn counted_word_and_paragraph_objects_extend_the_range() {
    for (text, motion, expected) in [
        ("one two three", "a_word", "three"),
        ("one\n\ntwo\n\nthree", "a_paragraph", "three"),
    ] {
        let (mut state, buffer, _) = editing(text, 0);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", motion),
                ("linewise", "false"),
            ],
            2,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn adjacent_multi_changes_keep_one_insertion_at_each_resulting_position() {
    use crate::editor::text::{ByteOffset, ByteRange};
    let (mut state, buffer, view) = editing("::x", 0);
    state.views[view].mode = crate::editor::EditorMode::Multi;
    state.views[view].multi_marks = vec![
        ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
        ByteRange::new(ByteOffset(1), ByteOffset(2)).unwrap(),
    ];
    apply(&mut state, "multi.insert", &[("placement", "change")], 1);
    apply(&mut state, "edit.insert_text", &[("text", "a")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "ax");
}

#[test]
fn multi_append_advances_empty_marks_and_line_cursors_use_display_columns() {
    let (mut state, buffer, _) = editing("ab\nab", 0);
    apply(&mut state, "multi.add_below", &[], 1);
    apply(&mut state, "multi.insert", &[("placement", "end")], 1);
    apply(&mut state, "edit.insert_text", &[("text", "X")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "aXb\naXb");
    let (mut state, _, view) = editing("\tx\n123456789", 1);
    state.policy.tab_width = 4;
    apply(&mut state, "multi.add_below", &[], 1);
    assert_eq!(state.views[view].cursor.get(), 7);
}

#[test]
fn escaped_quotes_insert_before_the_autopaired_closer() {
    let (mut state, buffer, _) = editing("", 0);
    state.policy.autopairs = true;
    for text in ["\"", "\\", "\""] {
        apply(&mut state, "edit.insert_text", &[("text", text)], 1);
    }
    assert_eq!(state.buffers[buffer].text.to_string(), "\"\\\"\"");
}

#[test]
fn empty_objects_can_be_surrounded_and_symmetric_surrounds_removed() {
    let (mut state, buffer, _) = editing("()", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "surround_add"),
            ("motion", "inner_paren"),
            ("linewise", "false"),
            ("character", "]"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "([])");
    let (mut state, buffer, _) = editing("*word*", 1);
    apply(
        &mut state,
        "edit.surround",
        &[("character", "*"), ("action", "delete")],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "word");
}

#[test]
fn indentation_and_joins_accept_multibyte_endpoints() {
    let (mut state, buffer, _) = editing("é", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "indent"),
            ("motion", "line"),
            ("linewise", "true"),
        ],
        1,
    );
    assert!(state.buffers[buffer].text.to_string().ends_with('é'));
    let (mut state, buffer, view) = editing("a   \né\n", 3);
    apply(&mut state, "edit.join_lines", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "a é\n");
    assert_eq!(state.views[view].cursor.get(), 1);
    let (mut state, buffer, view) = editing("a\né\n", 0);
    state.views[view].mode = crate::editor::EditorMode::VisualChar;
    state.views[view].selection = Some(crate::editor::Selection {
        anchor: crate::editor::text::ByteOffset(0),
        head: crate::editor::text::ByteOffset(2),
        kind: crate::editor::SelectionKind::Character,
    });
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "join"),
            ("motion", "visual"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "a é\n");
}

#[test]
fn comment_templates_with_outer_whitespace_round_trip() {
    for template in [" <!-- %s -->", "<!-- %s --> ", " <!-- %s --> "] {
        let (mut state, buffer, _) = editing("  hello\n  world\n", 0);
        let arguments = [
            ("operator", "comment"),
            ("motion", "line"),
            ("linewise", "true"),
            ("comment", template),
        ];
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(state.buffers[buffer].text.to_string(), "  <!-- hello -->\n  <!-- world -->\n");
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(state.buffers[buffer].text.to_string(), "  hello\n  world\n");
    }
}

#[test]
fn comment_insertion_uses_each_lines_unicode_whitespace_boundaries() {
    for (original, commented) in [
        ("\u{2003}one\n two\n", "\u{2003}// one\n // two\n"),
        ("\u{2003}\u{2003}one\n two\n", "\u{2003}// \u{2003}one\n // two\n"),
        (" one\n\u{2003}two\n", " // one\n\u{2003}// two\n"),
    ] {
        let (mut state, buffer, _) = editing(original, 0);
        let arguments = [
            ("operator", "comment"),
            ("motion", "line"),
            ("linewise", "true"),
            ("comment", "//"),
        ];
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(state.buffers[buffer].text.to_string(), commented);
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(state.buffers[buffer].text.to_string(), original);
    }
}

#[test]
fn blank_comment_ranges_finish_the_operator_and_visual_modes() {
    for mode in [
        crate::editor::EditorMode::OperatorPending,
        crate::editor::EditorMode::VisualLine,
    ] {
        let (mut state, buffer, view) = editing("\n\n", 0);
        state.views[view].mode = mode;
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "comment"),
                ("motion", "line"),
                ("linewise", "true"),
                ("comment", "//"),
            ],
            2,
        );
        assert_eq!(state.views[view].mode, crate::editor::EditorMode::Normal);
        assert_eq!(state.buffers[buffer].text.to_string(), "\n\n");
    }
}
