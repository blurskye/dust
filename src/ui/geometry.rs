//! Content rectangles shared by surface painting and interaction.
use crate::{
    config::{AmbiguousWidth, Config},
    ui::{PickerUiState, window::WindowRect},
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
        let numbers = preview
            .first_line
            .saturating_add(preview.lines.len())
            .max(1)
            .to_string()
            .len();
        preview
            .lines
            .iter()
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
        top.saturating_add(height.saturating_mul(2) / 3).clamp(
            top.saturating_add(6),
            top.saturating_add(height.saturating_sub(4)),
        )
    });
    let list_right = divider.unwrap_or(left.saturating_add(width - 1));
    let list_bottom = stacked_divider.unwrap_or(top.saturating_add(height - 1));
    let x = left.saturating_add(2);
    let y = top.saturating_add(3);
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
    })
}

#[must_use]
pub fn explorer_body_geometry(rect: WindowRect, config: &Config) -> WindowRect {
    WindowRect {
        x: rect.x,
        y: rect
            .y
            .saturating_add(3)
            .min(rect.y.saturating_add(rect.height)),
        width: rect.width.saturating_sub(1),
        height: rect
            .height
            .saturating_sub(u16::from(config.ui.status_line.enabled))
            .saturating_sub(3)
            .min(u16::try_from(config.ui.explorer.maximum_visible_rows).unwrap_or(u16::MAX)),
    }
}

#[must_use]
pub fn documentation_page_rows(config: &Config, width: u16, height: u16) -> usize {
    if !config.ui.documentation_popup.enabled || width < 4 {
        return 0;
    }
    usize::from(config.ui.documentation_popup.max_height.saturating_sub(2)).min(usize::from(
        height
            .saturating_sub(u16::from(config.ui.status_line.enabled))
            .saturating_sub(2),
    ))
}

#[must_use]
pub fn notification_geometry(config: &Config, width: u16, height: u16) -> Option<WindowRect> {
    let policy = &config.ui.notifications;
    let x = policy.left.min(width);
    let available = policy.max_width.min(width.saturating_sub(x));
    if available == 0 || height == 0 {
        return None;
    }
    Some(WindowRect {
        x,
        y: height
            .saturating_sub(policy.rows_from_bottom)
            .min(height - 1),
        width: available,
        height: 1,
    })
}

#[derive(Clone, Debug, Default)]
pub struct SurfaceContentGeometry {
    pub picker: Option<PickerContentGeometry>,
    pub explorer: Option<WindowRect>,
    pub terminal: Option<WindowRect>,
    pub documentation_rows: usize,
}
