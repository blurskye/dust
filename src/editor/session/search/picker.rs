//! Canonical picker rows, actions, interaction and derived presentation.
use super::{SearchSession, WorkspaceVersion};
use crate::{
    extensions::PluginLoadId,
    persistence::PersistedLocation,
    tools::{RegistryPackageMetadata, RegistryPackageSummary},
};
use crate::{
    ui::{PickerRow, PickerSurfaceKind, PickerUiState},
    workspace::{PickerItem, PickerItemId, PickerState},
};
use std::{
    collections::{HashMap, hash_map::Entry},
    path::PathBuf,
    sync::Arc,
};
#[derive(Clone, Debug)]
pub(in crate::editor::session) enum PickerPayload {
    Theme(crate::editor::session::configuration::ThemeKey),
    WorkspaceSearch(String),
    LspLocation(crate::language::lsp::LspLocation),
    Diagnostic(crate::editor::session::diagnostics::DiagnosticKey),
    CodeAction(crate::editor::session::lsp_edits::CodeActionKey),
    Buffer(crate::editor::BufferId),
    Command(String),
    ProposalFile(usize),
    WorkspaceRoot(crate::workspace::RootId),
    Terminal(crate::editor::session::terminal::TerminalKey),
    Window(crate::ui::window::WindowId),
    LspSession(String),
    Debug(crate::editor::session::debugger::DebugKey),
    PersistedLocation(PersistedLocation),
    GitHunk {
        path: PathBuf,
        status_revision: u64,
        repository: crate::workspace::git::GitRepositoryId,
        hunk: crate::workspace::git::GitHunkId,
        new_line: usize,
    },
    MatcherSource(MatcherSource),
    TaskTest(crate::editor::session::tasks::TestKey),
    RegistrySummary(RegistryPackageSummary),
    RegistryMetadata(Box<RegistryPackageMetadata>),
    PluginProviderItem {
        plugin: String,
        load: PluginLoadId,
        request: u64,
        item: String,
        action_command: String,
    },
}

