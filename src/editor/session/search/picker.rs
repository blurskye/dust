//! Canonical picker rows, actions, interaction and derived presentation.
use super::{SearchSession, WorkspaceVersion};
use crate::{
    extensions::PluginLoadId,
    persistence::PersistedLocation,
    tools::{RegistryPackageMetadata, RegistryPackageSummary},
};
use crate::{
    ui::{PickerCell, PickerRow, PickerSurfaceKind, PickerUiState, Style},
    workspace::{PickerItem, PickerItemId, PickerState},
};
use std::{
    collections::{HashMap, hash_map::Entry},
    path::PathBuf,
    sync::Arc,
};
#[derive(Clone, Debug)]
pub(in crate::editor::session) enum PickerPayload {
    /// A row that groups others and is not an action: accepting it does nothing, and
    /// selection steps over it.
    Section,
    /// A workspace from the project list.
    Project(std::path::PathBuf),
    /// A row of one of the agent pickers.
    Agent(crate::editor::session::dispatch::agent::AgentChoice),
    /// A pull request number from `gh pr list`.
    PullRequest(u64),
    GitWorktree {
        #[allow(dead_code)]
        name: String,
        /// Carried so the open and close commands act on the row's own worktree instead of
        /// re-deriving a path from display text.
        #[allow(dead_code)]
        path: std::path::PathBuf,
    },
    Theme(crate::editor::session::configuration::ThemeKey),
    WorkspaceSearch(String),
    LspLocation(crate::language::lsp::LspLocation),
    Diagnostic(crate::editor::session::diagnostics::DiagnosticKey),
    CodeAction(crate::editor::session::lsp_edits::CodeActionKey),
    Buffer(crate::editor::BufferId),
    Command(String),
    ProposalFile(usize),
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
    /// The surface's own footer legend, describing what its keys do here rather than the
    /// widget's mode. `None` leaves the projected keymap hint in place.
    footer: Option<String>,
    /// Which comparison the change surface's diff pane is showing. Only meaningful for that
    /// surface; it is where `Tab` switches between the unstaged and staged layers.
    changes_layer: crate::workspace::git::GitDiffLayer,
    origin: PickerOrigin,
    source: (WorkspaceVersion, u64),
    epoch: PickerEpoch,
}
#[derive(Default)]
pub(super) struct PickerSession {
    interaction: Option<PickerInteraction>,
    generation: u64,
    /// The surface that help was opened over, kept so closing help returns the user to the
    /// question they were looking at. Depth one: help opened over help replaces this, so the
    /// snapshot can never grow into an unbounded stack.
    suspended: Option<PickerInteraction>,
    /// Whether the surface on screen right now is a help page. Only then does closing resume
    /// the suspended surface; closing any other picker closes it as it always did.
    help_open: bool,
    /// Set while the funnel that opens a help page is installing that page, so the install
    /// does not discard the very snapshot the page was opened to preserve.
    help_install: bool,
}
impl PickerSession {
    /// Move the current surface aside so help can take the picker. Only one surface is ever
    /// suspended: help is a page you read and leave, not a place you nest.
    pub(in crate::editor::session) fn suspend_for_help(&mut self) {
        self.help_install = true;
        if self.help_open {
            // Help opened over help: the page underneath is another help page, so the snapshot
            // the user actually wants is the one already saved. Depth stays one: the
            // intermediate page is dropped rather than stacked.
            self.interaction = None;
            return;
        }
        if self.interaction.is_some() {
            self.suspended = self.interaction.take();
        }
    }
    /// Whether the surface on screen is a help page. Accepting a row inside help is not an
    /// invocation, so the accept path asks this before it runs a row's command.
    pub(in crate::editor::session) fn help_is_open(&self) -> bool {
        self.help_open
    }
    /// Mark that the surface now on screen is a help page. Called after the page is installed,
    /// so the install itself cannot discard what was suspended for it.
    pub(in crate::editor::session) fn mark_help_open(&mut self) {
        self.help_open = true;
    }
    /// Consume the one-shot marker left by the help funnel, so an ordinary install still
    /// discards a stale snapshot while a help page's own install does not.
    fn take_help_install(&mut self) -> bool {
        std::mem::take(&mut self.help_install)
    }
    /// If help is on screen, put the suspended surface back, preserving its query, its rows and
    /// its selected row, and report that the close was absorbed. Returns false when the surface
    /// being closed is not help, so an ordinary close still closes.
    pub(in crate::editor::session) fn resume_after_help(&mut self) -> bool {
        if !self.help_open {
            return false;
        }
        self.help_open = false;
        match self.suspended.take() {
            Some(previous) => {
                self.interaction = Some(previous);
                true
            }
            // Nothing was underneath, so this close closes the help page itself. Absorbing it
            // here would leave the page on screen with no way to dismiss it.
            None => false,
        }
    }
}
impl SearchSession {
    pub(in crate::editor::session) fn install_picker(
        &mut self,
        content: PickerContent,
        maximum: usize,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Result<PickerEpoch, &'static str> {
        if !self.picker.take_help_install() && self.picker.help_open {
            // A genuinely new surface supersedes both the help page and what it was showing, so
            // the stale snapshot is dropped rather than restored later.
            self.picker.help_open = false;
            self.picker.suspended = None;
        }
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
            footer: surface_footer(content.surface),
            changes_layer: crate::workspace::git::GitDiffLayer::Unstaged,
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

    /// Re-present the open picker on another surface, keeping its rows, query and selection.
    /// This is how a picker sends its results to the shared Results surface.
    pub(in crate::editor::session) fn set_picker_surface(
        &mut self,
        surface: PickerSurfaceKind,
    ) -> bool {
        let Some(picker) = self.picker.interaction.as_mut() else {
            return false;
        };
        picker.surface = surface;
        picker.footer = surface_footer(surface);
        true
    }

    /// Replace the open surface's footer legend, for a legend that carries a live count.
    pub(in crate::editor::session) fn set_picker_footer(&mut self, footer: String) {
        if let Some(picker) = self.picker.interaction.as_mut() {
            picker.footer = Some(footer);
        }
    }

    /// Which comparison the change surface's diff pane shows.
    pub(in crate::editor::session) fn changes_layer(&self) -> crate::workspace::git::GitDiffLayer {
        self.picker
            .interaction
            .as_ref()
            .map_or(crate::workspace::git::GitDiffLayer::Unstaged, |picker| {
                picker.changes_layer
            })
    }

    /// Switch the change surface's diff pane to the other comparison and report which one it
    /// now shows, so the caller can refresh the pane.
    pub(in crate::editor::session) fn switch_changes_layer(
        &mut self,
    ) -> Option<crate::workspace::git::GitDiffLayer> {
        let picker = self.picker.interaction.as_mut()?;
        if picker.surface != PickerSurfaceKind::Changes {
            return None;
        }
        picker.changes_layer = match picker.changes_layer {
            crate::workspace::git::GitDiffLayer::Unstaged => {
                crate::workspace::git::GitDiffLayer::Staged
            }
            crate::workspace::git::GitDiffLayer::Staged => {
                crate::workspace::git::GitDiffLayer::Unstaged
            }
        };
        Some(picker.changes_layer)
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
            footer: picker.footer.clone(),
            // No key in the document chooses the side-by-side layout yet, so both sides sit
            // at the top; the renderer honours whatever a surface passes here.
            preview_side_scroll: (0, 0),
            title: presentation.title,
            query: presentation.query,
            rows: presentation
                .rows
                .into_iter()
                .map(|row| match row.kind {
                    crate::workspace::PickerItemKind::Section => PickerRow::section(row.label),
                    crate::workspace::PickerItemKind::Entry => {
                        // Only the change rows carry the change columns; a row that is not a
                        // path, such as the commit draft, keeps the full list width.
                        let cells = if picker.surface == PickerSurfaceKind::Changes
                            && row.id.0.starts_with("git:")
                        {
                            changes_row_cells(&row)
                        } else {
                            Vec::new()
                        };
                        let mut rendered =
                            PickerRow::text(row.id.0, row.label, row.detail, row.source);
                        rendered.cells = cells;
                        rendered
                    }
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

/// A surface's own footer legend: what its keys do on that surface, rather than the widget's
/// mode. Only the surfaces that draw a footer have one.
fn surface_footer(surface: PickerSurfaceKind) -> Option<String> {
    match surface {
        // The change surface sets its own legend so it can carry the live staged count.
        PickerSurfaceKind::Changes => None,
        _ => None,
    }
}

/// The columns a change row draws: the two-character status code, the path, and an unsaved
/// badge when the buffer has edits.
///
/// The status code is its own glyph column rather than a colour, so the meaning survives a
/// terminal without colour. The path column is flexible, so a narrow picker clips the path
/// instead of the provenance.
fn changes_row_cells(source: &crate::workspace::PickerRowPresentation) -> Vec<PickerCell> {
    let code = source.source.clone();
    let status_style = match code.chars().next() {
        // A path with only unsaved edits has no comparison code to colour.
        Some(' ') | None => Style::Muted,
        // Unmerged: both sides touched it.
        Some('U') => Style::Error,
        // Untracked.
        Some('?') => Style::Muted,
        Some('A') => Style::StatusAccent,
        Some('D') => Style::Error,
        Some('R') => Style::Warning,
        _ => Style::Warning,
    };
    let mut cells = vec![
        PickerCell::new(code, status_style, 3),
        PickerCell::flex(source.label.clone(), Style::Default),
    ];
    // The section header already names the comparison, so only an unsaved buffer gets a badge.
    if !source.badge.is_empty() {
        let badge = format!(" {}", source.badge);
        let width = u16::try_from(badge.chars().count()).unwrap_or(u16::MAX);
        cells.push(PickerCell::new(badge, Style::Warning, width));
    }
    cells
}
