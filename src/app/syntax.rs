//! Syntax requests and accepted buffer highlights.

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use crate::config::SyntaxConfig;

use crate::editor::BufferId;

use crate::editor::text::BufferRevision;

use crate::editor::text::TextBuffer;

use crate::editor::text::TextVersion;

use crate::language::syntax::SyntaxDocumentId;

use crate::language::syntax::SyntaxEvent;

use crate::language::syntax::SyntaxLanguage;

use crate::language::syntax::SyntaxPolicy;

use crate::language::syntax::SyntaxResult;

use crate::language::syntax::SyntaxWorker;

use crate::language::syntax::SyntaxSupport;

pub(in crate::app) const SYNTAX_QUEUE_CAPACITY: usize = 8;
/// How long typing must pause before a buffer is parsed again.
const SYNTAX_DEBOUNCE_MS: u64 = 75;
/// How long a newly opened buffer's first frame waits for its colours and inlay hints before
/// it is drawn without them.
const FIRST_FRAME_WAIT_MS: u64 = 200;

pub(super) struct SyntaxDocument<'a> {
    pub buffer: BufferId,
    pub text: &'a TextBuffer,
    pub language: Option<SyntaxLanguage>,
    pub enabled: bool,
}

struct SyntaxFailure {
    revision: BufferRevision,
    request_id: u64,
    language: SyntaxLanguage,
    diagnostic: String,
}

#[derive(Debug, PartialEq)]
pub(super) enum IndentError {
    Pending,
    Unsupported,
    Truncated,
}

/// Owns parser requests, document identities, debounce and accepted results.
pub(super) struct Syntax {
    service: SyntaxWorker,
    versions: HashMap<BufferId, TextVersion>,
    documents: HashMap<BufferId, SyntaxDocumentId>,
    submitted: HashMap<BufferId, (BufferRevision, u64, SyntaxLanguage)>,
    accepted: HashMap<BufferId, (TextVersion, SyntaxResult)>,
    failures: HashMap<BufferId, SyntaxFailure>,
    due: HashMap<BufferId, (BufferRevision, Instant)>,
    opened: HashMap<BufferId, Instant>,
    next_document: u64,
    next_request_id: u64,
    tab_width: usize,
}

impl Syntax {
    pub(in crate::app) fn new(queue_capacity: usize) -> Self {
        Self {
            service: SyntaxWorker::start(queue_capacity),
            versions: HashMap::new(),
            documents: HashMap::new(),
            submitted: HashMap::new(),
            accepted: HashMap::new(),
            failures: HashMap::new(),
            due: HashMap::new(),
            opened: HashMap::new(),
            next_document: 1,
            next_request_id: 1,
            tab_width: 0,
        }
    }

    pub(in crate::app) fn close(&mut self, buffer: BufferId) {
        self.versions.remove(&buffer);
        if let Some(document) = self.documents.remove(&buffer) {
            self.service.cancel(document);
        }
        self.submitted.remove(&buffer);
        self.accepted.remove(&buffer);
        self.failures.remove(&buffer);
        self.due.remove(&buffer);
        self.opened.remove(&buffer);
    }

