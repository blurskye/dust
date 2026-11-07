//! Host tests for sandboxed component plugins and host requests.

use crate::plugins::host::QueuedRequest;
use crate::plugins::host::Request;
use crate::plugins::host::WorkerEvent;
use crate::plugins::manifest::MAXIMUM_STORAGE_TOTAL_BYTES;
use crate::plugins::*;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::time::Duration;
fn manager(sender: SyncSender<QueuedRequest>, receiver: Receiver<WorkerEvent>) -> PluginHost {
    PluginHost {
        owner: std::sync::Arc::new(()),
        queued: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        sender,
        receiver,
        statuses: BTreeMap::new(),
        loads: BTreeMap::new(),
        next_load: 1,
        pending_disables: VecDeque::new(),
        maximum_loads: 4,
        stopped: false,
    }
}
fn manifest() -> PluginManifest {
    PluginManifest {
        id: "demo".into(),
        version: "1.0.0".into(),
        component_sha256: "0".repeat(64),
        capabilities: BTreeSet::new(),
        commands: [(
            "run".into(),
            PluginCommandDeclaration {
                title: "Run".into(),
                description: "Run demo".into(),
                arguments: BTreeMap::new(),
                context: PluginCommandContext::Buffer,
                activation: PluginActivationBehavior::ActiveOnly,
            },
        )]
        .into_iter()
        .collect(),
        contributions: PluginContributions {
            schema_version: 1,
            input_contexts: vec![PluginInputContextContribution {
                id: "plugin.demo.chat".into(),
                title: "Chat".into(),
                parent_surface: Some("panel".into()),
            }],
            keymaps: vec![PluginKeymapContribution {
                id: "plugin.demo.keymap.open-chat".into(),
                context: "plugin.demo.chat".into(),
                mode: String::new(),
                keys: "<leader> a".into(),
                command: "plugin.demo.run".into(),
                arguments: BTreeMap::new(),
            }],
            key_groups: vec![PluginKeyGroupContribution {
                id: "plugin.demo.group.chat".into(),
                context: "plugin.demo.chat".into(),
                mode: String::new(),
                prefix: "<leader>".into(),
                label: "Chat".into(),
                icon: None,
                order: 1,
            }],
            content_types: vec![PluginContentTypeContribution {
                id: "plugin.demo.chat-panel".into(),
                title: "Demo Chat".into(),
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
                context: "plugin.demo.chat".into(),
                command: "plugin.demo.run".into(),
                private_cells: None,
                provenance: None,
            }],
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        },
    }
}

fn component_host() -> ComponentHostState {
    ComponentHostState {
        editor_requests: Default::default(),
        runtime_epoch: 0,
        limits: crate::plugins::component::ComponentLimits::new(
            wasmtime::StoreLimitsBuilder::new().build(),
            64 * 1024 * 1024,
        ),
        capabilities: BTreeSet::new(),
        declared_content_types: BTreeSet::new(),
        snapshot: None,
        proposals: Vec::new(),
        messages: Vec::new(),
        config: BTreeMap::new(),
        ui_revision: None,
        confirmation_available: true,
        confirmation_result_only: false,
        ui_requests: Vec::new(),
        ui_events: Vec::new(),
        next_ui_token: 1,
        pending_ui_tokens: BTreeSet::new(),
        confirmation_requests: Vec::new(),
        confirmation_results: Vec::new(),
        next_confirmation_token: 1,
        host_events: Vec::new(),
        delivered_host_event_ids: BTreeSet::new(),
        host_event_acknowledgements: Vec::new(),
        execution_available: true,
        pending_storage: BTreeMap::new(),
        storage_policy: None,
        storage_requests: Vec::new(),
        storage_results: Vec::new(),
        next_storage_token: 1,
        provider_requests: Vec::new(),
        delivered_provider_requests: BTreeSet::new(),
        provider_chunks: Vec::new(),
        virtual_document_requests: Vec::new(),
        delivered_virtual_document_requests: BTreeSet::new(),
        virtual_document_proposals: Vec::new(),
    }
}

#[test]
fn completed_component_execution_cancels_its_epoch_timer() {
    use sha2::Digest;
    use sha2::Sha256;
    let root = tempfile::tempdir().unwrap();
    let core = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/retained-guest/retained-guest.core.wasm"
    ));
    let bytes = wit_component::ComponentEncoder::default()
        .module(core)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let object = root.path().join("fixture.wasm");
    fs::write(&object, &bytes).unwrap();
    let mut plugin = manifest();
    plugin.component_sha256 = format!("{:x}", Sha256::digest(&bytes));
    plugin.contributions = Default::default();
    plugin.capabilities.insert(PluginCapability::ShowMessage);
    let declaration = plugin.commands["run"].clone();
    for command in [
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
        "confirmationprobe",
    ] {
        plugin.commands.insert(command.into(), declaration.clone());
    }
    let limits = PluginLimits {
        epoch_deadline_ticks: 100,
        ..Default::default()
    };
    let mut outcome =
        crate::plugins::index::load_component(root.path(), &object, plugin, &limits).unwrap();
    let mut invocation = empty_execution_request();
    invocation.command = "confirmationprobe".into();
    crate::plugins::index::execute_component(invocation, &mut outcome.runtime).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let runtime = &mut outcome.runtime;
    assert!(
        runtime
            .bindings
            .call_execute_command(&mut runtime.store, "confirmationprobe", &Default::default())
            .unwrap()
            .is_ok()
    );
}

#[test]
fn component_host_queues_declared_ui_requests_without_graph_authority() {
    use component_api::dust::plugin::host::Host;
    use component_api::dust::plugin::host::OpenWindowRequest;
    use component_api::dust::plugin::host::UiNode;
    use component_api::dust::plugin::host::UiNodeKind;
    use component_api::dust::plugin::host::UiRequest;
    use component_api::dust::plugin::host::UiStyle;
    use component_api::dust::plugin::host::UiTree;
    use component_api::dust::plugin::host::WindowPlacement;
    let mut host = component_host();
    host.declared_content_types
        .insert("plugin.demo.chat-panel".into());
    let request = |content_type: &str| {
        UiRequest::OpenWindow(OpenWindowRequest {
            expected_graph_revision: 4,
            content_type: content_type.into(),
            title: "Demo".into(),
            placement: WindowPlacement::Floating,
            x: 2,
            y: 3,
            width: 30,
            height: 8,
            size: 1,
            tree: Some(UiTree {
                revision: 1,
                root: 1,
                nodes: vec![UiNode {
                    id: 1,
                    kind: UiNodeKind::Text,
                    text: "hello".into(),
                    style: UiStyle::Accent,
                    action: None,
                    children: Vec::new(),
                }],
            }),
        })
    };
    assert!(matches!(
        host.queue_ui_request(request("plugin.demo.chat-panel")),
        Ok(1)
    ));
    assert_eq!(host.ui_requests.len(), 1);
    assert!(
        host.queue_ui_request(request("plugin.other.stolen"))
            .is_err()
    );
    assert_eq!(host.ui_requests.len(), 1);
    let mut oversized = request("plugin.demo.chat-panel");
    if let UiRequest::OpenWindow(open) = &mut oversized {
        open.tree.as_mut().unwrap().nodes[0].text = "x".repeat(4097);
    }
    assert!(host.queue_ui_request(oversized).is_err());
    let mut title = request("plugin.demo.chat-panel");
    if let UiRequest::OpenWindow(open) = &mut title {
        open.title = "t".repeat(4097);
    }
    assert!(host.queue_ui_request(title).is_err());
    let mut title = request("plugin.demo.chat-panel");
    if let UiRequest::OpenWindow(open) = &mut title {
        open.title = "t".repeat(4096);
    }
    assert!(host.queue_ui_request(title).is_ok());
    for _ in 0..64 {
        let mut large = request("plugin.demo.chat-panel");
        if let UiRequest::OpenWindow(open) = &mut large {
            let mut node = open.tree.as_ref().unwrap().nodes[0].clone();
            node.text = "x".repeat(4096);
            open.tree.as_mut().unwrap().nodes = vec![node; 16];
        }
        if host.queue_ui_request(large).is_err() {
            break;
        }
    }
    assert!(host.ui_requests.len() < 64);
    host.capabilities
        .insert(PluginCapability::EventSubscriptions);
    host.delivered_host_event_ids = BTreeSet::from([1, 2]);
    for _ in 0..64 {
        host.acknowledge_host_events(vec![1]).unwrap();
    }
    host.acknowledge_host_events(vec![2]).unwrap();
    assert_eq!(host.host_event_acknowledgements, vec![1, 2]);
}

