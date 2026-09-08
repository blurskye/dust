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
pub use model::*;
mod session;
pub use session::*;
mod protocol;
use protocol::*;
mod server_message;
use server_message::*;
mod paths;
pub use paths::*;
mod error;
pub use error::*;
mod snippet;
pub use snippet::*;
mod markdown;
pub use markdown::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use origins::DiagnosticLineage;