    /// Returns how many source strings were materialized, for allocation checks.
    /// No source snapshot is created before revision, debounce and queue reservation succeed.
    pub(in crate::app) fn tick(
        &mut self,
        documents: &[SyntaxDocument<'_>],
        config: &SyntaxConfig,
        tab_width: usize,
        now: Instant,
        too_large: &str,
    ) -> usize {
        if self.tab_width != tab_width {
            self.submitted.clear();
            self.accepted.clear();
            self.failures.clear();
            self.due.clear();
            self.tab_width = tab_width;
        }
        for doc in documents {
            let source = doc.text.source_version();
            if self
                .versions
                .get(&doc.buffer)
                .is_some_and(|previous| !previous.same_instance(&source))
            {
                self.close(doc.buffer);
            } else if self
                .versions
                .get(&doc.buffer)
                .is_some_and(|previous| *previous != source)
            {
                self.submitted.remove(&doc.buffer);
                self.failures.remove(&doc.buffer);
            }
            self.versions.insert(doc.buffer, source);
        }
        let live = documents
            .iter()
            .filter_map(|doc| {
                (config.enabled && doc.enabled)
                    .then_some(doc.language)
                    .flatten()
                    .map(|language| (doc.buffer, (doc.text.revision(), language)))
            })
            .collect::<HashMap<_, _>>();
        let removed = self
            .documents
            .keys()
            .chain(self.due.keys())
            .chain(self.versions.keys())
            .filter(|id| !live.contains_key(id))
            .copied()
            .collect::<Vec<_>>();
        for buffer in removed {
            self.close(buffer);
        }
        // An older parse stays until the next one lands, so editing doesn't blank the colours;
        // anything that needs the exact text checks the revision itself.
        self.accepted.retain(|buffer, (_, result)| {
            live.get(buffer)
                .is_some_and(|(_, language)| *language == result.language)
        });
        self.failures.retain(|buffer, failure| {
            live.get(buffer) == Some(&(failure.revision, failure.language))
                && self.submitted.get(buffer)
                    == Some(&(failure.revision, failure.request_id, failure.language))
        });
        let mut materialized = 0;
        for doc in documents {
            let Some(&(revision, language)) = live.get(&doc.buffer) else {
                continue;
            };
            if self
                .submitted
                .get(&doc.buffer)
                .is_some_and(|(previous, _, previous_language)| {
                    *previous == revision && *previous_language == language
                })
            {
                continue;
            }
            // Only edits wait for typing to pause; a buffer that was just opened parses now.
            let delay = if self.documents.contains_key(&doc.buffer) {
                Duration::from_millis(SYNTAX_DEBOUNCE_MS)
            } else {
                Duration::ZERO
            };
            let due = self
                .due
                .entry(doc.buffer)
                .or_insert((revision, now + delay));
            if due.0 != revision {
                *due = (revision, now + delay);
                continue;
            }
            if now < due.1 {
                continue;
            }
            let document = *self.documents.entry(doc.buffer).or_insert_with(|| {
                self.opened.insert(doc.buffer, now);
                let id = SyntaxDocumentId(self.next_document);
                self.next_document = self.next_document.saturating_add(1);
                id
            });
            let request_id = self.next_request_id;
            self.next_request_id = self.next_request_id.saturating_add(1);
            let policy = SyntaxPolicy {
                maximum_source_bytes: 8 * 1024 * 1024,
                maximum_spans: 100_000,
                maximum_folds: 10_000,
                maximum_indent_rows: 100_000,
                tab_width,
            };
            if self
                .service
                .submit(
                    document,
                    request_id,
                    revision.0,
                    language,
                    || {
                        materialized += 1;
                        doc.text.to_string()
                    },
                    policy,
                )
                .is_ok()
            {
                self.submitted
                    .insert(doc.buffer, (revision, request_id, language));
                self.due.remove(&doc.buffer);
            } else {
                break;
            }
        }
        while let Ok(Some(event)) = self.service.try_event() {
            self.accept_event(event, &live, too_large);
        }
        materialized
    }

    pub(in crate::app) fn accept_event(
        &mut self,
        event: SyntaxEvent,
        live: &HashMap<BufferId, (BufferRevision, SyntaxLanguage)>,
        too_large: &str,
    ) {
        let (document, revision, request_id, language) = match &event {
            SyntaxEvent::Parsed(result) => (
                result.document,
                result.revision,
                result.request_id,
                result.language,
            ),
            SyntaxEvent::Failed {
                document,
                revision,
                request_id,
                language,
                ..
            }
            | SyntaxEvent::Skipped {
                document,
                revision,
                request_id,
                language,
            } => (*document, *revision, *request_id, *language),
        };
        let Some((&buffer, _)) = self
            .documents
            .iter()
            .find(|(_, candidate)| **candidate == document)
        else {
            return;
        };
        if self.submitted.get(&buffer) != Some(&(BufferRevision(revision), request_id, language))
            || live.get(&buffer) != Some(&(BufferRevision(revision), language))
        {
            return;
        }
        match event {
            SyntaxEvent::Parsed(result) => {
                self.failures.remove(&buffer);
                let Some(source) = self.versions.get(&buffer) else {
                    return;
                };
                self.accepted.insert(buffer, (source.clone(), result));
            }
            SyntaxEvent::Failed { diagnostic, .. } => {
                self.accepted.remove(&buffer);
                self.failures.insert(
                    buffer,
                    SyntaxFailure {
                        revision: BufferRevision(revision),
                        request_id,
                        language,
                        diagnostic,
                    },
                );
            }
            SyntaxEvent::Skipped { .. } => {
                self.accepted.remove(&buffer);
                self.failures.insert(
                    buffer,
                    SyntaxFailure {
                        revision: BufferRevision(revision),
                        request_id,
                        language,
                        diagnostic: too_large.to_owned(),
                    },
                );
            }
        }
    }

    pub(in crate::app) fn result(&self, buffer: BufferId) -> Option<&SyntaxResult> {
        self.accepted.get(&buffer).map(|(_, result)| result)
    }
    pub(in crate::app) fn source(&self, buffer: BufferId) -> Option<&TextVersion> {
        self.accepted.get(&buffer).map(|(source, _)| source)
    }
    pub(super) fn results(&self) -> impl Iterator<Item = (&BufferId, &SyntaxResult)> {
        self.accepted
            .iter()
            .map(|(buffer, (_, result))| (buffer, result))
    }
    /// Whether a buffer opened moments ago hasn't had its first frame drawn, so that frame can
    /// wait for colours and hints instead of showing plain text that fills in a moment later.
    pub(super) fn first_frame_pending(&self, buffer: BufferId, now: Instant) -> bool {
        self.opened
            .get(&buffer)
            .is_some_and(|opened| now < *opened + Duration::from_millis(FIRST_FRAME_WAIT_MS))
    }
    pub(super) fn first_frame_shown(&mut self, buffer: BufferId) {
        self.opened.remove(&buffer);
    }
    pub(super) fn colours_pending(&self, buffer: BufferId) -> bool {
        !self.accepted.contains_key(&buffer) && !self.failures.contains_key(&buffer)
    }
    pub(in crate::app) fn failure(&self, buffer: BufferId) -> Option<&str> {
        self.failures
            .get(&buffer)
            .map(|failure| failure.diagnostic.as_str())
    }

    pub(super) fn indentation(
        &self,
        buffer: BufferId,
        revision: BufferRevision,
        language: SyntaxLanguage,
        line: usize,
    ) -> Result<usize, IndentError> {
        if self.source(buffer) != self.versions.get(&buffer) {
            return Err(IndentError::Pending);
        }
        let result = self
            .result(buffer)
            .filter(|result| result.revision == revision.0 && result.language == language)
            .ok_or(IndentError::Pending)?;
        if result.capabilities.indents == SyntaxSupport::Unsupported {
            return Err(IndentError::Unsupported);
        }
        if result.indents_truncated {
            return Err(IndentError::Truncated);
        }
        Ok(result
            .indents
            .iter()
            .filter(|range| line > range.start_row && line < range.end_row)
            .count())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::syntax::*;
    use slotmap::SlotMap;

    #[test]
    fn every_accepted_parse_event_is_delivered() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let text = TextBuffer::from_text("fn main() {}");
        let documents = (0..12)
            .map(|_| SyntaxDocument {
                buffer: ids.insert(()),
                text: &text,
                language: Some(SyntaxLanguage::Rust),
                enabled: true,
            })
            .collect::<Vec<_>>();
        let config = SyntaxConfig::default();
        let mut session = Syntax::new(2);
        let start = Instant::now();
        session.tick(&documents, &config, 4, start, "large");
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.results().count() < documents.len() {
            session.tick(
                &documents,
                &config,
                4,
                start + Duration::from_millis(100),
                "large",
            );
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn block_end_rows_keep_indentation_when_a_statement_precedes_the_closer() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let config = SyntaxConfig::default();
        for (source, levels) in [
            ("fn f() {\n    foo(); }\n", 1),
            ("fn f() {\n}\n", 0),
            ("fn f() {\n    if true {\n    } foo(); }\n", 1),
        ] {
            let text = TextBuffer::from_text(source);
            let mut session = Syntax::new(1);
            let start = Instant::now();
            let documents = [SyntaxDocument {
                buffer,
                text: &text,
                language: Some(SyntaxLanguage::Rust),
                enabled: true,
            }];
            session.tick(&documents, &config, 4, start, "large");
            let deadline = Instant::now() + Duration::from_secs(5);
            while session.result(buffer).is_none() {
                session.tick(
                    &documents,
                    &config,
                    4,
                    start + Duration::from_millis(100),
                    "large",
                );
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            let line = if source.contains("if true") { 2 } else { 1 };
            assert_eq!(
                session.indentation(buffer, text.revision(), SyntaxLanguage::Rust, line),
                Ok(levels),
                "{source}"
            );
        }
    }

    #[test]
    fn changing_tab_width_reparses_indentation_folds() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let text = TextBuffer::from_text("void f() {\n    x;\n\ty;\n}\n");
        let documents = [SyntaxDocument {
            buffer,
            text: &text,
            language: Some(SyntaxLanguage::C),
            enabled: true,
        }];
        let config = SyntaxConfig::default();
        let mut session = Syntax::new(1);
        let mut now = Instant::now();
        for (tab_width, folds) in [(4, 1), (8, 2)] {
            session.tick(&documents, &config, tab_width, now, "large");
            let deadline = Instant::now() + Duration::from_secs(5);
            while session.result(buffer).is_none() {
                now += Duration::from_millis(SYNTAX_DEBOUNCE_MS);
                session.tick(&documents, &config, tab_width, now, "large");
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert_eq!(session.result(buffer).unwrap().folds.len(), folds);
        }
    }

    #[test]
    fn unchanged_ticks_materialize_no_additional_source_and_disable_cancels_identity() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let text = TextBuffer::from_text("fn main() {}\n");
        let documents = [SyntaxDocument {
            buffer,
            text: &text,
            language: Some(SyntaxLanguage::Rust),
            enabled: true,
        }];
        let config = SyntaxConfig::default();
        let mut session = Syntax::new(2);
        let start = Instant::now();
        assert_eq!(session.tick(&documents, &config, 4, start, "too large"), 1);
        let now = start + std::time::Duration::from_millis(SYNTAX_DEBOUNCE_MS);
        for _ in 0..100 {
            assert_eq!(session.tick(&documents, &config, 4, now, "too large"), 0);
        }
        session.tick(
            &documents,
            &SyntaxConfig { enabled: false },
            4,
            now,
            "too large",
        );
        assert!(session.documents.is_empty());
        assert!(session.submitted.is_empty());
        assert_eq!(session.results().count(), 0);
    }

    #[test]
    fn an_edit_keeps_the_last_parse_until_the_next_one_lands() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let mut text = TextBuffer::from_text("// note\nfn main() {}\n");
        let config = SyntaxConfig::default();
        let mut session = Syntax::new(2);
        let mut now = Instant::now();
        for _ in 0..400 {
            let documents = [SyntaxDocument {
                buffer,
                text: &text,
                language: Some(SyntaxLanguage::Rust),
                enabled: true,
            }];
            session.tick(&documents, &config, 4, now, "too large");
            if session.result(buffer).is_some() {
                break;
            }
            now += Duration::from_millis(SYNTAX_DEBOUNCE_MS);
            std::thread::sleep(Duration::from_millis(5));
        }
        let parsed = session.result(buffer).expect("first parse").revision;
        text.restore_text("// notes\nfn main() {}\n").unwrap();
        let documents = [SyntaxDocument {
            buffer,
            text: &text,
            language: Some(SyntaxLanguage::Rust),
            enabled: true,
        }];
        session.tick(&documents, &config, 4, now, "too large");
        assert_eq!(
            session.result(buffer).map(|result| result.revision),
            Some(parsed)
        );
    }

    #[test]
    fn reload_at_the_same_revision_invalidates_syntax_objects() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let original = TextBuffer::from_text("fn old() {}\n");
        let mut session = Syntax::new(2);
        let config = SyntaxConfig::default();
        let mut now = Instant::now();
        for _ in 0..400 {
            session.tick(
                &[SyntaxDocument {
                    buffer,
                    text: &original,
                    language: Some(SyntaxLanguage::Rust),
                    enabled: true,
                }],
                &config,
                4,
                now,
                "large",
            );
            if session.result(buffer).is_some() {
                break;
            }
            now += Duration::from_millis(SYNTAX_DEBOUNCE_MS);
            std::thread::sleep(Duration::from_millis(5));
        }
        let old_document = session.result(buffer).expect("first parse").document;
        let replacement = TextBuffer::from_text("// replacement\n");
        assert_eq!(original.revision(), replacement.revision());
        session.tick(
            &[SyntaxDocument {
                buffer,
                text: &replacement,
                language: Some(SyntaxLanguage::Rust),
                enabled: true,
            }],
            &config,
            4,
            now,
            "large",
        );
        // A reopened buffer parses at once, so a fast worker may already have answered.
        assert!(
            session
                .result(buffer)
                .is_none_or(|result| result.document != old_document)
        );
        assert_eq!(
            session
                .submitted
                .get(&buffer)
                .map(|(revision, ..)| *revision),
            Some(replacement.revision())
        );
    }

