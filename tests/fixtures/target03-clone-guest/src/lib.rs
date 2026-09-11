// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "wit", world: "editor-plugin" });

use dust::plugin::host::{
    self, EventValue, ExecutionKind, Node, NodeKind, Notice, OpenView, Placement, Rect, Role,
    Severity, SnapshotRequest, Tree, UiRequest,
};

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
        Ok(vec!["open".into(), "clone".into(), "unsupported".into()])
    }

    fn execute_command(invocation: Invocation) -> Result<(), host::Failure> {
        if invocation.kind != ExecutionKind::Command {
            return Err(failure("expected command invocation"));
        }
        let ui = invocation.ui.ok_or_else(|| failure("missing UI context"))?;
        let (content, label) = match invocation.command.as_str() {
            "open" => (None, "target-0.3 original"),
            "clone" => (Some(ui.content), "target-0.3 clone"),
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
        host::queue_ui(&UiRequest::Open(OpenView {
            content_type: "plugin.demo.panel".into(),
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
            tree: Some(tree(label)),
        }))?;
        Ok(())
    }

    fn handle_events(events: Vec<Event>) -> Result<(), host::Failure> {
        for event in events {
            if let EventValue::UiResult((token, result)) = event.value {
                let receipt = result?;
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
        }
        Ok(())
    }

    fn deactivate() -> Result<(), host::Failure> {
        Ok(())
    }
}

export!(GuestComponent);
