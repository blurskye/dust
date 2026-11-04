//! Original task document captures and source-specific result interpretation.
use super::{AdapterContext, RunningTask, TaskSession, TestCaseResult};
use crate::{
    editor::{BufferId, EditorState, EditorWindowVersion, text::TextVersion},
    workspace::DiagnosticOwner,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
/// Per-run source capture; never reconstructed from a task name at result time.
#[derive(Clone, Debug)]
pub(in crate::editor::session) struct TaskDiagnosticRun {
    producer: crate::workspace::DiagnosticProducer,
    editor: crate::editor::EditorWindowVersion,
    documents: std::sync::Arc<
        BTreeMap<
            PathBuf,
            (
                crate::editor::session::diagnostics::DiagnosticDocument,
                bool,
            ),
        >,
    >,
    workspace: crate::editor::session::workspace::WorkspaceVersion,
    configuration: u64,
}
impl TaskDiagnosticRun {
    pub(super) fn editor_current(&self, state: &EditorState) -> bool {
        self.editor.same_editor(&state.window_version())
            && state.window_version().revision().is_some()
    }
    pub(in crate::editor::session) fn capture(
        name: &str,
        state: &crate::editor::EditorState,
        workspace: &crate::editor::session::workspace::WorkspaceSession,
        configuration: u64,
    ) -> Result<Self, String> {
        if configuration == u64::MAX || state.window_version().revision().is_none() {
            return Err("linter source revision exhausted".into());
        }
        if state.buffers.len() > 4096 {
            return Err("too many open documents for a linter source capture".into());
        }
        Ok(Self {
            editor: state.window_version(),
            producer: crate::workspace::DiagnosticProducer::new(
                DiagnosticOwner::new(format!("task:{name}")).map_err(|e| e.to_string())?,
            ),
            documents: std::sync::Arc::new(
                state
                    .buffers
                    .iter()
                    .filter_map(|(id, buffer)| {
                        Some((
                            source_path(buffer.path.as_deref()?),
                            (
                                crate::editor::session::diagnostics::DiagnosticDocument::capture(
                                    state, id,
                                )?,
                                !buffer.dirty,
                            ),
                        ))
                    })
                    .collect(),
            ),
            workspace: workspace.version(),
            configuration,
        })
    }
    pub(in crate::editor::session) fn producer(&self) -> &crate::workspace::DiagnosticProducer {
        &self.producer
    }
    pub(in crate::editor::session) fn current(
        &self,
        diagnostics: &crate::editor::session::diagnostics::DiagnosticSession,
        workspace: &crate::editor::session::workspace::WorkspaceSession,
        configuration: u64,
    ) -> bool {
        diagnostics.current(&self.producer)
            && self.workspace == workspace.version()
            && self.configuration == configuration
    }
    pub(in crate::editor::session) fn document(
        &self,
        path: &Path,
        state: &EditorState,
    ) -> Result<Option<crate::editor::session::diagnostics::DiagnosticDocument>, String> {
        self.checked_document(path, state)
            .map(|document| document.cloned())
    }
    pub(in crate::editor::session) fn checked_document(
        &self,
        path: &Path,
        state: &crate::editor::EditorState,
    ) -> Result<Option<&crate::editor::session::diagnostics::DiagnosticDocument>, String> {
        if !self.editor.same_editor(&state.window_version())
            || state.window_version().revision().is_none()
        {
            return Err("linter belongs to a foreign or exhausted editor".into());
        }
        match self.documents.get(path) {
            Some((document, true))
                if document.current(state)
                    && state
                        .buffers
                        .iter()
                        .any(|(_, b)| b.path.as_deref().is_some_and(|p| source_path(p) == path) && !b.dirty) =>
            {
                Ok(Some(document))
            }
            None if !state
                .buffers
                .iter()
                .any(|(_, b)| b.path.as_deref().is_some_and(|p| source_path(p) == path)) =>
            {
                Ok(None)
            }
            _ => Err("linter source document changed or was unsaved".into()),
        }
    }
}

pub(super) fn source_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl TaskSession {
    pub(super) fn apply_test_result(&mut self, task: &RunningTask) {
        let AdapterContext::Test { matcher } = &task.adapter else {
            return;
        };
        if task.name != task.requested {
            return;
        }
        let bytes = &task.output;
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let Some(regex) = self.matchers.get(&task.requested) else {
            return;
        };
        let cases = regex
            .captures_iter(text)
            .take(10_000)
            .map(|captures| TestCaseResult {
                name: matcher
                    .name_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(512).collect())
                    .unwrap_or_else(|| "test".into()),
                status: matcher
                    .status_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(32).collect())
                    .unwrap_or_else(|| "unknown".into()),
                duration: matcher
                    .duration_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(64).collect())
                    .unwrap_or_default(),
                message: captures
                    .name(&matcher.message_group)
                    .map(|value| value.as_str().chars().take(2048).collect())
                    .unwrap_or_default(),
                source: captures
                    .name(&matcher.path_group)
                    .map(|value| PathBuf::from(value.as_str()))
                    .map(|path| {
                        (
                            if path.is_absolute() {
                                path
                            } else {
                                task.cwd.join(path)
                            },
                            captures
                                .name(&matcher.line_group)
                                .and_then(|v| v.as_str().parse::<usize>().ok())
                                .unwrap_or(1)
                                .saturating_sub(1),
                            matcher
                                .column_group
                                .as_ref()
                                .and_then(|name| captures.name(name))
                                .and_then(|v| v.as_str().parse::<usize>().ok())
                                .unwrap_or(1)
                                .saturating_sub(1),
                        )
                    })
                    .filter(|source| source.0.starts_with(&task.root)),
            })
            .collect();
        self.tests.insert(task.requested.clone(), cases);
    }
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct FormatterSource {
    editor: EditorWindowVersion,
    buffer: BufferId,
    path: Option<PathBuf>,
    text: TextVersion,
}
impl FormatterSource {
    pub(in crate::editor::session) fn buffer(&self) -> BufferId {
        self.buffer
    }
    pub(in crate::editor::session) fn capture(
        editor: &EditorState,
        buffer: BufferId,
    ) -> Result<Self, String> {
        let state = editor
            .buffers
            .get(buffer)
            .ok_or("formatter buffer closed")?;
        if state.read_only || editor.window_version().revision().is_none() {
            return Err("formatter buffer is read-only or exhausted".into());
        }
        Ok(Self {
            editor: editor.window_version(),
            buffer,
            path: state.path.clone(),
            text: state.text.source_version(),
        })
    }
    pub(super) fn check(&self, editor: &EditorState) -> Result<(), String> {
        if self.editor.same_editor(&editor.window_version())
            && editor.window_version().revision().is_some()
            && editor.buffers.get(self.buffer).is_some_and(|buffer| {
                buffer.path == self.path
                    && buffer.text.source_version() == self.text
                    && !buffer.read_only
            })
        {
            Ok(())
        } else {
            Err("formatter document incarnation, content or ownership changed".into())
        }
    }
    pub(super) fn stdin(&self, editor: &EditorState, maximum: usize) -> Result<Vec<u8>, String> {
        self.check(editor)?;
        let text = &editor.buffers[self.buffer].text;
        if text.len_bytes() > maximum {
            return Err("formatter input exceeds its limit".into());
        }
        let bytes = text.to_bytes().to_vec();
        if bytes.len() > maximum {
            return Err("encoded formatter input exceeds its limit".into());
        }
        Ok(bytes)
    }
}
