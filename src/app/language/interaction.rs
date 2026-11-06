use crate::ui::render::COMPLETION_MAX_ROWS;
use crate::app::AppEffect;
use crate::app::ByteOffset;
use crate::app::ByteRange;
use crate::app::DocumentationPopupKind;
use crate::app::EditorOperation;
use crate::app::EditorTransaction;
use crate::app::Instant;
use crate::app::App;
use crate::app::LspFeature;
use crate::app::LspRuntimeSource;
use crate::app::LspWorkspaceEdit;
use crate::app::MatcherSource;
use crate::app::Path;
use crate::app::PathBuf;
use crate::app::PersistedLocation;
use crate::app::PickerAction;
use crate::app::PickerItem;
use crate::app::PickerItemId;
use crate::app::PickerPayload;
use crate::app::PickerSurfaceKind;
use crate::app::PositionCodec;
use crate::app::PositionEncoding;
use crate::app::TextBuffer;
use crate::app::TextEdit;
use crate::app::UndoPolicy;
use crate::app::discover_executable;
use crate::app::path_from_file_uri;
use crate::app::Duration;
use crate::app::LspEvent;
use crate::app::LspFeatureResult;
use crate::app::LspLaunch;
use crate::app::LspProgressKind;
use crate::app::UiFold;
use crate::app::UiInlayHint;
use crate::app::UiSyntaxSpan;
use crate::app::command;
use crate::app::diagnostics::MAXIMUM_DIAGNOSTICS_PER_URI;
use crate::app::language::LSP_MAXIMUM_LOG_LINES;
use crate::app::language::ProjectionChange;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;



impl App {

    pub(in crate::app) fn show_lsp_logs(&mut self) {
        let previous = if self.search.picker_surface() == PickerSurfaceKind::Logs {
            self.search.picker_bookmark()
        } else {
            None
        };
        let normal = previous.is_some() && self.input.picker_normal();
        let follow_tail = previous.is_none() || self.search.picker_follows_tail();
        let items = self
            .language
            .logs()
            .iter()
            .enumerate()
            .map(|(index, line)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!(
                            "lsp-log:{}",
                            self.language.log_start().saturating_add(index as u64)
                        )),
                        label: line.clone(),
                        detail: String::new(),
                        source: "lsp".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )
            })
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Logs,
            self.message("lsp_logs", &[("logs", String::new())]),
            items,
        );
        self.search.restore_picker_view(previous, follow_tail);
        self.input.set_picker_normal(normal);
        self.sync_picker_ui();
    }
}

impl App {

    pub(in crate::app) fn lsp_completion_kind(item: &crate::language::lsp::LspCompletionItem) -> &'static str {
        match item.kind {
            Some(1) => "Text",
            Some(2) => "Method",
            Some(3) => "Function",
            Some(4) => "Constructor",
            Some(5) => "Field",
            Some(6) => "Variable",
            Some(7) => "Class",
            Some(8) => "Interface",
            Some(9) => "Module",
            Some(10) => "Property",
            Some(11) => "Unit",
            Some(12) => "Value",
            Some(13) => "Enum",
            Some(14) => "Keyword",
            Some(15) => "Snippet",
            Some(16) => "Color",
            Some(17) => "File",
            Some(18) => "Reference",
            Some(19) => "Folder",
            Some(20) => "EnumMember",
            Some(21) => "Constant",
            Some(22) => "Struct",
            Some(23) => "Event",
            Some(24) => "Operator",
            Some(25) => "TypeParameter",
            _ => "Item",
        }
    }
}

impl App {


    pub(in crate::app) fn refresh_lsp_completion_documentation(&mut self) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        if self
            .language
            .completion_items()
            .get(selected)
            .is_some_and(|item| item.documentation.is_some() || item.signature().is_some())
        {
            self.open_lsp_completion_documentation(false);
        } else if self
            .input
            .documentation()
            .is_some_and(|popup| popup.kind == DocumentationPopupKind::Completion)
        {
            self.close_documentation();
        }
    }
}

impl App {


    pub(in crate::app) fn move_lsp_completion(&mut self, direction: i8) {
        let rows = usize::from(COMPLETION_MAX_ROWS.max(1));
        if let Some((selected, scroll)) = self.language.move_completion(direction, rows)
            && let Some(completion) = self.ui.language_completion.as_mut()
        {
            completion.selected = selected;
            completion.scroll = scroll;
        }
        self.refresh_lsp_completion_documentation();
    }
}

impl App {


