//! Translate surface imports to native prompts, save work and chrome.

use super::{callback_call, decode_value, encode_value};
use crate::app::App;
use crate::app::documents::save_work::{SaveStep, SaveWorkRequest};
use crate::app::notifications::NoticeRequest;
use crate::app::picker::replacement::SelectRequest;
use crate::app::prompt::replacement::InputRequest;
use crate::app::windows::chrome::ChromeLine;
use crate::editor::{BufferId, CommandAccess, CommandAuthority, CommandValue};
use crate::plugins::component::component_api::dust::plugin::host as wit;
use crate::plugins::surface::SurfaceRequest;
use slotmap::Key;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

struct Presenter<T> {
    plugin: String,
    handler: String,
    authority: CommandAuthority,
    requests: Receiver<T>,
}

struct SaveHandler {
    plugin: String,
    handler: String,
    authority: CommandAuthority,
    requests: Receiver<SaveWorkRequest>,
    write: bool,
    before: bool,
}

struct Interaction {
    plugin: String,
    handler: String,
    token: u64,
    authority: CommandAuthority,
    reply: Receiver<Option<String>>,
}

struct Reply {
    plugin: String,
    handler: String,
    authority: CommandAuthority,
    reply: wit::CommandReply,
}

enum Answer {
    Input(SyncSender<Option<String>>),
    Select(
        SyncSender<Option<crate::workspace::PickerItemId>>,
        BTreeSet<String>,
    ),
    Save(SaveWorkRequest, bool),
    Notice,
}

struct Callback {
    plugin: String,
    handler: String,
    authority: CommandAuthority,
    call: wit::CommandCall,
    answer: Answer,
}

#[derive(Default)]
pub(super) struct Surfaces {
    input: Option<Presenter<InputRequest>>,
    select: Option<Presenter<SelectRequest>>,
    notices: Option<Presenter<NoticeRequest>>,
    saves: BTreeMap<String, SaveHandler>,
    documents: BTreeMap<(String, String, String), BufferId>,
    segments: BTreeSet<(String, String)>,
    interactions: Vec<Interaction>,
    replies: Vec<Reply>,
    callbacks: BTreeMap<(String, u64), (Answer, CommandAuthority)>,
    deferred: BTreeMap<(String, u64), std::time::Instant>,
}

fn name(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err("invalid surface name".into())
    } else {
        Ok(())
    }
}

