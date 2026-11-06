//! Tool artifact preparation and Nix compatibility files.

use std::fs;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::Url;

use crate::tools::BUNDLED_NIX_LSP_FLAKE;

use crate::tools::BUNDLED_NIX_LSP_LOCK;

use crate::tools::catalog::ToolArchiveFormat;

use crate::tools::catalog::nix_lsp_workspace_root;

use crate::tools::package_lock::next_temp_suffix;

pub(super) const MAXIMUM_TOOL_DOWNLOAD_BYTES: usize = 512 * 1024 * 1024;
pub(super) const MAXIMUM_TOOL_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
pub(super) const MAXIMUM_TOOL_ARCHIVE_ENTRIES: usize = 16_384;
pub(super) const TOOL_INSTALL_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Clone, Copy)]
pub(super) struct BootstrapArtifact {
    pub(super) url: &'static str,
    pub(super) sha256: &'static str,
    pub(super) archive: ToolArchiveFormat,
    pub(super) executable_path: &'static str,
}

pub(super) fn archive_entry_matches(entry: &Path, expected: &Path) -> bool {
    let mut entry_components = entry.components();
    match entry_components.next() {
        Some(Component::CurDir) => entry_components.eq(expected.components()),
        Some(first @ Component::Normal(_)) => std::iter::once(first)
            .chain(entry_components)
            .eq(expected.components()),
        _ => false,
    }
}

pub(super) fn relative_catalog_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("tool catalog contains an unsafe archive path".into());
    }
    Ok(path.to_path_buf())
}

pub(super) fn validate_tool_id(value: &str) -> Result<(), String> {
    let path = relative_catalog_path(value)?;
    if path.components().count() != 1
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("tool ID is not a safe private-store directory name".into());
    }
    Ok(())
}

pub(super) fn ensure_private_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Dust tool store must be absolute".into());
    }
    fs::create_dir_all(path).map_err(|error| format!("cannot create tool store: {error}"))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect tool store: {error}"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("Dust tool store is not a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot protect tool store: {error}"))?;
    }
    Ok(())
}

pub(super) fn write_private_workspace_file(
    root: &Path,
    name: &str,
    content: &str,
) -> Result<(), String> {
    let destination = root.join(name);
    if destination.exists() {
        let metadata = fs::symlink_metadata(&destination)
            .map_err(|error| format!("cannot inspect managed Nix workspace file: {error}"))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err("managed Nix workspace file is not a regular file".into());
        }
        if metadata.len() > 1024 * 1024 {
            return Err("managed Nix workspace file exceeds its byte limit".into());
        }
        let existing = fs::read(&destination)
            .map_err(|error| format!("cannot read managed Nix workspace file: {error}"))?;
        if existing == content.as_bytes() {
            return Ok(());
        }
    }
    let staging = root.join(format!(".{name}-{}", next_temp_suffix()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| format!("cannot stage managed Nix workspace file: {error}"))?;
    let result = (|| {
        file.write_all(content.as_bytes())
            .map_err(|error| format!("cannot write managed Nix workspace file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync managed Nix workspace file: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("cannot protect managed Nix workspace file: {error}"))?;
        }
        fs::rename(&staging, &destination)
            .map_err(|error| format!("cannot activate managed Nix workspace file: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staging);
    }
    result
}

pub(super) fn prepare_nix_lsp_workspace() -> Result<PathBuf, String> {
    prepare_nix_lsp_workspace_at(&nix_lsp_workspace_root())
}

pub(super) fn prepare_nix_lsp_workspace_at(root: &Path) -> Result<PathBuf, String> {
    ensure_private_directory(root)?;
    ensure_private_directory(&root.join("profiles"))?;
    write_private_workspace_file(root, "flake.nix", BUNDLED_NIX_LSP_FLAKE)?;
    write_private_workspace_file(root, "flake.lock", BUNDLED_NIX_LSP_LOCK)?;
    Ok(root.to_path_buf())
}

pub(super) fn checked_https_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "tool catalog has an invalid artifact URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("tool artifact URLs must be anonymous HTTPS URLs".into());
    }
    Ok(url)
}
