//! Diagnostics, stale-source cleanup, selection and presentation.
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::EditorWindowVersion;
use crate::editor::text::BufferRevision;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextVersion;
use crate::ui::ContentTabDiagnostic;
use crate::ui::ContentTabDiagnosticSeverity;
use crate::ui::ExpandedDiagnosticState;
use crate::ui::UiDiagnostic;
use crate::workspace::Diagnostic;
use crate::workspace::DiagnosticOwner;
use crate::workspace::DiagnosticProducer;
use crate::workspace::DiagnosticSeverity;
use crate::workspace::DiagnosticStore;
use crate::workspace::ProblemRow;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub(in crate::app) const MAXIMUM_DIAGNOSTICS_PER_URI: usize = 2_000;
pub(in crate::app) const MAXIMUM_DIAGNOSTICS: usize = 10_000;

/// Captured by a source before work, or checked against its original transport context.
#[derive(Clone, Debug)]
pub(super) struct DiagnosticDocument {
    editor: EditorWindowVersion,
    id: BufferId,
    path: Option<PathBuf>,
    text: TextVersion,
}
impl DiagnosticDocument {
    pub(in crate::app) fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
    pub(in crate::app) fn uri(&self) -> String {
        document_uri(self.id, self.path.as_deref())
    }
    pub(in crate::app) fn capture(editor: &EditorState, id: BufferId) -> Option<Self> {
        let buffer = editor.buffers.get(id)?;
        Some(Self {
            editor: editor.window_version(),
            id,
            path: buffer.path.clone(),
            text: buffer.text.source_version(),
        })
    }
    pub(in crate::app) fn current(&self, editor: &EditorState) -> bool {
        self.editor.same_editor(&editor.window_version())
            && editor.window_version().revision().is_some()
            && editor.buffers.get(self.id).is_some_and(|buffer| {
                buffer.path == self.path && buffer.text.source_version() == self.text
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
    pub(in crate::app) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &other.epoch) && self.index == other.index
    }
}
struct Batch {
    epoch: Arc<()>,
    path: Option<PathBuf>,
    document: Option<DiagnosticDocument>,
    validity: DiagnosticValidity,
    /// The text the ranges point into; edits since then move them.
    revision: Option<BufferRevision>,
}
impl Batch {
    pub(in crate::app) fn current(&self, editor: &EditorState) -> bool {
        match &self.document {
            Some(document) => document.current(editor),
            None => !editor
                .buffers
                .iter()
                .any(|(_, buffer)| buffer.path == self.path),
        }
    }
}
struct Row {
    key: DiagnosticKey,
    problem: ProblemRow,
}
pub(super) struct Diagnostics {
    store: DiagnosticStore,
    producers: BTreeMap<DiagnosticOwner, DiagnosticProducer>,
    batches: BTreeMap<(DiagnosticOwner, String), Batch>,
    rows: Vec<Row>,
    counts: BTreeMap<PathBuf, ContentTabDiagnostic>,
    expanded: Option<(DiagnosticKey, Vec<DiagnosticSeverity>)>,
    dirty: bool,
}
impl Diagnostics {
    pub(in crate::app) fn new(total: usize, per_uri: usize) -> Self {
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
    pub(in crate::app) fn activate(&mut self, producer: DiagnosticProducer) {
        if self.current(&producer) {
            return;
        }
        self.clear_owner(producer.owner());
        self.producers.insert(producer.owner().clone(), producer);
    }
    pub(in crate::app) fn current(&self, producer: &DiagnosticProducer) -> bool {
        self.producers
            .get(producer.owner())
            .is_some_and(|p| p.same(producer))
    }
    pub(in crate::app) fn publish(
        &mut self,
        batch: DiagnosticBatch,
        editor: &EditorState,
    ) -> Result<(), String> {
        if self.publish_inner(batch, editor)? {
            self.changed();
        }
        Ok(())
    }
    pub(in crate::app) fn replace_source(
        &mut self,
        producer: &DiagnosticProducer,
        batches: Vec<DiagnosticBatch>,
        editor: &EditorState,
    ) -> Result<(), String> {
        if !self.current(producer) || batches.len() > self.store.limits().0 {
            return Err("diagnostic source stale or oversized".into());
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
    ) -> Result<Option<PathBuf>, String> {
        if !self.current(&batch.producer) {
            return Err("diagnostic producer stale".into());
        }
        let path = crate::language::lsp::path_from_file_uri(&batch.uri);
        if match &batch.document {
            Some(document) => document.uri() != batch.uri || !document.current(editor),
            None => path.is_none() || editor.buffers.iter().any(|(_, buffer)| buffer.path == path),
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
        self.store
            .replace(
                batch.producer.owner().clone(),
                batch.uri.clone(),
                version,
                revision,
                batch.items,
            )
            .map_err(|e| e.to_string())?;
        let retained = self
            .store
            .problems(None, None)
            .into_iter()
            .filter(|row| row.owner == set.0 && row.uri == set.1)
            .collect::<Vec<_>>();
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
        }) && previous.clone().count() == retained.len()
            && previous.zip(&retained).all(|(row, item)| {
                row.problem.range == item.range
                    && row.problem.severity == item.severity
                    && row.problem.source == item.source
                    && row.problem.code == item.code
                    && row.problem.message == item.message
                    && row.problem.data == item.data
            });
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
    pub(in crate::app) fn clear_source(&mut self, producer: &DiagnosticProducer) {
        if self.current(producer) {
            self.clear_owner(producer.owner());
        }
    }
    pub(in crate::app) fn clear_owner(&mut self, owner: &DiagnosticOwner) {
        self.producers.remove(owner);
        self.store.clear_owner(owner);
        self.batches.retain(|(o, _), _| o != owner);
        self.changed();
    }
    pub(in crate::app) fn clear_owner_uri(&mut self, owner: &DiagnosticOwner, uri: &str) {
        self.store.clear_owner_uri(owner, uri);
        self.batches.remove(&(owner.clone(), uri.into()));
        self.changed();
    }
    pub(in crate::app) fn changed(&mut self) {
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
    pub(in crate::app) fn clear_stale_documents(&mut self, editor: &EditorState) {
        self.follow_edits(editor);
        let stale = self
            .batches
            .iter()
            .filter(|(_, batch)| !batch.current(editor))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        if stale.is_empty() {
            return;
        }
        for (owner, uri) in stale {
            self.store.clear_owner_uri(&owner, &uri);
            self.batches.remove(&(owner, uri));
        }
        self.changed();
    }
    pub(in crate::app) fn resolve(
        &self,
        key: &DiagnosticKey,
        editor: &EditorState,
    ) -> Option<&ProblemRow> {
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
    pub(in crate::app) fn counts(&self) -> &BTreeMap<PathBuf, ContentTabDiagnostic> {
        &self.counts
    }
    pub(in crate::app) fn choices(
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
    pub(in crate::app) fn within(
        &self,
        uri: &str,
        start: ByteOffset,
        end: ByteOffset,
    ) -> Vec<&ProblemRow> {
        self.rows
            .iter()
            .filter(|row| {
                let range = row.problem.range;
                row.problem.uri == uri
                    && if start == end {
                        range.start <= start
                            && (start < range.end
                                || range.start == range.end && start == range.start)
                    } else if range.start == range.end {
                        start <= range.start && range.start < end
                    } else {
                        range.start < end && start < range.end
                    }
            })
            .map(|row| &row.problem)
            .collect()
    }
    pub(in crate::app) fn adjacent(
        &self,
        uri: &str,
        offset: ByteOffset,
        direction: i8,
        severities: &[DiagnosticSeverity],
        count: u64,
    ) -> Option<DiagnosticKey> {
        let mut rows = self
            .rows
            .iter()
            .filter(|r| r.problem.uri == uri && severities.contains(&r.problem.severity))
            .collect::<Vec<_>>();
        if direction < 0 {
            rows.reverse();
        }
        // Navigation skips other diagnostics at the same position, just as a single step does.
        rows.dedup_by_key(|row| row.problem.range.start);
        let first = rows
            .iter()
            .position(|row| {
                if direction < 0 {
                    row.problem.range.start < offset
                } else {
                    row.problem.range.start > offset
                }
            })
            .or_else(|| (!rows.is_empty()).then_some(0))?;
        let index = (first as u128 + u128::from(count.saturating_sub(1))) % rows.len() as u128;
        Some(rows[index as usize].key.clone())
    }
    pub(in crate::app) fn expand(
        &mut self,
        key: DiagnosticKey,
        severities: &[DiagnosticSeverity],
        editor: &EditorState,
    ) {
        if self.resolve(&key, editor).is_some() {
            self.expanded = Some((key, severities.to_vec()));
        }
    }
    pub(in crate::app) fn dismiss_expansion(&mut self) {
        self.expanded = None;
    }
    pub(super) fn expanded(&self, editor: &EditorState) -> Option<ExpandedDiagnosticState> {
        let (key, severities) = self.expanded.as_ref()?;
        let row = self.resolve(key, editor)?;
        let batch = self.batches.get(&(row.owner.clone(), row.uri.clone()))?;
        let id = batch.document.as_ref()?.id;
        let buffer = editor.buffers.get(id)?;
        Some(ExpandedDiagnosticState {
            buffer: Some(id),
            path: buffer.path.clone().unwrap_or_default(),
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
            // a reload or another file invalidates them.
            if text.revision() == revision
                || !document.editor.same_editor(&editor.window_version())
                || buffer.path != document.path
                || !text.source_version().same_instance(&document.text)
                || text.map_offset_since(revision, ByteOffset(0)).is_none()
            {
                continue;
            }
            let length = ByteOffset(text.len_bytes());
            self.store.move_ranges(owner, uri, |range| {
                let start = text
                    .map_offset_since(revision, range.start)
                    .unwrap_or(range.start)
                    .min(length);
                let end = text
                    .map_offset_since(revision, range.end)
                    .unwrap_or(range.end)
                    .clamp(start, length);
                crate::editor::text::ByteRange::new(start, end).unwrap_or(range)
            });
            // Sorted row indices may now refer to different diagnostics when ranges collapse.
            batch.epoch = Arc::new(());
            batch.revision = Some(text.revision());
            document.text = text.source_version();
            moved = true;
        }
        if moved {
            self.changed();
        }
    }
    pub(in crate::app) fn projection(&mut self, editor: &EditorState) -> Option<Vec<UiDiagnostic>> {
        self.clear_stale_documents(editor);
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        Some(
            self.rows
                .iter()
                .filter_map(|row| {
                    let batch = self
                        .batches
                        .get(&(row.problem.owner.clone(), row.problem.uri.clone()))?;
                    Some(UiDiagnostic {
                        buffer: batch.document.as_ref().map(|document| document.id),
                        path: batch.path.clone().unwrap_or_default(),
                        range: row.problem.range,
                        severity: row.problem.severity,
                        source: row.problem.source.clone(),
                        code: row.problem.code.as_ref().map(|code| {
                            code.as_str()
                                .map_or_else(|| code.to_string(), str::to_owned)
                        }),
                        message: row.problem.message.clone(),
                    })
                })
                .collect(),
        )
    }
}

pub(in crate::app) fn document_uri(id: BufferId, path: Option<&std::path::Path>) -> String {
    use slotmap::Key;
    path.map(crate::language::lsp::file_uri)
        .unwrap_or_else(|| format!("dust-buffer://{}", id.data().as_ffi()))
}

use crate::app::App;

impl App {
    pub(in crate::app) fn sync_ui_diagnostics(&mut self) {
        if let Some(candidate) = self.diagnostics.projection(&self.state) {
            self.ui.diagnostics = candidate;
            self.window_session.diagnostics_changed();
        }
        self.ui.expanded_diagnostic = self.diagnostics.expanded(&self.state);
    }
}

use crate::app::commands::AppEffect;

use crate::app::commands::DiagnosticFilter;
use crate::app::commands::PickerKind;
use crate::app::commands::register;
use crate::editor::ArgumentSchema;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, kind) in [
        (
            "diagnostic.open",
            "Open workspace problems",
            PickerKind::Diagnostics,
        ),
        (
            "buffer.picker.open",
            "Open buffer picker",
            PickerKind::Buffers,
        ),
        (
            "command_palette.open",
            "Open command picker",
            PickerKind::Commands,
        ),
        ("workspace.root.list", "List projects", PickerKind::Roots),
        (
            "navigation.recents",
            "Open recent files",
            PickerKind::Recents,
        ),
        ("navigation.marks", "Open marks", PickerKind::Marks),
        ("navigation.jumps", "Open jump list", PickerKind::Jumps),
        (
            "navigation.changes",
            "Open change list",
            PickerKind::Changes,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::OpenPicker(kind),
        )?;
    }
    for (id, title, direction, filter) in [
        (
            "diagnostic.next",
            "Go to next error or warning",
            1,
            DiagnosticFilter::ErrorOrWarning,
        ),
        (
            "diagnostic.previous",
            "Go to previous error or warning",
            -1,
            DiagnosticFilter::ErrorOrWarning,
        ),
        (
            "diagnostic.error.next",
            "Go to next error",
            1,
            DiagnosticFilter::Error,
        ),
        (
            "diagnostic.error.previous",
            "Go to previous error",
            -1,
            DiagnosticFilter::Error,
        ),
        (
            "diagnostic.warning.next",
            "Go to next warning",
            1,
            DiagnosticFilter::Warning,
        ),
        (
            "diagnostic.warning.previous",
            "Go to previous warning",
            -1,
            DiagnosticFilter::Warning,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |invocation| AppEffect::DiagnosticNavigate {
                direction,
                filter,
                count: invocation.count.map_or(1, |count| count.get()),
            },
        )?;
    }
    Ok(())
}

impl App {
    pub(in crate::app) fn process_diagnostics_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::DiagnosticNavigate {
                direction,
                filter,
                count,
            } => {
                self.navigate_diagnostic(direction, filter, count);
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::diagnostics::*;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    #[test]
    fn equivalent_unsorted_reports_keep_keys_and_metadata_updates_replace_payloads() {
        let mut editor = EditorState::new("/workspace".into());
        let path = std::path::PathBuf::from("/workspace/file");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("abc"), Some(path.clone()))
            .unwrap();
        let uri = crate::language::lsp::file_uri(&path);
        let producer = DiagnosticProducer::new(DiagnosticOwner::new("test").unwrap());
        let mut owner = Diagnostics::new(10, 10);
        owner.activate(producer.clone());
        let batch = |data: i32| DiagnosticBatch {
            producer: producer.clone(),
            uri: uri.clone(),
            document: DiagnosticDocument::capture(&editor, buffer),
            validity: DiagnosticValidity::RequestedSnapshot,
            items: [2, 0]
                .into_iter()
                .map(|at| Diagnostic {
                    uri: uri.clone(),
                    version: None,
                    range: ByteRange::empty(ByteOffset(at)),
                    severity: DiagnosticSeverity::Error,
                    source: "test".into(),
                    code: None,
                    message: "bad".into(),
                    related: Vec::new(),
                    tags: Vec::new(),
                    fixes: Vec::new(),
                    data: Some(serde_json::json!(data)),
                    timestamp: std::time::SystemTime::UNIX_EPOCH,
                })
                .collect(),
        };
        owner.publish(batch(1), &editor).unwrap();
        let key = owner.choices().next().unwrap().0;
        owner.publish(batch(1), &editor).unwrap();
        assert!(owner.resolve(&key, &editor).is_some());
        owner.publish(batch(2), &editor).unwrap();
        assert_eq!(
            owner.within(&uri, ByteOffset(0), ByteOffset(3))[0].data,
            Some(serde_json::json!(2))
        );
    }

    #[test]
    fn diagnostic_sources_keep_canonical_keys_limits_and_expansion_scoped() {
        let root = tempfile::tempdir().unwrap();
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("abc"), Some(path.clone()))
            .unwrap();
        let uri = crate::language::lsp::file_uri(&path);
        let old = DiagnosticProducer::new(DiagnosticOwner::new("same name").unwrap());
        let other = DiagnosticProducer::new(DiagnosticOwner::new("independent").unwrap());
        let mut owner = Diagnostics::new(4, 2);
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
        editor
            .open_text(TextBuffer::from_text("new"), Some(disk_path))
            .unwrap();
        assert!(
            owner.resolve(&key, &editor).is_none(),
            "an unbound disk report cannot adopt a newly opened buffer"
        );
    }

    #[test]
    fn diagnostic_intersections_are_half_open_and_include_points() {
        let mut owner = Diagnostics::new(10, 10);
        owner.rows = [(0, 1), (1, 2), (2, 3), (1, 1), (2, 2)]
            .into_iter()
            .enumerate()
            .map(|(index, (start, end))| Row {
                key: DiagnosticKey {
                    epoch: Arc::new(()),
                    index,
                },
                problem: ProblemRow {
                    owner: DiagnosticOwner::new("test").unwrap(),
                    uri: "file:///file".into(),
                    range: ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
                    severity: DiagnosticSeverity::Error,
                    source: "test".into(),
                    code: None,
                    message: index.to_string(),
                    data: None,
                },
            })
            .collect();
        let messages = |start, end| {
            owner
                .within("file:///file", ByteOffset(start), ByteOffset(end))
                .into_iter()
                .map(|row| row.message.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(messages(1, 2), vec!["1", "3"]);
        assert_eq!(messages(1, 1), vec!["1", "3"]);
        assert_eq!(messages(2, 2), vec!["2", "4"]);
    }

    #[test]
    fn collapsed_diagnostics_cannot_reassign_existing_keys() {
        let mut editor = EditorState::new("/workspace".into());
        let path = PathBuf::from("/workspace/file");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("abcd"), Some(path.clone()))
            .unwrap();
        let uri = crate::language::lsp::file_uri(&path);
        let producer = DiagnosticProducer::new(DiagnosticOwner::new("test").unwrap());
        let mut owner = Diagnostics::new(10, 10);
        owner.activate(producer.clone());
        owner
            .publish(
                DiagnosticBatch {
                    producer,
                    uri: uri.clone(),
                    document: DiagnosticDocument::capture(&editor, buffer),
                    validity: DiagnosticValidity::RequestedSnapshot,
                    items: [
                        (1, DiagnosticSeverity::Warning),
                        (2, DiagnosticSeverity::Error),
                    ]
                    .into_iter()
                    .map(|(at, severity)| Diagnostic {
                        uri: uri.clone(),
                        version: None,
                        range: ByteRange::empty(ByteOffset(at)),
                        severity,
                        source: "test".into(),
                        code: None,
                        message: format!("{severity:?}"),
                        related: Vec::new(),
                        tags: Vec::new(),
                        fixes: Vec::new(),
                        data: None,
                        timestamp: std::time::UNIX_EPOCH,
                    })
                    .collect(),
                },
                &editor,
            )
            .unwrap();
        let key = owner.choices().next().unwrap().0;
        assert_eq!(
            owner.resolve(&key, &editor).unwrap().severity,
            DiagnosticSeverity::Warning
        );
        let text = &mut editor.buffers[buffer].text;
        text.apply_edits(&crate::editor::text::TextEditSet::new(
            text.revision(),
            vec![crate::editor::text::TextEdit::new(
                ByteRange::new(ByteOffset(1), ByteOffset(3)).unwrap(),
                String::new(),
            )],
        ))
        .unwrap();
        owner.projection(&editor);
        assert!(owner.resolve(&key, &editor).is_none());
        assert_eq!(
            owner.choices().next().unwrap().1.severity,
            DiagnosticSeverity::Error
        );
    }

    #[test]
    fn diagnostics_move_with_edits_until_the_server_reports_again() {
        let root = tempfile::tempdir().unwrap();
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("abc\ndef\n"), Some(path.clone()))
            .unwrap();
        let uri = crate::language::lsp::file_uri(&path);
        let producer = DiagnosticProducer::new(DiagnosticOwner::new("server").unwrap());
        let mut owner = Diagnostics::new(4, 2);
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
            vec![crate::editor::text::TextEdit::new(
                ByteRange::new(ByteOffset(2), ByteOffset(3)).unwrap(),
                String::new(),
            )],
        );
        text.apply_edits(&edits).unwrap();
        let shown = owner.projection(&editor).expect("moved diagnostics");
        assert_eq!(
            shown[0].range,
            ByteRange::new(ByteOffset(2), ByteOffset(3)).unwrap()
        );
    }
}
