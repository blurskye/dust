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
use package_lock::{
    BUNDLED_TOOL_CATALOG, NEXT_PACKAGE_TEMP, read_bounded_regular, valid_digest, valid_source,
    write_protected_atomic,
};
pub use package_lock::{
    LockedPackage, PackageDependency, PackageError, PackageLock, PackageManifest, PackageSource,
    commit_store_bytes, resolve_manifests, verify_lock_store, write_lock_atomic,
};
mod mason_catalog;
pub use mason_catalog::mason_filetype_for_path;
use mason_catalog::mason_lsp_definitions;
#[cfg(test)]
use mason_catalog::mason_snapshot_counts;
mod catalog;
pub(crate) use catalog::nix_lsp_launch_environment_for_workspace_at;
pub use catalog::{
    DebugProgramPolicy, ToolArchiveFormat, ToolDefinition, ToolEntry, ToolInstall, ToolKind,
    ToolStatus, ToolVersionPin, bundled_tool_catalog, discover_executable, inspect_tools,
    inspect_tools_at, is_nixos_host, managed_tool_executable, managed_tool_installed_version,
    nix_lsp_launch_environment, nix_lsp_profile_installed, nix_lsp_shell, nix_lsp_tool_installed,
    nix_lsp_workspace_root, rust_analyzer_is_rustup_proxy, tool_launch_environment,
    tool_store_root,
};
use catalog::{NixCompatibilityRuntime, parse_nix_runtime_environment, write_managed_tool_version};
mod installer_service;
pub use installer_service::{ToolInstallProgress, ToolInstallResult, ToolInstaller, ToolOperation};
mod version;
#[cfg(not(unix))]
use version::probe_tool_version;
pub use version::{ToolVersionRequest, ToolVersionResult, ToolVersionService};
mod nix_workspace;
pub use nix_workspace::{NixWorkspaceResult, NixWorkspaceService};
mod install_prepare;
use install_prepare::*;
mod download;
use download::*;
mod compatibility;
use compatibility::*;
mod runtimes;
use runtimes::*;
mod install;
pub use install::ToolInstallerError;
use install::install_tool;
mod package_service;
pub use package_service::{
    PackageManager, PackageOperation, PackageOperationResult, PackageService, PackageServiceEvent,
    PreparedPackage,
};

#[cfg(test)]
mod tests;
