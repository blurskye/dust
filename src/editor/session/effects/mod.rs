use super::*;

mod configuration;
mod core;
pub(super) use core::{ExecutionAdmission, ExecutionAuthorization};
mod debugger;
mod extensions;
mod language;
mod misc;
mod project;
mod terminal;

impl InteractiveApplication {
    pub(super) fn process_effect(&mut self, effect: AppEffect) {
        let effect = match self.process_core_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_configuration_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_terminal_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_extensions_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_project_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_language_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_debugger_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        if self.process_misc_effect(effect).is_err() {
            unreachable!("every AppEffect variant has one owning handler");
        }
    }
}
