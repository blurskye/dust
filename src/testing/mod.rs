//! Headless editor and application test drivers.

#![forbid(unsafe_code)]
mod headless;

use crate::editor::input::ABSOLUTE_MAX_COUNT;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Config;

use crate::config::load_bundled_config;

use crate::editor::input::GrammarInput;

use crate::editor::input::InputAction;

use crate::editor::input::InputError;

use crate::editor::input::KeyEvent;

use crate::editor::input::KeymapSequence;

use crate::editor::input::MappingResolution;

use crate::editor::input::ModalResolver;

use crate::editor::text::TextBuffer;

use crate::app::commands::AppEffect;

use crate::app::configuration::ResolvedKeymaps;

use crate::app::configuration::compile_keymaps;

use crate::app::commands::register_app_commands;

use crate::editor::CommandError;

use crate::editor::CommandInvocation;

use crate::editor::CommandRegistry;

use crate::editor::CommandValue;

use crate::editor::EditorMode;

use crate::editor::EditorState;

use crate::editor::TransactionError;

use crate::editor::ViewId;

use crate::editor::register_builtin_commands;

/// Deterministic editor harness that exercises bundled configuration, the same
/// registry, semantic modal grammar, transactions, and reducer without a terminal.
pub struct HeadlessEditor {
    pub state: EditorState,
    pub registry: CommandRegistry<AppEffect>,
    pub input: ModalResolver,
    keymaps: ResolvedKeymaps,
    sequences: HashMap<String, KeymapSequence>,
    config: Config,
}

impl HeadlessEditor {
    pub fn with_text(text: &str) -> Self {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(TextBuffer::from_text(text), None).unwrap();
        let mut registry = CommandRegistry::default();
        register_builtin_commands(&mut registry).expect("built-in registry must be valid");
        register_app_commands(&mut registry).expect("application registry must be valid");
        let loaded = load_bundled_config();
        state.policy = loaded.config.editor.editing_policy();
        let (keymaps, diagnostic) =
            compile_keymaps(&registry, &loaded.keymaps, &loaded.config.input.keymaps);
        assert!(diagnostic.is_none(), "bundled keymaps must compile");
        Self {
            state,
            registry,
            input: ModalResolver::with_count_limit(ABSOLUTE_MAX_COUNT),
            keymaps,
            sequences: HashMap::new(),
            config: loaded.config,
        }
    }

    pub fn active_view(&self) -> ViewId {
        self.state
            .active_view()
            .expect("headless editor has a view")
    }

    pub fn text(&self) -> String {
        let buffer = self.state.active_buffer_id().expect("active buffer");
        self.state.buffers[buffer].text.to_string()
    }

    pub fn execute(&mut self, mut invocation: CommandInvocation) -> Result<(), HarnessError> {
        if !invocation.id.as_str().starts_with("input.") {
            self.input.decorate_invocation(&mut invocation);
        }
        if invocation.target.view.is_none() {
            invocation.target.view = self.state.active_view();
        }
        if invocation.target.buffer.is_none() {
            invocation.target.buffer = self.state.active_buffer_id();
        }
        let outcome = self.registry.execute(&self.state, &invocation)?;
        if let Some(transaction) = outcome.transaction {
            self.state.apply_transaction(&transaction)?;
        }
        for effect in outcome.effects {
            match effect {
                AppEffect::Grammar(input) => self.process_grammar(input)?,
                other => return Err(HarnessError::UnexpectedEffect(format!("{other:?}"))),
            }
        }
        Ok(())
    }

