#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::{
    application::{AppEffect, ResolvedKeymaps, compile_keymaps, register_app_commands},
    configuration::{Config, load_bundled_config},
    document::TextBuffer,
    editing::{
        CommandError, CommandInvocation, CommandRegistry, CommandValue, EditorMode, EditorState,
        TransactionError, ViewId, register_builtin_commands,
    },
    input::{GrammarInput, InputAction, InputError, KeyEvent, MappingResolution, ModalResolver},
};

/// Deterministic editor harness that exercises bundled configuration, the same
/// registry, semantic modal grammar, transactions, and reducer without a terminal.
pub struct HeadlessEditor {
    pub state: EditorState,
    pub registry: CommandRegistry<AppEffect>,
    pub input: ModalResolver,
    keymaps: ResolvedKeymaps,
    config: Config,
}

impl HeadlessEditor {
    pub fn with_text(text: &str) -> Self {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(TextBuffer::from_text(text), None);
        let mut registry = CommandRegistry::default();
        register_builtin_commands(&mut registry).expect("built-in registry must be valid");
        register_app_commands(&mut registry).expect("application registry must be valid");
        let loaded = load_bundled_config();
        state.policy = crate::editing::EditingPolicy {
            tab_width: loaded.config.editor.tab_width,
            indent_width: loaded.config.editor.indent_width,
            expand_tabs: loaded.config.editor.expand_tabs,
            undo_break_on_text_commit: loaded.config.editor.undo_break_on_text_commit,
            registers_enabled: loaded.config.editor.registers.storage
                == crate::configuration::RegisterStorage::Internal,
            mirror_named_to_unnamed: loaded.config.editor.registers.mirror_named_to_unnamed,
            ambiguous_width_wide: loaded.config.editor.ambiguous_width
                == crate::configuration::AmbiguousWidth::Wide,
        };
        let (keymaps, diagnostic) = compile_keymaps(&registry, &loaded.keymaps);
        assert!(diagnostic.is_none(), "bundled keymaps must compile");
        Self {
            state,
            registry,
            input: ModalResolver::with_count_limit(loaded.config.input.count_limit),
            keymaps,
            config: loaded.config,
        }
    }

