//! Request/projection state and acceptance. UI receives values, never mutation authority.
use crate::app::language::LspProjectionOwner;
use crate::app::language::LspSession;
use crate::config::InlayHintLevel;
use crate::config::LspConfig;
use crate::config::LspProgressConfig;
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::text::BufferRevision;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::PositionCodec;
use crate::language::lsp::LspCompletionItem;
use crate::language::lsp::LspDocument;
use crate::language::lsp::LspError;
use crate::language::lsp::LspFeature;
use crate::language::lsp::LspMarkupContent;
use crate::language::lsp::LspMarkupKind;
use crate::language::lsp::LspProgressKind;
use crate::language::lsp::LspSemanticTokenDelta;
use crate::language::lsp::LspSemanticTokens;
use crate::ui::UiLspProgress;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

const INLAY_DEBOUNCE: Duration = Duration::from_millis(120);
const INLAY_EMPTY_RETRY: Duration = Duration::from_millis(500);
const INLAY_RETRY_LIMIT: u8 = 5;
const INLAY_STARTUP: Duration = Duration::from_secs(8);
/// Colour requests sit between projection request IDs (below half) and inlay ones (above it).
pub(in crate::app) const COLOR_REQUEST_ID: u64 = u64::MAX / 2;
/// Answers for the status line's breadcrumbs, which never open the symbol picker.
pub(in crate::app) const BREADCRUMB_REQUEST_ID: u64 = COLOR_REQUEST_ID + 1;
#[derive(Default)]
pub(in crate::app) struct Projection {
    request_id: u64,
    inlay_request_id: u64,
    owner: Option<LspProjectionOwner>,
    request: Option<(u64, LspProjectionOwner)>,
    /// Servers that have not answered the current request; completion asks every one.
    request_servers: Vec<String>,
    requests: HashMap<u64, (LspProjectionOwner, Vec<String>, String)>,
    request_cursors: HashMap<u64, ByteOffset>,
    breadcrumb_retries: u8,
    breadcrumbs_owner: Option<(String, LspProjectionOwner)>,
    breadcrumbs_asked: Option<(BufferId, u64)>,
    breadcrumbs_seen: Option<((BufferId, u64), Instant)>,
    inlay_request: Option<(u64, LspProjectionOwner)>,
    inlay_due: Option<Instant>,
    inlay_retries: u8,
    completion_owner: Option<(u64, LspProjectionOwner)>,
    completion_sources: HashMap<String, CompletionSource>,
    completion_items: Vec<LspCompletionItem>,
    completion_servers: Vec<String>,
    /// Everything the servers offered; `completion_items` is the part matching what was typed.
    completion_all: Vec<(String, LspCompletionItem)>,
    completion_incomplete: bool,
    /// Where the word being completed starts.
    completion_start: Option<ByteOffset>,
    /// Opened by typing, so an empty answer or a failure stays quiet.
    completion_automatic: bool,
    completion_narrowed: Option<(ByteOffset, crate::editor::text::BufferRevision)>,
    typed: Option<char>,
    completion_selected: usize,
    completion_scroll: usize,
    resolve: Option<(u64, LspProjectionOwner, usize, ByteOffset)>,
    resolve_accept: bool,
    selection_ranges: Vec<ByteRange>,
    selection_cursor: Option<ByteOffset>,
    selection_request: Option<(u64, ByteOffset, bool)>,
    semantic: HashMap<(String, String), (Option<String>, Vec<u32>)>,
    pull_ids: HashMap<(String, String), String>,
    logs: VecDeque<String>,
    log_start: u64,
    loading: HashMap<String, std::collections::HashSet<String>>,
    progress: Vec<UiLspProgress>,
    progress_epoch: Option<Instant>,
    progress_frame: usize,
}
/// Sources share matching, selection and acceptance; inline is another view of the same item.
struct CompletionSource {
    encoding: crate::editor::text::PositionEncoding,
    inline: bool,
}

/// How the document the server's results belong to changed since the last frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum ProjectionChange {
    Same,
    /// The same document in the same window, edited.
    Typed,
    /// Another document, window or server.
    Moved,
}

