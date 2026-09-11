mod commands;
use super::NativeSurfaceTarget;
use super::{EditorTopology, EditorWindowCandidate, NativeDescriptions, WindowSession};
use crate::ui::window::{ContentTypeId, WindowTarget};
use crate::{
    editor::{EditorOperation, EditorWindowVersion, LayoutNode},
    extensions::PluginCatalogVersion,
    ui::window::{WindowId, WindowPlacement},
};
pub(in crate::editor::session) use commands::{
    CommandEffect, CommandError, CommandPolicy, WindowCommand,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub enum LayoutAction {
    Split(crate::editor::Axis),
    ClosePane,
    ResizePane(i8),
    FocusPane(isize),
    NewTab,
    CloseTab,
    FocusTab(isize),
}

/// Owner-created operations, not a caller-created replacement graph or editor.
pub(in crate::editor::session) struct WindowPlan {
    origin: Arc<()>,
    epoch: u64,
    source: EditorWindowVersion,
    catalog: PluginCatalogVersion,
    operations: Vec<EditorOperation>,
    target: Option<WindowId>,
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
impl WindowSession {
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
        policy: &crate::config::Config,
        catalog: PluginCatalogVersion,
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
        if source
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
                ratio: policy.ui.panes.default_ratio,
            },
            LayoutAction::ClosePane => EditorOperation::ClosePane,
            LayoutAction::ResizePane(direction) => EditorOperation::ResizePane {
                delta: i16::from(direction)
                    .saturating_mul(i16::try_from(policy.ui.panes.resize_step).unwrap_or(i16::MAX)),
            },
            LayoutAction::FocusPane(offset) => EditorOperation::FocusPane {
                offset,
                wrap: policy.ui.panes.focus_wrap,
            },
            LayoutAction::NewTab => EditorOperation::NewTab,
            LayoutAction::CloseTab => EditorOperation::CloseTab,
            LayoutAction::FocusTab(offset) => EditorOperation::FocusTab {
                offset,
                wrap: policy.ui.tab_line.focus_wrap,
            },
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
            descriptions.signature.explorer_focused = false;
            descriptions.signature.terminal_focused = false;
        }
        if plan.hide_dashboard {
            descriptions.signature.dashboard_visible = source.active_tab().is_none();
        }
        let mut candidate = self.prepare_reconcile(&source, scratch, descriptions)?;
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

    pub(in crate::editor::session) fn resolve_native_target(
        &self,
        target: &str,
        selected_terminal: Option<u64>,
    ) -> Result<NativeSurfaceTarget, TargetError> {
        let window = self.resolve_graph_target(target, selected_terminal)?;
        self.native_ids().target_for_window(window).ok_or_else(|| {
            let adapter = self
                .graph()
                .window(window)
                .and_then(|window| self.graph().view(window.view))
                .and_then(|view| self.graph().content(view.content))
                .map_or("unknown", |content| match content.adapter {
                    crate::ui::window::ContentAdapterKind::TextBuffer => "editor-tiled",
                    crate::ui::window::ContentAdapterKind::TerminalSession => "terminal",
                    crate::ui::window::ContentAdapterKind::VirtualDocument => "virtual-document",
                    crate::ui::window::ContentAdapterKind::DeclarativeUi => "declarative-ui",
                    crate::ui::window::ContentAdapterKind::PrivateCellSurface => "private-cell",
                    crate::ui::window::ContentAdapterKind::ExternalTui => "external-tui",
                });
            TargetError::Rejected(format!(
                "window adapter `{adapter}` has no mutable lifecycle hook"
            ))
        })
    }
}