    pub(in crate::app) fn accept_lsp_completion(&mut self) {
        if !self.language.completion_current(&self.state) {
            self.language.clear_completion();
            self.ui.language_completion = None;
            return;
        }
        let Some(completion) = &self.ui.language_completion else {
            return;
        };
        let Some(item) = self
            .language
            .completion_items()
            .get(completion.selected)
            .cloned()
        else {
            return;
        };
        let Some((buffer_id, document)) = self.active_lsp_document() else {
            return;
        };
        // The item belongs to the server that offered it: resolve there and read its positions in that server's encoding.
        let server = self
            .language
            .completion_server(completion.selected)
            .map(str::to_owned)
            .or_else(|| self.language.active().cloned())
            .unwrap_or_default();
        if !item.resolved
            && self
                .language
                .sessions()
                .get(&server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.completion_resolve)
            && item.resolve_item.is_some()
        {
            let offset = self
                .state
                .active_view
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            if let Err(error) =
                self.language
                    .resolve_completion(completion.selected, document, offset)
            {
                self.show_error(error);
            }
            return;
        }
        let encoding = self.language.server_encoding(&server);
        let snippet = item.snippet;
        let offset = self
            .state
            .active_view
            .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
        let buffer = TextBuffer::from_text(&document.text);
        let codec = PositionCodec::new(&buffer);
        let (line, character) = codec.byte_to_lsp(offset, encoding).unwrap_or((0, 0));
        let here = crate::language::lsp::LspPosition { line, character };
        let mut edit = match item.text_edit {
            // The range was the server's at request time; what was typed since goes too.
            Some(mut edit) => {
                if (edit.range.start.line, edit.range.start.character)
                    > (edit.range.end.line, edit.range.end.character)
                {
                    self.show_error("completion edit range is reversed");
                    return;
                }
                if edit.range.end.line == line && edit.range.end.character < character {
                    edit.range.end = here;
                }
                edit
            }
            // No range from the server: replace the word typed so far.
            None => {
                let start = self
                    .language
                    .completion_start()
                    .filter(|start| {
                        *start <= offset
                            && buffer.byte_to_line(*start).is_ok_and(|start| start == line)
                    })
                    .and_then(|start| codec.byte_to_lsp(start, encoding).ok())
                    .map_or(here.clone(), |(line, character)| {
                        crate::language::lsp::LspPosition { line, character }
                    });
                crate::language::lsp::LspTextEdit {
                    range: crate::language::lsp::LspRange { start, end: here },
                    new_text: item.insert_text,
                }
            }
        };
        let mut tabstops = Vec::new();
        let mut transforms = Vec::new();
        if snippet {
            let path = &document.path;
            let current_line = buffer.line(line).unwrap_or_default();
            let line_start = buffer.line_to_byte(line).unwrap_or(ByteOffset(0)).get();
            let column = offset
                .get()
                .saturating_sub(line_start)
                .min(current_line.len());
            let word_start = current_line[..column]
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map_or(0, |(at, c)| at + c.len_utf8());
            let word_end = current_line[column..]
                .char_indices()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map_or(current_line.len(), |(at, _)| column + at);
            let selected = self
                .state
                .active_view
                .and_then(|view| self.state.views[view].selection)
                .and_then(|selection| {
                    let start = selection.anchor.min(selection.head);
                    let end = selection.anchor.max(selection.head);
                    buffer.slice(ByteRange::new(start, end).ok()?).ok()
                })
                .unwrap_or_default();
            let mut variables = std::collections::HashMap::from([
                (
                    "TM_FILENAME".into(),
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "TM_FILENAME_BASE".into(),
                    path.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "TM_DIRECTORY".into(),
                    path.parent().unwrap_or(path).to_string_lossy().into_owned(),
                ),
                ("TM_FILEPATH".into(), path.to_string_lossy().into_owned()),
                (
                    "RELATIVE_FILEPATH".into(),
                    path.strip_prefix(&self.state.workspace_root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "WORKSPACE_NAME".into(),
                    self.state
                        .workspace_root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "WORKSPACE_FOLDER".into(),
                    self.state.workspace_root.to_string_lossy().into_owned(),
                ),
                ("TM_LINE_INDEX".into(), line.to_string()),
                ("TM_LINE_NUMBER".into(), (line + 1).to_string()),
                (
                    "TM_CURRENT_WORD".into(),
                    current_line[word_start..word_end].into(),
                ),
                (
                    "TM_CURRENT_LINE".into(),
                    current_line.trim_end_matches('\n').into(),
                ),
                ("TM_SELECTED_TEXT".into(), selected),
            ]);
            if variables
                .get("TM_SELECTED_TEXT")
                .is_some_and(String::is_empty)
            {
                variables.remove("TM_SELECTED_TEXT");
            }
            match crate::language::lsp::parse_snippet_with_variables(
                &edit.new_text,
                1024 * 1024,
                &variables,
            ) {
                Ok(parsed) => {
                    edit.new_text = parsed.text;
                    tabstops = parsed.tabstops;
                    transforms = parsed.transforms;
                }
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        }
        let insertion_start = TextBuffer::from_text(&document.text);
        let mut insertion_start = PositionCodec::new(&insertion_start)
            .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
            .ok();
        if let Some(start) = &mut insertion_start {
            let original = start.get();
            let mut shift = 0isize;
            for additional in &item.additional_edits {
                if let (Ok(begin), Ok(end)) = (
                    codec.lsp_to_byte(
                        additional.range.start.line,
                        additional.range.start.character,
                        encoding,
                    ),
                    codec.lsp_to_byte(
                        additional.range.end.line,
                        additional.range.end.character,
                        encoding,
                    ),
                ) && let Ok(range) = ByteRange::new(begin, end)
                    && end.get() <= original
                {
                    let inserted = TextEdit::new(range, additional.new_text.clone())
                        .replacement()
                        .len();
                    shift += inserted as isize - range.len() as isize;
                }
            }
            *start = ByteOffset(original.saturating_add_signed(shift));
        }
        let mut edits = vec![edit];
        edits.extend(item.additional_edits);
        let workspace_edit = LspWorkspaceEdit {
            resources: Vec::new(),
            documents: vec![crate::language::lsp::LspDocumentEdit {
                uri: document.uri(),
                version: Some(document.version),
                edits,
            }],
        };
        let applied = self.apply_lsp_workspace_edit(workspace_edit, encoding);
        let succeeded = applied.is_ok();
        if let Err(error) = applied {
            self.show_error(error);
        } else {
            if let Some(start) = insertion_start
                && !tabstops.is_empty()
            {
                let source = self.state.window_version();
                self.input.install_snippet(
                    crate::app::input::SnippetDocument {
                        editor: &source,
                        buffer: buffer_id,
                        text: &self.state.buffers[buffer_id].text,
                        view: self.state.active_view,
                    },
                    start,
                    tabstops,
                );
                self.input.install_snippet_transforms(start, transforms);
                self.select_snippet_current();
            }
        }
        self.language.clear_completion();
        self.ui.language_completion = None;
        if succeeded && let Some(command) = item.command {
            self.execute_lsp_command(&server, command);
        }
    }
}

impl App {


    pub(in crate::app) fn execute_lsp_command(
        &mut self,
        server: &str,
        command: crate::language::lsp::LspCommand,
    ) {
        self.sync_lsp_document();
        self.refresh_lsp_projection_owner();
        if let Some((_, document)) = self.active_lsp_document()
            && let Err(error) = self.language.request_projected(
                server,
                LspFeature::ExecuteCommand(command),
                document,
                ByteOffset(0),
            )
        {
            self.show_error(error);
        }
    }
}

impl App {


    pub(in crate::app) fn select_snippet_current(&mut self) {
        let source = self.state.window_version();
        let document = self.input.snippet_buffer().and_then(|id| {
            self.state.buffers.get(id).map(|buffer| {
                crate::app::input::SnippetDocument {
                    editor: &source,
                    buffer: id,
                    text: &buffer.text,
                    view: self.state.active_view.filter(|view| {
                        self.state
                            .views
                            .get(*view)
                            .is_some_and(|view| view.buffer == id)
                    }),
                }
            })
        });
        let Some(operations) = self.input.snippet_selection(document) else {
            return;
        };
        let mut transaction =
            EditorTransaction::new(self.transaction_id()).with_undo_policy(UndoPolicy::Ignore);
        transaction.operations.extend(operations);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}

impl App {

    pub(in crate::app) fn navigate_snippet(&mut self, direction: i8) {
        let buffer = self.input.snippet_buffer();
        if self.input.navigate_snippet(direction)
            && let Some(view) = self.state.active_view
            && Some(self.state.views[view].buffer) == buffer
        {
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_undo_policy(UndoPolicy::Ignore)
                .with_operation(EditorOperation::SetSelection {
                    view,
                    selection: None,
                });
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            }
        }
        self.select_snippet_current();
    }
}

impl App {

    pub(in crate::app) fn lsp_position_encoding(&self) -> PositionEncoding {
        self.language
            .capabilities()
            .map_or(PositionEncoding::Utf16, |c| c.position_encoding)
    }
}



/// How often, and how far apart, a request answered "content modified" before its server
/// reported loading is sent again: rust-analyzer does this for a file opened a moment ago.
const LSP_CONTENT_MODIFIED_RETRIES: u8 = 10;

const LSP_CONTENT_MODIFIED_DELAY: Duration = Duration::from_millis(500);


pub(in crate::app) enum NavigationRead {
    Matcher(MatcherSource),
    Persisted(PersistedLocation),
    Lsp(
        crate::language::lsp::LspRange,
        PositionEncoding,
        crate::app::windows::NavigationStamp,
        crate::app::workspace::WorkspaceVersion,
    ),
    ByteRange(ByteRange, Option<std::sync::Arc<[u8]>>),
    /// A recovery journal's path and text, to put into the file once it is open.
    Recovered(std::path::PathBuf, String),
}
impl App {

    fn clear_lsp_ui_rows(&mut self) {
        if self.ui.language_completion.is_some() {
            self.ui.language_completion = None;
        }
        // A Git or Markdown preview doesn't come from a server, and this runs each frame in a file
        // without one.
        if self.input.documentation().is_some_and(|popup| {
            !matches!(
                popup.kind,
                DocumentationPopupKind::Git | DocumentationPopupKind::Notice
            )
        }) {
            self.close_documentation();
        }
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_semantic_spans);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_highlight_spans);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_inlay_hints);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_folds);
    }
}

