#![forbid(unsafe_code)]

use std::time::Duration;

const COMMAND_QUEUE: usize = 64;
const MESSAGE_QUEUE: usize = 128;
const EVENT_QUEUE: usize = 256;
const MAX_PENDING_REQUESTS: usize = 128;
const MAX_LOG_LINES: usize = 200;
const MAX_LOG_LINE_BYTES: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
mod origins;
pub(crate) use origins::ApplyEditReply;
pub(crate) use origins::EditContext;
pub(crate) use origins::OwnedLspEvent;
pub(crate) use origins::PendingEditReply;
mod model;
pub use model::LspCapabilities;
pub use model::LspCodeAction;
pub use model::LspCodeLens;
pub use model::LspColor;
pub use model::LspCommand;
pub use model::LspCompletionItem;
pub use model::LspDiagnostic;
pub use model::LspDocument;
pub use model::LspDocumentEdit;
pub use model::LspEvent;
pub use model::LspFeature;
pub use model::LspFeatureResult;
pub use model::LspHealth;
pub use model::LspHover;
pub use model::LspInlayHint;
pub use model::LspLocation;
pub use model::LspMarkupContent;
pub use model::LspMarkupKind;
pub use model::LspPolicy;
pub use model::LspPosition;
pub use model::LspProgressKind;
pub use model::LspPullDiagnostics;
pub use model::LspRange;
pub use model::LspResourceOperation;
pub use model::LspSemanticTokenDelta;
pub use model::LspSemanticTokenEdit;
pub use model::LspSemanticTokens;
pub use model::LspServerConfiguration;
pub use model::LspSessionKey;
pub use model::LspSymbol;
pub use model::LspTextEdit;
pub use model::LspWorkspaceEdit;
pub use model::ServerRequestId;
pub use model::SyncKind;
pub use model::lsp_symbol_kind_name;
mod session;
pub use session::LspService;
pub use session::LspSessionLaunch;
mod paths;
mod protocol;
mod server_message;
pub use paths::file_uri;
pub use paths::language_id_for_path;
pub use paths::path_from_file_uri;
mod error;
pub use error::LspError;
mod snippet;
pub use snippet::ParsedSnippet;
pub use snippet::SnippetTabstop;
pub use snippet::SnippetTransform;
pub use snippet::parse_snippet;
pub use snippet::parse_snippet_with_variables;
mod markdown;
pub use markdown::sanitize_markdown;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use origins::DiagnosticLineage;

#[cfg(test)]
pub(crate) use origins::EditSourceFixture;
