//! The Settings page: every setting from `assets/defaults/settings.toml` with its current value.
//! A change is written to the user's config.toml and applied by the normal configuration reload,
//! which refreshes the page.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::commands::SettingsAction;
use crate::config::SettingInfo;
use crate::config::default_setting;
use crate::config::settings_catalog;
use crate::config::theme_display_name;
use crate::config::write_user_setting;
use crate::ui::SettingsEdit;
use crate::ui::SettingsHit;
use crate::ui::SettingsLine;
use crate::ui::SettingsPageState;
use crate::ui::SettingsRow;
use crate::ui::SettingsValue;
use crate::ui::settings_hit;
use crate::ui::settings_layout;
use crate::ui::settings_lines_height;

const MODIFIED_FILTER: &str = "@modified";

impl App {
    pub(in crate::app) fn open_settings(&mut self) {
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.close_prompt();
        self.close_picker();
        self.ui.tool_menu = None;
        self.input.capture_changed();
        if self.ui.settings.as_ref().is_none_or(|page| page.shortcuts) {
            self.ui.settings = Some(SettingsPageState::default());
        }
        self.refresh_settings();
    }

    /// Rebuild the rows from the configuration in use, keeping the search and the selection.
    pub(in crate::app) fn refresh_settings(&mut self) {
        let Some(page) = self.ui.settings.as_ref() else {
            return;
        };
        let selected = page
            .selected_row()
            .map(|row| (row.key.clone(), row.section));
        let (sections, rows) = if page.shortcuts {
            self.shortcut_rows()
        } else {
            self.setting_rows()
        };
        let Some(page) = self.ui.settings.as_mut() else {
            return;
        };
        page.sections = sections;
        page.rows = rows;
        page.saving = false;
        filter_lines(page);
        page.selected = selected
            .as_ref()
            .and_then(|(key, section)| {
                page.lines.iter().position(|line| {
                    matches!(line, SettingsLine::Setting(row)
                        if page.rows[*row].key == *key && page.rows[*row].section == *section)
                })
            })
            .unwrap_or_else(|| first_setting(page));
        if page.selected_row().map(|row| (&row.key, row.section))
            != selected.as_ref().map(|(key, section)| (key, *section))
        {
            page.edit = None;
        }
        let count = choice_count(page);
        if let Some(SettingsEdit::Choice { selected }) = &mut page.edit {
            *selected = (*selected).min(count.saturating_sub(1));
        }
        self.scroll_settings();
    }

    /// Every setting in the catalog, under its section, with its current value.
    fn setting_rows(&self) -> (Vec<String>, Vec<SettingsRow>) {
        let catalog = settings_catalog();
        let current = toml::Value::try_from(self.configuration.settings()).ok();
        let user_keys = self.configuration.user_setting_keys();
        let theme = theme_display_name(&self.configuration.theme().name);
        let mut rows = Vec::new();
        for (index, section) in catalog.sections.iter().enumerate() {
            let settings: Vec<&SettingInfo> = if section.common.is_empty() {
                section.settings.iter().collect()
            } else {
                section
                    .common
                    .iter()
                    .filter_map(|key| catalog.setting(key))
                    .collect()
            };
            rows.extend(settings.into_iter().map(|info| SettingsRow {
                key: info.key.clone(),
                section: index,
                title: info.title(),
                about: info.about.clone(),
                value: setting_value(info, current.as_ref(), &theme),
                modified: user_keys.contains(&info.key),
                restart: info.restart,
            }));
        }
        let sections = catalog
            .sections
            .iter()
            .map(|section| section.title.clone())
            .collect();
        (sections, rows)
    }

    pub(in crate::app) fn settings_status(&mut self, message: String, error: bool) {
        if let Some(page) = self.ui.settings.as_mut() {
            page.saving = false;
            page.status = Some((message, error));
        }
    }

    fn settings_height(&self) -> u16 {
        self.workspace_bounds()
            .map_or(self.window_session.height(), |bounds| bounds.height)
    }

