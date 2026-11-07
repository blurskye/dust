//! Plugin panel windows, content and actions.

use crate::app::plugins::panels::PluginPrivateState;
use crate::app::plugins::panels::PluginUiInteraction;
use crate::app::plugins::panels::PluginWindowHost;
use crate::app::plugins::panels::PluginWindowInstance;
use crate::app::plugins::panels::apply_private_patch;
use crate::app::plugins::panels::ensure_focused_visible;
use crate::app::plugins::panels::first_focusable_node;
use crate::app::plugins::panels::focusable_nodes;
use crate::app::plugins::panels::graph_placement;
use crate::app::plugins::panels::instance_revisions;
use crate::app::plugins::panels::is_focusable_node;
use crate::app::plugins::panels::map_graph_error;
use crate::app::plugins::panels::placement_kind;
use crate::app::plugins::panels::private_declaration;
use crate::app::plugins::panels::private_style;
use crate::app::plugins::panels::rect_contains;
use crate::app::plugins::panels::require_active_window;
use crate::app::plugins::panels::require_graph_revision;
use crate::app::plugins::panels::tree_rows;
use crate::app::plugins::panels::ui_error;
use crate::app::plugins::panels::validate_private_action;
use crate::app::plugins::panels::validate_private_snapshot;
use crate::app::plugins::panels::validate_tree;
use crate::plugins::PluginContributionIndex;
use crate::plugins::PluginPresenterKind;
use crate::plugins::PluginRequestedPlacement;
use crate::plugins::PluginSurfaceGeometry;
use crate::plugins::PluginUiError;
use crate::plugins::PluginUiErrorCode;
use crate::plugins::PluginUiEvent;
use crate::plugins::PluginUiRequest;
use crate::plugins::PluginUiRequestKind;
use crate::plugins::PluginWindowPlacementKind;
use crate::ui::PluginSurfaceUiState;
use crate::ui::PrivateSurfaceCell;
use crate::ui::PrivateSurfaceUiState;
use crate::windows::ContentAdapterKind;
use crate::windows::ContentCapabilities;
use crate::windows::ContentContract;
use crate::windows::ContentTypeId;
use crate::windows::WindowGraph;
use crate::windows::WindowId;
use crate::windows::WindowPlacement;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

type WindowMutationRevisions = (Option<u64>, Option<u64>, Option<u64>);

