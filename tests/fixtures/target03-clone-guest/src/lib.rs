// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "wit", world: "editor-plugin" });

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use dust::plugin::host::{
    self, Cell, CellUpdate, CloseView, Dock, Edge, EventValue, ExecutionKind, Focus, Node,
    NodeKind, Notice, OpenView, Place, Placement, Rect, Role, Severity, SnapshotRequest, Tree,
    UiRequest,
};

#[derive(Clone, Copy)]
enum BadPrivate {
    WrongView,
    StaleDimensions,
    StaleSurface,
    OutOfBounds,
    WideOverflow,
    TrapAfterQueue,
}

thread_local! {
    static PRIVATE_TOKENS: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static PRIVATE_VIEWS: RefCell<BTreeMap<u64, char>> = const { RefCell::new(BTreeMap::new()) };
    static FAIL_PRIVATE_TOKENS: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static FAIL_PRIVATE_VIEWS: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static OVERSIZED_PRIVATE_TOKENS: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static OVERSIZED_PRIVATE_VIEWS: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static CALLBACK_OPEN_TOKENS: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static BAD_PRIVATE_TOKENS: RefCell<BTreeMap<u64, BadPrivate>> = const { RefCell::new(BTreeMap::new()) };
    static BAD_PRIVATE_VIEWS: RefCell<BTreeMap<u64, BadPrivate>> = const { RefCell::new(BTreeMap::new()) };
}

struct GuestComponent;

fn failure(message: impl Into<String>) -> host::Failure {
    host::Failure {
        code: host::ErrorCode::ProtocolFailure,
        message: message.into(),
        retryable: false,
    }
}

fn tree(label: &str) -> Tree {
    Tree {
        revision: 1,
        root: 1,
        nodes: vec![Node {
            id: 1,
            kind: NodeKind::Text,
            label: String::new(),
            text: label.into(),
            value: String::new(),
            value_revision: 0,
            role: Role::Accent,
            enabled: true,
            required: false,
            checked: false,
            action: None,
            items: Vec::new(),
            children: Vec::new(),
        }],
    }
}

impl Guest for GuestComponent {
    fn activate() -> Result<Vec<String>, host::Failure> {
        Ok(vec![
            "open".into(),
            "clone".into(),
            "openprivate".into(),
            "cloneprivate".into(),
            "move".into(),
            "tile".into(),
            "dock".into(),
            "fullscreen".into(),
            "hide".into(),
            "focus".into(),
            "close".into(),
            "unsupported".into(),
            "oversized".into(),
            "failafterqueue".into(),
            "trapafterqueue".into(),
            "callbackopen".into(),
            "openprivatefail".into(),
            "openprivateoversized".into(),
            "openprivatewrongview".into(),
            "openprivatestaledim".into(),
            "openprivatestalesurface".into(),
            "openprivateoutofbounds".into(),
            "openprivatewide".into(),
            "openprivatetrap".into(),
        ])
    }

