//! Terminal screens, tab selection and PTY source lifetimes. No window/focus authority.
mod input;
use crate::app::windows::TerminalGeometrySource;
use crate::app::workspace::ExecutionReady;
use crate::app::workspace::Workspace;
use crate::app::workspace::WorkspaceVersion;
use crate::config::TerminalProfileConfig;
use crate::config::TerminalsConfig;
use crate::process::ExecutionPreflightRequest;
use crate::process::PtyEvent;
use crate::process::PtyId;
use crate::process::PtyPolicy;
use crate::process::PtyRequest;
use crate::process::PtyWorker;
use crate::ui::PtyScreen;
use crate::ui::TerminalPanelMode;
use crate::windows::ContentLifecycle;

pub(in crate::app) const TERMINAL_QUEUE_CAPACITY: usize = 256;
pub(in crate::app) const TERMINAL_MAXIMUM_INPUT_BYTES: usize = 1024 * 1024;
pub(in crate::app) const TERMINAL_SCROLLBACK_BYTES: usize = 8 * 1024 * 1024;

pub(in crate::app) use input::TerminalInput;
use input::clamp_terminal_cursor;
use input::keep_terminal_cursor_visible;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;

/// The profile name of a terminal a debugger opened for its program.
pub(in crate::app) const DEBUG_TERMINAL_PROFILE: &str = "debug";

#[derive(Clone, Debug)]
pub(in crate::app) struct TerminalKey {
    owner: Arc<()>,
    id: PtyId,
}
impl PartialEq for TerminalKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.owner, &other.owner)
    }
}
impl Eq for TerminalKey {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct TerminalIntent {
    key: TerminalKey,
    profile: String,
    agent: bool,
}
struct TerminalCloseSource {
    key: TerminalKey,
    pty: Option<PtyId>,
    phase: crate::ui::TerminalPhase,
}
struct Intent {
    token: TerminalIntent,
    open: Open,
    scope: WorkspaceVersion,
    epoch: Arc<()>,
    navigation: crate::app::windows::NavigationStamp,
    capture: Arc<()>,
}
pub(in crate::app) struct TerminalOpen {
    pub(in crate::app) key: TerminalKey,
    pub(in crate::app) request: ExecutionPreflightRequest,
    pub(in crate::app) navigation: crate::app::windows::NavigationStamp,
    pub(in crate::app) capture: Arc<()>,
    pub(in crate::app) float: bool,
}
struct Open {
    request: ExecutionPreflightRequest,
    config: TerminalProfileConfig,
    arguments: Vec<std::ffi::OsString>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) enum TerminalUpdate {
    Started(PtyId),
    Exited(PtyId, Option<u32>),
    IoComplete(PtyId),
    Closed(PtyId),
    StopUnknown(PtyId, String),
    Failed(PtyId, String),
    Unknown(PtyId, String),
    Changed,
}
pub(super) struct Terminals {
    service: PtyWorker,
    preflights: HashMap<crate::process::PreflightId, TerminalKey>,
    tabs: HashMap<PtyId, TerminalRuntime>,
    selected: Option<PtyId>,
    displayed: Option<PtyId>,
    next: Option<u64>,
    revision: Option<u64>,
    intent: Option<Intent>,
    owner: Arc<()>,
    epoch: Arc<()>,
    policy: TerminalsConfig,
    closed_ptys: HashSet<PtyId>,
    close_intent: Option<TerminalCloseSource>,
    input_hint_seen: bool,
    input_hint_until: Option<std::time::Instant>,
    closed: bool,
    /// Set for every terminal under the profile's own variables, such as `DUST_SERVER`.
    environment: BTreeMap<String, String>,
}
impl Terminals {
    pub(in crate::app) fn set_environment(&mut self, environment: BTreeMap<String, String>) {
        self.environment = environment;
    }