#[test]
fn component_confirmation_request_is_typed_bounded_and_single_per_execution() {
    use component_api::dust::plugin::host::ConfirmationAnswer;
    use component_api::dust::plugin::host::ConfirmationErrorCode;
    use component_api::dust::plugin::host::ConfirmationRequest;
    use component_api::dust::plugin::host::Host;
    let mut host = component_host();
    let request = || ConfirmationRequest {
        title: "Continue?".into(),
        message: "A bounded ordinary question".into(),
        default_answer: ConfirmationAnswer::No,
    };
    assert_eq!(host.queue_confirmation(request()).unwrap(), 1);
    assert_eq!(
        host.queue_confirmation(request()).unwrap_err().code,
        ConfirmationErrorCode::QueueFull
    );
    host.confirmation_requests.clear();
    host.confirmation_available = false;
    assert_eq!(
        host.queue_confirmation(request()).unwrap_err().code,
        ConfirmationErrorCode::QueueFull
    );
    host.confirmation_available = true;
    for invalid in [
        ConfirmationRequest {
            title: String::new(),
            ..request()
        },
        ConfirmationRequest {
            message: String::new(),
            ..request()
        },
        ConfirmationRequest {
            default_answer: ConfirmationAnswer::Cancelled,
            ..request()
        },
        ConfirmationRequest {
            title: "t".repeat(129),
            ..request()
        },
        ConfirmationRequest {
            message: "m".repeat(4_097),
            ..request()
        },
        ConfirmationRequest {
            title: "bad\0title".into(),
            ..request()
        },
        ConfirmationRequest {
            message: "bad\0message".into(),
            ..request()
        },
    ] {
        assert_eq!(
            host.queue_confirmation(invalid).unwrap_err().code,
            ConfirmationErrorCode::InvalidRequest
        );
        assert!(host.confirmation_requests.is_empty());
        assert_eq!(host.next_confirmation_token, 2);
    }
    for valid in [
        ConfirmationRequest {
            title: "t".repeat(128),
            ..request()
        },
        ConfirmationRequest {
            message: "m".repeat(4_096),
            ..request()
        },
    ] {
        assert!(host.queue_confirmation(valid).is_ok());
        assert_eq!(host.confirmation_requests.len(), 1);
        host.confirmation_requests.clear();
    }
}

#[test]
fn component_storage_queue_is_capability_key_and_count_bounded() {
    use component_api::dust::plugin::host::Host;
    use component_api::dust::plugin::host::StorageRequest;
    let policy = PluginStorageContribution {
        maximum_items: 32,
        maximum_value_bytes: 1_024,
        maximum_total_bytes: 16_384,
    };
    let mut host = component_host();
    host.storage_policy = Some(policy);
    host.capabilities.insert(PluginCapability::VirtualDocuments);
    assert_eq!(
        host.queue_storage_request(StorageRequest::Get("valid".into()))
            .unwrap_err()
            .code,
        component_api::dust::plugin::host::StorageErrorCode::Unavailable
    );
    assert!(host.storage_requests.is_empty());
    assert!(host.pending_storage.is_empty());
    assert_eq!(host.next_storage_token, 1);
    host.capabilities.insert(PluginCapability::PluginStorage);
    assert_eq!(
        host.queue_storage_request(StorageRequest::Get("valid".into()))
            .unwrap(),
        1
    );
    assert_eq!(host.storage_requests.len(), 1);
    assert_eq!(host.pending_storage.len(), 1);
    assert_eq!(
        host.queue_storage_request(StorageRequest::Get("bad/key".into()))
            .unwrap_err()
            .code,
        component_api::dust::plugin::host::StorageErrorCode::InvalidKey
    );
    for _ in 1..STORAGE_REQUEST_LIMIT {
        host.queue_storage_request(StorageRequest::Delete(
            component_api::dust::plugin::host::StorageDeleteRequest {
                key: "valid".into(),
                expected_revision: 1,
            },
        ))
        .expect("bounded storage request");
    }
    assert_eq!(
        host.queue_storage_request(StorageRequest::Get("valid".into()))
            .unwrap_err()
            .code,
        component_api::dust::plugin::host::StorageErrorCode::QueueFull
    );
    assert_eq!(host.storage_requests.len(), STORAGE_REQUEST_LIMIT);
    use component_api::dust::plugin::host::VirtualDocumentProposal;
    use component_api::dust::plugin::host::VirtualDocumentProposalKind;
    assert_eq!(
        host.queue_virtual_document_proposal(VirtualDocumentProposal {
            request: 9,
            document: 4,
            cancellation_id: 1,
            kind: VirtualDocumentProposalKind::CloseAcknowledged
        })
        .unwrap_err()
        .code,
        component_api::dust::plugin::host::VirtualDocumentErrorCode::UnknownRequest
    );
    assert!(host.virtual_document_proposals.is_empty());
}

#[test]
fn component_output_queues_enforce_cumulative_byte_budgets() {
    use component_api::dust::plugin::host::Host;
    use component_api::dust::plugin::host::PickerProviderChunk;
    use component_api::dust::plugin::host::PickerProviderItem;
    use component_api::dust::plugin::host::ProviderErrorCode;
    use component_api::dust::plugin::host::ProviderTerminal;
    use component_api::dust::plugin::host::ProviderTerminalKind;
    use component_api::dust::plugin::host::VirtualDocumentErrorCode;
    use component_api::dust::plugin::host::VirtualDocumentFull;
    use component_api::dust::plugin::host::VirtualDocumentPatch;
    use component_api::dust::plugin::host::VirtualDocumentPatchSet;
    use component_api::dust::plugin::host::VirtualDocumentProposal;
    use component_api::dust::plugin::host::VirtualDocumentProposalKind;
    let mut host = ComponentHostState {
        editor_requests: Default::default(),
        runtime_epoch: 0,
        limits: crate::plugins::component::ComponentLimits::new(
            wasmtime::StoreLimitsBuilder::new().build(),
            64 * 1024 * 1024,
        ),
        capabilities: [
            PluginCapability::PickerProviders,
            PluginCapability::VirtualDocuments,
        ]
        .into_iter()
        .collect(),
        declared_content_types: BTreeSet::new(),
        snapshot: None,
        proposals: Vec::new(),
        messages: Vec::new(),
        config: BTreeMap::new(),
        ui_revision: None,
        confirmation_available: true,
        confirmation_result_only: false,
        ui_requests: Vec::new(),
        ui_events: Vec::new(),
        next_ui_token: 1,
        pending_ui_tokens: BTreeSet::new(),
        confirmation_requests: Vec::new(),
        confirmation_results: Vec::new(),
        next_confirmation_token: 1,
        host_events: Vec::new(),
        delivered_host_event_ids: BTreeSet::new(),
        host_event_acknowledgements: Vec::new(),
        execution_available: true,
        pending_storage: BTreeMap::new(),
        storage_policy: None,
        storage_requests: Vec::new(),
        storage_results: Vec::new(),
        next_storage_token: 1,
        provider_requests: Vec::new(),
        delivered_provider_requests: BTreeSet::from([1]),
        provider_chunks: Vec::new(),
        virtual_document_requests: Vec::new(),
        delivered_virtual_document_requests: BTreeSet::from([1]),
        virtual_document_proposals: Vec::new(),
    };
    let chunk = || PickerProviderChunk {
        request: 1,
        sequence: 1,
        items: (0..128)
            .map(|id| PickerProviderItem {
                id: id.to_string(),
                primary: "p".repeat(1024),
                secondary: None,
                action_data: vec![0; 4096],
            })
            .collect(),
        terminal: ProviderTerminal {
            kind: ProviderTerminalKind::More,
            error: None,
        },
    };
    host.queue_picker_provider_chunk(chunk()).unwrap();
    assert_eq!(
        host.queue_picker_provider_chunk(chunk()).unwrap_err().code,
        ProviderErrorCode::ByteLimit
    );
    assert_eq!(host.provider_chunks.len(), 1);
    host.provider_chunks.clear();
    host.queue_picker_provider_chunk(chunk()).unwrap();

    let full = || VirtualDocumentProposal {
        request: 1,
        document: 1,
        cancellation_id: 1,
        kind: VirtualDocumentProposalKind::Full(VirtualDocumentFull {
            expected_revision: None,
            revision: 1,
            title: "Document".into(),
            language: "text".into(),
            text: "x".repeat(1024 * 1024),
        }),
    };
    for _ in 0..3 {
        host.queue_virtual_document_proposal(full()).unwrap();
    }
    assert_eq!(
        host.queue_virtual_document_proposal(full())
            .unwrap_err()
            .code,
        VirtualDocumentErrorCode::ByteLimit
    );
    assert_eq!(host.virtual_document_proposals.len(), 3);
    host.virtual_document_proposals.clear();
    let patches = || VirtualDocumentProposal {
        request: 1,
        document: 1,
        cancellation_id: 1,
        kind: VirtualDocumentProposalKind::Patches(VirtualDocumentPatchSet {
            expected_revision: 1,
            revision: 2,
            patches: vec![VirtualDocumentPatch {
                start: 0,
                end: 0,
                replacement: "x".repeat(256 * 1024),
            }],
        }),
    };
    for _ in 0..16 {
        host.queue_virtual_document_proposal(patches()).unwrap();
    }
    assert_eq!(
        host.queue_virtual_document_proposal(patches())
            .unwrap_err()
            .code,
        VirtualDocumentErrorCode::ByteLimit
    );
    assert_eq!(host.virtual_document_proposals.len(), 16);
    host.virtual_document_proposals.clear();
    host.queue_virtual_document_proposal(full()).unwrap();
}

