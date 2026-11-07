//! Command registration and execution for keyboard, mouse and mapped command input.

use crate::app::App;
use crate::app::commands::AppEffect;

impl App {
    pub(in crate::app) fn process_input_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Input(command) => self.run_input_command(command),
            AppEffect::PlaceMouseCursor => self.apply_mouse_cursor(false),
            AppEffect::ExtendMouseSelection => self.apply_mouse_cursor(true),
            AppEffect::CancelPendingMapping => self.cancel_pending_mapping(),
            effect => return Err(effect),
        }
        Ok(())
    }

    fn run_input_command(&mut self, command: crate::app::input::InputCommand) {
        use crate::app::input::InputCommand;
        match command {
            InputCommand::EnterMode(mode) => {
                if let Some(view) = self.state.active_view() {
                    self.set_mode(mode.base().unwrap_or(crate::editor::EditorMode::Normal));
                    self.input.cancel_pending();
                    self.input.cancel_mapping();
                    self.input.named_mode = Some((self.state.window_version(), view, *mode));
                    self.input.capture_changed();
                }
            }
            InputCommand::LeaveMode => {
                self.input.named_mode = None;
                self.input.cancel_pending();
                self.input.cancel_mapping();
                self.input.capture_changed();
            }
            InputCommand::PushLayer(layer) => {
                self.input.cancel_mapping();
                self.input.key_layers.push((None, *layer));
                self.input.capture_changed();
            }
            InputCommand::PopLayer => {
                self.input.cancel_mapping();
                self.input.key_layers.pop();
                self.input.capture_changed();
            }
            InputCommand::PushNamedLayer { owner, name, layer } => {
                self.input.cancel_mapping();
                self.input.key_layers.push((Some((owner, name)), *layer));
                self.input.capture_changed();
            }
            InputCommand::RemoveNamedLayer { owner, name } => {
                self.input.cancel_mapping();
                self.input
                    .key_layers
                    .retain(|(key, _)| key.as_ref() != Some(&(owner.clone(), name.clone())));
                self.input.capture_changed();
            }
            InputCommand::Wait(_) => {}
            InputCommand::Complete {
                id,
                succeeded,
                text_changed,
            } => {
                self.complete_input_command(id, succeeded, text_changed);
            }
        }
    }
}