    pub(in crate::app) fn new(policy: &TerminalsConfig) -> Self {
        Self {
            service: PtyWorker::start(TERMINAL_QUEUE_CAPACITY),
            preflights: HashMap::new(),
            tabs: HashMap::new(),
            selected: None,
            displayed: None,
            next: Some(1),
            revision: Some(0),
            intent: None,
            owner: Arc::new(()),
            epoch: Arc::new(()),
            policy: policy.clone(),
            closed_ptys: HashSet::new(),
            close_intent: None,
            input_hint_seen: false,
            input_hint_until: None,
            closed: false,
            environment: BTreeMap::new(),
        }
    }
    pub(in crate::app) fn selected(&self) -> Option<PtyId> {
        self.selected
    }
    pub(in crate::app) fn displayed(&self) -> Option<PtyId> {
        self.displayed.filter(|id| self.contains(*id))
    }
    /// Shows the terminal in the panel. A side terminal has its own window, so the panel keeps
    /// what it shows.
    pub(in crate::app) fn display(&mut self, id: PtyId) -> bool {
        if !self.contains(id) {
            return false;
        }
        if !self.side(id) {
            self.displayed = Some(id);
        }
        true
    }
    /// A side terminal, such as a terminal agent docked on the right, stays in its own window
    /// beside the panel instead of being one of the panel's tabs.
    pub(in crate::app) fn set_side(&mut self, id: PtyId) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.side = true;
        }
        if self.displayed == Some(id) {
            self.displayed = self.panel_fallback();
        }
    }
    pub(in crate::app) fn is_agent(&self, id: PtyId) -> bool {
        self.tabs.get(&id).is_some_and(|tab| tab.agent)
    }
    pub(in crate::app) fn side(&self, id: PtyId) -> bool {
        self.tabs.get(&id).is_some_and(|tab| tab.side)
    }
    pub(in crate::app) fn side_terminals(&self) -> Vec<PtyId> {
        let mut ids = self
            .tabs
            .iter()
            .filter(|(_, tab)| tab.side)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        ids.sort_by_key(|id| id.0);
        ids
    }
    /// The terminal the panel shows or would show.
    pub(in crate::app) fn panel(&self) -> Option<PtyId> {
        self.displayed().or_else(|| self.panel_fallback())
    }
    fn panel_fallback(&self) -> Option<PtyId> {
        self.tabs
            .iter()
            .filter(|(_, tab)| !tab.side)
            .map(|(id, _)| *id)
            .min_by_key(|id| id.0)
    }
    pub(in crate::app) fn revision(&self) -> Option<u64> {
        self.revision
    }
    pub(in crate::app) fn len(&self) -> usize {
        self.tabs.len()
    }
    pub(in crate::app) fn contains(&self, id: PtyId) -> bool {
        self.tabs.contains_key(&id)
    }
    pub(in crate::app) fn select(&mut self, id: PtyId) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.selected = Some(id);
        if self.displayed.is_none() && !self.side(id) {
            self.displayed = Some(id);
        }
        true
    }
    #[cfg(test)]
    pub(in crate::app) fn key(&self, id: PtyId) -> Option<TerminalKey> {
        self.key_for(id)
    }
    pub(in crate::app) fn resolve(&self, key: &TerminalKey) -> Option<PtyId> {
        (Arc::ptr_eq(&key.owner, &self.owner) && self.contains(key.id)).then_some(key.id)
    }
    pub(in crate::app) fn requires_close_confirmation(&self, id: PtyId) -> bool {
        self.tabs
            .get(&id)
            .is_some_and(|terminal| terminal.running || terminal.pty.is_some())
    }
    pub(in crate::app) fn begin_close(&mut self, id: PtyId) -> bool {
        let Some(key) = self.key_for(id) else {
            return false;
        };
        let terminal = &self.tabs[&id];
        self.close_intent = Some(TerminalCloseSource {
            key,
            pty: terminal.pty,
            phase: terminal.phase(),
        });
        true
    }
    pub(in crate::app) fn cancel_close(&mut self) {
        self.close_intent = None;
    }
    pub(in crate::app) fn take_close(&mut self) -> Result<Option<PtyId>, String> {
        let Some(source) = self.close_intent.take() else {
            return Ok(None);
        };
        let id = self
            .resolve(&source.key)
            .ok_or("terminal close source is stale or was replaced")?;
        let terminal = &self.tabs[&id];
        if terminal.pty != source.pty || terminal.phase() != source.phase {
            return Err("terminal close source lifecycle changed".into());
        }
        Ok(Some(id))
    }
    fn key_for(&self, id: PtyId) -> Option<TerminalKey> {
        self.contains(id).then(|| TerminalKey {
            owner: self.owner.clone(),
            id,
        })
    }
    pub(in crate::app) fn descriptions(&self) -> Vec<(u64, String, ContentLifecycle)> {
        let mut rows = self
            .tabs
            .iter()
            .map(|(id, terminal)| {
                let lifecycle = match terminal.phase() {
                    crate::ui::TerminalPhase::Preflight => ContentLifecycle::Preflight,
                    crate::ui::TerminalPhase::Starting => ContentLifecycle::Starting,
                    crate::ui::TerminalPhase::Running => ContentLifecycle::Running,
                    crate::ui::TerminalPhase::Stopping => ContentLifecycle::Stopping,
                    crate::ui::TerminalPhase::Exited => ContentLifecycle::Exited,
                    crate::ui::TerminalPhase::Failed => ContentLifecycle::Failed,
                    crate::ui::TerminalPhase::Unknown => ContentLifecycle::Unknown,
                };
                (id.0, terminal.profile.clone(), lifecycle)
            })
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|(id, ..)| *id);
        rows
    }
    pub(in crate::app) fn choices(&self) -> Vec<(TerminalKey, String, String)> {
        let mut rows = self
            .tabs
            .iter()
            .map(|(id, tab)| {
                (
                    TerminalKey {
                        owner: self.owner.clone(),
                        id: *id,
                    },
                    format!("#{} {}", id.0, tab.profile),
                    format!(
                        "{}{}",
                        match tab.phase() {
                            crate::ui::TerminalPhase::Preflight => "preflight",
                            crate::ui::TerminalPhase::Starting => "starting",
                            crate::ui::TerminalPhase::Running => "running",
                            crate::ui::TerminalPhase::Stopping => "stopping",
                            crate::ui::TerminalPhase::Exited => "exited",
                            crate::ui::TerminalPhase::Failed => "failed",
                            crate::ui::TerminalPhase::Unknown => "unknown",
                        },
                        if Some(*id) == self.selected {
                            " · active"
                        } else {
                            ""
                        }
                    ),
                )
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|(key, _, _)| key.id.0);
        rows
    }
    pub(in crate::app) fn mode(&self, id: PtyId) -> Option<TerminalPanelMode> {
        self.tabs.get(&id).map(|t| t.mode)
    }
    pub(in crate::app) fn running_count(&self) -> usize {
        self.tabs.values().filter(|t| t.running).count()
    }
    pub(in crate::app) fn cycle(&mut self, direction: i64) -> bool {
        let mut ids = self.descriptions();
        ids.retain(|(id, ..)| !self.side(PtyId(*id)));
        if ids.is_empty() {
            return false;
        }
        let current = self
            .selected
            .and_then(|id| ids.iter().position(|(i, ..)| *i == id.0))
            .unwrap_or(0);
        let next = if direction < 0 {
            current.checked_sub(1).unwrap_or(ids.len() - 1)
        } else {
            (current + 1) % ids.len()
        };
        let id = PtyId(ids[next].0);
        self.select(id) && self.display(id)
    }
    pub(in crate::app) fn request_open(
        &mut self,
        name: &str,
        workspace: &Workspace,
        file: Option<(&std::path::Path, usize)>,
        navigation: crate::app::windows::NavigationStamp,
        capture: Arc<()>,
    ) -> Result<TerminalIntent, String> {
        let config = self
            .policy
            .profiles
            .get(name)
            .ok_or("unknown terminal profile")?;
        let root = workspace
            .roots()
            .iter()
            .find(|r| Some(r.id) == workspace.active_root())
            .ok_or("terminal has no active workspace root")?;
        let (config, arguments) = expand_placeholders(config, &root.identity.canonical_path, file)?;
        let token = self.open_program(name, config, workspace, navigation, capture, false)?;
        self.intent
            .as_mut()
            .expect("new terminal intent")
            .open
            .arguments = arguments;
        Ok(token)
    }
    /// Opens `config` as it stands, for a program with no profile of its own.
    pub(in crate::app) fn open_program(
        &mut self,
        name: &str,
        config: crate::config::TerminalProfileConfig,
        workspace: &Workspace,
        navigation: crate::app::windows::NavigationStamp,
        capture: Arc<()>,
        agent: bool,
    ) -> Result<TerminalIntent, String> {
        if self.closed
            || self.next.is_none()
            || self.revision.is_none_or(|r| r == u64::MAX)
            || self.tabs.len() >= TERMINAL_QUEUE_CAPACITY
        {
            return Err("terminal source is closed, full or exhausted".into());
        }
        self.service.check_open().map_err(|e| e.to_string())?;
        let config = &config;
        if !(1..=1000).contains(&config.columns)
            || !(1..=1000).contains(&config.rows)
            || config.arguments.len() > 4096
            || config
                .arguments
                .iter()
                .fold(0usize, |n, s| n.saturating_add(s.len()))
                > 1024 * 1024
            || config.environment.iter().fold(0usize, |n, (k, v)| {
                n.saturating_add(k.len()).saturating_add(v.len())
            }) > 1024 * 1024
        {
            return Err("terminal profile exceeds its size limits".into());
        }
        let root = workspace
            .roots()
            .iter()
            .find(|r| Some(r.id) == workspace.active_root())
            .ok_or("terminal has no active workspace root")?;
        let cwd = config.cwd.as_ref().map_or_else(
            || root.identity.canonical_path.clone(),
            |p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    root.identity.canonical_path.join(p)
                }
            },
        );
        let request = ExecutionPreflightRequest {
            executable: crate::process::terminal_executable(&config.program, &cwd)
                .map_err(|error| error.to_string())?,
            cwd,
            roots: workspace
                .roots()
                .iter()
                .map(|r| r.identity.canonical_path.clone())
                .collect(),
            root_revision: workspace.root_revision(),
        };
        let id = PtyId(self.next.ok_or("terminal identity exhausted")?);
        self.next = id.0.checked_add(1);
        let token = TerminalIntent {
            key: TerminalKey {
                owner: self.owner.clone(),
                id,
            },
            profile: name.into(),
            agent,
        };
        self.intent = Some(Intent {
            token: token.clone(),
            open: Open {
                request,
                config: config.clone(),
                arguments: config.arguments.iter().map(Into::into).collect(),
            },
            scope: workspace.version(),
            epoch: self.epoch.clone(),
            navigation,
            capture,
        });
        Ok(token)
    }
    pub(in crate::app) fn cancel_open(&mut self) {
        self.intent = None;
    }
    pub(in crate::app) fn prepare(
        &mut self,
        token: TerminalIntent,
        workspace: &Workspace,
    ) -> Result<TerminalOpen, String> {
        workspace
            .execution_allowed()
            .map_err(|_| "terminals run only in a trusted workspace".to_owned())?;
        let source = self.intent.as_ref().ok_or("terminal intent closed")?;
        if self.closed
            || source.token != token
            || source.scope != workspace.version()
            || !Arc::ptr_eq(&source.epoch, &self.epoch)
            || self.revision.is_none_or(|r| r == u64::MAX)
            || self.tabs.len() >= TERMINAL_QUEUE_CAPACITY
        {
            return Err("terminal intent is stale, foreign, full or exhausted".into());
        }
        self.service.check_open().map_err(|e| e.to_string())?;
        let source = self.intent.take().expect("checked original intent");
        let key = source.token.key;
        let config = &source.open.config;
        let request = source.open.request.clone();
        let float = config.float;
        self.tabs.insert(
            key.id,
            TerminalRuntime {
                profile: source.token.profile,
                // The resolved executable, so the title names `zsh` rather than `$SHELL`.
                program: request.executable.clone(),
                agent: source.token.agent,
                cwd: request.cwd.clone(),
                close_on_exit: config.close_on_exit,
                debugger_owned: false,
                side: false,
                pty: None,
                scope: source.scope,
                epoch: source.epoch,
                requested_size: (config.columns.max(2), config.rows),
                geometry_source: None,
                next_resize: Some(1),
                pending_resizes: VecDeque::new(),
                pending_replies: Vec::new(),
                screen: PtyScreen::with_scrollback_bytes(
                    config.columns,
                    config.rows,
                    TERMINAL_SCROLLBACK_BYTES,
                ),
                pending: Some(source.open),
                running: false,
                stopping: false,
                exited: false,
                io_complete: false,
                failure: None,
                unknown: None,
                exit_code: None,
                truncated: false,
                mode: TerminalPanelMode::Input,
                normal_cursor: None,
                visual_anchor: None,
                visual_linewise: false,
                scroll_offset: 0,
            },
        );
        self.revision = self.revision.and_then(|r| r.checked_add(1));
        Ok(TerminalOpen {
            key,
            request,
            navigation: source.navigation,
            capture: source.capture,
            float,
        })
    }
    pub(in crate::app) fn preflight_count(&self) -> usize {
        self.preflights.len()
    }
    pub(in crate::app) fn take_preflight(
        &mut self,
        id: crate::process::PreflightId,
    ) -> Option<TerminalKey> {
        self.preflights.remove(&id)
    }
    pub(in crate::app) fn queue_preflight(
        &mut self,
        key: TerminalKey,
        request: ExecutionPreflightRequest,
        mut lane: crate::app::workspace::ExecutionLane<'_>,
    ) -> Result<(), String> {
        let id = self
            .resolve(&key)
            .ok_or("terminal preflight source is stale")?;
        let tab = &self.tabs[&id];
        let original = &tab
            .pending
            .as_ref()
            .ok_or("terminal preflight already consumed")?
            .request;
        if self.closed
            || !lane.current(&tab.scope)
            || self.preflights.len() >= 256
            || self.preflights.values().any(|pending| pending == &key)
            || !lane.preflight_available()?
            || original.executable != request.executable
            || original.cwd != request.cwd
            || original.roots != request.roots
            || original.root_revision != request.root_revision
        {
            return Err("terminal preflight source changed or full".into());
        }
        let id = lane.preflight(request).map_err(|error| error.to_string())?;
        self.preflights.insert(id, key);
        Ok(())
    }
    pub(in crate::app) fn launch(
        &mut self,
        key: TerminalKey,
        ready: ExecutionReady,
        workspace: &Workspace,
    ) -> Result<PtyId, String> {
        let id = self
            .resolve(&key)
            .ok_or("terminal open request is stale or foreign")?;
        let tab = self.tabs.get_mut(&id).ok_or("terminal closed")?;
        if self.closed || tab.scope != workspace.version() || !Arc::ptr_eq(&tab.epoch, &self.epoch)
        {
            return Err("terminal source is stale".into());
        }
        let open = tab
            .pending
            .as_ref()
            .ok_or("terminal preflight already consumed")?;
        let result = ready.into_execution_result(&open.request, workspace)?;
        let root = workspace
            .roots()
            .iter()
            .filter(|r| open.request.cwd.starts_with(&r.identity.canonical_path))
            .max_by_key(|r| r.identity.canonical_path.components().count())
            .ok_or("terminal cwd has no lexical root")?;
        if root.identity.canonical_path != result.root {
            return Err("terminal lexical and canonical roots differ".into());
        }
        let reservation = self.service.reserve().map_err(|e| e.to_string())?;
        let request = PtyRequest {
            program: result.executable,
            argv0: Some(open.request.executable.clone()),
            arguments: open.arguments.clone(),
            cwd: open.request.cwd.clone(),
            environment: self
                .environment
                .iter()
                .chain(&open.config.environment)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            columns: tab.requested_size.0,
            rows: tab.requested_size.1,
            root_revision: result.root_revision,
            policy: PtyPolicy {
                maximum_output_bytes: 0,
                maximum_input_bytes: TERMINAL_MAXIMUM_INPUT_BYTES,
                event_capacity: TERMINAL_QUEUE_CAPACITY,
                scrollback_bytes: TERMINAL_SCROLLBACK_BYTES,
            },
        };
        let pty = reservation
            .submit(request, Some(root.identity.clone()))
            .map_err(|e| e.to_string())?;
        tab.pty = Some(pty);
        tab.pending = None;
        self.revision = self.revision.and_then(|r| r.checked_add(1));
        Ok(id)
    }
    pub(in crate::app) fn request_stop(&mut self, id: PtyId) -> Result<bool, String> {
        let next_revision = self
            .revision
            .and_then(|revision| revision.checked_add(1))
            .ok_or("terminal revision exhausted")?;
        let tab = self.tabs.get_mut(&id).ok_or("terminal closed")?;
        let Some(pty) = tab.pty else {
            self.close(id);
            return Ok(false);
        };
        if tab.stopping {
            return Ok(true);
        }
        self.service.close(pty).map_err(|error| error.to_string())?;
        tab.stopping = true;
        tab.running = false;
        self.revision = Some(next_revision);
        Ok(true)
    }
    pub(in crate::app) fn stopping(&self, id: PtyId) -> bool {
        self.tabs.get(&id).is_some_and(|terminal| terminal.stopping)
    }
    pub(in crate::app) fn close(&mut self, id: PtyId) -> bool {
        self.preflights.retain(|_, key| key.id != id);
        let Some(tab) = self.tabs.remove(&id) else {
            return false;
        };
        if let Some(pty) = tab.pty {
            let _ = self.service.close(pty);
            self.closed_ptys.insert(pty);
        }
        self.revision = self.revision.and_then(|r| r.checked_add(1));
        if self.selected == Some(id) {
            self.selected = self.tabs.keys().min_by_key(|id| id.0).copied();
        }
        if self.displayed == Some(id) {
            self.displayed = self
                .selected
                .filter(|selected| self.contains(*selected) && !self.side(*selected))
                .or_else(|| self.panel_fallback());
        }
        true
    }
    pub(in crate::app) fn belongs_to_root(&self, root: &std::path::Path) -> bool {
        self.tabs.values().any(|tab| tab.cwd.starts_with(root))
    }
    pub(in crate::app) fn running_in_root(&self, root: &std::path::Path) -> usize {
        self.tabs
            .values()
            .filter(|tab| (tab.running || tab.pty.is_some()) && tab.cwd.starts_with(root))
            .count()
    }
    pub(in crate::app) fn ids_in_root(&self, root: &std::path::Path) -> Vec<PtyId> {
        self.tabs
            .iter()
            .filter_map(|(id, tab)| tab.cwd.starts_with(root).then_some(*id))
            .collect()
    }
    pub(in crate::app) fn close_root(&mut self, root: &std::path::Path) {
        let ids = self
            .tabs
            .iter()
            .filter_map(|(id, tab)| tab.cwd.starts_with(root).then_some(*id))
            .collect::<Vec<_>>();
        for id in ids {
            self.close(id);
        }
    }
    pub(in crate::app) fn retain_workspace(&mut self, workspace: &Workspace) {
        self.preflights.clear();
        self.cancel_open();
        self.cancel_close();
        let pending = self
            .tabs
            .iter()
            .filter_map(|(id, tab)| tab.pending.is_some().then_some(*id))
            .collect::<Vec<_>>();
        for id in pending {
            self.close(id);
        }
        for tab in self.tabs.values_mut() {
            tab.scope = workspace.version();
        }
    }
    /// New terminals open from `policy`; the running ones keep going as they were started.
    pub(in crate::app) fn configure(&mut self, policy: &TerminalsConfig) {
        self.policy = policy.clone();
    }
    pub(in crate::app) fn begin_shutdown(&mut self) {
        self.preflights.clear();
        self.cancel_open();
        self.cancel_close();
        self.closed = true;
        self.service.begin_shutdown();
    }
    pub(in crate::app) fn expire_shutdown(&mut self) -> Vec<TerminalUpdate> {
        let ids = self
            .tabs
            .iter()
            .filter_map(|(id, terminal)| terminal.pty.is_some().then_some(*id))
            .collect::<Vec<_>>();
        ids.into_iter()
            .map(|id| {
                let message = "shared shutdown deadline expired before PTY exit and I/O completion"
                    .to_owned();
                self.close(id);
                TerminalUpdate::Unknown(id, message)
            })
            .collect()
    }
    pub(in crate::app) fn pending(&self) -> usize {
        self.service.pending()
    }
    pub(in crate::app) fn reject(&mut self, key: &TerminalKey) {
        if let Some(id) = self.resolve(key) {
            self.close(id);
        }
    }
    pub(in crate::app) fn resize(
        &mut self,
        id: PtyId,
        columns: u16,
        rows: u16,
        source: TerminalGeometrySource,
        workspace: &Workspace,
    ) -> Result<(), String> {
        let Some(tab) = self.tabs.get_mut(&id) else {
            return Ok(());
        };
        if tab.scope != workspace.version() || !Arc::ptr_eq(&tab.epoch, &self.epoch) {
            return Err("terminal resize source is stale".into());
        }
        if columns == 0 || rows == 0 || usize::from(columns) * usize::from(rows) > 1_000_000 {
            return Err("terminal geometry is empty or exceeds its cell limit".into());
        }
        let columns = columns.max(2);
        let same_size = tab.requested_size == (columns, rows);
        let same_source = tab.geometry_source.as_ref() == Some(&source);
        let acknowledged = tab.screen.dimensions() == (usize::from(columns), usize::from(rows));
        if same_size && same_source && (acknowledged || !tab.pending_resizes.is_empty()) {
            return Ok(());
        }
        if same_size && !same_source && acknowledged && tab.pending_resizes.is_empty() {
            tab.geometry_source = Some(source);
            return Ok(());
        }
        if let Some(pty) = tab.pty {
            if tab.pending_resizes.len() >= 256 {
                return Err("terminal resize acknowledgement queue is full".into());
            }
            let sequence = tab
                .next_resize
                .ok_or("terminal resize identity exhausted")?;
            match self.service.resize_checked(pty, sequence, columns, rows) {
                // A shell that is ending has no screen left to fit.
                Err(crate::process::PtyError::Stopped) => return Ok(()),
                result => result.map_err(|e| e.to_string())?,
            }
            tab.next_resize = sequence.checked_add(1);
            tab.pending_resizes.push_back(PendingResize {
                terminal: id,
                pty,
                source: source.clone(),
                sequence,
                columns,
                rows,
            });
        } else {
            tab.screen.resize_tracked(
                columns,
                rows,
                &mut tab.normal_cursor,
                &mut tab.visual_anchor,
                &mut tab.scroll_offset,
            );
            keep_terminal_cursor_visible(tab);
        }
        tab.geometry_source = Some(source);
        tab.requested_size = (columns, rows);
        Ok(())
    }
    pub(in crate::app) fn set_theme(&mut self, theme: &crate::config::ThemeConfig) {
        for tab in self.tabs.values_mut() {
            tab.screen.set_theme(theme);
        }
    }
    pub(in crate::app) fn sync_deadline(&self) -> Option<std::time::Instant> {
        self.tabs
            .values()
            .filter_map(|tab| tab.screen.sync_deadline())
            .chain(self.input_hint_until)
            .min()
    }
    pub(in crate::app) fn drain(
        &mut self,
        workspace: &Workspace,
        current_geometry: impl Fn(PtyId) -> Option<TerminalGeometrySource>,
    ) -> Vec<TerminalUpdate> {
        let stale = self
            .tabs
            .iter()
            .filter_map(|(id, t)| (t.scope != workspace.version()).then_some(*id))
            .collect::<Vec<_>>();
        let mut updates = Vec::new();
        if self
            .input_hint_until
            .is_some_and(|until| until <= std::time::Instant::now())
        {
            self.input_hint_until = None;
            updates.push(TerminalUpdate::Changed);
        }
        for id in stale {
            self.close(id);
            updates.push(TerminalUpdate::Closed(id));
        }
        for tab in self.tabs.values_mut() {
            if tab
                .screen
                .sync_deadline()
                .is_some_and(|deadline| deadline <= std::time::Instant::now())
            {
                tab.feed_bytes(&[]);
                updates.push(TerminalUpdate::Changed);
            }
        }
        for event in self.service.drain_events() {
            let pty = match &event {
                PtyEvent::Started { id, .. }
                | PtyEvent::Output { id, .. }
                | PtyEvent::Exited { id, .. }
                | PtyEvent::IoComplete { id, .. }
                | PtyEvent::Failed { id, .. }
                | PtyEvent::Unknown { id, .. }
                | PtyEvent::Resized { id, .. } => *id,
            };
            if self.closed_ptys.contains(&pty) {
                if matches!(
                    event,
                    PtyEvent::IoComplete { .. }
                        | PtyEvent::Failed { .. }
                        | PtyEvent::Unknown { .. }
                ) {
                    self.closed_ptys.remove(&pty);
                }
                continue;
            }
            let Some(id) = self
                .tabs
                .iter()
                .find_map(|(id, t)| (t.pty == Some(pty)).then_some(*id))
            else {
                continue;
            };
            let tab = self.tabs.get_mut(&id).expect("located source");
            match event {
                PtyEvent::Started { .. } => {
                    if !tab.stopping {
                        tab.running = true;
                        updates.push(TerminalUpdate::Started(id));
                    }
                }
                PtyEvent::Output {
                    bytes, truncated, ..
                } => {
                    // Answers to the program's questions go straight back to it.
                    tab.feed_bytes(&bytes);
                    tab.truncated |= truncated;
                    updates.push(TerminalUpdate::Changed);
                }
                PtyEvent::Resized {
                    sequence,
                    columns,
                    rows,
                    ..
                } => {
                    let reply = ResizeReply {
                        terminal: id,
                        pty,
                        sequence,
                        columns,
                        rows,
                    };
                    let current = current_geometry(id);
                    if !acknowledge_resize(
                        &mut tab.pending_resizes,
                        &mut tab.screen,
                        reply,
                        current.as_ref(),
                        &mut tab.normal_cursor,
                        &mut tab.visual_anchor,
                        &mut tab.scroll_offset,
                    ) {
                        continue;
                    }
                    keep_terminal_cursor_visible(tab);
                    updates.push(TerminalUpdate::Changed);
                }
                PtyEvent::Exited { code, .. } => {
                    if tab.exited
                        || tab.io_complete
                        || tab.failure.is_some()
                        || tab.unknown.is_some()
                    {
                        continue;
                    }
                    tab.exited = true;
                    tab.running = false;
                    tab.exit_code = code;
                    updates.push(TerminalUpdate::Exited(id, code));
                }
                PtyEvent::IoComplete { result, .. } => {
                    if tab.io_complete || tab.failure.is_some() || tab.unknown.is_some() {
                        continue;
                    }
                    let stopping = tab.stopping;
                    // A debugged program's output stays to be read until the next run, and how it
                    // exited is the program's business, not a terminal failure.
                    let keep_output = tab.debugger_owned;
                    tab.pty = None;
                    tab.io_complete = true;
                    updates.push(TerminalUpdate::IoComplete(id));
                    if let Err(message) = result {
                        tab.failure = Some(message.clone());
                        tab.truncated = true;
                        updates.push(TerminalUpdate::Failed(id, message));
                    } else if !stopping
                        && !keep_output
                        && tab.exit_code.is_some_and(|code| code != 0)
                    {
                        let message = format!(
                            "terminal exited with status {}",
                            tab.exit_code.expect("nonzero exit status")
                        );
                        tab.failure = Some(message.clone());
                        updates.push(TerminalUpdate::Failed(id, message));
                    }
                    if stopping
                        || (!keep_output
                            && tab.close_on_exit
                            && tab.failure.is_none()
                            && tab.exit_code == Some(0))
                    {
                        self.close(id);
                        updates.push(TerminalUpdate::Closed(id));
                    }
                }
                PtyEvent::Failed { message, .. } => {
                    if tab.io_complete || tab.failure.is_some() || tab.unknown.is_some() {
                        continue;
                    }
                    let stopping = tab.stopping;
                    tab.pty = None;
                    tab.running = false;
                    tab.failure = Some(message.clone());
                    tab.truncated = true;
                    updates.push(TerminalUpdate::Failed(id, message));
                    if stopping {
                        self.close(id);
                        updates.push(TerminalUpdate::Closed(id));
                    }
                }
                PtyEvent::Unknown { message, .. } => {
                    if tab.io_complete || tab.failure.is_some() || tab.unknown.is_some() {
                        continue;
                    }
                    let stopping = tab.stopping;
                    tab.pty = None;
                    tab.running = false;
                    tab.unknown = Some(message.clone());
                    if stopping {
                        self.close(id);
                        updates.push(TerminalUpdate::StopUnknown(id, message));
                    } else {
                        updates.push(TerminalUpdate::Unknown(id, message));
                    }
                }
            }
        }
        for tab in self.tabs.values_mut() {
            tab.flush_replies(&self.service);
        }
        if updates
            .iter()
            .any(|update| !matches!(update, TerminalUpdate::Changed))
        {
            self.revision = self.revision.and_then(|r| r.checked_add(1));
        }
        updates
    }

    pub(in crate::app) fn set_debugger_owned(&mut self, id: PtyId) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.debugger_owned = true;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingResize {
    terminal: PtyId,
    pty: PtyId,
    source: TerminalGeometrySource,
    sequence: u64,
    columns: u16,
    rows: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResizeReply {
    terminal: PtyId,
    pty: PtyId,
    sequence: u64,
    columns: u16,
    rows: u16,
}

fn acknowledge_resize(
    pending: &mut VecDeque<PendingResize>,
    screen: &mut PtyScreen,
    reply: ResizeReply,
    current: Option<&TerminalGeometrySource>,
    cursor: &mut Option<(usize, usize)>,
    anchor: &mut Option<(usize, usize)>,
    offset: &mut usize,
) -> bool {
    let Some(expected) = pending.front() else {
        return false;
    };
    if (
        expected.terminal,
        expected.pty,
        expected.sequence,
        expected.columns,
        expected.rows,
    ) != (
        reply.terminal,
        reply.pty,
        reply.sequence,
        reply.columns,
        reply.rows,
    ) {
        return false;
    }
    let expected = pending.pop_front().expect("matched pending resize");
    screen.resize_tracked(reply.columns, reply.rows, cursor, anchor, offset);
    current == Some(&expected.source)
}

struct TerminalRuntime {
    profile: String,
    program: std::path::PathBuf,
    agent: bool,
    cwd: std::path::PathBuf,
    close_on_exit: bool,
    debugger_owned: bool,
    side: bool,
    pty: Option<PtyId>,
    scope: WorkspaceVersion,
    epoch: Arc<()>,
    pending: Option<Open>,
    requested_size: (u16, u16),
    geometry_source: Option<TerminalGeometrySource>,
    next_resize: Option<u64>,
    pending_resizes: VecDeque<PendingResize>,
    pending_replies: Vec<u8>,
    screen: PtyScreen,
    running: bool,
    stopping: bool,
    exited: bool,
    io_complete: bool,
    failure: Option<String>,
    unknown: Option<String>,
    exit_code: Option<u32>,
    truncated: bool,
    mode: TerminalPanelMode,
    normal_cursor: Option<(usize, usize)>,
    visual_anchor: Option<(usize, usize)>,
    visual_linewise: bool,
    scroll_offset: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) struct TerminalScreenState {
    pub(in crate::app) cursor: (usize, usize),
    pub(in crate::app) alternate: bool,
    pub(in crate::app) cursor_visible: bool,
    pub(in crate::app) application_cursor: bool,
    pub(in crate::app) bracketed_paste: bool,
    pub(in crate::app) mouse_mode: crate::ui::PtyMouseMode,
    pub(in crate::app) sgr_mouse: bool,
}

#[cfg(test)]
pub(in crate::app) struct TerminalFixture {
    pub(in crate::app) profile: String,
    pub(in crate::app) screen: PtyScreen,
    pub(in crate::app) running: bool,
    pub(in crate::app) exit_code: Option<u32>,
    pub(in crate::app) truncated: bool,
}
#[cfg(test)]
impl Terminals {
    pub(in crate::app) fn fixture(
        &mut self,
        id: PtyId,
        fixture: TerminalFixture,
        scope: WorkspaceVersion,
    ) {
        assert!(!self.tabs.contains_key(&id));
        let (columns, rows) = fixture.screen.dimensions();
        self.tabs.insert(
            id,
            TerminalRuntime {
                program: fixture.profile.clone().into(),
                profile: fixture.profile,
                agent: false,
                cwd: std::path::PathBuf::new(),
                close_on_exit: false,
                debugger_owned: false,
                side: false,
                pty: None,
                scope,
                epoch: self.epoch.clone(),
                pending: None,
                requested_size: (columns as u16, rows as u16),
                geometry_source: None,
                next_resize: Some(1),
                pending_resizes: VecDeque::new(),
                pending_replies: Vec::new(),
                screen: fixture.screen,
                running: fixture.running,
                stopping: false,
                exited: fixture.exit_code.is_some(),
                io_complete: fixture.exit_code.is_some(),
                failure: None,
                unknown: None,
                exit_code: fixture.exit_code,
                truncated: fixture.truncated,
                mode: TerminalPanelMode::Input,
                normal_cursor: None,
                visual_anchor: None,
                visual_linewise: false,
                scroll_offset: 0,
            },
        );
        self.next = self.next.and_then(|next| next.max(id.0).checked_add(1));
        self.revision = self.revision.and_then(|r| r.checked_add(1));
    }
    pub(in crate::app) fn fixture_cwd(&mut self, id: PtyId, cwd: std::path::PathBuf) {
        self.tabs.get_mut(&id).unwrap().cwd = cwd;
    }
    pub(in crate::app) fn fixture_dimensions(&self, id: PtyId) -> (usize, usize) {
        self.tabs[&id].screen.dimensions()
    }
    pub(in crate::app) fn fixture_pending_resizes(&self, id: PtyId) -> usize {
        self.tabs[&id].pending_resizes.len()
    }
    pub(in crate::app) fn fixture_pending_resize_matches(
        &self,
        id: PtyId,
        source: &TerminalGeometrySource,
    ) -> bool {
        let tab = &self.tabs[&id];
        tab.geometry_source.as_ref() == Some(source)
            && tab
                .pending_resizes
                .back()
                .is_some_and(|pending| &pending.source == source)
    }
    pub(in crate::app) fn fixture_screen_state(&self, id: PtyId) -> TerminalScreenState {
        let screen = &self.tabs[&id].screen;
        TerminalScreenState {
            cursor: screen.cursor(),
            alternate: screen.alternate_active(),
            cursor_visible: screen.cursor_visible(),
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_mode: screen.mouse_mode(),
            sgr_mouse: screen.sgr_mouse(),
        }
    }
    pub(in crate::app) fn fixture_lines(&self, id: PtyId) -> Vec<String> {
        self.tabs[&id].screen.lines()
    }
}

impl TerminalRuntime {
    pub(in crate::app) fn feed_bytes(&mut self, bytes: &[u8]) {
        self.screen.feed_bytes_tracked(
            bytes,
            &mut self.normal_cursor,
            &mut self.visual_anchor,
            &mut self.scroll_offset,
        );
        if self.mode == TerminalPanelMode::Visual && self.visual_anchor.is_none() {
            self.mode = TerminalPanelMode::Normal;
        }
        if self.mode != TerminalPanelMode::Input {
            if let Some(cursor) = self.normal_cursor {
                self.normal_cursor = Some(clamp_terminal_cursor(&self.screen, cursor));
            }
            keep_terminal_cursor_visible(self);
        }
        self.pending_replies.extend(self.screen.take_replies());
    }
    fn flush_replies(&mut self, service: &PtyWorker) {
        if !self.pending_replies.is_empty()
            && let Some(pty) = self.pty
            && let Ok(reservation) = service.reserve_input(pty)
        {
            let count = self.pending_replies.len().min(TERMINAL_MAXIMUM_INPUT_BYTES);
            if reservation
                .submit(self.pending_replies[..count].to_vec())
                .is_ok()
            {
                self.pending_replies.drain(..count);
            }
        }
    }
    pub(in crate::app) fn phase(&self) -> crate::ui::TerminalPhase {
        use crate::ui::TerminalPhase;
        if self.stopping {
            TerminalPhase::Stopping
        } else if self.running {
            TerminalPhase::Running
        } else if self.pending.is_some() {
            TerminalPhase::Preflight
        } else if self.unknown.is_some() {
            TerminalPhase::Unknown
        } else if self.failure.is_some() {
            TerminalPhase::Failed
        } else if self.exited {
            TerminalPhase::Exited
        } else if self.pty.is_some() {
            TerminalPhase::Starting
        } else {
            TerminalPhase::Failed
        }
    }
}

/// Fills `{root}`, `{file}`, `{dir}` and `{line}` in a profile's arguments and working
/// directory, so one profile can open a tool on the current file (`lazygit -f {file}`).
fn expand_placeholders(
    config: &crate::config::TerminalProfileConfig,
    root: &std::path::Path,
    file: Option<(&std::path::Path, usize)>,
) -> Result<
    (
        crate::config::TerminalProfileConfig,
        Vec<std::ffi::OsString>,
    ),
    String,
> {
    let expand = |value: &std::ffi::OsStr| -> Result<std::ffi::OsString, String> {
        #[cfg(unix)]
        use std::os::unix::ffi::OsStrExt;
        #[cfg(unix)]
        use std::os::unix::ffi::OsStringExt;
        #[cfg(windows)]
        use std::os::windows::ffi::OsStrExt;
        #[cfg(windows)]
        use std::os::windows::ffi::OsStringExt;
        #[cfg(unix)]
        let bytes = value.as_bytes();
        #[cfg(windows)]
        let bytes: Vec<u16> = value.encode_wide().collect();
        let native_slice = |range: std::ops::Range<usize>| {
            #[cfg(unix)]
            {
                std::ffi::OsString::from_vec(bytes[range].to_vec())
            }
            #[cfg(windows)]
            {
                std::ffi::OsString::from_wide(&bytes[range])
            }
        };
        let mut expanded = std::ffi::OsString::new();
        let mut copied = 0;
        let mut index = 0;
        while index < bytes.len() {
            let placeholder =
                ["{root}", "{file}", "{dir}", "{line}"]
                    .into_iter()
                    .find(|placeholder| {
                        bytes.len() - index >= placeholder.len()
                            && placeholder
                                .bytes()
                                .zip(&bytes[index..])
                                .all(|(a, b)| u32::from(a) == u32::from(*b))
                    });
            let Some(placeholder) = placeholder else {
                index += 1;
                continue;
            };
            // ASCII delimiters are valid native-string boundaries on every platform.
            expanded.push(native_slice(copied..index));
            if placeholder == "{root}" {
                expanded.push(root);
            } else {
                let (path, line) = file.ok_or("this terminal profile needs a file buffer")?;
                match placeholder {
                    "{file}" => expanded.push(path),
                    "{dir}" => expanded.push(path.parent().unwrap_or(root)),
                    _ => expanded.push(line.to_string()),
                }
            }
            index += placeholder.len();
            copied = index;
        }
        expanded.push(native_slice(copied..bytes.len()));
        Ok(expanded)
    };
    let arguments = config
        .arguments
        .iter()
        .map(|s| expand(s.as_ref()))
        .collect::<Result<_, _>>()?;
    let mut expanded = config.clone();
    if let Some(cwd) = &config.cwd {
        expanded.cwd = Some(expand(cwd.as_os_str())?.into());
    }
    Ok((expanded, arguments))
}

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::TrustedAction;
use crate::app::windows::NativeSurfaceTarget;
use crate::ui::PromptKind;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
// Remaining editor/window routing; terminal data and algorithms live in the owner.
use crate::editor::RegisterShape;
use crate::editor::RegisterValue;

impl App {
    /// Shows a terminal and gives it the keys, bringing its window back if it was hidden.
    pub(in crate::app) fn reveal_terminal(&mut self, id: PtyId) {
        self.terminal_session.select(id);
        self.terminal_session.display(id);
        if self.terminal_rect(id).is_none()
            && let Err(error) = self.show_native_window(NativeSurfaceTarget::Terminal(id.0))
        {
            self.show_error(error);
            return;
        }
        self.set_terminal_focus(true);
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
    }
}

impl App {
    pub(in crate::app) fn close_terminal(&mut self, id: PtyId) {
        if self.terminal_session.stopping(id) {
            self.show_error("terminal stop is already pending");
        } else if self.terminal_session.requires_close_confirmation(id) {
            if self.input.prompt().is_some() {
                self.show_error("close the active prompt before closing a running terminal");
            } else if self.terminal_session.begin_close(id) {
                self.open_confirmation_actions(
                    PromptKind::TerminalClose,
                    format!("Terminal #{} is active", id.0),
                    "Hide this terminal or stop its process?".into(),
                    crate::ui::ConfirmationChoice::No,
                    "Stop",
                    "Hide",
                );
            }
        } else {
            self.terminal_session.cancel_open();
            self.stop_or_remove_terminal(id, true);
        }
    }
}

impl App {
    pub(in crate::app) fn submit_terminal_close(
        &mut self,
        selected: crate::ui::ConfirmationChoice,
    ) {
        let source = self.terminal_session.take_close();
        // Restore the pre-prompt focus first; Hide or Stop then performs the
        // accepted final focus transition without the prompt lease undoing it.
        self.close_prompt();
        let id = match source {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        match selected {
            crate::ui::ConfirmationChoice::Yes => {
                match self
                    .window_session
                    .require_native_stop(NativeSurfaceTarget::Terminal(id.0))
                {
                    Ok(()) => {
                        self.terminal_session.cancel_open();
                        self.stop_or_remove_terminal(id, true);
                    }
                    Err(error) => self.show_error(error),
                }
            }
            crate::ui::ConfirmationChoice::No => {
                if let Err(error) = self.window_session.place_native(
                    NativeSurfaceTarget::Terminal(id.0),
                    WindowPlacement::Hidden,
                    &mut self.state,
                ) {
                    self.show_error(error);
                }
            }
        }
    }
}

impl App {
    fn terminal_command_target(&self) -> Option<PtyId> {
        self.keyboard_terminal()
            .or(self.terminal_session.selected())
    }
}

impl App {
    pub(in crate::app) fn reject_terminal_responses(&mut self, reason: &str) {
        self.debugger_session.reject_terminals(reason);
    }
}

impl App {
    /// Shows the terminal as a centered float covering most of the screen, like LazyVim's
    /// lazygit window.
    fn float_terminal(&mut self, id: PtyId) {
        self.terminal_session.display(id);
        let (width, height) = (self.window_session.width(), self.window_session.height());
        let rect = WindowRect {
            x: width / 20,
            y: height / 20,
            width: width - width / 10,
            height: height - height / 10,
        };
        self.window_session
            .prefer_terminal_placement(id.0, WindowPlacement::Floating(rect));
    }
}

impl App {
    /// A terminal running an agent profile's own program, for `agents.connection = "terminal"`.
    pub(in crate::app) fn agent_terminal(&self, id: PtyId) -> bool {
        self.terminal_session.is_agent(id)
    }
}

impl App {
    /// An agent's terminal opens at the agent panel's width on its side, where the ACP panel goes.
    fn dock_agent_terminal(&mut self, id: PtyId) {
        self.terminal_session.set_side(id);
        let width = u32::from(self.window_session.width());
        let share = width * u32::from(self.configuration.settings().agents.panel_width) / 100;
        // A conversation needs about 40 columns, however narrow the terminal is.
        let size = share.max(40).min(width * 7 / 10).max(1);
        self.window_session.prefer_terminal_placement(
            id.0,
            WindowPlacement::Docked {
                edge: match self.configuration.settings().agents.side {
                    crate::config::PanelSide::Left => DockEdge::Left,
                    crate::config::PanelSide::Right => DockEdge::Right,
                },
                size: u16::try_from(size).unwrap_or(u16::MAX),
            },
        );
    }
}

impl App {
    pub(in crate::app) fn stop_or_remove_terminal(&mut self, id: PtyId, request_close: bool) {
        let focused = self.window_session.focused_terminal() == Some(id.0);
        if request_close {
            match self.terminal_session.request_stop(id) {
                Ok(true) => {
                    if focused {
                        self.set_terminal_focus(false);
                    }
                    self.sync_terminal_panel();
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        } else {
            self.terminal_session.close(id);
        }
        if let Some(reply) = self.debugger_session.take_terminal(&id) {
            let _ = reply.respond(
                false,
                Some("terminal closed before startup response".into()),
            );
        }
        if focused {
            self.set_terminal_focus(self.terminal_session.selected().is_some());
        }
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.ensure_cursor_visible();
    }
}

impl App {
    pub(in crate::app) fn enter_terminal_normal_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_normal_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
}

impl App {
    pub(in crate::app) fn enter_terminal_input_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_input_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
}

impl App {
    pub(in crate::app) fn move_terminal_normal_cursor(&mut self, motion: &str) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        if let Err(error) = self
            .terminal_session
            .move_terminal_normal_cursor(id, motion)
        {
            self.show_error(error);
        }
        self.sync_terminal_panel();
    }
}

impl App {
    pub(in crate::app) fn start_terminal_visual(&mut self, linewise: bool) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.start_terminal_visual(id, linewise);
        }
        self.sync_terminal_panel();
    }
}

impl App {
    pub(in crate::app) fn yank_terminal_text(&mut self, line: bool) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        let Some((text, linewise)) = self.terminal_session.yank_terminal_text(id, line) else {
            return;
        };
        let policy = self.state.policy;
        self.state.registers.set(
            '"',
            RegisterValue {
                text,
                shape: if linewise {
                    RegisterShape::Line
                } else {
                    RegisterShape::Character
                },
            },
            policy.registers_enabled,
            policy.mirror_named_to_unnamed,
        );
        self.state.mark_editor_changed();
        self.notify_message(self.message("terminal_yanked", &[]), false);
        self.sync_terminal_panel();
    }
}

