//! The Keyboard Shortcuts page: the Settings page listing every key binding, one section per
//! context. Enter records new keys for the selected binding, which are written to the user's
//! keymaps.toml under the binding's id so they replace the default, and the normal
//! configuration reload applies them and refreshes the page.

use std::collections::HashMap;

use super::InteractiveApplication;
use super::KeyEvent;
use super::effects::create_user_file;
use crate::config::{DeclaredKeymap, declared_keymaps, remove_user_keymap, write_user_keymap};
use crate::ui::{SettingsEdit, SettingsPageState, SettingsRow, SettingsValue};

/// Contexts whose bindings are the inner workings of a page or the mouse, not shortcuts.
fn listed(context: &str) -> bool {
    !(context.ends_with("_mouse")
        || context.starts_with("settings")
        || matches!(context, "setup" | "focus" | "key_release"))
}

/// A binding longer than this many keys can't be typed in, as in keymaps.toml.
const MAXIMUM_KEYS: usize = 4;

impl InteractiveApplication {
    pub(super) fn open_shortcuts(&mut self) {
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

    /// The page's sections and rows: each binding in effect under its context and mode, its
    /// keys as they are pressed, and the other bindings there with the same keys.
    pub(super) fn shortcut_rows(&self) -> (Vec<String>, Vec<SettingsRow>) {
        let settings = self.configuration.settings();
        let policy = &settings.input.keymaps;
        let shown_keys = |keys: &str| {
            keys.split_whitespace()
                .map(|token| match token {
                    "<leader>" => policy.leader.as_str(),
                    "<localleader>" => policy.local_leader.as_str(),
                    token => token,
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let declared = self.declared_shortcuts();
        let mut sections = Vec::<String>::new();
        let mut section_of = HashMap::<(String, String), usize>::new();
        let mut rows = Vec::new();
        for keymap in &declared {
            let entry = &keymap.entry;
            let context = entry.context.clone().unwrap_or_default();
            let section = *section_of
                .entry((context.clone(), entry.mode.clone()))
                .or_insert_with(|| {
                    sections.push(section_title(&context, &entry.mode));
                    sections.len() - 1
                });
            let command = entry.command.clone().unwrap_or_default();
            let mut title = settings
                .command_labels
                .get(&command)
                .cloned()
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
            rows.push(SettingsRow {
                key: entry.id.clone(),
                section,
                title,
                about: command,
                value: SettingsValue::Keys(shown_keys(entry.keys.as_deref().unwrap_or_default())),
                modified: keymap.user,
                restart: false,
            });
        }
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
    pub(super) fn record_shortcut_key(&mut self, key: &KeyEvent) -> bool {
        let Some(page) = self.ui.settings.as_mut() else {
            return false;
        };
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
        // Starting with the leader key writes `<leader>`, so the binding follows the leader.
        let leader = &self.configuration.settings().input.keymaps.leader;
        let source = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                if index == 0 && key == leader {
                    "<leader>"
                } else {
                    key.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let shown = keys.join(" ");
        let clash = self.ui.settings.as_ref().and_then(|page| {
            page.rows
                .iter()
                .find(|other| {
                    other.section == row.section
                        && other.key != row.key
                        && other.value == SettingsValue::Keys(shown.clone())
                })
                .map(|other| other.title.clone())
        });
        let result = self.write_shortcuts(|path| write_user_keymap(path, &keymap.entry, &source));
        let status = match (&result, clash) {
            (Ok(()), Some(other)) => (format!("Saved. {shown} is also {other}"), false),
            (Ok(()), None) => ("Saved".into(), false),
            (Err(error), _) => (error.clone(), true),
        };
        if let Some(page) = self.ui.settings.as_mut() {
            page.edit = None;
            page.saving = result.is_ok();
            page.status = Some(status);
        }
    }

    pub(super) fn reset_shortcut(&mut self) {
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
