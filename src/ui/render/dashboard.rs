//! The start screen: a one-line header, the workspace with its Git state, the configured
//! actions in key-first columns, and this workspace's recent files beside other recent projects.

use crate::config::AmbiguousWidth;
use crate::config::Config;
use crate::config::DashboardConfig;
use crate::config::ExplorerIconMode;
use crate::ui::CellGrid;
use crate::ui::Style;
use crate::ui::geometry::text_width;
use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::grid::Frame;
use crate::ui::grid::ZERO_WIDTH_REPLACEMENT;
use crate::ui::render::DashboardAction;
use crate::ui::render::DashboardWorkspace;
use crate::ui::render::UiState;
use crate::ui::render::editor::cursor_style;
use crate::ui::render::overlays::truncate_content_title;
use std::path::Path;
use std::path::PathBuf;

/// Blank columns between action columns and between the two lists.
const COLUMN_GAP: u16 = 4;
/// Columns between an action's key and its label.
const KEY_GAP: u16 = 2;
/// The fewest rows a list is shortened to before it is left out: its title and two entries.
const LIST_MIN_ROWS: u16 = 3;
/// The narrowest a list may be when the two share a row.
const LIST_MIN_WIDTH: u16 = 20;
const RECENT_FILES_TITLE: &str = "Recent files";
const PROJECTS_TITLE: &str = "Projects";

pub(super) fn render_dashboard(
    root: &Path,
    ui: &UiState,
    configured: &DashboardConfig,
    editor_config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let ambiguous = editor_config.editor.ambiguous_width;
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        ambiguous == AmbiguousWidth::Wide,
    );
    let measure = |text: &str| u16::try_from(text_width(text, ambiguous)).unwrap_or(u16::MAX);
    let block_width = configured.content_width.min(width.saturating_sub(4));
    let actions = ActionColumns::new(&ui.dashboard_actions, block_width, &measure);
    let lists = list_columns(&ui.dashboard_workspace, block_width, &actions);
    let logo: Vec<&str> = configured.logo.lines().collect();
    let logo_fits = logo.iter().all(|line| measure(line) <= block_width);
    let wanted = Rows {
        logo: if logo_fits {
            u16::try_from(logo.len()).unwrap_or(u16::MAX)
        } else {
            0
        },
        header: if configured.show_identity || configured.show_version {
            2
        } else {
            0
        },
        workspace: 1 + u16::from(configured.show_path),
        actions: actions.rows,
        lists: lists
            .iter()
            .map(|(_, _, list)| list.len().saturating_add(1))
            .max()
            .unwrap_or(0),
        gap: 1,
    };
    let Some(rows) = fit_rows(wanted, height)
        .filter(|_| width >= configured.minimum_width && height >= configured.minimum_height)
        .filter(|_| block_width > 0)
    else {
        grid.put_text(
            1.min(width.saturating_sub(1)),
            1.min(height.saturating_sub(1)),
            &configured.resize_message,
            Style::Accent,
        );
        return Frame { grid, cursor: None };
    };

    let left = width.saturating_sub(block_width) / 2;
    let right = left.saturating_add(block_width);
    let mut y = height.saturating_sub(rows.total()) / 2;
    let advance = |y: &mut u16, used: u16| {
        if used > 0 {
            *y = y.saturating_add(used).saturating_add(rows.gap);
        }
    };
    if rows.logo > 0 {
        for (row, line) in logo.iter().enumerate() {
            let row = u16::try_from(row).unwrap_or(u16::MAX);
            grid.put_text_clipped(left, y.saturating_add(row), line, Style::Accent, right);
        }
    }
    advance(&mut y, rows.logo);
    if rows.header > 0 {
        draw_header(&mut grid, configured, left, right, y);
    }
    advance(&mut y, rows.header);
    if rows.workspace > 0 {
        let icons = editor_config.ui.explorer.icon_mode == ExplorerIconMode::NerdFont;
        draw_workspace(&mut grid, root, ui, icons, left, right, y);
        if rows.workspace > 1 {
            let path = home_relative(root, usize::from(block_width), ambiguous);
            grid.put_text_clipped(left, y.saturating_add(1), &path, Style::Muted, right);
        }
    }
    advance(&mut y, rows.workspace);
    let selected = ui
        .dashboard_selected
        .min(ui.dashboard_actions.len().saturating_sub(1));
    let mut cursor = None;
    for (index, action) in ui.dashboard_actions.iter().enumerate() {
        let (x, column_right, row) = actions.place(index, left, right);
        let label_x = draw_action(
            &mut grid,
            action,
            &actions,
            x,
            y.saturating_add(row),
            column_right,
        );
        // The cursor rests on the label of the item Enter runs.
        if index == selected && label_x < column_right {
            let style = cursor_style(editor_config.ui.cursor.normal);
            cursor = Some((label_x, y.saturating_add(row), style));
        }
    }
    advance(&mut y, rows.actions);
    if rows.lists > 0 {
        for (offset, list_width, list) in &lists {
            let x = left.saturating_add(*offset);
            let list_right = x.saturating_add(*list_width);
            list.draw(&mut grid, x, list_right, y, rows.lists, ambiguous);
        }
    }
    Frame { grid, cursor }
}

