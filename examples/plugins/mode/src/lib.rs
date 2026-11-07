//! A named navigation mode with its own cursor, keys and statusline segment.
// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "../../../wit", world: "editor-plugin" });
use dust::plugin::host;

struct Review;
fn empty() -> host::CommandValue {
    host::CommandValue {
        nodes: vec![host::ValueNode::Object(vec![])],
    }
}
impl Guest for Review {
    fn activate() -> Result<Vec<String>, String> {
        Ok(["enter", "leave", "left", "right"].map(String::from).into())
    }
    fn execute_command(command: String, call: host::CommandCall) -> Result<String, String> {
        if let Some(host::ValueNode::Object(fields)) = call.arguments.nodes.last()
            && fields.iter().any(|field| field.name == "event")
        {
            for reply in host::take_command_replies() {
                if let Some(error) = reply.error {
                    return Err(error);
                }
            }
            return Ok(String::new());
        }
        match command.as_str() {
            "enter" => {
                host::enter_mode(&host::NamedMode {
                    name: "review".into(),
                    label: "REVIEW".into(),
                    cursor: host::ModeCursor::Underline,
                    base: Some(host::BaseMode::Normal),
                    capture: None,
                    layer: host::KeyLayer {
                        name: "review".into(),
                        bindings: [
                            ("h", "left"),
                            ("l", "right"),
                            ("q", "leave"),
                            ("escape", "leave"),
                        ]
                        .map(|(keys, command)| host::KeyBinding {
                            keys: keys.into(),
                            command: format!("plugin.mode.{command}"),
                            arguments: empty(),
                        })
                        .into(),
                    },
                })?;
                host::set_chrome_segment(&host::ChromeSegment {
                    name: "review".into(),
                    location: host::SegmentLocation::Statusline,
                    order: 10,
                    text: vec![host::StyledText {
                        text: "REVIEW · h/l move · q leave".into(),
                        style: "accent".into(),
                    }],
                    command: None,
                })?;
            }
            "leave" => {
                host::leave_mode()?;
                host::remove_chrome_segment(host::SegmentLocation::Statusline, "review")?;
            }
            direction => {
                host::run_command(&host::CommandCall {
                    command: "view.cursor.move".into(),
                    arguments: host::CommandValue {
                        nodes: vec![
                            host::ValueNode::Text(direction.into()),
                            host::ValueNode::Object(vec![host::ValueField {
                                name: "motion".into(),
                                value: 0,
                            }]),
                        ],
                    },
                    buffer: None,
                    view: None,
                    count: None,
                    register: None,
                    pending: None,
                    after: false,
                    previous: None,
                })?;
                // Command replies are acknowledgements, not additional movement.
                host::take_command_replies();
            }
        }
        Ok(String::new())
    }
}
export!(Review);
