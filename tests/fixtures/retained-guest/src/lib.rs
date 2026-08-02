// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "wit", world: "editor-plugin" });

use dust::plugin::host::{
    self, CheckedWindowRequest, OpenWindowRequest, PatchPrivateRequest, PlaceWindowRequest,
    PrivateCell, PrivateCursor, PrivateHitRegion, PrivatePatch, PrivateRect, PrivateSnapshot,
    PickerProviderChunk, PickerProviderItem, PrivateStyle, ProviderTerminal,
    ProviderTerminalKind, ReplacePrivateRequest, ReplaceUiRequest, StorageCasRequest,
    StorageDeleteRequest, StorageListRequest, StorageRequest, UiNode, UiNodeKind, UiRequest,
    UiStyle, UiTree, VirtualDocumentFull, VirtualDocumentPatch, VirtualDocumentPatchSet,
    VirtualDocumentProposal, VirtualDocumentProposalKind, VirtualDocumentRequestKind,
    WindowPlacement,
};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

static PHASE: AtomicU8 = AtomicU8::new(0);
static PRIVATE_PHASE: AtomicU8 = AtomicU8::new(0);
static PRIVATE_HANDLE: AtomicU64 = AtomicU64::new(0);
static PRIVATE_DIMENSIONS: AtomicU64 = AtomicU64::new(0);
static STORAGE_PHASE: AtomicU8 = AtomicU8::new(0);
static STORAGE_REVISION: AtomicU64 = AtomicU64::new(0);
static PROVIDER_PHASE: AtomicU8 = AtomicU8::new(0);
static VIRTUAL_PHASE: AtomicU8 = AtomicU8::new(0);

struct GuestComponent;

impl Guest for GuestComponent {
    fn activate() -> Result<Vec<String>, String> {
        Ok(vec![
            "drive".into(),
            "trap".into(),
            "flood".into(),
            "spin".into(),
            "private".into(),
            "events".into(),
            "storage".into(),
            "provider".into(),
            "virtual".into(),
        ])
    }

    fn execute_command(command: String, arguments: Vec<String>) -> Result<String, String> {
        let graph = arguments.first().and_then(|value| value.parse().ok()).unwrap_or(1);
        if command == "trap" {
            let _ = host::queue_ui_request(&open_retained(graph, "Trap", "trap"));
            let _ = host::queue_storage_request(&StorageRequest::CompareAndSet(
                StorageCasRequest {
                    key: "trap.value".into(),
                    expected_revision: None,
                    value: b"never".to_vec(),
                },
            ));
            if let Some(request) = host::take_virtual_document_requests().first() {
                let _ = host::queue_virtual_document_proposal(&VirtualDocumentProposal {
                    request: request.request,
                    document: request.document,
                    cancellation_generation: request.cancellation_generation,
                    kind: VirtualDocumentProposalKind::Full(VirtualDocumentFull {
                        expected_revision: request.current_revision,
                        revision: request.current_revision.unwrap_or(0) + 1,
                        title: "Fixture help".into(), language: "markdown".into(), text: "never".into(),
                    }),
                });
            }
            if let Some(request) = host::take_picker_provider_requests().first() {
                let _ = host::queue_picker_provider_chunk(&PickerProviderChunk {
                    request: request.id,
                    sequence: 1,
                    items: Vec::new(),
                    terminal: ProviderTerminal {
                        kind: ProviderTerminalKind::More,
                        error: None,
                    },
                });
            }
            panic!("fixture trap")
        }
        if command == "spin" {
            let _ = host::queue_ui_request(&open_retained(graph, "Spin", "spin"));
            if let Some(request) = host::take_virtual_document_requests().first() {
                let _ = host::queue_virtual_document_proposal(&VirtualDocumentProposal {
                    request: request.request, document: request.document, cancellation_generation: request.cancellation_generation,
                    kind: VirtualDocumentProposalKind::Full(VirtualDocumentFull { expected_revision: request.current_revision, revision: request.current_revision.unwrap_or(0) + 1, title: "Fixture help".into(), language: "markdown".into(), text: "never spin".into() }),
                });
            }
            loop {
                std::hint::spin_loop();
            }
        }
        if command == "flood" {
            for _ in 0..64 {
                host::queue_ui_request(&open_retained(graph, "Flood", "flood")).unwrap();
            }
            let code = host::queue_ui_request(&open_retained(graph, "Overflow", "overflow"))
                .unwrap_err()
                .code;
            return Ok(format!("queue:{code:?}"));
        }
        if command == "private" {
            return drive_private(graph);
        }
        if command == "storage" {
            return drive_storage();
        }
        if command == "provider" {
            return drive_provider();
        }
        if command == "virtual" {
            return drive_virtual_document();
        }
        if command == "events" {
            let events = host::take_host_events();
            let ids = events.iter().map(|event| event.id).collect::<Vec<_>>();
            host::acknowledge_host_events(&ids)?;
            let coalesced = events.iter().map(|event| event.coalesced).sum::<u16>();
            let dropped = events.iter().map(|event| event.dropped).sum::<u16>();
            return Ok(format!(
                "events:{}:coalesced={coalesced}:dropped={dropped}",
                events.len()
            ));
        }

        let events = host::take_ui_events();
        let phase = PHASE.load(Ordering::SeqCst);
        let event = events.last();
        let handle = event.and_then(|event| event.handle).unwrap_or(0);
        let tree_revision = event.and_then(|event| event.tree_revision).unwrap_or(1);
        let request = match phase {
            0 => open_retained(graph, "Fixture", "open"),
            1 => UiRequest::ReplaceUi(ReplaceUiRequest {
                handle,
                expected_tree_revision: tree_revision,
                tree: tree(2, "replaced"),
            }),
            2 => UiRequest::PlaceWindow(PlaceWindowRequest {
                handle,
                expected_graph_revision: graph,
                placement: WindowPlacement::DockedRight,
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                size: 20,
            }),
            3 => UiRequest::FocusWindow(CheckedWindowRequest {
                handle,
                expected_graph_revision: graph,
            }),
            _ => UiRequest::CloseWindow(CheckedWindowRequest {
                handle,
                expected_graph_revision: graph,
            }),
        };
        host::queue_ui_request(&request).map_err(|error| format!("ui:{:?}", error.code))?;
        PHASE.store(phase.saturating_add(1), Ordering::SeqCst);
        Ok(format!("phase:{phase}"))
    }
}

