//! Named, cached chrome segments shared by projection and drawing.

use crate::ui::Style;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromeSegment {
    pub name: String,
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromeLayout {
    pub status: Vec<String>,
    pub buffer: Vec<String>,
    pub status_text: BTreeMap<String, (String, Style)>,
    pub buffer_text: BTreeMap<String, (String, Style)>,
}

impl Default for ChromeLayout {
    fn default() -> Self {
        #[derive(serde::Deserialize)]
        struct Defaults {
            status: Vec<String>,
            buffer: Vec<String>,
        }
        static DEFAULTS: std::sync::OnceLock<Defaults> = std::sync::OnceLock::new();
        let defaults = DEFAULTS.get_or_init(|| {
            toml::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/defaults/chrome.toml"
            )))
            .expect("bundled chrome segment names")
        });
        Self {
            status: defaults.status.clone(),
            buffer: defaults.buffer.clone(),
            status_text: BTreeMap::new(),
            buffer_text: BTreeMap::new(),
        }
    }
}

/// Hit testing uses the same clipped statusline layout as drawing.
pub(crate) fn status_segment_at_column(
    state: &crate::editor::EditorState,
    view: crate::editor::ViewId,
    ui: &crate::ui::UiState,
    config: &crate::config::Config,
    width: u16,
    height: u16,
    column: u16,
) -> Option<String> {
    if height == 0 || column >= width {
        return None;
    }
    let mut grid = crate::ui::CellGrid::new(width, height);
    super::editor::render_status_line(&mut grid, state, view, ui, config, width, height - 1)
        .into_iter()
        .find(|(_, range)| range.contains(&column))
        .map(|(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_segments_draw_added_text_and_reorder_native_blocks() {
        let mut state = crate::editor::EditorState::new("/workspace".into());
        let (_, view) = state
            .open_text(
                crate::editor::text::TextBuffer::from_text("text"),
                Some("/workspace/main.rs".into()),
            )
            .unwrap();
        let config = crate::config::load_bundled_config().config;
        let mut ui = crate::ui::UiState::default();
        let mut original = crate::ui::CellGrid::new(120, 1);
        crate::ui::render::editor::render_status_line(
            &mut original,
            &state,
            view,
            &ui,
            &config,
            120,
            0,
        );
        assert!(
            original.row_text(0).find("NORMAL").unwrap()
                < original.row_text(0).find("main.rs").unwrap()
        );
        ui.chrome.status = vec!["plugin.example".into(), "file".into(), "mode".into()];
        ui.chrome.status_text.insert(
            "plugin.example".into(),
            ("Clock".into(), Style::StatusWarning),
        );
        let mut custom = crate::ui::CellGrid::new(120, 1);
        crate::ui::render::editor::render_status_line(
            &mut custom,
            &state,
            view,
            &ui,
            &config,
            120,
            0,
        );
        let row = custom.row_text(0);
        assert!(row.find("Clock").unwrap() < row.find("main.rs").unwrap());
        assert!(row.find("main.rs").unwrap() < row.find("NORMAL").unwrap());
        ui.chrome.status.clear();
        let mut empty = crate::ui::CellGrid::new(120, 1);
        crate::ui::render::editor::render_status_line(
            &mut empty, &state, view, &ui, &config, 120, 0,
        );
        assert!(!empty.row_text(0).contains("NORMAL"));
        assert!(!empty.row_text(0).contains("main.rs"));
    }
}
