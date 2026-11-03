use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{
    config::SyntaxConfig,
    editor::{
        BufferId,
        text::{BufferRevision, TextBuffer},
    },
    language::syntax::{
        SyntaxDocumentId, SyntaxEvent, SyntaxLanguage, SyntaxPolicy, SyntaxResult, SyntaxService,
        SyntaxSupport,
    },
};

pub(crate) const SYNTAX_QUEUE_CAPACITY: usize = 8;
/// How long typing must pause before a buffer is parsed again.
const SYNTAX_DEBOUNCE_MS: u64 = 75;

pub(super) struct SyntaxDocument<'a> {
    pub buffer: BufferId,
    pub text: &'a TextBuffer,
    pub language: Option<SyntaxLanguage>,
    pub enabled: bool,
}

struct SyntaxFailure {
    revision: BufferRevision,
    generation: u64,
    language: SyntaxLanguage,
    diagnostic: String,
}

#[derive(Debug, PartialEq)]
pub(super) enum IndentError {
    Pending,
    Unsupported,
    Truncated,
}

/// Owns parser admission, document identities, debounce and accepted results.
pub(super) struct SyntaxSession {
    service: SyntaxService,
    documents: HashMap<BufferId, SyntaxDocumentId>,
    submitted: HashMap<BufferId, (BufferRevision, u64, SyntaxLanguage)>,
    accepted: HashMap<BufferId, SyntaxResult>,
    failures: HashMap<BufferId, SyntaxFailure>,
    due: HashMap<BufferId, (BufferRevision, Instant)>,
    next_document: u64,
    next_generation: u64,
}

impl SyntaxSession {
    pub(super) fn new(queue_capacity: usize) -> Self {
        Self {
            service: SyntaxService::start(queue_capacity),
            documents: HashMap::new(),
            submitted: HashMap::new(),
            accepted: HashMap::new(),
            failures: HashMap::new(),
            due: HashMap::new(),
            next_document: 1,
            next_generation: 1,
        }
    }

    fn close(&mut self, buffer: BufferId) {
        if let Some(document) = self.documents.remove(&buffer) {
            self.service.cancel(document);
        }
        self.submitted.remove(&buffer);
        self.accepted.remove(&buffer);
        self.failures.remove(&buffer);
        self.due.remove(&buffer);
    }