/// How many rows each part of the dashboard takes, and the blank rows between parts.
#[derive(Clone, Copy)]
struct Rows {
    logo: u16,
    header: u16,
    workspace: u16,
    actions: u16,
    lists: u16,
    gap: u16,
}

impl Rows {
    fn total(self) -> u16 {
        let parts = [
            self.logo,
            self.header,
            self.workspace,
            self.actions,
            self.lists,
        ];
        let shown = parts.iter().filter(|rows| **rows > 0).count();
        let gaps = u16::try_from(shown.saturating_sub(1)).unwrap_or(u16::MAX);
        parts
            .iter()
            .fold(self.gap.saturating_mul(gaps), |sum, rows| {
                sum.saturating_add(*rows)
            })
    }
}

/// The roomiest arrangement that fits `height`. Parts give way in order as the window gets
/// shorter: the logo, the blank rows and the length of the lists, the lists, the header, the
/// path and the workspace name. The actions always stay; `None` when even they do not fit.
fn fit_rows(wanted: Rows, height: u16) -> Option<Rows> {
    if wanted.total() <= height {
        return Some(wanted);
    }
    for gap in [1, 0] {
        let without_lists = Rows {
            logo: 0,
            lists: 0,
            gap,
            ..wanted
        };
        let Some(room) = height.checked_sub(without_lists.total()) else {
            continue;
        };
        let lists = room.saturating_sub(gap).min(wanted.lists);
        if lists >= LIST_MIN_ROWS.min(wanted.lists) {
            return Some(Rows {
                lists,
                ..without_lists
            });
        }
    }
    let base = Rows {
        logo: 0,
        lists: 0,
        ..wanted
    };
    [
        base,
        Rows { header: 0, ..base },
        Rows {
            header: 0,
            workspace: base.workspace.min(1),
            ..base
        },
        Rows {
            header: 0,
            workspace: 0,
            ..base
        },
    ]
    .into_iter()
    .flat_map(|rows| [1, 0].map(|gap| Rows { gap, ..rows }))
    .find(|rows| rows.total() <= height)
}

/// The identity and version on one row, with a rule under them as wide as the dashboard.
fn draw_header(grid: &mut CellGrid, configured: &DashboardConfig, left: u16, right: u16, y: u16) {
    let mut x = left;
    if configured.show_identity {
        x = grid
            .put_text_clipped(x, y, &configured.identity, Style::Accent, right)
            .saturating_add(2);
    }
    if configured.show_version {
        let version = configured
            .version_format
            .replace("{version}", env!("CARGO_PKG_VERSION"));
        grid.put_text_clipped(x, y, &version, Style::Muted, right);
    }
    for x in left..right {
        grid.set_cell(x, y.saturating_add(1), "─", Style::Border);
    }
}

