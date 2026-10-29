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
pub(in crate::editor::session) use input::TerminalInput;
use input::{clamp_terminal_cursor, keep_terminal_cursor_visible};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
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
    pub(in crate::editor::session) fn set_environment(&mut self, environment: BTreeMap<String, String>) {
        self.environment = environment;
    }

    pub(in crate::editor::session) fn new(policy: &TerminalsConfig) -> Self {
        Self {
            service: PtyService::start(policy.queue_capacity),
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
    pub(in crate::editor::session) fn display(&mut self, id: PtyId) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.displayed = Some(id);
        true
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
    pub(in crate::editor::session) fn closes_on_exit(&self, id: PtyId) -> bool {
        self.tabs.get(&id).is_some_and(|tab| tab.close_on_exit)
    }
    pub(in crate::editor::session) fn select(&mut self, id: PtyId) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.selected = Some(id);
        if self.displayed.is_none() {
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
        let ids = self.descriptions();
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
        self.select(PtyId(ids[next].0))
    }
    pub(in crate::editor::session) fn request_open(
        &mut self,
        name: &str,
        workspace: &WorkspaceSession,
        file: Option<(&std::path::Path, usize)>,
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
        let config = &expand_placeholders(config, &root.identity.canonical_path, file)?;
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
            || self.tabs.len() >= self.policy.queue_capacity.clamp(1, 256)
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
                close_on_exit: config.close_on_exit,
                pty: None,
                scope: source.scope,
                epoch: source.epoch,
                requested_size: (config.columns, config.rows),
                geometry_source: None,
                next_resize: Some(1),
                pending_resizes: VecDeque::new(),
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
            arguments: open.config.arguments.clone(),
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
            self.displayed = self.selected.filter(|selected| self.contains(*selected));
        }
        true
    }
    pub(in crate::editor::session) fn reset(&mut self) {
        self.preflights.clear();
        self.cancel_open();
        self.cancel_close();
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
        let same_size = tab.requested_size == (columns, rows);
        let same_source = tab.geometry_source.as_ref() == Some(&source);
        let acknowledged = tab.screen.dimensions() == (usize::from(columns), usize::from(rows));
        if same_size && same_source && (acknowledged || !tab.pending_resizes.is_empty()) {
            return Ok(());
        }
        if same_size && !same_source && acknowledged {
            tab.pending_resizes.clear();
            tab.geometry_source = Some(source);
            return Ok(());
        }
        if let Some(pty) = tab.pty {
            if same_source && tab.pending_resizes.len() >= 256 {
                return Err("terminal resize acknowledgement queue is full".into());
            }
            let sequence = tab
                .next_resize
                .ok_or("terminal resize identity exhausted")?;
            self.service
                .resize_checked(pty, sequence, columns, rows)
                .map_err(|e| e.to_string())?;
            tab.next_resize = sequence.checked_add(1);
            if !same_source {
                tab.pending_resizes.clear();
            }
            tab.pending_resizes.push_back(PendingResize {
                terminal: id,
                pty,
                source: source.clone(),
                sequence,
                columns,
                rows,
            });
        } else {
            tab.screen.resize(columns, rows);
        }
        tab.geometry_source = Some(source);
        tab.requested_size = (columns, rows);
        Ok(())
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
                    tab.pty = None;
                    tab.screen.finish_bytes();
                    tab.io_complete = true;
                    updates.push(TerminalUpdate::IoComplete(id));
                    if let Err(message) = result {
                        tab.failure = Some(message.clone());
                        tab.truncated = true;
                        updates.push(TerminalUpdate::Failed(id, message));
                    } else if tab.exit_code.is_some_and(|code| code != 0) {
                        let message = format!("terminal exited with status {:?}", tab.exit_code);
                        tab.failure = Some(message.clone());
                        updates.push(TerminalUpdate::Failed(id, message));
                    }
                    if stopping || (tab.failure.is_none() && tab.exit_code == Some(0)) {
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
                    tab.screen.finish_bytes();
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
                    tab.screen.finish_bytes();
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
        updates
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
    if !pending.is_empty() || current != Some(&expected.source) {
        return false;
    }
    screen.resize(receipt.columns, receipt.rows);
    true
}

struct TerminalRuntime {
    profile: String,
    close_on_exit: bool,
    pty: Option<PtyId>,
    scope: WorkspaceVersion,
    epoch: Arc<()>,
    pending: Option<Open>,
    requested_size: (u16, u16),
    geometry_source: Option<TerminalGeometrySource>,
    next_resize: Option<u64>,
    pending_resizes: VecDeque<PendingResize>,
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
                close_on_exit: false,
                pty: None,
                scope,
                epoch: self.epoch.clone(),
                pending: None,
                requested_size: (columns as u16, rows as u16),
                geometry_source: None,
                next_resize: Some(1),
                pending_resizes: VecDeque::new(),
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
            && tab.pending_resizes.len() == 1
            && tab
                .pending_resizes
                .front()
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
mod tests {
    use super::*;

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
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(8, 60, 20),
            Some(&source),
        ));
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            ResizeReceipt {
                terminal: PtyId(3),
                ..receipt(7, 40, 10)
            },
            Some(&source),
        ));
        assert_eq!(pending.len(), 2);
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(7, 40, 10),
            Some(&source),
        ));
        assert_eq!(pending.len(), 1, "superseded ACK was retired");
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(7, 40, 10),
            Some(&source),
        ));
        assert!(acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(8, 60, 20),
            Some(&source),
        ));
        assert_eq!(screen.dimensions(), (60, 20));
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(8, 60, 20),
            Some(&source),
        ));

        pending.push_back(pending_resize(&source, 9, 40, 10));
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(9, 40, 10),
            Some(&replacement),
        ));
        assert!(pending.is_empty(), "stale window receipt was retired");
        assert_eq!(screen.dimensions(), (60, 20));
        pending.push_back(pending_resize(&replacement, 10, 40, 10));
        assert!(!acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(9, 40, 10),
            Some(&replacement),
        ));
        assert!(acknowledge_resize(
            &mut pending,
            &mut screen,
            receipt(10, 40, 10),
            Some(&replacement),
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
) -> Result<crate::config::TerminalProfileConfig, String> {
    let expand = |value: &str| -> Result<String, String> {
        let mut value = value.replace("{root}", &root.to_string_lossy());
        for placeholder in ["{file}", "{dir}", "{line}"] {
            if !value.contains(placeholder) {
                continue;
            }
            let (path, line) = file.ok_or("this terminal profile needs a file buffer")?;
            let replacement = match placeholder {
                "{file}" => path.to_string_lossy().into_owned(),
                "{dir}" => path
                    .parent()
                    .unwrap_or(root)
                    .to_string_lossy()
                    .into_owned(),
                _ => line.to_string(),
            };
            value = value.replace(placeholder, &replacement);
        }
        Ok(value)
    };
    let mut expanded = config.clone();
    for argument in &mut expanded.arguments {
        *argument = expand(argument)?;
    }
    if let Some(cwd) = &expanded.cwd {
        expanded.cwd = Some(expand(&cwd.to_string_lossy())?.into());
    }
    Ok(expanded)
}