    /// Returns how many source strings were materialized, for admission-cost checks.
    /// No source snapshot is created before its revision/debounce checks succeed.
    pub(super) fn tick(
        &mut self,
        documents: &[SyntaxDocument<'_>],
        config: &SyntaxConfig,
        now: Instant,
        too_large: &str,
    ) -> usize {
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
            .filter(|id| !live.contains_key(id))
            .copied()
            .collect::<Vec<_>>();
        for buffer in removed {
            self.close(buffer);
        }
        // An older parse stays until the next one lands, so editing doesn't blank the colours;
        // anything that needs the exact text checks the revision itself.
        self.accepted.retain(|buffer, result| {
            live.get(buffer)
                .is_some_and(|(_, language)| *language == result.language)
        });
        self.failures.retain(|buffer, failure| {
            live.get(buffer) == Some(&(failure.revision, failure.language))
                && self.submitted.get(buffer)
                    == Some(&(failure.revision, failure.generation, failure.language))
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
            let due = self
                .due
                .entry(doc.buffer)
                .or_insert((revision, now + Duration::from_millis(SYNTAX_DEBOUNCE_MS)));
            if due.0 != revision {
                *due = (revision, now + Duration::from_millis(SYNTAX_DEBOUNCE_MS));
                continue;
            }
            if now < due.1 {
                continue;
            }
            let document = *self.documents.entry(doc.buffer).or_insert_with(|| {
                let id = SyntaxDocumentId(self.next_document);
                self.next_document = self.next_document.saturating_add(1);
                id
            });
            let generation = self.next_generation;
            self.next_generation = self.next_generation.saturating_add(1);
            let policy = SyntaxPolicy {
                maximum_source_bytes: 8 * 1024 * 1024,
                maximum_spans: 100_000,
                maximum_folds: 10_000,
                maximum_indent_rows: 100_000,
            };
            materialized += 1;
            if self
                .service
                .submit(
                    document,
                    generation,
                    revision.0,
                    language,
                    doc.text.to_string(),
                    policy,
                )
                .is_ok()
            {
                self.submitted
                    .insert(doc.buffer, (revision, generation, language));
                self.due.remove(&doc.buffer);
            }
        }
        while let Ok(Some(event)) = self.service.try_event() {
            self.accept_event(event, &live, too_large);
        }
        materialized
    }

    fn accept_event(
        &mut self,
        event: SyntaxEvent,
        live: &HashMap<BufferId, (BufferRevision, SyntaxLanguage)>,
        too_large: &str,
    ) {
        let (document, revision, generation, language) = match &event {
            SyntaxEvent::Parsed(result) => (
                result.document,
                result.revision,
                result.generation,
                result.language,
            ),
            SyntaxEvent::Failed {
                document,
                revision,
                generation,
                language,
                ..
            }
            | SyntaxEvent::Skipped {
                document,
                revision,
                generation,
                language,
            } => (*document, *revision, *generation, *language),
        };
        let Some((&buffer, _)) = self
            .documents
            .iter()
            .find(|(_, candidate)| **candidate == document)
        else {
            return;
        };
        if self.submitted.get(&buffer) != Some(&(BufferRevision(revision), generation, language))
            || live.get(&buffer) != Some(&(BufferRevision(revision), language))
        {
            return;
        }
        match event {
            SyntaxEvent::Parsed(result) => {
                self.failures.remove(&buffer);
                self.accepted.insert(buffer, result);
            }
            SyntaxEvent::Failed { diagnostic, .. } => {
                self.accepted.remove(&buffer);
                self.failures.insert(
                    buffer,
                    SyntaxFailure {
                        revision: BufferRevision(revision),
                        generation,
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
                        generation,
                        language,
                        diagnostic: too_large.to_owned(),
                    },
                );
            }
        }
    }

    pub(super) fn result(&self, buffer: BufferId) -> Option<&SyntaxResult> {
        self.accepted.get(&buffer)
    }
    pub(super) fn results(&self) -> impl Iterator<Item = (&BufferId, &SyntaxResult)> {
        self.accepted.iter()
    }
    pub(super) fn failure(&self, buffer: BufferId) -> Option<&str> {
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
    use super::*;
    use slotmap::SlotMap;

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
        let mut session = SyntaxSession::new(2);
        let start = Instant::now();
        assert_eq!(session.tick(&documents, &config, start, "too large"), 0);
        let now = start + std::time::Duration::from_millis(SYNTAX_DEBOUNCE_MS);
        assert_eq!(session.tick(&documents, &config, now, "too large"), 1);
        for _ in 0..100 {
            assert_eq!(session.tick(&documents, &config, now, "too large"), 0);
        }
        session.tick(
            &documents,
            &SyntaxConfig {
                enabled: false,
                ..config
            },
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
        let mut session = SyntaxSession::new(2);
        let mut now = Instant::now();
        for _ in 0..400 {
            let documents = [SyntaxDocument {
                buffer,
                text: &text,
                language: Some(SyntaxLanguage::Rust),
                enabled: true,
            }];
            session.tick(&documents, &config, now, "too large");
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
        session.tick(&documents, &config, now, "too large");
        assert_eq!(session.result(buffer).map(|result| result.revision), Some(parsed));
    }

    #[test]
    fn stale_generation_revision_language_and_closed_document_cannot_publish_failure() {
        let mut ids = SlotMap::<BufferId, ()>::with_key();
        let buffer = ids.insert(());
        let document = SyntaxDocumentId(42);
        let mut session = SyntaxSession::new(2);
        session.documents.insert(buffer, document);
        session
            .submitted
            .insert(buffer, (BufferRevision(3), 7, SyntaxLanguage::Rust));
        let live = HashMap::from([(buffer, (BufferRevision(3), SyntaxLanguage::Rust))]);
        let event = |revision, generation, language| SyntaxEvent::Failed {
            document,
            revision,
            generation,
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
