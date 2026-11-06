//! Command registration and execution for picker rows, selection and previews.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::picker::PickerPayload;
use crate::ui::PickerSurfaceKind;

use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::CommandValue;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, effect) in [
        (
            "picker.backspace",
            "Delete picker query character",
            AppEffect::PickerBackspace,
        ),
        (
            "picker.next",
            "Select next picker item",
            AppEffect::PickerMove(1),
        ),
        (
            "picker.previous",
            "Select previous picker item",
            AppEffect::PickerMove(-1),
        ),
        (
            "picker.preview.down",
            "Scroll the picker's diff down",
            AppEffect::PickerPreviewPage(1),
        ),
        (
            "picker.preview.up",
            "Scroll the picker's diff up",
            AppEffect::PickerPreviewPage(-1),
        ),
        (
            "picker.results",
            "Send these results to the shared Results surface",
            AppEffect::PickerResults,
        ),
        (
            "picker.remove",
            "Remove the selected row",
            AppEffect::PickerRemove,
        ),
        (
            "picker.accept",
            "Accept picker item",
            AppEffect::PickerAccept,
        ),
        ("picker.cancel", "Close picker", AppEffect::PickerCancel),
        (
            "picker.mode.normal",
            "Navigate picker results",
            AppEffect::PickerNormal,
        ),
        (
            "picker.mode.insert",
            "Edit picker query",
            AppEffect::PickerInsertMode,
        ),
        (
            "workspace.edit.toggle",
            "Toggle workspace edit file",
            AppEffect::WorkspaceEditToggle,
        ),
        (
            "workspace.edit.accept",
            "Accept workspace edit",
            AppEffect::WorkspaceEditAccept,
        ),
        (
            "workspace.resource.apply",
            "Apply staged resource operations",
            AppEffect::WorkspaceResourceApply,
        ),
        (
            "workspace.edit.reject",
            "Reject workspace edit",
            AppEffect::WorkspaceEditReject,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "picker.insert",
        "Insert picker query text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PickerInsert(
                string_arg(invocation, "text")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "picker.plugin.open",
        "Open a plugin picker source",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("provider", ArgumentType::String, true)
            .field("query", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::PluginPickerOpen {
                provider: string_arg(invocation, "provider")?.into(),
                query: invocation
                    .arguments
                    .get("query")
                    .map(CommandValue::as_str)
                    .transpose()?
                    .unwrap_or("")
                    .into(),
            })
        },
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_picker_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::OpenWorkspaceFilePicker => self.start_workspace_file_picker(),
            AppEffect::OpenPicker(kind) => self.open_builtin_picker(kind),
            AppEffect::PickerInsert(text) => self.picker_insert(&text),
            AppEffect::PickerBackspace => self.picker_backspace(),
            AppEffect::PickerMove(direction) => self.picker_move(direction),
            AppEffect::PickerPreviewPage(direction) => {
                self.search.picker_preview_page(direction);
                self.sync_picker_ui();
            }
            AppEffect::PickerResults => {
                if self.search.set_picker_surface(PickerSurfaceKind::Problems) {
                    self.sync_picker_ui();
                }
            }
            AppEffect::PickerRemove => match self.search.selected_picker_payload().cloned() {
                Some(PickerPayload::Recovery(journal)) => {
                    if let Err(error) =
                        self.recovery
                            .discard(&journal, true, self.documents.recovery_io())
                    {
                        self.show_error(error);
                    }
                }
                Some(PickerPayload::GitWorktree { .. }) => self.request_git_worktree_remove(),
                _ => {}
            },
            AppEffect::PickerAccept => self.accept_picker(),
            AppEffect::PickerCancel => self.close_picker(),
            AppEffect::PickerNormal if self.palette_is_open() => self.close_picker(),
            AppEffect::PickerNormal | AppEffect::PickerInsertMode => {
                if self.search.picker().is_some() && self.search.workspace_picker().is_none() {
                    self.input
                        .set_picker_normal(matches!(effect, AppEffect::PickerNormal));
                    self.sync_picker_ui();
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
