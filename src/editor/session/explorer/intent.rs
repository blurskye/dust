//! Explorer operations retain their exact request, confirmation and reconciliation context.
use super::{ExplorerKind, ExplorerSession, WorkspaceVersion};
use crate::editor::session::windows::NavigationStamp;
use crate::{
    editor::{EditorWindowVersion, transaction::BufferPathChange},
    files::{ExplorerIoReply, ExplorerMutation, PreparedExplorerMutation, RequestId},
    workspace::WorkspaceRoot,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(in crate::editor::session) struct MutationContext {
    pub workspace: WorkspaceVersion,
    pub configuration: u64,
    pub editor: EditorWindowVersion,
    pub change: Option<BufferPathChange>,
}
pub(in crate::editor::session) struct MutationTicket(Arc<()>);
pub(in crate::editor::session) struct MutationRequest {
    pub ticket: MutationTicket,
    pub operation: ExplorerMutation,
    pub roots: Vec<crate::workspace::RootIdentity>,
    pub maximum: u64,
}
enum Phase {
    Admitting,
    Preparing(RequestId),
    Ready(PreparedExplorerMutation),
    AdmittingApply,
    Applying(RequestId),
}
pub(super) struct MutationIntent {
    key: Arc<()>,
    epoch: Arc<()>,
    context: MutationContext,
    operation: ExplorerMutation,
    phase: Phase,
    confirmation: bool,
    invalid: bool,
}
pub(in crate::editor::session) enum MutationUpdate {
    Confirmation(PathBuf),
    Apply(PreparedExplorerMutation),
    Complete {
        operation: ExplorerMutation,
        change: Option<BufferPathChange>,
    },
    Failed(String),
}
pub(in crate::editor::session) struct OpenTicket(Arc<()>);
pub(in crate::editor::session) struct OpenRequest {
    pub ticket: OpenTicket,
    pub path: PathBuf,
    pub roots: Vec<crate::workspace::RootIdentity>,
}
pub(super) struct OpenIntent {
    key: Arc<()>,
    epoch: Arc<()>,
    path: PathBuf,
    source: (WorkspaceVersion, u64),
    editor: EditorWindowVersion,
    read: Option<(RequestId, NavigationStamp)>,
    split: bool,
    excluded: Vec<PathBuf>,
}
pub(in crate::editor::session) struct OpenReply {
    pub navigation: NavigationStamp,
    pub split: bool,
}
impl ExplorerSession {
    fn operation_roots(&self, roots: &[WorkspaceRoot]) -> Vec<crate::workspace::RootIdentity> {
        roots
            .iter()
            .map(|root| {
                if root.identity.filesystem_identity.is_some() {
                    return root.identity.clone();
                }
                self.observed_roots
                    .iter()
                    .find(|observed| observed.canonical_path == root.identity.canonical_path)
                    .unwrap_or(&root.identity)
                    .clone()
            })
            .collect()
    }
    pub(in crate::editor::session) fn mutation_pending(&self) -> bool {
        self.mutation.is_some()
    }
    fn selected_parent(&self) -> Result<PathBuf, String> {
        let node = self
            .tree
            .selected_node()
            .ok_or("select an explorer directory first")?;
        match node.kind {
            ExplorerKind::Root | ExplorerKind::Directory => Ok(node.path.clone()),
            _ => node
                .path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| "selected path has no parent".into()),
        }
    }
    fn mutable_path(&self) -> Result<PathBuf, String> {
        let node = self
            .tree
            .selected_node()
            .ok_or("select an explorer path first")?;
        if node.kind == ExplorerKind::Root {
            return Err("workspace roots cannot be renamed or deleted from the explorer".into());
        }
        Ok(node.path.clone())
    }
    pub(in crate::editor::session) fn create(
        &self,
        input: &str,
    ) -> Result<ExplorerMutation, String> {
        let directory = input.ends_with('/') || input.ends_with(std::path::MAIN_SEPARATOR);
        let input = input.trim_end_matches(['/', std::path::MAIN_SEPARATOR]);
        if input.is_empty() {
            return Err("explorer path must not be empty".into());
        }
        Ok(ExplorerMutation::Create {
            path: self.selected_parent()?.join(input),
            directory,
        })
    }
    pub(in crate::editor::session) fn rename(
        &self,
        target: &str,
    ) -> Result<ExplorerMutation, String> {
        if target.trim().is_empty() {
            return Err("rename target must not be empty".into());
        }
        let source = self.mutable_path()?;
        let target = source.parent().ok_or("source has no parent")?.join(target);
        Ok(ExplorerMutation::Rename { source, target })
    }
    pub(in crate::editor::session) fn delete(&self) -> Result<ExplorerMutation, String> {
        Ok(ExplorerMutation::Delete {
            path: self.mutable_path()?,
        })
    }
    pub(in crate::editor::session) fn clipboard(
        &mut self,
        cut: bool,
        scope: WorkspaceVersion,
    ) -> Result<String, String> {
        let path = self.mutable_path()?;
        let notice = format!(
            "{} `{}`; select a destination and press p",
            if cut { "moving" } else { "copied" },
            path.display()
        );
        self.clipboard = Some((path, cut, scope));
        Ok(notice)
    }
    pub(in crate::editor::session) fn paste(
        &self,
        scope: &WorkspaceVersion,
    ) -> Result<ExplorerMutation, String> {
        let (source, cut, expected) = self
            .clipboard
            .as_ref()
            .ok_or("explorer clipboard is empty")?;
        if expected != scope {
            return Err("explorer clipboard belongs to a retired workspace".into());
        }
        let target = self
            .selected_parent()?
            .join(source.file_name().ok_or("copied path has no filename")?);
        Ok(if *cut {
            ExplorerMutation::Rename {
                source: source.clone(),
                target,
            }
        } else {
            ExplorerMutation::Copy {
                source: source.clone(),
                target,
            }
        })
    }
    pub(in crate::editor::session) fn prepare_mutation(
        &mut self,
        operation: ExplorerMutation,
        context: MutationContext,
        roots: &[WorkspaceRoot],
        maximum: u64,
        confirmation: bool,
    ) -> Result<MutationRequest, String> {
        if self.mutation.is_some() {
            return Err("an explorer operation is already pending".into());
        }
        if context.editor.revision().is_none()
            || roots
                .iter()
                .map(|r| &r.identity.canonical_path)
                .ne(self.roots.iter())
        {
            return Err("explorer source is stale or exhausted".into());
        }
        let key = Arc::new(());
        self.mutation = Some(MutationIntent {
            key: key.clone(),
            epoch: self.epoch.clone(),
            context,
            operation: operation.clone(),
            phase: Phase::Admitting,
            confirmation,
            invalid: false,
        });
        Ok(MutationRequest {
            ticket: MutationTicket(key),
            operation,
            roots: self.operation_roots(roots),
            maximum,
        })
    }
    pub(in crate::editor::session) fn admit_mutation(
        &mut self,
        ticket: MutationTicket,
        result: Result<RequestId, String>,
    ) -> Result<(), String> {
        let intent = self
            .mutation
            .as_mut()
            .filter(|intent| {
                Arc::ptr_eq(&intent.key, &ticket.0) && matches!(intent.phase, Phase::Admitting)
            })
            .ok_or("explorer admission retired")?;
        match result {
            Ok(id) => {
                intent.phase = Phase::Preparing(id);
                Ok(())
            }
            Err(error) => {
                self.mutation = None;
                Err(error)
            }
        }
    }
    fn mutation_current(
        &self,
        intent: &MutationIntent,
        scope: &WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> bool {
        !intent.invalid
            && Arc::ptr_eq(&intent.epoch, &self.epoch)
            && &intent.context.workspace == scope
            && intent.context.configuration == configuration
            && intent.context.editor.same_editor(editor)
            && editor.revision().is_some()
    }
    pub(in crate::editor::session) fn accept_mutation(
        &mut self,
        reply: ExplorerIoReply,
        scope: WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> Option<MutationUpdate> {
        let intent = self.mutation.as_ref()?;
        let expected = match intent.phase {
            Phase::Preparing(id) | Phase::Applying(id) => id,
            _ => return None,
        };
        if reply.id() != expected {
            return None;
        }
        let current = self.mutation_current(intent, &scope, configuration, editor);
        let mut intent = self.mutation.take().expect("checked request");
        if !current {
            return Some(MutationUpdate::Failed("explorer result retired; an admitted disk operation may have completed, but buffers were not changed".into()));
        }
        match (reply, &intent.phase) {
            (
                ExplorerIoReply::Prepared {
                    result: Ok(prepared),
                    ..
                },
                Phase::Preparing(_),
            ) => {
                intent.operation = prepared.operation().clone();
                let update = if intent.confirmation {
                    let path = intent.operation.source().expect("delete source").into();
                    intent.phase = Phase::Ready(prepared);
                    MutationUpdate::Confirmation(path)
                } else {
                    intent.phase = Phase::AdmittingApply;
                    MutationUpdate::Apply(prepared)
                };
                self.mutation = Some(intent);
                Some(update)
            }
            (ExplorerIoReply::Applied { result: Ok(()), .. }, Phase::Applying(_)) => {
                if let ExplorerMutation::Rename { source, .. } = &intent.operation
                    && self
                        .clipboard
                        .as_ref()
                        .is_some_and(|(path, cut, _)| *cut && path == source)
                {
                    self.clipboard = None;
                }
                Some(MutationUpdate::Complete {
                    operation: intent.operation,
                    change: intent.context.change,
                })
            }
            (
                ExplorerIoReply::Prepared {
                    result: Err(error), ..
                }
                | ExplorerIoReply::Applied {
                    result: Err(error), ..
                },
                _,
            ) => Some(MutationUpdate::Failed(error.to_string())),
            _ => Some(MutationUpdate::Failed(
                "explorer worker returned the wrong terminal result".into(),
            )),
        }
    }
    pub(in crate::editor::session) fn confirm_delete(
        &mut self,
        phrase: &str,
        scope: WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> Result<PreparedExplorerMutation, String> {
        let intent = self
            .mutation
            .as_ref()
            .ok_or("explorer delete request is stale")?;
        if !intent.confirmation || !self.mutation_current(intent, &scope, configuration, editor) {
            return Err("explorer delete request is stale".into());
        }
        if phrase.trim() != "delete" {
            return Err("explorer delete confirmation did not match".into());
        }
        let mut intent = self.mutation.take().expect("checked confirmation");
        let Phase::Ready(prepared) = intent.phase else {
            self.mutation = Some(intent);
            return Err("explorer target inspection is not ready".into());
        };
        intent.phase = Phase::AdmittingApply;
        self.mutation = Some(intent);
        Ok(prepared)
    }
    pub(in crate::editor::session) fn admit_apply(
        &mut self,
        result: Result<RequestId, String>,
    ) -> Result<(), String> {
        let intent = self
            .mutation
            .as_mut()
            .filter(|i| matches!(i.phase, Phase::AdmittingApply))
            .ok_or("explorer apply admission retired")?;
        match result {
            Ok(id) => {
                intent.phase = Phase::Applying(id);
                Ok(())
            }
            Err(error) => {
                self.mutation = None;
                Err(error)
            }
        }
    }
    pub(super) fn retire_changed_confirmation(&mut self) {
        if let Some(intent) = &mut self.mutation
            && intent.confirmation
            && !matches!(intent.phase, Phase::Applying(_) | Phase::AdmittingApply)
            && self
                .tree
                .selected_node()
                .is_none_or(|node| intent.operation.source() != Some(node.path.as_path()))
        {
            intent.invalid = true;
        }
    }
    pub(in crate::editor::session) fn cancel_confirmation(&mut self) -> Option<RequestId> {
        if self.mutation.as_ref().is_some_and(|i| {
            i.confirmation && !matches!(i.phase, Phase::Applying(_) | Phase::AdmittingApply)
        }) {
            return self.mutation.take().and_then(|i| match i.phase {
                Phase::Preparing(id) => Some(id),
                _ => None,
            });
        }
        None
    }
    pub(in crate::editor::session) fn retire_stale_intents(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> (Vec<RequestId>, bool) {
        let mut ids = Vec::new();
        let mut may_write = false;
        if self
            .mutation
            .as_ref()
            .is_some_and(|intent| !self.mutation_current(intent, &scope, configuration, editor))
        {
            if let Some(intent) = self.mutation.take() {
                match intent.phase {
                    Phase::Preparing(id) => ids.push(id),
                    Phase::Applying(id) => {
                        ids.push(id);
                        may_write = true;
                    }
                    _ => {}
                }
            }
        }
        if self.open.as_ref().is_some_and(|open| {
            !Arc::ptr_eq(&open.epoch, &self.epoch)
                || open.source != (scope, configuration)
                || !open.editor.same_editor(editor)
                || editor.revision().is_none()
        }) && let Some(open) = self.open.take()
            && let Some((id, _)) = open.read
        {
            ids.push(id);
        }
        (ids, may_write)
    }
    pub(in crate::editor::session) fn confirmation_pending(&self) -> bool {
        self.mutation.as_ref().is_some_and(|intent| {
            intent.confirmation
                && !intent.invalid
                && matches!(intent.phase, Phase::Preparing(_) | Phase::Ready(_))
        })
    }
    pub(in crate::editor::session) fn retire_intents(&mut self) -> (Vec<RequestId>, bool) {
        let mut ids = Vec::new();
        if let Some(open) = self.open.take()
            && let Some((id, _)) = open.read
        {
            ids.push(id);
        }
        let mut may_write = false;
        if let Some(intent) = self.mutation.take() {
            match intent.phase {
                Phase::Preparing(id) => ids.push(id),
                Phase::Applying(id) => {
                    ids.push(id);
                    may_write = true;
                }
                _ => {}
            }
        }
        (ids, may_write)
    }
    pub(in crate::editor::session) fn prepare_open(
        &mut self,
        split: bool,
        source: (WorkspaceVersion, u64),
        editor: EditorWindowVersion,
        roots: &[WorkspaceRoot],
    ) -> Result<(Option<RequestId>, OpenRequest), String> {
        if roots
            .iter()
            .map(|r| &r.identity.canonical_path)
            .ne(self.roots.iter())
        {
            return Err("explorer roots changed".into());
        }
        let node = self
            .tree
            .selected_node()
            .ok_or("select an explorer path first")?;
        if matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory) {
            return Err("selected path is a directory".into());
        }
        if editor.revision().is_none() {
            return Err("editor source revision is exhausted".into());
        }
        let path = node.path.clone();
        let key = Arc::new(());
        let retired = self
            .open
            .take()
            .and_then(|open| open.read.map(|(id, _)| id));
        self.open = Some(OpenIntent {
            key: key.clone(),
            epoch: self.epoch.clone(),
            path: path.clone(),
            source,
            editor,
            read: None,
            split,
            excluded: Vec::new(),
        });
        Ok((
            retired,
            OpenRequest {
                ticket: OpenTicket(key),
                path,
                roots: self.operation_roots(roots),
            },
        ))
    }
    pub(in crate::editor::session) fn admit_open(
        &mut self,
        ticket: OpenTicket,
        id: RequestId,
        navigation: NavigationStamp,
    ) -> bool {
        let Some(open) = &mut self.open else {
            return false;
        };
        if !Arc::ptr_eq(&open.key, &ticket.0) || open.read.is_some() {
            return false;
        }
        open.read = Some((id, navigation));
        true
    }
    pub(in crate::editor::session) fn fail_open(&mut self, ticket: OpenTicket) {
        if self
            .open
            .as_ref()
            .is_some_and(|open| Arc::ptr_eq(&open.key, &ticket.0))
        {
            self.open = None;
        }
    }
    pub(in crate::editor::session) fn accept_open(
        &mut self,
        id: RequestId,
        path: &Path,
        source: (WorkspaceVersion, u64),
        editor: &EditorWindowVersion,
    ) -> Option<Result<OpenReply, String>> {
        let open = self.open.as_ref()?;
        if open
            .read
            .as_ref()
            .is_none_or(|(expected, _)| *expected != id)
        {
            return None;
        }
        let open = self.open.take().expect("checked read");
        if !Arc::ptr_eq(&open.epoch, &self.epoch)
            || open.source != source
            || !open.editor.same_editor(editor)
            || editor.revision().is_none()
            || open.excluded.iter().any(|closed| closed == path)
        {
            return Some(Err("explorer open result is stale".into()));
        }
        Some(Ok(OpenReply {
            navigation: open.read.expect("checked navigation").1,
            split: open.split,
        }))
    }
    pub(in crate::editor::session) fn retire_buffer_path(
        &mut self,
        path: &Path,
    ) -> Option<RequestId> {
        let open = self.open.as_mut()?;
        if open.path == path || open.excluded.len() >= 64 {
            return self
                .open
                .take()
                .and_then(|open| open.read.map(|(id, _)| id));
        }
        if !open.excluded.iter().any(|closed| closed == path) {
            open.excluded.push(path.into());
        }
        None
    }
}