    pub fn feed(&mut self, key: KeyEvent) -> Result<InputAction, HarnessError> {
        let context = self.active_context();
        match self.keymaps.resolve(
            self.sequences.entry(context.clone()).or_default(),
            &context,
            key.clone(),
        ) {
            MappingResolution::Ready(invocation) => {
                self.execute(invocation)?;
                Ok(InputAction::Pending)
            }
            MappingResolution::ReadyAndReplay(invocation, replay) => {
                self.execute(invocation)?;
                for key in replay {
                    self.feed(key)?;
                }
                Ok(InputAction::Pending)
            }
            MappingResolution::Replay(prefix, replay) => {
                let mode = self.state.views[self.active_view()].mode;
                if context.starts_with("editor.")
                    && !matches!(mode, EditorMode::Insert | EditorMode::Replace)
                {
                    self.input.cancel_pending();
                } else {
                    for key in prefix {
                        self.type_unmapped(&context, &key)?;
                    }
                    for key in replay {
                        self.feed(key)?;
                    }
                }
                Ok(InputAction::Pending)
            }
            MappingResolution::DisabledAndReplay(replay) => {
                for key in replay {
                    self.feed(key)?;
                }
                Ok(InputAction::Pending)
            }
            MappingResolution::Missing => {
                if let Some(action) = self.type_unmapped(&context, &key)? {
                    return Ok(action);
                }
                if let Some(command_id) =
                    self.config.input.unmatched_commands.get(&context).cloned()
                {
                    self.execute(CommandInvocation::new(
                        &command_id,
                        crate::editor::InvocationSource::Keymap,
                    )?)?;
                    if self.active_context() != context {
                        return self.feed(key);
                    }
                }
                Ok(InputAction::Unmapped)
            }
            MappingResolution::Disabled
            | MappingResolution::Pending
            | MappingResolution::Ambiguous => Ok(InputAction::Pending),
        }
    }

    fn type_unmapped(
        &mut self,
        context: &str,
        key: &KeyEvent,
    ) -> Result<Option<InputAction>, HarnessError> {
        let Some(character) = key.text_character() else {
            return Ok(None);
        };
        let Some(command_id) = self
            .config
            .input
            .character_fallback_commands
            .get(context)
            .or_else(|| self.config.input.text_commands.get(context))
            .cloned()
        else {
            return Ok(None);
        };
        self.execute(
            CommandInvocation::new(&command_id, crate::editor::InvocationSource::Keymap)?
                .with_argument("text", CommandValue::String(character.to_string())),
        )?;
        Ok(Some(InputAction::Text(character.to_string())))
    }

    fn process_grammar(&mut self, input: GrammarInput) -> Result<(), HarnessError> {
        let view = self.active_view();
        let mode = self.state.views[view].mode;
        match self.input.feed(mode, input)? {
            InputAction::Invoke(invocation) => self.execute(invocation)?,
            InputAction::Pending => {
                if self.input.effective_mode(mode) == EditorMode::OperatorPending
                    && mode != EditorMode::OperatorPending
                {
                    self.state.views[view].mode = EditorMode::OperatorPending;
                }
            }
            InputAction::Cancelled => {
                if mode == EditorMode::OperatorPending {
                    self.state.views[view].mode = EditorMode::Normal;
                }
            }
            InputAction::Text(_) | InputAction::Unmapped => {}
        }
        Ok(())
    }

