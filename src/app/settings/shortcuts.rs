//! The Keyboard Shortcuts page: the Settings page listing every key binding, one section per
//! context. Enter records new keys for the selected binding, which are written to the user's
//! keymaps.toml under the binding's id so they replace the default, and the normal
//! configuration reload applies them and refreshes the page.

use std::collections::HashMap;

use crate::app::App;
use crate::app::documents::create_user_file;
use crate::config::DeclaredKeymap;
use crate::config::declared_keymaps;
use crate::config::remove_user_keymap;
use crate::config::write_user_keymap;
use crate::editor::input::KeyEvent;
use crate::ui::SettingsEdit;
use crate::ui::SettingsPageState;
use crate::ui::SettingsRow;
use crate::ui::SettingsValue;

/// Mouse and terminal lifecycle events are not keyboard shortcuts.
fn listed(context: &str) -> bool {
    !(context.ends_with("_mouse") || matches!(context, "focus" | "key_release"))
}

/// A binding longer than this many keys can't be typed in, as in keymaps.toml.
const MAXIMUM_KEYS: usize = 4;

/// The settings listed first, under "Leader keys": the keys `<leader>` and `<localleader>`
/// stand for in every binding.
const LEADERS: [(&str, &str, &str); 2] = [
    (
        "input.keymaps.leader",
        "Leader",
        "Starts the shortcuts written <leader>, such as <leader> f f",
    ),
    (
        "input.keymaps.local_leader",
        "Local leader",
        "Starts the shortcuts written <localleader>, for the file type at hand",
    ),
];

fn is_leader(key: &str) -> bool {
    LEADERS.iter().any(|(setting, ..)| *setting == key)
}

impl App {
    pub(in crate::app) fn open_shortcuts(&mut self) {
        self.open_settings();
        if let Some(page) = self.ui.settings.as_mut() {
            *page = SettingsPageState {
                shortcuts: true,
                ..SettingsPageState::default()
            };
        }
        self.refresh_settings();
    }

    fn declared_shortcuts(&self) -> Vec<DeclaredKeymap> {
        let user = self
            .configuration
            .user_settings_path()
            .map(|_| crate::config::user_keymaps_path());
        declared_keymaps(user.as_deref())
            .into_iter()
            .filter(|keymap| {
                keymap.entry.command.is_some()
                    && keymap.entry.keys.is_some()
                    && keymap.entry.context.as_deref().is_some_and(listed)
            })
            .collect()
    }

