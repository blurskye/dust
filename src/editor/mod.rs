#![forbid(unsafe_code)]

mod command;
pub mod input;
mod launch;
pub(crate) mod session;
mod state;
pub mod text;
mod transaction;

pub use command::{
    ArgumentField, ArgumentSchema, ArgumentType, CommandArguments, CommandError, CommandId,
    CommandInvocation, CommandOutcome, CommandRegistry, CommandSource, CommandSpec, CommandTarget,
    CommandValue, ContextRequirement, InvocationSource, register_builtin_commands,
};
pub use launch::{StartupError, StartupTarget, resolve_startup};
pub use session::{
    AppEffect, ConfigurationSources, InteractiveError, KeymapInspection, LayoutAction, PickerKind,
    PromptMove, ResolvedConfiguration, ResolvedKeymaps, compile_keymaps,
    compile_keymaps_with_plugin_catalog, compile_keymaps_with_plugin_index, register_app_commands,
    reload_keymaps, run_interactive, validate_loaded_configuration,
};
pub use state::{
    Axis, Buffer, BufferId, BufferOrigin, EditingPolicy, EditorMode, EditorState, FocusDirection,
    LayoutNode, MAXIMUM_LAYOUT_DEPTH, MAXIMUM_LAYOUT_PANES, Pane, PaneId, RegisterShape,
    RegisterStore, RegisterValue, ReplaceRecord, Selection, SelectionKind, Tab, TabId,
    TransactionId, View, ViewId,
};
pub(crate) use state::{EditorTopology, EditorVersion, EditorWindowVersion};
pub(crate) use transaction::EditorDraft;
pub use transaction::{
    EditorOperation, EditorTransaction, Precondition, TransactionError, UndoPolicy, UndoTree,
};

use crate::editor::text::{EncodingError, TextBuffer};
use thiserror::Error;

/// Headless composition used by tests and automation. The terminal application
/// uses the same state construction and deterministic editing command registration.
pub struct HeadlessApplication {
    pub state: EditorState,
    pub commands: CommandRegistry<()>,
    pub startup: StartupTarget,
}

impl HeadlessApplication {
    pub fn bootstrap(startup: StartupTarget) -> Result<Self, ApplicationError> {
        let mut state = build_state(&startup);
        load_headless_startup(&mut state, &startup)?;
        let mut commands = CommandRegistry::default();
        register_builtin_commands(&mut commands)?;
        Ok(Self {
            state,
            commands,
            startup,
        })
    }

    pub fn execute(&mut self, invocation: &CommandInvocation) -> Result<(), ApplicationError> {
        let outcome = self.commands.execute(&self.state, invocation)?;
        if let Some(transaction) = outcome.transaction {
            self.state.apply_transaction(&transaction)?;
        }
        Ok(())
    }
}

pub(crate) fn build_state(startup: &StartupTarget) -> EditorState {
    EditorState::new(startup.workspace_root().to_path_buf())
}

fn load_headless_startup(
    state: &mut EditorState,
    startup: &StartupTarget,
) -> Result<(), ApplicationError> {
    let StartupTarget::File {
        path,
        additional_files,
        ..
    } = startup
    else {
        return Ok(());
    };
    let mut worker = crate::files::VfsWorker::start(additional_files.len().saturating_add(1));
    let mut first_view = None;
    for path in std::iter::once(path).chain(additional_files) {
        let request = worker.read(path.clone())?;
        let (resolved, bytes) = loop {
            match worker.try_result()? {
                Some(crate::files::VfsResult::Read { id, path, result }) if id == request => {
                    break (path, result?);
                }
                Some(_) | None => std::thread::yield_now(),
            }
        };
        let text = TextBuffer::from_bytes(&bytes)?;
        let (_, view) = state.open_text(text, Some(resolved));
        first_view.get_or_insert(view);
    }
    state.active_view = first_view;
    state.mark_window_changed();
    Ok(())
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error(transparent)]
    Vfs(#[from] crate::files::VfsError),
    #[error(transparent)]
    Encoding(#[from] EncodingError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Transaction(#[from] TransactionError),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::editor::{HeadlessApplication, StartupTarget, build_state};

    #[test]
    fn dashboard_has_no_fake_buffer() {
        let root = tempdir().expect("tempdir");
        let app = HeadlessApplication::bootstrap(StartupTarget::Dashboard {
            root: root.path().to_path_buf(),
        })
        .expect("application");
        assert!(app.state.buffers.is_empty());
        assert!(app.state.views.is_empty());
    }

    #[test]
    fn state_construction_does_not_read_startup_file_contents() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("file.txt");
        fs::write(&path, b"contents").expect("write");
        let startup = StartupTarget::File {
            workspace_root: root.path().to_path_buf(),
            path: path.clone(),
            additional_files: Vec::new(),
        };
        let state = build_state(&startup);
        fs::remove_file(path).expect("construction holds no open/read dependency");
        assert!(state.buffers.is_empty());
        assert!(state.views.is_empty());
    }

    #[test]
    fn file_bootstrap_decodes_real_buffer_through_vfs() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("file.txt");
        fs::write(&path, b"one\r\ntwo\r\n").expect("write");
        let app = HeadlessApplication::bootstrap(StartupTarget::File {
            workspace_root: root.path().to_path_buf(),
            path,
            additional_files: Vec::new(),
        })
        .expect("application");
        let buffer = app.state.active_buffer_id().expect("active buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "one\ntwo\n");
    }
}