#[test]
fn static_manifest_contribution_index_is_namespaced_deterministic_and_pure() {
    let manifest = manifest();
    let index =
        index_manifest_contributions(&manifest, 9, "manifest.toml".into()).expect("static index");
    assert_eq!(index.owner, "demo");
    assert_eq!(index.plugin_load, 9);
    assert_eq!(index.contexts[0].id, "plugin.demo.chat");
    assert_eq!(index.keymaps[0].id, "plugin.demo.keymap.open-chat");
    assert_eq!(index.content_types[0].id, "plugin.demo.chat-panel");
    let mut stolen = manifest;
    stolen.contributions.keymaps[0].id = "plugin.other.keymap.stolen".into();
    assert!(matches!(
        index_manifest_contributions(&stolen, 10, "manifest.toml".into()),
        Err(PluginError::InvalidManifest)
    ));
}

#[test]
fn static_manifest_command_ids_fit_the_command_registry() {
    let mut manifest = manifest();
    manifest.id = "p".repeat(64);
    manifest.contributions = PluginContributions::default();
    let declaration = manifest.commands["run"].clone();
    // "plugin." and the separator leave 56 bytes after a 64-byte plugin ID.
    manifest.commands = [("c".repeat(56), declaration.clone())]
        .into_iter()
        .collect();
    assert!(index_manifest_contributions(&manifest, 1, "manifest.toml".into()).is_ok());
    for length in [57, 64] {
        manifest.commands = [("c".repeat(length), declaration.clone())]
            .into_iter()
            .collect();
        assert!(matches!(
            index_manifest_contributions(&manifest, 1, "manifest.toml".into()),
            Err(PluginError::InvalidManifest)
        ));
    }
}

#[test]
fn candidate_activation_validates_the_manifest_before_loading() {
    let limits = PluginLimits::default();
    let loaded = retained_runtime(&limits, false).unwrap().loaded;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("guest.wasm");
    fs::write(&path, loaded.bytes).unwrap();
    let mut manifest = loaded.manifest;
    // The runtime fixture grants events without declaring subscriptions.
    manifest
        .capabilities
        .remove(&PluginCapability::EventSubscriptions);
    assert!(activate_candidate(root.path(), &path, manifest.clone(), &limits).is_ok());
    let version = std::mem::take(&mut manifest.version);
    assert!(matches!(
        activate_candidate(root.path(), &path, manifest.clone(), &limits),
        Err(PluginError::InvalidManifest)
    ));
    manifest.version = version;
    let limits = PluginLimits {
        maximum_commands: 0,
        ..limits
    };
    assert!(matches!(
        activate_candidate(root.path(), &path, manifest, &limits),
        Err(PluginError::InvalidManifest)
    ));
}

fn renamed_manifest(owner: &str) -> PluginManifest {
    let mut manifest = manifest();
    manifest.id = owner.into();
    for context in &mut manifest.contributions.input_contexts {
        context.id = context
            .id
            .replace("plugin.demo.", &format!("plugin.{owner}."));
    }
    for keymap in &mut manifest.contributions.keymaps {
        keymap.id = keymap
            .id
            .replace("plugin.demo.", &format!("plugin.{owner}."));
        keymap.context = keymap
            .context
            .replace("plugin.demo.", &format!("plugin.{owner}."));
        keymap.command = keymap
            .command
            .replace("plugin.demo.", &format!("plugin.{owner}."));
    }
    for group in &mut manifest.contributions.key_groups {
        group.id = group
            .id
            .replace("plugin.demo.", &format!("plugin.{owner}."));
        group.context = group
            .context
            .replace("plugin.demo.", &format!("plugin.{owner}."));
    }
    for content in &mut manifest.contributions.content_types {
        content.id = content
            .id
            .replace("plugin.demo.", &format!("plugin.{owner}."));
        content.context = content
            .context
            .replace("plugin.demo.", &format!("plugin.{owner}."));
        content.command = content
            .command
            .replace("plugin.demo.", &format!("plugin.{owner}."));
    }
    manifest
}

#[test]
fn contribution_catalog_orders_plugins_and_removes_exact_loads() {
    let demo = index_manifest_contributions(&manifest(), 3, "demo.toml".into()).expect("demo");
    let mut other_manifest = renamed_manifest("other");
    other_manifest.contributions.keymaps[0].keys = "<leader> b".into();
    other_manifest.contributions.key_groups[0].prefix = "<leader>".into();
    let other =
        index_manifest_contributions(&other_manifest, 4, "other.toml".into()).expect("other");
    let mut catalog = PluginContributionCatalog::default();
    catalog.install(other).expect("other first");
    catalog.install(demo).expect("demo second");
    assert_eq!(
        catalog
            .ordered()
            .map(|index| index.owner.as_str())
            .collect::<Vec<_>>(),
        vec!["demo", "other"]
    );
    assert!(catalog.context_snapshot().is_ok());
    assert!(!catalog.remove_load("demo", 2));
    assert!(catalog.get("demo").is_some());
    assert!(catalog.remove_load("demo", 3));
    assert!(catalog.get("demo").is_none());

    let mut demo_manifest = manifest();
    demo_manifest.contributions.keymaps[0].context = "editor".into();
    demo_manifest.contributions.keymaps[0].mode = "normal".into();
    let demo =
        index_manifest_contributions(&demo_manifest, 5, "demo.toml".into()).expect("editor demo");
    let mut other_manifest = renamed_manifest("other");
    other_manifest.contributions.keymaps[0].context = "editor".into();
    other_manifest.contributions.keymaps[0].mode = "normal".into();
    let other = index_manifest_contributions(&other_manifest, 6, "other.toml".into())
        .expect("editor other");
    let mut conflicts = PluginContributionCatalog::default();
    conflicts.install(demo).expect("first default");
    conflicts
        .install(other)
        .expect("key conflicts resolve after user overrides");
}

#[test]
fn plugin_manifest_reads_require_regular_bounded_files() {
    let root = tempfile::tempdir().unwrap();
    let relative = std::path::Path::new("manifest.toml");
    let path = root.path().join(relative);
    let limits = PluginLimits::default();
    let check = |expected: fn(&PluginError) -> bool| {
        let (_, error) = crate::plugins::index::load_manifest_component(
            root.path(),
            relative,
            std::path::Path::new("missing.wasm"),
            &limits,
        )
        .err()
        .expect("manifest must be rejected before loading the component");
        assert!(expected(&error), "{error}");
        let error = crate::plugins::index::index_manifest_from_store(
            root.path(),
            relative,
            std::path::Path::new("missing.wasm"),
            PluginLoadId(1),
            &limits,
        )
        .unwrap_err();
        assert!(expected(&error), "{error}");
    };
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
    check(
        |error| matches!(error, PluginError::Io(error) if error.kind() == std::io::ErrorKind::InvalidInput),
    );
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &path).unwrap();
    check(|error| matches!(error, PluginError::OutsideStore));
    fs::remove_file(&path).unwrap();

    let mut source = toml::to_string(&manifest()).unwrap();
    fs::write(&path, &source).unwrap();
    crate::plugins::index::index_manifest_from_store(
        root.path(),
        relative,
        std::path::Path::new("missing.wasm"),
        PluginLoadId(1),
        &limits,
    )
    .expect("a regular manifest can be indexed without its component");
    let (_, error) = crate::plugins::index::load_manifest_component(
        root.path(),
        relative,
        std::path::Path::new("missing.wasm"),
        &limits,
    )
    .err()
    .unwrap();
    assert!(
        matches!(error, PluginError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
    );

    source.extend(std::iter::repeat_n(' ', 1024 * 1024 + 1 - source.len()));
    fs::write(&path, source).unwrap();
    check(|error| matches!(error, PluginError::InvalidManifest));
}

