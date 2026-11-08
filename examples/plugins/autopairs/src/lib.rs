//! Pair insertion, skipping and deletion as wrappers around ordinary typing.
// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "../../../wit", world: "editor-plugin" });
use dust::plugin::host;

struct Pairs;
fn field<'a>(value: &'a host::CommandValue, name: &str) -> Option<&'a host::ValueNode> {
    let host::ValueNode::Object(fields) = value.nodes.last()? else {
        return None;
    };
    fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| &value.nodes[field.value as usize])
}
fn record(fields: Vec<(&str, host::ValueNode)>) -> host::CommandValue {
    let mut nodes = Vec::new();
    let fields = fields
        .into_iter()
        .map(|(name, value)| {
            let index = nodes.len() as u32;
            nodes.push(value);
            host::ValueField {
                name: name.into(),
                value: index,
            }
        })
        .collect();
    nodes.push(host::ValueNode::Object(fields));
    host::CommandValue { nodes }
}
fn closer(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '\'' | '"' | '`' => Some(open),
        _ => None,
    }
}
impl Guest for Pairs {
    fn activate() -> Result<Vec<String>, String> {
        Ok(vec!["enable".into(), "type".into()])
    }
    fn execute_command(command: String, call: host::CommandCall) -> Result<String, String> {
        if field(&call.arguments, "event").is_some() {
            for reply in host::take_command_replies() {
                if let Some(error) = reply.error {
                    return Err(error);
                }
            }
            return Ok(String::new());
        }
        if command == "enable" {
            for command in ["edit.insert_text", "edit.backspace"] {
                host::wrap_command(&host::CommandWrapper {
                    command: command.into(),
                    handler: "type".into(),
                    order: 0,
                })?;
            }
            return Ok(String::new());
        }
        if call.after {
            return Ok(String::new());
        }
        let snapshot = host::current_buffer()?;
        let cursor = host::current_view().ok_or("no view")?.cursor as usize;
        let next = snapshot.utf8[cursor..].chars().next();
        let before = snapshot.utf8[..cursor].chars().next_back();
        let edit = if call.command == "edit.backspace" {
            before
                .filter(|open| closer(*open).is_some_and(|close| Some(close) == next))
                .map(|open| {
                    (
                        cursor - open.len_utf8(),
                        cursor + next.unwrap().len_utf8(),
                        String::new(),
                        cursor - open.len_utf8(),
                    )
                })
        } else if let Some(host::ValueNode::Text(text)) = field(&call.arguments, "text") {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if Some(c) == next && ")]}'\"`".contains(c) => {
                    Some((cursor, cursor, String::new(), cursor + c.len_utf8()))
                }
                (Some(c), None) => closer(c)
                    .map(|close| (cursor, cursor, format!("{c}{close}"), cursor + c.len_utf8())),
                _ => None,
            }
        } else {
            None
        };
        if let Some((start, end, text, cursor)) = edit {
            host::run_command(&host::CommandCall {
                command: "edit.apply".into(),
                arguments: record(vec![
                    (
                        "revision",
                        host::ValueNode::Integer(snapshot.revision as i64),
                    ),
                    ("start", host::ValueNode::Integer(start as i64)),
                    ("end", host::ValueNode::Integer(end as i64)),
                    ("text", host::ValueNode::Text(text)),
                    ("cursor", host::ValueNode::Integer(cursor as i64)),
                ]),
                buffer: Some(snapshot.buffer),
                view: call.view,
                count: None,
                register: None,
                pending: None,
                after: false,
                previous: None,
            })?;
        } else {
            host::call_next(call.pending.ok_or("missing continuation")?)?;
        }
        Ok(String::new())
    }
}
export!(Pairs);
