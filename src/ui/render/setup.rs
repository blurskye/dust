//! The first-start setup: one question per page, its choices down the left and, on the right,
//! a small Dust drawn in the theme's colours the way the highlighted choices would lay it out.

use crate::config::AgentConnection;

use crate::config::Config;

use crate::config::PanelSide;

use crate::config::Highlight;
use crate::config::TerminalPlacement;
use crate::ui::Style;
use crate::ui::geometry::text_width;

use crate::ui::grid::Frame;

use crate::ui::render::editor::END_OF_BUFFER_MARKER;

use crate::ui::render::editor::GIT_SIGN_ADDED;

use crate::ui::render::editor::GIT_SIGN_UNCHANGED;

use crate::ui::render::editor::GUTTER_PADDING;

use crate::ui::render::settings::SetupState;

use crate::ui::render::settings::draw_box;

use crate::ui::render::settings::fill;

use crate::ui::render::settings::settings_layout;

use crate::ui::geometry::ellipsized;

/// The list of choices is at least this wide beside the preview, and at most the second.
const LIST_WIDTH: (u16, u16) = (40, 60);
/// The preview's smallest and largest size, in columns and rows.
const PREVIEW_MINIMUM: (u16, u16) = (50, 12);
const PREVIEW_MAXIMUM: (u16, u16) = (84, 24);

pub(super) fn render_setup(
    frame: &mut Frame,
    setup: &SetupState,
    status: Option<&(String, bool)>,
    config: &Config,
    width: u16,
    height: u16,
) {
    let ambiguous = config.editor.ambiguous_width;
    let Some(layout) = settings_layout(width, height) else {
        for y in 0..height {
            fill(frame, 0, y, width, Style::Default);
        }
        frame.grid.put_text_clipped(
            0,
            height / 2,
            "Setup: make the window larger, or Esc to skip",
            Style::Accent,
            width,
        );
        return;
    };
    let Some(page) = setup.pages.get(setup.page) else {
        return;
    };
    let picked = setup.picked.get(setup.page).copied().unwrap_or(0);
    let (left, top) = (layout.left, layout.top);
    let right = left + layout.width;
    let bottom = top + layout.height;
    draw_box(frame, left, top, layout.width, layout.height);
    frame
        .grid
        .put_text(left + 2, top, " Welcome to Dust ", Style::Accent);

    // Where the person is: every page's title, the current one highlighted.
    let mut x = left + 2;
    for (index, other) in setup.pages.iter().enumerate() {
        let style = match index.cmp(&setup.page) {
            std::cmp::Ordering::Equal => Style::Accent,
            std::cmp::Ordering::Less => Style::Default,
            std::cmp::Ordering::Greater => Style::Muted,
        };
        if index > 0 {
            x = frame
                .grid
                .put_text_clipped(x, top + 1, " › ", Style::Muted, right - 2);
        }
        x = frame
            .grid
            .put_text_clipped(x, top + 1, &other.title, style, right - 2);
    }
    frame.grid.put_text_clipped(
        left + 2,
        top + 3,
        &page.question,
        Style::Syntax(crate::config::Highlight::HEADING),
        right - 2,
    );

    let list_x = left + 2;
    let room = layout.width.saturating_sub(4);
    // The preview goes beside the list when both fit, and is left out otherwise.
    let preview_width = room.saturating_sub(LIST_WIDTH.0 + 3).min(PREVIEW_MAXIMUM.0);
    let preview_height = bottom.saturating_sub(top + 8).min(PREVIEW_MAXIMUM.1);
    let show_preview = !page.preview.is_empty()
        && preview_width >= PREVIEW_MINIMUM.0
        && preview_height >= PREVIEW_MINIMUM.1;
    let list_right = if show_preview {
        list_x + (room - 3 - preview_width).min(LIST_WIDTH.1)
    } else {
        right - 2
    };
    let list_width = usize::from(list_right - list_x);
    let mut y = top + 5;
    for (index, choice) in page.choices.iter().enumerate() {
        if y + 1 >= bottom - 2 {
            break;
        }
        let selected = index == picked;
        if selected {
            fill(frame, list_x, y, list_right - list_x, Style::Selection);
            fill(frame, list_x, y + 1, list_right - list_x, Style::Selection);
            frame.grid.set_cell(list_x, y, "▌", Style::Accent);
            frame.grid.set_cell(list_x, y + 1, "▌", Style::Accent);
        }
        let label = ellipsized(&choice.label, list_width.saturating_sub(2), ambiguous);
        frame.grid.put_text(
            list_x + 2,
            y,
            &label,
            if selected {
                Style::Selection
            } else {
                Style::Default
            },
        );
        let about = ellipsized(&choice.about, list_width.saturating_sub(2), ambiguous);
        frame.grid.put_text(
            list_x + 2,
            y + 1,
            &about,
            if selected {
                Style::Selection
            } else {
                Style::Muted
            },
        );
        y += 3;
    }
    if show_preview {
        let preview_x = list_right + 3 + (right - 2 - list_right - 3 - preview_width) / 2;
        let area = Area {
            x: preview_x,
            y: top + 5,
            width: preview_width,
            height: preview_height,
        };
        render_preview(
            frame,
            &Scene::of(setup, &page.preview, config),
            config,
            area,
        );
    }

    let last = setup.page + 1 == setup.pages.len();
    let help = format!(
        "j/k choose · enter {} · backspace back · esc skip",
        if last { "finish" } else { "next" }
    );
    let footer_y = bottom - 2;
    frame
        .grid
        .put_text_clipped(left + 2, footer_y, &help, Style::Muted, right - 2);
    if let Some((text, failed)) = status {
        let style = if *failed { Style::Error } else { Style::Accent };
        let text = ellipsized(text, usize::from(layout.width.saturating_sub(6)), ambiguous);
        let width = u16::try_from(text_width(&text, ambiguous)).unwrap_or(u16::MAX);
        frame
            .grid
            .put_text(right.saturating_sub(2 + width), footer_y, &text, style);
    }
}