impl App {
    pub(in crate::app) fn sync_terminal_panel(&mut self) {
        let focused = self.window_session.focused_terminal();
        if focused.is_some() && !self.terminal_session.input_hint_seen {
            self.terminal_session.input_hint_seen = true;
            let duration = self
                .configuration
                .settings()
                .ui
                .terminal_panel
                .input_hint_ms
                .unwrap_or(0);
            self.terminal_session.input_hint_until = std::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(u64::from(duration)));
        }
        let hint = self
            .terminal_session
            .input_hint_until
            .filter(|until| *until > std::time::Instant::now())
            .and_then(|_| {
                self.configuration
                    .keymaps()
                    .binding_for("terminal", "terminal.mode.normal")
            })
            .map(|keys| {
                self.message(
                    "terminal_input_hint",
                    &[("key", crate::app::picker::palette::readable_keys(keys))],
                )
            });
        let height = self.window_session.terminal_height();
        self.ui.terminal_panel = self.displayed_terminal().and_then(|id| {
            self.terminal_session
                .projection(id, focused == Some(id.0), height)
        });
        if let Some(panel) = &mut self.ui.terminal_panel {
            if panel.focused && panel.mode == TerminalPanelMode::Input {
                panel.input_hint = hint;
            }
        } else {
            self.set_terminal_focus(false);
        }
        self.reconcile_editor_windows();
        let active = self.ui.terminal_panel.as_ref().map(|terminal| terminal.id);
        self.ui.other_terminals = self
            .shown_terminal_windows()
            .into_iter()
            .filter(|(id, _)| Some(id.0) != active)
            .filter_map(|(id, window)| {
                Some((window, self.terminal_session.projection(id, false, height)?))
            })
            .collect();
    }
}

