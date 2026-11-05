#![forbid(unsafe_code)]
mod registry;
pub use registry::{
    OFFICIAL_REGISTRY, RegistryError, RegistryEvent, RegistryPackageMetadata,
    RegistryPackageSummary, RegistryPolicy, RegistryRequest, RegistryResult,
    RegistrySearchResponse, RegistryService,
};

use std::sync::OnceLock;

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
pub use package_lock::{
    LockedPackage, PackageDependency, PackageError, PackageLock, PackageManifest, PackageSource,
    commit_store_bytes, resolve_manifests, verify_lock_store, write_lock_atomic,
};
mod mason_catalog;
pub use mason_catalog::mason_filetype_for_path;
mod catalog;
pub(crate) use catalog::{executable_file, nix_lsp_launch_environment_for_workspace_at};
pub use catalog::{
    DebugProgramPolicy, ManagedFormatter, ToolArchiveFormat, ToolDefinition, ToolEntry, ToolInstall, ToolKind,
    ToolStatus, ToolVersionPin, bundled_tool_catalog, discover_executable, inspect_tools,
    inspect_tools_at, is_nixos_host, managed_formatter, managed_tool_executable,
    managed_tool_installed_version, nix_lsp_launch_environment, nix_lsp_profile_installed,
    nix_lsp_shell, nix_lsp_tool_installed, nix_lsp_workspace_root, rust_analyzer_is_rustup_proxy,
    tool_launch_environment, tool_store_root,
};
mod installer_service;
pub use installer_service::{ToolInstallProgress, ToolInstallResult, ToolInstaller, ToolOperation};
mod version;
pub use version::{ToolVersionRequest, ToolVersionResult, ToolVersionService};
mod nix_workspace;
pub use nix_workspace::{NixWorkspaceResult, NixWorkspaceService};
mod compatibility;
mod download;
mod install;
mod install_prepare;
mod runtimes;
pub use install::ToolInstallerError;
mod package_service;
pub use package_service::{
    PackageManager, PackageOperation, PackageOperationResult, PackageService, PackageServiceEvent,
    PreparedPackage,
};

#[cfg(test)]
mod tests;
