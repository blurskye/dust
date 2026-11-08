//! Translate guest marks and diagnostics into the native drawing owners.

use super::{App, CommandAccess, CommandAuthority, wit};
use crate::app::diagnostics::{DiagnosticBatch, DiagnosticDocument, DiagnosticValidity};
use crate::editor::decorations::{
    Decoration, Gravity, StyledText, VirtualText, VirtualTextPosition,
};
use crate::editor::text::{ByteOffset, ByteRange};
use crate::workspace::{Diagnostic, DiagnosticOwner, DiagnosticProducer, DiagnosticSeverity};
use std::collections::BTreeSet;

impl App {
    pub(super) fn require_plugin_drawing(
        &self,
        plugin: &str,
        authority: &CommandAuthority,
    ) -> Result<(), String> {
        self.effect_authority
            .restrict(authority)
            .restrict(&self.plugin_authority(plugin))
            .require(CommandAccess::Ui)
            .map_err(|error| error.to_string())
    }

    pub(super) fn replace_plugin_marks(
        &mut self,
        plugin: &str,
        authority: &CommandAuthority,
        set: wit::MarkSet,
    ) -> Result<(), String> {
        self.require_plugin_drawing(plugin, authority)?;
        namespace(&set.namespace)?;
        let total: usize = self
            .extension
            .abi
            .marks
            .iter()
            .filter(|((owner, buffer, namespace), _)| {
                owner == plugin && (*buffer != set.buffer || namespace != &set.namespace)
            })
            .map(|(_, marks)| marks.len())
            .sum();
        if total + set.marks.len() > 4096 {
            return Err("plugin mark limit exceeded".into());
        }
        let buffer_id = slotmap::KeyData::from_ffi(set.buffer).into();
        let buffer = self
            .state
            .buffers
            .get_mut(buffer_id)
            .ok_or("buffer closed")?;
        if buffer.text.revision().0 != set.revision {
            return Err("stale marks".into());
        }
        let namespace = format!("plugin.{plugin}.{}", set.namespace);
        let mut decorations = buffer.decorations.clone();
        decorations.clear(&namespace);
        let mut ids = BTreeSet::new();
        let mut marks = Vec::new();
        for mark in set.marks {
            if !ids.insert(mark.id) {
                return Err("duplicate mark ID".into());
            }
            let decoration = mark
                .decoration
                .as_ref()
                .map(native_decoration)
                .transpose()?
                .unwrap_or_default();
            let range = range(mark.start, mark.end)?;
            let gravity = |gravity| match gravity {
                wit::Gravity::Left => Gravity::Left,
                wit::Gravity::Right => Gravity::Right,
            };
            let id = decorations
                .insert(
                    &buffer.text,
                    &namespace,
                    range,
                    gravity(mark.start_gravity),
                    gravity(mark.end_gravity),
                )
                .ok_or("invalid mark range")?;
            decorations.decorate(id, decoration);
            marks.push((mark, id));
        }
        buffer.decorations = decorations;
        self.extension
            .abi
            .marks
            .insert((plugin.into(), set.buffer, set.namespace), marks);
        Ok(())
    }