impl PluginWindowHost {
    pub(in crate::app) fn set_ambiguous_width(&mut self, wide: bool) {
        if self.ambiguous_wide == wide {
            return;
        }
        self.ambiguous_wide = wide;
        // Cell coordinates belong to the width policy too. Request a fresh snapshot.
        for instance in self.instances.values_mut() {
            if let Some(private) = &mut instance.private {
                private.dimensions = None;
                private.snapshot = None;
            }
        }
    }
    pub(in crate::app) fn set_configured_rules(
        &mut self,
        windows: &crate::config::WindowsConfig,
    ) -> Result<(), String> {
        let mut placements = BTreeMap::new();
        for rule in &windows.rules {
            if !rule.content_type.starts_with("plugin.") {
                continue;
            }
            let placement = match &rule.placement {
                crate::config::WindowPlacementConfig::Automatic => continue,
                crate::config::WindowPlacementConfig::Docked { edge, size } => {
                    match edge.as_str() {
                        "left" => PluginRequestedPlacement::DockedLeft { size: *size },
                        "right" => PluginRequestedPlacement::DockedRight { size: *size },
                        "top" => PluginRequestedPlacement::DockedTop { size: *size },
                        "bottom" => PluginRequestedPlacement::DockedBottom { size: *size },
                        _ => return Err("invalid configured plugin dock edge".into()),
                    }
                }
                crate::config::WindowPlacementConfig::Floating {
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
                crate::config::WindowPlacementConfig::Fullscreen => {
                    PluginRequestedPlacement::Fullscreen
                }
                crate::config::WindowPlacementConfig::Hidden => PluginRequestedPlacement::Hidden,
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

    pub(in crate::app) fn rejected(
        token: u64,
        graph_revision: Option<u64>,
        code: PluginUiErrorCode,
        detail: String,
    ) -> PluginUiEvent {
        mutation_event(token, graph_revision, Err(ui_error(code, detail)))
    }
    /// Validation runs against immutable live state, before graph/host staging.
    /// Structural graph validation still runs on the accepted mutation candidate.
    pub(in crate::app) fn validate_window_request(
        &self,
        graph: &WindowGraph,
        index: &PluginContributionIndex,
        request: &PluginUiRequest,
    ) -> Option<PluginUiEvent> {
        match self.check_window_request(graph, index, &request.kind) {
            Err(error) => Some(mutation_event(request.token, graph.revision(), Err(error))),
            Ok(false) => {
                let handle = match &request.kind {
                    PluginUiRequestKind::PlaceWindow { handle, .. }
                    | PluginUiRequestKind::FocusWindow { handle, .. } => *handle,
                    _ => unreachable!("only placement/focus can be unchanged"),
                };
                Some(mutation_event(
                    request.token,
                    graph.revision(),
                    self.owned_instance(index, handle)
                        .map(|instance| instance_revisions(handle, instance)),
                ))
            }
            Ok(true) => None,
        }
    }
    fn check_window_request(
        &self,
        graph: &WindowGraph,
        index: &PluginContributionIndex,
        request: &PluginUiRequestKind,
    ) -> Result<bool, PluginUiError> {
        match request {
            PluginUiRequestKind::OpenWindow {
                expected_graph_revision,
                content_type,
                title,
                placement,
                tree,
            } => {
                require_graph_revision(graph, *expected_graph_revision)?;
                let declaration = index
                    .content_types
                    .iter()
                    .find(|d| &d.id == content_type)
                    .ok_or_else(|| {
                        ui_error(
                            PluginUiErrorCode::UndeclaredContent,
                            "content type is not owned by this plugin load",
                        )
                    })?;
                let placement = self
                    .configured_placements
                    .get(content_type)
                    .copied()
                    .unwrap_or(*placement);
                if !declaration
                    .allowed_placements
                    .contains(&placement_kind(placement))
                {
                    return Err(ui_error(
                        PluginUiErrorCode::DisallowedPlacement,
                        "window placement is not allowed by the static declaration",
                    ));
                }
                if self
                    .instances
                    .values()
                    .filter(|i| {
                        i.plugin == index.owner
                            && i.load == index.plugin_load
                            && i.content_type == *content_type
                    })
                    .count()
                    >= usize::from(declaration.maximum_instances)
                {
                    return Err(ui_error(
                        PluginUiErrorCode::InstanceLimit,
                        "plugin content instance limit reached",
                    ));
                }
                match declaration.presenter {
                    PluginPresenterKind::RetainedDeclarative => validate_tree(
                        index,
                        tree.as_ref().ok_or_else(|| {
                            ui_error(
                                PluginUiErrorCode::InvalidTree,
                                "retained presenter requires an initial tree",
                            )
                        })?,
                        None,
                    )?,
                    PluginPresenterKind::PrivateCells => {
                        if tree.is_some() || declaration.private_cells.is_none() {
                            return Err(ui_error(
                                PluginUiErrorCode::WrongPresenter,
                                "private presenter does not accept a retained tree",
                            ));
                        }
                    }
                }
                crate::windows::validate_title(if title.is_empty() {
                    &declaration.title
                } else {
                    title
                })
                .map_err(map_graph_error)?;
                graph_placement(placement)?;
                if graph.active_tab().is_none() {
                    return Err(ui_error(
                        PluginUiErrorCode::CrossTab,
                        "no active workspace tab",
                    ));
                }
                self.next_handle.max(1).checked_add(1).ok_or_else(|| {
                    ui_error(
                        PluginUiErrorCode::InternalRejected,
                        "plugin window handle space exhausted",
                    )
                })?;
                Ok(true)
            }
            PluginUiRequestKind::PlaceWindow {
                handle,
                expected_graph_revision,
                placement,
            } => {
                require_graph_revision(graph, *expected_graph_revision)?;
                let instance = self.owned_instance(index, *handle)?;
                require_active_window(graph, instance.window)?;
                let declaration = index
                    .content_types
                    .iter()
                    .find(|d| d.id == instance.content_type)
                    .ok_or_else(|| {
                        ui_error(
                            PluginUiErrorCode::LoadUnavailable,
                            "content declaration is stale",
                        )
                    })?;
                if !declaration
                    .allowed_placements
                    .contains(&placement_kind(*placement))
                {
                    return Err(ui_error(
                        PluginUiErrorCode::DisallowedPlacement,
                        "window placement is not allowed by the static declaration",
                    ));
                }
                let placement = graph_placement(*placement)?;
                Ok(graph
                    .window(instance.window)
                    .is_none_or(|window| window.placement != placement))
            }
            PluginUiRequestKind::FocusWindow {
                handle,
                expected_graph_revision,
            } => {
                require_graph_revision(graph, *expected_graph_revision)?;
                let instance = self.owned_instance(index, *handle)?;
                require_active_window(graph, instance.window)?;
                if graph
                    .window(instance.window)
                    .is_some_and(|window| window.placement == WindowPlacement::Hidden)
                {
                    return Err(ui_error(
                        PluginUiErrorCode::HiddenFocus,
                        "hidden plugin window cannot receive focus",
                    ));
                }
                Ok(graph.focused() != Some(instance.window))
            }
            PluginUiRequestKind::CloseWindow {
                handle,
                expected_graph_revision,
            } => {
                require_graph_revision(graph, *expected_graph_revision)?;
                let instance = self.owned_instance(index, *handle)?;
                require_active_window(graph, instance.window)?;
                Ok(true)
            }
            _ => Err(ui_error(
                PluginUiErrorCode::InternalRejected,
                "retained update used window validation",
            )),
        }
    }

    pub(in crate::app) fn process(
        &mut self,
        graph: &mut crate::app::windows::PluginWindowDraft,
        index: &PluginContributionIndex,
        request: PluginUiRequest,
    ) -> PluginUiEvent {
        let token = request.token;
        let result = self.process_checked(graph, index, request.kind);
        mutation_event(token, graph.revision(), result)
    }

    fn process_checked(
        &mut self,
        graph: &mut crate::app::windows::PluginWindowDraft,
        index: &PluginContributionIndex,
        request: PluginUiRequestKind,
    ) -> Result<WindowMutationRevisions, PluginUiError> {
        match request {
            PluginUiRequestKind::OpenWindow {
                expected_graph_revision,
                content_type,
                title,
                placement,
                tree,
            } => {
                require_graph_revision(graph.read(), expected_graph_revision)?;
                let declaration = index
                    .content_types
                    .iter()
                    .find(|declaration| declaration.id == content_type)
                    .ok_or_else(|| {
                        ui_error(
                            PluginUiErrorCode::UndeclaredContent,
                            "content type is not owned by this plugin load",
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
                            && instance.load == index.plugin_load
                            && instance.content_type == content_type
                    })
                    .count();
                if count >= usize::from(declaration.maximum_instances) {
                    return Err(ui_error(
                        PluginUiErrorCode::InstanceLimit,
                        "plugin content instance limit reached",
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
                let capabilities = ContentCapabilities {
                    focusable: declaration.focusable,
                    selectable: true,
                    editable: false,
                    clone_view: false,
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
                let placement = graph_placement(placement)?;
                let candidate = &mut *graph;
                let owner = format!("plugin.{}", index.owner);
                let content = candidate
                    .create_content_with_contract(
                        ContentTypeId::new(content_type.clone()).map_err(map_graph_error)?,
                        ContentContract::declared(adapter, capabilities),
                        if title.is_empty() {
                            declaration.title.clone()
                        } else {
                            title
                        },
                        false,
                        &owner,
                        index.plugin_load,
                    )
                    .map_err(map_graph_error)?;
                let view = candidate
                    .create_view(content, &owner, index.plugin_load)
                    .map_err(map_graph_error)?;
                let window = candidate
                    .open_window(
                        tab,
                        view,
                        placement,
                        declaration.focusable,
                        &owner,
                        index.plugin_load,
                    )
                    .map_err(map_graph_error)?;
                candidate.validate().map_err(map_graph_error)?;
                let handle = self.allocate_handle()?;
                let tree_revision = tree.as_ref().map(|tree| tree.revision);
                self.instances.insert(
                    handle,
                    PluginWindowInstance {
                        plugin: index.owner.clone(),
                        load: index.plugin_load,
                        checksum: index.checksum.clone(),
                        content_type,
                        content,
                        view,
                        window,
                        presenter: declaration.presenter,
                        focused_node,
                        tree,
                        private,
                        scroll: 0,
                        visible_rows: 0,
                    },
                );
                Ok((Some(handle), tree_revision, None))
            }
            PluginUiRequestKind::PlaceWindow {
                handle,
                expected_graph_revision,
                placement,
            } => {
                require_graph_revision(graph.read(), expected_graph_revision)?;
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph.read(), instance.window)?;
                let declaration = index
                    .content_types
                    .iter()
                    .find(|declaration| declaration.id == instance.content_type)
                    .ok_or_else(|| {
                        ui_error(
                            PluginUiErrorCode::LoadUnavailable,
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
                let candidate = &mut *graph;
                candidate
                    .place(instance.window, graph_placement(placement)?)
                    .map_err(map_graph_error)?;
                candidate.validate().map_err(map_graph_error)?;
                Ok(instance_revisions(handle, instance))
            }
            PluginUiRequestKind::FocusWindow {
                handle,
                expected_graph_revision,
            } => {
                require_graph_revision(graph.read(), expected_graph_revision)?;
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph.read(), instance.window)?;
                if graph
                    .window(instance.window)
                    .is_some_and(|window| matches!(window.placement, WindowPlacement::Hidden))
                {
                    return Err(ui_error(
                        PluginUiErrorCode::HiddenFocus,
                        "hidden plugin window cannot receive focus",
                    ));
                }
                let candidate = &mut *graph;
                candidate.focus(instance.window).map_err(map_graph_error)?;
                candidate.validate().map_err(map_graph_error)?;
                Ok(instance_revisions(handle, instance))
            }
            PluginUiRequestKind::CloseWindow {
                handle,
                expected_graph_revision,
            } => {
                require_graph_revision(graph.read(), expected_graph_revision)?;
                let instance = self.owned_instance(index, handle)?;
                require_active_window(graph.read(), instance.window)?;
                let revisions = instance_revisions(handle, instance);
                let candidate = &mut *graph;
                candidate
                    .close_window(instance.window)
                    .and_then(|_| candidate.remove_view(instance.view))
                    .map_err(map_graph_error)?;
                if candidate
                    .read()
                    .content_snapshots()
                    .into_iter()
                    .find(|content| content.id == instance.content)
                    .is_some_and(|content| content.view_count == 0)
                {
                    candidate
                        .remove_content(instance.content)
                        .map_err(map_graph_error)?;
                }
                candidate.validate().map_err(map_graph_error)?;
                self.instances.remove(&handle);
                Ok(revisions)
            }
            request => self.process_retained_checked(graph.read(), index, request),
        }
    }

    pub(in crate::app) fn is_retained_update(request: &PluginUiRequestKind) -> bool {
        matches!(
            request,
            PluginUiRequestKind::ReplaceUi { .. }
                | PluginUiRequestKind::ReplacePrivate { .. }
                | PluginUiRequestKind::PatchPrivate { .. }
        )
    }
    pub(in crate::app) fn process_retained(
        &mut self,
        graph: &WindowGraph,
        index: &PluginContributionIndex,
        request: PluginUiRequest,
    ) -> PluginUiEvent {
        let token = request.token;
        let result = self.process_retained_checked(graph, index, request.kind);
        mutation_event(token, graph.revision(), result)
    }
    fn process_retained_checked(
        &mut self,
        graph: &WindowGraph,
        index: &PluginContributionIndex,
        request: PluginUiRequestKind,
    ) -> Result<WindowMutationRevisions, PluginUiError> {
        match request {
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
                let revision = tree.revision;
                instance.tree = Some(tree);
                ensure_focused_visible(instance);
                Ok((Some(handle), Some(revision), None))
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
                    self.ambiguous_wide,
                )?;
                let revision = snapshot.revision;
                self.instances
                    .get_mut(&handle)
                    .expect("validated private handle")
                    .private
                    .as_mut()
                    .expect("private presenter")
                    .snapshot = Some(snapshot);
                Ok((Some(handle), None, Some(revision)))
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
                let candidate =
                    apply_private_patch(declaration, private, &patch, self.ambiguous_wide)?;
                let revision = candidate.revision;
                self.instances
                    .get_mut(&handle)
                    .expect("validated private handle")
                    .private
                    .as_mut()
                    .expect("private presenter")
                    .snapshot = Some(candidate);
                Ok((Some(handle), None, Some(revision)))
            }
            _ => Err(ui_error(
                PluginUiErrorCode::InternalRejected,
                "not a retained UI update",
            )),
        }
    }
    pub(in crate::app) fn allocate_handle(&mut self) -> Result<u64, PluginUiError> {
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
            || instance.load != index.plugin_load
            || instance.checksum != index.checksum
        {
            return Err(ui_error(
                PluginUiErrorCode::ForeignLoad,
                "plugin window handle belongs to another plugin load",
            ));
        }
        Ok(instance)
    }

    pub(in crate::app) fn loads(&self) -> Vec<(String, u64)> {
        self.instances
            .values()
            .map(|instance| (instance.plugin.clone(), instance.load))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub(in crate::app) fn clear_runtime(&mut self) {
        self.instances.clear();
    }
    pub(in crate::app) fn remove_load(&mut self, plugin: &str, load: u64) -> bool {
        let before = self.instances.len();
        self.instances
            .retain(|_, instance| !(instance.plugin == plugin && instance.load == load));
        self.instances.len() != before
    }

    pub(in crate::app) fn input_context(
        &self,
        window: WindowId,
        catalog: &crate::plugins::PluginContributionCatalog,
    ) -> Option<String> {
        let instance = self
            .instances
            .values()
            .find(|instance| instance.window == window)?;
        let index = catalog.get(&instance.plugin).filter(|index| {
            index.plugin_load == instance.load && index.checksum == instance.checksum
        })?;
        index
            .content_types
            .iter()
            .find(|content| content.id == instance.content_type)
            .map(|content| content.context.clone())
    }

    pub(in crate::app) fn snapshots(&self) -> Vec<PluginSurfaceUiState> {
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

    pub(in crate::app) fn private_snapshots(
        &self,
        graph: &WindowGraph,
    ) -> Vec<PrivateSurfaceUiState> {
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
                                    .unwrap_or(crate::ui::Style::Default),
                            })
                            .collect(),
                        cursor: snapshot.cursor.map(|cursor| (cursor.x, cursor.y)),
                        focused: graph.focused() == Some(instance.window),
                    }
                })
            })
            .collect()
    }

    pub(in crate::app) fn interact_focused(
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
                index.plugin_load == instance.load && index.checksum == instance.checksum
            })
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::LoadUnavailable,
                    "plugin contribution load is unavailable",
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

    pub(in crate::app) fn focus_node_at_row(
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

    pub(in crate::app) fn synchronize_private_geometry(
        &mut self,
        projection: Option<&crate::windows::CompositorProjection>,
    ) -> Vec<(String, u64, PluginUiEvent)> {
        let mut events = Vec::new();
        for (handle, instance) in &mut self.instances {
            let dimensions = projection
                .and_then(|projection| projection.window(instance.window))
                .map(|window| (window.rect.width, window.rect.height))
                .filter(|(width, height)| *width > 0 && *height > 0);
            let visible_rows = dimensions.map_or(0, |(_, height)| usize::from(height));
            if instance.visible_rows != visible_rows {
                instance.visible_rows = visible_rows;
                if visible_rows > 0 {
                    ensure_focused_visible(instance);
                }
            }
            let Some(private) = instance.private.as_mut() else {
                continue;
            };
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

    pub(in crate::app) fn private_action_at(
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
                index.plugin_load == instance.load && index.checksum == instance.checksum
            })
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::LoadUnavailable,
                    "private surface load is unavailable",
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

fn mutation_event(
    token: u64,
    graph_revision: Option<u64>,
    result: Result<WindowMutationRevisions, PluginUiError>,
) -> PluginUiEvent {
    match result {
        Ok((handle, tree_revision, surface_revision)) => PluginUiEvent {
            token,
            handle,
            graph_revision,
            tree_revision,
            surface_revision,
            geometry: None,
            action: None,
            error: None,
        },
        Err(error) => PluginUiEvent {
            token,
            handle: None,
            graph_revision,
            tree_revision: None,
            surface_revision: None,
            geometry: None,
            action: None,
            error: Some(error),
        },
    }
}
