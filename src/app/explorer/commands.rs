//! Command registration and execution for explorer navigation and filesystem changes.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::ui::PromptKind;

use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, effect) in [
        (
            "explorer.open",
            "Open file explorer",
            AppEffect::ExplorerOpen,
        ),
        (
            "explorer.close",
            "Close file explorer",
            AppEffect::ExplorerClose,
        ),
        (
            "explorer.panel.toggle",
            "Toggle file explorer",
            AppEffect::ExplorerPanelToggle,
        ),
        (
            "explorer.next",
            "Select next explorer row",
            AppEffect::ExplorerMove(1),
        ),
        (
            "explorer.previous",
            "Select previous explorer row",
            AppEffect::ExplorerMove(-1),
        ),
        (
            "explorer.page_up",
            "Page explorer selection up",
            AppEffect::ExplorerPage(-1),
        ),
        (
            "explorer.page_down",
            "Page explorer selection down",
            AppEffect::ExplorerPage(1),
        ),
        (
            "explorer.parent",
            "Select explorer parent",
            AppEffect::ExplorerParent,
        ),
        (
            "explorer.child",
            "Select first explorer child",
            AppEffect::ExplorerChild,
        ),
        (
            "explorer.toggle",
            "Expand or collapse explorer node",
            AppEffect::ExplorerToggle,
        ),
        (
            "explorer.refresh",
            "Refresh explorer",
            AppEffect::ExplorerRefresh,
        ),
        (
            "explorer.follow",
            "Follow active file in explorer",
            AppEffect::ExplorerFollow,
        ),
        (
            "explorer.hidden.toggle",
            "Toggle hidden explorer entries",
            AppEffect::ExplorerToggleHidden,
        ),
        (
            "explorer.ignored.toggle",
            "Toggle ignored explorer entries",
            AppEffect::ExplorerToggleIgnored,
        ),
        (
            "explorer.file.open",
            "Open explorer file",
            AppEffect::ExplorerFileOpen { split: false },
        ),
        (
            "explorer.file.split_open",
            "Split-open explorer file",
            AppEffect::ExplorerFileOpen { split: true },
        ),
        (
            "explorer.mouse.select",
            "Select explorer row at mouse",
            AppEffect::ExplorerMouseSelect,
        ),
        (
            "explorer.search",
            "Search the explorer",
            AppEffect::ExplorerSearch,
        ),
        (
            "explorer.search.delete",
            "Delete the last character of the explorer search",
            AppEffect::ExplorerSearchDelete,
        ),
        (
            "explorer.search.done",
            "Stop typing and keep the explorer filtered",
            AppEffect::ExplorerSearchStop { keep: true },
        ),
        (
            "explorer.search.cancel",
            "Clear the explorer search",
            AppEffect::ExplorerSearchStop { keep: false },
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "explorer.create",
        "Create explorer file or directory",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("path", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplorerCreate(
                string_arg(invocation, "path")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "explorer.search.insert",
        "Type into the explorer search",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplorerSearchInsert(
                string_arg(invocation, "text")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "explorer.rename.prompt",
        "Rename selected explorer path",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::ExplorerRenamePrompt,
    )?;
    register(
        registry,
        "explorer.rename",
        "Rename selected explorer path",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("target", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplorerRename(
                string_arg(invocation, "target")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "explorer.delete",
        "Delete selected explorer path",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::ExplorerDeleteRequest,
    )?;
    for (id, title, effect) in [
        (
            "explorer.copy",
            "Copy selected explorer path",
            AppEffect::ExplorerClipboard { cut: false },
        ),
        (
            "explorer.move",
            "Move selected explorer path",
            AppEffect::ExplorerClipboard { cut: true },
        ),
        (
            "explorer.paste",
            "Paste copied explorer path",
            AppEffect::ExplorerPaste,
        ),
        (
            "explorer.path.yank",
            "Yank selected explorer path",
            AppEffect::ExplorerYankPath,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    Ok(())
}

impl App {
    pub(in crate::app) fn process_explorer_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::ExplorerPanelToggle => {
                return self.process_explorer_effect(if self.ui.explorer.is_some() {
                    AppEffect::ExplorerClose
                } else {
                    AppEffect::ExplorerOpen
                });
            }
            AppEffect::ExplorerOpen => self.open_explorer(),
            AppEffect::ExplorerSearch => {
                self.explorer.start_search();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerSearchInsert(text) => {
                let query = format!("{}{text}", self.explorer.tree().filter());
                self.search_explorer(query);
            }
            AppEffect::ExplorerSearchDelete => {
                let mut query = self.explorer.tree().filter().to_owned();
                query.pop();
                self.search_explorer(query);
            }
            AppEffect::ExplorerSearchStop { keep } => {
                self.explorer.stop_search(keep);
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerClose if !self.explorer.tree().filter().is_empty() => {
                self.explorer.stop_search(false);
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerClose => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.ui.explorer = None;
                self.set_explorer_focus(false);
                self.reconcile_explorer_watch();
                self.ensure_cursor_visible();
            }
            AppEffect::ExplorerMove(delta) => {
                self.move_explorer_selection(isize::from(delta));
            }
            AppEffect::ExplorerPage(direction) => self.page_explorer_selection(direction),
            AppEffect::ExplorerParent => self.explorer_collapse_or_parent(),
            AppEffect::ExplorerChild => {
                self.explorer.select_first_child();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerToggle => self.explorer_toggle(),
            AppEffect::ExplorerRefresh => {
                self.refresh_explorer();
                self.start_git_status_refresh_with_feedback(false);
            }
            AppEffect::ExplorerFollow => self.follow_active_file_in_explorer(),
            AppEffect::ExplorerToggleHidden => {
                self.explorer.toggle_hidden();
                self.refresh_explorer();
            }
            AppEffect::ExplorerToggleIgnored => {
                self.explorer.toggle_ignored();
                self.refresh_explorer();
            }
            AppEffect::ExplorerFileOpen { split } => self.explorer_open_file(split),
            AppEffect::ExplorerCreate(path) => self.create_explorer_path(path),
            AppEffect::ExplorerRename(target) => self.rename_explorer_path(target),
            AppEffect::ExplorerRenamePrompt => self.open_explorer_rename_prompt(),
            AppEffect::ExplorerDeleteRequest => self.request_explorer_delete(),
            AppEffect::ExplorerClipboard { cut } => self.set_explorer_clipboard(cut),
            AppEffect::ExplorerPaste => self.paste_explorer_clipboard(),
            AppEffect::ExplorerYankPath => self.yank_explorer_path(),
            AppEffect::ExplorerMouseSelect => self.explorer_mouse_select(),
            effect => return Err(effect),
        }
        Ok(())
    }
}