impl LspSession {
    pub(in crate::app) fn request_id(&self) -> u64 {
        self.projection.request_id
    }
    pub(in crate::app) fn projection(&self) -> Option<&LspProjectionOwner> {
        self.projection.owner.as_ref()
    }
    pub(in crate::app) fn projection_source(
        &self,
        editor: &EditorState,
        buffer: BufferId,
        document: &LspDocument,
    ) -> Option<LspProjectionOwner> {
        let server = self.active.as_ref()?;
        let view = editor.active_view()?;
        let session = self.sessions.get(server)?;
        if !session.definition.supports_language(&document.language_id)
            || !session.documents.contains_key(&buffer)
            || !document.path.starts_with(&session.root_path)
        {
            return None;
        }
        Some(LspProjectionOwner {
            server: server.clone(),
            buffer,
            view,
            revision: BufferRevision(document.version),
            text: editor.buffers.get(buffer)?.text.source_version(),
            uri: document.uri(),
            language: document.language_id.clone(),
        })
    }
    pub(in crate::app) fn refresh_projection(
        &mut self,
        editor: &EditorState,
        document: Option<(BufferId, &LspDocument)>,
    ) -> ProjectionChange {
        let current =
            document.and_then(|(buffer, doc)| self.projection_source(editor, buffer, doc));
        if self.projection.owner == current {
            return ProjectionChange::Same;
        }
        // Typing moves the revision but keeps the menu: what was typed narrows it.
        let typing = match (&self.projection.owner, &current) {
            (Some(owner), Some(current)) => {
                owner.text.same_instance(&current.text)
                    && owner.server == current.server
                    && owner.buffer == current.buffer
                    && owner.view == current.view
                    && owner.uri == current.uri
                    && owner.text.same_instance(&current.text)
            }
            _ => false,
        };
        let retain_completion = typing
            && !self.projection.completion_all.iter().any(|(_, item)| {
                item.text_edit.is_some()
                    || !item.additional_edits.is_empty()
                    || item.resolve_item.is_some()
            });
        if retain_completion {
            if let (Some((_, owner)), Some(current)) =
                (&mut self.projection.completion_owner, &current)
            {
                *owner = current.clone();
            }
        } else if !self.local_completion_current(editor) {
            let start = self.projection.completion_start;
            let automatic = self.projection.completion_automatic;
            self.clear_completion();
            if typing {
                self.projection.completion_start = start;
                self.projection.completion_automatic = automatic;
            }
            if !typing {
                self.projection.breadcrumbs_asked = None;
                self.projection.breadcrumbs_seen = None;
                self.projection.breadcrumbs_owner = None;
                self.projection.breadcrumb_retries = 0;
            }
        }
        self.clear_projection_rows();
        if typing && !retain_completion {
            self.projection.completion_incomplete = true;
        }
        self.projection.owner = current;
        if typing {
            ProjectionChange::Typed
        } else {
            ProjectionChange::Moved
        }
    }
    pub(in crate::app) fn clear_projection(&mut self, editor: &EditorState) {
        if !self.local_completion_current(editor) {
            self.clear_completion();
        }
        self.clear_projection_rows();
    }
    fn local_completion_current(&self, editor: &EditorState) -> bool {
        self.completion_current(editor)
            && self
                .projection
                .completion_owner
                .as_ref()
                .is_some_and(|(_, owner)| owner.server.is_empty())
    }
    fn clear_projection_rows(&mut self) {
        self.projection.selection_ranges.clear();
        self.projection.selection_cursor = None;
        self.projection.selection_request = None;
        self.projection.owner = None;
        self.projection.request = None;
        self.projection.request_servers.clear();
        self.projection.requests.clear();
        self.projection.request_cursors.clear();
        self.projection.inlay_request = None;
        self.projection.inlay_retries = 0;
    }
    pub(in crate::app) fn next_request_id(&mut self) -> Option<u64> {
        let next = self
            .projection
            .request_id
            .checked_add(1)
            .filter(|next| *next < u64::MAX / 2)?;
        self.projection.request_id = next;
        Some(next)
    }
    /// The server for `feature` on this document: the active one when it can answer, else one
    /// that can, full language servers before linters such as ESLint.
    pub(in crate::app) fn supporting_server(
        &self,
        buffer: BufferId,
        document: &LspDocument,
        feature: &LspFeature,
    ) -> Option<String> {
        let supports = |state: &crate::app::language::LspRuntimeState| {
            state.definition.supports_language(&document.language_id)
                && state.documents.contains_key(&buffer)
                && document.path.starts_with(&state.root_path)
        };
        let offers = |state: &crate::app::language::LspRuntimeState| {
            state
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| feature.capability_for_document(capabilities, document))
        };
        let primary = |state: &crate::app::language::LspRuntimeState| {
            state
                .capabilities
                .as_ref()
                .is_none_or(|capabilities| !capabilities.definition)
        };
        let active = self
            .active
            .as_ref()
            .filter(|key| self.sessions.get(*key).is_some_and(&supports));
        active
            .filter(|key| self.sessions.get(*key).is_some_and(&offers))
            .or_else(|| {
                self.sessions
                    .iter()
                    .filter(|(_, state)| supports(state) && offers(state))
                    .min_by_key(|(key, state)| (primary(state), *key))
                    .map(|(key, _)| key)
            })
            .or(active)
            .or_else(|| {
                self.sessions
                    .iter()
                    .filter(|(_, state)| supports(state))
                    .map(|(key, _)| key)
                    .min()
            })
            .cloned()
    }
    pub(in crate::app) fn formatting_server(
        &self,
        buffer: BufferId,
        document: &LspDocument,
    ) -> Option<String> {
        self.sessions
            .iter()
            .filter(|(_, state)| {
                state.definition.supports_language(&document.language_id)
                    && state.documents.contains_key(&buffer)
                    && document.path.starts_with(&state.root_path)
                    && state.capabilities.as_ref().is_some_and(|c| {
                        c.formatting && c.supports_document("textDocument/formatting", document)
                    })
            })
            .map(|(key, _)| key.clone())
            .min()
    }
    /// Formatting belongs to the requested buffer even if focus moved during a chain.
    pub(in crate::app) fn request_buffer_format(
        &mut self,
        editor: &EditorState,
        buffer: BufferId,
        server: &str,
        feature: LspFeature,
        document: LspDocument,
    ) -> Result<(), LspError> {
        let view = editor
            .views
            .iter()
            .find_map(|(id, view)| (view.buffer == buffer).then_some(id))
            .or_else(|| editor.active_view())
            .ok_or_else(|| LspError::Busy(server.into()))?;
        let owner = LspProjectionOwner {
            server: server.into(),
            buffer,
            view,
            revision: BufferRevision(document.version),
            text: editor.buffers[buffer].text.source_version(),
            uri: document.uri(),
            language: document.language_id.clone(),
        };
        let previous = self.projection.owner.replace(owner);
        let result = self.request_projected(server, feature, document, ByteOffset(0));
        self.projection.owner = previous;
        result
    }

    pub(in crate::app) fn request_projected(
        &mut self,
        server: &str,
        mut feature: LspFeature,
        document: LspDocument,
        offset: ByteOffset,
    ) -> Result<(), LspError> {
        let Some(owner) = self.projection.owner.clone() else {
            return Ok(());
        };
        let key = (server.to_owned(), document.uri());
        if matches!(feature, LspFeature::PullDiagnostics { .. })
            && let Some(id) = self.projection.pull_ids.get(&key)
        {
            feature = LspFeature::PullDiagnostics {
                previous_result_id: Some(id.clone()),
            };
        }
        if matches!(feature, LspFeature::SemanticTokensFull)
            && let Some((Some(id), _)) = self.projection.semantic.get(&key).filter(|_| {
                self.sessions
                    .get(server)
                    .and_then(|s| s.capabilities.as_ref())
                    .is_some_and(|c| c.semantic_tokens_delta)
            })
        {
            feature = LspFeature::SemanticTokensDelta {
                previous_result_id: id.clone(),
            };
        }
        let request_id = self
            .next_request_id()
            .ok_or_else(|| LspError::Busy(server.into()))?;
        if matches!(feature, LspFeature::SelectionRanges) {
            self.projection.selection_request = Some((request_id, offset, false));
        }
        let others: Vec<String> = match feature {
            LspFeature::Completion => self
                .attached_servers(owner.buffer, &owner.language, |c| {
                    LspFeature::Completion.capability_for_document(c, &document)
                })
                .filter(|key| key != server)
                .collect(),
            LspFeature::Hover => self
                .attached_servers(owner.buffer, &owner.language, |c| {
                    LspFeature::Hover.capability_for_document(c, &document)
                })
                .filter(|key| key != server)
                .collect(),
            LspFeature::CodeAction { .. } => self
                .attached_servers(owner.buffer, &owner.language, |c| {
                    feature.capability_for_document(c, &document)
                })
                .filter(|key| key != server)
                .collect(),
            _ => Vec::new(),
        };
        if !self.synced(server, owner.buffer, &owner.text) {
            self.sync_owned(server, document.clone(), (owner.buffer, owner.text.clone()))?;
        }
        self.service.request(
            server,
            self.feature_for_server(&feature, &document, &owner.server, server)?,
            document.clone(),
            offset,
            request_id,
        )?;
        self.projection.request = Some((request_id, owner.clone()));
        self.projection.request_servers = vec![server.to_owned()];
        // Like VS Code, every attached server answers, e.g. Tailwind classes beside TypeScript.
        for other in others {
            if !self.synced(&other, owner.buffer, &owner.text)
                && self
                    .sync_owned(&other, document.clone(), (owner.buffer, owner.text.clone()))
                    .is_err()
            {
                continue;
            }
            if self
                .service
                .request(
                    &other,
                    self.feature_for_server(&feature, &document, &owner.server, &other)?,
                    document.clone(),
                    offset,
                    request_id,
                )
                .is_ok()
            {
                self.projection.request_servers.push(other);
            }
        }
        self.projection
            .requests
            .retain(|_, (_, _, method)| method != feature.method());
        self.projection
            .request_cursors
            .retain(|request_id, _| self.projection.requests.contains_key(request_id));
        if matches!(
            feature,
            LspFeature::Hover
                | LspFeature::SignatureHelp
                | LspFeature::Completion
                | LspFeature::Definition
                | LspFeature::Declaration
                | LspFeature::References
                | LspFeature::Implementation
                | LspFeature::TypeDefinition
                | LspFeature::Rename { .. }
                | LspFeature::CodeAction { .. }
                | LspFeature::DocumentHighlights
                | LspFeature::SelectionRanges
        ) {
            self.projection.request_cursors.insert(request_id, offset);
        }
        self.projection.requests.insert(
            request_id,
            (
                owner,
                self.projection.request_servers.clone(),
                feature.method().into(),
            ),
        );
        Ok(())
    }
    fn feature_for_server(
        &self,
        feature: &LspFeature,
        document: &LspDocument,
        source: &str,
        target: &str,
    ) -> Result<LspFeature, LspError> {
        let mut feature = feature.clone();
        if let LspFeature::CodeAction { range, diagnostics } = &mut feature {
            let text = crate::editor::text::TextBuffer::from_text(&document.text);
            let codec = PositionCodec::new(&text);
            let convert =
                |position: &mut crate::language::lsp::LspPosition| -> Result<(), LspError> {
                    let byte = codec
                        .lsp_to_byte(
                            position.line,
                            position.character,
                            self.server_encoding(source),
                        )
                        .map_err(|_| LspError::Busy(target.into()))?;
                    let (line, character) = codec
                        .byte_to_lsp(byte, self.server_encoding(target))
                        .map_err(|_| LspError::Busy(target.into()))?;
                    *position = crate::language::lsp::LspPosition { line, character };
                    Ok(())
                };
            convert(&mut range.start)?;
            convert(&mut range.end)?;
            for diagnostic in diagnostics {
                convert(&mut diagnostic.range.start)?;
                convert(&mut diagnostic.range.end)?;
            }
        }
        Ok(feature)
    }
    /// Running servers that hold `buffer` and offer a capability.
    fn attached_servers<'a>(
        &'a self,
        buffer: BufferId,
        language: &'a str,
        offers: impl Fn(&crate::language::lsp::LspCapabilities) -> bool + 'a,
    ) -> impl Iterator<Item = String> + 'a {
        self.sessions
            .iter()
            .filter(move |(_, state)| {
                state.documents.contains_key(&buffer)
                    && state.definition.supports_language(language)
                    && state.capabilities.as_ref().is_some_and(&offers)
            })
            .map(|(key, _)| key.clone())
    }
    /// Asks every attached server with a colour provider for the document's colours.
    pub(in crate::app) fn request_colors(
        &self,
        buffer: BufferId,
        document: &LspDocument,
    ) -> Vec<String> {
        self.attached_servers(buffer, &document.language_id, |c| {
            LspFeature::DocumentColors.capability_for_document(c, document)
        })
        .filter(|server| {
            self.service
                .request(
                    server,
                    LspFeature::DocumentColors,
                    document.clone(),
                    ByteOffset(0),
                    COLOR_REQUEST_ID,
                )
                .is_ok()
        })
        .collect()
    }
    pub(in crate::app) fn is_breadcrumb_request(&self, request_id: u64) -> bool {
        request_id == BREADCRUMB_REQUEST_ID
    }
    /// Asks for the document's symbols once its text has been still for a moment.
    pub(in crate::app) fn request_breadcrumbs(
        &mut self,
        buffer: BufferId,
        document: &LspDocument,
        now: Instant,
    ) -> Option<String> {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(400);
        let current = (buffer, document.version);
        match self.projection.breadcrumbs_seen {
            Some((seen, since)) if seen == current && now.duration_since(since) >= SETTLE => {}
            Some((seen, _)) if seen == current => return None,
            _ => {
                self.projection.breadcrumb_retries = 0;
                self.projection.breadcrumbs_seen = Some((current, now));
                return None;
            }
        }
        if self.projection.breadcrumbs_asked == Some(current)
            || self.projection.breadcrumb_retries >= 3
        {
            return None;
        }
        self.supporting_server(buffer, document, &LspFeature::DocumentSymbols)
            .filter(|server| {
                self.sessions
                    .get(server)
                    .and_then(|state| state.capabilities.as_ref())
                    .is_some_and(|capabilities| {
                        LspFeature::DocumentSymbols.capability_for_document(capabilities, document)
                    })
            })
    }
    pub(in crate::app) fn accept_breadcrumbs(
        &mut self,
        server: &str,
        uri: &str,
        version: u64,
    ) -> bool {
        if self
            .projection
            .breadcrumbs_owner
            .as_ref()
            .is_some_and(|(asked, owner)| {
                asked == server
                    && owner.uri == uri
                    && owner.revision.0 == version
                    && self.projection.owner.as_ref() == Some(owner)
            })
        {
            return true;
        }
        self.retry_breadcrumbs();
        false
    }
    pub(in crate::app) fn retry_breadcrumbs(&mut self) {
        self.projection.breadcrumbs_asked = None;
        self.projection.breadcrumb_retries = self.projection.breadcrumb_retries.saturating_add(1);
        if let Some((_, since)) = &mut self.projection.breadcrumbs_seen {
            *since = Instant::now();
        }
    }
    pub(in crate::app) fn send_breadcrumbs(
        &mut self,
        server: &str,
        buffer: BufferId,
        document: LspDocument,
    ) {
        let version = document.version;
        if self
            .service
            .request(
                server,
                LspFeature::DocumentSymbols,
                document,
                ByteOffset(0),
                BREADCRUMB_REQUEST_ID,
            )
            .is_ok()
        {
            self.projection.breadcrumbs_asked = Some((buffer, version));
            self.projection.breadcrumbs_owner = self
                .projection
                .owner
                .clone()
                .map(|owner| (server.into(), owner));
        }
    }
    pub(in crate::app) fn server_encoding(
        &self,
        server: &str,
    ) -> crate::editor::text::PositionEncoding {
        self.sessions
            .get(server)
            .and_then(|state| state.capabilities.as_ref())
            .map_or(crate::editor::text::PositionEncoding::Utf16, |c| {
                c.position_encoding
            })
    }
    pub(in crate::app) fn request_inlays(&mut self, document: LspDocument) {
        self.projection.inlay_due = None;
        let Some(owner) = self.projection.owner.clone() else {
            return;
        };
        let Some(capabilities) = self
            .sessions
            .get(&owner.server)
            .and_then(|s| s.capabilities.as_ref())
            .filter(|c| c.inlay_hints)
        else {
            return;
        };
        if owner.uri != document.uri() {
            return;
        }
        let text = crate::editor::text::TextBuffer::from_text(&document.text);
        let Ok((line, character)) = PositionCodec::new(&text)
            .byte_to_lsp(ByteOffset(text.len_bytes()), capabilities.position_encoding)
        else {
            return;
        };
        let Some(next) = self
            .projection
            .inlay_request_id
            .checked_add(1)
            .filter(|next| *next < u64::MAX / 2)
        else {
            return;
        };
        self.projection.inlay_request_id = next;
        let request_id = u64::MAX - next;
        let feature = LspFeature::InlayHints {
            range: crate::language::lsp::LspRange {
                start: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 0,
                },
                end: crate::language::lsp::LspPosition { line, character },
            },
        };
        match self
            .service
            .request(&owner.server, feature, document, ByteOffset(0), request_id)
        {
            Ok(()) => self.projection.inlay_request = Some((request_id, owner)),
            Err(LspError::Busy(_)) => {
                self.projection.inlay_due = Some(Instant::now() + INLAY_DEBOUNCE);
            }
            Err(_) => {}
        }
    }
    pub(in crate::app) fn accept_result(
        &mut self,
        server: &str,
        request_id: u64,
        method: &str,
        uri: &str,
        version: u64,
        cursor: ByteOffset,
    ) -> bool {
        if self
            .projection
            .request_cursors
            .get(&request_id)
            .is_some_and(|asked| *asked != cursor)
        {
            self.projection.requests.remove(&request_id);
            self.projection.request_cursors.remove(&request_id);
            self.finish_request(server, request_id);
            return false;
        }
        if let Some((owner, servers, expected_method)) =
            self.projection.requests.get_mut(&request_id)
        {
            if expected_method != method
                || Some(&*owner) != self.projection.owner.as_ref()
                || owner.uri != uri
                || owner.revision.0 != version
                || !servers.iter().any(|s| s == server)
            {
                return false;
            }
            servers.retain(|s| s != server);
            if servers.is_empty() {
                self.projection.requests.remove(&request_id);
                self.projection.request_cursors.remove(&request_id);
            }
            if self
                .projection
                .request
                .as_ref()
                .is_some_and(|(g, _)| *g == request_id)
            {
                self.projection.request_servers.retain(|s| s != server);
                if self.projection.request_servers.is_empty() {
                    self.projection.request = None;
                }
            }
            return true;
        }
        let inlay = method == "textDocument/inlayHint";
        if !inlay && request_id != self.projection.request_id {
            return false;
        }
        let request = if inlay {
            &mut self.projection.inlay_request
        } else {
            &mut self.projection.request
        };
        if !request.as_ref().is_some_and(|(g, owner)| {
            *g == request_id
                && Some(owner) == self.projection.owner.as_ref()
                && (if inlay {
                    owner.server == server
                } else {
                    self.projection.request_servers.iter().any(|s| s == server)
                })
                && owner.uri == uri
                && owner.revision.0 == version
        }) {
            return false;
        }
        if !inlay {
            self.projection.request_servers.retain(|s| s != server);
            if !self.projection.request_servers.is_empty() {
                return true;
            }
        }
        *request = None;
        true
    }
    /// Whether another server still owes an answer to this request.
    pub(in crate::app) fn request_pending(&self, request_id: u64) -> bool {
        self.projection
            .requests
            .get(&request_id)
            .is_some_and(|(_, servers, _)| !servers.is_empty())
            || (self
                .projection
                .request
                .as_ref()
                .is_some_and(|(current, _)| *current == request_id)
                && !self.projection.request_servers.is_empty())
    }
    pub(in crate::app) fn finish_request(&mut self, server: &str, request_id: u64) {
        self.forget_request(server, request_id);
        if self
            .projection
            .request
            .as_ref()
            .is_some_and(|(current, _)| *current == request_id)
            && self.projection.request_servers.is_empty()
        {
            self.projection.request = None;
        }
    }
    pub(in crate::app) fn forget_request(&mut self, server: &str, request_id: u64) {
        if let Some((_, servers, _)) = self.projection.requests.get_mut(&request_id) {
            servers.retain(|s| s != server);
            if servers.is_empty() {
                self.projection.requests.remove(&request_id);
            }
        }
        if self
            .projection
            .request
            .as_ref()
            .is_some_and(|(current, _)| *current == request_id)
        {
            self.projection.request_servers.retain(|s| s != server);
        }
    }
    pub(in crate::app) fn fail_inlay(&mut self, server: &str, request_id: u64) -> bool {
        if !self
            .projection
            .inlay_request
            .as_ref()
            .is_some_and(|(g, owner)| *g == request_id && owner.server == server)
        {
            return false;
        }
        self.projection.inlay_request = None;
        true
    }
    pub(in crate::app) fn inlay_pending(&self) -> bool {
        self.projection.inlay_due.is_some()
    }
    /// Whether inlay hints for `buffer` are scheduled or on their way.
    pub(in crate::app) fn inlays_in_flight(&self, buffer: BufferId) -> bool {
        self.projection
            .inlay_request
            .as_ref()
            .is_some_and(|(_, owner)| owner.buffer == buffer)
            || self.projection.inlay_due.is_some()
                && self
                    .projection
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.buffer == buffer)
    }
    pub(in crate::app) fn inlay_ready(&mut self, config: &LspConfig, now: Instant) -> bool {
        if !config.inlay_hints {
            self.disable_inlays();
            return false;
        }
        if self.projection.inlay_due.is_none_or(|due| now < due) {
            return false;
        }
        self.projection.inlay_due = None;
        true
    }
    fn disable_inlays(&mut self) {
        self.projection.inlay_due = None;
        self.projection.inlay_request = None;
        self.projection.inlay_retries = 0;
    }
    pub(in crate::app) fn schedule_inlays(
        &mut self,
        config: &LspConfig,
        immediate: bool,
        now: Instant,
    ) {
        if !config.inlay_hints {
            self.disable_inlays();
            return;
        }
        self.projection.inlay_retries = 0;
        self.projection.inlay_due = Some(
            now + if immediate {
                Duration::ZERO
            } else {
                INLAY_DEBOUNCE
            },
        );
    }
    pub(in crate::app) fn startup_inlays(&mut self, config: &LspConfig, now: Instant) {
        self.projection.inlay_retries = 0;
        self.projection.inlay_due = Some(
            now + if config.inlay_hint_level == InlayHintLevel::Max {
                INLAY_STARTUP
            } else {
                Duration::ZERO
            },
        );
    }
    pub(in crate::app) fn retry_inlays(&mut self, config: &LspConfig, now: Instant) -> bool {
        if !config.inlay_hints || self.projection.inlay_retries >= INLAY_RETRY_LIMIT {
            return false;
        }
        let multiplier = 1u32 << self.projection.inlay_retries.min(4);
        self.projection.inlay_retries += 1;
        self.projection.inlay_due = Some(now + INLAY_EMPTY_RETRY.saturating_mul(multiplier));
        true
    }
    pub(in crate::app) fn inlays_received(&mut self) {
        self.projection.inlay_retries = 0;
        self.projection.inlay_due = None;
    }
    pub(in crate::app) fn completion_current(&self, editor: &EditorState) -> bool {
        self.projection
            .completion_owner
            .as_ref()
            .is_some_and(|(_, owner)| {
                editor.active_view() == Some(owner.view)
                    && editor
                        .views
                        .get(owner.view)
                        .is_some_and(|view| view.buffer == owner.buffer)
                    && editor
                        .buffers
                        .get(owner.buffer)
                        .is_some_and(|buffer| buffer.text.source_version() == owner.text)
            })
    }
    pub(in crate::app) fn completion_items(&self) -> &[LspCompletionItem] {
        &self.projection.completion_items
    }
    pub(in crate::app) fn register_completion_source(
        &mut self,
        source: &str,
        encoding: crate::editor::text::PositionEncoding,
        inline: bool,
    ) {
        self.projection
            .completion_sources
            .insert(source.into(), CompletionSource { encoding, inline });
    }

    /// LSP uses the same source publication path as other completion producers.
    pub(in crate::app) fn install_completions(
        &mut self,
        server: &str,
        items: Vec<LspCompletionItem>,
        incomplete: bool,
        request_id: u64,
    ) {
        self.register_completion_source(server, self.server_encoding(server), false);
        if self
            .projection
            .completion_owner
            .as_ref()
            .is_none_or(|(id, _)| *id != request_id)
            && let Some(owner) = self.projection.owner.clone()
        {
            self.start_completions(owner, request_id);
        }
        self.publish_completions(server, items, incomplete, request_id);
    }

    /// The first answer replaces the list; later sources merge into it. Publishing again
    /// replaces that source's batch, so streaming updates cannot duplicate its rows.
    pub(in crate::app) fn publish_completions(
        &mut self,
        source: &str,
        items: Vec<LspCompletionItem>,
        incomplete: bool,
        request_id: u64,
    ) {
        if !self.projection.completion_sources.contains_key(source)
            || self
                .projection
                .completion_owner
                .as_ref()
                .is_none_or(|(id, _)| *id != request_id)
        {
            return;
        }
        let server = source;
        self.remove_completion_source_items(source);
        self.projection.completion_incomplete |= incomplete;
        self.projection.completion_narrowed = None;
        self.projection
            .completion_servers
            .extend(std::iter::repeat_n(server.to_owned(), items.len()));
        self.projection
            .completion_items
            .extend(items.iter().cloned());
        self.projection
            .completion_all
            .extend(items.into_iter().map(|item| (server.to_owned(), item)));
    }
    /// A request is bound to a document snapshot, not to a running language server.
    pub(in crate::app) fn start_completions(&mut self, owner: LspProjectionOwner, request_id: u64) {
        self.projection.completion_owner = Some((request_id, owner));
        self.projection.completion_items.clear();
        self.projection.completion_servers.clear();
        self.projection.completion_all.clear();
        self.projection.completion_incomplete = false;
        self.projection.completion_selected = 0;
        self.projection.completion_scroll = 0;
        self.projection.completion_narrowed = None;
        self.projection.resolve = None;
    }

    pub(in crate::app) fn completion_encoding(
        &self,
        source: &str,
    ) -> crate::editor::text::PositionEncoding {
        self.projection
            .completion_sources
            .get(source)
            .map_or_else(|| self.server_encoding(source), |source| source.encoding)
    }

    /// Ghost text is only a suffix of the selected insertion. Replacements that cannot
    /// be represented as ghost text still remain available in the menu.
    pub(in crate::app) fn inline_completion(&self, editor: &EditorState) -> Option<String> {
        if !self.completion_current(editor) {
            return None;
        }
        let selected = self.projection.completion_selected;
        let source = self.completion_server(selected)?;
        if !self.projection.completion_sources.get(source)?.inline {
            return None;
        }
        let item = self.completion_items().get(selected)?;
        let view = editor.views.get(editor.active_view()?)?;
        let text = &editor.buffers.get(view.buffer)?.text;
        let (start, insertion) = if let Some(edit) = &item.text_edit {
            let codec = PositionCodec::new(text);
            let encoding = self.completion_encoding(source);
            let end = codec
                .lsp_to_byte(edit.range.end.line, edit.range.end.character, encoding)
                .ok()?;
            if end != view.cursor {
                return None;
            }
            (
                codec
                    .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
                    .ok()?,
                edit.new_text.clone(),
            )
        } else {
            (
                self.completion_start().unwrap_or(view.cursor),
                item.insert_text.clone(),
            )
        };
        let insertion = if item.snippet {
            crate::language::lsp::parse_snippet(&insertion, 1024 * 1024)
                .ok()?
                .text
        } else {
            insertion
        };
        let typed = text.slice(ByteRange::new(start, view.cursor).ok()?).ok()?;
        insertion
            .strip_prefix(&typed)
            .filter(|suffix| !suffix.is_empty())
            .map(str::to_owned)
    }

    /// Records a typed character for the next sync, which decides whether it opens the menu.
    pub(in crate::app) fn note_typed(&mut self, text: &str) {
        let mut characters = text.chars();
        self.projection.typed = characters.next().filter(|_| characters.next().is_none());
    }
    pub(in crate::app) fn take_typed(&mut self) -> Option<char> {
        self.projection.typed.take()
    }
    pub(in crate::app) fn is_completion_trigger(&self, servers: &[String], typed: char) -> bool {
        let typed = typed.to_string();
        servers.iter().any(|server| {
            self.sessions
                .get(server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.completion_triggers.contains(&typed))
        })
    }
    pub(in crate::app) fn begin_completion(&mut self, start: Option<ByteOffset>, automatic: bool) {
        self.projection.completion_start = start;
        self.projection.completion_automatic = automatic;
    }
    pub(in crate::app) fn completion_start(&self) -> Option<ByteOffset> {
        self.projection.completion_start
    }
    pub(in crate::app) fn completion_automatic(&self) -> bool {
        self.projection.completion_automatic
    }
    pub(in crate::app) fn completion_incomplete(&self) -> bool {
        self.projection.completion_incomplete
    }
    pub(in crate::app) fn completion_position(&self) -> (usize, usize) {
        (
            self.projection.completion_selected,
            self.projection.completion_scroll,
        )
    }
    /// True once per cursor and text position, so an idle pass does no work.
    pub(in crate::app) fn completion_needs_narrowing(
        &self,
        cursor: ByteOffset,
        revision: crate::editor::text::BufferRevision,
    ) -> bool {
        self.projection.completion_narrowed != Some((cursor, revision))
    }
    pub(in crate::app) fn completion_encodings(
        &self,
    ) -> HashMap<String, crate::editor::text::PositionEncoding> {
        self.projection
            .completion_all
            .iter()
            .map(|(server, _)| (server.clone(), self.completion_encoding(server)))
            .collect()
    }
    /// Scores each item against the text `typed` gives for it; an item that doesn't match gets none.
    pub(in crate::app) fn score_completions(
        &self,
        typed: impl Fn(&str, &LspCompletionItem) -> String,
    ) -> Vec<Option<i64>> {
        hide_scattered_matches(
            self.projection
                .completion_all
                .iter()
                .map(|(server, item)| {
                    let target = item.filter_text.as_deref().unwrap_or(&item.label);
                    let query = typed(server, item);
                    let score = starts_at_a_word(&query, target)
                        .then(|| crate::workspace::fuzzy_score(&query, target, ""))
                        .flatten();
                    let whole = target.to_lowercase().contains(&query.to_lowercase());
                    (score, whole)
                })
                .collect(),
        )
    }
    /// Keeps the matching items, best first and in the server's order among equals. A selection
    /// the user moved stays on its item while that item is still listed.
    pub(in crate::app) fn narrow_completions(
        &mut self,
        scores: Vec<Option<i64>>,
        cursor: ByteOffset,
        revision: crate::editor::text::BufferRevision,
    ) {
        let selected = self.projection.completion_selected;
        let identity = |index: usize| {
            self.projection
                .completion_servers
                .get(index)
                .cloned()
                .zip(self.projection.completion_items.get(index).cloned())
        };
        let kept = (selected > 0 || self.projection.resolve.is_some())
            .then(|| identity(selected))
            .flatten();
        let resolving = self
            .projection
            .resolve
            .as_ref()
            .and_then(|(_, _, index, _)| identity(*index));
        let mut order = scores
            .iter()
            .enumerate()
            .filter_map(|(index, score)| score.map(|score| (index, score)))
            .collect::<Vec<_>>();
        order.sort_by(|(left, left_score), (right, right_score)| {
            let rank = |index: usize| {
                let item = &self.projection.completion_all[index].1;
                item.sort_text.as_deref().unwrap_or(&item.label)
            };
            right_score
                .cmp(left_score)
                .then_with(|| rank(*left).cmp(rank(*right)))
        });
        let (servers, items): (Vec<_>, Vec<_>) = order
            .into_iter()
            .map(|(index, _)| self.projection.completion_all[index].clone())
            .unzip();
        let position = |identity: &(String, LspCompletionItem)| {
            servers
                .iter()
                .zip(&items)
                .position(|(server, item)| server == &identity.0 && item == &identity.1)
        };
        if let Some(resolve) = &mut self.projection.resolve {
            if let Some(index) = resolving.as_ref().and_then(&position) {
                resolve.2 = index;
            } else {
                self.projection.resolve = None;
            }
        }
        self.projection.completion_selected = kept.as_ref().and_then(position).unwrap_or(0);
        self.projection.completion_servers = servers;
        self.projection.completion_items = items;
        self.projection.completion_scroll = self
            .projection
            .completion_scroll
            .min(self.projection.completion_selected);
        self.projection.completion_narrowed = Some((cursor, revision));
    }
    pub(in crate::app) fn completion_server(&self, index: usize) -> Option<&str> {
        self.projection
            .completion_servers
            .get(index)
            .map(String::as_str)
    }
    pub(in crate::app) fn cancel_completion(&mut self) {
        self.clear_completion();
    }
    pub(in crate::app) fn clear_completion(&mut self) {
        let pending_completion =
            self.projection
                .requests
                .iter()
                .any(|(request_id, (_, _, method))| {
                    *request_id == self.projection.request_id
                        && (method == "textDocument/completion"
                            || method == "completionItem/resolve")
                });
        for (request_id, (_, servers, method)) in &self.projection.requests {
            if method == "textDocument/completion" || method == "completionItem/resolve" {
                for server in servers {
                    let _ = self.service.cancel(server, *request_id);
                }
            }
        }
        self.projection.requests.retain(|_, (_, _, method)| {
            method != "textDocument/completion" && method != "completionItem/resolve"
        });
        if pending_completion
            || self
                .projection
                .completion_owner
                .as_ref()
                .is_some_and(|(request_id, _)| *request_id == self.projection.request_id)
            || self
                .projection
                .resolve
                .as_ref()
                .is_some_and(|(request_id, _, _, _)| *request_id == self.projection.request_id)
        {
            for server in &self.projection.request_servers {
                let _ = self.service.cancel(server, self.projection.request_id);
            }
            self.projection.request = None;
            self.projection.request_servers.clear();
        }
        self.projection.completion_items.clear();
        self.projection.completion_servers.clear();
        self.projection.completion_all.clear();
        self.projection.completion_incomplete = false;
        self.projection.completion_start = None;
        self.projection.completion_automatic = false;
        self.projection.completion_narrowed = None;
        self.projection.completion_owner = None;
        self.projection.resolve = None;
        self.projection.completion_selected = 0;
        self.projection.completion_scroll = 0;
    }
    pub(in crate::app) fn move_completion(
        &mut self,
        direction: i8,
        rows: usize,
    ) -> Option<(usize, usize)> {
        let count = self.projection.completion_items.len();
        if count == 0 {
            return None;
        }
        let selected = &mut self.projection.completion_selected;
        *selected = if direction < 0 {
            selected.checked_sub(1).unwrap_or(count - 1)
        } else {
            (*selected + 1) % count
        };
        let scroll = &mut self.projection.completion_scroll;
        if *selected < *scroll {
            *scroll = *selected;
        } else if *selected >= scroll.saturating_add(rows.max(1)) {
            *scroll = selected.saturating_add(1).saturating_sub(rows.max(1));
        }
        Some((*selected, *scroll))
    }
    pub(in crate::app) fn completion_documentation(
        &self,
        editor: &EditorState,
        selected: usize,
    ) -> Result<(String, String, LspMarkupContent), &'static str> {
        let (_, owner) = self
            .projection
            .completion_owner
            .as_ref()
            .ok_or("completion documentation is stale")?;
        if self.projection.owner.as_ref() != Some(owner)
            || editor.active_view() != Some(owner.view)
            || !editor
                .views
                .get(owner.view)
                .is_some_and(|view| view.buffer == owner.buffer)
            || !editor
                .buffers
                .get(owner.buffer)
                .is_some_and(|buffer| buffer.text.source_version() == owner.text)
        {
            return Err("completion documentation is stale");
        }
        let item = self
            .projection
            .completion_items
            .get(selected)
            .ok_or("selected completion has no documentation")?;
        let documentation = match (item.signature(), item.documentation.clone()) {
            (Some(signature), documentation) => LspMarkupContent {
                kind: LspMarkupKind::Markdown,
                value: format!(
                    "```{}\n{signature}\n```{}",
                    owner.language,
                    documentation.map_or(String::new(), |doc| format!("\n\n{}", doc.value))
                ),
            },
            (None, Some(documentation)) => documentation,
            (None, None) => return Err("selected completion has no documentation"),
        };
        let server = self
            .completion_server(selected)
            .unwrap_or(&owner.server)
            .to_owned();
        Ok((server, item.label.clone(), documentation))
    }
    pub(in crate::app) fn resolve_completion(
        &mut self,
        selected: usize,
        document: LspDocument,
        offset: ByteOffset,
        accept: bool,
    ) -> Result<(), LspError> {
        if self
            .projection
            .resolve
            .as_ref()
            .is_some_and(|(_, _, index, at)| *index == selected && *at == offset)
        {
            self.projection.resolve_accept |= accept;
            return Ok(());
        }
        let Some(owner) = self.projection.owner.clone() else {
            return Ok(());
        };
        let Some(item) = self
            .projection
            .completion_items
            .get(selected)
            .and_then(|item| item.resolve_item.clone())
        else {
            return Ok(());
        };
        let server = self
            .completion_server(selected)
            .unwrap_or(&owner.server)
            .to_owned();
        let request_id = self
            .next_request_id()
            .ok_or_else(|| LspError::Busy(server.clone()))?;
        self.service.request(
            &server,
            LspFeature::CompletionResolve { item },
            document,
            offset,
            request_id,
        )?;
        self.projection.request = Some((request_id, owner.clone()));
        self.projection.request_servers = vec![server.clone()];
        self.projection.requests.insert(
            request_id,
            (owner.clone(), vec![server], "completionItem/resolve".into()),
        );
        self.projection.resolve = Some((request_id, owner, selected, offset));
        self.projection.resolve_accept = accept;
        Ok(())
    }
    pub(in crate::app) fn completion_resolve_accepts(&self, request_id: u64) -> bool {
        self.projection.resolve_accept
            && self
                .projection
                .resolve
                .as_ref()
                .is_some_and(|(id, ..)| *id == request_id)
    }
    pub(in crate::app) fn accept_resolved(
        &mut self,
        request_id: u64,
        item: LspCompletionItem,
        editor: &EditorState,
    ) -> bool {
        let Some((expected, owner, index, offset)) = self.projection.resolve.as_ref() else {
            return false;
        };
        if request_id != *expected
            || self.projection.owner.as_ref() != Some(owner)
            || *index >= self.projection.completion_items.len()
        {
            return false;
        }
        let index = *index;
        let activate = index == self.projection.completion_selected
            && editor.active_view() == Some(owner.view)
            && editor
                .views
                .get(owner.view)
                .is_some_and(|view| view.buffer == owner.buffer && view.cursor == *offset)
            && editor
                .buffers
                .get(owner.buffer)
                .is_some_and(|buffer| buffer.text.source_version() == owner.text);
        self.projection.resolve = None;
        if let Some(canonical) =
            self.projection
                .completion_all
                .iter_mut()
                .find(|(server, candidate)| {
                    self.projection.completion_servers.get(index) == Some(server)
                        && candidate == &self.projection.completion_items[index]
                })
        {
            canonical.1 = item.clone();
        }
        self.projection.completion_items[index] = item;
        activate
    }
    pub(in crate::app) fn selection_ranges(&self) -> &[ByteRange] {
        &self.projection.selection_ranges
    }
    pub(in crate::app) fn install_selection_ranges(&mut self, ranges: Vec<ByteRange>) {
        self.projection.selection_ranges = ranges;
    }
    pub(in crate::app) fn selection_ranges_current(&self, cursor: ByteOffset) -> bool {
        self.projection
            .selection_cursor
            .is_none_or(|origin| origin == cursor)
    }
    pub(in crate::app) fn expand_requested_selection(&mut self) {
        if let Some((_, _, expand)) = &mut self.projection.selection_request {
            *expand = true;
        }
    }
    pub(in crate::app) fn complete_selection_ranges(
        &mut self,
        request_id: u64,
        ranges: Vec<ByteRange>,
    ) -> Option<ByteOffset> {
        let request = self
            .projection
            .selection_request
            .take()
            .filter(|(pending, _, _)| *pending == request_id);
        self.projection.selection_cursor = request.map(|(_, cursor, _)| cursor);
        self.install_selection_ranges(ranges);
        request
            .filter(|(_, cursor, expand)| {
                *expand
                    && self
                        .projection
                        .selection_ranges
                        .iter()
                        .any(|range| range.contains(*cursor))
            })
            .map(|(_, cursor, _)| cursor)
    }
    pub(in crate::app) fn record_pull(&mut self, server: &str, uri: &str, id: String) {
        self.projection
            .pull_ids
            .insert((server.into(), uri.into()), id);
    }
    pub(in crate::app) fn record_semantic(
        &mut self,
        server: &str,
        uri: &str,
        result: &LspSemanticTokens,
    ) -> bool {
        if result.data.len() > 500_000 || !result.data.len().is_multiple_of(5) {
            return false;
        }
        self.projection.semantic.insert(
            (server.into(), uri.into()),
            (result.result_id.clone(), result.data.clone()),
        );
        true
    }
    pub(in crate::app) fn semantic_delta(
        &mut self,
        server: &str,
        uri: &str,
        delta: LspSemanticTokenDelta,
    ) -> Option<LspSemanticTokens> {
        let key = (server.to_owned(), uri.to_owned());
        let (_, original) = self.projection.semantic.get(&key)?;
        let mut edits = delta.edits;
        edits.sort_by_key(|edit| edit.start);
        let mut len = original.len();
        let mut previous_end = 0;
        for edit in &edits {
            let Some(end) = edit.start.checked_add(edit.delete_count) else {
                self.projection.semantic.remove(&key);
                return None;
            };
            if edit.start < previous_end || end > original.len() {
                self.projection.semantic.remove(&key);
                return None;
            }
            previous_end = end;
            len = len
                .checked_sub(edit.delete_count)?
                .checked_add(edit.data.len())?;
        }
        if len > 500_000 || len % 5 != 0 {
            self.projection.semantic.remove(&key);
            return None;
        }
        let mut data = original.clone();
        for edit in edits.into_iter().rev() {
            data.splice(edit.start..edit.start + edit.delete_count, edit.data);
        }
        Some(LspSemanticTokens {
            result_id: delta.result_id,
            data,
        })
    }
    pub(in crate::app) fn clear_server_projection(&mut self, server: &str) {
        self.projection.semantic.retain(|(key, _), _| key != server);
        self.projection.pull_ids.retain(|(key, _), _| key != server);
        self.projection.request_servers.retain(|key| key != server);
        self.projection.requests.retain(|_, (_, servers, _)| {
            servers.retain(|key| key != server);
            !servers.is_empty()
        });
        if self.projection.request_servers.is_empty() {
            self.projection.request = None;
        }
        self.projection.completion_sources.remove(server);
        self.remove_completion_source_items(server);
    }

    pub(in crate::app) fn remove_completion_source(&mut self, source: &str) {
        self.projection.completion_sources.remove(source);
        self.remove_completion_source_items(source);
    }

    fn remove_completion_source_items(&mut self, server: &str) {
        let retained_index = |index: usize| {
            (self
                .projection
                .completion_servers
                .get(index)
                .is_some_and(|key| key != server))
            .then(|| {
                self.projection.completion_servers[..index]
                    .iter()
                    .filter(|key| *key != server)
                    .count()
            })
        };
        if let Some((_, _, index, _)) = &mut self.projection.resolve {
            if let Some(retained) = retained_index(*index) {
                *index = retained;
            } else {
                self.projection.resolve = None;
            }
        }
        self.projection.completion_selected =
            retained_index(self.projection.completion_selected).unwrap_or(0);
        let mut index = 0;
        self.projection.completion_items.retain(|_| {
            let keep = self.projection.completion_servers[index] != server;
            index += 1;
            keep
        });
        self.projection
            .completion_servers
            .retain(|key| key != server);
        self.projection
            .completion_all
            .retain(|(key, _)| key != server);
        self.projection.completion_narrowed = None;
        self.projection.completion_scroll = self
            .projection
            .completion_scroll
            .min(self.projection.completion_selected);
    }
    pub(in crate::app) fn clear_document_projection(&mut self, server: &str, uri: &str) {
        let key = (server.into(), uri.into());
        self.projection.semantic.remove(&key);
        self.projection.pull_ids.remove(&key);
    }
    pub(in crate::app) fn append_log(&mut self, line: String, maximum: usize) {
        while self.projection.logs.len() >= maximum.max(1) {
            self.projection.logs.pop_front();
            self.projection.log_start = self.projection.log_start.saturating_add(1);
        }
        self.projection.logs.push_back(line);
    }
    pub(in crate::app) fn logs(&self) -> &VecDeque<String> {
        &self.projection.logs
    }
    pub(in crate::app) fn log_start(&self) -> u64 {
        self.projection.log_start
    }
    pub(in crate::app) fn server_loading(&self, server: &str) -> bool {
        self.projection
            .loading
            .get(server)
            .is_some_and(|tokens| !tokens.is_empty())
    }
    pub(in crate::app) fn server_label(&self, key: &str) -> String {
        self.sessions
            .get(key)
            .map(|s| s.server_id.clone())
            .or_else(|| {
                key.strip_prefix("root:")
                    .and_then(|key| key.rsplit_once(':'))
                    .map(|(_, server)| server.to_owned())
            })
            .unwrap_or_else(|| key.to_owned())
    }
    pub(in crate::app) fn progress(&self) -> (&[UiLspProgress], usize) {
        (&self.projection.progress, self.projection.progress_frame)
    }
}