impl App {

    pub(in crate::app) fn clear_lsp_ui_projection(&mut self) {
        self.language.clear_projection();
        self.clear_lsp_ui_rows();
    }
}

impl App {

    pub(in crate::app) fn refresh_lsp_projection_owner(&mut self) {
        let document = self.active_lsp_document_without_text();
        let change = self.language.refresh_projection(
            &self.state,
            document
                .as_ref()
                .map(|(buffer, document)| (*buffer, document)),
        );
        if change != ProjectionChange::Same {
            // Typing keeps the server's colours, moved along with the edit, until it sends new
            // ones; dropping them flashes the text back to tree-sitter's.
            let semantic = std::mem::take(&mut self.ui.lsp_semantic_spans);
            let mut hints = std::mem::take(&mut self.ui.lsp_inlay_hints);
            self.clear_lsp_ui_rows();
            if change == ProjectionChange::Typed {
                self.ui.lsp_semantic_spans = semantic;
            }
            // Each buffer keeps its hints, moved along with edits, as in Neovim: switching back
            // to a file or typing shows them at once while fresh ones load.
            hints.retain_mut(|hint| {
                let Some(buffer) = self.state.buffers.get(hint.buffer) else {
                    return false;
                };
                let Some(position) = buffer.text.map_offset_since(hint.revision, hint.position)
                else {
                    return false;
                };
                hint.position = position;
                hint.revision = buffer.text.revision();
                true
            });
            self.ui.lsp_inlay_hints = hints;
            if !self.language.completion_items().is_empty() {
                self.project_lsp_completion();
            }
            if self.language.projection().is_some() {
                self.schedule_lsp_inlay_refresh(true);
            }
        }
    }
}

impl App {

    pub(in crate::app) fn clear_lsp_session_state(&mut self) {
        if let Some(server) = self.language.active().cloned() {
            self.clear_lsp_server_ownership(&server);
        }
        self.language.clear_active();
        self.sync_ui_diagnostics();
        self.clear_lsp_ui_projection();
    }
}

impl App {

    pub(in crate::app) fn schedule_lsp_inlay_startup_refresh(&mut self) {
        self.language
            .startup_inlays(&self.configuration.settings().lsp, Instant::now());
    }
}

impl App {

    pub(in crate::app) fn schedule_lsp_inlay_warmup_retry(&mut self) -> bool {
        self.language
            .retry_inlays(&self.configuration.settings().lsp, Instant::now())
    }
}

impl App {

    pub(in crate::app) fn schedule_lsp_inlay_refresh(&mut self, immediate: bool) {
        self.language.schedule_inlays(
            &self.configuration.settings().lsp,
            immediate,
            Instant::now(),
        );
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
}

impl App {

    pub(in crate::app) fn process_lsp_inlay_refresh(&mut self) {
        if self
            .language
            .inlay_ready(&self.configuration.settings().lsp, Instant::now())
        {
            self.request_lsp_inlay_hints();
        }
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
}

impl App {

    fn reprobe_failed_path_runtime(&mut self, server: &str) {
        let Some(definition) = self.language.sessions().get(server).and_then(|state| {
            matches!(&state.launch, LspLaunch::Direct { managed: false, .. })
                .then(|| state.definition.clone())
        }) else {
            return;
        };
        let Some(executable) = discover_executable(&definition.executable) else {
            return;
        };
        let key = self.tool_version_key(&definition, LspRuntimeSource::Path, Some(&executable));
        self.tooling.forget_probe(&key);
        self.queue_tool_version_probe(&definition, LspRuntimeSource::Path, executable);
    }
}

impl App {


    pub(in crate::app) fn append_lsp_log(&mut self, line: String) {
        self.language.append_log(line, LSP_MAXIMUM_LOG_LINES);
        if self.search.picker_surface() == PickerSurfaceKind::Logs && self.ui.picker.is_some() {
            self.show_lsp_logs();
        }
    }
}

impl App {


    pub(in crate::app) fn lsp_server_label(&self, server_key: &str) -> String {
        self.language.server_label(server_key)
    }
}

impl App {


    pub(in crate::app) fn update_lsp_progress(
        &mut self,
        server: String,
        token: String,
        kind: LspProgressKind,
        title: String,
        message: String,
        percentage: Option<u8>,
    ) {
        let was_loading = self.language.server_loading(&server);
        self.language.update_progress(
            (server.clone(), token, kind, title, message, percentage),
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        if was_loading && !self.language.server_loading(&server) {
            self.schedule_lsp_inlay_refresh(true);
        }
        self.project_lsp_progress();
    }
}

impl App {

    pub(in crate::app) fn clear_lsp_progress(&mut self, server: &str) {
        self.language.clear_progress(server);
        self.project_lsp_progress();
    }
}

impl App {

    fn project_lsp_progress(&mut self) {
        let (rows, frame) = self.language.progress();
        if self.ui.lsp_progress != rows {
            self.ui.lsp_progress = rows.to_vec();
        }
        if self.lsp_waiting_server.as_ref().is_some_and(|server| {
            self.language
                .sessions()
                .get(server)
                .is_some_and(|session| session.capabilities.is_some())
                && !self.language.server_loading(server)
        }) {
            self.lsp_waiting_server = None;
            self.lsp_retry_at = None;
            if let Some(feature) = self.lsp_waiting.take() {
                self.request_lsp(feature);
            }
        }
        self.ui.set(frame, |ui| &mut ui.lsp_progress_frame);
    }
}

impl App {

    pub(in crate::app) fn update_lsp_progress_animation(&mut self, now: Instant) {
        self.language
            .animate_progress(&self.configuration.settings().ui.lsp_progress, now);
        self.project_lsp_progress();
    }
}

impl App {

    /// Whether the server is still working through its project.
    /// Sends the waiting request again once a "content modified" retry is due.
    pub(in crate::app) fn process_lsp_retry(&mut self) {
        if self.lsp_retry_at.is_none_or(|due| Instant::now() < due) {
            return;
        }
        self.lsp_retry_at = None;
        self.lsp_waiting_server = None;
        if let Some(feature) = self.lsp_waiting.take() {
            self.request_lsp(feature);
        }
    }
}

impl App {

    fn lsp_server_loading(&self, server: &str) -> bool {
        self.language.server_loading(server)
    }
}

impl App {

    fn show_lsp_loading(&mut self) {
        self.ui.lsp_loading = Some(self.message("lsp_loading", &[]));
    }
}

impl App {

    /// An empty answer from a server still loading its project says nothing about the code.
    pub(in crate::app) fn show_lsp_no_result(&mut self, server: &str) {
        if self.lsp_server_loading(server) {
            self.show_lsp_loading();
            self.lsp_waiting = self.lsp_asked.clone();
            self.lsp_waiting_server = Some(server.into());
        } else {
            self.show_error(self.message("lsp_no_result", &[]));
        }
    }
}

impl App {

