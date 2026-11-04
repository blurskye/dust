use super::{EditorTopology, WindowSession};
use crate::ui::render::overlays::{CUSTOM_ICON, SCRATCH_ICON, TERMINAL_ICON, VIRTUAL_ICON};
use crate::{
    config::Config,
    editor::BufferId,
    ui::{ContentTabDiagnostic, ContentTabEntry, ContentTabIdentity, ContentTabKind},
};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct ContentLine {
    order: Vec<BufferId>,
    pins: BTreeSet<BufferId>,
    order_revision: u64,
    diagnostic_revision: u64,
    signature: Option<(u64, u64, u64, u64)>,
    entries: Vec<ContentTabEntry>,
    #[cfg(test)]
    builds: usize,
}
impl WindowSession {
    pub(in crate::editor::session) fn content_tabs(&self) -> &[ContentTabEntry] {
        &self.content_line.entries
    }
    pub(in crate::editor::session) fn content_current(&self, configuration_revision: u64) -> bool {
        self.core.graph.revision().is_some_and(|revision| {
            self.content_line.signature
                == Some((
                    revision,
                    self.content_line.diagnostic_revision,
                    self.content_line.order_revision,
                    configuration_revision,
                ))
        })
    }
    pub(in crate::editor::session) fn diagnostics_changed(&mut self) {
        self.content_line.diagnostic_revision =
            self.content_line.diagnostic_revision.saturating_add(1);
        self.content_line.signature = None;
    }
    pub(in crate::editor::session) fn toggle_pin(&mut self, buffer: BufferId) {
        if self.core.editor.content_for(buffer).is_none() {
            return;
        }
        let line = &mut self.content_line;
        if !line.pins.remove(&buffer) {
            line.pins.insert(buffer);
        }
        line.order_revision = line.order_revision.saturating_add(1);
        line.signature = None;
    }
    pub(in crate::editor::session) fn restore_pins(&mut self, buffers: BTreeSet<BufferId>) {
        let line = &mut self.content_line;
        line.pins = buffers;
        line.order_revision = line.order_revision.saturating_add(1);
        line.signature = None;
    }
    /// Put these buffers first on the buffer line, in this order, as a restored session had them.
    pub(in crate::editor::session) fn restore_content_order(&mut self, buffers: Vec<BufferId>) {
        let line = &mut self.content_line;
        line.order.retain(|buffer| !buffers.contains(buffer));
        line.order.splice(0..0, buffers);
        line.order_revision = line.order_revision.saturating_add(1);
        line.signature = None;
    }
    pub(in crate::editor::session) fn move_content(
        &mut self,
        buffer: BufferId,
        direction: isize,
    ) -> Result<(), &'static str> {
        let line = &mut self.content_line;
        let pinned = line.pins.contains(&buffer);
        let index = line
            .order
            .iter()
            .position(|candidate| *candidate == buffer)
            .ok_or("active buffer has no content-line entry")?;
        let target = if direction < 0 {
            line.order[..index]
                .iter()
                .rposition(|candidate| line.pins.contains(candidate) == pinned)
        } else {
            line.order[index.saturating_add(1)..]
                .iter()
                .position(|candidate| line.pins.contains(candidate) == pinned)
                .map(|offset| index + 1 + offset)
        }
        .ok_or("buffer is already at the content-line boundary")?;
        line.order.swap(index, target);
        line.order_revision = line.order_revision.saturating_add(1);
        line.signature = None;
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn content_builds(&self) -> usize {
        self.content_line.builds
    }
    pub(in crate::editor::session) fn pins(&self) -> &BTreeSet<BufferId> {
        &self.content_line.pins
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn content_order(&self) -> &[BufferId] {
        &self.content_line.order
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn content_stamp(&self) -> Option<(u64, u64, u64, u64)> {
        self.content_line.signature
    }
    pub(in crate::editor::session) fn project_content_tabs(
        &mut self,
        state: &EditorTopology<'_>,
        diagnostics: &std::collections::BTreeMap<std::path::PathBuf, ContentTabDiagnostic>,
        config: &Config,
        configuration_revision: u64,
    ) {
        let graph = &self.core.graph;
        let bridge = &self.core.editor;
        let line = &mut self.content_line;
        let graph_revision = graph.revision().unwrap_or(u64::MAX);
        let signature = (
            graph_revision,
            line.diagnostic_revision,
            line.order_revision,
            configuration_revision,
        );
        if line.signature != Some(signature) {
            #[cfg(test)]
            {
                line.builds += 1;
            }
            let snapshots = graph
                .content_snapshots()
                .into_iter()
                .filter(|content| {
                    matches!(
                        content.adapter,
                        crate::ui::window::ContentAdapterKind::TextBuffer
                            | crate::ui::window::ContentAdapterKind::VirtualDocument
                    ) || matches!(
                        content.adapter,
                        crate::ui::window::ContentAdapterKind::DeclarativeUi
                            | crate::ui::window::ContentAdapterKind::PrivateCellSurface
                    ) && (content.content_type.as_str().starts_with("plugin.")
                        || matches!(
                            content.content_type.as_str(),
                            "builtin.problems"
                                | "builtin.output"
                                | "builtin.logs"
                                | "builtin.debug"
                        ) && content.active)
                })
                // The agent panel has its own tab line.
                .filter(|content| {
                    bridge
                        .buffer_for_content(content.id)
                        .and_then(|buffer| state.buffers().get(buffer))
                        .and_then(|buffer| buffer.virtual_owner())
                        != Some(crate::editor::session::dispatch::agent::TRANSCRIPT_OWNER)
                })
                .collect::<Vec<_>>();
            let represented = snapshots
                .iter()
                .filter_map(|content| bridge.buffer_for_content(content.id))
                .collect::<BTreeSet<_>>();
            let before_order = line.order.clone();
            let before_pins = line.pins.clone();
            line.order.retain(|buffer| represented.contains(buffer));
            line.pins.retain(|buffer| represented.contains(buffer));
            for content in &snapshots {
                if let Some(buffer) = bridge.buffer_for_content(content.id)
                    && !line.order.contains(&buffer)
                {
                    line.order.push(buffer)
                }
            }
            if before_order != line.order || before_pins != line.pins {
                line.order_revision = line.order_revision.saturating_add(1);
            }
            let mut entries = snapshots
                .into_iter()
                .enumerate()
                .map(|(native_index, content)| {
                    let buffer = bridge.buffer_for_content(content.id);
                    let pinned = buffer.is_some_and(|buffer| line.pins.contains(&buffer));
                    let order = buffer
                        .and_then(|buffer| {
                            line.order.iter().position(|candidate| *candidate == buffer)
                        })
                        .unwrap_or(line.order.len().saturating_add(native_index));
                    let (icon, diagnostic, closeable) = buffer.map_or_else(
                        || {
                            let icon = match content.adapter {
                                _ if config.ui.explorer.icon_mode
                                    != crate::config::ExplorerIconMode::NerdFont =>
                                {
                                    config.ui.explorer.active_glyphs().file.clone()
                                }
                                crate::ui::window::ContentAdapterKind::TerminalSession
                                | crate::ui::window::ContentAdapterKind::ExternalTui => {
                                    TERMINAL_ICON.to_owned()
                                }
                                crate::ui::window::ContentAdapterKind::VirtualDocument => {
                                    VIRTUAL_ICON.to_owned()
                                }
                                _ => CUSTOM_ICON.to_owned(),
                            };
                            (icon, None, false)
                        },
                        |buffer| {
                            let state = &state.buffers()[buffer];
                            // Without a Nerd Font, a tab uses the explorer's plain file glyph.
                            let icon = if config.ui.explorer.icon_mode
                                != crate::config::ExplorerIconMode::NerdFont
                            {
                                config.ui.explorer.active_glyphs().file.clone()
                            } else if state.is_virtual_read_only() {
                                VIRTUAL_ICON.to_owned()
                            } else if let Some(path) = &state.path {
                                path.extension()
                                    .and_then(|value| value.to_str())
                                    .map(str::to_ascii_lowercase)
                                    .and_then(|extension| {
                                        config.ui.buffer_line.extension_icons.get(&extension).cloned()
                                    })
                                    .or_else(|| {
                                        let name = path.file_name()?.to_str()?;
                                        crate::files::file_icon(name)
                                            .map(|icon| icon.glyph.to_owned())
                                    })
                                    .unwrap_or_else(|| CUSTOM_ICON.to_owned())
                            } else {
                                SCRATCH_ICON.to_owned()
                            };
                            let diagnostic = state
                                .path
                                .as_ref()
                                .and_then(|path| diagnostics.get(path))
                                .copied();
                            (icon, diagnostic, true)
                        },
                    );
                    let entry = ContentTabEntry {
                        identity: ContentTabIdentity(content.id),
                        kind: if buffer
                            .is_some_and(|buffer| state.buffers()[buffer].is_virtual_read_only())
                        {
                            ContentTabKind::VirtualDocument
                        } else {
                            ContentTabKind::from(content.adapter)
                        },
                        title: content.title,
                        active: content.active,
                        dirty: content.dirty,
                        pinned,
                        diagnostic,
                        icon,
                        closeable,
                    };
                    (
                        (
                            if buffer.is_some() {
                                u8::from(!pinned)
                            } else {
                                2
                            },
                            order,
                        ),
                        entry,
                    )
                })
                .collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            line.entries = entries.into_iter().map(|(_, entry)| entry).collect();
            line.signature = Some((
                graph_revision,
                line.diagnostic_revision,
                line.order_revision,
                configuration_revision,
            ));
        }
    }
}