impl App {
    /// Every terminal with a window on screen, the active one first.
    pub(in crate::app) fn shown_terminal_windows(&self) -> Vec<(PtyId, crate::windows::WindowId)> {
        let Some(projection) = self.compositor_projection() else {
            return Vec::new();
        };
        self.displayed_terminal()
            .into_iter()
            .chain(self.terminal_session.displayed())
            .chain(self.terminal_session.side_terminals())
            .fold(Vec::new(), |mut ids, id| {
                if !ids.contains(&id) {
                    ids.push(id);
                }
                ids
            })
            .into_iter()
            .filter_map(|id| {
                let window = self.window_session.native_ids().terminal(id.0)?.window;
                projection.window(window).map(|_| (id, window))
            })
            .collect()
    }
}

impl App {
    pub(in crate::app) fn request_terminal_open(
        &mut self,
        profile: &str,
        reply: Option<crate::debugger::DapTerminalReply>,
    ) {
        if reply.as_ref().is_some_and(|r| !r.current()) {
            self.show_error("requesting debugger closed");
            return;
        }
        let file = self.state.active_view().and_then(|view| {
            let view = &self.state.views[view];
            let buffer = &self.state.buffers[view.buffer];
            let line = buffer
                .text
                .byte_to_line(view.cursor)
                .map_or(1, |line| line + 1);
            buffer.path.as_deref().map(|path| (path, line))
        });
        match self.terminal_session.request_open(
            profile,
            &self.workspace,
            file,
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view()),
            self.input.capture_epoch(),
        ) {
            Ok(intent) => self.run_trusted(if let Some(reply) = reply {
                TrustedAction::OpenDapTerminal { intent, reply }
            } else {
                TrustedAction::OpenTerminal(intent)
            }),
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.respond(false, Some(error.clone()));
                }
                self.show_error(error);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn open_terminal_with_response(
        &mut self,
        intent: crate::app::terminal::TerminalIntent,
        reply: Option<crate::debugger::DapTerminalReply>,
    ) {
        if reply.as_ref().is_some_and(|r| !r.current()) {
            self.show_error("the debugger that asked for this terminal has stopped");
            return;
        }
        let for_debugger = reply.is_some();
        // A debugged program's output stays docked under the source it is stepping through.
        let placement = if for_debugger {
            crate::config::TerminalPlacement::Bottom
        } else {
            self.configuration.settings().ui.terminal_panel.placement
        };
        match self.terminal_session.prepare(intent, &self.workspace) {
            Ok(open) => {
                let id = self
                    .terminal_session
                    .resolve(&open.key)
                    .expect("new terminal");
                match self.terminal_session.queue_preflight(
                    open.key.clone(),
                    open.request,
                    self.workspace
                        .execution_lane(crate::app::workspace::DiscoveryKind::Language),
                ) {
                    Ok(()) => {
                        if let Some(reply) = reply {
                            self.terminal_session.set_debugger_owned(id);
                            self.debugger_session.register_terminal(id, reply);
                            self.close_debug_terminal();
                            self.debug_terminal = Some(id);
                        }
                        if self.window_session.navigation_current(
                            &open.navigation,
                            &self.state.window_version(),
                            self.state.active_view(),
                        ) && self.input.capture_current(&open.capture)
                            && self.input.capture_context().is_none()
                            && self.ui.picker.is_none()
                            && self.ui.tool_menu.is_none()
                            && self.ui.settings.is_none()
                        {
                            self.terminal_session.select(id);
                            self.terminal_session.display(id);
                            if self.agent_terminal(id) {
                                self.dock_agent_terminal(id);
                            } else if open.float
                                || placement == crate::config::TerminalPlacement::Float
                            {
                                self.float_terminal(id);
                            } else if placement == crate::config::TerminalPlacement::Fullscreen {
                                self.window_session
                                    .prefer_terminal_placement(id.0, WindowPlacement::Fullscreen);
                            }
                            // A debugged program's terminal shows, and the user stays on the source.
                            if !for_debugger {
                                self.set_terminal_focus(true);
                            }
                        }
                        self.resize_active_terminal_to_panel();
                        self.sync_terminal_panel();
                        self.sync_explorer_ui();
                        self.ensure_cursor_visible();
                    }
                    Err(error) => {
                        self.terminal_session.reject(&open.key);
                        if let Some(reply) = reply {
                            let _ = reply.respond(false, Some(error.to_string()));
                        }
                        self.show_error(error);
                    }
                }
            }
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.respond(false, Some(error.clone()));
                }
                self.show_error(error);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn process_pty_events(&mut self) {
        self.terminal_session.set_theme(self.configuration.theme());
        let windows = &self.window_session;
        let updates = self
            .terminal_session
            .drain(&self.workspace, |id| windows.terminal_geometry_source(id.0));
        if updates.is_empty() {
            return;
        }
        // What ran there may have committed or switched branch.
        let ended = updates.iter().any(|update| {
            matches!(
                update,
                TerminalUpdate::Exited(..)
                    | TerminalUpdate::IoComplete(_)
                    | TerminalUpdate::Closed(_)
            )
        });
        for update in updates {
            match update {
                TerminalUpdate::Started(id) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id)
                        && let Err(error) = reply.respond(true, None)
                    {
                        self.show_error(error);
                    }
                }
                // Nothing is left to type to; keys go back to the source.
                TerminalUpdate::Exited(id, _)
                    if self.debug_terminal == Some(id)
                        && self.window_session.focused_terminal() == Some(id.0) =>
                {
                    self.set_terminal_focus(false);
                }
                TerminalUpdate::Exited(_, _) | TerminalUpdate::IoComplete(_) => {}
                TerminalUpdate::StopUnknown(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ =
                            reply.respond(false, Some("terminal stop outcome is unknown".into()));
                    }
                    self.show_error(format!("terminal stop outcome is unknown: {message}"));
                }
                TerminalUpdate::Failed(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some(message.clone()));
                    }
                    self.show_error(message);
                }
                TerminalUpdate::Unknown(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some("terminal outcome is unknown".into()));
                    }
                    self.show_error(format!("terminal #{} outcome is unknown: {message}", id.0));
                }
                TerminalUpdate::Closed(id) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some("terminal source closed".into()));
                    }
                    if self.window_session.focused_terminal() == Some(id.0) {
                        self.set_terminal_focus(false);
                    }
                }
                TerminalUpdate::Changed => {}
            }
        }
        if ended {
            self.start_git_status_refresh_with_feedback(false);
        }
        self.sync_terminal_panel();
        self.resize_active_terminal_to_panel();
    }
}