    pub(in crate::app) fn handle_lsp_request_failure(
        &mut self,
        server: String,
        generation: u64,
        method: String,
        message: String,
    ) {
        // The status line's own request: an empty trail is the whole result.
        if self.language.is_breadcrumb_request(generation) {
            self.language.retry_breadcrumbs();
            return;
        }
        // A formatter that fails must not cost the save.
        if method == "textDocument/formatting"
            && let Some(buffer) = self.formatting_save_for_request(&server, generation)
        {
            self.append_lsp_log(format!("format on save failed: {message}"));
            self.finish_format_on_save(buffer);
            return;
        }
        if method == "textDocument/inlayHint" {
            if self.language.fail_inlay(&server, generation) {
                self.schedule_lsp_inlay_warmup_retry();
                let server = self.lsp_server_label(&server);
                self.append_lsp_log(format!("{server}: inlay hint refresh failed: {message}"));
            }
            // Inlay hints are background, revision-owned projections. A failure
            // for any superseded generation is stale by definition, while a
            // current failure is retried above. Neither is user-actionable.
            return;
        }
        let current_asked = self.lsp_asked_generation == Some(generation)
            && self
                .lsp_asked
                .as_ref()
                .is_some_and(|feature| feature.method() == method);
        if current_asked {
            self.ui.lsp_loading = None;
        }
        // Colours are background work, and a second server failing a completion
        // must not hide the items the others returned.
        let secondary = method == "textDocument/completion"
            && (self.language.completion_automatic()
                || self
                    .language
                    .active()
                    .is_some_and(|active| *active != server));
        // A server that fails code actions must not hide what the others offered.
        if method == "textDocument/codeAction" {
            self.language.forget_request(&server, generation);
            let main = self
                .language
                .active()
                .is_some_and(|active| *active == server);
            let label = self.lsp_server_label(&server);
            if main {
                self.show_error(self.message(
                    "lsp_request_failed",
                    &[
                        ("server", label),
                        ("method", method),
                        ("diagnostic", message),
                    ],
                ));
            } else {
                self.append_lsp_log(format!("{label}: {method} failed: {message}"));
            }
            if !self.language.request_pending(generation)
                && (!main || self.language.has_offered_actions())
            {
                self.open_lsp_code_actions_picker(&server);
            }
            return;
        }
        if method == "textDocument/documentColor" || secondary {
            self.language.forget_request(&server, generation);
            let server = self.lsp_server_label(&server);
            self.append_lsp_log(format!("{server}: {method} failed: {message}"));
            return;
        }
        // A server still indexing answers "content modified"; the protocol says to ask again,
        // so the request waits for the load to finish, as an empty answer does.
        // Right after opening a file the server can answer that way before it reports loading;
        // then the request is sent again shortly, a bounded number of times.
        if message.contains("-32801") && current_asked {
            let loading = self.lsp_server_loading(&server);
            if loading || self.lsp_retries < LSP_CONTENT_MODIFIED_RETRIES {
                if !loading {
                    self.lsp_retries += 1;
                    self.lsp_retry_at = Some(Instant::now() + LSP_CONTENT_MODIFIED_DELAY);
                }
                self.show_lsp_loading();
                self.lsp_waiting = self.lsp_asked.clone();
                self.lsp_waiting_server = loading.then_some(server);
                return;
            }
        }
        if !current_asked && message.contains("-32801") {
            self.language.forget_request(&server, generation);
            return;
        }
        let server = self.lsp_server_label(&server);
        self.show_error(self.message(
            "lsp_request_failed",
            &[
                ("server", server),
                ("method", method),
                ("diagnostic", message),
            ],
        ));
    }
}

impl App {


    pub(in crate::app) fn process_lsp_events(&mut self) {
        for error in self.language.retry_configurations() {
            self.show_error(error);
        }
        for error in self.language.flush_edit_replies() {
            self.show_error(error);
        }
        while let Some(owned) = self.language.poll_event() {
            let crate::language::lsp::OwnedLspEvent {
                event,
                context,
                reply,
            } = owned;
            if !self.language.owns_event_context(&context) {
                self.language
                    .reject_reply(reply, "requesting language-server session retired".into());
                continue;
            }
            match event {
                LspEvent::Starting { .. } => {}
                LspEvent::Running {
                    server,
                    capabilities,
                } => {
                    self.ui.lsp_loading = None;
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_startup_refresh();
                    }
                    if self
                        .lsp_waiting_server
                        .as_ref()
                        .is_none_or(|waiting| waiting == &server)
                        && !self.language.server_loading(&server)
                    {
                        self.lsp_waiting_server = None;
                        if let Some(feature) = self.lsp_waiting.take() {
                            self.request_lsp(feature);
                        }
                    }
                    self.request_lsp_colors();
                    // Successful lifecycle transitions are reflected by the
                    // status line and bounded progress surface. They must not
                    // leave persistent bottom-left notifications.
                }
                LspEvent::CapabilitiesChanged {
                    server,
                    capabilities,
                } => {
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                    self.request_lsp_colors();
                }

                LspEvent::InlayHintsRefresh { server } => {
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }
                LspEvent::Diagnostics {
                    server,
                    uri,
                    version,
                    diagnostics,
                } => {
                    let result = self
                        .language
                        .diagnostic_batch(
                            &context,
                            &self.state,
                            (&uri, version, diagnostics),
                            (
                                false,
                                MAXIMUM_DIAGNOSTICS_PER_URI,
                                &self.lsp_server_label(&server),
                            ),
                        )
                        .and_then(|batch| self.diagnostics.publish(batch, &self.state));
                    match result {
                        Ok(()) => self.sync_ui_diagnostics(),
                        Err(error) => self.append_lsp_log(error),
                    }
                }

                LspEvent::Result {
                    server,
                    generation,
                    method,
                    uri,
                    version,
                    result,
                } => {
                    if context
                        .document_source(&uri)
                        .is_some_and(|(buffer, source)| {
                            self.state
                                .buffers
                                .get(*buffer)
                                .is_none_or(|buffer| buffer.text.source_version() != *source)
                        })
                    {
                        continue;
                    }
                    let save_format = method == "textDocument/formatting"
                        && self
                            .formatting_save_for_request(&server, generation)
                            .is_some();
                    if save_format {
                        self.present_lsp_result_owned(
                            &server,
                            generation,
                            &method,
                            &uri,
                            result,
                            Some(context),
                        );
                        continue;
                    }
                    let current = self.active_lsp_document_without_text();
                    if !current.as_ref().is_some_and(|(_, document)| {
                        document.uri() == uri && document.version == version
                    }) {
                        continue;
                    }
                    if let LspFeatureResult::Colors(colors) = result {
                        self.install_lsp_colors(&server, &uri, colors, context.encoding());
                        continue;
                    }
                    if self.language.is_breadcrumb_request(generation) {
                        self.refresh_lsp_projection_owner();
                        if !self.language.accept_breadcrumbs(&server, &uri, version) {
                            continue;
                        }
                        if let LspFeatureResult::Symbols(symbols) = result {
                            self.install_lsp_breadcrumbs(symbols, context.encoding());
                        }
                        continue;
                    }
                    self.refresh_lsp_projection_owner();
                    if !self.language.accept_result(
                        &server,
                        generation,
                        &method,
                        &uri,
                        version,
                        self.state
                            .active_view
                            .and_then(|id| self.state.views.get(id))
                            .map_or(ByteOffset(0), |view| view.cursor),
                    ) {
                        continue;
                    }
                    self.present_lsp_result_owned(
                        &server,
                        generation,
                        &method,
                        &uri,
                        result,
                        Some(context),
                    );
                }
                LspEvent::ApplyEditRequest {
                    server: _,
                    request_id: _,
                    label,
                    edit,
                } => {
                    self.begin_workspace_proposal(
                        label.unwrap_or_else(|| self.message("workspace_edit_server_label", &[])),
                        edit,
                        reply,
                        context.encoding(),
                    );
                }
                LspEvent::Progress {
                    server,
                    token,
                    kind,
                    title,
                    message,
                    percentage,
                } => self.update_lsp_progress(server, token, kind, title, message, percentage),
                LspEvent::Log { server, message } => {
                    let server = self.lsp_server_label(&server);
                    self.append_lsp_log(format!("{server}: {message}"));
                }
                LspEvent::ServerStatus {
                    server,
                    healthy,
                    message,
                } => {
                    if self.language.status_changed(&server, healthy, &message) {
                        self.report_lsp_status(&server, &message);
                    }
                }
                LspEvent::Health { server, health } => {
                    let server = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message(
                            "lsp_health",
                            &[
                                ("server", server),
                                ("documents", health.attached_documents.to_string()),
                                ("pending", health.pending_requests.to_string()),
                                (
                                    "encoding",
                                    health.capabilities.position_encoding.lsp_name().into(),
                                ),
                            ],
                        ),
                        false,
                    );
                }
                LspEvent::Unsupported { server, method } => {
                    let server = self.lsp_server_label(&server);
                    self.show_error(
                        self.message("lsp_unsupported", &[("server", server), ("method", method)]),
                    );
                }
                LspEvent::RequestFailed {
                    server,
                    generation,
                    method,
                    message,
                } => self.handle_lsp_request_failure(server, generation, method, message),
                LspEvent::Stopped { server, success } => {
                    if context.diagnostic_ambiguous() {
                        self.language.request_start(&server, None);
                    }
                    if !success {
                        self.reprobe_failed_path_runtime(&server);
                    }
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    self.ui.lsp_colors.retain(|span| span.server != server);
                    let server_label = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message("lsp_stopped", &[("server", server_label)]),
                        false,
                    );
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
                LspEvent::Failed { server, message } => {
                    self.reprobe_failed_path_runtime(&server);
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    self.ui.lsp_colors.retain(|span| span.server != server);
                    let server_label = self.lsp_server_label(&server);
                    self.language.failed(server_label.clone(), message.clone());
                    self.show_error(self.message(
                        "lsp_failed",
                        &[("server", server_label), ("diagnostic", message)],
                    ));
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
            }
        }
    }
}

impl App {


