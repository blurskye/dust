//! Terminal screens, tab selection and PTY source lifetimes. No window/focus authority.
mod input;
use super::workspace::{ExecutionReady, TerminalPermit, WorkspaceSession, WorkspaceVersion};
use crate::{
    config::{TerminalProfileConfig, TerminalsConfig},
    process::{ExecutionPreflightRequest, PtyEvent, PtyId, PtyPolicy, PtyRequest, PtyService},
    ui::{PtyScreen, TerminalPanelMode, TerminalPanelState, TerminalTabState},
};
pub(in crate::editor::session) use input::TerminalInput;
use input::{clamp_terminal_cursor, keep_terminal_cursor_visible};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

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
    nonce: Arc<()>,
}
impl TerminalIntent {
    pub(in crate::editor::session) fn profile(&self) -> &str {
        &self.profile
    }
    pub(in crate::editor::session) fn nonce(&self) -> Arc<()> {
        self.nonce.clone()
    }
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
}
struct Open {
    request: ExecutionPreflightRequest,
    config: TerminalProfileConfig,
}
pub(in crate::editor::session) enum TerminalUpdate {
    Started(PtyId),
    Closed(PtyId),
    Failed(PtyId, String),
    Changed,
}
pub(super) struct TerminalSession {
    service: PtyService,
    tabs: HashMap<PtyId, TerminalRuntime>,
    selected: Option<PtyId>,
    next: Option<u64>,
    revision: Option<u64>,
    intent: Option<Intent>,
    owner: Arc<()>,
    epoch: Arc<()>,
    policy: TerminalsConfig,
    retired: HashSet<PtyId>,
    closed: bool,
}
impl TerminalSession {
    pub(in crate::editor::session) fn new(policy: &TerminalsConfig) -> Self {
        Self {
            service: PtyService::start(policy.queue_capacity),
            tabs: HashMap::new(),
            selected: None,
            next: Some(1),
            revision: Some(0),
            intent: None,
            owner: Arc::new(()),
            epoch: Arc::new(()),
            policy: policy.clone(),
            retired: HashSet::new(),
            closed: false,
        }
    }
    pub(in crate::editor::session) fn selected(&self) -> Option<PtyId> {
        self.selected
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
        true
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn key(&self, id: PtyId) -> Option<TerminalKey> {
        self.contains(id).then(|| TerminalKey {
            owner: self.owner.clone(),
            id,
        })
    }
    pub(in crate::editor::session) fn resolve(&self, key: &TerminalKey) -> Option<PtyId> {
        (Arc::ptr_eq(&key.owner, &self.owner) && self.contains(key.id)).then_some(key.id)
    }
    pub(in crate::editor::session) fn descriptions(&self) -> Vec<(u64, String)> {
        let mut rows = self
            .tabs
            .iter()
            .map(|(id, t)| (id.0, t.profile.clone()))
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|(id, _)| *id);
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
                            crate::ui::TerminalPhase::Exited => "exited",
                            crate::ui::TerminalPhase::Failed => "failed",
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
        let ids = self.descriptions();
        if ids.is_empty() {
            return false;
        }
        let current = self
            .selected
            .and_then(|id| ids.iter().position(|(i, _)| *i == id.0))
            .unwrap_or(0);
        let next = if direction < 0 {
            current.checked_sub(1).unwrap_or(ids.len() - 1)
        } else {
            (current + 1) % ids.len()
        };
        self.select(PtyId(ids[next].0))
    }
    pub(in crate::editor::session) fn request_open(
        &mut self,
        name: &str,
        workspace: &WorkspaceSession,
        navigation: super::windows::NavigationStamp,
        capture: Arc<()>,
    ) -> Result<TerminalIntent, String> {
        if self.closed
            || self.next.is_none()
            || self.revision.is_none_or(|r| r == u64::MAX)
            || self.tabs.len() >= self.policy.queue_capacity.clamp(1, 256)
        {
            return Err("terminal source is closed, full or exhausted".into());
        }
        self.service.check_open().map_err(|e| e.to_string())?;
        let config = self
            .policy
            .profiles
            .get(name)
            .ok_or("unknown terminal profile")?;
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
            executable: config.program.clone(),
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
            nonce: Arc::new(()),
        };
        self.intent = Some(Intent {
            token: token.clone(),
            open: Open {
                request,
                config: config.clone(),
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
        permit: TerminalPermit,
        workspace: &WorkspaceSession,
    ) -> Result<TerminalOpen, String> {
        if !workspace.consume_terminal_permit(permit, token.profile(), &token.nonce) {
            return Err("terminal launch requires its original execution permit".into());
        }
        let source = self.intent.as_ref().ok_or("terminal intent closed")?;
        if self.closed
            || source.token != token
            || source.scope != workspace.version()
            || !Arc::ptr_eq(&source.epoch, &self.epoch)
            || self.revision.is_none_or(|r| r == u64::MAX)
            || self.tabs.len() >= self.policy.queue_capacity.clamp(1, 256)
        {
            return Err("terminal intent retired, foreign, full or exhausted".into());
        }
        self.service.check_open().map_err(|e| e.to_string())?;
        let source = self.intent.take().expect("checked original intent");
        let key = source.token.key;
        let config = &source.open.config;
        let request = source.open.request.clone();
        self.tabs.insert(
            key.id,
            TerminalRuntime {
                profile: source.token.profile,
                pty: None,
                scope: source.scope,
                epoch: source.epoch,
                requested_size: (config.columns, config.rows),
                screen: PtyScreen::new(
                    config.columns,
                    config.rows,
                    self.policy
                        .scrollback_bytes
                        .saturating_div(usize::from(config.columns.max(1)))
                        .max(1),
                ),
                pending: Some(source.open),
                running: false,
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
        })
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
            arguments: open.config.arguments.clone(),
            cwd: open.request.cwd.clone(),
            environment: open.config.environment.clone(),
            columns: tab.requested_size.0,
            rows: tab.requested_size.1,
            root_revision: result.root_revision,
            policy: PtyPolicy {
                maximum_output_bytes: self.policy.maximum_output_bytes,
                maximum_input_bytes: self.policy.maximum_input_bytes,
                event_capacity: self.policy.queue_capacity,
                scrollback_bytes: self.policy.scrollback_bytes,
            },
        };
        let pty = admission
            .submit(request, Some(root.identity.clone()))
            .map_err(|e| e.to_string())?;
        tab.pty = Some(pty);
        tab.pending = None;
        Ok(id)
    }
    pub(in crate::editor::session) fn close(&mut self, id: PtyId) -> bool {
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
        true
    }
    pub(in crate::editor::session) fn reset(&mut self) {
        self.cancel_open();
        let ids = self.tabs.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.close(id);
        }
        self.epoch = Arc::new(());
    }
    pub(in crate::editor::session) fn configure(&mut self, policy: &TerminalsConfig) -> bool {
        if &self.policy != policy {
            self.reset();
            self.policy = policy.clone();
            return true;
        }
        false
    }
    pub(in crate::editor::session) fn begin_shutdown(&mut self) {
        self.closed = true;
        self.reset();
        self.service.begin_shutdown();
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
        if tab.requested_size == (columns, rows) {
            return Ok(());
        }
        if let Some(pty) = tab.pty {
            self.service
                .resize(pty, columns, rows)
                .map_err(|e| e.to_string())?;
        } else {
            tab.screen.resize(columns, rows);
        }
        tab.requested_size = (columns, rows);
        Ok(())
    }
    pub(in crate::editor::session) fn drain(
        &mut self,
        workspace: &WorkspaceSession,
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
        for event in self.service.drain_events() {
            let pty = match &event {
                PtyEvent::Started { id, .. }
                | PtyEvent::Output { id, .. }
                | PtyEvent::Exited { id, .. }
                | PtyEvent::Failed { id, .. }
                | PtyEvent::Resized { id, .. } => *id,
            };
            if self.retired.contains(&pty) {
                if matches!(event, PtyEvent::Exited { .. } | PtyEvent::Failed { .. }) {
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
                    tab.running = true;
                    updates.push(TerminalUpdate::Started(id));
                }
                PtyEvent::Output {
                    bytes, truncated, ..
                } => {
                    tab.screen.feed_bytes(&bytes);
                    tab.truncated |= truncated;
                    if tab.mode != TerminalPanelMode::Input {
                        if let Some(cursor) = tab.normal_cursor {
                            tab.normal_cursor = Some(clamp_terminal_cursor(&tab.screen, cursor));
                        }
                        keep_terminal_cursor_visible(tab);
                    }
                    updates.push(TerminalUpdate::Changed);
                }
                PtyEvent::Resized { columns, rows, .. } => {
                    tab.screen.resize(columns, rows);
                    keep_terminal_cursor_visible(tab);
                    updates.push(TerminalUpdate::Changed);
                }
                PtyEvent::Exited { code, .. } => {
                    tab.pty = None;
                    tab.screen.finish_bytes();
                    tab.running = false;
                    tab.exit_code = code;
                    if code == Some(0) {
                        self.close(id);
                        updates.push(TerminalUpdate::Closed(id));
                    } else {
                        updates.push(TerminalUpdate::Failed(
                            id,
                            format!("terminal exited: {code:?}"),
                        ));
                    }
                }
                PtyEvent::Failed { message, .. } => {
                    tab.pty = None;
                    tab.screen.finish_bytes();
                    tab.running = false;
                    tab.truncated = true;
                    updates.push(TerminalUpdate::Failed(id, message));
                }
            }
        }
        updates
    }
}

