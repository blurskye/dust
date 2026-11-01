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
pub(crate) use origins::{ApplyEditReply, EditContext, OwnedLspEvent, PendingEditReply};
mod model;
pub use model::{
    LspCapabilities, LspCodeAction, LspCodeLens, LspCommand, LspCompletionItem, LspDiagnostic, LspDocument,
    LspDocumentEdit, LspEvent, LspFeature, LspColor, LspFeatureResult, LspHealth, LspHover, LspInlayHint,
    LspLocation, LspMarkupContent, LspMarkupKind, LspPolicy, LspPosition, LspProgressKind,
    LspPullDiagnostics, LspRange, LspResourceOperation, LspSemanticTokenDelta,
    LspSemanticTokenEdit, LspSemanticTokens, LspServerConfiguration, LspSessionKey, LspSymbol,
    LspTextEdit, LspWorkspaceEdit, ServerRequestId, SyncKind,
};
mod session;
pub use session::{LspService, LspSessionLaunch};
mod paths;
mod protocol;
mod server_message;
pub use paths::{file_uri, language_id_for_path, path_from_file_uri};
mod error;
pub use error::LspError;
mod snippet;
pub use snippet::{ParsedSnippet, SnippetTabstop, parse_snippet};
mod markdown;
pub use markdown::sanitize_markdown;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use origins::DiagnosticLineage;
