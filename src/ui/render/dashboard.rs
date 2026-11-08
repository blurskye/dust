//! The start screen: the DUST wordmark beside the workspace's name, branch, path and last
//! commit, then four boxes for recent files, changed files, projects and the configured
//! actions. Smaller windows drop the wordmark first, then show only recent files and actions.

use crate::config::AmbiguousWidth;
use crate::config::BorderStyle;
use crate::config::Config;
use crate::config::DashboardConfig;
use crate::config::ExplorerIconMode;
use crate::ui::CellGrid;
use crate::ui::SelectedText;
use crate::ui::Style;
use crate::ui::geometry::ellipsized;
use crate::ui::geometry::text_width;
use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::grid::Frame;
use crate::ui::grid::ZERO_WIDTH_REPLACEMENT;
use crate::ui::render::DashboardPanel;
use crate::ui::render::UiState;
use crate::ui::render::overlays::truncate_content_title;
use std::path::Path;

/// The smallest window that fits the four boxes.
const GRID_MIN_WIDTH: u16 = 90;
const GRID_MIN_HEIGHT: u16 = 19;
/// The shortest window that also fits the wordmark above the boxes.
const WORDMARK_MIN_HEIGHT: u16 = 27;
/// The shortest single-column window that fits recent files above the actions.
const COLUMN_RECENT_MIN_HEIGHT: u16 = 16;
/// The shortest window that keeps the key hints on its last row.
const HINTS_MIN_HEIGHT: u16 = 12;
/// Columns kept clear around the boxes and between them.
const MARGIN: u16 = 2;
/// Columns between an action's key and its label.
const KEY_GAP: u16 = 2;
const HINTS: &str =
    "ctrl-h/j/k/l boxes · j/k move · enter open · 1-9 recent file · space space find · F1 commands";
const ALL_PANELS: [DashboardPanel; 4] = [
    DashboardPanel::RecentFiles,
    DashboardPanel::Changes,
    DashboardPanel::Projects,
    DashboardPanel::Start,
];
/// D, U, S and T on a five-by-eight grid. Each pair of rows becomes one row of half blocks.
const WORDMARK_LETTERS: [[&str; 8]; 4] = [
    [
        "11110", "10001", "10001", "10001", "10001", "10001", "10001", "11110",
    ],
    [
        "10001", "10001", "10001", "10001", "10001", "10001", "10001", "01110",
    ],
    [
        "01111", "10000", "10000", "01110", "00001", "00001", "00001", "11110",
    ],
    [
        "11111", "00100", "00100", "00100", "00100", "00100", "00100", "00100",
    ],
];

/// The boxes a dashboard this size shows, in reading order.
pub(crate) fn dashboard_panels(width: u16, height: u16) -> &'static [DashboardPanel] {
    if width >= GRID_MIN_WIDTH && height >= GRID_MIN_HEIGHT {
        &ALL_PANELS
    } else if height >= COLUMN_RECENT_MIN_HEIGHT {
        &[DashboardPanel::RecentFiles, DashboardPanel::Start]
    } else {
        &[DashboardPanel::Start]
    }
}

/// Where a box sits as (column, row): a 2x2 grid when all four show, else one column.
pub(crate) fn dashboard_cell(
    shown: &[DashboardPanel],
    panel: DashboardPanel,
) -> Option<(usize, usize)> {
    let index = shown.iter().position(|shown| *shown == panel)?;
    Some(if shown.len() == ALL_PANELS.len() {
        (index % 2, index / 2)
    } else {
        (0, index)
    })
}

pub(crate) fn dashboard_rows(ui: &UiState, panel: DashboardPanel) -> usize {
    let workspace = &ui.dashboard_workspace;
    match panel {
        DashboardPanel::RecentFiles => workspace.recent_files.len(),
        DashboardPanel::Changes => workspace
            .changes
            .as_ref()
            .map_or(0, |changes| changes.files.len()),
        DashboardPanel::Projects => workspace.projects.len(),
        DashboardPanel::Start => ui.dashboard_actions.len(),
    }
}

