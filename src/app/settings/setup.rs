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
                self.set_dashboard_visibility(true);
                // Answers only live in SetupState until Finish. Mark the questions as seen
                // without writing any draft answers or touching an existing configuration.
                let result = self
                    .configuration
                    .user_settings_path()
                    .filter(|path| !path.exists())
                    .map_or(Ok(()), |path| create_user_file(&path));
                match result {
                    Ok(()) => self.notify_message("Setup skipped. :setup asks again".into(), false),
                    Err(error) => self.notify_message(
                        format!(
                            "Setup skipped, but could not remember it: {error}. :setup asks again"
                        ),
                        true,
                    ),
                }
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
                self.set_dashboard_visibility(true);
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

#[cfg(test)]
mod tests {
    use crate::app::commands::SetupAction;
    use crate::app::tests::driver::TestSession;

    #[test]
    fn every_setup_page_shows_skip_keys_and_keeps_the_six_questions() {
        let mut session = TestSession::new(crate::config::load_bundled_config());
        session.application_mut().setup_action(SetupAction::Open);
        let titles: Vec<_> = session
            .application()
            .ui
            .settings
            .as_ref()
            .unwrap()
            .setup
            .as_ref()
            .unwrap()
            .pages
            .iter()
            .map(|page| page.title.as_str())
            .collect();
        assert_eq!(
            titles,
            [
                "Layout",
                "Terminal",
                "AI agent",
                "Agent",
                "Agent tools",
                "Leader key"
            ]
        );
        for page in 0..6 {
            for (width, height) in [(80, 24), (120, 40), (160, 45)] {
                session.resize(width, height);
                let frame = session.frame();
                let text = (0..height)
                    .map(|y| frame.grid.row_text(y))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    text.contains("s / Esc · Start editing with defaults"),
                    "{text}"
                );
                assert!(
                    text.contains(if page == 5 {
                        "enter finish"
                    } else {
                        "enter next"
                    }),
                    "{text}"
                );
            }
            if page < 5 {
                session.key("enter");
            }
        }
        session.resize(80, 8);
        let frame = session.frame();
        assert!(
            frame
                .grid
                .row_text(4)
                .contains("s / Esc · Start editing with defaults")
        );
    }

    #[test]
    fn setup_keeps_every_choice_and_the_complete_question_visible() {
        for (width, height) in [(80, 24), (100, 30), (160, 45)] {
            let mut session = TestSession::new(crate::config::load_bundled_config());
            session.resize(width, height);
            session.application_mut().setup_action(SetupAction::Open);
            for page in 0..6 {
                let choices = session
                    .application()
                    .ui
                    .settings
                    .as_ref()
                    .unwrap()
                    .setup
                    .as_ref()
                    .unwrap()
                    .pages[page]
                    .choices
                    .clone();
                for (index, choice) in choices.iter().enumerate() {
                    session
                        .application_mut()
                        .ui
                        .settings
                        .as_mut()
                        .unwrap()
                        .setup
                        .as_mut()
                        .unwrap()
                        .picked[page] = index;
                    let frame = session.frame();
                    assert!(
                        (0..height).any(|y| {
                            frame.grid.row_text(y).contains(&choice.label)
                                && (0..width).any(|x| {
                                    frame.grid.get(x, y).unwrap().style
                                        == crate::ui::Style::Selection
                                })
                        }),
                        "{} missing at {width}x{height}",
                        choice.label
                    );
                    let question = session
                        .application()
                        .ui
                        .settings
                        .as_ref()
                        .unwrap()
                        .setup
                        .as_ref()
                        .unwrap()
                        .pages[page]
                        .question
                        .clone();
                    let heading = (0..height)
                        .map(|y| {
                            (0..width)
                                .filter_map(|x| {
                                    let cell = frame.grid.get(x, y).unwrap();
                                    (cell.style
                                        == crate::ui::Style::Syntax(
                                            crate::config::Highlight::HEADING,
                                        ))
                                    .then_some(cell.symbol.as_str())
                                })
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert_eq!(
                        heading.split_whitespace().collect::<Vec<_>>().join(" "),
                        question
                    );
                }
                if page < 5 {
                    session.key("enter");
                }
            }
        }
    }

    #[test]
    fn skip_discards_answers_remembers_first_run_and_offers_the_tutor() {
        // Isolate user paths from other tests and from the developer's configuration.
        if std::env::var_os("DUST_TEST_SETUP_SKIP").is_none() {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::settings::setup::tests::skip_discards_answers_remembers_first_run_and_offers_the_tutor", "--nocapture"])
                .env("DUST_TEST_SETUP_SKIP", "1")
                .env("DUST_CONFIG_HOME", root.path().join("config"))
                .env("DUST_STATE_HOME", root.path().join("state"))
                .output().unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
                "{output:?}"
            );
            return;
        }
        let path = crate::config::user_config_path();
        for key in ["s", "escape"] {
            for page in 0..6 {
                let mut session = TestSession::new(crate::config::load_user_config());
                session.resize(120, 40);
                let initial =
                    toml::Value::try_from(session.application().configuration.settings()).unwrap();
                session.new_buffer();
                let buffer = session.application().state.active_buffer_id().unwrap();
                session.application_mut().open_setup_on_first_start();
                assert_eq!(session.application_mut().active_input_context(), "setup");
                // Change an answer on every page visited, but never Finish.
                for _ in 0..page {
                    session.key("j");
                    session.key("enter");
                }
                session.key("j");
                assert!(!path.exists(), "draft answers must not create config.toml");
                session.key(key);
                assert!(session.application().ui.settings.is_none());
                assert!(session.application().state.buffers.contains_key(buffer));
                assert_eq!(
                    session.application_mut().active_input_context(),
                    "dashboard"
                );
                assert_eq!(
                    toml::Value::try_from(session.application().configuration.settings()).unwrap(),
                    initial
                );
                let source = std::fs::read_to_string(&path).unwrap();
                let saved: toml::Value = toml::from_str(&source).unwrap();
                assert!(saved.as_table().unwrap().is_empty(), "{source}");
                session.application_mut().open_setup_on_first_start();
                assert!(session.application().ui.settings.is_none());
                assert!(session.application().ui.tutor.is_none());
                assert!(session.application().ui.picker.is_none());
                let frame = session.frame();
                let text = (0..40)
                    .map(|y| frame.grid.row_text(y))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(text.contains("Tutor — learn by editing"), "{text}");
                session.key("t");
                assert!(
                    session.application().ui.picker.is_some(),
                    "t offers tutor languages without forcing one"
                );
                session.application_mut().setup_action(SetupAction::Open);
                let setup = session
                    .application()
                    .ui
                    .settings
                    .as_ref()
                    .unwrap()
                    .setup
                    .as_ref()
                    .unwrap();
                assert_eq!(setup.page, 0);
                assert_eq!(setup.picked[0], 0, "skip discarded the layout answer");
                session.key("s");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
                drop(session);
                std::fs::remove_file(&path).unwrap();
            }
        }

        // Finish still saves the selected answers; revisiting and skipping leaves them alone.
        let mut session = TestSession::new(crate::config::load_user_config());
        session.new_buffer();
        session.application_mut().open_setup_on_first_start();
        session.key("j");
        for _ in 0..6 {
            session.key("enter");
        }
        assert_eq!(
            session.application_mut().active_input_context(),
            "dashboard"
        );
        assert!(session.application().ui.tutor.is_none());
        let source = std::fs::read_to_string(&path).unwrap();
        let saved: toml::Value = toml::from_str(&source).unwrap();
        assert_eq!(saved["ui"]["explorer"]["side"].as_str(), Some("right"));
        // Exercise the public :setup alias as well as the owner action.
        for key in [":", "s", "e", "t", "u", "p", "enter"] {
            session.key(key);
        }
        assert_eq!(session.application_mut().active_input_context(), "setup");
        session.key("j");
        session.key("escape");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        session.key("t");
        assert!(session.application().ui.picker.is_some());

        std::fs::write(&path, "").unwrap();
        session.application_mut().setup_action(SetupAction::Open);
        session.key("s");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        // An unwritable config location must not prevent editing or claim it was remembered.
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(path.parent().unwrap()).unwrap();
        std::fs::write(path.parent().unwrap(), "not a directory").unwrap();
        session.application_mut().setup_action(SetupAction::Open);
        session.key("s");
        assert_eq!(
            session.application_mut().active_input_context(),
            "dashboard"
        );
        assert!(session.application().ui.settings.is_none());
        assert!(
            session
                .notification()
                .unwrap()
                .contains("could not remember it")
        );
        assert!(session.application().ui.notification_error);
    }
}
