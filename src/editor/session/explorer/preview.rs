//! Selected-file preview admission and exact, bounded reply acceptance.
use super::{ExplorerKind, ExplorerNodeId, ExplorerSession, WorkspaceVersion};
use crate::{
    config::ExplorerConfig,
    files::{RequestId, VfsError},
};
use std::{path::PathBuf, sync::Arc};

pub(in crate::editor::session) struct PreviewTicket(Arc<()>);
pub(in crate::editor::session) struct PreviewRead {
    pub ticket: PreviewTicket,
    pub path: PathBuf,
    pub roots: Vec<PathBuf>,
    pub maximum: u64,
}
pub(super) struct Preview {
    key: Arc<()>,
    epoch: Arc<()>,
    node: ExplorerNodeId,
    path: PathBuf,
    source: (WorkspaceVersion, u64),
    maximum: usize,
    maximum_lines: usize,
    request: Option<RequestId>,
    lines: Vec<String>,
    binary: String,
    failed: String,
}
impl ExplorerSession {
    fn preview_current(&self, preview: &Preview) -> bool {
        Arc::ptr_eq(&preview.epoch, &self.epoch)
            && self.tree.selected_node().is_some_and(|node| {
                node.id == preview.node
                    && node.path == preview.path
                    && node.kind == ExplorerKind::File
            })
    }
    pub(super) fn retire_changed_preview_selection(&mut self) {
        self.retire_changed_confirmation();
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| !self.preview_current(preview))
        {
            // Retain the read ID for cancellation, but never revive a ticket on selection ABA.
            if let Some(preview) = &mut self.preview {
                preview.epoch = Arc::new(());
            }
        }
    }
    pub(in crate::editor::session) fn clear_preview(&mut self) -> Option<RequestId> {
        self.preview.take().and_then(|preview| preview.request)
    }
    pub(in crate::editor::session) fn prepare_preview(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
        policy: &ExplorerConfig,
        binary: &str,
        failed: &str,
    ) -> (Option<RequestId>, Option<PreviewRead>) {
        let node = self
            .tree
            .selected_node()
            .filter(|node| policy.preview_enabled && node.kind == ExplorerKind::File);
        let Some(node) = node else {
            return (self.clear_preview(), None);
        };
        if self.preview.as_ref().is_some_and(|preview| {
            self.preview_current(preview) && preview.source == (scope.clone(), configuration)
        }) {
            return (None, None);
        }
        let (node, path) = (node.id, node.path.clone());
        let retired = self.clear_preview();
        let key = Arc::new(());
        self.preview = Some(Preview {
            key: key.clone(),
            epoch: self.epoch.clone(),
            node,
            path: path.clone(),
            source: (scope, configuration),
            maximum: policy.maximum_preview_bytes,
            maximum_lines: policy.maximum_preview_lines,
            request: None,
            lines: Vec::new(),
            binary: binary.into(),
            failed: failed.into(),
        });
        (
            retired,
            Some(PreviewRead {
                ticket: PreviewTicket(key),
                path,
                roots: self.roots.clone(),
                maximum: policy.maximum_preview_bytes as u64,
            }),
        )
    }
    pub(in crate::editor::session) fn admit_preview(
        &mut self,
        ticket: PreviewTicket,
        result: Result<RequestId, String>,
    ) -> bool {
        if !self.preview.as_ref().is_some_and(|preview| {
            self.preview_current(preview)
                && Arc::ptr_eq(&preview.key, &ticket.0)
                && preview.request.is_none()
        }) {
            return false;
        }
        let preview = self.preview.as_mut().expect("checked preview");
        match result {
            Ok(id) => preview.request = Some(id),
            Err(error) => preview.lines = vec![preview.failed.replace("{diagnostic}", &error)],
        }
        true
    }
    pub(in crate::editor::session) fn accept_preview(
        &mut self,
        id: RequestId,
        result: &Result<Vec<u8>, VfsError>,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        let Some(preview) = self
            .preview
            .as_ref()
            .filter(|preview| preview.request == Some(id))
        else {
            return false;
        };
        if !self.preview_current(preview) || preview.source != (scope, configuration) {
            self.preview = None;
            return true;
        }
        let preview = self.preview.as_mut().expect("checked preview");
        preview.request = None;
        preview.lines = match result {
            Ok(bytes) if bytes.len() > preview.maximum => vec![
                preview
                    .failed
                    .replace("{diagnostic}", "preview exceeds admitted byte limit"),
            ],
            Ok(bytes) => std::str::from_utf8(bytes).map_or_else(
                |_| vec![preview.binary.clone()],
                |text| {
                    text.lines()
                        .take(preview.maximum_lines)
                        .map(|line| {
                            line.chars()
                                .filter(|c| !c.is_control() || *c == '\t')
                                .take(4096)
                                .collect()
                        })
                        .collect()
                },
            ),
            Err(error) => vec![preview.failed.replace("{diagnostic}", &error.to_string())],
        };
        true
    }
    pub(super) fn preview_lines(&self) -> Vec<String> {
        self.preview
            .as_ref()
            .filter(|preview| self.preview_current(preview))
            .map_or_else(Vec::new, |preview| preview.lines.clone())
    }
}