    pub(in crate::app) fn schedule_lsp_restart(&mut self, key: String) {
        if let Some(server) = self.language.schedule_restart(&key, Instant::now()) {
            self.show_error(self.message("lsp_restart_disabled", &[("server", server)]));
        }
    }
}

impl App {


    pub(in crate::app) fn restart_lsp_if_due(&mut self) {
        if let Some(key) = self.language.restart_due(Instant::now()) {
            self.restart_lsp_session(key);
        }
    }
}

impl App {

    pub(in crate::app) fn request_lsp_inlay_hints(&mut self) {
        self.refresh_lsp_projection_owner();
        let Some(owner) = self.language.projection() else {
            return;
        };
        if self.ui.large_file_buffers.contains(&owner.buffer)
            || !self
                .language
                .sessions()
                .get(&owner.server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.inlay_hints)
        {
            self.ui
                .lsp_inlay_hints
                .retain(|hint| hint.buffer != owner.buffer);
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if buffer == owner.buffer {
            self.language.request_inlays(document);
        }
    }
}

impl App {


    /// Keeps the symbols of the document on screen fresh for the status line.
    pub(in crate::app) fn process_lsp_breadcrumbs(&mut self) {
        let settings = self.configuration.settings();
        let shown = settings.ui.status_line.enabled
            && settings
                .ui
                .status_line
                .components
                .iter()
                .any(|component| component == "breadcrumbs");
        let Some((buffer, document)) = self.active_lsp_document_without_text().filter(|_| shown)
        else {
            self.ui.set(None, |ui| &mut ui.breadcrumbs);
            return;
        };
        if self.ui.large_file_buffers.contains(&buffer) {
            return;
        }
        if let Some(server) = self
            .language
            .request_breadcrumbs(buffer, &document, Instant::now())
            && let Some((_, document)) = self.active_lsp_document()
        {
            self.language.send_breadcrumbs(&server, buffer, document);
        }
    }
}

impl App {

    /// The symbols a cursor can be inside, in the server's order: outer ones before inner ones.
    fn install_lsp_breadcrumbs(
        &mut self,
        symbols: Vec<crate::language::lsp::LspSymbol>,
        encoding: PositionEncoding,
    ) {
        // As LazyVim's status line: Variables and such aren't places in the file.
        const PLACES: [u8; 12] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 23];
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        let codec = PositionCodec::new(&self.state.buffers[buffer].text);
        let symbols = symbols
            .into_iter()
            .filter(|symbol| symbol.kind.is_some_and(|kind| PLACES.contains(&kind)))
            .filter_map(|symbol| {
                let extent = symbol.extent?;
                let start = codec.lsp_to_byte(extent.start.line, extent.start.character, encoding);
                let end = codec.lsp_to_byte(extent.end.line, extent.end.character, encoding);
                Some(crate::ui::UiBreadcrumb {
                    name: symbol.name.trim_start().to_owned(),
                    range: ByteRange::new(start.ok()?, end.ok()?).ok()?,
                })
            })
            .collect();
        self.ui
            .set(Some((buffer, symbols)), |ui| &mut ui.breadcrumbs);
    }
}

impl App {


    /// Asks every attached server with a colour provider for the active document's colours.
    pub(in crate::app) fn request_lsp_colors(&mut self) {
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if !self.ui.large_file_buffers.contains(&buffer) {
            self.language.request_colors(buffer, &document);
        }
    }
}

impl App {

    fn install_lsp_colors(
        &mut self,
        server: &str,
        uri: &str,
        colors: Vec<crate::language::lsp::LspColor>,
        encoding: PositionEncoding,
    ) {
        let Some(path) = path_from_file_uri(uri) else {
            return;
        };
        let Some((buffer_id, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            return;
        };
        let codec = PositionCodec::new(&buffer.text);
        let revision = buffer.text.revision();
        let spans = colors.into_iter().filter_map(|color| {
            let start = codec
                .lsp_to_byte(
                    color.range.start.line,
                    color.range.start.character,
                    encoding,
                )
                .ok()?;
            let end = codec
                .lsp_to_byte(color.range.end.line, color.range.end.character, encoding)
                .ok()?;
            Some(crate::ui::UiColorSpan {
                server: server.to_owned(),
                buffer: buffer_id,
                revision,
                range: ByteRange::new(start, end).ok()?,
                rgb: color.rgb,
            })
        });
        let mut next = self
            .ui
            .lsp_colors
            .iter()
            .filter(|span| {
                (span.server != server || span.buffer != buffer_id)
                    && self.state.buffers.get(span.buffer).is_some()
            })
            .cloned()
            .chain(spans)
            .collect::<Vec<_>>();
        next.sort_unstable_by_key(|span| (span.buffer, span.range.start, span.range.end));
        self.ui.set(next, |ui| &mut ui.lsp_colors);
    }
}

impl App {


