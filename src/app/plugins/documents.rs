//! Plugin document requests and editor publication.

#![forbid(unsafe_code)]

use crate::editor::text::TextBuffer;

use crate::editor::BufferId;

use crate::editor::EditorDraft;

use crate::plugins::PluginLoadId;

use crate::plugins::PluginVirtualDocumentSnapshot;
use std::collections::BTreeMap;
const MAXIMUM_VIRTUAL_EDITOR_DOCUMENTS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct VirtualDocumentEditorEntry {
    pub plugin: String,
    pub load: PluginLoadId,
    pub document: u64,
    pub source: String,
    pub key: String,
    pub buffer: BufferId,
    pub revision: u64,
    text: crate::editor::text::TextVersion,
}
impl VirtualDocumentEditorEntry {
    pub(in crate::app) fn matches(&self, state: crate::editor::EditorTopology<'_>) -> bool {
        state.buffers().get(self.buffer).is_some_and(|buffer| buffer.text.source_version() == self.text && matches!(&buffer.origin,
            crate::editor::BufferOrigin::VirtualReadOnly { owner, load_id: plugin_load, document, revision }
                if owner == &self.plugin && *plugin_load == self.load.0 && *document == self.document && *revision == self.revision))
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) enum VirtualDocumentEditorError {
    InvalidInitial,
    StaleRevision,
    DocumentLimit,
    Layout(&'static str),
    SourceMismatch,
}
#[derive(Clone, Debug, Default)]
pub(in crate::app) struct VirtualDocumentEditorBridge {
    entries: BTreeMap<(String, PluginLoadId, u64), VirtualDocumentEditorEntry>,
}
impl VirtualDocumentEditorBridge {
    pub(in crate::app) fn publish(
        &mut self,
        state: &mut EditorDraft,
        snapshot: &PluginVirtualDocumentSnapshot,
    ) -> Result<BufferId, VirtualDocumentEditorError> {
        let key = (snapshot.plugin.clone(), snapshot.load, snapshot.document);
        if let Some(entry) = self.entries.get_mut(&key) {
            if !entry.matches(state.topology()) {
                return Err(VirtualDocumentEditorError::SourceMismatch);
            }
            if snapshot.revision
                != entry
                    .revision
                    .checked_add(1)
                    .ok_or(VirtualDocumentEditorError::StaleRevision)?
            {
                return Err(VirtualDocumentEditorError::StaleRevision);
            }
            let mut text = state.topology().buffers()[entry.buffer].text.clone();
            text.apply_edits(&crate::editor::text::TextEditSet::new(
                text.revision(),
                vec![crate::editor::text::TextEdit::new(
                    crate::editor::text::ByteRange::new(
                        crate::editor::text::ByteOffset(0),
                        crate::editor::text::ByteOffset(text.len_bytes()),
                    )
                    .map_err(|_| VirtualDocumentEditorError::StaleRevision)?,
                    snapshot.text.clone(),
                )],
            ))
            .map_err(|_| VirtualDocumentEditorError::StaleRevision)?;
            if !state.replace_virtual_read_only(
                entry.buffer,
                entry.revision,
                snapshot.revision,
                text,
            ) {
                return Err(VirtualDocumentEditorError::StaleRevision);
            }
            state.set_virtual_metadata(
                entry.buffer,
                snapshot.title.clone(),
                snapshot.language.clone(),
            );
            entry.revision = snapshot.revision;
            entry.text = state.topology().buffers()[entry.buffer]
                .text
                .source_version();
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
            return Err(VirtualDocumentEditorError::DocumentLimit);
        }
        if self.entries.values().any(|e| {
            e.plugin == snapshot.plugin
                && e.load == snapshot.load
                && e.source == snapshot.source
                && e.key == snapshot.key
        }) {
            return Err(VirtualDocumentEditorError::SourceMismatch);
        }
        let (buffer, _) = state
            .open_virtual_read_only(
                TextBuffer::from_text(&snapshot.text),
                snapshot.plugin.clone(),
                snapshot.load.0,
                snapshot.document,
                snapshot.revision,
            )
            .map_err(VirtualDocumentEditorError::Layout)?;
        state.set_virtual_metadata(buffer, snapshot.title.clone(), snapshot.language.clone());
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
                text: state.topology().buffers()[buffer].text.source_version(),
            },
        );
        Ok(buffer)
    }
    pub(in crate::app) fn find(
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
    pub(in crate::app) fn has_load(&self, plugin: &str, load: PluginLoadId) -> bool {
        self.entries
            .keys()
            .any(|(owner, plugin_load, _)| owner == plugin && *plugin_load == load)
    }
    pub(in crate::app) fn remove_load(
        &mut self,
        state: &mut EditorDraft,
        plugin: &str,
        load: PluginLoadId,
    ) -> Result<usize, String> {
        let keys = self
            .entries
            .keys()
            .filter(|(owner, plugin_load, _)| owner == plugin && *plugin_load == load)
            .cloned()
            .collect::<Vec<_>>();
        let count = keys.len();
        for key in keys {
            let entry = self.entries.get(&key).expect("collected virtual source");
            if !entry.matches(state.topology()) {
                return Err("virtual document removal source changed".into());
            }
            state.remove_virtual_document(
                entry.buffer,
                &entry.plugin,
                entry.load.0,
                entry.document,
            )?;
            self.entries.remove(&key);
        }
        Ok(count)
    }
    pub(in crate::app) fn forget_document(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        document: u64,
    ) -> bool {
        self.entries
            .remove(&(plugin.to_owned(), load, document))
            .is_some()
    }

    pub(in crate::app) fn entry_for_buffer(
        &self,
        buffer: BufferId,
    ) -> Option<&VirtualDocumentEditorEntry> {
        self.entries.values().find(|e| e.buffer == buffer)
    }

    /// Semantic provider coordinates only. Plugin loads, document handles,
    /// revisions, and retained content are deliberately not exposed to persistence.
    pub(in crate::app) fn descriptor_sources(&self) -> Vec<(BufferId, String, String, String)> {
        self.entries
            .values()
            .map(|entry| {
                (
                    entry.buffer,
                    entry.plugin.clone(),
                    entry.source.clone(),
                    entry.key.clone(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::app::plugins::documents::VirtualDocumentEditorBridge;
    use crate::app::plugins::documents::VirtualDocumentEditorError;
    use crate::editor::BufferId;
    use crate::editor::EditorState;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    use crate::plugins::PluginLoadId;
    use crate::plugins::PluginVirtualDocumentSnapshot;
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
    pub(in crate::app) fn publish(
        bridge: &mut VirtualDocumentEditorBridge,
        state: &mut EditorState,
        snapshot: &PluginVirtualDocumentSnapshot,
    ) -> Result<BufferId, VirtualDocumentEditorError> {
        let mut draft = state.draft();
        let buffer = bridge.publish(&mut draft, snapshot)?;
        *state = draft.accept(&state.version()).unwrap();
        Ok(buffer)
    }
    #[test]
    fn publish_refresh_preserves_identity_and_rejects_stale_or_foreign_sources() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let mut bridge = VirtualDocumentEditorBridge::default();
        let b = publish(&mut bridge, &mut state, &snap(1, "héllo")).unwrap();
        assert_eq!(state.buffers[b].virtual_title(), Some("Help"));
        assert_eq!(
            state.buffers[b].virtual_language.as_deref(),
            Some("markdown")
        );
        let view = state.active_view().unwrap();
        state.views[view].cursor = ByteOffset(3);
        let buffers = state.buffers.len();
        let text_revision = state.buffers[b].text.revision();
        assert_eq!(
            publish(&mut bridge, &mut state, &snap(2, "héllo refreshed")).unwrap(),
            b
        );
        assert!(state.buffers[b].text.revision() > text_revision);
        assert_eq!(state.active_view(), Some(view));
        assert_eq!(state.views[view].cursor, ByteOffset(3));
        assert_eq!(state.buffers[b].virtual_title(), Some("Help"));
        assert_eq!(
            state.buffers[b].virtual_language.as_deref(),
            Some("markdown")
        );
        let before = state.buffers[b].text.to_string();
        assert_eq!(
            publish(&mut bridge, &mut state, &snap(4, "stale")),
            Err(VirtualDocumentEditorError::StaleRevision)
        );
        assert_eq!(state.buffers[b].text.to_string(), before);
        let original = state.buffers[b].text.clone();
        state.buffers[b].text = TextBuffer::from_text(&before);
        assert_eq!(
            publish(&mut bridge, &mut state, &snap(3, "foreign instance")),
            Err(VirtualDocumentEditorError::SourceMismatch)
        );
        state.buffers[b].text = original;
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
            crate::editor::BufferOrigin::VirtualReadOnly { .. }
        ));
        let before = state.version();
        let mut draft = state.draft();
        assert_eq!(
            bridge
                .remove_load(&mut draft, "demo", PluginLoadId(8))
                .unwrap(),
            0
        );
        assert_eq!(
            bridge
                .remove_load(&mut draft, "demo", PluginLoadId(7))
                .unwrap(),
            1
        );
        state = draft.accept(&before).unwrap();
        assert!(!state.buffers.contains_key(b));
    }
}