    /// The page's sections and rows: the leader keys, then each binding in effect under its
    /// context and mode with its keys as keymaps.toml writes them, and the other bindings there
    /// with the same keys.
    pub(in crate::app) fn shortcut_rows(&self) -> (Vec<String>, Vec<SettingsRow>) {
        let settings = self.configuration.settings();
        let policy = &settings.input.keymaps;
        let user_keys = self.configuration.user_setting_keys();
        let mut rows: Vec<SettingsRow> = LEADERS
            .iter()
            .zip([&policy.leader, &policy.local_leader])
            .map(|((setting, title, about), keys)| SettingsRow {
                key: (*setting).into(),
                section: 0,
                title: (*title).into(),
                about: (*about).into(),
                value: SettingsValue::Keys(keys.clone()),
                modified: user_keys.contains(*setting),
                restart: false,
            })
            .collect();
        let declared = self.declared_shortcuts();
        let mut found = Vec::<(String, String)>::new();
        let mut section_of = HashMap::<(String, String), usize>::new();
        let mut bindings = Vec::new();
        for keymap in &declared {
            let entry = &keymap.entry;
            let place = (
                entry.context.clone().unwrap_or_default(),
                entry.mode.clone(),
            );
            let section = *section_of.entry(place.clone()).or_insert_with(|| {
                found.push(place);
                found.len() - 1
            });
            let command = entry.command.clone().unwrap_or_default();
            let mut title = entry
                .desc
                .as_ref()
                .or_else(|| settings.command_labels.get(&command))
                .cloned()
                .or_else(|| {
                    self.commands
                        .specs()
                        .find(|spec| spec.id.as_str() == command)
                        .map(|spec| spec.title.clone())
                })
                .unwrap_or_else(|| command.clone());
            // What the arguments say, as in "Move: down, linewise".
            let arguments: Vec<String> = entry
                .arguments
                .iter()
                .filter_map(|(name, value)| match value {
                    toml::Value::String(text) => Some(text.replace('_', " ")),
                    toml::Value::Boolean(true) => Some(name.replace('_', " ")),
                    toml::Value::Boolean(false) => None,
                    value => Some(format!("{} {value}", name.replace('_', " "))),
                })
                .collect();
            if !arguments.is_empty() {
                title = format!("{title}: {}", arguments.join(", "));
            }
            let keys = entry.keys.as_deref().unwrap_or_default();
            bindings.push(SettingsRow {
                key: entry.id.clone(),
                section,
                title,
                about: command,
                value: SettingsValue::Keys(keys.split_whitespace().collect::<Vec<_>>().join(" ")),
                modified: keymap.user,
                restart: false,
            });
        }
        // Everywhere first, then the editor's modes in the order they are used, then the rest
        // by name; each section's bindings together under its heading.
        let titles: Vec<String> = found
            .iter()
            .map(|(context, mode)| section_title(context, mode))
            .collect();
        let mut order: Vec<usize> = (0..found.len()).collect();
        order.sort_by(|a, b| {
            section_rank(&found[*a])
                .cmp(&section_rank(&found[*b]))
                .then_with(|| titles[*a].cmp(&titles[*b]))
        });
        let mut position = vec![0; found.len()];
        for (index, section) in order.iter().enumerate() {
            position[*section] = index + 1;
        }
        for row in &mut bindings {
            row.section = position[row.section];
        }
        bindings.sort_by_key(|row| row.section);
        rows.extend(bindings);
        let sections = std::iter::once("Leader keys".to_owned())
            .chain(order.iter().map(|section| titles[*section].clone()))
            .collect();
        // Same keys in the same section: only one of them runs, so both say so.
        let mut by_keys = HashMap::<(usize, String), Vec<usize>>::new();
        for (index, row) in rows.iter().enumerate() {
            if let SettingsValue::Keys(keys) = &row.value {
                by_keys
                    .entry((row.section, keys.clone()))
                    .or_default()
                    .push(index);
            }
        }
        for indexes in by_keys.values().filter(|indexes| indexes.len() > 1) {
            for &index in indexes {
                let others: Vec<String> = indexes
                    .iter()
                    .filter(|other| **other != index)
                    .map(|other| rows[*other].title.clone())
                    .collect();
                let about = format!("{} · same keys as {}", rows[index].about, others.join(", "));
                rows[index].about = about;
            }
        }
        (sections, rows)
    }

    /// While new keys are being recorded every key goes to them: Enter saves, Escape cancels
    /// and Backspace takes back the last key. Returns whether the key was used.
    pub(in crate::app) fn record_shortcut_key(&mut self, key: &KeyEvent) -> bool {
        let Some(page) = self.ui.settings.as_mut() else {
            return false;
        };
        let leader_row = page.selected_row().is_some_and(|row| is_leader(&row.key));
        let leader = self.configuration.settings().input.keymaps.leader.clone();
        let Some(SettingsEdit::Keys(keys)) = &mut page.edit else {
            return false;
        };
        match key.to_string().as_str() {
            "enter" if keys.is_empty() => {}
            "enter" => {
                let keys = keys.clone();
                self.save_shortcut(&keys);
            }
            "escape" => page.edit = None,
            "backspace" => {
                keys.pop();
            }
            // A binding that starts with the leader key is written `<leader>`, so it follows
            // the leader when that changes.
            notation if keys.is_empty() && !leader_row && notation == leader.as_str() => {
                keys.push("<leader>".into());
            }
            notation if keys.len() < MAXIMUM_KEYS => keys.push(notation.to_owned()),
            _ => {}
        }
        true
    }

    fn save_shortcut(&mut self, keys: &[String]) {
        let Some(row) = self
            .ui
            .settings
            .as_ref()
            .and_then(SettingsPageState::selected_row)
            .cloned()
        else {
            return;
        };
        if is_leader(&row.key) {
            if let [key] = keys {
                let _ = self.save_setting(&row.key, Some(toml::Value::String(key.clone())), false);
                if let Some(page) = self.ui.settings.as_mut() {
                    page.edit = None;
                }
            } else {
                self.settings_status("A leader is one key".into(), true);
            }
            return;
        }
        let Some(keymap) = self
            .declared_shortcuts()
            .into_iter()
            .find(|keymap| keymap.entry.id == row.key)
        else {
            self.settings_status(
                "That binding is gone; the page has been refreshed".into(),
                true,
            );
            self.refresh_settings();
            return;
        };
        let source = keys.join(" ");
        let clash = self.ui.settings.as_ref().and_then(|page| {
            page.rows
                .iter()
                .find(|other| {
                    other.section == row.section
                        && other.key != row.key
                        && other.value == SettingsValue::Keys(source.clone())
                })
                .map(|other| other.title.clone())
        });
        let result = self.write_shortcuts(|path| write_user_keymap(path, &keymap.entry, &source));
        let status = match (&result, clash) {
            (Ok(()), Some(other)) => (format!("Saved. {source} is also {other}"), false),
            (Ok(()), None) => ("Saved".into(), false),
            (Err(error), _) => (error.clone(), true),
        };
        if let Some(page) = self.ui.settings.as_mut() {
            page.edit = None;
            page.saving = result.is_ok();
            page.status = Some(status);
        }
    }

