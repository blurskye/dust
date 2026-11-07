//! Window content descriptions and application actions.

use crate::app::windows::Windows;
use crate::config::Config;
use crate::editor::BufferId;
use crate::editor::EditorTopology;
use crate::ui::ContentTabDiagnostic;
use crate::ui::ContentTabEntry;
use crate::ui::ContentTabIdentity;
use crate::ui::ContentTabKind;
use crate::ui::render::overlays::CUSTOM_ICON;
use crate::ui::render::overlays::SCRATCH_ICON;
use crate::ui::render::overlays::TERMINAL_ICON;
use crate::ui::render::overlays::VIRTUAL_ICON;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct ContentLine {
    order: Vec<BufferId>,
    pins: BTreeSet<BufferId>,
    order_revision: u64,
    diagnostic_revision: u64,
    pub(super) signature: Option<(u64, u64, u64, u64)>,
    entries: Vec<ContentTabEntry>,
    #[cfg(test)]
    builds: usize,
}
impl Windows {
    pub(in crate::app) fn content_tabs(&self) -> &[ContentTabEntry] {
        &self.content_line.entries
    }
    /// The buffers on the buffer line, left to right.
    pub(in crate::app) fn content_tab_buffers(
        &self,
        state: &crate::editor::EditorState,
    ) -> Vec<BufferId> {
        self.content_line
            .entries
            .iter()
            .filter_map(|entry| state.buffer_for_content(entry.identity.0))
            .collect()
    }
    pub(in crate::app) fn content_current(&self, configuration_revision: u64) -> bool {
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
    pub(in crate::app) fn diagnostics_changed(&mut self) {
        self.content_line.diagnostic_revision =
            self.content_line.diagnostic_revision.saturating_add(1);
        self.content_line.signature = None;
    }
    pub(in crate::app) fn toggle_pin(
        &mut self,
        buffer: BufferId,
        state: &crate::editor::EditorState,
    ) {
        if state.content_for_buffer(buffer).is_none() {
            return;
        }
        let line = &mut self.content_line;
        if !line.pins.remove(&buffer) {
            line.pins.insert(buffer);
        }
        line.order_revision = line.order_revision.saturating_add(1);
        line.signature = None;
    }
    pub(in crate::app) fn restore_pins(&mut self, buffers: BTreeSet<BufferId>) {
        let line = &mut self.content_line;
        line.pins = buffers;
        line.order_revision = line.order_revision.saturating_add(1);
        line.signature = None;
    }
    /// Put these buffers first on the buffer line, in this order, as a restored session had them.
    pub(in crate::app) fn restore_content_order(&mut self, buffers: Vec<BufferId>) {
        let line = &mut self.content_line;
        line.order.retain(|buffer| !buffers.contains(buffer));
        line.order.splice(0..0, buffers);
        line.order_revision = line.order_revision.saturating_add(1);
        line.signature = None;
    }
    pub(in crate::app) fn move_content(
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
    pub(in crate::app) fn content_builds(&self) -> usize {
        self.content_line.builds
    }
    pub(in crate::app) fn pins(&self) -> &BTreeSet<BufferId> {
        &self.content_line.pins
    }
    #[cfg(test)]
    pub(in crate::app) fn content_order(&self) -> &[BufferId] {
        &self.content_line.order
    }
    #[cfg(test)]
    pub(in crate::app) fn content_stamp(&self) -> Option<(u64, u64, u64, u64)> {
        self.content_line.signature
    }
    pub(in crate::app) fn project_content_tabs(
        &mut self,
        state: &EditorTopology<'_>,
        diagnostics: &std::collections::BTreeMap<std::path::PathBuf, ContentTabDiagnostic>,
        config: &Config,
        configuration_revision: u64,
    ) {
        let graph = &self.core.graph;
        let documents = state.state();
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
                        crate::windows::ContentAdapterKind::TextBuffer
                            | crate::windows::ContentAdapterKind::VirtualDocument
                    ) || matches!(
                        content.adapter,
                        crate::windows::ContentAdapterKind::DeclarativeUi
                            | crate::windows::ContentAdapterKind::PrivateCellSurface
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
                    documents
                        .buffer_for_content(content.id)
                        .and_then(|buffer| state.buffers().get(buffer))
                        .and_then(|buffer| buffer.virtual_owner())
                        != Some(crate::app::agent::TRANSCRIPT_OWNER)
                })
                .collect::<Vec<_>>();
            let represented = snapshots
                .iter()
                .filter_map(|content| documents.buffer_for_content(content.id))
                .collect::<BTreeSet<_>>();
            let before_order = line.order.clone();
            let before_pins = line.pins.clone();
            line.order.retain(|buffer| represented.contains(buffer));
            line.pins.retain(|buffer| represented.contains(buffer));
            for content in &snapshots {
                if let Some(buffer) = documents.buffer_for_content(content.id)
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
                    let buffer = documents.buffer_for_content(content.id);
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
                                crate::windows::ContentAdapterKind::TerminalSession
                                | crate::windows::ContentAdapterKind::ExternalTui => {
                                    TERMINAL_ICON.to_owned()
                                }
                                crate::windows::ContentAdapterKind::VirtualDocument => {
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
                                        config
                                            .ui
                                            .buffer_line
                                            .extension_icons
                                            .get(&extension)
                                            .cloned()
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
                    let mut entry = ContentTabEntry {
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
                        segments: None,
                    };
                    entry.segments = Some(crate::app::windows::chrome::project_buffer_segments(
                        &entry,
                        config,
                        &self.chrome,
                    ));
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

pub(super) fn editor_buffer_titles(
    state: &crate::editor::EditorTopology<'_>,
    scratch_label: &str,
) -> std::collections::HashMap<BufferId, String> {
    let mut bases = std::collections::HashMap::<String, usize>::new();
    for (_, buffer) in state.buffers() {
        let base = editor_buffer_base(
            buffer.path.as_deref(),
            buffer.virtual_title().unwrap_or(scratch_label),
        );
        *bases.entry(base).or_default() += 1;
    }
    let mut occurrences = std::collections::HashMap::<String, usize>::new();
    state
        .buffers()
        .iter()
        .map(|(id, buffer)| {
            let base = editor_buffer_base(
                buffer.path.as_deref(),
                buffer.virtual_title().unwrap_or(scratch_label),
            );
            let duplicate = bases.get(&base).copied().unwrap_or(0) > 1;
            let occurrence = occurrences.entry(base.clone()).or_default();
            *occurrence = occurrence.saturating_add(1);
            let title = if duplicate {
                let context = buffer.path.as_deref().map_or_else(
                    || base.clone(),
                    |path| {
                        path.strip_prefix(state.workspace_root())
                            .unwrap_or(path)
                            .to_string_lossy()
                            .into_owned()
                    },
                );
                format!("{context} ·{}", *occurrence)
            } else {
                base
            };
            (id, sanitize_title(&title))
        })
        .collect()
}

fn editor_buffer_base(path: Option<&std::path::Path>, scratch_label: &str) -> String {
    path.map_or_else(
        || scratch_label.to_owned(),
        |path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        },
    )
}

pub(super) fn sanitize_title(title: &str) -> String {
    let mut output = String::new();
    for character in title.chars() {
        let character = if character.is_control() {
            '\u{fffd}'
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > 4096 {
            break;
        }
        output.push(character);
    }
    output
}