#[derive(Clone, Copy)]
struct Area {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

impl Area {
    fn right(self) -> u16 {
        self.x + self.width
    }

    fn bottom(self) -> u16 {
        self.y + self.height
    }
}

/// What the small editor shows: the panels where the highlighted choices put them, and what
/// the page is about open in it.
struct Scene<'a> {
    explorer: PanelSide,
    /// The agent panel's side, whether it is Dust's panel (ACP) rather than the agent's own
    /// program, and the agent's name.
    agent: Option<(PanelSide, bool, &'a str)>,
    terminal: Option<TerminalPlacement>,
    /// The leader key, with its shortcuts shown.
    leader: Option<&'a str>,
}

impl<'a> Scene<'a> {
    fn of(setup: &'a SetupState, preview: &str, config: &'a Config) -> Self {
        // The highlighted choice on every page, so each page shows the answers given so far.
        let chosen = |key: &str| {
            setup
                .pages
                .iter()
                .zip(&setup.picked)
                .filter_map(|(page, picked)| page.choices.get(*picked))
                .flat_map(|choice| &choice.settings)
                .find(|(name, _)| name == key)
                .and_then(|(_, value)| value.as_str())
        };
        let side = |key: &str, current: PanelSide| match chosen(key) {
            Some("left") => PanelSide::Left,
            Some("right") => PanelSide::Right,
            _ => current,
        };
        let acp = match chosen("agents.connection") {
            Some(connection) => connection == "acp",
            None => config.agents.connection == AgentConnection::Acp,
        };
        let agent = (
            side("agents.side", config.agents.side),
            acp,
            // "Ask every time" names no agent.
            Some(chosen("agents.default").unwrap_or(&config.agents.default))
                .filter(|name| !name.is_empty())
                .unwrap_or("agent"),
        );
        let terminal = match chosen("ui.terminal_panel.placement") {
            Some("float") => TerminalPlacement::Float,
            Some("fullscreen") => TerminalPlacement::Fullscreen,
            Some(_) => TerminalPlacement::Bottom,
            None => config.ui.terminal_panel.placement,
        };
        let leader = chosen("input.keymaps.leader").unwrap_or(&config.input.keymaps.leader);
        Self {
            explorer: side("ui.explorer.side", config.ui.explorer.side),
            agent: matches!(preview, "layout" | "agent").then_some(agent),
            terminal: (preview == "terminal").then_some(terminal),
            leader: (preview == "leader").then_some(leader),
        }
    }
}

/// A token of the sample code and its colour; `None` is plain text.
type Token = (&'static str, Option<Highlight>);

const KEYWORD: Option<Highlight> = Some(Highlight::KEYWORD);
const FUNCTION: Option<Highlight> = Some(Highlight::FUNCTION);
const STRING: Option<Highlight> = Some(Highlight::STRING);
const TYPE: Option<Highlight> = Some(Highlight::TYPE);
const COMMENT: Option<Highlight> = Some(Highlight::COMMENT);

const CODE: &[&[Token]] = &[
    &[("use", KEYWORD), (" std::fs;", None)],
    &[],
    &[("// How long the notes are.", COMMENT)],
    &[
        ("fn", KEYWORD),
        (" ", None),
        ("main", FUNCTION),
        ("() {", None),
    ],
    &[
        ("    ", None),
        ("let", KEYWORD),
        (" text = fs::", None),
        ("read_to_string", FUNCTION),
        ("(", None),
        ("\"notes.md\"", STRING),
        (");", None),
    ],
    &[
        ("    ", None),
        ("println!", FUNCTION),
        ("(", None),
        ("\"{} lines\"", STRING),
        (", ", None),
        ("count", FUNCTION),
        ("(&text));", None),
    ],
    &[("}", None)],
    &[],
    &[
        ("fn", KEYWORD),
        (" ", None),
        ("count", FUNCTION),
        ("(text: &", None),
        ("str", TYPE),
        (") -> ", None),
        ("usize", TYPE),
        (" {", None),
    ],
    &[
        ("    text.", None),
        ("lines", FUNCTION),
        ("().", None),
        ("count", FUNCTION),
        ("()", None),
    ],
    &[("}", None)],
];

/// The sample lines added since the last commit, for the git signs.
const ADDED_LINES: [usize; 2] = [5, 6];
/// The line the cursor is on, which the status line names too.
const CURSOR_LINE: usize = 5;

fn render_preview(frame: &mut Frame, scene: &Scene, config: &Config, area: Area) {
    framed(frame, area, Style::Border, Style::Default);
    let inner = Area {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };
    let body = Area {
        height: inner.height - 1,
        ..inner
    };
    // The panels go at their side's edge, the explorer outermost, and the code takes the rest.
    let explorer_width = (inner.width / 4).clamp(14, 22);
    let agent_width = (inner.width * 3 / 10).clamp(16, 28);
    let (mut code_left, mut code_right) = (body.x, body.right());
    let mut place = |side: PanelSide, width: u16| match side {
        PanelSide::Left => {
            let x = code_left;
            code_left += width + 1;
            (x, x + width)
        }
        PanelSide::Right => {
            code_right -= width + 1;
            (code_right + 1, code_right)
        }
    };
    let (explorer_x, explorer_divider) = place(scene.explorer, explorer_width);
    let agent = scene
        .agent
        .map(|(side, acp, name)| (place(side, agent_width), acp, name));
    let code = Area {
        x: code_left,
        width: code_right.saturating_sub(code_left),
        ..body
    };

    render_explorer(
        frame,
        config,
        Area {
            x: explorer_x,
            width: explorer_width,
            ..body
        },
    );
    for y in body.y..body.bottom() {
        frame
            .grid
            .set_cell(explorer_divider, y, "│", Style::ExplorerBorder);
    }
    if let Some(((x, divider), acp, name)) = agent {
        for y in body.y..body.bottom() {
            frame.grid.set_cell(divider, y, "│", Style::Border);
        }
        let panel = Area {
            x,
            width: agent_width,
            ..body
        };
        if acp {
            render_agent_panel(frame, name, panel);
        } else {
            render_agent_program(frame, name, panel);
        }
    }
    render_code(frame, code);
    match scene.terminal {
        Some(TerminalPlacement::Bottom) => {
            let height = (code.height * 2 / 5).max(4);
            render_terminal(
                frame,
                Area {
                    y: code.bottom() - height,
                    height,
                    ..code
                },
                false,
            );
        }
        Some(TerminalPlacement::Float) => {
            let (width, height) = (body.width * 3 / 5, (body.height * 3 / 5).max(6));
            render_terminal(
                frame,
                Area {
                    x: body.x + (body.width - width) / 2,
                    y: body.y + (body.height - height) / 2,
                    width,
                    height,
                },
                true,
            );
        }
        Some(TerminalPlacement::Fullscreen) => render_terminal(frame, body, false),
        None => {}
    }
    if let Some(leader) = scene.leader {
        render_leader_keys(frame, leader, code);
    }
    render_status(frame, scene, inner);
}

/// A box in `border`, its inside filled with `inside`.
fn framed(frame: &mut Frame, area: Area, border: Style, inside: Style) {
    let (right, bottom) = (area.right() - 1, area.bottom() - 1);
    for y in area.y..=bottom {
        fill(frame, area.x, y, area.width, inside);
        frame.grid.set_cell(area.x, y, "│", border);
        frame.grid.set_cell(right, y, "│", border);
    }
    for x in area.x..=right {
        frame.grid.set_cell(x, area.y, "─", border);
        frame.grid.set_cell(x, bottom, "─", border);
    }
    frame.grid.set_cell(area.x, area.y, "╭", border);
    frame.grid.set_cell(right, area.y, "╮", border);
    frame.grid.set_cell(area.x, bottom, "╰", border);
    frame.grid.set_cell(right, bottom, "╯", border);
}

/// Text in one row of `area`, cut off at its right edge.
fn line(frame: &mut Frame, area: Area, row: u16, x: u16, text: &str, style: Style) -> u16 {
    if row >= area.height {
        return x;
    }
    frame
        .grid
        .put_text_clipped(x, area.y + row, text, style, area.right())
}

fn render_explorer(frame: &mut Frame, config: &Config, area: Area) {
    let glyphs = config.ui.explorer.active_glyphs();
    for y in area.y..area.bottom() {
        fill(frame, area.x, y, area.width, Style::ExplorerBackground);
    }
    line(frame, area, 0, area.x + 1, "my-app", Style::ExplorerHeader);
    let space = &glyphs.icon_spacing;
    let entries: [(&str, &str, &str, Style); 6] = [
        ("", &glyphs.directory_open, "src", Style::ExplorerDirectory),
        (
            &glyphs.tree_middle,
            &glyphs.file,
            "lib.rs",
            Style::ExplorerFile,
        ),
        (
            &glyphs.tree_last,
            &glyphs.file,
            "main.rs",
            Style::ExplorerFile,
        ),
        ("", &glyphs.directory, "tests", Style::ExplorerDirectory),
        ("", &glyphs.file, "Cargo.toml", Style::ExplorerFile),
        ("", &glyphs.file, "README.md", Style::ExplorerFile),
    ];
    for (row, (tree, icon, name, style)) in (2..).zip(entries) {
        let selected = name == "main.rs";
        if selected && row < area.height {
            fill(
                frame,
                area.x,
                area.y + row,
                area.width,
                Style::ExplorerSelection,
            );
        }
        let pick = |style| {
            if selected {
                Style::ExplorerSelection
            } else {
                style
            }
        };
        let x = line(
            frame,
            area,
            row,
            area.x + 1,
            tree,
            pick(Style::ExplorerTree),
        );
        let x = line(
            frame,
            area,
            row,
            x,
            &format!("{icon}{space}"),
            pick(Style::ExplorerIcon),
        );
        line(frame, area, row, x, name, pick(style));
    }
}

fn render_code(frame: &mut Frame, area: Area) {
    // The open files along the top, as the buffer line shows them.
    fill(
        frame,
        area.x,
        area.y,
        area.width,
        Style::BufferLineBackground,
    );
    let x = line(frame, area, 0, area.x, " main.rs ", Style::BufferLineActive);
    line(frame, area, 0, x, " lib.rs ", Style::BufferLineInactive);
    // The gutter as the editor draws it: relative line numbers, the cursor's line in full,
    // then the git sign column and `~` past the end.
    for row in 1..area.height {
        let number = usize::from(row);
        let tokens = CODE.get(number - 1).copied();
        let (label, style) = match tokens {
            Some(_) if number == CURSOR_LINE => (format!("{number:>2} "), Style::Accent),
            Some(_) => (
                format!("{:>2} ", number.abs_diff(CURSOR_LINE)),
                Style::Muted,
            ),
            None => (format!("{:>2} ", END_OF_BUFFER_MARKER), Style::Muted),
        };
        let x = line(frame, area, row, area.x, &label, style);
        let (sign, style) = if ADDED_LINES.contains(&number) {
            (GIT_SIGN_ADDED, Style::GitSignAdded)
        } else {
            (GIT_SIGN_UNCHANGED, Style::Muted)
        };
        let x = line(frame, area, row, x, sign, style);
        let mut x = x + GUTTER_PADDING;
        for &(text, highlight) in tokens.unwrap_or_default() {
            let style = highlight.map_or(Style::Default, Style::Syntax);
            x = line(frame, area, row, x, text, style);
        }
    }
}

fn render_terminal(frame: &mut Frame, area: Area, floating: bool) {
    let inside = if floating {
        framed(
            frame,
            area,
            Style::TerminalBorder,
            Style::TerminalBackground,
        );
        frame.grid.put_text_clipped(
            area.x + 2,
            area.y,
            " Terminal ",
            Style::TerminalTitle,
            area.right(),
        );
        Area {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width - 2,
            height: area.height - 2,
        }
    } else {
        for x in area.x..area.right() {
            frame.grid.set_cell(x, area.y, "─", Style::TerminalBorder);
        }
        line(
            frame,
            area,
            0,
            area.x + 1,
            " Terminal ",
            Style::TerminalTitle,
        );
        for y in area.y + 1..area.bottom() {
            fill(frame, area.x, y, area.width, Style::TerminalBackground);
        }
        Area {
            y: area.y + 1,
            height: area.height - 1,
            ..area
        }
    };
    let output = [
        "$ cargo test",
        "   Compiling my-app v0.1.0",
        "    Finished in 0.42s",
        "test result: ok. 3 passed",
        "$ ",
    ];
    for (row, text) in (0..).zip(output) {
        line(
            frame,
            inside,
            row,
            inside.x + 1,
            text,
            Style::TerminalBackground,
        );
    }
}

fn render_agent_panel(frame: &mut Frame, name: &str, area: Area) {
    let x = line(frame, area, 0, area.x + 1, name, Style::Accent);
    line(frame, area, 0, x, " · agent", Style::Muted);
    let transcript = [
        (2, "You", Style::Accent),
        (3, "How long is notes.md?", Style::Default),
        (5, name, Style::Accent),
        (6, "● Read notes.md", Style::Muted),
        (7, "It has 42 lines.", Style::Default),
    ];
    for (row, text, style) in transcript {
        if row + 3 < area.height {
            line(frame, area, row, area.x + 1, text, style);
        }
    }
    let input = area.height - 2;
    for x in area.x..area.right() {
        frame.grid.set_cell(x, area.y + input, "─", Style::Border);
    }
    line(
        frame,
        area,
        input + 1,
        area.x + 1,
        &format!("Ask {name}…"),
        Style::Muted,
    );
}

/// The agent's own program, running in a terminal in the agent's place.
fn render_agent_program(frame: &mut Frame, name: &str, area: Area) {
    for y in area.y..area.bottom() {
        fill(frame, area.x, y, area.width, Style::TerminalBackground);
    }
    line(
        frame,
        area,
        0,
        area.x,
        &format!(" {name} "),
        Style::TerminalTitle,
    );
    let output = [
        format!("$ {name}"),
        "> how long is notes.md?".into(),
        "● Read(notes.md)".into(),
        "  It has 42 lines.".into(),
        String::new(),
        "> ".into(),
    ];
    for (row, text) in (2..).zip(&output) {
        line(
            frame,
            area,
            row,
            area.x + 1,
            text,
            Style::TerminalBackground,
        );
    }
}

/// The shortcuts that follow the leader key, as the key hints show them after it is pressed.
fn render_leader_keys(frame: &mut Frame, leader: &str, code: Area) {
    let entries = [
        ("a", "+agent", true),
        ("e", "Explorer", false),
        ("f", "+find", true),
        ("g", "+git", true),
        ("s", "+search", true),
        ("w", "+windows", true),
        ("q", "+quit", true),
    ];
    let width = 18.min(code.width);
    let height = (u16::try_from(entries.len()).unwrap_or(u16::MAX) + 2).min(code.height - 1);
    let area = Area {
        x: code.right() - width,
        y: code.bottom() - height,
        width,
        height,
    };
    framed(frame, area, Style::KeyHintBorder, Style::KeyHintBackground);
    frame.grid.put_text_clipped(
        area.x + 2,
        area.y,
        &format!(" {leader} "),
        Style::KeyHintPrefix,
        area.right() - 1,
    );
    let inside = Area {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };
    for (row, (key, label, group)) in (0..).zip(entries) {
        let x = line(frame, inside, row, inside.x + 1, key, Style::KeyHintKey);
        let x = line(frame, inside, row, x, " → ", Style::KeyHintBackground);
        let style = if group {
            Style::KeyHintGroup
        } else {
            Style::KeyHintCommand
        };
        line(frame, inside, row, x, label, style);
    }
}

fn render_status(frame: &mut Frame, scene: &Scene, inner: Area) {
    let row = inner.height - 1;
    fill(frame, inner.x, inner.y + row, inner.width, Style::Status);
    let x = line(frame, inner, row, inner.x, " NORMAL ", Style::StatusAccent);
    line(frame, inner, row, x, " main.rs", Style::Status);
    // The leader just pressed shows at the right, as typed keys do.
    let right = match scene.leader {
        Some(leader) => format!("{leader}  5:9 "),
        None => "5:9 ".into(),
    };
    let width = u16::try_from(right.chars().count()).unwrap_or(u16::MAX);
    line(
        frame,
        inner,
        row,
        inner.right().saturating_sub(width),
        &right,
        Style::Status,
    );
}