    pub fn active_view(&self) -> ViewId {
        self.state.active_view.expect("headless editor has a view")
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
            invocation.target.view = self.state.active_view;
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
        match self.keymaps.resolve(&context, key.clone()) {
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
            MappingResolution::Replay(replay) => {
                for key in replay {
                    self.feed(key)?;
                }
                Ok(InputAction::Pending)
            }
            MappingResolution::Missing => {
                if let Some(character) = key.text_character()
                    && let Some(command_id) = self
                        .config
                        .input
                        .character_fallback_commands
                        .get(&context)
                        .cloned()
                        .or_else(|| self.config.input.text_commands.get(&context).cloned())
                {
                    let invocation = CommandInvocation::new(
                        &command_id,
                        crate::editing::InvocationSource::Keymap,
                    )?
                    .with_argument("text", CommandValue::String(character.to_string()));
                    self.execute(invocation)?;
                    return Ok(InputAction::Text(character.to_string()));
                }
                Ok(InputAction::Unmapped)
            }
            MappingResolution::Disabled
            | MappingResolution::Pending
            | MappingResolution::Ambiguous => Ok(InputAction::Pending),
        }
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
                crate::input::PendingInputContext::Register => "register",
                crate::input::PendingInputContext::Prefix => "prefix",
                crate::input::PendingInputContext::Replace => "replace",
                crate::input::PendingInputContext::MacroRecord => "macro_record",
                crate::input::PendingInputContext::MacroReplay => "macro_replay",
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
    use crate::input::KeyEvent;

    use super::HeadlessEditor;

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
    fn insert_backspace_and_line_operator_use_registry_transactions() {
        let mut editor = HeadlessEditor::with_text("one\ntwo\n");
        editor.feed(KeyEvent::character('i')).expect("insert mode");
        editor.feed(KeyEvent::character('X')).expect("insert text");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Backspace))
            .expect("backspace");
        assert_eq!(editor.text(), "one\ntwo\n");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Escape))
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
            .feed(KeyEvent::plain(crate::input::KeyCode::Escape))
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
            .feed(KeyEvent::plain(crate::input::KeyCode::Escape))
            .expect("normal");
        editor.feed(KeyEvent::character('u')).expect("undo commit");
        assert_eq!(editor.text(), "Abase");
    }

    #[test]
    fn replace_mode_restores_replaced_text_and_undoes_as_one_session() {
        let mut editor = HeadlessEditor::with_text("abc");
        editor.feed(KeyEvent::character('R')).expect("replace mode");
        editor.feed(KeyEvent::character('X')).expect("replace a");
        editor.feed(KeyEvent::character('Y')).expect("replace b");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Backspace))
            .expect("restore b");
        assert_eq!(editor.text(), "Xbc");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Escape))
            .expect("normal mode");
        editor.feed(KeyEvent::character('u')).expect("undo replace");
        assert_eq!(editor.text(), "abc");

        editor.feed(KeyEvent::character('R')).expect("replace mode");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Enter))
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
        editor.state.views[view].cursor = crate::document::ByteOffset("\tZ\n界ab".len());
        editor.state.views[view].mode = crate::editing::EditorMode::VisualBlock;
        editor.state.views[view].selection = Some(crate::editing::Selection {
            anchor: crate::document::ByteOffset("\tZ\n界ab".len()),
            head: crate::document::ByteOffset(0),
            kind: crate::editing::SelectionKind::Block,
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
        editor.state.views[view].cursor = crate::document::ByteOffset("a\u{301}b\n\n·".len());
        editor.state.views[view].mode = crate::editing::EditorMode::VisualBlock;
        editor.state.views[view].selection = Some(crate::editing::Selection {
            anchor: crate::document::ByteOffset(0),
            head: crate::document::ByteOffset("a\u{301}b\n\n·".len()),
            kind: crate::editing::SelectionKind::Block,
        });
        editor.feed(KeyEvent::character('y')).expect("block yank");
        let register = editor.state.registers.get('"').expect("unnamed register");
        assert_eq!(register.shape, crate::editing::RegisterShape::Block);
        assert_eq!(register.text, "a\u{301}b\n\n·x");
        assert_eq!(editor.text(), "a\u{301}b\n\n·x");
    }

    #[test]
    fn blockwise_change_pads_ragged_lines_to_the_display_rectangle() {
        let mut editor = HeadlessEditor::with_text("abcd\nx\npqrs");
        let view = editor.active_view();
        editor.state.views[view].cursor = crate::document::ByteOffset(10);
        editor.state.views[view].mode = crate::editing::EditorMode::VisualBlock;
        editor.state.views[view].selection = Some(crate::editing::Selection {
            anchor: crate::document::ByteOffset(2),
            head: crate::document::ByteOffset(10),
            kind: crate::editing::SelectionKind::Block,
        });
        editor.feed(KeyEvent::character('c')).expect("block change");
        editor.feed(KeyEvent::character('Z')).expect("insert block");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Escape))
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
            .feed(KeyEvent::plain(crate::input::KeyCode::Backspace))
            .expect("block backspace");
        assert_eq!(editor.text(), "Xc\nXf");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Escape))
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
            .feed(KeyEvent::plain(crate::input::KeyCode::Right))
            .expect("right arrow");
        assert_eq!(editor.state.views[editor.active_view()].cursor.get(), 1);
        for _ in 0..8 {
            editor
                .feed(KeyEvent::plain(crate::input::KeyCode::Right))
                .expect("right");
        }
        editor.feed(KeyEvent::character('%')).expect("pair");
        assert_eq!(editor.state.views[editor.active_view()].cursor.get(), 11);
    }

    #[test]
    fn direct_change_family_and_indent_are_transactional() {
        let mut editor = HeadlessEditor::with_text("ab\n  cd\n");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Right))
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
        editor.feed(KeyEvent::character('s')).expect("substitute");
        editor
            .feed(KeyEvent::character('Q'))
            .expect("insert replacement");
        editor
            .feed(KeyEvent::plain(crate::input::KeyCode::Escape))
            .expect("normal");
        assert_eq!(editor.text(), "Q cd\n");
    }
}
