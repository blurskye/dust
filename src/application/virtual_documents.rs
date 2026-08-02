#![forbid(unsafe_code)]

use crate::{
    document::{ByteOffset, TextBuffer},
    editing::{BufferId, EditorState},
    plugins::{PluginLoadId, PluginVirtualDocumentSnapshot},
};
use std::collections::BTreeMap;
const MAXIMUM_VIRTUAL_EDITOR_DOCUMENTS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VirtualDocumentEditorEntry {
    pub plugin: String,
    pub load: PluginLoadId,
    pub document: u64,
    pub source: String,
    pub key: String,
    pub buffer: BufferId,
    pub revision: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum VirtualDocumentEditorError {
    InvalidInitial,
    StaleRevision,
    Ceiling,
    GenerationMismatch,
}
#[derive(Clone, Debug, Default)]
pub(super) struct VirtualDocumentEditorBridge {
    entries: BTreeMap<(String, PluginLoadId, u64), VirtualDocumentEditorEntry>,
}
impl VirtualDocumentEditorBridge {
    pub(super) fn publish(
        &mut self,
        state: &mut EditorState,
        snapshot: &PluginVirtualDocumentSnapshot,
    ) -> Result<BufferId, VirtualDocumentEditorError> {
        let key = (snapshot.plugin.clone(), snapshot.load, snapshot.document);
        if let Some(entry) = self.entries.get_mut(&key) {
            if snapshot.revision
                != entry
                    .revision
                    .checked_add(1)
                    .ok_or(VirtualDocumentEditorError::StaleRevision)?
            {
                return Err(VirtualDocumentEditorError::StaleRevision);
            }
            let views = state
                .views
                .iter()
                .filter(|(_, v)| v.buffer == entry.buffer)
                .map(|(id, v)| (id, v.cursor, v.selection, v.scroll_line, v.scroll_column))
                .collect::<Vec<_>>();
            if !state.replace_virtual_read_only(
                entry.buffer,
                entry.revision,
                snapshot.revision,
                TextBuffer::from_text(&snapshot.text),
            ) {
                return Err(VirtualDocumentEditorError::StaleRevision);
            }
            let len = state.buffers[entry.buffer].text.len_bytes();
            for (id, cursor, selection, line, column) in views {
                if let Some(view) = state.views.get_mut(id) {
                    view.cursor = ByteOffset(cursor.get().min(len));
                    while !state.buffers[entry.buffer]
                        .text
                        .is_char_boundary(view.cursor)
                    {
                        view.cursor = ByteOffset(view.cursor.get().saturating_sub(1));
                    }
                    view.selection =
                        selection.filter(|s| s.anchor.get() <= len && s.head.get() <= len);
                    view.scroll_line = line.min(
                        state.buffers[entry.buffer]
                            .text
                            .len_lines()
                            .saturating_sub(1),
                    );
                    view.scroll_column = column;
                }
            }
            entry.revision = snapshot.revision;
            return Ok(entry.buffer);
        }
        if snapshot.revision != 1
            || snapshot.closed
            || snapshot.title.is_empty()
            || snapshot.source.is_empty()
        {
            return Err(VirtualDocumentEditorError::InvalidInitial);
        }
        if self.entries.len() >= MAXIMUM_VIRTUAL_EDITOR_DOCUMENTS {
            return Err(VirtualDocumentEditorError::Ceiling);
        }
        if self.entries.values().any(|e| {
            e.plugin == snapshot.plugin
                && e.load == snapshot.load
                && e.source == snapshot.source
                && e.key == snapshot.key
        }) {
            return Err(VirtualDocumentEditorError::GenerationMismatch);
        }
        let (buffer, _) = state.open_virtual_read_only(
            TextBuffer::from_text(&snapshot.text),
            snapshot.plugin.clone(),
            snapshot.load.0,
            snapshot.document,
            snapshot.revision,
        );
        self.entries.insert(
            key,
            VirtualDocumentEditorEntry {
                plugin: snapshot.plugin.clone(),
                load: snapshot.load,
                document: snapshot.document,
                source: snapshot.source.clone(),
                key: snapshot.key.clone(),
                buffer,
                revision: 1,
            },
        );
        Ok(buffer)
    }
    pub(super) fn find(
        &self,
        plugin: &str,
        load: PluginLoadId,
        source: &str,
        key: &str,
    ) -> Option<&VirtualDocumentEditorEntry> {
        self.entries
            .values()
            .find(|e| e.plugin == plugin && e.load == load && e.source == source && e.key == key)
    }
    pub(super) fn remove_generation(
        &mut self,
        state: &mut EditorState,
        plugin: &str,
        load: PluginLoadId,
    ) -> usize {
        let keys = self
            .entries
            .keys()
            .filter(|(p, l, _)| p == plugin && *l == load)
            .cloned()
            .collect::<Vec<_>>();
        let mut removed = 0;
        for key in keys {
            if let Some(entry) = self.entries.remove(&key) {
                let views = state
                    .views
                    .iter()
                    .filter_map(|(id, v)| (v.buffer == entry.buffer).then_some(id))
                    .collect::<Vec<_>>();
                for view in views {
                    let _ = state.close_view(view);
                }
                state.remove_buffer(entry.buffer);
                removed += 1;
            }
        }
        removed
    }
    #[allow(
        dead_code,
        reason = "session coordinator currently replaces the complete bridge"
    )]
    pub(super) fn reset_runtime(&mut self, state: &mut EditorState) {
        let entries = std::mem::take(&mut self.entries);
        for entry in entries.into_values() {
            let views = state
                .views
                .iter()
                .filter_map(|(id, v)| (v.buffer == entry.buffer).then_some(id))
                .collect::<Vec<_>>();
            for view in views {
                let _ = state.close_view(view);
            }
            state.remove_buffer(entry.buffer);
        }
    }
    pub(super) fn forget_document(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        document: u64,
    ) -> bool {
        self.entries
            .remove(&(plugin.to_owned(), load, document))
            .is_some()
    }

    pub(super) fn entry_for_buffer(&self, buffer: BufferId) -> Option<&VirtualDocumentEditorEntry> {
        self.entries.values().find(|e| e.buffer == buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn snap(revision: u64, text: &str) -> PluginVirtualDocumentSnapshot {
        PluginVirtualDocumentSnapshot {
            plugin: "demo".into(),
            load: PluginLoadId(7),
            source: "plugin.demo.virtual.help".into(),
            document: 3,
            key: "help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            text: text.into(),
            revision,
            closed: false,
        }
    }
    #[test]
    fn publish_refresh_is_identity_preserving_stale_atomic_and_generation_owned() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let mut bridge = VirtualDocumentEditorBridge::default();
        let b = bridge.publish(&mut state, &snap(1, "héllo")).unwrap();
        let view = state.active_view.unwrap();
        state.views[view].cursor = ByteOffset(3);
        let buffers = state.buffers.len();
        assert_eq!(
            bridge
                .publish(&mut state, &snap(2, "héllo refreshed"))
                .unwrap(),
            b
        );
        assert_eq!(state.active_view, Some(view));
        assert_eq!(state.views[view].cursor, ByteOffset(3));
        let before = state.buffers[b].text.to_string();
        assert_eq!(
            bridge.publish(&mut state, &snap(4, "stale")),
            Err(VirtualDocumentEditorError::StaleRevision)
        );
        assert_eq!(state.buffers[b].text.to_string(), before);
        for _ in 0..256 {
            assert_eq!(
                bridge
                    .find("demo", PluginLoadId(7), "plugin.demo.virtual.help", "help")
                    .unwrap()
                    .buffer,
                b
            );
        }
        assert_eq!(state.buffers.len(), buffers);
        assert!(matches!(
            state.buffers[b].origin,
            crate::editing::BufferOrigin::VirtualReadOnly { .. }
        ));
        assert_eq!(
            bridge.remove_generation(&mut state, "demo", PluginLoadId(8)),
            0
        );
        assert_eq!(
            bridge.remove_generation(&mut state, "demo", PluginLoadId(7)),
            1
        );
        assert!(!state.buffers.contains_key(b));
    }
}
