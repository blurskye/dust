//! Command registration and execution for workspace roots and trust.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::workspace::WorkspaceTrust;

use crate::app::commands::integer_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use crate::ui::PromptKind;
use std::path::PathBuf;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "workspace.root.add",
        "Add workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("path", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceRootAdd(PathBuf::from(string_arg(
                invocation, "path",
            )?)))
        },
    )?;
    register(
        registry,
        "workspace.root.activate",
        "Activate workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            let id = u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                CommandError::InvalidArguments("root id must be non-negative".into())
            })?;
            Ok(AppEffect::WorkspaceRootActivate(id))
        },
    )?;
    register(
        registry,
        "workspace.root.remove",
        "Remove workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            let id = u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                CommandError::InvalidArguments("root id must be non-negative".into())
            })?;
            Ok(AppEffect::WorkspaceRootRemove(id))
        },
    )?;
    register(
        registry,
        "workspace.root.status",
        "Show active workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceRootStatus,
    )?;
    register(
        registry,
        "workspace.search.prompt",
        "Open workspace search prompt",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceSearchPrompt,
    )?;
    register(
        registry,
        "workspace.replace.prompt",
        "Open workspace replacement prompt",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenPrompt {
            kind: PromptKind::ReplaceFind,
            initial: String::new(),
        },
    )?;
    register(
        registry,
        "workspace.search",
        "Search workspace",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("query", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceSearch(
                string_arg(invocation, "query")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "workspace.search.todo",
        "Search TODO, FIXME, HACK and NOTE comments",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TodoSearch,
    )?;
    register(
        registry,
        "workspace.search.cancel",
        "Cancel workspace search",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceSearchCancel,
    )?;
    register(
        registry,
        "workspace.replace",
        "Replace across workspace",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("query", ArgumentType::String, true)
            .field("replacement", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceReplace {
                query: string_arg(invocation, "query")?.into(),
                replacement: string_arg(invocation, "replacement")?.into(),
            })
        },
    )?;
    register(
        registry,
        "workspace.file.find",
        "Open file",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenWorkspaceFilePicker,
    )?;
    for (id, title, effect) in [
        (
            "workspace.trust",
            "Trust workspace",
            AppEffect::WorkspaceTrust,
        ),
        (
            "workspace.untrust",
            "Revoke workspace trust",
            AppEffect::WorkspaceUntrust,
        ),
        (
            "workspace.trust.status",
            "Show workspace trust",
            AppEffect::WorkspaceStatus,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "history.clear",
        "Clear command and search history",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HistoryClear,
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_workspace_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::WorkspaceTrust => self.request_workspace_trust(),
            AppEffect::WorkspaceUntrust => self.revoke_workspace_trust(),
            AppEffect::WorkspaceStatus => {
                let state = if self.workspace.trust() == WorkspaceTrust::Trusted {
                    self.message("workspace_trust_trusted", &[])
                } else {
                    self.message("workspace_trust_untrusted", &[])
                };
                self.notify_message(
                    self.message("workspace_trust_status", &[("state", state)]),
                    false,
                );
            }
            AppEffect::WorkspaceRootAdd(path) => self.add_workspace_root(path),
            AppEffect::WorkspaceRootActivate(id) => {
                self.activate_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootRemove(id) => {
                self.remove_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootStatus => {
                if let Some(root) = self
                    .workspace
                    .active_root()
                    .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                {
                    self.notify_message(
                        self.message(
                            "workspace_root_status",
                            &[
                                ("id", root.id.0.to_string()),
                                ("path", root.identity.canonical_path.display().to_string()),
                            ],
                        ),
                        false,
                    );
                }
            }
            AppEffect::SafeModeStatus => {
                self.notify_message(
                    self.message(
                        "safe_mode_status",
                        &[(
                            "state",
                            if self.workspace.safe_mode() {
                                "active".into()
                            } else {
                                "inactive".into()
                            },
                        )],
                    ),
                    false,
                );
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