    #[test]
    fn an_opened_buffer_parses_at_once_and_holds_its_first_frame_only_briefly() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let text = TextBuffer::from_text("fn main() {}\n");
        let documents = [SyntaxDocument {
            buffer,
            text: &text,
            language: Some(SyntaxLanguage::Rust),
            enabled: true,
        }];
        let config = SyntaxConfig::default();
        let mut session = Syntax::new(2);
        let start = Instant::now();
        assert_eq!(session.tick(&documents, &config, 4, start, "large"), 1);
        assert!(session.first_frame_pending(buffer, start));
        assert!(session.colours_pending(buffer));
        let late = start + Duration::from_millis(FIRST_FRAME_WAIT_MS);
        assert!(!session.first_frame_pending(buffer, late));
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.result(buffer).is_none() {
            session.tick(&documents, &config, 4, start, "large");
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!session.colours_pending(buffer));
        session.first_frame_shown(buffer);
        assert!(!session.first_frame_pending(buffer, start));
    }

    #[test]
    fn stale_request_revision_language_and_closed_document_cannot_publish_failure() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let document = SyntaxDocumentId(42);
        let mut session = Syntax::new(2);
        session.documents.insert(buffer, document);
        session
            .submitted
            .insert(buffer, (BufferRevision(3), 7, SyntaxLanguage::Rust));
        let live = HashMap::from([(buffer, (BufferRevision(3), SyntaxLanguage::Rust))]);
        let event = |revision, request_id, language| SyntaxEvent::Failed {
            document,
            revision,
            request_id,
            language,
            diagnostic: "failure".into(),
        };
        session.accept_event(event(3, 6, SyntaxLanguage::Rust), &live, "large");
        session.accept_event(event(2, 7, SyntaxLanguage::Rust), &live, "large");
        session.accept_event(event(3, 7, SyntaxLanguage::Go), &live, "large");
        assert_eq!(session.failure(buffer), None);
        session.accept_event(event(3, 7, SyntaxLanguage::Rust), &live, "large");
        assert_eq!(session.failure(buffer), Some("failure"));
        session.close(buffer);
        session.accept_event(event(3, 7, SyntaxLanguage::Rust), &live, "large");
        assert_eq!(session.failure(buffer), None);
    }
}