    fn active_context(&self) -> String {
        if let Some(pending) = self.input.pending_context() {
            return match pending {
                crate::editor::input::PendingInputContext::Register => "register",
                crate::editor::input::PendingInputContext::Prefix => "prefix",
                crate::editor::input::PendingInputContext::Replace => "replace",
                crate::editor::input::PendingInputContext::Character => "character",
                crate::editor::input::PendingInputContext::MacroRecord => "macro_record",
                crate::editor::input::PendingInputContext::MacroReplay => "macro_replay",
            }
            .into();
        }
        let mode = self.state.views[self.active_view()].mode;
        let mode = match mode {
            EditorMode::Normal => "normal",
            EditorMode::Insert => "insert",
            EditorMode::Replace => "replace",
            EditorMode::VisualChar => "visual_char",
            EditorMode::VisualLine => "visual_line",
            EditorMode::VisualBlock => "visual_block",
            EditorMode::Multi => "multi",
            EditorMode::OperatorPending => "operator_pending",
            EditorMode::CommandLine => "command_line",
            EditorMode::Prompt => "prompt",
        };
        format!("editor.{mode}")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Transaction(#[from] TransactionError),
    #[error("headless harness does not process effect: {0}")]
    UnexpectedEffect(String),
}

#[cfg(test)]
mod tests {
    use crate::editor::input::KeyEvent;

    use crate::testing::HeadlessEditor;

    #[test]
    fn modal_sequence_edits_and_undoes_through_bundled_keymaps() {
        let mut editor = HeadlessEditor::with_text("one two");
        editor.feed(KeyEvent::character('w')).expect("move");
        editor.feed(KeyEvent::character('x')).expect("delete");
        assert_eq!(editor.text(), "one wo");
        editor.feed(KeyEvent::character('u')).expect("undo");
        assert_eq!(editor.text(), "one two");
    }

    #[test]
    fn rejected_normal_mapping_consumes_the_suffix_and_cancels_grammar() {
        let mut editor = HeadlessEditor::with_text("one two");
        for character in "2]q".chars() {
            editor
                .feed(KeyEvent::character(character))
                .expect("rejected mapping");
        }
        assert_eq!(editor.active_context(), "editor.normal");
        assert!(editor.input.pending_context().is_none());
        editor.feed(KeyEvent::character('x')).expect("delete once");
        assert_eq!(editor.text(), "ne two");
    }

    #[test]
    fn rejected_insert_mapping_types_its_prefix_before_replaying_the_suffix() {
        let mut editor = HeadlessEditor::with_text("base");
        let mut loaded = crate::config::load_bundled_config();
        let mut mapping = loaded
            .keymaps
            .entries
            .iter()
            .find(|mapping| mapping.command.as_deref() == Some("mode.normal"))
            .expect("normal mode mapping")
            .clone();
        mapping.id = "test.insert.jk".into();
        mapping.context = Some("editor".into());
        mapping.mode = "insert".into();
        mapping.keys = Some("j k".into());
        loaded.keymaps.entries.push(mapping);
        let (keymaps, diagnostic) = crate::app::configuration::compile_keymaps(
            &editor.registry,
            &loaded.keymaps,
            &loaded.config.input.keymaps,
        );
        assert!(diagnostic.is_none());
        editor.keymaps = keymaps;
        editor.feed(KeyEvent::character('i')).expect("insert mode");
        for character in "ja".chars() {
            editor
                .feed(KeyEvent::character(character))
                .expect("insert text");
        }
        assert_eq!(editor.text(), "jabase");
        assert_eq!(editor.active_context(), "editor.insert");
    }

    #[test]
    fn unmatched_prefix_cancels_the_operator_and_replays_in_normal_mode() {
        let mut editor = HeadlessEditor::with_text("one two");
        editor.feed(KeyEvent::character('d')).expect("operator");
        editor.feed(KeyEvent::character('g')).expect("prefix");
        assert_eq!(
            editor.state.views[editor.active_view()].mode,
            crate::editor::EditorMode::OperatorPending
        );
        editor
            .feed(KeyEvent::character('x'))
            .expect("cancel and delete");
        assert_eq!(editor.text(), "ne two");
        assert_eq!(editor.active_context(), "editor.normal");
        assert!(editor.input.pending_context().is_none());
    }

    #[test]
    fn insert_backspace_and_line_operator_use_registry_transactions() {
        let mut editor = HeadlessEditor::with_text("one\ntwo\n");
        editor.feed(KeyEvent::character('i')).expect("insert mode");
        editor.feed(KeyEvent::character('X')).expect("insert text");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Backspace))
            .expect("backspace");
        assert_eq!(editor.text(), "one\ntwo\n");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal mode");
        editor.feed(KeyEvent::character('d')).expect("operator");
        editor.feed(KeyEvent::character('d')).expect("line motion");
        assert_eq!(editor.text(), "two\n");
    }

    #[test]
    fn one_insert_session_is_one_undo_change() {
        let mut editor = HeadlessEditor::with_text("base");
        editor.feed(KeyEvent::character('i')).expect("insert mode");
        for character in "ABC".chars() {
            editor
                .feed(KeyEvent::character(character))
                .expect("insert character");
        }
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal mode");
        assert_eq!(editor.text(), "ABCbase");
        editor.feed(KeyEvent::character('u')).expect("undo");
        assert_eq!(editor.text(), "base");
    }

