//! Statusline and buffer-line segment ordering and cached plugin text.

use crate::app::windows::Windows;
use crate::ui::Style;
use crate::ui::render::chrome::{ChromeLayout, ChromeSegment};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ChromeLine {
    Status,
    Buffer,
}

pub(super) struct NamedSegment {
    line: ChromeLine,
    order: i32,
    parts: Vec<String>,
    pub(super) command: Option<(String, crate::editor::CommandAuthority)>,
}

impl Windows {
    pub(in crate::app) fn set_named_segment(
        &mut self,
        line: ChromeLine,
        name: String,
        order: i32,
        text: Vec<(String, Style)>,
        command: Option<String>,
        authority: crate::editor::CommandAuthority,
    ) -> Result<(), String> {
        authority
            .require(crate::editor::CommandAccess::Ui)
            .map_err(|error| error.to_string())?;
        self.remove_named_segment(&name);
        let parts: Vec<_> = text
            .into_iter()
            .enumerate()
            .map(|(index, text)| {
                let part = format!("{name}/{index}");
                self.set_segment(line, part.clone(), Some(text));
                part
            })
            .collect();
        self.named_segments.insert(
            name,
            NamedSegment {
                line,
                order,
                parts,
                command: command.map(|command| (command, authority)),
            },
        );
        let mut segments: Vec<_> = self
            .named_segments
            .iter()
            .filter(|(_, segment)| segment.line == line)
            .collect();
        segments.sort_by_key(|(name, segment)| (segment.order, *name));
        let parts: Vec<_> = segments
            .into_iter()
            .flat_map(|(_, segment)| segment.parts.iter().cloned())
            .collect();
        let order = match line {
            ChromeLine::Status => &mut self.chrome.status,
            ChromeLine::Buffer => &mut self.chrome.buffer,
        };
        order.retain(|name| !parts.contains(name));
        order.extend(parts);
        Ok(())
    }

    pub(in crate::app) fn remove_named_segment(&mut self, name: &str) {
        if let Some(segment) = self.named_segments.remove(name) {
            for part in segment.parts {
                self.set_segment(segment.line, part, None);
            }
        }
    }

    pub(in crate::app) fn segment_command(
        &self,
        part: &str,
    ) -> Option<(String, crate::editor::CommandAuthority)> {
        self.named_segments
            .values()
            .find(|segment| segment.parts.iter().any(|name| name == part))
            .and_then(|segment| segment.command.clone())
    }

    /// An empty order intentionally removes every optional segment.
    #[allow(dead_code)]
    pub(in crate::app) fn order_segments(&mut self, line: ChromeLine, names: Vec<String>) {
        let order = match line {
            ChromeLine::Status => &mut self.chrome.status,
            ChromeLine::Buffer => &mut self.chrome.buffer,
        };
        *order = names;
        self.content_line.signature = None;
    }

    #[allow(dead_code)]
    pub(in crate::app) fn set_segment(
        &mut self,
        line: ChromeLine,
        name: String,
        text: Option<(String, Style)>,
    ) {
        let (order, values) = match line {
            ChromeLine::Status => (&mut self.chrome.status, &mut self.chrome.status_text),
            ChromeLine::Buffer => (&mut self.chrome.buffer, &mut self.chrome.buffer_text),
        };
        if let Some(text) = text {
            if !order.contains(&name) {
                order.push(name.clone());
            }
            values.insert(name, text);
        } else {
            order.retain(|candidate| candidate != &name);
            values.remove(&name);
        }
        self.content_line.signature = None;
    }

    pub(super) fn chrome_layout(&self) -> &ChromeLayout {
        &self.chrome
    }
}

pub(super) fn project_buffer_segments(
    entry: &crate::ui::ContentTabEntry,
    config: &crate::config::Config,
    layout: &ChromeLayout,
) -> Vec<ChromeSegment> {
    let native = crate::ui::render::overlays::builtin_content_tab_segments(entry, config);
    layout
        .buffer
        .iter()
        .flat_map(|name| {
            if let Some((text, style)) = layout.buffer_text.get(name) {
                vec![ChromeSegment {
                    name: name.clone(),
                    text: text.clone(),
                    style: *style,
                }]
            } else {
                native
                    .iter()
                    .filter(|segment| &segment.name == name)
                    .cloned()
                    .collect()
            }
        })
        .collect()
}

