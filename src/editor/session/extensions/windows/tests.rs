use super::*;
use crate::{
    extensions::{
        PluginActivationBehavior, PluginCommandContext, PluginCommandDeclaration,
        PluginContentTypeContribution, PluginPresenterKind, PluginPrivateCellPolicy,
    },
    ui::window::ContentLifecycle,
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
            crate::extensions::PluginUiNode {
                id: 1,
                kind: PluginUiNodeKind::Container,
                text: String::new(),
                style: PluginUiStyle::Default,
                action: None,
                children: vec![2, 3],
            },
            crate::extensions::PluginUiNode {
                id: 2,
                kind: PluginUiNodeKind::Text,
                text: text.into(),
                style: PluginUiStyle::Accent,
                action: None,
                children: Vec::new(),
            },
            crate::extensions::PluginUiNode {
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
    open_request_for("demo", revision, tree)
}

fn open_request_for(owner: &str, revision: u64, mut tree: PluginUiTree) -> PluginUiRequest {
    for node in &mut tree.nodes {
        if node.action.as_deref() == Some("plugin.demo.open") {
            node.action = Some(format!("plugin.{owner}.open"));
        }
    }
    PluginUiRequest {
        token: 1,
        kind: PluginUiRequestKind::OpenWindow {
            expected_graph_revision: revision,
            content_type: format!("plugin.{owner}.panel"),
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

pub(in crate::editor::session) fn fixture_manifest(
    checksum: String,
) -> crate::extensions::PluginManifest {
    let mut indexed = index("demo", 1);
    let contribution = indexed.content_types.remove(0);
    let command = |title: &str| PluginCommandDeclaration {
        title: title.into(),
        description: String::new(),
        arguments: BTreeMap::new(),
        context: PluginCommandContext::Application,
        activation: PluginActivationBehavior::ActiveOnly,
    };
    crate::extensions::PluginManifest {
        id: "demo".into(),
        version: "1.0.0".into(),
        component_sha256: checksum,
        capabilities: [
            crate::extensions::PluginCapability::ReadCurrentBuffer,
            crate::extensions::PluginCapability::ProposeEdits,
            crate::extensions::PluginCapability::ShowMessage,
            crate::extensions::PluginCapability::PrivateCells,
            crate::extensions::PluginCapability::EventSubscriptions,
            crate::extensions::PluginCapability::PluginStorage,
            crate::extensions::PluginCapability::PickerProviders,
            crate::extensions::PluginCapability::VirtualDocuments,
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
            ("confirmation".into(), command("Confirmation example")),
            (
                "confirmationprobe".into(),
                command("Confirmation capacity probe"),
            ),
        ]
        .into_iter()
        .collect(),
        contributions: crate::extensions::PluginContributions {
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
                    id: "plugin.demo.confirmation-panel".into(),
                    title: "Confirmation fixture".into(),
                    command: "plugin.demo.confirmation".into(),
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
            subscriptions: vec![crate::extensions::PluginSubscriptionContribution {
                id: "plugin.demo.subscription.workspace".into(),
                kind: crate::extensions::PluginSubscriptionKind::WorkspaceMetadataChanged,
                maximum_queued: 8,
                maximum_rate_per_second: 30,
                coalesce: true,
                provenance: None,
            }],
            providers: vec![crate::extensions::PluginProviderContribution {
                id: "plugin.demo.provider.files".into(),
                kind: crate::extensions::PluginProviderKind::PickerSource,
                command: "plugin.demo.provider".into(),
                maximum_concurrent_requests: 1,
                maximum_items: 16,
                maximum_bytes: 8_192,
                deadline_milliseconds: 30_000,
                provenance: None,
            }],
            virtual_documents: vec![crate::extensions::PluginVirtualDocumentContribution {
                id: "plugin.demo.virtual.help".into(),
                title: "Fixture help".into(),
                language: "markdown".into(),
                mode: crate::extensions::PluginVirtualDocumentMode::ReadOnly,
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
            storage: Some(crate::extensions::PluginStorageContribution {
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
    let opened = process_fixture(
        &mut host,
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
    let content = graph
        .content_for_window(window)
        .expect("private content")
        .id;
    let before_clone = graph.clone();
    assert_eq!(
        graph.create_view(content, "plugin.demo", 9),
        Err(crate::ui::window::WindowError::Unsupported("clone-view"))
    );
    assert_eq!(graph, before_clone);
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
            crate::extensions::PluginPrivateCell {
                x: 0,
                y: 0,
                grapheme: "e\u{301}".into(),
                continuation: false,
                style: 1,
            },
            crate::extensions::PluginPrivateCell {
                x: 1,
                y: 0,
                grapheme: "界".into(),
                continuation: false,
                style: 1,
            },
            crate::extensions::PluginPrivateCell {
                x: 2,
                y: 0,
                grapheme: String::new(),
                continuation: true,
                style: 1,
            },
        ],
        cursor: Some(crate::extensions::PluginPrivateCursor { x: 0, y: 0 }),
        hit_regions: vec![crate::extensions::PluginPrivateHitRegion {
            x: 0,
            y: 0,
            width: 3,
            height: 1,
            action: "plugin.demo.open".into(),
        }],
    };
    let replaced = process_fixture(
        &mut host,
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
    let rejected = process_fixture(
        &mut host,
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

    let stale = process_fixture(
        &mut host,
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

    let split_wide = process_fixture(
        &mut host,
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
            let mut catalog = crate::extensions::PluginContributionCatalog::default();
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
    let moved = process_fixture(
        &mut host,
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
    assert!(remove_fixture(&mut host, &mut graph, "demo", 9).expect("cleanup"));
    assert!(host.private_snapshots(&graph).is_empty());
}

#[test]
fn target03_dimensions_invalidates_hidden_views_and_updates_visible_siblings() {
    use crate::extensions::target03::{TargetAdmission, api::dust::plugin::host as wire};

    let mut admission = TargetAdmission::parse(include_str!(
        "../../../../../tests/fixtures/target03-clone-guest/manifest.toml"
    ), 9).unwrap();
    admission.index = private_index("demo", 9);
    admission.index.content_types[0].maximum_instances = 2;
    let mut catalogue = crate::extensions::PluginContributionCatalog::default();
    catalogue.install(admission.index.clone()).unwrap();
    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("builtin.test", 1).unwrap();
    let mut host = PluginWindowHost::default();
    let mut handles = Vec::new();
    for token in 1..=2 {
        let revision = graph.revision().unwrap();
        let opened = process_fixture(&mut host, &mut graph, &admission.index, PluginUiRequest {
            token,
            kind: PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Cells".into(),
                placement: PluginRequestedPlacement::Floating {
                    x: 2, y: 1, width: 12, height: 4,
                },
                tree: None,
            },
        });
        assert!(opened.error.is_none(), "{:?}", opened.error);
        handles.push(opened.handle.unwrap());
    }
    let projection = |graph: &WindowGraph| graph.compositor_snapshot(tab).unwrap().project(
        WindowRect { x: 0, y: 0, width: 80, height: 24 }, 1, 1,
    );
    assert_eq!(host.target03_dimensions(&projection(&graph), &catalogue, &admission, 1).unwrap().len(), 2);
    for handle in &handles {
        let revision = host.instances[handle].private.as_ref().unwrap().dimensions_revision;
        let event = process_fixture(&mut host, &mut graph, &admission.index, PluginUiRequest {
            token: 3,
            kind: PluginUiRequestKind::ReplacePrivate {
                handle: *handle,
                snapshot: PluginPrivateSnapshot {
                    revision: 1,
                    dimensions_revision: revision,
                    styles: Vec::new(), cells: Vec::new(), cursor: None, hit_regions: Vec::new(),
                },
            },
        });
        assert!(event.error.is_none(), "{:?}", event.error);
    }
    assert_eq!(host.private_snapshots(&graph).len(), 2);
    for (handle, placement) in [
        (handles[0], PluginRequestedPlacement::Hidden),
        (handles[1], PluginRequestedPlacement::Floating { x: 2, y: 1, width: 16, height: 6 }),
    ] {
        let revision = graph.revision().unwrap();
        let event = process_fixture(&mut host, &mut graph, &admission.index, PluginUiRequest {
            token: 4,
            kind: PluginUiRequestKind::PlaceWindow {
                handle, expected_graph_revision: revision, placement,
            },
        });
        assert!(event.error.is_none(), "{:?}", event.error);
    }
    let events = host.target03_dimensions(&projection(&graph), &catalogue, &admission, 10).unwrap();
    assert_eq!(events.len(), 1);
    let wire::EventValue::Dimensions(dimensions) = &events[0].value else {
        panic!("expected dimensions event");
    };
    assert_eq!(dimensions.view, host.instances[&handles[1]].view.0);
    assert_eq!((dimensions.width, dimensions.height), (16, 6));
    for (handle, expected) in [(handles[0], None), (handles[1], Some((16, 6)))] {
        let private = host.instances[&handle].private.as_ref().unwrap();
        assert_eq!(private.dimensions, expected);
        assert_eq!(private.dimensions_revision, 2);
        assert!(private.snapshot.is_none());
    }
    assert!(host.private_snapshots(&graph).is_empty());
    assert!(host.target03_dimensions(&projection(&graph), &catalogue, &admission, 11).unwrap().is_empty());

    // Even the latest dimensions revision cannot authorize writes while hidden.
    let hidden = &host.instances[&handles[0]];
    // Exercise the existing private-surface validator with the invalidated geometry.
    let error = validate_private_snapshot(
        &admission.index.content_types[0],
        hidden.private.as_ref().unwrap(),
        &PluginPrivateSnapshot {
            revision: 1,
            dimensions_revision: hidden.private.as_ref().unwrap().dimensions_revision,
            styles: Vec::new(), cells: Vec::new(), cursor: None, hit_regions: Vec::new(),
        },
        None,
    ).unwrap_err();
    assert_eq!(error.code, PluginUiErrorCode::StaleDimensions);
}

#[test]
fn real_component_drives_the_retained_window_protocol_without_wasi() {
    use sha2::{Digest, Sha256};
    let core =
        include_bytes!("../../../../../tests/fixtures/retained-guest/retained-guest.core.wasm");
    assert_eq!(
        format!("{:x}", Sha256::digest(core)),
        "3773ae204a9b1fe0436f2252161663562be895aea2392b1b4bd187c43af74416"
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
    let mut outcome = crate::extensions::load_component(
        store.path(),
        &path,
        manifest.clone(),
        &crate::extensions::PluginLimits::default(),
    )
    .expect("production component load");
    assert_eq!(
        outcome.loaded.activated_commands,
        vec![
            "drive",
            "trap",
            "flood",
            "spin",
            "private",
            "events",
            "storage",
            "provider",
            "virtual",
            "confirmation",
            "confirmationprobe"
        ]
    );

    let event_execution = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("events").into(),
            arguments: Vec::new(),
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: vec![crate::extensions::PluginHostEventEnvelope {
                id: 7,
                sequence: 3,
                kind: crate::extensions::PluginSubscriptionKind::WorkspaceMetadataChanged,
                revision: 9,
                payload: "roots=2".into(),
                coalesced: 2,
                dropped: 1,
            }],
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 44,
        },
        &mut outcome.runtime,
    )
    .expect("host event execution");
    assert_eq!(event_execution.host_event_acknowledgements, [7]);
    assert!(
        event_execution
            .result
            .contains("events:1:coalesced=2:dropped=1")
    );

    let opened = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: "demo".into(),
            command: "confirmation".into(),
            arguments: vec!["1".into()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 45,
        },
        &mut outcome.runtime,
    )
    .expect("guest confirmation view");
    assert!(opened.confirmation_requests.is_empty());
    assert_eq!(opened.ui_requests.len(), 1);
    let confirmation_action = || crate::extensions::PluginUiEvent {
        token: 1,
        handle: Some(9),
        graph_revision: Some(2),
        tree_revision: Some(1),
        surface_revision: None,
        geometry: None,
        action: Some("plugin.demo.confirmation".into()),
        error: None,
    };
    let requested = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: "demo".into(),
            command: "confirmation".into(),
            arguments: Vec::new(),
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: vec![confirmation_action()],
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 46,
        },
        &mut outcome.runtime,
    )
    .expect("guest confirmation request");
    assert_eq!(requested.confirmation_requests.len(), 1);
    assert!(requested.ui_requests.is_empty());
    let confirmation_request = requested.confirmation_requests[0].token;
    let full = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: "demo".into(),
            command: "confirmation".into(),
            arguments: vec!["1".into()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: false,
            ui_events: vec![confirmation_action()],
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 46,
        },
        &mut outcome.runtime,
    )
    .expect("real guest observes reserved host capacity pressure");
    assert_eq!(full.result, "confirmation:ConfirmationErrorCode::QueueFull");
    assert!(full.confirmation_requests.is_empty());
    for (answer, label) in [
        (
            crate::extensions::PluginConfirmationAnswer::Yes,
            "Confirmed: Yes",
        ),
        (
            crate::extensions::PluginConfirmationAnswer::No,
            "Confirmed: No",
        ),
        (
            crate::extensions::PluginConfirmationAnswer::Cancelled,
            "Confirmed: Cancelled",
        ),
    ] {
        let answered = crate::extensions::execute_component(
            crate::extensions::PluginExecutionRequest {
                runtime_epoch: 0,
                plugin: "demo".into(),
                command: "confirmation".into(),
                arguments: Vec::new(),
                snapshot: None,
                config: BTreeMap::new(),
                ui_revision: None,
                confirmation_available: false,
                ui_events: vec![crate::extensions::PluginUiEvent {
                    token: 1,
                    handle: Some(9),
                    graph_revision: Some(2),
                    tree_revision: Some(1),
                    surface_revision: None,
                    geometry: None,
                    action: None,
                    error: None,
                }],
                confirmation_results: vec![crate::extensions::PluginConfirmationResult {
                    request: confirmation_request,
                    outcome: crate::extensions::PluginConfirmationOutcome::Answered(answer),
                }],
                host_events: Vec::new(),
                storage_results: Vec::new(),
                provider_requests: Vec::new(),
                virtual_document_requests: Vec::new(),
                generation: 46,
            },
            &mut outcome.runtime,
        )
        .expect("guest confirmation result");
        assert_eq!(answered.result, label);
        assert!(answered.proposals.is_empty());
        assert!(answered.storage_requests.is_empty());
        assert_eq!(answered.ui_requests.len(), 1);
        let crate::extensions::PluginUiRequestKind::ReplaceUi { tree, .. } =
            &answered.ui_requests[0].kind
        else {
            panic!("guest result must update its retained view");
        };
        assert!(tree.nodes.iter().any(|node| node.text == label));
    }

    let mut storage =
        crate::extensions::PluginStorageService::start(store.path().join("plugin-data"), 8);
    storage.install_generation("demo", crate::extensions::PluginLoadId(44));
    let policy = manifest
        .contributions
        .storage
        .clone()
        .expect("storage policy");
    let mut storage_results = Vec::new();
    for phase in 0..7 {
        let execution = crate::extensions::execute_component(
            crate::extensions::PluginExecutionRequest {
                runtime_epoch: 0,
                plugin: ("demo").into(),
                command: ("storage").into(),
                arguments: Vec::new(),
                snapshot: None,
                config: BTreeMap::new(),
                ui_revision: None,
                confirmation_available: true,
                ui_events: Vec::new(),
                confirmation_results: Vec::new(),
                host_events: Vec::new(),
                storage_results,
                provider_requests: Vec::new(),
                virtual_document_requests: Vec::new(),
                generation: 44,
            },
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
                crate::extensions::PluginLoadId(44),
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
        crate::extensions::index_manifest_contributions(&manifest, 44, "fixture manifest".into())
            .expect("index");
    let mut providers = crate::extensions::PluginProviderBroker::default();
    providers.install(&static_index).expect("provider owner");
    let provider_request = providers
        .start(crate::extensions::PluginProviderStart {
            plugin: "demo".into(),
            load: crate::extensions::PluginLoadId(44),
            provider: "plugin.demo.provider.files".into(),
            query: String::new(),
            context: "editor".into(),
            workspace_revision: Some(3),
            buffer_revision: Some(9),
        })
        .expect("provider request");
    let mut last_envelope = None;
    for phase in 0..3 {
        let requests = providers.requests(
            "demo",
            crate::extensions::PluginLoadId(44),
            "plugin.demo.provider",
        );
        last_envelope = requests.first().cloned();
        let execution = crate::extensions::execute_component(
            crate::extensions::PluginExecutionRequest {
                runtime_epoch: 0,
                plugin: ("demo").into(),
                command: ("provider").into(),
                arguments: Vec::new(),
                snapshot: None,
                config: BTreeMap::new(),
                ui_revision: None,
                confirmation_available: true,
                ui_events: Vec::new(),
                confirmation_results: Vec::new(),
                host_events: Vec::new(),
                storage_results: Vec::new(),
                provider_requests: requests,
                virtual_document_requests: Vec::new(),
                generation: 44,
            },
            &mut outcome.runtime,
        )
        .expect("provider guest execution");
        assert_eq!(execution.provider_chunks.len(), 1);
        let status = providers
            .apply_chunk(
                "demo",
                crate::extensions::PluginLoadId(44),
                Some(3),
                Some(9),
                execution.provider_chunks.into_iter().next().unwrap(),
            )
            .expect("provider chunk");
        if phase < 2 {
            assert_eq!(
                status,
                crate::extensions::PluginProviderSessionStatus::Active
            );
        }
    }
    let projection = providers.projection(provider_request).expect("projection");
    assert_eq!(projection.items.len(), 2);
    assert_eq!(
        projection.status,
        crate::extensions::PluginProviderSessionStatus::Complete
    );
    assert!(providers.cancel(provider_request));
    let late = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("provider").into(),
            arguments: Vec::new(),
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: vec![last_envelope.expect("delivered provider request")],
            virtual_document_requests: Vec::new(),
            generation: 44,
        },
        &mut outcome.runtime,
    )
    .expect("late provider guest execution");
    assert_eq!(
        providers
            .apply_chunk(
                "demo",
                crate::extensions::PluginLoadId(44),
                Some(3),
                Some(9),
                late.provider_chunks.into_iter().next().unwrap(),
            )
            .unwrap_err()
            .code,
        crate::extensions::PluginProviderErrorCode::UnknownRequest
    );

    let mut virtuals = crate::extensions::PluginVirtualDocumentBroker::default();
    virtuals
        .install(&static_index)
        .expect("virtual source owner");
    let document = virtuals
        .start(
            "demo",
            crate::extensions::PluginLoadId(44),
            "plugin.demo.virtual.help",
            "help".into(),
        )
        .expect("virtual open");
    for phase in 0..2 {
        let requests = virtuals.requests_for_command(
            "demo",
            crate::extensions::PluginLoadId(44),
            "plugin.demo.virtual",
        );
        let execution = crate::extensions::execute_component(
            crate::extensions::PluginExecutionRequest {
                runtime_epoch: 0,
                plugin: ("demo").into(),
                command: ("virtual").into(),
                arguments: Vec::new(),
                snapshot: None,
                config: BTreeMap::new(),
                ui_revision: None,
                confirmation_available: true,
                ui_events: Vec::new(),
                confirmation_results: Vec::new(),
                host_events: Vec::new(),
                storage_results: Vec::new(),
                provider_requests: Vec::new(),
                virtual_document_requests: requests,
                generation: 44,
            },
            &mut outcome.runtime,
        )
        .expect("virtual guest execution");
        assert_eq!(execution.virtual_document_proposals.len(), 1);
        virtuals
            .apply(
                "demo",
                crate::extensions::PluginLoadId(44),
                execution
                    .virtual_document_proposals
                    .into_iter()
                    .next()
                    .unwrap(),
            )
            .expect("virtual proposal");
        if phase == 0 {
            virtuals
                .refresh("demo", crate::extensions::PluginLoadId(44), document)
                .expect("virtual refresh");
        }
    }
    let virtual_snapshot = virtuals.snapshot(document).expect("virtual snapshot");
    assert_eq!(virtual_snapshot.revision, 2);
    assert!(virtual_snapshot.text.starts_with("## Héllo"));

    let mut trapped_outcome = crate::extensions::load_component(
        store.path(),
        &path,
        manifest.clone(),
        &crate::extensions::PluginLimits::default(),
    )
    .expect("trap runtime");
    let trapped = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("trap").into(),
            arguments: vec![graph.revision().expect("revision").to_string()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: vec![crate::extensions::PluginPickerRequestEnvelope {
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
            virtual_document_requests: vec![crate::extensions::PluginVirtualDocumentRequest {
                request: 100,
                document: 101,
                source: "plugin.demo.virtual.help".into(),
                key: "trap".into(),
                kind: crate::extensions::PluginVirtualDocumentRequestKind::Open,
                current_revision: None,
                deadline_milliseconds: 1_000,
                cancellation_generation: 1,
                maximum_document_bytes: 4096,
                maximum_document_lines: 128,
                maximum_patch_bytes: 256,
                maximum_patches: 8,
            }],
            generation: 45,
        },
        &mut trapped_outcome.runtime,
    );
    assert!(matches!(
        trapped,
        Err(crate::extensions::PluginError::Execution(_))
    ));
    assert!(
        host.snapshots().is_empty(),
        "trapped proposals never escape execution"
    );

    let mut deadline_outcome = crate::extensions::load_component(
        store.path(),
        &path,
        manifest.clone(),
        &crate::extensions::PluginLimits::default(),
    )
    .expect("deadline runtime");
    let interrupted = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("spin").into(),
            arguments: vec![graph.revision().expect("revision").to_string()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: vec![crate::extensions::PluginVirtualDocumentRequest {
                request: 102,
                document: 103,
                source: "plugin.demo.virtual.help".into(),
                key: "spin".into(),
                kind: crate::extensions::PluginVirtualDocumentRequestKind::Open,
                current_revision: None,
                deadline_milliseconds: 1_000,
                cancellation_generation: 1,
                maximum_document_bytes: 4096,
                maximum_document_lines: 128,
                maximum_patch_bytes: 256,
                maximum_patches: 8,
            }],
            generation: 46,
        },
        &mut deadline_outcome.runtime,
    );
    assert!(matches!(
        interrupted,
        Err(crate::extensions::PluginError::Execution(_))
    ));
    assert!(
        host.snapshots().is_empty(),
        "interrupted proposals never escape execution"
    );

    let mut events = Vec::new();
    let mut opaque_handle = None;
    for phase in 0..5 {
        let execution = crate::extensions::execute_component(
            crate::extensions::PluginExecutionRequest {
                runtime_epoch: 0,
                plugin: ("demo").into(),
                command: ("drive").into(),
                arguments: vec![graph.revision().expect("revision").to_string()],
                snapshot: None,
                config: BTreeMap::new(),
                ui_revision: None,
                confirmation_available: true,
                ui_events: events,
                confirmation_results: Vec::new(),
                host_events: Vec::new(),
                storage_results: Vec::new(),
                provider_requests: Vec::new(),
                virtual_document_requests: Vec::new(),
                generation: 44,
            },
            &mut outcome.runtime,
        )
        .expect("guest execution");
        assert_eq!(execution.ui_requests.len(), 1);
        let event = process_fixture(
            &mut host,
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

    let private_open = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("private").into(),
            arguments: vec![graph.revision().expect("revision").to_string()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 44,
        },
        &mut outcome.runtime,
    )
    .expect("private open execution");
    let open_event = process_fixture(
        &mut host,
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
    let private_replace = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("private").into(),
            arguments: vec![graph.revision().expect("revision").to_string()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: vec![open_event, geometry_event],
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 44,
        },
        &mut outcome.runtime,
    )
    .expect("private replace execution");
    let replace_event = process_fixture(
        &mut host,
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

    let private_patch = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("private").into(),
            arguments: vec![graph.revision().expect("revision").to_string()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: vec![replace_event],
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 44,
        },
        &mut outcome.runtime,
    )
    .expect("private patch execution");
    let patch_event = process_fixture(
        &mut host,
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
            let mut catalog = crate::extensions::PluginContributionCatalog::default();
            catalog.install(static_index.clone()).expect("catalog");
            catalog
        })
        .expect("private action");
    let private_close = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 0,
            plugin: ("demo").into(),
            command: ("private").into(),
            arguments: vec![graph.revision().expect("revision").to_string()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: vec![patch_event, action_event],
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 44,
        },
        &mut outcome.runtime,
    )
    .expect("private close execution");
    assert!(private_close.result.contains("action=true"));
    let close_event = process_fixture(
        &mut host,
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

    // A restored session retires the earlier, deliberately undelivered receipts.
    let flood = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            runtime_epoch: 1,
            plugin: ("demo").into(),
            command: ("flood").into(),
            arguments: vec![graph.revision().expect("revision").to_string()],
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: true,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: 44,
        },
        &mut outcome.runtime,
    )
    .expect("bounded queue execution");
    assert_eq!(flood.ui_requests.len(), 64);
    assert!(flood.result.contains("QueueFull"));
    assert!(!remove_fixture(&mut host, &mut graph, "demo", 43).expect("stale cleanup"));
    assert!(remove_fixture(&mut host, &mut graph, "demo", 44).is_ok());
    assert_eq!(opaque_handle, Some(1));
}

#[test]
fn actual_target03_component_opens_and_clones_through_checked_existing_owners() {
    use crate::extensions::target03::{Runtime, TargetAdmission, api::dust::plugin::host as wire};
    use sha2::{Digest, Sha256};

    let core = include_bytes!(
        "../../../../../tests/fixtures/target03-clone-guest/target03-clone-guest.core.wasm"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(core)),
        "fd96d2ac94bc372426094dfc535cadc00e1ca471abd5431529f3c8cec7e7b3e5"
    );
    assert_eq!(
        include_str!("../../../../../docs/plugin-abi-next.wit"),
        include_str!("../../../../../tests/fixtures/target03-clone-guest/wit/dust-plugin.wit")
    );

    let manifest_source =
        include_str!("../../../../../tests/fixtures/target03-clone-guest/manifest.toml");
    let admission =
        TargetAdmission::parse(manifest_source, 9).expect("strict target manifest admission");
    assert!(TargetAdmission::parse(&manifest_source.replace("0.3.0", "0.4.0"), 9).is_err());
    assert!(TargetAdmission::parse(&format!("{manifest_source}\nunknown = true\n"), 9).is_err());
    assert!(
        TargetAdmission::parse(
            &manifest_source.replace(
                "clone_view = [\"plugin.demo.panel\", \"plugin.demo.private\"]",
                "clone_view = [\"plugin.demo.panel\", \"plugin.demo.panel\"]",
            ),
            9,
        )
        .is_err()
    );
    assert!(
        TargetAdmission::parse(
            &manifest_source.replace(
                "tile_view = [\"plugin.demo.panel\", \"plugin.demo.private\"]",
                "tile_view = [\"plugin.demo.panel\", \"plugin.demo.panel\"]",
            ),
            9,
        )
        .is_err()
    );
    let mut catalogue = crate::extensions::PluginContributionCatalog::default();
    catalogue
        .install(admission.index.clone())
        .expect("target catalogue admission");
    let mut runtime = Runtime::load(core, &admission).expect("actual target 0.3 Component");
    assert_eq!(
        runtime.activated_commands(),
        [
            "callbackopen".to_owned(),
            "clone".to_owned(),
            "cloneprivate".to_owned(),
            "close".to_owned(),
            "dock".to_owned(),
            "failafterqueue".to_owned(),
            "focus".to_owned(),
            "fullscreen".to_owned(),
            "fuelafterqueue".to_owned(),
            "hide".to_owned(),
            "move".to_owned(),
            "open".to_owned(),
            "openprivate".to_owned(),
            "openprivatefail".to_owned(),
            "openprivateoutofbounds".to_owned(),
            "openprivateoversized".to_owned(),
            "openprivatestaledim".to_owned(),
            "openprivatestalesurface".to_owned(),
            "openprivatetrap".to_owned(),
            "openprivatewide".to_owned(),
            "openprivatewrongview".to_owned(),
            "oversized".to_owned(),
            "oversizednotice".to_owned(),
            "oversizedresult".to_owned(),
            "oversizedtitle".to_owned(),
            "tile".to_owned(),
            "trapafterqueue".to_owned(),
            "unsupported".to_owned(),
            "unsupportedreplace".to_owned(),
        ]
        .into_iter()
        .collect()
    );

    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("builtin.test", 1).expect("tab");
    let editor_content = graph
        .create_content(
            ContentTypeId::new("builtin.text-buffer").unwrap(),
            ContentAdapterKind::TextBuffer,
            "editor".into(),
            false,
            "builtin.test",
            1,
        )
        .unwrap();
    let editor_view = graph
        .create_view(editor_content, "builtin.test", 1)
        .unwrap();
    let editor_window = graph
        .open_window(
            tab,
            editor_view,
            WindowPlacement::Floating(WindowRect {
                x: 0,
                y: 0,
                width: 20,
                height: 6,
            }),
            true,
            "builtin.test",
            1,
        )
        .unwrap();
    let invocation = |id: u64,
                      command: &str,
                      graph: &WindowGraph,
                      content: crate::ui::window::ContentId,
                      view: crate::ui::window::ViewId,
                      window: crate::ui::window::WindowId| wire::Invocation {
        id,
        kind: wire::ExecutionKind::Command,
        command: command.into(),
        arguments: Vec::new(),
        source: None,
        ui: Some(wire::UiContext {
            graph_revision: graph.revision().expect("graph revision"),
            tab: tab.0,
            window: window.0,
            view: view.0,
            content: content.0,
            document_bytes: None,
        }),
    };
    let callback =
        |id: u64, token: u64, result: Result<wire::UiReceipt, wire::Failure>| wire::Event {
            id,
            context: wire::Invocation {
                id: id + 100,
                kind: wire::ExecutionKind::ResultOnly,
                command: "ui-result".into(),
                arguments: Vec::new(),
                source: None,
                ui: None,
            },
            value: wire::EventValue::UiResult((token, result)),
        };

    let mut host = PluginWindowHost::default();
    let graph_before = graph.clone();
    let error = runtime
        .execute(
            invocation(
                0,
                "unsupported",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect_err("unimplemented target import must be typed Unsupported");
    assert!(error.starts_with("ErrorCode::Unsupported:"), "{error}");
    assert!(
        error.contains("queue-snapshot is not implemented"),
        "{error}"
    );
    assert!(runtime.take_requests().is_empty());
    assert!(runtime.take_notices().is_empty());
    assert_eq!(graph, graph_before);
    assert!(host.snapshots().is_empty());

    let error = runtime
        .execute(
            invocation(
                1,
                "unsupportedreplace",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect_err("unimplemented retained replacement must be typed Unsupported");
    assert!(error.starts_with("ErrorCode::Unsupported:"), "{error}");
    assert!(
        error.contains("queue-ui request is not implemented"),
        "{error}"
    );
    assert!(runtime.take_requests().is_empty());
    assert!(runtime.take_notices().is_empty());
    assert_eq!(graph, graph_before);
    assert!(host.snapshots().is_empty());

    let error = runtime
        .execute(
            invocation(
                2,
                "failafterqueue",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect_err("failed execution must roll back its accepted queue-ui");
    assert!(error.contains("failure after accepted queue-ui"), "{error}");
    assert!(runtime.take_requests().is_empty());
    assert!(runtime.take_notices().is_empty());
    assert_eq!(graph, graph_before);
    assert!(host.snapshots().is_empty());

    let mut trap_runtime = Runtime::load(core, &admission).expect("trap probe Component");
    let error = trap_runtime
        .execute(
            invocation(
                2,
                "trapafterqueue",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect_err("trapping execution must roll back its accepted queue-ui");
    assert!(error.contains("error while executing"), "{error}");
    assert!(trap_runtime.take_requests().is_empty());
    assert!(trap_runtime.take_notices().is_empty());
    drop(trap_runtime);
    assert_eq!(graph, graph_before);
    assert!(host.snapshots().is_empty());

    let mut fuel_runtime = Runtime::load(core, &admission).expect("fuel probe Component");
    let error = fuel_runtime
        .execute(
            invocation(
                3,
                "fuelafterqueue",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect_err("fuel exhaustion must roll back its accepted queue-ui");
    assert!(error.to_ascii_lowercase().contains("fuel"), "{error}");
    assert!(fuel_runtime.take_requests().is_empty());
    assert!(fuel_runtime.take_notices().is_empty());
    drop(fuel_runtime);
    assert_eq!(graph, graph_before);
    assert!(host.snapshots().is_empty());

    let mut bounded_runtime = Runtime::load(core, &admission).expect("bounded result Component");
    for (id, command, expected) in [
        (4, "oversizednotice", "InvalidArgument"),
        (5, "oversizedresult", "LimitExceeded"),
        (6, "oversizedtitle", "InvalidArgument"),
    ] {
        let error = bounded_runtime
            .execute(
                invocation(
                    id,
                    command,
                    &graph,
                    editor_content,
                    editor_view,
                    editor_window,
                ),
                &catalogue,
            )
            .expect_err("oversized actual Component output must reject");
        assert!(
            error.starts_with(&format!("ErrorCode::{expected}:")),
            "{error}"
        );
        assert!(bounded_runtime.take_requests().is_empty());
        assert!(bounded_runtime.take_notices().is_empty());
        assert_eq!(graph, graph_before);
        assert!(host.snapshots().is_empty());
    }
    let mut oversized_invocation = invocation(
        7,
        "open",
        &graph,
        editor_content,
        editor_view,
        editor_window,
    );
    oversized_invocation.arguments.push(wire::Argument {
        name: "payload".into(),
        value: wire::Scalar::Text("x".repeat(1024 * 1024)),
    });
    let error = bounded_runtime
        .execute(oversized_invocation, &catalogue)
        .expect_err("oversized invocation must reject before entering the Component");
    assert!(error.contains("request byte ceiling"), "{error}");
    assert!(bounded_runtime.take_requests().is_empty());
    bounded_runtime
        .deactivate()
        .expect("bounded result load retirement");

    let error = runtime
        .execute(
            invocation(
                8,
                "oversized",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect_err("oversized actual guest tree must fail before reservation");
    assert!(error.starts_with("ErrorCode::LimitExceeded:"), "{error}");
    assert!(runtime.take_requests().is_empty());
    assert_eq!(graph, graph_before);
    assert!(host.snapshots().is_empty());

    runtime
        .execute(
            invocation(
                4,
                "open",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect("guest open callback");
    let mut requests = runtime.take_requests();
    assert_eq!(requests.len(), 1);
    let original_token = requests[0].token;
    assert_eq!(original_token, 2, "failed execution never reuses its token");
    let original = host
        .process_target03(&mut graph, &catalogue, &admission, requests.remove(0))
        .expect("checked original publication");
    let original_content = crate::ui::window::ContentId(original.content);
    assert!(
        graph
            .content(original_content)
            .expect("actual Component content")
            .capabilities
            .clone_view
    );
    assert_eq!(
        graph.require_persist_descriptor(original_content),
        Err(crate::ui::window::WindowError::Unsupported(
            "persist-descriptor"
        ))
    );
    let graph_after_original = graph.clone();
    let host_after_original = host.clone();
    let error = runtime
        .handle_events(
            vec![callback(
                50,
                original_token,
                Err(wire::Failure {
                    code: wire::ErrorCode::ProtocolFailure,
                    message: "x".repeat(4_097),
                    retryable: false,
                }),
            )],
            &catalogue,
        )
        .expect_err("oversized callback result must reject before token delivery");
    assert!(error.contains("message ceiling"), "{error}");
    assert_eq!(graph, graph_after_original);
    assert_eq!(host, host_after_original);
    assert!(runtime.take_requests().is_empty());
    assert!(runtime.take_notices().is_empty());
    runtime
        .handle_events(vec![callback(1, original_token, Ok(original))], &catalogue)
        .expect("exact original UI result callback after bounded refusal");
    assert_eq!(
        runtime.take_notices(),
        [format!(
            "ui-result:{original_token}:{}:{}:{}:{}:{}",
            original.content,
            original.view,
            original.window,
            original.graph_revision,
            original.view_revision
        )]
    );

    runtime
        .execute(
            invocation(
                4,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("exact activation clone staging");
    let mut wrong_activation = runtime.take_requests();
    let wrong_activation_token = wrong_activation[0].token;
    let same_metadata = TargetAdmission::parse(manifest_source, 9)
        .expect("separate activation with the same textual metadata");
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(
            &mut graph,
            &catalogue,
            &same_metadata,
            wrong_activation.remove(0),
        )
        .expect_err("a different activation cannot adopt staged work");
    assert_eq!(error.code, wire::ErrorCode::StaleSource);
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    19,
                    wrong_activation_token,
                    Err(wire::Failure {
                        code: error.code,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue,
            )
            .is_err()
    );

    for (id, mismatch) in [(191, "owner"), (192, "checksum")] {
        runtime
            .execute(
                invocation(
                    id,
                    "clone",
                    &graph,
                    crate::ui::window::ContentId(original.content),
                    crate::ui::window::ViewId(original.view),
                    crate::ui::window::WindowId(original.window),
                ),
                &catalogue,
            )
            .expect("load-identity mismatch staging");
        let mut request = runtime.take_requests();
        let token = request[0].token;
        let mut wrong_admission = admission.clone();
        match mismatch {
            "owner" => wrong_admission.index.owner = "foreign".into(),
            "checksum" => wrong_admission.index.checksum = "e".repeat(64),
            _ => unreachable!(),
        }
        let graph_before = graph.clone();
        let host_before = host.clone();
        let error = host
            .process_target03(&mut graph, &catalogue, &wrong_admission, request.remove(0))
            .expect_err("wrong load identity cannot adopt staged work");
        assert_eq!(error.code, wire::ErrorCode::StaleSource);
        assert_eq!(graph, graph_before);
        assert_eq!(host, host_before);
        assert!(
            runtime
                .handle_events(
                    vec![callback(
                        id,
                        token,
                        Err(wire::Failure {
                            code: error.code,
                            message: error.message,
                            retryable: false,
                        }),
                    )],
                    &catalogue,
                )
                .is_err()
        );
    }

    runtime
        .execute(
            invocation(
                5,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("guest clone callback");
    let mut requests = runtime.take_requests();
    assert_eq!(requests.len(), 1);
    let clone_token = requests[0].token;
    let clone = host
        .process_target03(&mut graph, &catalogue, &admission, requests.remove(0))
        .expect("checked clone publication");
    assert_eq!(clone.content, original.content);
    assert_ne!(clone.view, original.view);
    assert_ne!(clone.window, original.window);
    assert_eq!(graph.content_snapshots()[1].view_count, 2);
    runtime
        .handle_events(vec![callback(2, clone_token, Ok(clone))], &catalogue)
        .expect("exact clone UI result callback");
    assert_eq!(runtime.take_notices().len(), 1);
    assert!(
        runtime
            .handle_events(vec![callback(3, clone_token, Ok(clone))], &catalogue)
            .unwrap_err()
            .contains("stale or duplicated")
    );
    assert!(runtime.take_notices().is_empty());

    macro_rules! apply_component_window_operation {
        ($id:expr, $command:expr, $source:expr) => {{
            let source = $source;
            runtime
                .execute(
                    invocation(
                        $id,
                        $command,
                        &graph,
                        crate::ui::window::ContentId(source.content),
                        crate::ui::window::ViewId(source.view),
                        crate::ui::window::WindowId(source.window),
                    ),
                    &catalogue,
                )
                .unwrap_or_else(|error| panic!("actual Component {} staging: {error}", $command));
            let mut requests = runtime.take_requests();
            assert_eq!(requests.len(), 1, "one {} request", $command);
            let token = requests[0].token;
            let receipt = host
                .process_target03(&mut graph, &catalogue, &admission, requests.remove(0))
                .unwrap_or_else(|error| {
                    panic!("actual Component {} publication: {error:?}", $command)
                });
            runtime
                .handle_events(vec![callback($id + 100, token, Ok(receipt))], &catalogue)
                .unwrap_or_else(|error| panic!("actual Component {} callback: {error}", $command));
            runtime.take_notices();
            receipt
        }};
    }
    let moved = apply_component_window_operation!(6, "move", clone);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(clone.window))
            .expect("moved target window")
            .placement,
        WindowPlacement::Floating(WindowRect {
            x: 9,
            y: 5,
            width: 36,
            height: 11,
        })
    );
    let tiled = apply_component_window_operation!(7, "tile", moved);
    assert_eq!(tiled.content, original.content);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(clone.window))
            .expect("tiled target window")
            .placement,
        WindowPlacement::Tiled
    );
    let docked = apply_component_window_operation!(8, "dock", tiled);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(clone.window))
            .expect("docked target window")
            .placement,
        WindowPlacement::Docked {
            edge: crate::ui::window::DockEdge::Right,
            size: 19,
        }
    );
    let fullscreen = apply_component_window_operation!(9, "fullscreen", docked);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(clone.window))
            .expect("fullscreen target window")
            .placement,
        WindowPlacement::Fullscreen
    );
    let hidden = apply_component_window_operation!(10, "hide", fullscreen);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(clone.window))
            .expect("hidden target window")
            .placement,
        WindowPlacement::Hidden
    );
    runtime
        .execute(
            invocation(
                11,
                "focus",
                &graph,
                crate::ui::window::ContentId(hidden.content),
                crate::ui::window::ViewId(hidden.view),
                crate::ui::window::WindowId(hidden.window),
            ),
            &catalogue,
        )
        .expect("hidden focus request is staged by the actual Component");
    let mut hidden_focus = runtime.take_requests();
    let hidden_focus_token = hidden_focus[0].token;
    let hidden_graph = graph.clone();
    let hidden_host = host.clone();
    let hidden_error = host
        .process_target03(&mut graph, &catalogue, &admission, hidden_focus.remove(0))
        .expect_err("hidden target cannot receive focus");
    assert_eq!(hidden_error.code, wire::ErrorCode::InvalidArgument);
    assert_eq!(graph, hidden_graph);
    assert_eq!(host, hidden_host);
    assert!(
        runtime
            .handle_events(
                vec![callback(111, hidden_focus_token, Err(hidden_error))],
                &catalogue,
            )
            .is_err()
    );
    let shown = apply_component_window_operation!(12, "move", hidden);
    let focused = apply_component_window_operation!(13, "focus", shown);
    assert_eq!(
        graph.focused(),
        Some(crate::ui::window::WindowId(clone.window))
    );
    let other_tab = graph
        .create_workspace_tab("builtin.cross-tab", 1)
        .expect("cross-tab switch target");
    graph.set_active_tab(other_tab).expect("switch away");
    assert_ne!(graph.active_tab(), Some(tab));
    let cross_tab_focused = apply_component_window_operation!(140, "focus", focused);
    assert_eq!(cross_tab_focused.window, focused.window);
    assert_eq!(graph.active_tab(), Some(tab));
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(focused.window))
            .expect("same cross-tab retained window")
            .view
            .0,
        focused.view
    );
    graph
        .remove_workspace_tab(other_tab)
        .expect("retire empty cross-tab fixture");
    let close_source = apply_component_window_operation!(14, "clone", focused);
    assert_ne!(close_source.window, clone.window);
    let closed = apply_component_window_operation!(15, "close", close_source);
    assert_eq!(closed.window, close_source.window);
    assert!(
        graph
            .window(crate::ui::window::WindowId(close_source.window))
            .is_none()
    );
    assert!(
        graph
            .window(crate::ui::window::WindowId(clone.window))
            .is_some()
    );
    assert_eq!(
        graph
            .content(crate::ui::window::ContentId(original.content))
            .expect("original shared content survives clone close")
            .id
            .0,
        original.content
    );

    runtime
        .execute(
            invocation(
                20,
                "clone",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect("hostile foreign-source guest callback");
    let mut foreign = runtime.take_requests();
    assert_eq!(foreign.len(), 1);
    let foreign_token = foreign[0].token;
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(&mut graph, &catalogue, &admission, foreign.remove(0))
        .expect_err("foreign clone source must reject");
    assert_eq!(error.code, wire::ErrorCode::ForeignHandle);
    assert!(error.message.contains("stale or foreign"));
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    20,
                    foreign_token,
                    Err(wire::Failure {
                        code: wire::ErrorCode::ForeignHandle,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue
            )
            .is_err()
    );
    assert!(runtime.take_notices().is_empty());

    for (id, identity) in [(520, "window"), (521, "view"), (522, "content")] {
        let mut hostile = invocation(
            id,
            "move",
            &graph,
            crate::ui::window::ContentId(original.content),
            crate::ui::window::ViewId(original.view),
            crate::ui::window::WindowId(original.window),
        );
        let ui = hostile.ui.as_mut().expect("hostile identity UI source");
        match identity {
            "window" => ui.window = editor_window.0,
            "view" => ui.view = editor_view.0,
            "content" => ui.content = editor_content.0,
            _ => unreachable!(),
        }
        runtime
            .execute(hostile, &catalogue)
            .unwrap_or_else(|error| panic!("actual Component wrong-{identity} staging: {error}"));
        let mut hostile_request = runtime.take_requests();
        assert_eq!(hostile_request.len(), 1);
        let token = hostile_request[0].token;
        let graph_before = graph.clone();
        let host_before = host.clone();
        let error = host
            .process_target03(
                &mut graph,
                &catalogue,
                &admission,
                hostile_request.remove(0),
            )
            .unwrap_err();
        assert_eq!(error.code, wire::ErrorCode::ForeignHandle);
        assert_eq!(graph, graph_before);
        assert_eq!(host, host_before);
        assert!(
            runtime
                .handle_events(
                    vec![callback(
                        id,
                        token,
                        Err(wire::Failure {
                            code: error.code,
                            message: error.message,
                            retryable: false,
                        }),
                    )],
                    &catalogue,
                )
                .is_err()
        );
        assert!(runtime.take_requests().is_empty());
        assert!(runtime.take_notices().is_empty());
    }

    runtime
        .execute(
            invocation(
                21,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("capability-refusal guest callback");
    let mut denied = runtime.take_requests();
    assert_eq!(denied.len(), 1);
    let denied_token = denied[0].token;
    let denied_admission = admission.without_clone("plugin.demo.panel");
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(&mut graph, &catalogue, &denied_admission, denied.remove(0))
        .expect_err("clone-view=false must reject");
    assert_eq!(error.code, wire::ErrorCode::PermissionDenied);
    assert!(error.message.contains("denied"));
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    21,
                    denied_token,
                    Err(wire::Failure {
                        code: wire::ErrorCode::PermissionDenied,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue
            )
            .is_err()
    );
    assert!(runtime.take_notices().is_empty());

    runtime
        .execute(
            invocation(
                211,
                "tile",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("capability-invalid tile staging");
    let mut denied_tile = runtime.take_requests();
    assert_eq!(denied_tile.len(), 1);
    let denied_tile_token = denied_tile[0].token;
    let denied_tile_admission = admission.without_tile("plugin.demo.panel");
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(
            &mut graph,
            &catalogue,
            &denied_tile_admission,
            denied_tile.remove(0),
        )
        .expect_err("tile-view=false must reject");
    assert_eq!(error.code, wire::ErrorCode::PermissionDenied);
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    211,
                    denied_tile_token,
                    Err(wire::Failure {
                        code: error.code,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue,
            )
            .is_err()
    );
    assert!(runtime.take_notices().is_empty());

    runtime
        .execute(
            invocation(
                22,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("wrong-generation guest callback");
    let mut wrong_generation = runtime.take_requests();
    assert_eq!(wrong_generation.len(), 1);
    let wrong_generation_token = wrong_generation[0].token;
    let mut replacement_admission = admission.clone();
    replacement_admission.index.generation += 1;
    replacement_admission.index.checksum = "f".repeat(64);
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(
            &mut graph,
            &catalogue,
            &replacement_admission,
            wrong_generation.remove(0),
        )
        .expect_err("wrong generation must reject");
    assert_eq!(error.code, wire::ErrorCode::StaleSource);
    assert!(error.message.contains("retired"));
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    22,
                    wrong_generation_token,
                    Err(wire::Failure {
                        code: error.code,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue
            )
            .is_err()
    );
    assert!(runtime.take_notices().is_empty());

    let snapshots = host.snapshots();
    assert_eq!(snapshots.len(), 2);
    assert!(
        snapshots[0]
            .rows
            .iter()
            .any(|row| row.text == "target-0.3 original")
    );
    assert!(
        snapshots[1]
            .rows
            .iter()
            .any(|row| row.text == "target-0.3 clone")
    );
    let clone_handle = snapshots[1].handle;
    let update = process_fixture(
        &mut host,
        &mut graph,
        &admission.index,
        PluginUiRequest {
            token: 90,
            kind: PluginUiRequestKind::ReplaceUi {
                handle: clone_handle,
                expected_tree_revision: 1,
                tree: tree(2, "clone-only update"),
            },
        },
    );
    assert!(update.error.is_none(), "{:?}", update.error);
    let snapshots = host.snapshots();
    assert!(
        snapshots[0]
            .rows
            .iter()
            .any(|row| row.text == "target-0.3 original")
    );
    assert!(
        snapshots[1]
            .rows
            .iter()
            .any(|row| row.text == "clone-only update")
    );

    runtime
        .execute(
            invocation(
                30,
                "openprivate",
                &graph,
                crate::ui::window::ContentId(clone.content),
                crate::ui::window::ViewId(clone.view),
                crate::ui::window::WindowId(clone.window),
            ),
            &catalogue,
        )
        .expect("guest private open callback");
    let mut private_open = runtime.take_requests();
    assert_eq!(private_open.len(), 1);
    let private_open_token = private_open[0].token;
    let private_original = host
        .process_target03(&mut graph, &catalogue, &admission, private_open.remove(0))
        .expect("checked private original publication");
    assert_eq!(private_original.view_revision, 0);
    runtime
        .handle_events(
            vec![callback(30, private_open_token, Ok(private_original))],
            &catalogue,
        )
        .expect("private original result acknowledgement");
    runtime.take_notices();
    assert!(host.private_snapshots(&graph).is_empty());

    let projection = graph
        .compositor_snapshot(tab)
        .expect("target graph snapshot")
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
    let private_dimensions = host
        .target03_dimensions(&projection, &catalogue, &admission, 31)
        .expect("private original dimensions");
    assert_eq!(private_dimensions.len(), 1);
    let duplicate_dimensions = private_dimensions[0].clone();
    runtime
        .handle_events(private_dimensions, &catalogue)
        .expect("guest dimensions acknowledgement and fresh cell callback");
    assert!(runtime.take_notices()[0].starts_with("dimensions:"));
    let mut private_cells = runtime.take_requests();
    assert_eq!(private_cells.len(), 1);
    let private_cells_token = private_cells[0].token;
    let private_cells_receipt = host
        .process_target03(&mut graph, &catalogue, &admission, private_cells.remove(0))
        .expect("fresh original private cells");
    runtime
        .handle_events(
            vec![callback(32, private_cells_token, Ok(private_cells_receipt))],
            &catalogue,
        )
        .expect("private cell result acknowledgement");
    runtime.take_notices();
    assert_eq!(host.private_snapshots(&graph)[0].cells[0].grapheme, "A");
    assert!(
        runtime
            .handle_events(vec![duplicate_dimensions], &catalogue)
            .unwrap_err()
            .contains("stale, duplicated")
    );
    assert!(runtime.take_requests().is_empty());

    let private_moved = apply_component_window_operation!(120, "move", private_original);
    assert_eq!(private_moved.content, private_original.content);
    assert_eq!(private_moved.view, private_original.view);
    assert_eq!(private_moved.window, private_original.window);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(private_moved.window))
            .expect("moved private window")
            .placement,
        WindowPlacement::Floating(WindowRect {
            x: 9,
            y: 5,
            width: 36,
            height: 11,
        })
    );
    let private_tiled = apply_component_window_operation!(121, "tile", private_moved);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(private_tiled.window))
            .expect("tiled private window")
            .placement,
        WindowPlacement::Tiled
    );
    let private_docked = apply_component_window_operation!(122, "dock", private_tiled);
    assert!(matches!(
        graph
            .window(crate::ui::window::WindowId(private_docked.window))
            .expect("docked private window")
            .placement,
        WindowPlacement::Docked { .. }
    ));
    let private_fullscreen = apply_component_window_operation!(123, "fullscreen", private_docked);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(private_fullscreen.window))
            .expect("fullscreen private window")
            .placement,
        WindowPlacement::Fullscreen
    );
    let private_hidden = apply_component_window_operation!(124, "hide", private_fullscreen);
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(private_hidden.window))
            .expect("hidden private window")
            .placement,
        WindowPlacement::Hidden
    );
    let private_shown = apply_component_window_operation!(125, "move", private_hidden);
    let private_focused = apply_component_window_operation!(126, "focus", private_shown);
    assert_eq!(
        graph.focused().map(|window| window.0),
        Some(private_focused.window)
    );
    let private_other_tab = graph
        .create_workspace_tab("builtin.private-cross-tab", 1)
        .expect("private cross-tab switch target");
    graph
        .set_active_tab(private_other_tab)
        .expect("switch away from private view");
    let private_cross_tab_focused =
        apply_component_window_operation!(141, "focus", private_focused);
    assert_eq!(private_cross_tab_focused.window, private_focused.window);
    assert_eq!(graph.active_tab(), Some(tab));
    assert_eq!(
        graph
            .window(crate::ui::window::WindowId(private_focused.window))
            .expect("same cross-tab private window")
            .view
            .0,
        private_focused.view
    );
    graph
        .remove_workspace_tab(private_other_tab)
        .expect("retire empty private cross-tab fixture");
    let projection = graph
        .compositor_snapshot(tab)
        .expect("moved private graph snapshot")
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
    let moved_dimensions = host
        .target03_dimensions(&projection, &catalogue, &admission, 127)
        .expect("moved private dimensions");
    assert_eq!(moved_dimensions.len(), 1);
    runtime
        .handle_events(moved_dimensions, &catalogue)
        .expect("moved private dimensions callback");
    runtime.take_notices();
    let mut moved_cells = runtime.take_requests();
    assert_eq!(moved_cells.len(), 1);
    let moved_cells_token = moved_cells[0].token;
    let moved_cells_receipt = host
        .process_target03(&mut graph, &catalogue, &admission, moved_cells.remove(0))
        .expect("moved private cells");
    runtime
        .handle_events(
            vec![callback(128, moved_cells_token, Ok(moved_cells_receipt))],
            &catalogue,
        )
        .expect("moved private cells receipt");
    runtime.take_notices();

    let denied_private_admission = admission.without_clone("plugin.demo.private");
    let mut denied_private =
        Runtime::load(core, &denied_private_admission).expect("private denial Component");
    let error = denied_private
        .execute(
            invocation(
                33,
                "cloneprivate",
                &graph,
                crate::ui::window::ContentId(private_original.content),
                crate::ui::window::ViewId(private_original.view),
                crate::ui::window::WindowId(private_original.window),
            ),
            &catalogue,
        )
        .expect_err("clone_view=false rejects before token reservation");
    assert!(error.starts_with("ErrorCode::PermissionDenied:"), "{error}");
    assert!(denied_private.take_requests().is_empty());
    denied_private.deactivate().expect("denied load retirement");

    runtime
        .execute(
            invocation(
                34,
                "cloneprivate",
                &graph,
                crate::ui::window::ContentId(private_original.content),
                crate::ui::window::ViewId(private_original.view),
                crate::ui::window::WindowId(private_original.window),
            ),
            &catalogue,
        )
        .expect("guest private clone callback");
    let mut private_clone = runtime.take_requests();
    assert_eq!(private_clone.len(), 1);
    let private_clone_token = private_clone[0].token;
    let private_clone = host
        .process_target03(&mut graph, &catalogue, &admission, private_clone.remove(0))
        .expect("checked private clone publication");
    assert_eq!(private_clone.content, private_original.content);
    assert_ne!(private_clone.view, private_original.view);
    assert_ne!(private_clone.window, private_original.window);
    assert_eq!(private_clone.view_revision, 0);
    runtime
        .handle_events(
            vec![callback(33, private_clone_token, Ok(private_clone))],
            &catalogue,
        )
        .expect("private clone result acknowledgement");
    runtime.take_notices();
    assert_eq!(host.private_snapshots(&graph).len(), 1);

    let projection = graph
        .compositor_snapshot(tab)
        .expect("target clone graph snapshot")
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
    let private_clone_dimensions = host
        .target03_dimensions(&projection, &catalogue, &admission, 34)
        .expect("private clone dimensions");
    assert_eq!(private_clone_dimensions.len(), 1);
    runtime
        .handle_events(private_clone_dimensions, &catalogue)
        .expect("guest clone dimensions acknowledgement and fresh cells");
    runtime.take_notices();
    let mut clone_cells = runtime.take_requests();
    assert_eq!(clone_cells.len(), 1);
    let clone_cells_token = clone_cells[0].token;
    let clone_cells_receipt = host
        .process_target03(&mut graph, &catalogue, &admission, clone_cells.remove(0))
        .expect("fresh cloned private cells");
    runtime
        .handle_events(
            vec![callback(35, clone_cells_token, Ok(clone_cells_receipt))],
            &catalogue,
        )
        .expect("cloned private cell result acknowledgement");
    runtime.take_notices();
    let private = host.private_snapshots(&graph);
    assert_eq!(private.len(), 2);
    assert_eq!(private[0].cells[0].grapheme, "A");
    assert_eq!(private[1].cells[0].grapheme, "B");
    assert_ne!(private[0].window, private[1].window);
    assert_eq!(
        graph
            .view(crate::ui::window::ViewId(private_original.view))
            .expect("private original view")
            .content,
        graph
            .view(crate::ui::window::ViewId(private_clone.view))
            .expect("private clone view")
            .content
    );
    assert!(
        host.target03_dimensions(&projection, &catalogue, &admission, 36)
            .expect("stable private geometry")
            .is_empty()
    );

    runtime
        .execute(
            invocation(
                42,
                "openprivatefail",
                &graph,
                crate::ui::window::ContentId(private_clone.content),
                crate::ui::window::ViewId(private_clone.view),
                crate::ui::window::WindowId(private_clone.window),
            ),
            &catalogue,
        )
        .expect("callback rollback private open");
    let mut fail_private = runtime.take_requests();
    let fail_private_token = fail_private[0].token;
    let fail_private_receipt = host
        .process_target03(&mut graph, &catalogue, &admission, fail_private.remove(0))
        .expect("callback rollback private publication");
    runtime
        .handle_events(
            vec![callback(42, fail_private_token, Ok(fail_private_receipt))],
            &catalogue,
        )
        .expect("callback rollback private receipt");
    runtime.take_notices();
    let projection = graph
        .compositor_snapshot(tab)
        .expect("callback rollback graph snapshot")
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
    let fail_dimensions = host
        .target03_dimensions(&projection, &catalogue, &admission, 43)
        .expect("callback rollback dimensions");
    assert_eq!(fail_dimensions.len(), 1);
    let duplicate_fail_dimensions = fail_dimensions[0].clone();
    let error = runtime
        .handle_events(fail_dimensions, &catalogue)
        .expect_err("failed callback rolls back its queued cells");
    assert!(
        error.contains("failure after queued private cells"),
        "{error}"
    );
    assert!(runtime.take_requests().is_empty());
    assert!(runtime.take_notices().is_empty());
    assert!(
        runtime
            .handle_events(vec![duplicate_fail_dimensions], &catalogue)
            .unwrap_err()
            .contains("stale, duplicated")
    );
    assert_eq!(host.private_snapshots(&graph).len(), 2);

    runtime
        .execute(
            invocation(
                44,
                "openprivateoversized",
                &graph,
                crate::ui::window::ContentId(private_clone.content),
                crate::ui::window::ViewId(private_clone.view),
                crate::ui::window::WindowId(private_clone.window),
            ),
            &catalogue,
        )
        .expect("oversized private open");
    let mut oversized_private = runtime.take_requests();
    let oversized_private_token = oversized_private[0].token;
    let oversized_private_receipt = host
        .process_target03(
            &mut graph,
            &catalogue,
            &admission,
            oversized_private.remove(0),
        )
        .expect("oversized private publication");
    runtime
        .handle_events(
            vec![callback(
                44,
                oversized_private_token,
                Ok(oversized_private_receipt),
            )],
            &catalogue,
        )
        .expect("oversized private receipt");
    runtime.take_notices();
    let projection = graph
        .compositor_snapshot(tab)
        .expect("oversized private graph snapshot")
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
    let oversized_dimensions = host
        .target03_dimensions(&projection, &catalogue, &admission, 45)
        .expect("oversized private dimensions");
    assert_eq!(oversized_dimensions.len(), 1);
    let error = runtime
        .handle_events(oversized_dimensions, &catalogue)
        .expect_err("oversized actual private cells reject before reservation");
    assert!(error.starts_with("ErrorCode::LimitExceeded:"), "{error}");
    assert!(runtime.take_requests().is_empty());
    assert!(runtime.take_notices().is_empty());
    assert_eq!(host.private_snapshots(&graph).len(), 2);

    let mut trap_callback_runtime =
        Runtime::load(core, &admission).expect("callback trap probe Component");
    trap_callback_runtime
        .execute(
            invocation(
                900,
                "openprivatetrap",
                &graph,
                crate::ui::window::ContentId(private_clone.content),
                crate::ui::window::ViewId(private_clone.view),
                crate::ui::window::WindowId(private_clone.window),
            ),
            &catalogue,
        )
        .expect("callback trap private open staging");
    let mut trap_private = trap_callback_runtime.take_requests();
    let trap_private_token = trap_private[0].token;
    let trap_private_receipt = host
        .process_target03(&mut graph, &catalogue, &admission, trap_private.remove(0))
        .expect("callback trap private publication");
    trap_callback_runtime
        .handle_events(
            vec![callback(900, trap_private_token, Ok(trap_private_receipt))],
            &catalogue,
        )
        .expect("callback trap private receipt");
    trap_callback_runtime.take_notices();
    let projection = graph
        .compositor_snapshot(tab)
        .expect("callback trap graph snapshot")
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
    let trap_dimensions = host
        .target03_dimensions(&projection, &catalogue, &admission, 901)
        .expect("callback trap dimensions");
    assert_eq!(trap_dimensions.len(), 1);
    let duplicate_trap_dimensions = trap_dimensions[0].clone();
    let error = trap_callback_runtime
        .handle_events(trap_dimensions, &catalogue)
        .expect_err("trapping callback must roll back accepted private cells");
    assert!(error.contains("error while executing"), "{error}");
    assert!(trap_callback_runtime.take_requests().is_empty());
    assert!(trap_callback_runtime.take_notices().is_empty());
    assert!(
        trap_callback_runtime
            .handle_events(vec![duplicate_trap_dimensions], &catalogue)
            .unwrap_err()
            .contains("stale, duplicated")
    );
    drop(trap_callback_runtime);
    assert_eq!(host.private_snapshots(&graph).len(), 2);

    let mut hostile_event = 1_000;
    for (probe, runtime_rejects) in [
        ("openprivatewrongview", true),
        ("openprivatestaledim", true),
        ("openprivatestalesurface", false),
        ("openprivateoutofbounds", false),
        ("openprivatewide", false),
    ] {
        let event = hostile_event;
        hostile_event += 3;
        runtime
            .execute(
                invocation(
                    event,
                    probe,
                    &graph,
                    crate::ui::window::ContentId(private_clone.content),
                    crate::ui::window::ViewId(private_clone.view),
                    crate::ui::window::WindowId(private_clone.window),
                ),
                &catalogue,
            )
            .expect("hostile private open staging");
        let mut open = runtime.take_requests();
        assert_eq!(open.len(), 1);
        let open_token = open[0].token;
        let receipt = host
            .process_target03(&mut graph, &catalogue, &admission, open.remove(0))
            .expect("hostile private view publication");
        runtime
            .handle_events(vec![callback(event, open_token, Ok(receipt))], &catalogue)
            .unwrap_or_else(|error| panic!("{probe} view receipt: {error}"));
        runtime.take_notices();
        let projection = graph
            .compositor_snapshot(tab)
            .expect("hostile private graph snapshot")
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
        let dimensions = host
            .target03_dimensions(&projection, &catalogue, &admission, event + 1)
            .expect("hostile private dimensions");
        assert_eq!(dimensions.len(), 1);
        let callback_result = runtime.handle_events(dimensions, &catalogue);
        if runtime_rejects {
            let error = callback_result.expect_err("malformed callback import must reject");
            assert!(error.starts_with("ErrorCode::InvalidArgument:"), "{error}");
            assert!(runtime.take_requests().is_empty());
        } else {
            callback_result.expect("host receives the structurally admissible hostile update");
            runtime.take_notices();
            let mut cells = runtime.take_requests();
            assert_eq!(cells.len(), 1);
            let cell_token = cells[0].token;
            let graph_before = graph.clone();
            let host_before = host.clone();
            let error = host
                .process_target03(&mut graph, &catalogue, &admission, cells.remove(0))
                .expect_err("hostile private cells must reject atomically");
            assert!(matches!(
                error.code,
                wire::ErrorCode::Conflict | wire::ErrorCode::InvalidArgument
            ));
            assert_eq!(graph, graph_before);
            assert_eq!(host, host_before);
            assert!(
                runtime
                    .handle_events(
                        vec![callback(
                            event + 2,
                            cell_token,
                            Err(wire::Failure {
                                code: error.code,
                                message: error.message,
                                retryable: false,
                            }),
                        )],
                        &catalogue,
                    )
                    .is_err()
            );
        }
        assert!(runtime.take_notices().is_empty());
        assert_eq!(host.private_snapshots(&graph).len(), 2);
    }
    assert_eq!(host.snapshots().len(), 2);

    runtime
        .execute(
            invocation(
                90,
                "openprivate",
                &graph,
                crate::ui::window::ContentId(private_clone.content),
                crate::ui::window::ViewId(private_clone.view),
                crate::ui::window::WindowId(private_clone.window),
            ),
            &catalogue,
        )
        .expect("full private content request staging");
    let mut full_private = runtime.take_requests();
    let full_private_token = full_private[0].token;
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(&mut graph, &catalogue, &admission, full_private.remove(0))
        .expect_err("full private content admission must reject");
    assert_eq!(error.code, wire::ErrorCode::LimitExceeded);
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    46,
                    full_private_token,
                    Err(wire::Failure {
                        code: error.code,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue,
            )
            .is_err()
    );
    assert!(runtime.take_requests().is_empty());

    runtime
        .execute(
            invocation(
                47,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("delayed guest clone callback");
    let mut delayed = runtime.take_requests();
    assert_eq!(delayed.len(), 1);
    let delayed_token = delayed[0].token;
    let graph_before = graph.clone();
    let host_before = host.clone();
    graph
        .focus(editor_window)
        .expect("intervening graph change");
    let changed = graph.clone();
    let error = host
        .process_target03(&mut graph, &catalogue, &admission, delayed.remove(0))
        .expect_err("stale delayed callback must not publish");
    assert_eq!(error.code, wire::ErrorCode::StaleSource);
    assert!(error.message.contains("stale"));
    assert_eq!(host, host_before);
    assert_ne!(graph, graph_before);
    assert_eq!(graph, changed);
    let failure = wire::Failure {
        code: wire::ErrorCode::StaleSource,
        message: error.message,
        retryable: false,
    };
    assert!(
        runtime
            .handle_events(vec![callback(4, delayed_token, Err(failure))], &catalogue)
            .unwrap_err()
            .contains("stale")
    );
    assert!(
        runtime
            .handle_events(vec![callback(5, delayed_token, Ok(clone))], &catalogue)
            .unwrap_err()
            .contains("stale or duplicated")
    );
    assert!(runtime.take_notices().is_empty());

    for (offset, command) in [
        "open",
        "clone",
        "openprivate",
        "cloneprivate",
        "move",
        "tile",
        "dock",
        "fullscreen",
        "hide",
        "focus",
        "close",
    ]
    .into_iter()
    .enumerate()
    {
        let id = 400 + u64::try_from(offset).expect("operation ID");
        runtime
            .execute(
                invocation(
                    id,
                    command,
                    &graph,
                    crate::ui::window::ContentId(original.content),
                    crate::ui::window::ViewId(original.view),
                    crate::ui::window::WindowId(original.window),
                ),
                &catalogue,
            )
            .unwrap_or_else(|error| panic!("actual Component {command} staging: {error}"));
        let mut request = runtime.take_requests();
        assert_eq!(request.len(), 1, "one staged {command} request");
        let token = request[0].token;
        let mut stale_graph = graph.clone();
        let mut stale_host = host.clone();
        stale_graph
            .focus(crate::ui::window::WindowId(original.window))
            .expect("intervening focus changes graph revision");
        let changed_graph = stale_graph.clone();
        let unchanged_host = stale_host.clone();
        let error = match stale_host.process_target03(
            &mut stale_graph,
            &catalogue,
            &admission,
            request.remove(0),
        ) {
            Ok(receipt) => panic!("stale {command} published {receipt:?}"),
            Err(error) => error,
        };
        assert_eq!(error.code, wire::ErrorCode::StaleSource, "{command}");
        assert_eq!(stale_graph, changed_graph, "{command} graph changed");
        assert_eq!(stale_host, unchanged_host, "{command} host changed");
        let callback_error = runtime
            .handle_events(
                vec![callback(
                    id,
                    token,
                    Err(wire::Failure {
                        code: error.code,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue,
            )
            .expect_err("guest receives one terminal stale result");
        assert!(callback_error.starts_with("ErrorCode::StaleSource:"));
        assert!(runtime.take_requests().is_empty());
        assert!(runtime.take_notices().is_empty());
    }

    runtime
        .execute(
            invocation(
                48,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("closed-source clone staging");
    let mut closed_source = runtime.take_requests();
    let closed_source_token = closed_source[0].token;
    let mut closed_graph = graph.clone();
    let mut closed_host = host.clone();
    closed_graph
        .close_window(crate::ui::window::WindowId(original.window))
        .and_then(|_| closed_graph.remove_view(crate::ui::window::ViewId(original.view)))
        .expect("close exact staged source view");
    let closed_before = closed_graph.clone();
    let closed_host_before = closed_host.clone();
    let error = closed_host
        .process_target03(
            &mut closed_graph,
            &catalogue,
            &admission,
            closed_source.remove(0),
        )
        .expect_err("closed source must reject before candidate publication");
    assert_eq!(error.code, wire::ErrorCode::Closed);
    assert_eq!(closed_graph, closed_before);
    assert_eq!(closed_host, closed_host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    48,
                    closed_source_token,
                    Err(wire::Failure {
                        code: error.code,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue,
            )
            .is_err()
    );

    runtime
        .execute(
            invocation(
                49,
                "callbackopen",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect("result-only authority probe staging");
    let mut callback_open = runtime.take_requests();
    assert_eq!(callback_open.len(), 1);
    let callback_open_token = callback_open[0].token;
    let callback_open_receipt = host
        .process_target03(&mut graph, &catalogue, &admission, callback_open.remove(0))
        .expect("authority probe publication");
    runtime
        .handle_events(
            vec![callback(41, callback_open_token, Ok(callback_open_receipt))],
            &catalogue,
        )
        .expect("result-only forbidden open was observed by real guest");
    let notices = runtime.take_notices();
    assert!(
        notices
            .iter()
            .any(|notice| notice == "result-only-open-refused")
    );
    assert!(runtime.take_requests().is_empty());
    let callback_handle = host
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.window.0 == callback_open_receipt.window)
        .expect("authority probe host view")
        .handle;
    let close_revision = graph.revision().expect("close revision");
    let close = process_fixture(
        &mut host,
        &mut graph,
        &admission.index,
        PluginUiRequest {
            token: 999,
            kind: PluginUiRequestKind::CloseWindow {
                handle: callback_handle,
                expected_graph_revision: close_revision,
            },
        },
    );
    assert!(close.error.is_none(), "{:?}", close.error);
    let retired_window_ceiling = host
        .snapshots()
        .into_iter()
        .map(|snapshot| snapshot.window.0)
        .chain(
            host.private_snapshots(&graph)
                .into_iter()
                .map(|snapshot| snapshot.window.0),
        )
        .max()
        .expect("retiring target window ceiling");

    let mut wrong_tab_graph = graph.clone();
    let wrong_tab = wrong_tab_graph
        .create_workspace_tab("wrong-tab", 1)
        .expect("hostile target tab");
    let mut wrong_tab_invocation = invocation(
        1,
        "clone",
        &wrong_tab_graph,
        crate::ui::window::ContentId(original.content),
        crate::ui::window::ViewId(original.view),
        crate::ui::window::WindowId(original.window),
    );
    wrong_tab_invocation
        .ui
        .as_mut()
        .expect("wrong-tab UI source")
        .tab = wrong_tab.0;
    let mut wrong_tab_runtime =
        Runtime::load(core, &admission).expect("wrong-tab target Component");
    wrong_tab_runtime
        .execute(wrong_tab_invocation, &catalogue)
        .expect("wrong-tab guest clone staging");
    let mut wrong_tab_request = wrong_tab_runtime.take_requests();
    assert_eq!(wrong_tab_request.len(), 1);
    let wrong_tab_token = wrong_tab_request[0].token;
    let mut wrong_tab_host = host.clone();
    let wrong_tab_graph_before = wrong_tab_graph.clone();
    let wrong_tab_host_before = wrong_tab_host.clone();
    let error = wrong_tab_host
        .process_target03(
            &mut wrong_tab_graph,
            &catalogue,
            &admission,
            wrong_tab_request.remove(0),
        )
        .expect_err("cross-tab source mismatch must reject");
    assert_eq!(error.code, wire::ErrorCode::StaleSource);
    assert!(error.message.contains("tab source is stale"));
    assert_eq!(wrong_tab_graph, wrong_tab_graph_before);
    assert_eq!(wrong_tab_host, wrong_tab_host_before);
    let callback_error = wrong_tab_runtime
        .handle_events(
            vec![callback(
                1,
                wrong_tab_token,
                Err(wire::Failure {
                    code: error.code,
                    message: error.message,
                    retryable: false,
                }),
            )],
            &catalogue,
        )
        .expect_err("wrong-tab clone gets one typed terminal result");
    assert!(
        callback_error.starts_with("ErrorCode::StaleSource:"),
        "{callback_error}"
    );
    wrong_tab_runtime
        .deactivate()
        .expect("wrong-tab load retirement");

    let mut ceiling_graph = graph.clone();
    let mut ceiling_host = host.clone();
    while ceiling_graph
        .windows()
        .filter(|window| {
            window.tab == tab && window.placement != crate::ui::window::WindowPlacement::Tiled
        })
        .count()
        < 128
    {
        let view = ceiling_graph
            .create_view(editor_content, "builtin.test", 1)
            .expect("ceiling filler view");
        ceiling_graph
            .open_window(
                tab,
                view,
                WindowPlacement::Floating(WindowRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                }),
                true,
                "builtin.test",
                1,
            )
            .expect("window below non-tiled ceiling");
    }
    assert_eq!(
        ceiling_graph
            .windows()
            .filter(|window| {
                window.tab == tab && window.placement != crate::ui::window::WindowPlacement::Tiled
            })
            .count(),
        128
    );
    ceiling_graph.validate().expect("valid graph at ceiling");
    let mut ceiling_runtime =
        Runtime::load(core, &admission).expect("graph ceiling target Component");
    ceiling_runtime
        .execute(
            invocation(
                1,
                "clone",
                &ceiling_graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("valid guest clone at graph ceiling");
    let mut ceiling_request = ceiling_runtime.take_requests();
    assert_eq!(ceiling_request.len(), 1);
    let ceiling_token = ceiling_request[0].token;
    let ceiling_graph_before = ceiling_graph.clone();
    let ceiling_host_before = ceiling_host.clone();
    let error = ceiling_host
        .process_target03(
            &mut ceiling_graph,
            &catalogue,
            &admission,
            ceiling_request.remove(0),
        )
        .expect_err("universal non-tiled ceiling rejects actual Component clone");
    assert_eq!(error.code, wire::ErrorCode::LimitExceeded);
    assert!(error.message.contains("non-tiled window ceiling"));
    assert_eq!(ceiling_graph, ceiling_graph_before);
    assert_eq!(ceiling_host, ceiling_host_before);
    let callback_error = ceiling_runtime
        .handle_events(
            vec![callback(
                1,
                ceiling_token,
                Err(wire::Failure {
                    code: error.code,
                    message: error.message,
                    retryable: false,
                }),
            )],
            &catalogue,
        )
        .expect_err("graph-ceiling clone gets one typed terminal result");
    assert!(
        callback_error.starts_with("ErrorCode::LimitExceeded:"),
        "{callback_error}"
    );
    assert!(ceiling_runtime.take_requests().is_empty());
    assert!(ceiling_runtime.take_notices().is_empty());
    ceiling_runtime
        .deactivate()
        .expect("graph ceiling load retirement");

    assert!(!remove_fixture(&mut host, &mut graph, "demo", 8).unwrap());
    assert_eq!(host.snapshots().len(), 2);

    let mut saturation = Runtime::load(core, &admission).expect("saturation Component");
    for id in 100..164 {
        saturation
            .execute(
                invocation(
                    id,
                    "open",
                    &graph,
                    editor_content,
                    editor_view,
                    editor_window,
                ),
                &catalogue,
            )
            .expect("reserved target request");
    }
    assert_eq!(saturation.take_requests().len(), 64);
    let error = saturation
        .execute(
            invocation(
                164,
                "open",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect_err("pending terminal capacity stays reserved after staging drain");
    assert!(error.starts_with("ErrorCode::QueueFull:"), "{error}");
    saturation
        .deactivate()
        .expect("saturated target retirement");
    assert!(saturation.take_requests().is_empty());
    assert!(
        saturation
            .execute(
                invocation(
                    165,
                    "open",
                    &graph,
                    editor_content,
                    editor_view,
                    editor_window,
                ),
                &catalogue
            )
            .unwrap_err()
            .contains("retired")
    );

    runtime
        .execute(
            invocation(
                166,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("catalogue-bound clone staging");
    let mut catalogue_stale = runtime.take_requests();
    let catalogue_stale_token = catalogue_stale[0].token;
    assert!(catalogue.remove_generation("demo", 9));
    catalogue
        .install(admission.index.clone())
        .expect("same generation reinstallation");
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(
            &mut graph,
            &catalogue,
            &admission,
            catalogue_stale.remove(0),
        )
        .expect_err("catalogue identity changed after staging");
    assert_eq!(error.code, wire::ErrorCode::StaleSource);
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .handle_events(
                vec![callback(
                    6,
                    catalogue_stale_token,
                    Err(wire::Failure {
                        code: error.code,
                        message: error.message,
                        retryable: false,
                    }),
                )],
                &catalogue,
            )
            .is_err()
    );

    let other_index = index("other", 77);
    catalogue
        .install(other_index.clone())
        .expect("unrelated catalogue generation");
    let other_revision = graph.revision().expect("unrelated open revision");
    let other_event = process_fixture(
        &mut host,
        &mut graph,
        &other_index,
        open_request_for("other", other_revision, tree(1, "unrelated generation")),
    );
    assert!(other_event.error.is_none(), "{:?}", other_event.error);
    let other_snapshot = host
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.handle == other_event.handle.expect("unrelated handle"))
        .expect("unrelated host snapshot");
    let other_window = graph
        .window(other_snapshot.window)
        .expect("unrelated graph window")
        .clone();
    let other_content = graph
        .content_for_window(other_snapshot.window)
        .expect("unrelated graph content")
        .clone();

    let closed_private_clone = apply_component_window_operation!(165, "close", private_clone);
    assert_eq!(closed_private_clone.content, private_original.content);
    assert!(
        graph
            .window(crate::ui::window::WindowId(private_clone.window))
            .is_none()
    );
    assert!(
        graph
            .content(crate::ui::window::ContentId(private_original.content))
            .is_some(),
        "shared private content survives until its last view closes"
    );
    let closed_private_original = apply_component_window_operation!(166, "close", private_focused);
    assert_eq!(closed_private_original.content, private_original.content);
    assert!(
        graph
            .content(crate::ui::window::ContentId(private_original.content))
            .is_none()
    );
    assert!(host.private_snapshots(&graph).is_empty());

    runtime
        .execute(
            invocation(
                167,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("retirement-bound clone staging");
    runtime
        .execute(
            invocation(
                168,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("replacement-race clone staging");
    let mut retired = runtime.take_requests();
    assert_eq!(retired.len(), 2);
    runtime.deactivate().expect("bounded target deactivation");
    let graph_before = graph.clone();
    let host_before = host.clone();
    let error = host
        .process_target03(&mut graph, &catalogue, &admission, retired.remove(0))
        .expect_err("taken request cannot outlive its runtime");
    assert_eq!(error.code, wire::ErrorCode::StaleSource);
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);
    assert!(
        runtime
            .execute(
                invocation(
                    169,
                    "open",
                    &graph,
                    editor_content,
                    editor_view,
                    editor_window,
                ),
                &catalogue,
            )
            .unwrap_err()
            .contains("retired")
    );
    assert!(runtime.take_requests().is_empty());

    let old_replacement_request = retired.remove(0);
    let mut dropped = Runtime::load(core, &admission).expect("drop retirement Component");
    dropped
        .execute(
            invocation(
                170,
                "clone",
                &graph,
                crate::ui::window::ContentId(original.content),
                crate::ui::window::ViewId(original.view),
                crate::ui::window::WindowId(original.window),
            ),
            &catalogue,
        )
        .expect("drop-bound clone staging");
    let mut dropped_request = dropped.take_requests();
    drop(dropped);
    let graph_before = graph.clone();
    let host_before = host.clone();
    assert_eq!(
        host.process_target03(
            &mut graph,
            &catalogue,
            &admission,
            dropped_request.remove(0),
        )
        .expect_err("taken request cannot outlive a dropped runtime")
        .code,
        wire::ErrorCode::StaleSource
    );
    assert_eq!(graph, graph_before);
    assert_eq!(host, host_before);

    assert!(remove_fixture(&mut host, &mut graph, "demo", 9).unwrap());
    assert!(catalogue.remove_generation("demo", 9));
    assert_eq!(
        host.snapshots().as_slice(),
        std::slice::from_ref(&other_snapshot)
    );
    assert_eq!(graph.window(other_snapshot.window), Some(&other_window));
    assert_eq!(
        graph.content_for_window(other_snapshot.window),
        Some(&other_content)
    );
    assert!(catalogue.get("other").is_some_and(|index| {
        index.owner == other_index.owner
            && index.generation == other_index.generation
            && index.checksum == other_index.checksum
    }));
    assert!(
        graph
            .content_snapshots()
            .iter()
            .all(|content| content.id != crate::ui::window::ContentId(original.content))
    );

    let replacement = TargetAdmission::parse(manifest_source, 10).expect("replacement admission");
    catalogue
        .install(replacement.index.clone())
        .expect("replacement catalogue generation");
    let mut replacement_runtime =
        Runtime::load(core, &replacement).expect("replacement target Component");
    replacement_runtime
        .execute(
            invocation(
                171,
                "open",
                &graph,
                editor_content,
                editor_view,
                editor_window,
            ),
            &catalogue,
        )
        .expect("replacement guest remains usable");
    let mut replacement_requests = replacement_runtime.take_requests();
    let replacement_receipt = host
        .process_target03(
            &mut graph,
            &catalogue,
            &replacement,
            replacement_requests.remove(0),
        )
        .expect("replacement generation publication");
    assert_ne!(replacement_receipt.content, original.content);
    assert_ne!(replacement_receipt.view, original.view);
    assert!(replacement_receipt.window > retired_window_ceiling);
    let replacement_graph = graph.clone();
    let replacement_host = host.clone();
    assert_eq!(
        host.process_target03(
            &mut graph,
            &catalogue,
            &replacement,
            old_replacement_request,
        )
        .expect_err("old staged request cannot enter replacement generation")
        .code,
        wire::ErrorCode::StaleSource
    );
    assert_eq!(graph, replacement_graph);
    assert_eq!(host, replacement_host);
    replacement_runtime
        .deactivate()
        .expect("replacement retirement");
    assert!(remove_fixture(&mut host, &mut graph, "demo", 10).unwrap());
    assert!(catalogue.remove_generation("demo", 10));
    assert_eq!(host.snapshots(), [other_snapshot]);
    assert_eq!(graph.window(other_window.id), Some(&other_window));
    assert_eq!(graph.content(other_content.id), Some(&other_content));
    assert!(catalogue.get("other").is_some_and(|index| {
        index.owner == other_index.owner
            && index.generation == other_index.generation
            && index.checksum == other_index.checksum
    }));
    assert!(remove_fixture(&mut host, &mut graph, "other", 77).unwrap());
    assert!(catalogue.remove_generation("other", 77));
}

#[test]
fn generation_owned_retained_window_lifecycle_is_revision_checked() {
    let mut graph = WindowGraph::default();
    graph.create_workspace_tab("builtin.test", 1).expect("tab");
    let mut host = PluginWindowHost::default();
    let demo_index = index("demo", 7);
    let open_revision = graph.revision().expect("revision");
    let event = process_fixture(
        &mut host,
        &mut graph,
        &demo_index,
        open_request(open_revision, tree(1, "Hello")),
    );
    assert!(event.error.is_none(), "{:?}", event.error);
    let handle = event.handle.expect("handle");
    assert_eq!(host.snapshots()[0].rows[0].text, "Hello");
    let content = graph
        .content_snapshots()
        .into_iter()
        .next()
        .expect("plugin content");
    assert_eq!(content.lifecycle, ContentLifecycle::Ready);
    let capabilities = content.capabilities;
    assert!(capabilities.focusable);
    assert!(capabilities.selectable);
    assert!(capabilities.dock);
    assert!(capabilities.float);
    assert!(capabilities.hide);
    assert!(capabilities.close_view);
    assert!(!capabilities.editable);
    assert!(!capabilities.clone_view);
    assert_eq!(
        graph.require_clone_view(content.id),
        Err(crate::ui::window::WindowError::Unsupported("clone-view"))
    );
    let before_clone = graph.clone();
    assert_eq!(
        graph.create_view(content.id, "plugin.demo", 7),
        Err(crate::ui::window::WindowError::Unsupported("clone-view"))
    );
    assert_eq!(graph, before_clone);
    assert!(!capabilities.tile);
    assert!(!capabilities.fullscreen);
    assert!(!capabilities.stop_resource);
    assert!(!capabilities.persist_descriptor);
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
    let duplicate = process_fixture(
        &mut host,
        &mut graph,
        &demo_index,
        open_request(duplicate_revision, tree(1, "Duplicate")),
    );
    assert_eq!(
        duplicate.error.as_ref().map(|error| error.code),
        Some(PluginUiErrorCode::InstanceCeiling)
    );
    assert_eq!(graph, graph_after_open);

    let stale = process_fixture(
        &mut host,
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

    let replaced = process_fixture(
        &mut host,
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
    let mut catalog = crate::extensions::PluginContributionCatalog::default();
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
    let repaired = process_fixture(
        &mut host,
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
                    nodes: vec![crate::extensions::PluginUiNode {
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
        process_fixture(
            &mut host,
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
    let focus_hidden = process_fixture(
        &mut host,
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
    let foreign_event = process_fixture(
        &mut host,
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
                    nodes: vec![crate::extensions::PluginUiNode {
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

    assert!(remove_fixture(&mut host, &mut graph, "demo", 7).expect("cleanup"));
    assert!(host.snapshots().is_empty());
    assert_eq!(graph.counts(), (0, 0, 0, 1));

    let replacement = index("demo", 8);
    let replacement_revision = graph.revision().expect("replacement revision");
    let replacement_event = process_fixture(
        &mut host,
        &mut graph,
        &replacement,
        open_request(replacement_revision, tree(1, "Replacement")),
    );
    assert!(
        replacement_event.error.is_none(),
        "{:?}",
        replacement_event.error
    );
    assert!(!remove_fixture(&mut host, &mut graph, "demo", 7).expect("stale cleanup"));
    assert_eq!(host.snapshots()[0].rows[0].text, "Replacement");
}

#[test]
fn plugin_window_operations_reject_cross_tab_scope_without_mutation() {
    let mut graph = WindowGraph::default();
    graph.create_workspace_tab("builtin.test", 1).expect("tab");
    let mut host = PluginWindowHost::default();
    let demo = index("demo", 3);
    let revision = graph.revision().expect("revision");
    let opened = process_fixture(
        &mut host,
        &mut graph,
        &demo,
        open_request(revision, tree(1, "one")),
    );
    let handle = opened.handle.expect("handle");
    let second = graph
        .create_workspace_tab("builtin.test", 1)
        .expect("second tab");
    graph.set_active_tab(second).expect("activate second");
    let before = graph.clone();
    let event = process_fixture(
        &mut host,
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
    let undeclared = process_fixture(
        &mut host,
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
    let invalid = process_fixture(
        &mut host,
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
    let stale = process_fixture(
        &mut host,
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
    shuffled.nodes.push(crate::extensions::PluginUiNode {
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
    unreachable.nodes.push(crate::extensions::PluginUiNode {
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

// Owned initial graph fixtures exercise the same scoped proposal/commit boundary.
fn process_fixture(
    host: &mut PluginWindowHost,
    graph: &mut WindowGraph,
    index: &PluginContributionIndex,
    request: PluginUiRequest,
) -> PluginUiEvent {
    let mut windows =
        crate::editor::session::windows::WindowSession::from_test_graph(graph.clone());
    let mut draft = windows.begin_plugin_fixture(&index.owner, index.generation);
    let mut candidate = host.clone();
    let event = candidate.process(&mut draft, index, request);
    if event.error.is_none() {
        windows
            .commit_plugin_fixture(draft)
            .expect("checked plugin commit");
        *host = candidate;
        *graph = windows.graph().clone();
    }
    event
}
fn remove_fixture(
    host: &mut PluginWindowHost,
    graph: &mut WindowGraph,
    plugin: &str,
    load: u64,
) -> Result<bool, String> {
    let mut windows =
        crate::editor::session::windows::WindowSession::from_test_graph(graph.clone());
    windows.remove_plugin_generation(plugin, load)?;
    *graph = windows.graph().clone();
    Ok(host.remove_generation(plugin, load))
}

#[test]
fn retained_ui_rejects_shared_children_and_bounds_traversals() {
    let mut tree = tree(1, "hello");
    tree.nodes[0].children.push(3);
    assert_eq!(
        validate_tree(&index("demo", 1), &tree, None)
            .unwrap_err()
            .code,
        PluginUiErrorCode::InvalidTree
    );
    assert_eq!(tree_rows(&tree, None).len(), 2);
    assert_eq!(focusable_nodes(&tree), vec![3]);
    tree.nodes[0].children.pop();
    tree.nodes[1].children.push(3);
    assert!(validate_tree(&index("demo", 1), &tree, None).is_err());
    tree.nodes[2].children.push(1);
    assert_eq!(tree_rows(&tree, None).len(), 2);
}

#[test]
fn retained_keyboard_focus_scrolls_with_the_projected_height() {
    use crate::ui::window::WindowRect;
    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("builtin.test", 1).unwrap();
    let mut host = PluginWindowHost::default();
    let index = index("demo", 1);
    let mut tree = tree(1, "");
    tree.nodes.truncate(1);
    tree.nodes[0].children = (2..=11).collect();
    for id in 2..=11 {
        tree.nodes.push(crate::extensions::PluginUiNode {
            id,
            kind: PluginUiNodeKind::Button,
            text: format!("Button {id}"),
            style: PluginUiStyle::Default,
            action: Some("plugin.demo.open".into()),
            children: Vec::new(),
        });
    }
    let mut request = open_request(graph.revision().unwrap(), tree);
    if let PluginUiRequestKind::OpenWindow { placement, .. } = &mut request.kind {
        *placement = PluginRequestedPlacement::Floating {
            x: 0,
            y: 0,
            width: 20,
            height: 3,
        };
    }
    assert!(
        process_fixture(&mut host, &mut graph, &index, request)
            .error
            .is_none()
    );
    let projection = graph.compositor_snapshot(tab).unwrap().project(
        WindowRect {
            x: 0,
            y: 0,
            width: 30,
            height: 10,
        },
        1,
        1,
    );
    host.synchronize_private_geometry(Some(&projection));
    let height = host.instances.values().next().unwrap().visible_rows;
    assert!(height > 0);
    let mut catalog = crate::extensions::PluginContributionCatalog::default();
    catalog.install(index).unwrap();
    host.interact_focused(&graph, &catalog, PluginUiInteraction::Focus(4))
        .unwrap();
    let instance = host.instances.values().next().unwrap();
    assert_eq!(instance.focused_node, Some(6));
    assert!(instance.scroll <= 4 && instance.scroll + height > 4);
    assert!(instance.scroll > 0);
    host.interact_focused(&graph, &catalog, PluginUiInteraction::Scroll(5))
        .unwrap();
    let manual_scroll = host.instances.values().next().unwrap().scroll;
    host.synchronize_private_geometry(Some(&projection));
    assert_eq!(
        host.instances.values().next().unwrap().scroll,
        manual_scroll
    );
    host.interact_focused(&graph, &catalog, PluginUiInteraction::Focus(-4))
        .unwrap();
    assert_eq!(host.instances.values().next().unwrap().scroll, 0);
}
