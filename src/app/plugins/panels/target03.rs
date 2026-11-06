//! Test-only adapter from the normative target ABI into the existing window owners.
use crate::app::plugins::panels::*;
use crate::plugins::target03::StagedUiRequest;
use crate::plugins::target03::TargetAdmission;
use crate::plugins::target03::api::dust::plugin::host as wire;

fn failure(code: wire::ErrorCode, message: impl std::fmt::Display) -> wire::Failure {
    wire::Failure {
        code,
        message: message.to_string(),
        retryable: false,
    }
}

fn map_error(error: PluginUiError) -> wire::Failure {
    let code = match error.code {
        PluginUiErrorCode::StaleGraph
        | PluginUiErrorCode::StaleTree
        | PluginUiErrorCode::StaleSurface
        | PluginUiErrorCode::StaleDimensions => wire::ErrorCode::StaleSource,
        PluginUiErrorCode::StaleHandle | PluginUiErrorCode::GenerationLost => {
            wire::ErrorCode::Closed
        }
        PluginUiErrorCode::ForeignGeneration => wire::ErrorCode::ForeignHandle,
        PluginUiErrorCode::QueueFull => wire::ErrorCode::QueueFull,
        PluginUiErrorCode::InstanceCeiling
        | PluginUiErrorCode::GraphCeiling
        | PluginUiErrorCode::PatchCeiling => wire::ErrorCode::LimitExceeded,
        PluginUiErrorCode::DisallowedPlacement
        | PluginUiErrorCode::UndeclaredAction
        | PluginUiErrorCode::UndeclaredContent => wire::ErrorCode::PermissionDenied,
        PluginUiErrorCode::InternalRejected => wire::ErrorCode::ProtocolFailure,
        _ => wire::ErrorCode::InvalidArgument,
    };
    failure(code, error.detail)
}