fn drive_virtual_document() -> Result<String, String> {
    let requests = host::take_virtual_document_requests();
    let request = requests.last().ok_or_else(|| "virtual:no-request".to_string())?;
    let phase = VIRTUAL_PHASE.load(Ordering::SeqCst);
    let kind = match request.kind {
        VirtualDocumentRequestKind::Open => VirtualDocumentProposalKind::Full(VirtualDocumentFull {
            expected_revision: None, revision: 1, title: "Fixture help".into(), language: "markdown".into(), text: "# Héllo\nvirtual document".into(),
        }),
        VirtualDocumentRequestKind::Refresh => VirtualDocumentProposalKind::Patches(VirtualDocumentPatchSet {
            expected_revision: request.current_revision.unwrap_or(1), revision: request.current_revision.unwrap_or(1) + 1,
            patches: vec![VirtualDocumentPatch { start: 0, end: 1, replacement: "##".into() }],
        }),
        VirtualDocumentRequestKind::CloseNotice => VirtualDocumentProposalKind::CloseAcknowledged,
    };
    host::queue_virtual_document_proposal(&VirtualDocumentProposal { request: request.request, document: request.document, cancellation_generation: request.cancellation_generation, kind })
        .map_err(|error| format!("virtual:{:?}", error.code))?;
    VIRTUAL_PHASE.store(phase.saturating_add(1), Ordering::SeqCst);
    Ok(format!("virtual:{phase}"))
}

fn drive_provider() -> Result<String, String> {
    let requests = host::take_picker_provider_requests();
    let request = requests.last().ok_or_else(|| "provider:no-request".to_string())?;
    let phase = PROVIDER_PHASE.load(Ordering::SeqCst);
    let (items, terminal) = match phase {
        0 => (
            vec![PickerProviderItem {
                id: "alpha".into(),
                primary: "Alpha".into(),
                secondary: Some("first".into()),
                action_data: b"a".to_vec(),
            }],
            ProviderTerminal {
                kind: ProviderTerminalKind::More,
                error: None,
            },
        ),
        1 => (
            vec![PickerProviderItem {
                id: "beta".into(),
                primary: "Beta".into(),
                secondary: Some("second".into()),
                action_data: b"b".to_vec(),
            }],
            ProviderTerminal {
                kind: ProviderTerminalKind::More,
                error: None,
            },
        ),
        _ => (
            Vec::new(),
            ProviderTerminal {
                kind: ProviderTerminalKind::Complete,
                error: None,
            },
        ),
    };
    host::queue_picker_provider_chunk(&PickerProviderChunk {
        request: request.id,
        sequence: u64::from(phase) + 1,
        items,
        terminal,
    })
    .map_err(|error| format!("provider:{:?}", error.code))?;
    PROVIDER_PHASE.store(phase.saturating_add(1), Ordering::SeqCst);
    Ok(format!("provider:{phase}"))
}

