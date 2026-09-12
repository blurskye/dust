#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    editor::text::{ByteOffset, PositionCodec, PositionEncoding, TextBuffer},
    process::{read_content_length_message, write_content_length_message},
    tools::{ToolDefinition, ToolKind},
};

const COMMAND_QUEUE: usize = 64;
const MESSAGE_QUEUE: usize = 128;
const EVENT_QUEUE: usize = 256;
const MAX_PENDING_REQUESTS: usize = 128;
const MAX_LOG_LINES: usize = 200;
const MAX_LOG_LINE_BYTES: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
mod origins;
use origins::EventSender;
pub(crate) use origins::{ApplyEditReply, EditContext, OwnedLspEvent, PendingEditReply};
mod model;
pub use model::{
    LspCapabilities, LspCodeAction, LspCodeLens, LspCompletionItem, LspDiagnostic, LspDocument,
    LspDocumentEdit, LspEvent, LspFeature, LspFeatureResult, LspHealth, LspHover, LspInlayHint,
    LspLocation, LspMarkupContent, LspMarkupKind, LspPolicy, LspPosition, LspProgressKind,
    LspPullDiagnostics, LspRange, LspResourceOperation, LspSemanticTokenDelta,
    LspSemanticTokenEdit, LspSemanticTokens, LspServerConfiguration, LspSessionKey, LspSymbol,
    LspTextEdit, LspWorkspaceEdit, ServerRequestId, SyncKind,
};
mod session;
pub use session::{LspService, LspSessionLaunch};
use session::{PendingRequest, SessionCommand};
mod protocol;
use protocol::*;
mod server_message;
use server_message::*;
mod paths;
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