    pub(in crate::app) fn reset_shortcut(&mut self) {
        let Some(row) = self
            .ui
            .settings
            .as_ref()
            .and_then(SettingsPageState::selected_row)
            .cloned()
        else {
            return;
        };
        if !row.modified {
            self.settings_status("Already the default".into(), false);
            return;
        }
        if is_leader(&row.key) {
            let _ = self.save_setting(&row.key, None, false);
            return;
        }
        let result = self.write_shortcuts(|path| remove_user_keymap(path, &row.key));
        if let Some(page) = self.ui.settings.as_mut() {
            page.saving = result.is_ok();
            page.status = Some(match result {
                Ok(()) => ("Reset".into(), false),
                Err(error) => (error, true),
            });
        }
    }

    /// Change keymaps.toml, creating it first, and reload so the change applies.
    fn write_shortcuts(
        &mut self,
        write: impl FnOnce(&std::path::Path) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.ui.settings.as_ref().is_some_and(|page| page.saving) {
            return Err("Configuration reload is still pending".into());
        }
        if self.configuration.user_settings_path().is_none() {
            return Err(
                "Dust started without a user configuration, so nothing can be saved".into(),
            );
        }
        let path = crate::config::user_keymaps_path();
        create_user_file(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        write(&path)?;
        self.configuration
            .request_reload()
            .map_err(|error| error.to_string())
    }
}

fn section_rank((context, mode): &(String, String)) -> (u8, usize) {
    const EDITOR_MODES: [&str; 8] = [
        "normal",
        "insert",
        "visual_char",
        "visual_line",
        "visual_block",
        "operator_pending",
        "replace",
        "multi",
    ];
    match context.as_str() {
        "global" => (0, 0),
        "editor" => (
            1,
            EDITOR_MODES
                .iter()
                .position(|known| known == mode)
                .unwrap_or(EDITOR_MODES.len()),
        ),
        _ => (2, 0),
    }
}

fn section_title(context: &str, mode: &str) -> String {
    let context = match context {
        "global" => "Everywhere".to_owned(),
        context => {
            let words = context.replace('_', " ");
            let mut characters = words.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        }
    };
    if mode.is_empty() {
        context
    } else {
        format!("{context} · {}", mode.replace('_', " "))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::app;

    #[test]
    fn shortcuts_include_every_keyboard_context_and_binding_descriptions() {
        let (_dirs, mut app) = app();
        for key in ["space", "s", "k"] {
            app.handle_configured_key(key.parse().unwrap());
        }
        assert!(app.ui.settings.as_ref().unwrap().shortcuts);
        let (sections, rows) = app.shortcut_rows();
        let declared = crate::config::declared_keymaps(None);
        for binding in declared.iter().filter(|binding| {
            binding.entry.command.is_some()
                && binding.entry.keys.is_some()
                && binding.entry.context.as_deref().is_some_and(super::listed)
        }) {
            let row = rows
                .iter()
                .find(|row| row.key == binding.entry.id)
                .unwrap_or_else(|| panic!("missing {}", binding.entry.id));
            assert!(!row.title.is_empty());
            if let Some(description) = &binding.entry.desc {
                assert!(row.title.starts_with(description), "{}", row.key);
            }
        }
        for context in ["settings", "setup", "terminal_visual", "editor"] {
            let binding = declared
                .iter()
                .find(|binding| binding.entry.context.as_deref() == Some(context))
                .unwrap();
            assert!(
                rows.iter().any(|row| row.key == binding.entry.id),
                "{context}"
            );
        }
        assert!(
            sections
                .iter()
                .any(|section| section.to_lowercase().contains("insert"))
        );
        app.ui.settings.as_mut().unwrap().query = "terminal visual".into();
        app.refresh_settings();
        let page = app.ui.settings.as_ref().unwrap();
        assert!(
            page.lines
                .iter()
                .any(|line| matches!(line, crate::ui::SettingsLine::Setting(_)))
        );
    }
}
