//! Vim editing in the screen's text boxes, on the editor's own engine.
//!
//! When the editor is modal, a box (a form field: the commit message, a comment, a pull
//! request's title and description, a filter query, a review summary) is edited the way a
//! buffer is: the editor's bundled `editor` key bindings resolve the keys, its Vim grammar
//! (`ModalResolver`) turns counts, operators, motions and text objects into commands, and its
//! built-in editing commands change the text through an `EditorState` that holds just this
//! box. Nothing here is a second Vim: whatever the editor's engine learns, the boxes do too.
//! Commands that need the whole editor (save, pickers, `.` repeat, macros) are not bound here.
use crate::editor::input::{
    GrammarInput, InputAction, KeyCode, KeyEvent, Keymap, KeymapSequence, MappingResolution,
    ModalResolver, PendingInputContext,
};
use crate::editor::text::{ByteOffset, TextBuffer};
use crate::editor::{
    BufferId, CommandInvocation, CommandRegistry, CommandValue, EditorMode, EditorState,
    InvocationSource, RegisterStore, SelectionKind, ViewId, register_builtin_commands,
};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::OnceLock;

/// The editor's bundled bindings for the modes a box has, keyed by mode name; `prefix` holds
/// the keys that finish a pending prefix.
fn bindings() -> &'static BTreeMap<String, Keymap> {
    static BINDINGS: OnceLock<BTreeMap<String, Keymap>> = OnceLock::new();
    BINDINGS.get_or_init(|| {
        let mut registry = CommandRegistry::<()>::default();
        // The bundled commands always register; an empty set only leaves the grammar bound.
        let _ = register_builtin_commands(&mut registry);
        let builtin = registry
            .specs()
            .map(|spec| spec.id.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let mut modes = BTreeMap::<String, Keymap>::new();
        for entry in crate::config::load_bundled_config().keymaps.entries {
            let mode = match entry.context.as_deref() {
                Some("editor")
                    if matches!(
                        entry.mode.as_str(),
                        "normal"
                            | "insert"
                            | "replace"
                            | "operator_pending"
                            | "visual_char"
                            | "visual_line"
                            | "visual_block"
                    ) =>
                {
                    entry.mode.clone()
                }
                Some("prefix") => "prefix".to_owned(),
                _ => continue,
            };
            let (Some(keys), Some(command)) = (entry.keys.as_deref(), entry.command.as_deref())
            else {
                continue;
            };
            if entry.unmap
                || entry.disable
                || !(command.starts_with("input.") || builtin.contains(command))
            {
                continue;
            }
            // Leader chords are the editor's own commands, never text editing.
            let Ok(sequence) = keys
                .split_whitespace()
                .map(KeyEvent::from_str)
                .collect::<Result<Vec<_>, _>>()
            else {
                continue;
            };
            let Ok(mut invocation) = CommandInvocation::new(command, InvocationSource::Keymap)
            else {
                continue;
            };
            for (name, value) in &entry.arguments {
                let value = match value {
                    toml::Value::String(text) => CommandValue::String(text.clone()),
                    toml::Value::Boolean(flag) => CommandValue::Bool(*flag),
                    toml::Value::Integer(number) => CommandValue::Integer(*number),
                    _ => continue,
                };
                invocation = invocation.with_argument(name, value);
            }
            modes.entry(mode).or_default().insert(&sequence, invocation);
        }
        modes
    })
}

/// The keys the editor's bundled bindings give `command` in `mode`, as the keys sheet shows
/// them (`i`, `Esc`, `C-v`), in the bindings' order, each once.
pub(super) fn keys_for(mode: &str, command: &str) -> Vec<String> {
    static ENTRIES: OnceLock<Vec<(String, String, String)>> = OnceLock::new();
    let entries = ENTRIES.get_or_init(|| {
        crate::config::load_bundled_config()
            .keymaps
            .entries
            .into_iter()
            .filter(|entry| {
                entry.context.as_deref() == Some("editor") && !entry.unmap && !entry.disable
            })
            .filter_map(|entry| Some((entry.mode, entry.keys?, entry.command?)))
            .collect()
    });
    let shown = |keys: &str| {
        keys.split_whitespace()
            .map(|token| match token {
                "escape" => "Esc".to_owned(),
                "space" => "Space".to_owned(),
                "enter" => "Enter".to_owned(),
                token => match token.strip_prefix("ctrl-") {
                    Some(rest) => format!("C-{rest}"),
                    None => token.to_owned(),
                },
            })
            .collect::<String>()
    };
    let mut found = Vec::<String>::new();
    for (_, keys, _) in entries.iter().filter(|(m, _, c)| m == mode && c == command) {
        let keys = shown(keys);
        if !found.contains(&keys) {
            found.push(keys);
        }
    }
    found
}

