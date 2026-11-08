//! Canonical picker rows, actions, interaction and derived presentation.
use crate::app::search::Search;
use crate::app::workspace::WorkspaceVersion;
use crate::persistence::PersistedLocation;
use crate::plugins::PluginLoadId;
use crate::tools::RegistryPackageMetadata;
use crate::tools::RegistryPackageSummary;
use crate::ui::PickerCell;
use crate::ui::PickerRow;
use crate::ui::PickerSurfaceKind;
use crate::ui::PickerUiState;
use crate::ui::Style;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::workspace::PickerState;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::sync::Arc;
#[derive(Clone, Debug)]
pub(in crate::app) enum PickerPayload {
    Selection(String),
    Plugin(crate::app::plugins::browser::PluginChoice),
    /// A row whose whole text opens in a popup with this title, such as a past notice.
    Notice {
        title: String,
        text: String,
    },
    /// A row that groups others and is not an action: accepting it does nothing, and
    /// selection steps over it.
    Section,
    /// A workspace from the project list.
    Project(std::path::PathBuf),
    /// A recovery journal, by the journal's own path.
    Recovery(std::path::PathBuf),
    /// A code lens resolved and run on the server that offered it.
    LspCodeLens {
        server: String,
        context: Box<crate::language::lsp::EditContext>,
        lens: crate::language::lsp::LspCodeLens,
    },
    /// A row of one of the agent pickers.
    Agent(crate::app::agent::AgentChoice),
    /// Where the tutor starts: the saved step, or the first one in a language.
    Tutor(crate::app::tutor::TutorStart),
    /// A pull request number from `gh pr list`.
    PullRequest {
        root: PathBuf,
        request_id: u64,
        number: u64,
        local: Option<PathBuf>,
    },
    GitWorktree {
        name: String,
        main: bool,
        /// Carried so the open and close commands act on the row's own worktree instead of
        /// re-deriving a path from display text.
        #[allow(dead_code)]
        path: std::path::PathBuf,
    },
    Theme(crate::app::configuration::ThemeKey),
    WorkspaceSearch(String),
    LspLocation(
        crate::language::lsp::LspLocation,
        crate::editor::text::PositionEncoding,
    ),
    Diagnostic(crate::app::diagnostics::DiagnosticKey),
    CodeAction(crate::app::language::edits::CodeActionKey),
    Buffer(crate::editor::BufferId),
    Command(String),
    ProposalFile(usize),
    Terminal(crate::app::terminal::TerminalKey),
    Window(crate::windows::WindowId),
    LspSession(String),
    Debug(crate::app::debugger::DebugKey),
    DebugLaunch(Box<crate::app::debugger::PresetLaunch>),
    DebugConfiguration(String),
    PersistedLocation(PersistedLocation),
    GitHunk {
        path: PathBuf,
        status_revision: u64,
        repository: crate::git::GitRepositoryId,
        hunk: crate::git::GitHunkId,
        new_line: usize,
    },
    MatcherSource(MatcherSource),
    TaskTest(crate::app::tasks::TestKey),
    TaskOutput(String),
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
pub(in crate::app) struct PickerEpoch(Arc<()>);
#[derive(Clone, Copy)]
pub(in crate::app) enum PickerOrigin {
    General,
    Plugins,
    PluginProvider(u64),
    WorkspaceEdit(crate::workspace::ProposalId),
    Files,
    Grep,
}
pub(in crate::app) struct PickerContent {
    pub surface: PickerSurfaceKind,
    pub origin: PickerOrigin,
    pub title: String,
    pub items: Vec<(PickerItem, PickerPayload)>,
    pub query: String,
    pub complete: bool,
}
pub(in crate::app) struct PickerInteraction {
    state: PickerState,
    payloads: HashMap<PickerItemId, PickerPayload>,
    surface: PickerSurfaceKind,
    /// The surface's own footer legend, describing what its keys do here rather than the
    /// widget's mode. `None` leaves the projected keymap hint in place.
    footer: Option<String>,
    /// Which comparison the change surface's diff pane is showing. Only meaningful for that
    /// surface; it is where `Tab` switches between the unstaged and staged layers.
    changes_other_layer: bool,
    /// Where the diff pane starts. `None` until the next layout places it on the first change.
    preview_scroll: Option<usize>,
    /// The diff pane's height in the last layout, which is what a page key moves by.
    preview_rows: usize,
    origin: PickerOrigin,
    source: (WorkspaceVersion, u64),
    epoch: PickerEpoch,
}
#[derive(Default)]
pub(in crate::app) struct Picker {
    interaction: Option<PickerInteraction>,
    stream_id: u64,
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
    replacement: replacement::SelectReplacement,
}
impl Picker {
    /// Move the current surface aside so help can take the picker. Only one surface is ever
    /// suspended: help is a page you read and leave, not a place you nest.
    pub(in crate::app) fn suspend_for_help(&mut self) {
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
    pub(in crate::app) fn help_is_open(&self) -> bool {
        self.help_open
    }
    /// Mark that the surface now on screen is a help page. Called after the page is installed,
    /// so the install itself cannot discard what was suspended for it.
    pub(in crate::app) fn mark_help_open(&mut self) {
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
    pub(in crate::app) fn resume_after_help(&mut self) -> bool {
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

/// More grep rows than any terminal is tall.
pub(in crate::app) const GREP_ROWS_ON_SCREEN: usize = 200;

#[derive(Clone, Debug)]
pub(in crate::app) struct MatcherSource {
    pub(in crate::app) path: PathBuf,
    pub(in crate::app) line: usize,
    pub(in crate::app) column: usize,
    pub(in crate::app) encoding: crate::editor::text::PositionEncoding,
}

/// A surface's own footer legend: what its keys do on that surface, rather than the widget's
/// mode. Only the surfaces that draw a footer have one.
pub(in crate::app) fn surface_footer(surface: PickerSurfaceKind) -> Option<String> {
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
/// A choice with an icon: the badge is the icon, coloured as the explorer colours the file
/// named by the row's source, such as `practice.rs`.
pub(in crate::app) fn choice_row_cells(
    source: &crate::workspace::PickerRowPresentation,
) -> Vec<PickerCell> {
    let icon = crate::files::file_icon(&source.source).map_or(Style::ExplorerIcon, |icon| {
        crate::ui::explorer_segment_style(crate::app::explorer::explorer_icon_role(icon.color))
    });
    let label = format!("{}  ", source.label);
    let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
    vec![
        PickerCell::new(format!("{} ", source.badge), icon, 2),
        PickerCell::new(label, Style::Default, width),
        PickerCell::flex(source.detail.clone(), Style::Muted),
    ]
}

/// Match-first grep columns: icon and highlighted code, followed by path, line and column.
/// The renderer fits the last three cells to the available location column.
pub(in crate::app) fn grep_row_cells(
    found: &crate::files::WorkspaceSearchMatch,
    row: &crate::workspace::PickerRowPresentation,
    file_icon: Option<&str>,
) -> Vec<PickerCell> {
    let path = found.relative_path.display().to_string();
    let line = found.line.saturating_add(1).to_string();
    let column = found.column.saturating_add(1).to_string();
    let mut cells = path_cells(&path, "", file_icon);
    cells.truncate(usize::from(file_icon.is_some()));
    let text = found.line_text.as_str();
    let start = text
        .get(..found.column)
        .filter(|prefix| prefix.trim_start().chars().count() > 24)
        .and_then(|prefix| prefix.char_indices().rev().nth(16))
        .map_or(0, |(offset, _)| offset);
    let text = text[start..].trim();
    if start > 0 {
        cells.push(cell("…", Style::Muted));
    }
    let spans = crate::language::syntax::language_for_path(&found.canonical_path)
        .map(|language| crate::language::syntax::highlight(language, text))
        .unwrap_or_default();
    let mut at = 0;
    for span in spans {
        let (Some(before), Some(styled)) =
            (text.get(at..span.start), text.get(span.start..span.end))
        else {
            continue;
        };
        cells.push(cell(before, Style::Default));
        cells.push(cell(styled, Style::Syntax(span.role)));
        at = span.end;
    }
    cells.push(cell(text.get(at..).unwrap_or_default(), Style::Default));
    cells.retain(|cell| !cell.text.is_empty());
    let path = if row.source.is_empty() {
        path
    } else {
        format!("{}/{path}", row.source)
    };
    cells.push(cell(&path, Style::Muted));
    cells.push(cell(&format!(":{line}"), Style::Accent));
    cells.push(cell(&format!(":{column}"), Style::Muted));
    cells
}

/// A path as LazyVim's pickers draw it: the file's icon, then the folder dimmed and the
/// name bright. With more than one root the root's name comes first, dimmed too.
pub(in crate::app) fn path_cells(
    path: &str,
    root: &str,
    file_icon: Option<&str>,
) -> Vec<PickerCell> {
    let split = path.rfind(std::path::is_separator).map_or(0, |at| at + 1);
    let (folder, name) = path.split_at(split);
    let mut cells = Vec::new();
    if let Some(fallback) = file_icon {
        let (glyph, style) =
            crate::files::file_icon(name).map_or((fallback, Style::ExplorerIcon), |icon| {
                let role = crate::app::explorer::explorer_icon_role(icon.color);
                (icon.glyph, crate::ui::explorer_segment_style(role))
            });
        cells.push(cell(&format!("{glyph} "), style));
    }
    if !root.is_empty() {
        cells.push(cell(&format!("{root}/"), Style::Muted));
    }
    if !folder.is_empty() {
        cells.push(cell(folder, Style::Muted));
    }
    cells.push(cell(name, Style::Default));
    cells
}

/// A cell exactly as wide as its text.
fn cell(text: &str, style: Style) -> PickerCell {
    let width = unicode_width::UnicodeWidthStr::width(text);
    PickerCell::new(text, style, u16::try_from(width).unwrap_or(u16::MAX))
}

pub(in crate::app) fn changes_row_cells(
    source: &crate::workspace::PickerRowPresentation,
) -> Vec<PickerCell> {
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

pub(in crate::app) mod palette;
pub(in crate::app) mod preview;

pub(in crate::app) mod commands;
pub(in crate::app) mod replacement;

impl Search {
    pub(in crate::app) fn plugin_browser_open(&self) -> bool {
        self.picker
            .interaction
            .as_ref()
            .is_some_and(|picker| matches!(picker.origin, PickerOrigin::Plugins))
    }

    pub(in crate::app) fn install_picker(
        &mut self,
        content: PickerContent,
        maximum: usize,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Result<PickerEpoch, &'static str> {
        let stream_id = self
            .picker
            .stream_id
            .checked_add(1)
            .ok_or("picker stream identity exhausted")?;
        let help_install = self.picker.take_help_install();
        let same_interaction = |picker: &PickerInteraction| {
            let same_origin = match (picker.origin, content.origin) {
                (PickerOrigin::Files, PickerOrigin::Files)
                | (PickerOrigin::Grep, PickerOrigin::Grep)
                | (PickerOrigin::Plugins, PickerOrigin::Plugins) => true,
                (PickerOrigin::PluginProvider(a), PickerOrigin::PluginProvider(b)) => a == b,
                (PickerOrigin::WorkspaceEdit(a), PickerOrigin::WorkspaceEdit(b)) => a == b,
                _ => false,
            };
            picker.source == (scope.clone(), configuration) && same_origin
        };
        let refresh_suspended = !help_install
            && self.picker.help_open
            && self.picker.suspended.as_ref().is_some_and(same_interaction);
        if !help_install && self.picker.help_open && !refresh_suspended {
            // A genuinely new surface supersedes both the help page and what it was showing, so
            // the stale snapshot is dropped rather than restored later.
            self.picker.help_open = false;
            self.picker.suspended = None;
        }
        // Stream refreshes replace rows, not the user's interaction. A new search
        // invocation closes the old picker before starting its producer.
        let slot = if refresh_suspended {
            &mut self.picker.suspended
        } else {
            &mut self.picker.interaction
        };
        let previous = if slot.as_ref().is_some_and(same_interaction) {
            slot.take()
        } else {
            None
        };
        let mut state = PickerState::new(stream_id, content.title, maximum);
        let mut payloads = HashMap::new();
        state.append(
            stream_id,
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
        if let Some(previous) = &previous {
            let previous_query = previous.state.query();
            epoch = previous.epoch.clone();
            if matches!(
                content.origin,
                PickerOrigin::Files
                    | PickerOrigin::PluginProvider(_)
                    | PickerOrigin::Plugins
                    | PickerOrigin::WorkspaceEdit(_)
            ) {
                query = previous_query.to_owned();
            }
            if query == previous_query {
                selected = previous.state.selected_item().map(|item| item.id.clone());
            }
        }
        if matches!(
            content.origin,
            PickerOrigin::Files | PickerOrigin::PluginProvider(_) | PickerOrigin::WorkspaceEdit(_)
        ) {
            state.set_query(query);
        } else {
            state.set_unfiltered_query(query);
        }
        if let Some(selected) = selected {
            state.select_id(&selected);
        }
        if !help_install && !matches!(content.origin, PickerOrigin::Files) {
            self.cancel_files();
        }
        if !help_install && !matches!(content.origin, PickerOrigin::Grep) && self.live {
            self.cancel_grep();
            self.live = false;
            self.query.clear();
        }
        let interaction = if let Some(mut previous) = previous {
            previous.state = state;
            previous.payloads = payloads;
            previous
        } else {
            PickerInteraction {
                state,
                payloads,
                surface: content.surface,
                footer: surface_footer(content.surface),
                changes_other_layer: false,
                preview_scroll: None,
                preview_rows: 0,
                origin: content.origin,
                source: (scope, configuration),
                epoch: epoch.clone(),
            }
        };
        if refresh_suspended {
            self.picker.suspended = Some(interaction);
        } else {
            self.picker.interaction = Some(interaction);
        }
        self.picker.stream_id = stream_id;
        Ok(epoch)
    }
    pub(in crate::app) fn theme_picker_key(&self) -> Option<&crate::app::configuration::ThemeKey> {
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
    /// What is typed in the worktree list when no row matches it: the name of a new worktree.
    pub(in crate::app) fn new_worktree_name(&self) -> Option<String> {
        let picker = self.picker.interaction.as_ref()?;
        let name = picker.state.query().trim();
        (!name.is_empty()
            && picker.state.selected_item().is_none()
            && picker
                .payloads
                .contains_key(&PickerItemId("git-worktree-new".into())))
        .then(|| name.to_owned())
    }
    pub(in crate::app) fn selected_picker_payload(&self) -> Option<&PickerPayload> {
        let picker = self.picker.interaction.as_ref()?;
        picker.payloads.get(&picker.state.selected_item()?.id)
    }
    pub(in crate::app) fn take_picker_payload(&mut self) -> Option<PickerPayload> {
        let picker = self.picker.interaction.as_mut()?;
        picker.payloads.remove(&picker.state.selected_item()?.id)
    }
    pub(in crate::app) fn picker_code_actions<'a>(
        &'a self,
        epoch: &'a PickerEpoch,
    ) -> impl Iterator<Item = &'a crate::app::language::edits::CodeActionKey> {
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
    pub(in crate::app) fn workspace_picker(&self) -> Option<crate::workspace::ProposalId> {
        match self.picker.interaction.as_ref()?.origin {
            PickerOrigin::WorkspaceEdit(id) => Some(id),
            _ => None,
        }
    }
    pub(in crate::app) fn select_picker_item(&mut self, id: &PickerItemId) {
        if let Some(picker) = self.picker.interaction.as_mut() {
            picker.state.select_id(id);
        }
    }
    pub(in crate::app) fn picker(&self) -> Option<&PickerState> {
        self.picker.interaction.as_ref().map(|picker| &picker.state)
    }
    pub(in crate::app) fn picker_epoch(&self) -> Option<&PickerEpoch> {
        self.picker.interaction.as_ref().map(|picker| &picker.epoch)
    }
    pub(in crate::app) fn owns_picker_epoch(&self, epoch: &PickerEpoch) -> bool {
        self.picker
            .interaction
            .as_ref()
            .is_some_and(|picker| Arc::ptr_eq(&picker.epoch.0, &epoch.0))
    }
    pub(in crate::app) fn picker_is_stale(
        &self,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        self.picker.interaction.as_ref().is_some_and(|picker| {
            picker.source != (scope, configuration)
                || match picker.origin {
                    PickerOrigin::General
                    | PickerOrigin::Plugins
                    | PickerOrigin::WorkspaceEdit(_)
                    | PickerOrigin::PluginProvider(_) => false,
                    PickerOrigin::Files => self.file_scope.is_none(),
                    PickerOrigin::Grep => {
                        self.scope.is_none() && !(self.live && self.query.is_empty())
                    }
                }
        })
    }
    pub(in crate::app) fn picker_surface(&self) -> PickerSurfaceKind {
        self.picker
            .interaction
            .as_ref()
            .map_or(PickerSurfaceKind::Generic, |picker| picker.surface)
    }
    pub(in crate::app) fn clear_picker(&mut self) {
        self.picker.interaction = None;
    }
    pub(in crate::app) fn picker_insert(&mut self, text: &str) -> Option<String> {
        let picker = &mut self.picker.interaction.as_mut()?.state;
        picker.insert_query(text);
        Some(picker.query().to_owned())
    }
    pub(in crate::app) fn picker_backspace(&mut self) -> Option<String> {
        let picker = &mut self.picker.interaction.as_mut()?.state;
        picker.backspace_query();
        Some(picker.query().to_owned())
    }

    /// Re-present the open picker on another surface, keeping its rows, query and selection.
    /// This is how a picker sends its results to the shared Results surface.
    pub(in crate::app) fn set_picker_surface(&mut self, surface: PickerSurfaceKind) -> bool {
        let Some(picker) = self.picker.interaction.as_mut() else {
            return false;
        };
        picker.surface = surface;
        picker.footer = surface_footer(surface);
        true
    }

    /// Replace the open surface's footer legend, for a legend that carries a live count.
    pub(in crate::app) fn set_picker_footer(&mut self, footer: String) {
        if let Some(picker) = self.picker.interaction.as_mut() {
            picker.footer = Some(footer);
        }
    }

    /// Which comparison the change surface's diff pane shows.
    pub(in crate::app) fn changes_layer(&self) -> crate::git::GitDiffLayer {
        // A row shows its own comparison, and Tab shows the other one.
        let staged = self.picker.interaction.as_ref().is_some_and(|picker| {
            let staged_row = picker
                .state
                .selected_item()
                .is_some_and(|item| item.id.0.starts_with("git:Staged:"));
            staged_row != picker.changes_other_layer
        });
        if staged {
            crate::git::GitDiffLayer::Staged
        } else {
            crate::git::GitDiffLayer::Unstaged
        }
    }

    /// Switch the change surface's diff pane to the other comparison and report which one it
    /// now shows, so the caller can refresh the pane.
    pub(in crate::app) fn switch_changes_layer(&mut self) -> Option<crate::git::GitDiffLayer> {
        let picker = self.picker.interaction.as_mut()?;
        if picker.surface != PickerSurfaceKind::Changes {
            return None;
        }
        picker.changes_other_layer = !picker.changes_other_layer;
        Some(self.changes_layer())
    }

    pub(in crate::app) fn move_diff_hunk(&mut self, direction: i8) {
        let Some(snapshot) = &self.diff_snapshot else {
            return;
        };
        let count = snapshot.hunks.len();
        if count == 0 {
            return;
        }
        self.diff_hunk = if direction < 0 {
            (self.diff_hunk + count - 1) % count
        } else {
            (self.diff_hunk + 1) % count
        };
        let Some(crate::ui::PickerPreviewBody::Diff(diff)) =
            self.preview.as_mut().map(|preview| &mut preview.body)
        else {
            return;
        };
        let layer = match snapshot.layer {
            crate::git::GitDiffLayer::Staged => "HEAD -> index",
            crate::git::GitDiffLayer::Unstaged => "index -> disk",
        };
        diff.provenance = format!("{layer} · Hunk {}/{count}", self.diff_hunk + 1);
        if let Some(picker) = &mut self.picker.interaction {
            picker.preview_scroll = Some(diff.hunk_scroll(self.diff_hunk));
        }
    }

    pub(in crate::app) fn picker_move(&mut self, direction: i8, wrap: bool) {
        if let Some(picker) = &mut self.picker.interaction {
            picker.state.move_selection(direction, wrap);
        }
    }
    pub(in crate::app) fn picker_scroll(&mut self, rows: usize) -> Option<usize> {
        let picker = &mut self.picker.interaction.as_mut()?.state;
        picker.ensure_selected_visible(rows);
        Some(picker.scroll())
    }
    /// Where the diff pane starts in a box of `rows` rows. It opens on the first change and
    /// keeps its place after that.
    pub(in crate::app) fn picker_preview_scroll(&mut self, rows: usize) -> Option<usize> {
        let picker = self.picker.interaction.as_mut()?;
        let crate::ui::PickerPreviewBody::Diff(diff) = &self.preview.as_ref()?.body else {
            return None;
        };
        let scroll = picker
            .preview_scroll
            .unwrap_or_else(|| diff.opening_scroll(rows))
            .min(diff.last_scroll(rows));
        picker.preview_scroll = Some(scroll);
        picker.preview_rows = rows;
        Some(scroll)
    }
    /// Move the diff pane by half its height. Nothing happens where no diff is showing.
    pub(in crate::app) fn picker_preview_page(&mut self, direction: i8) {
        let Some(picker) = self.picker.interaction.as_mut() else {
            return;
        };
        let Some(crate::ui::PickerPreviewBody::Diff(diff)) =
            self.preview.as_ref().map(|preview| &preview.body)
        else {
            return;
        };
        let step = (picker.preview_rows / 2).max(1);
        let current = picker.preview_scroll.unwrap_or(0);
        let scroll = if direction < 0 {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step)
        };
        picker.preview_scroll = Some(scroll.min(diff.last_scroll(picker.preview_rows)));
    }
    /// A different diff is about to show, so the next layout places it afresh.
    pub(in crate::app) fn picker_preview_replaced(&mut self) {
        if let Some(picker) = &mut self.picker.interaction {
            picker.preview_scroll = None;
        }
    }
    pub(in crate::app) fn picker_follows_tail(&self) -> bool {
        self.picker().is_none_or(PickerState::selected_is_last)
    }
    pub(in crate::app) fn picker_bookmark(&self) -> Option<(String, Option<PickerItemId>)> {
        self.picker().map(|picker| {
            (
                picker.query().into(),
                picker.selected_item().map(|item| item.id.clone()),
            )
        })
    }
    pub(in crate::app) fn restore_picker_view(
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
    pub(in crate::app) fn picker_projection(&self) -> Option<PickerUiState> {
        let picker = self.picker.interaction.as_ref()?;
        let presentation = picker.state.presentation();
        let preview_scroll = picker.preview_scroll.unwrap_or(0);
        let grep_matches = if picker.surface == PickerSurfaceKind::Grep {
            self.grep_matches_on_screen(&presentation)
        } else {
            HashMap::new()
        };
        Some(PickerUiState {
            input_hint: String::new(),
            surface: picker.surface,
            footer: picker.footer.clone(),
            // Both sides move together; nothing scrolls one side of a diff alone yet.
            preview_side_scroll: (preview_scroll, preview_scroll),
            title: presentation.title,
            query: presentation.query,
            total: presentation.total,
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
                        } else if picker.surface == PickerSurfaceKind::Choice
                            && !row.badge.is_empty()
                        {
                            choice_row_cells(&row)
                        } else if let Some(found) = grep_matches.get(row.id.0.as_str()) {
                            grep_row_cells(found, &row, self.file_icon.as_deref())
                        } else if picker.surface == PickerSurfaceKind::Files {
                            path_cells(&row.label, &row.source, self.file_icon.as_deref())
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
    /// The grep matches of the rows a screen can show, by id. Only these are drawn, so only
    /// these get columns and highlighting, however many thousand matches there are.
    fn grep_matches_on_screen(
        &self,
        presentation: &crate::workspace::PickerPresentation,
    ) -> HashMap<&str, &crate::files::WorkspaceSearchMatch> {
        let shown: std::collections::HashSet<&str> = presentation
            .rows
            .iter()
            .skip(presentation.scroll)
            .take(GREP_ROWS_ON_SCREEN)
            .map(|row| row.id.0.as_str())
            .collect();
        self.matches
            .iter()
            .filter(|found| shown.contains(found.id.as_str()))
            .map(|found| (found.id.as_str(), found))
            .collect()
    }
}

use crate::app::App;
use crate::app::PickerKind;
use crate::app::search::MAXIMUM_PICKER_ITEMS;
use crate::app::search::SEARCH_MAXIMUM_FILES;
use crate::editor::text::ByteOffset;
use crate::editor::text::PositionEncoding;
use crate::editor::text::TextBuffer;
use crate::language::lsp::LspFeature;
use crate::language::lsp::LspMarkupContent;
use crate::language::lsp::path_from_file_uri;
use crate::tools::RegistryRequest;
use crate::ui::DocumentationPopupKind;
use crate::ui::render::SCRATCH_LABEL;
use crate::ui::render::editor::STATUS_DIRTY_MARKER;
use crate::ui::render::picker::PICKER_MAXIMUM_PREVIEW_BYTES;
use crate::workspace::PickerAction;

use crate::app::documents::RECOVERY_PREVIEW_LINES;

impl App {
    pub(in crate::app) fn install_picker(
        &mut self,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
    ) {
        self.install_surface_picker(PickerSurfaceKind::Generic, title, items);
    }
}

impl App {
    pub(in crate::app) fn install_surface_picker(
        &mut self,
        surface: PickerSurfaceKind,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
    ) {
        self.install_surface_picker_state(
            surface,
            title,
            items,
            String::new(),
            true,
            PickerOrigin::General,
        );
    }
}

impl App {
    pub(in crate::app) fn install_surface_picker_state(
        &mut self,
        surface: PickerSurfaceKind,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
        query: String,
        complete: bool,
        origin: PickerOrigin,
    ) {
        if let PickerOrigin::PluginProvider(request) = origin
            && !self.extension.provider_presentable(request)
        {
            self.show_error("plugin picker source is stale");
            return;
        }
        let replaced_preview = self
            .search
            .workspace_picker()
            .filter(|id| !matches!(origin, PickerOrigin::WorkspaceEdit(current) if current == *id));
        let previous_epoch = self.search.picker_epoch().cloned();
        let epoch = match self.search.install_picker(
            PickerContent {
                surface,
                origin,
                title,
                items,
                query,
                complete,
            },
            if matches!(origin, PickerOrigin::Files) {
                SEARCH_MAXIMUM_FILES
            } else {
                MAXIMUM_PICKER_ITEMS
            },
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(epoch) => epoch,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        // A stream may have refreshed the picker underneath help, not the visible page.
        if self.search.help_is_open() {
            return;
        }
        if self.search.theme_picker_key().is_none() {
            self.configuration.cancel_theme_picker();
        }
        self.extension.picker_installed(match origin {
            PickerOrigin::PluginProvider(request) => Some(request),
            _ => None,
        });
        if !previous_epoch.is_some_and(|epoch| self.search.owns_picker_epoch(&epoch)) {
            self.input.begin_picker();
        }
        // A refused replacement must not destroy the still-valid old preview.
        if let Some(id) = replaced_preview {
            self.reject_workspace_picker(id);
        }
        self.language
            .retain_actions(self.search.picker_code_actions(&epoch));
        self.input.cancel_prompt_completions();
        self.clear_picker_preview();
        self.sync_picker_ui();
        self.request_select_replacement();
    }
}

impl App {
    pub(in crate::app) fn close_stale_picker(&mut self) {
        if self.search.picker_is_stale(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            self.close_picker();
        }
    }
}

impl App {
    pub(in crate::app) fn selected_picker_payload(&self) -> Option<&PickerPayload> {
        self.search.selected_picker_payload()
    }
}

impl App {
    pub(in crate::app) fn open_lsp_locations_picker(
        &mut self,
        locations: Vec<crate::language::lsp::LspLocation>,
        encoding: PositionEncoding,
    ) {
        let excerpts = self.reference_excerpts(&locations);
        let items = locations
            .into_iter()
            .enumerate()
            .map(|(index, location)| {
                let line = location.range.start.line;
                let path = path_from_file_uri(&location.uri);
                let code = path
                    .as_deref()
                    .and_then(|path| excerpts.get(&(path.to_owned(), line)).cloned())
                    .unwrap_or_default();
                let path = path.map_or_else(
                    || location.uri.clone(),
                    |path| self.workspace_relative_label(&path),
                );
                let id = PickerItemId(format!("location:{index}:{}", location.uri));
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: format!("{path}:{}", line.saturating_add(1)),
                        detail: code,
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspLocation(location, encoding),
                )
            })
            .collect();
        self.install_picker(self.message("picker_references_title", &[]), items);
    }
}

impl App {
    /// Read each reference file once and retain only the requested, bounded excerpts.
    pub(in crate::app) fn reference_excerpts(
        &self,
        locations: &[crate::language::lsp::LspLocation],
    ) -> std::collections::HashMap<(std::path::PathBuf, usize), String> {
        const MAXIMUM_FILE: u64 = 1024 * 1024;
        let mut remaining = 1024 * 1024usize;
        let mut requested = std::collections::BTreeMap::<_, std::collections::BTreeSet<_>>::new();
        for location in locations {
            if let Some(path) = path_from_file_uri(&location.uri) {
                requested
                    .entry(path)
                    .or_default()
                    .insert(location.range.start.line);
            }
        }
        let mut excerpts = std::collections::HashMap::new();
        for (path, lines) in requested {
            if remaining < 4096 {
                break;
            }
            let open = self
                .state
                .buffers
                .values()
                .find(|buffer| buffer.path.as_ref() == Some(&path));
            let disk = if open.is_none() {
                crate::files::read_file_bounded(&path, MAXIMUM_FILE)
                    .ok()
                    .and_then(|bytes| crate::editor::text::decode_utf8(&bytes).ok())
            } else {
                None
            };
            // Walk a closed file once, rather than rescanning it for each reference.
            let mut disk_lines = disk
                .as_ref()
                .map(|text| text.text.lines().enumerate().peekable());
            for line in lines {
                if remaining < 4096 {
                    break;
                }
                let excerpt = if let Some(buffer) = open {
                    buffer.text.line(line).ok()
                } else if let Some(source) = &mut disk_lines {
                    while source.peek().is_some_and(|(index, _)| *index < line) {
                        source.next();
                    }
                    source
                        .next()
                        .filter(|(index, _)| *index == line)
                        .map(|(_, text)| text.to_owned())
                } else {
                    None
                };
                if let Some(excerpt) = excerpt {
                    let excerpt: String = excerpt.trim().chars().take(1024).collect();
                    remaining -= excerpt.len();
                    excerpts.insert((path.clone(), line), excerpt);
                }
            }
        }
        excerpts
    }
}

impl App {
    pub(in crate::app) fn open_lsp_symbols_picker(
        &mut self,
        server: &str,
        symbols: Vec<crate::language::lsp::LspSymbol>,
    ) {
        let encoding = self.language.server_encoding(server);
        let server = self.lsp_server_label(server);
        let items = symbols
            .into_iter()
            .enumerate()
            .map(|(index, symbol)| {
                let id = PickerItemId(format!("symbol:{index}:{}", symbol.name));
                let payload = symbol.location.map_or_else(
                    || PickerPayload::Command(String::new()),
                    |location| PickerPayload::LspLocation(location, encoding),
                );
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: symbol.name,
                        detail: symbol.kind.map_or_else(String::new, |kind| {
                            self.message(
                                "picker_symbol_kind",
                                &[(
                                    "kind",
                                    crate::language::lsp::lsp_symbol_kind_name(kind).to_owned(),
                                )],
                            )
                        }),
                        source: server.clone(),
                        action: PickerAction::None,
                    },
                    payload,
                )
            })
            .collect();
        self.install_picker(self.message("picker_symbols_title", &[]), items);
    }
}

impl App {
    pub(in crate::app) fn open_lsp_code_actions_picker(&mut self, server: &str) {
        let choices = match self.language.install_offered_actions(MAXIMUM_PICKER_ITEMS) {
            Ok(choices) => choices,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if choices.is_empty() {
            self.show_lsp_no_result(server);
            return;
        }
        let items = choices
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("code-action:{index}:{}", choice.title)),
                        label: choice.title,
                        detail: choice.detail,
                        source: self.lsp_server_label(&choice.server),
                        action: PickerAction::None,
                    },
                    PickerPayload::CodeAction(choice.key),
                )
            })
            .collect();
        self.install_picker(self.message("picker_code_actions_title", &[]), items);
    }
}

impl App {
    pub(in crate::app) fn open_builtin_picker(&mut self, kind: PickerKind) {
        match kind {
            PickerKind::Diagnostics => {
                let mut items: Vec<_> = self
                    .diagnostics
                    .choices()
                    .enumerate()
                    .map(|(index, (key, problem, _))| {
                        let id = PickerItemId(format!("diagnostic:{index}:{}", problem.uri));
                        let path = path_from_file_uri(&problem.uri);
                        let line = path.as_ref().and_then(|path| {
                            let open = self
                                .state
                                .buffers
                                .iter()
                                .find(|(_, buffer)| buffer.path.as_ref() == Some(path));
                            open?.1.text.byte_to_line(problem.range.start).ok()
                        });
                        let place = path.map_or_else(
                            || problem.uri.clone(),
                            |path| self.workspace_relative_label(&path),
                        );
                        let place =
                            line.map_or(place.clone(), |line| format!("{place}:{}", line + 1));
                        // Where comes first: at any width the row says which file to look at.
                        let message = problem.message.lines().map(str::trim).collect::<Vec<_>>();
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id,
                                label: place,
                                detail: message.join(" "),
                                source: problem.source.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Diagnostic(key),
                        )
                    })
                    .collect();
                items.extend(self.tasks.failed_tests().into_iter().map(
                    |(path, line, summary, key)| {
                        let place = self.workspace_relative_label(&path);
                        let summary = summary.lines().map(str::trim).collect::<Vec<_>>();
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("test:{}:{}", key.name, key.index)),
                                label: format!("{place}:{}", line + 1),
                                detail: summary.join(" "),
                                source: key.name.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::TaskTest(key),
                        )
                    },
                ));
                self.install_surface_picker(
                    PickerSurfaceKind::Problems,
                    self.message("picker_diagnostics_title", &[]),
                    items,
                );
            }
            PickerKind::Buffers => {
                let items = self
                    .state
                    .buffers
                    .iter()
                    .enumerate()
                    .map(|(index, (buffer, state))| {
                        let id = PickerItemId(format!("buffer:{index}"));
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id,
                                label: state.path.as_ref().map_or_else(
                                    || SCRATCH_LABEL.to_owned(),
                                    |path| self.workspace_relative_label(path),
                                ),
                                detail: if state.dirty {
                                    STATUS_DIRTY_MARKER.to_owned()
                                } else {
                                    String::new()
                                },
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Buffer(buffer),
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_buffers_title", &[]), items);
            }
            PickerKind::Roots => {
                // Every workspace Dust has opened on this machine; choosing one moves the
                // editor there like a worktree switch.
                let current = self.state.workspace_root.clone();
                let mut projects = self
                    .workspace
                    .seen_roots()
                    .into_iter()
                    .chain(
                        self.workspace
                            .roots()
                            .iter()
                            .map(|root| root.identity.canonical_path.clone()),
                    )
                    .filter(|path| path.is_dir())
                    .collect::<Vec<_>>();
                projects.sort();
                projects.dedup();
                let open_roots = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect::<Vec<_>>();
                let home = std::env::var_os("HOME").map(PathBuf::from);
                let items = projects
                    .into_iter()
                    .map(|path| {
                        let name = path.file_name().map_or_else(
                            || path.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                        let shown = match home
                            .as_deref()
                            .and_then(|home| path.strip_prefix(home).ok())
                        {
                            Some(rest) => format!("~/{}", rest.display()),
                            None => path.display().to_string(),
                        };
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("project:{}", path.display())),
                                label: name,
                                detail: shown,
                                source: if path == current {
                                    "open here".into()
                                } else if open_roots.contains(&path) {
                                    "workspace root".into()
                                } else {
                                    String::new()
                                },
                                action: PickerAction::None,
                            },
                            PickerPayload::Project(path),
                        )
                    })
                    .collect();
                self.install_picker("Projects".to_owned(), items);
                self.search
                    .set_picker_footer("Enter Switch · Esc Esc Close".to_owned());
                self.sync_picker_ui();
            }
            PickerKind::Recents => {
                let items = self
                    .persistence
                    .recent_files()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, path)| {
                        let location = PersistedLocation {
                            path: path.clone(),
                            byte: 0,
                            anchor: 0,
                            scroll_line: 0,
                            scroll_column: 0,
                        };
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("recent:{index}")),
                                label: self.workspace_relative_label(&path),
                                detail: String::new(),
                                source: self.message("navigation_recent_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_recents_title", &[]), items);
            }
            PickerKind::Marks => {
                self.persistence.reconcile_marks(&self.state);
                let items = self
                    .persistence
                    .marks()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, mark)| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("mark:{index}:{}", mark.name)),
                                label: mark.name,
                                detail: self.persisted_location_label(&mark.location),
                                source: self.message("navigation_mark_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(mark.location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_marks_title", &[]), items);
            }
            PickerKind::Jumps | PickerKind::Changes => {
                let (source, title, rows) = if matches!(kind, PickerKind::Jumps) {
                    (
                        self.message("navigation_jump_source", &[]),
                        self.message("navigation_jumps_title", &[]),
                        self.persistence.jumps().to_vec(),
                    )
                } else {
                    (
                        self.message("navigation_change_source", &[]),
                        self.message("navigation_changes_title", &[]),
                        self.persistence.changes().to_vec(),
                    )
                };
                let items = rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, location)| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("navigation:{index}")),
                                label: self.persisted_location_label(&location),
                                detail: String::new(),
                                source: source.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(title, items);
            }
            PickerKind::Commands => self.open_command_palette(),
        }
    }
}