    pub(in crate::app) fn request_lsp(&mut self, mut feature: LspFeature) {
        if matches!(feature, LspFeature::Formatting { .. })
            && let Some(tool) = self.active_formatter()
        {
            self.process_effect(AppEffect::FormatRun(tool));
            return;
        }
        if matches!(feature, LspFeature::InlayHints { .. }) {
            self.schedule_lsp_inlay_refresh(true);
            self.process_lsp_inlay_refresh();
            return;
        }
        if matches!(&feature, LspFeature::WorkspaceSymbols { query } if query.is_empty()) {
            self.open_workspace_symbols_prompt();
            return;
        }
        self.close_documentation();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let offset = view.cursor;
        let selection = view.selection;
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        let server = if matches!(feature, LspFeature::Formatting { .. }) {
            // Not every server on a file formats it: Tailwind sits beside the TypeScript one.
            self.language
                .formatting_server(buffer, &document)
                .or_else(|| self.language.supporting_server(buffer, &document, &feature))
        } else {
            self.language.supporting_server(buffer, &document, &feature)
        };
        // Someone is waiting on these, unlike a save's format or what the editor asks by itself.
        let asked = !self.format_on_save.contains_key(&buffer)
            && !matches!(
                feature,
                LspFeature::SemanticTokensFull | LspFeature::SemanticTokensDelta { .. }
            )
            && !(matches!(feature, LspFeature::Completion) && self.language.completion_automatic());
        let Some(server) = server else {
            self.start_lsp(None);
            if asked
                && self
                    .language
                    .sessions()
                    .values()
                    .any(|session| session.capabilities.is_none())
            {
                self.show_lsp_loading();
                self.lsp_waiting = Some(feature);
                self.lsp_waiting_server = None;
            }
            return;
        };
        // Switching the server or the buffer's owner clears completion state, including where
        // this one started.
        let completion = matches!(feature, LspFeature::Completion).then(|| {
            (
                self.language.completion_start(),
                self.language.completion_automatic(),
            )
        });
        // A formatter beside the file's main server, such as Ruff by basedpyright, doesn't replace it.
        let formats = matches!(
            feature,
            LspFeature::Formatting { .. } | LspFeature::RangeFormatting { .. }
        );
        if !formats && self.language.active() != Some(&server) {
            self.select_lsp_session(&server);
        }
        // The application owns a session record while its process is still
        // initializing. Do not expose that record as a runnable server or
        // send a request that can race the worker's startup/disconnect path.
        // `Running` publishes capabilities before this becomes requestable.
        if self
            .language
            .sessions()
            .get(&server)
            .is_none_or(|session| session.capabilities.is_none())
        {
            if asked {
                self.show_lsp_loading();
                self.lsp_waiting = Some(feature);
                self.lsp_waiting_server = Some(server.clone());
            }
            return;
        }
        if asked && (matches!(feature, LspFeature::Completion) || self.lsp_server_loading(&server))
        {
            self.show_lsp_loading();
        }
        if asked {
            self.lsp_asked = Some(feature.clone());
            self.lsp_asked_generation = None;
        }
        if matches!(feature, LspFeature::CodeAction { .. }) {
            self.language.clear_offered_actions();
            let encoding = self
                .language
                .capabilities()
                .map_or(PositionEncoding::Utf16, |capabilities| {
                    capabilities.position_encoding
                });
            let buffer = TextBuffer::from_text(&document.text);
            let codec = PositionCodec::new(&buffer);
            let (start, end) = selection.map_or((offset, offset), |selection| {
                if selection.anchor <= selection.head {
                    (selection.anchor, selection.head)
                } else {
                    (selection.head, selection.anchor)
                }
            });
            let Ok((start_line, start_character)) = codec.byte_to_lsp(start, encoding) else {
                return;
            };
            let Ok((end_line, end_character)) = codec.byte_to_lsp(end, encoding) else {
                return;
            };
            // As LazyVim asks: everything on the cursor's lines, so a fix is offered from anywhere
            // on the line that has the problem.
            let line_end = buffer
                .line_to_byte(end_line.saturating_add(1))
                .unwrap_or_else(|_| ByteOffset(buffer.len_bytes()));
            let line_start = buffer.line_to_byte(start_line).unwrap_or(start);
            let position = |offset| {
                codec
                    .byte_to_lsp(offset, encoding)
                    .ok()
                    .map(|(line, character)| crate::language::lsp::LspPosition { line, character })
            };
            let diagnostics = self
                .diagnostics
                .within(&document.uri(), line_start, line_end)
                .into_iter()
                .filter_map(|problem| {
                    Some(crate::language::lsp::LspDiagnostic {
                        range: crate::language::lsp::LspRange {
                            start: position(problem.range.start)?,
                            end: position(problem.range.end)?,
                        },
                        severity: Some(match problem.severity {
                            crate::workspace::DiagnosticSeverity::Error => 1,
                            crate::workspace::DiagnosticSeverity::Warning => 2,
                            crate::workspace::DiagnosticSeverity::Information => 3,
                            crate::workspace::DiagnosticSeverity::Hint => 4,
                        }),
                        code: problem.code.clone(),
                        related: Vec::new(),
                        source: Some(problem.source.clone()),
                        message: problem.message.clone(),
                        data: problem.data.clone(),
                    })
                })
                .collect();
            feature = LspFeature::CodeAction {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: start_line,
                        character: start_character,
                    },
                    end: crate::language::lsp::LspPosition {
                        line: end_line,
                        character: end_character,
                    },
                },
                diagnostics,
            };
        }
        match &mut feature {
            LspFeature::Formatting {
                tab_size,
                insert_spaces,
            }
            | LspFeature::RangeFormatting {
                tab_size,
                insert_spaces,
                ..
            } => {
                *tab_size = self.state.policy.tab_width;
                *insert_spaces = self.state.policy.expand_tabs;
            }
            _ => {}
        }
        self.refresh_lsp_projection_owner();
        if let Some((start, automatic)) = completion {
            self.language.begin_completion(start, automatic);
        }
        match self
            .language
            .request_projected(&server, feature, document, offset)
        {
            Ok(()) => {
                if asked {
                    self.lsp_asked_generation = Some(self.language.generation());
                }
                if formats
                    && let Some(pending) = self
                        .format_on_save
                        .get_mut(&buffer)
                        .filter(|pending| pending.request.is_none())
                {
                    pending.request = Some((server.clone(), self.language.generation()));
                }
            }
            // The worker has already reported (or will imminently report) its
            // real startup/exit failure. Avoid replacing that diagnostic with
            // a misleading raw transport-key "not running" error.
            Err(crate::language::lsp::LspError::NotRunning(_)) => {
                self.language.stopped(&server);
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    pub(in crate::app) fn open_lsp_completion_documentation(&mut self, asked: bool) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        let (server, label, documentation) = match self
            .language
            .completion_documentation(&self.state, selected)
        {
            Ok(value) => value,
            Err(error) => {
                let stale = error == "completion documentation is stale";
                if stale {
                    self.close_documentation();
                }
                // Docs that open by themselves simply lag the menu; that is nothing to report.
                if !stale || asked {
                    self.show_error(error);
                }
                return;
            }
        };
        let server_label = self.lsp_server_label(&server);
        self.present_documentation_popup(
            DocumentationPopupKind::Completion,
            self.message(
                "lsp_completion_documentation_title",
                &[("server", server_label), ("label", label)],
            ),
            server,
            None,
            vec![documentation],
        );
    }
}

impl App {


