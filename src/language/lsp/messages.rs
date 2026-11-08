//! Language-server request and response data.

use crate::language::lsp::paths::file_uri;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;

use crate::language::lsp::server::LspCapabilities;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspDocument {
    pub path: PathBuf,
    pub language_id: String,
    pub version: u64,
    pub text: String,
}

impl LspDocument {
    #[must_use]
    pub fn uri(&self) -> String {
        file_uri(&self.path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspPosition {
    pub line: usize,
    pub character: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspTextEdit {
    pub range: LspRange,
    pub new_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspDocumentEdit {
    pub uri: String,
    pub version: Option<u64>,
    pub edits: Vec<LspTextEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspResourceOperation {
    Create {
        uri: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Rename {
        old_uri: String,
        new_uri: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Delete {
        uri: String,
        recursive: bool,
        ignore_if_not_exists: bool,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspWorkspaceEdit {
    pub documents: Vec<LspDocumentEdit>,
    pub resources: Vec<LspResourceOperation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspDiagnostic {
    pub range: LspRange,
    pub severity: Option<u8>,
    pub code: Option<Value>,
    pub related: Vec<(LspLocation, String)>,
    pub source: Option<String>,
    pub message: String,
    /// Handed back unread in a code action request; gopls and Ruff need it for their fixes.
    pub data: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspMarkupKind {
    PlainText,
    Markdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspMarkupContent {
    pub kind: LspMarkupKind,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspHover {
    pub sections: Vec<LspMarkupContent>,
    pub range: Option<LspRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCompletionItem {
    pub label: String,
    /// What typing is matched against, when not the label.
    pub filter_text: Option<String>,
    /// Where the server ranks it, when not by label.
    pub sort_text: Option<String>,
    pub detail: String,
    pub insert_text: String,
    pub text_edit: Option<LspTextEdit>,
    pub additional_edits: Vec<LspTextEdit>,
    pub kind: Option<u8>,
    pub snippet: bool,
    pub documentation: Option<LspMarkupContent>,
    pub command: Option<LspCommand>,
    pub resolve_item: Option<Value>,
    pub resolved: bool,
}

impl LspCompletionItem {
    /// What the menu row has no room for: a signature that says more than the label.
    #[must_use]
    pub fn signature(&self) -> Option<&str> {
        (!self.detail.is_empty() && !self.label.starts_with(&self.detail))
            .then_some(self.detail.as_str())
    }
    /// The colour of a Color-kind item. Servers put it in the documentation or
    /// detail as `#rrggbb` or `rgb(...)`, which is what VS Code reads too.
    #[must_use]
    pub fn color(&self) -> Option<(u8, u8, u8)> {
        const COLOR_KIND: u8 = 16;
        if self.kind != Some(COLOR_KIND) {
            return None;
        }
        self.documentation
            .as_ref()
            .and_then(|doc| css_color(&doc.value))
            .or_else(|| css_color(&self.detail))
    }
}

/// The first `#rgb`, `#rrggbb` or `rgb()`/`rgba()` colour in `text`.
pub(crate) fn css_color(text: &str) -> Option<(u8, u8, u8)> {
    for (at, _) in text.match_indices('#') {
        let digits: String = text[at + 1..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        let channel = |hex: &str| u8::from_str_radix(hex, 16).ok();
        match digits.len() {
            6 | 8 => {
                return Some((
                    channel(&digits[0..2])?,
                    channel(&digits[2..4])?,
                    channel(&digits[4..6])?,
                ));
            }
            3 | 4 => {
                let short = |i: usize| channel(&digits[i..=i]).map(|v| v * 17);
                return Some((short(0)?, short(1)?, short(2)?));
            }
            _ => {}
        }
    }
    let start = text.find("rgb")?;
    let open = start + text[start..].find('(')?;
    let close = open + text[open..].find(')')?;
    let mut channels = text[open + 1..close]
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let value = if let Some(percent) = part.strip_suffix('%') {
                percent.parse::<f64>().ok()? * 2.55
            } else {
                part.parse::<f64>().ok()?
            };
            value
                .is_finite()
                .then(|| value.clamp(0.0, 255.0).round() as u8)
        });
    Some((channels.next()??, channels.next()??, channels.next()??))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSymbol {
    pub name: String,
    pub kind: Option<u8>,
    pub location: Option<LspLocation>,
    /// All of the symbol, where `location` is only its name when the server gives both.
    pub extent: Option<LspRange>,
}

/// The name LSP gives a symbol kind number.
#[must_use]
pub fn lsp_symbol_kind_name(kind: u8) -> &'static str {
    const NAMES: [&str; 26] = [
        "File",
        "Module",
        "Namespace",
        "Package",
        "Class",
        "Method",
        "Property",
        "Field",
        "Constructor",
        "Enum",
        "Interface",
        "Function",
        "Variable",
        "Constant",
        "String",
        "Number",
        "Boolean",
        "Array",
        "Object",
        "Key",
        "Null",
        "EnumMember",
        "Struct",
        "Event",
        "Operator",
        "TypeParameter",
    ];
    usize::from(kind)
        .checked_sub(1)
        .and_then(|index| NAMES.get(index))
        .copied()
        .unwrap_or("Symbol")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspInlayHint {
    pub position: LspPosition,
    pub label: String,
}

/// A colour the server found in the document, such as a CSS value or a Tailwind class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspColor {
    pub range: LspRange,
    pub rgb: (u8, u8, u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCodeLens {
    pub range: LspRange,
    pub title: String,
    pub command: Option<LspCommand>,
    pub resolve_item: Option<Value>,
}

/// Something a server runs on request, through `workspace/executeCommand`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCommand {
    pub command: String,
    pub arguments: Vec<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSemanticTokens {
    pub result_id: Option<String>,
    pub data: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSemanticTokenEdit {
    pub start: usize,
    pub delete_count: usize,
    pub data: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSemanticTokenDelta {
    pub result_id: Option<String>,
    pub edits: Vec<LspSemanticTokenEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspPullDiagnostics {
    pub result_id: Option<String>,
    pub items: Vec<LspDiagnostic>,
    pub unchanged: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub preferred: bool,
    pub edit: Option<LspWorkspaceEdit>,
    pub command: Option<LspCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspFeature {
    Hover,
    Completion,
    CompletionResolve {
        item: Value,
    },
    SignatureHelp,
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    References,
    DocumentSymbols,
    WorkspaceSymbols {
        query: String,
    },
    Rename {
        new_name: String,
    },
    Formatting {
        tab_size: usize,
        insert_spaces: bool,
    },
    RangeFormatting {
        range: LspRange,
        tab_size: usize,
        insert_spaces: bool,
    },
    CodeAction {
        range: LspRange,
        diagnostics: Vec<LspDiagnostic>,
    },
    ExecuteCommand(LspCommand),
    PullDiagnostics {
        previous_result_id: Option<String>,
    },
    SemanticTokensFull,
    SemanticTokensDelta {
        previous_result_id: String,
    },
    InlayHints {
        range: LspRange,
    },
    CodeLens,
    CodeLensResolve {
        item: Value,
    },
    DocumentHighlights,
    FoldingRanges,
    SelectionRanges,
    DocumentColors,
}

impl LspFeature {
    pub(crate) fn method(&self) -> &'static str {
        match self {
            Self::Hover => "textDocument/hover",
            Self::Completion => "textDocument/completion",
            Self::CompletionResolve { .. } => "completionItem/resolve",
            Self::SignatureHelp => "textDocument/signatureHelp",
            Self::Definition => "textDocument/definition",
            Self::Declaration => "textDocument/declaration",
            Self::TypeDefinition => "textDocument/typeDefinition",
            Self::Implementation => "textDocument/implementation",
            Self::References => "textDocument/references",
            Self::DocumentSymbols => "textDocument/documentSymbol",
            Self::WorkspaceSymbols { .. } => "workspace/symbol",
            Self::Rename { .. } => "textDocument/rename",
            Self::Formatting { .. } => "textDocument/formatting",
            Self::RangeFormatting { .. } => "textDocument/rangeFormatting",
            Self::CodeAction { .. } => "textDocument/codeAction",
            Self::ExecuteCommand(_) => "workspace/executeCommand",
            Self::PullDiagnostics { .. } => "textDocument/diagnostic",
            Self::SemanticTokensFull => "textDocument/semanticTokens/full",
            Self::SemanticTokensDelta { .. } => "textDocument/semanticTokens/full/delta",
            Self::InlayHints { .. } => "textDocument/inlayHint",
            Self::CodeLens => "textDocument/codeLens",
            Self::CodeLensResolve { .. } => "codeLens/resolve",
            Self::DocumentHighlights => "textDocument/documentHighlight",
            Self::FoldingRanges => "textDocument/foldingRange",
            Self::DocumentColors => "textDocument/documentColor",
            Self::SelectionRanges => "textDocument/selectionRange",
        }
    }

    pub fn capability_for_document(
        &self,
        capabilities: &LspCapabilities,
        document: &LspDocument,
    ) -> bool {
        let method = match self {
            Self::CompletionResolve { .. } => "textDocument/completion",
            Self::SemanticTokensFull | Self::SemanticTokensDelta { .. } => {
                "textDocument/semanticTokens"
            }
            _ => self.method(),
        };
        self.capability(capabilities) && capabilities.supports_document(method, document)
    }

    pub fn capability(&self, capabilities: &LspCapabilities) -> bool {
        match self {
            Self::Hover => capabilities.hover,
            Self::Completion => capabilities.completion,
            Self::CompletionResolve { .. } => capabilities.completion_resolve,
            Self::SignatureHelp => capabilities.signature_help,
            Self::Definition => capabilities.definition,
            Self::Declaration => capabilities.declaration,
            Self::TypeDefinition => capabilities.type_definition,
            Self::Implementation => capabilities.implementation,
            Self::References => capabilities.references,
            Self::DocumentSymbols => capabilities.document_symbols,
            Self::WorkspaceSymbols { .. } => capabilities.workspace_symbols,
            Self::Rename { .. } => capabilities.rename,
            Self::Formatting { .. } => capabilities.formatting,
            Self::RangeFormatting { .. } => capabilities.range_formatting,
            Self::CodeAction { .. } => capabilities.code_action,
            Self::ExecuteCommand(_) => capabilities.execute_command,
            Self::PullDiagnostics { .. } => capabilities.pull_diagnostics,
            Self::SemanticTokensFull => capabilities.semantic_tokens,
            Self::SemanticTokensDelta { .. } => capabilities.semantic_tokens_delta,
            Self::InlayHints { .. } => capabilities.inlay_hints,
            Self::CodeLens => capabilities.code_lens,
            Self::CodeLensResolve { .. } => capabilities.code_lens_resolve,
            Self::DocumentHighlights => capabilities.document_highlights,
            Self::FoldingRanges => capabilities.folding_ranges,
            Self::SelectionRanges => capabilities.selection_ranges,
            Self::DocumentColors => capabilities.document_colors,
        }
    }

    pub(super) fn replace_key(&self) -> Option<&'static str> {
        match self {
            Self::Hover => Some("hover"),
            Self::Completion | Self::CompletionResolve { .. } => Some("completion"),
            Self::SignatureHelp => Some("signature"),
            Self::Definition | Self::Declaration | Self::TypeDefinition | Self::Implementation => {
                Some("navigation")
            }
            Self::References => Some("references"),
            Self::CodeAction { .. } => Some("code-action"),
            Self::PullDiagnostics { .. } => Some("pull-diagnostics"),
            Self::SemanticTokensFull | Self::SemanticTokensDelta { .. } => Some("semantic-tokens"),
            Self::InlayHints { .. } => Some("inlay-hints"),
            Self::CodeLens | Self::CodeLensResolve { .. } => Some("code-lens"),
            Self::DocumentHighlights => Some("highlights"),
            Self::FoldingRanges => Some("folds"),
            Self::SelectionRanges => Some("selection-ranges"),
            Self::DocumentColors => Some("colors"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum LspFeatureResult {
    Hover(LspHover),
    /// `incomplete` means the server cut the list short and wants asking again as typing goes on.
    Completion {
        items: Vec<LspCompletionItem>,
        incomplete: bool,
    },
    CompletionResolved(Box<LspCompletionItem>),
    Signature(LspMarkupContent),
    Locations(Vec<LspLocation>),
    Symbols(Vec<LspSymbol>),
    WorkspaceEdit(LspWorkspaceEdit),
    CodeActions(Vec<LspCodeAction>),
    PullDiagnostics(LspPullDiagnostics),
    SemanticTokens(LspSemanticTokens),
    SemanticTokenDelta(LspSemanticTokenDelta),
    InlayHints(Vec<LspInlayHint>),
    Colors(Vec<LspColor>),
    CodeLens(Vec<LspCodeLens>),
    Ranges(Vec<LspRange>),
    Empty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspHealth {
    pub root: PathBuf,
    pub executable: PathBuf,
    pub capabilities: LspCapabilities,
    pub attached_documents: usize,
    pub pending_requests: usize,
    pub recent_logs: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ServerRequestId {
    Number(i64),
    String(String),
}

impl ServerRequestId {
    pub(super) fn from_value(value: &Value) -> Option<Self> {
        value
            .as_i64()
            .map(Self::Number)
            .or_else(|| value.as_str().map(|value| Self::String(value.into())))
    }

    pub(super) fn value(&self) -> Value {
        match self {
            Self::Number(value) => json!(value),
            Self::String(value) => json!(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspProgressKind {
    Begin,
    Report,
    End,
}

#[derive(Clone, Debug)]
pub enum LspEvent {
    Starting {
        server: String,
    },
    Running {
        server: String,
        capabilities: LspCapabilities,
    },
    CapabilitiesChanged {
        server: String,
        method: String,
        capabilities: LspCapabilities,
    },
    InlayHintsRefresh {
        server: String,
    },
    Diagnostics {
        server: String,
        uri: String,
        version: Option<u64>,
        diagnostics: Vec<LspDiagnostic>,
    },
    Result {
        server: String,
        request_id: u64,
        method: String,
        uri: String,
        version: u64,
        result: LspFeatureResult,
    },
    ApplyEditRequest {
        server: String,
        request_id: ServerRequestId,
        label: Option<String>,
        edit: LspWorkspaceEdit,
    },
    Progress {
        server: String,
        token: String,
        kind: LspProgressKind,
        title: String,
        message: String,
        percentage: Option<u8>,
    },
    Log {
        server: String,
        /// Absent for process stderr, which is not a protocol notification.
        method: Option<String>,
        message: String,
    },
    ServerStatus {
        server: String,
        healthy: bool,
        message: String,
    },
    Health {
        server: String,
        health: LspHealth,
    },
    Unsupported {
        server: String,
        method: String,
    },
    RequestFailed {
        server: String,
        request_id: u64,
        method: String,
        message: String,
    },
    Stopped {
        server: String,
        success: bool,
    },
    Failed {
        server: String,
        message: String,
    },
}
