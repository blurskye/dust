//! Terminal screens, tab selection and PTY source lifetimes. No window/focus authority.
mod input;
use super::windows::TerminalGeometrySource;
use super::workspace::{ExecutionReady, WorkspaceSession, WorkspaceVersion};
use crate::{
    config::{TerminalProfileConfig, TerminalsConfig},
    process::{ExecutionPreflightRequest, PtyEvent, PtyId, PtyPolicy, PtyRequest, PtyService},
    ui::{
        PtyScreen, TerminalPanelMode, TerminalPanelState, TerminalTabState,
        window::ContentLifecycle,
    },
};

pub(crate) const TERMINAL_QUEUE_CAPACITY: usize = 256;
pub(crate) const TERMINAL_MAXIMUM_INPUT_BYTES: usize = 1024 * 1024;
pub(crate) const TERMINAL_SCROLLBACK_BYTES: usize = 8 * 1024 * 1024;

pub(in crate::editor::session) use input::TerminalInput;
use input::{clamp_terminal_cursor, keep_terminal_cursor_visible};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::Arc,
};

/// The profile name of a terminal a debugger opened for its program.
pub(in crate::editor::session) const DEBUG_TERMINAL_PROFILE: &str = "debug";

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct TerminalKey {
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
pub(in crate::editor::session) struct TerminalIntent {
    key: TerminalKey,
    profile: String,
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
    navigation: super::windows::NavigationStamp,
    capture: Arc<()>,
}
pub(in crate::editor::session) struct TerminalOpen {
    pub(in crate::editor::session) key: TerminalKey,
    pub(in crate::editor::session) request: ExecutionPreflightRequest,
    pub(in crate::editor::session) navigation: super::windows::NavigationStamp,
    pub(in crate::editor::session) capture: Arc<()>,
    pub(in crate::editor::session) float: bool,
}
struct Open {
    request: ExecutionPreflightRequest,
    config: TerminalProfileConfig,
    arguments: Vec<std::ffi::OsString>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum TerminalUpdate {
    Started(PtyId),
    Exited(PtyId, Option<u32>),
    IoComplete(PtyId),
    Closed(PtyId),
    StopUnknown(PtyId, String),
    Failed(PtyId, String),
    Unknown(PtyId, String),
    Changed,
}
pub(super) struct TerminalSession {
    service: PtyService,
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
    retired: HashSet<PtyId>,
    close_intent: Option<TerminalCloseSource>,
    closed: bool,
    /// Set for every terminal under the profile's own variables, such as `DUST_SERVER`.
    environment: BTreeMap<String, String>,
}
impl TerminalSession {
    pub(in crate::editor::session) fn set_environment(
        &mut self,
        environment: BTreeMap<String, String>,
    ) {
        self.environment = environment;
    }

    pub(in crate::editor::session) fn new(policy: &TerminalsConfig) -> Self {
        Self {
            service: PtyService::start(TERMINAL_QUEUE_CAPACITY),
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
            retired: HashSet::new(),
            close_intent: None,
            closed: false,
            environment: BTreeMap::new(),
        }
    }
    pub(in crate::editor::session) fn selected(&self) -> Option<PtyId> {
        self.selected
    }
    pub(in crate::editor::session) fn displayed(&self) -> Option<PtyId> {
        self.displayed.filter(|id| self.contains(*id))
    }
    /// Shows the terminal in the panel. A side terminal has its own window, so the panel keeps
    /// what it shows.
    pub(in crate::editor::session) fn display(&mut self, id: PtyId) -> bool {
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
    pub(in crate::editor::session) fn set_side(&mut self, id: PtyId) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.side = true;
        }
        if self.displayed == Some(id) {
            self.displayed = self.panel_fallback();
        }
    }
    pub(in crate::editor::session) fn side(&self, id: PtyId) -> bool {
        self.tabs.get(&id).is_some_and(|tab| tab.side)
    }
    pub(in crate::editor::session) fn side_terminals(&self) -> Vec<PtyId> {
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
    pub(in crate::editor::session) fn panel(&self) -> Option<PtyId> {
        self.displayed().or_else(|| self.panel_fallback())
    }
    fn panel_fallback(&self) -> Option<PtyId> {
        self.tabs
            .iter()
            .filter(|(_, tab)| !tab.side)
            .map(|(id, _)| *id)
            .min_by_key(|id| id.0)
    }
    pub(in crate::editor::session) fn revision(&self) -> Option<u64> {
        self.revision
    }
    pub(in crate::editor::session) fn len(&self) -> usize {
        self.tabs.len()
    }
    pub(in crate::editor::session) fn contains(&self, id: PtyId) -> bool {
        self.tabs.contains_key(&id)
    }
    pub(in crate::editor::session) fn select(&mut self, id: PtyId) -> bool {
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
    pub(in crate::editor::session) fn key(&self, id: PtyId) -> Option<TerminalKey> {
        self.key_for(id)
    }
    pub(in crate::editor::session) fn resolve(&self, key: &TerminalKey) -> Option<PtyId> {
        (Arc::ptr_eq(&key.owner, &self.owner) && self.contains(key.id)).then_some(key.id)
    }
    pub(in crate::editor::session) fn requires_close_confirmation(&self, id: PtyId) -> bool {
        self.tabs
            .get(&id)
            .is_some_and(|terminal| terminal.running || terminal.pty.is_some())
    }
    pub(in crate::editor::session) fn begin_close(&mut self, id: PtyId) -> bool {
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
    pub(in crate::editor::session) fn cancel_close(&mut self) {
        self.close_intent = None;
    }
    pub(in crate::editor::session) fn take_close(&mut self) -> Result<Option<PtyId>, String> {
        let Some(source) = self.close_intent.take() else {
            return Ok(None);
        };
        let id = self
            .resolve(&source.key)
            .ok_or("terminal close source retired or was replaced")?;
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
    pub(in crate::editor::session) fn descriptions(&self) -> Vec<(u64, String, ContentLifecycle)> {
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
    pub(in crate::editor::session) fn choices(&self) -> Vec<(TerminalKey, String, String)> {
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
    pub(in crate::editor::session) fn mode(&self, id: PtyId) -> Option<TerminalPanelMode> {
        self.tabs.get(&id).map(|t| t.mode)
    }
    pub(in crate::editor::session) fn running_count(&self) -> usize {
        self.tabs.values().filter(|t| t.running).count()
    }
    pub(in crate::editor::session) fn cycle(&mut self, direction: i64) -> bool {
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
    pub(in crate::editor::session) fn request_open(
        &mut self,
        name: &str,
        workspace: &WorkspaceSession,
        file: Option<(&std::path::Path, usize)>,
        navigation: super::windows::NavigationStamp,
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
        let token = self.open_program(name, config, workspace, navigation, capture)?;
        self.intent.as_mut().expect("new terminal intent").open.arguments = arguments;
        Ok(token)
    }
    /// Opens `config` as it stands, for a program with no profile of its own.
    pub(in crate::editor::session) fn open_program(
        &mut self,
        name: &str,
        config: crate::config::TerminalProfileConfig,
        workspace: &WorkspaceSession,
        navigation: super::windows::NavigationStamp,
        capture: Arc<()>,
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
            return Err("terminal profile exceeds its admission bounds".into());
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
            executable: if config.program.is_absolute() {
                config.program.clone()
            } else {
                cwd.join(&config.program)
            },
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
    pub(in crate::editor::session) fn cancel_open(&mut self) {
        self.intent = None;
    }
    pub(in crate::editor::session) fn prepare(
        &mut self,
        token: TerminalIntent,
        workspace: &WorkspaceSession,
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
            return Err("terminal intent retired, foreign, full or exhausted".into());
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
    pub(in crate::editor::session) fn preflight_count(&self) -> usize {
        self.preflights.len()
    }
    pub(in crate::editor::session) fn take_preflight(
        &mut self,
        id: crate::process::PreflightId,
    ) -> Option<TerminalKey> {
        self.preflights.remove(&id)
    }
    pub(in crate::editor::session) fn queue_preflight(
        &mut self,
        key: TerminalKey,
        request: ExecutionPreflightRequest,
        mut lane: super::workspace::ExecutionLane<'_>,
    ) -> Result<(), String> {
        let id = self
            .resolve(&key)
            .ok_or("terminal preflight source retired")?;
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
    pub(in crate::editor::session) fn launch(
        &mut self,
        key: TerminalKey,
        ready: ExecutionReady,
        workspace: &WorkspaceSession,
    ) -> Result<PtyId, String> {
        let id = self
            .resolve(&key)
            .ok_or("terminal open retired or foreign")?;
        let tab = self.tabs.get_mut(&id).ok_or("terminal closed")?;
        if self.closed || tab.scope != workspace.version() || !Arc::ptr_eq(&tab.epoch, &self.epoch)
        {
            return Err("terminal source retired".into());
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
        let admission = self.service.reserve().map_err(|e| e.to_string())?;
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
        let pty = admission
            .submit(request, Some(root.identity.clone()))
            .map_err(|e| e.to_string())?;
        tab.pty = Some(pty);
        tab.pending = None;
        self.revision = self.revision.and_then(|r| r.checked_add(1));
        Ok(id)
    }
    pub(in crate::editor::session) fn request_stop(&mut self, id: PtyId) -> Result<bool, String> {
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
    pub(in crate::editor::session) fn stopping(&self, id: PtyId) -> bool {
        self.tabs.get(&id).is_some_and(|terminal| terminal.stopping)
    }
    pub(in crate::editor::session) fn close(&mut self, id: PtyId) -> bool {
        self.preflights.retain(|_, key| key.id != id);
        let Some(tab) = self.tabs.remove(&id) else {
            return false;
        };
        if let Some(pty) = tab.pty {
            let _ = self.service.close(pty);
            self.retired.insert(pty);
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
    pub(in crate::editor::session) fn belongs_to_root(&self, root: &std::path::Path) -> bool {
        self.tabs.values().any(|tab| tab.cwd.starts_with(root))
    }
    pub(in crate::editor::session) fn running_in_root(&self, root: &std::path::Path) -> usize {
        self.tabs
            .values()
            .filter(|tab| (tab.running || tab.pty.is_some()) && tab.cwd.starts_with(root))
            .count()
    }
    pub(in crate::editor::session) fn close_root(&mut self, root: &std::path::Path) {
        let ids = self
            .tabs
            .iter()
            .filter_map(|(id, tab)| tab.cwd.starts_with(root).then_some(*id))
            .collect::<Vec<_>>();
        for id in ids {
            self.close(id);
        }
    }
    pub(in crate::editor::session) fn retain_workspace(&mut self, workspace: &WorkspaceSession) {
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
    pub(in crate::editor::session) fn configure(&mut self, policy: &TerminalsConfig) {
        self.policy = policy.clone();
    }
    pub(in crate::editor::session) fn begin_shutdown(&mut self) {
        self.preflights.clear();
        self.cancel_open();
        self.cancel_close();
        self.closed = true;
        self.service.begin_shutdown();
    }
    pub(in crate::editor::session) fn expire_shutdown(&mut self) -> Vec<TerminalUpdate> {
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
    pub(in crate::editor::session) fn pending(&self) -> usize {
        self.service.pending()
    }
    pub(in crate::editor::session) fn reject(&mut self, key: &TerminalKey) {
        if let Some(id) = self.resolve(key) {
            self.close(id);
        }
    }
    pub(in crate::editor::session) fn resize(
        &mut self,
        id: PtyId,
        columns: u16,
        rows: u16,
        source: TerminalGeometrySource,
        workspace: &WorkspaceSession,
    ) -> Result<(), String> {
        let Some(tab) = self.tabs.get_mut(&id) else {
            return Ok(());
        };
        if tab.scope != workspace.version() || !Arc::ptr_eq(&tab.epoch, &self.epoch) {
            return Err("terminal resize source retired".into());
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
            tab.screen.resize_tracked(columns, rows, &mut tab.normal_cursor, &mut tab.visual_anchor, &mut tab.scroll_offset);
            keep_terminal_cursor_visible(tab);
        }
        tab.geometry_source = Some(source);
        tab.requested_size = (columns, rows);
        Ok(())
    }
    pub(in crate::editor::session) fn set_theme(&mut self, theme: &crate::config::ThemeConfig) {
        for tab in self.tabs.values_mut() {
            tab.screen.set_theme(theme);
        }
    }
    pub(in crate::editor::session) fn sync_deadline(&self) -> Option<std::time::Instant> {
        self.tabs
            .values()
            .filter_map(|tab| tab.screen.sync_deadline())
            .min()
    }
    pub(in crate::editor::session) fn drain(
        &mut self,
        workspace: &WorkspaceSession,
        current_geometry: impl Fn(PtyId) -> Option<TerminalGeometrySource>,
    ) -> Vec<TerminalUpdate> {
        let stale = self
            .tabs
            .iter()
            .filter_map(|(id, t)| (t.scope != workspace.version()).then_some(*id))
            .collect::<Vec<_>>();
        let mut updates = Vec::new();
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
            if self.retired.contains(&pty) {
                if matches!(
                    event,
                    PtyEvent::IoComplete { .. }
                        | PtyEvent::Failed { .. }
                        | PtyEvent::Unknown { .. }
                ) {
                    self.retired.remove(&pty);
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
                    let receipt = ResizeReceipt {
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
                        receipt,
                        current.as_ref(),
                        &mut tab.normal_cursor, &mut tab.visual_anchor, &mut tab.scroll_offset,
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
                        let message = format!("terminal exited with status {:?}", tab.exit_code);
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
        if updates.iter().any(|update| !matches!(update, TerminalUpdate::Changed)) {
            self.revision = self.revision.and_then(|r| r.checked_add(1));
        }
        updates
    }

    pub(in crate::editor::session) fn set_debugger_owned(&mut self, id: PtyId) {
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
struct ResizeReceipt {
    terminal: PtyId,
    pty: PtyId,
    sequence: u64,
    columns: u16,
    rows: u16,
}

fn acknowledge_resize(
    pending: &mut VecDeque<PendingResize>,
    screen: &mut PtyScreen,
    receipt: ResizeReceipt,
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
        receipt.terminal,
        receipt.pty,
        receipt.sequence,
        receipt.columns,
        receipt.rows,
    ) {
        return false;
    }
    let expected = pending.pop_front().expect("matched pending resize");
    screen.resize_tracked(receipt.columns, receipt.rows, cursor, anchor, offset);
    current == Some(&expected.source)
}

struct TerminalRuntime {
    profile: String,
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
pub(in crate::editor::session) struct TerminalScreenState {
    pub(in crate::editor::session) cursor: (usize, usize),
    pub(in crate::editor::session) alternate: bool,
    pub(in crate::editor::session) cursor_visible: bool,
    pub(in crate::editor::session) application_cursor: bool,
    pub(in crate::editor::session) bracketed_paste: bool,
    pub(in crate::editor::session) mouse_mode: crate::ui::PtyMouseMode,
    pub(in crate::editor::session) sgr_mouse: bool,
}

#[cfg(test)]
pub(in crate::editor::session) struct TerminalFixture {
    pub(in crate::editor::session) profile: String,
    pub(in crate::editor::session) screen: PtyScreen,
    pub(in crate::editor::session) running: bool,
    pub(in crate::editor::session) exit_code: Option<u32>,
    pub(in crate::editor::session) truncated: bool,
}
#[cfg(test)]
impl TerminalSession {
    pub(in crate::editor::session) fn fixture(
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
                profile: fixture.profile,
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
    pub(in crate::editor::session) fn fixture_cwd(&mut self, id: PtyId, cwd: std::path::PathBuf) {
        self.tabs.get_mut(&id).unwrap().cwd = cwd;
    }
    pub(in crate::editor::session) fn fixture_dimensions(&self, id: PtyId) -> (usize, usize) {
        self.tabs[&id].screen.dimensions()
    }
    pub(in crate::editor::session) fn fixture_pending_resizes(&self, id: PtyId) -> usize {
        self.tabs[&id].pending_resizes.len()
    }
    pub(in crate::editor::session) fn fixture_pending_resize_matches(
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
    pub(in crate::editor::session) fn fixture_screen_state(
        &self,
        id: PtyId,
    ) -> TerminalScreenState {
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
    pub(in crate::editor::session) fn fixture_lines(&self, id: PtyId) -> Vec<String> {
        self.tabs[&id].screen.lines()
    }
}

impl TerminalRuntime {
    fn feed_bytes(&mut self, bytes: &[u8]) {
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
    fn flush_replies(&mut self, service: &PtyService) {
        if !self.pending_replies.is_empty()
            && let Some(pty) = self.pty
            && let Ok(admission) = service.reserve_input(pty)
        {
            let count = self.pending_replies.len().min(TERMINAL_MAXIMUM_INPUT_BYTES);
            if admission.submit(self.pending_replies[..count].to_vec()).is_ok() {
                self.pending_replies.drain(..count);
            }
        }
    }
    fn phase(&self) -> crate::ui::TerminalPhase {
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, WorkspaceSession, TerminalSession) {
        let root = tempfile::tempdir().unwrap();
        let state = crate::workspace::WorkspaceState::new(crate::workspace::WorkspaceId(1), [(
            "root".into(), crate::workspace::RootIdentity::capture(root.path()).unwrap(),
        )]).unwrap();
        let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
        let mut session = TerminalSession::new(&TerminalsConfig::default());
        session.fixture(PtyId(1), TerminalFixture {
            profile: "shell".into(), screen: PtyScreen::new(10, 4, 100),
            running: false, exit_code: None, truncated: false,
        }, workspace.version());
        session.fixture_cwd(PtyId(1), root.path().to_path_buf());
        (root, workspace, session)
    }

    #[cfg(unix)]
    fn launch_shell(root: &std::path::Path, workspace: &WorkspaceSession, session: &mut TerminalSession) {
        let mut config = TerminalsConfig::default().profiles["shell"].clone();
        config.arguments = vec!["-c".into(), "read value; exit 0".into()];
        session.tabs.get_mut(&PtyId(1)).unwrap().pending = Some(Open {
            request: ExecutionPreflightRequest {
                executable: "/bin/sh".into(), cwd: root.into(), roots: vec![root.into()],
                root_revision: workspace.root_revision(),
            },
            arguments: config.arguments.iter().map(Into::into).collect(), config,
        });
        let ready = ExecutionReady::fixture(workspace, crate::process::ExecutionPreflightResult {
            id: crate::process::PreflightId(1), executable: "/bin/sh".into(),
            cwd: root.into(), root: root.into(), root_revision: workspace.root_revision(),
        });
        session.launch(session.key(PtyId(1)).unwrap(), ready, workspace).unwrap();
    }

    #[test]
    fn relative_terminal_programs_use_the_requested_working_directory() {
        let (root, workspace, mut session) = fixture();
        let editor = crate::editor::EditorState::new(root.path().into());
        let windows = super::super::windows::WindowSession::new(false, 8);
        let mut config = TerminalsConfig::default().profiles["shell"].clone();
        config.program = "./app".into();
        config.cwd = Some("build".into());
        session.open_program("debug", config, &workspace,
            windows.navigation_stamp(editor.window_version(), None), Arc::new(())).unwrap();
        let request = &session.intent.as_ref().unwrap().open.request;
        assert_eq!(request.cwd, root.path().join("build"));
        assert_eq!(request.executable, root.path().join("build/./app"));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_lifecycle_revisions_include_launch_and_exit_and_starting_counts_as_running() {
        let (root, workspace, mut session) = fixture();
        let before = session.revision();
        launch_shell(root.path(), &workspace, &mut session);
        assert!(session.revision() > before);
        assert_eq!(session.descriptions()[0].2, ContentLifecycle::Starting);
        assert_eq!(session.running_in_root(root.path()), 1);
        assert!(!session.tabs[&PtyId(1)].running);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !session.tabs[&PtyId(1)].running {
            session.drain(&workspace, |_| None);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let running_revision = session.revision();
        let pty = session.tabs[&PtyId(1)].pty.unwrap();
        session.service.input(pty, b"done\n".to_vec(), TERMINAL_MAXIMUM_INPUT_BYTES).unwrap();
        while !session.tabs[&PtyId(1)].io_complete {
            session.drain(&workspace, |_| None);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(session.revision() > running_revision);
        assert_eq!(session.descriptions()[0].2, ContentLifecycle::Exited);
        assert_eq!(session.running_in_root(root.path()), 0);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_replies_wait_for_input_capacity_and_retry_failed_submissions() {
        let (root, workspace, mut session) = fixture();
        launch_shell(root.path(), &workspace, &mut session);
        let pty = session.tabs[&PtyId(1)].pty.unwrap();
        let admissions = (0..TERMINAL_QUEUE_CAPACITY).map(|_| session.service.reserve_input(pty).unwrap()).collect::<Vec<_>>();
        let tab = session.tabs.get_mut(&PtyId(1)).unwrap();
        tab.feed_bytes(b"\x1b[6n");
        let replies = tab.pending_replies.clone();
        assert_eq!(replies, b"\x1b[1;1R");
        tab.flush_replies(&session.service);
        assert_eq!(tab.pending_replies, replies);
        drop(admissions);
        tab.flush_replies(&session.service);
        assert!(tab.pending_replies.is_empty());
        tab.feed_bytes(b"\x1b[6n");
        session.service.close(pty).unwrap();
        tab.flush_replies(&session.service);
        assert_eq!(tab.pending_replies, replies);
    }

    #[test]
    fn one_column_terminal_geometry_settles_at_the_screen_minimum() {
        let (_root, workspace, mut session) = fixture();
        let source = TerminalGeometrySource::fixture(1);
        session.resize(PtyId(1), 1, 4, source.clone(), &workspace).unwrap();
        assert_eq!(session.tabs[&PtyId(1)].requested_size, (2, 4));
        assert_eq!(session.tabs[&PtyId(1)].screen.dimensions(), (2, 4));
        session.resize(PtyId(1), 1, 4, source, &workspace).unwrap();
        assert!(session.tabs[&PtyId(1)].pending_resizes.is_empty());
    }

    #[test]
    fn terminal_resize_keeps_visual_text_through_wrapping() {
        let (_root, workspace, mut session) = fixture();
        let tab = session.tabs.get_mut(&PtyId(1)).unwrap();
        tab.screen.feed_bytes(b"0123456789abc\r\n$ ");
        tab.mode = TerminalPanelMode::Visual;
        tab.normal_cursor = Some((0, 8));
        tab.visual_anchor = Some((0, 7));
        let source = TerminalGeometrySource::fixture(1);
        session.resize(PtyId(1), 5, 6, source.clone(), &workspace).unwrap();
        assert_eq!(session.tabs[&PtyId(1)].normal_cursor, Some((1, 3)));
        assert_eq!(session.tabs[&PtyId(1)].visual_anchor, Some((1, 2)));
        assert_eq!(session.yank_terminal_text(PtyId(1), false), Some(("78".into(), false)));
        session.start_terminal_visual(PtyId(1), false);
        session.move_terminal_normal_cursor(PtyId(1), "left").unwrap();
        session.resize(PtyId(1), 20, 4, source, &workspace).unwrap();
        assert_eq!(session.tabs[&PtyId(1)].normal_cursor, Some((0, 7)));
        assert_eq!(session.tabs[&PtyId(1)].visual_anchor, Some((0, 8)));
        assert_eq!(session.yank_terminal_text(PtyId(1), false), Some(("78".into(), false)));
    }

    #[test]
    fn terminal_placeholders_expand_only_original_tokens() {
        let mut config = TerminalsConfig::default().profiles["shell"].clone();
        config.arguments = vec!["--file={file}:{line}".into(), "{root}/{dir}".into()];
        config.cwd = Some("{dir}".into());
        let (expanded, arguments) = expand_placeholders(&config, std::path::Path::new("/root/{line}"),
            Some((std::path::Path::new("/root/{line}/a{line}.rs"), 7))).unwrap();
        assert_eq!(arguments[0], "--file=/root/{line}/a{line}.rs:7");
        assert_eq!(arguments[1], "/root/{line}//root/{line}");
        assert_eq!(expanded.cwd.unwrap(), std::path::Path::new("/root/{line}"));
        config.arguments = vec!["{root}".into()];
        config.cwd = None;
        assert!(expand_placeholders(&config, std::path::Path::new("/root/{file}"), None).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_placeholders_preserve_native_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let root = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/root/\xff".to_vec()));
        let file = root.join(std::ffi::OsString::from_vec(b"a\xfe{line}.rs".to_vec()));
        let mut config = TerminalsConfig::default().profiles["shell"].clone();
        config.arguments = vec!["{root}".into(), "prefix:{file}".into(), "{dir}".into()];
        config.cwd = Some(root.join("{line}"));
        let (expanded, arguments) = expand_placeholders(&config, &root, Some((&file, 9))).unwrap();
        assert_eq!(arguments[0], root.as_os_str());
        let mut expected = std::ffi::OsString::from("prefix:");
        expected.push(&file);
        assert_eq!(arguments[1], expected);
        assert_eq!(arguments[2], root.as_os_str());
        assert_eq!(expanded.cwd.unwrap(), root.join("9"));
    }

    #[test]
    fn terminal_normal_motions_and_yanks_skip_wide_continuations() {
        let root = tempfile::tempdir().unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [(
                "test".into(),
                crate::workspace::RootIdentity::capture(root.path()).unwrap(),
            )],
        )
        .unwrap();
        let workspace = WorkspaceSession::new(state, root.path().join("state"), 1, false, false, 1);
        let mut session = TerminalSession::new(&TerminalsConfig::default());
        let mut screen = PtyScreen::new(10, 2, 4);
        screen.feed_bytes("中X".as_bytes());
        let id = PtyId(1);
        session.fixture(
            id,
            TerminalFixture {
                profile: "shell".into(),
                screen,
                running: false,
                exit_code: Some(0),
                truncated: false,
            },
            workspace.version(),
        );
        session.enter_terminal_normal_mode(id);
        session
            .move_terminal_normal_cursor(id, "line_start")
            .unwrap();
        session.move_terminal_normal_cursor(id, "right").unwrap();
        assert_eq!(session.tabs[&id].normal_cursor, Some((0, 2)));
        session.start_terminal_visual(id, false);
        assert_eq!(
            session.yank_terminal_text(id, false),
            Some(("X".into(), false))
        );
        session.move_terminal_normal_cursor(id, "left").unwrap();
        assert_eq!(session.tabs[&id].normal_cursor, Some((0, 0)));
        session.start_terminal_visual(id, false);
        assert_eq!(
            session.yank_terminal_text(id, false),
            Some(("中".into(), false))
        );
    }

    #[test]
    fn workspace_changes_keep_running_terminals_and_retire_unlaunched_tabs() {
        let root = tempfile::tempdir().unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [(
                "root".into(),
                crate::workspace::RootIdentity::capture(root.path()).unwrap(),
            )],
        )
        .unwrap();
        let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
        let policy = crate::config::load_bundled_config().config.terminals;
        let mut terminals = TerminalSession::new(&policy);
        for (id, running) in [(PtyId(1), true), (PtyId(2), false)] {
            terminals.fixture(
                id,
                TerminalFixture {
                    profile: "shell".into(),
                    screen: PtyScreen::new(80, 24, 100),
                    running,
                    exit_code: None,
                    truncated: false,
                },
                workspace.version(),
            );
        }
        terminals.tabs.get_mut(&PtyId(2)).unwrap().pending = Some(Open {
            config: policy.profiles["shell"].clone(),
            arguments: Vec::new(),
            request: ExecutionPreflightRequest {
                executable: "sh".into(),
                cwd: root.path().into(),
                roots: vec![root.path().into()],
                root_revision: workspace.root_revision(),
            },
        });
        terminals.retain_workspace(&workspace);
        assert!(terminals.contains(PtyId(1)));
        assert!(!terminals.contains(PtyId(2)));
    }

    #[test]
    fn resize_acknowledgements_are_ordered_exact_and_one_shot() {
        let terminal = PtyId(2);
        let pty = PtyId(12);
        let source = TerminalGeometrySource::fixture(42);
        let replacement = TerminalGeometrySource::fixture(43);
        let pending_resize =
            |source: &TerminalGeometrySource, sequence, columns, rows| PendingResize {
                terminal,
                pty,
                source: source.clone(),
                sequence,
                columns,
                rows,
            };
        let receipt = |sequence, columns, rows| ResizeReceipt {
            terminal,
            pty,
            sequence,
            columns,
            rows,
        };
        let mut pending = VecDeque::from([
            pending_resize(&source, 7, 40, 10),
            pending_resize(&source, 8, 60, 20),
        ]);
        let mut screen = PtyScreen::new(20, 5, 20);
        screen.feed_bytes(b"0123456789abcdefghijAB");
        let mut cursor = Some((1, 1));
        let mut anchor = Some((0, 18));
        let mut offset = 0;
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(8, 60, 20),
            Some(&source),
            &mut None, &mut None, &mut 0,
        ));
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            ResizeReceipt {
                terminal: PtyId(3),
                ..receipt(7, 40, 10)
            },
            Some(&source),
            &mut None, &mut None, &mut 0,
        ));
        assert_eq!(pending.len(), 2);
        assert!(acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(7, 40, 10),
            Some(&source),
            &mut cursor, &mut anchor, &mut offset,
        ));
        assert_eq!(screen.dimensions(), (40, 10));
        assert_eq!(cursor, Some((0, 21)));
        assert_eq!(anchor, Some((0, 18)));
        assert_eq!(pending.len(), 1, "intermediate ACK was applied");
        screen.feed_bytes(b"\x1b[?1049h\x1b[1;999HX");
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(7, 40, 10),
            Some(&source),
            &mut None, &mut None, &mut 0,
        ));
        assert!(acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(8, 60, 20),
            Some(&source),
            &mut None, &mut None, &mut 0,
        ));
        assert_eq!(screen.dimensions(), (60, 20));
        assert_eq!(screen.history_row(0).unwrap()[39].text, "X");
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(8, 60, 20),
            Some(&source),
            &mut None, &mut None, &mut 0,
        ));

        pending.push_back(pending_resize(&source, 9, 40, 10));
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(9, 40, 10),
            Some(&replacement),
            &mut None, &mut None, &mut 0,
        ));
        assert!(pending.is_empty(), "stale window receipt was retired");
        assert_eq!(screen.dimensions(), (40, 10));
        pending.push_back(pending_resize(&replacement, 10, 40, 10));
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(9, 40, 10),
            Some(&replacement),
            &mut None, &mut None, &mut 0,
        ));
        assert!(acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(10, 40, 10),
            Some(&replacement),
            &mut None, &mut None, &mut 0,
        ));
        assert!(pending.is_empty());
        assert_eq!(screen.dimensions(), (40, 10));
    }
}

/// Fills `{root}`, `{file}`, `{dir}` and `{line}` in a profile's arguments and working
/// directory, so one profile can open a tool on the current file (`lazygit -f {file}`).
fn expand_placeholders(
    config: &crate::config::TerminalProfileConfig,
    root: &std::path::Path,
    file: Option<(&std::path::Path, usize)>,
) -> Result<(crate::config::TerminalProfileConfig, Vec<std::ffi::OsString>), String> {
    let expand = |value: &std::ffi::OsStr| -> Result<std::ffi::OsString, String> {
        #[cfg(unix)]
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        #[cfg(windows)]
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        #[cfg(unix)]
        let bytes = value.as_bytes();
        #[cfg(windows)]
        let bytes: Vec<u16> = value.encode_wide().collect();
        let native_slice = |range: std::ops::Range<usize>| {
            #[cfg(unix)]
            { std::ffi::OsString::from_vec(bytes[range].to_vec()) }
            #[cfg(windows)]
            { std::ffi::OsString::from_wide(&bytes[range]) }
        };
        let mut expanded = std::ffi::OsString::new();
        let mut copied = 0;
        let mut index = 0;
        while index < bytes.len() {
            let placeholder = ["{root}", "{file}", "{dir}", "{line}"]
                .into_iter()
                .find(|placeholder| {
                    bytes.len() - index >= placeholder.len()
                        && placeholder.bytes().zip(&bytes[index..]).all(|(a, b)| u32::from(a) == u32::from(*b))
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
    let arguments = config.arguments.iter().map(|s| expand(s.as_ref())).collect::<Result<_, _>>()?;
    let mut expanded = config.clone();
    if let Some(cwd) = &config.cwd {
        expanded.cwd = Some(expand(cwd.as_os_str())?.into());
    }
    Ok((expanded, arguments))
}
