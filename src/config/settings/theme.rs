use std::{collections::BTreeMap, sync::OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::language::syntax::Highlight;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeColor {
    Reset,
    Black,
    DarkGrey,
    Red,
    DarkRed,
    Green,
    DarkGreen,
    Yellow,
    DarkYellow,
    Blue,
    DarkBlue,
    Magenta,
    DarkMagenta,
    Cyan,
    DarkCyan,
    White,
    Grey,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl ThemeColor {
    fn parse(value: &str) -> Result<Self, String> {
        let named = match value {
            "reset" => Self::Reset,
            "black" => Self::Black,
            "dark_grey" => Self::DarkGrey,
            "red" => Self::Red,
            "dark_red" => Self::DarkRed,
            "green" => Self::Green,
            "dark_green" => Self::DarkGreen,
            "yellow" => Self::Yellow,
            "dark_yellow" => Self::DarkYellow,
            "blue" => Self::Blue,
            "dark_blue" => Self::DarkBlue,
            "magenta" => Self::Magenta,
            "dark_magenta" => Self::DarkMagenta,
            "cyan" => Self::Cyan,
            "dark_cyan" => Self::DarkCyan,
            "white" => Self::White,
            "grey" => Self::Grey,
            _ if value.starts_with("indexed:") => {
                let index = value[8..]
                    .parse::<u8>()
                    .map_err(|_| format!("invalid indexed color `{value}`"))?;
                Self::Indexed(index)
            }
            _ if value.starts_with('#') && value.len() == 7 && value.is_ascii() => {
                let red = u8::from_str_radix(&value[1..3], 16)
                    .map_err(|_| format!("invalid RGB color `{value}`"))?;
                let green = u8::from_str_radix(&value[3..5], 16)
                    .map_err(|_| format!("invalid RGB color `{value}`"))?;
                let blue = u8::from_str_radix(&value[5..7], 16)
                    .map_err(|_| format!("invalid RGB color `{value}`"))?;
                Self::Rgb(red, green, blue)
            }
            _ => return Err(format!("unknown theme color `{value}`")),
        };
        Ok(named)
    }

    fn notation(self) -> String {
        match self {
            Self::Reset => "reset".into(),
            Self::Black => "black".into(),
            Self::DarkGrey => "dark_grey".into(),
            Self::Red => "red".into(),
            Self::DarkRed => "dark_red".into(),
            Self::Green => "green".into(),
            Self::DarkGreen => "dark_green".into(),
            Self::Yellow => "yellow".into(),
            Self::DarkYellow => "dark_yellow".into(),
            Self::Blue => "blue".into(),
            Self::DarkBlue => "dark_blue".into(),
            Self::Magenta => "magenta".into(),
            Self::DarkMagenta => "dark_magenta".into(),
            Self::Cyan => "cyan".into(),
            Self::DarkCyan => "dark_cyan".into(),
            Self::White => "white".into(),
            Self::Grey => "grey".into(),
            Self::Indexed(index) => format!("indexed:{index}"),
            Self::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
        }
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

impl Serialize for ThemeColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.notation())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAttribute {
    Bold,
    Dim,
    Italic,
    Underlined,
    Reverse,
    Strikethrough,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeStyle {
    pub foreground: ThemeColor,
    pub background: ThemeColor,
    #[serde(default)]
    pub foreground_256: Option<ThemeColor>,
    #[serde(default)]
    pub background_256: Option<ThemeColor>,
    #[serde(default)]
    pub foreground_16: Option<ThemeColor>,
    #[serde(default)]
    pub background_16: Option<ThemeColor>,
    #[serde(default)]
    pub attributes: Vec<ThemeAttribute>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeStyles {
    pub default: ThemeStyle,
    pub muted: ThemeStyle,
    pub accent: ThemeStyle,
    pub status: ThemeStyle,
    pub status_accent: ThemeStyle,
    pub selection: ThemeStyle,
    pub search: ThemeStyle,
    pub error: ThemeStyle,
    pub warning: ThemeStyle,
    pub border: ThemeStyle,
    pub buffer_line_background: ThemeStyle,
    pub buffer_line_active: ThemeStyle,
    pub buffer_line_inactive: ThemeStyle,
    pub buffer_line_dirty: ThemeStyle,
    pub buffer_line_icon: ThemeStyle,
    pub buffer_line_pin: ThemeStyle,
    pub buffer_line_diagnostic: ThemeStyle,
    pub buffer_line_close: ThemeStyle,
    pub buffer_line_separator: ThemeStyle,
    pub key_hint_background: ThemeStyle,
    pub key_hint_border: ThemeStyle,
    pub key_hint_key: ThemeStyle,
    pub key_hint_group: ThemeStyle,
    pub key_hint_command: ThemeStyle,
    pub key_hint_prefix: ThemeStyle,
    pub documentation_background: ThemeStyle,
    pub documentation_border: ThemeStyle,
    pub documentation_title: ThemeStyle,
    pub documentation_text: ThemeStyle,
    pub git_sign_added: ThemeStyle,
    pub git_sign_changed: ThemeStyle,
    pub git_sign_deleted: ThemeStyle,
    pub explorer_background: ThemeStyle,
    pub explorer_header: ThemeStyle,
    pub explorer_count: ThemeStyle,
    pub explorer_tree: ThemeStyle,
    pub explorer_icon: ThemeStyle,
    pub explorer_directory: ThemeStyle,
    pub explorer_file: ThemeStyle,
    pub explorer_symlink: ThemeStyle,
    pub explorer_git: ThemeStyle,
    pub explorer_diagnostic: ThemeStyle,
    pub explorer_decoration: ThemeStyle,
    pub explorer_selection: ThemeStyle,
    pub explorer_border: ThemeStyle,
    pub terminal_background: ThemeStyle,
    pub terminal_border: ThemeStyle,
    pub terminal_title: ThemeStyle,
    pub terminal_status: ThemeStyle,
    pub terminal_status_accent: ThemeStyle,
    pub terminal_text: ThemeStyle,
}

/// A Neovim highlight group's definition, in the shape `nvim_set_hl` takes and
/// `nvim_get_hl` returns, so a colorscheme's groups can be copied across as they are.
/// Keys Dust doesn't use, such as `sp` or `blend`, are ignored.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct HighlightDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<ThemeColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<ThemeColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctermfg: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctermbg: Option<u8>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub undercurl: bool,
    #[serde(default)]
    pub strikethrough: bool,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// Every group's style, worked out on first use.
#[derive(Clone, Debug, Default)]
struct ResolvedHighlights(OnceLock<Vec<Option<ThemeStyle>>>);
impl PartialEq for ResolvedHighlights {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for ResolvedHighlights {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub name: String,
    pub styles: ThemeStyles,
    /// Neovim highlight groups, such as `"@function" = { fg = "#7aa2f7" }` or
    /// `"@lsp.type.property" = { link = "@property" }`.
    #[serde(default)]
    pub highlights: BTreeMap<String, HighlightDefinition>,
    #[serde(skip)]
    resolved: ResolvedHighlights,
}

impl ThemeConfig {
    /// A group's style, following links and parent groups as Neovim does: the theme's own
    /// definition first, then Neovim's default for that group, then the parent group's.
    /// `None` when nothing colours it.
    #[must_use]
    pub fn highlight(&self, group: Highlight) -> Option<&ThemeStyle> {
        self.resolved
            .0
            .get_or_init(|| Highlight::all().map(|group| self.resolve(group.name())).collect())
            .get(group.index())?
            .as_ref()
    }

    fn resolve<'a>(&'a self, mut name: &'a str) -> Option<ThemeStyle> {
        // Links can loop in a hand-written theme.
        for _ in 0..32 {
            match self.highlights.get(name).or_else(|| Highlight::default_definition(name)) {
                Some(HighlightDefinition { link: Some(link), .. }) => name = link,
                Some(definition) => return Some(self.highlight_style(definition)),
                None if name.starts_with('@') => name = name.rsplit_once('.')?.0,
                None => return None,
            }
        }
        None
    }

    fn highlight_style(&self, definition: &HighlightDefinition) -> ThemeStyle {
        let base = &self.styles.default;
        let channel = |colour: Option<ThemeColor>, cterm: Option<u8>, fallback| match colour {
            Some(colour) => (
                colour,
                cterm.map(ThemeColor::Indexed).or_else(|| nearest_256(colour)),
                nearest_16(colour),
            ),
            None => fallback,
        };
        let foreground = channel(
            definition.fg,
            definition.ctermfg,
            (base.foreground, base.foreground_256, base.foreground_16),
        );
        let background = channel(
            definition.bg,
            definition.ctermbg,
            (base.background, base.background_256, base.background_16),
        );
        let attributes = [
            (definition.bold, ThemeAttribute::Bold),
            (definition.italic, ThemeAttribute::Italic),
            (definition.underline || definition.undercurl, ThemeAttribute::Underlined),
            (definition.strikethrough, ThemeAttribute::Strikethrough),
            (definition.reverse, ThemeAttribute::Reverse),
        ]
        .into_iter()
        .filter_map(|(on, attribute)| on.then_some(attribute))
        .collect();
        ThemeStyle {
            foreground: foreground.0,
            foreground_256: foreground.1,
            foreground_16: foreground.2,
            background: background.0,
            background_256: background.1,
            background_16: background.2,
            attributes,
        }
    }

    pub(in crate::config) fn validate(&self) -> Result<(), String> {
        let styles = [
            ("default", &self.styles.default),
            ("muted", &self.styles.muted),
            ("accent", &self.styles.accent),
            ("status", &self.styles.status),
            ("status_accent", &self.styles.status_accent),
            ("selection", &self.styles.selection),
            ("search", &self.styles.search),
            ("error", &self.styles.error),
            ("warning", &self.styles.warning),
            ("border", &self.styles.border),
            (
                "buffer_line_background",
                &self.styles.buffer_line_background,
            ),
            ("buffer_line_active", &self.styles.buffer_line_active),
            ("buffer_line_inactive", &self.styles.buffer_line_inactive),
            ("buffer_line_dirty", &self.styles.buffer_line_dirty),
            ("buffer_line_icon", &self.styles.buffer_line_icon),
            ("buffer_line_pin", &self.styles.buffer_line_pin),
            (
                "buffer_line_diagnostic",
                &self.styles.buffer_line_diagnostic,
            ),
            ("buffer_line_close", &self.styles.buffer_line_close),
            ("buffer_line_separator", &self.styles.buffer_line_separator),
            ("key_hint_background", &self.styles.key_hint_background),
            ("key_hint_border", &self.styles.key_hint_border),
            ("key_hint_key", &self.styles.key_hint_key),
            ("key_hint_group", &self.styles.key_hint_group),
            ("key_hint_command", &self.styles.key_hint_command),
            ("key_hint_prefix", &self.styles.key_hint_prefix),
            (
                "documentation_background",
                &self.styles.documentation_background,
            ),
            ("documentation_border", &self.styles.documentation_border),
            ("documentation_title", &self.styles.documentation_title),
            ("documentation_text", &self.styles.documentation_text),
            ("git_sign_added", &self.styles.git_sign_added),
            ("git_sign_changed", &self.styles.git_sign_changed),
            ("git_sign_deleted", &self.styles.git_sign_deleted),
            ("explorer_background", &self.styles.explorer_background),
            ("explorer_header", &self.styles.explorer_header),
            ("explorer_count", &self.styles.explorer_count),
            ("explorer_tree", &self.styles.explorer_tree),
            ("explorer_icon", &self.styles.explorer_icon),
            ("explorer_directory", &self.styles.explorer_directory),
            ("explorer_file", &self.styles.explorer_file),
            ("explorer_symlink", &self.styles.explorer_symlink),
            ("explorer_git", &self.styles.explorer_git),
            ("explorer_diagnostic", &self.styles.explorer_diagnostic),
            ("explorer_decoration", &self.styles.explorer_decoration),
            ("explorer_selection", &self.styles.explorer_selection),
            ("explorer_border", &self.styles.explorer_border),
            ("terminal_background", &self.styles.terminal_background),
            ("terminal_border", &self.styles.terminal_border),
            ("terminal_title", &self.styles.terminal_title),
            ("terminal_status", &self.styles.terminal_status),
            (
                "terminal_status_accent",
                &self.styles.terminal_status_accent,
            ),
            ("terminal_text", &self.styles.terminal_text),
        ];
        for (name, style) in styles {
            if style.foreground == style.background && style.foreground != ThemeColor::Reset {
                return Err(format!(
                    "theme style `{name}` has identical foreground and background"
                ));
            }
            for (channel, primary, fallback_256, fallback_16) in [
                (
                    "foreground",
                    style.foreground,
                    style.foreground_256,
                    style.foreground_16,
                ),
                (
                    "background",
                    style.background,
                    style.background_256,
                    style.background_16,
                ),
            ] {
                if matches!(primary, ThemeColor::Rgb(..)) && fallback_256.is_none() {
                    return Err(format!(
                        "theme style `{name}` {channel} requires a 256-color fallback"
                    ));
                }
                if matches!(primary, ThemeColor::Rgb(..) | ThemeColor::Indexed(_))
                    && fallback_16.is_none()
                {
                    return Err(format!(
                        "theme style `{name}` {channel} requires a 16-color fallback"
                    ));
                }
                if fallback_256.is_some_and(|color| matches!(color, ThemeColor::Rgb(..))) {
                    return Err(format!(
                        "theme style `{name}` {channel}_256 cannot be truecolor"
                    ));
                }
                if fallback_16.is_some_and(|color| {
                    matches!(color, ThemeColor::Rgb(..) | ThemeColor::Indexed(_))
                }) {
                    return Err(format!(
                        "theme style `{name}` {channel}_16 must use a named ANSI color"
                    ));
                }
            }
        }
        Ok(())
    }
}

const XTERM_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The nearest xterm-256 colour, for terminals without truecolor.
fn nearest_256(colour: ThemeColor) -> Option<ThemeColor> {
    let ThemeColor::Rgb(red, green, blue) = colour else {
        return matches!(colour, ThemeColor::Indexed(_)).then_some(colour);
    };
    let cube = (0..216u8).map(|index| {
        let level = |step: u8| XTERM_LEVELS[usize::from(step)];
        (16 + index, (level(index / 36), level(index / 6 % 6), level(index % 6)))
    });
    let greys = (0..24u8).map(|step| (232 + step, (8 + step * 10, 8 + step * 10, 8 + step * 10)));
    cube.chain(greys)
        .min_by_key(|(_, rgb)| distance((red, green, blue), *rgb))
        .map(|(index, _)| ThemeColor::Indexed(index))
}

/// The nearest of the sixteen ANSI colours, for the most limited terminals.
fn nearest_16(colour: ThemeColor) -> Option<ThemeColor> {
    let (red, green, blue) = match colour {
        ThemeColor::Rgb(red, green, blue) => (red, green, blue),
        ThemeColor::Indexed(index) => return nearest_16(indexed_rgb(index)),
        _ => return None,
    };
    [
        (ThemeColor::Black, (0, 0, 0)),
        (ThemeColor::DarkRed, (205, 0, 0)),
        (ThemeColor::DarkGreen, (0, 205, 0)),
        (ThemeColor::DarkYellow, (205, 205, 0)),
        (ThemeColor::DarkBlue, (0, 0, 238)),
        (ThemeColor::DarkMagenta, (205, 0, 205)),
        (ThemeColor::DarkCyan, (0, 205, 205)),
        (ThemeColor::Grey, (229, 229, 229)),
        (ThemeColor::DarkGrey, (127, 127, 127)),
        (ThemeColor::Red, (255, 0, 0)),
        (ThemeColor::Green, (0, 255, 0)),
        (ThemeColor::Yellow, (255, 255, 0)),
        (ThemeColor::Blue, (92, 92, 255)),
        (ThemeColor::Magenta, (255, 0, 255)),
        (ThemeColor::Cyan, (0, 255, 255)),
        (ThemeColor::White, (255, 255, 255)),
    ]
    .into_iter()
    .min_by_key(|(_, rgb)| distance((red, green, blue), *rgb))
    .map(|(named, _)| named)
}

fn indexed_rgb(index: u8) -> ThemeColor {
    match index {
        16..=231 => {
            let index = index - 16;
            let level = |step: u8| XTERM_LEVELS[usize::from(step)];
            ThemeColor::Rgb(level(index / 36), level(index / 6 % 6), level(index % 6))
        }
        232..=255 => {
            let grey = 8 + (index - 232) * 10;
            ThemeColor::Rgb(grey, grey, grey)
        }
        _ => ThemeColor::Rgb(128, 128, 128),
    }
}

fn distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> u32 {
    let channel = |a: u8, b: u8| u32::from(a.abs_diff(b)).pow(2);
    channel(left.0, right.0) + channel(left.1, right.1) + channel(left.2, right.2)
}

