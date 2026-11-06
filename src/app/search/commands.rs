
use crate::app::commands::AppEffect;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use crate::app::commands::register;
use crate::app::commands::bool_arg;

pub(in crate::app) fn register_commands(registry: &mut CommandRegistry<AppEffect>) -> Result<(), CommandError> {

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