use crate::app::App;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::Precondition;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextEdit;
use std::collections::HashSet;

use crate::editor::text::TextEditSet;
use crate::language::syntax::language_for_document;
use crate::ui::UiFold;
use crate::ui::UiSyntaxSpan;

impl App {
    pub(in crate::app) fn syntax_support_label(&self, support: SyntaxSupport) -> String {
        self.message(
            match support {
                SyntaxSupport::Full => "syntax_support_full",
                SyntaxSupport::Partial => "syntax_support_partial",
                SyntaxSupport::Unsupported => "syntax_support_unsupported",
            },
            &[],
        )
    }
}

impl App {
    pub(in crate::app) fn apply_syntax_indent_line(&mut self) {
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            return;
        };
        let Some(language) = language_for_document(
            buffer.path.as_deref(),
            buffer.text.chunks().next().unwrap_or_default(),
        ) else {
            self.show_error(self.message("syntax_indent_unavailable", &[]));
            return;
        };
        let revision = buffer.text.revision();
        let line = match buffer.text.byte_to_line(cursor) {
            Ok(line) => line,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let levels = match self.syntax.indentation(buffer_id, revision, language, line) {
            Ok(levels) => levels,
            Err(error) => {
                let message = match error {
                    IndentError::Pending => self.message("syntax_indent_pending", &[]),
                    IndentError::Unsupported => self.message(
                        "syntax_indent_unsupported",
                        &[("language", language.name().to_owned())],
                    ),
                    IndentError::Truncated => self.message("syntax_indent_truncated", &[]),
                };
                self.show_error(message);
                return;
            }
        };
        let desired = if self.state.policy.expand_tabs {
            " ".repeat(levels.saturating_mul(self.state.policy.indent_width))
        } else {
            "\t".repeat(levels)
        };
        let text = match buffer.text.line(line) {
            Ok(text) => text,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let whitespace = text
            .bytes()
            .take_while(|byte| matches!(*byte, b' ' | b'\t'))
            .count();
        if text.get(..whitespace) == Some(desired.as_str()) {
            self.notify_message(self.message("syntax_indent_unchanged", &[]), false);
            return;
        }
        let start = match buffer.text.line_to_byte(line) {
            Ok(start) => start,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let end = ByteOffset(start.get().saturating_add(whitespace));
        let edits = TextEditSet::new(
            revision,
            vec![TextEdit::new(
                ByteRange::new(start, end).expect("ordered indentation"),
                desired,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error)
        } else {
            self.notify_message(self.message("syntax_indent_applied", &[]), false);
        }
    }
}

impl App {
    /// A file that was just opened is drawn once it has its colours and, when they are on,
    /// its inlay hints, unless those take longer than a moment.
    pub(in crate::app) fn hold_first_frame(&mut self) -> bool {
        let now = self.clock.monotonic();
        let Some(buffer) = self.state.active_buffer_id() else {
            return false;
        };
        if !self.syntax.first_frame_pending(buffer, now) {
            return false;
        }
        let waiting = self.syntax.colours_pending(buffer)
            || self.configuration.settings().lsp.inlay_hints
                && self.language.inlays_in_flight(buffer);
        // Once drawn, later edits and hint refreshes never hold a frame back.
        if !waiting {
            self.syntax.first_frame_shown(buffer);
        }
        waiting
    }
}

impl App {
    pub(in crate::app) fn process_syntax(&mut self) {
        // Move old fold rows with their text before matching them to a new parse.
        let mut folds_changed = false;
        for (fold, source) in self
            .ui
            .value
            .syntax_folds
            .iter_mut()
            .zip(&mut self.syntax_fold_positions)
        {
            let Some((buffer_id, version, revision, range)) = source else {
                continue;
            };
            let Some(buffer) = self.state.buffers.get(*buffer_id).filter(|buffer| {
                self.state
                    .views
                    .get(fold.view)
                    .is_some_and(|view| view.buffer == *buffer_id)
                    && version.same_instance(&buffer.text.source_version())
            }) else {
                folds_changed |= fold.collapsed;
                fold.collapsed = false;
                continue;
            };
            if let Some(mapped) = buffer.text.map_range_since(*revision, *range) {
                let start = buffer
                    .text
                    .byte_to_line(mapped.start)
                    .unwrap_or(fold.start_line);
                let end = buffer
                    .text
                    .byte_to_line(mapped.end)
                    .unwrap_or(fold.end_line);
                folds_changed |= (fold.start_line, fold.end_line) != (start, end);
                fold.start_line = start;
                fold.end_line = end;
                *range = mapped;
                *version = buffer.text.source_version();
                *revision = buffer.text.revision();
            }
        }
        if folds_changed {
            self.ui.revision = self.ui.revision.wrapping_add(1);
        }
        let documents = self
            .state
            .buffers
            .iter()
            .map(|(buffer, state)| SyntaxDocument {
                buffer,
                text: &state.text,
                language: state
                    .virtual_language
                    .as_deref()
                    .and_then(crate::language::syntax::language_for_fence)
                    .or_else(|| {
                        language_for_document(
                            state.path.as_deref(),
                            state.text.chunks().next().unwrap_or_default(),
                        )
                    }),
                enabled: !self.effective_large_file(buffer),
            })
            .collect::<Vec<_>>();
        let too_large = self.message("syntax_source_too_large", &[]);
        self.syntax.tick(
            &documents,
            &self.configuration.settings().syntax,
            self.configuration.settings().editor.tab_width,
            self.clock.monotonic(),
            &too_large,
        );
        for (buffer, result) in self.syntax.results() {
            let revision = BufferRevision(result.revision);
            let stale = self.state.buffers.get(*buffer).is_some_and(|target| {
                self.syntax.source(*buffer) == Some(&target.text.source_version())
                    && !self.syntax_spans_source.0.contains(&(
                        *buffer,
                        result.request_id,
                        result.revision,
                    ))
            });
            if stale {
                self.state.buffers[*buffer].syntax_objects =
                    Some((revision, result.objects.clone()));
            }
        }
        // Folds and spans are rebuilt only when a parse result or the views changed, not on
        // every tick.
        let source = (
            self.syntax
                .results()
                .map(|(buffer, result)| (*buffer, result.request_id, result.revision))
                .collect::<Vec<_>>(),
            self.state
                .views
                .iter()
                .map(|(view_id, view)| (view_id, view.buffer))
                .collect::<Vec<_>>(),
        );
        if source != self.syntax_spans_source {
            self.syntax_spans_source = source;
            self.rebuild_syntax_rows();
        }
        // Until a new parse or token reply arrives, the old colours follow the edit instead of
        // dropping out, so typed text doesn't flash plain and back.
        let syntax_changed =
            move_spans_onto_current_text(&mut self.ui.value.syntax_spans, &self.state);
        let semantic_changed =
            move_spans_onto_current_text(&mut self.ui.value.lsp_semantic_spans, &self.state);
        if syntax_changed || semantic_changed {
            self.ui.revision = self.ui.revision.wrapping_add(1);
        }
    }
}

impl App {
    fn rebuild_syntax_rows(&mut self) {
        let collapsed = self
            .ui
            .syntax_folds
            .iter()
            .filter(|fold| fold.collapsed)
            .map(|fold| (fold.view, fold.start_line, fold.end_line))
            .collect::<HashSet<_>>();
        let mut syntax_folds = Vec::new();
        for (view_id, view) in self.state.views.iter() {
            if let Some(result) = self.syntax.result(view.buffer) {
                let text = &self.state.buffers[view.buffer].text;
                let Some(parsed) = text.text_at(BufferRevision(result.revision)) else {
                    continue;
                };
                for fold in &result.folds {
                    let range = parsed
                        .line_to_byte(fold.start_row)
                        .ok()
                        .zip(parsed.line_to_byte(fold.end_row).ok())
                        .and_then(|(start, end)| ByteRange::new(start, end).ok())
                        .and_then(|range| {
                            text.map_range_since(BufferRevision(result.revision), range)
                        });
                    let Some((start_line, end_line)) = range.and_then(|range| {
                        Some((
                            text.byte_to_line(range.start).ok()?,
                            text.byte_to_line(range.end).ok()?,
                        ))
                    }) else {
                        continue;
                    };
                    syntax_folds.push(UiFold {
                        view: view_id,
                        start_line,
                        end_line,
                        collapsed: collapsed.contains(&(view_id, start_line, end_line)),
                    });
                }
            }
        }
        self.syntax_fold_positions = syntax_folds
            .iter()
            .map(|fold| {
                let buffer = self
                    .state
                    .buffers
                    .get(self.state.views.get(fold.view)?.buffer)?;
                Some((
                    self.state.views.get(fold.view)?.buffer,
                    buffer.text.source_version(),
                    buffer.text.revision(),
                    ByteRange::new(
                        buffer.text.line_to_byte(fold.start_line).ok()?,
                        buffer.text.line_to_byte(fold.end_line).ok()?,
                    )
                    .ok()?,
                ))
            })
            .collect();
        self.ui.set(syntax_folds, |ui| &mut ui.syntax_folds);
        self.ui.syntax_spans = self
            .syntax
            .results()
            .flat_map(|(buffer, result)| {
                result.spans.iter().filter_map(|span| {
                    Some(UiSyntaxSpan {
                        buffer: *buffer,
                        revision: BufferRevision(result.revision),
                        range: ByteRange::new(ByteOffset(span.start), ByteOffset(span.end)).ok()?,
                        role: span.role,
                    })
                })
            })
            .collect();
        self.ui
            .syntax_spans
            .sort_unstable_by_key(|span| (span.range.start, span.range.end));
        self.ui.markdown_marks = self
            .syntax
            .results()
            .flat_map(|(buffer, result)| {
                result.markdown.iter().filter_map(|span| {
                    Some(crate::ui::UiMarkdownMark {
                        buffer: *buffer,
                        revision: BufferRevision(result.revision),
                        range: ByteRange::new(ByteOffset(span.start), ByteOffset(span.end)).ok()?,
                        mark: span.mark,
                    })
                })
            })
            .collect();
        self.ui
            .markdown_marks
            .sort_unstable_by_key(|mark| mark.range.start);
    }
}

/// Where the `count`th non-blank character (from zero) of `text` starts, or its end.
pub(in crate::app) fn nth_non_blank(text: &str, count: usize) -> usize {
    text.char_indices()
        .filter(|(_, character)| !character.is_whitespace())
        .nth(count)
        .map_or(text.len(), |(at, _)| at)
}

fn move_spans_onto_current_text(
    spans: &mut Vec<UiSyntaxSpan>,
    state: &crate::editor::EditorState,
) -> bool {
    let current = |span: &UiSyntaxSpan| {
        state
            .buffers
            .get(span.buffer)
            .map(|buffer| buffer.text.revision())
    };
    if spans
        .iter()
        .all(|span| current(span).is_none_or(|at| at == span.revision))
    {
        return false;
    }
    spans.retain_mut(|span| {
        let Some(buffer) = state.buffers.get(span.buffer) else {
            return true;
        };
        if span.revision == buffer.text.revision() {
            return true;
        }
        let Some(range) = buffer.text.map_range_since(span.revision, span.range) else {
            return false;
        };
        span.range = range;
        span.revision = buffer.text.revision();
        true
    });
    true
}

#[cfg(test)]
mod formatter_cursor_tests {
    #[test]
    fn a_formatted_file_keeps_the_cursor_on_its_word() {
        let before = "<html><head></head>\n<body><p>Hello World</p></body></html>\n";
        let after =
            "<html>\n  <head></head>\n  <body>\n    <p>Hello World</p>\n  </body>\n</html>\n";
        let cursor = before.find("World").unwrap();
        let non_blank = before[..cursor]
            .chars()
            .filter(|c| !c.is_whitespace())
            .count();
        let moved = crate::app::syntax::nth_non_blank(after, non_blank);
        assert_eq!(&after[moved..moved + 5], "World");
        assert_eq!(crate::app::syntax::nth_non_blank("a b", 9), 3);
    }
}
use crate::app::commands::AppEffect;
use crate::app::commands::SyntaxFoldAction;

use crate::app::picker::PickerPayload;
use crate::language::syntax::builtin_languages;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

impl App {
    pub(in crate::app) fn process_syntax_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::SyntaxFold(action) => {
                if let Some(view) = self.state.active_view() {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    let folds = self
                        .ui
                        .syntax_folds
                        .iter_mut()
                        .filter(|fold| fold.view == view);
                    match action {
                        SyntaxFoldAction::OpenAll => folds.for_each(|fold| fold.collapsed = false),
                        SyntaxFoldAction::CloseAll => folds.for_each(|fold| fold.collapsed = true),
                        SyntaxFoldAction::Toggle
                        | SyntaxFoldAction::Open
                        | SyntaxFoldAction::Close => {
                            // The innermost fold around the cursor, the one Vim's `za` acts on.
                            if let Some(fold) = folds
                                .filter(|fold| {
                                    line.is_some_and(|line| {
                                        line >= fold.start_line && line <= fold.end_line
                                    })
                                })
                                .max_by_key(|fold| fold.start_line)
                            {
                                fold.collapsed = match action {
                                    SyntaxFoldAction::Open => false,
                                    SyntaxFoldAction::Close => true,
                                    _ => !fold.collapsed,
                                };
                            } else {
                                self.notify_message(self.message("syntax_no_fold", &[]), false);
                            }
                        }
                    }
                }
            }
            AppEffect::SyntaxIndentLine => self.apply_syntax_indent_line(),
            AppEffect::SyntaxStatus => {
                let active = self.state.active_buffer_id();
                let active_language = active
                    .and_then(|buffer| self.state.buffers.get(buffer))
                    .and_then(|buffer| {
                        crate::language::syntax::language_for_document(
                            buffer.path.as_deref(),
                            buffer.text.chunks().next().unwrap_or_default(),
                        )
                    });
                let items = builtin_languages()
                    .enumerate()
                    .map(|(index, info)| {
                        let state = if Some(info.language) == active_language {
                            if let Some(buffer) = active {
                                if let Some(error) = self.syntax.failure(buffer) {
                                    self.message(
                                        "syntax_state_failed",
                                        &[("error", error.to_owned())],
                                    )
                                } else if self.syntax.result(buffer).is_some_and(|result| {
                                    self.state.buffers.get(buffer).is_some_and(|state| {
                                        state.text.revision().0 == result.revision
                                    }) && result.language == info.language
                                }) {
                                    self.message(
                                        if self
                                            .syntax
                                            .result(buffer)
                                            .is_some_and(|result| result.truncated)
                                        {
                                            "syntax_state_active_truncated"
                                        } else {
                                            "syntax_state_active"
                                        },
                                        &[],
                                    )
                                } else {
                                    self.message("syntax_state_pending", &[])
                                }
                            } else {
                                self.message("syntax_state_available", &[])
                            }
                        } else {
                            self.message("syntax_state_available", &[])
                        };
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("syntax:{index}:{}", info.id)),
                                label: info.language.name().to_owned(),
                                detail: self.message(
                                    "syntax_capabilities",
                                    &[
                                        (
                                            "highlights",
                                            self.syntax_support_label(info.capabilities.highlights),
                                        ),
                                        (
                                            "folds",
                                            self.syntax_support_label(info.capabilities.folds),
                                        ),
                                        (
                                            "indents",
                                            self.syntax_support_label(info.capabilities.indents),
                                        ),
                                        ("state", state),
                                    ],
                                ),
                                source: self.message("syntax_builtin_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("syntax_status_title", &[]), items);
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
