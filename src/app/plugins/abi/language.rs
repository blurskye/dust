//! Feed plugin completion batches into the native menu and inline pipeline.

use super::*;
use crate::editor::text::{ByteOffset, PositionCodec, PositionEncoding};
use crate::language::lsp::{LspCompletionItem, LspPosition, LspRange, LspTextEdit};

impl App {
    pub(super) fn install_plugin_completion(
        &mut self,
        plugin: &str,
        source: wit::CompletionSource,
    ) -> Result<(), String> {
        self.handler(plugin, &source.handler)?;
        if source.name.is_empty() || source.name.len() > 128 {
            return Err("invalid completion source name".into());
        }
        if self.extension.abi.completions.len() >= 64 {
            return Err("completion source limit exceeded".into());
        }
        let name = format!("plugin.{plugin}.{}", source.name);
        self.language
            .register_completion_source(&name, PositionEncoding::Utf8, source.inline);
        self.extension.abi.completions.insert(
            (plugin.into(), source.name),
            (source.handler, source.inline),
        );
        self.extension.abi.completion_observation = None;
        Ok(())
    }

    pub(super) fn collect_plugin_completion_callbacks(
        &mut self,
        callbacks: &mut Vec<(String, String, wit::CommandCall)>,
    ) {
        if self.extension.abi.completions.is_empty() {
            return;
        }
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        if view.mode != crate::editor::EditorMode::Insert {
            return;
        }
        let text = &self.state.buffers[view.buffer].text;
        let observation = (
            view.buffer.data().as_ffi(),
            text.revision().0,
            view.cursor.0 as u64,
        );
        if self.extension.abi.completion_observation == Some(observation) {
            return;
        }
        self.extension.abi.completion_observation = Some(observation);
        self.extension.abi.next += 1;
        let request = self.extension.abi.next;
        let owner = crate::app::language::LspProjectionOwner {
            server: String::new(),
            buffer: view.buffer,
            view: view_id,
            revision: text.revision(),
            text: text.source_version(),
            uri: String::new(),
            language: String::new(),
        };
        self.language.start_completions(owner, request);
        for ((plugin, source), (handler, _)) in &self.extension.abi.completions {
            let arguments = CommandValue::Record(BTreeMap::from([
                ("request".into(), CommandValue::Integer(request as i64)),
                ("source".into(), CommandValue::String(source.clone())),
                ("cursor".into(), CommandValue::Integer(view.cursor.0 as i64)),
            ]));
            callbacks.push((
                plugin.clone(),
                handler.clone(),
                wit::CommandCall {
                    command: format!("plugin.{plugin}.{handler}"),
                    arguments: encode_value(&arguments),
                    buffer: Some(observation.0),
                    view: Some(view_id.data().as_ffi()),
                    ..Default::default()
                },
            ));
        }
    }

    pub(super) fn publish_plugin_completions(
        &mut self,
        plugin: &str,
        batch: wit::CompletionBatch,
    ) -> Result<(), String> {
        if !self
            .extension
            .abi
            .completions
            .contains_key(&(plugin.into(), batch.source.clone()))
        {
            return Err("completion source is not registered".into());
        }
        let buffer = slotmap::KeyData::from_ffi(batch.buffer).into();
        let text = &self
            .state
            .buffers
            .get(buffer)
            .ok_or("completion buffer closed")?
            .text;
        if text.revision().0 != batch.revision || !self.language.completion_current(&self.state) {
            return Err("stale completion batch".into());
        }
        let codec = PositionCodec::new(text);
        let mut items = Vec::new();
        for item in batch.items {
            if item.start > item.end {
                return Err("invalid completion range".into());
            }
            let position = |offset| -> Result<LspPosition, String> {
                let point = codec
                    .byte_to_lsp(ByteOffset(offset as usize), PositionEncoding::Utf8)
                    .map_err(|error| error.to_string())?;
                Ok(LspPosition {
                    line: point.0,
                    character: point.1,
                })
            };
            items.push(LspCompletionItem {
                label: item.label,
                filter_text: None,
                sort_text: None,
                detail: item.detail.unwrap_or_default(),
                insert_text: item.text.clone(),
                text_edit: Some(LspTextEdit {
                    range: LspRange {
                        start: position(item.start)?,
                        end: position(item.end)?,
                    },
                    new_text: item.text,
                }),
                additional_edits: Vec::new(),
                kind: None,
                snippet: item.snippet,
                documentation: None,
                command: None,
                resolve_item: None,
                resolved: true,
            });
        }
        self.language.publish_completions(
            &format!("plugin.{plugin}.{}", batch.source),
            items,
            batch.incomplete,
            batch.request,
        );
        self.project_lsp_completion();
        Ok(())
    }
}
