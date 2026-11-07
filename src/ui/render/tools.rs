//! Tool catalog, runtime choices and installation progress drawing.

use crate::config::BorderStyle;
use crate::config::Config;
use crate::ui::Style;
use crate::ui::geometry::text_width;
use crate::ui::grid::Frame;
use crate::ui::render::ToolMenuState;
use crate::ui::render::overlays::truncate_content_title;

use crate::ui::geometry::ellipsized;
use crate::ui::geometry::wrapped_key_help;
use crate::ui::render::picker::clip_path_tail;
use crate::ui::render::picker::looks_like_path;

pub(crate) const TOOL_MANAGER_MINIMUM_HEIGHT: u16 = 14;

pub(crate) const TOOL_MANAGER_MINIMUM_WIDTH: u16 = 48;

pub(crate) const TOOL_MANAGER_WIDTH_PERCENT: u16 = 82;

pub(crate) const TOOL_MANAGER_HEIGHT_PERCENT: u16 = 84;

pub(crate) const TOOL_MANAGER_TITLE: &str = "Tools";

pub(crate) const TOOL_MANAGER_HELP: &str =
    "j/k move · / search · Tab category · Enter install or start · r refresh · Esc close";

pub(crate) const TOOL_LANGUAGE_SEPARATOR: &str = ", ";

pub(crate) const TOOL_CATEGORY_ORDER: &[&str] =
    &["all", "lsp", "dap", "linter", "formatter", "agent"];

