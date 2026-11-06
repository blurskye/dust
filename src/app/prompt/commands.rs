
use crate::ui::PromptKind;
use crate::app::AppEffect;
use crate::app::App;

use crate::app::WorkspaceTrust;
use crate::app::command;
use crate::app::PromptMove;

impl App {
    pub(in crate::app) fn process_prompt_effect(&mut self, effect: AppEffect) -> Result<(), AppEffect> {
        match effect {
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
            // y and n answer at once, like Vim's own confirm prompts.
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
