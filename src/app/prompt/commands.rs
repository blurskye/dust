//! Command registration and execution for input prompts, confirmations and command completion.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::ui::PromptKind;

impl App {
    pub(in crate::app) fn process_prompt_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::OpenPrompt { kind, initial } => self.open_prompt(kind, initial),
            AppEffect::OpenFilePrompt => {
                let alias = self
                    .configuration
                    .settings()
                    .input
                    .command_aliases
                    .iter()
                    .find(|(_, command)| command.as_str() == "file.open")
                    .map_or("file.open", |(alias, _)| alias.as_str());
                self.open_prompt(PromptKind::Command, format!("{alias} "));
            }
            AppEffect::PromptInsert(text) => self.prompt_insert(&text),
            AppEffect::PromptBackspace => self.prompt_backspace(),
            AppEffect::PromptDeleteWord => self.prompt_delete(|input| input.prompt_delete_word()),
            AppEffect::PromptDeleteToStart => {
                self.prompt_delete(|input| input.prompt_delete_to_start());
            }
            AppEffect::PromptMove(movement) => self.prompt_move(movement),
            AppEffect::PromptHistory(direction) => self.prompt_history(direction),
            AppEffect::PromptComplete(direction) => self.prompt_complete(direction),
            AppEffect::PromptConfirmationSelect(choice) => {
                self.input.select_confirmation(choice);
                self.submit_prompt();
            }
            AppEffect::PromptConfirmationToggle => {
                self.input.toggle_confirmation();
                self.project_input_prompt();
            }
            AppEffect::PromptSubmit => self.submit_prompt(),
            AppEffect::PromptCancel => {
                let declined_trust = self
                    .input
                    .prompt()
                    .is_some_and(|prompt| prompt.kind == PromptKind::WorkspaceTrust);
                self.close_prompt();
                if declined_trust {
                    self.notify_message(self.message("workspace_trust_declined", &[]), false);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