pub(super) fn render_tool_menu(
    frame: &mut Frame,
    menu: &ToolMenuState,
    config: &Config,
    terminal_width: u16,
    terminal_height: u16,
) {
    let width = (u32::from(terminal_width) * u32::from(TOOL_MANAGER_WIDTH_PERCENT) / 100)
        .try_into()
        .unwrap_or(terminal_width);
    let height = (u32::from(terminal_height) * u32::from(TOOL_MANAGER_HEIGHT_PERCENT) / 100)
        .try_into()
        .unwrap_or(terminal_height);
    let width = width
        .max(TOOL_MANAGER_MINIMUM_WIDTH.min(terminal_width))
        .min(terminal_width);
    let mut height = height
        .max(TOOL_MANAGER_MINIMUM_HEIGHT.min(terminal_height))
        .min(terminal_height);
    if let Some(picker) = &menu.runtime_picker {
        let compact_height = u16::try_from(picker.options.len())
            .unwrap_or(u16::MAX)
            .saturating_mul(2)
            .saturating_add(11)
            .max(TOOL_MANAGER_MINIMUM_HEIGHT);
        height = height.min(compact_height);
    }
    if width < 4 || height < 6 {
        return;
    }
    let left = terminal_width.saturating_sub(width) / 2;
    let top = terminal_height.saturating_sub(height) / 2;
    for y in top..top.saturating_add(height) {
        for x in left..left.saturating_add(width) {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        frame.grid.border_glyphs(BorderStyle::Single);
    for x in left..left + width {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::Border);
        frame
            .grid
            .set_cell(x, top + height - 1, &horizontal.to_string(), Style::Border);
    }
    for y in top..top + height {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::Border);
        frame
            .grid
            .set_cell(left + width - 1, y, &vertical.to_string(), Style::Border);
    }
    for (x, y, character) in [
        (left, top, top_left),
        (left + width - 1, top, top_right),
        (left, top + height - 1, bottom_left),
        (left + width - 1, top + height - 1, bottom_right),
    ] {
        frame
            .grid
            .set_cell(x, y, &character.to_string(), Style::Border);
    }
    let title = menu.runtime_picker.as_ref().map_or_else(
        || {
            if menu.searching || !menu.query.is_empty() {
                let cursor = if menu.searching { "▏" } else { "" };
                format!(" {} · /{}{cursor} ", TOOL_MANAGER_TITLE, menu.query)
            } else {
                format!(" {} ", TOOL_MANAGER_TITLE)
            }
        },
        |picker| {
            format!(
                " {} · {} · choose build ",
                TOOL_MANAGER_TITLE, picker.server_id
            )
        },
    );
    frame
        .grid
        .put_text_clipped(left + 2, top, &title, Style::Accent, left + width - 1);
    let content_left = left + 2;
    let content_width = width.saturating_sub(4);
    let ambiguous = config.editor.ambiguous_width;
    let clip = |text: &str| ellipsized(text, usize::from(content_width), ambiguous);
    let clip_indented = |text: &str| {
        truncate_content_title(
            text,
            usize::from(content_width.saturating_sub(2)),
            ambiguous,
        )
    };
    let mut y = top + 1;

    if let Some(picker) = &menu.runtime_picker {
        // Keep the help, header, two-row option and details in separate regions.
        if height < 11 {
            frame.grid.put_text_clipped(
                content_left,
                y,
                "Resize to choose a build",
                Style::Muted,
                left + width - 1,
            );
            frame.cursor = None;
            return;
        }
        for (row, help) in wrapped_key_help(
            "j/k move · Enter use this build · i install · u uninstall · r refresh · Esc back",
            usize::from(content_width),
            ambiguous,
        )
        .iter()
        .enumerate()
        {
            frame
                .grid
                .put_text(content_left, y + row as u16, help, Style::Muted);
        }
        y += 2;
        let picker_version_x = content_left.saturating_add(2);
        let picker_release_x = content_left.saturating_add(content_width.saturating_mul(22) / 100);
        let picker_source_x = content_left.saturating_add(content_width.saturating_mul(46) / 100);
        let picker_state_x = content_left.saturating_add(content_width.saturating_mul(68) / 100);
        let picker_active_x = content_left.saturating_add(content_width.saturating_mul(91) / 100);
        frame
            .grid
            .put_text(picker_version_x, y, "VERSION", Style::Muted);
        frame
            .grid
            .put_text(picker_release_x, y, "RELEASE", Style::Muted);
        frame
            .grid
            .put_text(picker_source_x, y, "SOURCE", Style::Muted);
        frame
            .grid
            .put_text(picker_state_x, y, "STATE", Style::Muted);
        frame
            .grid
            .put_text(picker_active_x, y, "IN USE", Style::Muted);
        y += 1;

        let detail_top = top.saturating_add(height).saturating_sub(5);
        let visible = usize::from(detail_top.saturating_sub(y) / 2).max(1);
        for (index, option) in picker
            .options
            .iter()
            .enumerate()
            .skip(
                picker
                    .scroll
                    .min(picker.selected)
                    .max(picker.selected.saturating_add(1).saturating_sub(visible))
                    .min(picker.options.len().saturating_sub(visible)),
            )
            .take(visible)
        {
            let focused = index == picker.selected;
            let marker = if focused { ">" } else { " " };
            let active = if option.selected && option.installed {
                "●"
            } else {
                ""
            };
            let style = if focused {
                Style::Selection
            } else if option.selected {
                Style::Accent
            } else if option.installable || option.installed {
                Style::Default
            } else {
                Style::Muted
            };
            if focused {
                for x in content_left..content_left.saturating_add(content_width) {
                    frame.grid.set_cell(x, y, " ", Style::Selection);
                }
            }
            frame.grid.put_text(content_left, y, marker, style);
            let column = |text: &str, from: u16, to: u16| {
                ellipsized(
                    text,
                    usize::from(to.saturating_sub(from).saturating_sub(1)),
                    ambiguous,
                )
            };
            frame.grid.put_text(
                picker_version_x,
                y,
                &column(&option.version, picker_version_x, picker_release_x),
                style,
            );
            let release = if option.release == "catalog build" {
                "—"
            } else {
                option.release.as_str()
            };
            frame.grid.put_text(
                picker_release_x,
                y,
                &column(release, picker_release_x, picker_source_x),
                style,
            );
            frame.grid.put_text(
                picker_source_x,
                y,
                &column(&option.label, picker_source_x, picker_state_x),
                style,
            );
            frame.grid.put_text(
                picker_state_x,
                y,
                &column(&option.status, picker_state_x, picker_active_x),
                style,
            );
            frame.grid.put_text(picker_active_x, y, active, style);
            y += 1;
            if option.installed {
                frame.grid.put_text(
                    content_left + 2,
                    y,
                    &clip_indented(&option.location),
                    Style::Muted,
                );
            }
            y += 1;
        }

        if let Some(option) = picker.options.get(picker.selected) {
            y = detail_top;
            frame.grid.put_text(
                content_left,
                y,
                &clip(&horizontal.to_string().repeat(usize::from(content_width))),
                Style::Border,
            );
            y += 1;
            let action = if option.selected && option.installed {
                if option.uninstallable {
                    "In use · u uninstall"
                } else {
                    "In use"
                }
            } else if option.installed {
                if option.uninstallable {
                    "Enter use this build · u uninstall"
                } else {
                    "Enter use this build"
                }
            } else if option.installable {
                "Enter or i install this build"
            } else {
                "Not available on this system"
            };
            frame
                .grid
                .put_text(content_left, y, &clip(action), Style::Accent);
            y += 1;
            frame
                .grid
                .put_text(content_left, y, &clip(&option.note), Style::Muted);
        }
        frame.cursor = None;
        return;
    }

    for (row, help) in wrapped_key_help(TOOL_MANAGER_HELP, usize::from(content_width), ambiguous)
        .iter()
        .enumerate()
    {
        frame
            .grid
            .put_text(content_left, y + row as u16, help, Style::Muted);
    }
    y += 2;
    let mut tab_x = content_left;
    for category in TOOL_CATEGORY_ORDER {
        let label = config
            .ui
            .tool_manager
            .categories
            .get(*category)
            .map_or(*category, String::as_str);
        let text = format!(" {label} ");
        let style = if category == &menu.category {
            Style::StatusAccent
        } else {
            Style::Status
        };
        let right = content_left.saturating_add(content_width);
        let next_x = frame.grid.put_text_clipped(tab_x, y, &text, style, right);
        if usize::from(next_x.saturating_sub(tab_x)) < text_width(&text, ambiguous) {
            break;
        }
        tab_x = next_x.saturating_add(1);
    }
    y += 2;
    let name_x = content_left.saturating_add(2);
    let version_x = content_left.saturating_add(content_width.saturating_mul(50) / 100);
    let runtime_x = content_left.saturating_add(content_width.saturating_mul(64) / 100);
    let state_x = content_left.saturating_add(content_width.saturating_mul(78) / 100);
    let right = content_left.saturating_add(content_width);
    // Too narrow for every column: the version and source stay in the details below.
    let compact = content_width < 70;
    frame.grid.put_text(name_x, y, "NAME", Style::Muted);
    if !compact {
        frame.grid.put_text(version_x, y, "VERSION", Style::Muted);
        frame.grid.put_text(runtime_x, y, "SOURCE", Style::Muted);
    }
    frame.grid.put_text(state_x, y, "STATE", Style::Muted);
    y += 1;

    let remaining = top
        .saturating_add(height)
        .saturating_sub(1)
        .saturating_sub(y);
    // A short window keeps what the user can act on, the note and the keys, and gives the list
    // the rest.
    let short = remaining < 12;
    let usual_detail_rows = if short { 5u16 } else { 9 };
    // A failure explains itself in the details, so it takes rows from the list until it fits.
    let failure_rows = menu
        .rows
        .get(menu.selected)
        .filter(|row| tool_state_glyph(row).1 == Style::Error)
        .map_or(0, |row| {
            let note = crate::ui::render::wrap_plain_text(
                &row.note,
                usize::from(content_width),
                ambiguous,
            )
            .len();
            usual_detail_rows - 2 + u16::try_from(note).unwrap_or(u16::MAX)
        })
        .min(remaining.saturating_sub(2));
    let detail_rows = usual_detail_rows
        .max(failure_rows)
        .min(remaining.saturating_sub(1));
    let list_bottom = top + height - 1 - detail_rows;
    // At most two group headings; keep the selected tool visible across the boundary.
    let visible_rows = usize::from(list_bottom.saturating_sub(y).saturating_sub(2)).max(1);
    let scroll = menu
        .scroll
        .max((menu.selected + 1).saturating_sub(visible_rows));
    let mut group = None;
    for (index, row) in menu.rows.iter().enumerate().skip(scroll).take(visible_rows) {
        if group != Some(row.for_project) && y + 1 < list_bottom {
            frame.grid.put_text_clipped(
                content_left,
                y,
                if row.for_project {
                    "For this project"
                } else {
                    "Other tools"
                },
                Style::Accent,
                right,
            );
            y += 1;
            group = Some(row.for_project);
        }
        if y >= list_bottom {
            break;
        }
        let selected = index == menu.selected;
        let style = if selected {
            Style::Selection
        } else if row.installed {
            Style::Default
        } else {
            Style::Muted
        };
        if selected {
            for x in content_left..right {
                frame.grid.set_cell(x, y, " ", Style::Selection);
            }
        }
        let (glyph, glyph_style) = tool_state_glyph(row);
        frame.grid.put_text(content_left, y, glyph, glyph_style);
        let name_right = if compact { state_x } else { version_x }.saturating_sub(2);
        let column = |text: &str, from: u16, to: u16| {
            ellipsized(text, usize::from(to.saturating_sub(from)), ambiguous)
        };
        frame
            .grid
            .put_text(name_x, y, &column(&row.name, name_x, name_right), style);
        let languages_x = name_x
            .saturating_add(
                u16::try_from(text_width(&row.name, config.editor.ambiguous_width)).unwrap_or(0),
            )
            .saturating_add(2);
        if !row.languages.is_empty() && languages_x < name_right {
            frame.grid.put_text_clipped(
                languages_x,
                y,
                &column(&row.languages, languages_x, name_right),
                if selected {
                    Style::Selection
                } else {
                    Style::Muted
                },
                name_right,
            );
        }
        if !compact {
            let runtime_right = runtime_x.saturating_sub(1);
            frame.grid.put_text(
                version_x,
                y,
                &column(&row.version, version_x, runtime_right),
                style,
            );
            let state_left = state_x.saturating_sub(1);
            frame.grid.put_text(
                runtime_x,
                y,
                &column(&row.runtime, runtime_x, state_left),
                style,
            );
        }
        // The list shows the short state; byte counts stay in the detail pane.
        let state = row.lifecycle.split(" (").next().unwrap_or_default();
        frame.grid.put_text(
            state_x,
            y,
            &column(state, state_x, right),
            if selected {
                Style::Selection
            } else {
                glyph_style
            },
        );
        y += 1;
    }

    if menu.rows.is_empty() {
        let hint = config
            .messages
            .get("tool_no_matches")
            .expect("bundled tool empty hint");
        for line in crate::ui::render::wrap_plain_text(hint, usize::from(content_width), ambiguous)
        {
            if y >= top + height - 1 {
                break;
            }
            frame
                .grid
                .put_text_clipped(content_left, y, &line, Style::Muted, right);
            y += 1;
        }
    }

    if let Some(row) = menu.rows.get(menu.selected) {
        y = list_bottom;
        frame.grid.put_text(
            content_left,
            y,
            &clip(&horizontal.to_string().repeat(usize::from(content_width))),
            Style::Border,
        );
        y += 1;
        let (_, state_style) = tool_state_glyph(row);
        let status = ellipsized(
            &row.lifecycle,
            usize::from(content_width) * 2 / 3,
            ambiguous,
        );
        let status_x =
            right.saturating_sub(u16::try_from(text_width(&status, ambiguous)).unwrap_or(u16::MAX));
        let name = ellipsized(
            &row.name,
            usize::from(status_x.saturating_sub(content_left + 1)),
            ambiguous,
        );
        frame
            .grid
            .put_text_clipped(content_left, y, &name, Style::Accent, status_x);
        if !row.languages.is_empty() {
            let x = content_left
                .saturating_add(
                    u16::try_from(text_width(&row.name, config.editor.ambiguous_width))
                        .unwrap_or(0),
                )
                .saturating_add(2);
            frame.grid.put_text_clipped(
                x,
                y,
                &row.languages,
                Style::Muted,
                status_x.saturating_sub(1),
            );
        }
        frame
            .grid
            .put_text_clipped(status_x, y, &status, state_style, right);
        y += if short { 1 } else { 2 };
        let mut details = Vec::new();
        if row.installed {
            let location = if row.executable.is_empty() {
                row.runtime.clone()
            } else {
                format!("{}  {}", row.runtime, row.executable)
            };
            details.push(format!("Source    {location}"));
        } else if row.runtime != "—" {
            details.push(format!("Source    {} (not installed)", row.runtime));
        }
        if row.version != "—" {
            let release = match row.release.as_str() {
                "—" | "catalog build" | "reported by executable" | "" => String::new(),
                release => format!("  ({release})"),
            };
            details.push(format!("Version   {}{release}", row.version));
        }
        if !row.sessions.is_empty() {
            details.push(format!("Sessions  {}", row.sessions.join(" · ")));
        }
        let bottom = top + height - 1;
        if short {
            details.clear();
        }
        for line in details {
            if y >= bottom {
                break;
            }
            let line = if looks_like_path(line.rsplit(' ').next().unwrap_or_default()) {
                clip_path_tail(&line, usize::from(content_width), ambiguous)
            } else {
                clip(&line)
            };
            frame.grid.put_text(content_left, y, &line, Style::Muted);
            y += 1;
        }
        y += u16::from(!short);
        if y < bottom {
            let note_style = if state_style == Style::Error {
                Style::Error
            } else {
                Style::Default
            };
            // Install failures explain themselves here, so the note wraps above the keys.
            let keys_row = bottom.saturating_sub(1);
            for line in
                crate::ui::render::wrap_plain_text(&row.note, usize::from(content_width), ambiguous)
            {
                if y >= keys_row {
                    break;
                }
                frame
                    .grid
                    .put_text(content_left, y, &clip(&line), note_style);
                y += 1;
            }
        }
        let keys = tool_row_keys(row).join(" · ");
        if y < bottom && !keys.is_empty() {
            frame
                .grid
                .put_text(content_left, y, &clip(&keys), Style::Accent);
        }
    }
    frame.cursor = None;
}

