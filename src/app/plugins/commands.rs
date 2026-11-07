//! Command registration and execution for plugin commands and application effects.

use crate::app::App;
use crate::app::commands::AppEffect;

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
        "plugin.disable",
        "Disable plugin",
        ContextRequirement::Application,
        ArgumentSchema::default().field("id", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PluginDisable(
                string_arg(invocation, "id")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "plugin.validate",
        "Validate configured plugins",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PluginValidate,
    )?;
    register(
        registry,
        "plugin.list",
        "List plugin status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PluginList,
    )?;
    Ok(())
}

use crate::app::plugins::PluginUiInteraction;

impl App {
    pub(in crate::app) fn process_plugins_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::PluginValidate => self.load_configured_plugins(),
            AppEffect::PluginList => self.open_plugin_browser(),
            AppEffect::PluginDisable(id) => {
                if !self.extension.statuses().contains_key(&id) {
                    self.show_error(self.message("plugin_unknown", &[("id", id)]));
                    return Ok(());
                }
                self.disable_browser_plugin(&id);
                self.notify_message(self.message("plugin_disabled", &[("id", id)]), false);
                self.refresh_plugin_browser();
            }
            AppEffect::PluginPickerOpen { provider, query } => {
                self.open_plugin_picker(provider, query)
            }
            AppEffect::PluginExecute {
                plugin,
                command,
                arguments,
            } => {
                self.execute_plugin_command(plugin, command, arguments);
            }
            AppEffect::RetainedUiFocus(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Focus(delta));
            }
            AppEffect::RetainedUiScroll(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Scroll(delta));
            }
            AppEffect::RetainedUiActivate => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Activate);
            }
            AppEffect::RetainedUiMouseSelect => self.select_retained_ui_at_mouse(),
            AppEffect::PrivateUiActivate { at_mouse } => {
                self.activate_private_surface_action(at_mouse);
            }
            AppEffect::VirtualDocumentOpen { source, key } => {
                let result = self.extension.open_virtual(&source, key, &self.state);
                self.route_virtual_open(result);
            }
            AppEffect::VirtualDocumentRefresh => {
                let result = self
                    .state
                    .active_buffer_id()
                    .ok_or_else(|| "no active virtual document".to_owned())
                    .and_then(|buffer| self.extension.refresh_virtual(buffer, &self.state));
                self.route_virtual_open(result);
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
