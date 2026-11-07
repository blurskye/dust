//! An editable directory: stable IDs identify renames; saves confirm and apply file operations.
// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "../../../wit", world: "editor-plugin" });
use dust::plugin::host;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Default)]
struct Directory {
    entries: BTreeMap<u64, String>,
    buffer: Option<u64>,
    saving: Option<host::Snapshot>,
    operations: VecDeque<host::FileOperation>,
    approved: Option<String>,
    next_entries: BTreeMap<u64, String>,
}
thread_local! { static DIRECTORY: RefCell<Directory> = RefCell::new(Directory::default()); }
struct Oil;
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
fn value(node: host::ValueNode) -> host::CommandValue {
    host::CommandValue { nodes: vec![node] }
}
fn listing(entries: &BTreeMap<u64, String>) -> String {
    entries
        .iter()
        .map(|(id, name)| format!("{id}\t{name}\n"))
        .collect()
}
fn path(name: &str) -> &str {
    name.trim_end_matches('/')
}
fn plan(directory: &mut Directory, snapshot: host::Snapshot) -> Result<(), String> {
    let mut next = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut new_id = directory.entries.keys().next_back().copied().unwrap_or(0) + 1;
    for line in snapshot.utf8.lines().filter(|line| !line.is_empty()) {
        let (id, name) = if let Some((id, name)) = line.split_once('\t') {
            let id = id
                .parse::<u64>()
                .map_err(|_| "keep the numeric ID before the tab")?;
            if !directory.entries.contains_key(&id) {
                return Err("unknown entry ID".into());
            }
            (id, name)
        } else {
            let id = new_id;
            new_id += 1;
            (id, line)
        };
        let filename = path(name);
        if filename.is_empty()
            || matches!(filename, "." | "..")
            || filename.contains('/')
            || filename.chars().any(char::is_control)
        {
            return Err("use one plain filename per line; a trailing / creates a directory".into());
        }
        if !names.insert(filename) || next.insert(id, name.to_owned()).is_some() {
            return Err("duplicate filename or ID".into());
        }
    }
    let mut operations = VecDeque::new();
    for (id, old) in &directory.entries {
        match next.get(id) {
            None => operations.push_back(host::FileOperation::Delete(path(old).into())),
            Some(new) if old != new => {
                if old.ends_with('/') != new.ends_with('/') {
                    return Err("renaming cannot change file type".into());
                }
                if directory
                    .entries
                    .values()
                    .any(|entry| path(entry) == path(new))
                {
                    return Err("rename destination exists; save intermediate renames first".into());
                }
                operations.push_back(host::FileOperation::Rename(host::FileRename {
                    source: path(old).into(),
                    target: path(new).into(),
                }));
            }
            _ => {}
        }
    }
    for (id, name) in &next {
        if !directory.entries.contains_key(id) {
            if directory
                .entries
                .values()
                .any(|entry| path(entry) == path(name))
            {
                return Err("new file already exists".into());
            }
            operations.push_back(if name.ends_with('/') {
                host::FileOperation::CreateDirectory(path(name).into())
            } else {
                host::FileOperation::Write(host::FileWrite {
                    path: name.clone(),
                    bytes: vec![],
                })
            });
        }
    }
    directory.operations = operations;
    directory.next_entries = next;
    directory.saving = Some(snapshot);
    Ok(())
}
fn unchanged(directory: &Directory) -> Result<host::Snapshot, String> {
    let now = host::current_buffer()?;
    let saved = directory.saving.as_ref().ok_or("no pending save")?;
    if now.buffer != saved.buffer || now.revision != saved.revision {
        return Err("directory buffer changed; save again".into());
    }
    Ok(now)
}
fn advance(directory: &mut Directory) -> Result<(), String> {
    let snapshot = unchanged(directory)?;
    if let Some(operation) = directory.operations.pop_front() {
        host::request_file(&operation, "applied")?;
    } else {
        let normalized = listing(&directory.next_entries);
        let fields = ["revision", "start", "end", "text", "cursor"];
        let mut nodes = vec![
            host::ValueNode::Integer(snapshot.revision as i64),
            host::ValueNode::Integer(0),
            host::ValueNode::Integer(snapshot.utf8.len() as i64),
            host::ValueNode::Text(normalized.clone()),
            host::ValueNode::Integer(0),
        ];
        nodes.push(host::ValueNode::Object(
            fields
                .iter()
                .enumerate()
                .map(|(i, name)| host::ValueField {
                    name: (*name).into(),
                    value: i as u32,
                })
                .collect(),
        ));
        let mut call = host::CommandCall {
            command: "edit.apply".into(),
            arguments: host::CommandValue { nodes },
            buffer: Some(snapshot.buffer),
            view: None,
            count: None,
            register: None,
            pending: None,
            after: false,
            previous: None,
        };
        host::run_command(&call)?;
        directory.entries = directory.next_entries.clone();
        directory.approved = Some(normalized);
        directory.saving = None;
        call.command = "file.save".into();
        call.arguments = value(host::ValueNode::Object(vec![]));
        host::run_command(&call)?;
    }
    Ok(())
}
impl Guest for Oil {
    fn activate() -> Result<Vec<String>, String> {
        Ok([
            "open", "listed", "write", "save", "confirm", "checked", "applied",
        ]
        .map(String::from)
        .into())
    }
    fn execute_command(command: String, call: host::CommandCall) -> Result<String, String> {
        let result = DIRECTORY.with(|state| -> Result<(), String> {
            let mut directory = state.borrow_mut();
            if text(&call.arguments, "event") == Some("commands") {
                for reply in host::take_command_replies() {
                    if let Some(error) = reply.error {
                        return Err(error);
                    }
                }
                return Ok(());
            }
            match command.as_str() {
                "open" => {
                    if directory.buffer.is_some() {
                        return Err("close and reload the plugin to open a fresh listing".into());
                    }
                    host::wrap_command(&host::CommandWrapper {
                        command: "file.save".into(),
                        handler: "save".into(),
                        order: 0,
                    })?;
                    host::request_file(&host::FileOperation::ListDirectory(".".into()), "listed")?;
                }
                "listed" => {
                    for event in host::take_system_events() {
                        let host::SystemValue::File(host::FileValue::Entries(entries)) =
                            event.value?
                        else {
                            return Err("expected directory entries".into());
                        };
                        directory.entries = entries
                            .into_iter()
                            .enumerate()
                            .map(|(id, entry)| {
                                (
                                    id as u64 + 1,
                                    format!(
                                        "{}{}",
                                        entry.name,
                                        if matches!(entry.kind, host::FileKind::Directory) {
                                            "/"
                                        } else {
                                            ""
                                        }
                                    ),
                                )
                            })
                            .collect();
                        host::open_editable_document(
                            &host::EditableDocument {
                                source: "directory".into(),
                                key: ".".into(),
                                title: "Oil · workspace".into(),
                                language: "text".into(),
                                text: listing(&directory.entries),
                            },
                            "write",
                        )?;
                    }
                }
                "write" => {
                    if text(&call.arguments, "event") == Some("document-write") {
                        let accepted =
                            text(&call.arguments, "text") == directory.approved.as_deref();
                        host::set_command_result(&value(host::ValueNode::Boolean(accepted)))?;
                    } else {
                        for reply in host::take_command_replies() {
                            if let Some(error) = reply.error {
                                return Err(error);
                            }
                            if let Some(host::ValueNode::Integer(buffer)) =
                                reply.value.as_ref().and_then(|value| value.nodes.last())
                            {
                                directory.buffer = Some(*buffer as u64);
                            }
                        }
                    }
                }
                "save" if !call.after => {
                    let snapshot = host::current_buffer()?;
                    if directory.buffer != Some(snapshot.buffer)
                        || directory.approved.as_deref() == Some(&snapshot.utf8)
                    {
                        host::call_next(call.pending.ok_or("missing continuation")?)?;
                    } else {
                        if directory.saving.is_some() {
                            return Err("directory save already pending".into());
                        }
                        plan(&mut directory, snapshot)?;
                        let summary = directory
                            .operations
                            .iter()
                            .map(|op| match op {
                                host::FileOperation::Delete(path) => format!("delete {path}"),
                                host::FileOperation::Rename(rename) => {
                                    format!("rename {} → {}", rename.source, rename.target)
                                }
                                host::FileOperation::Write(file) => format!("create {}", file.path),
                                host::FileOperation::CreateDirectory(path) => {
                                    format!("create {path}/")
                                }
                                _ => unreachable!("directory plan contains only mutations"),
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        host::ui_select(
                            &host::SelectRequest {
                                title: "Apply directory changes?".into(),
                                items: vec![
                                    host::SelectItem {
                                        id: "no".into(),
                                        label: "Cancel".into(),
                                        detail: None,
                                    },
                                    host::SelectItem {
                                        id: "yes".into(),
                                        label: "Apply changes".into(),
                                        detail: Some(summary),
                                    },
                                ],
                            },
                            "confirm",
                        )?;
                    }
                }
                "confirm" => {
                    let yes = host::take_command_replies().iter().any(|reply| {
                        reply
                            .value
                            .as_ref()
                            .and_then(|value| value.nodes.last())
                            .is_some_and(
                                |node| matches!(node, host::ValueNode::Text(text) if text == "yes"),
                            )
                    });
                    if yes {
                        unchanged(&directory)?;
                        host::request_file(
                            &host::FileOperation::ListDirectory(".".into()),
                            "checked",
                        )?;
                    } else {
                        directory.saving = None;
                        directory.operations.clear();
                    }
                }
                "checked" => {
                    for event in host::take_system_events() {
                        let host::SystemValue::File(host::FileValue::Entries(entries)) =
                            event.value?
                        else {
                            return Err("expected directory entries".into());
                        };
                        let names: BTreeSet<_> =
                            entries.iter().map(|entry| entry.name.as_str()).collect();
                        let original: BTreeSet<_> = directory
                            .entries
                            .values()
                            .map(|entry| path(entry))
                            .collect();
                        if names != original {
                            return Err("directory changed on disk; reopen the listing".into());
                        }
                        advance(&mut directory)?;
                    }
                }
                "applied" => {
                    for event in host::take_system_events() {
                        event.value?;
                        advance(&mut directory)?;
                    }
                }
                _ => {}
            }
            Ok(())
        });
        if let Err(error) = result {
            DIRECTORY.with(|state| {
                let mut state = state.borrow_mut();
                state.saving = None;
                state.operations.clear();
            });
            return Err(error);
        }
        Ok(String::new())
    }
}
export!(Oil);
