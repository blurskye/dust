//! Translate system imports to the native job, filesystem and syntax owners.

use super::{callback_call, wit};
use crate::app::App;
use crate::editor::{BufferId, CommandAccess, CommandAuthority};
use crate::files::workspace_access::{FileInfo, FileOperation, FileValue};
use crate::files::{RequestId, RootIdentity, VfsResult, VfsWorker};
use crate::plugins::PluginLoadId;
use crate::plugins::requests::SystemRequest;
use crate::process::{JobEvent, JobId, JobPolicy, JobRequest, JobStream, JobSupervisor};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone)]
struct Callback {
    plugin: String,
    load: PluginLoadId,
    handler: String,
    authority: CommandAuthority,
    token: u64,
}

struct FileCall {
    callback: Callback,
    root: RootIdentity,
    operation: FileOperation,
    buffers: Vec<(BufferId, PathBuf)>,
    written: bool,
}

#[derive(Default)]
pub(super) struct SystemAccess {
    jobs: Option<JobSupervisor>,
    processes: HashMap<JobId, Callback>,
    files: Option<VfsWorker>,
    pending_files: HashMap<RequestId, FileCall>,
    events: VecDeque<(Callback, wit::SystemEvent)>,
}

impl SystemAccess {
    fn shutdown(&mut self) {
        if let Some(jobs) = &mut self.jobs {
            jobs.begin_shutdown();
        }
        if let Some(files) = &mut self.files {
            files.begin_shutdown();
        }
    }

    pub(super) fn remove(&mut self, plugin: &str) {
        self.processes.retain(|job, callback| {
            if callback.plugin != plugin {
                return true;
            }
            if let Some(jobs) = &mut self.jobs {
                let _ = jobs.cancel(*job);
            }
            false
        });
        self.pending_files
            .retain(|_, call| call.callback.plugin != plugin);
        self.events
            .retain(|(callback, _)| callback.plugin != plugin);
    }

    pub(super) fn delivery(
        &self,
        plugin: &str,
        handler: &str,
        authority: &CommandAuthority,
    ) -> Vec<wit::SystemEvent> {
        self.events
            .iter()
            .filter(|(callback, _)| {
                callback.plugin == plugin
                    && callback.handler == handler
                    && &callback.authority == authority
            })
            .take(64)
            .map(|(_, event)| event.clone())
            .collect()
    }

    pub(super) fn delivered(
        &mut self,
        plugin: &str,
        handler: &str,
        authority: &CommandAuthority,
        mut count: usize,
    ) {
        self.events.retain(|(callback, _)| {
            if count != 0
                && callback.plugin == plugin
                && callback.handler == handler
                && &callback.authority == authority
            {
                count -= 1;
                false
            } else {
                true
            }
        });
    }

    fn reply(&mut self, callback: Callback, value: Result<wit::SystemValue, String>) {
        self.events.push_back((
            callback.clone(),
            wit::SystemEvent {
                request: callback.token,
                value,
            },
        ));
    }
}

impl App {
    pub(in crate::app) fn shutdown_plugin_system(&mut self) {
        self.extension.abi.system.shutdown();
    }

    pub(super) fn apply_system_request(
        &mut self,
        plugin: &str,
        token: u64,
        request: SystemRequest,
    ) -> Result<(), String> {
        if self.extension.abi.system.events.len() >= 256
            || self.extension.abi.system.pending_files.len() >= 64
        {
            return Err("system request queue is full".into());
        }
        let handler = match &request {
            SystemRequest::Start(request) => request.handler.clone(),
            SystemRequest::File(_, handler)
            | SystemRequest::Query(_, handler)
            | SystemRequest::Node(_, _, handler) => handler.clone(),
            SystemRequest::Input(input) => self
                .process_callback(plugin, input.process)?
                .1
                .handler
                .clone(),
            SystemRequest::Kill(process) => {
                self.process_callback(plugin, *process)?.1.handler.clone()
            }
        };
        self.handler(plugin, &handler)?;
        let callback = Callback {
            plugin: plugin.into(),
            load: self.extension.load_id(plugin).ok_or("plugin unloaded")?,
            handler,
            authority: self.effect_authority.clone(),
            token,
        };
        let result = self.perform_system_request(&callback, request);
        match result {
            Ok(Some(value)) => self.extension.abi.system.reply(callback, Ok(value)),
            Ok(None) => {}
            Err(error) => self.extension.abi.system.reply(callback, Err(error)),
        }
        Ok(())
    }