fn mode_name(mode: EditorMode) -> &'static str {
    match mode {
        EditorMode::Insert => "insert",
        EditorMode::Replace => "replace",
        EditorMode::VisualChar => "visual_char",
        EditorMode::VisualLine => "visual_line",
        EditorMode::VisualBlock => "visual_block",
        EditorMode::OperatorPending => "operator_pending",
        _ => "normal",
    }
}

/// The editor's `input.*` commands are its grammar's inputs (`app::commands`).
fn grammar_input(invocation: &CommandInvocation) -> Option<GrammarInput> {
    let text = |name: &str| {
        invocation
            .arguments
            .get(name)
            .and_then(|value| value.as_str().ok())
            .map(str::to_owned)
    };
    let flag = |name: &str| {
        invocation
            .arguments
            .get(name)
            .and_then(|value| value.as_bool().ok())
            .unwrap_or(false)
    };
    Some(match invocation.id.as_str() {
        "input.cancel" => GrammarInput::Cancel,
        "input.zero" => GrammarInput::Zero,
        "input.register.begin" => GrammarInput::RegisterBegin,
        "input.prefix.begin" => GrammarInput::PrefixBegin,
        "input.replace.begin" => GrammarInput::ReplaceBegin,
        "input.macro.record.toggle" => GrammarInput::MacroRecordToggle,
        "input.macro.replay.begin" => GrammarInput::MacroReplayBegin,
        "input.count" => GrammarInput::Count(
            u8::try_from(invocation.arguments.get("digit")?.as_integer().ok()?).ok()?,
        ),
        "input.register.select" => GrammarInput::RegisterSelect(text("text")?),
        "input.replace.text" => GrammarInput::ReplaceText(text("text")?),
        "input.character" => GrammarInput::Character(text("text")?),
        "input.find.begin" => GrammarInput::FindBegin(text("text")?),
        "input.mark.begin" => GrammarInput::MarkBegin(text("text")?),
        "input.surround.begin" => GrammarInput::SurroundBegin(text("text")?),
        "input.macro.record.select" => GrammarInput::MacroRecordSelect(text("text")?),
        "input.macro.replay.select" => GrammarInput::MacroReplaySelect(text("text")?),
        "input.find.repeat" => GrammarInput::FindRepeat { reverse: false },
        "input.find.reverse" => GrammarInput::FindRepeat { reverse: true },
        "input.operator.begin" => GrammarInput::OperatorBegin(text("operator")?),
        "input.operator.repeat" => GrammarInput::OperatorRepeat(text("operator")?),
        "input.motion" => GrammarInput::Motion {
            motion: text("motion")?,
            linewise: flag("linewise"),
        },
        "input.visual.operator" => GrammarInput::VisualOperator {
            operator: text("operator")?,
            linewise: flag("linewise"),
        },
        _ => return None,
    })
}

fn plain(key: &KeyEvent) -> bool {
    !key.modifiers.control && !key.modifiers.alt
}

/// One box's text on the editor's engine, from the moment it gets focus until it loses it.
pub(super) struct TextBox {
    state: EditorState,
    buffer: BufferId,
    view: ViewId,
    registry: CommandRegistry<()>,
    grammar: ModalResolver,
    sequence: KeymapSequence,
    /// The field this box edits: the form's title, its field count and the field's index.
    pub(super) owner: (String, usize, usize),
    /// The text the box last gave its field, so a change from elsewhere (a paste, a chosen
    /// template) is noticed and the box starts again from the field.
    pub(super) synced: String,
}

