//! Tool definitions imported from the Mason catalog.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::files::ProjectRootStrategy;

use crate::tools::catalog::DebugProgramPolicy;

use crate::tools::catalog::ToolDefinition;

use crate::tools::catalog::ToolKind;

use crate::tools::install_prepare::validate_tool_id;

const MASON_LSP_CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/tools/mason-lsp-catalog.json"
));
const MASON_LSP_SCHEMA_VERSION: u32 = 1;
const MASON_LSPCONFIG_REVISION: &str = "9d28935aa95fa6c1dc7c4d6747f101a72a8a4bec";
const NVIM_LSPCONFIG_REVISION: &str = "af9adce488c75ca0a81017945c2b7fa7b461bc23";
const MAXIMUM_MASON_SERVERS: usize = 512;
const MAXIMUM_SERVER_LANGUAGES: usize = 128;
const MAXIMUM_SERVER_ARGUMENTS: usize = 64;
const MAXIMUM_SERVER_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MasonCatalogSnapshot {
    schema_version: u32,
    sources: MasonCatalogSources,
    servers: Vec<MasonServerSnapshot>,
    filetypes: MasonFiletypeSnapshot,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MasonCatalogSources {
    mason_lspconfig_revision: String,
    nvim_lspconfig_revision: String,
    mason_registry_sha256: String,
    neovim_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MasonServerSnapshot {
    id: String,
    mason_package: String,
    name: String,
    executable: Option<String>,
    launch_args: Vec<String>,
    languages: Vec<String>,
    project_root_markers: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MasonFiletypeSnapshot {
    extensions: BTreeMap<String, String>,
    filenames: BTreeMap<String, String>,
}

fn bounded_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAXIMUM_SERVER_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

fn bounded_argument(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAXIMUM_SERVER_TEXT_BYTES && !value.contains('\0')
}

fn valid_executable(value: &str) -> bool {
    value.len() <= 128
        && bounded_text(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
}

fn valid_root_marker(marker: &str) -> bool {
    !marker.is_empty()
        && marker.len() <= 255
        && marker != "."
        && marker != ".."
        && !marker.contains('/')
        && !marker.contains('\\')
        && !marker.contains('\0')
}

fn validate_snapshot(snapshot: &MasonCatalogSnapshot) -> Result<(), String> {
    if snapshot.schema_version != MASON_LSP_SCHEMA_VERSION
        || snapshot.sources.mason_lspconfig_revision != MASON_LSPCONFIG_REVISION
        || snapshot.sources.nvim_lspconfig_revision != NVIM_LSPCONFIG_REVISION
        || snapshot.sources.mason_registry_sha256.len() != 64
        || !snapshot
            .sources
            .mason_registry_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !bounded_text(&snapshot.sources.neovim_version)
    {
        return Err("Mason LSP snapshot provenance is invalid".into());
    }
    if snapshot.servers.is_empty() || snapshot.servers.len() > MAXIMUM_MASON_SERVERS {
        return Err("Mason LSP snapshot server count is invalid".into());
    }
    let mut packages = BTreeSet::new();
    for server in &snapshot.servers {
        validate_tool_id(&server.id)?;
        validate_tool_id(&server.mason_package)?;
        if !packages.insert(&server.mason_package) {
            return Err("Mason LSP snapshot contains duplicate package ownership".into());
        }
        if !bounded_text(&server.name)
            || server
                .executable
                .as_deref()
                .is_some_and(|executable| !valid_executable(executable))
            || server.launch_args.len() > MAXIMUM_SERVER_ARGUMENTS
            || server
                .launch_args
                .iter()
                .any(|argument| !bounded_argument(argument))
            || server.languages.is_empty()
            || server.languages.len() > MAXIMUM_SERVER_LANGUAGES
            || server
                .languages
                .iter()
                .any(|language| !bounded_text(language))
            || server.project_root_markers.is_empty()
            || server.project_root_markers.len() > crate::files::PROJECT_ROOT_MARKER_LIMIT
            || server
                .project_root_markers
                .iter()
                .any(|marker| !valid_root_marker(marker))
        {
            return Err(format!(
                "Mason LSP snapshot entry `{}` is invalid",
                server.id
            ));
        }
    }
    for (extension, filetype) in &snapshot.filetypes.extensions {
        if extension.len() > 128
            || !bounded_text(extension)
            || extension.contains('.')
            || !bounded_text(filetype)
        {
            return Err("Mason filetype extension mapping is invalid".into());
        }
    }
    for (filename, filetype) in &snapshot.filetypes.filenames {
        if filename.len() > 255
            || !bounded_text(filename)
            || filename.contains('/')
            || filename.contains('\\')
            || !bounded_text(filetype)
        {
            return Err("Mason filetype filename mapping is invalid".into());
        }
    }
    Ok(())
}

fn snapshot() -> &'static MasonCatalogSnapshot {
    static SNAPSHOT: OnceLock<MasonCatalogSnapshot> = OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        let snapshot: MasonCatalogSnapshot =
            serde_json::from_str(MASON_LSP_CATALOG).expect("bundled Mason LSP snapshot parses");
        validate_snapshot(&snapshot).expect("bundled Mason LSP snapshot validates");
        snapshot
    })
}

pub(super) fn mason_lsp_definitions() -> Vec<ToolDefinition> {
    snapshot()
        .servers
        .iter()
        .filter_map(|server| {
            let executable = server.executable.clone()?;
            Some(ToolDefinition {
                id: server.id.clone(),
                name: server.name.clone(),
                kind: ToolKind::Lsp,
                languages: server.languages.clone(),
                executable,
                version: None,
                release: None,
                launch_args: server.launch_args.clone(),
                format_arguments: Vec::new(),
                probe_args: Vec::new(),
                debug_program_policy: DebugProgramPolicy::default(),
                project_root_markers: server.project_root_markers.clone(),
                project_root_strategy: ProjectRootStrategy::Marker,
                install: None,
                pins: Vec::new(),
            })
        })
        .collect()
}

#[must_use]
pub fn mason_filetype_for_path(path: &Path) -> Option<&'static str> {
    let snapshot = snapshot();
    if let Some(filename) = path.file_name().and_then(|name| name.to_str())
        && let Some(filetype) = snapshot.filetypes.filenames.get(filename)
    {
        return Some(filetype);
    }
    let extension = path.extension()?.to_str()?;
    snapshot
        .filetypes
        .extensions
        .get(extension)
        .or_else(|| {
            snapshot
                .filetypes
                .extensions
                .get(&extension.to_ascii_lowercase())
        })
        .map(String::as_str)
}

#[cfg(test)]
pub(super) fn mason_snapshot_counts() -> (usize, usize, usize) {
    let snapshot = snapshot();
    (
        snapshot.servers.len(),
        snapshot.filetypes.extensions.len(),
        snapshot.filetypes.filenames.len(),
    )
}