    fn perform_system_request(
        &mut self,
        callback: &Callback,
        request: SystemRequest,
    ) -> Result<Option<wit::SystemValue>, String> {
        match request {
            SystemRequest::Start(request) => {
                self.effect_authority
                    .require(CommandAccess::Process)
                    .map_err(|e| e.to_string())?;
                self.workspace
                    .execution_allowed()
                    .map_err(|_| "process requires a trusted workspace")?;
                if self
                    .extension
                    .abi
                    .system
                    .processes
                    .values()
                    .filter(|call| call.plugin == callback.plugin)
                    .count()
                    >= 8
                {
                    return Err("process limit exceeded (8 per plugin)".into());
                }
                let root = self.system_root()?;
                let cwd = root.canonical_path.join(request.cwd);
                if !cwd.starts_with(&root.canonical_path) {
                    return Err("process cwd is outside the workspace".into());
                }
                let job = JobRequest {
                    program: request.program.into(),
                    argv0: None,
                    arguments: request.arguments.into_iter().map(Into::into).collect(),
                    cwd,
                    environment: std::env::vars_os()
                        .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
                        .chain(
                            request
                                .environment
                                .into_iter()
                                .map(|(key, value)| (key, value.into())),
                        )
                        .collect(),
                    root_revision: self.workspace.root_revision(),
                    stdin: request.stdin,
                    policy: JobPolicy {
                        maximum_output_bytes: 1024 * 1024,
                        maximum_line_bytes: 8192,
                        maximum_stdin_bytes: 1024 * 1024,
                        timeout: Duration::from_secs(86_400),
                    },
                };
                let id = self
                    .extension
                    .abi
                    .system
                    .jobs
                    .get_or_insert_with(|| JobSupervisor::new(64))
                    .start_streaming(job, root)
                    .map_err(|e| e.to_string())?;
                self.extension
                    .abi
                    .system
                    .processes
                    .insert(id, callback.clone());
                Ok(None)
            }
            SystemRequest::Input(input) => {
                self.effect_authority
                    .require(CommandAccess::Process)
                    .map_err(|e| e.to_string())?;
                let (job, _) = self.process_callback(&callback.plugin, input.process)?;
                self.extension
                    .abi
                    .system
                    .jobs
                    .as_mut()
                    .ok_or("process runner stopped")?
                    .write_stdin(job, input.bytes, input.close)
                    .map_err(|e| e.to_string())?;
                Ok(Some(wit::SystemValue::Done))
            }
            SystemRequest::Kill(process) => {
                self.effect_authority
                    .require(CommandAccess::Process)
                    .map_err(|e| e.to_string())?;
                let (job, _) = self.process_callback(&callback.plugin, process)?;
                self.extension
                    .abi
                    .system
                    .jobs
                    .as_mut()
                    .ok_or("process runner stopped")?
                    .cancel(job)
                    .map_err(|e| e.to_string())?;
                Ok(Some(wit::SystemValue::Done))
            }
            SystemRequest::File(operation, _) => {
                self.effect_authority
                    .require(CommandAccess::Files)
                    .map_err(|e| e.to_string())?;
                let root = self.system_root()?;
                let path = |path: String| root.canonical_path.join(path);
                let operation = match operation {
                    wit::FileOperation::Read(name) => FileOperation::Read(path(name)),
                    wit::FileOperation::Write(write) => {
                        FileOperation::Write(path(write.path), write.bytes)
                    }
                    wit::FileOperation::CreateDirectory(name) => {
                        FileOperation::CreateDirectory(path(name))
                    }
                    wit::FileOperation::ListDirectory(name) => {
                        FileOperation::ListDirectory(path(name))
                    }
                    wit::FileOperation::Stat(name) => FileOperation::Stat(path(name)),
                    wit::FileOperation::Rename(rename) => {
                        FileOperation::Rename(path(rename.source), path(rename.target))
                    }
                    wit::FileOperation::Delete(name) => FileOperation::Delete(path(name)),
                };
                self.resolve_system_file(FileCall {
                    callback: callback.clone(),
                    root,
                    operation,
                    buffers: Vec::new(),
                    written: false,
                })?;
                Ok(None)
            }
            SystemRequest::Query(query, _) => {
                let parsed = self.system_syntax(query.buffer)?;
                let captures = parsed
                    .query(
                        &query.query,
                        query
                            .range
                            .map(|range| range.start as usize..range.end as usize),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(Some(wit::SystemValue::Syntax(wit::SyntaxValue::Captures(
                    captures
                        .into_iter()
                        .map(|capture| wit::SyntaxCapture {
                            name: capture.name,
                            range: range(capture.range),
                        })
                        .collect(),
                ))))
            }
            SystemRequest::Node(buffer, position, _) => {
                let nodes = self
                    .system_syntax(buffer)?
                    .node_at(position as usize)
                    .map_err(|e| e.to_string())?;
                Ok(Some(wit::SystemValue::Syntax(wit::SyntaxValue::Nodes(
                    nodes
                        .into_iter()
                        .map(|node| wit::SyntaxNode {
                            kind: node.kind,
                            range: range(node.range),
                        })
                        .collect(),
                ))))
            }
        }
    }

    fn system_root(&self) -> Result<RootIdentity, String> {
        self.workspace
            .roots()
            .iter()
            .find(|root| root.identity.canonical_path == self.state.workspace_root)
            .or_else(|| self.workspace.roots().first())
            .map(|root| root.identity.clone())
            .ok_or_else(|| "workspace has no root".into())
    }

    fn process_callback(&self, plugin: &str, process: u64) -> Result<(JobId, Callback), String> {
        self.extension
            .abi
            .system
            .processes
            .iter()
            .find(|(_, call)| call.plugin == plugin && call.token == process)
            .map(|(job, call)| (*job, call.clone()))
            .ok_or_else(|| "unknown process owned by this plugin".into())
    }

    fn system_syntax(&self, buffer: u64) -> Result<&crate::language::syntax::SyntaxResult, String> {
        let buffer = slotmap::KeyData::from_ffi(buffer).into();
        let text = &self.state.buffers.get(buffer).ok_or("buffer closed")?.text;
        let parsed = self
            .syntax
            .result(buffer)
            .ok_or("buffer has no parsed syntax tree")?;
        if self.syntax.source(buffer) != Some(&text.source_version()) {
            return Err("buffer syntax tree is not current; retry after parsing".into());
        }
        Ok(parsed)
    }

    fn system_buffer_paths(&self) -> Vec<(BufferId, PathBuf)> {
        self.state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| buffer.path.clone().map(|path| (id, path)))
            .collect()
    }