/// The workspace's name, then its branch and how many files differ from it.
fn draw_workspace(
    grid: &mut CellGrid,
    root: &Path,
    ui: &UiState,
    icons: bool,
    left: u16,
    right: u16,
    y: u16,
) {
    let mut x = grid.put_text_clipped(left, y, &directory_name(root), Style::Default, right);
    let mut separator = "   ";
    if let Some(branch) = &ui.git_branch {
        let icon = if icons { "\u{e0a0} " } else { "" };
        x = grid.put_text_clipped(x, y, separator, Style::Muted, right);
        x = grid.put_text_clipped(x, y, &format!("{icon}{branch}"), Style::Accent, right);
        separator = " · ";
    }
    let Some((changed, untracked)) = ui.dashboard_workspace.git_changes else {
        return;
    };
    let mut counts = Vec::new();
    if changed > 0 {
        counts.push((format!("{changed} changed"), Style::GitSignChanged));
    }
    if untracked > 0 {
        counts.push((format!("{untracked} untracked"), Style::GitSignAdded));
    }
    if counts.is_empty() {
        counts.push(("clean".to_owned(), Style::Muted));
    }
    for (text, style) in counts {
        x = grid.put_text_clipped(x, y, separator, Style::Muted, right);
        x = grid.put_text_clipped(x, y, &text, style, right);
        separator = " · ";
    }
}

/// Draws one action as its key, icon and label, and returns where the label starts.
fn draw_action(
    grid: &mut CellGrid,
    action: &DashboardAction,
    columns: &ActionColumns,
    x: u16,
    y: u16,
    right: u16,
) -> u16 {
    if let Some(key) = &action.key {
        grid.put_text_clipped(x, y, key, Style::Accent, right);
    }
    let icon_x = if columns.key_width > 0 {
        x.saturating_add(columns.key_width).saturating_add(KEY_GAP)
    } else {
        x
    };
    if !action.icon.is_empty() {
        grid.put_text_clipped(icon_x, y, &action.icon, Style::Muted, right);
    }
    let label_x = icon_x.saturating_add(columns.icon_width);
    grid.put_text_clipped(label_x, y, &action.label, Style::Default, right);
    label_x
}

/// The actions laid out in as many columns as fit, filled top to bottom so `j` and `k` move
/// down and up a column.
struct ActionColumns {
    columns: u16,
    rows: u16,
    /// The distance from one column's start to the next.
    pitch: u16,
    key_width: u16,
    icon_width: u16,
}

impl ActionColumns {
    fn new(actions: &[DashboardAction], block_width: u16, measure: &impl Fn(&str) -> u16) -> Self {
        let key_width = actions
            .iter()
            .filter_map(|action| action.key.as_deref())
            .map(measure)
            .max()
            .unwrap_or(0);
        let icon_width = actions
            .iter()
            .filter(|action| !action.icon.is_empty())
            .map(|action| measure(&action.icon).saturating_add(1))
            .max()
            .unwrap_or(0);
        let label_width = actions
            .iter()
            .map(|action| measure(&action.label))
            .max()
            .unwrap_or(0);
        let item_width = key_width
            .saturating_add(if key_width > 0 { KEY_GAP } else { 0 })
            .saturating_add(icon_width)
            .saturating_add(label_width);
        let count = u16::try_from(actions.len()).unwrap_or(u16::MAX);
        let fit = (block_width.saturating_add(COLUMN_GAP)
            / item_width.saturating_add(COLUMN_GAP).max(1))
        .clamp(1, count.max(1));
        let rows = count.div_ceil(fit);
        let columns = count.div_ceil(rows.max(1)).max(1);
        Self {
            columns,
            rows,
            pitch: block_width.saturating_add(COLUMN_GAP) / columns,
            key_width,
            icon_width,
        }
    }

    /// The `index`th action's column start, the column's right edge, and its row.
    fn place(&self, index: usize, left: u16, right: u16) -> (u16, u16, u16) {
        let index = u16::try_from(index).unwrap_or(u16::MAX);
        let column = index / self.rows.max(1);
        let x = left.saturating_add(column.saturating_mul(self.pitch));
        let column_right = if column.saturating_add(1) >= self.columns {
            right
        } else {
            x.saturating_add(self.pitch.saturating_sub(COLUMN_GAP))
        };
        (x, column_right, index % self.rows.max(1))
    }
}

