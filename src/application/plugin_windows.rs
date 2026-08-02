use std::collections::{BTreeMap, BTreeSet};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    plugins::{
        PluginContributionIndex, PluginPresenterKind, PluginPrivatePatch, PluginPrivateRect,
        PluginPrivateSnapshot, PluginPrivateStyle, PluginRequestedPlacement, PluginSurfaceGeometry,
        PluginUiError, PluginUiErrorCode, PluginUiEvent, PluginUiNodeKind, PluginUiRequest,
        PluginUiRequestKind, PluginUiStyle, PluginUiTree, PluginWindowPlacementKind,
    },
    terminal_ui::{
        PluginSurfaceRow, PluginSurfaceUiState, PrivateSurfaceCell, PrivateSurfaceUiState,
    },
    windows::{
        ContentAdapterKind, ContentId, ContentTypeId, DockEdge, ViewId, WindowError, WindowGraph,
        WindowId, WindowPlacement, WindowRect,
    },
};

const MAXIMUM_PLUGIN_UI_NODES: usize = 256;
const MAXIMUM_PLUGIN_UI_DEPTH: usize = 32;
const MAXIMUM_PLUGIN_UI_CHILDREN: usize = 1_024;
const MAXIMUM_PLUGIN_UI_TEXT_BYTES: usize = 64 * 1024;
const MAXIMUM_PLUGIN_UI_NODE_TEXT_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PluginPrivateState {
    dimensions: Option<(u16, u16)>,
    dimensions_revision: u64,
    snapshot: Option<PluginPrivateSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PluginWindowInstance {
    plugin: String,
    load: u64,
    checksum: String,
    content_type: String,
    content: ContentId,
    view: ViewId,
    window: WindowId,
    presenter: PluginPresenterKind,
    tree: Option<PluginUiTree>,
    private: Option<PluginPrivateState>,
    focused_node: Option<u64>,
    scroll: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PluginWindowHost {
    next_handle: u64,
    instances: BTreeMap<u64, PluginWindowInstance>,
    configured_placements: BTreeMap<String, PluginRequestedPlacement>,
}

impl PluginWindowHost {
    pub(super) fn set_configured_rules(
        &mut self,
        windows: &crate::configuration::WindowsConfig,
    ) -> Result<(), String> {
        let mut placements = BTreeMap::new();
        for rule in &windows.rules {
            if !rule.content_type.starts_with("plugin.") {
                continue;
            }
            let placement = match &rule.placement {
                crate::configuration::WindowPlacementConfig::Automatic => continue,
                crate::configuration::WindowPlacementConfig::Docked { edge, size } => {
                    match edge.as_str() {
                        "left" => PluginRequestedPlacement::DockedLeft { size: *size },
                        "right" => PluginRequestedPlacement::DockedRight { size: *size },
                        "top" => PluginRequestedPlacement::DockedTop { size: *size },
                        "bottom" => PluginRequestedPlacement::DockedBottom { size: *size },
                        _ => return Err("invalid configured plugin dock edge".into()),
                    }
                }
                crate::configuration::WindowPlacementConfig::Floating {
                    x,
                    y,
                    width,
                    height,
                } => PluginRequestedPlacement::Floating {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                },
                crate::configuration::WindowPlacementConfig::Fullscreen => {
                    PluginRequestedPlacement::Fullscreen
                }
                crate::configuration::WindowPlacementConfig::Hidden => {
                    PluginRequestedPlacement::Hidden
                }
            };
            if placements
                .insert(rule.content_type.clone(), placement)
                .is_some()
            {
                return Err("duplicate configured plugin window rule".into());
            }
        }
        self.configured_placements = placements;
        Ok(())
    }

    pub(super) fn process(
        &mut self,
        graph: &mut WindowGraph,
        index: &PluginContributionIndex,
        request: PluginUiRequest,
    ) -> PluginUiEvent {
        let token = request.token;
        let result = self.process_checked(graph, index, request.kind);
        match result {
            Ok((handle, tree_revision, surface_revision)) => PluginUiEvent {
                token,
                handle,
                graph_revision: graph.revision(),
                tree_revision,
                surface_revision,
                geometry: None,
                action: None,
                error: None,
            },
            Err(error) => PluginUiEvent {
                token,
                handle: None,
                graph_revision: graph.revision(),
                tree_revision: None,
                surface_revision: None,
                geometry: None,
                action: None,
                error: Some(error),
            },
        }
    }

    fn process_checked(
        &mut self,
        graph: &mut WindowGraph,
        index: &PluginContributionIndex,
        request: PluginUiRequestKind,
    ) -> Result<(Option<u64>, Option<u64>, Option<u64>), PluginUiError> {
        match request {
            PluginUiRequestKind::OpenWindow {
                expected_graph_revision,
                content_type,
                title,
                placement,
                tree,
            } => {
                require_graph_revision(graph, expected_graph_revision)?;
                let declaration = index
                    .content_types
                    .iter()
                    .find(|declaration| declaration.id == content_type)
                    .ok_or_else(|| {
                        ui_error(
                            PluginUiErrorCode::UndeclaredContent,
                            "content type is not owned by this plugin generation",
                        )
                    })?;
                let placement = self
                    .configured_placements
                    .get(&content_type)
                    .copied()
                    .unwrap_or(placement);
                let kind = placement_kind(placement);
                if !declaration.allowed_placements.contains(&kind) {
                    return Err(ui_error(
                        PluginUiErrorCode::DisallowedPlacement,
                        "window placement is not allowed by the static declaration",
                    ));
                }
                let count = self
                    .instances
                    .values()
                    .filter(|instance| {
                        instance.plugin == index.owner
                            && instance.load == index.generation
                            && instance.content_type == content_type
                    })
                    .count();
                if count >= usize::from(declaration.maximum_instances) {
                    return Err(ui_error(
                        PluginUiErrorCode::InstanceCeiling,
                        "plugin content instance ceiling reached",
                    ));
                }
                let (adapter, focused_node, private) = match declaration.presenter {
                    PluginPresenterKind::RetainedDeclarative => {
                        let tree = tree.as_ref().ok_or_else(|| {
                            ui_error(
                                PluginUiErrorCode::InvalidTree,
                                "retained presenter requires an initial tree",
                            )
                        })?;
                        validate_tree(index, tree, None)?;
                        (
                            ContentAdapterKind::DeclarativeUi,
                            first_focusable_node(tree),
                            None,
                        )
                    }
                    PluginPresenterKind::PrivateCells => {
                        if tree.is_some() || declaration.private_cells.is_none() {
                            return Err(ui_error(
                                PluginUiErrorCode::WrongPresenter,
                                "private presenter does not accept a retained tree",
                            ));
                        }
                        (
                            ContentAdapterKind::PrivateCellSurface,
                            None,
                            Some(PluginPrivateState::default()),
                        )
                    }
                };
                let tab = graph.active_tab().ok_or_else(|| {
                    ui_error(PluginUiErrorCode::CrossTab, "no active workspace tab")
                })?;
                let placement = graph_placement(placement)?;
                let mut candidate = graph.clone();
                let owner = format!("plugin.{}", index.owner);
                let content = candidate
                    .create_content(
                        ContentTypeId::new(content_type.clone()).map_err(map_graph_error)?,
                        adapter,
                        if title.is_empty() {
                            declaration.title.clone()
                        } else {
                            title
                        },
                        false,
                        &owner,
                        index.generation,
                    )
                    .map_err(map_graph_error)?;
                let view = candidate
                    .create_view(content, &owner, index.generation)
                    .map_err(map_graph_error)?;
                let window = candidate
                    .open_window(
                        tab,
                        view,
                        placement,
                        declaration.focusable,
                        &owner,
                        index.generation,
                    )
                    .map_err(map_graph_error)?;
                candidate.validate().map_err(map_graph_error)?;
                let handle = self.allocate_handle()?;
                self.instances.insert(
                    handle,
                    PluginWindowInstance {
                        plugin: index.owner.clone(),
                        load: index.generation,
                        checksum: index.checksum.clone(),
                        content_type,
                        content,
                        view,
                        window,
                        presenter: declaration.presenter,
                        focused_node,
                        tree: tree.clone(),
                        private,
                        scroll: 0,
                    },
                );
                *graph = candidate;
                Ok((Some(handle), tree.as_ref().map(|tree| tree.revision), None))
            }
            PluginUiRequestKind::ReplaceUi {
                handle,
                expected_tree_revision,
                tree,
            } => {
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph, instance.window)?;
                let current = instance.tree.as_ref().ok_or_else(|| {
                    ui_error(
                        PluginUiErrorCode::WrongPresenter,
                        "handle is not retained UI",
                    )
                })?;
                if current.revision != expected_tree_revision {
                    return Err(ui_error(
                        PluginUiErrorCode::StaleTree,
                        "retained UI revision is stale",
                    ));
                }
                validate_tree(index, &tree, Some(expected_tree_revision))?;
                let instance = self.instances.get_mut(&handle).expect("validated handle");
                instance.focused_node = instance
                    .focused_node
                    .filter(|node| is_focusable_node(&tree, *node))
                    .or_else(|| first_focusable_node(&tree));
                instance.scroll = instance
                    .scroll
                    .min(tree_rows(&tree, None).len().saturating_sub(1));
                instance.tree = Some(tree.clone());
                Ok((Some(handle), Some(tree.revision), None))
            }
            PluginUiRequestKind::PlaceWindow {
                handle,
                expected_graph_revision,
                placement,
            } => {
                require_graph_revision(graph, expected_graph_revision)?;
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph, instance.window)?;
                let declaration = index
                    .content_types
                    .iter()
                    .find(|declaration| declaration.id == instance.content_type)
                    .ok_or_else(|| {
                        ui_error(
                            PluginUiErrorCode::GenerationLost,
                            "content declaration is stale",
                        )
                    })?;
                if !declaration
                    .allowed_placements
                    .contains(&placement_kind(placement))
                {
                    return Err(ui_error(
                        PluginUiErrorCode::DisallowedPlacement,
                        "window placement is not allowed by the static declaration",
                    ));
                }
                let mut candidate = graph.clone();
                candidate
                    .place(instance.window, graph_placement(placement)?)
                    .map_err(map_graph_error)?;
                candidate.validate().map_err(map_graph_error)?;
                *graph = candidate;
                Ok(instance_revisions(handle, instance))
            }
            PluginUiRequestKind::FocusWindow {
                handle,
                expected_graph_revision,
            } => {
                require_graph_revision(graph, expected_graph_revision)?;
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph, instance.window)?;
                if graph
                    .window(instance.window)
                    .is_some_and(|window| matches!(window.placement, WindowPlacement::Hidden))
                {
                    return Err(ui_error(
                        PluginUiErrorCode::HiddenFocus,
                        "hidden plugin window cannot receive focus",
                    ));
                }
                let mut candidate = graph.clone();
                candidate.focus(instance.window).map_err(map_graph_error)?;
                candidate.validate().map_err(map_graph_error)?;
                *graph = candidate;
                Ok(instance_revisions(handle, instance))
            }
            PluginUiRequestKind::CloseWindow {
                handle,
                expected_graph_revision,
            } => {
                require_graph_revision(graph, expected_graph_revision)?;
                let instance = self.owned_instance(index, handle)?.clone();
                require_active_window(graph, instance.window)?;
                let mut candidate = graph.clone();
                candidate
                    .close_window(instance.window)
                    .and_then(|_| candidate.remove_view(instance.view))
                    .and_then(|_| candidate.remove_content(instance.content))
                    .map_err(map_graph_error)?;
                candidate.validate().map_err(map_graph_error)?;
                self.instances.remove(&handle);
                *graph = candidate;
                Ok(instance_revisions(handle, &instance))
            }
            PluginUiRequestKind::ReplacePrivate { handle, snapshot } => {
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph, instance.window)?;
                let declaration = private_declaration(index, instance)?;
                let private = instance.private.as_ref().ok_or_else(|| {
                    ui_error(
                        PluginUiErrorCode::WrongPresenter,
                        "handle is not private cells",
                    )
                })?;
                validate_private_snapshot(
                    declaration,
                    private,
                    &snapshot,
                    private.snapshot.as_ref().map(|snapshot| snapshot.revision),
                )?;
                self.instances
                    .get_mut(&handle)
                    .expect("validated private handle")
                    .private
                    .as_mut()
                    .expect("private presenter")
                    .snapshot = Some(snapshot.clone());
                Ok((Some(handle), None, Some(snapshot.revision)))
            }
            PluginUiRequestKind::PatchPrivate { handle, patch } => {
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph, instance.window)?;
                let declaration = private_declaration(index, instance)?;
                let private = instance.private.as_ref().ok_or_else(|| {
                    ui_error(
                        PluginUiErrorCode::WrongPresenter,
                        "handle is not private cells",
                    )
                })?;
                let candidate = apply_private_patch(declaration, private, &patch)?;
                self.instances
                    .get_mut(&handle)
                    .expect("validated private handle")
                    .private
                    .as_mut()
                    .expect("private presenter")
                    .snapshot = Some(candidate.clone());
                Ok((Some(handle), None, Some(candidate.revision)))
            }
        }
    }

    fn allocate_handle(&mut self) -> Result<u64, PluginUiError> {
        let handle = self.next_handle.max(1);
        self.next_handle = handle.checked_add(1).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::InternalRejected,
                "plugin window handle space exhausted",
            )
        })?;
        Ok(handle)
    }

    fn owned_instance(
        &self,
        index: &PluginContributionIndex,
        handle: u64,
    ) -> Result<&PluginWindowInstance, PluginUiError> {
        let instance = self.instances.get(&handle).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::StaleHandle,
                "plugin window handle is stale",
            )
        })?;
        if instance.plugin != index.owner
            || instance.load != index.generation
            || instance.checksum != index.checksum
        {
            return Err(ui_error(
                PluginUiErrorCode::ForeignGeneration,
                "plugin window handle belongs to another generation",
            ));
        }
        Ok(instance)
    }

    pub(super) fn reset_runtime(&mut self, graph: &mut WindowGraph) -> Result<(), WindowError> {
        let owners = self
            .instances
            .values()
            .map(|instance| (instance.plugin.clone(), instance.load))
            .collect::<BTreeSet<_>>();
        for (plugin, load) in owners {
            graph.remove_owner_generation(&format!("plugin.{plugin}"), load)?;
        }
        self.instances.clear();
        Ok(())
    }

    pub(super) fn remove_generation(
        &mut self,
        graph: &mut WindowGraph,
        plugin: &str,
        load: u64,
    ) -> Result<bool, WindowError> {
        let owner = format!("plugin.{plugin}");
        let removed = self
            .instances
            .values()
            .any(|instance| instance.plugin == plugin && instance.load == load);
        graph.remove_owner_generation(&owner, load)?;
        self.instances
            .retain(|_, instance| !(instance.plugin == plugin && instance.load == load));
        Ok(removed)
    }

    pub(super) fn input_context(
        &self,
        window: WindowId,
        catalog: &crate::plugins::PluginContributionCatalog,
    ) -> Option<String> {
        let instance = self
            .instances
            .values()
            .find(|instance| instance.window == window)?;
        let index = catalog.get(&instance.plugin).filter(|index| {
            index.generation == instance.load && index.checksum == instance.checksum
        })?;
        index
            .content_types
            .iter()
            .find(|content| content.id == instance.content_type)
            .map(|content| content.context.clone())
    }

    pub(super) fn snapshots(&self) -> Vec<PluginSurfaceUiState> {
        self.instances
            .iter()
            .filter_map(|(handle, instance)| {
                let tree = instance.tree.as_ref()?;
                Some(PluginSurfaceUiState {
                    handle: *handle,
                    window: instance.window,
                    tree_revision: tree.revision,
                    scroll: instance.scroll,
                    rows: tree_rows(tree, instance.focused_node),
                })
            })
            .collect()
    }

    pub(super) fn private_snapshots(&self, graph: &WindowGraph) -> Vec<PrivateSurfaceUiState> {
        self.instances
            .iter()
            .filter_map(|(handle, instance)| {
                let private = instance.private.as_ref()?;
                let (width, height) = private.dimensions?;
                let snapshot = private.snapshot.as_ref()?;
                (snapshot.dimensions_revision == private.dimensions_revision).then(|| {
                    let styles = snapshot
                        .styles
                        .iter()
                        .map(|style| (style.id, private_style(style)))
                        .collect::<BTreeMap<_, _>>();
                    PrivateSurfaceUiState {
                        handle: *handle,
                        window: instance.window,
                        dimensions_revision: private.dimensions_revision,
                        surface_revision: snapshot.revision,
                        width,
                        height,
                        cells: snapshot
                            .cells
                            .iter()
                            .map(|cell| PrivateSurfaceCell {
                                x: cell.x,
                                y: cell.y,
                                grapheme: cell.grapheme.clone(),
                                continuation: cell.continuation,
                                style: styles
                                    .get(&cell.style)
                                    .copied()
                                    .unwrap_or(crate::terminal_ui::Style::Default),
                            })
                            .collect(),
                        cursor: snapshot.cursor.map(|cursor| (cursor.x, cursor.y)),
                        focused: graph.focused() == Some(instance.window),
                    }
                })
            })
            .collect()
    }

    pub(super) fn interact_focused(
        &mut self,
        graph: &WindowGraph,
        catalog: &crate::plugins::PluginContributionCatalog,
        interaction: PluginUiInteraction,
    ) -> Result<Option<String>, PluginUiError> {
        let window = graph
            .focused()
            .ok_or_else(|| ui_error(PluginUiErrorCode::StaleHandle, "no focused plugin window"))?;
        let (_, instance) = self
            .instances
            .iter_mut()
            .find(|(_, instance)| instance.window == window)
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::StaleHandle,
                    "focused window is not retained plugin UI",
                )
            })?;
        let index = catalog
            .get(&instance.plugin)
            .filter(|index| {
                index.generation == instance.load && index.checksum == instance.checksum
            })
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::GenerationLost,
                    "plugin contribution generation is unavailable",
                )
            })?;
        require_active_window(graph, instance.window)?;
        let tree = instance.tree.as_ref().ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::WrongPresenter,
                "focused handle is not retained UI",
            )
        })?;
        let focusable = focusable_nodes(tree);
        match interaction {
            PluginUiInteraction::Focus(delta) => {
                if focusable.is_empty() {
                    instance.focused_node = None;
                    return Ok(None);
                }
                let current = instance
                    .focused_node
                    .and_then(|node| focusable.iter().position(|candidate| *candidate == node))
                    .unwrap_or(0);
                let next = if delta < 0 {
                    current.saturating_sub(delta.unsigned_abs())
                } else {
                    current
                        .saturating_add(delta as usize)
                        .min(focusable.len() - 1)
                };
                instance.focused_node = Some(focusable[next]);
                ensure_focused_visible(instance);
                Ok(None)
            }
            PluginUiInteraction::Scroll(delta) => {
                let maximum = tree_rows(tree, instance.focused_node)
                    .len()
                    .saturating_sub(1);
                instance.scroll = if delta < 0 {
                    instance.scroll.saturating_sub(delta.unsigned_abs())
                } else {
                    instance.scroll.saturating_add(delta as usize).min(maximum)
                };
                Ok(None)
            }
            PluginUiInteraction::Activate => {
                let node = instance
                    .focused_node
                    .and_then(|id| tree.nodes.iter().find(|node| node.id == id));
                let action = node.and_then(|node| node.action.clone()).ok_or_else(|| {
                    ui_error(
                        PluginUiErrorCode::UndeclaredAction,
                        "focused retained node has no action",
                    )
                })?;
                let local = action.strip_prefix(&format!("plugin.{}.", instance.plugin));
                if !local.is_some_and(|command| index.commands.contains_key(command)) {
                    return Err(ui_error(
                        PluginUiErrorCode::UndeclaredAction,
                        "focused retained action is no longer declared",
                    ));
                }
                Ok(Some(action))
            }
        }
    }

    pub(super) fn focus_node_at_row(
        &mut self,
        window: WindowId,
        visible_row: usize,
    ) -> Result<(), PluginUiError> {
        let instance = self
            .instances
            .values_mut()
            .find(|instance| instance.window == window)
            .ok_or_else(|| ui_error(PluginUiErrorCode::StaleHandle, "plugin window is stale"))?;
        let tree = instance.tree.as_ref().ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::WrongPresenter,
                "window is not retained UI",
            )
        })?;
        let rows = tree_rows(tree, instance.focused_node);
        let row = instance.scroll.saturating_add(visible_row);
        let node = rows.get(row).map(|row| row.node).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::InvalidTree,
                "mouse row is outside retained UI",
            )
        })?;
        if is_focusable_node(tree, node) {
            instance.focused_node = Some(node);
        }
        Ok(())
    }

    pub(super) fn synchronize_private_geometry(
        &mut self,
        projection: Option<&crate::windows::CompositorProjection>,
    ) -> Vec<(String, u64, PluginUiEvent)> {
        let mut events = Vec::new();
        for (handle, instance) in &mut self.instances {
            let Some(private) = instance.private.as_mut() else {
                continue;
            };
            let dimensions = projection
                .and_then(|projection| projection.window(instance.window))
                .map(|window| (window.rect.width, window.rect.height))
                .filter(|(width, height)| *width > 0 && *height > 0);
            if private.dimensions == dimensions {
                continue;
            }
            let Some(revision) = private.dimensions_revision.checked_add(1) else {
                private.dimensions = None;
                private.snapshot = None;
                continue;
            };
            private.dimensions = dimensions;
            private.dimensions_revision = revision;
            private.snapshot = None;
            let geometry = dimensions.map(|(width, height)| PluginSurfaceGeometry {
                handle: *handle,
                width,
                height,
                dimensions_revision: revision,
            });
            events.push((
                instance.plugin.clone(),
                instance.load,
                PluginUiEvent {
                    token: 0,
                    handle: Some(*handle),
                    graph_revision: None,
                    tree_revision: None,
                    surface_revision: None,
                    geometry,
                    action: None,
                    error: dimensions.is_none().then(|| {
                        ui_error(
                            PluginUiErrorCode::StaleDimensions,
                            "private surface has no writable local geometry",
                        )
                    }),
                },
            ));
        }
        events
    }

    pub(super) fn private_action_at(
        &self,
        window: WindowId,
        x: u16,
        y: u16,
        catalog: &crate::plugins::PluginContributionCatalog,
    ) -> Result<(String, u64, PluginUiEvent), PluginUiError> {
        let (handle, instance) = self
            .instances
            .iter()
            .find(|(_, instance)| instance.window == window)
            .ok_or_else(|| ui_error(PluginUiErrorCode::StaleHandle, "private window is stale"))?;
        let private = instance.private.as_ref().ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::WrongPresenter,
                "window is not private cells",
            )
        })?;
        let snapshot = private.snapshot.as_ref().ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::StaleSurface,
                "private surface has no snapshot",
            )
        })?;
        let index = catalog
            .get(&instance.plugin)
            .filter(|index| {
                index.generation == instance.load && index.checksum == instance.checksum
            })
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::GenerationLost,
                    "private surface generation is unavailable",
                )
            })?;
        let action = snapshot
            .hit_regions
            .iter()
            .rev()
            .find(|hit| rect_contains(hit.x, hit.y, hit.width, hit.height, x, y))
            .map(|hit| hit.action.clone())
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::InvalidHitRegion,
                    "no private action at local point",
                )
            })?;
        validate_private_action(index, &action)?;
        Ok((
            instance.plugin.clone(),
            instance.load,
            PluginUiEvent {
                token: 0,
                handle: Some(*handle),
                graph_revision: None,
                tree_revision: None,
                surface_revision: Some(snapshot.revision),
                geometry: None,
                action: Some(action),
                error: None,
            },
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PluginUiInteraction {
    Focus(isize),
    Scroll(isize),
    Activate,
}

fn instance_revisions(
    handle: u64,
    instance: &PluginWindowInstance,
) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        Some(handle),
        instance.tree.as_ref().map(|tree| tree.revision),
        instance
            .private
            .as_ref()
            .and_then(|private| private.snapshot.as_ref())
            .map(|snapshot| snapshot.revision),
    )
}

