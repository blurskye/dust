//! Content rectangles shared by surface painting and interaction.
use crate::config::AmbiguousWidth;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn text_width(text: &str, ambiguous_width: AmbiguousWidth) -> usize {
    if ambiguous_width == AmbiguousWidth::Wide {
        UnicodeWidthStr::width_cjk(text)
    } else {
        UnicodeWidthStr::width(text)
    }
}

/// Width of text painted by CellGrid, including its visible replacements.
pub(crate) fn cell_text_width(text: &str, ambiguous: AmbiguousWidth) -> usize {
    text.graphemes(true)
        .map(|grapheme| {
            let safe = if grapheme == "\t" {
                " "
            } else if grapheme.chars().any(char::is_control) {
                crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT
            } else if text_width(grapheme, ambiguous) == 0 {
                crate::ui::grid::ZERO_WIDTH_REPLACEMENT
            } else {
                grapheme
            };
            text_width(safe, ambiguous).clamp(1, 2)
        })
        .sum()
}

/// What fits of `text`, ending in an ellipsis when some had to go.
pub(super) fn ellipsized(
    text: &str,
    maximum: usize,
    ambiguous: crate::config::AmbiguousWidth,
) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if text_width(text, ambiguous) <= maximum || maximum == 0 {
        return if maximum == 0 {
            String::new()
        } else {
            text.to_owned()
        };
    }
    let mut kept = String::new();
    let mut width = text_width("…", ambiguous);
    if width > maximum {
        return kept;
    }
    for grapheme in text.graphemes(true) {
        width += text_width(grapheme, ambiguous).max(1);
        if width > maximum {
            break;
        }
        kept.push_str(grapheme);
    }
    format!("{kept}…")
}

/// Key hints broken between items into at most two rows, so narrow screens keep them all.
pub(super) fn wrapped_key_help(
    help: &str,
    maximum: usize,
    ambiguous: crate::config::AmbiguousWidth,
) -> Vec<String> {
    const SEPARATOR: &str = " · ";
    let mut first = String::new();
    let mut rest = Vec::new();
    for item in help.split(SEPARATOR) {
        let candidate = if first.is_empty() {
            item.to_owned()
        } else {
            format!("{first}{SEPARATOR}{item}")
        };
        if rest.is_empty() && text_width(&candidate, ambiguous) <= maximum {
            first = candidate;
        } else {
            rest.push(item);
        }
    }
    let mut rows = vec![ellipsized(&first, maximum, ambiguous)];
    if !rest.is_empty() {
        rows.push(ellipsized(&rest.join(SEPARATOR), maximum, ambiguous));
    }
    rows
}

/// A footer legend shortened to whole entries that fit.
///
/// A narrow surface drops whole actions rather than clipping one, so a footer can never show
/// half an action that reads as a different one.
pub(crate) fn fitted_footer(
    text: &str,
    width: usize,
    ambiguous_width: crate::config::AmbiguousWidth,
) -> String {
    if text_width(text, ambiguous_width) <= width {
        return text.to_owned();
    }
    let mut fitted = String::new();
    for entry in text.split(" · ") {
        let candidate = if fitted.is_empty() {
            entry.to_owned()
        } else {
            format!("{fitted} · {entry}")
        };
        if text_width(&candidate, ambiguous_width) > width {
            break;
        }
        fitted = candidate;
    }
    fitted
}
