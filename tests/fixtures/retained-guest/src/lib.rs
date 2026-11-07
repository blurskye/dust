// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "wit", world: "editor-plugin" });

use dust::plugin::host;
mod surface;

use dust::plugin::host::CheckedWindowRequest;

use dust::plugin::host::ConfirmationAnswer;

use dust::plugin::host::ConfirmationOutcome;

use dust::plugin::host::ConfirmationRequest;

use dust::plugin::host::OpenWindowRequest;

use dust::plugin::host::PatchPrivateRequest;

use dust::plugin::host::PlaceWindowRequest;

use dust::plugin::host::PrivateCell;

use dust::plugin::host::PrivateCursor;

use dust::plugin::host::PrivateHitRegion;

use dust::plugin::host::PrivatePatch;

use dust::plugin::host::PrivateRect;

use dust::plugin::host::PrivateSnapshot;

use dust::plugin::host::PickerProviderChunk;

use dust::plugin::host::PickerProviderItem;

use dust::plugin::host::PrivateStyle;

use dust::plugin::host::ProviderTerminal;

use dust::plugin::host::ProviderTerminalKind;

use dust::plugin::host::ReplacePrivateRequest;

use dust::plugin::host::ReplaceUiRequest;

use dust::plugin::host::StorageCasRequest;

use dust::plugin::host::StorageDeleteRequest;

use dust::plugin::host::StorageListRequest;

use dust::plugin::host::StorageRequest;

use dust::plugin::host::UiNode;

use dust::plugin::host::UiNodeKind;

use dust::plugin::host::UiRequest;

use dust::plugin::host::UiStyle;

use dust::plugin::host::UiTree;

use dust::plugin::host::VirtualDocumentFull;

use dust::plugin::host::VirtualDocumentPatch;

use dust::plugin::host::VirtualDocumentPatchSet;

use dust::plugin::host::VirtualDocumentProposal;

use dust::plugin::host::VirtualDocumentProposalKind;

use dust::plugin::host::VirtualDocumentRequestKind;

use dust::plugin::host::WindowPlacement;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static FILTER: AtomicU8 = AtomicU8::new(0);
static PHASE: AtomicU8 = AtomicU8::new(0);
static PRIVATE_PHASE: AtomicU8 = AtomicU8::new(0);
static PRIVATE_HANDLE: AtomicU64 = AtomicU64::new(0);
static PRIVATE_DIMENSIONS: AtomicU64 = AtomicU64::new(0);
static STORAGE_PHASE: AtomicU8 = AtomicU8::new(0);
static STORAGE_REVISION: AtomicU64 = AtomicU64::new(0);
static STORAGE_TOKEN: AtomicU64 = AtomicU64::new(0);
static PROVIDER_PHASE: AtomicU8 = AtomicU8::new(0);
static VIRTUAL_PHASE: AtomicU8 = AtomicU8::new(0);
static CONFIRMATION_HANDLE: AtomicU64 = AtomicU64::new(0);
static CONFIRMATION_REVISION: AtomicU64 = AtomicU64::new(1);
static CONFIRMATION_PROBE_ORDINAL: AtomicU64 = AtomicU64::new(0);