    #[test]
    fn configured_text_commit_undo_boundaries_are_respected() {
        let mut editor = HeadlessEditor::with_text("base");
        editor.state.policy.undo_break_on_text_commit = true;
        editor.feed(KeyEvent::character('i')).expect("insert mode");
        for character in "AB".chars() {
            editor.feed(KeyEvent::character(character)).expect("insert");
        }
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal");
        editor.feed(KeyEvent::character('u')).expect("undo commit");
        assert_eq!(editor.text(), "Abase");
    }

    #[test]
    fn change_line_keeps_the_line_and_its_indent() {
        let mut editor = HeadlessEditor::with_text("  one\ntwo\n");
        for key in ['c', 'c', 'X'] {
            editor.feed(KeyEvent::character(key)).expect("change line");
        }
        assert_eq!(editor.text(), "  X\ntwo\n");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal mode");
        editor.feed(KeyEvent::character('u')).expect("undo change");
        assert_eq!(editor.text(), "  one\ntwo\n");
    }

    #[test]
    fn replace_mode_restores_replaced_text_and_undoes_as_one_session() {
        let mut editor = HeadlessEditor::with_text("abc");
        editor.feed(KeyEvent::character('R')).expect("replace mode");
        editor.feed(KeyEvent::character('X')).expect("replace a");
        editor.feed(KeyEvent::character('Y')).expect("replace b");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Backspace))
            .expect("restore b");
        assert_eq!(editor.text(), "Xbc");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal mode");
        editor.feed(KeyEvent::character('u')).expect("undo replace");
        assert_eq!(editor.text(), "abc");

        editor.feed(KeyEvent::character('R')).expect("replace mode");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Enter))
            .expect("replace newline");
        assert_eq!(editor.text(), "\nabc");
    }

    #[test]
    fn blockwise_visual_delete_uses_disjoint_line_ranges() {
        let mut editor = HeadlessEditor::with_text("abc\ndef\nghi");
        editor
            .feed("ctrl-v".parse::<KeyEvent>().expect("key notation"))
            .expect("block visual");
        editor.feed(KeyEvent::character('j')).expect("second row");
        editor
            .feed(KeyEvent::character('l'))
            .expect("second column");
        editor.feed(KeyEvent::character('d')).expect("block delete");
        assert_eq!(editor.text(), "c\nf\nghi");
        editor.feed(KeyEvent::character('p')).expect("block put");
        assert_eq!(editor.text(), "cab\nfde\nghi");

        let mut unicode = HeadlessEditor::with_text("éa\nxy");
        unicode
            .feed("ctrl-v".parse::<KeyEvent>().expect("key notation"))
            .expect("block visual");
        unicode.feed(KeyEvent::character('l')).expect("column");
        unicode.feed(KeyEvent::character('j')).expect("row");
        unicode.feed(KeyEvent::character('d')).expect("delete");
        assert_eq!(unicode.text(), "\n");
    }

    #[test]
    fn blockwise_display_cells_cover_tabs_wide_glyphs_ragged_and_reversed_ranges() {
        let mut editor = HeadlessEditor::with_text("\tZ\n界ab\nx");
        let view = editor.active_view();
        editor.state.views[view].cursor = crate::editor::text::ByteOffset("\tZ\n界ab".len());
        editor.state.views[view].mode = crate::editor::EditorMode::VisualBlock;
        editor.state.views[view].selection = Some(crate::editor::Selection {
            anchor: crate::editor::text::ByteOffset("\tZ\n界ab".len()),
            head: crate::editor::text::ByteOffset(0),
            kind: crate::editor::SelectionKind::Block,
        });
        editor.feed(KeyEvent::character('d')).expect("block delete");
        assert_eq!(editor.text(), "\n\nx");
        editor.feed(KeyEvent::character('P')).expect("block put");
        assert_eq!(editor.text(), "\tZ\n界ab\nx");
    }

    #[test]
    fn blockwise_yank_preserves_empty_rows_and_ambiguous_combining_cells() {
        let mut editor = HeadlessEditor::with_text("a\u{301}b\n\n·x");
        editor.state.policy.ambiguous_width_wide = true;
        let view = editor.active_view();
        editor.state.views[view].cursor = crate::editor::text::ByteOffset("a\u{301}b\n\n·".len());
        editor.state.views[view].mode = crate::editor::EditorMode::VisualBlock;
        editor.state.views[view].selection = Some(crate::editor::Selection {
            anchor: crate::editor::text::ByteOffset(0),
            head: crate::editor::text::ByteOffset("a\u{301}b\n\n·".len()),
            kind: crate::editor::SelectionKind::Block,
        });
        editor.feed(KeyEvent::character('y')).expect("block yank");
        let register = editor.state.registers.get('"').expect("unnamed register");
        assert_eq!(register.shape, crate::editor::RegisterShape::Block);
        assert_eq!(register.text, "a\u{301}b\n\n·x");
        assert_eq!(editor.text(), "a\u{301}b\n\n·x");
    }

    #[test]
    fn blockwise_change_pads_ragged_lines_to_the_display_rectangle() {
        let mut editor = HeadlessEditor::with_text("abcd\nx\npqrs");
        let view = editor.active_view();
        editor.state.views[view].cursor = crate::editor::text::ByteOffset(10);
        editor.state.views[view].mode = crate::editor::EditorMode::VisualBlock;
        editor.state.views[view].selection = Some(crate::editor::Selection {
            anchor: crate::editor::text::ByteOffset(2),
            head: crate::editor::text::ByteOffset(10),
            kind: crate::editor::SelectionKind::Block,
        });
        editor.feed(KeyEvent::character('c')).expect("block change");
        editor.feed(KeyEvent::character('Z')).expect("insert block");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal");
        assert_eq!(editor.text(), "abZ\nx Z\npqZ");
    }

    #[test]
    fn blockwise_change_inserts_and_backspaces_across_each_selected_line() {
        let mut editor = HeadlessEditor::with_text("abc\ndef");
        editor
            .feed("ctrl-v".parse::<KeyEvent>().expect("key notation"))
            .expect("block visual");
        editor.feed(KeyEvent::character('j')).expect("second row");
        editor
            .feed(KeyEvent::character('l'))
            .expect("second column");
        editor.feed(KeyEvent::character('c')).expect("block change");
        editor.feed(KeyEvent::character('X')).expect("block X");
        editor.feed(KeyEvent::character('Y')).expect("block Y");
        assert_eq!(editor.text(), "XYc\nXYf");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Backspace))
            .expect("block backspace");
        assert_eq!(editor.text(), "Xc\nXf");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal");
        editor
            .feed(KeyEvent::character('u'))
            .expect("undo block change");
        assert_eq!(editor.text(), "abc\ndef");
    }

    #[test]
    fn configured_word_variants_arrows_and_matching_pair_move_semantically() {
        let mut editor = HeadlessEditor::with_text("aa.bb cc (x)");
        editor.feed(KeyEvent::character('W')).expect("WORD");
        assert_eq!(editor.state.views[editor.active_view()].cursor.get(), 6);
        editor.feed(KeyEvent::character('B')).expect("back WORD");
        assert_eq!(editor.state.views[editor.active_view()].cursor.get(), 0);
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Right))
            .expect("right arrow");
        assert_eq!(editor.state.views[editor.active_view()].cursor.get(), 1);
        for _ in 0..8 {
            editor
                .feed(KeyEvent::plain(crate::editor::input::KeyCode::Right))
                .expect("right");
        }
        editor.feed(KeyEvent::character('%')).expect("pair");
        assert_eq!(editor.state.views[editor.active_view()].cursor.get(), 11);
    }

    #[test]
    fn a_count_sends_g_and_gg_to_that_line() {
        let mut editor = HeadlessEditor::with_text("a\nb\nc\nd\n");
        for key in ['3', 'G', 'd', 'd'] {
            editor.feed(KeyEvent::character(key)).expect("3G dd");
        }
        assert_eq!(editor.text(), "a\nb\nd\n");
        for key in ['1', 'g', 'g', 'd', 'd'] {
            editor.feed(KeyEvent::character(key)).expect("1gg dd");
        }
        assert_eq!(editor.text(), "b\nd\n");
    }

    #[test]
    fn direct_change_family_and_indent_are_transactional() {
        let mut editor = HeadlessEditor::with_text("ab\n  cd\n");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Right))
            .expect("right");
        editor
            .feed(KeyEvent::character('X'))
            .expect("backward delete");
        assert_eq!(editor.text(), "b\n  cd\n");
        editor
            .feed(KeyEvent::character('r'))
            .expect("replace pending");
        editor.feed(KeyEvent::character('Z')).expect("replace");
        assert_eq!(editor.text(), "Z\n  cd\n");
        editor.feed(KeyEvent::character('~')).expect("toggle");
        assert_eq!(editor.text(), "z\n  cd\n");
        editor.feed(KeyEvent::character('J')).expect("join");
        assert_eq!(editor.text(), "z cd\n");
        editor
            .feed(KeyEvent::character('>'))
            .expect("indent operator");
        editor.feed(KeyEvent::character('>')).expect("indent line");
        assert_eq!(editor.text(), "    z cd\n");
        editor
            .feed(KeyEvent::character('<'))
            .expect("unindent operator");
        editor
            .feed(KeyEvent::character('<'))
            .expect("unindent line");
        assert_eq!(editor.text(), "z cd\n");
        // `s` is flash in LazyVim; `cl` substitutes.
        editor
            .feed(KeyEvent::character('c'))
            .expect("change operator");
        editor.feed(KeyEvent::character('l')).expect("substitute");
        editor
            .feed(KeyEvent::character('Q'))
            .expect("insert replacement");
        editor
            .feed(KeyEvent::plain(crate::editor::input::KeyCode::Escape))
            .expect("normal");
        assert_eq!(editor.text(), "Q cd\n");
    }

    /// Feeds `keys` one character at a time; `\u{1b}` is Escape and `\u{8}` is Backspace.
    fn edited(text: &str, keys: &str) -> String {
        let mut editor = HeadlessEditor::with_text(text);
        for character in keys.chars() {
            let key = match character {
                '\u{1b}' => KeyEvent::plain(crate::editor::input::KeyCode::Escape),
                '\u{8}' => KeyEvent::plain(crate::editor::input::KeyCode::Backspace),
                other => KeyEvent::character(other),
            };
            editor
                .feed(key)
                .unwrap_or_else(|error| panic!("key {character:?} in {keys:?}: {error}"));
        }
        editor.text()
    }

    #[test]
    fn find_motions_move_operate_and_repeat_on_the_line() {
        assert_eq!(edited("a,b,c,d", "f,;x"), "a,bc,d");
        assert_eq!(edited("a,b,c,d", "f,;,x"), "ab,c,d");
        assert_eq!(edited("abc,def", "dt,"), ",def");
        assert_eq!(edited("abc,def", "df,"), "def");
        assert_eq!(edited("abc,def", "$F,x"), "abcdef");
        assert_eq!(edited("abc,def", "$dT,"), "abc,");
        // `;` after `t` steps past the character the cursor already stands before.
        assert_eq!(edited("a,b,c", "t,;x"), "a,,c");
    }

    #[test]
    fn counted_motions_and_word_operators_use_the_bundled_key_paths() {
        assert_eq!(edited("a\nb\nc", "d2$"), "\nc");
        assert_eq!(edited("a\nb", "d$"), "\nb");
        assert_eq!(edited("a\n\nb", "jd$"), "a\n\nb");
        assert_eq!(edited("one\ntwo\nthree", "d2w"), "three");
        assert_eq!(edited("one\n  \ntwo", "dw"), "\n  \ntwo");
        assert_eq!(
            edited("one\ntwo\nthree\nfour", "50%x"),
            "one\nwo\nthree\nfour"
        );
        assert_eq!(edited("one\ntwo\nthree\nfour", "d50%"), "three\nfour");
        assert_eq!(edited("  one\ntwo", "jggx"), "  ne\ntwo");
    }

    #[test]
    fn text_objects_work_after_operators_and_in_visual_mode() {
        assert_eq!(edited("call(one, two)", "fnci(x\u{1b}"), "call(x)");
        assert_eq!(edited("say \"hello world\" now", "di\""), "say \"\" now");
        assert_eq!(edited("say \"hello world\" now", "da\""), "say  now");
        assert_eq!(edited("one two three", "wdaw"), "one three");
        assert_eq!(edited("one two", "ciwX\u{1b}"), "X two");
        assert_eq!(edited("one two", "dW"), "two");
        assert_eq!(edited("a [b (c) d] e", "fcdi]"), "a [] e");
        assert_eq!(edited("a [b (c) d] e", "fcdab"), "a [b  d] e");
        assert_eq!(edited("a\nb\n\nc\n", "dip"), "\nc\n");
        assert_eq!(edited("a\nb\n\nc\n", "dap"), "c\n");
        assert_eq!(edited("fn x() {\n    body\n}\n", "jdi{"), "fn x() {\n}\n");
        assert_eq!(edited("one two", "wviwd"), "one ");
        assert_eq!(edited("(a b)", "lva(d"), "");
    }

    #[test]
    fn linewise_motions_after_operators_cover_every_line_to_the_target() {
        assert_eq!(edited("a\nb\nc\n", "dj"), "c\n");
        assert_eq!(edited("a\nb\nc\n", "jdk"), "c\n");
        assert_eq!(edited("a\nb\nc\n", "jdG"), "a\n");
        assert_eq!(edited("a\nb\nc\n", "Gdgg"), "");
    }

    #[test]
    fn surround_adds_deletes_and_replaces_pairs() {
        assert_eq!(edited("one two", "gsaiw)"), "(one) two");
        assert_eq!(edited("one two", "gsaiw("), "( one ) two");
        assert_eq!(edited("one two", "gsae\""), "\"one\" two");
        assert_eq!(edited("one two", "viwgsa]"), "[one] two");
        assert_eq!(edited("(one) two", "lgsd)"), "one two");
        assert_eq!(edited("( one ) two", "llgsd("), "one two");
        assert_eq!(edited("'one' two", "lgsr'\""), "\"one\" two");
        assert_eq!(edited("[one] two", "lgsr]b"), "(one) two");
    }

    #[test]
    fn autopairs_close_skip_and_delete_pairs_in_insert_mode() {
        assert_eq!(edited("", "i(x\u{1b}"), "(x)");
        assert_eq!(edited("", "i()\u{1b}"), "()");
        assert_eq!(edited("", "i(\u{8}\u{1b}"), "");
        assert_eq!(edited("", "i\"a\"\u{1b}"), "\"a\"");
        assert_eq!(edited("", "iit's\u{1b}"), "it's");
        assert_eq!(edited("ab", "i(\u{1b}"), "(ab");
        assert_eq!(edited("", "i{[\u{1b}"), "{[]}");
    }

    #[test]
    fn comment_operator_toggles_lines_at_the_smallest_indent() {
        use crate::editor::CommandInvocation;
        use crate::editor::CommandValue;
        use crate::editor::InvocationSource;
        let source = "fn a() {\n    x();\n\n    y();\n}\n";
        let mut editor = HeadlessEditor::with_text(source);
        let toggle = || {
            let mut invocation = CommandInvocation::new("edit.operator", InvocationSource::Test)
                .expect("command")
                .with_argument("operator", CommandValue::String("comment".into()))
                .with_argument("motion", CommandValue::String("line".into()))
                .with_argument("linewise", CommandValue::Bool(true))
                .with_argument("comment", CommandValue::String("//".into()));
            invocation.count = std::num::NonZeroU64::new(5);
            invocation
        };
        editor.execute(toggle()).expect("comment");
        assert_eq!(
            editor.text(),
            "// fn a() {\n//     x();\n\n//     y();\n// }\n"
        );
        editor.execute(toggle()).expect("uncomment");
        assert_eq!(editor.text(), source);

        let mut editor = HeadlessEditor::with_text("    a\n    // b\n");
        let mut invocation = toggle();
        invocation.count = std::num::NonZeroU64::new(2);
        editor
            .execute(invocation)
            .expect("mixed lines are commented");
        assert_eq!(editor.text(), "    // a\n    // // b\n");
    }
}
