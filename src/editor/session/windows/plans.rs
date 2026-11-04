mod commands;
use super::NativeSurfaceTarget;
use super::{EditorTopology, EditorWindowCandidate, NativeDescriptions, WindowSession};
use crate::ui::window::{ContentTypeId, WindowTarget};
use crate::ui::window::{PANE_DEFAULT_RATIO, PANE_RESIZE_STEP};
use crate::{
    editor::text::TextVersion,
    editor::{BufferId, EditorOperation, EditorWindowVersion, LayoutNode, PaneId, TabId, ViewId},
    extensions::PluginCatalogVersion,
    ui::window::{ContentId, ViewId as WindowViewId, WindowId, WindowPlacement},
};
pub(in crate::editor::session) use commands::{
    CommandEffect, CommandError, CommandPolicy, WindowCommand,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutAction {
    Split(crate::editor::Axis),
    ClosePane,
    ResizePane(i8),
    FocusPane(isize),
    NewTab,
    CloseTab,
    FocusTab(isize),
    Zoom,
}

/// Owner-created operations, not a caller-created replacement graph or editor.
pub(in crate::editor::session) struct WindowPlan {
    origin: Arc<()>,
    epoch: u64,
    source: EditorWindowVersion,
    catalog: PluginCatalogVersion,
    operations: Vec<EditorOperation>,
    target: Option<WindowId>,
    target_source: Option<(
        WindowId,
        WindowViewId,
        ContentId,
        crate::ui::window::WorkspaceTabId,
    )>,
    clone_source: Option<(WindowId, WindowViewId, ContentId)>,
    focus_editor: bool,
    hide_dashboard: bool,
    native: Option<commands::NativeAction>,
}
impl WindowPlan {
    pub(in crate::editor::session) fn operations(&self) -> &[EditorOperation] {
        &self.operations
    }
}
pub(in crate::editor::session) enum LayoutPlanError {
    Dirty,
    Rejected(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum EditorCloseTarget {
    Buffer { view: ViewId, buffer: BufferId },
    Layout(LayoutAction),
    Window { pane: PaneId, window: WindowId },
}

#[derive(Clone, Debug)]
struct DirtyBufferSource {
    buffer: BufferId,
    text: TextVersion,
    title: String,
}

pub(in crate::editor::session) struct EditorCloseIntent {
    nonce: Arc<()>,
    source: EditorWindowVersion,
    target: EditorCloseTarget,
    tab: TabId,
    focused: PaneId,
    pane: PaneId,
    dirty: Vec<DirtyBufferSource>,
    next: usize,
    waiting: bool,
    discarded: Vec<BufferId>,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct EditorCloseSave {
    origin: Arc<()>,
    nonce: Arc<()>,
    buffer: BufferId,
    text: TextVersion,
}
impl EditorCloseSave {
    pub(in crate::editor::session) fn buffer(&self) -> BufferId {
        self.buffer
    }
}

pub(in crate::editor::session) enum EditorCloseStep {
    Prompt {
        title: String,
        message: String,
    },
    Save(EditorCloseSave),
    Ready {
        target: EditorCloseTarget,
        discarded: Vec<BufferId>,
    },
}

/// A temporary input capture retains placement identity, not picker payloads or
/// terminal selection. Reset, close, cross-tab and hidden targets cannot restore.
pub(in crate::editor::session) struct FocusLease {
    origin: Arc<()>,
    window: WindowId,
    view: crate::ui::window::ViewId,
    source: EditorWindowVersion,
    focus_stamp: Arc<()>,
}
/// Async document activation follows navigation, not text or resize revisions.
#[derive(Clone)]
pub(in crate::editor::session) struct NavigationStamp {
    origin: Arc<()>,
    focus: Arc<()>,
    editor: EditorWindowVersion,
    view: Option<crate::editor::ViewId>,
}
impl WindowSession {
    pub(in crate::editor::session) fn navigation_stamp(
        &self,
        editor: EditorWindowVersion,
        view: Option<crate::editor::ViewId>,
    ) -> NavigationStamp {
        NavigationStamp {
            origin: self.origin.clone(),
            focus: self.focus_stamp.clone(),
            editor,
            view,
        }
    }
    pub(in crate::editor::session) fn navigation_current(
        &self,
        stamp: &NavigationStamp,
        editor: &EditorWindowVersion,
        view: Option<crate::editor::ViewId>,
    ) -> bool {
        editor.revision().is_some()
            && stamp.editor.same_editor(editor)
            && stamp.view == view
            && Arc::ptr_eq(&stamp.origin, &self.origin)
            && Arc::ptr_eq(&stamp.focus, &self.focus_stamp)
    }
}
fn editor_panes(node: &LayoutNode) -> Vec<PaneId> {
    match node {
        LayoutNode::Pane(pane) => vec![*pane],
        LayoutNode::Split { first, second, .. } => {
            let mut panes = editor_panes(first);
            panes.extend(editor_panes(second));
            panes
        }
    }
}

impl WindowSession {
    fn validate_editor_close(
        &self,
        intent: &EditorCloseIntent,
        source: &EditorTopology<'_>,
    ) -> Result<(), String> {
        if source.version() != &intent.source
            || source.window_revision().is_none()
            || self.core.editor.source() != Some(source.version())
            || source.active_tab() != Some(intent.tab)
            || source
                .tabs()
                .get(intent.tab)
                .is_none_or(|tab| tab.focused != intent.focused)
        {
            return Err("dirty close source or focused target changed".into());
        }
        if let EditorCloseTarget::Buffer { view, buffer } = intent.target
            && (source
                .views()
                .get(view)
                .is_none_or(|candidate| candidate.buffer != buffer)
                || source
                    .panes()
                    .get(intent.pane)
                    .is_none_or(|pane| pane.view != view))
        {
            return Err("dirty close buffer/view target changed".into());
        }
        if let EditorCloseTarget::Window { pane, window } = intent.target
            && (pane != intent.pane
                || self.core.editor.pane_for_window(window) != Some(pane)
                || source.panes().get(pane).is_none())
        {
            return Err("dirty close window target changed".into());
        }
        if intent.dirty.iter().enumerate().any(|(index, pending)| {
            (index >= intent.next || intent.discarded.contains(&pending.buffer))
                && source
                    .buffers()
                    .get(pending.buffer)
                    .is_none_or(|buffer| buffer.text.source_version() != pending.text)
        }) {
            return Err("dirty close text source changed".into());
        }
        Ok(())
    }

    fn editor_close_step(&mut self) -> EditorCloseStep {
        let intent = self.editor_close.as_ref().expect("editor close intent");
        if let Some(source) = intent.dirty.get(intent.next) {
            EditorCloseStep::Prompt {
                title: "Close dirty document".into(),
                message: format!("Save changes to {} before closing?", source.title),
            }
        } else {
            let intent = self.editor_close.take().expect("ready editor close intent");
            EditorCloseStep::Ready {
                target: intent.target,
                discarded: intent.discarded,
            }
        }
    }

    pub(in crate::editor::session) fn begin_buffer_close(
        &mut self,
        source: &EditorTopology<'_>,
        view: ViewId,
        buffer: BufferId,
    ) -> Result<EditorCloseStep, String> {
        let tab = source.active_tab().ok_or("no active editor tab")?;
        let pane = source
            .tabs()
            .get(tab)
            .ok_or("active editor tab is stale")?
            .focused;
        if source
            .panes()
            .get(pane)
            .is_none_or(|candidate| candidate.view != view)
            || source
                .views()
                .get(view)
                .is_none_or(|candidate| candidate.buffer != buffer)
        {
            return Err("close buffer target changed".into());
        }
        let buffer_state = source
            .buffers()
            .get(buffer)
            .ok_or("close buffer target is missing")?;
        let dirty = buffer_state.dirty.then(|| DirtyBufferSource {
            buffer,
            text: buffer_state.text.source_version(),
            title: buffer_state.path.as_ref().map_or_else(
                || "scratch buffer".into(),
                |path| path.display().to_string(),
            ),
        });
        self.editor_close = Some(EditorCloseIntent {
            nonce: Arc::new(()),
            source: source.version().clone(),
            target: EditorCloseTarget::Buffer { view, buffer },
            tab,
            focused: pane,
            pane,
            dirty: dirty.into_iter().collect(),
            next: 0,
            waiting: false,
            discarded: Vec::new(),
        });
        self.validate_editor_close(self.editor_close.as_ref().unwrap(), source)?;
        Ok(self.editor_close_step())
    }

    pub(in crate::editor::session) fn begin_layout_close(
        &mut self,
        action: LayoutAction,
        source: &EditorTopology<'_>,
    ) -> Result<EditorCloseStep, String> {
        if !matches!(action, LayoutAction::ClosePane | LayoutAction::CloseTab) {
            return Err("editor close intent requires a close action".into());
        }
        let tab = source.active_tab().ok_or("no active editor tab")?;
        let active = source.tabs().get(tab).ok_or("active editor tab is stale")?;
        let pane = active.focused;
        let closing_panes = if action == LayoutAction::ClosePane {
            vec![pane]
        } else {
            editor_panes(&active.root)
        };
        let closing_views = closing_panes
            .iter()
            .filter_map(|pane| source.panes().get(*pane).map(|pane| pane.view))
            .collect::<Vec<_>>();
        let mut dirty = Vec::new();
        for view in &closing_views {
            let Some(buffer) = source.views().get(*view).map(|view| view.buffer) else {
                continue;
            };
            if dirty
                .iter()
                .any(|source: &DirtyBufferSource| source.buffer == buffer)
            {
                continue;
            }
            let total = source
                .views()
                .values()
                .filter(|view| view.buffer == buffer)
                .count();
            let closing = closing_views
                .iter()
                .filter(|view| {
                    source
                        .views()
                        .get(**view)
                        .is_some_and(|view| view.buffer == buffer)
                })
                .count();
            let buffer_state = &source.buffers()[buffer];
            if total == closing && buffer_state.dirty {
                dirty.push(DirtyBufferSource {
                    buffer,
                    text: buffer_state.text.source_version(),
                    title: buffer_state.path.as_ref().map_or_else(
                        || "scratch buffer".into(),
                        |path| path.display().to_string(),
                    ),
                });
            }
        }
        self.editor_close = Some(EditorCloseIntent {
            nonce: Arc::new(()),
            source: source.version().clone(),
            target: EditorCloseTarget::Layout(action),
            tab,
            focused: pane,
            pane,
            dirty,
            next: 0,
            waiting: false,
            discarded: Vec::new(),
        });
        self.validate_editor_close(self.editor_close.as_ref().unwrap(), source)?;
        Ok(self.editor_close_step())
    }

    pub(in crate::editor::session) fn begin_window_close(
        &mut self,
        window: WindowId,
        source: &EditorTopology<'_>,
    ) -> Result<EditorCloseStep, String> {
        let pane = self
            .core
            .editor
            .pane_for_window(window)
            .ok_or("close target is not an editor window")?;
        let tab = source.active_tab().ok_or("no active editor tab")?;
        let active = source.tabs().get(tab).ok_or("active editor tab is stale")?;
        if !editor_panes(&active.root).contains(&pane) {
            return Err("editor window close target belongs to another workspace tab".into());
        }
        let view = source
            .panes()
            .get(pane)
            .ok_or("editor window pane is stale")?
            .view;
        let buffer = source
            .views()
            .get(view)
            .ok_or("editor window view is stale")?
            .buffer;
        let buffer_state = source
            .buffers()
            .get(buffer)
            .ok_or("editor window content is stale")?;
        let last_reference = source
            .views()
            .values()
            .filter(|candidate| candidate.buffer == buffer)
            .count()
            == 1;
        let dirty = (last_reference && buffer_state.dirty).then(|| DirtyBufferSource {
            buffer,
            text: buffer_state.text.source_version(),
            title: buffer_state.path.as_ref().map_or_else(
                || "scratch buffer".into(),
                |path| path.display().to_string(),
            ),
        });
        self.editor_close = Some(EditorCloseIntent {
            nonce: Arc::new(()),
            source: source.version().clone(),
            target: EditorCloseTarget::Window { pane, window },
            tab,
            focused: active.focused,
            pane,
            dirty: dirty.into_iter().collect(),
            next: 0,
            waiting: false,
            discarded: Vec::new(),
        });
        self.validate_editor_close(self.editor_close.as_ref().unwrap(), source)?;
        Ok(self.editor_close_step())
    }

    pub(in crate::editor::session) fn choose_editor_close(
        &mut self,
        save: bool,
        source: &EditorTopology<'_>,
    ) -> Result<EditorCloseStep, String> {
        let intent = self
            .editor_close
            .as_ref()
            .ok_or("dirty close intent is closed")?;
        self.validate_editor_close(intent, source)?;
        if intent.waiting {
            return Err("dirty close save is already pending".into());
        }
        let pending = intent
            .dirty
            .get(intent.next)
            .ok_or("dirty close has no pending document")?;
        let buffer = pending.buffer;
        let text = pending.text.clone();
        if save {
            let token = EditorCloseSave {
                origin: self.origin.clone(),
                nonce: intent.nonce.clone(),
                buffer,
                text,
            };
            self.editor_close.as_mut().unwrap().waiting = true;
            Ok(EditorCloseStep::Save(token))
        } else {
            let intent = self.editor_close.as_mut().unwrap();
            intent.discarded.push(buffer);
            intent.next += 1;
            Ok(self.editor_close_step())
        }
    }

    pub(in crate::editor::session) fn finish_editor_close_save(
        &mut self,
        token: &EditorCloseSave,
        source: &EditorTopology<'_>,
    ) -> Result<EditorCloseStep, String> {
        let intent = self
            .editor_close
            .as_ref()
            .ok_or("dirty close intent is closed")?;
        let pending = intent
            .dirty
            .get(intent.next)
            .ok_or("dirty close save target is closed")?;
        if !Arc::ptr_eq(&token.origin, &self.origin)
            || !Arc::ptr_eq(&token.nonce, &intent.nonce)
            || !intent.waiting
            || token.buffer != pending.buffer
            || token.text != pending.text
        {
            return Err("dirty close save acknowledgement is duplicate, stale, or foreign".into());
        }
        if source
            .buffers()
            .get(token.buffer)
            .is_none_or(|buffer| buffer.dirty || buffer.text.source_version() != token.text)
        {
            self.editor_close = None;
            return Err("dirty close save acknowledgement no longer matches its text".into());
        }
        let intent = self.editor_close.as_mut().unwrap();
        intent.source = source.version().clone();
        intent.waiting = false;
        intent.next += 1;
        self.validate_editor_close(self.editor_close.as_ref().unwrap(), source)?;
        Ok(self.editor_close_step())
    }

    pub(in crate::editor::session) fn fail_editor_close_save(&mut self, token: &EditorCloseSave) {
        if self.editor_close.as_ref().is_some_and(|intent| {
            Arc::ptr_eq(&token.origin, &self.origin)
                && Arc::ptr_eq(&token.nonce, &intent.nonce)
                && intent.waiting
                && intent.dirty.get(intent.next).is_some_and(|pending| {
                    token.buffer == pending.buffer && token.text == pending.text
                })
        }) {
            self.editor_close = None;
        }
    }

    pub(in crate::editor::session) fn cancel_editor_close(&mut self) {
        self.editor_close = None;
    }

    fn plan(
        &self,
        source: &EditorWindowVersion,
        catalog: PluginCatalogVersion,
    ) -> Result<WindowPlan, String> {
        if source.revision().is_none() || self.core.editor.source() != Some(source) {
            return Err("window planning editor source is stale".into());
        }
        Ok(WindowPlan {
            origin: self.origin.clone(),
            epoch: self.epoch,
            source: source.clone(),
            catalog,
            operations: Vec::new(),
            target: None,
            target_source: None,
            clone_source: None,
            focus_editor: false,
            hide_dashboard: false,
            native: None,
        })
    }
    pub(in crate::editor::session) fn check_plan(
        &self,
        plan: &WindowPlan,
        current: &EditorWindowVersion,
        catalog: &PluginCatalogVersion,
    ) -> Result<(), String> {
        self.check_transition_capacity()?;
        if !Arc::ptr_eq(&self.origin, &plan.origin)
            || self.epoch != plan.epoch
            || &plan.source != current
            || &plan.catalog != catalog
        {
            return Err("window plan owner, editor or catalog source is stale".into());
        }
        if let Some((window, view, content, tab)) = plan.target_source
            && (self
                .core
                .graph
                .window(window)
                .map(|descriptor| (descriptor.view, descriptor.tab))
                != Some((view, tab))
                || self
                    .core
                    .graph
                    .view(view)
                    .map(|descriptor| descriptor.content)
                    != Some(content))
        {
            return Err("window target identity is stale".into());
        }
        if let Some((window, view, content)) = plan.clone_source
            && (self
                .core
                .graph
                .window(window)
                .map(|descriptor| descriptor.view)
                != Some(view)
                || self
                    .core
                    .graph
                    .view(view)
                    .map(|descriptor| descriptor.content)
                    != Some(content))
        {
            return Err("window clone source identity is stale".into());
        }
        Ok(())
    }
    pub(in crate::editor::session) fn plan_focus(
        &self,
        window: WindowId,
        source: &EditorWindowVersion,
        catalog: PluginCatalogVersion,
    ) -> Result<WindowPlan, String> {
        let mut plan = self.plan(source, catalog)?;
        let descriptor = self
            .core
            .graph
            .window(window)
            .ok_or("focus target is closed")?;
        if !descriptor.focusable || descriptor.placement == WindowPlacement::Hidden {
            return Err("focus target is hidden or not focusable".into());
        }
        if Some(descriptor.tab) != self.core.graph.active_tab() {
            let editor_tab = self
                .core
                .editor
                .editor_tab_for(descriptor.tab)
                .ok_or("focus target tab is not owned by the active editor")?;
            plan.operations
                .push(EditorOperation::FocusTabId { tab: editor_tab });
        }
        plan.target = Some(window);
        let view = descriptor.view;
        let content = self
            .core
            .graph
            .view(view)
            .ok_or("focus target view is closed")?
            .content;
        plan.target_source = Some((window, view, content, descriptor.tab));
        if let Some(pane) = self.core.editor.pane_for_window(window) {
            plan.operations.push(EditorOperation::FocusPaneId { pane });
            plan.focus_editor = true;
            plan.hide_dashboard = true;
        }
        Ok(plan)
    }
    pub(in crate::editor::session) fn plan_layout(
        &self,
        action: LayoutAction,
        source: &EditorTopology<'_>,
        catalog: PluginCatalogVersion,
    ) -> Result<WindowPlan, LayoutPlanError> {
        self.plan_layout_inner(action, source, catalog, true)
    }
    pub(in crate::editor::session) fn plan_accepted_editor_close(
        &self,
        target: EditorCloseTarget,
        discarded: Vec<BufferId>,
        source: &EditorTopology<'_>,
        catalog: PluginCatalogVersion,
    ) -> Result<WindowPlan, LayoutPlanError> {
        let mut plan = match target {
            EditorCloseTarget::Layout(action) => {
                self.plan_layout_inner(action, source, catalog, false)?
            }
            EditorCloseTarget::Window { pane, window } => {
                let mut plan = self
                    .plan(source.version(), catalog)
                    .map_err(LayoutPlanError::Rejected)?;
                let descriptor = self
                    .core
                    .graph
                    .window(window)
                    .ok_or_else(|| LayoutPlanError::Rejected("close window is stale".into()))?;
                let view = descriptor.view;
                let content = self
                    .core
                    .graph
                    .view(view)
                    .ok_or_else(|| LayoutPlanError::Rejected("close view is stale".into()))?
                    .content;
                if self.core.editor.pane_for_window(window) != Some(pane) {
                    return Err(LayoutPlanError::Rejected(
                        "close window identity changed".into(),
                    ));
                }
                plan.target_source = Some((window, view, content, descriptor.tab));
                plan.operations = vec![
                    EditorOperation::FocusPaneId { pane },
                    EditorOperation::ClosePane,
                ];
                plan
            }
            EditorCloseTarget::Buffer { .. } => {
                return Err(LayoutPlanError::Rejected(
                    "accepted buffer close is not a layout action".into(),
                ));
            }
        };
        if !discarded.is_empty() {
            plan.operations
                .push(EditorOperation::DiscardUnreferencedBuffers { buffers: discarded });
        }
        Ok(plan)
    }
    fn plan_layout_inner(
        &self,
        action: LayoutAction,
        source: &EditorTopology<'_>,
        catalog: PluginCatalogVersion,
        reject_dirty: bool,
    ) -> Result<WindowPlan, LayoutPlanError> {
        let mut plan = self
            .plan(source.version(), catalog)
            .map_err(LayoutPlanError::Rejected)?;
        let dirty = |pane| {
            source
                .panes()
                .get(pane)
                .and_then(|pane| source.views().get(pane.view))
                .and_then(|view| source.buffers().get(view.buffer))
                .is_some_and(|buffer| buffer.dirty)
        };
        fn any_dirty(node: &LayoutNode, dirty: &impl Fn(crate::editor::PaneId) -> bool) -> bool {
            match node {
                LayoutNode::Pane(pane) => dirty(*pane),
                LayoutNode::Split { first, second, .. } => {
                    any_dirty(first, dirty) || any_dirty(second, dirty)
                }
            }
        }
        if reject_dirty
            && source
                .active_tab()
                .and_then(|tab| source.tabs().get(tab))
                .is_some_and(|tab| match action {
                    LayoutAction::ClosePane => dirty(tab.focused),
                    LayoutAction::CloseTab => any_dirty(&tab.root, &dirty),
                    _ => false,
                })
        {
            return Err(LayoutPlanError::Dirty);
        }
        plan.operations.push(match action {
            LayoutAction::Split(axis) => EditorOperation::SplitPane {
                axis,
                ratio: PANE_DEFAULT_RATIO,
            },
            LayoutAction::ClosePane => EditorOperation::ClosePane,
            LayoutAction::ResizePane(direction) => EditorOperation::ResizePane {
                delta: i16::from(direction)
                    .saturating_mul(i16::try_from(PANE_RESIZE_STEP).unwrap_or(i16::MAX)),
            },
            LayoutAction::FocusPane(offset) => EditorOperation::FocusPane { offset, wrap: true },
            LayoutAction::NewTab => EditorOperation::NewTab,
            LayoutAction::CloseTab => EditorOperation::CloseTab,
            LayoutAction::Zoom => EditorOperation::ToggleZoom,
            LayoutAction::FocusTab(offset) => EditorOperation::FocusTab { offset, wrap: true },
        });
        plan.hide_dashboard = true;
        Ok(plan)
    }
    pub(in crate::editor::session) fn prepare_editor_plan(
        &self,
        plan: WindowPlan,
        versions: (&crate::editor::EditorVersion, &PluginCatalogVersion),
        proposal: crate::editor::EditorDraft,
        scratch: &str,
        mut descriptions: NativeDescriptions<'_>,
    ) -> Result<EditorWindowCandidate, String> {
        let (current, catalog) = versions;
        proposal.check_current(current)?;
        self.check_plan(&plan, proposal.before_window(), catalog)?;
        let source = proposal.topology();
        if plan.operations.is_empty() {
            return Err("native focus plan is not an editor transition".into());
        }
        if plan.focus_editor {
            descriptions.signature.focused_explorer = None;
            descriptions.signature.terminal_focused = false;
        }
        if plan.hide_dashboard {
            descriptions.signature.dashboard_visible = source.active_tab().is_none();
        }
        let mut candidate = self.prepare_reconcile(&source, scratch, descriptions)?;
        if let Some(native) = plan.native {
            match native {
                commands::NativeAction::PlaceEditor(pane, window, placement) => {
                    candidate
                        .core
                        .editor
                        .place_editor(&mut candidate.core.graph, pane, window, placement)
                        .map_err(|error| error.to_string())?;
                    candidate
                        .core
                        .editor
                        .rebuild_tiled_layout(&source, &mut candidate.core.graph)
                        .map_err(|error| error.to_string())?;
                    if candidate.core.graph.focused().is_none() {
                        candidate.dashboard = Some(true);
                    }
                }
                commands::NativeAction::SwapEditors(first, second) => {
                    candidate
                        .core
                        .editor
                        .swap_editors(&mut candidate.core.graph, first, second)
                        .map_err(|error| error.to_string())?;
                }
                _ => return Err("editor transition carries an invalid window action".into()),
            }
        }
        if let Some(target) = plan.target
            && candidate.core.graph.focused() != Some(target)
        {
            candidate
                .core
                .graph
                .focus(target)
                .map_err(|error| error.to_string())?;
        }
        if plan.hide_dashboard {
            candidate.dashboard = Some(false);
        }
        self.stage_editor_transition(proposal, candidate, catalog.clone())
    }
    pub(in crate::editor::session) fn accept_native_plan(
        &mut self,
        plan: WindowPlan,
        current: &EditorWindowVersion,
        catalog: &PluginCatalogVersion,
    ) -> Result<(), String> {
        self.check_plan(&plan, current, catalog)?;
        if !plan.operations.is_empty() {
            return Err("editor focus requires a checked editor transaction".into());
        }
        let window = plan.target.ok_or("missing native focus target")?;
        self.focus_window(window)
    }
    pub(in crate::editor::session) fn capture_focus(&self) -> Option<FocusLease> {
        let window = self.core.graph.focused()?;
        let view = self.core.graph.window(window)?.view;
        Some(FocusLease {
            focus_stamp: self.focus_stamp.clone(),
            source: self.core.editor.source()?.clone(),
            origin: self.origin.clone(),
            window,
            view,
        })
    }
    pub(in crate::editor::session) fn restore_focus_plan(
        &self,
        lease: FocusLease,
        current: &EditorWindowVersion,
        catalog: PluginCatalogVersion,
    ) -> Option<WindowPlan> {
        if !Arc::ptr_eq(&self.focus_stamp, &lease.focus_stamp)
            || !lease.source.same_editor(current)
            || !Arc::ptr_eq(&self.origin, &lease.origin)
            || self.core.graph.window(lease.window)?.view != lease.view
        {
            return None;
        }
        self.plan_focus(lease.window, current, catalog).ok()
    }
}

pub(in crate::editor::session) enum TargetError {
    MissingTerminal,
    Rejected(String),
}
impl WindowSession {
    pub(in crate::editor::session) fn resolve_graph_target(
        &self,
        target: &str,
        selected_terminal: Option<u64>,
    ) -> Result<crate::ui::window::WindowId, TargetError> {
        if target == "terminal" {
            let terminal = selected_terminal.ok_or(TargetError::MissingTerminal)?;
            return self
                .native_ids()
                .terminal(terminal)
                .map(|ids| ids.window)
                .ok_or_else(|| {
                    TargetError::Rejected("selected terminal window is unavailable".into())
                });
        }
        let target = match target {
            "focused" => WindowTarget::Focused,
            "explorer" => WindowTarget::ContentType(
                ContentTypeId::new("builtin.explorer")
                    .map_err(|error| TargetError::Rejected(error.to_string()))?,
            ),
            value if value.starts_with("window:") => WindowTarget::Id(crate::ui::window::WindowId(
                value["window:".len()..].parse::<u64>().map_err(|_| {
                    TargetError::Rejected(format!("invalid runtime window target `{value}`"))
                })?,
            )),
            value => WindowTarget::ContentType(
                ContentTypeId::new(value)
                    .map_err(|error| TargetError::Rejected(error.to_string()))?,
            ),
        };
        self.graph()
            .resolve_target(&target)
            .map_err(|error| TargetError::Rejected(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorState, text::TextBuffer};

    #[test]
    fn discarded_text_is_revalidated_after_another_save() {
        let mut state = EditorState::new("/workspace".into());
        let (a, _) = state.open_text(TextBuffer::from_text("a"), None).unwrap();
        state.buffers[a].dirty = true;
        state
            .split_active(crate::editor::Axis::Vertical, 500)
            .unwrap();
        let (b, _) = state.open_text(TextBuffer::from_text("b"), None).unwrap();
        state.buffers[b].dirty = true;
        state.new_tab().unwrap();
        state.open_text(TextBuffer::from_text("other"), None).unwrap();
        state.focus_tab(-1, true).unwrap();
        let mut windows = WindowSession::default();
        windows
            .core
            .editor
            .reconcile(&(&state).into(), &mut windows.core.graph, "scratch")
            .unwrap();
        windows
            .begin_layout_close(LayoutAction::CloseTab, &(&state).into())
            .unwrap();
        windows
            .choose_editor_close(false, &(&state).into())
            .unwrap();
        let EditorCloseStep::Save(token) =
            windows.choose_editor_close(true, &(&state).into()).unwrap()
        else {
            panic!("expected save");
        };
        state.buffers[a].text = TextBuffer::from_text("new edits");
        state.buffers[b].dirty = false;
        windows
            .core
            .editor
            .reconcile(&(&state).into(), &mut windows.core.graph, "scratch")
            .unwrap();
        assert!(
            windows
                .finish_editor_close_save(&token, &(&state).into())
                .is_err()
        );
        assert_eq!(state.buffers[a].text.to_string(), "new edits");
    }
}