    /// Scroll so the selected setting, its heading and an open dropdown are on screen.
    fn scroll_settings(&mut self) {
        let Some(layout) = settings_layout(self.window_session.width(), self.settings_height())
        else {
            return;
        };
        let ambiguous = self.configuration.settings().editor.ambiguous_width;
        let Some(page) = self.ui.settings.as_mut() else {
            return;
        };
        let view = usize::from(layout.list_height);
        let height = |lines| settings_lines_height(page, lines, layout, ambiguous);
        let selected = page.selected.min(page.lines.len());
        let top = height(0..selected);
        let start = match page.selected.checked_sub(1).map(|index| page.lines[index]) {
            Some(SettingsLine::Heading(_)) => top - 2,
            _ => top,
        };
        let below = match (&page.edit, page.selected_row().map(|row| &row.value)) {
            (Some(SettingsEdit::Choice { .. }), Some(SettingsValue::Choice { choices, .. })) => {
                choices.len() + 2
            }
            _ => 0,
        };
        // The setting's title and description, without the blank row after it.
        let end = top + height(selected..selected + 1).saturating_sub(1) + below;
        let total = height(0..page.lines.len());
        if start < page.scroll {
            page.scroll = start;
        } else if end > page.scroll + view {
            page.scroll = (end - view).min(start);
        }
        page.scroll = page.scroll.min(total.saturating_sub(view));
    }

    pub(in crate::app) fn settings_action(&mut self, action: SettingsAction) {
        match action {
            SettingsAction::Open => return self.open_settings(),
            SettingsAction::Shortcuts => return self.open_shortcuts(),
            _ => {}
        }
        let Some(page) = self.ui.settings.as_mut() else {
            return;
        };
        match action {
            SettingsAction::Open | SettingsAction::Shortcuts => {}
            SettingsAction::Close => self.ui.settings = None,
            SettingsAction::Move(by) => {
                let count = choice_count(page);
                if let Some(SettingsEdit::Choice { selected }) = &mut page.edit {
                    *selected = offset(*selected, by, count);
                } else {
                    page.edit = None;
                    move_selection(page, by);
                }
            }
            SettingsAction::Section(by) => {
                page.edit = None;
                jump_section(page, by);
                self.scroll_settings_to_top();
                return;
            }
            SettingsAction::Activate | SettingsAction::Toggle => self.activate_setting(),
            SettingsAction::Reset if page.shortcuts => {
                page.edit = None;
                self.reset_shortcut();
            }
            SettingsAction::Reset => {
                page.edit = None;
                if let Some(row) = page.selected_row() {
                    if row.modified {
                        let key = row.key.clone();
                        let _ = self.save_setting(&key, None, false);
                    } else {
                        page.status = Some(("Already the default".into(), false));
                    }
                }
            }
            SettingsAction::Search => {
                page.edit = None;
                page.searching = true;
            }
            SettingsAction::Modified => {
                page.edit = None;
                page.query = if page
                    .query
                    .split_whitespace()
                    .any(|word| word == MODIFIED_FILTER)
                {
                    String::new()
                } else {
                    MODIFIED_FILTER.into()
                };
                refilter(page);
            }
            SettingsAction::OpenFile => {
                let path = if page.shortcuts {
                    crate::config::user_keymaps_path()
                } else {
                    crate::config::user_config_path()
                };
                self.ui.settings = None;
                self.process_effect(AppEffect::OpenUserFile(path));
            }
            SettingsAction::Cancel => {
                if page.edit.take().is_none() {
                    if page.searching || !page.query.is_empty() {
                        page.searching = false;
                        page.query.clear();
                        refilter(page);
                    } else {
                        self.ui.settings = None;
                    }
                }
            }
            SettingsAction::Insert(text) => {
                let text: String = text
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                if let Some(SettingsEdit::Text {
                    text: value,
                    cursor,
                    error,
                }) = &mut page.edit
                {
                    value.insert_str(*cursor, &text);
                    *cursor += text.len();
                    *error = None;
                } else if page.searching {
                    page.query.push_str(&text);
                    refilter(page);
                }
            }
            SettingsAction::Backspace => {
                if let Some(SettingsEdit::Text {
                    text,
                    cursor,
                    error,
                }) = &mut page.edit
                {
                    if let Some(previous) = text[..*cursor].chars().next_back() {
                        *cursor -= previous.len_utf8();
                        text.remove(*cursor);
                    }
                    *error = None;
                } else if page.searching {
                    page.query.pop();
                    refilter(page);
                }
            }
            SettingsAction::Cursor(to) => {
                if let Some(SettingsEdit::Text { text, cursor, .. }) = &mut page.edit {
                    *cursor = to.apply(text, *cursor);
                }
            }
            SettingsAction::Clear => {
                if let Some(SettingsEdit::Text {
                    text,
                    cursor,
                    error,
                }) = &mut page.edit
                {
                    text.clear();
                    *cursor = 0;
                    *error = None;
                } else if page.searching {
                    page.query.clear();
                    refilter(page);
                }
            }
        }
        self.scroll_settings();
    }