fn drive_storage() -> Result<String, String> {
    let results = host::take_storage_results();
    for result in &results {
        if let Some(revision) = result.revision {
            STORAGE_REVISION.store(revision, Ordering::SeqCst);
        }
    }
    let phase = STORAGE_PHASE.load(Ordering::SeqCst);
    let revision = STORAGE_REVISION.load(Ordering::SeqCst);
    let request = match phase {
        0 => StorageRequest::CompareAndSet(StorageCasRequest {
            key: "item.one".into(),
            expected_revision: None,
            value: b"value".to_vec(),
        }),
        1 => StorageRequest::Get("item.one".into()),
        2 => StorageRequest::CompareAndSet(StorageCasRequest {
            key: "item.one".into(),
            expected_revision: None,
            value: b"stale".to_vec(),
        }),
        3 => StorageRequest::PrefixList(StorageListRequest {
            prefix: "item".into(),
            maximum: 8,
        }),
        4 => StorageRequest::Delete(StorageDeleteRequest {
            key: "item.one".into(),
            expected_revision: revision,
        }),
        5 => StorageRequest::Get("item.one".into()),
        _ => {
            let missing = results
                .last()
                .is_some_and(|result| result.value.is_none() && result.error.is_none());
            return Ok(format!("storage:done:missing={missing}"));
        }
    };
    let token = host::queue_storage_request(&request)
        .map_err(|error| format!("storage:{:?}", error.code))?;
    let error = results
        .last()
        .and_then(|result| result.error.as_ref())
        .map(|error| format!("{:?}", error.code))
        .unwrap_or_else(|| "none".into());
    STORAGE_PHASE.store(phase.saturating_add(1), Ordering::SeqCst);
    Ok(format!("storage:{phase}:token={token}:error={error}"))
}

fn drive_private(graph: u64) -> Result<String, String> {
    let events = host::take_ui_events();
    for event in &events {
        if let Some(handle) = event.handle {
            PRIVATE_HANDLE.store(handle, Ordering::SeqCst);
        }
        if let Some(geometry) = &event.geometry {
            PRIVATE_HANDLE.store(geometry.handle, Ordering::SeqCst);
            PRIVATE_DIMENSIONS.store(geometry.dimensions_revision, Ordering::SeqCst);
        }
    }
    let phase = PRIVATE_PHASE.load(Ordering::SeqCst);
    let handle = PRIVATE_HANDLE.load(Ordering::SeqCst);
    let dimensions_revision = PRIVATE_DIMENSIONS.load(Ordering::SeqCst);
    let surface_revision = events
        .iter()
        .rev()
        .find_map(|event| event.surface_revision)
        .unwrap_or(1);
    let action_seen = events
        .iter()
        .any(|event| event.action.as_deref() == Some("plugin.demo.private"));
    let request = match phase {
        0 => UiRequest::OpenWindow(OpenWindowRequest {
            expected_graph_revision: graph,
            content_type: "plugin.demo.cells".into(),
            title: "Private fixture".into(),
            placement: WindowPlacement::Floating,
            x: 3,
            y: 2,
            width: 20,
            height: 6,
            size: 6,
            tree: None,
        }),
        1 => UiRequest::ReplacePrivate(ReplacePrivateRequest {
            handle,
            snapshot: private_snapshot(1, dimensions_revision, "A"),
        }),
        2 => UiRequest::PatchPrivate(PatchPrivateRequest {
            handle,
            patch: PrivatePatch {
                revision: 2,
                expected_surface_revision: surface_revision,
                dimensions_revision,
                dirty: vec![PrivateRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                }],
                styles: Vec::new(),
                cells: vec![PrivateCell {
                    x: 0,
                    y: 0,
                    grapheme: "B".into(),
                    continuation: false,
                    style: 1,
                }],
                replace_cursor: false,
                cursor: None,
                replace_hit_regions: false,
                hit_regions: Vec::new(),
            },
        }),
        _ => UiRequest::CloseWindow(CheckedWindowRequest {
            handle,
            expected_graph_revision: graph,
        }),
    };
    host::queue_ui_request(&request).map_err(|error| format!("ui:{:?}", error.code))?;
    PRIVATE_PHASE.store(phase.saturating_add(1), Ordering::SeqCst);
    Ok(format!("private:{phase}:action={action_seen}"))
}

fn open_retained(graph: u64, title: &str, text: &str) -> UiRequest {
    UiRequest::OpenWindow(OpenWindowRequest {
        expected_graph_revision: graph,
        content_type: "plugin.demo.panel".into(),
        title: title.into(),
        placement: WindowPlacement::Floating,
        x: 2,
        y: 2,
        width: 30,
        height: 8,
        size: 8,
        tree: Some(tree(1, text)),
    })
}

fn tree(revision: u64, text: &str) -> UiTree {
    UiTree {
        revision,
        root: 1,
        nodes: vec![
            UiNode {
                id: 1,
                kind: UiNodeKind::Container,
                text: String::new(),
                style: UiStyle::Default,
                action: None,
                children: vec![2],
            },
            UiNode {
                id: 2,
                kind: UiNodeKind::Button,
                text: text.into(),
                style: UiStyle::Accent,
                action: Some("plugin.demo.drive".into()),
                children: vec![],
            },
        ],
    }
}

fn private_snapshot(revision: u64, dimensions_revision: u64, text: &str) -> PrivateSnapshot {
    PrivateSnapshot {
        revision,
        dimensions_revision,
        styles: vec![PrivateStyle {
            id: 1,
            style: UiStyle::Accent,
        }],
        cells: vec![PrivateCell {
            x: 0,
            y: 0,
            grapheme: text.into(),
            continuation: false,
            style: 1,
        }],
        cursor: Some(PrivateCursor { x: 0, y: 0 }),
        hit_regions: vec![PrivateHitRegion {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            action: "plugin.demo.private".into(),
        }],
    }
}

export!(GuestComponent);
