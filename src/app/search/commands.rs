//! Command registration and execution for buffer and workspace search.

use crate::app::commands::AppEffect;
use crate::app::commands::bool_arg;
use crate::app::commands::register;
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
        "workspace.search.resume",
        "Resume workspace search",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceSearchResume,
    )?;
    register(
        registry,
        "search.word",
        "Search word under cursor",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("forward", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::SearchWord {
                forward: bool_arg(invocation, "forward")?,
                count: invocation.count.map_or(1, |count| count.get()),
            })
        },
    )?;
    register(
        registry,
        "search.select_next",
        "Select the next search match",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |invocation| {
            Ok(AppEffect::SearchSelectNext(
                invocation.count.map_or(1, |count| count.get()),
            ))
        },
    )?;
    register(
        registry,
        "search.repeat",
        "Repeat search",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("reverse", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::SearchRepeat {
                reverse: bool_arg(invocation, "reverse")?,
                count: invocation.count.map_or(1, |count| count.get()),
            })
        },
    )?;
    Ok(())
}

use crate::app::App;

impl App {
    pub(in crate::app) fn process_search_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::WorkspaceSearchPrompt => self.open_live_workspace_search(),
            AppEffect::WorkspaceSearchResume => self.resume_workspace_search(),
            AppEffect::WorkspaceSearch(query) => self.start_workspace_search(query, None),
            AppEffect::WorkspaceSearchCancel => {
                self.search.cancel_grep();
                self.notify_message(self.message("workspace_search_cancelled", &[]), false);
            }
            AppEffect::WorkspaceReplace { query, replacement } => {
                self.start_workspace_search(query, Some(replacement));
            }
            AppEffect::TodoSearch => self.start_todo_search(),
            AppEffect::SearchRepeat { .. }
            | AppEffect::SearchWord { .. }
            | AppEffect::SearchSelectNext(_) => {
                unreachable!("search effects return their status through process_effect_checked")
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