impl LspSession {
    pub(in crate::app) fn update_progress(
        &mut self,
        event: (String, String, LspProgressKind, String, String, Option<u8>),
        policy: &LspProgressConfig,
        now: Instant,
    ) {
        let (server_key, token, kind, title, message, percentage) = event;
        if kind == LspProgressKind::End {
            if let Some(tokens) = self.projection.loading.get_mut(&server_key) {
                tokens.remove(&token);
                if tokens.is_empty() {
                    self.projection.loading.remove(&server_key);
                }
            }
        } else {
            self.projection
                .loading
                .entry(server_key.clone())
                .or_default()
                .insert(token.clone());
        }
        if !policy.enabled {
            self.projection.progress.clear();
            self.projection.progress_frame = 0;
            return;
        }
        let server = self.server_label(&server_key);
        let had_active = self.projection.progress.iter().any(|item| !item.done);
        if let Some(item) = self
            .projection
            .progress
            .iter_mut()
            .find(|item| item.server_key == server_key && item.token == token)
        {
            if kind == LspProgressKind::End {
                if !title.is_empty() {
                    item.title = title;
                }
                if !message.is_empty() {
                    item.message = message;
                }
                item.percentage = None;
                item.done = true;
                item.completed_at = Some(now);
                return;
            }
            if item.done && kind != LspProgressKind::Begin {
                return;
            }
            item.server = server;
            if kind == LspProgressKind::Begin || !title.is_empty() {
                item.title = title;
            }
            if kind == LspProgressKind::Begin || !message.is_empty() {
                item.message = message;
            }
            if kind == LspProgressKind::Begin || percentage.is_some() {
                item.percentage = percentage;
            }
            if kind == LspProgressKind::Begin {
                item.done = false;
                item.completed_at = None;
                if !had_active {
                    self.projection.progress_epoch = Some(now);
                    self.projection.progress_frame = 0;
                }
            }
            return;
        }
        if kind == LspProgressKind::End {
            return;
        }

        let was_empty = self.projection.progress.is_empty();
        let maximum = usize::from(crate::ui::render::overlays::LSP_PROGRESS_MAX_ROWS);
        while self.projection.progress.len() >= maximum {
            self.projection.progress.remove(0);
        }
        self.projection.progress.push(UiLspProgress {
            server_key,
            token,
            server,
            title,
            message,
            percentage,
            done: false,
            completed_at: None,
        });
        if was_empty {
            self.projection.progress_epoch = Some(now);
            self.projection.progress_frame = 0;
        }
    }