fn record(fields: impl IntoIterator<Item = (&'static str, CommandValue)>) -> CommandValue {
    CommandValue::Record(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

fn text(value: impl Into<String>) -> CommandValue {
    CommandValue::String(value.into())
}
fn integer(value: u64) -> CommandValue {
    CommandValue::Integer(value as i64)
}
fn call(event: &str, fields: Vec<(&'static str, CommandValue)>) -> wit::CommandCall {
    let mut fields = fields;
    fields.push(("event", text(event)));
    wit::CommandCall {
        arguments: encode_value(&record(fields)),
        ..Default::default()
    }
}

fn segment_name(plugin: &str, location: wit::SegmentLocation, name: &str) -> String {
    let line = match location {
        wit::SegmentLocation::Statusline => "status",
        wit::SegmentLocation::BufferLine => "buffer",
    };
    format!("plugin.{plugin}.{line}.{name}")
}

fn style(value: &str) -> Result<crate::ui::Style, String> {
    use crate::ui::Style;
    match value {
        "default" => Ok(Style::Default),
        "muted" => Ok(Style::Muted),
        "accent" => Ok(Style::Accent),
        "warning" => Ok(Style::Warning),
        "error" => Ok(Style::Error),
        "selected" => Ok(Style::Selection),
        "border" => Ok(Style::Border),
        "status" => Ok(Style::Status),
        _ => Err(format!("unknown segment style: {value}")),
    }
}

impl App {
    pub(super) fn apply_surface_request(
        &mut self,
        plugin: &str,
        authority: &CommandAuthority,
        request: SurfaceRequest,
    ) -> Result<(), String> {
        let authority = self
            .effect_authority
            .restrict(authority)
            .restrict(&self.plugin_authority(plugin));
        let access = if matches!(
            request,
            SurfaceRequest::Document(..) | SurfaceRequest::Formatter(..)
        ) {
            CommandAccess::Edit
        } else {
            CommandAccess::Ui
        };
        authority
            .require(access)
            .map_err(|error| error.to_string())?;
        let previous = std::mem::replace(&mut self.effect_authority, authority.clone());
        let result = self.apply_surface(plugin, authority, request);
        self.effect_authority = previous;
        result
    }

    fn apply_surface(
        &mut self,
        plugin: &str,
        authority: CommandAuthority,
        request: SurfaceRequest,
    ) -> Result<(), String> {
        match request {
            SurfaceRequest::Input(token, request, handler) => {
                self.handler(plugin, &handler)?;
                name(&request.title)?;
                if self.extension.abi.surfaces.interactions.len() >= 64 {
                    return Err("interaction limit exceeded".into());
                }
                let (reply, receiver) = sync_channel(1);
                self.request_text_input(request.title, request.initial, request.secret, reply)?;
                self.extension.abi.surfaces.interactions.push(Interaction {
                    plugin: plugin.into(),
                    handler,
                    token,
                    authority,
                    reply: receiver,
                });
            }
            SurfaceRequest::Select(token, request, handler) => {
                self.handler(plugin, &handler)?;
                name(&request.title)?;
                let mut ids = BTreeSet::new();
                for item in &request.items {
                    name(&item.id)?;
                    if !ids.insert(&item.id) {
                        return Err("duplicate select item ID".into());
                    }
                }
                if self.extension.abi.surfaces.interactions.len() >= 64 {
                    return Err("interaction limit exceeded".into());
                }
                let (reply, receiver) = sync_channel(1);
                self.request_item_select(
                    request.title,
                    request
                        .items
                        .into_iter()
                        .map(|item| (item.id, item.label, item.detail.unwrap_or_default()))
                        .collect(),
                    reply,
                )?;
                self.extension.abi.surfaces.interactions.push(Interaction {
                    plugin: plugin.into(),
                    handler,
                    token,
                    authority,
                    reply: receiver,
                });
            }
            SurfaceRequest::AnswerInput(request, value) => {
                self.answer_surface(plugin, request, value, true)?
            }
            SurfaceRequest::AnswerSelect(request, value) => {
                self.answer_surface(plugin, request, value, false)?
            }
            SurfaceRequest::Notify(request) => match request.level {
                wit::NoticeLevel::Info => self.notify_message(request.message, false),
                wit::NoticeLevel::Warning => self.notify_warning(request.message),
                wit::NoticeLevel::Error => self.notify_message(request.message, true),
            },
            SurfaceRequest::Presenter(kind, handler) => {
                if let Some(handler) = &handler {
                    self.handler(plugin, handler)?;
                }
                macro_rules! install {
                    ($field:ident, $install:expr) => {{
                        if handler.is_none()
                            && self
                                .extension
                                .abi
                                .surfaces
                                .$field
                                .as_ref()
                                .is_some_and(|presenter| presenter.plugin != plugin)
                        {
                            return Err("presenter belongs to another plugin".into());
                        }
                        let (sender, receiver) = sync_channel(16);
                        $install(handler.as_ref().map(|_| sender));
                        self.extension.abi.surfaces.$field = handler.map(|handler| Presenter {
                            plugin: plugin.into(),
                            handler,
                            authority,
                            requests: receiver,
                        });
                    }};
                }
                match kind {
                    wit::PresenterKind::Input => install!(input, |sender| self
                        .input_replacement
                        .install(sender, authority.clone())),
                    wit::PresenterKind::Select => install!(select, |sender| self
                        .replace_select(sender, authority.clone())),
                    wit::PresenterKind::Notifications => {
                        install!(notices, |sender| self.notices.replace(sender))
                    }
                }
                self.project_input_prompt();
                self.project_notice();
            }
            SurfaceRequest::Document(token, document, handler) => {
                self.handler(plugin, &handler)?;
                name(&document.source)?;
                name(&document.key)?;
                name(&document.title)?;
                name(&document.language)?;
                let key = (
                    plugin.to_owned(),
                    document.source.clone(),
                    document.key.clone(),
                );
                let existing = self
                    .extension
                    .abi
                    .surfaces
                    .documents
                    .get(&key)
                    .copied()
                    .filter(|buffer| self.state.buffers.contains_key(*buffer));
                let buffer = if let Some(buffer) = existing {
                    self.state
                        .show_buffer(buffer)
                        .map_err(|error| error.to_string())?;
                    buffer
                } else {
                    if self.extension.abi.surfaces.saves.len() >= 128 {
                        return Err("save handler limit exceeded".into());
                    }
                    let step_name = format!(
                        "plugin.{plugin}.document.{}:{}:{}",
                        document.source.len(),
                        document.source,
                        document.key
                    );
                    let (requests, receiver) = sync_channel(8);
                    let buffer = self.open_editable_document(
                        document.title,
                        document.language,
                        document.text,
                        SaveStep {
                            name: step_name.clone(),
                            requests,
                        },
                    )?;
                    self.extension.abi.surfaces.saves.insert(
                        step_name,
                        SaveHandler {
                            plugin: plugin.into(),
                            handler: handler.clone(),
                            authority: authority.clone(),
                            requests: receiver,
                            write: true,
                            before: false,
                        },
                    );
                    self.extension.abi.surfaces.documents.insert(key, buffer);
                    self.reconcile_editor_windows();
                    buffer
                };
                self.extension.abi.surfaces.replies.push(Reply {
                    plugin: plugin.into(),
                    handler,
                    authority,
                    reply: wit::CommandReply {
                        request: token,
                        value: Some(encode_value(&integer(buffer.data().as_ffi()))),
                        error: None,
                    },
                });
            }
            SurfaceRequest::Formatter(filetype, step, before) => {
                self.handler(plugin, &step.handler)?;
                name(&step.name)?;
                name(&filetype)?;
                let key = format!(
                    "plugin.{plugin}.{}.{filetype}.{}",
                    if before { "before" } else { "format" },
                    step.name
                );
                if self.extension.abi.surfaces.saves.len() >= 128
                    && !self.extension.abi.surfaces.saves.contains_key(&key)
                {
                    return Err("save handler limit exceeded".into());
                }
                let (requests, receiver) = sync_channel(8);
                self.documents.register_save_step(
                    filetype,
                    before,
                    step.order,
                    SaveStep {
                        name: key.clone(),
                        requests,
                    },
                );
                self.extension.abi.surfaces.saves.insert(
                    key,
                    SaveHandler {
                        plugin: plugin.into(),
                        handler: step.handler,
                        authority,
                        requests: receiver,
                        write: false,
                        before,
                    },
                );
            }
            SurfaceRequest::Segment(segment) => {
                name(&segment.name)?;
                if let Some(command) = &segment.command {
                    crate::editor::CommandId::new(command).map_err(|error| error.to_string())?;
                }
                let name = segment_name(plugin, segment.location, &segment.name);
                if self.extension.abi.surfaces.segments.len() >= 128
                    && !self
                        .extension
                        .abi
                        .surfaces
                        .segments
                        .contains(&(plugin.into(), name.clone()))
                {
                    return Err("segment limit exceeded".into());
                }
                let parts = segment
                    .text
                    .into_iter()
                    .map(|part| Ok((part.text, style(&part.style)?)))
                    .collect::<Result<Vec<_>, String>>()?;
                let line = match segment.location {
                    wit::SegmentLocation::Statusline => ChromeLine::Status,
                    wit::SegmentLocation::BufferLine => ChromeLine::Buffer,
                };
                self.window_session.set_named_segment(
                    line,
                    name.clone(),
                    segment.order,
                    parts,
                    segment.command,
                    authority,
                )?;
                self.extension
                    .abi
                    .surfaces
                    .segments
                    .insert((plugin.into(), name));
                self.project_window_content_tabs();
            }
            SurfaceRequest::RemoveSegment(location, name) => {
                let name = segment_name(plugin, location, &name);
                self.window_session.remove_named_segment(&name);
                self.extension
                    .abi
                    .surfaces
                    .segments
                    .remove(&(plugin.into(), name));
                self.project_window_content_tabs();
            }
        }
        Ok(())
    }

    fn answer_surface(
        &mut self,
        plugin: &str,
        request: u64,
        value: Option<String>,
        input: bool,
    ) -> Result<(), String> {
        let key = (plugin.to_owned(), request);
        let (answer, _) = self
            .extension
            .abi
            .surfaces
            .callbacks
            .get(&key)
            .ok_or("unknown presenter request")?;
        if !matches!(
            (answer, input),
            (Answer::Input(_), true) | (Answer::Select(..), false)
        ) {
            return Err("wrong presenter answer kind".into());
        }
        self.extension.abi.surfaces.deferred.remove(&key);
        self.finish_surface_callback(
            &key,
            Ok(Some(encode_value(
                &value.map_or(CommandValue::Bool(false), text),
            ))),
        );
        Ok(())
    }

    pub(super) fn poll_surface_callbacks(&mut self) {
        let expired: Vec<_> = self
            .extension
            .abi
            .surfaces
            .deferred
            .iter()
            .filter(|(_, deadline)| std::time::Instant::now() >= **deadline)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.extension.abi.surfaces.deferred.remove(&key);
            self.finish_surface_callback(&key, Err("presenter answer timed out".into()));
        }
        let mut callbacks = Vec::new();
        let surfaces = &mut self.extension.abi.surfaces;
        if let Some(presenter) = &surfaces.input {
            while let Ok(request) = presenter.requests.try_recv() {
                let title = request.prompt.help.clone().unwrap_or_else(|| {
                    request
                        .prompt
                        .kind
                        .title(&self.configuration.settings().ui.command_line.text)
                        .to_owned()
                });
                callbacks.push(Callback {
                    plugin: presenter.plugin.clone(),
                    handler: presenter.handler.clone(),
                    authority: request.authority.restrict(&presenter.authority),
                    call: call(
                        "ui-input",
                        vec![
                            ("title", text(title)),
                            ("initial", text(request.prompt.input)),
                            ("secret", CommandValue::Bool(request.secret)),
                        ],
                    ),
                    answer: Answer::Input(request.reply),
                });
            }
        }
        if let Some(presenter) = &surfaces.select {
            while let Ok(request) = presenter.requests.try_recv() {
                let mut ids = BTreeSet::new();
                let items = request
                    .picker
                    .rows
                    .iter()
                    .filter(|row| row.kind == crate::ui::PickerRowKind::Entry)
                    .map(|row| {
                        ids.insert(row.id.clone());
                        record([
                            ("id", text(row.id.clone())),
                            ("label", text(row.label.clone())),
                            ("detail", text(row.detail.clone())),
                        ])
                    })
                    .collect();
                callbacks.push(Callback {
                    plugin: presenter.plugin.clone(),
                    handler: presenter.handler.clone(),
                    authority: request.authority.restrict(&presenter.authority),
                    call: call(
                        "ui-select",
                        vec![
                            ("title", text(request.picker.title)),
                            ("items", CommandValue::List(items)),
                        ],
                    ),
                    answer: Answer::Select(request.reply, ids),
                });
            }
        }
        if let Some(presenter) = &surfaces.notices {
            while let Ok(request) = presenter.requests.try_recv() {
                let level = if request.error {
                    "error"
                } else if request.warning {
                    "warning"
                } else {
                    "info"
                };
                callbacks.push(Callback {
                    plugin: presenter.plugin.clone(),
                    handler: presenter.handler.clone(),
                    authority: request.authority.restrict(&presenter.authority),
                    call: call(
                        "notify",
                        vec![("message", text(request.message)), ("level", text(level))],
                    ),
                    answer: Answer::Notice,
                });
            }
        }
        for handler in surfaces.saves.values() {
            while let Ok(request) = handler.requests.try_recv() {
                let mut call = call(
                    if handler.write {
                        "document-write"
                    } else {
                        "format"
                    },
                    vec![
                        ("buffer", integer(request.buffer.data().as_ffi())),
                        ("revision", integer(request.text.revision().0)),
                        ("text", text(request.text.to_string())),
                        ("start", integer(0)),
                        ("end", integer(request.text.len_bytes() as u64)),
                        ("before-save", CommandValue::Bool(handler.before)),
                    ],
                );
                call.buffer = Some(request.buffer.data().as_ffi());
                callbacks.push(Callback {
                    plugin: handler.plugin.clone(),
                    handler: handler.handler.clone(),
                    authority: request.authority.restrict(&handler.authority),
                    call,
                    answer: Answer::Save(request, handler.write),
                });
            }
        }
        let mut pending = Vec::new();
        for interaction in std::mem::take(&mut surfaces.interactions) {
            let value = match interaction.reply.try_recv() {
                Ok(value) => value,
                Err(TryRecvError::Disconnected) => None,
                Err(TryRecvError::Empty) => {
                    pending.push(interaction);
                    continue;
                }
            };
            surfaces.replies.push(Reply {
                plugin: interaction.plugin,
                handler: interaction.handler,
                authority: interaction.authority,
                reply: wit::CommandReply {
                    request: interaction.token,
                    value: value.map(|value| encode_value(&text(value))),
                    error: None,
                },
            });
        }
        surfaces.interactions = pending;
        let replies = std::mem::take(&mut surfaces.replies);
        for reply in replies {
            self.extension
                .abi
                .replies
                .entry(reply.plugin.clone())
                .or_default()
                .push(reply.reply);
            self.execute_plugin_call(
                reply.plugin,
                reply.handler,
                callback_call("surface-reply"),
                None,
                None,
                reply.authority,
            );
        }
        for mut callback in callbacks {
            if self.extension.abi.surfaces.callbacks.len() >= 64 {
                self.show_error("surface callback limit exceeded");
                continue;
            }
            let id = self.extension.abi.next + 1;
            let key = (callback.plugin.clone(), id);
            let nodes = &mut callback.call.arguments.nodes;
            let Some(wit::ValueNode::Object(mut fields)) = nodes.pop() else {
                unreachable!("native callback record")
            };
            fields.push(wit::ValueField {
                name: "request".into(),
                value: nodes.len() as u32,
            });
            nodes.push(wit::ValueNode::Integer(id as i64));
            nodes.push(wit::ValueNode::Object(fields));
            self.extension
                .abi
                .surfaces
                .callbacks
                .insert(key.clone(), (callback.answer, callback.authority.clone()));
            if !self.execute_plugin_call(
                callback.plugin,
                callback.handler,
                callback.call,
                None,
                None,
                callback.authority,
            ) {
                self.finish_surface_callback(&key, Err("surface callback could not start".into()));
            }
        }
    }

    pub(super) fn finish_surface_callback(
        &mut self,
        key: &(String, u64),
        result: Result<Option<wit::CommandValue>, String>,
    ) {
        let Some((answer, authority)) = self.extension.abi.surfaces.callbacks.remove(key) else {
            return;
        };
        let access = if matches!(answer, Answer::Save(..)) {
            CommandAccess::Edit
        } else {
            CommandAccess::Ui
        };
        let result = authority
            .restrict(&self.plugin_authority(&key.0))
            .require(access)
            .map_err(|error| error.to_string())
            .and(result)
            .and_then(|value| value.map(decode_value).transpose());
        if matches!(answer, Answer::Input(_) | Answer::Select(..)) && matches!(result, Ok(None)) {
            self.extension
                .abi
                .surfaces
                .callbacks
                .insert(key.clone(), (answer, authority));
            self.extension.abi.surfaces.deferred.insert(
                key.clone(),
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            );
            return;
        }
        match answer {
            Answer::Save(request, write) => {
                let result = if write {
                    match result {
                        Ok(Some(CommandValue::Bool(true))) => Ok(None),
                        Ok(_) => Err("document write was rejected".into()),
                        Err(error) => Err(error),
                    }
                } else {
                    match result.and_then(|value| format_text(&request.text, value)) {
                        Ok(text) => Ok(text),
                        Err(error) => {
                            self.show_error(error);
                            Ok(None)
                        }
                    }
                };
                let _ = request.reply.try_send(result);
            }
            Answer::Input(reply) => {
                if let Ok(value) = result {
                    match value {
                        Some(CommandValue::String(text)) => {
                            let _ = reply.try_send(Some(text));
                        }
                        None | Some(CommandValue::Bool(false)) => {
                            let _ = reply.try_send(None);
                        }
                        _ => self.show_error("input presenter must return text or false"),
                    }
                }
            }
            Answer::Select(reply, ids) => {
                if let Ok(value) = result {
                    match value {
                        Some(CommandValue::String(id)) if ids.contains(&id) => {
                            let _ = reply.try_send(Some(crate::workspace::PickerItemId(id)));
                        }
                        None | Some(CommandValue::Bool(false)) => {
                            let _ = reply.try_send(None);
                        }
                        _ => self.show_error("select presenter returned an unknown item"),
                    }
                }
            }
            Answer::Notice => {
                if result.is_err()
                    && self
                        .extension
                        .abi
                        .surfaces
                        .notices
                        .as_ref()
                        .is_some_and(|presenter| presenter.plugin == key.0)
                {
                    self.notices.replace(None);
                    self.extension.abi.surfaces.notices = None;
                    self.project_notice();
                }
            }
        }
    }

    pub(super) fn remove_surface_plugin(&mut self, plugin: &str) {
        if self
            .extension
            .abi
            .surfaces
            .input
            .as_ref()
            .is_some_and(|value| value.plugin == plugin)
        {
            self.input_replacement.install(None, CommandAuthority::Host);
            self.extension.abi.surfaces.input = None;
        }
        if self
            .extension
            .abi
            .surfaces
            .select
            .as_ref()
            .is_some_and(|value| value.plugin == plugin)
        {
            self.replace_select(None, CommandAuthority::Host);
            self.extension.abi.surfaces.select = None;
        }
        if self
            .extension
            .abi
            .surfaces
            .notices
            .as_ref()
            .is_some_and(|value| value.plugin == plugin)
        {
            self.notices.replace(None);
            self.extension.abi.surfaces.notices = None;
        }
        let surfaces = &mut self.extension.abi.surfaces;
        surfaces.saves.retain(|name, handler| {
            if handler.plugin == plugin {
                self.documents.remove_save_step(name);
                false
            } else {
                true
            }
        });
        surfaces.segments.retain(|(owner, name)| {
            if owner == plugin {
                self.window_session.remove_named_segment(name);
                false
            } else {
                true
            }
        });
        surfaces.callbacks.retain(|(owner, _), _| owner != plugin);
        surfaces.deferred.retain(|(owner, _), _| owner != plugin);
        surfaces
            .documents
            .retain(|(owner, _, _), _| owner != plugin);
        surfaces.interactions.retain(|value| value.plugin != plugin);
        surfaces.replies.retain(|value| value.plugin != plugin);
        self.project_input_prompt();
        self.project_notice();
        self.project_window_content_tabs();
    }
}

fn format_text(
    snapshot: &crate::editor::text::TextBuffer,
    value: Option<CommandValue>,
) -> Result<Option<String>, String> {
    let Some(CommandValue::List(values)) = value else {
        return Err("formatter must return a list of edits".into());
    };
    let mut edits = Vec::new();
    for value in values {
        let CommandValue::Record(fields) = value else {
            return Err("format edit must be a record".into());
        };
        let offset = |key: &str| match fields.get(key) {
            Some(CommandValue::Integer(value)) if *value >= 0 => usize::try_from(*value)
                .map(crate::editor::text::ByteOffset)
                .map_err(|_| "edit offset is too large".to_owned()),
            _ => Err(format!("missing edit {key}")),
        };
        let Some(CommandValue::String(replacement)) = fields.get("replacement") else {
            return Err("missing edit replacement".into());
        };
        edits.push(crate::editor::text::TextEdit::new(
            crate::editor::text::ByteRange {
                start: offset("start")?,
                end: offset("end")?,
            },
            replacement.clone(),
        ));
    }
    let mut text = snapshot.clone();
    text.apply_edits(&crate::editor::text::TextEditSet::new(
        snapshot.revision(),
        edits,
    ))
    .map_err(|error| error.to_string())?;
    Ok(Some(text.to_string()))
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