    fn execute_command(invocation: Invocation) -> Result<(), host::Failure> {
        if invocation.kind != ExecutionKind::Command {
            return Err(failure("expected command invocation"));
        }
        let ui = invocation.ui.ok_or_else(|| failure("missing UI context"))?;
        let placement = match invocation.command.as_str() {
            "tile" => Some(Placement::Tiled(host::Tile {
                neighbor: None,
                axis: host::Axis::Horizontal,
                after: true,
                ratio: 500,
            })),
            "move" => Some(Placement::Floating(Rect {
                x: 9,
                y: 5,
                width: 36,
                height: 11,
            })),
            "dock" => Some(Placement::Docked(Dock {
                edge: Edge::Right,
                size: 19,
            })),
            "fullscreen" => Some(Placement::Fullscreen),
            "hide" => Some(Placement::Hidden),
            _ => None,
        };
        if let Some(placement) = placement {
            host::queue_ui(&UiRequest::Place(Place {
                window: ui.window,
                graph_revision: ui.graph_revision,
                tab: ui.tab,
                placement,
            }))?;
            return Ok(());
        }
        if invocation.command == "focus" {
            host::queue_ui(&UiRequest::Focus(Focus {
                window: ui.window,
                graph_revision: ui.graph_revision,
            }))?;
            return Ok(());
        }
        if invocation.command == "close" {
            host::queue_ui(&UiRequest::Close(CloseView {
                view: ui.view,
                graph_revision: ui.graph_revision,
            }))?;
            return Ok(());
        }
        if invocation.command == "oversized" {
            let mut oversized = tree("oversized");
            oversized.nodes.extend((2..=257).map(|id| Node {
                id,
                kind: NodeKind::Text,
                label: String::new(),
                text: "x".into(),
                value: String::new(),
                value_revision: 0,
                role: Role::Normal,
                enabled: true,
                required: false,
                checked: false,
                action: None,
                items: Vec::new(),
                children: Vec::new(),
            }));
            host::queue_ui(&UiRequest::Open(OpenView {
                content_type: "plugin.demo.panel".into(),
                content: None,
                title: "Oversized target tree".into(),
                graph_revision: ui.graph_revision,
                tab: ui.tab,
                placement: Placement::Floating(Rect {
                    x: 3,
                    y: 2,
                    width: 30,
                    height: 8,
                }),
                tree: Some(oversized),
            }))?;
            return Err(failure("oversized tree unexpectedly succeeded"));
        }
        let fail_after_queue = invocation.command == "failafterqueue";
        let trap_after_queue = invocation.command == "trapafterqueue";
        let callback_open = invocation.command == "callbackopen";
        let fail_private = invocation.command == "openprivatefail";
        let oversized_private = invocation.command == "openprivateoversized";
        let bad_private = match invocation.command.as_str() {
            "openprivatewrongview" => Some(BadPrivate::WrongView),
            "openprivatestaledim" => Some(BadPrivate::StaleDimensions),
            "openprivatestalesurface" => Some(BadPrivate::StaleSurface),
            "openprivateoutofbounds" => Some(BadPrivate::OutOfBounds),
            "openprivatewide" => Some(BadPrivate::WideOverflow),
            "openprivatetrap" => Some(BadPrivate::TrapAfterQueue),
            _ => None,
        };
        let (content, content_type, label) = match invocation.command.as_str() {
            "open" | "failafterqueue" | "trapafterqueue" | "callbackopen" => {
                (None, "plugin.demo.panel", Some("target-0.3 original"))
            }
            "clone" => (
                Some(ui.content),
                "plugin.demo.panel",
                Some("target-0.3 clone"),
            ),
            "openprivate"
            | "openprivatefail"
            | "openprivateoversized"
            | "openprivatewrongview"
            | "openprivatestaledim"
            | "openprivatestalesurface"
            | "openprivateoutofbounds"
            | "openprivatewide"
            | "openprivatetrap" => (None, "plugin.demo.private", None),
            "cloneprivate" => (Some(ui.content), "plugin.demo.private", None),
            "unsupported" => {
                host::queue_snapshot(SnapshotRequest {
                    source: ui.content,
                    start: 0,
                    end: 1,
                })?;
                return Err(failure("unsupported import unexpectedly succeeded"));
            }
            _ => return Err(failure("unknown command")),
        };
        let token = host::queue_ui(&UiRequest::Open(OpenView {
            content_type: content_type.into(),
            content,
            title: "Target 0.3 fixture".into(),
            graph_revision: ui.graph_revision,
            tab: ui.tab,
            placement: Placement::Floating(Rect {
                x: 3,
                y: 2,
                width: 30,
                height: 8,
            }),
            tree: label.map(tree),
        }))?;
        if label.is_none() {
            PRIVATE_TOKENS.with(|tokens| {
                tokens.borrow_mut().insert(token);
            });
        }
        if callback_open {
            CALLBACK_OPEN_TOKENS.with(|tokens| {
                tokens.borrow_mut().insert(token);
            });
        }
        if fail_private {
            FAIL_PRIVATE_TOKENS.with(|tokens| {
                tokens.borrow_mut().insert(token);
            });
        }
        if oversized_private {
            OVERSIZED_PRIVATE_TOKENS.with(|tokens| {
                tokens.borrow_mut().insert(token);
            });
        }
        if let Some(kind) = bad_private {
            BAD_PRIVATE_TOKENS.with(|tokens| {
                tokens.borrow_mut().insert(token, kind);
            });
        }
        if trap_after_queue {
            panic!("trap after accepted queue-ui");
        }
        if fail_after_queue {
            Err(failure("failure after accepted queue-ui"))
        } else {
            Ok(())
        }
    }

