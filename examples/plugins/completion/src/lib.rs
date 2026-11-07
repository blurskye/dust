//! Buffer words offered through Dust's menu and inline completion pipeline.
// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "../../../wit", world: "editor-plugin" });
use dust::plugin::host;
use std::collections::BTreeSet;

struct Words;
impl Guest for Words {
    fn activate() -> Result<Vec<String>, String> {
        Ok(vec!["enable".into(), "complete".into()])
    }

    fn execute_command(command: String, call: host::CommandCall) -> Result<String, String> {
        if command == "enable" {
            host::register_completion_source(&host::CompletionSource {
                name: "words".into(),
                handler: "complete".into(),
                inline: true,
            })?;
        } else {
            let snapshot = host::current_buffer()?;
            let cursor = host::current_view().ok_or("no view")?.cursor as usize;
            let before = snapshot.utf8.get(..cursor).ok_or("invalid cursor")?;
            let start = before
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map_or(0, |(i, c)| i + c.len_utf8());
            let prefix = &before[start..];
            let words: BTreeSet<_> = snapshot
                .utf8
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|word| word.starts_with(prefix) && word.len() > prefix.len())
                .collect();
            let Some(host::ValueNode::Object(fields)) = call.arguments.nodes.last() else {
                return Err("expected completion arguments".into());
            };
            let request = fields
                .iter()
                .find(|field| field.name == "request")
                .and_then(|field| match call.arguments.nodes[field.value as usize] {
                    host::ValueNode::Integer(id) => Some(id as u64),
                    _ => None,
                })
                .ok_or("missing completion request")?;
            host::publish_completions(&host::CompletionBatch {
                source: "words".into(),
                request,
                buffer: snapshot.buffer,
                revision: snapshot.revision,
                items: words
                    .into_iter()
                    .take(128)
                    .map(|word| host::CompletionItem {
                        label: word.into(),
                        detail: Some("buffer word".into()),
                        start: start as u64,
                        end: cursor as u64,
                        text: word.into(),
                        snippet: false,
                    })
                    .collect(),
                incomplete: false,
            })?;
        }
        Ok(String::new())
    }
}
export!(Words);