    pub(super) fn publish_plugin_diagnostics(
        &mut self,
        plugin: &str,
        authority: &CommandAuthority,
        set: wit::DiagnosticSet,
    ) -> Result<(), String> {
        self.require_plugin_drawing(plugin, authority)?;
        namespace(&set.namespace)?;
        if set.diagnostics.len() > crate::app::diagnostics::MAXIMUM_DIAGNOSTICS_PER_URI {
            return Err("diagnostic limit exceeded".into());
        }
        let id = slotmap::KeyData::from_ffi(set.buffer).into();
        let buffer = self.state.buffers.get(id).ok_or("buffer closed")?;
        if buffer.text.revision().0 != set.revision {
            return Err("stale diagnostics".into());
        }
        let document = DiagnosticDocument::capture(&self.state, id).ok_or("buffer closed")?;
        let uri = document.uri();
        let mut items = Vec::new();
        for item in set.diagnostics {
            let range = range(item.start, item.end)?;
            buffer
                .text
                .validate_offset(range.start)
                .map_err(|error| error.to_string())?;
            buffer
                .text
                .validate_offset(range.end)
                .map_err(|error| error.to_string())?;
            if item.message.len() > 16_384 || item.source.len() > 256 {
                return Err("diagnostic text limit exceeded".into());
            }
            items.push(Diagnostic {
                uri: uri.clone(),
                version: Some(buffer.text.revision()),
                range,
                severity: match item.severity {
                    wit::DiagnosticSeverity::Error => DiagnosticSeverity::Error,
                    wit::DiagnosticSeverity::Warning => DiagnosticSeverity::Warning,
                    wit::DiagnosticSeverity::Information => DiagnosticSeverity::Information,
                    wit::DiagnosticSeverity::Hint => DiagnosticSeverity::Hint,
                },
                source: item.source,
                message: item.message,
                code: None,
                related: Vec::new(),
                tags: Vec::new(),
                fixes: Vec::new(),
                data: None,
                timestamp: std::time::SystemTime::now(),
            });
        }
        let key = (plugin.to_owned(), set.namespace);
        if !self.extension.abi.diagnostics.contains_key(&key)
            && self.extension.abi.diagnostics.len() >= 256
        {
            return Err("diagnostic namespace limit exceeded".into());
        }
        let producer = if let Some(producer) = self.extension.abi.diagnostics.get(&key) {
            producer.clone()
        } else {
            let owner = DiagnosticOwner::new(format!("plugin.{plugin}.{}", key.1))
                .map_err(|error| error.to_string())?;
            let producer = DiagnosticProducer::new(owner);
            self.diagnostics.activate(producer.clone());
            self.extension.abi.diagnostics.insert(key, producer.clone());
            producer
        };
        self.diagnostics.publish(
            DiagnosticBatch {
                producer,
                uri,
                document: Some(document),
                validity: DiagnosticValidity::RequestedSnapshot,
                items,
            },
            &self.state,
        )?;
        self.sync_ui_diagnostics();
        Ok(())
    }

    pub(super) fn remove_plugin_diagnostics(&mut self, plugin: &str) {
        self.extension
            .abi
            .diagnostics
            .retain(|(owner, _), producer| {
                if owner == plugin {
                    self.diagnostics.clear_source(producer);
                    false
                } else {
                    true
                }
            });
        self.sync_ui_diagnostics();
    }
}

fn namespace(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        Err("invalid drawing namespace".into())
    } else {
        Ok(())
    }
}

fn range(start: u64, end: u64) -> Result<ByteRange, String> {
    ByteRange::new(
        ByteOffset(usize::try_from(start).map_err(|_| "invalid range start")?),
        ByteOffset(usize::try_from(end).map_err(|_| "invalid range end")?),
    )
    .map_err(|error| error.to_string())
}

fn style(name: &str) -> Result<String, String> {
    crate::config::Highlight::named(name)
        .ok_or_else(|| format!("unknown highlight group: {name}"))?;
    Ok(name.into())
}

fn native_decoration(value: &wit::MarkDecoration) -> Result<Decoration, String> {
    let text = |text: &str, name: &str| -> Result<StyledText, String> {
        if text.len() > 4096 || text.chars().any(char::is_control) {
            return Err("decoration text must be a single line of at most 4096 bytes".into());
        }
        Ok(StyledText {
            text: text.into(),
            style: style(name)?,
        })
    };
    Ok(Decoration {
        highlight: value.highlight.as_deref().map(style).transpose()?,
        virtual_text: value
            .virtual_text
            .as_ref()
            .map(|value| {
                Ok::<_, String>(VirtualText {
                    text: text(&value.text, &value.style)?,
                    position: match value.position {
                        wit::VirtualTextPosition::Inline => VirtualTextPosition::Inline,
                        wit::VirtualTextPosition::EndOfLine => VirtualTextPosition::EndOfLine,
                        wit::VirtualTextPosition::Overlay => VirtualTextPosition::Overlay,
                    },
                })
            })
            .transpose()?,
        sign: value
            .sign
            .as_ref()
            .map(|value| {
                use unicode_segmentation::UnicodeSegmentation;
                use unicode_width::UnicodeWidthStr;
                if value.text.graphemes(true).count() != 1
                    || value.text.width() != 1
                    || value.text.width_cjk() != 1
                {
                    return Err("sign must occupy one terminal cell".into());
                }
                text(&value.text, &value.style)
            })
            .transpose()?,
    })
}

#[cfg(test)]
#[path = "drawing_tests.rs"]
mod tests;