static SYSTEM_PROCESS: AtomicU64 = AtomicU64::new(0);
static SYSTEM_RESULTS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

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
            "confirmation".into(),
            "confirmationprobe".into(),
        ])
    }

    fn execute_command(command: String, call: host::CommandCall) -> Result<String, String> {
        if call.command == "edit.insert_text" || call.command.starts_with("lsp.incoming.") {
            if call.command.starts_with("lsp.incoming.") && FILTER.load(Ordering::SeqCst) == 1 {
                return Ok("filtered".into());
            }
            if !call.after {
                host::call_next(call.pending.ok_or("missing wrapper id")?)?;
            } else if let Some(value) = &call.previous {
                host::set_command_result(value)?;
            }
            return Ok(if call.after {
                "wrapper:after"
            } else {
                "wrapper:before"
            }
            .into());
        }
        if let Some(event) = field_text(&call.arguments, "event") {
            if event == "system" {
                let events = host::take_system_events();
                let mut results = SYSTEM_RESULTS.lock().unwrap();
                for event in events {
                    results.push_str(&format!("{:?}\n", event.value));
                }
                host::show_message(&results)?;
                return Ok(results.clone());
            }
            if let Some(result) = surface::event(&event, &call) { return result; }
            let events = host::take_editor_events();
            let replies = host::take_command_replies();
            let http = host::take_http_responses();
            return Ok(format!(
                "callback:{event}:events={}:replies={}:http={}",
                events.as_ref().map_or(0, |batch| batch.records.len()),
                replies.len(),
                http.len()
            ));
        }
        if field_text(&call.arguments, "source").is_some() {
            let snapshot = host::current_buffer()?;
            let view = host::current_view().ok_or("no view")?;
            let request = field_integer(&call.arguments, "request").ok_or("no request")? as u64;
            host::publish_completions(&host::CompletionBatch {
                source: "fixture".into(),
                request,
                buffer: snapshot.buffer,
                revision: snapshot.revision,
                items: vec![host::CompletionItem {
                    label: "plugin-value".into(),
                    detail: Some("fixture source".into()),
                    start: view.cursor,
                    end: view.cursor,
                    text: "plugin-value".into(),
                    snippet: false,
                }],
                incomplete: false,
            })?;
            return Ok("completion:published".into());
        }
        if let Some(action) = field_text(&call.arguments, "abi") {
            return drive_abi(&action, &call);
        }
        let arguments: Vec<String> = match call.arguments.nodes.last() {
            Some(host::ValueNode::Object(fields)) => fields
                .iter()
                .filter_map(|field| match &call.arguments.nodes[field.value as usize] {
                    host::ValueNode::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let graph = host::current_ui_revision()
            .or_else(|| arguments.first().and_then(|value| value.parse().ok()))
            .unwrap_or(1);
        if command == "trap" {
            let _ = host::queue_ui_request(&open_retained(graph, "Trap", "trap"));
            let _ =
                host::queue_storage_request(&StorageRequest::CompareAndSet(StorageCasRequest {
                    key: "trap.value".into(),
                    expected_revision: None,
                    value: b"never".to_vec(),
                }));
            if let Some(request) = host::take_virtual_document_requests().first() {
                let _ = host::queue_virtual_document_proposal(&VirtualDocumentProposal {
                    request: request.request,
                    document: request.document,
                    cancellation_id: request.cancellation_id,
                    kind: VirtualDocumentProposalKind::Full(VirtualDocumentFull {
                        expected_revision: request.current_revision,
                        revision: request.current_revision.unwrap_or(0) + 1,
                        title: "Fixture help".into(),
                        language: "markdown".into(),
                        text: "never".into(),
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
                    request: request.request,
                    document: request.document,
                    cancellation_id: request.cancellation_id,
                    kind: VirtualDocumentProposalKind::Full(VirtualDocumentFull {
                        expected_revision: request.current_revision,
                        revision: request.current_revision.unwrap_or(0) + 1,
                        title: "Fixture help".into(),
                        language: "markdown".into(),
                        text: "never spin".into(),
                    }),
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
        if command == "confirmation" {
            return drive_confirmation(graph);
        }
        if command == "confirmationprobe" {
            let results = host::take_confirmation_results();
            if !results.is_empty() {
                return Ok(format!("confirmationprobe:result-only:{}", results.len()));
            }
            let ordinal = CONFIRMATION_PROBE_ORDINAL.fetch_add(1, Ordering::SeqCst) + 1;
            let outcome = match host::queue_confirmation(&ConfirmationRequest {
                title: "Probe confirmation capacity".into(),
                message: "This request tests the host queue limit".into(),
                default_answer: ConfirmationAnswer::No,
            }) {
                Ok(_) => "accepted",
                Err(error) => match error.code {
                    host::ConfirmationErrorCode::QueueFull => "queue-full",
                    _ => "other-error",
                },
            };
            let marker = format!("confirmationprobe:{ordinal}:{outcome}");
            host::show_message(&marker)?;
            return Ok(marker);
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

fn drive_confirmation(graph: u64) -> Result<String, String> {
    let results = host::take_confirmation_results();
    if let Some(result) = results.last() {
        let label = match &result.outcome {
            ConfirmationOutcome::Answered(ConfirmationAnswer::Yes) => "Confirmed: Yes",
            ConfirmationOutcome::Answered(ConfirmationAnswer::No) => "Confirmed: No",
            ConfirmationOutcome::Answered(ConfirmationAnswer::Cancelled) => "Confirmed: Cancelled",
            ConfirmationOutcome::Failed(error) => {
                return Ok(format!("confirmation:{:?}", error.code));
            }
        };
        let events = host::take_ui_events();
        let event = events.iter().rev().find(|event| event.handle.is_some());
        let handle = event
            .and_then(|event| event.handle)
            .unwrap_or_else(|| CONFIRMATION_HANDLE.load(Ordering::SeqCst));
        let revision = event
            .and_then(|event| event.tree_revision)
            .unwrap_or_else(|| CONFIRMATION_REVISION.load(Ordering::SeqCst));
        if host::propose_edits(1, 1, &[]).is_ok() {
            return Err("confirmation result accepted edit proposal".into());
        }
        if host::queue_ui_request(&open_confirmation_view(graph)).is_ok() {
            return Err("confirmation result accepted new window".into());
        }
        if host::queue_ui_request(&UiRequest::FocusWindow(CheckedWindowRequest {
            handle,
            expected_graph_revision: graph,
        }))
        .is_ok()
        {
            return Err("confirmation result accepted focus".into());
        }
        if host::queue_storage_request(&StorageRequest::PrefixList(StorageListRequest {
            prefix: String::new(),
            maximum: 1,
        }))
        .is_ok()
        {
            return Err("confirmation result accepted storage".into());
        }
        host::queue_ui_request(&UiRequest::ReplaceUi(ReplaceUiRequest {
            handle,
            expected_tree_revision: revision,
            tree: confirmation_tree(revision + 1, label),
        }))
        .map_err(|error| format!("confirmation-ui:{:?}", error.code))?;
        return Ok(label.into());
    }
    let events = host::take_ui_events();
    if let Some(event) = events.iter().rev().find(|event| event.handle.is_some()) {
        CONFIRMATION_HANDLE.store(event.handle.unwrap_or(0), Ordering::SeqCst);
        CONFIRMATION_REVISION.store(event.tree_revision.unwrap_or(1), Ordering::SeqCst);
    } else {
        host::queue_ui_request(&open_confirmation_view(graph))
            .map_err(|error| format!("confirmation-ui:{:?}", error.code))?;
        return Ok("confirmation:view-opened".into());
    }
    let request = host::queue_confirmation(&ConfirmationRequest {
        title: "Run custom action?".into(),
        message: "Update the plugin-owned view with the selected result?".into(),
        default_answer: ConfirmationAnswer::No,
    });
    match request {
        Ok(request) => Ok(format!("confirmation:requested:{request}")),
        Err(error) => Ok(format!("confirmation:{:?}", error.code)),
    }
}

fn drive_virtual_document() -> Result<String, String> {
    let requests = host::take_virtual_document_requests();
    let request = requests
        .last()
        .ok_or_else(|| "virtual:no-request".to_string())?;
    let phase = VIRTUAL_PHASE.load(Ordering::SeqCst);
    let kind = match request.kind {
        VirtualDocumentRequestKind::Open => {
            VirtualDocumentProposalKind::Full(VirtualDocumentFull {
                expected_revision: None,
                revision: 1,
                title: "Fixture help".into(),
                language: "markdown".into(),
                text: "# Héllo\nvirtual document".into(),
            })
        }
        VirtualDocumentRequestKind::Refresh => {
            VirtualDocumentProposalKind::Patches(VirtualDocumentPatchSet {
                expected_revision: request.current_revision.unwrap_or(1),
                revision: request.current_revision.unwrap_or(1) + 1,
                patches: vec![VirtualDocumentPatch {
                    start: 0,
                    end: 1,
                    replacement: "##".into(),
                }],
            })
        }
        VirtualDocumentRequestKind::CloseNotice => VirtualDocumentProposalKind::CloseAcknowledged,
    };
    host::queue_virtual_document_proposal(&VirtualDocumentProposal {
        request: request.request,
        document: request.document,
        cancellation_id: request.cancellation_id,
        kind,
    })
    .map_err(|error| format!("virtual:{:?}", error.code))?;
    VIRTUAL_PHASE.store(phase.saturating_add(1), Ordering::SeqCst);
    Ok(format!("virtual:{phase}"))
}

fn drive_provider() -> Result<String, String> {
    let requests = host::take_picker_provider_requests();
    let request = requests
        .last()
        .ok_or_else(|| "provider:no-request".to_string())?;
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
    let phase = STORAGE_PHASE.load(Ordering::SeqCst);
    let mut revision = STORAGE_REVISION.load(Ordering::SeqCst);
    if phase == 0 {
        if !results.is_empty() {
            return Err("storage:0:unexpected-results".into());
        }
    } else {
        let [result] = results.as_slice() else {
            return Err(format!("storage:{phase}:expected-one-result"));
        };
        let valid_error = if phase == 3 {
            result
                .error
                .as_ref()
                .is_some_and(|error| error.code == host::StorageErrorCode::StaleRevision)
        } else {
            result.error.is_none()
        };
        let valid_content = match phase {
            1 => {
                result.value.is_none()
                    && result.revision.is_some_and(|revision| revision > 0)
                    && result.entries.is_empty()
            }
            2 => {
                result.value.as_deref() == Some(b"value".as_slice())
                    && result.revision == Some(revision)
                    && result.entries.is_empty()
            }
            3 | 6 => {
                result.value.is_none() && result.revision.is_none() && result.entries.is_empty()
            }
            4 => {
                result.value.is_none()
                    && result.revision.is_none()
                    && matches!(result.entries.as_slice(), [entry]
                    if entry.key == "item.one"
                        && entry.value == b"value"
                        && entry.revision == revision)
            }
            5 => {
                result.value.is_none()
                    && result.revision == Some(revision)
                    && result.entries.is_empty()
            }
            _ => false,
        };
        if result.token != STORAGE_TOKEN.load(Ordering::SeqCst)
            || result.truncated
            || !valid_error
            || !valid_content
        {
            return Err(format!("storage:{phase}:invalid-result"));
        }
        if phase == 1 {
            revision = result.revision.unwrap();
            STORAGE_REVISION.store(revision, Ordering::SeqCst);
        }
    }
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
        _ => return Ok("storage:done:missing=true".into()),
    };
    let token = host::queue_storage_request(&request)
        .map_err(|error| format!("storage:{:?}", error.code))?;
    STORAGE_TOKEN.store(token, Ordering::SeqCst);
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

fn open_confirmation_view(graph: u64) -> UiRequest {
    UiRequest::OpenWindow(OpenWindowRequest {
        expected_graph_revision: graph,
        content_type: "plugin.demo.confirmation-panel".into(),
        title: "Confirmation example".into(),
        placement: WindowPlacement::Floating,
        x: 2,
        y: 2,
        width: 30,
        height: 8,
        size: 8,
        tree: Some(confirmation_tree(1, "Request confirmation")),
    })
}

fn confirmation_tree(revision: u64, text: &str) -> UiTree {
    let mut tree = tree(revision, text);
    tree.nodes[1].action = Some("plugin.demo.confirmation".into());
    tree
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

fn field_text(value: &host::CommandValue, name: &str) -> Option<String> {
    let host::ValueNode::Object(fields) = value.nodes.last()? else {
        return None;
    };
    let field = fields.iter().find(|field| field.name == name)?;
    match &value.nodes[field.value as usize] {
        host::ValueNode::Text(text) => Some(text.clone()),
        _ => None,
    }
}

fn drive_abi(action: &str, call: &host::CommandCall) -> Result<String, String> {
    if let Some(action) = action.strip_prefix("surface-") { return surface::action(action); }
    match action {
        "system-results" => {
            let results = SYSTEM_RESULTS.lock().unwrap().clone();
            host::show_message(&results)?;
            return Ok(results);
        }
        "system-clear" => SYSTEM_RESULTS.lock().unwrap().clear(),
        "system-process" | "system-sleep" | "system-process-escape" => {
            let process = host::start_process(&host::ProcessStart {
                program: "sh".into(),
                arguments: vec!["-c".into(), if action == "system-sleep" { "sleep 60" } else { "printf ready; IFS= read -r line; printf '%s' \"$line\"; printf '%s' \"$DUST_SYSTEM_TEST\" >&2; cat; exit 7" }.into()],
                cwd: if action == "system-process-escape" { ".." } else { "." }.into(),
                environment: vec![("DUST_SYSTEM_TEST".into(), "environment".into())],
                stdin: Some(b"initial\n".to_vec()), handler: "drive".into(),
            })?;
            SYSTEM_PROCESS.store(process, Ordering::SeqCst);
        }
        "system-input" => {
            host::write_process_stdin(&host::ProcessInput {
                process: SYSTEM_PROCESS.load(Ordering::SeqCst),
                bytes: b"later".to_vec(),
                close: true,
            })?;
        }
        "system-kill" => {
            host::kill_process(SYSTEM_PROCESS.load(Ordering::SeqCst))?;
        }
        "system-write" | "system-read" | "system-mkdir" | "system-list" | "system-stat"
        | "system-rename" | "system-delete" | "system-escape" => {
            let operation = match action {
                "system-write" => host::FileOperation::Write(host::FileWrite {
                    path: "system.txt".into(),
                    bytes: b"new contents\n".to_vec(),
                }),
                "system-read" => host::FileOperation::Read("system.txt".into()),
                "system-mkdir" => host::FileOperation::CreateDirectory("system-dir".into()),
                "system-list" => host::FileOperation::ListDirectory(".".into()),
                "system-stat" => host::FileOperation::Stat("system.txt".into()),
                "system-rename" => host::FileOperation::Rename(host::FileRename {
                    source: "system.txt".into(),
                    target: "system-renamed.txt".into(),
                }),
                "system-delete" => host::FileOperation::Delete("system-renamed.txt".into()),
                _ => host::FileOperation::Read("system-link/secret".into()),
            };
            host::request_file(&operation, "drive")?;
        }
        "system-query" | "system-query-range" => {
            host::query_syntax(
                &host::SyntaxQuery {
                    buffer: host::current_buffer()?.buffer,
                    query: "(identifier) @name".into(),
                    range: if action == "system-query-range" {
                        Some(host::ByteRange { start: 9, end: 13 })
                    } else {
                        None
                    },
                },
                "drive",
            )?;
        }
        "system-node" => {
            host::node_at_position(host::current_buffer()?.buffer, 4, "drive")?;
        }
        "echo" => {
            host::set_command_result(&call.arguments)?;
        }
        "register" => {
            host::register_command(&host::CommandDefinition {
                name: "echo".into(),
                title: "Echo structured values".into(),
                handler: "drive".into(),
                arguments: vec![
                    host::ArgumentField {
                        name: "abi".into(),
                        kind: host::ArgumentKind::Text,
                        required: true,
                    },
                    host::ArgumentField {
                        name: "value".into(),
                        kind: host::ArgumentKind::Object,
                        required: false,
                    },
                ],
                result_kind: Some(host::ArgumentKind::Object),
                access: vec![],
            })?;
        }
        "wrap" => {
            host::wrap_command(&host::CommandWrapper {
                command: "edit.insert_text".into(),
                handler: "drive".into(),
                order: 0,
            })?;
        }
        "filter-pass" | "filter-drop" => {
            FILTER.store(
                if action == "filter-drop" { 1 } else { 0 },
                Ordering::SeqCst,
            );
            host::filter_lsp(&host::LspFilter {
                method: "window/showMessage".into(),
                handler: "drive".into(),
                order: 0,
            })?;
        }
        "run" => {
            host::run_command(&host::CommandCall {
                command: "edit.insert_text".into(),
                arguments: host::CommandValue {
                    nodes: vec![
                        host::ValueNode::Text("run".into()),
                        host::ValueNode::Object(vec![host::ValueField {
                            name: "text".into(),
                            value: 0,
                        }]),
                    ],
                },
                buffer: call.buffer,
                view: call.view,
                count: call.count,
                register: call.register,
                pending: None,
                after: false,
                previous: None,
            })?;
        }
        "marks-read" => {
            let snapshot = host::current_buffer()?;
            let marks = host::read_marks(snapshot.buffer, "points")?;
            return Ok(format!(
                "marks:{}:{}",
                marks.len(),
                marks.first().map_or(0, |mark| mark.start)
            ));
        }
        "error" => {
            host::show_message("must-not-publish")?;
            return Err("ordinary guest error".into());
        }
        "next" => {
            host::call_next(call.pending.ok_or("missing pending command")?)?;
        }
        "mode" => {
            host::enter_mode(&host::NamedMode {
                name: "review".into(),
                label: "REVIEW".into(),
                cursor: host::ModeCursor::Underline,
                layer: host::KeyLayer {
                    name: "review".into(),
                    bindings: Vec::new(),
                },
                base: Some(host::BaseMode::Normal),
                capture: None,
            })?;
        }
        "capture" => {
            host::enter_mode(&host::NamedMode {
                name: "capture".into(),
                label: "CAPTURE".into(),
                cursor: host::ModeCursor::Bar,
                layer: host::KeyLayer {
                    name: "capture".into(),
                    bindings: Vec::new(),
                },
                base: None,
                capture: Some("plugin.demo.drive".into()),
            })?;
        }
        "marks" => {
            let snapshot = host::current_buffer()?;
            host::replace_marks(&host::MarkSet {
                buffer: snapshot.buffer,
                revision: snapshot.revision,
                namespace: "points".into(),
                marks: vec![host::Mark {
                    id: 1,
                    start: 0,
                    end: 0,
                    start_gravity: host::Gravity::Right,
                    end_gravity: host::Gravity::Right,
                    decoration: None,
                }],
            })?;
        }
        "drawing" | "drawing-clear" => {
            let snapshot = host::current_buffer()?;
            let marks = if action == "drawing-clear" { Vec::new() } else {
                [host::VirtualTextPosition::Inline, host::VirtualTextPosition::EndOfLine, host::VirtualTextPosition::Overlay].into_iter().enumerate().map(|(index, position)| host::Mark {
                    id: index as u64, start: index as u64, end: index as u64 + 1,
                    start_gravity: host::Gravity::Right, end_gravity: host::Gravity::Left,
                    decoration: Some(host::MarkDecoration {
                        highlight: Some("@keyword".into()),
                        virtual_text: Some(host::VirtualText { text: ["before ", " after", "Z"][index].into(), style: "Comment".into(), position }),
                        sign: (index == 0).then_some(host::StyledText { text: "!".into(), style: "DiagnosticWarn".into() }),
                    }),
                }).collect()
            };
            host::replace_marks(&host::MarkSet { buffer: snapshot.buffer, revision: snapshot.revision, namespace: "drawing".into(), marks })?;
        }
        "diagnostics" | "diagnostics-clear" => {
            let snapshot = host::current_buffer()?;
            let diagnostics = if action == "diagnostics-clear" { Vec::new() } else { vec![host::Diagnostic {
                start: 1, end: 2, severity: host::DiagnosticSeverity::Warning,
                message: "check this value".into(), source: "fixture".into(),
            }] };
            host::publish_diagnostics(&host::DiagnosticSet { buffer: snapshot.buffer, revision: snapshot.revision, namespace: "lint".into(), diagnostics })?;
        }
        "events" => {
            host::subscribe_events("drive")?;
        }
        "completion" => {
            host::register_completion_source(&host::CompletionSource {
                name: "fixture".into(),
                handler: "drive".into(),
                inline: true,
            })?;
        }
        "timer" => {
            host::start_timer(&host::TimerRequest {
                milliseconds: 1,
                handler: "drive".into(),
            })?;
        }
        "layer" => {
            host::push_key_layer(&host::KeyLayer {
                name: "fixture".into(),
                bindings: Vec::new(),
            })?;
        }
        "http-denied" => {
            host::request_http(&host::HttpRequest {
                url: "https://not-granted.invalid/".into(),
                method: "GET".into(),
                headers: Vec::new(),
                body: Vec::new(),
                handler: "drive".into(),
            })?;
        }
        _ => return Err("unknown ABI action".into()),
    }
    Ok(format!("abi:{action}"))
}

fn field_integer(value: &host::CommandValue, name: &str) -> Option<i64> {
    let host::ValueNode::Object(fields) = value.nodes.last()? else {
        return None;
    };
    let field = fields.iter().find(|field| field.name == name)?;
    match &value.nodes[field.value as usize] {
        host::ValueNode::Integer(value) => Some(*value),
        _ => None,
    }
}