    pub(in crate::app) fn clear_progress(&mut self, server_key: &str) {
        self.projection.loading.remove(server_key);
        self.projection
            .progress
            .retain(|item| item.server_key != server_key);
        if self.projection.progress.is_empty() {
            self.projection.progress_frame = 0;
        }
    }

    pub(in crate::app) fn animate_progress(&mut self, policy: &LspProgressConfig, now: Instant) {
        if self.projection.progress.is_empty() {
            return;
        }
        if !policy.enabled {
            self.projection.progress.clear();
            self.projection.progress_frame = 0;
            return;
        }
        let maximum = usize::from(crate::ui::render::overlays::LSP_PROGRESS_MAX_ROWS);
        if self.projection.progress.len() > maximum {
            let excess = self.projection.progress.len().saturating_sub(maximum);
            self.projection.progress.drain(..excess);
        }
        let completed_timeout =
            Duration::from_millis(crate::ui::render::overlays::LSP_PROGRESS_DONE_TIMEOUT_MS);
        self.projection.progress.retain(|item| {
            item.completed_at.is_none_or(|completed| {
                now.saturating_duration_since(completed) < completed_timeout
            })
        });
        if self.projection.progress.is_empty() {
            self.projection.progress_frame = 0;
            return;
        }
        if !self.projection.progress.iter().any(|item| !item.done) {
            return;
        }
        let elapsed = now.saturating_duration_since(self.projection.progress_epoch.unwrap_or(now));
        let phase =
            elapsed.as_millis() / u128::from(crate::ui::render::overlays::SPINNER_INTERVAL_MS);
        let frame =
            usize::try_from(phase % crate::ui::render::overlays::SPINNER_FRAMES.len() as u128)
                .unwrap_or(0);
        if self.projection.progress_frame != frame {
            self.projection.progress_frame = frame;
        }
    }
}

