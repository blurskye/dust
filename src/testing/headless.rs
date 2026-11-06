//! Headless application execution for tests.

use crate::app::startup::ApplicationError;
use crate::app::startup::StartupTarget;
use crate::app::startup::build_state;
use crate::app::startup::new_file_path;
use crate::editor::text::TextBuffer;
use crate::editor::*;

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
    let mut first_buffer = None;
    for path in std::iter::once(path).chain(additional_files) {
        if !path.exists() && path.parent().is_some_and(std::path::Path::is_dir) {
            let (buffer, _) = state
                .open_text(TextBuffer::from_text(""), Some(new_file_path(path)))
                .map_err(|error| {
                    ApplicationError::Command(CommandError::InvalidArguments(error.into()))
                })?;
            first_buffer.get_or_insert(buffer);
            continue;
        }
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
        let (buffer, _) = state.open_text(text, Some(resolved)).map_err(|error| {
            ApplicationError::Command(CommandError::InvalidArguments(error.into()))
        })?;
        first_buffer.get_or_insert(buffer);
    }
    if let Some(buffer) = first_buffer {
        state.show_buffer(buffer).map_err(|error| {
            ApplicationError::Command(CommandError::InvalidArguments(error.into()))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::app::startup::StartupTarget;
    use crate::app::startup::build_state;
    use crate::testing::headless::HeadlessApplication;

    #[test]
    fn headless_commands_apply_transactions_to_the_startup_document() {
        use crate::editor::{CommandInvocation, CommandValue, EditorMode, InvocationSource};

        let directory = tempdir().unwrap();
        let mut app = HeadlessApplication::bootstrap(StartupTarget::File {
            path: directory.path().join("new.txt"),
            additional_files: Vec::new(),
            workspace_root: directory.path().into(),
        })
        .unwrap();
        assert_eq!(app.startup.workspace_root(), directory.path());
        assert!(
            app.commands
                .specs()
                .any(|spec| spec.id.as_str() == "mode.insert")
        );
        app.execute(
            &CommandInvocation::new("mode.insert", InvocationSource::Test)
                .unwrap()
                .with_argument("placement", CommandValue::String("before".into())),
        )
        .unwrap();
        let view = app.state.active_view().unwrap();
        assert_eq!(app.state.views[view].mode, EditorMode::Insert);
    }

    #[test]
    fn multiple_startup_files_keep_a_live_active_view() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("a");
        let second = directory.path().join("b");
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&second, "b").unwrap();
        let app = HeadlessApplication::bootstrap(StartupTarget::File {
            path: first,
            additional_files: vec![second],
            workspace_root: directory.path().into(),
        })
        .unwrap();
        assert_eq!(
            app.state.buffers[app.state.active_buffer_id().unwrap()]
                .text
                .to_string(),
            "a"
        );
        app.state.validate_layout().unwrap();
    }

    #[test]
    fn headless_startup_opens_new_named_buffers_alongside_existing_files() {
        let directory = tempdir().unwrap();
        let existing = directory.path().join("existing.txt");
        fs::write(&existing, "existing").unwrap();
        for paths in [
            vec!["new.txt", "existing.txt", "another.txt"],
            vec!["existing.txt", "new.txt"],
        ] {
            let startup = crate::app::startup::resolve_startup(
                &paths
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect::<Vec<_>>(),
                directory.path(),
            )
            .unwrap();
            let app = HeadlessApplication::bootstrap(startup).unwrap();
            assert_eq!(app.state.buffers.len(), paths.len());
            for name in &paths {
                let buffer = app
                    .state
                    .buffers
                    .values()
                    .find(|buffer| {
                        buffer
                            .path
                            .as_ref()
                            .is_some_and(|path| path.ends_with(name))
                    })
                    .unwrap();
                assert_eq!(
                    buffer.text.to_string(),
                    if *name == "existing.txt" {
                        "existing"
                    } else {
                        ""
                    }
                );
                assert!(!buffer.dirty);
            }
            assert!(
                app.state.buffers[app.state.active_buffer_id().unwrap()]
                    .path
                    .as_ref()
                    .unwrap()
                    .ends_with(paths[0])
            );
            app.state.validate_layout().unwrap();
        }
        assert!(!directory.path().join("new.txt").exists());
    }

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
