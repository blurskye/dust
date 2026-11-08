//! Dashboard actions and visibility.

use crate::app::App;

impl App {
    pub(in crate::app) fn set_dashboard_visibility(&mut self, visible: bool) {
        if visible {
            self.window_session.open_dashboard();
        } else {
            self.window_session.close_dashboard();
        }
        self.project_window_activation();
    }
}

mod commands;

#[cfg(test)]
pub(in crate::app) mod tests;