    fn handle_events(events: Vec<Event>) -> Result<(), host::Failure> {
        for event in events {
            match event.value {
                EventValue::UiResult((token, result)) => {
                    let receipt = result?;
                    let private = PRIVATE_TOKENS.with(|tokens| tokens.borrow_mut().remove(&token));
                    let fail_private =
                        FAIL_PRIVATE_TOKENS.with(|tokens| tokens.borrow_mut().remove(&token));
                    let oversized_private =
                        OVERSIZED_PRIVATE_TOKENS.with(|tokens| tokens.borrow_mut().remove(&token));
                    let bad_private =
                        BAD_PRIVATE_TOKENS.with(|tokens| tokens.borrow_mut().remove(&token));
                    if let Some(kind) = bad_private {
                        BAD_PRIVATE_VIEWS.with(|views| {
                            views.borrow_mut().insert(receipt.view, kind);
                        });
                    } else if fail_private {
                        FAIL_PRIVATE_VIEWS.with(|views| {
                            views.borrow_mut().insert(receipt.view);
                        });
                    } else if oversized_private {
                        OVERSIZED_PRIVATE_VIEWS.with(|views| {
                            views.borrow_mut().insert(receipt.view);
                        });
                    } else if private {
                        PRIVATE_VIEWS.with(|views| {
                            let mut views = views.borrow_mut();
                            let label = if views.is_empty() { 'A' } else { 'B' };
                            views.insert(receipt.view, label);
                        });
                    }
                    if CALLBACK_OPEN_TOKENS.with(|tokens| tokens.borrow_mut().remove(&token)) {
                        let attempt = host::queue_ui(&UiRequest::Open(OpenView {
                            content_type: "plugin.demo.panel".into(),
                            content: None,
                            title: "Forbidden callback open".into(),
                            graph_revision: receipt.graph_revision,
                            tab: 1,
                            placement: Placement::Floating(Rect {
                                x: 1,
                                y: 1,
                                width: 10,
                                height: 4,
                            }),
                            tree: Some(tree("forbidden")),
                        }));
                        if attempt.is_ok() {
                            return Err(failure("result-only open unexpectedly succeeded"));
                        }
                        host::notify(&Notice {
                            severity: Severity::Info,
                            message: "result-only-open-refused".into(),
                        })?;
                    }
                    host::notify(&Notice {
                        severity: Severity::Info,
                        message: format!(
                            "ui-result:{token}:{}:{}:{}:{}:{}",
                            receipt.content,
                            receipt.view,
                            receipt.window,
                            receipt.graph_revision,
                            receipt.view_revision
                        ),
                    })?;
                }
                EventValue::Dimensions(dimensions) => {
                    let fail =
                        FAIL_PRIVATE_VIEWS.with(|views| views.borrow().contains(&dimensions.view));
                    let oversized = OVERSIZED_PRIVATE_VIEWS
                        .with(|views| views.borrow().contains(&dimensions.view));
                    let bad = BAD_PRIVATE_VIEWS
                        .with(|views| views.borrow().get(&dimensions.view).copied());
                    let label = PRIVATE_VIEWS
                        .with(|views| views.borrow().get(&dimensions.view).copied())
                        .unwrap_or('X');
                    let cells = if oversized {
                        (0..=4_096)
                            .map(|_| Cell {
                                x: 0,
                                y: 0,
                                grapheme: "X".into(),
                                continuation: false,
                                role: Role::Accent,
                            })
                            .collect()
                    } else {
                        vec![Cell {
                            x: if matches!(bad, Some(BadPrivate::OutOfBounds)) {
                                dimensions.width
                            } else if matches!(bad, Some(BadPrivate::WideOverflow)) {
                                dimensions.width.saturating_sub(1)
                            } else {
                                0
                            },
                            y: 0,
                            grapheme: if matches!(bad, Some(BadPrivate::WideOverflow)) {
                                "界".into()
                            } else {
                                label.to_string()
                            },
                            continuation: false,
                            role: Role::Accent,
                        }]
                    };
                    host::queue_ui(&UiRequest::Cells(CellUpdate {
                        view: if matches!(bad, Some(BadPrivate::WrongView)) {
                            dimensions.view.saturating_add(1_000)
                        } else {
                            dimensions.view
                        },
                        dimensions_revision: if matches!(bad, Some(BadPrivate::StaleDimensions)) {
                            dimensions.revision.saturating_sub(1)
                        } else {
                            dimensions.revision
                        },
                        expected: if matches!(bad, Some(BadPrivate::StaleSurface)) {
                            1
                        } else {
                            0
                        },
                        revision: if matches!(bad, Some(BadPrivate::StaleSurface)) {
                            2
                        } else {
                            1
                        },
                        full: true,
                        dirty: Vec::new(),
                        cells,
                        hits: Vec::new(),
                        cursor: Some((0, 0)),
                    }))?;
                    if matches!(bad, Some(BadPrivate::TrapAfterQueue)) {
                        panic!("trap after queued private cells");
                    }
                    if fail {
                        return Err(failure("failure after queued private cells"));
                    }
                    host::notify(&Notice {
                        severity: Severity::Info,
                        message: format!(
                            "dimensions:{}:{}:{}:{}",
                            dimensions.view,
                            dimensions.revision,
                            dimensions.width,
                            dimensions.height
                        ),
                    })?;
                }
                _ => return Err(failure("unexpected target event")),
            }
        }
        Ok(())
    }

    fn deactivate() -> Result<(), host::Failure> {
        Ok(())
    }
}

export!(GuestComponent);
