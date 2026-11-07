//! Background file previews for picker selections.

use crate::app::search::Search;
use crate::app::workspace::WorkspaceVersion;
use crate::files::RequestId;
use crate::files::VfsError;
use crate::ui::PickerPreview;
use crate::ui::PickerPreviewBody;
use crate::ui::PickerTextPreview;
use std::path::PathBuf;
use std::sync::Arc;
pub(in crate::app) struct PreviewTicket(Arc<()>);
pub(in crate::app) fn search_picker_preview(
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
        |text| {
            let column = bytes[start..match_start]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(match_start - start, |offset| {
                    match_start - start - offset - 1
                });
            picker_preview_lines_from(
                text,
                first_visible_offset,
                512,
                Some((selected_offset, column)),
            )
        },
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

pub(in crate::app) fn picker_preview_lines_from(
    text: &str,
    first_line: usize,
    maximum_lines: usize,
    selected: Option<(usize, usize)>,
) -> Vec<String> {
    text.lines()
        .enumerate()
        .skip(first_line)
        .take(maximum_lines)
        .map(|(index, line)| {
            let start = selected
                .filter(|(selected_line, _)| *selected_line == index)
                .and_then(|(_, column)| line.get(..column))
                .filter(|_| line.chars().take(4097).count() > 4096)
                .and_then(|prefix| prefix.char_indices().rev().nth(16))
                .map_or(0, |(offset, _)| offset);
            line[start..]
                .chars()
                .filter(|character| !character.is_control() || *character == '\t')
                .take(4096)
                .collect()
        })
        .collect()
}

impl Search {
    pub(in crate::app) fn preview(&self) -> &Option<PickerPreview> {
        &self.preview
    }
    pub(in crate::app) fn clear_preview(&mut self) -> Option<RequestId> {
        if self.preview.is_none()
            && self.preview_path.is_none()
            && self.preview_read.is_none()
            && self.diff_preview_key.is_none()
        {
            return None;
        }
        self.preview = None;
        self.preview_snapshot_key = None;
        self.preview_path = None;
        self.preview_location = None;
        self.diff_preview.cancel();
        self.diff_preview_key = None;
        self.preview_scope = None;
        self.preview_epoch = Arc::new(());
        self.preview_read.take().map(|(id, _)| id)
    }
    /// Replace the preview with one the session already holds, such as a diff built from the
    /// structured hunk read. Any in-flight file preview is invalidated first, so a late read can
    /// never overwrite it with a different path's content.
    pub(in crate::app) fn set_preview(&mut self, preview: PickerPreview) -> Option<RequestId> {
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
        let previous_request = self.clear_preview();
        self.preview = Some(preview);
        previous_request
    }
    pub(in crate::app) fn preview_match(&mut self, id: &str, maximum: usize) -> Option<RequestId> {
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
        let previous_request = self.clear_preview();
        self.preview = Some(preview);
        self.preview_snapshot_key = Some(key);
        self.preview_path = Some(path);
        previous_request
    }
    pub(in crate::app) fn prepare_preview(
        &mut self,
        path: PathBuf,
        location: Option<(usize, usize)>,
        title: String,
        loading: String,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> (Option<RequestId>, Option<PreviewTicket>) {
        if self.preview_path.as_ref() == Some(&path)
            && self.preview_location == location
            && self.preview_scope.as_ref() == Some(&(scope.clone(), configuration))
            && self
                .preview
                .as_ref()
                .and_then(PickerPreview::as_text)
                .is_some_and(|text| text.highlight_line == location.map(|(line, _)| line))
        {
            return (None, None);
        }
        let previous_request = self.clear_preview();
        self.preview_scope = Some((scope, configuration));
        self.preview = Some(PickerPreview {
            title,
            body: PickerPreviewBody::Text(PickerTextPreview::new(
                vec![loading],
                location.map_or(0, |(line, _)| line),
                location.map(|(line, _)| line),
                crate::language::syntax::language_for_path(&path),
            )),
        });
        self.preview_path = Some(path);
        self.preview_location = location;
        (
            previous_request,
            Some(PreviewTicket(self.preview_epoch.clone())),
        )
    }
    pub(in crate::app) fn prepare_diff_preview(
        &mut self,
        policy: crate::git::GitHunkPolicy,
        scope: WorkspaceVersion,
        configuration: u64,
        title: String,
    ) -> Option<RequestId> {
        let key = (
            scope.clone(),
            policy.status_revision,
            policy.file.clone(),
            policy.layer,
        );
        if self.diff_preview_key.as_ref() == Some(&key)
            && self.preview_scope.as_ref() == Some(&(scope.clone(), configuration))
        {
            return None;
        }
        let previous_request = self.clear_preview();
        self.picker_preview_replaced();
        self.preview_scope = Some((scope, configuration));
        self.diff_preview_key = Some(key);
        if self.diff_preview.start(policy).is_ok() {
            self.preview = Some(PickerPreview {
                title,
                body: PickerPreviewBody::Text(PickerTextPreview::new(
                    vec!["loading…".into()],
                    0,
                    None,
                    None,
                )),
            });
        }
        previous_request
    }

    pub(in crate::app) fn poll_diff_preview(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        if self.diff_preview_key.is_none() {
            return false;
        }
        if self.preview_scope.as_ref() != Some(&(scope, configuration)) {
            self.clear_preview();
            return true;
        }
        match self.diff_preview.poll() {
            Ok(Some(snapshot)) => {
                let provenance = match snapshot.layer {
                    crate::git::GitDiffLayer::Staged => "HEAD -> index",
                    crate::git::GitDiffLayer::Unstaged => "index -> disk",
                };
                let body = PickerPreviewBody::Diff(crate::app::git::diff_preview_from_hunks(
                    &snapshot.hunks,
                    provenance,
                    false,
                    0,
                ));
                if let Some(preview) = &mut self.preview {
                    preview.body = body;
                }
                true
            }
            Err(_) => {
                self.preview = None;
                true
            }
            Ok(None) => false,
        }
    }

    pub(in crate::app) fn track_preview_read(
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
    pub(in crate::app) fn preview_needs_retry(&self) -> bool {
        self.preview_path.is_some()
            && self.preview_scope.is_none()
            && self.preview_snapshot_key.is_none()
    }
    pub(in crate::app) fn preview_failed(&mut self, ticket: PreviewTicket, error: VfsError) {
        if Arc::ptr_eq(&ticket.0, &self.preview_epoch) {
            if matches!(error, VfsError::QueueFull) {
                self.preview_scope = None;
            }
            if let Some(text) = self.preview.as_mut().and_then(PickerPreview::as_text_mut) {
                text.set_content(
                    vec![format!("preview unavailable: {error}")],
                    text.language(),
                );
            }
        }
    }
    pub(in crate::app) fn accept_preview(
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
                    vec!["preview exceeds requested byte limit".into()]
                }
                Ok(bytes) => crate::editor::text::decode_utf8(bytes).map_or_else(
                    |_| vec!["binary preview unavailable".into()],
                    |decoded| {
                        let first = self
                            .preview_location
                            .map_or(0, |(line, _)| line.saturating_sub(8));
                        text.first_line = first;
                        text.highlight_line = self.preview_location.map(|(line, _)| line);
                        picker_preview_lines_from(&decoded.text, first, 512, self.preview_location)
                    },
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
