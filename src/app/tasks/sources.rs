//! Original task document captures and source-specific result interpretation.
use crate::app::tasks::AdapterContext;
use crate::app::tasks::RunningTask;
use crate::app::tasks::Tasks;
use crate::app::tasks::TestCaseResult;
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::EditorWindowVersion;
use crate::editor::text::TextVersion;
use crate::workspace::DiagnosticOwner;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
/// Per-run source capture; never reconstructed from a task name at result time.
#[derive(Clone, Debug)]
pub(in crate::app) struct TaskDiagnosticRun {
    producer: crate::workspace::DiagnosticProducer,
    editor: crate::editor::EditorWindowVersion,
    documents:
        std::sync::Arc<BTreeMap<PathBuf, (crate::app::diagnostics::DiagnosticDocument, bool)>>,
    workspace: crate::app::workspace::WorkspaceVersion,
    configuration: u64,
}
impl TaskDiagnosticRun {
    pub(in crate::app) fn editor_current(&self, state: &EditorState) -> bool {
        self.editor.same_editor(&state.window_version())
            && state.window_version().revision().is_some()
    }
    pub(in crate::app) fn capture(
        name: &str,
        state: &crate::editor::EditorState,
        workspace: &crate::app::workspace::Workspace,
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
                                crate::app::diagnostics::DiagnosticDocument::capture(state, id)?,
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
    pub(in crate::app) fn producer(&self) -> &crate::workspace::DiagnosticProducer {
        &self.producer
    }
    pub(in crate::app) fn current(
        &self,
        diagnostics: &crate::app::diagnostics::Diagnostics,
        workspace: &crate::app::workspace::Workspace,
        configuration: u64,
    ) -> bool {
        diagnostics.current(&self.producer)
            && self.workspace == workspace.version()
            && self.configuration == configuration
    }
    pub(in crate::app) fn document(
        &self,
        path: &Path,
        state: &EditorState,
    ) -> Result<Option<crate::app::diagnostics::DiagnosticDocument>, String> {
        self.checked_document(path, state)
            .map(|document| document.cloned())
    }
    pub(in crate::app) fn checked_document(
        &self,
        path: &Path,
        state: &crate::editor::EditorState,
    ) -> Result<Option<&crate::app::diagnostics::DiagnosticDocument>, String> {
        if !self.editor.same_editor(&state.window_version())
            || state.window_version().revision().is_none()
        {
            return Err("linter belongs to a foreign or exhausted editor".into());
        }
        match self.documents.get(path) {
            Some((document, true))
                if document.current(state)
                    && state.buffers.iter().any(|(_, b)| {
                        b.path.as_deref().is_some_and(|p| source_path(p) == path) && !b.dirty
                    }) =>
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
    resolve_source_path(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn resolve_source_path(path: &Path) -> std::io::Result<PathBuf> {
    crate::files::canonicalize(path).or_else(|error| {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error);
        }
        // A new open buffer can have a missing file under a symlinked directory.
        let parent = crate::files::canonicalize(path.parent().ok_or(error)?)?;
        Ok(parent.join(
            path.file_name()
                .ok_or_else(|| std::io::Error::other("linter source has no filename"))?,
        ))
    })
}

impl Tasks {
    pub(in crate::app) fn apply_test_result(&mut self, task: &RunningTask) {
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
                    .map(|path| crate::app::picker::MatcherSource {
                        path: if path.is_absolute() {
                            path
                        } else {
                            task.cwd.join(path)
                        },
                        line: captures
                            .name(&matcher.line_group)
                            .and_then(|v| v.as_str().parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1),
                        column: matcher
                            .column_group
                            .as_ref()
                            .and_then(|name| captures.name(name))
                            .and_then(|v| v.as_str().parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1),
                        encoding: matcher
                            .column_encoding
                            .unwrap_or(crate::editor::text::PositionEncoding::Utf8),
                    })
                    .filter(|source| source.path.starts_with(&task.root)),
            })
            .collect();
        self.tests.insert(task.requested.clone(), cases);
    }
}

#[derive(Clone, Debug)]
pub(in crate::app) struct FormatterSource {
    editor: EditorWindowVersion,
    buffer: BufferId,
    path: Option<PathBuf>,
    text: TextVersion,
}
impl FormatterSource {
    pub(in crate::app) fn buffer(&self) -> BufferId {
        self.buffer
    }
    pub(in crate::app) fn capture(editor: &EditorState, buffer: BufferId) -> Result<Self, String> {
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
    pub(in crate::app) fn check(&self, editor: &EditorState) -> Result<(), String> {
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
            Err("formatter document instance, content or ownership changed".into())
        }
    }
    pub(in crate::app) fn stdin(
        &self,
        editor: &EditorState,
        maximum: usize,
    ) -> Result<Vec<u8>, String> {
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