    fn resolve_system_file(&mut self, mut call: FileCall) -> Result<(), String> {
        call.buffers = self.system_buffer_paths();
        let paths = call
            .operation
            .paths()
            .into_iter()
            .chain(call.buffers.iter().map(|(_, path)| path.clone()))
            .collect();
        let id = self
            .extension
            .abi
            .system
            .files
            .get_or_insert_with(|| VfsWorker::start(64))
            .resolve_paths(paths)
            .map_err(|e| e.to_string())?;
        self.extension.abi.system.pending_files.insert(id, call);
        Ok(())
    }

    fn accept_system_file(&mut self, result: VfsResult) {
        let Some(mut call) = self
            .extension
            .abi
            .system
            .pending_files
            .remove(&result.request_id())
        else {
            return;
        };
        if !self
            .extension
            .is_active_load(&call.callback.plugin, call.callback.load)
        {
            return;
        }
        let response = (|| -> Result<Option<wit::FileValue>, String> {
            let authority = call
                .callback
                .authority
                .restrict(&self.plugin_authority(&call.callback.plugin));
            let previous = std::mem::replace(&mut self.effect_authority, authority);
            let allowed = self
                .effect_authority
                .require(CommandAccess::Files)
                .map_err(|e| e.to_string());
            self.effect_authority = previous;
            allowed?;
            if self.system_root()? != call.root {
                return Err("workspace changed during file request".into());
            }
            match result {
                VfsResult::PathsResolved { paths, .. } => {
                    if call.buffers != self.system_buffer_paths() {
                        self.resolve_system_file(FileCall {
                            callback: call.callback.clone(),
                            root: call.root.clone(),
                            operation: call.operation.clone(),
                            buffers: Vec::new(),
                            written: call.written,
                        })?;
                        return Ok(None);
                    }
                    let count = call.operation.paths().len();
                    call.operation.resolve(&paths[..count]);
                    for path in &paths[..count] {
                        if !path.starts_with(&call.root.canonical_path) {
                            return Err("file path is outside the workspace".into());
                        }
                    }
                    for ((id, _), path) in call.buffers.iter_mut().zip(&paths[count..]) {
                        if call.operation.changes(path) && self.state.buffers[*id].dirty {
                            return Err("file has unsaved changes in Dust".into());
                        }
                    }
                    for ((_, stored), path) in call.buffers.iter_mut().zip(&paths[count..]) {
                        *stored = path.clone();
                    }
                    if call.written {
                        self.reload_system_write(&call)?;
                        return Ok(Some(wit::FileValue::Done));
                    }
                    let id = self
                        .extension
                        .abi
                        .system
                        .files
                        .as_mut()
                        .expect("file worker")
                        .workspace_file(call.root.clone(), call.operation.clone())
                        .map_err(|e| e.to_string())?;
                    self.extension.abi.system.pending_files.insert(
                        id,
                        FileCall {
                            callback: call.callback.clone(),
                            root: call.root.clone(),
                            operation: call.operation.clone(),
                            buffers: call.buffers.clone(),
                            written: false,
                        },
                    );
                    Ok(None)
                }
                VfsResult::WorkspaceFile { result, .. } => {
                    let result = result.map_err(|e| e.to_string())?;
                    if matches!(call.operation, FileOperation::Write(..)) {
                        self.resolve_system_file(FileCall {
                            callback: call.callback.clone(),
                            root: call.root.clone(),
                            operation: call.operation.clone(),
                            buffers: Vec::new(),
                            written: true,
                        })?;
                        return Ok(None);
                    }
                    Ok(Some(file_value(result)))
                }
                _ => Err("unexpected file worker reply".into()),
            }
        })();
        match response {
            Ok(Some(value)) => self
                .extension
                .abi
                .system
                .reply(call.callback, Ok(wit::SystemValue::File(value))),
            Ok(None) => {}
            Err(error) => self.extension.abi.system.reply(call.callback, Err(error)),
        }
    }