fn ui_error(code: PluginUiErrorCode, detail: impl std::fmt::Display) -> PluginUiError {
    PluginUiError::new(code, detail)
}

fn map_graph_error(error: WindowError) -> PluginUiError {
    let code = match error {
        WindowError::InvalidPlacement => PluginUiErrorCode::InvalidPlacement,
        WindowError::NotFocusable => PluginUiErrorCode::HiddenFocus,
        WindowError::ViewStillHosted | WindowError::ContentStillViewed => {
            PluginUiErrorCode::CloseOrder
        }
        WindowError::TooManyTiledWindows
        | WindowError::TooManyNonTiledWindows
        | WindowError::TooManyOverlayWindows
        | WindowError::MultipleFullscreenWindows
        | WindowError::TiledLayoutTooDeep
        | WindowError::RevisionExhausted
        | WindowError::IdentifierExhausted => PluginUiErrorCode::GraphCeiling,
        WindowError::WindowOutsideActiveTab | WindowError::FocusOutsideActiveTab => {
            PluginUiErrorCode::CrossTab
        }
        _ => PluginUiErrorCode::InternalRejected,
    };
    ui_error(code, error)
}

fn require_active_window(graph: &WindowGraph, window: WindowId) -> Result<(), PluginUiError> {
    let active = graph
        .active_tab()
        .ok_or_else(|| ui_error(PluginUiErrorCode::CrossTab, "no active workspace tab"))?;
    if graph
        .window(window)
        .is_some_and(|window| window.tab == active)
    {
        Ok(())
    } else {
        Err(ui_error(
            PluginUiErrorCode::CrossTab,
            "plugin window operation crosses the active workspace tab",
        ))
    }
}

