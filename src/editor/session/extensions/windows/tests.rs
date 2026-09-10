use super::*;
use crate::extensions::{
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
        let requests = providers.requests("demo", crate::extensions::PluginLoadId(44));
        last_envelope = requests.first().cloned();
        let execution = crate::extensions::execute_component(
            crate::extensions::PluginExecutionRequest {
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
        crate::extensions::PluginProviderErrorCode::Cancelled
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

    let flood = crate::extensions::execute_component(
        crate::extensions::PluginExecutionRequest {
            plugin: ("demo").into(),
            command: ("flood").into(),
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
    .expect("bounded queue execution");
    assert_eq!(flood.ui_requests.len(), 64);
    assert!(flood.result.contains("QueueFull"));
    assert!(!remove_fixture(&mut host, &mut graph, "demo", 43).expect("stale cleanup"));
    assert!(remove_fixture(&mut host, &mut graph, "demo", 44).is_ok());
    assert_eq!(opaque_handle, Some(1));
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
    let capabilities = graph.content_snapshots()[0].capabilities;
    assert!(capabilities.focusable);
    assert!(capabilities.selectable);
    assert!(capabilities.dock);
    assert!(capabilities.float);
    assert!(capabilities.hide);
    assert!(capabilities.close_view);
    assert!(!capabilities.editable);
    assert!(!capabilities.clone_view);
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