/// The box the selection is in: the one Tab moved to while it is shown and has rows,
/// otherwise recent files when there are some, otherwise the actions.
pub(crate) fn dashboard_focus(ui: &UiState, shown: &[DashboardPanel]) -> DashboardPanel {
    let usable = |panel: &DashboardPanel| shown.contains(panel) && dashboard_rows(ui, *panel) > 0;
    ui.dashboard_focus
        .filter(usable)
        .or_else(|| Some(DashboardPanel::RecentFiles).filter(usable))
        .unwrap_or(DashboardPanel::Start)
}

/// The selected row of `panel`, kept inside its rows.
pub(crate) fn dashboard_selected_row(ui: &UiState, panel: DashboardPanel) -> usize {
    let row = if panel == DashboardPanel::Start {
        ui.dashboard_selected
    } else {
        ui.dashboard_row
    };
    row.min(dashboard_rows(ui, panel).saturating_sub(1))
}

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
    if width < configured.minimum_width || height < configured.minimum_height {
        grid.put_text(
            1.min(width.saturating_sub(1)),
            1.min(height.saturating_sub(1)),
            &configured.resize_message,
            Style::Accent,
        );
        return Frame { grid, cursor: None };
    }
    let shown = dashboard_panels(width, height);
    let mut screen = Screen {
        grid,
        ui,
        root,
        configured,
        ambiguous,
        icons: editor_config.ui.explorer.icon_mode == ExplorerIconMode::NerdFont,
        focus: dashboard_focus(ui, shown),
    };
    let block = configured.content_width.max(GRID_MIN_WIDTH).min(width);
    let left = (width - block) / 2;
    let hints = height >= HINTS_MIN_HEIGHT;
    // The boxes end above a blank row and the hints.
    let bottom = if hints { height - 2 } else { height };
    let boxes = if shown.len() == ALL_PANELS.len() {
        let top = screen.draw_header(left, block, height);
        grid_boxes(left + MARGIN, block - 2 * MARGIN, top, bottom)
    } else {
        let margin = if block >= 40 { MARGIN } else { 0 };
        screen.draw_header_line(left + margin, left + block - margin);
        let start_height = screen.start_rows(block - 2 * margin).saturating_add(3);
        column_boxes(
            shown,
            left + margin,
            block - 2 * margin,
            start_height,
            2,
            bottom,
        )
    };
    for (panel, rect) in boxes {
        screen.draw_panel(panel, rect);
    }
    if hints {
        let y = height - 1;
        for x in 0..width {
            screen.grid.set_cell(x, y, " ", Style::StatusMuted);
        }
        screen
            .grid
            .put_text_clipped(1, y, HINTS, Style::StatusMuted, width.saturating_sub(1));
    }
    Frame {
        grid: screen.grid,
        cursor: None,
    }
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

/// Two boxes over two, splitting the room evenly with a gap between them.
fn grid_boxes(x: u16, width: u16, top: u16, bottom: u16) -> Vec<(DashboardPanel, Rect)> {
    let left_width = width.saturating_sub(MARGIN) / 2;
    let right_x = x + left_width + MARGIN;
    let right_width = width.saturating_sub(left_width + MARGIN);
    let rows = bottom.saturating_sub(top);
    let upper = rows / 2;
    let lower = rows - upper;
    let rect = |x, y, width, height| Rect {
        x,
        y,
        width,
        height,
    };
    vec![
        (DashboardPanel::RecentFiles, rect(x, top, left_width, upper)),
        (
            DashboardPanel::Changes,
            rect(right_x, top, right_width, upper),
        ),
        (
            DashboardPanel::Projects,
            rect(x, top + upper, left_width, lower),
        ),
        (
            DashboardPanel::Start,
            rect(right_x, top + upper, right_width, lower),
        ),
    ]
}

/// Recent files above the actions, which keep the rows they need when there is room.
fn column_boxes(
    shown: &[DashboardPanel],
    x: u16,
    width: u16,
    start_height: u16,
    top: u16,
    bottom: u16,
) -> Vec<(DashboardPanel, Rect)> {
    let rows = bottom.saturating_sub(top);
    let rect = |y, height| Rect {
        x,
        y,
        width,
        height,
    };
    if shown.contains(&DashboardPanel::RecentFiles) {
        // Recent files keep at least a border, a blank row and two files.
        let start = start_height.min(rows.saturating_sub(5));
        vec![
            (DashboardPanel::RecentFiles, rect(top, rows - start)),
            (DashboardPanel::Start, rect(top + rows - start, start)),
        ]
    } else {
        vec![(DashboardPanel::Start, rect(top, start_height.min(rows)))]
    }
}

