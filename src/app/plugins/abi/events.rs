//! Translate occurrence batches and moving anchors without observing edits twice.

use super::*;
use crate::editor::{EditorEvent, EditorState, ViewId};

fn view_state(state: &EditorState, id: ViewId) -> Option<wit::ViewState> {
    let view = state.views.get(id)?;
    Some(wit::ViewState {
        view: id.data().as_ffi(),
        buffer: view.buffer.data().as_ffi(),
        cursor: view.cursor.0 as u64,
        mode: format!("{:?}", view.mode),
    })
}

pub(super) fn event_batch(
    state: &EditorState,
    after: Option<u64>,
) -> Result<wit::EventBatch, String> {
    let batch = crate::app::events::batch(state, after);
    if batch.snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.buffers.len() > 4096
            || snapshot
                .buffers
                .iter()
                .map(|buffer| buffer.text.len_bytes())
                .sum::<usize>()
                > crate::plugins::requests::BYTE_LIMIT
    }) {
        return Err("editor snapshot exceeds plugin delivery limit".into());
    }
    if batch
        .records
        .iter()
        .any(|(_, event)| matches!(event, EditorEvent::BufferReloaded { .. }))
    {
        return event_batch(state, None);
    }
    let bytes: usize = batch
        .records
        .iter()
        .map(|(_, event)| match event {
            EditorEvent::BufferChanged { change, .. } => change
                .edits
                .edits()
                .iter()
                .map(|edit| edit.replacement().len())
                .sum(),
            _ => 0,
        })
        .sum();
    if bytes > crate::plugins::requests::BYTE_LIMIT {
        return event_batch(state, None);
    }
    let records = batch
        .records
        .into_iter()
        .filter_map(|(sequence, event)| {
            let value = match event {
                EditorEvent::BufferOpened { buffer } => {
                    wit::Occurrence::Opened(buffer.data().as_ffi())
                }
                EditorEvent::BufferClosed { buffer } => {
                    wit::Occurrence::Closed(buffer.data().as_ffi())
                }
                EditorEvent::BufferSaved { buffer, .. } => {
                    wit::Occurrence::Saved(buffer.data().as_ffi())
                }
                EditorEvent::BufferChanged { buffer, change } => {
                    wit::Occurrence::Changed(wit::BufferChange {
                        buffer: buffer.data().as_ffi(),
                        revision: change.after.0,
                        edits: change
                            .edits
                            .edits()
                            .iter()
                            .map(|edit| wit::Edit {
                                start: edit.range().start.0 as u64,
                                end: edit.range().end.0 as u64,
                                replacement: edit.replacement().into(),
                            })
                            .collect(),
                    })
                }
                EditorEvent::BufferReloaded { .. } => unreachable!("reload requests a snapshot"),
                EditorEvent::CursorMoved { view, position } => {
                    let mut view = view_state(state, view)?;
                    view.cursor = position.0 as u64;
                    wit::Occurrence::Cursor(view)
                }
                EditorEvent::ModeChanged { view, mode } => {
                    let mut view = view_state(state, view)?;
                    view.mode = format!("{mode:?}");
                    wit::Occurrence::Mode(view)
                }
                EditorEvent::WorkspaceChanged { root } => {
                    wit::Occurrence::Workspace(root.to_string_lossy().into())
                }
            };
            Some(wit::EventRecord { sequence, value })
        })
        .collect();
    let snapshot = batch.snapshot.map(|snapshot| wit::EditorSnapshot {
        buffers: snapshot
            .buffers
            .into_iter()
            .map(|buffer| wit::BufferState {
                buffer: buffer.id.data().as_ffi(),
                revision: buffer.text.revision().0,
                text: buffer.text.to_string(),
                dirty: buffer.dirty,
                read_only: buffer.read_only,
            })
            .collect(),
        views: snapshot
            .views
            .into_iter()
            .map(|(id, view)| wit::ViewState {
                view: id.data().as_ffi(),
                buffer: view.buffer.data().as_ffi(),
                cursor: view.cursor.0 as u64,
                mode: format!("{:?}", view.mode),
            })
            .collect(),
        active_view: snapshot.active_view.map(|id| id.data().as_ffi()),
        workspace: snapshot.workspace_root.to_string_lossy().into(),
    });
    Ok(wit::EventBatch {
        sequence: batch.sequence,
        records,
        snapshot,
    })
}

impl App {
    pub(super) fn plugin_delivery(&mut self, plugin: &str) -> Result<PluginDelivery, String> {
        let mut delivery = PluginDelivery {
            view: self
                .state
                .active_view()
                .and_then(|id| view_state(&self.state, id)),
            ..Default::default()
        };
        if let Some((_, after)) = self.extension.abi.subscriptions.get_mut(plugin) {
            let batch = event_batch(&self.state, *after)?;
            delivery.events = Some(batch);
        }
        for ((owner, buffer, namespace), marks) in &self.extension.abi.marks {
            if owner != plugin {
                continue;
            }
            let id = slotmap::KeyData::from_ffi(*buffer).into();
            if let Some(buffer_state) = self.state.buffers.get(id) {
                delivery.marks.push(wit::MarkSet {
                    buffer: *buffer,
                    revision: buffer_state.text.revision().0,
                    namespace: namespace.clone(),
                    marks: marks
                        .iter()
                        .filter_map(|(mark, id)| {
                            let range = buffer_state.decorations.range(*id)?;
                            Some(wit::Mark {
                                start: range.start.0 as u64,
                                end: range.end.0 as u64,
                                ..mark.clone()
                            })
                        })
                        .collect(),
                });
            }
        }
        Ok(delivery)
    }

    pub(super) fn collect_plugin_event_callbacks(
        &self,
        callbacks: &mut Vec<(String, String, wit::CommandCall)>,
    ) {
        for (plugin, (handler, after)) in &self.extension.abi.subscriptions {
            if !self
                .extension
                .abi
                .waiting
                .keys()
                .any(|(owner, _)| owner == plugin)
                && self.extension.load_id(plugin).is_some()
                && after.is_none_or(|after| self.state.events.sequence() > after)
            {
                callbacks.push((plugin.clone(), handler.clone(), callback_call("editor")));
            }
        }
    }
}
