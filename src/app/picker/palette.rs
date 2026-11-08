//! The command palette (F1, `<leader>:`), like VS Code's: one row per command a person would
//! run by name, read as "Category: Label", with its key on the right and the commands run
//! from here most recently at the top. Enter runs the command.

use crate::editor::CommandInvocation;

use crate::editor::CommandValue;

use crate::app::App;

use crate::editor::InvocationSource;

use crate::app::picker::PickerPayload;
use crate::editor::ArgumentType;
use crate::ui::PromptKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::workspace::PickerItemKind;

/// How many recently run commands go first.
const RECENT: usize = 8;

/// Keys as people read them: `<leader> g w` is `Space g w`.
pub(in crate::app) fn readable_keys(keys: &str) -> String {
    keys.split(' ')
        .map(|key| if key == "<leader>" { "Space" } else { key })
        .collect::<Vec<_>>()
        .join(" ")
}

impl App {
    pub(in crate::app) fn open_command_palette(&mut self) {
        let settings = self.configuration.settings();
        let palette = &settings.command_palette;
        let mut rows = self
            .commands
            .specs()
            .filter(|spec| {
                let id = spec.id.as_str();
                !palette
                    .hidden
                    .iter()
                    .any(|prefix| id.starts_with(prefix.as_str()))
                    && (palette.prompts.contains_key(id)
                        || spec
                            .arguments
                            .fields
                            .values()
                            .all(|field| !field.required || field.kind == ArgumentType::Bool))
            })
            .map(|spec| {
                let id = spec.id.as_str().to_owned();
                let label = settings
                    .command_labels
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| spec.title.clone());
                let category = palette
                    .categories
                    .iter()
                    .filter(|(prefix, _)| id == **prefix || id.starts_with(&format!("{prefix}.")))
                    .max_by_key(|(prefix, _)| prefix.len())
                    .map(|(_, category)| category.as_str());
                let label = match category {
                    Some(category) => format!("{category}: {label}"),
                    None => label,
                };
                let keys = ["editor.normal", "global"]
                    .iter()
                    .find_map(|context| self.configuration.keymaps().binding_for(context, &id))
                    .map(readable_keys)
                    .unwrap_or_default();
                (label, keys, id)
            })
            .collect::<Vec<_>>();
        let recent = |id: &str| {
            self.palette_recent
                .iter()
                .position(|previous| previous == id)
                .unwrap_or(usize::MAX)
        };
        rows.sort_by(|a, b| recent(&a.2).cmp(&recent(&b.2)).then_with(|| a.0.cmp(&b.0)));
        let items = rows
            .into_iter()
            .map(|(label, keys, id)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: PickerItemKind::Entry,
                        id: PickerItemId(format!("command:{id}")),
                        label,
                        // The id is matched too, so `git.diff` still finds its row.
                        detail: id.clone(),
                        source: keys,
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(id),
                )
            })
            .collect();
        match crate::app::plugins::integration::compose_command_picker_items(items) {
            Ok(items) => {
                self.install_picker(self.message("picker_commands_title", &[]), items);
                self.search
                    .set_picker_footer("Enter Run · Esc Close".to_owned());
                self.sync_picker_ui();
            }
            Err(error) => self.show_error(error.to_string()),
        }
    }

    /// Esc closes the palette at once, as in VS Code: it has no list mode worth stopping in.
    pub(in crate::app) fn palette_is_open(&self) -> bool {
        let title = self.message("picker_commands_title", &[]);
        self.search
            .picker()
            .is_some_and(|picker| picker.presentation().title == title)
    }

    /// Runs the chosen row and moves it to the top of the palette. Flags such as `force` stay
    /// off; a command that needs typed text opens the command line instead.
    pub(in crate::app) fn run_palette_command(&mut self, id: &str) {
        self.palette_recent.retain(|previous| previous != id);
        self.palette_recent.insert(0, id.to_owned());
        self.palette_recent.truncate(RECENT);
        let prompt = self
            .configuration
            .settings()
            .command_palette
            .prompts
            .get(id);
        if let Some(initial) = prompt.cloned() {
            self.open_prompt(PromptKind::Command, initial);
            return;
        }
        let Ok(mut invocation) = CommandInvocation::new(id, InvocationSource::Picker) else {
            return;
        };
        let flags = self
            .commands
            .specs()
            .filter(|spec| spec.id.as_str() == id)
            .flat_map(|spec| spec.arguments.fields.iter())
            .filter(|(_, field)| field.required && field.kind == ArgumentType::Bool)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for name in flags {
            invocation = invocation.with_argument(&name, CommandValue::Bool(false));
        }
        // Settle focus now the picker is gone: a command that navigates checks it hasn't moved.
        self.reconcile_editor_windows();
        self.invoke(invocation);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn leader_reads_as_space() {
        assert_eq!(
            crate::app::picker::palette::readable_keys("<leader> g w"),
            "Space g w"
        );
        assert_eq!(
            crate::app::picker::palette::readable_keys("ctrl-o"),
            "ctrl-o"
        );
    }
}
