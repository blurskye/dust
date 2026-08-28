use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
mod status;
pub use status::*;
mod hunks;
pub use hunks::*;
mod mutation;
pub use mutation::*;

#[cfg(test)]
mod tests;
