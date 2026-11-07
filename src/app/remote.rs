//! Requests that reach the running Dust over its socket: files that programs in its
//! terminals ask it to open, such as lazygit's edit key, and subagents that agents start
//! through `dust --agent-tools`.

use std::collections::BTreeMap;

use crate::app::App;
use crate::remote::RemoteRequest;
use crate::remote::RemoteServer;
use crate::remote::SERVER_VARIABLE;
use crate::remote::lazygit_config;
use crate::windows::WindowPlacement;

impl App {
    /// Listen for `dust --remote` and tell terminals where to find this session. Without a
    /// socket Dust still works; lazygit then opens files in its own configured editor.
    pub(in crate::app) fn start_remote_server(&mut self) {
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

    pub(in crate::app) fn process_remote_opens(&mut self) {
        let Some(server) = &self.remote else {
            return;
        };
        let requests = std::iter::from_fn(|| server.try_request()).collect::<Vec<_>>();
        let mut opens = Vec::new();
        for request in requests {
            match request {
                RemoteRequest::Open(open) => opens.push(open),
                RemoteRequest::Agent(request) => self.start_subagent(request),
            }
        }
        if opens.is_empty() {
            return;
        }
        // The program that asked, such as lazygit in a float, hides like `ctrl+/` would hide
        // it and keeps running.
        if self.window_session.terminal_focused() {
            self.place_active_terminal(WindowPlacement::Hidden);
        }
        for request in opens {
            let navigation = self
                .window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view());
            if let Err(error) = self.documents.request_open_at_line(
                request.path,
                &self.state.workspace_root,
                &self.state.window_version(),
                self.workspace.version(),
                navigation,
                Some(request.line.unwrap_or(1).saturating_sub(1)),
            ) {
                self.show_document_failure(error);
            }
        }
    }
}
