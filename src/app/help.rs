//! Help pages and command documentation.

use crate::app::App;
use crate::app::picker::PickerContent;
use crate::app::picker::PickerOrigin;
use crate::app::picker::PickerPayload;
use crate::app::search::MAXIMUM_PICKER_ITEMS;
use crate::ui::PickerSurfaceKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

impl App {
    /// Show a help page without losing the surface the user was looking at. Help replaces the
    /// picker while it is open, and closing it returns the suspended surface with its query,
    /// its rows and its selected row exactly as they were.
    fn install_help_page(&mut self, title: String, items: Vec<(PickerItem, PickerPayload)>) {
        self.search.suspend_for_help();
        let result = self.search.install_picker(
            PickerContent {
                surface: PickerSurfaceKind::Generic,
                origin: PickerOrigin::General,
                title,
                items,
                query: String::new(),
                complete: true,
            },
            MAXIMUM_PICKER_ITEMS,
            self.workspace.version(),
            self.configuration.binding_revision(),
        );
        if let Err(error) = result {
            self.show_error(error);
            return;
        }
        self.search.mark_help_open();
        self.project_input_prompt();
        self.sync_picker_ui();
    }
}

impl App {
    /// Teach the focused surface's effective keys, including user remaps and descriptions.
    pub(in crate::app) fn open_contextual_help(&mut self) {
        if self.input.prompt().is_some() {
            self.open_form_help();
            return;
        }
        let context = self.active_input_context();
        let mut bindings = self
            .configuration
            .keymaps()
            .reachable_bindings()
            .filter(|binding| binding.context == context)
            .collect::<Vec<_>>();
        // Show the surface's short keys before shared leader sequences.
        bindings.sort_by(|left, right| {
            left.keys
                .split_whitespace()
                .count()
                .cmp(&right.keys.split_whitespace().count())
                .then_with(|| left.keys.to_lowercase().cmp(&right.keys.to_lowercase()))
                .then_with(|| left.keys.cmp(&right.keys))
        });
        let items = bindings
            .into_iter()
            .map(|binding| {
                let command = binding.command.as_deref().unwrap_or_default();
                let description = binding
                    .desc
                    .as_ref()
                    .or_else(|| self.configuration.settings().command_labels.get(command))
                    .cloned()
                    .or_else(|| {
                        self.commands
                            .specs()
                            .find(|spec| spec.id.as_str() == command)
                            .map(|spec| spec.title.clone())
                    })
                    .unwrap_or_else(|| command.to_owned());
                let arguments = binding
                    .arguments
                    .iter()
                    .map(|(name, value)| {
                        let value = match value {
                            crate::editor::CommandValue::String(text) => text.clone(),
                            crate::editor::CommandValue::Bool(value) => value.to_string(),
                            crate::editor::CommandValue::Integer(value) => value.to_string(),
                            value => format!("{value:?}"),
                        };
                        format!("{name}={value}")
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let id = PickerItemId(format!("help:{}", binding.id));
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: format!("{}  {description}", binding.keys),
                        detail: format!("{command} {arguments}").trim_end().to_owned(),
                        source: context.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(command.to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Help · {context} · {} keys", items.len());
        self.install_help_page(title, items);
    }
}

impl App {
    /// Help for the form or confirmation in front of the user. It names the surface by its own
    /// title and message, so the help is about the question actually being asked, and it leaves
    /// that question untouched: opening help is not answering it.
    pub(in crate::app) fn open_form_help(&mut self) {
        let Some(prompt) = self.input.prompt() else {
            self.show_error("no form or confirmation is open");
            return;
        };
        let kind = format!("{:?}", prompt.kind);
        let mut rows = vec![("surface".to_owned(), kind.clone())];
        if let Some(confirmation) = prompt.confirmation.as_ref() {
            rows.push(("asks".to_owned(), confirmation.title.clone()));
            rows.push(("detail".to_owned(), confirmation.message.clone()));
            rows.push((
                "choices".to_owned(),
                format!(
                    "{} / {}",
                    confirmation.positive_label, confirmation.negative_label
                ),
            ));
            if !confirmation.hint.is_empty() {
                rows.push(("keys".to_owned(), confirmation.hint.clone()));
            }
        } else if !prompt.input.is_empty() {
            rows.push(("draft".to_owned(), prompt.input.clone()));
        }
        // The commands available on this surface come from the registry through the dispatcher's
        // own context check, so the help can never list a command this form would refuse.
        let invocation = crate::editor::CommandInvocation::new(
            "overlay.help.form",
            crate::editor::InvocationSource::Keymap,
        )
        .expect("static command id");
        for spec in self
            .commands
            .specs()
            .filter(|spec| spec.context.satisfied(&self.state, &invocation))
        {
            if spec.id.as_str() == "overlay.help.form" {
                continue;
            }
            rows.push(("available".to_owned(), spec.id.as_str().to_owned()));
        }
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, (kind, value))| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("form-help:{index}:{kind}")),
                        label: value,
                        detail: kind,
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command("overlay.help.form".to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Help · {kind}");
        self.install_help_page(title, items);
    }
}

impl App {
    /// The reference for the selected command, assembled from the registry entry itself: its
    /// title, its description, where it is available and every argument the schema declares,
    /// marked required or optional. Nothing here is a second copy of the truth.
    pub(in crate::app) fn open_command_reference(&mut self) {
        let Some(PickerPayload::Command(command)) = self.search.selected_picker_payload().cloned()
        else {
            self.show_error("select a command first");
            return;
        };
        let Some(spec) = self
            .commands
            .specs()
            .find(|spec| spec.id.as_str() == command)
        else {
            self.show_error("that command is no longer registered");
            return;
        };
        let mut rows = vec![
            ("command".to_owned(), spec.id.as_str().to_owned()),
            ("does".to_owned(), spec.title.clone()),
        ];
        if !spec.description.is_empty() {
            rows.push(("detail".to_owned(), spec.description.clone()));
        }
        rows.push((
            "available".to_owned(),
            format!("{:?}", spec.context).to_lowercase(),
        ));
        // The effective binding, read from the same resolved table the dispatcher uses, so the
        // reference can never show a key the user cannot press: a remapped key is the key shown,
        // an unmapped one is not offered, and a command with none here says so.
        for (label, context) in [("binding", "editor.normal"), ("binding here", "picker")] {
            let effective = self
                .configuration
                .keymaps()
                .effective_binding(context, spec.id.as_str());
            if effective.is_bound() || label == "binding" {
                rows.push((label.to_owned(), effective.display()));
            }
        }
        for (name, field) in &spec.arguments.fields {
            rows.push((
                if field.required {
                    "argument"
                } else {
                    "optional"
                }
                .to_owned(),
                name.clone(),
            ));
        }
        if spec.arguments.fields.is_empty() {
            rows.push(("takes".to_owned(), "no arguments".to_owned()));
        }
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, (kind, value))| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("reference:{index}:{kind}")),
                        label: value,
                        detail: kind,
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(spec.id.as_str().to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Help · {}", spec.id.as_str());
        self.install_help_page(title, items);
    }
}

use crate::app::commands::AppEffect;
use crate::app::commands::register;
use crate::editor::ArgumentSchema;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "overlay.help.contextual",
        "Show what is available on the focused surface",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::HelpContextual),
    )?;
    register(
        registry,
        "overlay.help.form",
        "Show help for the form or confirmation in front of you",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::HelpForForm),
    )?;
    register(
        registry,
        "overlay.help.reference",
        "Show the reference for the selected command",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::HelpCommandReference),
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_help_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::HelpForForm => self.open_form_help(),
            AppEffect::HelpContextual => self.open_contextual_help(),
            AppEffect::HelpCommandReference => self.open_command_reference(),
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::app;
    use crate::editor::input::{KeymapSequence, MappingResolution};

    #[test]
    fn explorer_help_teaches_keys_and_returns_without_running_the_selected_command() {
        let (_dirs, mut app) = app();
        app.open_explorer();
        app.handle_configured_key("?".parse().unwrap());
        assert!(app.search.help_is_open());
        let picker = app.search.picker().unwrap().presentation();
        assert!(picker.rows.iter().any(|row| {
            row.label.starts_with("r  ") && row.detail.contains("explorer.rename.prompt")
        }));
        assert!(picker.rows.iter().all(|row| row.source == "explorer"));
        app.picker_insert("rename");
        assert!(!app.search.picker().unwrap().presentation().rows.is_empty());
        app.accept_picker();
        assert!(!app.search.help_is_open());
        assert_eq!(app.active_input_context(), "explorer");
        assert!(app.input.prompt().is_none());
    }

    #[test]
    fn surface_help_is_bound_only_outside_text_input_and_existing_search_keys() {
        let (_dirs, app) = app();
        for context in [
            "explorer",
            "terminal_normal",
            "terminal_visual",
            "settings",
            "setup",
            "tool_manager",
            "agent_normal",
        ] {
            let resolution = app.configuration.keymaps().resolve(
                &mut KeymapSequence::default(),
                context,
                "?".parse().unwrap(),
            );
            assert!(
                matches!(resolution, MappingResolution::Ready(command)
                if command.id.as_str() == "overlay.help.contextual"),
                "{context}"
            );
        }
        for context in [
            "terminal",
            "editor.insert",
            "explorer_search",
            "settings_search",
            "tool_manager_search",
            "picker",
        ] {
            assert!(
                app.configuration
                    .keymaps()
                    .inspect_key(context, "?")
                    .is_none(),
                "{context}"
            );
        }
        for context in ["editor.normal", "dashboard"] {
            assert_ne!(
                app.configuration
                    .keymaps()
                    .inspect_key(context, "?")
                    .unwrap()
                    .command
                    .as_deref(),
                Some("overlay.help.contextual")
            );
        }
    }

    #[test]
    fn terminal_help_preserves_selection_mode_and_restores_focus() {
        let (_dirs, mut app) = app();
        app.state
            .open_text(crate::editor::text::TextBuffer::from_text("editor"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        let id = crate::process::PtyId(1);
        app.terminal_session.fixture(
            id,
            crate::app::terminal::TerminalFixture {
                profile: "shell".into(),
                screen: crate::ui::PtyScreen::new(80, 8, 100),
                running: true,
                exit_code: None,
                truncated: false,
            },
            app.workspace.version(),
        );
        app.terminal_session.select(id);
        app.set_terminal_focus(true);
        app.sync_terminal_panel();
        app.enter_terminal_normal_mode();
        for context in ["terminal_normal", "terminal_visual"] {
            if context == "terminal_visual" {
                app.handle_configured_key("v".parse().unwrap());
            }
            assert_eq!(app.active_input_context(), context);
            let mode = app.terminal_session.mode(id);
            app.handle_configured_key("?".parse().unwrap());
            assert!(app.search.help_is_open());
            assert!(
                app.search
                    .picker()
                    .unwrap()
                    .presentation()
                    .rows
                    .iter()
                    .all(|row| row.source == context)
            );
            app.handle_configured_key("ctrl-c".parse().unwrap());
            assert_eq!(app.active_input_context(), context);
            assert_eq!(app.terminal_session.mode(id), mode);
        }
    }

    #[test]
    fn settings_help_returns_to_the_filtered_shortcuts_page() {
        let (_dirs, mut app) = app();
        app.open_shortcuts();
        app.ui.settings.as_mut().unwrap().query = "terminal visual".into();
        app.refresh_settings();
        let selected = app.ui.settings.as_ref().unwrap().selected;
        app.handle_configured_key("?".parse().unwrap());
        assert!(app.search.help_is_open());
        app.handle_configured_key("ctrl-c".parse().unwrap());
        assert_eq!(app.active_input_context(), "settings");
        let page = app.ui.settings.as_ref().unwrap();
        assert_eq!(page.query, "terminal visual");
        assert_eq!(page.selected, selected);
    }

    #[test]
    fn help_uses_remapped_keys_and_binding_descriptions() {
        let (_dirs, mut app) = app();
        let mut loaded = crate::config::load_bundled_config().keymaps;
        let mut binding = loaded
            .entries
            .iter()
            .find(|entry| {
                entry.context.as_deref() == Some("explorer") && entry.keys.as_deref() == Some("r")
            })
            .unwrap()
            .clone();
        binding.keys = Some("z".into());
        binding.desc = Some("Rename this entry".into());
        loaded.entries.push(binding);
        let (keymaps, error) = crate::app::configuration::compile_keymaps(
            &app.commands,
            &loaded,
            &app.configuration.settings().input.keymaps,
        );
        assert!(error.is_none(), "{error:?}");
        app.configuration.install_bindings(keymaps, false);
        app.open_explorer();
        app.open_contextual_help();
        let items = app.search.picker().unwrap().presentation().rows;
        assert!(items.iter().any(|row| row.label == "z  Rename this entry"));
        assert!(!items.iter().any(|row| row.label.starts_with("r  ")));
    }
}
