#![forbid(unsafe_code)]

pub mod git;
pub mod search_workflow;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::editor::text::{
    BufferRevision, ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet,
};

const ABSOLUTE_ROOT_LIMIT: usize = 64;
const ABSOLUTE_DIAGNOSTIC_LIMIT: usize = 100_000;
const ABSOLUTE_PICKER_ITEM_LIMIT: usize = 100_000;
const ABSOLUTE_PROPOSAL_FILE_LIMIT: usize = 1_024;
const ABSOLUTE_PROPOSAL_EDIT_LIMIT: usize = 100_000;
mod root;
pub use root::*;
mod trust;
pub use trust::*;
mod explorer;
pub use explorer::*;
mod diagnostics;
pub use diagnostics::*;
mod picker;
pub use picker::*;
mod edit;
pub use edit::*;

#[cfg(test)]
mod tests;