fn require_graph_revision(graph: &WindowGraph, expected: u64) -> Result<(), PluginUiError> {
    if graph.revision() == Some(expected) {
        Ok(())
    } else {
        Err(ui_error(
            PluginUiErrorCode::StaleGraph,
            "window graph revision is stale or exhausted",
        ))
    }
}

fn placement_kind(placement: PluginRequestedPlacement) -> PluginWindowPlacementKind {
    match placement {
        PluginRequestedPlacement::DockedLeft { .. }
        | PluginRequestedPlacement::DockedRight { .. }
        | PluginRequestedPlacement::DockedTop { .. }
        | PluginRequestedPlacement::DockedBottom { .. } => PluginWindowPlacementKind::Docked,
        PluginRequestedPlacement::Floating { .. } => PluginWindowPlacementKind::Floating,
        PluginRequestedPlacement::Fullscreen => PluginWindowPlacementKind::Fullscreen,
        PluginRequestedPlacement::Hidden => PluginWindowPlacementKind::Hidden,
    }
}

fn graph_placement(placement: PluginRequestedPlacement) -> Result<WindowPlacement, PluginUiError> {
    let placement = match placement {
        PluginRequestedPlacement::DockedLeft { size } => WindowPlacement::Docked {
            edge: DockEdge::Left,
            size,
        },
        PluginRequestedPlacement::DockedRight { size } => WindowPlacement::Docked {
            edge: DockEdge::Right,
            size,
        },
        PluginRequestedPlacement::DockedTop { size } => WindowPlacement::Docked {
            edge: DockEdge::Top,
            size,
        },
        PluginRequestedPlacement::DockedBottom { size } => WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size,
        },
        PluginRequestedPlacement::Floating {
            x,
            y,
            width,
            height,
        } => WindowPlacement::Floating(WindowRect {
            x,
            y,
            width,
            height,
        }),
        PluginRequestedPlacement::Fullscreen => WindowPlacement::Fullscreen,
        PluginRequestedPlacement::Hidden => WindowPlacement::Hidden,
    };
    placement
        .validate()
        .map_err(|error| ui_error(PluginUiErrorCode::InvalidPlacement, error))
}

fn private_declaration<'a>(
    index: &'a PluginContributionIndex,
    instance: &PluginWindowInstance,
) -> Result<&'a crate::plugins::PluginContentTypeContribution, PluginUiError> {
    index
        .content_types
        .iter()
        .find(|declaration| {
            declaration.id == instance.content_type
                && declaration.presenter == PluginPresenterKind::PrivateCells
                && declaration.private_cells.is_some()
        })
        .ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::WrongPresenter,
                "content was not declared as private cells",
            )
        })
}

fn validate_private_action(
    index: &PluginContributionIndex,
    action: &str,
) -> Result<(), PluginUiError> {
    let namespace = format!("plugin.{}.", index.owner);
    if action
        .strip_prefix(&namespace)
        .is_some_and(|command| index.commands.contains_key(command))
    {
        Ok(())
    } else {
        Err(ui_error(
            PluginUiErrorCode::UndeclaredAction,
            "private hit action was not statically declared",
        ))
    }
}

fn rect_contains(x: u16, y: u16, width: u16, height: u16, px: u16, py: u16) -> bool {
    width > 0
        && height > 0
        && px >= x
        && py >= y
        && px < x.saturating_add(width)
        && py < y.saturating_add(height)
}

fn rects_overlap(left: PluginPrivateRect, right: PluginPrivateRect) -> bool {
    left.x < right.x.saturating_add(right.width)
        && right.x < left.x.saturating_add(left.width)
        && left.y < right.y.saturating_add(right.height)
        && right.y < left.y.saturating_add(left.height)
}

fn rect_in_bounds(rect: PluginPrivateRect, width: u16, height: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect
            .x
            .checked_add(rect.width)
            .is_some_and(|right| right <= width)
        && rect
            .y
            .checked_add(rect.height)
            .is_some_and(|bottom| bottom <= height)
}