impl crate::app::App {
    pub(in crate::app) fn handle_chrome_mouse(
        &mut self,
        key: &crate::editor::input::KeyEvent,
        column: u16,
        row: u16,
    ) -> bool {
        if key.terminal_mouse() != Some(crate::editor::input::TerminalMouseInput::LeftDown)
            || self.input.prompt().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
            || self.ui.settings.is_some()
        {
            return false;
        }
        let config = self.configuration.settings();
        let width = self.window_session.width();
        let height = self.window_session.height();
        let part = if row.checked_add(1) == Some(height)
            && crate::ui::render::screen_foot_rows(&self.state, &self.ui, config, height) > 0
        {
            self.state.active_view().and_then(|view| {
                crate::ui::render::chrome::status_segment_at_column(
                    &self.state,
                    view,
                    &self.ui,
                    config,
                    width,
                    height,
                    column,
                )
            })
        } else if let Some(bounds) = self.workspace_bounds() {
            let shown = config.ui.buffer_line.enabled
                && (!config.ui.buffer_line.hide_when_single || self.ui.content_tabs.len() > 1);
            if shown
                && row
                    == bounds
                        .y
                        .saturating_add(u16::from(self.state.editor_tabs().count() > 1))
                && column >= bounds.x
            {
                crate::ui::render::overlays::content_segment_at_column(
                    &self.ui.content_tabs,
                    config,
                    bounds.width,
                    column - bounds.x,
                )
            } else {
                None
            }
        } else {
            None
        };
        let Some((command, authority)) =
            part.and_then(|part| self.window_session.segment_command(&part))
        else {
            return false;
        };
        match crate::editor::CommandInvocation::new(
            &command,
            crate::editor::InvocationSource::Plugin,
        ) {
            Ok(invocation) => {
                self.invoke_with_authority(invocation, authority);
            }
            Err(error) => self.show_error(error),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn buffer_projection_preserves_default_cells_and_hit_testing_after_reordering() {
        let config = crate::config::load_bundled_config().config;
        let mut entry = crate::ui::ContentTabEntry {
            identity: crate::ui::ContentTabIdentity(crate::windows::ContentId(1)),
            kind: crate::ui::ContentTabKind::Text,
            title: "main.rs".into(),
            active: true,
            dirty: true,
            pinned: true,
            diagnostic: Some(crate::ui::ContentTabDiagnostic {
                severity: crate::ui::ContentTabDiagnosticSeverity::Error,
                count: 2,
            }),
            icon: "R".into(),
            closeable: true,
            segments: None,
        };
        let mut windows = Windows::default();
        let original = crate::ui::render::overlays::builtin_content_tab_segments(&entry, &config);
        entry.segments = Some(project_buffer_segments(
            &entry,
            &config,
            windows.chrome_layout(),
        ));
        assert_eq!(entry.segments.as_ref().unwrap(), &original);
        windows.set_segment(
            ChromeLine::Buffer,
            "plugin.example".into(),
            Some(("X".into(), Style::Warning)),
        );
        windows.order_segments(
            ChromeLine::Buffer,
            vec!["plugin.example".into(), "title".into()],
        );
        entry.segments = Some(project_buffer_segments(
            &entry,
            &config,
            windows.chrome_layout(),
        ));
        let text: String = entry
            .segments
            .as_ref()
            .unwrap()
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        assert!(text.starts_with('X'));
        assert!(!text.contains('R'));
        assert_eq!(
            crate::ui::render::content_tab_at_column(&[entry.clone()], &config, 80, 0),
            Some(entry.identity)
        );
    }

    #[test]
    fn segments_can_be_added_reordered_and_removed() {
        let mut windows = Windows::default();
        windows.set_segment(
            ChromeLine::Status,
            "plugin.example.clock".into(),
            Some(("12:00".into(), Style::Status)),
        );
        assert_eq!(
            windows.chrome.status.last().unwrap(),
            "plugin.example.clock"
        );
        windows.order_segments(
            ChromeLine::Status,
            vec!["plugin.example.clock".into(), "mode".into()],
        );
        assert_eq!(windows.chrome.status[0], "plugin.example.clock");
        windows.set_segment(ChromeLine::Status, "plugin.example.clock".into(), None);
        assert_eq!(windows.chrome.status, ["mode"]);
        assert!(windows.chrome.status_text.is_empty());
        windows.order_segments(ChromeLine::Buffer, Vec::new());
        assert!(windows.chrome.buffer.is_empty());
    }
}
