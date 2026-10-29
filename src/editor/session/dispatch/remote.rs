//! Files that programs in Dust's terminals ask it to open, such as lazygit's edit key.

use std::collections::BTreeMap;

use super::InteractiveApplication;
use super::WindowPlacement;
use crate::{
    language::lsp::{LspLocation, LspPosition, LspRange, file_uri},
    remote::{RemoteServer, SERVER_VARIABLE, lazygit_config},
};

impl InteractiveApplication {
    /// Listen for `dust --remote` and tell terminals where to find this session. Without a
    /// socket Dust still works; lazygit then opens files in its own configured editor.
    pub(super) fn start_remote_server(&mut self) {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(|directory| std::path::PathBuf::from(directory).join("dust"))
            .unwrap_or_else(|| crate::config::user_state_directory().join("run"));
        let server = match RemoteServer::start(&directory) {
            Ok(server) => server,
            Err(error) => {
                self.notify_message(format!("dust --remote is unavailable: {error}"), false);
                return;
            }
        };
        let mut environment = BTreeMap::from([(
            SERVER_VARIABLE.to_owned(),
            server.path().display().to_string(),
        )]);
        if let Ok(dust) = std::env::current_exe()
            && let Ok(files) = lazygit_config(&directory, &dust)
        {
            environment.insert("LG_CONFIG_FILE".into(), files);
        }
        self.terminal_session.set_environment(environment);
        self.remote = Some(server);
    }

    pub(super) fn process_remote_opens(&mut self) {
        let Some(server) = &self.remote else {
            return;
        };
        let requests = std::iter::from_fn(|| server.try_request()).collect::<Vec<_>>();
        if requests.is_empty() {
            return;
        }
        // The program that asked, such as lazygit in a float, hides like `ctrl+/` would hide
        // it and keeps running.
        if self.window_session.terminal_focused() {
            self.place_active_terminal(WindowPlacement::Hidden);
        }
        for request in requests {
            let position = LspPosition {
                line: request.line.unwrap_or(1) - 1,
                character: 0,
            };
            self.navigate_lsp_location(&LspLocation {
                uri: file_uri(&request.path),
                range: LspRange {
                    start: position.clone(),
                    end: position,
                },
            });
        }
    }
}