#[derive(Clone)]
pub(in crate::editor::session) struct PickerEpoch(Arc<()>);
#[derive(Clone, Copy)]
pub(in crate::editor::session) enum PickerOrigin {
    General,
    PluginProvider(u64),
    WorkspaceEdit(crate::workspace::ProposalId),
    Files,
    Grep,
}
pub(in crate::editor::session) struct PickerContent {
    pub surface: PickerSurfaceKind,
    pub origin: PickerOrigin,
    pub title: String,
    pub items: Vec<(PickerItem, PickerPayload)>,
    pub query: String,
    pub complete: bool,
}
struct PickerInteraction {
    state: PickerState,
    payloads: HashMap<PickerItemId, PickerPayload>,
    surface: PickerSurfaceKind,
    origin: PickerOrigin,
    source: (WorkspaceVersion, u64),
    epoch: PickerEpoch,
}
#[derive(Default)]
pub(super) struct PickerSession {
    interaction: Option<PickerInteraction>,
    generation: u64,
}
impl SearchSession {
    pub(in crate::editor::session) fn install_picker(
        &mut self,
        content: PickerContent,
        maximum: usize,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Result<PickerEpoch, &'static str> {
        // Stream refreshes replace rows, not the user's interaction. A new search
        // invocation closes the old picker before admitting its producer.
        let previous = self
            .picker
            .interaction
            .as_ref()
            .filter(|picker| {
                picker.source == (scope.clone(), configuration)
                    && matches!(
                        (picker.origin, content.origin),
                        (PickerOrigin::Files, PickerOrigin::Files)
                            | (PickerOrigin::Grep, PickerOrigin::Grep)
                    )
            })
            .map(|picker| {
                (
                    picker.epoch.clone(),
                    picker.state.query().to_owned(),
                    picker.state.selected_item().map(|item| item.id.clone()),
                )
            });
        let generation = self
            .picker
            .generation
            .checked_add(1)
            .ok_or("picker generation exhausted")?;
        let mut state = PickerState::new(generation, content.title, maximum);
        let mut payloads = HashMap::new();
        state.append(
            generation,
            content.items.into_iter().filter_map(|(item, payload)| {
                // First row wins, exactly as PickerState::append does; overflow is never retained.
                if let Entry::Vacant(entry) = payloads.entry(item.id.clone()) {
                    entry.insert(payload);
                    Some(item)
                } else {
                    None
                }
            }),
            content.complete,
        );
        let mut epoch = PickerEpoch(Arc::new(()));
        let mut query = content.query;
        let mut selected = None;
        if let Some((original, previous_query, previous_selection)) = previous {
            epoch = original;
            if matches!(content.origin, PickerOrigin::Files) {
                query = previous_query.clone();
            }
            if query == previous_query {
                selected = previous_selection;
            }
        }
        if matches!(content.origin, PickerOrigin::Files) {
            state.set_query(query);
        } else {
            state.set_unfiltered_query(query);
        }
        if let Some(selected) = selected {
            state.select_id(&selected);
        }
        if !matches!(content.origin, PickerOrigin::Files) {
            self.cancel_files();
        }
        if !matches!(content.origin, PickerOrigin::Grep) && self.live {
            self.cancel_grep();
            self.live = false;
            self.query.clear();
        }
        self.picker.interaction = Some(PickerInteraction {
            state,
            payloads,
            surface: content.surface,
            origin: content.origin,
            source: (scope, configuration),
            epoch: epoch.clone(),
        });
        self.picker.generation = generation;
        Ok(epoch)
    }
    pub(in crate::editor::session) fn theme_picker_key(
        &self,
    ) -> Option<&crate::editor::session::configuration::ThemeKey> {
        self.picker
            .interaction
            .as_ref()?
            .payloads
            .values()
            .find_map(|payload| {
                if let PickerPayload::Theme(key) = payload {
                    Some(key)
                } else {
                    None
                }
            })
    }
    pub(in crate::editor::session) fn selected_picker_payload(&self) -> Option<&PickerPayload> {
        let picker = self.picker.interaction.as_ref()?;
        picker.payloads.get(&picker.state.selected_item()?.id)
    }
    pub(in crate::editor::session) fn take_picker_payload(&mut self) -> Option<PickerPayload> {
        let picker = self.picker.interaction.as_mut()?;
        picker.payloads.remove(&picker.state.selected_item()?.id)
    }
    pub(in crate::editor::session) fn picker_code_actions<'a>(
        &'a self,
        epoch: &'a PickerEpoch,
    ) -> impl Iterator<Item = &'a crate::editor::session::lsp_edits::CodeActionKey> {
        self.picker
            .interaction
            .iter()
            .filter(move |_| self.owns_picker_epoch(epoch))
            .flat_map(|picker| picker.payloads.values())
            .filter_map(|payload| match payload {
                PickerPayload::CodeAction(key) => Some(key),
                _ => None,
            })
    }
    pub(in crate::editor::session) fn workspace_picker(
        &self,
    ) -> Option<crate::workspace::ProposalId> {
        match self.picker.interaction.as_ref()?.origin {
            PickerOrigin::WorkspaceEdit(id) => Some(id),
            _ => None,
        }
    }
    pub(in crate::editor::session) fn picker(&self) -> Option<&PickerState> {
        self.picker.interaction.as_ref().map(|picker| &picker.state)
    }
    pub(in crate::editor::session) fn picker_epoch(&self) -> Option<&PickerEpoch> {
        self.picker.interaction.as_ref().map(|picker| &picker.epoch)
    }
    pub(in crate::editor::session) fn owns_picker_epoch(&self, epoch: &PickerEpoch) -> bool {
        self.picker
            .interaction
            .as_ref()
            .is_some_and(|picker| Arc::ptr_eq(&picker.epoch.0, &epoch.0))
    }
    pub(in crate::editor::session) fn picker_is_stale(
        &self,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        self.picker.interaction.as_ref().is_some_and(|picker| {
            picker.source != (scope, configuration)
                || match picker.origin {
                    PickerOrigin::General
                    | PickerOrigin::WorkspaceEdit(_)
                    | PickerOrigin::PluginProvider(_) => false,
                    PickerOrigin::Files => self.file_scope.is_none(),
                    PickerOrigin::Grep => {
                        self.scope.is_none() && !(self.live && self.query.is_empty())
                    }
                }
        })
    }
    pub(in crate::editor::session) fn picker_surface(&self) -> PickerSurfaceKind {
        self.picker
            .interaction
            .as_ref()
            .map_or(PickerSurfaceKind::Generic, |picker| picker.surface)
    }
    pub(super) fn retire_picker(&mut self) {
        self.picker.interaction = None;
    }
    pub(in crate::editor::session) fn picker_insert(&mut self, text: &str) -> Option<String> {
        let picker = &mut self.picker.interaction.as_mut()?.state;
        picker.insert_query(text);
        Some(picker.query().to_owned())
    }
    pub(in crate::editor::session) fn picker_backspace(&mut self) -> Option<String> {
        let picker = &mut self.picker.interaction.as_mut()?.state;
        picker.backspace_query();
        Some(picker.query().to_owned())
    }
    pub(in crate::editor::session) fn picker_move(&mut self, direction: i8, wrap: bool) {
        if let Some(picker) = &mut self.picker.interaction {
            picker.state.move_selection(direction, wrap);
        }
    }
    pub(in crate::editor::session) fn picker_scroll(&mut self, rows: usize) -> Option<usize> {
        let picker = &mut self.picker.interaction.as_mut()?.state;
        picker.ensure_selected_visible(rows);
        Some(picker.scroll())
    }
    pub(in crate::editor::session) fn picker_follows_tail(&self) -> bool {
        self.picker().is_none_or(PickerState::selected_is_last)
    }
    pub(in crate::editor::session) fn picker_bookmark(
        &self,
    ) -> Option<(String, Option<PickerItemId>)> {
        self.picker().map(|picker| {
            (
                picker.query().into(),
                picker.selected_item().map(|item| item.id.clone()),
            )
        })
    }
    pub(in crate::editor::session) fn restore_picker_view(
        &mut self,
        previous: Option<(String, Option<PickerItemId>)>,
        follow_tail: bool,
    ) {
        let Some(picker) = &mut self.picker.interaction else {
            return;
        };
        if let Some((query, selected)) = previous {
            picker.state.set_query(query);
            if let Some(selected) = selected {
                picker.state.select_id(&selected);
            }
        }
        if follow_tail {
            picker.state.select_last();
        }
    }
    pub(in crate::editor::session) fn picker_projection(&self) -> Option<PickerUiState> {
        let picker = self.picker.interaction.as_ref()?;
        let presentation = picker.state.presentation();
        Some(PickerUiState {
            input_hint: String::new(),
            surface: picker.surface,
            title: presentation.title,
            query: presentation.query,
            rows: presentation
                .rows
                .into_iter()
                .map(|(id, label, detail, source)| PickerRow {
                    id: id.0,
                    label,
                    detail,
                    source,
                })
                .collect(),
            selected: presentation.selected,
            scroll: presentation.scroll,
            loading: presentation.loading,
            preview: self.preview.clone(),
        })
    }
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct MatcherSource {
    pub(in crate::editor::session) path: PathBuf,
    pub(in crate::editor::session) line: usize,
    pub(in crate::editor::session) column: usize,
}