    /// Change the selected setting: flip it, open its dropdown or text field, or confirm the
    /// dropdown or text field that is open.
    fn activate_setting(&mut self) {
        let Some(page) = self.ui.settings.as_mut() else {
            return;
        };
        if page.searching && page.edit.is_none() {
            page.searching = false;
            return;
        }
        let Some(row) = page.selected_row().cloned() else {
            return;
        };
        match (page.edit.take(), &row.value) {
            (Some(SettingsEdit::Choice { selected }), SettingsValue::Choice { choices, .. }) => {
                if let Some(choice) = choices.get(selected) {
                    let empty = settings_catalog()
                        .setting(&row.key)
                        .and_then(|info| info.empty.as_ref())
                        .is_some_and(|empty| empty == choice);
                    let value = typed_like(&row.key, if empty { "" } else { choice });
                    let _ = self.save_setting(&row.key, Some(value), row.restart);
                }
            }
            (Some(SettingsEdit::Text { text, cursor, .. }), value) => {
                // A rejected value stays in the field with the reason, ready to correct.
                let saved = parsed_text(&row.key, value, &text)
                    .and_then(|value| self.save_setting(&row.key, value, row.restart));
                if let (Err(error), Some(page)) = (saved, self.ui.settings.as_mut()) {
                    page.status = None;
                    page.edit = Some(SettingsEdit::Text {
                        text,
                        cursor,
                        error: Some(error),
                    });
                }
            }
            (_, SettingsValue::Bool(on)) => {
                let _ = self.save_setting(&row.key, Some(toml::Value::Boolean(!on)), row.restart);
            }
            (_, SettingsValue::Choice { current, choices }) => {
                let selected = choices
                    .iter()
                    .position(|choice| choice == current)
                    .unwrap_or(0);
                page.edit = Some(SettingsEdit::Choice { selected });
            }
            (_, SettingsValue::Keys(_)) => page.edit = Some(SettingsEdit::Keys(Vec::new())),
            (_, SettingsValue::Theme(_)) => {
                let _ = self.process_configuration_effect(AppEffect::SelectTheme);
            }
            (
                _,
                SettingsValue::Number(text) | SettingsValue::Text(text) | SettingsValue::List(text),
            ) => {
                page.edit = Some(SettingsEdit::Text {
                    text: text.clone(),
                    cursor: text.len(),
                    error: None,
                });
            }
        }
    }