impl App {
    /// A saved location as `file:line`; its byte offset means nothing to a person. A file that
    /// isn't open is read only for a nearby offset, because this runs for every row.
    pub(in crate::app) fn persisted_location_label(&self, location: &PersistedLocation) -> String {
        const READ_LIMIT: usize = 1024 * 1024;
        let name = self.workspace_relative_label(&location.path);
        let open = self
            .state
            .buffers
            .values()
            .find(|buffer| buffer.path.as_ref() == Some(&location.path));
        let line = match open {
            Some(buffer) => buffer.text.byte_to_line(ByteOffset(location.byte)).ok(),
            None if location.byte <= READ_LIMIT => {
                use std::io::Read;
                crate::files::open_read_no_symlinks(&location.path)
                    .ok()
                    .and_then(|file| {
                        if !file.metadata().ok()?.is_file() {
                            return None;
                        }
                        let mut bytes = Vec::new();
                        // CRLF takes two disk bytes for one normalized byte; allow a BOM
                        // and a partial final UTF-8 character as well.
                        file.take((location.byte * 2 + 7) as u64)
                            .read_to_end(&mut bytes)
                            .ok()?;
                        if let Err(error) = std::str::from_utf8(&bytes) {
                            if error.error_len().is_some() {
                                return None;
                            }
                            bytes.truncate(error.valid_up_to());
                        }
                        TextBuffer::from_bytes(&bytes)
                            .ok()?
                            .byte_to_line(ByteOffset(location.byte))
                            .ok()
                    })
            }
            None => None,
        };
        match line {
            Some(line) => format!("{name}:{}", line + 1),
            None => name,
        }
    }
}

