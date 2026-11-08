//! Rust function text objects and a surround operator using syntax captures and native edits.
// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "../../../wit", world: "editor-plugin" });
use dust::plugin::host;
use std::cell::{Cell, RefCell};
thread_local! {
    static QUERY: RefCell<Option<(u64, host::Snapshot)>> = const { RefCell::new(None) };
    static RETRY: Cell<u32> = const { Cell::new(0) };
    static TIMER: Cell<Option<u64>> = const { Cell::new(None) };
}

struct Functions;
fn field<'a>(value: &'a host::CommandValue, name: &str) -> Option<&'a host::ValueNode> {
    let host::ValueNode::Object(fields) = value.nodes.last()? else {
        return None;
    };
    fields
        .iter()
        .find(|f| f.name == name)
        .map(|f| &value.nodes[f.value as usize])
}
fn text<'a>(value: &'a host::CommandValue, name: &str) -> Option<&'a str> {
    match field(value, name)? {
        host::ValueNode::Text(text) => Some(text),
        _ => None,
    }
}
fn set(value: &mut host::CommandValue, name: &str, node: host::ValueNode) {
    let Some(host::ValueNode::Object(mut fields)) = value.nodes.pop() else {
        unreachable!()
    };
    if let Some(field) = fields.iter().find(|field| field.name == name) {
        value.nodes[field.value as usize] = node;
    } else {
        fields.push(host::ValueField {
            name: name.into(),
            value: value.nodes.len() as u32,
        });
        value.nodes.push(node);
    }
    value.nodes.push(host::ValueNode::Object(fields));
}
fn schedule(delay: u32) -> Result<(), String> {
    if let Some(timer) = TIMER.take() {
        host::cancel_timer(timer)?;
    }
    TIMER.set(Some(host::start_timer(&host::TimerRequest {
        milliseconds: delay,
        handler: "objects".into(),
    })?));
    Ok(())
}
fn refresh() -> Result<(), String> {
    let snapshot = host::current_buffer()?;
    let id = host::query_syntax(
        &host::SyntaxQuery {
            buffer: snapshot.buffer,
            range: None,
            query: "(function_item body: (block) @body) @function".into(),
        },
        "objects",
    )?;
    QUERY.with(|query| *query.borrow_mut() = Some((id, snapshot)));
    Ok(())
}
impl Guest for Functions {
    fn activate() -> Result<Vec<String>, String> {
        Ok(vec!["enable".into(), "objects".into(), "operator".into()])
    }
    fn execute_command(command: String, mut call: host::CommandCall) -> Result<String, String> {
        if command == "enable" {
            host::wrap_command(&host::CommandWrapper {
                command: "edit.operator".into(),
                handler: "operator".into(),
                order: 0,
            })?;
            host::subscribe_events("objects")?;
            refresh()?;
        } else if command == "objects" {
            if text(&call.arguments, "event") == Some("editor") {
                if host::take_editor_events().is_some_and(|batch| {
                    batch.snapshot.is_some()
                        || batch.records.iter().any(|record| {
                            matches!(
                                record.value,
                                host::Occurrence::Changed(_) | host::Occurrence::Opened(_)
                            )
                        })
                }) {
                    RETRY.set(0);
                    schedule(25)?;
                }
            } else if text(&call.arguments, "event") == Some("timer") {
                TIMER.set(None);
                refresh()?;
            } else {
                for event in host::take_system_events() {
                    let query = QUERY.with(|query| query.borrow_mut().take());
                    let Some((id, snapshot)) = query else {
                        continue;
                    };
                    if id != event.request {
                        QUERY.with(|query| *query.borrow_mut() = Some((id, snapshot)));
                        continue;
                    }
                    let captures = match event.value {
                        Ok(host::SystemValue::Syntax(host::SyntaxValue::Captures(captures))) => {
                            captures
                        }
                        Err(error) if error.contains("not current") && RETRY.get() < 5 => {
                            let attempt = RETRY.get();
                            RETRY.set(attempt + 1);
                            schedule(25 << attempt)?;
                            continue;
                        }
                        Err(error) => return Err(error),
                        _ => return Err("expected syntax captures".into()),
                    };
                    let current = host::current_buffer()?;
                    if current.buffer != snapshot.buffer || current.revision != snapshot.revision {
                        continue;
                    }
                    for (namespace, capture) in [("outer", "function"), ("inner", "body")] {
                        let marks = captures
                            .iter()
                            .filter(|item| item.name == capture)
                            .enumerate()
                            .map(|(id, item)| host::Mark {
                                id: id as u64,
                                start: item.range.start + u64::from(capture == "body"),
                                end: item.range.end - u64::from(capture == "body"),
                                start_gravity: host::Gravity::Left,
                                end_gravity: host::Gravity::Right,
                                decoration: None,
                            })
                            .collect();
                        host::replace_marks(&host::MarkSet {
                            buffer: snapshot.buffer,
                            revision: snapshot.revision,
                            namespace: namespace.into(),
                            marks,
                        })?;
                    }
                }
            }
        } else if text(&call.arguments, "event").is_some() {
            for reply in host::take_command_replies() {
                if let Some(error) = reply.error {
                    return Err(error);
                }
            }
        } else if !call.after {
            let motion = text(&call.arguments, "motion").unwrap_or("");
            if matches!(motion, "inner_function" | "a_function") {
                let snapshot = host::current_buffer()?;
                let cursor = host::current_view().ok_or("no view")?.cursor;
                let outer = host::read_marks(snapshot.buffer, "outer")?;
                let mut enclosing: Vec<_> = outer
                    .iter()
                    .filter(|mark| mark.start <= cursor && cursor < mark.end)
                    .collect();
                enclosing.sort_by_key(|mark| mark.end - mark.start);
                let selected = if enclosing.is_empty() {
                    outer
                        .iter()
                        .filter(|mark| mark.start > cursor)
                        .min_by_key(|mark| mark.start)
                } else {
                    enclosing.get(call.count.unwrap_or(1) as usize - 1).copied()
                }
                .ok_or("no function at this count")?;
                let (start, end) = if motion == "a_function" {
                    (selected.start, selected.end)
                } else {
                    let inner = host::read_marks(snapshot.buffer, "inner")?;
                    let body = inner
                        .iter()
                        .find(|mark| mark.id == selected.id)
                        .ok_or("function has no body")?;
                    (body.start, body.end)
                };
                if text(&call.arguments, "operator") == Some("surround_add") {
                    let (open, close) = match text(&call.arguments, "character").unwrap_or("(") {
                        "(" | ")" | "b" => ('(', ')'),
                        "[" | "]" => ('[', ']'),
                        "{" | "}" | "B" => ('{', '}'),
                        "\"" => ('"', '"'),
                        "'" => ('\'', '\''),
                        _ => return Err("choose (), [], {}, or quotes".into()),
                    };
                    let mut arguments = host::CommandValue {
                        nodes: vec![host::ValueNode::Object(vec![])],
                    };
                    for (name, value) in [
                        ("revision", snapshot.revision),
                        ("start", start),
                        ("end", end),
                        ("cursor", start),
                    ] {
                        set(&mut arguments, name, host::ValueNode::Integer(value as i64));
                    }
                    set(
                        &mut arguments,
                        "text",
                        host::ValueNode::Text(format!(
                            "{open}{}{close}",
                            &snapshot.utf8[start as usize..end as usize]
                        )),
                    );
                    call.command = "edit.apply".into();
                    call.arguments = arguments;
                    call.pending = None;
                    host::run_command(&call)?;
                    return Ok(String::new());
                }
                for (name, value) in [
                    ("revision", snapshot.revision),
                    ("start", start),
                    ("end", end),
                ] {
                    set(
                        &mut call.arguments,
                        name,
                        host::ValueNode::Integer(value as i64),
                    );
                }
                host::set_command_result(&call.arguments)?;
            }
            host::call_next(call.pending.ok_or("missing continuation")?)?;
        }
        Ok(String::new())
    }
}
export!(Functions);
