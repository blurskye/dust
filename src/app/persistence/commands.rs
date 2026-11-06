//! Command registration and execution for saved editor state and session restoration.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::persistence::MARK_LIMIT;

use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "navigation.mark.set",
        "Set navigation mark",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| Ok(AppEffect::MarkSet(string_arg(invocation, "name")?.into())),
    )?;
    register(
        registry,
        "navigation.mark.jump",
        "Jump to navigation mark",
        ContextRequirement::Application,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| Ok(AppEffect::MarkJump(string_arg(invocation, "name")?.into())),
    )?;
    for (id, description, effect) in [
        (
            "session.tutor.next",
            "Skip to the next tutor step",
            AppEffect::TutorNext,
        ),
        (
            "session.tutor.back",
            "Go back to the previous tutor step",
            AppEffect::TutorBack,
        ),
        (
            "session.tutor.quit",
            "Stop the tutor; :Tutor picks up where it stopped",
            AppEffect::TutorQuit,
        ),
        (
            "navigation.jump.back",
            "Jump back to where the cursor was",
            AppEffect::WalkJumps(true),
        ),
        (
            "navigation.jump.forward",
            "Jump forward again",
            AppEffect::WalkJumps(false),
        ),
    ] {
        register(
            registry,
            id,
            description,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_invocation| Ok(effect.clone()),
        )?;
    }
    register(
        registry,
        "session.save",
        "Save session state",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::SessionSave,
    )?;
    register(
        registry,
        "session.load",
        "Load session state",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::SessionLoad,
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_persistence_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::HistoryClear => {
                self.input.clear_history();
                self.notify_message(self.message("history_cleared", &[]), false);
            }
            AppEffect::SessionSave => {
                if self.configuration.settings().persistence.enabled {
                    self.persistence.request_save_notice();
                    self.write_persistence();
                } else {
                    self.show_error(self.message("session_disabled", &[]));
                }
            }
            AppEffect::SessionLoad => self.load_session_on_request(),
            AppEffect::MarkJump(name) => {
                self.persistence.reconcile_marks(&self.state);
                let location = self
                    .persistence
                    .marks()
                    .iter()
                    .find(|mark| mark.name == name)
                    .map(|mark| mark.location.clone());
                match location {
                    Some(location) => self.navigate_persisted_location(location),
                    None => self.show_error(self.message("mark_unset", &[("name", name)])),
                }
            }
            AppEffect::MarkSet(name) => {
                if let Some(location) = self.current_persisted_location() {
                    self.persistence
                        .set_mark(name, location, MARK_LIMIT, &self.state);
                    self.persistence.reconcile_marks(&self.state);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
