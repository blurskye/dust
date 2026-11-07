//! Diagnostics grouped by source and document revision.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::editor::text::BufferRevision;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::workspace::ABSOLUTE_DIAGNOSTIC_LIMIT;

use crate::workspace::edit::WorkspaceError;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticOwner(pub String);

impl DiagnosticOwner {
    pub fn new(value: impl Into<String>) -> Result<Self, WorkspaceError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(WorkspaceError::InvalidDiagnosticOwner);
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedDiagnostic {
    pub uri: String,
    /// Source coordinates, retained even when the related file is not open.
    pub range: std::ops::Range<(usize, usize)>,
    pub encoding: crate::editor::text::PositionEncoding,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub uri: String,
    pub version: Option<BufferRevision>,
    pub range: ByteRange,
    pub severity: DiagnosticSeverity,
    pub source: String,
    pub code: Option<serde_json::Value>,
    pub message: String,
    pub related: Vec<RelatedDiagnostic>,
    pub tags: Vec<String>,
    pub fixes: Vec<String>,
    pub data: Option<serde_json::Value>,
    pub timestamp: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProblemRow {
    pub owner: DiagnosticOwner,
    pub uri: String,
    pub range: ByteRange,
    pub severity: DiagnosticSeverity,
    pub source: String,
    pub code: Option<serde_json::Value>,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct DiagnosticStore {
    maximum_total: usize,
    maximum_per_owner_uri: usize,
    sets: BTreeMap<(DiagnosticOwner, String), Vec<Diagnostic>>,
    total: usize,
}

impl DiagnosticStore {
    #[must_use]
    pub fn new(maximum_total: usize, maximum_per_owner_uri: usize) -> Self {
        Self {
            maximum_total: maximum_total.clamp(1, ABSOLUTE_DIAGNOSTIC_LIMIT),
            maximum_per_owner_uri: maximum_per_owner_uri.clamp(1, ABSOLUTE_DIAGNOSTIC_LIMIT),
            sets: BTreeMap::new(),
            total: 0,
        }
    }

    pub(crate) fn limits(&self) -> (usize, usize) {
        (self.maximum_total, self.maximum_per_owner_uri)
    }

    pub fn replace(
        &mut self,
        owner: DiagnosticOwner,
        uri: String,
        version: Option<BufferRevision>,
        current_revision: Option<BufferRevision>,
        mut diagnostics: Vec<Diagnostic>,
    ) -> Result<(), WorkspaceError> {
        Self::check_set(&uri, version, current_revision, &diagnostics)?;
        diagnostics.truncate(self.maximum_per_owner_uri);
        let key = (owner, uri);
        let previous = self.sets.get(&key).map_or(0, Vec::len);
        let available = self
            .maximum_total
            .saturating_sub(self.total.saturating_sub(previous));
        diagnostics.truncate(available);
        if diagnostics.is_empty() {
            self.sets.remove(&key);
        } else {
            self.sets.insert(key, diagnostics);
        }
        self.total = self.sets.values().map(Vec::len).sum();
        Ok(())
    }

    pub(crate) fn check_set(
        uri: &str,
        version: Option<BufferRevision>,
        current_revision: Option<BufferRevision>,
        diagnostics: &[Diagnostic],
    ) -> Result<(), WorkspaceError> {
        if version.is_some() && current_revision != version {
            return Err(WorkspaceError::StaleDiagnostics {
                expected: current_revision,
                actual: version,
            });
        }
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.uri != uri || diagnostic.version != version)
        {
            return Err(WorkspaceError::MixedDiagnosticSet);
        }
        Ok(())
    }

    pub fn clear_owner_uri(&mut self, owner: &DiagnosticOwner, uri: &str) {
        self.sets.remove(&(owner.clone(), uri.to_owned()));
        self.total = self.sets.values().map(Vec::len).sum();
    }

    /// Moves one owner's diagnostics for a file, as an edit moved the text under them.
    pub fn move_ranges(
        &mut self,
        owner: &DiagnosticOwner,
        uri: &str,
        mut move_range: impl FnMut(ByteRange) -> ByteRange,
    ) {
        if let Some(diagnostics) = self.sets.get_mut(&(owner.clone(), uri.to_owned())) {
            for diagnostic in diagnostics {
                diagnostic.range = move_range(diagnostic.range);
            }
        }
    }

    pub fn clear_owner(&mut self, owner: &DiagnosticOwner) {
        self.sets.retain(|(candidate, _), _| candidate != owner);
        self.total = self.sets.values().map(Vec::len).sum();
    }

    pub fn clear_uri(&mut self, uri: &str) {
        self.sets.retain(|(_, candidate), _| candidate != uri);
        self.total = self.sets.values().map(Vec::len).sum();
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.total
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.total == 0
    }

    #[must_use]
    pub fn problems(
        &self,
        severity: Option<DiagnosticSeverity>,
        source: Option<&str>,
    ) -> Vec<ProblemRow> {
        let mut rows = self
            .sets
            .iter()
            .flat_map(|((owner, _), diagnostics)| {
                diagnostics
                    .iter()
                    .filter(|diagnostic| {
                        severity.is_none_or(|value| value == diagnostic.severity)
                            && source.is_none_or(|value| value == diagnostic.source)
                    })
                    .map(|diagnostic| ProblemRow {
                        owner: owner.clone(),
                        uri: diagnostic.uri.clone(),
                        range: diagnostic.range,
                        severity: diagnostic.severity,
                        source: diagnostic.source.clone(),
                        code: diagnostic.code.clone(),
                        message: diagnostic.message.clone(),
                        data: diagnostic.data.clone(),
                    })
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.uri
                .cmp(&right.uri)
                .then(left.range.start.cmp(&right.range.start))
                .then(left.severity.cmp(&right.severity))
        });
        rows
    }

    #[must_use]
    pub fn adjacent(
        &self,
        uri: &str,
        offset: ByteOffset,
        direction: i8,
        severities: &[DiagnosticSeverity],
    ) -> Option<ProblemRow> {
        let rows = self
            .problems(None, None)
            .into_iter()
            .filter(|row| row.uri == uri && severities.contains(&row.severity))
            .collect::<Vec<_>>();
        if direction < 0 {
            rows.iter()
                .rev()
                .find(|row| row.range.start < offset)
                .or_else(|| rows.last())
                .cloned()
        } else {
            rows.iter()
                .find(|row| row.range.start > offset)
                .or_else(|| rows.first())
                .cloned()
        }
    }
}

/// Internal producer instance, separate from its user-visible owner name.
#[derive(Clone, Debug)]
pub(crate) struct DiagnosticProducer {
    owner: DiagnosticOwner,
    origin: std::sync::Arc<()>,
}
impl DiagnosticProducer {
    pub(crate) fn new(owner: DiagnosticOwner) -> Self {
        Self {
            owner,
            origin: std::sync::Arc::new(()),
        }
    }
    pub(crate) fn from_runtime(owner: DiagnosticOwner, origin: std::sync::Arc<()>) -> Self {
        Self { owner, origin }
    }
    pub(crate) fn owner(&self) -> &DiagnosticOwner {
        &self.owner
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.owner == other.owner && std::sync::Arc::ptr_eq(&self.origin, &other.origin)
    }
}
