//! Content rectangles shared by surface painting and interaction.
use crate::{
    config::{AmbiguousWidth, Config},
    ui::{PickerSurfaceKind, PickerUiState, window::WindowRect},
};
use unicode_width::UnicodeWidthStr;

pub(crate) fn text_width(text: &str, ambiguous_width: AmbiguousWidth) -> usize {
    if ambiguous_width == AmbiguousWidth::Wide {
        UnicodeWidthStr::width_cjk(text)
    } else {
        UnicodeWidthStr::width(text)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PickerContentGeometry {
    pub outer: WindowRect,
    pub list: WindowRect,
    pub divider: Option<u16>,
    pub stacked_divider: Option<u16>,
    /// The rows the preview has inside the border, zero when none is showing.
    pub preview_rows: u16,
}

/// Theme selection leaves most of the editor visible for live color preview.
/// Other picker sizes retain their configured behavior.
#[must_use]
pub(crate) fn picker_window_geometry(
    picker: &PickerUiState,
    config: &Config,
    bounds: WindowRect,
) -> Option<WindowRect> {
    let mut rect = crate::ui::picker_geometry(config, bounds)?;
    if picker.surface == crate::ui::PickerSurfaceKind::Theme {
        rect.width = rect.width.min(64);
        rect.height = rect
            .height
            .min(u16::try_from(picker.rows.len().clamp(2, 8)).unwrap_or(8) + 4);
        rect.x = bounds
            .x
            .saturating_add(bounds.width.saturating_sub(rect.width) / 2);
        rect.y = bounds
            .y
            .saturating_add((bounds.height.saturating_sub(rect.height) / 4).min(4));
    }
    Some(rect)
}

/// The preview decision uses the same bounded content sample as painting.
#[must_use]
pub fn picker_content_geometry(
    picker: &PickerUiState,
    config: &Config,
    rect: WindowRect,
) -> Option<PickerContentGeometry> {
    let WindowRect {
        x: left,
        y: top,
        width,
        height,
    } = rect;
    if width < 4 || height < 6 {
        return None;
    }
    let preview = picker
        .preview
        .as_ref()
        .filter(|_| width >= 40 && height >= 12);
    let list_width = picker
        .rows
        .iter()
        .take(64)
        .map(|row| {
            text_width(&row.label, config.editor.ambiguous_width)
                .saturating_add(text_width(&row.detail, config.editor.ambiguous_width))
                .saturating_add(text_width(&row.source, config.editor.ambiguous_width))
                .saturating_add(8)
        })
        .max()
        .unwrap_or(28)
        .clamp(28, 72);
    let preview_width = preview.map_or(0, |preview| {
        // Measurement reads the same rows the painter draws, so the two cannot disagree
        // about how wide a preview is.
        let (rows, numbers) = match &preview.body {
            crate::ui::PickerPreviewBody::Text(text) => (
                text.lines.clone(),
                text.first_line
                    .saturating_add(text.lines.len())
                    .max(1)
                    .to_string()
                    .len(),
            ),
            crate::ui::PickerPreviewBody::Diff(diff) => {
                let numbers = diff.number_width().saturating_mul(2).saturating_add(3);
                // The diff's own width is measured from the same rows the painter reads.
                let width = diff.content_width(config.editor.ambiguous_width);
                (vec![], width.saturating_add(numbers).saturating_add(4))
            }
        };
        if rows.is_empty() {
            return numbers.clamp(32, 88);
        }
        rows.iter()
            .take(64)
            .map(|line| text_width(line, config.editor.ambiguous_width))
            .max()
            .unwrap_or(28)
            .saturating_add(numbers)
            .saturating_add(4)
            .clamp(32, 88)
    });
    let side_by_side = preview.is_some()
        && usize::from(width) >= list_width.saturating_add(preview_width).saturating_add(3);
    let divider = side_by_side.then(|| {
        left.saturating_add(u16::try_from(list_width).unwrap_or(width - 1))
            .saturating_add(1)
            .min(left.saturating_add(width - 2))
    });
    let stacked_divider = (preview.is_some() && !side_by_side).then(|| {
        // A short list of changes leaves its spare rows to the diff.
        let list_rows = (picker.surface == PickerSurfaceKind::Changes)
            .then(|| u16::try_from(picker.rows.len()).unwrap_or(u16::MAX).saturating_add(3))
            .unwrap_or(u16::MAX);
        let list_share = (height.saturating_mul(2) / 3).min(list_rows);
        top.saturating_add(list_share).clamp(
            top.saturating_add(6),
            top.saturating_add(height.saturating_sub(4)),
        )
    });
    let list_right = divider.unwrap_or(left.saturating_add(width - 1));
    let list_bottom = stacked_divider.unwrap_or(top.saturating_add(height - 1));
    let x = left.saturating_add(2);
    let y = top.saturating_add(3);
    let preview_rows = if preview.is_some() {
        height
            .saturating_sub(stacked_divider.map_or(0, |divider| divider.saturating_sub(top)))
            .saturating_sub(2)
    } else {
        0
    };
    Some(PickerContentGeometry {
        outer: rect,
        list: WindowRect {
            x,
            y,
            width: list_right.saturating_sub(x),
            height: list_bottom.saturating_sub(y),
        },
        divider,
        stacked_divider,
        preview_rows,
    })
}

/// The box around the explorer's search row.
const EXPLORER_SEARCH_ROWS: u16 = 3;

#[must_use]
pub fn explorer_body_geometry(rect: WindowRect, config: &Config) -> WindowRect {
    WindowRect {
        x: rect.x,
        y: rect.y.saturating_add(EXPLORER_SEARCH_ROWS).min(rect.y.saturating_add(rect.height)),
        width: rect.width.saturating_sub(1),
        height: rect
            .height
            .saturating_sub(EXPLORER_SEARCH_ROWS)
            .min(u16::try_from(config.ui.explorer.maximum_visible_rows).unwrap_or(u16::MAX)),
    }
}

#[must_use]
pub fn documentation_page_rows(config: &Config, width: u16, height: u16) -> usize {
    if !config.ui.documentation_popup.enabled || width < 4 {
        return 0;
    }
    usize::from(config.ui.documentation_popup.max_height.saturating_sub(2))
        .min(usize::from(height.saturating_sub(2)))
}

pub(crate) fn error_notification_geometry(
    config: &Config,
    width: u16,
    height: u16,
) -> Option<WindowRect> {
    let margin = 1_u16.min(width / 2);
    let available = config
        .ui
        .notifications
        .max_width
        .min(width.saturating_sub(margin.saturating_mul(2)));
    (available >= 4 && height >= 3).then_some(WindowRect {
        x: width.saturating_sub(margin).saturating_sub(available),
        y: 1.min(height.saturating_sub(3)),
        width: available,
        height: 3,
    })
}

#[derive(Clone, Debug, Default)]
pub struct SurfaceContentGeometry {
    pub picker: Option<PickerContentGeometry>,
    pub explorer: Option<WindowRect>,
    pub terminal: Option<WindowRect>,
    pub documentation_rows: usize,
}