    pub(in crate::app) fn present_lsp_result(
        &mut self,
        server: &str,
        generation: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
    ) {
        self.present_lsp_result_owned(server, generation, method, uri, result, None);
    }
}

impl App {

    pub(in crate::app) fn present_lsp_result_owned(
        &mut self,
        server: &str,
        generation: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
        context: Option<crate::language::lsp::EditContext>,
    ) {
        self.ui.lsp_loading = None;
        let server_label = self.lsp_server_label(server);
        // A null result has the same meaning as an empty list for these features.
        let result = match (method, result) {
            ("textDocument/codeAction", LspFeatureResult::Empty) => {
                LspFeatureResult::CodeActions(Vec::new())
            }
            (
                "textDocument/documentHighlight"
                | "textDocument/foldingRange"
                | "textDocument/selectionRange",
                LspFeatureResult::Empty,
            ) => LspFeatureResult::Ranges(Vec::new()),
            (
                "textDocument/semanticTokens/full" | "textDocument/semanticTokens/full/delta",
                LspFeatureResult::Empty,
            ) => LspFeatureResult::SemanticTokens(crate::language::lsp::LspSemanticTokens {
                result_id: None,
                data: Vec::new(),
            }),
            ("textDocument/inlayHint", LspFeatureResult::Empty) => {
                LspFeatureResult::InlayHints(Vec::new())
            }
            (_, result) => result,
        };
        match result {
            // Another server on this file already answered.
            LspFeatureResult::Hover(_) if self.hover_is_showing() => {}
            LspFeatureResult::Hover(hover) => {
                let source_range = hover.range.and_then(|range| {
                    let buffer_id = self.state.active_buffer_id()?;
                    let buffer = self.state.buffers.get(buffer_id)?;
                    let codec = PositionCodec::new(&buffer.text);
                    let encoding = self.lsp_position_encoding();
                    let start = codec
                        .lsp_to_byte(range.start.line, range.start.character, encoding)
                        .ok()?;
                    let end = codec
                        .lsp_to_byte(range.end.line, range.end.character, encoding)
                        .ok()?;
                    ByteRange::new(start, end).ok()
                });
                self.present_documentation_popup(
                    DocumentationPopupKind::Hover,
                    self.message("lsp_hover_title", &[("server", server_label.clone())]),
                    server.into(),
                    source_range,
                    hover.sections,
                );
            }
            LspFeatureResult::Signature(content) => {
                self.present_documentation_popup(
                    DocumentationPopupKind::Signature,
                    self.message("lsp_signature_title", &[("server", server_label.clone())]),
                    server.into(),
                    None,
                    vec![content],
                );
            }
            LspFeatureResult::Completion { items, incomplete } => {
                self.language
                    .install_completions(server, items, incomplete, generation);
                self.narrow_lsp_completion();
                if self.ui.language_completion.is_none()
                    && !self.language.request_pending(generation)
                    && !self.language.completion_automatic()
                {
                    self.show_lsp_no_result(server);
                }
            }
            LspFeatureResult::CompletionResolved(item) => {
                if self
                    .language
                    .accept_resolved(generation, *item, &self.state)
                {
                    self.project_lsp_completion();
                    self.accept_lsp_completion();
                }
            }
            LspFeatureResult::Locations(locations) => {
                if locations.is_empty() {
                    self.show_lsp_no_result(server);
                } else if method == "textDocument/references" || locations.len() > 1 {
                    self.open_lsp_locations_picker(
                        locations,
                        self.language.server_encoding(server),
                    );
                } else if let Some(location) = locations.first() {
                    self.navigate_lsp_location_encoded(
                        location,
                        self.language.server_encoding(server),
                    );
                }
            }
            LspFeatureResult::Symbols(mut symbols) => {
                for location in symbols
                    .iter_mut()
                    .filter_map(|symbol| symbol.location.as_mut())
                {
                    if location.uri.is_empty() {
                        location.uri = uri.into();
                    }
                }
                self.open_lsp_symbols_picker(server, symbols);
            }
            LspFeatureResult::WorkspaceEdit(mut edit) => {
                for document in &mut edit.documents {
                    if document.uri.is_empty() {
                        document.uri = uri.into();
                    }
                }
                // Formatting was asked for and touches one file, so it applies without a review.
                if matches!(
                    method,
                    "textDocument/formatting" | "textDocument/rangeFormatting"
                ) {
                    // Typing while the server worked moved the text its edits point at.
                    let edit_is_current = context.as_ref().is_some_and(|context| {
                        self.language.edit_context_current(context)
                            && context
                                .document_source(uri)
                                .is_some_and(|(buffer, version)| {
                                    self.state.buffers.get(*buffer).is_some_and(|buffer| {
                                        buffer.text.source_version() == *version
                                    })
                                })
                    });
                    let has_edits = edit
                        .documents
                        .iter()
                        .any(|document| !document.edits.is_empty());
                    if edit_is_current && has_edits {
                        let encoding = self.language.server_encoding(server);
                        let replaced = edit
                            .documents
                            .iter()
                            .flat_map(|document| &document.edits)
                            .filter(|edit| edit.range.end.line > edit.range.start.line)
                            .map(|edit| edit.range.start.line..=edit.range.end.line)
                            .collect::<Vec<_>>();
                        let positions = self.view_positions(|line| {
                            replaced.iter().any(|lines| lines.contains(&line))
                        });
                        if let Err(error) = self.apply_lsp_workspace_edit(edit, encoding) {
                            self.show_error(error);
                        }
                        self.restore_view_positions(positions);
                    }
                    if let Some(buffer) = self.formatting_save_for_request(server, generation) {
                        self.finish_format_on_save(buffer);
                    }
                    return;
                }
                self.begin_workspace_proposal_owned(
                    self.message("workspace_edit_lsp_label", &[]),
                    edit,
                    None,
                    self.lsp_position_encoding(),
                    context,
                );
            }
            LspFeatureResult::CodeActions(actions) => {
                self.language.offer_actions(server, uri, actions, context);
                if !self.language.request_pending(generation) {
                    self.open_lsp_code_actions_picker(server);
                }
            }
            LspFeatureResult::PullDiagnostics(result) => {
                if let Some(id) = result.result_id.clone() {
                    self.language.record_pull(server, uri, id);
                }
                if result.unchanged {
                    return;
                }
                let normalized = context
                    .as_ref()
                    .ok_or_else(|| "pull diagnostics lack a requesting context".to_string())
                    .and_then(|context| {
                        self.language.diagnostic_batch(
                            context,
                            &self.state,
                            (uri, None, result.items),
                            (true, MAXIMUM_DIAGNOSTICS_PER_URI, &server_label),
                        )
                    });
                match normalized.and_then(|batch| self.diagnostics.publish(batch, &self.state)) {
                    Ok(()) => self.sync_ui_diagnostics(),
                    Err(error) => self.append_lsp_log(error),
                }
            }
            LspFeatureResult::SemanticTokens(result) => {
                if !self.language.record_semantic(server, uri, &result) {
                    return;
                }
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let legend = self
                        .language
                        .capabilities()
                        .map(|value| value.semantic_token_types.as_slice())
                        .unwrap_or(&[]);
                    // As in Neovim, a token colours only when the theme gives its
                    // `@lsp.type.*` group a colour; otherwise tree-sitter's stays.
                    let theme = self.configuration.theme();
                    let groups: Vec<_> = legend
                        .iter()
                        .map(|name| {
                            crate::config::Highlight::for_semantic_token(name)
                                .filter(|group| theme.highlight(*group).is_some())
                        })
                        .collect();
                    let mut line = 0usize;
                    let mut character = 0usize;
                    let mut spans = Vec::new();
                    for token in result.data.chunks_exact(5).take(100_000) {
                        line = line.saturating_add(token[0] as usize);
                        character = if token[0] == 0 {
                            character.saturating_add(token[1] as usize)
                        } else {
                            token[1] as usize
                        };
                        let Some(role) = groups.get(token[3] as usize).copied().flatten() else {
                            continue;
                        };
                        let Ok(start) = codec.lsp_to_byte(line, character, encoding) else {
                            continue;
                        };
                        let Ok(end) = codec.lsp_to_byte(
                            line,
                            character.saturating_add(token[2] as usize),
                            encoding,
                        ) else {
                            continue;
                        };
                        let Ok(range) = ByteRange::new(start, end) else {
                            continue;
                        };
                        spans.push(UiSyntaxSpan {
                            buffer: buffer_id,
                            revision: buffer.text.revision(),
                            range,
                            role,
                        });
                    }
                    self.ui.lsp_semantic_spans = spans;
                }
            }
            LspFeatureResult::SemanticTokenDelta(delta) => {
                if let Some(full) = self.language.semantic_delta(server, uri, delta) {
                    self.present_lsp_result(
                        server,
                        generation,
                        method,
                        uri,
                        LspFeatureResult::SemanticTokens(full),
                    );
                } else {
                    self.request_lsp(LspFeature::SemanticTokensFull);
                }
            }
            LspFeatureResult::InlayHints(items) => {
                let provisional = items
                    .iter()
                    .any(|item| item.label.to_ascii_lowercase().contains("{unknown}"));
                // A server still loading answers with nothing; keep the hints on screen
                // until the retry brings real ones.
                let retrying = if items.is_empty() || provisional {
                    self.schedule_lsp_inlay_warmup_retry()
                } else {
                    self.language.inlays_received();
                    false
                };
                let keep_shown = retrying
                    && self.state.active_buffer_id().is_some_and(|active| {
                        self.ui
                            .lsp_inlay_hints
                            .iter()
                            .any(|hint| hint.buffer == active)
                    });
                if let Some(path) = path_from_file_uri(uri).filter(|_| !keep_shown)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let hints = items
                        .into_iter()
                        .filter_map(|item| {
                            Some(UiInlayHint {
                                buffer: buffer_id,
                                revision: buffer.text.revision(),
                                position: codec
                                    .lsp_to_byte(
                                        item.position.line,
                                        item.position.character,
                                        encoding,
                                    )
                                    .ok()?,
                                label: item.label,
                            })
                        })
                        .collect::<Vec<_>>();
                    self.ui
                        .lsp_inlay_hints
                        .retain(|hint| hint.buffer != buffer_id);
                    self.ui.lsp_inlay_hints.extend(hints);
                }
            }
            LspFeatureResult::CodeLens(items) => {
                let Some(context) =
                    context.filter(|context| self.language.edit_context_current(context))
                else {
                    self.show_error("code-lens source retired");
                    return;
                };
                let rows = items
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        let label = if item.command.is_some() {
                            item.title.clone()
                        } else if item.resolve_item.is_some()
                            && self
                                .language
                                .sessions()
                                .get(server)
                                .and_then(|session| session.capabilities.as_ref())
                                .is_some_and(|caps| caps.code_lens_resolve)
                        {
                            format!("Line {}", item.range.start.line + 1)
                        } else {
                            return None;
                        };
                        Some((
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("code-lens:{index}")),
                                label,
                                detail: item
                                    .command
                                    .as_ref()
                                    .map(|command| command.command.clone())
                                    .unwrap_or_default(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::LspCodeLens {
                                server: server.into(),
                                context: Box::new(context.clone()),
                                lens: item,
                            },
                        ))
                    })
                    .collect::<Vec<_>>();
                if rows.is_empty() {
                    self.show_lsp_no_result(server);
                } else {
                    self.install_picker(self.message("picker_code_lens_title", &[]), rows);
                }
            }
            LspFeatureResult::Ranges(items) => {
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let ranges = items
                        .into_iter()
                        .filter_map(|item| {
                            ByteRange::new(
                                codec
                                    .lsp_to_byte(item.start.line, item.start.character, encoding)
                                    .ok()?,
                                codec
                                    .lsp_to_byte(item.end.line, item.end.character, encoding)
                                    .ok()?,
                            )
                            .ok()
                        })
                        .collect::<Vec<_>>();
                    match method {
                        "textDocument/documentHighlight" => {
                            self.ui.lsp_highlight_spans = ranges
                                .into_iter()
                                .map(|range| UiSyntaxSpan {
                                    buffer: buffer_id,
                                    revision: buffer.text.revision(),
                                    range,
                                    role: crate::config::Highlight::VARIABLE,
                                })
                                .collect()
                        }
                        "textDocument/foldingRange" => {
                            if let Some(view) = self.state.active_view {
                                self.ui.lsp_folds = ranges
                                    .into_iter()
                                    .filter_map(|range| {
                                        Some(UiFold {
                                            view,
                                            start_line: buffer
                                                .text
                                                .byte_to_line(range.start)
                                                .ok()?,
                                            end_line: buffer.text.byte_to_line(range.end).ok()?,
                                            collapsed: false,
                                        })
                                    })
                                    .collect()
                            }
                        }
                        "textDocument/selectionRange" => {
                            let expand =
                                self.language.complete_selection_ranges(generation, ranges);
                            if let Some(cursor) = expand
                                && self
                                    .state
                                    .active_view
                                    .is_some_and(|view| self.state.views[view].cursor == cursor)
                            {
                                self.process_effect(AppEffect::LspSelectionExpand);
                            }
                        }
                        _ => {}
                    }
                }
            }
            LspFeatureResult::Colors(_) => {}
            // Nothing to change is a normal answer from a formatter.
            LspFeatureResult::Empty
                if matches!(
                    method,
                    "textDocument/formatting" | "textDocument/rangeFormatting"
                ) =>
            {
                if let Some(buffer) = self.formatting_save_for_request(server, generation) {
                    self.finish_format_on_save(buffer);
                }
            }
            // Only the last of several servers can say that nobody knows.
            LspFeatureResult::Empty
                if method == "textDocument/hover"
                    && (self.language.request_pending(generation) || self.hover_is_showing()) => {}
            // Completion asks by itself as you type, and nothing to offer is no news.
            LspFeatureResult::Empty
                if method == "textDocument/completion"
                    && (self.language.request_pending(generation)
                        || self.language.completion_automatic()
                        || self.ui.language_completion.is_some()) => {}
            LspFeatureResult::Empty if method == "workspace/executeCommand" => {}
            LspFeatureResult::Empty => self.show_lsp_no_result(server),
        }
    }
}
