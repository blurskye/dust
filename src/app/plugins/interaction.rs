use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::picker::PickerOrigin;
use crate::app::plugins::PluginUiInteraction;
use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::ui::PickerSurfaceKind;
use crate::ui::PromptKind;

use crate::app::plugins::integration::compose_plugin_picker_items;

impl App {
    pub(in crate::app) fn apply_retained_ui_interaction(
        &mut self,
        interaction: PluginUiInteraction,
    ) {
        match self
            .extension
            .window_interaction(self.window_session.graph(), interaction)
        {
            Ok(action) => {
                self.ui.plugin_surfaces = self.extension.window_snapshots();
                if let Some(action) = action {
                    self.invoke_mapped_command(&action);
                }
            }
            Err(error) => self.show_error(error.detail),
        }
    }
}

impl App {
    pub(in crate::app) fn select_retained_ui_at_mouse(&mut self) {
        let Some((column, row)) = self.input.mouse_position() else {
            return;
        };
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let Some(window) = projection.hit_test(column, row) else {
            return;
        };
        let Some(rect) = projection.window(window).map(|projected| projected.rect) else {
            return;
        };
        if let Err(error) = self
            .extension
            .focus_window_row(window, usize::from(row.saturating_sub(rect.y)))
        {
            self.show_error(error.detail);
            return;
        }
        self.ui.plugin_surfaces = self.extension.window_snapshots();
    }
}

impl App {
    pub(in crate::app) fn activate_private_surface_action(&mut self, at_mouse: bool) {
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let target = if at_mouse {
            self.input.mouse_position().and_then(|(x, y)| {
                projection.hit_test(x, y).and_then(|window| {
                    projection.window(window).map(|projected| {
                        (
                            window,
                            x.saturating_sub(projected.rect.x),
                            y.saturating_sub(projected.rect.y),
                        )
                    })
                })
            })
        } else {
            self.window_session.graph().focused().and_then(|window| {
                self.ui
                    .private_surfaces
                    .iter()
                    .find(|surface| surface.window == window)
                    .and_then(|surface| surface.cursor.map(|(x, y)| (window, x, y)))
            })
        };
        let Some((window, x, y)) = target else { return };
        match self.extension.private_action(window, x, y) {
            Ok(Some(action)) => self.invoke_mapped_command(&action),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn present_next_plugin_confirmation(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        let Some(request) = self.extension.next_confirmation(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
            &self.input,
            &self.window_session,
        ) else {
            let _ = self.extension.dispatch_confirmation_results(
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            );
            return;
        };
        let selected = match request.default_answer {
            crate::plugins::PluginConfirmationAnswer::Yes => crate::ui::ConfirmationChoice::Yes,
            crate::plugins::PluginConfirmationAnswer::No
            | crate::plugins::PluginConfirmationAnswer::Cancelled => {
                crate::ui::ConfirmationChoice::No
            }
        };
        self.open_confirmation(
            PromptKind::PluginConfirmation,
            format!("Plugin {} · {}", request.plugin, request.title),
            request.message,
            selected,
        );
    }
}

impl App {
    pub(in crate::app) fn submit_plugin_confirmation(
        &mut self,
        selected: crate::ui::ConfirmationChoice,
    ) {
        let answer = match selected {
            crate::ui::ConfirmationChoice::Yes => crate::plugins::PluginConfirmationAnswer::Yes,
            crate::ui::ConfirmationChoice::No => crate::plugins::PluginConfirmationAnswer::No,
        };
        self.extension.answer_confirmation(answer);
        self.close_prompt();
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
    }
}

impl App {
    pub(in crate::app) fn open_plugin_picker(&mut self, provider: String, query: String) {
        let invocation = match self.extension.start_provider(
            &provider,
            query,
            self.workspace.root_revision(),
            &self.state,
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            format!("Plugin · {provider}"),
            Vec::new(),
            String::new(),
            true,
            PickerOrigin::PluginProvider(invocation.request),
        );
        if self.extension.provider_visible(invocation.request) {
            self.process_effect(AppEffect::PluginExecute {
                plugin: invocation.plugin,
                command: invocation.command,
                arguments: Vec::new(),
            });
        } else {
            self.extension
                .discard_unpresented_provider(invocation.request);
        }
    }
}

impl App {
    pub(in crate::app) fn execute_plugin_command(
        &mut self,
        plugin: String,
        command: String,
        arguments: Vec<String>,
    ) -> bool {
        let Some(load) = self.extension.load_id(&plugin) else {
            self.show_error(crate::plugins::PluginError::NotActive);
            return false;
        };
        if !self.extension.is_active_generation(&plugin, load) {
            self.show_error(crate::plugins::PluginError::NotActive);
            return false;
        }
        self.refresh_plugin_host_events();
        self.retire_plugin_edit_sources();
        let maximum = SEARCH_MAXIMUM_FILE_BYTES;
        let input_capture = self.input.capture_epoch();
        let interaction_available = self.input.prompt().is_none()
            && self.ui.picker.is_none()
            && self.ui.tool_menu.is_none()
            && self.ui.settings.is_none()
            && self.input.documentation().is_none()
            && !self.input.mapping_pending()
            && !self.input.grammar_pending();
        let navigation = self
            .window_session
            .navigation_stamp(self.state.window_version(), self.state.active_view);
        let result = self.extension.execute(
            &plugin,
            &command,
            crate::app::plugins::InvocationContext {
                editor: &self.state,
                workspace: &self.workspace,
                configuration: self.configuration.binding_revision_ref(),
                generation: self.configuration.generation(),
                ui_revision: self.window_session.graph().revision(),
                input_capture,
                navigation,
                interaction_available,
                maximum,
                arguments,
            },
        );
        if let Err(error) = result {
            self.show_error(error);
            return false;
        }
        true
    }
}

impl App {
    pub(in crate::app) fn present_plugin_picker(&mut self, request: u64) {
        if !self.extension.provider_visible(request) {
            return;
        }
        let Some(projection) = self.extension.provider_projection(request) else {
            return;
        };
        if let Some(error) = &projection.error {
            self.show_error(&error.detail);
        }
        let items = match compose_plugin_picker_items(&projection) {
            Ok(items) => items,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            format!("Plugin · {}", projection.provider),
            items,
            String::new(),
            true,
            PickerOrigin::PluginProvider(request),
        );
    }
}
