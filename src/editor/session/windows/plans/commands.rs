use super::{
    EditorOperation, EditorTopology, NativeSurfaceTarget, PluginCatalogVersion, TargetError,
    WindowPlacement, WindowPlan, WindowSession,
};
use crate::editor::FocusDirection;

#[derive(Clone, Copy)]
pub(in crate::editor::session) enum WindowCommand<'a> {
    Clone(&'a str, crate::editor::Axis, u16),
    CloneExplorer(&'a str, u64, u64, crate::editor::Axis),
    Resize(&'a str, i16),
    Direction(FocusDirection, Option<NativeSurfaceTarget>),
    Close(&'a str),
    Swap(&'a str, &'a str),
}
pub(in crate::editor::session) struct CommandPolicy {
    pub selected_terminal: Option<u64>,
    pub pane_step: u16,
    pub terminal_step: u16,
}
pub(in crate::editor::session) enum CommandError {
    Dirty,
    Target(TargetError),
    Rejected(String),
}
impl From<String> for CommandError {
    fn from(s: String) -> Self {
        Self::Rejected(s)
    }
}
impl From<TargetError> for CommandError {
    fn from(e: TargetError) -> Self {
        Self::Target(e)
    }
}
pub(super) enum NativeAction {
    None,
    CloneExplorer(NativeSurfaceTarget, u64, crate::editor::Axis),
    Place(NativeSurfaceTarget, WindowPlacement),
    Close(NativeSurfaceTarget),
    Swap(NativeSurfaceTarget, NativeSurfaceTarget),
}
pub(in crate::editor::session) enum CommandEffect {
    None,
    ClonedExplorer(u64),
    ClosedExplorer,
    ClosedExplorerView(u64),
    CloseTerminal(u64),
    ClosedPicker(crate::ui::PickerSurfaceKind),
}
impl WindowSession {
    pub(in crate::editor::session) fn plan_window_command(
        &self,
        command: WindowCommand<'_>,
        source: &EditorTopology<'_>,
        policy: CommandPolicy,
        catalog: PluginCatalogVersion,
    ) -> Result<WindowPlan, CommandError> {
        self.require_projected_graph()?;
        self.check_editor_admission(source)?;
        let mut plan = self.plan(source.version(), catalog)?;
        let target = |name| self.resolve_graph_target(name, policy.selected_terminal);
        let pane = |window| {
            self.core.editor.pane_for_window(window).ok_or_else(|| {
                CommandError::Rejected("window adapter has no authoritative editor hook".into())
            })
        };
        match command {
            WindowCommand::Clone(name, axis, ratio) => {
                let window = target(name)?;
                let source_view = self
                    .core
                    .graph
                    .window(window)
                    .ok_or_else(|| CommandError::Rejected("clone target is stale".into()))?
                    .view;
                let source_content = self
                    .core
                    .graph
                    .view(source_view)
                    .ok_or_else(|| CommandError::Rejected("clone source view is stale".into()))?
                    .content;
                self.core
                    .graph
                    .require_clone_view(source_content)
                    .map_err(|error| CommandError::Rejected(error.to_string()))?;
                plan.operations.push(EditorOperation::ClonePane {
                    pane: pane(window)?,
                    axis,
                    ratio,
                });
                plan.clone_source = Some((window, source_view, source_content));
                plan.hide_dashboard = true;
            }
            WindowCommand::CloneExplorer(name, source, destination, axis) => {
                let window = target(name)?;
                let native = self.core.native.target_for_window(window).ok_or_else(|| {
                    CommandError::Rejected("clone target is not a native tree view".into())
                })?;
                let actual = match native {
                    NativeSurfaceTarget::Explorer => 1,
                    NativeSurfaceTarget::ExplorerView(view) => view,
                    _ => {
                        return Err(CommandError::Rejected(
                            "clone target is not a native tree view".into(),
                        ));
                    }
                };
                if actual != source {
                    return Err(CommandError::Rejected(
                        "explorer clone source identity is stale".into(),
                    ));
                }
                let ids = self
                    .core
                    .native
                    .ids_for_target(native)
                    .ok_or_else(|| CommandError::Rejected("clone target is stale".into()))?;
                self.core
                    .graph
                    .require_clone_view(ids.content)
                    .map_err(|error| CommandError::Rejected(error.to_string()))?;
                plan.clone_source = Some((window, ids.view, ids.content));
                plan.native = Some(NativeAction::CloneExplorer(native, destination, axis));
            }
            WindowCommand::Resize(name, amount) => {
                let window = target(name)?;
                if let Some(native) = self.core.native.target_for_window(window) {
                    plan.native = Some(NativeAction::Place(
                        native,
                        self.resized_native_placement(native, amount)?,
                    ));
                } else {
                    plan.operations = vec![
                        EditorOperation::FocusPaneId {
                            pane: pane(window)?,
                        },
                        EditorOperation::ResizePane { delta: amount },
                    ];
                }
            }
            WindowCommand::Direction(direction, explicit) => {
                let native = if explicit.is_some() {
                    explicit
                } else if self.terminal_focused() {
                    Some(NativeSurfaceTarget::Terminal(
                        policy
                            .selected_terminal
                            .ok_or(TargetError::MissingTerminal)?,
                    ))
                } else if self.explorer_focused() {
                    Some(NativeSurfaceTarget::Explorer)
                } else {
                    None
                };
                if let Some(native) = native {
                    plan.native = Some(
                        self.directional_native_placement(
                            native,
                            direction,
                            i16::try_from(policy.terminal_step).unwrap_or(i16::MAX),
                        )?
                        .map_or(NativeAction::None, |p| NativeAction::Place(native, p)),
                    );
                } else {
                    let (axis, grow) = match direction {
                        FocusDirection::Left => (crate::editor::Axis::Horizontal, false),
                        FocusDirection::Right => (crate::editor::Axis::Horizontal, true),
                        FocusDirection::Up => (crate::editor::Axis::Vertical, true),
                        FocusDirection::Down => (crate::editor::Axis::Vertical, false),
                    };
                    let step = i16::try_from(policy.pane_step).unwrap_or(i16::MAX);
                    plan.operations.push(EditorOperation::ResizePaneOnAxis {
                        axis,
                        delta: if grow { step } else { -step },
                    });
                }
            }
            WindowCommand::Close(name) => {
                let window = target(name)?;
                if let Some(native) = self.core.native.target_for_window(window) {
                    if native == NativeSurfaceTarget::Explorer
                        && self.core.native.explorer_view_count() > 1
                    {
                        return Err(CommandError::Rejected(
                            "close cloned explorer views before the primary explorer".into(),
                        ));
                    }
                    if native == NativeSurfaceTarget::Dashboard && source.active_tab().is_none() {
                        return Err(CommandError::Rejected(
                            "cannot close the only dashboard surface".into(),
                        ));
                    }
                    plan.native = Some(NativeAction::Close(native));
                } else {
                    let pane = pane(window)?;
                    if source
                        .panes()
                        .get(pane)
                        .and_then(|p| source.views().get(p.view))
                        .and_then(|v| source.buffers().get(v.buffer))
                        .is_some_and(|b| b.dirty)
                    {
                        return Err(CommandError::Dirty);
                    }
                    plan.operations = vec![
                        EditorOperation::FocusPaneId { pane },
                        EditorOperation::ClosePane,
                    ];
                }
            }
            WindowCommand::Swap(first, second) => {
                let (first, second) = (target(first)?, target(second)?);
                plan.native = if first == second {
                    Some(NativeAction::None)
                } else {
                    match (
                        self.core.native.target_for_window(first),
                        self.core.native.target_for_window(second),
                    ) {
                        (Some(a), Some(b)) => Some(NativeAction::Swap(a, b)),
                        (None, None) => {
                            plan.operations.push(EditorOperation::SwapPanes {
                                first: pane(first)?,
                                second: pane(second)?,
                            });
                            None
                        }
                        _ => {
                            return Err(CommandError::Rejected(
                                "cannot swap editor and non-editor window adapters".into(),
                            ));
                        }
                    }
                };
            }
        }
        Ok(plan)
    }
    pub(in crate::editor::session) fn accept_window_command(
        &mut self,
        plan: WindowPlan,
        source: &crate::editor::EditorWindowVersion,
        catalog: &PluginCatalogVersion,
    ) -> Result<CommandEffect, String> {
        self.check_plan(&plan, source, catalog)?;
        if !plan.operations.is_empty() {
            return Err("editor command requires its consuming editor proposal".into());
        }
        match plan.native.ok_or("not a native command plan")? {
            NativeAction::None => {}
            NativeAction::CloneExplorer(source, view, axis) => {
                let mut candidate = self.candidate()?;
                candidate
                    .core
                    .native
                    .clone_explorer(&mut candidate.core.graph, source, view, axis)
                    .map_err(|error| error.to_string())?;
                self.commit(candidate)?;
                return Ok(CommandEffect::ClonedExplorer(view));
            }
            NativeAction::Place(target, placement) => self.place_native(target, placement)?,
            NativeAction::Swap(a, b) => self.swap_native(a, b)?,
            NativeAction::Close(NativeSurfaceTarget::Terminal(id)) => {
                return Ok(CommandEffect::CloseTerminal(id));
            }
            NativeAction::Close(target) => {
                let mut candidate = self.candidate()?;
                candidate
                    .core
                    .native
                    .close_target(&mut candidate.core.graph, target)
                    .map_err(|e| e.to_string())?;
                if target == NativeSurfaceTarget::Dashboard {
                    candidate.dashboard = Some(false);
                }
                self.commit(candidate)?;
                return Ok(match target {
                    NativeSurfaceTarget::Explorer => CommandEffect::ClosedExplorer,
                    NativeSurfaceTarget::ExplorerView(view) => {
                        CommandEffect::ClosedExplorerView(view)
                    }
                    NativeSurfaceTarget::Picker(kind) => CommandEffect::ClosedPicker(kind),
                    _ => CommandEffect::None,
                });
            }
        }
        Ok(CommandEffect::None)
    }
}
