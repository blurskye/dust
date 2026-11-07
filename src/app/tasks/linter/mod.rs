//! Linter output parsing and pending source reads for each job.
use crate::app::diagnostics::DiagnosticBatch;
use crate::app::diagnostics::DiagnosticValidity;
use crate::app::diagnostics::Diagnostics;
use crate::app::documents::TaskCancellation;
use crate::app::documents::TaskReadKey;
use crate::app::documents::TaskReadReply;
use crate::app::documents::TaskReadStopped;
use crate::app::documents::TaskSubmission;
use crate::app::tasks::AdapterContext;
use crate::app::tasks::RunKey;
use crate::app::tasks::RunningTask;
use crate::app::tasks::TaskDiagnosticRun;
use crate::app::tasks::Tasks;
use crate::app::workspace::Workspace;
use crate::editor::EditorState;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::files::RequestId;
use crate::files::VfsResult;
use crate::workspace::Diagnostic;
use crate::workspace::DiagnosticProducer;
use crate::workspace::DiagnosticSeverity;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

struct Raw {
    line: usize,
    column: usize,
    encoding: crate::editor::text::PositionEncoding,
    severity: DiagnosticSeverity,
    message: String,
}
struct Read {
    key: TaskReadKey,
    path: PathBuf,
    raw: Vec<Raw>,
}
struct Build {
    name: String,
    key: RunKey,
    source: TaskDiagnosticRun,
    unread: BTreeMap<PathBuf, Vec<Raw>>,
    pending: HashMap<RequestId, Read>,
    grouped: BTreeMap<PathBuf, Vec<Diagnostic>>,
    maximum: u64,
    failed: Option<String>,
}
impl Build {
    pub(in crate::app) fn fail(&mut self, reason: impl Into<String>) {
        if self.failed.is_none() {
            self.failed = Some(reason.into());
        }
        self.unread.clear();
    }
}
#[derive(Default)]
pub(super) struct Linters {
    next: u64,
    builds: BTreeMap<u64, Build>,
}
impl Linters {
    pub(in crate::app) fn stop_run(&mut self, key: &RunKey) -> Option<DiagnosticProducer> {
        let mut producer = None;
        for build in self
            .builds
            .values_mut()
            .filter(|build| std::sync::Arc::ptr_eq(&build.key.epoch, &key.epoch))
        {
            producer = Some(build.source.producer().clone());
            build.fail("linter stopped; accepted reads may still complete");
        }
        producer
    }
    pub(in crate::app) fn invalidate(&mut self, reason: &str) {
        for build in self.builds.values_mut() {
            build.fail(reason);
        }
    }
}
/// Live source borrow spans request materialization, real submission and registration.
pub(in crate::app) struct LinterReadLoan<'a> {
    build: &'a mut Build,
    editor: &'a EditorState,
    _workspace: &'a Workspace,
    _configuration: &'a u64,
    _diagnostics: &'a Diagnostics,
    roots: &'a [crate::workspace::WorkspaceRoot],
}
impl LinterReadLoan<'_> {
    pub(in crate::app) fn request(
        &self,
        worker: &std::sync::Arc<()>,
    ) -> Result<(PathBuf, Vec<crate::files::RootIdentity>, u64), String> {
        if self
            .build
            .pending
            .values()
            .any(|read| !read.key.belongs_to_worker(worker))
        {
            return Err("linter read worker changed with outstanding reads".into());
        }
        let path = self
            .build
            .unread
            .first_key_value()
            .ok_or("linter read is stale")?
            .0;
        self.build.source.checked_document(path, self.editor)?;
        Ok((
            path.clone(),
            self.roots
                .iter()
                .map(|root| root.identity.clone())
                .collect(),
            self.build.maximum,
        ))
    }
    pub(in crate::app) fn finish(self, submission: TaskSubmission) -> Result<bool, String> {
        match submission.into_result() {
            Ok(None) => Ok(false),
            Ok(Some(key)) => {
                let (path, raw) = self
                    .build
                    .unread
                    .pop_first()
                    .expect("source loan retains its pending read");
                self.build.pending.insert(key.id(), Read { key, path, raw });
                Ok(true)
            }
            Err(error) => {
                self.build.fail(error.clone());
                Err(error)
            }
        }
    }
}
pub(in crate::app) enum LinterUpdate {
    Publish {
        producer: DiagnosticProducer,
        batches: Vec<DiagnosticBatch>,
    },
    Failed(String),
}
impl Tasks {
    pub(in crate::app) fn begin_linter(
        &mut self,
        task: RunningTask,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
        diagnostics: &Diagnostics,
        limits: (usize, u64),
    ) -> Result<(), String> {
        let (maximum_items, maximum_bytes) = limits;
        if !self.current(&task, workspace, configuration) {
            return Err("linter run is stale".into());
        }
        let AdapterContext::Linter { matcher, .. } = &task.adapter else {
            return Ok(());
        };
        let source = task
            .diagnostic_source
            .as_ref()
            .ok_or("linter has no producing source")?;
        if !source.editor_current(editor)
            || !source.current(diagnostics, workspace, configuration)
            || configuration == u64::MAX
            || maximum_bytes == 0
            || maximum_items == 0
            || self.linters.builds.len() >= 64
        {
            return Err("linter source is stale, exhausted or full".into());
        }
        let next = self
            .linters
            .next
            .checked_add(1)
            .ok_or("linter build identity exhausted")?;
        // Parse only once; every matched record must be usable before any source read is sent.
        let text = std::str::from_utf8(&task.output).map_err(|_| "linter output is not UTF-8")?;
        let regex = self
            .matchers
            .get(&task.name)
            .or_else(|| self.matchers.get(&task.requested))
            .ok_or("linter matcher unavailable")?;
        let mut raw = BTreeMap::<PathBuf, Vec<Raw>>::new();
        let mut paths = HashMap::<String, PathBuf>::new();
        for (index, captures) in regex
            .captures_iter(text)
            .take(maximum_items.min(65536).saturating_add(1))
            .enumerate()
        {
            if index >= maximum_items.min(65536) {
                return Err("linter output exceeds its diagnostic limit".into());
            }
            let path = captures
                .name(&matcher.path_group)
                .ok_or("linter record has no path")?
                .as_str();
            let path = if let Some(resolved) = paths.get(path) {
                resolved.clone()
            } else {
                let reported = Path::new(path);
                let joined = if reported.is_absolute() {
                    reported.to_path_buf()
                } else {
                    task.cwd.join(reported)
                };
                let resolved = crate::app::tasks::sources::resolve_source_path(&joined)
                    .map_err(|error| format!("cannot resolve linter source: {error}"))?;
                if !resolved.starts_with(&task.root) {
                    return Err("linter path is outside its task root".into());
                }
                paths.insert(path.to_owned(), resolved.clone());
                resolved
            };
            let line = captures
                .name(&matcher.line_group)
                .and_then(|c| c.as_str().parse::<usize>().ok())
                .filter(|v| *v > 0)
                .ok_or("invalid linter line")?
                - 1;
            let column = match matcher
                .column_group
                .as_ref()
                .and_then(|name| captures.name(name))
            {
                Some(c) => {
                    c.as_str()
                        .parse::<usize>()
                        .ok()
                        .filter(|v| *v > 0)
                        .ok_or("invalid linter column")?
                        - 1
                }
                None => 0,
            };
            let severity = matcher
                .severity_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .map_or(DiagnosticSeverity::Error, |c| {
                    match c.as_str().to_ascii_lowercase().as_str() {
                        "warn" | "warning" => DiagnosticSeverity::Warning,
                        "info" | "information" => DiagnosticSeverity::Information,
                        "hint" => DiagnosticSeverity::Hint,
                        _ => DiagnosticSeverity::Error,
                    }
                });
            let message = captures
                .name(&matcher.message_group)
                .ok_or("linter record has no message")?
                .as_str()
                .chars()
                .take(4096)
                .collect();
            if !raw.contains_key(&path) && raw.len() >= 1024 {
                return Err("too many linter source files".into());
            }
            raw.entry(path).or_default().push(Raw {
                line,
                column,
                encoding: matcher
                    .column_encoding
                    .unwrap_or(crate::editor::text::PositionEncoding::Utf8),
                severity,
                message,
            });
        }
        let mut grouped = BTreeMap::new();
        let mut unread = BTreeMap::new();
        for (path, raw) in raw {
            source.checked_document(&path, editor)?;
            if let Some(buffer) = editor.buffers.iter().find_map(|(_, b)| {
                b.path
                    .as_ref()
                    .filter(|p| crate::app::tasks::sources::source_path(p) == path)
                    .map(|_| b)
            }) {
                grouped.insert(
                    path.clone(),
                    convert(
                        &path,
                        raw,
                        &buffer.text,
                        Some(buffer.text.revision()),
                        &task.requested,
                    )?,
                );
            } else {
                unread.insert(path, raw);
            }
        }
        self.linters.next = next;
        self.linters.builds.insert(
            next,
            Build {
                name: task.requested,
                key: task.key,
                source: source.clone(),
                unread,
                pending: HashMap::new(),
                grouped,
                maximum: maximum_bytes,
                failed: None,
            },
        );
        Ok(())
    }
    pub(in crate::app) fn linter_count(&self) -> usize {
        self.linters.builds.len()
    }
    pub(in crate::app) fn invalidate_linter_sources(
        &mut self,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
        diagnostics: &Diagnostics,
    ) {
        for build in self.linters.builds.values_mut() {
            if !build.source.editor_current(editor)
                || self.closed
                || !self
                    .keys
                    .get(&build.name)
                    .is_some_and(|key| std::sync::Arc::ptr_eq(&key.epoch, &build.key.epoch))
                || !build.source.current(diagnostics, workspace, configuration)
            {
                build.fail("linter producing run is stale");
                continue;
            }
            if build
                .unread
                .keys()
                .chain(build.grouped.keys())
                .chain(build.pending.values().map(|read| &read.path))
                .any(|path| build.source.checked_document(path, editor).is_err())
            {
                build.fail("linter source document changed");
            }
        }
    }
    pub(in crate::app) fn linter_read<'a>(
        &'a mut self,
        editor: &'a EditorState,
        workspace: &'a Workspace,
        configuration: &'a u64,
        diagnostics: &'a Diagnostics,
    ) -> Option<LinterReadLoan<'a>> {
        self.invalidate_linter_sources(editor, workspace, *configuration, diagnostics);
        let build = self
            .linters
            .builds
            .values_mut()
            .find(|b| b.failed.is_none() && !b.unread.is_empty())?;
        Some(LinterReadLoan {
            build,
            editor,
            _workspace: workspace,
            _configuration: configuration,
            _diagnostics: diagnostics,
            roots: workspace.roots(),
        })
    }
    pub(in crate::app) fn accept_linter_read(
        &mut self,
        reply: TaskReadReply,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
        diagnostics: &Diagnostics,
    ) -> Result<(), String> {
        if !self.linters.builds.values().any(|build| {
            build
                .pending
                .get(&reply.id())
                .is_some_and(|read| reply.matches(&read.key))
        }) {
            return Err("unowned or duplicate linter source terminal".into());
        }
        self.invalidate_linter_sources(editor, workspace, configuration, diagnostics);
        let Some(build) = self.linters.builds.values_mut().find(|build| {
            build
                .pending
                .get(&reply.id())
                .is_some_and(|read| reply.matches(&read.key))
        }) else {
            return Err("unowned or duplicate linter source terminal".into());
        };
        let read = build
            .pending
            .remove(&reply.id())
            .expect("matched source read");
        if build.failed.is_some() {
            return Ok(());
        }
        let result: Result<(), String> = (|| {
            let VfsResult::Read { path, result, .. } = reply.into_result() else {
                return Err("wrong linter source terminal".into());
            };
            if path != read.path {
                return Err("linter read path changed".into());
            }
            build.source.checked_document(&path, editor)?;
            let bytes = result.map_err(|e| e.to_string())?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > build.maximum {
                return Err("linter source exceeds its requested byte limit".into());
            }
            let text = TextBuffer::from_bytes(&bytes).map_err(|e| e.to_string())?;
            let items = convert(&path, read.raw, &text, None, &build.name)?;
            build.grouped.insert(path, items);
            Ok(())
        })();
        if let Err(error) = result {
            build.fail(error);
        }
        Ok(())
    }
    pub(in crate::app) fn linter_cancellations(&self) -> Vec<TaskReadKey> {
        self.linters
            .builds
            .values()
            .filter(|b| b.failed.is_some())
            .flat_map(|b| b.pending.values().map(|r| r.key.clone()))
            .collect()
    }
    pub(in crate::app) fn linter_cancelled(&mut self, cancel: TaskCancellation) {
        let key = cancel.into_key();
        for build in self.linters.builds.values_mut() {
            if build
                .pending
                .get(&key.id())
                .is_some_and(|r| r.key.same(&key))
            {
                build.pending.remove(&key.id());
                break;
            }
        }
    }
    pub(in crate::app) fn linter_worker_stopped(&mut self, stop: TaskReadStopped) {
        for build in self.linters.builds.values_mut() {
            if build.pending.values().any(|read| stop.matches(&read.key)) {
                build.fail("linter source worker stopped before completion");
                build.pending.retain(|_, read| !stop.matches(&read.key));
            }
        }
    }
    pub(in crate::app) fn finish_linter_reads(&mut self, reason: &str) -> usize {
        let count = self.linters.builds.values().map(|b| b.pending.len()).sum();
        self.linters.invalidate(reason);
        for build in self.linters.builds.values_mut() {
            build.pending.clear();
        }
        count
    }
    pub(in crate::app) fn next_linter_update(
        &mut self,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
        diagnostics: &Diagnostics,
    ) -> Option<LinterUpdate> {
        self.invalidate_linter_sources(editor, workspace, configuration, diagnostics);
        let id = *self
            .linters
            .builds
            .iter()
            .find(|(_, b)| b.unread.is_empty() && b.pending.is_empty())?
            .0;
        let build = self.linters.builds.remove(&id)?;
        if let Some(error) = build.failed {
            return Some(LinterUpdate::Failed(error));
        }
        let result = build
            .grouped
            .into_iter()
            .map(|(path, mut items)| {
                let document = build.source.document(&path, editor)?;
                // Reads share a canonical identity, but open-buffer diagnostics use the
                // buffer's own path, as required by the diagnostic publisher and gutter.
                let uri = crate::language::lsp::file_uri(
                    document
                        .as_ref()
                        .and_then(|document| document.path())
                        .unwrap_or(path.as_path()),
                );
                for item in &mut items {
                    item.uri = uri.clone();
                }
                Ok(DiagnosticBatch {
                    document,
                    producer: build.source.producer().clone(),
                    uri,
                    validity: DiagnosticValidity::DiskReport,
                    items,
                })
            })
            .collect::<Result<Vec<_>, String>>();
        Some(match result {
            Ok(batches) => LinterUpdate::Publish {
                producer: build.source.producer().clone(),
                batches,
            },
            Err(error) => LinterUpdate::Failed(error),
        })
    }
}
fn convert(
    path: &Path,
    mut raw: Vec<Raw>,
    text: &TextBuffer,
    version: Option<crate::editor::text::BufferRevision>,
    source: &str,
) -> Result<Vec<Diagnostic>, String> {
    let uri = crate::language::lsp::file_uri(path);
    // Sorted positions let each encoding walk a source line only once.
    raw.sort_by_key(|raw| (raw.line, raw.encoding.lsp_name(), raw.column));
    let mut line = None;
    let mut line_text = String::new();
    let mut line_start = 0;
    let mut encoding = None;
    let mut units = 0;
    let mut byte_column = 0;
    raw.into_iter()
        .map(|raw| {
            if raw.line >= text.len_lines() {
                return Err("linter line is outside its source".into());
            }
            if line != Some(raw.line) {
                line_text = text.line(raw.line).map_err(|e| e.to_string())?;
                line_start = text
                    .line_to_byte(raw.line)
                    .map_err(|e| e.to_string())?
                    .get();
                line = Some(raw.line);
                encoding = None;
            }
            if encoding != Some(raw.encoding) {
                encoding = Some(raw.encoding);
                units = 0;
                byte_column = 0;
            }
            let content = line_text.strip_suffix('\n').unwrap_or(&line_text);
            while units < raw.column && byte_column < content.len() {
                let scalar = content[byte_column..]
                    .chars()
                    .next()
                    .expect("remaining scalar");
                let width = match raw.encoding {
                    crate::editor::text::PositionEncoding::Utf8 => scalar.len_utf8(),
                    crate::editor::text::PositionEncoding::Utf16 => scalar.len_utf16(),
                    crate::editor::text::PositionEncoding::Utf32 => 1,
                };
                if width > raw.column - units {
                    return Err("linter column is outside its source or splits UTF-8".into());
                }
                units += width;
                byte_column += scalar.len_utf8();
            }
            if units != raw.column {
                return Err("linter column is outside its source".into());
            }
            let start = crate::editor::text::ByteOffset(line_start + byte_column);
            let offset = start.get();
            let end = text
                .line_to_byte(raw.line.saturating_add(1))
                .map(|next| next.get().saturating_sub(1))
                .unwrap_or(text.len_bytes());
            if offset > end {
                return Err("linter column crosses its source line".into());
            }

            Ok(Diagnostic {
                uri: uri.clone(),
                version,
                range: ByteRange::new(start, start).map_err(|e| e.to_string())?,
                severity: raw.severity,
                source: source.into(),
                code: None,
                message: raw.message,
                related: Vec::new(),
                tags: Vec::new(),
                fixes: Vec::new(),
                data: None,
                timestamp: crate::clock::system_time(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