impl App {
    pub(in crate::app) fn sync_picker_ui(&mut self) {
        self.close_stale_picker();
        let explorer = &self.configuration.settings().ui.explorer;
        let icon = (explorer.icon_mode == crate::config::ExplorerIconMode::NerdFont)
            .then(|| explorer.active_glyphs().file.clone());
        self.search.set_file_icon(icon);
        self.ui.picker = if self.select_replacement_pending() {
            None
        } else {
            self.search.picker_projection()
        };
        let theme = match self.search.selected_picker_payload() {
            Some(PickerPayload::Theme(key)) => Some(key),
            _ => None,
        };
        self.configuration.preview_theme(theme);
        let context = self.active_input_context();
        let theme_picker = self.search.theme_picker_key().is_some();
        if let Some(picker) = &mut self.ui.picker {
            let (mode, commands): (&str, &[(&str, &str)]) =
                if self.search.workspace_picker().is_some() {
                    (
                        "Review",
                        &[
                            ("workspace.edit.toggle", "toggle"),
                            ("workspace.edit.accept", "apply"),
                            ("workspace.edit.reject", "reject"),
                        ],
                    )
                } else if theme_picker {
                    ("", &[("picker.cancel", "cancel")])
                } else if self.input.picker_normal() {
                    (
                        "Normal",
                        &[
                            ("picker.mode.insert", "edit"),
                            ("picker.accept", "accept"),
                            ("picker.cancel", "close"),
                        ],
                    )
                } else {
                    (
                        "Insert",
                        &[
                            ("picker.mode.normal", "normal"),
                            ("picker.accept", "accept"),
                            ("picker.cancel", "close"),
                        ],
                    )
                };
            let hints = commands
                .iter()
                .filter_map(|(command, action)| {
                    self.configuration
                        .keymaps()
                        .inspect_key(&context, "escape")
                        .filter(|binding| {
                            *command == "picker.cancel"
                                && binding.command.as_deref() == Some(*command)
                        })
                        .or_else(|| self.configuration.keymaps().inspect(&context, command))
                        .map(|binding| {
                            if theme_picker {
                                let key = if binding.keys == "escape" {
                                    "Escape"
                                } else {
                                    &binding.keys
                                };
                                format!("Press {key} to {action}")
                            } else {
                                format!("{} {action}", binding.keys)
                            }
                        })
                })
                .collect::<Vec<_>>()
                .join(" · ");
            picker.input_hint = if mode.is_empty() {
                hints
            } else {
                format!("{mode} · {hints}")
            };
        }
        if !self.search.help_is_open() {
            self.sync_picker_preview();
        }
        if let Some(picker) = &mut self.ui.picker {
            if self.search.help_is_open() {
                picker.preview = None;
            } else {
                picker.preview.clone_from(self.search.preview());
            }
        }
        self.refresh_window_content();
    }
}