enum DashboardList<'a> {
    Files(&'a [String]),
    Projects(&'a [PathBuf]),
}

impl DashboardList<'_> {
    fn len(&self) -> u16 {
        let len = match self {
            Self::Files(files) => files.len(),
            Self::Projects(projects) => projects.len(),
        };
        u16::try_from(len).unwrap_or(u16::MAX)
    }

    /// The title and as many entries as fit in `rows` rows.
    fn draw(
        &self,
        grid: &mut CellGrid,
        x: u16,
        right: u16,
        y: u16,
        rows: u16,
        ambiguous: AmbiguousWidth,
    ) {
        let width = usize::from(right.saturating_sub(x));
        let title = match self {
            Self::Files(_) => RECENT_FILES_TITLE,
            Self::Projects(_) => PROJECTS_TITLE,
        };
        grid.put_text_clipped(x, y, title, Style::Muted, right);
        let entries = usize::from(rows.saturating_sub(1));
        let rows = (1..).map(|row| y.saturating_add(row));
        match self {
            Self::Files(files) => {
                for (file, y) in files.iter().take(entries).zip(rows) {
                    // The folder is dimmed so the file name stands out; a long path keeps its end.
                    let shown = truncate_content_title(file, width, ambiguous);
                    let name_start = shown.rfind('/').map_or(0, |slash| slash + 1);
                    let (folder, name) = shown.split_at(name_start);
                    let name_x = grid.put_text_clipped(x, y, folder, Style::Muted, right);
                    grid.put_text_clipped(name_x, y, name, Style::Default, right);
                }
            }
            Self::Projects(projects) => {
                for (project, y) in projects.iter().take(entries).zip(rows) {
                    let path_x = grid
                        .put_text_clipped(x, y, &directory_name(project), Style::Default, right)
                        .saturating_add(2);
                    let room = usize::from(right.saturating_sub(path_x));
                    let path = home_relative(project, room, ambiguous);
                    grid.put_text_clipped(path_x, y, &path, Style::Muted, right);
                }
            }
        }
    }
}

/// Recent files and projects side by side when both fit, with the projects lined up under the
/// last action column; otherwise only the recent files.
fn list_columns<'a>(
    workspace: &'a DashboardWorkspace,
    block_width: u16,
    actions: &ActionColumns,
) -> Vec<(u16, u16, DashboardList<'a>)> {
    let split = if actions.columns >= 2 {
        actions.pitch.saturating_mul(actions.columns - 1)
    } else {
        block_width.saturating_mul(3) / 5
    };
    let files =
        (!workspace.recent_files.is_empty()).then(|| DashboardList::Files(&workspace.recent_files));
    let projects = (!workspace.recent_projects.is_empty())
        .then(|| DashboardList::Projects(&workspace.recent_projects));
    let side_by_side = split >= LIST_MIN_WIDTH.saturating_add(COLUMN_GAP)
        && block_width.saturating_sub(split) >= LIST_MIN_WIDTH;
    match (files, projects) {
        (Some(files), Some(projects)) if side_by_side => vec![
            (0, split - COLUMN_GAP, files),
            (split, block_width - split, projects),
        ],
        (Some(list), _) | (None, Some(list)) => vec![(0, block_width, list)],
        (None, None) => Vec::new(),
    }
}

fn directory_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A path with home shown as `~`, keeping its end when it is wider than `width`.
fn home_relative(path: &Path, width: usize, ambiguous_width: AmbiguousWidth) -> String {
    let mut text = path.display().to_string();
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty())
        && let Ok(rest) = path.strip_prefix(&home)
    {
        text = format!("~/{}", rest.display());
    }
    truncate_content_title(&text, width, ambiguous_width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::geometry::cell_text_width;

    #[test]
    fn home_relative_paths_keep_their_suffix_within_display_columns() {
        for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
            let path = format!("/tmp/{}e\u{301}.rs", "界".repeat(20));
            let shown = home_relative(Path::new(&path), 36, ambiguous);
            assert!(shown.starts_with('…'));
            assert!(shown.ends_with("e\u{301}.rs"));
            assert!(cell_text_width(&shown, ambiguous) <= 36);
            for width in 0..4 {
                let shown = home_relative(Path::new(&path), width, ambiguous);
                assert!(cell_text_width(&shown, ambiguous) <= width);
            }
        }
    }
}
