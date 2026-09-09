//! Canonical picker interaction and derived presentation; provider payloads stay outside this owner.
use super::{SearchSession, WorkspaceVersion};
use crate::{
    ui::{PickerRow, PickerSurfaceKind, PickerUiState},
    workspace::{PickerItem, PickerItemId, PickerState},
};
use std::sync::Arc;

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
    pub items: Vec<PickerItem>,
    pub query: String,
    pub complete: bool,
}
struct PickerInteraction {
    state: PickerState,
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
        let generation = self
            .picker
            .generation
            .checked_add(1)
            .ok_or("picker generation exhausted")?;
        let mut state = PickerState::new(generation, content.title, maximum);
        state.append(generation, content.items, content.complete);
        state.set_unfiltered_query(content.query);
        if !matches!(content.origin, PickerOrigin::Files) {
            self.cancel_files();
        }
        if !matches!(content.origin, PickerOrigin::Grep) && self.live {
            self.cancel_grep();
            self.live = false;
            self.query.clear();
        }
        let epoch = PickerEpoch(Arc::new(()));
        self.picker.interaction = Some(PickerInteraction {
            state,
            surface: content.surface,
            origin: content.origin,
            source: (scope, configuration),
            epoch: epoch.clone(),
        });
        self.picker.generation = generation;
        Ok(epoch)
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