fn tree(tree: wire::Tree) -> Result<PluginUiTree, wire::Failure> {
    let nodes = tree
        .nodes
        .into_iter()
        .map(|node| {
            let kind = match node.kind {
                wire::NodeKind::Container => PluginUiNodeKind::Container,
                wire::NodeKind::Text => PluginUiNodeKind::Text,
                wire::NodeKind::ListNode => PluginUiNodeKind::List,
                wire::NodeKind::Button if node.enabled => PluginUiNodeKind::Button,
                _ => {
                    return Err(failure(
                        wire::ErrorCode::Unsupported,
                        "target node kind is not implemented in this lane",
                    ));
                }
            };
            let style = match node.role {
                wire::Role::Normal => PluginUiStyle::Default,
                wire::Role::Muted => PluginUiStyle::Muted,
                wire::Role::Accent | wire::Role::Success => PluginUiStyle::Accent,
                wire::Role::Selected => PluginUiStyle::Selected,
                wire::Role::Warning => PluginUiStyle::Warning,
                wire::Role::Error => PluginUiStyle::Error,
                wire::Role::Border => PluginUiStyle::Border,
            };
            Ok(crate::plugins::PluginUiNode {
                id: node.id,
                kind,
                text: node.text,
                style,
                action: node.action,
                children: node.children,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PluginUiTree {
        revision: tree.revision,
        root: tree.root,
        nodes,
    })
}

#[test]
fn disabled_target03_buttons_are_rejected_before_conversion() {
    let button = |enabled| wire::Tree {
        revision: 1,
        root: 1,
        nodes: vec![wire::Node {
            id: 1,
            kind: wire::NodeKind::Button,
            label: String::new(),
            text: "Run".into(),
            value: String::new(),
            value_revision: 0,
            role: wire::Role::Normal,
            enabled,
            required: false,
            checked: false,
            action: Some("plugin.demo.open".into()),
            items: Vec::new(),
            children: Vec::new(),
        }],
    };
    let error = tree(button(false)).unwrap_err();
    assert!(matches!(error.code, wire::ErrorCode::Unsupported));
    let converted = tree(button(true)).unwrap();
    assert_eq!(converted.nodes[0].kind, PluginUiNodeKind::Button);
    assert_eq!(
        converted.nodes[0].action.as_deref(),
        Some("plugin.demo.open")
    );
}

enum TargetPlacement {
    Ordinary(PluginRequestedPlacement),
    Tiled(wire::Tile),
}

fn placement(placement: wire::Placement) -> Result<TargetPlacement, wire::Failure> {
    match placement {
        wire::Placement::Docked(dock) if dock.size > 0 => {
            Ok(TargetPlacement::Ordinary(match dock.edge {
                wire::Edge::Left => PluginRequestedPlacement::DockedLeft { size: dock.size },
                wire::Edge::Right => PluginRequestedPlacement::DockedRight { size: dock.size },
                wire::Edge::Top => PluginRequestedPlacement::DockedTop { size: dock.size },
                wire::Edge::Bottom => PluginRequestedPlacement::DockedBottom { size: dock.size },
            }))
        }
        wire::Placement::Floating(rect) if rect.width > 0 && rect.height > 0 => Ok(
            TargetPlacement::Ordinary(PluginRequestedPlacement::Floating {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            }),
        ),
        wire::Placement::Fullscreen => Ok(TargetPlacement::Ordinary(
            PluginRequestedPlacement::Fullscreen,
        )),
        wire::Placement::Hidden => Ok(TargetPlacement::Ordinary(PluginRequestedPlacement::Hidden)),
        wire::Placement::Tiled(tile) if (1..=999).contains(&tile.ratio) => {
            Ok(TargetPlacement::Tiled(tile))
        }
        _ => Err(failure(
            wire::ErrorCode::InvalidArgument,
            "target placement dimensions are invalid",
        )),
    }
}

fn placement_admitted(
    admission: &TargetAdmission,
    declaration: &crate::plugins::PluginContentTypeContribution,
    placement: &TargetPlacement,
) -> bool {
    match placement {
        TargetPlacement::Ordinary(placement) => declaration
            .allowed_placements
            .contains(&placement_kind(*placement)),
        TargetPlacement::Tiled(_) => admission.can_tile(&declaration.id),
    }
}

fn ordinary_placement(placement: &TargetPlacement) -> Result<WindowPlacement, wire::Failure> {
    match placement {
        TargetPlacement::Ordinary(placement) => graph_placement(*placement).map_err(map_error),
        TargetPlacement::Tiled(_) => Ok(WindowPlacement::Tiled),
    }
}

fn tile_parameters(
    placement: &TargetPlacement,
) -> Option<(Option<WindowId>, crate::windows::TileAxis, bool, u16)> {
    let TargetPlacement::Tiled(tile) = placement else {
        return None;
    };
    Some((
        tile.neighbor.map(WindowId),
        match tile.axis {
            wire::Axis::Horizontal => crate::windows::TileAxis::Horizontal,
            wire::Axis::Vertical => crate::windows::TileAxis::Vertical,
        },
        tile.after,
        tile.ratio,
    ))
}

fn style(role: wire::Role) -> PluginUiStyle {
    match role {
        wire::Role::Normal => PluginUiStyle::Default,
        wire::Role::Muted => PluginUiStyle::Muted,
        wire::Role::Accent | wire::Role::Success => PluginUiStyle::Accent,
        wire::Role::Selected => PluginUiStyle::Selected,
        wire::Role::Warning => PluginUiStyle::Warning,
        wire::Role::Error => PluginUiStyle::Error,
        wire::Role::Border => PluginUiStyle::Border,
    }
}

impl PluginWindowHost {
    pub(in crate::app) fn process_target03(
        &mut self,
        graph: &mut WindowGraph,
        catalogue: &crate::plugins::PluginContributionCatalog,
        admission: &TargetAdmission,
        staged: StagedUiRequest,
    ) -> Result<wire::UiReceipt, wire::Failure> {
        if !staged.current(admission, catalogue) {
            return Err(failure(
                wire::ErrorCode::StaleSource,
                "target load or catalogue source is retired",
            ));
        }
        match staged.request {
            wire::UiRequest::Open(open) => {
                self.open_target03(graph, admission, staged.invocation, open)
            }
            wire::UiRequest::Cells(update) => self.cells_target03(graph, admission, update),
            wire::UiRequest::Place(request) => {
                self.place_target03(graph, admission, staged.invocation, request)
            }
            wire::UiRequest::Focus(request) => {
                self.focus_target03(graph, admission, staged.invocation, request)
            }
            wire::UiRequest::Close(request) => {
                self.close_target03(graph, admission, staged.invocation, request)
            }
            _ => Err(failure(
                wire::ErrorCode::Unsupported,
                "target UI request is not implemented in this lane",
            )),
        }
    }

    fn open_target03(
        &mut self,
        graph: &mut WindowGraph,
        admission: &TargetAdmission,
        invocation: wire::Invocation,
        open: wire::OpenView,
    ) -> Result<wire::UiReceipt, wire::Failure> {
        let ui = invocation.ui.ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target invocation has no UI source",
            )
        })?;
        if open.content.is_some()
            && (graph.window(WindowId(ui.window)).is_none()
                || graph.view(ViewId(ui.view)).is_none())
        {
            return Err(failure(
                wire::ErrorCode::Closed,
                "target clone source is closed",
            ));
        }
        if open.graph_revision != ui.graph_revision
            || graph.revision() != Some(open.graph_revision)
            || open.tab != ui.tab
            || graph.active_tab().map(|tab| tab.0) != Some(open.tab)
        {
            return Err(failure(
                wire::ErrorCode::StaleSource,
                "target graph or tab source is stale",
            ));
        }
        let index = &admission.index;
        let declaration = index
            .content_types
            .iter()
            .find(|content| content.id == open.content_type)
            .ok_or_else(|| {
                failure(
                    wire::ErrorCode::PermissionDenied,
                    "target content type is undeclared",
                )
            })?;
        let requested_placement = placement(open.placement)?;
        if !placement_admitted(admission, declaration, &requested_placement) {
            return Err(failure(
                wire::ErrorCode::PermissionDenied,
                "target placement is not declared",
            ));
        }
        let retained = match (declaration.presenter, open.tree) {
            (PluginPresenterKind::RetainedDeclarative, Some(value)) => {
                let value = tree(value)?;
                validate_tree(index, &value, None).map_err(map_error)?;
                Some(value)
            }
            (PluginPresenterKind::PrivateCells, None) if declaration.private_cells.is_some() => {
                None
            }
            _ => {
                return Err(failure(
                    wire::ErrorCode::InvalidArgument,
                    "target presenter payload is invalid",
                ));
            }
        };
        let count = self
            .instances
            .values()
            .filter(|instance| {
                instance.plugin == index.owner
                    && instance.load == index.generation
                    && instance.content_type == declaration.id
            })
            .count();
        if count >= usize::from(declaration.maximum_instances) {
            return Err(failure(
                wire::ErrorCode::LimitExceeded,
                "target content instance ceiling reached",
            ));
        }

        let mut windows =
            crate::app::windows::WindowSession::from_test_graph(graph.clone());
        let mut draft = windows.begin_plugin_fixture(&index.owner, index.generation);
        let mut candidate = self.clone();
        let owner = format!("plugin.{}", index.owner);
        let content =
            if let Some(content) = open.content {
                if !admission.can_clone(&declaration.id) || content != ui.content {
                    return Err(failure(
                        wire::ErrorCode::PermissionDenied,
                        "target clone-view is denied or has the wrong content",
                    ));
                }
                let source_window = draft.read().window(WindowId(ui.window)).ok_or_else(|| {
                    failure(wire::ErrorCode::Closed, "target clone window is closed")
                })?;
                let source_view = draft.read().view(ViewId(ui.view)).ok_or_else(|| {
                    failure(wire::ErrorCode::Closed, "target clone view is closed")
                })?;
                let source = candidate.instances.values().find(|instance| {
                    instance.plugin == index.owner
                        && instance.load == index.generation
                        && instance.checksum == index.checksum
                        && instance.content_type == declaration.id
                        && instance.content.0 == content
                        && instance.view.0 == ui.view
                        && instance.window.0 == ui.window
                });
                if source_window.tab.0 != ui.tab
                    || source_window.view.0 != ui.view
                    || source_view.content.0 != content
                    || source.is_none()
                {
                    return Err(failure(
                        wire::ErrorCode::ForeignHandle,
                        "target clone source is stale or foreign",
                    ));
                }
                draft
                    .read()
                    .require_clone_view(ContentId(content))
                    .map_err(|error| failure(wire::ErrorCode::PermissionDenied, error))?;
                ContentId(content)
            } else {
                let capabilities = ContentCapabilities {
                    focusable: declaration.focusable,
                    selectable: true,
                    editable: false,
                    clone_view: admission.can_clone(&declaration.id),
                    tile: admission.can_tile(&declaration.id),
                    dock: declaration
                        .allowed_placements
                        .contains(&PluginWindowPlacementKind::Docked),
                    float: declaration
                        .allowed_placements
                        .contains(&PluginWindowPlacementKind::Floating),
                    fullscreen: declaration
                        .allowed_placements
                        .contains(&PluginWindowPlacementKind::Fullscreen),
                    hide: declaration
                        .allowed_placements
                        .contains(&PluginWindowPlacementKind::Hidden),
                    close_view: true,
                    stop_resource: false,
                    persist_descriptor: false,
                };
                let adapter = match declaration.presenter {
                    PluginPresenterKind::RetainedDeclarative => ContentAdapterKind::DeclarativeUi,
                    PluginPresenterKind::PrivateCells => ContentAdapterKind::PrivateCellSurface,
                };
                draft
                    .create_content_with_contract(
                        ContentTypeId::new(open.content_type)
                            .map_err(|error| failure(wire::ErrorCode::InvalidArgument, error))?,
                        ContentContract::declared(adapter, capabilities),
                        open.title,
                        false,
                        &owner,
                        index.generation,
                    )
                    .map_err(|error| failure(wire::ErrorCode::InvalidArgument, error))?
            };
        let view = draft
            .create_view(content, &owner, index.generation)
            .map_err(|error| failure(wire::ErrorCode::LimitExceeded, error))?;
        let window =
            if let Some((neighbor, axis, after, ratio)) = tile_parameters(&requested_placement) {
                draft.open_tiled_window(
                    crate::windows::WorkspaceTabId(open.tab),
                    view,
                    declaration.focusable,
                    &owner,
                    index.generation,
                    (neighbor, axis, after, ratio),
                )
            } else {
                draft.open_window(
                    crate::windows::WorkspaceTabId(open.tab),
                    view,
                    ordinary_placement(&requested_placement)?,
                    declaration.focusable,
                    &owner,
                    index.generation,
                )
            }
            .map_err(|error| map_error(map_graph_error(error)))?;
        draft
            .validate()
            .map_err(|error| failure(wire::ErrorCode::ProtocolFailure, error))?;
        let handle = candidate.allocate_handle().map_err(map_error)?;
        let view_revision = retained.as_ref().map_or(0, |tree| tree.revision);
        candidate.instances.insert(
            handle,
            PluginWindowInstance {
                plugin: index.owner.clone(),
                load: index.generation,
                checksum: index.checksum.clone(),
                content_type: declaration.id.clone(),
                content,
                view,
                window,
                presenter: declaration.presenter,
                focused_node: retained.as_ref().and_then(first_focusable_node),
                tree: retained,
                private: (declaration.presenter == PluginPresenterKind::PrivateCells)
                    .then(PluginPrivateState::default),
                scroll: 0,
                visible_rows: 0,
            },
        );
        windows
            .commit_plugin_fixture(draft)
            .map_err(|error| failure(wire::ErrorCode::StaleSource, error))?;
        *self = candidate;
        *graph = windows.graph().clone();
        Ok(wire::UiReceipt {
            content: content.0,
            view: view.0,
            window: window.0,
            graph_revision: graph
                .revision()
                .ok_or_else(|| failure(wire::ErrorCode::Exhausted, "graph revision exhausted"))?,
            view_revision,
        })
    }

    fn target03_instance(
        &self,
        graph: &WindowGraph,
        admission: &TargetAdmission,
        ui: &wire::UiContext,
    ) -> Result<(u64, PluginWindowInstance), wire::Failure> {
        let index = &admission.index;
        let (handle, instance) = self
            .instances
            .iter()
            .find(|(_, instance)| {
                instance.plugin == index.owner
                    && instance.load == index.generation
                    && instance.checksum == index.checksum
                    && instance.content.0 == ui.content
                    && instance.view.0 == ui.view
                    && instance.window.0 == ui.window
            })
            .ok_or_else(|| failure(wire::ErrorCode::ForeignHandle, "target window is foreign"))?;
        let window = graph
            .window(instance.window)
            .ok_or_else(|| failure(wire::ErrorCode::Closed, "target window is closed"))?;
        let view = graph
            .view(instance.view)
            .ok_or_else(|| failure(wire::ErrorCode::Closed, "target view is closed"))?;
        if window.view != instance.view
            || window.tab.0 != ui.tab
            || view.content != instance.content
            || graph.revision() != Some(ui.graph_revision)
        {
            return Err(failure(
                wire::ErrorCode::StaleSource,
                "target window source is stale",
            ));
        }
        Ok((*handle, instance.clone()))
    }

    fn place_target03(
        &mut self,
        graph: &mut WindowGraph,
        admission: &TargetAdmission,
        invocation: wire::Invocation,
        request: wire::Place,
    ) -> Result<wire::UiReceipt, wire::Failure> {
        let ui = invocation.ui.ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target placement has no UI source",
            )
        })?;
        if request.window != ui.window
            || request.graph_revision != ui.graph_revision
            || request.tab != ui.tab
            || graph.active_tab().map(|tab| tab.0) != Some(request.tab)
        {
            return Err(failure(
                wire::ErrorCode::StaleSource,
                "target placement request does not match its exact source",
            ));
        }
        let (_, instance) = self.target03_instance(graph, admission, &ui)?;
        let requested = placement(request.placement)?;
        let declaration = admission
            .index
            .content_types
            .iter()
            .find(|content| content.id == instance.content_type)
            .ok_or_else(|| failure(wire::ErrorCode::Closed, "target declaration is retired"))?;
        if !placement_admitted(admission, declaration, &requested) {
            return Err(failure(
                wire::ErrorCode::PermissionDenied,
                "target placement is not declared",
            ));
        }
        let mut windows =
            crate::app::windows::WindowSession::from_test_graph(graph.clone());
        let mut draft =
            windows.begin_plugin_fixture(&admission.index.owner, admission.index.generation);
        if let Some((neighbor, axis, after, ratio)) = tile_parameters(&requested) {
            draft
                .tile(instance.window, (neighbor, axis, after, ratio))
                .map_err(|error| map_error(map_graph_error(error)))?;
        } else {
            draft
                .place(instance.window, ordinary_placement(&requested)?)
                .map_err(|error| map_error(map_graph_error(error)))?;
        }
        draft
            .validate()
            .map_err(|error| failure(wire::ErrorCode::ProtocolFailure, error))?;
        windows
            .commit_plugin_fixture(draft)
            .map_err(|error| failure(wire::ErrorCode::StaleSource, error))?;
        *graph = windows.graph().clone();
        Ok(wire::UiReceipt {
            content: instance.content.0,
            view: instance.view.0,
            window: instance.window.0,
            graph_revision: graph
                .revision()
                .ok_or_else(|| failure(wire::ErrorCode::Exhausted, "graph revision exhausted"))?,
            view_revision: instance.tree.as_ref().map_or(0, |tree| tree.revision),
        })
    }

    fn focus_target03(
        &mut self,
        graph: &mut WindowGraph,
        admission: &TargetAdmission,
        invocation: wire::Invocation,
        request: wire::Focus,
    ) -> Result<wire::UiReceipt, wire::Failure> {
        let ui = invocation.ui.ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target focus has no UI source",
            )
        })?;
        if request.window != ui.window || request.graph_revision != ui.graph_revision {
            return Err(failure(
                wire::ErrorCode::StaleSource,
                "target focus request does not match its exact source",
            ));
        }
        let (_, instance) = self.target03_instance(graph, admission, &ui)?;
        let mut windows =
            crate::app::windows::WindowSession::from_test_graph(graph.clone());
        let mut draft =
            windows.begin_plugin_fixture(&admission.index.owner, admission.index.generation);
        draft
            .focus(instance.window)
            .map_err(|error| map_error(map_graph_error(error)))?;
        windows
            .commit_plugin_fixture(draft)
            .map_err(|error| failure(wire::ErrorCode::StaleSource, error))?;
        *graph = windows.graph().clone();
        Ok(wire::UiReceipt {
            content: instance.content.0,
            view: instance.view.0,
            window: instance.window.0,
            graph_revision: graph
                .revision()
                .ok_or_else(|| failure(wire::ErrorCode::Exhausted, "graph revision exhausted"))?,
            view_revision: instance.tree.as_ref().map_or(0, |tree| tree.revision),
        })
    }

    fn close_target03(
        &mut self,
        graph: &mut WindowGraph,
        admission: &TargetAdmission,
        invocation: wire::Invocation,
        request: wire::CloseView,
    ) -> Result<wire::UiReceipt, wire::Failure> {
        let ui = invocation.ui.ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target close has no UI source",
            )
        })?;
        if request.view != ui.view || request.graph_revision != ui.graph_revision {
            return Err(failure(
                wire::ErrorCode::StaleSource,
                "target close request does not match its exact source",
            ));
        }
        let (handle, instance) = self.target03_instance(graph, admission, &ui)?;
        let mut windows =
            crate::app::windows::WindowSession::from_test_graph(graph.clone());
        let mut draft =
            windows.begin_plugin_fixture(&admission.index.owner, admission.index.generation);
        draft
            .close_window(instance.window)
            .and_then(|_| draft.remove_view(instance.view))
            .map_err(|error| map_error(map_graph_error(error)))?;
        if draft
            .read()
            .content_snapshots()
            .into_iter()
            .find(|content| content.id == instance.content)
            .is_some_and(|content| content.view_count == 0)
        {
            draft
                .remove_content(instance.content)
                .map_err(|error| map_error(map_graph_error(error)))?;
        }
        draft
            .validate()
            .map_err(|error| failure(wire::ErrorCode::ProtocolFailure, error))?;
        windows
            .commit_plugin_fixture(draft)
            .map_err(|error| failure(wire::ErrorCode::StaleSource, error))?;
        self.instances.remove(&handle);
        *graph = windows.graph().clone();
        Ok(wire::UiReceipt {
            content: instance.content.0,
            view: instance.view.0,
            window: instance.window.0,
            graph_revision: graph
                .revision()
                .ok_or_else(|| failure(wire::ErrorCode::Exhausted, "graph revision exhausted"))?,
            view_revision: instance.tree.as_ref().map_or(0, |tree| tree.revision),
        })
    }

    fn cells_target03(
        &mut self,
        graph: &WindowGraph,
        admission: &TargetAdmission,
        update: wire::CellUpdate,
    ) -> Result<wire::UiReceipt, wire::Failure> {
        let index = &admission.index;
        let (handle, instance) = self
            .instances
            .iter()
            .find(|(_, instance)| {
                instance.plugin == index.owner
                    && instance.load == index.generation
                    && instance.checksum == index.checksum
                    && instance.view.0 == update.view
            })
            .ok_or_else(|| failure(wire::ErrorCode::ForeignHandle, "target view is foreign"))?;
        let private = instance.private.as_ref().ok_or_else(|| {
            failure(
                wire::ErrorCode::InvalidArgument,
                "target view is not a private presenter",
            )
        })?;
        let current = private
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision);
        if current != update.expected
            || update.expected.checked_add(1) != Some(update.revision)
            || private.dimensions_revision != update.dimensions_revision
        {
            return Err(failure(
                wire::ErrorCode::Conflict,
                "target surface or dimensions revision is stale",
            ));
        }
        if !update.full {
            return Err(failure(
                wire::ErrorCode::Unsupported,
                "target cell patches are not implemented in this lane",
            ));
        }
        let mut styles = Vec::<PluginPrivateStyle>::new();
        let mut cells = Vec::with_capacity(update.cells.len());
        for cell in update.cells {
            let mapped = style(cell.role);
            let id = if let Some(style) = styles.iter().find(|style| style.style == mapped) {
                style.id
            } else {
                let id = u16::try_from(styles.len() + 1)
                    .map_err(|_| failure(wire::ErrorCode::LimitExceeded, "too many cell styles"))?;
                styles.push(PluginPrivateStyle { id, style: mapped });
                id
            };
            cells.push(crate::plugins::PluginPrivateCell {
                x: cell.x,
                y: cell.y,
                grapheme: cell.grapheme,
                continuation: cell.continuation,
                style: id,
            });
        }
        let snapshot = PluginPrivateSnapshot {
            revision: update.revision,
            dimensions_revision: update.dimensions_revision,
            styles,
            cells,
            cursor: update
                .cursor
                .map(|(x, y)| crate::plugins::PluginPrivateCursor { x, y }),
            hit_regions: update
                .hits
                .into_iter()
                .map(|hit| crate::plugins::PluginPrivateHitRegion {
                    x: hit.bounds.x,
                    y: hit.bounds.y,
                    width: hit.bounds.width,
                    height: hit.bounds.height,
                    action: hit.action,
                })
                .collect(),
        };
        let content = instance.content;
        let view = instance.view;
        let window = instance.window;
        let mut candidate = self.clone();
        let event = candidate.process_retained(
            graph,
            index,
            PluginUiRequest {
                token: 1,
                kind: PluginUiRequestKind::ReplacePrivate {
                    handle: *handle,
                    snapshot,
                },
            },
        );
        if let Some(error) = event.error {
            return Err(map_error(error));
        }
        *self = candidate;
        Ok(wire::UiReceipt {
            content: content.0,
            view: view.0,
            window: window.0,
            graph_revision: graph
                .revision()
                .ok_or_else(|| failure(wire::ErrorCode::Exhausted, "graph revision exhausted"))?,
            view_revision: update.revision,
        })
    }

    pub(in crate::app) fn target03_dimensions(
        &mut self,
        projection: &crate::windows::CompositorProjection,
        catalogue: &crate::plugins::PluginContributionCatalog,
        admission: &TargetAdmission,
        first_event: u64,
    ) -> Result<Vec<wire::Event>, wire::Failure> {
        if !catalogue.get(&admission.index.owner).is_some_and(|index| {
            index.generation == admission.index.generation
                && index.checksum == admission.index.checksum
        }) {
            return Err(failure(
                wire::ErrorCode::StaleSource,
                "target catalogue generation is retired",
            ));
        }
        let mut candidate = self.clone();
        let mut events = Vec::new();
        for instance in candidate.instances.values_mut().filter(|instance| {
            instance.plugin == admission.index.owner
                && instance.load == admission.index.generation
                && instance.checksum == admission.index.checksum
        }) {
            let Some(private) = instance.private.as_mut() else {
                continue;
            };
            let dimensions = projection
                .window(instance.window)
                .map(|window| (window.rect.width, window.rect.height))
                .filter(|(width, height)| *width > 0 && *height > 0);
            if private.dimensions == dimensions {
                continue;
            }
            let revision = private.dimensions_revision.checked_add(1).ok_or_else(|| {
                failure(wire::ErrorCode::Exhausted, "dimensions revision exhausted")
            })?;
            private.dimensions = dimensions;
            private.dimensions_revision = revision;
            private.snapshot = None;
            let Some((width, height)) = dimensions else {
                continue;
            };
            let id = first_event
                .checked_add(u64::try_from(events.len()).unwrap_or(u64::MAX))
                .ok_or_else(|| failure(wire::ErrorCode::Exhausted, "event ID exhausted"))?;
            events.push(wire::Event {
                id,
                context: wire::Invocation {
                    id,
                    kind: wire::ExecutionKind::Lifecycle,
                    command: "dimensions".into(),
                    arguments: Vec::new(),
                    source: None,
                    ui: None,
                },
                value: wire::EventValue::Dimensions(wire::Dimensions {
                    view: instance.view.0,
                    revision,
                    width,
                    height,
                }),
            });
        }
        *self = candidate;
        Ok(events)
    }
}

use crate::plugins::PluginPrivateStyle;
use crate::plugins::PluginUiError;
use crate::plugins::PluginUiErrorCode;
use crate::plugins::PluginUiNodeKind;
use crate::plugins::PluginUiRequest;
use crate::plugins::PluginUiRequestKind;
use crate::plugins::PluginUiStyle;
use crate::plugins::PluginWindowPlacementKind;
use crate::windows::ContentAdapterKind;
use crate::windows::ContentCapabilities;
use crate::windows::ContentContract;
use crate::windows::ContentTypeId;
use crate::windows::WindowGraph;
use crate::windows::WindowPlacement;