/// Where a box's rows go: the cells a selected row fills and the columns text stays within.
struct Content {
    fill_left: u16,
    fill_right: u16,
    x: u16,
    right: u16,
    y: u16,
    rows: u16,
}

struct Screen<'a> {
    grid: CellGrid,
    ui: &'a UiState,
    root: &'a Path,
    configured: &'a DashboardConfig,
    ambiguous: AmbiguousWidth,
    icons: bool,
    focus: DashboardPanel,
}

impl Screen<'_> {
    fn width(&self, text: &str) -> u16 {
        u16::try_from(text_width(text, self.ambiguous)).unwrap_or(u16::MAX)
    }

    fn put(&mut self, x: u16, y: u16, text: &str, style: Style, right: u16) -> u16 {
        self.grid.put_text_clipped(x, y, text, style, right)
    }

    /// The wordmark (or the configured logo) with the workspace beside it when the window is
    /// tall and wide enough, otherwise the same facts as text. Returns the row the boxes
    /// start on.
    fn draw_header(&mut self, left: u16, block: u16, height: u16) -> u16 {
        let art = if self.configured.logo.trim().is_empty() {
            wordmark()
        } else {
            self.configured.logo.lines().map(str::to_owned).collect()
        };
        let art_rows = u16::try_from(art.len()).unwrap_or(u16::MAX);
        let art_width = art.iter().map(|line| self.width(line)).max().unwrap_or(0);
        let x = left + 2 * MARGIN;
        let right = left + block - 2 * MARGIN;
        let info_x = x.saturating_add(art_width).saturating_add(8);
        let with_art =
            height >= WORDMARK_MIN_HEIGHT && art_rows <= 6 && info_x.saturating_add(30) <= right;
        let version = self
            .configured
            .version_format
            .replace("{version}", env!("CARGO_PKG_VERSION"));
        if with_art {
            for (row, line) in (1..).zip(&art) {
                self.put(x, row, line, Style::Accent, right);
            }
            let mut name_right = right;
            if self.configured.show_version {
                name_right = right.saturating_sub(self.width(&version));
                self.put(name_right, 2, &version, Style::Muted, right);
            }
            self.draw_workspace_line(info_x, 2, name_right.saturating_sub(2));
            self.draw_path(info_x, 3, right);
            self.draw_last_commit(info_x, 5, right);
            return art_rows.max(5) + 2;
        }
        let mut name_right = right;
        let mut identity = String::new();
        if self.configured.show_identity {
            identity.push_str(&self.configured.identity);
        }
        if self.configured.show_version {
            if !identity.is_empty() {
                identity.push(' ');
            }
            identity.push_str(&version);
        }
        if !identity.is_empty() {
            name_right = right.saturating_sub(self.width(&identity));
            self.put(name_right, 1, &identity, Style::Muted, right);
        }
        self.draw_workspace_line(x, 1, name_right.saturating_sub(2));
        self.draw_path(x, 2, right);
        self.draw_last_commit(x, 3, right);
        5
    }

    /// The workspace's name, its branch and how many files differ from it.
    fn draw_workspace_line(&mut self, x: u16, y: u16, right: u16) {
        let mut x = self.put(x, y, &directory_name(self.root), Style::Default, right);
        if let Some(branch) = &self.ui.git_branch {
            let icon = if self.icons { "\u{e0a0} " } else { "" };
            x = self.put(x, y, " · ", Style::Muted, right);
            x = self.put(x, y, &format!("{icon}{branch}"), Style::Accent, right);
        }
        let ui = self.ui;
        let Some(changes) = &ui.dashboard_workspace.changes else {
            return;
        };
        // The same files the changes box lists: tracked changes, then new files.
        let mut parts = Vec::new();
        if changes.changed > 0 {
            parts.push((
                format!("{} changed", changes.changed),
                Style::GitSignChanged,
            ));
        }
        if changes.untracked > 0 {
            parts.push((format!("{} new", changes.untracked), Style::GitSignAdded));
        }
        if parts.is_empty() {
            parts.push(("clean".to_owned(), Style::Muted));
        }
        for (summary, style) in parts {
            x = self.put(x, y, " · ", Style::Muted, right);
            x = self.put(x, y, &summary, style, right);
        }
    }

    fn draw_path(&mut self, x: u16, y: u16, right: u16) {
        if self.configured.show_path {
            let path = home_relative(
                self.root,
                usize::from(right.saturating_sub(x)),
                self.ambiguous,
            );
            self.put(x, y, &path, Style::Muted, right);
        }
    }

    fn draw_last_commit(&mut self, x: u16, y: u16, right: u16) {
        let Some((subject, age)) = &self.ui.dashboard_workspace.last_commit else {
            return;
        };
        let x = self.put(x, y, "last commit ", Style::Muted, right);
        let age = format!(" {age}");
        let room = right.saturating_sub(x).saturating_sub(self.width(&age));
        let subject = ellipsized(subject, usize::from(room), self.ambiguous);
        let x = self.put(x, y, &subject, Style::Default, right);
        self.put(x, y, &age, Style::Muted, right);
    }

    /// One row for a window too small for the boxes side by side: `dust v0.2.1 · tally · main`.
    fn draw_header_line(&mut self, x: u16, right: u16) {
        let start = x;
        let mut x = x;
        let mut separator = "";
        if self.configured.show_identity {
            x = self.put(x, 0, &self.configured.identity, Style::Accent, right);
            separator = " ";
        }
        if self.configured.show_version {
            let version = self
                .configured
                .version_format
                .replace("{version}", env!("CARGO_PKG_VERSION"));
            x = self.put(x, 0, separator, Style::Muted, right);
            x = self.put(x, 0, &version, Style::Muted, right);
        }
        if x > start {
            x = self.put(x, 0, " · ", Style::Muted, right);
        }
        x = self.put(x, 0, &directory_name(self.root), Style::Default, right);
        if let Some(branch) = &self.ui.git_branch {
            x = self.put(x, 0, " · ", Style::Muted, right);
            self.put(x, 0, branch, Style::Accent, right);
        }
    }

    /// A rounded box with its title in the top border, and where its rows go.
    fn draw_box(&mut self, rect: Rect, title: &str, hint: &str, focused: bool) -> Content {
        let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
            self.grid.border_glyphs(BorderStyle::Rounded);
        let border = if focused {
            Style::Accent
        } else {
            Style::Border
        };
        let right = rect.x + rect.width - 1;
        let bottom = rect.y + rect.height - 1;
        for x in rect.x..=right {
            let (top, low) = if x == rect.x {
                (top_left, bottom_left)
            } else if x == right {
                (top_right, bottom_right)
            } else {
                (horizontal, horizontal)
            };
            self.grid.set_cell(x, rect.y, &top.to_string(), border);
            self.grid.set_cell(x, bottom, &low.to_string(), border);
        }
        for y in rect.y + 1..bottom {
            self.grid.set_cell(rect.x, y, &vertical.to_string(), border);
            self.grid.set_cell(right, y, &vertical.to_string(), border);
        }
        let title_style = if focused { Style::Accent } else { Style::Muted };
        let title_end = self.put(
            rect.x + 2,
            rect.y,
            &format!(" {title} "),
            title_style,
            right,
        );
        if !hint.is_empty() {
            let hint = format!(" {hint} ");
            let hint_x = right.saturating_sub(self.width(&hint) + 1);
            if hint_x > title_end {
                self.put(hint_x, rect.y, &hint, Style::Muted, right);
            }
        }
        // A blank row under the title when the box has room for it.
        let gap = u16::from(rect.height >= 6);
        Content {
            fill_left: rect.x + 1,
            fill_right: right,
            x: rect.x + 2,
            right: right - 1,
            y: rect.y + 1 + gap,
            rows: rect.height.saturating_sub(2 + gap),
        }
    }

    fn draw_panel(&mut self, panel: DashboardPanel, rect: Rect) {
        if rect.width < 8 || rect.height < 3 {
            return;
        }
        let focused = panel == self.focus;
        let selected = focused.then(|| dashboard_selected_row(self.ui, panel));
        let ui = self.ui;
        let workspace = &ui.dashboard_workspace;
        match panel {
            DashboardPanel::RecentFiles => {
                let hint = if workspace.recent_files.is_empty() {
                    ""
                } else {
                    "1-9"
                };
                let content = self.draw_box(rect, "Recent files", hint, focused);
                if workspace.recent_files.is_empty() {
                    self.put_empty(&content, "Files you open show up here");
                }
                for (row, index) in visible_rows(&content, workspace.recent_files.len(), selected) {
                    self.draw_recent_file(&content, row, index, selected == Some(index));
                }
            }
            DashboardPanel::Changes => {
                let branch = ui.git_branch.as_deref().unwrap_or_default();
                let content = self.draw_box(rect, "Changes", branch, focused);
                match &workspace.changes {
                    None => self.put_empty(&content, "Not a Git repository"),
                    Some(changes) if changes.files.is_empty() => self.put_empty(&content, "clean"),
                    Some(changes) => {
                        for (row, index) in visible_rows(&content, changes.files.len(), selected) {
                            self.draw_change(&content, row, index, selected == Some(index));
                        }
                    }
                }
            }
            DashboardPanel::Projects => {
                let content = self.draw_box(rect, "Projects", "", focused);
                let names = workspace
                    .projects
                    .iter()
                    .map(|project| self.width(&project.name))
                    .max()
                    .unwrap_or(0);
                let path_offset = names
                    .saturating_add(3)
                    .min(content.right.saturating_sub(content.x) / 3);
                for (row, index) in visible_rows(&content, workspace.projects.len(), selected) {
                    self.draw_project(&content, row, index, path_offset, selected == Some(index));
                }
            }
            DashboardPanel::Start => {
                let content = self.draw_box(rect, "Start", "", focused);
                self.draw_actions(&content, selected);
            }
        }
    }

    fn put_empty(&mut self, content: &Content, text: &str) {
        if content.rows > 0 {
            self.put(content.x, content.y, text, Style::Muted, content.right);
        }
    }

    fn fill(&mut self, content: &Content, y: u16) {
        for x in content.fill_left..content.fill_right {
            self.grid.set_cell(x, y, " ", Style::Selection);
        }
    }

    /// A path with its folder dimmed so the file name stands out.
    fn put_path(&mut self, x: u16, y: u16, path: &str, selected: bool, right: u16) -> u16 {
        let name_start = path.rfind('/').map_or(0, |slash| slash + 1);
        let (folder, name) = path.split_at(name_start);
        let x = self.put(x, y, folder, tone(Style::Muted, selected), right);
        self.put(x, y, name, tone(Style::Default, selected), right)
    }

    fn draw_recent_file(&mut self, content: &Content, y: u16, index: usize, selected: bool) {
        let ui = self.ui;
        let file = &ui.dashboard_workspace.recent_files[index];
        if selected {
            self.fill(content, y);
        }
        let right = content.right;
        if index < 9 {
            let number = (index + 1).to_string();
            self.put(content.x, y, &number, tone(Style::Accent, selected), right);
        }
        let age_x = right.saturating_sub(self.width(&file.age));
        self.put(age_x, y, &file.age, tone(Style::Muted, selected), right);
        let x = content.x + 3;
        let project = file
            .project
            .as_ref()
            .map_or(0, |project| self.width(project) + 2);
        let room = age_x.saturating_sub(x + 1).saturating_sub(project);
        let path = truncate_content_title(&file.label, usize::from(room), self.ambiguous);
        let x = self.put_path(x, y, &path, selected, age_x);
        if let Some(project) = &file.project {
            self.put(x + 2, y, project, tone(Style::Accent, selected), age_x);
        }
    }

    fn draw_change(&mut self, content: &Content, y: u16, index: usize, selected: bool) {
        let Some(changes) = &self.ui.dashboard_workspace.changes else {
            return;
        };
        let change = &changes.files[index];
        if selected {
            self.fill(content, y);
        }
        let right = content.right;
        let code_style = match change.code {
            '?' | 'A' => Style::GitSignAdded,
            'D' => Style::GitSignDeleted,
            'U' => Style::Error,
            _ => Style::GitSignChanged,
        };
        self.put(
            content.x,
            y,
            &change.code.to_string(),
            tone(code_style, selected),
            right,
        );
        let counts: Vec<(String, Style)> = match (change.code, change.lines) {
            ('?', _) => vec![("new".into(), Style::GitSignAdded)],
            (_, Some((added, removed))) => vec![
                (format!("+{added}"), Style::GitSignAdded),
                (format!("-{removed}"), Style::GitSignDeleted),
            ],
            (_, None) => Vec::new(),
        };
        let counts_width = counts
            .iter()
            .map(|(text, _)| self.width(text) + 1)
            .sum::<u16>();
        let mut x = right.saturating_sub(counts_width.saturating_sub(1));
        let counts_x = x;
        for (text, style) in counts {
            x = self.put(x, y, &text, tone(style, selected), right) + 1;
        }
        let path_x = content.x + 2;
        let room = counts_x.saturating_sub(path_x + 1);
        let path = truncate_content_title(&change.label, usize::from(room), self.ambiguous);
        self.put_path(path_x, y, &path, selected, counts_x);
    }

    fn draw_project(
        &mut self,
        content: &Content,
        y: u16,
        index: usize,
        path_offset: u16,
        selected: bool,
    ) {
        let ui = self.ui;
        let project = &ui.dashboard_workspace.projects[index];
        if selected {
            self.fill(content, y);
        }
        let right = content.right;
        let path_x = content.x + path_offset;
        let mut path_right = right;
        if let Some(branch) = &project.branch {
            // A long branch name takes at most half the room after the name.
            let icon = if self.icons { "\u{e0a0} " } else { "" };
            let room = usize::from(right.saturating_sub(path_x) / 2);
            let branch = ellipsized(&format!("{icon}{branch}"), room, self.ambiguous);
            path_right = right.saturating_sub(self.width(&branch));
            self.put(path_right, y, &branch, tone(Style::Accent, selected), right);
        }
        self.put(
            content.x,
            y,
            &project.name,
            tone(Style::Default, selected),
            path_x.saturating_sub(1),
        );
        let room = path_right.saturating_sub(path_x + 1);
        let path = home_relative(&project.path, usize::from(room), self.ambiguous);
        self.put(path_x, y, &path, tone(Style::Muted, selected), path_right);
    }

    /// How many columns of actions fit in `width`, at most three.
    fn start_columns(&self, width: u16) -> u16 {
        let ui = self.ui;
        let actions = &ui.dashboard_actions;
        let item = self.key_width()
            + self.icon_width()
            + actions
                .iter()
                .map(|action| self.width(&action.label))
                .max()
                .unwrap_or(0)
            + KEY_GAP;
        let count = u16::try_from(actions.len()).unwrap_or(u16::MAX);
        (width.saturating_sub(4) / item.max(1))
            .clamp(1, 3)
            .min(count.max(1))
    }

    /// The rows the actions take in a box `width` columns wide.
    fn start_rows(&self, width: u16) -> u16 {
        let count = u16::try_from(self.ui.dashboard_actions.len()).unwrap_or(u16::MAX);
        count.div_ceil(self.start_columns(width))
    }

    fn key_width(&self) -> u16 {
        self.ui
            .dashboard_actions
            .iter()
            .filter_map(|action| action.key.as_deref())
            .map(|key| self.width(key))
            .max()
            .map_or(0, |width| width + KEY_GAP)
    }

    fn icon_width(&self) -> u16 {
        self.ui
            .dashboard_actions
            .iter()
            .filter(|action| !action.icon.is_empty())
            .map(|action| self.width(&action.icon) + 1)
            .max()
            .unwrap_or(0)
    }

    /// The actions in rows of up to three, each key before its label.
    fn draw_actions(&mut self, content: &Content, selected: Option<usize>) {
        let columns = self.start_columns(content.right.saturating_sub(content.x) + 4);
        let pitch = (content.right.saturating_sub(content.x) + 1) / columns;
        let key_width = self.key_width();
        let icon_width = self.icon_width();
        let rows = self
            .ui
            .dashboard_actions
            .len()
            .div_ceil(usize::from(columns));
        let selected_row = selected.map(|index| index / usize::from(columns));
        let actions = &self.ui.dashboard_actions;
        for (y, row) in visible_rows(content, rows, selected_row) {
            for column in 0..columns {
                let index = row * usize::from(columns) + usize::from(column);
                let Some(action) = actions.get(index) else {
                    break;
                };
                let x = content.x + column * pitch;
                let column_right = (x + pitch).min(content.right);
                let chosen = selected == Some(index);
                if chosen {
                    let fill_right = (x + pitch - 1).min(content.fill_right);
                    for fill in x.saturating_sub(1)..fill_right {
                        self.grid.set_cell(fill, y, " ", Style::Selection);
                    }
                }
                if let Some(key) = &action.key {
                    self.put(x, y, key, tone(Style::Accent, chosen), column_right);
                }
                if !action.icon.is_empty() {
                    let icon_x = x + key_width;
                    self.put(
                        icon_x,
                        y,
                        &action.icon,
                        tone(Style::Muted, chosen),
                        column_right,
                    );
                }
                let label_x = x + key_width + icon_width;
                let label_right = column_right.saturating_sub(1).max(label_x);
                self.put(
                    label_x,
                    y,
                    &action.label,
                    tone(Style::Muted, chosen),
                    label_right,
                );
            }
        }
    }
}

