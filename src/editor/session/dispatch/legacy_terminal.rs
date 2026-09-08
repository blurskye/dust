//! Remaining editor/window routing; terminal data and algorithms live in the owner.
use super::{InteractiveApplication, PtyId, RegisterShape, RegisterValue};
impl InteractiveApplication {
    fn terminal_command_target(&self) -> Option<PtyId> {
        self.keyboard_terminal()
            .or(self.terminal_session.selected())
    }
    pub(super) fn reject_terminal_responses(&mut self, reason: &str) {
        self.debugger_session.reject_terminals(reason);
    }

    pub(super) fn retire_terminal(&mut self, id: PtyId, _request_close: bool) {
        let focused = self.window_session.focused_terminal() == Some(id.0);
        self.terminal_session.close(id);
        if let Some(reply) = self.debugger_session.take_terminal(&id) {
            let _ = reply.respond(
                false,
                Some("terminal closed before startup response".into()),
            );
        }
        if focused {
            self.set_terminal_focus(self.terminal_session.selected().is_some());
        }
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.ensure_cursor_visible();
    }
    pub(super) fn enter_terminal_normal_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_normal_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
    pub(super) fn enter_terminal_input_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_input_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
    pub(super) fn move_terminal_normal_cursor(&mut self, motion: &str) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        if let Err(error) = self
            .terminal_session
            .move_terminal_normal_cursor(id, motion)
        {
            self.show_error(error);
        }
        self.sync_terminal_panel();
    }
    pub(super) fn start_terminal_visual(&mut self, linewise: bool) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.start_terminal_visual(id, linewise);
        }
        self.sync_terminal_panel();
    }
    pub(super) fn yank_terminal_text(&mut self, line: bool) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        let Some((text, linewise)) = self.terminal_session.yank_terminal_text(id, line) else {
            return;
        };
        self.state.registers.set(
            '"',
            RegisterValue {
                text,
                shape: if linewise {
                    RegisterShape::Line
                } else {
                    RegisterShape::Character
                },
            },
            self.state.policy.registers_enabled,
            self.state.policy.mirror_named_to_unnamed,
        );
        self.state.mark_editor_changed();
        self.notify_message(self.message("terminal_yanked", &[]), false);
        self.sync_terminal_panel();
    }
    pub(super) fn sync_terminal_panel(&mut self) {
        self.ui.terminal_panel = self.terminal_session.projection(
            self.window_session.focused_terminal(),
            self.window_session.terminal_height(),
        );
        if self.ui.terminal_panel.is_none() {
            self.set_terminal_focus(false);
        }
        self.reconcile_editor_windows();
    }
}