    /// Write one setting to config.toml and reload; the reload refreshes the page.
    pub(in crate::app) fn save_setting(
        &mut self,
        key: &str,
        value: Option<toml::Value>,
        restart: bool,
    ) -> Result<(), String> {
        if self.ui.settings.as_ref().is_some_and(|page| page.saving) {
            return Err("Configuration reload is still pending".into());
        }
        let result = match self.configuration.user_settings_path() {
            None => {
                Err("Dust started without a user configuration, so nothing can be saved".into())
            }
            Some(path) => write_user_setting(&path, key, value).and_then(|()| {
                self.configuration
                    .request_reload()
                    .map_err(|error| error.to_string())
            }),
        };
        let status = match &result {
            Ok(()) if restart => ("Saved. Restart Dust to apply it".into(), false),
            Ok(()) => ("Saved".into(), false),
            Err(error) => (error.clone(), true),
        };
        if let Some(page) = self.ui.settings.as_mut() {
            page.saving = result.is_ok();
            page.status = Some(status);
        }
        result
    }

    pub(in crate::app) fn settings_mouse(
        &mut self,
        key: &crate::editor::input::KeyEvent,
        column: u16,
        row: u16,
    ) {
        use crate::editor::input::TerminalMouseInput;
        let (width, height) = (self.window_session.width(), self.settings_height());
        let ambiguous = self.configuration.settings().editor.ambiguous_width;
        let Some(page) = self
            .ui
            .settings
            .as_mut()
            .filter(|page| page.setup.is_none())
        else {
            return;
        };
        match key.terminal_mouse() {
            Some(TerminalMouseInput::ScrollUp) => page.scroll = page.scroll.saturating_sub(3),
            Some(TerminalMouseInput::ScrollDown) => {
                if let Some(layout) = settings_layout(width, height) {
                    let total = settings_lines_height(page, 0..page.lines.len(), layout, ambiguous);
                    let view = usize::from(layout.list_height);
                    page.scroll = (page.scroll + 3).min(total.saturating_sub(view));
                }
            }
            Some(TerminalMouseInput::LeftDown) => {
                match settings_hit(page, (width, height), ambiguous, column, row) {
                    Some(SettingsHit::Choice(selected)) => {
                        page.edit = Some(SettingsEdit::Choice { selected });
                        self.activate_setting();
                        self.scroll_settings();
                    }
                    Some(SettingsHit::Border) => {}
                    Some(SettingsHit::Search) => {
                        page.edit = None;
                        page.searching = true;
                    }
                    Some(SettingsHit::Section(section)) => {
                        page.edit = None;
                        show_section(page, section);
                        self.scroll_settings_to_top();
                    }
                    Some(SettingsHit::Line(line))
                        if line == page.selected && page.edit.is_none() =>
                    {
                        self.activate_setting();
                        self.scroll_settings();
                    }
                    Some(SettingsHit::Line(line)) => {
                        page.edit = None;
                        page.searching = false;
                        page.selected = line;
                        self.scroll_settings();
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// After a section is picked, put its heading at the top of the list.
    fn scroll_settings_to_top(&mut self) {
        let ambiguous = self.configuration.settings().editor.ambiguous_width;
        let layout = settings_layout(self.window_session.width(), self.settings_height());
        if let (Some(page), Some(layout)) = (self.ui.settings.as_mut(), layout) {
            let heading = page.selected.saturating_sub(1).min(page.lines.len());
            page.scroll = settings_lines_height(page, 0..heading, layout, ambiguous);
        }
        self.scroll_settings();
    }
}

/// The row a value kind and current value come from: the catalog's choices, the bundled
/// default's TOML type, or the theme in use.
fn setting_value(info: &SettingInfo, current: Option<&toml::Value>, theme: &str) -> SettingsValue {
    if info.key == "ui.theme" {
        return SettingsValue::Theme(theme.into());
    }
    let value = current
        .and_then(|current| lookup(current, &info.key))
        .or_else(|| default_setting(&info.key));
    let text = value.as_ref().map_or_else(String::new, plain_text);
    let mut choices: Vec<String> = match &info.choices_from {
        Some(table) => current
            .and_then(|current| lookup(current, table))
            .and_then(|value| {
                value
                    .as_table()
                    .map(|table| table.keys().cloned().collect())
            })
            .unwrap_or_default(),
        None => info.choices.clone(),
    };
    if let Some(empty) = &info.empty {
        choices.insert(0, empty.clone());
    }
    if !choices.is_empty() {
        let current = match &info.empty {
            Some(empty) if text.is_empty() => empty.clone(),
            _ => text,
        };
        return SettingsValue::Choice { current, choices };
    }
    match default_setting(&info.key).or(value) {
        Some(toml::Value::Boolean(on)) => SettingsValue::Bool(
            current
                .and_then(|current| lookup(current, &info.key))
                .map_or(on, |value| value.as_bool().unwrap_or(on)),
        ),
        Some(toml::Value::Integer(_) | toml::Value::Float(_)) => SettingsValue::Number(text),
        Some(toml::Value::Array(_)) => SettingsValue::List(text),
        _ => SettingsValue::Text(text),
    }
}

pub(super) fn lookup(value: &toml::Value, key: &str) -> Option<toml::Value> {
    key.split('.')
        .try_fold(value, |value, segment| value.get(segment))
        .cloned()
}

fn plain_text(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Array(items) => toml::Value::Array(items.clone()).to_string(),
        other => other.to_string(),
    }
}

/// A dropdown choice as the TOML type the setting has: most are strings, a few are numbers.
fn typed_like(key: &str, choice: &str) -> toml::Value {
    match default_setting(key) {
        Some(toml::Value::Integer(_)) => choice
            .parse()
            .map_or_else(|_| toml::Value::String(choice.into()), toml::Value::Integer),
        _ => toml::Value::String(choice.into()),
    }
}

/// What typed text means for a setting; `None` removes an optional setting left empty.
fn parsed_text(
    key: &str,
    value: &SettingsValue,
    text: &str,
) -> Result<Option<toml::Value>, String> {
    let text = text.trim();
    if text.is_empty() && default_setting(key).is_none() {
        return Ok(None);
    }
    Ok(Some(match value {
        SettingsValue::Number(_) => match default_setting(key) {
            Some(toml::Value::Float(_)) => {
                toml::Value::Float(text.parse().map_err(|_| "Enter a number".to_owned())?)
            }
            _ => match text.parse::<i64>() {
                Ok(number) if number < 0 => return Err("Enter 0 or more".into()),
                Ok(number) => toml::Value::Integer(number),
                Err(_) => return Err("Enter a whole number".into()),
            },
        },
        SettingsValue::List(_) => {
            let value = format!("value = {text}")
                .parse::<toml::Table>()
                .map_err(|_| {
                    "Enter a list of quoted values, such as [\"src/**\", \"tests/**\"]".to_owned()
                })?
                .remove("value")
                .ok_or("Enter a list of quoted values")?;
            if !value
                .as_array()
                .is_some_and(|items| items.iter().all(toml::Value::is_str))
            {
                return Err("Enter a list of quoted values".into());
            }
            value
        }
        // Keys are checked here because a bad one only fails when the keymaps are rebuilt.
        _ if matches!(key, "input.keymaps.leader" | "input.keymaps.local_leader") => {
            crate::app::configuration::literal_key_sequence(key, text).map_err(|_| {
                "Enter a key such as space or , (up to 4, separated by spaces)".to_owned()
            })?;
            toml::Value::String(text.into())
        }
        _ => toml::Value::String(text.into()),
    }))
}

fn choice_count(page: &SettingsPageState) -> usize {
    match page.selected_row().map(|row| &row.value) {
        Some(SettingsValue::Choice { choices, .. }) => choices.len(),
        _ => 0,
    }
}

fn offset(index: usize, by: i64, count: usize) -> usize {
    let moved = i64::try_from(index).unwrap_or(i64::MAX).saturating_add(by);
    usize::try_from(moved.max(0))
        .unwrap_or(0)
        .min(count.saturating_sub(1))
}

fn first_setting(page: &SettingsPageState) -> usize {
    page.lines
        .iter()
        .position(|line| matches!(line, SettingsLine::Setting(_)))
        .unwrap_or(0)
}

fn setting_lines(page: &SettingsPageState) -> Vec<usize> {
    page.lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| matches!(line, SettingsLine::Setting(_)).then_some(index))
        .collect()
}

fn move_selection(page: &mut SettingsPageState, by: i64) {
    let settings = setting_lines(page);
    let position = settings
        .iter()
        .position(|line| *line == page.selected)
        .unwrap_or(0);
    if let Some(line) = settings.get(offset(position, by, settings.len())) {
        page.selected = *line;
    }
}

/// The first setting of the next or previous section on screen.
fn jump_section(page: &mut SettingsPageState, by: i64) {
    let starts: Vec<usize> = page
        .lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| matches!(line, SettingsLine::Heading(_)).then_some(index + 1))
        .filter(|index| *index < page.lines.len())
        .collect();
    let current = starts
        .iter()
        .rposition(|start| *start <= page.selected)
        .unwrap_or(0);
    if let Some(line) = starts.get(offset(current, by, starts.len())) {
        page.selected = *line;
    }
}

/// Select the first setting of a section, clearing a search that hides it.
fn show_section(page: &mut SettingsPageState, section: usize) {
    let find = |page: &SettingsPageState| {
        page.lines
            .iter()
            .position(|line| matches!(line, SettingsLine::Heading(index) if *index == section))
    };
    if find(page).is_none() {
        page.query.clear();
        page.searching = false;
        filter_lines(page);
    }
    if let Some(heading) = find(page) {
        page.selected = (heading + 1).min(page.lines.len().saturating_sub(1));
    }
}

fn refilter(page: &mut SettingsPageState) {
    filter_lines(page);
    page.selected = first_setting(page);
    page.scroll = 0;
}

/// Lay out the settings the search matches, each section under its heading. Every word must
/// appear in the title, key, description or section name; `@modified` keeps only the
/// settings the user changed. The section of favourites is left out while searching so a
/// match is not listed twice.
fn filter_lines(page: &mut SettingsPageState) {
    let query = page.query.to_lowercase();
    let modified_only = query.split_whitespace().any(|word| word == MODIFIED_FILTER);
    let words: Vec<&str> = query
        .split_whitespace()
        .filter(|word| *word != MODIFIED_FILTER)
        .collect();
    let catalog = settings_catalog();
    page.lines.clear();
    let mut section = None;
    for (index, row) in page.rows.iter().enumerate() {
        let favourites = !page.shortcuts
            && catalog
                .sections
                .get(row.section)
                .is_some_and(|section| !section.common.is_empty());
        if (favourites && !query.trim().is_empty()) || (modified_only && !row.modified) {
            continue;
        }
        let keys = match &row.value {
            SettingsValue::Keys(keys) => keys.as_str(),
            _ => "",
        };
        let haystack = format!(
            "{} {} {} {} {keys}",
            row.title,
            row.key,
            row.about,
            page.sections.get(row.section).map_or("", String::as_str)
        )
        .to_lowercase();
        if !words.iter().all(|word| haystack.contains(word)) {
            continue;
        }
        if section != Some(row.section) {
            page.lines.push(SettingsLine::Heading(row.section));
            section = Some(row.section);
        }
        page.lines.push(SettingsLine::Setting(index));
    }
}

#[cfg(test)]
mod tests {
    use crate::app::settings::*;

    #[test]
    fn list_text_round_trips_commas_quotes_and_empty_values() {
        let value = toml::Value::Array(
            ["reports/2026,final/**", "a\"b", "", "back\\slash"]
                .into_iter()
                .map(|value| toml::Value::String(value.into()))
                .collect(),
        );
        let text = plain_text(&value);
        assert_eq!(
            parsed_text(
                "workspace.search.exclude_globs",
                &SettingsValue::List(text.clone()),
                &text
            )
            .unwrap(),
            Some(value)
        );
        assert!(
            parsed_text(
                "workspace.search.exclude_globs",
                &SettingsValue::List(String::new()),
                "[unterminated"
            )
            .is_err()
        );
    }
}

pub(in crate::app) mod setup;
pub(in crate::app) mod shortcuts;

pub(in crate::app) mod commands;