impl App {
    pub(in crate::app) fn picker_insert(&mut self, text: &str) {
        self.close_stale_picker();
        let query = self.search.picker_insert(text);
        self.sync_picker_ui();
        if self.search.live()
            && let Some(query) = query
        {
            self.restart_live_workspace_search(query);
        }
    }
}

impl App {
    pub(in crate::app) fn picker_backspace(&mut self) {
        self.close_stale_picker();
        let query = self.search.picker_backspace();
        self.sync_picker_ui();
        if self.search.live()
            && let Some(query) = query
        {
            self.restart_live_workspace_search(query);
        }
    }
}

impl App {
    pub(in crate::app) fn picker_move(&mut self, direction: i8) {
        self.close_stale_picker();
        self.search.picker_move(direction, true);
        self.sync_picker_ui();
    }
}

impl App {
    /// Read the focused change's diff on the preview's own hunk worker.
    fn sync_changes_diff_preview(&mut self) {
        let Some(crate::app::picker::PickerPayload::MatcherSource(source)) =
            self.selected_picker_payload().cloned()
        else {
            self.clear_picker_preview();
            return;
        };
        let Some(status) = self.git.status().cloned() else {
            self.clear_picker_preview();
            return;
        };
        let layer = self.search.changes_layer();
        // A hunk policy takes a path RELATIVE to the scope root: it rejects an absolute one,
        // and the status row carries an absolute path.
        let Ok(relative) = source.path.strip_prefix(&status.scope_root) else {
            self.clear_picker_preview();
            return;
        };
        let policy = crate::app::git::Git::policy(&status, relative.to_owned(), layer);
        if let Some(id) = self.search.prepare_diff_preview(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
            relative.display().to_string(),
        ) {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    fn clear_picker_preview(&mut self) {
        if let Some(id) = self.search.clear_preview() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    /// The change surface's focused row as a path RELATIVE to the scope root, when that
    /// surface is open.
    ///
    /// `None` means the caller should act on the active buffer, which is what every other
    /// surface wants. The change surface labels `s` and `u` by focus, so it stages the row
    /// under the cursor rather than whatever buffer happens to be active.
    pub(in crate::app) fn changes_focused_relative(&self) -> Option<std::path::PathBuf> {
        if self.search.picker_surface() != PickerSurfaceKind::Changes {
            return None;
        }
        let crate::app::picker::PickerPayload::MatcherSource(source) =
            self.selected_picker_payload()?
        else {
            return None;
        };
        let status = self.git.status()?;
        source
            .path
            .strip_prefix(&status.scope_root)
            .ok()
            .map(std::path::Path::to_owned)
    }
}

impl App {
    fn sync_picker_preview(&mut self) {
        if self.search.picker_surface() == PickerSurfaceKind::Changes {
            self.sync_changes_diff_preview();
            return;
        }
        if let Some(PickerPayload::ProposalFile(index)) = self.selected_picker_payload() {
            let index = *index;
            let preview = self.workspace_edits.proposal().and_then(|proposal| {
                let file = proposal.files.get(index)?;
                Some(crate::ui::PickerPreview {
                    title: file
                        .canonical_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    body: crate::ui::PickerPreviewBody::Diff(crate::app::git::text_diff_preview(
                        &file.original_bytes,
                        &file.proposed_bytes,
                        "current -> proposed",
                    )),
                })
            });
            match preview {
                Some(preview) => {
                    if let Some(id) = self.search.set_preview(preview) {
                        self.documents.cancel_request(id);
                    }
                }
                None => self.clear_picker_preview(),
            }
            return;
        }
        let Some((label, payload)) = self
            .search
            .picker()
            .and_then(PickerState::selected_item)
            .and_then(|item| {
                self.selected_picker_payload()
                    .cloned()
                    .map(|payload| (item.label.clone(), payload))
            })
        else {
            self.clear_picker_preview();
            return;
        };
        let payload = if let PickerPayload::TaskTest(key) = payload {
            let Some(source) = self.tasks.resolve_test(&key) else {
                self.clear_picker_preview();
                return;
            };
            PickerPayload::MatcherSource(source)
        } else {
            payload
        };
        match payload {
            PickerPayload::Recovery(journal) => {
                let preview = self
                    .recovery
                    .records()
                    .iter()
                    .find(|(path, _)| *path == journal)
                    .map(|(_, record)| crate::ui::PickerPreview {
                        title: label,
                        body: crate::ui::PickerPreviewBody::Text(
                            crate::ui::PickerTextPreview::new(
                                record
                                    .content
                                    .lines()
                                    .take(RECOVERY_PREVIEW_LINES)
                                    .map(str::to_owned)
                                    .collect(),
                                0,
                                None,
                                record
                                    .path()
                                    .as_deref()
                                    .and_then(crate::language::syntax::language_for_path),
                            ),
                        ),
                    });
                match preview {
                    Some(preview) => {
                        if let Some(id) = self.search.set_preview(preview) {
                            self.documents.cancel_request(id);
                        }
                    }
                    None => self.clear_picker_preview(),
                }
            }
            PickerPayload::WorkspaceSearch(key) => {
                if let Some(id) = self
                    .search
                    .preview_match(&key, PICKER_MAXIMUM_PREVIEW_BYTES)
                {
                    self.documents.cancel_request(id);
                }
            }
            PickerPayload::MatcherSource(source) => {
                let (previous_request, ticket) = self.search.prepare_preview(
                    source.path.clone(),
                    Some((source.line, source.column)),
                    label,
                    "loading…".into(),
                    self.workspace.version(),
                    self.configuration.binding_revision(),
                );
                if let Some(id) = previous_request {
                    self.documents.cancel_request(id);
                }
                let Some(ticket) = ticket else {
                    return;
                };
                let roots = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect();
                let maximum = PICKER_MAXIMUM_PREVIEW_BYTES;
                match self
                    .documents
                    .read_scoped_bounded(source.path, roots, maximum as u64)
                {
                    Ok(id) => {
                        if !self.search.track_preview_read(ticket, id, maximum) {
                            self.documents.cancel_request(id);
                        }
                    }
                    Err(error) => self.search.preview_failed(ticket, error),
                }
            }
            _ => self.clear_picker_preview(),
        }
    }
}

impl App {
    pub(in crate::app) fn close_picker(&mut self) {
        self.finish_item_select(None);
        self.cancel_select_replacement();
        if self.search.help_is_open() {
            self.search.close_picker();
            self.project_input_prompt();
            if self.search.picker_projection().is_some() {
                self.sync_picker_ui();
                self.refresh_window_content();
                return;
            }
        }
        self.configuration.cancel_theme_picker();
        self.debugger_session.dismiss_choices();
        if self.ui.picker.is_some() {
            self.input.capture_changed();
        }
        if let Some(id) = self.search.workspace_picker() {
            self.reject_workspace_picker(id);
        }
        self.language.clear_actions();
        if let Some(id) = self.search.close_picker() {
            self.documents.cancel_request(id);
        }
        self.extension.picker_installed(None);
        self.tasks.select_output(None);
        self.ui.picker = None;
        self.refresh_window_content();
        // Restore graph focus before the accepted row captures a navigation target.
        self.reconcile_editor_windows();
    }
}

impl App {
    pub(in crate::app) fn accept_picker(&mut self) {
        // A help page documents; it does not act. Accepting inside help closes the page and
        // returns the surface it was opened over, exactly like Escape, instead of running the
        // row's command and resurrecting a surface underneath it.
        if self.search.help_is_open() {
            self.close_picker();
            return;
        }
        self.close_stale_picker();
        if let Some(name) = self.search.new_worktree_name() {
            self.close_picker();
            self.create_git_worktree(name, None);
            return;
        }
        if let Some(PickerPayload::Theme(key)) = self.search.selected_picker_payload() {
            match self.configuration.apply_theme(key) {
                Ok(name) => {
                    self.close_picker();
                    self.finish_theme_choice(&name);
                }
                Err(error) => self.show_error(error),
            }
            return;
        }
        let review_line = if self.search.picker_surface() == PickerSurfaceKind::Changes {
            self.git_review_line()
        } else {
            None
        };
        let mut payload = self.search.take_picker_payload();
        if let (Some(line), Some(PickerPayload::MatcherSource(source))) =
            (review_line, &mut payload)
        {
            source.line = line;
        }
        if let Some(PickerPayload::Selection(id)) = &payload {
            self.finish_item_select(Some(id.clone()));
        }
        if let Some(PickerPayload::ProposalFile(index)) = &payload {
            if let Err(error) = self.workspace_edits.toggle(*index) {
                self.show_error(error);
            }
            self.show_workspace_proposal();
            return;
        }
        // Navigation owns its snapshot independently of subsequent searches.
        let search_match = match &payload {
            Some(PickerPayload::WorkspaceSearch(id)) => self
                .search
                .matches()
                .iter()
                .find(|found| &found.id == id)
                .cloned(),
            _ => None,
        };
        let code_action = match &payload {
            Some(PickerPayload::CodeAction(key)) => {
                let documents = self
                    .state
                    .buffers
                    .iter()
                    .filter_map(|(id, buffer)| {
                        Some(crate::app::language::edits::EditDocument {
                            id,
                            path: buffer.path.as_deref()?,
                            text: &buffer.text,
                            dirty: buffer.dirty,
                        })
                    })
                    .collect::<Vec<_>>();
                Some(self.language.take_action(key, &documents))
            }
            _ => None,
        };
        let debug_selection = match &payload {
            Some(PickerPayload::Debug(key)) => Some(self.debugger_session.take_selection(key)),
            _ => None,
        };
        let provider_selection = match &payload {
            Some(PickerPayload::PluginProviderItem {
                plugin,
                load,
                request,
                item,
                action_command,
            }) => self.extension.provider_selection(
                plugin,
                *load,
                *request,
                item,
                action_command,
                &self.state,
            ),
            _ => None,
        };
        self.close_picker();
        match payload {
            Some(PickerPayload::Theme(_)) => {}
            Some(PickerPayload::Plugin(choice)) => self.accept_plugin_choice(choice),
            Some(PickerPayload::WorkspaceSearch(_)) => {
                if let Some(found) = search_match {
                    self.navigate_workspace_search_match(found);
                }
            }
            Some(PickerPayload::LspLocation(location, encoding)) => {
                self.navigate_lsp_location_encoded(&location, encoding)
            }
            Some(PickerPayload::Diagnostic(key)) => {
                if let Some(row) = self.diagnostics.resolve(&key, &self.state) {
                    let (uri, range) = (row.uri.clone(), row.range);
                    self.navigate_byte_location(&uri, range);
                } else {
                    self.show_error("diagnostic source is stale");
                }
            }
            Some(PickerPayload::CodeAction(_)) => {
                let offered = match code_action.expect("selected code action") {
                    Ok(offered) => offered,
                    Err(error) => {
                        self.show_error(error);
                        return;
                    }
                };
                let action = offered.action;
                if let Some(edit) = action.edit {
                    if self.workspace_edits.check_begin().is_err() {
                        self.show_error("another workspace edit is pending");
                        return;
                    }
                    self.begin_workspace_proposal_owned(
                        action.title,
                        edit,
                        None,
                        self.language.server_encoding(&offered.server),
                        offered.context,
                    );
                    if let Some(command) = action.command {
                        self.language.set_edit_command(offered.server, command);
                    }
                } else if let Some(command) = action.command {
                    self.select_lsp_session(&offered.server);
                    self.request_lsp(LspFeature::ExecuteCommand(command));
                }
            }
            Some(PickerPayload::LspCodeLens {
                server,
                context,
                lens,
            }) => {
                if !self.language.edit_context_current(&context) {
                    self.show_error("code-lens source is stale");
                    return;
                }
                if let Some(command) = lens.command {
                    self.execute_lsp_command(&server, command);
                } else if let Some(item) = lens.resolve_item {
                    self.refresh_lsp_projection_owner();
                    if let Some((_, document)) = self.active_lsp_document()
                        && let Err(error) = self.language.request_projected(
                            &server,
                            LspFeature::CodeLensResolve { item },
                            document,
                            ByteOffset(0),
                        )
                    {
                        self.show_error(error);
                    }
                }
            }
            Some(PickerPayload::Buffer(buffer)) => {
                if let Err(error) = self.state.show_buffer(buffer) {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::GitWorktree { path, .. } | PickerPayload::Project(path)) => {
                self.switch_workspace_root(path)
            }
            Some(PickerPayload::Agent(choice)) => self.accept_agent_choice(choice),
            Some(PickerPayload::PullRequest {
                root,
                request_id,
                number,
                local,
            }) => self.checkout_pull_request(root, request_id, number, local),
            Some(PickerPayload::Terminal(key)) => {
                if let Some(id) = self.terminal_session.resolve(&key) {
                    self.terminal_session.select(id);
                    self.reveal_terminal(id);
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            Some(PickerPayload::Window(window)) => {
                self.reconcile_editor_windows();
                let result = self
                    .window_session
                    .plan_focus(
                        window,
                        &self.state.window_version(),
                        self.extension.contributions().version(),
                    )
                    .and_then(|plan| self.apply_window_plan(plan));
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::LspSession(key)) => self.select_lsp_session(&key),
            Some(PickerPayload::DebugLaunch(launch)) => self.start_debug_preset(*launch),
            Some(PickerPayload::DebugConfiguration(name)) => self.start_dap_configuration(&name),
            Some(PickerPayload::Debug(_)) => {
                self.debugger_session.refresh(
                    &self.configuration.settings().debug,
                    self.workspace.version(),
                );
                let target = crate::app::debugger::SourceTarget {
                    line: 1,
                    column: 1,
                    navigation: self
                        .window_session
                        .navigation_stamp(self.state.window_version(), self.state.active_view()),
                    capture: self.input.capture_epoch(),
                };
                let debug_selection = debug_selection.expect("debug selection routed");
                // A value with nothing inside it opens to the whole of itself, unclipped.
                if let Some(value) = debug_selection.as_ref().ok().and_then(|s| s.value()) {
                    let value = value.to_owned();
                    self.debugger_session.record_inspected_value(value.clone());
                    self.notify_message(value, false);
                }
                match debug_selection
                    .and_then(|selection| self.debugger_session.activate(selection, target))
                {
                    Ok(Some(location)) => self.navigate_debug_location(location),
                    Ok(None) => {}
                    Err(error) => self.show_error(error),
                }
            }
            Some(PickerPayload::GitHunk {
                path,
                status_revision,
                repository,
                hunk,
                new_line,
            }) => {
                if self.git.select(
                    status_revision,
                    repository,
                    self.workspace.root_revision(),
                    &path,
                    hunk,
                ) {
                    self.navigate_matcher_source(MatcherSource {
                        path,
                        line: new_line,
                        column: 0,
                        encoding: PositionEncoding::Utf8,
                    })
                } else {
                    self.show_error("Git hunk is stale")
                }
            }
            Some(PickerPayload::TaskOutput(name)) => self.show_output_picker(&name),
            Some(PickerPayload::TaskTest(key)) => {
                if let Some(source) = self.tasks.resolve_test(&key) {
                    self.navigate_matcher_source(source);
                } else {
                    self.show_error("test result source is stale");
                }
            }
            Some(PickerPayload::MatcherSource(source)) => self.navigate_matcher_source(source),
            Some(PickerPayload::PersistedLocation(location)) => {
                self.navigate_persisted_location(location)
            }
            Some(PickerPayload::RegistrySummary(summary)) => self.request_registry(
                RegistryRequest::Show {
                    publisher: summary.publisher,
                    name: summary.name,
                    version: summary.version,
                },
                false,
            ),
            Some(PickerPayload::RegistryMetadata(metadata)) => {
                self.request_registry(RegistryRequest::Fetch { metadata }, true)
            }
            Some(PickerPayload::Command(command)) if !command.is_empty() => {
                self.run_palette_command(&command);
            }
            Some(PickerPayload::Recovery(journal)) => self.recover_journal(&journal),
            Some(PickerPayload::Tutor(start)) => self.start_tutor(start),
            Some(PickerPayload::Notice {
                title,
                text: message,
            }) => {
                self.present_documentation_popup(
                    DocumentationPopupKind::Notice,
                    title,
                    "application".into(),
                    None,
                    vec![LspMarkupContent {
                        kind: crate::language::lsp::LspMarkupKind::PlainText,
                        value: message,
                    }],
                );
            }
            Some(PickerPayload::PluginProviderItem { plugin, .. }) => {
                if let Some((command, arguments)) = provider_selection {
                    self.execute_plugin_command(plugin, command, arguments);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