#[test]
fn index_worker_does_not_touch_component_and_requires_host_acceptance() {
    let root = tempfile::tempdir().expect("store");
    let manifest_source = toml::to_string(&manifest()).expect("serialize manifest");
    fs::write(root.path().join("manifest.toml"), manifest_source).expect("manifest");
    let mut manager = PluginHost::start(4);
    let load = manager
        .index_from_store(
            "demo".into(),
            root.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing-component.wasm"),
            PluginLimits::default(),
        )
        .expect("queue index");
    assert!(matches!(
        manager.activate_indexed("demo", load, "0".repeat(64)),
        Err(PluginError::IndexNotAccepted)
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let index = loop {
        let events = manager.poll().expect("poll");
        if let Some(index) = events.into_iter().find_map(|event| match event {
            PluginEvent::Indexed { index, .. } => Some(index),
            _ => None,
        }) {
            break index;
        }
        assert!(std::time::Instant::now() < deadline, "index timed out");
        std::thread::yield_now();
    };
    assert_eq!(index.owner, "demo");
    let provenance = index.content_types[0]
        .provenance
        .as_ref()
        .expect("content provenance");
    assert_eq!(provenance.owner, "demo");
    assert_eq!(provenance.plugin_load, load.0);
    assert!(provenance.span.is_some());
    manager
        .activate_indexed("demo", load, index.checksum)
        .expect("accept index");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let events = manager.poll().expect("poll activation");
        if events.into_iter().any(|event| {
                matches!(event, PluginEvent::Failed { load: candidate, .. } if candidate == load)
            }) {
                break;
            }
        assert!(std::time::Instant::now() < deadline, "activation timed out");
        std::thread::yield_now();
    }
}

#[test]
fn execution_before_activation_removes_the_workers_indexed_candidate() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("manifest.toml"),
        toml::to_string(&manifest()).unwrap(),
    )
    .unwrap();
    let mut manager = PluginHost::start(4);
    let load = manager
        .index_from_store(
            "demo".into(),
            root.path().into(),
            "manifest.toml".into(),
            "missing-component.wasm".into(),
            PluginLimits::default(),
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if manager
            .poll()
            .unwrap()
            .iter()
            .any(|event| matches!(event, PluginEvent::Indexed { .. }))
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    manager.execute(empty_execution_request()).unwrap();
    assert!(
        matches!(manager.receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
        WorkerEvent::Failed { load: failed, error: PluginError::NotActive, .. } if failed == load)
    );
    // Foreground has not polled the failure yet. Activation must not find a
    // candidate, rather than trying to load its missing component.
    manager
        .activate_indexed("demo", load, "0".repeat(64))
        .unwrap();
    assert!(
        matches!(manager.receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
        WorkerEvent::Failed { load: failed, error: PluginError::IndexNotAccepted, .. } if failed == load)
    );
}

#[test]
fn hostile_static_contributions_are_rejected_before_component_access() {
    let absent = format!(
        "id='demo'\nversion='1'\ncomponent_sha256='{}'\ncapabilities=[]\n[commands]\n'plugin.demo.run'='Run'\n",
        "0".repeat(64)
    );
    assert!(toml::from_str::<PluginManifest>(&absent).is_err());

    let mut cases = Vec::new();
    let mut candidate = manifest();
    candidate.contributions.schema_version = 2;
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.id = "BAD".into();
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.commands = [("Stolen".into(), candidate.commands["run"].clone())]
        .into_iter()
        .collect();
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.keymaps[0].command = "plugin.demo.missing".into();
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.keymaps[0].context = "plugin.demo.missing".into();
    cases.push(candidate);
    let mut candidate = manifest();
    let mut duplicate = candidate.contributions.keymaps[0].clone();
    duplicate.id = "plugin.demo.keymap.duplicate".into();
    candidate.contributions.keymaps.push(duplicate);
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.keymaps[0]
        .arguments
        .insert("nested".into(), toml::Value::Array(vec![]));
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.key_groups[0].icon = Some("\u{0301}".into());
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.content_types[0].id = "plugin.other.stolen".into();
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.content_types[0].command = "plugin.demo.missing".into();
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.content_types[0].maximum_instances = 17;
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.content_types[0]
        .allowed_placements
        .remove(&PluginWindowPlacementKind::Floating);
    cases.push(candidate);
    let mut candidate = manifest();
    candidate.contributions.input_contexts = (0..65)
        .map(|index| PluginInputContextContribution {
            id: format!("plugin.demo.c{index}"),
            title: format!("Context {index}"),
            parent_surface: Some("panel".into()),
        })
        .collect();
    cases.push(candidate);
    let mut private = manifest();
    private.capabilities.insert(PluginCapability::PrivateCells);
    private.contributions.content_types[0].presenter = PluginPresenterKind::PrivateCells;
    private.contributions.content_types[0].private_cells = Some(PluginPrivateCellPolicy {
        maximum_cells: 256,
        maximum_patch_cells: 128,
        maximum_dirty_rectangles: 8,
        maximum_styles: 8,
        maximum_hit_regions: 8,
        maximum_bytes: 8_192,
        cursor: true,
    });
    assert!(index_manifest_contributions(&private, 1, "manifest.toml".into()).is_ok());
    let mut no_capability = private.clone();
    no_capability
        .capabilities
        .remove(&PluginCapability::PrivateCells);
    cases.push(no_capability);
    let mut excessive = private.clone();
    excessive.contributions.content_types[0]
        .private_cells
        .as_mut()
        .expect("policy")
        .maximum_cells = 4_097;
    cases.push(excessive);
    let mut mismatched = manifest();
    mismatched.contributions.content_types[0].private_cells =
        private.contributions.content_types[0].private_cells.clone();
    cases.push(mismatched);
    for candidate in cases {
        assert!(matches!(
            index_manifest_contributions(&candidate, 1, "manifest.toml".into()),
            Err(PluginError::InvalidManifest)
        ));
    }
    let mut retained = manifest();
    retained.contributions.keymaps[0].command = "surface.retained.activate".into();
    assert!(index_manifest_contributions(&retained, 1, "manifest.toml".into()).is_ok());
    retained.contributions.keymaps[0].command = "health.open".into();
    assert!(matches!(
        index_manifest_contributions(&retained, 1, "manifest.toml".into()),
        Err(PluginError::InvalidManifest)
    ));
    assert!(matches!(
        index_manifest_contributions(&manifest(), 1, "bad\nsource".into()),
        Err(PluginError::InvalidManifest)
    ));
}

#[test]
fn static_events_providers_and_storage_are_capability_gated_and_bounded() {
    let mut candidate = manifest();
    candidate.capabilities.extend([
        PluginCapability::EventSubscriptions,
        PluginCapability::PickerProviders,
        PluginCapability::PluginStorage,
        PluginCapability::VirtualDocuments,
    ]);
    candidate.contributions.subscriptions = vec![PluginSubscriptionContribution {
        id: "plugin.demo.subscription.workspace".into(),
        kind: PluginSubscriptionKind::WorkspaceMetadataChanged,
        maximum_queued: 8,
        maximum_rate_per_second: 30,
        coalesce: true,
        provenance: None,
    }];
    candidate.contributions.providers = vec![PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 2,
        maximum_items: 128,
        maximum_bytes: 16_384,
        deadline_milliseconds: 500,
        provenance: None,
    }];
    candidate.contributions.virtual_documents = vec![PluginVirtualDocumentContribution {
        id: "plugin.demo.virtual.help".into(),
        title: "Help".into(),
        language: "markdown".into(),
        mode: PluginVirtualDocumentMode::ReadOnly,
        maximum_concurrent_documents: 2,
        maximum_document_bytes: 16_384,
        maximum_document_lines: 512,
        maximum_patch_bytes: 4_096,
        maximum_patches_per_request: 16,
        deadline_milliseconds: 500,
        command: "plugin.demo.run".into(),
        context: "dashboard".into(),
        provenance: None,
    }];
    candidate.contributions.storage = Some(PluginStorageContribution {
        maximum_items: 32,
        maximum_value_bytes: 1_024,
        maximum_total_bytes: 16_384,
    });
    let indexed = index_manifest_contributions(&candidate, 7, "manifest.toml".into())
        .expect("bounded static extension declarations");
    assert_eq!(
        indexed.subscriptions[0]
            .provenance
            .as_ref()
            .unwrap()
            .plugin_load,
        7
    );
    assert_eq!(indexed.providers[0].kind, PluginProviderKind::PickerSource);
    assert!(indexed.storage.is_some());
    assert_eq!(indexed.virtual_documents[0].language, "markdown");

    let mut unsupported = candidate.clone();
    unsupported.contributions.providers[0].kind = PluginProviderKind::VirtualDocument;
    assert!(matches!(
        index_manifest_contributions(&unsupported, 7, "manifest.toml".into()),
        Err(PluginError::InvalidManifest)
    ));
    let mut no_capability = candidate.clone();
    no_capability
        .capabilities
        .remove(&PluginCapability::EventSubscriptions);
    assert!(matches!(
        index_manifest_contributions(&no_capability, 7, "manifest.toml".into()),
        Err(PluginError::InvalidManifest)
    ));
    let mut excessive = candidate;
    excessive
        .contributions
        .storage
        .as_mut()
        .unwrap()
        .maximum_total_bytes = MAXIMUM_STORAGE_TOTAL_BYTES + 1;
    assert!(matches!(
        index_manifest_contributions(&excessive, 7, "manifest.toml".into()),
        Err(PluginError::InvalidManifest)
    ));
}