impl App {
    pub(in crate::app) fn set_terminal_focus(&mut self, focused: bool) {
        if focused {
            if let Some(id) = self.terminal_session.selected() {
                self.terminal_session.display(id);
                self.window_session.focus_terminal(id.0);
            } else {
                self.window_session.leave_terminal();
            }
        } else {
            self.window_session.leave_terminal();
        }
        self.project_window_activation();
    }
}

impl App {
    pub(in crate::app) fn displayed_terminal(&self) -> Option<PtyId> {
        self.window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
            .or(self.terminal_session.displayed())
    }
}

impl App {
    /// Where the terminal's window is, if it is on screen.
    pub(in crate::app) fn terminal_rect(&self, id: PtyId) -> Option<WindowRect> {
        let window = self.window_session.native_ids().terminal(id.0)?.window;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }
}

impl App {
    pub(in crate::app) fn terminal_window_rect(&self) -> Option<WindowRect> {
        let layout = self.window_session.layout()?;
        let (_, window) = layout.surfaces.terminal?;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn terminal_body_dimensions(&self) -> Option<(u16, u16)> {
        self.compositor_projection()?;
        let rect = self.window_session.content().terminal?;
        Some((rect.width.max(1), rect.height.max(1)))
    }
}

impl App {
    /// Sizes each terminal on screen to its window.
    pub(in crate::app) fn resize_active_terminal_to_panel(&mut self) {
        self.reconcile_editor_windows();
        for (id, window) in self.shown_terminal_windows() {
            let Some(geometry) = self
                .compositor_projection()
                .and_then(|projection| projection.window(window))
                .and_then(crate::ui::terminal_window_geometry)
            else {
                continue;
            };
            let Some(source) = self.window_session.terminal_geometry_source(id.0) else {
                self.show_error("terminal geometry source is missing, stale or foreign");
                return;
            };
            let (columns, rows) = (geometry.width.max(1), geometry.body_height.max(1));
            if let Err(error) =
                self.terminal_session
                    .resize(id, columns, rows, source, &self.workspace)
            {
                self.show_error(error);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn place_active_terminal(&mut self, placement: WindowPlacement) {
        let Some(id) = self.displayed_terminal() else {
            self.show_error(self.message("terminal_missing", &[]));
            return;
        };
        if let Err(error) =
            self.apply_native_window_placement(NativeSurfaceTarget::Terminal(id.0), placement)
        {
            self.show_error(error);
        }
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
