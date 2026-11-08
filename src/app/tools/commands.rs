//! Command registration and execution for tool installation and runtime selection.

use crate::app::App;
use crate::app::TrustedAction;
use crate::app::commands::AppEffect;
use crate::tools::PackageOperation;
use crate::tools::RegistryRequest;

use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use std::path::PathBuf;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, update) in [
        ("package.install", "Install verified package", false),
        ("package.update", "Update verified package", true),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default()
                .field("package_manifest", ArgumentType::String, true)
                .field("plugin_manifest", ArgumentType::String, true)
                .field("component", ArgumentType::String, true),
            move |invocation| {
                Ok(AppEffect::PackageInstall {
                    package_manifest: PathBuf::from(string_arg(invocation, "package_manifest")?),
                    plugin_manifest: PathBuf::from(string_arg(invocation, "plugin_manifest")?),
                    component: PathBuf::from(string_arg(invocation, "component")?),
                    update,
                })
            },
        )?;
    }
    register(
        registry,
        "package.registry.search",
        "Search official package registry",
        ContextRequirement::Application,
        ArgumentSchema::default().field("query", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PackageRegistrySearch(
                string_arg(invocation, "query")?.into(),
            ))
        },
    )?;
    for (id, title, install) in [
        (
            "package.registry.show",
            "Show exact registry package",
            false,
        ),
        (
            "package.registry.install",
            "Install exact registry package",
            true,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default()
                .field("publisher", ArgumentType::String, true)
                .field("name", ArgumentType::String, true)
                .field("version", ArgumentType::String, true),
            move |invocation| {
                let publisher = string_arg(invocation, "publisher")?.into();
                let name = string_arg(invocation, "name")?.into();
                let version = string_arg(invocation, "version")?.into();
                Ok(if install {
                    AppEffect::PackageRegistryInstall {
                        publisher,
                        name,
                        version,
                    }
                } else {
                    AppEffect::PackageRegistryShow {
                        publisher,
                        name,
                        version,
                    }
                })
            },
        )?;
    }
    register(
        registry,
        "package.list",
        "List installed packages",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageList,
    )?;
    register(
        registry,
        "package.verify",
        "Verify package store",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageVerify,
    )?;
    register(
        registry,
        "package.rollback",
        "Rollback package lock",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageRollback,
    )?;
    register(
        registry,
        "package.remove",
        "Remove installed package",
        ContextRequirement::Application,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PackageRemove(
                string_arg(invocation, "name")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "package.gc",
        "Collect unreferenced package objects",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageGarbageCollect,
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_tools_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::PackageInstall {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => {
                if self.workspace.safe_mode() {
                    self.show_error(self.message("safe_mode_blocked", &[]));
                } else {
                    self.run_trusted(TrustedAction::InstallPackage {
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    });
                }
            }
            AppEffect::PackageRegistrySearch(query) => {
                self.request_registry(RegistryRequest::Search { query }, false)
            }
            AppEffect::PackageRegistryShow {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                false,
            ),
            AppEffect::PackageRegistryInstall {
                publisher,
                name,
                version,
            } => {
                if self.workspace.safe_mode() {
                    self.show_error(self.message("safe_mode_blocked", &[]))
                } else {
                    self.run_trusted(TrustedAction::InstallRegistry {
                        publisher,
                        name,
                        version,
                    })
                }
            }
            AppEffect::PackageList => {
                if let Err(error) = self.extension.request_package(PackageOperation::List) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageVerify => {
                if let Err(error) = self.extension.request_package(PackageOperation::Verify) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRollback => {
                if let Err(error) = self.extension.request_package(PackageOperation::Rollback) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRemove(name) => {
                if let Err(error) = self
                    .extension
                    .request_package(PackageOperation::Remove(name))
                {
                    self.show_error(error)
                }
            }
            AppEffect::PackageGarbageCollect => {
                if let Err(error) = self
                    .extension
                    .request_package(PackageOperation::GarbageCollect { maximum: 256 })
                {
                    self.show_error(error)
                }
            }
            AppEffect::OpenToolMenu(category) => self.open_tool_menu(category),
            AppEffect::CloseToolMenu
                if self
                    .ui
                    .tool_menu
                    .as_ref()
                    .is_some_and(|menu| !menu.query.is_empty()) =>
            {
                self.search_tool_menu(None, false);
            }
            AppEffect::CloseToolMenu => self.close_tool_menu(),
            AppEffect::ToolSearch => self.search_tool_menu(None, true),
            AppEffect::ToolSearchInsert(text) => {
                let query = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map(|menu| format!("{}{text}", menu.query));
                self.search_tool_menu(query, true);
            }
            AppEffect::ToolSearchDelete => {
                let query = self.ui.tool_menu.as_ref().map(|menu| {
                    let mut query = menu.query.clone();
                    query.pop();
                    query
                });
                self.search_tool_menu(query, true);
            }
            AppEffect::ToolSearchStop { keep } => {
                let query = self.ui.tool_menu.as_ref().map(|menu| menu.query.clone());
                self.search_tool_menu(query.filter(|_| keep), false);
            }
            AppEffect::MoveToolMenu(direction) => self.move_tool_menu(direction),
            AppEffect::CycleToolCategory(direction) => self.cycle_tool_category(direction),
            AppEffect::RefreshTools => self.refresh_tools(),
            AppEffect::ActivateTool => self.activate_selected_tool(),
            AppEffect::InstallSelectedTool => self.install_selected_tool(),
            AppEffect::UninstallSelectedTool => self.uninstall_selected_tool(),
            AppEffect::StopSelectedLsp => self.stop_selected_lsp(),
            AppEffect::RestartSelectedLsp => self.restart_selected_lsp(),
            AppEffect::ToggleToolRuntime => self.toggle_selected_lsp_runtime(),
            effect => return Err(effect),
        }
        Ok(())
    }
}
