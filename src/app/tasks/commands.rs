//! Command registration and execution for task execution and output.

use crate::app::App;
use crate::app::TrustedAction;
use crate::app::commands::AppEffect;
use crate::app::tasks::AdapterContext;

use crate::app::commands::SyntaxFoldAction;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use crate::ui::PromptKind;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "format.run",
        "Run configured formatter",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::FormatRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "lint.run",
        "Run configured linter",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LintRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "lint.clear",
        "Clear linter diagnostics",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LintClear(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    for (id, description, initial) in [
        ("test.run.prompt", "Run test by name", "test.run name="),
        (
            "test.list.prompt",
            "Show test results by name",
            "test.list name=",
        ),
        ("test.stop.prompt", "Stop test by name", "test.stop name="),
    ] {
        register(
            registry,
            id,
            description,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| AppEffect::OpenPrompt {
                kind: PromptKind::Command,
                initial: initial.to_owned(),
            },
        )?;
    }
    register(
        registry,
        "test.run",
        "Run configured test",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "test.stop",
        "Stop test run",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestStop(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "test.list",
        "List test results",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestList(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "syntax.status",
        "Show built-in syntax language status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxStatus,
    )?;
    for (id, name, action) in [
        (
            "syntax.fold.toggle",
            "Toggle fold",
            SyntaxFoldAction::Toggle,
        ),
        ("syntax.fold.open", "Open fold", SyntaxFoldAction::Open),
        ("syntax.fold.close", "Close fold", SyntaxFoldAction::Close),
        (
            "syntax.fold.open_all",
            "Open all folds",
            SyntaxFoldAction::OpenAll,
        ),
        (
            "syntax.fold.close_all",
            "Close all folds",
            SyntaxFoldAction::CloseAll,
        ),
    ] {
        register(
            registry,
            id,
            name,
            ContextRequirement::View,
            ArgumentSchema::default(),
            move |_| AppEffect::SyntaxFold(action),
        )?;
    }
    register(
        registry,
        "syntax.indent.line",
        "Indent current line from built-in syntax structure",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxIndentLine,
    )?;
    register(
        registry,
        "task.list",
        "List task status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TaskList,
    )?;
    register(
        registry,
        "task.restart",
        "Restart task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskRestart(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "task.run",
        "Run configured task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "task.stop",
        "Stop running task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskStop(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "task.output",
        "Show task output",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskOutput(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    Ok(())
}

use crate::workspace::DiagnosticOwner;

impl App {
    pub(in crate::app) fn process_tasks_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::TestRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                // Without a matcher there are no per-test rows, but the run and its exit code stand.
                match task.matcher.clone() {
                    Some(matcher) => self.run_trusted(TrustedAction::RunTest { name, matcher }),
                    None => self.run_trusted(TrustedAction::RunTask(name)),
                }
            }
            AppEffect::TestStop(name) => self.stop_task(&name),
            AppEffect::TestList(name) => self.show_test_picker(&name),
            AppEffect::TaskList => self.show_task_picker(),
            AppEffect::TaskRestart(name) => {
                let adapter = self.tasks.last_adapter(&name).cloned();
                // A finished task has nothing to stop: restarting just runs it again.
                if self.tasks.is_running(&name) {
                    self.stop_task(&name);
                }
                match adapter {
                    Some(AdapterContext::Test { matcher }) => {
                        self.run_trusted(TrustedAction::RunTest { name, matcher })
                    }
                    Some(AdapterContext::Linter {
                        matcher,
                        lint: true,
                    }) => self.run_trusted(TrustedAction::RunLinter { name, matcher }),
                    Some(AdapterContext::Formatter { source }) => {
                        match crate::app::tasks::FormatterSource::capture(
                            &self.state,
                            source.buffer(),
                        ) {
                            Ok(source) => {
                                self.run_trusted(TrustedAction::RunFormatter { name, source })
                            }
                            Err(error) => self.show_error(error),
                        }
                    }
                    _ => self.run_trusted(TrustedAction::RunTask(name)),
                }
            }
            AppEffect::TaskRun(name) => self.run_trusted(TrustedAction::RunTask(name)),
            AppEffect::TaskStop(name) => self.stop_task(&name),
            AppEffect::TaskOutput(name) => self.show_output_picker(&name),
            AppEffect::LintRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("linter_matcher_missing", &[]));
                    return Ok(());
                };
                self.run_trusted(TrustedAction::RunLinter { name, matcher });
            }
            AppEffect::LintClear(name) => {
                if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                    self.diagnostics.clear_owner(&owner);
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::FormatRun(name) => {
                let Some(view_id) = self.state.active_view() else {
                    return Ok(());
                };
                let Some(view) = self.state.views.get(view_id) else {
                    return Ok(());
                };
                let buffer = view.buffer;
                match crate::app::tasks::FormatterSource::capture(&self.state, buffer) {
                    Ok(source) => self.run_trusted(TrustedAction::RunFormatter { name, source }),
                    Err(error) => self.show_error(error),
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tasks::commands::*;
    use crate::editor::CommandInvocation;
    use crate::editor::EditorState;
    use crate::editor::InvocationSource;

    #[test]
    fn test_prompts_use_stable_command_ids() {
        let root = tempfile::tempdir().unwrap();
        let state = EditorState::new(root.path().into());
        let mut registry = CommandRegistry::default();
        register_commands(&mut registry).unwrap();
        for command in ["test.run", "test.list", "test.stop"] {
            let invocation =
                CommandInvocation::new(&format!("{command}.prompt"), InvocationSource::Test)
                    .unwrap();
            let outcome = registry.execute(&state, &invocation).unwrap();
            assert!(
                matches!(outcome.effects.as_slice(), [AppEffect::OpenPrompt { kind: PromptKind::Command, initial }] if initial == &format!("{command} name="))
            );
        }
    }
}
