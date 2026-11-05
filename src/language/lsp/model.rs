use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::editor::text::PositionEncoding;

use super::{MAX_LOG_LINES, REQUEST_TIMEOUT, paths::file_uri};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LspPolicy {
    pub request_timeout: Duration,
    pub maximum_log_lines: usize,
}

impl Default for LspPolicy {
    fn default() -> Self {
        Self {
            request_timeout: REQUEST_TIMEOUT,
            maximum_log_lines: MAX_LOG_LINES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspServerConfiguration {
    pub(super) initialization_options: Option<Value>,
    pub(super) settings: Value,
}

impl LspServerConfiguration {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            initialization_options: None,
            settings: json!({}),
        }
    }

    #[must_use]
    pub fn for_server(server_id: &str, executable: &Path) -> Self {
        if matches!(
            server_id,
            "astro-language-server" | "astro" | "vue-language-server" | "vue_ls"
        ) && let Some(tsdk) = typescript_sdk_for_executable(executable)
        {
            return Self {
                initialization_options: Some(json!({
                    "typescript":{"tsdk":tsdk.to_string_lossy()}
                })),
                settings: json!({}),
            };
        }
        // What nvim-lspconfig sends: these servers format and validate only when told to.
        match server_id {
            "vscode-json-language-server" => Self {
                initialization_options: Some(json!({"provideFormatter":true})),
                settings: json!({"json":{"validate":{"enable":true},"format":{"enable":true}}}),
            },
            "vscode-css-language-server" => Self {
                initialization_options: Some(json!({"provideFormatter":true})),
                settings: json!({"css":{"validate":true},"scss":{"validate":true},"less":{"validate":true}}),
            },
            "vscode-html-language-server" => Self {
                initialization_options: Some(json!({
                    "provideFormatter":true,
                    "embeddedLanguages":{"css":true,"javascript":true},
                    "configurationSection":["html","css","javascript"]
                })),
                settings: json!({}),
            },
            _ => Self::standard(),
        }
    }

    pub(crate) fn overlay(&mut self, other: Self) {
        fn merge(target: &mut Value, source: Value) {
            match (target, source) {
                (Value::Object(target), Value::Object(source)) => {
                    for (key, value) in source {
                        merge(target.entry(key).or_insert(Value::Null), value);
                    }
                }
                (target, source) => *target = source,
            }
        }
        if let Some(options) = other.initialization_options {
            merge(
                self.initialization_options.get_or_insert(Value::Null),
                options,
            );
        }
        merge(&mut self.settings, other.settings);
    }
    #[must_use]
    pub fn initialization_options(&self) -> Option<&Value> {
        self.initialization_options.as_ref()
    }

    #[must_use]
    pub fn maximum_inlay_hints(server_id: &str) -> Self {
        match server_id {
            "rust-analyzer" => {
                let hints = json!({
                    "bindingModeHints":{"enable":true},
                    "chainingHints":{"enable":true},
                    "closingBraceHints":{"enable":true,"minLines":0},
                    "closureCaptureHints":{"enable":true},
                    "closureReturnTypeHints":{"enable":"always"},
                    "discriminantHints":{"enable":"always"},
                    "expressionAdjustmentHints":{
                        "disableReborrows":false,"enable":"always","hideOutsideUnsafe":false,"mode":"prefix"
                    },
                    "genericParameterHints":{
                        "const":{"enable":true},"lifetime":{"enable":true},"type":{"enable":true}
                    },
                    "implicitDrops":{"enable":true},
                    "implicitSizedBoundHints":{"enable":true},
                    "impliedDynTraitHints":{"enable":true},
                    "lifetimeElisionHints":{"enable":"always","useParameterNames":true},
                    "maxLength":null,
                    "parameterHints":{"enable":true,"missingArguments":{"enable":true}},
                    "rangeExclusiveHints":{"enable":true},
                    "renderColons":true,
                    "typeHints":{
                        "enable":true,
                        "hideClosureInitialization":false,
                        "hideClosureParameter":false,
                        "hideInferredTypes":false,
                        "hideNamedConstructor":false,
                        "location":"inline"
                    }
                });
                Self {
                    initialization_options: Some(json!({"inlayHints":hints.clone()})),
                    settings: json!({"rust-analyzer":{"inlayHints":hints}}),
                }
            }
            "typescript-language-server" => {
                let hints = json!({
                    "includeInlayEnumMemberValueHints":true,
                    "includeInlayFunctionLikeReturnTypeHints":true,
                    "includeInlayFunctionParameterTypeHints":true,
                    "includeInlayParameterNameHints":"all",
                    "includeInlayParameterNameHintsWhenArgumentMatchesName":true,
                    "includeInlayPropertyDeclarationTypeHints":true,
                    "includeInlayVariableTypeHints":true,
                    "includeInlayVariableTypeHintsWhenTypeMatchesName":true
                });
                let preferences = json!({
                    "includeInlayEnumMemberValueHints":true,
                    "includeInlayFunctionLikeReturnTypeHints":true,
                    "includeInlayFunctionParameterTypeHints":true,
                    "includeInlayParameterNameHints":"all",
                    "includeInlayParameterNameHintsWhenArgumentMatchesName":true,
                    "includeInlayPropertyDeclarationTypeHints":true,
                    "includeInlayVariableTypeHints":true,
                    "includeInlayVariableTypeHintsWhenTypeMatchesName":true
                });
                Self {
                    initialization_options: Some(json!({"preferences":preferences})),
                    settings: json!({
                        "typescript":{"inlayHints":hints.clone()},
                        "javascript":{"inlayHints":hints}
                    }),
                }
            }
            "gopls" => {
                let hints = json!({
                    "assignVariableTypes":true,
                    "compositeLiteralFields":true,
                    "compositeLiteralTypes":true,
                    "constantValues":true,
                    "functionTypeParameters":true,
                    "parameterNames":true,
                    "rangeVariableTypes":true
                });
                Self {
                    initialization_options: Some(json!({"hints":hints.clone()})),
                    settings: json!({"gopls":{"hints":hints}}),
                }
            }
            _ => Self::standard(),
        }
    }
}

fn typescript_sdk_for_executable(executable: &Path) -> Option<PathBuf> {
    let executable = std::fs::canonicalize(executable).ok()?;
    for ancestor in executable.ancestors().take(12) {
        for candidate in [
            ancestor.join("typescript/lib"),
            ancestor.join("node_modules/typescript/lib"),
            ancestor.join("install/global/node_modules/typescript/lib"),
        ] {
            if candidate.join("typescript.js").is_file()
                || candidate.join("tsserverlibrary.js").is_file()
            {
                return std::fs::canonicalize(candidate).ok();
            }
        }
    }
    None
}

impl Default for LspServerConfiguration {
    fn default() -> Self {
        Self::standard()
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LspSessionKey {
    /// Identity/trust owner of the enclosing Dust workspace root.
    pub root_id: u64,
    /// Canonical adapter project root selected by the generic project resolver.
    pub project_root: PathBuf,
    pub server_id: String,
}
impl LspSessionKey {
    #[must_use]
    pub fn transport_key(&self) -> String {
        let digest = Sha256::digest(self.project_root.as_os_str().as_encoded_bytes());
        format!(
            "root:{}:project:{digest:x}:{}",
            self.root_id, self.server_id
        )
    }
}

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyncKind {
    None,
    #[default]
    Full,
    Incremental,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspCapabilities {
    pub position_encoding: PositionEncoding,
    pub sync_kind: SyncKind,
    pub completion: bool,
    pub completion_resolve: bool,
    pub completion_triggers: Vec<String>,
    pub hover: bool,
    pub signature_help: bool,
    pub definition: bool,
    pub declaration: bool,
    pub type_definition: bool,
    pub implementation: bool,
    pub references: bool,
    pub document_symbols: bool,
    pub workspace_symbols: bool,
    pub rename: bool,
    pub formatting: bool,
    pub range_formatting: bool,
    pub code_action: bool,
    pub execute_command: bool,
    pub semantic_tokens: bool,
    pub semantic_tokens_delta: bool,
    pub inlay_hints: bool,
    pub pull_diagnostics: bool,
    pub code_lens: bool,
    pub code_lens_resolve: bool,
    pub document_highlights: bool,
    pub folding_ranges: bool,
    pub selection_ranges: bool,
    pub document_colors: bool,
    pub semantic_token_types: Vec<String>,
    /// Selectors for providers enabled only by dynamic registration.
    pub document_selectors: std::collections::HashMap<String, Vec<Value>>,
    pub sync_registrations: Vec<Value>,
}

impl LspCapabilities {
    pub fn supports_document(&self, method: &str, document: &LspDocument) -> bool {
        self.document_selectors.get(method).is_none_or(|options| {
            options.iter().any(|options| registration_matches(options, document))
        })
    }

    pub(super) fn sync_kind_for_document(&self, document: &LspDocument) -> SyncKind {
        let mut kinds = self.sync_registrations.iter()
            .filter(|options| registration_matches(options, document))
            .filter_map(|options| options.get("syncKind").and_then(Value::as_u64))
            .peekable();
        if kinds.peek().is_none() {
            return self.sync_kind;
        }
        kinds.fold(SyncKind::None, |kind, registered| match registered {
            1 => SyncKind::Full,
            2 if kind != SyncKind::Full => SyncKind::Incremental,
            _ => kind,
        })
    }
}

fn registration_matches(options: &Value, document: &LspDocument) -> bool {
    let selector = options.get("documentSelector").unwrap_or(&Value::Null);
    if selector.is_null() {
        return true;
    }
    selector.as_array().is_some_and(|filters| filters.iter().any(|filter| {
        filter.is_object()
            && filter.get("language").is_none_or(|language| language.as_str() == Some(&document.language_id))
            && filter.get("scheme").is_none_or(|scheme| scheme.as_str() == Some("file"))
            && filter.get("pattern").is_none_or(|pattern| {
                let (pattern, path) = if let Some(pattern) = pattern.as_str() {
                    (pattern, document.path.as_path())
                } else {
                    let Some(base) = pattern.get("baseUri") else { return false };
                    let Some(base) = base.as_str().or_else(|| base.get("uri").and_then(Value::as_str))
                        .and_then(super::paths::path_from_file_uri) else { return false };
                    let (Some(pattern), Ok(path)) = (
                        pattern.get("pattern").and_then(Value::as_str), document.path.strip_prefix(base),
                    ) else { return false };
                    (pattern, path)
                };
                globset::GlobBuilder::new(pattern).literal_separator(true).build().ok()
                    .is_some_and(|glob| glob.compile_matcher().is_match(path))
            })
    }))
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
    CodeLensResolve { item: Value },
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

    pub fn capability_for_document(&self, capabilities: &LspCapabilities, document: &LspDocument) -> bool {
        let method = match self {
            Self::CompletionResolve { .. } => "textDocument/completion",
            Self::SemanticTokensFull | Self::SemanticTokensDelta { .. } => "textDocument/semanticTokens",
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
        generation: u64,
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
        generation: u64,
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
