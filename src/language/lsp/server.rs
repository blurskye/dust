//! Language-server definitions and launch configuration.

use crate::editor::text::PositionEncoding;
use crate::language::lsp::MAX_LOG_LINES;
use crate::language::lsp::REQUEST_TIMEOUT;
use serde_json::Value;
use serde_json::json;
use sha2::Sha256;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::language::lsp::messages::LspDocument;

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
    let executable = crate::files::canonicalize(executable).ok()?;
    for ancestor in executable.ancestors().take(12) {
        for candidate in [
            ancestor.join("typescript/lib"),
            ancestor.join("node_modules/typescript/lib"),
            ancestor.join("install/global/node_modules/typescript/lib"),
        ] {
            if candidate.join("typescript.js").is_file()
                || candidate.join("tsserverlibrary.js").is_file()
            {
                return crate::files::canonicalize(candidate).ok();
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
            options
                .iter()
                .any(|options| registration_matches(options, document))
        })
    }

    pub(super) fn sync_kind_for_document(&self, document: &LspDocument) -> SyncKind {
        let mut kinds = self
            .sync_registrations
            .iter()
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
    selector.as_array().is_some_and(|filters| {
        filters.iter().any(|filter| {
            filter.is_object()
                && filter
                    .get("language")
                    .is_none_or(|language| language.as_str() == Some(&document.language_id))
                && filter
                    .get("scheme")
                    .is_none_or(|scheme| scheme.as_str() == Some("file"))
                && filter.get("pattern").is_none_or(|pattern| {
                    let (pattern, path) = if let Some(pattern) = pattern.as_str() {
                        (pattern, document.path.as_path())
                    } else {
                        let Some(base) = pattern.get("baseUri") else {
                            return false;
                        };
                        let Some(base) = base
                            .as_str()
                            .or_else(|| base.get("uri").and_then(Value::as_str))
                            .and_then(crate::language::lsp::paths::path_from_file_uri)
                        else {
                            return false;
                        };
                        let (Some(pattern), Ok(path)) = (
                            pattern.get("pattern").and_then(Value::as_str),
                            document.path.strip_prefix(base),
                        ) else {
                            return false;
                        };
                        (pattern, path)
                    };
                    globset::GlobBuilder::new(pattern)
                        .literal_separator(true)
                        .build()
                        .ok()
                        .is_some_and(|glob| glob.compile_matcher().is_match(path))
                })
        })
    })
}
use sha2::Digest;