fn validate_private_snapshot(
    declaration: &crate::plugins::PluginContentTypeContribution,
    private: &PluginPrivateState,
    snapshot: &PluginPrivateSnapshot,
    previous_revision: Option<u64>,
) -> Result<(), PluginUiError> {
    let policy = declaration.private_cells.as_ref().ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::WrongPresenter,
            "private policy is missing",
        )
    })?;
    let (width, height) = private.dimensions.ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private surface has no writable dimensions",
        )
    })?;
    if snapshot.dimensions_revision != private.dimensions_revision {
        return Err(ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private surface dimensions revision is stale",
        ));
    }
    if snapshot.revision == 0
        || previous_revision.is_some_and(|revision| snapshot.revision <= revision)
    {
        return Err(ui_error(
            PluginUiErrorCode::StaleSurface,
            "private surface revision is stale",
        ));
    }
    if snapshot.cells.len() > usize::from(policy.maximum_cells)
        || snapshot.styles.len() > usize::from(policy.maximum_styles)
        || snapshot.hit_regions.len() > usize::from(policy.maximum_hit_regions)
    {
        return Err(ui_error(
            PluginUiErrorCode::PatchCeiling,
            "private snapshot exceeds its declaration",
        ));
    }
    let mut bytes = 0_usize;
    let mut styles = BTreeSet::new();
    for style in &snapshot.styles {
        if style.id == 0 || !styles.insert(style.id) {
            return Err(ui_error(
                PluginUiErrorCode::InvalidStyle,
                "private styles require unique nonzero IDs",
            ));
        }
        bytes = bytes.checked_add(8).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::PatchCeiling,
                "private byte count overflow",
            )
        })?;
    }
    let mut cells = BTreeMap::new();
    for cell in &snapshot.cells {
        bytes = bytes.checked_add(cell.grapheme.len()).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::PatchCeiling,
                "private byte count overflow",
            )
        })?;
        if cell.x >= width || cell.y >= height {
            return Err(ui_error(
                PluginUiErrorCode::InvalidLocalBounds,
                "private cell leaves local bounds",
            ));
        }
        if !styles.contains(&cell.style) {
            return Err(ui_error(
                PluginUiErrorCode::InvalidStyle,
                "private cell references an unknown style",
            ));
        }
        if cells.insert((cell.x, cell.y), cell).is_some() {
            return Err(ui_error(
                PluginUiErrorCode::CellOverlap,
                "private cell coordinate is duplicated",
            ));
        }
        if cell.continuation {
            if !cell.grapheme.is_empty() {
                return Err(ui_error(
                    PluginUiErrorCode::SplitWideGrapheme,
                    "wide continuation must have empty text",
                ));
            }
        } else {
            let mut graphemes = cell.grapheme.graphemes(true);
            let Some(grapheme) = graphemes.next() else {
                return Err(ui_error(
                    PluginUiErrorCode::InvalidGrapheme,
                    "private lead cell is empty",
                ));
            };
            let width = UnicodeWidthStr::width(grapheme);
            if graphemes.next().is_some()
                || grapheme.chars().any(char::is_control)
                || !(1..=2).contains(&width)
            {
                return Err(ui_error(
                    PluginUiErrorCode::InvalidGrapheme,
                    "private lead cell must contain one printable grapheme",
                ));
            }
        }
    }
    for ((x, y), cell) in &cells {
        if cell.continuation {
            let lead = x
                .checked_sub(1)
                .and_then(|x| cells.get(&(x, *y)))
                .filter(|lead| {
                    !lead.continuation && UnicodeWidthStr::width(lead.grapheme.as_str()) == 2
                });
            if lead.is_none() {
                return Err(ui_error(
                    PluginUiErrorCode::SplitWideGrapheme,
                    "private continuation has no width-two lead",
                ));
            }
        } else if UnicodeWidthStr::width(cell.grapheme.as_str()) == 2
            && !cells
                .get(&(x.saturating_add(1), *y))
                .is_some_and(|continuation| continuation.continuation)
        {
            return Err(ui_error(
                PluginUiErrorCode::SplitWideGrapheme,
                "width-two private lead has no continuation",
            ));
        }
    }
    if let Some(cursor) = snapshot.cursor {
        if !policy.cursor || cursor.x >= width || cursor.y >= height {
            return Err(ui_error(
                PluginUiErrorCode::InvalidCursor,
                "private cursor is disallowed or outside local bounds",
            ));
        }
    }
    let mut hit_rectangles = Vec::new();
    for hit in &snapshot.hit_regions {
        let rect = PluginPrivateRect {
            x: hit.x,
            y: hit.y,
            width: hit.width,
            height: hit.height,
        };
        bytes = bytes.checked_add(hit.action.len()).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::PatchCeiling,
                "private byte count overflow",
            )
        })?;
        if !rect_in_bounds(rect, width, height)
            || hit.action != declaration.command
            || hit_rectangles
                .iter()
                .any(|existing| rects_overlap(*existing, rect))
        {
            return Err(ui_error(
                PluginUiErrorCode::InvalidHitRegion,
                "private hit region is outside bounds, overlaps, or is undeclared",
            ));
        }
        hit_rectangles.push(rect);
    }
    if bytes > usize::try_from(policy.maximum_bytes).unwrap_or(usize::MAX) {
        return Err(ui_error(
            PluginUiErrorCode::PatchCeiling,
            "private snapshot byte ceiling exceeded",
        ));
    }
    Ok(())
}

fn apply_private_patch(
    declaration: &crate::plugins::PluginContentTypeContribution,
    private: &PluginPrivateState,
    patch: &PluginPrivatePatch,
) -> Result<PluginPrivateSnapshot, PluginUiError> {
    let policy = declaration.private_cells.as_ref().ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::WrongPresenter,
            "private policy is missing",
        )
    })?;
    let current = private.snapshot.as_ref().ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::StaleSurface,
            "private surface has no base snapshot",
        )
    })?;
    if patch.expected_surface_revision != current.revision {
        return Err(ui_error(
            PluginUiErrorCode::StaleSurface,
            "private patch base revision is stale",
        ));
    }
    if patch.dimensions_revision != private.dimensions_revision {
        return Err(ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private patch dimensions revision is stale",
        ));
    }
    if patch.cells.len() > usize::from(policy.maximum_patch_cells)
        || patch.styles.len() > usize::from(policy.maximum_styles)
        || patch.hit_regions.len() > usize::from(policy.maximum_hit_regions)
        || patch.dirty.is_empty()
        || patch.dirty.len() > usize::from(policy.maximum_dirty_rectangles)
    {
        return Err(ui_error(
            PluginUiErrorCode::PatchCeiling,
            "private patch exceeds its declaration",
        ));
    }
    let (width, height) = private.dimensions.ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private dimensions are missing",
        )
    })?;
    for (index, rect) in patch.dirty.iter().enumerate() {
        if !rect_in_bounds(*rect, width, height)
            || patch.dirty[..index]
                .iter()
                .any(|previous| rects_overlap(*previous, *rect))
        {
            return Err(ui_error(
                PluginUiErrorCode::InvalidLocalBounds,
                "private dirty rectangles overlap or leave local bounds",
            ));
        }
    }
    if patch.cells.iter().any(|cell| {
        !patch
            .dirty
            .iter()
            .any(|rect| rect_contains(rect.x, rect.y, rect.width, rect.height, cell.x, cell.y))
    }) {
        return Err(ui_error(
            PluginUiErrorCode::InvalidLocalBounds,
            "private patch cell is outside every dirty rectangle",
        ));
    }
    let mut candidate = current.clone();
    candidate.revision = patch.revision;
    candidate.dimensions_revision = patch.dimensions_revision;
    candidate.cells.retain(|cell| {
        !patch
            .dirty
            .iter()
            .any(|rect| rect_contains(rect.x, rect.y, rect.width, rect.height, cell.x, cell.y))
    });
    candidate.cells.extend(patch.cells.clone());
    for style in &patch.styles {
        candidate.styles.retain(|existing| existing.id != style.id);
        candidate.styles.push(style.clone());
    }
    if patch.replace_cursor {
        candidate.cursor = patch.cursor;
    }
    if patch.replace_hit_regions {
        candidate.hit_regions = patch.hit_regions.clone();
    }
    validate_private_snapshot(declaration, private, &candidate, Some(current.revision))?;
    Ok(candidate)
}

fn private_style(style: &PluginPrivateStyle) -> crate::terminal_ui::Style {
    match style.style {
        PluginUiStyle::Default => crate::terminal_ui::Style::Default,
        PluginUiStyle::Muted => crate::terminal_ui::Style::Muted,
        PluginUiStyle::Accent | PluginUiStyle::Warning => crate::terminal_ui::Style::Accent,
        PluginUiStyle::Error => crate::terminal_ui::Style::Error,
        PluginUiStyle::Selected => crate::terminal_ui::Style::Selection,
        PluginUiStyle::Border => crate::terminal_ui::Style::Border,
    }
}

fn validate_tree(
    index: &PluginContributionIndex,
    tree: &PluginUiTree,
    previous_revision: Option<u64>,
) -> Result<(), PluginUiError> {
    if tree.revision == 0
        || previous_revision.is_some_and(|revision| tree.revision <= revision)
        || tree.nodes.is_empty()
        || tree.nodes.len() > MAXIMUM_PLUGIN_UI_NODES
    {
        return Err(ui_error(
            PluginUiErrorCode::InvalidTree,
            "invalid retained UI revision or node count",
        ));
    }
    let mut nodes = BTreeMap::new();
    let mut text_bytes = 0_usize;
    let mut child_count = 0_usize;
    let namespace = format!("plugin.{}.", index.owner);
    for node in &tree.nodes {
        text_bytes = text_bytes.checked_add(node.text.len()).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::InvalidTree,
                "retained UI byte count overflow",
            )
        })?;
        child_count = child_count
            .checked_add(node.children.len())
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::InvalidTree,
                    "retained UI child count overflow",
                )
            })?;
        let action_valid = node.action.as_ref().is_none_or(|action| {
            action
                .strip_prefix(&namespace)
                .is_some_and(|command| index.commands.contains_key(command))
                && node.kind == PluginUiNodeKind::Button
        });
        if !action_valid {
            return Err(ui_error(
                PluginUiErrorCode::UndeclaredAction,
                "retained UI action is undeclared or not attached to a button",
            ));
        }
        if node.id == 0
            || node.text.len() > MAXIMUM_PLUGIN_UI_NODE_TEXT_BYTES
            || node.text.chars().any(char::is_control)
            || nodes.insert(node.id, node).is_some()
        {
            return Err(ui_error(
                PluginUiErrorCode::InvalidTree,
                "invalid retained UI node",
            ));
        }
    }
    if text_bytes > MAXIMUM_PLUGIN_UI_TEXT_BYTES || child_count > MAXIMUM_PLUGIN_UI_CHILDREN {
        return Err(ui_error(
            PluginUiErrorCode::InvalidTree,
            "retained UI aggregate ceiling exceeded",
        ));
    }
    let mut visited = BTreeSet::new();
    let mut stack = BTreeSet::new();
    fn visit(
        id: u64,
        depth: usize,
        nodes: &BTreeMap<u64, &crate::plugins::PluginUiNode>,
        visited: &mut BTreeSet<u64>,
        stack: &mut BTreeSet<u64>,
    ) -> Result<(), String> {
        if depth > MAXIMUM_PLUGIN_UI_DEPTH || !stack.insert(id) {
            return Err("retained UI depth or cycle violation".into());
        }
        let node = nodes
            .get(&id)
            .ok_or_else(|| "retained UI child is missing".to_owned())?;
        if visited.insert(id) {
            for child in &node.children {
                visit(*child, depth + 1, nodes, visited, stack)?;
            }
        }
        stack.remove(&id);
        Ok(())
    }
    visit(tree.root, 1, &nodes, &mut visited, &mut stack)
        .map_err(|error| ui_error(PluginUiErrorCode::InvalidTree, error))?;
    if visited.len() != nodes.len() {
        return Err(ui_error(
            PluginUiErrorCode::InvalidTree,
            "retained UI contains unreachable nodes",
        ));
    }
    Ok(())
}

fn focusable_nodes(tree: &PluginUiTree) -> Vec<u64> {
    let nodes = tree
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = Vec::new();
    fn collect(
        id: u64,
        nodes: &BTreeMap<u64, &crate::plugins::PluginUiNode>,
        ordered: &mut Vec<u64>,
    ) {
        let Some(node) = nodes.get(&id) else { return };
        if !node.text.is_empty()
            && (matches!(node.kind, PluginUiNodeKind::List | PluginUiNodeKind::Button)
                || node.action.is_some())
        {
            ordered.push(id);
        }
        for child in &node.children {
            collect(*child, nodes, ordered);
        }
    }
    collect(tree.root, &nodes, &mut ordered);
    ordered
}