/// The screen rows of a box and the list rows they show, scrolled so the selected row is in
/// view.
fn visible_rows(
    content: &Content,
    count: usize,
    selected: Option<usize>,
) -> impl Iterator<Item = (u16, usize)> {
    let rows = usize::from(content.rows);
    let first = selected.map_or(0, |row| (row + 1).saturating_sub(rows));
    (content.y..content.y + content.rows).zip(first..count)
}

/// A style as it shows on the selected row: the selection's background, with coloured text
/// keeping its colour.
fn tone(style: Style, selected: bool) -> Style {
    if !selected {
        return style;
    }
    match style {
        Style::Accent => Style::QueryMatch { selected: true },
        Style::Muted => Style::SelectionText(SelectedText::Muted),
        Style::GitSignAdded => Style::SelectionText(SelectedText::Added),
        Style::GitSignChanged => Style::SelectionText(SelectedText::Changed),
        Style::GitSignDeleted => Style::SelectionText(SelectedText::Deleted),
        Style::Error => Style::SelectionText(SelectedText::Error),
        _ => Style::Selection,
    }
}

/// DUST in half blocks: each letter twice as wide as its grid, two columns apart.
fn wordmark() -> Vec<String> {
    let rows: Vec<String> = (0..8)
        .map(|row| {
            WORDMARK_LETTERS
                .iter()
                .flat_map(|letter| {
                    letter[row]
                        .chars()
                        .flat_map(|bit| [bit, bit])
                        .chain(['0', '0'])
                })
                .collect()
        })
        .collect();
    rows.chunks(2)
        .map(|pair| {
            pair[0]
                .chars()
                .zip(pair[1].chars())
                .map(|cells| match cells {
                    ('1', '1') => '█',
                    ('1', _) => '▀',
                    (_, '1') => '▄',
                    _ => ' ',
                })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
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

    #[test]
    fn the_wordmark_is_four_half_block_rows() {
        assert_eq!(
            wordmark(),
            [
                "██▀▀▀▀▀▀▄▄  ██      ██  ▄▄▀▀▀▀▀▀▀▀  ▀▀▀▀██▀▀▀▀",
                "██      ██  ██      ██  ▀▀▄▄▄▄▄▄        ██",
                "██      ██  ██      ██          ██      ██",
                "██▄▄▄▄▄▄▀▀  ▀▀▄▄▄▄▄▄▀▀  ▄▄▄▄▄▄▄▄▀▀      ██",
            ]
        );
    }
}