#[test]
fn coalesced_host_events_keep_observation_order() {
    let mut candidate = manifest();
    candidate
        .capabilities
        .insert(PluginCapability::EventSubscriptions);
    candidate.contributions.subscriptions = [
        (
            "workspace",
            PluginSubscriptionKind::WorkspaceMetadataChanged,
        ),
        ("surface", PluginSubscriptionKind::ActiveSurfaceChanged),
    ]
    .into_iter()
    .map(|(name, kind)| PluginSubscriptionContribution {
        id: format!("plugin.demo.subscription.{name}"),
        kind,
        maximum_queued: 1,
        maximum_rate_per_second: 30,
        coalesce: true,
        provenance: None,
    })
    .collect();
    let index = index_manifest_contributions(&candidate, 9, "manifest.toml".into()).unwrap();
    let mut queue = PluginHostEventQueue::default();
    queue.install(&index).unwrap();
    for (kind, revision) in [
        (PluginSubscriptionKind::WorkspaceMetadataChanged, 1),
        (PluginSubscriptionKind::ActiveSurfaceChanged, 1),
        (PluginSubscriptionKind::WorkspaceMetadataChanged, 2),
    ] {
        queue
            .observe("demo", PluginLoadId(9), kind, revision, "changed")
            .unwrap();
    }
    let events = queue.deliver("demo", PluginLoadId(9));
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(events[0].kind, PluginSubscriptionKind::ActiveSurfaceChanged);
    assert_eq!(
        events[1].kind,
        PluginSubscriptionKind::WorkspaceMetadataChanged
    );
    assert_eq!(events[1].coalesced, 1);
    queue.acknowledge(
        "demo",
        PluginLoadId(9),
        &events.iter().map(|event| event.id).collect::<Vec<_>>(),
    );
    assert_eq!(queue.pending("demo", PluginLoadId(9)), 0);
}

#[test]
fn host_events_coalesce_acknowledge_and_remove_exact_load() {
    let mut candidate = manifest();
    candidate
        .capabilities
        .insert(PluginCapability::EventSubscriptions);
    candidate.contributions.subscriptions = vec![
        PluginSubscriptionContribution {
            id: "plugin.demo.subscription.workspace".into(),
            kind: PluginSubscriptionKind::WorkspaceMetadataChanged,
            maximum_queued: 1,
            maximum_rate_per_second: 30,
            coalesce: true,
            provenance: None,
        },
        PluginSubscriptionContribution {
            id: "plugin.demo.subscription.surface".into(),
            kind: PluginSubscriptionKind::ActiveSurfaceChanged,
            maximum_queued: 1,
            maximum_rate_per_second: 30,
            coalesce: true,
            provenance: None,
        },
    ];
    let index =
        index_manifest_contributions(&candidate, 9, "manifest.toml".into()).expect("event index");
    let mut queue = PluginHostEventQueue::default();
    queue.install(&index).expect("install");
    assert!(
        queue
            .observe(
                "demo",
                PluginLoadId(9),
                PluginSubscriptionKind::WorkspaceMetadataChanged,
                1,
                "roots=1"
            )
            .unwrap()
    );
    assert!(
        !queue
            .observe(
                "demo",
                PluginLoadId(9),
                PluginSubscriptionKind::WorkspaceMetadataChanged,
                1,
                "roots=1"
            )
            .unwrap()
    );
    assert!(
        queue
            .observe(
                "demo",
                PluginLoadId(9),
                PluginSubscriptionKind::WorkspaceMetadataChanged,
                2,
                "roots=2"
            )
            .unwrap()
    );
    let event = queue.deliver("demo", PluginLoadId(9)).pop().unwrap();
    assert_eq!((event.revision, event.coalesced), (2, 1));
    queue.acknowledge("demo", PluginLoadId(8), &[event.id]);
    assert_eq!(queue.pending("demo", PluginLoadId(9)), 1);
    queue
        .observe(
            "demo",
            PluginLoadId(9),
            PluginSubscriptionKind::WorkspaceMetadataChanged,
            3,
            "roots=3",
        )
        .unwrap();
    queue.acknowledge("demo", PluginLoadId(9), &[event.id]);
    assert_eq!(queue.pending("demo", PluginLoadId(9)), 1);
    queue
        .observe(
            "demo",
            PluginLoadId(9),
            PluginSubscriptionKind::ActiveSurfaceChanged,
            1,
            "panel",
        )
        .unwrap();
    queue
        .observe(
            "demo",
            PluginLoadId(9),
            PluginSubscriptionKind::WorkspaceMetadataChanged,
            3,
            "roots=3",
        )
        .unwrap();
    assert_eq!(queue.pending("demo", PluginLoadId(9)), 2);
    queue.acknowledge("demo", PluginLoadId(9), &[event.id]);
    assert_eq!(queue.pending("demo", PluginLoadId(9)), 2);
    let stable = queue.clone();
    for _ in 0..256 {
        assert!(
            !queue
                .observe(
                    "demo",
                    PluginLoadId(9),
                    PluginSubscriptionKind::WorkspaceMetadataChanged,
                    3,
                    "roots=3"
                )
                .unwrap()
        );
    }
    assert_eq!(queue, stable);
    assert!(!queue.remove_load("demo", PluginLoadId(8)));
    assert!(queue.remove_load("demo", PluginLoadId(9)));
}

#[test]
fn failed_enqueue_does_not_publish_loading() {
    let (tx, rx) = mpsc::sync_channel(1);
    drop(rx);
    let (_etx, erx) = mpsc::sync_channel(1);
    let mut manager = manager(tx, erx);
    assert!(matches!(
        manager.load_from_store(
            "demo".into(),
            PathBuf::from("store"),
            PathBuf::from("manifest"),
            PathBuf::from("component"),
            PluginLimits::default()
        ),
        Err(PluginError::Stopped)
    ));
    assert!(!manager.statuses.contains_key("demo"));
    assert!(manager.load_id("demo").is_none());
    assert!(manager.stopped);
}
#[test]
fn stale_failure_and_unload_cannot_replace_current_load() {
    let (tx, _rx) = mpsc::sync_channel(4);
    let (etx, erx) = mpsc::sync_channel(4);
    let mut manager = manager(tx, erx);
    manager.loads.insert("demo".into(), PluginLoadId(2));
    manager
        .statuses
        .insert("demo".into(), PluginStatus::Loading);
    etx.send(WorkerEvent::Failed {
        load: PluginLoadId(1),
        plugin: "demo".into(),
        error: PluginError::NotActive,
    })
    .unwrap();
    assert!(manager.poll().unwrap().is_empty());
    assert_eq!(manager.load_id("demo"), Some(PluginLoadId(2)));
    manager.unload_load("demo", PluginLoadId(1)).unwrap();
    assert_eq!(manager.load_id("demo"), Some(PluginLoadId(2)));
}
#[test]
fn active_load_limit_bounds_disable_ownership() {
    let (tx, _rx) = mpsc::sync_channel(4);
    let (_etx, erx) = mpsc::sync_channel(1);
    let mut manager = manager(tx, erx);
    manager.maximum_loads = 1;
    manager.loads.insert("one".into(), PluginLoadId(1));
    assert!(matches!(
        manager.load_from_store(
            "two".into(),
            PathBuf::from("store"),
            PathBuf::from("manifest"),
            PathBuf::from("component"),
            PluginLimits::default()
        ),
        Err(PluginError::LoadLimit)
    ));
    manager.unload_load("one", PluginLoadId(1)).unwrap();
    assert!(manager.pending_disables.len() <= manager.maximum_loads);
}
#[test]
fn queue_full_cancellation_stays_pending_and_suppresses_events() {
    let (tx, rx) = mpsc::sync_channel(1);
    let (etx, erx) = mpsc::sync_channel(2);
    let mut manager = manager(tx, erx);
    manager.loads.insert("occupied".into(), PluginLoadId(9));
    manager.unload("occupied").unwrap();
    manager.loads.insert("demo".into(), PluginLoadId(1));
    manager
        .statuses
        .insert("demo".into(), PluginStatus::Loading);
    manager.unload_load("demo", PluginLoadId(1)).unwrap();
    assert_eq!(manager.pending_disables.len(), 1);
    assert!(matches!(
        manager.load_from_store(
            "demo".into(),
            PathBuf::from("store"),
            PathBuf::from("manifest"),
            PathBuf::from("component"),
            PluginLimits::default()
        ),
        Err(PluginError::QueueFull)
    ));
    etx.send(WorkerEvent::Failed {
        load: PluginLoadId(1),
        plugin: "demo".into(),
        error: PluginError::NotActive,
    })
    .unwrap();
    assert!(manager.poll().unwrap().is_empty());
    assert_eq!(manager.pending_disables.len(), 1);
    let _ = rx.recv().unwrap().into_request();
    assert!(manager.poll().unwrap().is_empty());
    assert!(manager.pending_disables.is_empty());
    assert!(
        matches!(rx.recv().unwrap().into_request(),Request::Disable{load:PluginLoadId(1),plugin}if plugin=="demo")
    );
}
#[test]
fn disable_is_ordered_before_same_id_replacement_and_duplicates_are_rejected() {
    let (tx, rx) = mpsc::sync_channel(4);
    let (_etx, erx) = mpsc::sync_channel(1);
    let mut manager = manager(tx, erx);
    manager.loads.insert("demo".into(), PluginLoadId(1));
    manager.next_load = 2;
    assert!(matches!(
        manager.load_from_store(
            "demo".into(),
            PathBuf::from("store"),
            PathBuf::from("manifest"),
            PathBuf::from("component"),
            PluginLimits::default()
        ),
        Err(PluginError::AlreadyActive)
    ));
    manager.unload_load("demo", PluginLoadId(1)).unwrap();
    let replacement = manager
        .load_from_store(
            "demo".into(),
            PathBuf::from("store"),
            PathBuf::from("manifest"),
            PathBuf::from("component"),
            PluginLimits::default(),
        )
        .unwrap();
    assert_ne!(replacement, PluginLoadId(1));
    assert!(matches!(
        rx.recv().unwrap().into_request(),
        Request::Disable {
            load: PluginLoadId(1),
            ..
        }
    ));
    assert!(
        matches!(rx.recv().unwrap().into_request(),Request::LoadFromStore{load,..}if load==replacement)
    );
}

