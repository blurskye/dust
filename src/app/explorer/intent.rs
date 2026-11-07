//! Explorer operations retain their exact request, confirmation and reconciliation context.
use crate::app::explorer::Explorer;
use crate::app::explorer::tree::ExplorerKind;
use crate::app::windows::NavigationStamp;
use crate::app::workspace::WorkspaceVersion;
use crate::editor::BufferPathChange;
use crate::editor::EditorWindowVersion;
use crate::files::ExplorerIoReply;
use crate::files::ExplorerMutation;
use crate::files::PreparedExplorerMutation;
use crate::files::RequestId;
use crate::workspace::WorkspaceRoot;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) struct RenamePrompt {
    path: PathBuf,
    root: PathBuf,
    epoch: Arc<()>,
    scope: WorkspaceVersion,
    configuration: u64,
    editor: EditorWindowVersion,
    pending: Option<RequestId>,
    inspected: Option<crate::files::ExplorerRenameSource>,
    submitted: Option<Arc<()>>,
}
pub(in crate::app) struct RenameInspection<'a>(&'a mut RenamePrompt);
impl RenameInspection<'_> {
    pub(in crate::app) fn register(self, id: RequestId) {
        self.0.pending = Some(id);
    }
}
pub(in crate::app) struct RenamePromptRequest {
    pub initial: String,
    pub root: PathBuf,
    pub path: PathBuf,
    pub roots: Vec<crate::files::RootIdentity>,
}
pub(in crate::app) struct MutationContext {
    pub workspace: WorkspaceVersion,
    pub configuration: u64,
    pub editor: EditorWindowVersion,
    pub change: Option<BufferPathChange>,
}
pub(in crate::app) struct MutationTicket(Arc<()>);
pub(in crate::app) struct MutationRequest {
    pub ticket: MutationTicket,
    pub operation: ExplorerMutation,
    pub roots: Vec<crate::files::RootIdentity>,
    pub maximum: u64,
}
enum Phase {
    Submitting,
    Preparing(RequestId),
    Ready(PreparedExplorerMutation),
    SubmittingApply,
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
pub(in crate::app) enum MutationUpdate {
    Confirmation(PathBuf),
    Apply(PreparedExplorerMutation),
    Complete {
        operation: ExplorerMutation,
        change: Option<BufferPathChange>,
    },
    Failed(String),
}
pub(in crate::app) struct OpenTicket(Arc<()>);
pub(in crate::app) struct OpenRequest {
    pub ticket: OpenTicket,
    pub path: PathBuf,
    pub roots: Vec<crate::files::RootIdentity>,
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
pub(in crate::app) struct OpenReply {
    pub navigation: NavigationStamp,
    pub split: bool,
}
impl Explorer {
    fn operation_roots(&self, roots: &[WorkspaceRoot]) -> Vec<crate::files::RootIdentity> {
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
    pub(in crate::app) fn mutation_pending(&self) -> bool {
        self.mutation.is_some()
    }
    fn selected_parent(&self) -> Result<PathBuf, String> {
        let node = self
            .tree()
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
            .tree()
            .selected_node()
            .ok_or("select an explorer path first")?;
        if node.kind == ExplorerKind::Root {
            return Err("workspace roots cannot be renamed or deleted from the explorer".into());
        }
        Ok(node.path.clone())
    }
    pub(in crate::app) fn create(&self, input: &str) -> Result<ExplorerMutation, String> {
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
    pub(in crate::app) fn begin_rename(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
        editor: EditorWindowVersion,
        roots: &[WorkspaceRoot],
    ) -> Result<RenamePromptRequest, String> {
        if self.rename.is_some() || self.mutation.is_some() {
            return Err("an explorer operation is already pending".into());
        }
        let path = self.mutable_path()?;
        let root = self
            .roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .ok_or("rename source has no workspace root")?
            .clone();
        let initial = path
            .strip_prefix(&root)
            .ok()
            .and_then(Path::to_str)
            .filter(|value| !value.chars().any(char::is_control) && value.len() <= 4096)
            .ok_or("rename source cannot be represented safely in the prompt")?
            .to_owned();
        if editor.revision().is_none()
            || roots
                .iter()
                .map(|r| &r.identity.canonical_path)
                .ne(self.roots.iter())
        {
            return Err("explorer rename source is stale".into());
        }
        self.rename = Some(RenamePrompt {
            path: path.clone(),
            root: root.clone(),
            epoch: self.epoch.clone(),
            scope,
            configuration,
            editor,
            pending: None,
            inspected: None,
            submitted: None,
        });
        Ok(RenamePromptRequest {
            initial,
            root,
            path,
            roots: self.operation_roots(roots),
        })
    }
    pub(in crate::app) fn rename_inspection(&mut self) -> Result<RenameInspection<'_>, String> {
        self.rename
            .as_mut()
            .filter(|rename| {
                rename.pending.is_none() && rename.inspected.is_none() && rename.submitted.is_none()
            })
            .map(RenameInspection)
            .ok_or_else(|| "rename inspection is stale or already consumed".into())
    }
    pub(in crate::app) fn accept_rename_inspection(
        &mut self,
        id: RequestId,
        result: Result<crate::files::ExplorerRenameSource, crate::files::VfsError>,
        scope: WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> Option<Result<PathBuf, String>> {
        if !self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.pending == Some(id))
        {
            return None;
        }
        let current = self.rename_current(&scope, configuration, editor);
        let rename = self.rename.as_mut().expect("matched rename inspection");
        rename.pending = None;
        Some(if !current {
            Err("explorer rename source is stale".into())
        } else {
            result
                .map_err(|error| error.to_string())
                .and_then(|source| {
                    if source.path() != rename.path {
                        return Err("explorer rename source path changed".into());
                    }
                    rename.inspected = Some(source);
                    Ok(rename.root.clone())
                })
        })
    }
    pub(in crate::app) fn rename_current(
        &self,
        scope: &WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> bool {
        self.rename.as_ref().is_some_and(|rename| {
            Arc::ptr_eq(&rename.epoch, &self.epoch)
                && &rename.scope == scope
                && rename.configuration == configuration
                && rename.editor.same_editor(editor)
                && editor.revision().is_some()
        })
    }
    pub(in crate::app) fn rename_value(
        &self,
        value: &str,
        scope: WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> Result<ExplorerMutation, String> {
        if !self.rename_current(&scope, configuration, editor) {
            return Err("explorer rename source is stale".into());
        }
        let rename = self.rename.as_ref().expect("current rename");
        if rename.inspected.is_none() {
            return Err(
                "rename source inspection is not ready; reopen the prompt if inspection failed"
                    .into(),
            );
        }
        if value.is_empty()
            || value.len() > 4096
            || value.chars().any(char::is_control)
            || !Path::new(value)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(
                "enter a nonempty workspace-relative path without traversal (maximum 4096 bytes)"
                    .into(),
            );
        }
        let target = rename.root.join(value);
        if target == rename.path {
            return Err("rename target is unchanged".into());
        }
        Ok(ExplorerMutation::Rename {
            source: rename.path.clone(),
            target,
        })
    }
    pub(in crate::app) fn submitted_rename_source(&mut self) -> crate::files::ExplorerRenameSource {
        let rename = self.rename.as_mut().expect("validated rename prompt");
        rename.submitted = self.mutation.as_ref().map(|intent| intent.key.clone());
        rename
            .inspected
            .as_ref()
            .expect("inspected rename source")
            .clone()
    }
    pub(in crate::app) fn cancel_rename(&mut self) -> Vec<RequestId> {
        let Some(rename) = self.rename.take() else {
            return Vec::new();
        };
        let mut ids: Vec<_> = rename.pending.into_iter().collect();
        if self.mutation.as_ref().is_some_and(|intent| {
            rename
                .submitted
                .as_ref()
                .is_some_and(|key| Arc::ptr_eq(key, &intent.key))
                && matches!(intent.phase, Phase::Submitting | Phase::Preparing(_))
        }) && let Some(intent) = self.mutation.take()
            && let Phase::Preparing(id) = intent.phase
        {
            ids.push(id);
        }
        ids
    }
    pub(in crate::app) fn rename(&self, target: &str) -> Result<ExplorerMutation, String> {
        if target.trim().is_empty() {
            return Err("rename target must not be empty".into());
        }
        let source = self.mutable_path()?;
        let target = source.parent().ok_or("source has no parent")?.join(target);
        Ok(ExplorerMutation::Rename { source, target })
    }
    pub(in crate::app) fn delete(&self) -> Result<ExplorerMutation, String> {
        Ok(ExplorerMutation::Delete {
            path: self.mutable_path()?,
        })
    }
    pub(in crate::app) fn clipboard(
        &mut self,
        cut: bool,
        scope: WorkspaceVersion,
    ) -> Result<String, String> {
        let path = self.mutable_path()?;
        let notice = format!(
            "{} `{}`; select a destination and press p",
            if cut { "moving" } else { "copied" },
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned()
            )
        );
        self.clipboard = Some((path, cut, scope));
        Ok(notice)
    }
    pub(in crate::app) fn paste(
        &self,
        scope: &WorkspaceVersion,
    ) -> Result<ExplorerMutation, String> {
        let (source, cut, expected) = self
            .clipboard
            .as_ref()
            .ok_or("explorer clipboard is empty")?;
        if expected != scope {
            return Err("explorer clipboard belongs to a previous workspace".into());
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
    pub(in crate::app) fn prepare_mutation(
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
            phase: Phase::Submitting,
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
    pub(in crate::app) fn track_mutation_request(
        &mut self,
        ticket: MutationTicket,
        result: Result<RequestId, String>,
    ) -> Result<(), String> {
        let intent = self
            .mutation
            .as_mut()
            .filter(|intent| {
                Arc::ptr_eq(&intent.key, &ticket.0) && matches!(intent.phase, Phase::Submitting)
            })
            .ok_or("explorer request is stale")?;
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
    pub(in crate::app) fn accept_mutation(
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
        // Once a write is accepted, its result must still reconcile buffer paths even
        // if the explorer closes or its configuration changes in the meantime.
        if !current && !matches!(intent.phase, Phase::Applying(_)) {
            return Some(MutationUpdate::Failed("explorer result is stale".into()));
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
                    intent.phase = Phase::SubmittingApply;
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
    pub(in crate::app) fn confirm_delete(
        &mut self,
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
        let mut intent = self.mutation.take().expect("checked confirmation");
        let Phase::Ready(prepared) = intent.phase else {
            self.mutation = Some(intent);
            return Err("explorer target inspection is not ready".into());
        };
        intent.phase = Phase::SubmittingApply;
        self.mutation = Some(intent);
        Ok(prepared)
    }
    pub(in crate::app) fn track_apply_request(
        &mut self,
        result: Result<RequestId, String>,
    ) -> Result<(), String> {
        let intent = self
            .mutation
            .as_mut()
            .filter(|i| matches!(i.phase, Phase::SubmittingApply))
            .ok_or("explorer apply request is stale")?;
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
    pub(in crate::app) fn cancel_confirmation(&mut self) -> Option<RequestId> {
        if self.mutation.as_ref().is_some_and(|i| {
            i.confirmation && !matches!(i.phase, Phase::Applying(_) | Phase::SubmittingApply)
        }) {
            return self.mutation.take().and_then(|i| match i.phase {
                Phase::Preparing(id) => Some(id),
                _ => None,
            });
        }
        None
    }
    pub(in crate::app) fn cancel_stale_intents(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
        editor: &EditorWindowVersion,
    ) -> Vec<RequestId> {
        let mut ids =
            if self.rename.is_some() && !self.rename_current(&scope, configuration, editor) {
                self.cancel_rename()
            } else {
                Vec::new()
            };
        if self.mutation.as_ref().is_some_and(|intent| {
            !matches!(intent.phase, Phase::Applying(_))
                && !self.mutation_current(intent, &scope, configuration, editor)
        }) && let Some(intent) = self.mutation.take()
            && let Phase::Preparing(id) = intent.phase
        {
            ids.push(id);
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
        ids
    }
    pub(in crate::app) fn confirmation_pending(&self) -> bool {
        self.mutation.as_ref().is_some_and(|intent| {
            intent.confirmation
                && !intent.invalid
                && matches!(intent.phase, Phase::Preparing(_) | Phase::Ready(_))
        })
    }
    pub(in crate::app) fn cancel_intents(&mut self) -> Vec<RequestId> {
        let mut ids = self.cancel_rename();
        if let Some(open) = self.open.take()
            && let Some((id, _)) = open.read
        {
            ids.push(id);
        }
        if let Some(intent) = self.mutation.take() {
            match intent.phase {
                Phase::Preparing(id) => ids.push(id),
                Phase::Applying(_) => self.mutation = Some(intent),
                _ => {}
            }
        }
        ids
    }
    pub(in crate::app) fn transport_stopped(&mut self) {
        self.cancel_intents();
        self.mutation = None;
    }
    pub(in crate::app) fn prepare_open(
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
            .tree()
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
        let previous_request = self
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
            previous_request,
            OpenRequest {
                ticket: OpenTicket(key),
                path,
                roots: self.operation_roots(roots),
            },
        ))
    }
    pub(in crate::app) fn track_open_request(
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
    pub(in crate::app) fn fail_open(&mut self, ticket: OpenTicket) {
        if self
            .open
            .as_ref()
            .is_some_and(|open| Arc::ptr_eq(&open.key, &ticket.0))
        {
            self.open = None;
        }
    }
    pub(in crate::app) fn accept_open(
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
    pub(in crate::app) fn exclude_buffer_path(&mut self, path: &Path) -> Option<RequestId> {
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