impl TextBox {
    /// A box over `text` with the cursor at byte `cursor`, in Insert or Normal mode. The
    /// registers carry yanks from one box to the next.
    pub(super) fn open(
        text: &str,
        cursor: usize,
        insert: bool,
        registers: RegisterStore,
        owner: (String, usize, usize),
    ) -> Self {
        let mut state = EditorState::new(std::path::PathBuf::new());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text(text), None)
            .expect("a new editor state takes a scratch buffer");
        state.registers = registers;
        let mut cursor = cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        state.views[view].cursor = ByteOffset(cursor);
        let mut registry = CommandRegistry::<()>::default();
        let _ = register_builtin_commands(&mut registry);
        let mut text_box = Self {
            state,
            buffer,
            view,
            registry,
            grammar: ModalResolver::default(),
            sequence: KeymapSequence::default(),
            owner,
            synced: text.to_owned(),
        };
        if insert
            && let Ok(invocation) = CommandInvocation::new("mode.insert", InvocationSource::Keymap)
        {
            text_box.execute(
                invocation.with_argument("placement", CommandValue::String("before".into())),
                0,
            );
        }
        text_box
    }

    /// The mode as the status chip shows it: an operator waiting for its motion counts as
    /// Normal there, as in the editor's statusline.
    pub(super) fn mode(&self) -> EditorMode {
        self.state.views[self.view].mode
    }

    /// No count, operator, register or half-typed key sequence is waiting.
    pub(super) fn idle(&self) -> bool {
        !self.grammar.has_pending_grammar() && self.sequence.pending_sequence().is_empty()
    }

    pub(super) fn text(&self) -> String {
        self.state.buffers[self.buffer].text.to_string()
    }

    pub(super) fn cursor(&self) -> usize {
        self.state.views[self.view].cursor.0
    }

    /// Moves the cursor to byte `at` (a key the form handled itself, such as Home or End).
    pub(super) fn set_cursor(&mut self, at: usize) {
        let text = self.text();
        let mut at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        self.state.views[self.view].cursor = ByteOffset(at);
    }

    pub(super) fn registers(&self) -> &RegisterStore {
        &self.state.registers
    }

    /// The Visual selection as a byte range of the text, for the box to paint.
    pub(super) fn selection(&self) -> Option<std::ops::Range<usize>> {
        let selection = self.state.views[self.view].selection?;
        if !matches!(
            self.mode(),
            EditorMode::VisualChar | EditorMode::VisualLine | EditorMode::VisualBlock
        ) {
            return None;
        }
        let text = self.text();
        let low = selection.anchor.0.min(selection.head.0).min(text.len());
        let high = selection.anchor.0.max(selection.head.0).min(text.len());
        Some(match selection.kind {
            SelectionKind::Line => {
                let start = text[..low].rfind('\n').map_or(0, |at| at + 1);
                let end = text[high..].find('\n').map_or(text.len(), |at| high + at);
                start..end
            }
            _ => {
                let end = text[high..]
                    .chars()
                    .next()
                    .map_or(high, |next| high + next.len_utf8());
                low..end
            }
        })
    }

    /// One key, as the editor reads it in this mode.
    pub(super) fn key(&mut self, key: &KeyEvent) {
        if let Some(context) = self.grammar.pending_context() {
            self.sequence.cancel();
            let character = match key.code {
                KeyCode::Char(c) if plain(key) => Some(c.to_string()),
                _ => None,
            };
            let input = match (context, character) {
                (_, None) => GrammarInput::Cancel,
                (PendingInputContext::Prefix, Some(_)) => {
                    let resolution = bindings()
                        .get("prefix")
                        .map_or(MappingResolution::Missing, |prefix| {
                            self.sequence.feed_layers([prefix], key.clone())
                        });
                    match resolution {
                        MappingResolution::Ready(invocation) => {
                            self.dispatch(invocation);
                            return;
                        }
                        _ => {
                            self.sequence.cancel();
                            GrammarInput::Cancel
                        }
                    }
                }
                (PendingInputContext::Register, Some(text)) => GrammarInput::RegisterSelect(text),
                (PendingInputContext::Replace, Some(text)) => GrammarInput::ReplaceText(text),
                (PendingInputContext::Character, Some(text)) => GrammarInput::Character(text),
                (PendingInputContext::MacroRecord, Some(text)) => {
                    GrammarInput::MacroRecordSelect(text)
                }
                (PendingInputContext::MacroReplay, Some(text)) => {
                    GrammarInput::MacroReplaySelect(text)
                }
            };
            self.feed(input);
            return;
        }
        let mode = mode_name(self.grammar.effective_mode(self.mode()));
        let resolution = bindings()
            .get(mode)
            .map_or(MappingResolution::Missing, |keymap| {
                self.sequence.feed_layers([keymap], key.clone())
            });
        match resolution {
            MappingResolution::Ready(invocation) => self.dispatch(invocation),
            MappingResolution::ReadyAndReplay(invocation, rest) => {
                self.dispatch(invocation);
                for key in rest {
                    self.key(&key);
                }
            }
            MappingResolution::Missing => self.unmapped(key),
            // No timeout in a box: a sequence that went nowhere is typed (Insert) or dropped.
            MappingResolution::Replay(started, last) => {
                for key in started.iter().chain(&last) {
                    self.unmapped(key);
                }
            }
            MappingResolution::Pending
            | MappingResolution::Ambiguous
            | MappingResolution::Disabled
            | MappingResolution::DisabledAndReplay(_) => {}
        }
    }

    /// Typed text, or nothing: an unbound key in Normal mode only drops what was pending.
    fn unmapped(&mut self, key: &KeyEvent) {
        let typing = matches!(self.mode(), EditorMode::Insert | EditorMode::Replace);
        match key.code {
            KeyCode::Char(c) if typing && plain(key) => self.type_text(&c.to_string()),
            _ => self.grammar.cancel_pending(),
        }
    }

    /// Text typed or pasted at the cursor, as Insert mode types it.
    pub(super) fn type_text(&mut self, text: &str) {
        let id = if self.mode() == EditorMode::Replace {
            "edit.replace_text"
        } else {
            "edit.insert_text"
        };
        if let Ok(invocation) = CommandInvocation::new(id, InvocationSource::Keymap) {
            self.execute(
                invocation.with_argument("text", CommandValue::String(text.to_owned())),
                0,
            );
        }
    }

    fn dispatch(&mut self, mut invocation: CommandInvocation) {
        if let Some(input) = grammar_input(&invocation) {
            self.feed(input);
        } else {
            self.grammar.decorate_invocation(&mut invocation);
            self.execute(invocation, 0);
        }
    }

    fn feed(&mut self, input: GrammarInput) {
        match self.grammar.feed(self.mode(), input) {
            Ok(InputAction::Invoke(invocation)) => self.execute(invocation, 0),
            Ok(InputAction::Text(text)) => self.type_text(&text),
            _ => {}
        }
    }

    fn execute(&mut self, mut invocation: CommandInvocation, depth: usize) {
        if depth > 8 {
            return;
        }
        invocation.target.view = Some(self.view);
        invocation.target.buffer = Some(self.buffer);
        let Ok(outcome) = self.registry.execute(&self.state, &invocation) else {
            return;
        };
        if let Some(transaction) = outcome.transaction
            && self.state.apply_transaction(&transaction).is_err()
        {
            return;
        }
        for next in outcome.follow_up {
            self.execute(next, depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text_box: &mut TextBox, keys: &str) {
        for token in keys.split_whitespace() {
            text_box.key(&KeyEvent::from_str(token).unwrap());
        }
    }

    fn open(text: &str, cursor: usize, insert: bool) -> TextBox {
        TextBox::open(
            text,
            cursor,
            insert,
            RegisterStore::default(),
            (String::new(), 1, 0),
        )
    }

    #[test]
    fn insert_then_normal_motions_and_operators_edit_through_the_editor_engine() {
        let mut text_box = open("", 0, true);
        assert_eq!(text_box.mode(), EditorMode::Insert);
        keys(&mut text_box, "h e l l o space w o r l d");
        assert_eq!(text_box.text(), "hello world");
        keys(&mut text_box, "escape");
        assert_eq!(text_box.mode(), EditorMode::Normal);
        keys(&mut text_box, "0");
        assert_eq!(text_box.cursor(), 0);
        keys(&mut text_box, "w");
        assert_eq!(text_box.cursor(), 6);
        keys(&mut text_box, "c i w t h e r e escape");
        assert_eq!(text_box.text(), "hello there");
        keys(&mut text_box, "0 x");
        assert_eq!(text_box.text(), "ello there");
        keys(&mut text_box, "u");
        assert_eq!(text_box.text(), "hello there");
        keys(&mut text_box, "$");
        assert_eq!(text_box.cursor(), "hello ther".len());
        keys(&mut text_box, "b");
        assert_eq!(text_box.cursor(), 6);
        keys(&mut text_box, "e");
        assert_eq!(text_box.cursor(), "hello ther".len());
    }

    #[test]
    fn delete_line_yank_and_put_use_the_boxes_registers() {
        let mut text_box = open("one\ntwo", 0, false);
        keys(&mut text_box, "d d");
        assert_eq!(text_box.text(), "two");
        // The editor's linewise put after a last line without a newline ends with one.
        keys(&mut text_box, "p");
        assert_eq!(text_box.text().trim_end_matches('\n'), "two\none");
        let mut next = TextBox::open(
            "x",
            0,
            false,
            text_box.registers().clone(),
            (String::new(), 1, 0),
        );
        keys(&mut next, "p");
        assert_eq!(next.text().trim_end_matches('\n'), "x\none");
    }

    #[test]
    fn visual_mode_selects_and_deletes() {
        let mut text_box = open("alpha beta", 0, false);
        keys(&mut text_box, "v e");
        assert_eq!(text_box.mode(), EditorMode::VisualChar);
        assert_eq!(text_box.selection(), Some(0..5));
        keys(&mut text_box, "d");
        assert_eq!(text_box.text(), " beta");
        assert_eq!(text_box.mode(), EditorMode::Normal);
    }

    #[test]
    fn upper_d_c_and_y_run_to_the_end_of_the_line() {
        let mut text_box = open("hello world", 5, false);
        keys(&mut text_box, "D");
        assert_eq!(text_box.text(), "hello");
        assert_eq!(text_box.mode(), EditorMode::Normal);
        keys(&mut text_box, "0 l C");
        assert_eq!(text_box.text(), "h");
        assert_eq!(text_box.mode(), EditorMode::Insert);
        let mut text_box = open("one two", 4, false);
        keys(&mut text_box, "Y 0 P");
        assert_eq!(text_box.text(), "twoone two");
    }

    #[test]
    fn boxes_get_the_editors_gi_gv_and_tag_objects() {
        // `cit` on the nearest tag pair; the box has no parser, so the text decides.
        let mut text_box = open("see <b>this</b> now", 8, false);
        keys(&mut text_box, "c i t t h a t escape");
        assert_eq!(text_box.text(), "see <b>that</b> now");
        // `gi` types again where Insert mode stopped, wherever the cursor went.
        keys(&mut text_box, "0 g i !");
        assert_eq!(text_box.text(), "see <b>that!</b> now");
        assert_eq!(text_box.mode(), EditorMode::Insert);
        // `gv` brings back the last Visual selection.
        keys(&mut text_box, "escape 0 v e escape $ g v");
        assert_eq!(text_box.mode(), EditorMode::VisualChar);
        keys(&mut text_box, "d");
        assert_eq!(text_box.text(), " <b>that!</b> now");
    }

    #[test]
    fn a_pending_operator_is_not_idle_and_escape_cancels_it() {
        let mut text_box = open("word", 0, false);
        assert!(text_box.idle());
        keys(&mut text_box, "d");
        assert!(!text_box.idle());
        keys(&mut text_box, "escape");
        assert!(text_box.idle());
        assert_eq!(text_box.text(), "word");
    }
}
