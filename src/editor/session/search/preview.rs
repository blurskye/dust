use super::SearchSession;
use crate::editor::session::workspace::WorkspaceVersion;
use crate::{
    files::{RequestId, VfsError},
    ui::{PickerPreview, PickerPreviewBody, PickerTextPreview},
};
use std::{path::PathBuf, sync::Arc};
pub(in crate::editor::session) struct PreviewTicket(Arc<()>);
impl SearchSession {
    pub(in crate::editor::session) fn preview(&self) -> &Option<PickerPreview> {
        &self.preview
    }
    pub(in crate::editor::session) fn clear_preview(&mut self) -> Option<RequestId> {
        if self.preview.is_none() && self.preview_path.is_none() && self.preview_read.is_none() {
            return None;
        }
        self.preview = None;
        self.preview_snapshot_key = None;
        self.preview_path = None;
        self.preview_scope = None;
        self.preview_epoch = Arc::new(());
        self.preview_read.take().map(|(id, _)| id)
    }
    /// Replace the preview with one the session already holds, such as a diff built from the
    /// structured hunk read. Any in-flight file preview is retired first, so a late read can
    /// never overwrite it with a different path's content.
    pub(in crate::editor::session) fn set_preview(
        &mut self,
        preview: PickerPreview,
    ) -> Option<RequestId> {
        let same_diff = self.preview.as_ref().is_some_and(|shown| {
            shown.title == preview.title
                && match (&shown.body, &preview.body) {
                    (PickerPreviewBody::Diff(shown), PickerPreviewBody::Diff(next)) => {
                        shown.provenance == next.provenance
                    }
                    _ => false,
                }
        });
        if !same_diff {
            self.picker_preview_replaced();
        }
        let retired = self.clear_preview();
        self.preview = Some(preview);
        retired
    }
    pub(in crate::editor::session) fn preview_match(
        &mut self,
        id: &str,
        maximum: usize,
    ) -> Option<RequestId> {
        let Some(found) = self.matches.iter().find(|found| found.id == id) else {
            return self.clear_preview();
        };
        if self
            .preview_snapshot_key
            .as_ref()
            .is_some_and(|(bytes, range, limit)| {
                Arc::ptr_eq(bytes, &found.snapshot) && *range == found.range && *limit == maximum
            })
            && self.preview_path.as_ref() == Some(&found.canonical_path)
            && self
                .preview
                .as_ref()
                .and_then(PickerPreview::as_text)
                .is_some_and(|text| text.highlight_line == Some(found.line))
        {
            return None;
        }
        let preview = search_picker_preview(found, maximum);
        let key = (found.snapshot.clone(), found.range.clone(), maximum);
        let path = found.canonical_path.clone();
        let retired = self.clear_preview();
        self.preview = Some(preview);
        self.preview_snapshot_key = Some(key);
        self.preview_path = Some(path);
        retired
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn preview_snapshot(
        &mut self,
        found: &crate::files::WorkspaceSearchMatch,
        maximum: usize,
    ) -> Option<RequestId> {
        if self
            .preview_snapshot_key
            .as_ref()
            .is_some_and(|(bytes, range, limit)| {
                Arc::ptr_eq(bytes, &found.snapshot) && *range == found.range && *limit == maximum
            })
            && self.preview_path.as_ref() == Some(&found.canonical_path)
            && self
                .preview
                .as_ref()
                .and_then(PickerPreview::as_text)
                .is_some_and(|text| text.highlight_line == Some(found.line))
        {
            return None;
        }
        let retired = self.clear_preview();
        self.preview = Some(search_picker_preview(found, maximum));
        self.preview_path = Some(found.canonical_path.clone());
        self.preview_snapshot_key = Some((found.snapshot.clone(), found.range.clone(), maximum));
        retired
    }
    pub(in crate::editor::session) fn prepare_preview(
        &mut self,
        path: PathBuf,
        title: String,
        loading: String,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> (Option<RequestId>, Option<PreviewTicket>) {
        if self.preview_path.as_ref() == Some(&path)
            && self.preview_scope.as_ref() == Some(&(scope.clone(), configuration))
        {
            return (None, None);
        }
        let retired = self.clear_preview();
        self.preview_scope = Some((scope, configuration));
        self.preview = Some(PickerPreview {
            title,
            body: PickerPreviewBody::Text(PickerTextPreview::new(
                vec![loading],
                0,
                None,
                crate::language::syntax::language_for_path(&path),
            )),
        });
        self.preview_path = Some(path);
        (retired, Some(PreviewTicket(self.preview_epoch.clone())))
    }
    pub(in crate::editor::session) fn admit_preview(
        &mut self,
        ticket: PreviewTicket,
        id: RequestId,
        maximum: usize,
    ) -> bool {
        if !Arc::ptr_eq(&ticket.0, &self.preview_epoch) || self.preview_read.is_some() {
            return false;
        }
        self.preview_read = Some((id, maximum));
        true
    }
    pub(in crate::editor::session) fn preview_failed(
        &mut self,
        ticket: PreviewTicket,
        error: String,
    ) {
        if Arc::ptr_eq(&ticket.0, &self.preview_epoch)
            && let Some(text) = self.preview.as_mut().and_then(PickerPreview::as_text_mut)
        {
            text.set_content(vec![format!("preview unavailable: {error}")], text.language());
        }
    }
    pub(in crate::editor::session) fn accept_preview(
        &mut self,
        id: RequestId,
        result: &Result<Vec<u8>, VfsError>,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        let Some((expected, maximum)) = self.preview_read else {
            return false;
        };
        if id != expected {
            return false;
        }
        self.preview_read = None;
        if self.preview_scope.as_ref() != Some(&(scope, configuration)) {
            self.clear_preview();
            return true;
        }
        if let Some(text) = self.preview.as_mut().and_then(PickerPreview::as_text_mut) {
            let lines = match result {
                Ok(bytes) if bytes.len() > maximum => {
                    vec!["preview exceeds admitted byte limit".into()]
                }
                Ok(bytes) => std::str::from_utf8(bytes).map_or_else(
                    |_| vec!["binary preview unavailable".into()],
                    |text| picker_preview_lines(text, 512),
                ),
                Err(error) => vec![format!("preview unavailable: {error}")],
            };
            let language = text.language().or_else(|| {
                crate::language::syntax::language_for_document(
                    None,
                    lines.first().map_or("", String::as_str),
                )
            });
            text.set_content(lines, language);
        }
        true
    }
}
pub(super) fn search_picker_preview(
    found: &crate::files::WorkspaceSearchMatch,
    maximum_bytes: usize,
) -> PickerPreview {
    let normalized = crate::editor::text::TextBuffer::from_bytes(&found.snapshot)
        .map(|text| text.to_string())
        .unwrap_or_default();
    let bytes = normalized.as_bytes();
    let maximum_bytes = maximum_bytes.max(1);
    let match_start = found.range.start.min(bytes.len());
    let mut start = match_start.saturating_sub(maximum_bytes / 3);
    while start < match_start
        && bytes
            .get(start)
            .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
    {
        start = start.saturating_add(1);
    }
    if start > 0
        && let Some(line_break) = bytes[start..match_start]
            .iter()
            .position(|byte| *byte == b'\n')
    {
        start = start.saturating_add(line_break).saturating_add(1);
    }
    let mut end = start.saturating_add(maximum_bytes).min(bytes.len());
    while end > start && std::str::from_utf8(&bytes[start..end]).is_err() {
        end = end.saturating_sub(1);
    }
    let before_lines = bytes[start..match_start]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    let mut first_line = found.line.saturating_sub(before_lines);
    let selected_offset = found.line.saturating_sub(first_line);
    let first_visible_offset = selected_offset.saturating_sub(8);
    let lines = std::str::from_utf8(&bytes[start..end]).map_or_else(
        |_| vec!["binary preview unavailable".into()],
        |text| picker_preview_lines_from(text, first_visible_offset, 512),
    );
    first_line = first_line.saturating_add(first_visible_offset);
    PickerPreview {
        title: found.relative_path.display().to_string(),
        body: PickerPreviewBody::Text(PickerTextPreview::new(
            lines,
            first_line,
            Some(found.line),
            crate::language::syntax::language_for_path(&found.relative_path),
        )),
    }
}

pub(super) fn picker_preview_lines(text: &str, maximum_lines: usize) -> Vec<String> {
    picker_preview_lines_from(text, 0, maximum_lines)
}

fn picker_preview_lines_from(text: &str, first_line: usize, maximum_lines: usize) -> Vec<String> {
    text.lines()
        .skip(first_line)
        .take(maximum_lines)
        .map(|line| {
            line.chars()
                .filter(|character| !character.is_control() || *character == '\t')
                .take(4096)
                .collect()
        })
        .collect()
}
