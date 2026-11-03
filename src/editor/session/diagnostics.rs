//! Canonical diagnostics, source retirement, selection and derived presentation.
use crate::{
    editor::{
        BufferId, EditorState, EditorWindowVersion,
        text::{BufferRevision, ByteOffset, TextVersion},
    },
    ui::{
        ContentTabDiagnostic, ContentTabDiagnosticSeverity, ExpandedDiagnosticState, UiDiagnostic,
    },
    workspace::{
        Diagnostic, DiagnosticOwner, DiagnosticProducer, DiagnosticSeverity, DiagnosticStore,
        ProblemRow,
    },
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

pub(crate) const MAXIMUM_DIAGNOSTICS_PER_URI: usize = 2_000;
pub(crate) const MAXIMUM_DIAGNOSTICS: usize = 10_000;

/// Captured by a source before work, or checked against its original transport context.
#[derive(Clone, Debug)]
pub(super) struct DiagnosticDocument {
    editor: EditorWindowVersion,
    id: BufferId,
    path: PathBuf,
    text: TextVersion,
}
impl DiagnosticDocument {
    pub(super) fn capture(editor: &EditorState, id: BufferId) -> Option<Self> {
        let buffer = editor.buffers.get(id)?;
        Some(Self {
            editor: editor.window_version(),
            id,
            path: buffer.path.clone()?,
            text: buffer.text.source_version(),
        })
    }
    pub(super) fn current(&self, editor: &EditorState) -> bool {
        self.editor.same_editor(&editor.window_version())
            && editor.window_version().revision().is_some()
            && editor.buffers.get(self.id).is_some_and(|buffer| {
                buffer.path.as_ref() == Some(&self.path)
                    && buffer.text.source_version() == self.text
            })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DiagnosticValidity {
    RequestedSnapshot,
    ReportedVersion,
    UnversionedReport,
    DiskReport,
}
pub(super) struct DiagnosticBatch {
    pub producer: DiagnosticProducer,
    pub uri: String,
    pub document: Option<DiagnosticDocument>,
    pub validity: DiagnosticValidity,
    pub items: Vec<Diagnostic>,
}
#[derive(Clone, Debug)]
pub(super) struct DiagnosticKey {
    epoch: Arc<()>,
    index: usize,
}
impl DiagnosticKey {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &other.epoch) && self.index == other.index
    }
}
struct Batch {
    epoch: Arc<()>,
    path: PathBuf,
    document: Option<DiagnosticDocument>,
    validity: DiagnosticValidity,
    /// The text the ranges point into; edits since then move them.
    revision: Option<BufferRevision>,
}
impl Batch {
    fn current(&self, editor: &EditorState) -> bool {
        match &self.document {
            Some(document) => document.current(editor),
            None => !editor
                .buffers
                .iter()
                .any(|(_, buffer)| buffer.path.as_ref() == Some(&self.path)),
        }
    }
}
struct Row {
    key: DiagnosticKey,
    problem: ProblemRow,
}
pub(super) struct DiagnosticSession {
    store: DiagnosticStore,
    producers: BTreeMap<DiagnosticOwner, DiagnosticProducer>,
    batches: BTreeMap<(DiagnosticOwner, String), Batch>,
    rows: Vec<Row>,
    counts: BTreeMap<PathBuf, ContentTabDiagnostic>,
    expanded: Option<(DiagnosticKey, Vec<DiagnosticSeverity>)>,
    dirty: bool,
}
impl DiagnosticSession {
    pub(super) fn new(total: usize, per_uri: usize) -> Self {
        Self {
            store: DiagnosticStore::new(total, per_uri),
            producers: BTreeMap::new(),
            batches: BTreeMap::new(),
            rows: Vec::new(),
            counts: BTreeMap::new(),
            expanded: None,
            dirty: true,
        }
    }
    /// Explicit producer activation, not a late result's implicit name lookup.
    pub(super) fn activate(&mut self, producer: DiagnosticProducer) {
        if self.current(&producer) {
            return;
        }
        self.clear_owner(producer.owner());
        self.producers.insert(producer.owner().clone(), producer);
    }
    pub(super) fn current(&self, producer: &DiagnosticProducer) -> bool {
        self.producers
            .get(producer.owner())
            .is_some_and(|p| p.same(producer))
    }
    pub(super) fn publish(
        &mut self,
        batch: DiagnosticBatch,
        editor: &EditorState,
    ) -> Result<(), String> {
        if self.publish_inner(batch, editor)? {
            self.changed();
        }
        Ok(())
    }
    pub(super) fn replace_source(
        &mut self,
        producer: &DiagnosticProducer,
        batches: Vec<DiagnosticBatch>,
        editor: &EditorState,
    ) -> Result<(), String> {
        if !self.current(producer) || batches.len() > self.store.limits().0 {
            return Err("diagnostic source retired or oversized".into());
        }
        let mut uris = std::collections::BTreeSet::new();
        for batch in &batches {
            if !batch.producer.same(producer) || !uris.insert(&batch.uri) {
                return Err("mixed or duplicate diagnostic source batches".into());
            }
            self.check_batch(batch, editor)?;
        }
        self.store.clear_owner(producer.owner());
        self.batches
            .retain(|(owner, _), _| owner != producer.owner());
        for batch in batches {
            if let Err(error) = self.publish_inner(batch, editor) {
                self.changed();
                return Err(error);
            }
        }
        self.changed();
        Ok(())
    }
    fn check_batch(
        &self,
        batch: &DiagnosticBatch,
        editor: &EditorState,
    ) -> Result<PathBuf, String> {
        if !self.current(&batch.producer) {
            return Err("diagnostic producer retired".into());
        }
        let path =
            crate::language::lsp::path_from_file_uri(&batch.uri).ok_or("invalid diagnostic URI")?;
        if match &batch.document {
            Some(document) => document.path != path || !document.current(editor),
            None => editor
                .buffers
                .iter()
                .any(|(_, buffer)| buffer.path.as_ref() == Some(&path)),
        } {
            return Err("diagnostic document changed".into());
        }
        let version = batch.items.first().and_then(|d| d.version);
        let revision = batch
            .document
            .as_ref()
            .and_then(|d| editor.buffers.get(d.id))
            .map(|b| b.text.revision());
        DiagnosticStore::check_set(&batch.uri, version, revision, &batch.items)
            .map_err(|error| error.to_string())?;
        Ok(path)
    }
    fn publish_inner(
        &mut self,
        batch: DiagnosticBatch,
        editor: &EditorState,
    ) -> Result<bool, String> {
        let path = self.check_batch(&batch, editor)?;
        let version = batch.items.first().and_then(|item| item.version);
        let revision = batch
            .document
            .as_ref()
            .and_then(|document| editor.buffers.get(document.id))
            .map(|buffer| buffer.text.revision());
        let set = (batch.producer.owner().clone(), batch.uri.clone());
        let previous = self
            .rows
            .iter()
            .filter(|row| row.problem.owner == set.0 && row.problem.uri == set.1);
        let unchanged = self.batches.get(&set).is_some_and(|old| {
            old.validity == batch.validity
                && match (&old.document, &batch.document) {
                    (None, None) => true,
                    (Some(old), Some(new)) => {
                        old.id == new.id
                            && old.text == new.text
                            && old.editor.same_editor(&new.editor)
                            && old.current(editor)
                    }
                    _ => false,
                }
        }) && previous.clone().count() == batch.items.len()
            && previous.zip(&batch.items).all(|(row, item)| {
                row.problem.range == item.range
                    && row.problem.severity == item.severity
                    && row.problem.source == item.source
                    && row.problem.code == item.code
                    && row.problem.message == item.message
            });
        self.store
            .replace(
                batch.producer.owner().clone(),
                batch.uri.clone(),
                version,
                revision,
                batch.items,
            )
            .map_err(|e| e.to_string())?;
        if unchanged {
            if let Some(old) = self.batches.get_mut(&set) {
                old.revision = revision;
            }
            return Ok(false);
        }
        self.batches.insert(
            (batch.producer.owner().clone(), batch.uri),
            Batch {
                epoch: Arc::new(()),
                path,
                document: batch.document,
                validity: batch.validity,
                revision,
            },
        );
        Ok(true)
    }
    pub(super) fn clear_source(&mut self, producer: &DiagnosticProducer) {
        if self.current(producer) {
            self.clear_owner(producer.owner());
        }
    }
    pub(super) fn clear_owner(&mut self, owner: &DiagnosticOwner) {
        self.producers.remove(owner);
        self.store.clear_owner(owner);
        self.batches.retain(|(o, _), _| o != owner);
        self.changed();
    }
    pub(super) fn clear_owner_uri(&mut self, owner: &DiagnosticOwner, uri: &str) {
        self.store.clear_owner_uri(owner, uri);
        self.batches.remove(&(owner.clone(), uri.into()));
        self.changed();
    }
    fn changed(&mut self) {
        let mut indices = BTreeMap::<(DiagnosticOwner, String), usize>::new();
        self.rows = self
            .store
            .problems(None, None)
            .into_iter()
            .filter_map(|problem| {
                let set = (problem.owner.clone(), problem.uri.clone());
                let batch = self.batches.get(&set)?;
                let index = indices.entry(set).or_default();
                let key = DiagnosticKey {
                    epoch: batch.epoch.clone(),
                    index: *index,
                };
                *index += 1;
                Some(Row { key, problem })
            })
            .collect();
        self.counts.clear();
        for row in &self.rows {
            if let Some(path) = crate::language::lsp::path_from_file_uri(&row.problem.uri) {
                let count = self.counts.entry(path).or_insert(ContentTabDiagnostic {
                    severity: ContentTabDiagnosticSeverity::Warning,
                    count: 0,
                });
                count.count = count.count.saturating_add(1);
                if row.problem.severity == DiagnosticSeverity::Error {
                    count.severity = ContentTabDiagnosticSeverity::Error;
                }
            }
        }
        self.batches.retain(|key, _| indices.contains_key(key));
        if self
            .expanded
            .as_ref()
            .is_some_and(|(key, _)| !self.rows.iter().any(|r| r.key.same(key)))
        {
            self.expanded = None;
        }
        self.dirty = true;
    }
    pub(super) fn retire_documents(&mut self, editor: &EditorState) {
        self.follow_edits(editor);
        let retired = self
            .batches
            .iter()
            .filter(|(_, batch)| !batch.current(editor))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        if retired.is_empty() {
            return;
        }
        for (owner, uri) in retired {
            self.store.clear_owner_uri(&owner, &uri);
            self.batches.remove(&(owner, uri));
        }
        self.changed();
    }
    pub(super) fn resolve(&self, key: &DiagnosticKey, editor: &EditorState) -> Option<&ProblemRow> {
        let row = self.rows.iter().find(|row| row.key.same(key))?;
        let batch = self
            .batches
            .values()
            .find(|batch| Arc::ptr_eq(&batch.epoch, &key.epoch))?;
        if !batch.current(editor) {
            return None;
        }
        Some(&row.problem)
    }
    pub(super) fn counts(&self) -> &BTreeMap<PathBuf, ContentTabDiagnostic> {
        &self.counts
    }
    pub(super) fn choices(
        &self,
    ) -> impl Iterator<Item = (DiagnosticKey, &ProblemRow, DiagnosticValidity)> {
        self.rows.iter().map(|row| {
            (
                row.key.clone(),
                &row.problem,
                self.batches[&(row.problem.owner.clone(), row.problem.uri.clone())].validity,
            )
        })
    }
    /// The problems within this stretch of a file, which a code action there may fix.
    pub(super) fn within(&self, uri: &str, start: ByteOffset, end: ByteOffset) -> Vec<&ProblemRow> {
        self.rows
            .iter()
            .filter(|row| {
                row.problem.uri == uri && row.problem.range.start <= end && row.problem.range.end >= start
            })
            .map(|row| &row.problem)
            .collect()
    }
    pub(super) fn adjacent(
        &self,
        uri: &str,
        offset: ByteOffset,
        direction: i8,
        severities: &[DiagnosticSeverity],
    ) -> Option<DiagnosticKey> {
        let mut rows = self
            .rows
            .iter()
            .filter(|r| r.problem.uri == uri && severities.contains(&r.problem.severity));
        if direction < 0 {
            rows.clone()
                .rev()
                .find(|r| r.problem.range.start < offset)
                .or_else(|| rows.next_back())
        } else {
            rows.clone()
                .find(|r| r.problem.range.start > offset)
                .or_else(|| rows.next())
        }
        .map(|row| row.key.clone())
    }
    pub(super) fn expand(
        &mut self,
        key: DiagnosticKey,
        severities: &[DiagnosticSeverity],
        editor: &EditorState,
    ) {
        if self.resolve(&key, editor).is_some() {
            self.expanded = Some((key, severities.to_vec()));
        }
    }
    pub(super) fn dismiss_expansion(&mut self) {
        self.expanded = None;
    }
    pub(super) fn expanded(&self, editor: &EditorState) -> Option<ExpandedDiagnosticState> {
        let (key, severities) = self.expanded.as_ref()?;
        let row = self.resolve(key, editor)?;
        let path = crate::language::lsp::path_from_file_uri(&row.uri)?;
        let buffer = editor
            .buffers
            .iter()
            .find(|(_, b)| b.path.as_ref() == Some(&path))?
            .1;
        Some(ExpandedDiagnosticState {
            path,
            line: buffer.text.byte_to_line(row.range.start).ok()?,
            severities: severities.clone(),
        })
    }
    /// Until the server reports again, its diagnostics move with the edits, so one doesn't
    /// jump to the next line when the text before it shrinks.
    fn follow_edits(&mut self, editor: &EditorState) {
        let mut moved = false;
        for ((owner, uri), batch) in &mut self.batches {
            let (Some(document), Some(revision)) = (&mut batch.document, batch.revision) else {
                continue;
            };
            let Some(buffer) = editor.buffers.get(document.id) else {
                continue;
            };
            let text = &buffer.text;
            // Only edits of the same file in this editor, which are still remembered, move them;
            // a reload or another file retires them as before.
            if text.revision() == revision
                || !document.editor.same_editor(&editor.window_version())
                || buffer.path.as_ref() != Some(&document.path)
                || !text.source_version().same_incarnation(&document.text)
                || text.map_offset_since(revision, ByteOffset(0)).is_none()
            {
                continue;
            }
            let length = ByteOffset(text.len_bytes());
            self.store.move_ranges(owner, uri, |range| {
                let start = text.map_offset_since(revision, range.start).unwrap_or(range.start).min(length);
                let end = text.map_offset_since(revision, range.end).unwrap_or(range.end).clamp(start, length);
                crate::editor::text::ByteRange::new(start, end).unwrap_or(range)
            });
            batch.revision = Some(text.revision());
            document.text = text.source_version();
            moved = true;
        }
        if moved {
            self.changed();
        }
    }
    pub(super) fn projection(&mut self, editor: &EditorState) -> Option<Vec<UiDiagnostic>> {
        self.retire_documents(editor);
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        Some(
            self.rows
                .iter()
                .filter_map(|row| {
                    Some(UiDiagnostic {
                        path: crate::language::lsp::path_from_file_uri(&row.problem.uri)?,
                        range: row.problem.range,
                        severity: row.problem.severity,
                        source: row.problem.source.clone(),
                        code: row.problem.code.clone(),
                        message: row.problem.message.clone(),
                    })
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::text::{ByteRange, TextBuffer};
    #[test]
    fn diagnostic_sources_keep_canonical_keys_limits_and_expansion_scoped() {
        let root = tempfile::tempdir().unwrap();
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source");
        let (buffer, _) = editor.open_text(TextBuffer::from_text("abc"), Some(path.clone()));
        let uri = crate::language::lsp::file_uri(&path);
        let old = DiagnosticProducer::new(DiagnosticOwner::new("same name").unwrap());
        let other = DiagnosticProducer::new(DiagnosticOwner::new("independent").unwrap());
        let mut owner = DiagnosticSession::new(4, 2);
        let batch =
            |producer: &DiagnosticProducer, message: &str, editor: &EditorState| DiagnosticBatch {
                producer: producer.clone(),
                uri: uri.clone(),
                document: DiagnosticDocument::capture(editor, buffer),
                validity: DiagnosticValidity::RequestedSnapshot,
                items: vec![Diagnostic {
                    uri: uri.clone(),
                    version: None,
                    range: ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                    severity: DiagnosticSeverity::Error,
                    source: "test".into(),
                    code: None,
                    message: message.into(),
                    related: Vec::new(),
                    tags: Vec::new(),
                    fixes: Vec::new(),
                    data: None,
                    timestamp: std::time::SystemTime::UNIX_EPOCH,
                }],
            };
        owner.activate(old.clone());
        owner.publish(batch(&old, "old", &editor), &editor).unwrap();
        assert_eq!(owner.counts()[&path].count, 1);
        assert_eq!(
            owner.counts()[&path].severity,
            ContentTabDiagnosticSeverity::Error
        );
        let key = owner.choices().next().unwrap().0;
        owner.expand(key.clone(), &[DiagnosticSeverity::Error], &editor);
        assert!(owner.projection(&editor).is_some());
        for _ in 0..100 {
            assert!(owner.projection(&editor).is_none());
        }
        owner.publish(batch(&old, "old", &editor), &editor).unwrap();
        assert!(owner.projection(&editor).is_none());
        assert!(owner.resolve(&key, &editor).is_some());
        owner.activate(other.clone());
        owner
            .replace_source(&other, vec![batch(&other, "other", &editor)], &editor)
            .unwrap();
        assert!(owner.resolve(&key, &editor).is_some());
        assert!(owner.expanded(&editor).is_some());
        let mut invalid = batch(&other, "invalid", &editor);
        invalid.items[0].uri = "file:///different".into();
        assert!(
            owner
                .replace_source(&other, vec![invalid], &editor)
                .is_err()
        );
        assert_eq!(owner.choices().count(), 2);
        assert!(owner.resolve(&key, &editor).is_some());
        let replacement = DiagnosticProducer::new(DiagnosticOwner::new("same name").unwrap());
        owner.activate(replacement.clone());
        owner
            .publish(batch(&replacement, "new", &editor), &editor)
            .unwrap();
        owner.clear_source(&old);
        assert_eq!(owner.choices().count(), 2);
        assert!(owner.resolve(&key, &editor).is_none());
        assert!(owner.expanded(&editor).is_none());
        assert!(
            owner
                .publish(batch(&old, "late", &editor), &editor)
                .is_err()
        );
        let pending = batch(&replacement, "captured", &editor);
        editor.buffers[buffer].text = TextBuffer::from_text("abc");
        let materializations = TextBuffer::test_materializations();
        assert!(owner.publish(pending, &editor).is_err());
        assert_eq!(TextBuffer::test_materializations(), materializations);
        owner.projection(&editor);
        assert_eq!(owner.choices().count(), 0);
        owner
            .publish(batch(&replacement, "fresh", &editor), &editor)
            .unwrap();
        owner
            .publish(batch(&other, "independent", &editor), &editor)
            .unwrap();
        owner.clear_source(&other);
        owner.clear_source(&replacement);
        owner.activate(replacement.clone());
        let disk_path = root.path().join("closed");
        let mut disk = batch(&replacement, "disk", &editor);
        disk.uri = crate::language::lsp::file_uri(&disk_path);
        disk.items[0].uri = disk.uri.clone();
        disk.document = None;
        disk.validity = DiagnosticValidity::DiskReport;
        owner.publish(disk, &editor).unwrap();
        let key = owner.choices().next().unwrap().0;
        editor.open_text(TextBuffer::from_text("new"), Some(disk_path));
        assert!(
            owner.resolve(&key, &editor).is_none(),
            "an unbound disk report cannot adopt a newly opened buffer"
        );
    }

    #[test]
    fn diagnostics_move_with_edits_until_the_server_reports_again() {
        let root = tempfile::tempdir().unwrap();
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source");
        let (buffer, _) = editor.open_text(TextBuffer::from_text("abc\ndef\n"), Some(path.clone()));
        let uri = crate::language::lsp::file_uri(&path);
        let producer = DiagnosticProducer::new(DiagnosticOwner::new("server").unwrap());
        let mut owner = DiagnosticSession::new(4, 2);
        owner.activate(producer.clone());
        // At the end of the first line, as rust-analyzer reports "expected COMMA".
        let batch = DiagnosticBatch {
            producer,
            uri: uri.clone(),
            document: DiagnosticDocument::capture(&editor, buffer),
            validity: DiagnosticValidity::RequestedSnapshot,
            items: vec![Diagnostic {
                uri,
                version: None,
                range: ByteRange::new(ByteOffset(3), ByteOffset(4)).unwrap(),
                severity: DiagnosticSeverity::Error,
                source: "test".into(),
                code: None,
                message: "expected COMMA".into(),
                related: Vec::new(),
                tags: Vec::new(),
                fixes: Vec::new(),
                data: None,
                timestamp: std::time::SystemTime::UNIX_EPOCH,
            }],
        };
        owner.publish(batch, &editor).unwrap();
        assert_eq!(owner.projection(&editor).expect("published").len(), 1);
        let text = &mut editor.buffers[buffer].text;
        let edits = crate::editor::text::TextEditSet::new(
            text.revision(),
            vec![crate::editor::text::TextEdit::new(ByteRange::new(ByteOffset(2), ByteOffset(3)).unwrap(), String::new())],
        );
        text.apply_edits(&edits).unwrap();
        let shown = owner.projection(&editor).expect("moved diagnostics");
        assert_eq!(shown[0].range, ByteRange::new(ByteOffset(2), ByteOffset(3)).unwrap());
    }
}