/// Whether the state is a transient install phase (downloading, verifying, installing 40%…).
fn tool_busy(state: &str) -> bool {
    !matches!(
        state,
        "installed"
            | "not installed"
            | "failed"
            | "blocked: untrusted"
            | "blocked: safe mode"
            | "starting"
            | "restarting"
    ) && !state.starts_with("running")
}

/// The glyph and colour for a tool's state: filled when installed, half while busy, a cross
/// when broken, hollow when not installed.
fn tool_state_glyph(row: &crate::ui::ToolMenuRow) -> (&'static str, Style) {
    match row.lifecycle.as_str() {
        "failed" => ("✗", Style::Error),
        "blocked: untrusted" | "blocked: safe mode" => ("!", Style::Warning),
        "not installed" => ("○", Style::Muted),
        "starting" | "restarting" => ("◐", Style::Warning),
        state if tool_busy(state) => ("◐", Style::Warning),
        _ => ("●", Style::GitSignAdded),
    }
}

/// The keys that do something for this row right now.
fn tool_row_keys(row: &crate::ui::ToolMenuRow) -> Vec<&'static str> {
    let state = row.lifecycle.as_str();
    let mut keys = Vec::new();
    if state == "blocked: untrusted" {
        keys.push("t trust workspace");
        if row.lsp {
            keys.push("p versions");
        }
        return keys;
    }
    if tool_busy(state) || state == "blocked: safe mode" {
        return keys;
    }
    if row.installed {
        if row.lsp {
            let active = row.session_state.starts_with("running")
                || matches!(row.session_state.as_str(), "starting" | "restarting");
            if state == "installed" || state == "failed" && !active {
                keys.push(if row.can_start {
                    "Enter start"
                } else {
                    "open a matching file to start"
                });
            } else {
                keys.push("x stop");
                keys.push("R restart");
            }
        }
        if row.update.is_some() {
            keys.push("i update");
        }
        if row.can_uninstall {
            keys.push("u uninstall");
        }
    } else if row.can_install {
        keys.push(if state == "failed" {
            "i retry install"
        } else {
            "i install"
        });
    }
    if row.lsp {
        keys.push("p versions");
    }
    keys
}