#[cfg(test)]
impl LspSession {
    pub(in crate::app) fn fixture_request_id(&mut self, request_id: u64) {
        self.projection.request_id = request_id;
    }
    pub(in crate::app) fn fixture_projection(&mut self, owner: Option<LspProjectionOwner>) {
        self.projection.owner = owner;
    }
    pub(in crate::app) fn fixture_completion_source(
        &mut self,
        owner: Option<(u64, LspProjectionOwner)>,
    ) {
        self.projection.completion_owner = owner;
    }
    pub(in crate::app) fn fixture_completion_items(&mut self, items: Vec<LspCompletionItem>) {
        self.projection.completion_items = items;
    }
    pub(in crate::app) fn fixture_documentation(
        &mut self,
        index: usize,
        documentation: Option<LspMarkupContent>,
    ) {
        self.projection.completion_items[index].documentation = documentation;
    }
    pub(in crate::app) fn fixture_inlay_source(
        &mut self,
        source: Option<(u64, LspProjectionOwner)>,
    ) {
        self.projection.inlay_request = source;
    }
    pub(in crate::app) fn inlay_source(&self) -> Option<(u64, LspProjectionOwner)> {
        self.projection.inlay_request.clone()
    }
    pub(in crate::app) fn inlay_retries(&self) -> u8 {
        self.projection.inlay_retries
    }
    pub(in crate::app) fn fixture_progress_epoch(&mut self, epoch: Instant) {
        self.projection.progress_epoch = Some(epoch);
    }
    pub(in crate::app) fn fixture_progress_completed(&mut self, completed: Instant) {
        for item in &mut self.projection.progress {
            if item.done {
                item.completed_at = Some(completed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::language::LspProjectionOwner;
    use crate::app::language::LspSession;
    use crate::editor::EditorState;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    use crate::language::lsp::LspCompletionItem;
    use crate::language::lsp::LspDocument;
    use crate::language::lsp::LspSemanticTokenDelta;
    use crate::language::lsp::LspSemanticTokenEdit;
    use crate::language::lsp::LspSemanticTokens;
    use std::collections::HashMap;
    use std::time::Duration;
    use std::time::Instant;

    #[test]
    fn cancelled_completion_rejects_a_late_secondary_response() {
        let mut editor = EditorState::new("/root".into());
        let (buffer, view) = editor
            .open_text(TextBuffer::from_text("a"), Some("/root/a.rs".into()))
            .unwrap();
        let source = LspProjectionOwner {
            server: "first".into(),
            buffer,
            view,
            text: editor.buffers[buffer].text.source_version(),
            revision: editor.buffers[buffer].text.revision(),
            uri: "file:///root/a.rs".into(),
            language: "rust".into(),
        };
        let mut owner = LspSession::default();
        owner.projection.request_id = 7;
        owner.projection.owner = Some(source.clone());
        owner.projection.request = Some((7, source.clone()));
        owner.projection.request_servers = vec!["second".into()];
        owner.install_completions("first", vec![item("answer")], false, 7);
        owner.cancel_completion();
        assert!(!owner.accept_result(
            "second",
            7,
            "textDocument/completion",
            &source.uri,
            source.revision.0,
            ByteOffset(0)
        ));
        assert!(owner.completion_items().is_empty());
    }
    fn item(label: &str) -> LspCompletionItem {
        LspCompletionItem {
            label: label.into(),
            filter_text: None,
            sort_text: None,
            detail: String::new(),
            insert_text: label.into(),
            text_edit: None,
            additional_edits: Vec::new(),
            kind: None,
            snippet: false,
            documentation: None,
            command: None,
            resolve_item: None,
            resolved: true,
        }
    }
    pub(in crate::app) fn fixture() -> (
        EditorState,
        crate::editor::BufferId,
        LspDocument,
        LspSession,
    ) {
        use crate::app::language::LspLaunch;
        use crate::app::language::LspRuntimeState;
        let root = std::path::PathBuf::from("/fixture");
        let mut editor = EditorState::new(root.clone());
        let (buffer, _) = editor
            .open_text(
                TextBuffer::from_text("fooo tail"),
                Some(root.join("source.rs")),
            )
            .unwrap();
        let document = LspDocument {
            path: root.join("source.rs"),
            language_id: "rust".into(),
            version: 0,
            text: "fooo tail".into(),
        };
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|d| d.id == "rust-analyzer")
            .unwrap();
        let mut owner = LspSession::default();
        owner.sessions.insert(
            "owned".into(),
            LspRuntimeState {
                root: crate::workspace::RootId(1),
                root_path: root,
                server_id: definition.id.clone(),
                definition,
                launch: LspLaunch::Direct {
                    executable: "/server".into(),
                    managed: false,
                },
                capabilities: Some(Default::default()),
                documents: HashMap::from([(buffer, document.clone())]),
                restart_count: 0,
                restart_due: None,
                circuit_open: false,
                desired_running: true,
                activate_on_restart: true,
            },
        );
        owner.active = Some("owned".into());
        owner.refresh_projection(&editor, Some((buffer, &document)));
        (editor, buffer, document, owner)
    }
    #[test]
    fn configuration_updates_retry_busy_queues_and_keep_the_latest_settings() {
        let (editor, buffer, document, mut owner) = fixture();
        let endpoint = owner.service.replace_edit_source_fixture(
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
        );
        endpoint.block_responses();
        let standard = crate::language::lsp::LspServerConfiguration::standard();
        assert!(matches!(
            owner.configure("owned-edit-fixture", standard.clone()),
            Err(crate::language::lsp::LspError::Busy(_))
        ));
        assert!(owner.retry_configurations().is_empty());
        let maximum =
            crate::language::lsp::LspServerConfiguration::maximum_inlay_hints("rust-analyzer");
        assert!(
            owner
                .configure("owned-edit-fixture", maximum.clone())
                .is_err()
        );
        endpoint.unblock_responses();
        assert!(owner.retry_configurations().is_empty());
        assert_eq!(endpoint.configured(), Some(maximum));
        assert!(owner.retry_configurations().is_empty());
        assert!(endpoint.configured().is_none());
        owner
            .configure("owned-edit-fixture", standard.clone())
            .unwrap();
        assert_eq!(endpoint.configured(), Some(standard));
    }

    #[test]
    fn document_replay_retries_every_rejected_queue_submission() {
        let (mut editor, buffer, document, mut owner) = fixture();
        let endpoint = owner.service.replace_edit_source_fixture(
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
        );
        let mut state = owner.sessions.remove("owned").unwrap();
        for index in 0..70 {
            let path = std::path::PathBuf::from(format!("/fixture/{index}.rs"));
            let (id, _) = editor
                .open_text(TextBuffer::from_text("fn f() {}"), Some(path.clone()))
                .unwrap();
            state.documents.insert(
                id,
                LspDocument {
                    path,
                    language_id: "rust".into(),
                    version: 0,
                    text: "fn f() {}".into(),
                },
            );
        }
        let count = state.documents.len();
        owner.sessions.insert("owned-edit-fixture".into(), state);
        endpoint.block_responses();
        owner.retry_documents(&editor);
        assert!(owner.synced.is_empty());
        endpoint.unblock_responses();
        let mut accepted = std::collections::HashSet::new();
        for _ in 0..count {
            owner.retry_documents(&editor);
            assert!(accepted.insert(endpoint.take_synced_source().unwrap()));
        }
        assert_eq!(accepted.len(), count);
        owner.retry_documents(&editor);
        assert!(endpoint.take_synced_source().is_none());
    }
    #[test]
    fn document_replay_synchronizes_edited_inactive_buffers() {
        let (mut editor, buffer, document, mut owner) = fixture();
        let endpoint = owner.service.replace_edit_source_fixture(
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
        );
        let session = owner.sessions.remove("owned").unwrap();
        owner.sessions.insert("owned-edit-fixture".into(), session);
        editor
            .open_text(
                TextBuffer::from_text("other"),
                Some("/fixture/other.rs".into()),
            )
            .unwrap();
        owner.retry_documents(&editor);
        assert!(endpoint.take_synced_source().is_some());
        editor.buffers[buffer]
            .text
            .restore_text("edited inactive text")
            .unwrap();
        owner.retry_documents(&editor);
        assert_eq!(endpoint.take_synced_source(), Some(buffer));
        assert!(owner.synced(
            "owned-edit-fixture",
            buffer,
            &editor.buffers[buffer].text.source_version()
        ));
        assert_eq!(
            owner.sessions["owned-edit-fixture"].documents[&buffer].text,
            "edited inactive text"
        );
        owner.retry_documents(&editor);
        assert!(endpoint.take_synced_source().is_none());
    }
    #[test]
    fn document_replay_closes_missing_and_renamed_buffers_even_when_stopped() {
        let (mut editor, buffer, document, mut owner) = fixture();
        let (renamed, _) = editor
            .open_text(
                TextBuffer::from_text("renamed"),
                Some("/fixture/old.rs".into()),
            )
            .unwrap();
        let (kept, _) = editor
            .open_text(
                TextBuffer::from_text("kept"),
                Some("/fixture/kept.rs".into()),
            )
            .unwrap();
        for id in [buffer, renamed, kept] {
            let mut registered = document.clone();
            registered.path = editor.buffers[id].path.clone().unwrap();
            owner
                .sessions
                .get_mut("owned")
                .unwrap()
                .documents
                .insert(id, registered);
            owner.synced.insert(
                ("owned".into(), id),
                editor.buffers[id].text.source_version(),
            );
        }
        editor.buffers.remove(buffer);
        editor.buffers[renamed].path = Some("/fixture/new.rs".into());
        owner.sessions.get_mut("owned").unwrap().capabilities = None;
        owner.retry_documents(&editor);
        assert_eq!(
            owner.sessions["owned"]
                .documents
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![kept]
        );
        assert_eq!(owner.synced.len(), 1);
        assert!(owner.synced.contains_key(&("owned".into(), kept)));
    }

    #[test]
    fn document_close_retries_a_busy_close_before_forgetting_the_registration() {
        let (mut editor, buffer, document, mut owner) = fixture();
        let endpoint = owner.service.replace_edit_source_fixture_for(
            "owned",
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
            1,
        );
        owner.retry_documents(&editor);
        assert_eq!(endpoint.take_synced_source(), Some(buffer));
        editor.buffers.remove(buffer);
        endpoint.block_responses();
        owner.retry_documents(&editor);
        assert!(owner.sessions["owned"].documents.contains_key(&buffer));
        endpoint.unblock_responses();
        owner.retry_documents(&editor);
        assert_eq!(endpoint.closed_uri(), Some(document.uri()));
        assert!(owner.sessions["owned"].documents.is_empty());
        assert!(owner.synced.is_empty());
    }

    #[test]
    fn document_replay_synchronizes_changed_inactive_buffers() {
        let (mut editor, buffer, document, mut owner) = fixture();
        let endpoint = owner.service.replace_edit_source_fixture_for(
            "owned",
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
            1,
        );
        owner.retry_documents(&editor);
        assert_eq!(endpoint.take_synced_source(), Some(buffer));
        let old_version = editor.buffers[buffer].text.source_version();
        editor
            .open_text(
                TextBuffer::from_text("active"),
                Some("/fixture/active.rs".into()),
            )
            .unwrap();
        editor.buffers[buffer]
            .text
            .restore_text("new contents")
            .unwrap();
        let new_version = editor.buffers[buffer].text.source_version();
        assert_ne!(old_version, new_version);
        endpoint.block_responses();
        owner.retry_documents(&editor);
        assert!(owner.synced("owned", buffer, &old_version));
        endpoint.unblock_responses();
        owner.retry_documents(&editor);
        assert_eq!(endpoint.take_synced_source(), Some(buffer));
        assert!(owner.synced("owned", buffer, &new_version));
        assert_eq!(
            owner.sessions["owned"].documents[&buffer].text,
            "new contents"
        );
        owner.retry_documents(&editor);
        assert!(endpoint.take_synced_source().is_none());
    }

    #[test]
    fn formatting_server_ignores_dynamic_providers_for_other_documents() {
        let (_, buffer, document, mut owner) = fixture();
        let capabilities = owner
            .sessions
            .get_mut("owned")
            .unwrap()
            .capabilities
            .as_mut()
            .unwrap();
        capabilities.formatting = true;
        capabilities.document_selectors.insert(
            "textDocument/formatting".into(),
            vec![serde_json::json!({
                "documentSelector":[{"language":"javascript"}]
            })],
        );
        assert!(owner.formatting_server(buffer, &document).is_none());
        owner
            .sessions
            .get_mut("owned")
            .unwrap()
            .capabilities
            .as_mut()
            .unwrap()
            .document_selectors
            .get_mut("textDocument/formatting")
            .unwrap()
            .push(serde_json::json!({"documentSelector":[{"language":"rust"}]}));
        assert_eq!(
            owner.formatting_server(buffer, &document).as_deref(),
            Some("owned")
        );
    }

    #[test]
    fn code_action_ranges_are_encoded_for_each_receiving_server() {
        let (_, buffer, mut document, mut owner) = fixture();
        document.text = "😀x".into();
        let mut second = owner.sessions["owned"].clone();
        second.capabilities.as_mut().unwrap().position_encoding =
            crate::editor::text::PositionEncoding::Utf8;
        owner.sessions.insert("second".into(), second);
        let range = crate::language::lsp::LspRange {
            start: crate::language::lsp::LspPosition {
                line: 0,
                character: 2,
            },
            end: crate::language::lsp::LspPosition {
                line: 0,
                character: 3,
            },
        };
        let diagnostic = crate::language::lsp::LspDiagnostic {
            range: range.clone(),
            severity: Some(1),
            source: None,
            code: None,
            related: Vec::new(),
            message: "problem".into(),
            data: None,
        };
        let feature = crate::language::lsp::LspFeature::CodeAction {
            range,
            diagnostics: vec![diagnostic],
        };
        let crate::language::lsp::LspFeature::CodeAction { range, diagnostics } = owner
            .feature_for_server(&feature, &document, "owned", "second")
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(range.start.character, 4);
        assert_eq!(range.end.character, 5);
        assert_eq!(diagnostics[0].range, range);
        assert!(owner.sessions["second"].documents.contains_key(&buffer));
    }
    #[test]
    fn edited_completion_ranges_are_invalidated_before_reuse() {
        let (mut editor, buffer, mut document, mut owner) = fixture();
        let mut completion = item("foooX");
        completion.text_edit = Some(crate::language::lsp::LspTextEdit {
            range: crate::language::lsp::LspRange {
                start: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 0,
                },
                end: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 4,
                },
            },
            new_text: "foooX".into(),
        });
        owner.install_completions("owned", vec![completion], false, 0);
        owner.begin_completion(Some(ByteOffset(1)), true);
        editor.buffers[buffer]
            .text
            .restore_text("foo tail")
            .unwrap();
        document.version = editor.buffers[buffer].text.revision().0;
        document.text = "foo tail".into();
        assert_eq!(
            owner.refresh_projection(&editor, Some((buffer, &document))),
            crate::app::language::projection::ProjectionChange::Typed
        );
        assert!(owner.completion_items().is_empty());
        assert!(
            owner.completion_incomplete(),
            "edited ranges require a fresh request"
        );
        assert_eq!(owner.completion_start(), Some(ByteOffset(1)));
        assert!(owner.completion_automatic());
    }
    #[test]
    fn reloaded_text_clears_projection_even_at_the_same_revision() {
        let (mut editor, buffer, document, mut owner) = fixture();
        let old = owner.projection.owner.clone().unwrap();
        owner.projection.request = Some((0, old));
        owner.projection.request_servers = vec!["owned".into()];
        owner.install_completions("owned", vec![item("old")], false, 0);
        editor.buffers[buffer].text = TextBuffer::from_text("other");
        assert_eq!(
            owner.refresh_projection(&editor, Some((buffer, &document))),
            crate::app::language::projection::ProjectionChange::Moved
        );
        assert!(!owner.accept_result(
            "owned",
            0,
            "textDocument/hover",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert!(owner.completion_items().is_empty());
    }
    #[test]
    fn dismissed_completion_cancels_every_pending_server() {
        let (_, _, document, mut owner) = fixture();
        owner.projection.request_id = 3;
        owner.projection.request = Some((3, owner.projection.owner.clone().unwrap()));
        owner.projection.request_servers = vec!["owned".into(), "second".into()];
        owner.install_completions("owned", vec![item("first")], false, 3);
        owner.clear_completion();
        assert!(!owner.accept_result(
            "second",
            3,
            "textDocument/completion",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
    }
    #[test]
    fn completion_resolve_survives_an_unrelated_request() {
        let (editor, buffer, document, mut owner) = fixture();
        let _endpoint = owner.service.replace_edit_source_fixture_for(
            "owned",
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
            4,
        );
        let mut unresolved = item("foo");
        unresolved.resolve_item = Some(serde_json::json!({"label": "foo"}));
        unresolved.resolved = false;
        owner.install_completions("owned", vec![unresolved], false, 0);
        owner
            .resolve_completion(0, document.clone(), ByteOffset(0), true)
            .unwrap();
        let request_id = owner.request_id();
        assert!(owner.projection.requests.contains_key(&request_id));
        owner.next_request_id().unwrap();
        let source = owner.projection.owner.clone().unwrap();
        owner.projection.request = Some((owner.request_id(), source));
        owner.projection.request_servers = vec!["owned".into()];
        assert!(owner.accept_result(
            "owned",
            request_id,
            "completionItem/resolve",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert!(owner.accept_resolved(request_id, item("resolved foo"), &editor));
        assert_eq!(owner.completion_items()[0].label, "resolved foo");
    }
    #[test]
    fn completion_preview_resolves_once_and_enter_reuses_the_pending_request() {
        let (editor, buffer, document, mut owner) = fixture();
        let _endpoint = owner.service.replace_edit_source_fixture_for(
            "owned",
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
            4,
        );
        let mut unresolved = item("foo");
        unresolved.resolve_item = Some(serde_json::json!({"label": "foo"}));
        unresolved.resolved = false;
        owner.install_completions("owned", vec![unresolved], false, 0);
        owner
            .resolve_completion(0, document.clone(), ByteOffset(0), false)
            .unwrap();
        let id = owner.request_id();
        assert!(!owner.completion_resolve_accepts(id));
        owner
            .resolve_completion(0, document.clone(), ByteOffset(0), false)
            .unwrap();
        assert_eq!(owner.request_id(), id);
        owner
            .resolve_completion(0, document, ByteOffset(0), true)
            .unwrap();
        assert_eq!(owner.request_id(), id);
        assert!(owner.completion_resolve_accepts(id));
        assert!(!owner.completion_resolve_accepts(id + 1));
        assert!(owner.accept_resolved(id, item("resolved foo"), &editor));
        assert_eq!(owner.completion_items()[0].label, "resolved foo");
        assert!(!owner.completion_resolve_accepts(id));
    }

    #[test]
    fn retiring_a_secondary_server_removes_its_items_and_resolve_data() {
        let (editor, _, document, mut owner) = fixture();
        let source = owner.projection.owner.clone().unwrap();
        owner.install_completions("second", vec![item("stale")], false, 0);
        owner.install_completions("owned", vec![item("kept")], false, 0);
        owner.move_completion(1, 10);
        owner.projection.resolve = Some((1, source.clone(), 1, ByteOffset(0)));
        owner.projection.requests.insert(
            1,
            (
                source.clone(),
                vec!["owned".into()],
                "completionItem/resolve".into(),
            ),
        );
        owner.projection.requests.insert(
            2,
            (
                source.clone(),
                vec!["second".into()],
                "textDocument/completion".into(),
            ),
        );
        owner.projection.completion_narrowed =
            Some((ByteOffset(0), editor.buffers[source.buffer].text.revision()));
        owner.stopped("second");
        assert!(owner.completion_needs_narrowing(
            ByteOffset(0),
            editor.buffers[source.buffer].text.revision()
        ));
        assert_eq!(owner.completion_items(), &[item("kept")]);
        assert_eq!(
            owner.projection.completion_all,
            vec![("owned".into(), item("kept"))]
        );
        assert!(!owner.accept_result(
            "second",
            2,
            "textDocument/completion",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert_eq!(owner.completion_position().0, 0);
        assert!(owner.accept_resolved(1, item("resolved kept"), &editor));

        owner.install_completions("second", vec![item("new process")], false, 0);
        owner.move_completion(1, 10);
        owner.projection.resolve = Some((3, source.clone(), 1, ByteOffset(0)));
        owner.projection.requests.insert(
            3,
            (
                source,
                vec!["second".into()],
                "completionItem/resolve".into(),
            ),
        );
        owner.stopped("second");
        assert!(owner.projection.resolve.is_none());
        assert!(!owner.accept_result(
            "second",
            3,
            "completionItem/resolve",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert!(!owner.accept_resolved(3, item("stale resolve"), &editor));
        assert_eq!(owner.completion_server(0), Some("owned"));
    }
    #[test]
    fn narrowing_keeps_a_resolve_on_its_original_item() {
        let (editor, _, _, mut owner) = fixture();
        owner.projection.request_id = 3;
        owner.install_completions("owned", vec![item("abX"), item("aXb")], false, 3);
        owner.projection.resolve =
            Some((3, owner.projection.owner.clone().unwrap(), 0, ByteOffset(0)));
        owner.narrow_completions(
            vec![Some(1), Some(2)],
            ByteOffset(1),
            crate::editor::text::BufferRevision(0),
        );
        assert_eq!(owner.completion_position().0, 1);
        assert!(owner.accept_resolved(3, item("resolved abX"), &editor));
        assert_eq!(owner.completion_items()[0].label, "aXb");
        assert_eq!(owner.completion_items()[1].label, "resolved abX");
    }
    #[test]
    fn narrowing_cannot_resolve_into_a_different_row() {
        let (editor, _, _, mut owner) = fixture();
        owner.projection.request_id = 3;
        owner.install_completions("owned", vec![item("abX"), item("aXb")], false, 3);
        owner.projection.resolve =
            Some((3, owner.projection.owner.clone().unwrap(), 0, ByteOffset(0)));
        owner.narrow_completions(
            vec![Some(1), Some(2)],
            ByteOffset(1),
            crate::editor::text::BufferRevision(0),
        );
        // Resolves now follow the original item through reordering, never the old row index.
        assert!(owner.accept_resolved(3, item("resolved abX"), &editor));
        assert_eq!(owner.completion_items()[0].label, "aXb");
        assert_eq!(owner.completion_items()[1].label, "resolved abX");
    }
    #[test]
    fn narrowing_rejects_a_resolve_when_its_item_is_filtered_out() {
        let (editor, _, _, mut owner) = fixture();
        owner.projection.request_id = 3;
        owner.install_completions("owned", vec![item("abX"), item("aXb")], false, 3);
        owner.projection.resolve =
            Some((3, owner.projection.owner.clone().unwrap(), 0, ByteOffset(0)));
        owner.narrow_completions(
            vec![None, Some(2)],
            ByteOffset(0),
            crate::editor::text::BufferRevision(0),
        );
        assert!(!owner.accept_resolved(3, item("resolved abX"), &editor));
        assert_eq!(owner.completion_items()[0].label, "aXb");
    }
    #[test]
    fn narrowing_preserves_the_server_and_item_not_just_its_label() {
        let (_, _, _, mut owner) = fixture();
        owner.install_completions("owned", vec![item("foo"), item("foo")], false, 0);
        let mut overload = item("foo");
        overload.insert_text = "foo(value)".into();
        owner.install_completions("second", vec![overload.clone()], false, 0);
        owner.move_completion(1, 10);
        owner.move_completion(1, 10);
        owner.install_completions("third", vec![item("aaa")], false, 0);
        owner.narrow_completions(
            vec![Some(1); 4],
            ByteOffset(0),
            crate::editor::text::BufferRevision(0),
        );
        let selected = owner.completion_position().0;
        assert_eq!(owner.completion_server(selected), Some("second"));
        assert_eq!(owner.completion_items()[selected], overload);
    }
    #[test]
    fn cancelling_completion_keeps_an_unrelated_format_request() {
        let (_, _, document, mut owner) = fixture();
        let source = owner.projection.owner.clone().unwrap();
        owner.install_completions("owned", vec![item("foo")], false, 1);
        owner.projection.requests.insert(
            1,
            (
                source.clone(),
                vec!["second".into()],
                "textDocument/completion".into(),
            ),
        );
        owner.projection.request_id = 2;
        owner.projection.request = Some((2, source.clone()));
        owner.projection.request_servers = vec!["owned".into()];
        owner.projection.requests.insert(
            2,
            (
                source,
                vec!["owned".into()],
                "textDocument/formatting".into(),
            ),
        );
        owner.cancel_completion();
        assert!(owner.completion_items().is_empty());
        assert!(!owner.accept_result(
            "second",
            1,
            "textDocument/completion",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert!(owner.request_pending(2));
        assert!(owner.accept_result(
            "owned",
            2,
            "textDocument/formatting",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
    }
    #[test]
    fn pending_servers_belong_to_the_result_request_id() {
        let (_, _, document, mut owner) = fixture();
        let source = owner.projection.owner.clone().unwrap();
        owner.projection.requests.insert(
            1,
            (
                source.clone(),
                vec!["first".into(), "second".into()],
                "textDocument/codeAction".into(),
            ),
        );
        owner.projection.requests.insert(
            2,
            (
                source.clone(),
                vec!["owned".into()],
                "textDocument/hover".into(),
            ),
        );
        owner.projection.request_id = 2;
        owner.projection.request = Some((2, source));
        owner.projection.request_servers = vec!["owned".into()];
        assert!(owner.accept_result(
            "first",
            1,
            "textDocument/codeAction",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert!(owner.request_pending(1));
        assert!(owner.accept_result(
            "second",
            1,
            "textDocument/codeAction",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert!(!owner.request_pending(1));
        assert!(owner.request_pending(2));
    }
    #[test]
    fn formatting_and_hover_keep_independent_request_lifetimes() {
        let (_, _, document, mut owner) = fixture();
        let source = owner.projection.owner.clone().unwrap();
        owner.projection.requests.insert(
            1,
            (
                source.clone(),
                vec!["owned".into()],
                "textDocument/formatting".into(),
            ),
        );
        owner.projection.requests.insert(
            2,
            (source, vec!["owned".into()], "textDocument/hover".into()),
        );
        owner.projection.request_id = 2;
        assert!(owner.accept_result(
            "owned",
            2,
            "textDocument/hover",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
        assert!(owner.accept_result(
            "owned",
            1,
            "textDocument/formatting",
            &document.uri(),
            0,
            ByteOffset(0)
        ));
    }
    #[test]
    fn semantic_splices_use_the_original_array_and_are_cleared_with_the_server() {
        let mut owner = LspSession::default();
        let full = LspSemanticTokens {
            result_id: Some("old".into()),
            data: (0..15).collect(),
        };
        owner.record_semantic("owned", "uri", &full);
        let result = owner
            .semantic_delta(
                "owned",
                "uri",
                LspSemanticTokenDelta {
                    result_id: Some("new".into()),
                    edits: vec![
                        LspSemanticTokenEdit {
                            start: 0,
                            delete_count: 0,
                            data: vec![99; 5],
                        },
                        LspSemanticTokenEdit {
                            start: 10,
                            delete_count: 5,
                            data: vec![],
                        },
                    ],
                },
            )
            .unwrap();
        assert_eq!(result.data, [vec![99; 5], (0..10).collect()].concat());
        owner.remove("owned");
        assert!(owner.projection.semantic.is_empty());
    }
    #[test]
    fn detaching_a_document_clears_its_pull_and_semantic_result_ids() {
        let (_, buffer, document, mut owner) = fixture();
        let uri = document.uri();
        owner.record_pull("owned", &uri, "old-pull".into());
        owner.record_pull("owned", "another-uri", "keep-pull".into());
        let tokens = LspSemanticTokens {
            result_id: Some("old-semantic".into()),
            data: vec![0; 5],
        };
        assert!(owner.record_semantic("owned", &uri, &tokens));
        assert!(owner.record_semantic("owned", "another-uri", &tokens));
        owner.detach("owned", buffer);
        assert!(
            !owner
                .projection
                .pull_ids
                .contains_key(&("owned".into(), uri.clone()))
        );
        assert!(
            !owner
                .projection
                .semantic
                .contains_key(&("owned".into(), uri))
        );
        assert_eq!(owner.projection.pull_ids.len(), 1);
        assert_eq!(owner.projection.semantic.len(), 1);
    }
    #[test]
    fn starting_a_replacement_transport_clears_old_result_ids() {
        use crate::language::lsp::LspPolicy;
        use crate::language::lsp::LspServerConfiguration;
        use crate::language::lsp::LspSessionLaunch;
        let root = tempfile::tempdir().unwrap();
        let (mut editor, buffer, mut document, mut owner) = fixture();
        document.path = root.path().join("main.rs");
        editor.buffers[buffer].path = Some(document.path.clone());
        let definition = owner.sessions["owned"].definition.clone();
        let executable = std::env::current_exe().unwrap();
        let launch = crate::app::language::LspLaunch::Direct {
            executable: executable.clone(),
            managed: false,
        };
        owner.record_semantic(
            "owned",
            &document.uri(),
            &LspSemanticTokens {
                result_id: Some("old-process".into()),
                data: vec![0, 0, 1, 0, 0],
            },
        );
        owner.record_pull("owned", &document.uri(), "old-process".into());
        owner
            .start(
                LspSessionLaunch {
                    session_key: "owned",
                    definition: &definition,
                    executable,
                    arguments: vec!["--help".into()],
                    root: root.path().into(),
                    document: Some(document),
                    policy: LspPolicy::default(),
                    configuration: LspServerConfiguration::standard(),
                    environment: Default::default(),
                },
                (buffer, editor.buffers[buffer].text.source_version()),
                (crate::workspace::RootId(1), launch, true),
                &editor,
            )
            .unwrap();
        assert!(owner.projection.semantic.is_empty());
        assert!(owner.projection.pull_ids.is_empty());
        owner.service.stop("owned").unwrap();
    }
    #[test]
    fn breadcrumb_failures_allow_only_bounded_retries() {
        let (_, buffer, mut document, mut owner) = fixture();
        owner.projection.breadcrumbs_seen =
            Some(((buffer, 0), Instant::now() - Duration::from_secs(1)));
        owner
            .sessions
            .get_mut("owned")
            .unwrap()
            .capabilities
            .as_mut()
            .unwrap()
            .document_symbols = true;
        owner.projection.breadcrumbs_asked = Some((buffer, 0));
        owner.retry_breadcrumbs();
        assert_eq!(
            owner.request_breadcrumbs(buffer, &document, Instant::now() + Duration::from_secs(1)),
            Some("owned".into())
        );
        for _ in 0..2 {
            owner.retry_breadcrumbs();
        }
        assert!(owner.projection.breadcrumbs_asked.is_none());
        assert!(
            owner
                .request_breadcrumbs(buffer, &document, Instant::now() + Duration::from_secs(1))
                .is_none()
        );
        document.version += 1;
        let now = Instant::now();
        assert!(owner.request_breadcrumbs(buffer, &document, now).is_none());
        assert_eq!(owner.projection.breadcrumb_retries, 0);
        assert_eq!(
            owner.request_breadcrumbs(buffer, &document, now + Duration::from_secs(1)),
            Some("owned".into())
        );
    }
    #[test]
    fn inlay_refresh_retries_after_a_full_command_queue() {
        let (editor, buffer, document, mut owner) = fixture();
        owner
            .sessions
            .get_mut("owned")
            .unwrap()
            .capabilities
            .as_mut()
            .unwrap()
            .inlay_hints = true;
        let endpoint = owner.service.replace_edit_source_fixture_for(
            "owned",
            document.uri(),
            (buffer, editor.buffers[buffer].text.source_version()),
            crate::editor::text::PositionEncoding::Utf16,
            1,
        );
        endpoint.block_responses();
        owner.request_inlays(document.clone());
        assert!(owner.projection.inlay_request.is_none());
        assert!(owner.inlay_pending());
        endpoint.unblock_responses();
        let config = crate::config::load_bundled_config().config.lsp;
        assert!(owner.inlay_ready(&config, Instant::now() + Duration::from_secs(1)));
        owner.request_inlays(document);
        assert!(owner.projection.inlay_request.is_some());
        assert!(!owner.inlay_pending());
    }
    #[test]
    fn completion_from_every_server_lands_in_one_list() {
        let root = tempfile::tempdir().unwrap();
        let mut editor = EditorState::new(root.path().into());
        let (buffer, view) = editor
            .open_text(
                TextBuffer::from_text("<div class=\"\">"),
                Some(root.path().join("index.html")),
            )
            .unwrap();
        let source = LspProjectionOwner {
            server: "html".into(),
            buffer,
            view,
            revision: editor.buffers[buffer].text.revision(),
            text: editor.buffers[buffer].text.source_version(),
            uri: "file:///index.html".into(),
            language: "html".into(),
        };
        let mut owner = LspSession::default();
        owner.projection.owner = Some(source.clone());
        owner.projection.request_id = 3;
        owner.projection.request = Some((3, source.clone()));
        owner.projection.request_servers = vec!["html".into(), "tailwind".into()];
        let accept = |owner: &mut LspSession, server: &str| {
            owner.accept_result(
                server,
                3,
                "textDocument/completion",
                &source.uri,
                source.revision.0,
                ByteOffset(0),
            )
        };
        assert!(accept(&mut owner, "tailwind"));
        owner.install_completions("tailwind", vec![item("bg-red-500")], false, 3);
        assert!(owner.request_pending(3));
        assert!(accept(&mut owner, "html"));
        owner.install_completions("html", vec![item("class")], false, 3);
        assert!(!owner.request_pending(3));
        assert!(!accept(&mut owner, "html"));
        let labels = owner
            .completion_items()
            .iter()
            .map(|i| i.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["bg-red-500", "class"]);
        assert_eq!(owner.completion_server(0), Some("tailwind"));
        assert_eq!(owner.completion_server(1), Some("html"));
        // A new request starts a fresh list.
        owner.install_completions("html", vec![item("id")], false, 4);
        assert_eq!(owner.completion_items().len(), 1);
        assert_eq!(owner.completion_server(0), Some("html"));
    }
    #[test]
    fn projection_resolve_identity_activation_and_delta_bounds() {
        let root = tempfile::tempdir().unwrap();
        let mut editor = EditorState::new(root.path().into());
        let (buffer, view) = editor
            .open_text(
                TextBuffer::from_text("ab"),
                Some(root.path().join("source.rs")),
            )
            .unwrap();
        let source = LspProjectionOwner {
            server: "owned".into(),
            buffer,
            view,
            revision: editor.buffers[buffer].text.revision(),
            text: editor.buffers[buffer].text.source_version(),
            uri: "file:///owned/source.rs".into(),
            language: "rust".into(),
        };
        let mut owner = LspSession::default();
        owner.projection.owner = Some(source.clone());
        owner.projection.request_id = 7;
        owner.install_completions("owned", vec![item("first"), item("second")], false, 7);
        owner.projection.request = Some((7, source.clone()));
        owner.projection.request_servers = vec!["owned".into()];
        owner.projection.resolve = Some((7, source.clone(), 0, ByteOffset(0)));
        assert!(!owner.accept_result(
            "foreign",
            7,
            "completionItem/resolve",
            &source.uri,
            source.revision.0,
            ByteOffset(0)
        ));
        assert!(owner.projection.request.is_some());
        assert!(owner.accept_result(
            "owned",
            7,
            "completionItem/resolve",
            &source.uri,
            source.revision.0,
            ByteOffset(0)
        ));
        assert!(!owner.accept_result(
            "owned",
            7,
            "completionItem/resolve",
            &source.uri,
            source.revision.0,
            ByteOffset(0)
        ));
        owner.move_completion(1, 10);
        assert!(!owner.accept_resolved(6, item("stale"), &editor));
        assert!(owner.projection.resolve.is_some());
        assert!(!owner.accept_resolved(7, item("resolved first"), &editor));
        assert_eq!(owner.completion_items()[0].label, "resolved first");
        assert_eq!(owner.completion_items()[1].label, "second");
        assert!(!owner.accept_resolved(7, item("duplicate"), &editor));
        owner.move_completion(-1, 10);
        owner.projection.request_id = 8;
        owner.projection.resolve = Some((8, source.clone(), 0, ByteOffset(0)));
        editor.views[view].cursor = ByteOffset(1);
        assert!(!owner.accept_resolved(8, item("moved cursor"), &editor));
        editor.views[view].cursor = ByteOffset(0);
        owner.projection.request_id = 9;
        owner.projection.resolve = Some((9, source.clone(), 0, ByteOffset(0)));
        assert!(owner.accept_resolved(9, item("original action"), &editor));
        owner.projection.resolve = Some((9, source, 0, ByteOffset(0)));
        owner.clear_completion();
        assert!(!owner.accept_resolved(9, item("cancelled"), &editor));
        owner.projection.request_id = u64::MAX / 2 - 1;
        assert!(owner.next_request_id().is_none());
        assert_eq!(owner.request_id(), u64::MAX / 2 - 1);
        let full = LspSemanticTokens {
            result_id: Some("base".into()),
            data: vec![0, 0, 1, 0, 0],
        };
        assert!(owner.record_semantic("owned", "uri", &full));
        assert!(
            owner
                .semantic_delta(
                    "owned",
                    "uri",
                    LspSemanticTokenDelta {
                        result_id: Some("bad".into()),
                        edits: vec![
                            LspSemanticTokenEdit {
                                start: 2,
                                delete_count: 1,
                                data: vec![2]
                            },
                            LspSemanticTokenEdit {
                                start: usize::MAX,
                                delete_count: 1,
                                data: vec![]
                            }
                        ]
                    }
                )
                .is_none()
        );
        assert!(
            !owner
                .projection
                .semantic
                .contains_key(&("owned".into(), "uri".into()))
        );
        assert!(owner.record_semantic("owned", "uri", &full));
        let delta = owner
            .semantic_delta(
                "owned",
                "uri",
                LspSemanticTokenDelta {
                    result_id: Some("next".into()),
                    edits: vec![LspSemanticTokenEdit {
                        start: 2,
                        delete_count: 1,
                        data: vec![2],
                    }],
                },
            )
            .unwrap();
        assert_eq!(delta.data, vec![0, 0, 2, 0, 0]);
        assert!(owner.record_semantic("owned", "uri", &delta));
        assert!(!owner.record_semantic(
            "owned",
            "uri",
            &LspSemanticTokens {
                result_id: None,
                data: vec![1, 2, 3]
            }
        ));
    }
    #[test]
    fn completion_items_and_resolves_reject_a_reloaded_text_instance() {
        let (mut editor, buffer, view) = {
            let mut editor = EditorState::new("/workspace".into());
            let (buffer, view) = editor
                .open_text(TextBuffer::from_text("old"), None)
                .unwrap();
            (editor, buffer, view)
        };
        let source = LspProjectionOwner {
            server: "server".into(),
            buffer,
            view,
            revision: editor.buffers[buffer].text.revision(),
            text: editor.buffers[buffer].text.source_version(),
            uri: "file:///workspace/test".into(),
            language: "text".into(),
        };
        let mut session = LspSession::default();
        session.projection.owner = Some(source.clone());
        session.projection.request_id = 1;
        session.install_completions("server", vec![item("old")], false, 1);
        assert!(session.completion_current(&editor));
        session.projection.resolve = Some((1, source, 0, ByteOffset(0)));
        editor.buffers[buffer].text = TextBuffer::from_text("new");
        assert!(!session.completion_current(&editor));
        assert!(!session.accept_resolved(1, item("old resolved"), &editor));
    }

    #[test]
    fn stopping_a_nonactive_server_clears_its_delta_caches() {
        let mut session = LspSession::default();
        session.record_pull("old", "uri", "previous".into());
        session.record_semantic(
            "old",
            "uri",
            &LspSemanticTokens {
                result_id: Some("previous".into()),
                data: vec![0, 0, 1, 0, 0],
            },
        );
        session.stopped("old");
        assert!(session.projection.pull_ids.is_empty());
        assert!(session.projection.semantic.is_empty());
    }

    #[test]
    fn retiring_a_save_format_request_rejects_its_late_result() {
        let mut editor = EditorState::new("/workspace".into());
        let (buffer, view) = editor
            .open_text(TextBuffer::from_text("text"), None)
            .unwrap();
        let source = LspProjectionOwner {
            server: "formatter".into(),
            buffer,
            view,
            revision: editor.buffers[buffer].text.revision(),
            text: editor.buffers[buffer].text.source_version(),
            uri: "uri".into(),
            language: "text".into(),
        };
        let mut session = LspSession::default();
        session.projection.owner = Some(source.clone());
        session.projection.request_id = 1;
        session.projection.request = Some((1, source.clone()));
        session.projection.request_servers = vec!["formatter".into()];
        session.projection.requests.insert(
            1,
            (
                source.clone(),
                vec!["formatter".into()],
                "textDocument/formatting".into(),
            ),
        );
        session.finish_request("formatter", 1);
        assert!(!session.accept_result(
            "formatter",
            1,
            "textDocument/formatting",
            "uri",
            0,
            ByteOffset(0)
        ));
        session.projection.requests.insert(
            1,
            (
                source.clone(),
                vec!["formatter".into()],
                "textDocument/formatting".into(),
            ),
        );
        session.projection.request_id = 2;
        session.projection.request = Some((2, source.clone()));
        session.projection.request_servers = vec!["hover".into()];
        session.projection.requests.insert(
            2,
            (source, vec!["hover".into()], "textDocument/hover".into()),
        );
        session.finish_request("formatter", 1);
        assert!(!session.accept_result(
            "formatter",
            1,
            "textDocument/formatting",
            "uri",
            0,
            ByteOffset(0)
        ));
        assert!(session.accept_result("hover", 2, "textDocument/hover", "uri", 0, ByteOffset(0)));
    }
}

/// Scattered letters only help when nothing holds the typed text in one piece: `gret` finds
/// `greeting`, and once `greet` does, `SVGGraphicsElement` stops being listed under it.
fn hide_scattered_matches(matches: Vec<(Option<i64>, bool)>) -> Vec<Option<i64>> {
    let any_whole = matches
        .iter()
        .any(|(score, whole)| score.is_some() && *whole);
    matches
        .into_iter()
        .map(|(score, whole)| score.filter(|_| whole || !any_whole))
        .collect()
}

/// Whether the typed letters can match from the start of a word in the item, as VS Code asks of
/// completions: `greet` finds `greetingResult` and not the scattered letters of `ProgressEvent`.
fn starts_at_a_word(query: &str, target: &str) -> bool {
    let mut letters = query.chars().flat_map(char::to_lowercase);
    let Some(first) = letters.next() else {
        return true;
    };
    let rest = letters.collect::<Vec<_>>();
    let chars = target.chars().collect::<Vec<_>>();
    (0..chars.len()).any(|index| {
        let word_start = index == 0 || {
            let previous = chars[index - 1];
            let next_is_lower = chars.get(index + 1).is_some_and(|next| next.is_lowercase());
            !previous.is_alphanumeric()
                || (chars[index].is_uppercase() && (previous.is_lowercase() || next_is_lower))
        };
        let mut remaining = chars[index..].iter().flat_map(|c| c.to_lowercase());
        word_start && remaining.next() == Some(first) && {
            rest.iter().all(|letter| remaining.any(|c| c == *letter))
        }
    })
}

#[cfg(test)]
mod completion_filter_tests {
    use crate::app::language::projection::hide_scattered_matches;
    use crate::app::language::projection::starts_at_a_word;

    #[test]
    fn scattered_matches_show_only_when_nothing_matches_in_one_piece() {
        assert_eq!(
            hide_scattered_matches(vec![(Some(9), true), (Some(5), false), (None, false)]),
            vec![Some(9), None, None]
        );
        assert_eq!(
            hide_scattered_matches(vec![(Some(5), false), (None, true)]),
            vec![Some(5), None]
        );
    }

    #[test]
    fn typed_letters_must_start_at_a_word_of_the_item() {
        for (query, target) in [
            ("greet", "greeting"),
            ("greet", "greetingResult"),
            ("res", "greetingResult"),
            ("str", "toString"),
            ("bar", "foo_bar"),
            ("el", "SVGRectElement"),
            ("İ", "İtem"),
            ("İt", "İtem"),
            ("İ", "some_İtem"),
            ("", "anything"),
        ] {
            assert!(starts_at_a_word(query, target), "{query} {target}");
        }
        for (query, target) in [
            ("greet", "SVGRectElement"),
            ("greet", "ProgressEvent"),
            ("greet", "HTMLProgressElement"),
            ("ing", "toString"),
            ("İ", "xitem"),
        ] {
            assert!(!starts_at_a_word(query, target), "{query} {target}");
        }
    }
}
