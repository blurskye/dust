//! Language-server connections and protocol data.

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
mod messages;
mod server;
pub use messages::LspCodeAction;
pub use messages::LspCodeLens;
pub use messages::LspColor;
pub use messages::LspCommand;
pub use messages::LspCompletionItem;
pub use messages::LspDiagnostic;
pub use messages::LspDocument;
pub use messages::LspDocumentEdit;
pub use messages::LspEvent;
pub use messages::LspFeature;
pub use messages::LspFeatureResult;
pub use messages::LspHealth;
pub use messages::LspHover;
pub use messages::LspInlayHint;
pub use messages::LspLocation;
pub use messages::LspMarkupContent;
pub use messages::LspMarkupKind;
pub use messages::LspPosition;
pub use messages::LspProgressKind;
pub use messages::LspPullDiagnostics;
pub use messages::LspRange;
pub use messages::LspResourceOperation;
pub use messages::LspSemanticTokenDelta;
pub use messages::LspSemanticTokenEdit;
pub use messages::LspSemanticTokens;
pub use messages::LspSymbol;
pub use messages::LspTextEdit;
pub use messages::LspWorkspaceEdit;
pub use messages::ServerRequestId;
pub use messages::lsp_symbol_kind_name;
pub use server::LspCapabilities;
pub use server::LspPolicy;
pub use server::LspServerConfiguration;
pub use server::LspSessionKey;
pub use server::SyncKind;
mod session;
pub use session::LspClient;
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