// Bounded command endpoint, not a Wasm guest. It exercises production reservation,
// request provenance and owned-event polling without constructing source replies.
pub(crate) struct ExecutionFixture {
    requests: Receiver<QueuedRequest>,
    events: SyncSender<WorkerEvent>,
}
pub(crate) struct CapturedExecution {
    load: PluginLoadId,
    origin: Option<PluginInvocationOrigin>,
    request: PluginExecutionRequest,
}
impl CapturedExecution {
    pub(crate) fn arguments(&self) -> &[String] {
        &self.request.arguments
    }
    pub(crate) fn call(
        &self,
    ) -> &crate::plugins::component::component_api::dust::plugin::host::CommandCall {
        &self.request.call
    }
    pub(crate) fn virtual_documents(&self) -> &[crate::plugins::PluginVirtualDocumentRequest] {
        &self.request.virtual_document_requests
    }
    pub(crate) fn snapshot(&self) -> Option<&PluginSnapshot> {
        self.request.snapshot.as_ref()
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.origin
            .as_ref()
            .unwrap()
            .same(other.origin.as_ref().unwrap())
    }
    pub(crate) fn confirmation_results(&self) -> &[PluginConfirmationResult] {
        &self.request.confirmation_results
    }
    pub(crate) fn confirmation_available(&self) -> bool {
        self.request.confirmation_available
    }
}
impl ExecutionFixture {
    pub(crate) fn new(capacity: usize) -> (PluginHost, Self) {
        let (tx, requests) = mpsc::sync_channel(capacity);
        let (events, rx) = mpsc::sync_channel(capacity);
        let mut manager = manager(tx, rx);
        manager.maximum_loads = capacity;
        manager.loads.insert("demo".into(), PluginLoadId(1));
        manager
            .statuses
            .insert("demo".into(), PluginStatus::Active(vec!["run".into()]));
        manager.next_load = 2;
        (manager, Self { requests, events })
    }
    pub(crate) fn assert_no_requests(&self) {
        assert!(self.is_empty());
    }
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self.requests.try_recv(), Err(mpsc::TryRecvError::Empty))
    }
    pub(crate) fn take(&self) -> CapturedExecution {
        match self
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .into_request()
        {
            Request::Execute {
                load,
                origin,
                invocation,
            } => CapturedExecution {
                load,
                origin,
                request: *invocation,
            },
            other => panic!("expected invocation, got {other:?}"),
        }
    }
    pub(crate) fn indexed(&self, index: PluginContributionIndex) {
        self.events
            .try_send(WorkerEvent::Indexed {
                load: PluginLoadId(index.plugin_load),
                plugin: index.owner.clone(),
                index,
            })
            .unwrap();
    }
    pub(crate) fn loaded(&self, load: PluginLoadId, component: LoadedComponent) {
        self.events
            .try_send(WorkerEvent::Loaded { load, component })
            .unwrap();
    }
    pub(crate) fn reply(&self, captured: &CapturedExecution, proposals: Vec<PluginEditProposal>) {
        self.reply_with(captured, |execution| execution.proposals = proposals);
    }
    pub(crate) fn reply_with(
        &self,
        captured: &CapturedExecution,
        update: impl FnOnce(&mut PluginExecution),
    ) {
        let mut execution = PluginExecution {
            error: None,
            editor_requests: Vec::new(),
            value: None,
            call_next: None,
            plugin: captured.request.plugin.clone(),
            command: captured.request.command.clone(),
            request_id: captured.request.request_id,
            result: String::new(),
            messages: Vec::new(),
            proposals: Vec::new(),
            ui_requests: Vec::new(),
            confirmation_requests: Vec::new(),
            host_event_acknowledgements: Vec::new(),
            storage_requests: Vec::new(),
            provider_chunks: Vec::new(),
            virtual_document_proposals: Vec::new(),
        };
        update(&mut execution);
        self.events
            .try_send(WorkerEvent::Executed {
                load: captured.load,
                origin: captured.origin.clone(),
                execution,
            })
            .unwrap();
    }
    pub(crate) fn reply_confirmation_request(
        &self,
        captured: &CapturedExecution,
        request: PluginConfirmationRequest,
    ) {
        self.events
            .try_send(WorkerEvent::Executed {
                load: captured.load,
                origin: captured.origin.clone(),
                execution: PluginExecution {
                    error: None,
                    editor_requests: Vec::new(),
                    value: None,
                    call_next: None,
                    plugin: captured.request.plugin.clone(),
                    command: captured.request.command.clone(),
                    request_id: captured.request.request_id,
                    result: String::new(),
                    messages: Vec::new(),
                    proposals: Vec::new(),
                    ui_requests: Vec::new(),
                    confirmation_requests: vec![request],
                    host_event_acknowledgements: Vec::new(),
                    storage_requests: Vec::new(),
                    provider_chunks: Vec::new(),
                    virtual_document_proposals: Vec::new(),
                },
            })
            .unwrap();
    }
    pub(crate) fn reply_virtual(
        &self,
        captured: &CapturedExecution,
        proposal: crate::plugins::PluginVirtualDocumentProposal,
    ) {
        self.events
            .try_send(WorkerEvent::Executed {
                load: captured.load,
                origin: captured.origin.clone(),
                execution: PluginExecution {
                    error: None,
                    editor_requests: Vec::new(),
                    value: None,
                    call_next: None,
                    plugin: captured.request.plugin.clone(),
                    command: captured.request.command.clone(),
                    request_id: captured.request.request_id,
                    result: String::new(),
                    messages: Vec::new(),
                    proposals: Vec::new(),
                    ui_requests: Vec::new(),
                    confirmation_requests: Vec::new(),
                    host_event_acknowledgements: Vec::new(),
                    storage_requests: Vec::new(),
                    provider_chunks: Vec::new(),
                    virtual_document_proposals: vec![proposal],
                },
            })
            .unwrap();
    }
    pub(crate) fn command_failed(&self, plugin: &str, load: PluginLoadId, error: PluginError) {
        self.events
            .try_send(WorkerEvent::CommandFailed {
                load,
                plugin: plugin.into(),
                error,
            })
            .unwrap();
    }
    pub(crate) fn fail(&self, captured: &CapturedExecution) {
        self.events
            .try_send(WorkerEvent::Failed {
                load: captured.load,
                plugin: captured.request.plugin.clone(),
                error: PluginError::NotActive,
            })
            .unwrap();
    }
}
pub(crate) fn empty_execution_request() -> PluginExecutionRequest {
    PluginExecutionRequest {
        call: Default::default(),
        delivery: Default::default(),
        runtime_epoch: 0,
        plugin: "demo".into(),
        command: "run".into(),
        request_id: 7,
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
        virtual_document_requests: Vec::new(),
    }
}

#[test]
fn component_memory_budget_covers_multiple_memories() {
    let engine = wasmtime::Engine::default();
    let limits = crate::plugins::component::ComponentLimits::new(
        wasmtime::StoreLimitsBuilder::new().memories(4).build(),
        128 * 1024,
    );
    let mut store = wasmtime::Store::new(&engine, limits);
    store.limiter(|limits| limits);
    let first = wasmtime::Memory::new(&mut store, wasmtime::MemoryType::new(1, None)).unwrap();
    let second = wasmtime::Memory::new(&mut store, wasmtime::MemoryType::new(1, Some(1))).unwrap();
    assert!(first.grow(&mut store, 1).is_err());
    assert!(wasmtime::Memory::new(&mut store, wasmtime::MemoryType::new(1, None)).is_err());
    assert_eq!(second.size(&store), 1);
}

