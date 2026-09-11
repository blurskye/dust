//! Test-only adapter from the normative target ABI into the existing window owners.
use super::*;
use crate::extensions::target03::{
    StagedUiRequest, TargetAdmission, api::dust::plugin::host as wire,
};

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
                wire::NodeKind::Button => PluginUiNodeKind::Button,
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
            Ok(crate::extensions::PluginUiNode {
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

fn placement(placement: wire::Placement) -> Result<PluginRequestedPlacement, wire::Failure> {
    match placement {
        wire::Placement::Floating(rect) if rect.width > 0 && rect.height > 0 => {
            Ok(PluginRequestedPlacement::Floating {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
        }
        _ => Err(failure(
            wire::ErrorCode::Unsupported,
            "target placement is not implemented in this lane",
        )),
    }
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
    pub(in crate::editor::session) fn process_target03(
        &mut self,
        graph: &mut WindowGraph,
        admission: &TargetAdmission,
        staged: StagedUiRequest,
    ) -> Result<wire::UiReceipt, wire::Failure> {
        match staged.request {
            wire::UiRequest::Open(open) => {
                self.open_target03(graph, admission, staged.invocation, open)
            }
            wire::UiRequest::Cells(update) => self.cells_target03(graph, admission, update),
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
        if !declaration
            .allowed_placements
            .contains(&placement_kind(requested_placement))
        {
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
            crate::editor::session::windows::WindowSession::from_test_graph(graph.clone());
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
                    tile: false,
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
        let window = draft
            .open_window(
                crate::ui::window::WorkspaceTabId(open.tab),
                view,
                graph_placement(requested_placement).map_err(map_error)?,
                declaration.focusable,
                &owner,
                index.generation,
            )
            .map_err(|error| failure(wire::ErrorCode::LimitExceeded, error))?;
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
            cells.push(crate::extensions::PluginPrivateCell {
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
                .map(|(x, y)| crate::extensions::PluginPrivateCursor { x, y }),
            hit_regions: update
                .hits
                .into_iter()
                .map(|hit| crate::extensions::PluginPrivateHitRegion {
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

    pub(in crate::editor::session) fn target03_dimensions(
        &mut self,
        projection: &crate::ui::window::CompositorProjection,
        admission: &TargetAdmission,
        first_event: u64,
    ) -> Result<Vec<wire::Event>, wire::Failure> {
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
            let (width, height) = dimensions.ok_or_else(|| {
                failure(
                    wire::ErrorCode::Closed,
                    "private view has no writable geometry",
                )
            })?;
            private.dimensions = Some((width, height));
            private.dimensions_revision = revision;
            private.snapshot = None;
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
