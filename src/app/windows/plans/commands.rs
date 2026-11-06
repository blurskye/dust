//! Command registration and execution for checked window and editor-close plans.

use crate::app::windows::NativeSurfaceTarget;
use crate::app::windows::Windows;
use crate::app::windows::plans::TargetError;
use crate::app::windows::plans::WindowPlan;
use crate::editor::EditorOperation;
use crate::editor::EditorTopology;
use crate::plugins::PluginCatalogVersion;
use crate::windows::SpatialDirection;
use crate::windows::WindowPlacement;

#[derive(Clone, Copy)]
pub(in crate::app) enum WindowCommand<'a> {
    Clone(&'a str, crate::windows::TileAxis, u16),
    CloneExplorer(&'a str, u64, u64, crate::windows::TileAxis),
    Place(&'a str, WindowPlacement),
    Show(&'a str),
    Move(&'a str, u16, u16),
    Resize(&'a str, i16),
    Direction(SpatialDirection, Option<NativeSurfaceTarget>, u64),
    Close(&'a str),
    Swap(&'a str, &'a str),
}
pub(in crate::app) struct CommandPolicy {
    pub selected_terminal: Option<u64>,
    pub pane_step: u16,
    pub terminal_step: u16,
}
pub(in crate::app) enum CommandError {
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
#[derive(Clone, Copy)]
pub(super) enum NativeAction {
    None,
    CloneExplorer(NativeSurfaceTarget, u64, crate::windows::TileAxis),
    Place(NativeSurfaceTarget, WindowPlacement),
    PlaceEditor(crate::windows::WindowId, WindowPlacement),
    ShowEditor(crate::windows::WindowId),
    SwapEditors(crate::windows::WindowId, crate::windows::WindowId),
    Close(NativeSurfaceTarget),
    Swap(NativeSurfaceTarget, NativeSurfaceTarget),
}
pub(in crate::app) enum CommandEffect {
    None,
    ClonedExplorer(u64),
    ClosedExplorer,
    ClosedExplorerView(u64),
    CloseTerminal(u64),
    ClosedPicker(crate::ui::PickerSurfaceKind),
}
impl Windows {
    /// A shown terminal docked on the right (for `Horizontal`) or at the bottom (`Vertical`).
    fn docked_terminal_on(&self, axis: crate::windows::TileAxis) -> Option<NativeSurfaceTarget> {
        let wanted = match axis {
            crate::windows::TileAxis::Horizontal => crate::windows::DockEdge::Right,
            crate::windows::TileAxis::Vertical => crate::windows::DockEdge::Bottom,
        };
        let mut ids: Vec<u64> = self.native_ids().terminal_ids().collect();
        ids.sort_unstable();
        ids.into_iter()
            .map(NativeSurfaceTarget::Terminal)
            .find(|target| {
                matches!(
                    self.native_placement(*target),
                    Ok(WindowPlacement::Docked { edge, .. }) if edge == wanted
                )
            })
    }

    pub(in crate::app) fn plan_window_command(
        &self,
        command: WindowCommand<'_>,
        source: &EditorTopology<'_>,
        policy: CommandPolicy,
        catalog: PluginCatalogVersion,
    ) -> Result<WindowPlan, CommandError> {
        self.require_projected_graph()?;
        self.check_editor_source(source)?;
        let mut plan = self.plan(source.version(), catalog)?;
        let target = |name| self.resolve_graph_target(name, policy.selected_terminal);
        let pane = |window| {
            self.core
                .graph
                .window(window)
                .filter(|window| window.owner == "builtin.editor")
                .map(|window| window.id)
                .ok_or_else(|| {
                    CommandError::Rejected("window adapter has no authoritative editor hook".into())
                })
        };
        let bind_target = |plan: &mut WindowPlan, window| -> Result<(), CommandError> {
            let descriptor = self
                .core
                .graph
                .window(window)
                .ok_or_else(|| CommandError::Rejected("window target is stale".into()))?;
            let content = self
                .core
                .graph
                .view(descriptor.view)
                .ok_or_else(|| CommandError::Rejected("window target view is stale".into()))?
                .content;
            plan.target_source = Some((window, descriptor.view, content, descriptor.tab));
            Ok(())
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
            WindowCommand::Place(name, placement) => {
                let window = target(name)?;
                bind_target(&mut plan, window)?;
                if let Some(native) = self.core.native.target_for_window(window) {
                    plan.native = Some(NativeAction::Place(native, placement));
                } else {
                    let target_pane = pane(window)?;
                    if placement == WindowPlacement::Fullscreen {
                        plan.operations
                            .push(EditorOperation::FocusPaneId { pane: target_pane });
                        plan.focus_editor = true;
                    }
                    if placement == WindowPlacement::Hidden
                        && self.core.graph.focused() == Some(window)
                    {
                        let tab = source.active_tab().ok_or_else(|| {
                            CommandError::Rejected("active editor tab is stale".into())
                        })?;
                        let ordered = source.state().tab_windows(tab);
                        let index = ordered
                            .iter()
                            .position(|pane| *pane == target_pane)
                            .ok_or_else(|| {
                                CommandError::Rejected("editor pane is absent from its tab".into())
                            })?;
                        let fallback = ordered[index + 1..]
                            .iter()
                            .chain(ordered[..index].iter().rev())
                            .copied()
                            .find(|candidate| {
                                Some(*candidate)
                                    .and_then(|window| self.core.graph.window(window))
                                    .is_some_and(|window| {
                                        window.focusable
                                            && window.placement != WindowPlacement::Hidden
                                    })
                            });
                        if let Some(fallback) = fallback {
                            plan.operations
                                .push(EditorOperation::FocusPaneId { pane: fallback });
                        }
                    }
                    plan.native = Some(NativeAction::PlaceEditor(target_pane, placement));
                }
            }
            WindowCommand::Show(name) => {
                let window = target(name)?;
                bind_target(&mut plan, window)?;
                if self
                    .core
                    .graph
                    .window(window)
                    .is_none_or(|descriptor| descriptor.placement != WindowPlacement::Hidden)
                {
                    return Err(CommandError::Rejected("window is not hidden".into()));
                }
                if let Some(native) = self.core.native.target_for_window(window) {
                    let placement =
                        self.core
                            .native
                            .placement_for_target(native)
                            .ok_or_else(|| {
                                CommandError::Rejected(
                                    "native window has no restorable placement".into(),
                                )
                            })?;
                    plan.native = Some(NativeAction::Place(native, placement));
                } else {
                    let pane = pane(window)?;
                    if self
                        .core
                        .graph
                        .window(pane)
                        .ok_or_else(|| CommandError::Rejected("stale window".into()))?
                        .restore_placement
                        == WindowPlacement::Fullscreen
                    {
                        plan.operations.push(EditorOperation::FocusPaneId { pane });
                        plan.focus_editor = true;
                        plan.target = Some(window);
                    }
                    plan.hide_dashboard = true;
                    plan.native = Some(NativeAction::ShowEditor(pane));
                }
            }
            WindowCommand::Move(name, x, y) => {
                let window = target(name)?;
                bind_target(&mut plan, window)?;
                let WindowPlacement::Floating(mut placement) = self
                    .core
                    .graph
                    .window(window)
                    .ok_or_else(|| CommandError::Rejected("window target is stale".into()))?
                    .placement
                else {
                    return Err(CommandError::Rejected(
                        "only floating ordinary windows can be moved".into(),
                    ));
                };
                placement.x = x;
                placement.y = y;
                if let Some(native) = self.core.native.target_for_window(window) {
                    plan.native = Some(NativeAction::Place(
                        native,
                        WindowPlacement::Floating(placement),
                    ));
                } else {
                    plan.native = Some(NativeAction::PlaceEditor(
                        pane(window)?,
                        WindowPlacement::Floating(placement),
                    ));
                }
            }
            WindowCommand::Resize(name, amount) => {
                let window = target(name)?;
                bind_target(&mut plan, window)?;
                if let Some(native) = self.core.native.target_for_window(window) {
                    plan.native = Some(NativeAction::Place(
                        native,
                        self.resized_native_placement(native, amount)?,
                    ));
                } else {
                    let pane = pane(window)?;
                    match self
                        .core
                        .graph
                        .window(window)
                        .ok_or_else(|| CommandError::Rejected("window target is stale".into()))?
                        .placement
                    {
                        WindowPlacement::Tiled => {
                            plan.operations = vec![
                                EditorOperation::FocusPaneId { pane },
                                EditorOperation::ResizePane { delta: amount },
                            ];
                        }
                        WindowPlacement::Docked { edge, size } => {
                            plan.native = Some(NativeAction::PlaceEditor(
                                pane,
                                WindowPlacement::Docked {
                                    edge,
                                    size: size.saturating_add_signed(amount).max(1),
                                },
                            ));
                        }
                        WindowPlacement::Floating(mut rect) => {
                            rect.width = rect.width.saturating_add_signed(amount).max(1);
                            rect.height = rect.height.saturating_add_signed(amount).max(1);
                            plan.native = Some(NativeAction::PlaceEditor(
                                pane,
                                WindowPlacement::Floating(rect),
                            ));
                        }
                        WindowPlacement::Fullscreen
                        | WindowPlacement::Hidden
                        | WindowPlacement::Overlay(_) => {
                            return Err(CommandError::Rejected(
                                "only tiled, docked, or floating editor windows can be resized"
                                    .into(),
                            ));
                        }
                    }
                }
            }
            WindowCommand::Direction(direction, explicit, count) => {
                let scaled_step =
                    |step: u16| u64::from(step).saturating_mul(count).min(i16::MAX as u64) as i16;
                let native = if explicit.is_some() {
                    explicit
                } else if self.terminal_focused() {
                    Some(NativeSurfaceTarget::Terminal(
                        policy
                            .selected_terminal
                            .ok_or(TargetError::MissingTerminal)?,
                    ))
                } else if self.explorer_focused() {
                    Some(match self.focused_explorer() {
                        Some(1) | None => NativeSurfaceTarget::Explorer,
                        Some(view) => NativeSurfaceTarget::ExplorerView(view),
                    })
                } else {
                    None
                };
                if let Some(native) = native {
                    plan.native = Some(
                        self.directional_native_placement(
                            native,
                            direction,
                            scaled_step(policy.terminal_step),
                        )?
                        .map_or(NativeAction::None, |p| NativeAction::Place(native, p)),
                    );
                } else {
                    let window = self
                        .core
                        .graph
                        .focused()
                        .ok_or_else(|| CommandError::Rejected("no focused window".into()))?;
                    bind_target(&mut plan, window)?;
                    let pane = pane(window)?;
                    let placement = self
                        .core
                        .graph
                        .window(window)
                        .ok_or_else(|| CommandError::Rejected("focused window is stale".into()))?
                        .placement;
                    let step = scaled_step(policy.pane_step);
                    match placement {
                        WindowPlacement::Tiled => {
                            let (axis, grow) = match direction {
                                SpatialDirection::Left => {
                                    (crate::windows::TileAxis::Horizontal, false)
                                }
                                SpatialDirection::Right => {
                                    (crate::windows::TileAxis::Horizontal, true)
                                }
                                SpatialDirection::Up => (crate::windows::TileAxis::Vertical, true),
                                SpatialDirection::Down => {
                                    (crate::windows::TileAxis::Vertical, false)
                                }
                            };
                            // With no split on that axis the border is a docked terminal's, such
                            // as an agent on the right, so that terminal resizes instead.
                            let split = self
                                .core
                                .graph
                                .resized_split(pane, axis, if grow { step } else { -step })
                                .map(|(first, second, ratio)| EditorOperation::SetSplitRatio {
                                    first,
                                    second,
                                    ratio,
                                });
                            if let Some(split) = split {
                                plan.operations.push(split);
                            } else if let Some(dock) = self.docked_terminal_on(axis) {
                                // The editor grows as the terminal shrinks: right grows the code
                                // and narrows a right dock, up grows it and lowers a bottom one.
                                let toward = match direction {
                                    SpatialDirection::Up => SpatialDirection::Down,
                                    SpatialDirection::Down => SpatialDirection::Up,
                                    other => other,
                                };
                                plan.native = Some(
                                    self.directional_native_placement(
                                        dock,
                                        toward,
                                        scaled_step(policy.terminal_step),
                                    )?
                                    .map_or(NativeAction::None, |p| NativeAction::Place(dock, p)),
                                );
                            } else {
                                plan.native = Some(NativeAction::None);
                            }
                        }
                        WindowPlacement::Docked { edge, size } => {
                            let grow = match (edge, direction) {
                                (crate::windows::DockEdge::Bottom, SpatialDirection::Up)
                                | (crate::windows::DockEdge::Top, SpatialDirection::Down)
                                | (crate::windows::DockEdge::Left, SpatialDirection::Right)
                                | (crate::windows::DockEdge::Right, SpatialDirection::Left) => true,
                                (crate::windows::DockEdge::Bottom, SpatialDirection::Down)
                                | (crate::windows::DockEdge::Top, SpatialDirection::Up)
                                | (crate::windows::DockEdge::Left, SpatialDirection::Left)
                                | (crate::windows::DockEdge::Right, SpatialDirection::Right) => {
                                    false
                                }
                                _ => {
                                    plan.native = Some(NativeAction::None);
                                    return Ok(plan);
                                }
                            };
                            plan.native = Some(NativeAction::PlaceEditor(
                                pane,
                                WindowPlacement::Docked {
                                    edge,
                                    size: size
                                        .saturating_add_signed(if grow { step } else { -step })
                                        .max(1),
                                },
                            ));
                        }
                        WindowPlacement::Floating(mut rect) => {
                            match direction {
                                SpatialDirection::Left => {
                                    rect.width = rect
                                        .width
                                        .saturating_sub(u16::try_from(step).unwrap_or(u16::MAX))
                                        .max(1)
                                }
                                SpatialDirection::Right => {
                                    rect.width = rect.width.saturating_add_signed(step).max(1)
                                }
                                SpatialDirection::Up => {
                                    rect.height = rect.height.saturating_add_signed(step).max(1)
                                }
                                SpatialDirection::Down => {
                                    rect.height = rect
                                        .height
                                        .saturating_sub(u16::try_from(step).unwrap_or(u16::MAX))
                                        .max(1)
                                }
                            }
                            plan.native = Some(NativeAction::PlaceEditor(
                                pane,
                                WindowPlacement::Floating(rect),
                            ));
                        }
                        WindowPlacement::Fullscreen
                        | WindowPlacement::Hidden
                        | WindowPlacement::Overlay(_) => {
                            return Err(CommandError::Rejected(
                                "focused window cannot be directionally resized".into(),
                            ));
                        }
                    }
                }
            }
            WindowCommand::Close(name) => {
                let window = target(name)?;
                bind_target(&mut plan, window)?;
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
                        .state()
                        .window_view(pane)
                        .and_then(|view| source.views().get(view))
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
                if first == second {
                    bind_target(&mut plan, first)?;
                }
                plan.native = if first == second {
                    Some(NativeAction::None)
                } else {
                    match (
                        self.core.native.target_for_window(first),
                        self.core.native.target_for_window(second),
                    ) {
                        (Some(a), Some(b)) => Some(NativeAction::Swap(a, b)),
                        (None, None) => {
                            let first_pane = pane(first)?;
                            let second_pane = pane(second)?;
                            let first_placement = self
                                .core
                                .graph
                                .window(first)
                                .ok_or_else(|| {
                                    CommandError::Rejected("first window is stale".into())
                                })?
                                .placement;
                            let second_placement = self
                                .core
                                .graph
                                .window(second)
                                .ok_or_else(|| {
                                    CommandError::Rejected("second window is stale".into())
                                })?
                                .placement;
                            match (first_placement, second_placement) {
                                (WindowPlacement::Tiled, WindowPlacement::Tiled) => {
                                    plan.operations.push(EditorOperation::SwapPanes {
                                        first: first_pane,
                                        second: second_pane,
                                    });
                                    None
                                }
                                (WindowPlacement::Tiled, _) | (_, WindowPlacement::Tiled) => {
                                    return Err(CommandError::Rejected(
                                        "cannot swap tiled and non-tiled editor windows".into(),
                                    ));
                                }
                                _ => {
                                    let focused = self.core.graph.focused();
                                    let focus = if first_placement == WindowPlacement::Fullscreen
                                        || (focused == Some(first)
                                            && second_placement == WindowPlacement::Hidden)
                                    {
                                        Some((second_pane, second))
                                    } else if second_placement == WindowPlacement::Fullscreen
                                        || (focused == Some(second)
                                            && first_placement == WindowPlacement::Hidden)
                                    {
                                        Some((first_pane, first))
                                    } else {
                                        None
                                    };
                                    if let Some((pane, window)) = focus {
                                        plan.operations.push(EditorOperation::FocusPaneId { pane });
                                        plan.target = Some(window);
                                        plan.focus_editor = true;
                                    }
                                    Some(NativeAction::SwapEditors(first_pane, second_pane))
                                }
                            }
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
    pub(in crate::app) fn accept_window_command(
        &mut self,
        plan: WindowPlan,
        source: &crate::editor::EditorWindowVersion,
        catalog: &PluginCatalogVersion,
        state: &mut crate::editor::EditorState,
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
                self.commit(candidate, state)?;
                return Ok(CommandEffect::ClonedExplorer(view));
            }
            NativeAction::Place(target, placement) => {
                self.place_native(target, placement, state)?
            }
            NativeAction::PlaceEditor(window, placement) => {
                let mut candidate = self.candidate()?;
                candidate
                    .core
                    .graph
                    .place(window, placement)
                    .map_err(|error| error.to_string())?;
                if candidate.core.graph.focused().is_none() {
                    candidate.dashboard = Some(true);
                }
                self.commit(candidate, state)?;
            }
            NativeAction::ShowEditor(window) => {
                let mut candidate = self.candidate()?;
                candidate
                    .core
                    .graph
                    .show_window(window)
                    .map_err(|error| error.to_string())?;
                candidate.dashboard = Some(false);
                self.commit(candidate, state)?;
            }
            NativeAction::Swap(a, b) => self.swap_native(a, b, state)?,
            NativeAction::SwapEditors(first, second) => {
                let mut candidate = self.candidate()?;
                candidate
                    .core
                    .graph
                    .swap_non_tiled_placements(first, second)
                    .map_err(|error| error.to_string())?;
                self.commit(candidate, state)?;
            }
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
                self.commit(candidate, state)?;
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