struct TerminalRuntime {
    profile: String,
    pty: Option<PtyId>,
    scope: WorkspaceVersion,
    epoch: Arc<()>,
    pending: Option<Open>,
    requested_size: (u16, u16),
    screen: PtyScreen,
    running: bool,
    exit_code: Option<u32>,
    truncated: bool,
    mode: TerminalPanelMode,
    normal_cursor: Option<(usize, usize)>,
    visual_anchor: Option<(usize, usize)>,
    visual_linewise: bool,
    scroll_offset: usize,
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
                pty: None,
                scope,
                epoch: self.epoch.clone(),
                pending: None,
                requested_size: (columns as u16, rows as u16),
                screen: fixture.screen,
                running: fixture.running,
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
    pub(in crate::editor::session) fn fixture_dimensions(&self, id: PtyId) -> (usize, usize) {
        self.tabs[&id].screen.dimensions()
    }
    pub(in crate::editor::session) fn fixture_lines(&self, id: PtyId) -> Vec<String> {
        self.tabs[&id].screen.lines()
    }
}

impl TerminalRuntime {
    fn phase(&self) -> crate::ui::TerminalPhase {
        use crate::ui::TerminalPhase;
        if self.running {
            TerminalPhase::Running
        } else if self.pending.is_some() {
            TerminalPhase::Preflight
        } else if self.pty.is_some() {
            TerminalPhase::Starting
        } else if self.exit_code.is_some() {
            TerminalPhase::Exited
        } else {
            TerminalPhase::Failed
        }
    }
}
