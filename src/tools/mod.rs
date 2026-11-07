//! Tool catalogs, installation and package storage.

#![forbid(unsafe_code)]
mod registry;
pub use registry::OFFICIAL_REGISTRY;
pub use registry::RegistryError;
pub use registry::RegistryEvent;
pub use registry::RegistryPackageMetadata;
pub use registry::RegistryPackageSummary;
pub use registry::RegistryPolicy;
pub use registry::RegistryRequest;
pub use registry::RegistryResult;
pub use registry::RegistrySearchResponse;
pub use registry::RegistryWorker;

use std::sync::OnceLock;

const BUNDLED_NIX_LSP_FLAKE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/tools/nix-workspace/flake.nix"
));
const BUNDLED_NIX_LSP_LOCK: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/tools/nix-workspace/flake.lock"
));
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
pub use package_lock::LockedPackage;
pub use package_lock::PackageDependency;
pub use package_lock::PackageError;
pub use package_lock::PackageLock;
pub use package_lock::PackageManifest;
pub use package_lock::PackageSource;
pub use package_lock::commit_store_bytes;
pub use package_lock::resolve_manifests;
pub use package_lock::verify_lock_store;
pub use package_lock::write_lock_atomic;
mod mason_catalog;
pub use mason_catalog::mason_filetype_for_path;
mod catalog;
pub use catalog::DebugProgramPolicy;
pub use catalog::ManagedFormatter;
pub use catalog::ToolArchiveFormat;
pub use catalog::ToolDefinition;
pub use catalog::ToolEntry;
pub use catalog::ToolInstall;
pub use catalog::ToolKind;
pub use catalog::ToolStatus;
pub use catalog::ToolVersionPin;
pub use catalog::bundled_tool_catalog;
pub use catalog::discover_executable;
pub use catalog::inspect_tools;
pub use catalog::inspect_tools_at;
pub use catalog::is_nixos_host;
pub use catalog::managed_formatter;
pub use catalog::managed_tool_executable;
pub use catalog::managed_tool_installed_version;
pub use catalog::nix_lsp_launch_environment;
pub(crate) use catalog::nix_lsp_launch_environment_for_workspace_at;
pub use catalog::nix_lsp_profile_installed;
pub use catalog::nix_lsp_shell;
pub use catalog::nix_lsp_tool_installed;
pub use catalog::nix_lsp_workspace_root;
pub use catalog::rust_analyzer_is_rustup_proxy;
pub use catalog::tool_launch_environment;
pub use catalog::tool_store_root;
mod installer;
pub use installer::ToolInstallProgress;
pub use installer::ToolInstallResult;
pub use installer::ToolInstaller;
pub use installer::ToolOperation;
mod version;
pub use version::ToolVersionRequest;
pub use version::ToolVersionResult;
pub use version::ToolVersionWorker;
mod nix_workspace;
pub use nix_workspace::NixWorkspaceResult;
pub use nix_workspace::NixWorkspaceWorker;
mod compatibility;
mod download;
mod install;
mod install_prepare;
mod runtimes;
pub use install::ToolInstallerError;
mod packages;
pub use packages::PackageEvent;
pub use packages::PackageOperation;
pub use packages::PackageOperationResult;
pub use packages::PackageStore;
pub use packages::PackageWorker;
pub use packages::PreparedPackage;

#[cfg(test)]
mod tests;

pub(crate) use packages::installed_plugins;
