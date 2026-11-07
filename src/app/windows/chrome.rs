//! Statusline and buffer-line segment ordering and cached plugin text.

use crate::app::windows::Windows;
use crate::ui::Style;
use crate::ui::render::chrome::{ChromeLayout, ChromeSegment};

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(in crate::app) enum ChromeLine {
    Status,
    Buffer,
}

impl Windows {
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
