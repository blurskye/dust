//! The first-start setup: a few questions from `assets/defaults/setup.toml` about layout,
//! terminal, agent and leader key, asked once when there is no config.toml yet. The answers
//! are written to config.toml like Settings page changes, and `:setup` asks again.

use crate::app::App;
use crate::app::commands::SetupAction;
use crate::app::documents::create_user_file;
use crate::app::settings::lookup;
use crate::config::SetupChoiceInfo;
use crate::config::setup_catalog;
use crate::config::write_user_setting;
use crate::ui::SetupChoice;
use crate::ui::SetupPage;
use crate::ui::SetupState;

impl App {
    /// Ask the setup questions when this is the first start, that is, config.toml is missing.
    pub(in crate::app) fn open_setup_on_first_start(&mut self) {
        if self
            .configuration
            .user_settings_path()
            .is_some_and(|path| !path.exists())
        {
            self.open_setup();
        }
    }

    fn open_setup(&mut self) {
        let current = toml::Value::try_from(self.configuration.settings()).ok();
        let pages: Vec<SetupPage> = setup_catalog()
            .pages
            .iter()
            .map(|info| {
                let choices = match (&info.choices_from, &info.key) {
                    // A page whose choices are the entries of a table, such as agent profiles.
                    (Some(table), Some(key)) => current
                        .as_ref()
                        .and_then(|current| lookup(current, table))
                        .and_then(|value| value.as_table().cloned())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(name, entry)| SetupChoice {
                            about: entry
                                .get("program")
                                .and_then(toml::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            settings: vec![(key.clone(), toml::Value::String(name.clone()))],
                            label: name,
                        })
                        .chain(info.choices.iter().map(setup_choice))
                        .collect(),
                    _ => info.choices.iter().map(setup_choice).collect(),
                };
                SetupPage {
                    title: info.title.clone(),
                    question: info.question.clone(),
                    preview: info.preview.clone(),
                    choices,
                }
            })
            .filter(|page| !page.choices.is_empty())
            .collect();
        // Each page starts on the choice that matches the settings in use.
        let picked = pages
            .iter()
            .map(|page| {
                page.choices
                    .iter()
                    .position(|choice| {
                        choice.settings.iter().all(|(key, value)| {
                            current
                                .as_ref()
                                .and_then(|current| lookup(current, key))
                                .as_ref()
                                == Some(value)
                        })
                    })
                    .unwrap_or(0)
            })
            .collect();
        self.open_settings();
        if let Some(page) = self.ui.settings.as_mut() {
            page.status = None;
            page.setup = Some(SetupState {
                pages,
                page: 0,
                picked,
            });
        }
    }

    pub(in crate::app) fn setup_action(&mut self, action: SetupAction) {
        if matches!(action, SetupAction::Open) {
            self.open_setup();
            return;
        }
        let Some(setup) = self
            .ui
            .settings
            .as_mut()
            .and_then(|page| page.setup.as_mut())
        else {
            return;
        };
        match action {
            SetupAction::Open => {}
            SetupAction::Move(by) => {
                let count = setup
                    .pages
                    .get(setup.page)
                    .map_or(0, |page| page.choices.len());
                if let Some(picked) = setup.picked.get_mut(setup.page) {
                    let moved = i64::try_from(*picked).unwrap_or(0).saturating_add(by);
                    *picked = usize::try_from(moved.max(0))
                        .unwrap_or(0)
                        .min(count.saturating_sub(1));
                }
            }
            SetupAction::Next if setup.page + 1 < setup.pages.len() => setup.page += 1,
            SetupAction::Next => self.finish_setup(),
            SetupAction::Back => setup.page = setup.page.saturating_sub(1),
            SetupAction::Skip => {
                self.ui.settings = None;
                // The file marks that the questions were seen, so they aren't asked again.
                if let Some(path) = self.configuration.user_settings_path() {
                    let _ = create_user_file(&path);
                }
                self.notify_message("Setup skipped. :setup asks again".into(), false);
            }
        }
    }

    /// Write the picked choices to config.toml, leaving out what already has that value.
    fn finish_setup(&mut self) {
        let Some(setup) = self
            .ui
            .settings
            .as_ref()
            .and_then(|page| page.setup.as_ref())
        else {
            return;
        };
        let current = toml::Value::try_from(self.configuration.settings()).ok();
        let changes: Vec<(String, toml::Value)> = setup
            .pages
            .iter()
            .zip(&setup.picked)
            .filter_map(|(page, picked)| page.choices.get(*picked))
            .flat_map(|choice| choice.settings.iter().cloned())
            .filter(|(key, value)| {
                current
                    .as_ref()
                    .and_then(|current| lookup(current, key))
                    .as_ref()
                    != Some(value)
            })
            .collect();
        let result = match self.configuration.user_settings_path() {
            None => {
                Err("Dust started without a user configuration, so nothing can be saved".into())
            }
            Some(path) => create_user_file(&path)
                .map_err(|error| format!("{}: {error}", path.display()))
                .and_then(|()| {
                    changes
                        .into_iter()
                        .try_for_each(|(key, value)| write_user_setting(&path, &key, Some(value)))
                })
                .and_then(|()| {
                    self.configuration
                        .request_reload()
                        .map_err(|error| error.to_string())
                }),
        };
        match result {
            Ok(()) => {
                self.ui.settings = None;
                self.notify_message(
                    "Setup saved. :setup asks again, and Settings (ctrl-,) has every option".into(),
                    false,
                );
            }
            Err(error) => self.settings_status(error, true),
        }
    }
}

fn setup_choice(choice: &SetupChoiceInfo) -> SetupChoice {
    SetupChoice {
        label: choice.label.clone(),
        about: choice.about.clone(),
        settings: choice
            .settings
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    }
}
