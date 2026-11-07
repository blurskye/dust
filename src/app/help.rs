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
        self.sync_picker_ui();
    }
}

impl App {
    /// Contextual help: what is available on the focused surface, read from the command
    /// registry through the dispatcher's own context check, so help can never advertise a
    /// command the dispatcher would refuse and can never drift from a renamed command.
    pub(in crate::app) fn open_contextual_help(&mut self) {
        let invocation = crate::editor::CommandInvocation::new(
            "overlay.help.contextual",
            crate::editor::InvocationSource::Keymap,
        )
        .expect("static command id");
        let items = self
            .commands
            .specs()
            .filter(|spec| spec.context.satisfied(&self.state, &invocation))
            .enumerate()
            .map(|(index, spec)| {
                let id = PickerItemId(format!("help:{index}:{}", spec.id.as_str()));
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: spec.id.as_str().to_owned(),
                        detail: spec.title.clone(),
                        source: format!("{:?}", spec.context).to_lowercase(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(spec.id.as_str().to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Help · {} commands available here", items.len());
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
