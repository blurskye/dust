#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::fs::File;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;
mod icons;
pub use icons::*;
mod recovery;
pub use recovery::*;
mod vfs;
pub use vfs::*;
mod project_root;
pub use project_root::*;
mod directory;
pub use directory::*;
mod watch;
pub use watch::*;
mod workspace_search;
pub use workspace_search::*;
mod error;
pub use error::*;
mod workspace_files;
pub use workspace_files::*;

#[cfg(test)]
mod tests;
