// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "wit", world: "editor-plugin" });

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use dust::plugin::host::{
    self, Cell, CellUpdate, EventValue, ExecutionKind, Node, NodeKind, Notice, OpenView, Placement,
    Rect, Role, Severity, SnapshotRequest, Tree, UiRequest,
};

thread_local! {
    static PRIVATE_TOKENS: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static PRIVATE_VIEWS: RefCell<BTreeMap<u64, char>> = const { RefCell::new(BTreeMap::new()) };
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
            "unsupported".into(),
            "oversized".into(),
        ])
    }

    fn execute_command(invocation: Invocation) -> Result<(), host::Failure> {
        if invocation.kind != ExecutionKind::Command {
            return Err(failure("expected command invocation"));
        }
        let ui = invocation.ui.ok_or_else(|| failure("missing UI context"))?;
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
        let (content, content_type, label) = match invocation.command.as_str() {
            "open" => (None, "plugin.demo.panel", Some("target-0.3 original")),
            "clone" => (
                Some(ui.content),
                "plugin.demo.panel",
                Some("target-0.3 clone"),
            ),
            "openprivate" => (None, "plugin.demo.private", None),
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
        Ok(())
    }

    fn handle_events(events: Vec<Event>) -> Result<(), host::Failure> {
        for event in events {
            match event.value {
                EventValue::UiResult((token, result)) => {
                    let receipt = result?;
                    let private = PRIVATE_TOKENS.with(|tokens| tokens.borrow_mut().remove(&token));
                    if private {
                        PRIVATE_VIEWS.with(|views| {
                            let mut views = views.borrow_mut();
                            let label = if views.is_empty() { 'A' } else { 'B' };
                            views.insert(receipt.view, label);
                        });
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
                    let label = PRIVATE_VIEWS
                        .with(|views| views.borrow().get(&dimensions.view).copied())
                        .ok_or_else(|| failure("dimensions for unknown private view"))?;
                    host::queue_ui(&UiRequest::Cells(CellUpdate {
                        view: dimensions.view,
                        dimensions_revision: dimensions.revision,
                        expected: 0,
                        revision: 1,
                        full: true,
                        dirty: Vec::new(),
                        cells: vec![Cell {
                            x: 0,
                            y: 0,
                            grapheme: label.to_string(),
                            continuation: false,
                            role: Role::Accent,
                        }],
                        hits: Vec::new(),
                        cursor: Some((0, 0)),
                    }))?;
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