fn retained_runtime(
    limits: &PluginLimits,
    looping_activation: bool,
) -> Result<crate::plugins::execution::LoadOutcome, PluginError> {
    retained_runtime_with_activation(
        limits,
        looping_activation.then_some(&[0, 3, 64, 12, 0, 11, 0, 11]),
    )
}

fn retained_runtime_with_activation(
    limits: &PluginLimits,
    activation_body: Option<&[u8]>,
) -> Result<crate::plugins::execution::LoadOutcome, PluginError> {
    fn number(bytes: &[u8], at: &mut usize) -> usize {
        let (mut value, mut shift) = (0, 0);
        loop {
            let byte = bytes[*at];
            *at += 1;
            value |= usize::from(byte & 127) << shift;
            if byte < 128 {
                return value;
            }
            shift += 7;
        }
    }
    fn encode(mut value: usize, output: &mut Vec<u8>) {
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            output.push(byte | if value > 0 { 128 } else { 0 });
            if value == 0 {
                break;
            }
        }
    }
    let original = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/retained-guest/retained-guest.core.wasm"
    ));
    let mut core = original.to_vec();
    if let Some(activation_body) = activation_body {
        // Find the exported function rather than depending on compiler function order.
        let mut imported_functions = 0;
        let mut activation = None;
        let mut section_at = 8;
        while section_at < original.len() {
            let section = original[section_at];
            section_at += 1;
            let length = number(original, &mut section_at);
            let end = section_at + length;
            if section == 2 {
                let count = number(original, &mut section_at);
                for _ in 0..count {
                    for _ in 0..2 {
                        let length = number(original, &mut section_at);
                        section_at += length;
                    }
                    assert_eq!(original[section_at], 0, "fixture imports only functions");
                    section_at += 1;
                    number(original, &mut section_at);
                    imported_functions += 1;
                }
            } else if section == 7 {
                let count = number(original, &mut section_at);
                for _ in 0..count {
                    let length = number(original, &mut section_at);
                    let name = &original[section_at..section_at + length];
                    section_at += length;
                    let kind = original[section_at];
                    section_at += 1;
                    let index = number(original, &mut section_at);
                    if name == b"activate" && kind == 0 {
                        activation = Some(index);
                    }
                }
            }
            section_at = end;
        }
        let activation = activation.expect("fixture exports activate") - imported_functions;
        let mut at = 8;
        while at < original.len() {
            let section = original[at];
            at += 1;
            let length = number(original, &mut at);
            let end = at + length;
            if section == 10 {
                let count = number(original, &mut at);
                let mut payload = Vec::new();
                encode(count, &mut payload);
                for function in 0..count {
                    let length = number(original, &mut at);
                    let body: &[u8] = if function == activation {
                        activation_body
                    } else {
                        &original[at..at + length]
                    };
                    encode(body.len(), &mut payload);
                    payload.extend_from_slice(body);
                    at += length;
                }
                // Locate the section header separately; its length is variable-width.
                let mut header = 8;
                while original[header] != 10 {
                    header += 1;
                    let length = number(original, &mut header);
                    header += length;
                }
                core = original[..header + 1].to_vec();
                encode(payload.len(), &mut core);
                core.extend(payload);
                core.extend_from_slice(&original[end..]);
                break;
            }
            at = end;
        }
    }
    use sha2::Digest;
    let component = wit_component::ComponentEncoder::default()
        .module(&core)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("guest.wasm");
    fs::write(&path, &component).unwrap();
    let mut manifest = manifest();
    manifest.component_sha256 = format!("{:x}", sha2::Sha256::digest(&component));
    let command = manifest.commands["run"].clone();
    for name in [
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
        "confirmationprobe",
    ] {
        manifest.commands.insert(name.into(), command.clone());
    }
    manifest
        .capabilities
        .insert(PluginCapability::EventSubscriptions);
    manifest
        .capabilities
        .insert(PluginCapability::PluginStorage);
    manifest.contributions.storage = Some(PluginStorageContribution {
        maximum_items: 64,
        maximum_value_bytes: 256 * 1024,
        maximum_total_bytes: 4 * 1024 * 1024,
    });
    let mut activation_limits = limits.clone();
    if activation_body.is_none() {
        activation_limits.epoch_deadline_ticks = 1000;
    }
    let mut outcome =
        crate::plugins::index::load_component(root.path(), &path, manifest, &activation_limits)?;
    outcome.runtime.limits = limits.clone();
    Ok(outcome)
}

