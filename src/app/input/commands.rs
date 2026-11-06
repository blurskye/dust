
use crate::app::commands::AppEffect;
use crate::app::App;


impl App {
    pub(in crate::app) fn process_input_effect(&mut self, effect: AppEffect) -> Result<(), AppEffect> {
        match effect {
AppEffect::PlaceMouseCursor => self.apply_mouse_cursor(false),
AppEffect::ExtendMouseSelection => self.apply_mouse_cursor(true),
AppEffect::CancelPendingMapping => self.cancel_pending_mapping(),
            effect => return Err(effect),
        }
        Ok(())
    }
}