    fn reload_system_write(&mut self, call: &FileCall) -> Result<(), String> {
        let FileOperation::Write(path, bytes) = &call.operation else {
            return Ok(());
        };
        let text = crate::editor::text::TextBuffer::from_bytes(bytes).map_err(|e| e.to_string())?;
        let mut draft = self.state.draft();
        for (id, resolved) in &call.buffers {
            if resolved == path
                && let Some(buffer) = self.state.buffers.get(*id)
            {
                if buffer.dirty {
                    return Err("file written; newer unsaved changes were preserved".into());
                }
                draft.reload_document(
                    *id,
                    text.clone(),
                    buffer.path.clone().ok_or("buffer path changed")?,
                );
            }
        }
        self.commit_editor_draft(draft, None, false)
            .map_err(|e| e.to_string())?;
        self.git.forget_hunks();
        self.keep_cursor_off_final_newline();
        Ok(())
    }

    pub(super) fn poll_system_callbacks(&mut self) {
        for _ in 0..64 {
            let result = self
                .extension
                .abi
                .system
                .files
                .as_ref()
                .map(VfsWorker::try_result);
            match result {
                Some(Ok(Some(result))) => self.accept_system_file(result),
                Some(Err(error)) => {
                    let calls = std::mem::take(&mut self.extension.abi.system.pending_files);
                    for (_, call) in calls {
                        self.extension
                            .abi
                            .system
                            .reply(call.callback, Err(error.to_string()));
                    }
                    break;
                }
                _ => break,
            }
        }
        if self.extension.abi.system.events.len() < 128 {
            let events = self
                .extension
                .abi
                .system
                .jobs
                .as_mut()
                .map(JobSupervisor::poll)
                .unwrap_or_default();
            for event in events {
                let (job, value, done) = match event {
                    JobEvent::Started { .. } => continue,
                    JobEvent::Output {
                        job,
                        stream,
                        bytes,
                        truncated,
                    } => {
                        let value = if truncated {
                            Err("process output limit exceeded".into())
                        } else {
                            Ok(wit::SystemValue::Process(match stream {
                                JobStream::Stdout => wit::ProcessOutput::Stdout(bytes),
                                JobStream::Stderr => wit::ProcessOutput::Stderr(bytes),
                            }))
                        };
                        (job, value, false)
                    }
                    JobEvent::Exited {
                        job,
                        code,
                        timed_out,
                    } => (
                        job,
                        if timed_out {
                            Err("process timed out".into())
                        } else {
                            Ok(wit::SystemValue::Process(wit::ProcessOutput::Exited(code)))
                        },
                        true,
                    ),
                    JobEvent::Failed { job, message } => (job, Err(message), true),
                };
                if let Some(callback) = self.extension.abi.system.processes.get(&job).cloned() {
                    if self
                        .extension
                        .is_active_load(&callback.plugin, callback.load)
                    {
                        self.extension.abi.system.reply(callback, value);
                    } else if let Some(jobs) = &mut self.extension.abi.system.jobs {
                        let _ = jobs.cancel(job);
                    }
                    if done {
                        self.extension.abi.system.processes.remove(&job);
                    }
                }
            }
        }
        let mut callbacks = BTreeMap::new();
        for (callback, _) in &self.extension.abi.system.events {
            if !self
                .extension
                .abi
                .waiting
                .keys()
                .any(|(owner, _)| owner == &callback.plugin)
            {
                callbacks
                    .entry(callback.plugin.clone())
                    .or_insert_with(|| callback.clone());
            }
        }
        for (_, callback) in callbacks {
            if self
                .extension
                .is_active_load(&callback.plugin, callback.load)
            {
                self.execute_plugin_call(
                    callback.plugin,
                    callback.handler,
                    callback_call("system"),
                    None,
                    None,
                    callback.authority,
                );
            }
        }
    }
}

fn range(range: std::ops::Range<usize>) -> wit::ByteRange {
    wit::ByteRange {
        start: range.start as u64,
        end: range.end as u64,
    }
}
fn file_info(info: FileInfo) -> wit::FileInfo {
    wit::FileInfo {
        name: info.name,
        kind: if info.directory {
            wit::FileKind::Directory
        } else if info.file {
            wit::FileKind::File
        } else {
            wit::FileKind::Other
        },
        bytes: info.bytes,
    }
}
fn file_value(value: FileValue) -> wit::FileValue {
    match value {
        FileValue::Done => wit::FileValue::Done,
        FileValue::Bytes(bytes) => wit::FileValue::Bytes(bytes),
        FileValue::Entries(entries) => {
            wit::FileValue::Entries(entries.into_iter().map(file_info).collect())
        }
        FileValue::Info(info) => wit::FileValue::Info(file_info(info)),
    }
}

#[cfg(test)]
#[path = "system_tests.rs"]
mod tests;