#[test]
fn component_activation_checks_counts_and_strings_before_copying() {
    fn constant(mut value: i32, body: &mut Vec<u8>) {
        body.push(0x41); // i32.const
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            let done = (value == 0 && byte & 64 == 0) || (value == -1 && byte & 64 != 0);
            body.push(byte | if done { 0 } else { 128 });
            if done {
                break;
            }
        }
    }
    for (count, length, expected) in [(4096, i32::MAX, "count limit"), (2, 257, "byte limit")] {
        // Replace activate with a result whose list entries alias one guest string.
        let mut body = vec![0]; // no locals
        for (address, value) in [
            (0, 0),
            (4, 32),
            (8, count),
            (32, 4096),
            (36, length),
            (40, 4096),
            (44, length),
        ] {
            constant(address, &mut body);
            constant(value, &mut body);
            body.extend_from_slice(&[0x36, 2, 0]); // i32.store
        }
        constant(0, &mut body); // result pointer
        body.push(0x0b);
        let error = retained_runtime_with_activation(&PluginLimits::default(), Some(&body))
            .err()
            .expect("oversized activation must fail");
        assert!(matches!(error, PluginError::Activation(_)), "{error}");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn component_execution_errors_include_the_trap_cause() {
    let mut runtime = retained_runtime(&PluginLimits::default(), false)
        .unwrap()
        .runtime;
    runtime.limits.fuel = 1;
    let mut request = empty_execution_request();
    request.command = "spin".into();
    let error = crate::plugins::index::execute_component(request, &mut runtime).unwrap_err();
    assert!(matches!(error, PluginError::Execution(_)));
    assert!(error.to_string().contains("fuel"), "{error}");
}

#[test]
fn missed_command_deadline_discards_effects_and_the_instance_runs_again() {
    let mut runtime = retained_runtime(&PluginLimits::default(), false)
        .unwrap()
        .runtime;
    runtime.limits.epoch_deadline_ticks = 0;
    let mut request = empty_execution_request();
    request.command = "storage".into();
    assert!(matches!(
        crate::plugins::index::execute_component(request, &mut runtime),
        Err(PluginError::Deadline)
    ));
    assert!(runtime.store.data().storage_requests.is_empty());
    assert!(runtime.store.data().pending_storage.is_empty());
    // A guest error after the deadline is discarded too, not delivered as a reply.
    let mut request = empty_execution_request();
    request.command = "storage".into();
    assert!(matches!(
        crate::plugins::index::execute_component(request, &mut runtime),
        Err(PluginError::Deadline)
    ));
    runtime.limits.epoch_deadline_ticks = 1000;
    let mut request = empty_execution_request();
    request.command = "storage".into();
    let execution = crate::plugins::index::execute_component(request, &mut runtime).unwrap();
    assert!(
        execution
            .error
            .as_ref()
            .is_some_and(|error| error.starts_with("storage:1:"))
    );
    assert!(execution.storage_requests.is_empty());
    assert!(execution.editor_requests.is_empty());
    let mut request = empty_execution_request();
    request.command = "events".into();
    assert!(crate::plugins::index::execute_component(request, &mut runtime).is_ok());
}

#[test]
fn missed_deadline_and_command_error_preserve_the_active_load_but_a_trap_unloads() {
    let (tx, _rx) = mpsc::sync_channel(4);
    let (etx, erx) = mpsc::sync_channel(4);
    let mut manager = manager(tx, erx);
    let load = PluginLoadId(1);
    manager.loads.insert("demo".into(), load);
    manager
        .statuses
        .insert("demo".into(), PluginStatus::Active(vec!["run".into()]));
    for error in [
        PluginError::Deadline,
        PluginError::CommandFailed("ordinary error".into()),
    ] {
        etx.send(WorkerEvent::CommandFailed {
            load,
            plugin: "demo".into(),
            error,
        })
        .unwrap();
        assert!(matches!(
            manager.poll().unwrap().as_slice(),
            [PluginEvent::CommandFailed { .. }]
        ));
        assert!(manager.is_active_load("demo", load));
        assert!(matches!(manager.statuses["demo"], PluginStatus::Active(_)));
    }
    etx.send(WorkerEvent::Failed {
        load,
        plugin: "demo".into(),
        error: PluginError::Execution("trap".into()),
    })
    .unwrap();
    assert!(matches!(
        manager.poll().unwrap().as_slice(),
        [PluginEvent::Failed { .. }]
    ));
    assert!(!manager.is_active_load("demo", load));
}

#[test]
fn component_activation_and_each_command_have_their_own_deadline() {
    let limits = PluginLimits {
        fuel: 1_000_000_000,
        epoch_deadline_ticks: 1,
        ..Default::default()
    };
    let error = retained_runtime(&limits, true)
        .err()
        .expect("activation must time out");
    assert!(error.to_string().contains("interrupt"), "{error}");
    let limits = PluginLimits {
        fuel: u64::MAX,
        epoch_deadline_ticks: 20,
        ..Default::default()
    };
    let mut runtime = retained_runtime(&limits, false).unwrap().runtime;
    let mut request = empty_execution_request();
    request.command = "events".into();
    crate::plugins::index::execute_component(request, &mut runtime).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let start = std::time::Instant::now();
    let mut request = empty_execution_request();
    request.command = "spin".into();
    assert!(crate::plugins::index::execute_component(request, &mut runtime).is_err());
    assert!(
        start.elapsed() >= Duration::from_millis(120),
        "a previous invocation interrupted this one"
    );
}

#[test]
fn component_storage_driver_validates_each_reply_before_advancing() {
    let limits = PluginLimits {
        epoch_deadline_ticks: 1000,
        ..Default::default()
    };
    let mut runtime = retained_runtime(&limits, false).unwrap().runtime;
    let mut request = empty_execution_request();
    request.command = "storage".into();
    let mut execution = crate::plugins::index::execute_component(request, &mut runtime).unwrap();
    let revision = 17;
    for phase in 1..=6 {
        let token = execution.storage_requests[0].token;
        let mut result = PluginStorageResult {
            plugin: "demo".into(),
            load: PluginLoadId(1),
            token,
            value: None,
            revision: None,
            entries: Vec::new(),
            truncated: false,
            error: None,
        };
        match phase {
            1 | 5 => result.revision = Some(revision),
            2 => {
                result.value = Some(b"value".to_vec());
                result.revision = Some(revision);
            }
            3 => {
                result.error = Some(PluginStorageError::new(
                    PluginStorageErrorCode::StaleRevision,
                    "expected stale CAS",
                ));
            }
            4 => result.entries.push(PluginStorageEntry {
                key: "item.one".into(),
                revision,
                value: b"value".to_vec(),
            }),
            _ => {}
        }
        let mut invalid = vec![Vec::new(), vec![result.clone(), result.clone()]];
        let mut wrong = result.clone();
        wrong.token += 1;
        invalid.push(vec![wrong]);
        let mut wrong = result.clone();
        wrong.truncated = true;
        invalid.push(vec![wrong]);
        let mut wrong = result.clone();
        wrong.error = if phase == 3 {
            None
        } else {
            Some(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "failed",
            ))
        };
        invalid.push(vec![wrong]);
        let mut wrong = result.clone();
        wrong.revision = Some(0);
        invalid.push(vec![wrong]);
        let mut wrong = result.clone();
        wrong.value = Some(b"wrong".to_vec());
        invalid.push(vec![wrong]);
        let mut wrong = result.clone();
        wrong.entries.push(PluginStorageEntry {
            key: "unexpected".into(),
            revision,
            value: Vec::new(),
        });
        invalid.push(vec![wrong]);
        if phase == 2 {
            let mut wrong = result.clone();
            wrong.value = None;
            wrong.revision = None;
            invalid.push(vec![wrong]);
        }
        if phase == 4 {
            let mut wrong = result.clone();
            wrong.entries.clear();
            invalid.push(vec![wrong]);
            for field in 0..3 {
                let mut wrong = result.clone();
                match field {
                    0 => wrong.entries[0].key = "wrong".into(),
                    1 => wrong.entries[0].revision += 1,
                    _ => wrong.entries[0].value = b"stale".to_vec(),
                }
                invalid.push(vec![wrong]);
            }
        }
        for replies in invalid {
            let mut request = empty_execution_request();
            request.command = "storage".into();
            request.storage_results = replies;
            let execution =
                crate::plugins::index::execute_component(request, &mut runtime).unwrap();
            let error = execution.error.expect("invalid storage reply must fail");
            assert!(error.starts_with(&format!("storage:{phase}:")), "{error}");
            assert!(execution.storage_requests.is_empty());
            assert!(runtime.store.data().storage_requests.is_empty());
        }
        let mut request = empty_execution_request();
        request.command = "storage".into();
        request.storage_results = vec![result];
        execution = crate::plugins::index::execute_component(request, &mut runtime).unwrap();
        if phase == 6 {
            assert_eq!(execution.result, "storage:done:missing=true");
            assert!(execution.storage_requests.is_empty());
        } else {
            assert!(execution.result.starts_with(&format!("storage:{phase}:")));
            assert_eq!(execution.storage_requests.len(), 1);
            if phase == 4 {
                assert!(matches!(
                    execution.storage_requests[0].kind,
                    PluginStorageRequestKind::Delete { expected_revision, .. }
                        if expected_revision == revision
                ));
            }
        }
    }
}

#[test]
fn component_storage_inbox_retains_unread_results_and_reserves_capacity() {
    use component_api::dust::plugin::host::Host;
    use component_api::dust::plugin::host::StorageRequest;
    let limits = PluginLimits {
        epoch_deadline_ticks: 1000,
        ..Default::default()
    };
    let mut runtime = retained_runtime(&limits, false).unwrap().runtime;
    assert!(
        runtime
            .store
            .data_mut()
            .queue_storage_request(StorageRequest::Get("one".into()))
            .is_err(),
        "activation storage is unavailable"
    );
    let mut request = empty_execution_request();
    request.command = "events".into();
    request.storage_results.push(PluginStorageResult {
        plugin: "demo".into(),
        load: PluginLoadId(1),
        token: 1,
        value: Some(vec![42]),
        revision: Some(1),
        entries: Vec::new(),
        truncated: false,
        error: None,
    });
    crate::plugins::index::execute_component(request, &mut runtime).unwrap();
    let mut request = empty_execution_request();
    request.command = "events".into();
    crate::plugins::index::execute_component(request, &mut runtime).unwrap();
    assert_eq!(
        runtime.store.data_mut().take_storage_results()[0].value,
        Some(vec![42])
    );
    let host = runtime.store.data_mut();
    let mut accepted = 0;
    while host
        .queue_storage_request(StorageRequest::Get("one".into()))
        .is_ok()
    {
        accepted += 1;
    }
    assert!(accepted > 0 && accepted < 64);
    assert_eq!(host.pending_storage.len(), accepted);
    let mut request = empty_execution_request();
    request.command = "events".into();
    request.runtime_epoch = 1;
    crate::plugins::index::execute_component(request, &mut runtime).unwrap();
    assert!(runtime.store.data().pending_storage.is_empty());
}

#[test]
fn component_ui_queue_reserves_result_slots_until_guest_drain() {
    use component_api::dust::plugin::host::Host;
    use component_api::dust::plugin::host::OpenWindowRequest;
    use component_api::dust::plugin::host::UiEvent;
    use component_api::dust::plugin::host::UiRequest;
    use component_api::dust::plugin::host::WindowPlacement;
    let limits = PluginLimits {
        epoch_deadline_ticks: 1000,
        ..Default::default()
    };
    let mut runtime = retained_runtime(&limits, false).unwrap().runtime;
    let mut invocation = empty_execution_request();
    invocation.command = "events".into();
    crate::plugins::index::execute_component(invocation, &mut runtime).unwrap();
    let host = runtime.store.data_mut();
    let request = || {
        UiRequest::OpenWindow(OpenWindowRequest {
            expected_graph_revision: 1,
            content_type: "plugin.demo.chat-panel".into(),
            title: "Panel".into(),
            placement: WindowPlacement::Floating,
            x: 0,
            y: 0,
            width: 20,
            height: 3,
            size: 3,
            tree: None,
        })
    };
    for _ in 0..64 {
        host.queue_ui_request(request()).unwrap();
    }
    host.ui_requests.clear();
    assert!(host.queue_ui_request(request()).is_err());
    host.ui_events.push(UiEvent {
        token: 1,
        handle: Some(10),
        graph_revision: Some(2),
        tree_revision: None,
        surface_revision: None,
        geometry: None,
        action: None,
        error: None,
    });
    assert_eq!(host.take_ui_events()[0].handle, Some(10));
    assert!(host.queue_ui_request(request()).is_ok());
}

#[test]
fn manager_accepts_a_component_inside_a_symlinked_store() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("real");
    fs::create_dir(&store).unwrap();
    let link = root.path().join("store");
    std::os::unix::fs::symlink(&store, &link).unwrap();
    fs::write(store.join("guest.wasm"), b"\0asm").unwrap();
    let mut manager = PluginHost::start(4);
    let load = manager
        .load(
            link.clone(),
            link.join("guest.wasm"),
            manifest(),
            PluginLimits::default(),
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = manager.poll().unwrap();
        if events
            .iter()
            .any(|event| matches!(event, PluginEvent::Indexed { .. }))
        {
            break;
        }
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, PluginEvent::Failed { .. })),
            "symlinked store rejected"
        );
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(manager.load_id("demo"), Some(load));
}