fn first_focusable_node(tree: &PluginUiTree) -> Option<u64> {
    focusable_nodes(tree).into_iter().next()
}

fn is_focusable_node(tree: &PluginUiTree, id: u64) -> bool {
    focusable_nodes(tree).contains(&id)
}

fn ensure_focused_visible(instance: &mut PluginWindowInstance) {
    let Some(tree) = instance.tree.as_ref() else {
        return;
    };
    let rows = tree_rows(tree, instance.focused_node);
    if let Some(index) = instance
        .focused_node
        .and_then(|node| rows.iter().position(|row| row.node == node))
    {
        instance.scroll = instance.scroll.min(index);
    }
}

fn tree_rows(tree: &PluginUiTree, focused_node: Option<u64>) -> Vec<PluginSurfaceRow> {
    let nodes = tree
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    fn collect(
        id: u64,
        depth: usize,
        nodes: &BTreeMap<u64, &crate::plugins::PluginUiNode>,
        focused_node: Option<u64>,
        rows: &mut Vec<PluginSurfaceRow>,
    ) {
        let Some(node) = nodes.get(&id) else { return };
        if !node.text.is_empty() {
            rows.push(PluginSurfaceRow {
                node: id,
                depth: u16::try_from(depth).unwrap_or(u16::MAX),
                text: node.text.clone(),
                style: match node.style {
                    PluginUiStyle::Default => crate::terminal_ui::Style::Default,
                    PluginUiStyle::Muted => crate::terminal_ui::Style::Muted,
                    PluginUiStyle::Accent => crate::terminal_ui::Style::Accent,
                    PluginUiStyle::Warning => crate::terminal_ui::Style::Accent,
                    PluginUiStyle::Error => crate::terminal_ui::Style::Error,
                    PluginUiStyle::Selected => crate::terminal_ui::Style::Selection,
                    PluginUiStyle::Border => crate::terminal_ui::Style::Border,
                },
                actionable: node.action.is_some(),
                focused: focused_node == Some(id),
            });
        }
        for child in &node.children {
            collect(*child, depth + 1, nodes, focused_node, rows);
        }
    }
    collect(tree.root, 0, &nodes, focused_node, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        PluginActivationBehavior, PluginCommandContext, PluginCommandDeclaration,
        PluginContentTypeContribution, PluginPresenterKind, PluginPrivateCellPolicy,
    };

    fn index(owner: &str, generation: u64) -> PluginContributionIndex {
        PluginContributionIndex {
            owner: owner.into(),
            version: "1.0.0".into(),
            checksum: format!("{generation:064x}"),
            generation,
            source: "manifest.toml:bytes 0..1".into(),
            commands: [(
                "open".into(),
                PluginCommandDeclaration {
                    title: "Open".into(),
                    description: String::new(),
                    arguments: BTreeMap::new(),
                    context: PluginCommandContext::Application,
                    activation: PluginActivationBehavior::ActiveOnly,
                },
            )]
            .into_iter()
            .collect(),
            contexts: Vec::new(),
            keymaps: Vec::new(),
            key_groups: Vec::new(),
            content_types: vec![PluginContentTypeContribution {
                id: format!("plugin.{owner}.panel"),
                title: "Panel".into(),
                presenter: PluginPresenterKind::RetainedDeclarative,
                default_placement: PluginWindowPlacementKind::Floating,
                allowed_placements: [
                    PluginWindowPlacementKind::Floating,
                    PluginWindowPlacementKind::Docked,
                    PluginWindowPlacementKind::Hidden,
                ]
                .into_iter()
                .collect(),
                focusable: true,
                maximum_instances: 1,
                context: "dashboard".into(),
                command: format!("plugin.{owner}.open"),
                private_cells: None,
                provenance: None,
            }],
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        }
    }

    fn private_index(owner: &str, generation: u64) -> PluginContributionIndex {
        let mut index = index(owner, generation);
        let content = &mut index.content_types[0];
        content.presenter = PluginPresenterKind::PrivateCells;
        content.private_cells = Some(PluginPrivateCellPolicy {
            maximum_cells: 128,
            maximum_patch_cells: 32,
            maximum_dirty_rectangles: 8,
            maximum_styles: 8,
            maximum_hit_regions: 8,
            maximum_bytes: 4_096,
            cursor: true,
        });
        index
    }

    fn tree(revision: u64, text: &str) -> PluginUiTree {
        PluginUiTree {
            revision,
            root: 1,
            nodes: vec![
                crate::plugins::PluginUiNode {
                    id: 1,
                    kind: PluginUiNodeKind::Container,
                    text: String::new(),
                    style: PluginUiStyle::Default,
                    action: None,
                    children: vec![2, 3],
                },
                crate::plugins::PluginUiNode {
                    id: 2,
                    kind: PluginUiNodeKind::Text,
                    text: text.into(),
                    style: PluginUiStyle::Accent,
                    action: None,
                    children: Vec::new(),
                },
                crate::plugins::PluginUiNode {
                    id: 3,
                    kind: PluginUiNodeKind::Button,
                    text: "Run".into(),
                    style: PluginUiStyle::Selected,
                    action: Some("plugin.demo.open".into()),
                    children: Vec::new(),
                },
            ],
        }
    }

    fn open_request(revision: u64, tree: PluginUiTree) -> PluginUiRequest {
        PluginUiRequest {
            token: 1,
            kind: PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Demo".into(),
                placement: PluginRequestedPlacement::Floating {
                    x: 5,
                    y: 3,
                    width: 30,
                    height: 8,
                },
                tree: Some(tree),
            },
        }
    }

    fn fixture_manifest(checksum: String) -> crate::plugins::PluginManifest {
        let mut indexed = index("demo", 1);
        let contribution = indexed.content_types.remove(0);
        let command = |title: &str| PluginCommandDeclaration {
            title: title.into(),
            description: String::new(),
            arguments: BTreeMap::new(),
            context: PluginCommandContext::Application,
            activation: PluginActivationBehavior::ActiveOnly,
        };
        crate::plugins::PluginManifest {
            id: "demo".into(),
            version: "1.0.0".into(),
            component_sha256: checksum,
            capabilities: [
                crate::plugins::PluginCapability::PrivateCells,
                crate::plugins::PluginCapability::EventSubscriptions,
                crate::plugins::PluginCapability::PluginStorage,
                crate::plugins::PluginCapability::PickerProviders,
                crate::plugins::PluginCapability::VirtualDocuments,
            ]
            .into_iter()
            .collect(),
            commands: [
                ("drive".into(), command("Drive")),
                ("trap".into(), command("Trap")),
                ("flood".into(), command("Flood")),
                ("spin".into(), command("Spin")),
                ("private".into(), command("Private")),
                ("events".into(), command("Events")),
                ("storage".into(), command("Storage")),
                ("provider".into(), command("Provider")),
                ("virtual".into(), command("Virtual document")),
            ]
            .into_iter()
            .collect(),
            contributions: crate::plugins::PluginContributions {
                schema_version: 1,
                input_contexts: Vec::new(),
                keymaps: Vec::new(),
                key_groups: Vec::new(),
                content_types: vec![
                    PluginContentTypeContribution {
                        command: "plugin.demo.drive".into(),
                        ..contribution.clone()
                    },
                    PluginContentTypeContribution {
                        id: "plugin.demo.cells".into(),
                        title: "Private fixture".into(),
                        presenter: PluginPresenterKind::PrivateCells,
                        command: "plugin.demo.private".into(),
                        private_cells: Some(PluginPrivateCellPolicy {
                            maximum_cells: 128,
                            maximum_patch_cells: 32,
                            maximum_dirty_rectangles: 8,
                            maximum_styles: 8,
                            maximum_hit_regions: 8,
                            maximum_bytes: 4_096,
                            cursor: true,
                        }),
                        ..contribution
                    },
                ],
                subscriptions: vec![crate::plugins::PluginSubscriptionContribution {
                    id: "plugin.demo.subscription.workspace".into(),
                    kind: crate::plugins::PluginSubscriptionKind::WorkspaceMetadataChanged,
                    maximum_queued: 8,
                    maximum_rate_per_second: 30,
                    coalesce: true,
                    provenance: None,
                }],
                providers: vec![crate::plugins::PluginProviderContribution {
                    id: "plugin.demo.provider.files".into(),
                    kind: crate::plugins::PluginProviderKind::PickerSource,
                    command: "plugin.demo.provider".into(),
                    maximum_concurrent_requests: 1,
                    maximum_items: 16,
                    maximum_bytes: 8_192,
                    deadline_milliseconds: 30_000,
                    provenance: None,
                }],
                virtual_documents: vec![crate::plugins::PluginVirtualDocumentContribution {
                    id: "plugin.demo.virtual.help".into(),
                    title: "Fixture help".into(),
                    language: "markdown".into(),
                    mode: crate::plugins::PluginVirtualDocumentMode::ReadOnly,
                    maximum_concurrent_documents: 1,
                    maximum_document_bytes: 4096,
                    maximum_document_lines: 128,
                    maximum_patch_bytes: 256,
                    maximum_patches_per_request: 8,
                    deadline_milliseconds: 30_000,
                    command: "plugin.demo.virtual".into(),
                    context: "dashboard".into(),
                    provenance: None,
                }],
                storage: Some(crate::plugins::PluginStorageContribution {
                    maximum_items: 32,
                    maximum_value_bytes: 1_024,
                    maximum_total_bytes: 16_384,
                }),
            },
        }
    }

    #[test]
    fn private_cells_are_local_revisioned_atomic_and_generation_owned() {
        let mut graph = WindowGraph::default();
        let tab = graph.create_workspace_tab("builtin.test", 1).expect("tab");
        let index = private_index("demo", 9);
        let mut host = PluginWindowHost::default();
        let revision = graph.revision().expect("revision");
        let opened = host.process(
            &mut graph,
            &index,
            PluginUiRequest {
                token: 1,
                kind: PluginUiRequestKind::OpenWindow {
                    expected_graph_revision: revision,
                    content_type: "plugin.demo.panel".into(),
                    title: "Cells".into(),
                    placement: PluginRequestedPlacement::Floating {
                        x: 2,
                        y: 1,
                        width: 12,
                        height: 4,
                    },
                    tree: None,
                },
            },
        );
        assert!(opened.error.is_none(), "{:?}", opened.error);
        let handle = opened.handle.expect("handle");
        let window = graph.focused().expect("focused private window");
        let projection = graph.compositor_snapshot(tab).expect("snapshot").project(
            WindowRect {
                x: 0,
                y: 0,
                width: 30,
                height: 10,
            },
            1,
            1,
        );
        let geometry = host.synchronize_private_geometry(Some(&projection));
        assert_eq!(geometry.len(), 1);
        let dimensions = geometry[0]
            .2
            .geometry
            .as_ref()
            .expect("local geometry")
            .clone();
        assert_eq!((dimensions.width, dimensions.height), (12, 4));
        assert_eq!(dimensions.handle, handle);

        let snapshot = PluginPrivateSnapshot {
            revision: 1,
            dimensions_revision: dimensions.dimensions_revision,
            styles: vec![PluginPrivateStyle {
                id: 1,
                style: PluginUiStyle::Accent,
            }],
            cells: vec![
                crate::plugins::PluginPrivateCell {
                    x: 0,
                    y: 0,
                    grapheme: "e\u{301}".into(),
                    continuation: false,
                    style: 1,
                },
                crate::plugins::PluginPrivateCell {
                    x: 1,
                    y: 0,
                    grapheme: "界".into(),
                    continuation: false,
                    style: 1,
                },
                crate::plugins::PluginPrivateCell {
                    x: 2,
                    y: 0,
                    grapheme: String::new(),
                    continuation: true,
                    style: 1,
                },
            ],
            cursor: Some(crate::plugins::PluginPrivateCursor { x: 0, y: 0 }),
            hit_regions: vec![crate::plugins::PluginPrivateHitRegion {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
                action: "plugin.demo.open".into(),
            }],
        };
        let replaced = host.process(
            &mut graph,
            &index,
            PluginUiRequest {
                token: 2,
                kind: PluginUiRequestKind::ReplacePrivate {
                    handle,
                    snapshot: snapshot.clone(),
                },
            },
        );
        assert!(replaced.error.is_none(), "{:?}", replaced.error);
        assert_eq!(replaced.surface_revision, Some(1));
        let rendered = host.private_snapshots(&graph);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].window, window);
        assert_eq!(rendered[0].cells[1].grapheme, "界");
        assert!(rendered[0].focused);

        let before = host.clone();
        let mut outside = snapshot.clone();
        outside.revision = 2;
        outside.cells[0].x = dimensions.width;
        let rejected = host.process(
            &mut graph,
            &index,
            PluginUiRequest {
                token: 21,
                kind: PluginUiRequestKind::ReplacePrivate {
                    handle,
                    snapshot: outside,
                },
            },
        );
        assert_eq!(
            rejected.error.map(|error| error.code),
            Some(PluginUiErrorCode::InvalidLocalBounds)
        );
        assert_eq!(host, before);

        let stale = host.process(
            &mut graph,
            &index,
            PluginUiRequest {
                token: 3,
                kind: PluginUiRequestKind::PatchPrivate {
                    handle,
                    patch: PluginPrivatePatch {
                        revision: 2,
                        expected_surface_revision: 0,
                        dimensions_revision: dimensions.dimensions_revision,
                        dirty: vec![PluginPrivateRect {
                            x: 0,
                            y: 0,
                            width: 1,
                            height: 1,
                        }],
                        styles: Vec::new(),
                        cells: Vec::new(),
                        replace_cursor: false,
                        cursor: None,
                        replace_hit_regions: false,
                        hit_regions: Vec::new(),
                    },
                },
            },
        );
        assert_eq!(
            stale.error.map(|error| error.code),
            Some(PluginUiErrorCode::StaleSurface)
        );
        assert_eq!(host, before);

        let split_wide = host.process(
            &mut graph,
            &index,
            PluginUiRequest {
                token: 4,
                kind: PluginUiRequestKind::PatchPrivate {
                    handle,
                    patch: PluginPrivatePatch {
                        revision: 2,
                        expected_surface_revision: 1,
                        dimensions_revision: dimensions.dimensions_revision,
                        dirty: vec![PluginPrivateRect {
                            x: 2,
                            y: 0,
                            width: 1,
                            height: 1,
                        }],
                        styles: Vec::new(),
                        cells: Vec::new(),
                        replace_cursor: false,
                        cursor: None,
                        replace_hit_regions: false,
                        hit_regions: Vec::new(),
                    },
                },
            },
        );
        assert_eq!(
            split_wide.error.map(|error| error.code),
            Some(PluginUiErrorCode::SplitWideGrapheme)
        );
        assert_eq!(host, before);

        let (plugin, load, action) = host
            .private_action_at(window, 1, 0, &{
                let mut catalog = crate::plugins::PluginContributionCatalog::default();
                catalog.install(index.clone()).expect("catalog");
                catalog
            })
            .expect("private hit action");
        assert_eq!((plugin.as_str(), load), ("demo", 9));
        assert_eq!(action.action.as_deref(), Some("plugin.demo.open"));

        let stable = host.clone();
        for _ in 0..256 {
            assert!(
                host.synchronize_private_geometry(Some(&projection))
                    .is_empty()
            );
        }
        assert_eq!(host, stable);
        let revision = graph.revision().expect("revision");
        let moved = host.process(
            &mut graph,
            &index,
            PluginUiRequest {
                token: 5,
                kind: PluginUiRequestKind::PlaceWindow {
                    handle,
                    expected_graph_revision: revision,
                    placement: PluginRequestedPlacement::Floating {
                        x: 1,
                        y: 1,
                        width: 8,
                        height: 3,
                    },
                },
            },
        );
        assert!(moved.error.is_none(), "{:?}", moved.error);
        let resized_projection = graph
            .compositor_snapshot(tab)
            .expect("resized snapshot")
            .project(
                WindowRect {
                    x: 0,
                    y: 0,
                    width: 30,
                    height: 10,
                },
                1,
                1,
            );
        assert_eq!(
            host.synchronize_private_geometry(Some(&resized_projection))
                .len(),
            1
        );
        assert!(
            host.private_snapshots(&graph).is_empty(),
            "resizing invalidates stale private cells"
        );
        assert!(
            host.remove_generation(&mut graph, "demo", 9)
                .expect("cleanup")
        );
        assert!(host.private_snapshots(&graph).is_empty());
    }

    #[test]
    fn real_component_drives_the_retained_window_protocol_without_wasi() {
        use sha2::{Digest, Sha256};
        let core = include_bytes!("../../tests/fixtures/retained-guest/retained-guest.core.wasm");
        assert_eq!(
            format!("{:x}", Sha256::digest(core)),
            "2b87dac4f0aa9998ec86d41ee26e1eb329d7cedf09d0a39efacaa88c61b601ec"
        );
        let component = wit_component::ComponentEncoder::default()
            .module(core)
            .expect("decode fixture metadata")
            .validate(true)
            .encode()
            .expect("encode deterministic component");
        let store = tempfile::tempdir().expect("store");
        let path = store.path().join("fixture.wasm");
        std::fs::write(&path, &component).expect("component fixture");
        let checksum = format!("{:x}", Sha256::digest(&component));
        let manifest = fixture_manifest(checksum);
        let mut outcome = crate::plugins::load_component(
            store.path(),
            &path,
            manifest.clone(),
            &crate::plugins::PluginLimits::default(),
        )
        .expect("production component load");
        assert_eq!(
            outcome.loaded.activated_commands,
            vec![
                "drive", "trap", "flood", "spin", "private", "events", "storage", "provider",
                "virtual"
            ]
        );

        let event_execution = crate::plugins::execute_component(
            "demo",
            "events",
            Vec::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
            vec![crate::plugins::PluginHostEventEnvelope {
                id: 7,
                sequence: 3,
                kind: crate::plugins::PluginSubscriptionKind::WorkspaceMetadataChanged,
                revision: 9,
                payload: "roots=2".into(),
                coalesced: 2,
                dropped: 1,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            44,
            &mut outcome.runtime,
        )
        .expect("host event execution");
        assert_eq!(event_execution.host_event_acknowledgements, [7]);
        assert!(
            event_execution
                .result
                .contains("events:1:coalesced=2:dropped=1")
        );

        let mut storage =
            crate::plugins::PluginStorageService::start(store.path().join("plugin-data"), 8);
        storage.install_generation("demo", crate::plugins::PluginLoadId(44));
        let policy = manifest
            .contributions
            .storage
            .clone()
            .expect("storage policy");
        let mut storage_results = Vec::new();
        for phase in 0..7 {
            let execution = crate::plugins::execute_component(
                "demo",
                "storage",
                Vec::new(),
                None,
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                storage_results,
                Vec::new(),
                Vec::new(),
                44,
                &mut outcome.runtime,
            )
            .expect("storage guest execution");
            if phase == 3 {
                assert!(execution.result.contains("StaleRevision"));
            }
            if phase == 6 {
                assert!(execution.result.contains("storage:done:missing=true"));
                assert!(execution.storage_requests.is_empty());
                break;
            }
            assert_eq!(execution.storage_requests.len(), 1);
            storage
                .request(
                    "demo".into(),
                    crate::plugins::PluginLoadId(44),
                    policy.clone(),
                    execution.storage_requests.into_iter().next().unwrap(),
                )
                .expect("storage worker request");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            storage_results = loop {
                let results = storage.poll();
                if !results.is_empty() {
                    break results;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "storage fixture timed out"
                );
                std::thread::yield_now();
            };
        }

        let mut graph = WindowGraph::default();
        graph.create_workspace_tab("builtin.test", 1).expect("tab");
        let mut host = PluginWindowHost::default();
        let static_index =
            crate::plugins::index_manifest_contributions(&manifest, 44, "fixture manifest".into())
                .expect("index");
        let mut providers = crate::plugins::PluginProviderBroker::default();
        providers.install(&static_index).expect("provider owner");
        let provider_request = providers
            .start(
                "demo",
                crate::plugins::PluginLoadId(44),
                "plugin.demo.provider.files",
                String::new(),
                "editor".into(),
                Some(3),
                Some(9),
            )
            .expect("provider request");
        let mut last_envelope = None;
        for phase in 0..3 {
            let requests = providers.requests("demo", crate::plugins::PluginLoadId(44));
            last_envelope = requests.first().cloned();
            let execution = crate::plugins::execute_component(
                "demo",
                "provider",
                Vec::new(),
                None,
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                requests,
                Vec::new(),
                44,
                &mut outcome.runtime,
            )
            .expect("provider guest execution");
            assert_eq!(execution.provider_chunks.len(), 1);
            let status = providers
                .apply_chunk(
                    "demo",
                    crate::plugins::PluginLoadId(44),
                    Some(3),
                    Some(9),
                    execution.provider_chunks.into_iter().next().unwrap(),
                )
                .expect("provider chunk");
            if phase < 2 {
                assert_eq!(status, crate::plugins::PluginProviderSessionStatus::Active);
            }
        }
        let projection = providers.projection(provider_request).expect("projection");
        assert_eq!(projection.items.len(), 2);
        assert_eq!(
            projection.status,
            crate::plugins::PluginProviderSessionStatus::Complete
        );
        assert!(providers.cancel(provider_request));
        let late = crate::plugins::execute_component(
            "demo",
            "provider",
            Vec::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![last_envelope.expect("delivered provider request")],
            Vec::new(),
            44,
            &mut outcome.runtime,
        )
        .expect("late provider guest execution");
        assert_eq!(
            providers
                .apply_chunk(
                    "demo",
                    crate::plugins::PluginLoadId(44),
                    Some(3),
                    Some(9),
                    late.provider_chunks.into_iter().next().unwrap(),
                )
                .unwrap_err()
                .code,
            crate::plugins::PluginProviderErrorCode::Cancelled
        );

        let mut virtuals = crate::plugins::PluginVirtualDocumentBroker::default();
        virtuals
            .install(&static_index)
            .expect("virtual source owner");
        let document = virtuals
            .start(
                "demo",
                crate::plugins::PluginLoadId(44),
                "plugin.demo.virtual.help",
                "help".into(),
            )
            .expect("virtual open");
        for phase in 0..2 {
            let requests = virtuals.requests_for_command(
                "demo",
                crate::plugins::PluginLoadId(44),
                "plugin.demo.virtual",
            );
            let execution = crate::plugins::execute_component(
                "demo",
                "virtual",
                Vec::new(),
                None,
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                requests,
                44,
                &mut outcome.runtime,
            )
            .expect("virtual guest execution");
            assert_eq!(execution.virtual_document_proposals.len(), 1);
            virtuals
                .apply(
                    "demo",
                    crate::plugins::PluginLoadId(44),
                    execution
                        .virtual_document_proposals
                        .into_iter()
                        .next()
                        .unwrap(),
                )
                .expect("virtual proposal");
            if phase == 0 {
                virtuals
                    .refresh("demo", crate::plugins::PluginLoadId(44), document)
                    .expect("virtual refresh");
            }
        }
        let virtual_snapshot = virtuals.snapshot(document).expect("virtual snapshot");
        assert_eq!(virtual_snapshot.revision, 2);
        assert!(virtual_snapshot.text.starts_with("## Héllo"));

        let mut trapped_outcome = crate::plugins::load_component(
            store.path(),
            &path,
            manifest.clone(),
            &crate::plugins::PluginLimits::default(),
        )
        .expect("trap runtime");
        let trapped = crate::plugins::execute_component(
            "demo",
            "trap",
            vec![graph.revision().expect("revision").to_string()],
            None,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![crate::plugins::PluginPickerRequestEnvelope {
                id: 99,
                provider: "plugin.demo.provider.files".into(),
                query: String::new(),
                context: "editor".into(),
                workspace_revision: Some(3),
                buffer_revision: Some(9),
                deadline_milliseconds: 1_000,
                cancellation_generation: 1,
                maximum_items: 16,
                maximum_bytes: 8_192,
            }],
            vec![crate::plugins::PluginVirtualDocumentRequest {
                request: 100,
                document: 101,
                source: "plugin.demo.virtual.help".into(),
                key: "trap".into(),
                kind: crate::plugins::PluginVirtualDocumentRequestKind::Open,
                current_revision: None,
                deadline_milliseconds: 1_000,
                cancellation_generation: 1,
                maximum_document_bytes: 4096,
                maximum_document_lines: 128,
                maximum_patch_bytes: 256,
                maximum_patches: 8,
            }],
            45,
            &mut trapped_outcome.runtime,
        );
        assert!(matches!(
            trapped,
            Err(crate::plugins::PluginError::Execution(_))
        ));
        assert!(
            host.snapshots().is_empty(),
            "trapped proposals never escape execution"
        );

        let mut deadline_outcome = crate::plugins::load_component(
            store.path(),
            &path,
            manifest.clone(),
            &crate::plugins::PluginLimits::default(),
        )
        .expect("deadline runtime");
        let interrupted = crate::plugins::execute_component(
            "demo",
            "spin",
            vec![graph.revision().expect("revision").to_string()],
            None,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![crate::plugins::PluginVirtualDocumentRequest {
                request: 102,
                document: 103,
                source: "plugin.demo.virtual.help".into(),
                key: "spin".into(),
                kind: crate::plugins::PluginVirtualDocumentRequestKind::Open,
                current_revision: None,
                deadline_milliseconds: 1_000,
                cancellation_generation: 1,
                maximum_document_bytes: 4096,
                maximum_document_lines: 128,
                maximum_patch_bytes: 256,
                maximum_patches: 8,
            }],
            46,
            &mut deadline_outcome.runtime,
        );
        assert!(matches!(
            interrupted,
            Err(crate::plugins::PluginError::Execution(_))
        ));
        assert!(
            host.snapshots().is_empty(),
            "interrupted proposals never escape execution"
        );

        let mut events = Vec::new();
        let mut opaque_handle = None;
        for phase in 0..5 {
            let execution = crate::plugins::execute_component(
                "demo",
                "drive",
                vec![graph.revision().expect("revision").to_string()],
                None,
                BTreeMap::new(),
                events,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                44,
                &mut outcome.runtime,
            )
            .expect("guest execution");
            assert_eq!(execution.ui_requests.len(), 1);
            let event = host.process(
                &mut graph,
                &static_index,
                execution.ui_requests.into_iter().next().expect("request"),
            );
            assert!(event.error.is_none(), "phase {phase}: {:?}", event.error);
            if phase == 0 {
                opaque_handle = event.handle;
                let snapshot = host.snapshots().pop().expect("opened snapshot");
                assert_eq!(snapshot.rows[0].text, "open");
                assert_eq!(snapshot.handle, opaque_handle.expect("opaque handle"));
            }
            if phase == 1 {
                assert_eq!(host.snapshots()[0].rows[0].text, "replaced");
            }
            events = vec![event];
        }
        assert!(host.snapshots().is_empty());
        assert_eq!(graph.counts(), (0, 0, 0, 1));

        let private_open = crate::plugins::execute_component(
            "demo",
            "private",
            vec![graph.revision().expect("revision").to_string()],
            None,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            44,
            &mut outcome.runtime,
        )
        .expect("private open execution");
        let open_event = host.process(
            &mut graph,
            &static_index,
            private_open
                .ui_requests
                .into_iter()
                .next()
                .expect("private open"),
        );
        assert!(open_event.error.is_none(), "{:?}", open_event.error);
        let private_handle = open_event.handle.expect("private handle");
        let tab = graph.active_tab().expect("active tab");
        let projection = graph
            .compositor_snapshot(tab)
            .expect("private compositor snapshot")
            .project(
                WindowRect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 24,
                },
                1,
                1,
            );
        let geometry_event = host
            .synchronize_private_geometry(Some(&projection))
            .pop()
            .expect("geometry event")
            .2;
        let private_replace = crate::plugins::execute_component(
            "demo",
            "private",
            vec![graph.revision().expect("revision").to_string()],
            None,
            BTreeMap::new(),
            vec![open_event, geometry_event],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            44,
            &mut outcome.runtime,
        )
        .expect("private replace execution");
        let replace_event = host.process(
            &mut graph,
            &static_index,
            private_replace
                .ui_requests
                .into_iter()
                .next()
                .expect("private replace"),
        );
        assert!(replace_event.error.is_none(), "{:?}", replace_event.error);
        assert_eq!(host.private_snapshots(&graph)[0].cells[0].grapheme, "A");

        let private_patch = crate::plugins::execute_component(
            "demo",
            "private",
            vec![graph.revision().expect("revision").to_string()],
            None,
            BTreeMap::new(),
            vec![replace_event],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            44,
            &mut outcome.runtime,
        )
        .expect("private patch execution");
        let patch_event = host.process(
            &mut graph,
            &static_index,
            private_patch
                .ui_requests
                .into_iter()
                .next()
                .expect("private patch"),
        );
        assert!(patch_event.error.is_none(), "{:?}", patch_event.error);
        assert_eq!(host.private_snapshots(&graph)[0].cells[0].grapheme, "B");
        let private_window = host.private_snapshots(&graph)[0].window;
        let (_, _, action_event) = host
            .private_action_at(private_window, 0, 0, &{
                let mut catalog = crate::plugins::PluginContributionCatalog::default();
                catalog.install(static_index.clone()).expect("catalog");
                catalog
            })
            .expect("private action");
        let private_close = crate::plugins::execute_component(
            "demo",
            "private",
            vec![graph.revision().expect("revision").to_string()],
            None,
            BTreeMap::new(),
            vec![patch_event, action_event],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            44,
            &mut outcome.runtime,
        )
        .expect("private close execution");
        assert!(private_close.result.contains("action=true"));
        let close_event = host.process(
            &mut graph,
            &static_index,
            private_close
                .ui_requests
                .into_iter()
                .next()
                .expect("private close"),
        );
        assert!(close_event.error.is_none(), "{:?}", close_event.error);
        assert_eq!(close_event.handle, Some(private_handle));
        assert!(host.private_snapshots(&graph).is_empty());

        let flood = crate::plugins::execute_component(
            "demo",
            "flood",
            vec![graph.revision().expect("revision").to_string()],
            None,
            BTreeMap::new(),
            events,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            44,
            &mut outcome.runtime,
        )
        .expect("bounded queue execution");
        assert_eq!(flood.ui_requests.len(), 64);
        assert!(flood.result.contains("QueueFull"));
        assert!(
            !host
                .remove_generation(&mut graph, "demo", 43)
                .expect("stale cleanup")
        );
        assert!(host.remove_generation(&mut graph, "demo", 44).is_ok());
        assert_eq!(opaque_handle, Some(1));
    }

    #[test]
    fn generation_owned_retained_window_lifecycle_is_revision_checked() {
        let mut graph = WindowGraph::default();
        graph.create_workspace_tab("builtin.test", 1).expect("tab");
        let mut host = PluginWindowHost::default();
        let demo_index = index("demo", 7);
        let open_revision = graph.revision().expect("revision");
        let event = host.process(
            &mut graph,
            &demo_index,
            open_request(open_revision, tree(1, "Hello")),
        );
        assert!(event.error.is_none(), "{:?}", event.error);
        let handle = event.handle.expect("handle");
        assert_eq!(host.snapshots()[0].rows[0].text, "Hello");
        let graph_after_open = graph.clone();
        let tab = graph.active_tab().expect("tab");
        let mut projection = graph.compositor_snapshot(tab).expect("snapshot").project(
            WindowRect {
                x: 0,
                y: 0,
                width: 20,
                height: 6,
            },
            1,
            1,
        );
        projection.set_protected_regions(vec![WindowRect {
            x: 6,
            y: 4,
            width: 3,
            height: 2,
        }]);
        assert_eq!(projection.hit_test(6, 4), None);

        let duplicate_revision = graph.revision().expect("revision");
        let duplicate = host.process(
            &mut graph,
            &demo_index,
            open_request(duplicate_revision, tree(1, "Duplicate")),
        );
        assert_eq!(
            duplicate.error.as_ref().map(|error| error.code),
            Some(PluginUiErrorCode::InstanceCeiling)
        );
        assert_eq!(graph, graph_after_open);

        let stale = host.process(
            &mut graph,
            &demo_index,
            PluginUiRequest {
                token: 2,
                kind: PluginUiRequestKind::PlaceWindow {
                    handle,
                    expected_graph_revision: 0,
                    placement: PluginRequestedPlacement::Hidden,
                },
            },
        );
        assert_eq!(
            stale.error.as_ref().map(|error| error.code),
            Some(PluginUiErrorCode::StaleGraph)
        );
        assert_eq!(graph, graph_after_open);

        let replaced = host.process(
            &mut graph,
            &demo_index,
            PluginUiRequest {
                token: 3,
                kind: PluginUiRequestKind::ReplaceUi {
                    handle,
                    expected_tree_revision: 1,
                    tree: tree(2, "Updated"),
                },
            },
        );
        assert!(replaced.error.is_none(), "{:?}", replaced.error);
        assert_eq!(host.snapshots()[0].rows[0].text, "Updated");
        let mut catalog = crate::plugins::PluginContributionCatalog::default();
        catalog.install(demo_index.clone()).expect("catalog");
        let snapshots = host.snapshots();
        let focused = snapshots[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .expect("host-owned focused node");
        assert_eq!(focused.text, "Run");
        assert_eq!(
            host.interact_focused(&graph, &catalog, PluginUiInteraction::Activate,)
                .expect("activate"),
            Some("plugin.demo.open".into())
        );
        host.interact_focused(&graph, &catalog, PluginUiInteraction::Scroll(1))
            .expect("bounded scroll");
        assert_eq!(host.snapshots()[0].scroll, 1);
        host.focus_node_at_row(host.snapshots()[0].window, 0)
            .expect("mouse focus");
        let repaired = host.process(
            &mut graph,
            &demo_index,
            PluginUiRequest {
                token: 31,
                kind: PluginUiRequestKind::ReplaceUi {
                    handle,
                    expected_tree_revision: 2,
                    tree: PluginUiTree {
                        revision: 3,
                        root: 1,
                        nodes: vec![crate::plugins::PluginUiNode {
                            id: 1,
                            kind: PluginUiNodeKind::Text,
                            text: "No selectable node".into(),
                            style: PluginUiStyle::Default,
                            action: None,
                            children: Vec::new(),
                        }],
                    },
                },
            },
        );
        assert!(repaired.error.is_none(), "{:?}", repaired.error);
        assert!(!host.snapshots()[0].rows.iter().any(|row| row.focused));
        assert_eq!(host.snapshots()[0].scroll, 0);

        let revision = graph.revision().expect("revision");
        assert!(
            host.process(
                &mut graph,
                &demo_index,
                PluginUiRequest {
                    token: 4,
                    kind: PluginUiRequestKind::PlaceWindow {
                        handle,
                        expected_graph_revision: revision,
                        placement: PluginRequestedPlacement::Hidden,
                    },
                },
            )
            .error
            .is_none()
        );
        let hidden_revision = graph.revision().expect("hidden revision");
        let focus_hidden = host.process(
            &mut graph,
            &demo_index,
            PluginUiRequest {
                token: 5,
                kind: PluginUiRequestKind::FocusWindow {
                    handle,
                    expected_graph_revision: hidden_revision,
                },
            },
        );
        assert_eq!(
            focus_hidden.error.as_ref().map(|error| error.code),
            Some(PluginUiErrorCode::HiddenFocus)
        );

        let foreign = index("other", 8);
        let foreign_event = host.process(
            &mut graph,
            &foreign,
            PluginUiRequest {
                token: 6,
                kind: PluginUiRequestKind::ReplaceUi {
                    handle,
                    expected_tree_revision: 2,
                    tree: PluginUiTree {
                        revision: 3,
                        root: 1,
                        nodes: vec![crate::plugins::PluginUiNode {
                            id: 1,
                            kind: PluginUiNodeKind::Text,
                            text: "foreign".into(),
                            style: PluginUiStyle::Default,
                            action: None,
                            children: Vec::new(),
                        }],
                    },
                },
            },
        );
        assert_eq!(
            foreign_event.error.as_ref().map(|error| error.code),
            Some(PluginUiErrorCode::ForeignGeneration)
        );

        assert!(
            host.remove_generation(&mut graph, "demo", 7)
                .expect("cleanup")
        );
        assert!(host.snapshots().is_empty());
        assert_eq!(graph.counts(), (0, 0, 0, 1));

        let replacement = index("demo", 8);
        let replacement_revision = graph.revision().expect("replacement revision");
        let replacement_event = host.process(
            &mut graph,
            &replacement,
            open_request(replacement_revision, tree(1, "Replacement")),
        );
        assert!(
            replacement_event.error.is_none(),
            "{:?}",
            replacement_event.error
        );
        assert!(
            !host
                .remove_generation(&mut graph, "demo", 7)
                .expect("stale cleanup")
        );
        assert_eq!(host.snapshots()[0].rows[0].text, "Replacement");
    }

    #[test]
    fn plugin_window_operations_reject_cross_tab_scope_without_mutation() {
        let mut graph = WindowGraph::default();
        graph.create_workspace_tab("builtin.test", 1).expect("tab");
        let mut host = PluginWindowHost::default();
        let demo = index("demo", 3);
        let revision = graph.revision().expect("revision");
        let opened = host.process(&mut graph, &demo, open_request(revision, tree(1, "one")));
        let handle = opened.handle.expect("handle");
        let second = graph
            .create_workspace_tab("builtin.test", 1)
            .expect("second tab");
        graph.set_active_tab(second).expect("activate second");
        let before = graph.clone();
        let event = host.process(
            &mut graph,
            &demo,
            PluginUiRequest {
                token: 9,
                kind: PluginUiRequestKind::PlaceWindow {
                    handle,
                    expected_graph_revision: before.revision().expect("revision"),
                    placement: PluginRequestedPlacement::Hidden,
                },
            },
        );
        assert_eq!(
            event.error.as_ref().map(|error| error.code),
            Some(PluginUiErrorCode::CrossTab)
        );
        assert_eq!(graph, before);
    }

    #[test]
    fn retained_ui_errors_are_stable_typed_results() {
        let demo = index("demo", 1);
        let mut graph = WindowGraph::default();
        graph.create_workspace_tab("builtin.test", 1).expect("tab");
        let mut host = PluginWindowHost::default();
        let revision = graph.revision().expect("revision");
        let undeclared = host.process(
            &mut graph,
            &demo,
            PluginUiRequest {
                token: 1,
                kind: PluginUiRequestKind::OpenWindow {
                    expected_graph_revision: revision,
                    content_type: "plugin.demo.missing".into(),
                    title: String::new(),
                    placement: PluginRequestedPlacement::Floating {
                        x: 1,
                        y: 1,
                        width: 4,
                        height: 2,
                    },
                    tree: Some(tree(1, "missing")),
                },
            },
        );
        assert_eq!(
            undeclared.error.map(|error| error.code),
            Some(PluginUiErrorCode::UndeclaredContent)
        );
        let revision = graph.revision().expect("revision");
        let invalid = host.process(
            &mut graph,
            &demo,
            PluginUiRequest {
                token: 2,
                kind: PluginUiRequestKind::OpenWindow {
                    expected_graph_revision: revision,
                    content_type: "plugin.demo.panel".into(),
                    title: String::new(),
                    placement: PluginRequestedPlacement::Floating {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                    },
                    tree: Some(tree(1, "invalid")),
                },
            },
        );
        assert_eq!(
            invalid.error.map(|error| error.code),
            Some(PluginUiErrorCode::InvalidPlacement)
        );
        let revision = graph.revision().expect("revision");
        let stale = host.process(
            &mut graph,
            &demo,
            PluginUiRequest {
                token: 3,
                kind: PluginUiRequestKind::CloseWindow {
                    handle: 999,
                    expected_graph_revision: revision,
                },
            },
        );
        assert_eq!(
            stale.error.map(|error| error.code),
            Some(PluginUiErrorCode::StaleHandle)
        );
    }

    #[test]
    fn retained_focus_order_follows_visual_children_not_manifest_vector_order() {
        let mut shuffled = tree(1, "first");
        shuffled.nodes.swap(0, 2);
        assert_eq!(shuffled.nodes[0].id, 3, "fixture declaration is shuffled");
        assert_eq!(focusable_nodes(&shuffled), vec![3]);
        shuffled.nodes.push(crate::plugins::PluginUiNode {
            id: 4,
            kind: PluginUiNodeKind::List,
            text: "After".into(),
            style: PluginUiStyle::Default,
            action: None,
            children: Vec::new(),
        });
        shuffled
            .nodes
            .iter_mut()
            .find(|node| node.id == 1)
            .expect("root")
            .children
            .push(4);
        shuffled.nodes.swap(0, 3);
        assert_eq!(focusable_nodes(&shuffled), vec![3, 4]);
    }

    #[test]
    fn retained_tree_rejects_cycles_unreachable_nodes_and_undeclared_actions() {
        let index = index("demo", 1);
        let mut cyclic = tree(1, "cycle");
        cyclic.nodes[1].children.push(1);
        assert!(validate_tree(&index, &cyclic, None).is_err());
        let mut unreachable = tree(1, "unreachable");
        unreachable.nodes.push(crate::plugins::PluginUiNode {
            id: 99,
            kind: PluginUiNodeKind::Text,
            text: "orphan".into(),
            style: PluginUiStyle::Default,
            action: None,
            children: Vec::new(),
        });
        assert!(validate_tree(&index, &unreachable, None).is_err());
        let mut undeclared = tree(1, "action");
        undeclared.nodes[2].action = Some("plugin.demo.missing".into());
        assert!(validate_tree(&index, &undeclared, None).is_err());
    }
}
