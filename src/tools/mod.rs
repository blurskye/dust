#![forbid(unsafe_code)]
mod registry;
pub use registry::{
    OFFICIAL_REGISTRY, RegistryError, RegistryEvent, RegistryPackageMetadata,
    RegistryPackageSummary, RegistryRequest, RegistryResult, RegistrySearchResponse,
    RegistryService,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    extensions::{PluginLimits, PluginManifest, activate_candidate},
    files::ProjectRootStrategy,
};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const BUNDLED_NIX_LSP_FLAKE: &str = include_str!("../../assets/tools/nix-workspace/flake.nix");
const BUNDLED_NIX_LSP_LOCK: &str = include_str!("../../assets/tools/nix-workspace/flake.lock");
/// Exact nixpkgs revision locked by Dust's bundled Linux compatibility runtime.
pub const NIX_LSP_NIXPKGS_REVISION: &str = "b6018f87da91d19d0ab4cf979885689b469cdd41";
const NIX_COMPATIBILITY_FLAKE_PACKAGE: &str = "compat-runtime";
const NIX_COMPATIBILITY_RUNTIME_ID: &str =
    "v1-b6018f87da91d19d0ab4cf979885689b469cdd41-common-libraries-1";
const NIX_RUNTIME_ENVIRONMENT_FILE: &str = "share/dust/runtime.env";
const MAXIMUM_NIX_RUNTIME_ENVIRONMENT_BYTES: u64 = 32 * 1024;
const MAXIMUM_OS_RELEASE_BYTES: u64 = 16 * 1024;
static NIXOS_HOST: OnceLock<bool> = OnceLock::new();
mod package_lock;
pub use package_lock::*;
mod mason_catalog;
pub use mason_catalog::mason_filetype_for_path;
use mason_catalog::mason_lsp_definitions;
#[cfg(test)]
use mason_catalog::mason_snapshot_counts;
mod catalog;
pub use catalog::*;
mod installer_service;
pub use installer_service::*;
mod version;
pub use version::*;
mod nix_workspace;
pub use nix_workspace::*;
mod install_prepare;
use install_prepare::*;
mod download;
use download::*;
mod compatibility;
use compatibility::*;
mod runtimes;
use runtimes::*;
mod install;
pub use install::*;
mod package_service;
pub use package_service::*;

#[cfg(test)]
mod tests;
