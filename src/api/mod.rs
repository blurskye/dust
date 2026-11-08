//! Public command and configuration descriptions.

use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
pub const COMMAND_API_VERSION: u32 = 1;
pub const WIT_API_VERSION: &str = "0.4.0";
pub const CONFIG_SCHEMA_VERSION: u32 = crate::config::CONFIG_SCHEMA_VERSION;
pub const PERSISTENCE_SCHEMA_VERSION: u32 = crate::persistence::STATE_VERSION;
pub const PACKAGE_LOCK_SCHEMA_VERSION: u32 = 1;
pub const THEME_SCHEMA_VERSION: u32 = 1;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiManifest {
    pub command_api: u32,
    pub wit_api: String,
    pub config_schema: u32,
    pub persistence_schema: u32,
    pub package_lock_schema: u32,
    pub theme_schema: u32,
    pub minimum_compatible_command_api: u32,
    pub deprecated: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub config_sections: Vec<String>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, String>,
}
pub fn check_compatibility(path: &Path, maximum_bytes: usize) -> Result<ApiCompatibility, String> {
    let file = crate::files::open_regular_file(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > maximum_bytes as u64 {
        return Err("API fixture exceeds byte limit".into());
    }
    let mut bytes = Vec::new();
    file.take((maximum_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > maximum_bytes {
        return Err("API fixture exceeds byte limit".into());
    }
    let required: ApiManifest =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let current = manifest()?;
    let mut reasons = Vec::new();
    if required.command_api < current.minimum_compatible_command_api
        || required.command_api > current.command_api
        || required.minimum_compatible_command_api > current.command_api
    {
        reasons.push("command API version is unsupported".into())
    }
    if required.wit_api != current.wit_api {
        reasons.push("WIT API version differs".into())
    }
    if required.config_schema > current.config_schema {
        reasons.push("config schema is newer".into())
    }
    if required.persistence_schema != current.persistence_schema {
        reasons.push("persistence schema is unsupported".into())
    }
    if required.package_lock_schema > current.package_lock_schema {
        reasons.push("package lock schema is newer".into())
    }
    if required.theme_schema > current.theme_schema {
        reasons.push("theme schema is newer".into())
    }
    for command in &required.commands {
        if !current.commands.contains(command) {
            reasons.push(format!("required command `{command}` is missing"));
        }
    }
    for section in &required.config_sections {
        if !current.config_sections.contains(section) {
            reasons.push(format!("required config section `{section}` is missing"));
        }
    }
    for (name, value) in &required.capabilities {
        if current.capabilities.get(name) != Some(value) {
            reasons.push(format!("required capability `{name}={value}` differs"));
        }
    }
    Ok(ApiCompatibility {
        compatible: reasons.is_empty(),
        required,
        current,
        reasons,
    })
}
#[derive(Clone, Debug, Serialize)]
pub struct ApiCompatibility {
    pub compatible: bool,
    pub required: ApiManifest,
    pub current: ApiManifest,
    pub reasons: Vec<String>,
}

pub fn manifest() -> Result<ApiManifest, String> {
    let mut registry = crate::editor::CommandRegistry::<crate::app::commands::AppEffect>::default();
    crate::editor::register_builtin_commands(&mut registry)
        .map_err(|error| format!("built-in command inventory failed: {error}"))?;
    crate::app::commands::register_app_commands(&mut registry)
        .map_err(|error| format!("application command inventory failed: {error}"))?;
    let commands = registry
        .specs()
        .map(|spec| spec.id.as_str().to_owned())
        .collect();
    Ok(ApiManifest {
        command_api: COMMAND_API_VERSION,
        wit_api: WIT_API_VERSION.into(),
        config_schema: CONFIG_SCHEMA_VERSION,
        persistence_schema: PERSISTENCE_SCHEMA_VERSION,
        package_lock_schema: PACKAGE_LOCK_SCHEMA_VERSION,
        theme_schema: THEME_SCHEMA_VERSION,
        minimum_compatible_command_api: 1,
        deprecated: Vec::new(),
        commands,
        config_sections: vec![
            "editor",
            "ui",
            "startup",
            "input",
            "search",
            "recovery",
            "lsp",
            "persistence",
            "plugins",
            "debug",
            "tasks",
            "syntax",
            "terminals",
            "agents",
            "windows",
            "workspace",
            "messages",
            "command_labels",
            "command_palette",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        capabilities: BTreeMap::from([
            ("plugin_runtime".into(), "wasmtime-component-no-wasi".into()),
            (
                "plugin_ui".into(),
                "retained-and-private-cells-queued-typed-v1".into(),
            ),
            (
                "shared_confirmation".into(),
                "native-plugin-queued-yes-no-cancelled-v1".into(),
            ),
            (
                "plugin_events".into(),
                "static-coalesced-queued-acknowledged-v1".into(),
            ),
            (
                "plugin_providers".into(),
                "picker-source-queued-stream-v1".into(),
            ),
            (
                "plugin_virtual_documents".into(),
                "queued-read-only-tiled-health-dogfood-v1".into(),
            ),
            (
                "plugin_storage".into(),
                "private-queued-atomic-cas-v1".into(),
            ),
            (
                "package_network".into(),
                "official-https-no-redirect-v1".into(),
            ),
            (
                "buffer_line_mouse".into(),
                "select-right-close-dirty-safe-v1".into(),
            ),
            (
                "ux_navigation".into(),
                "fold-wrap-visual-row-margin-page-mouse-v2".into(),
            ),
            (
                "indent_guides".into(),
                "configurable-display-column-scope-toggle-v7".into(),
            ),
            (
                "documentation".into(),
                "typed-sections-ranges-exact-completion-v1".into(),
            ),
            (
                "lsp".into(),
                "trusted-installed-project-root-autostart-selection-progress-v3".into(),
            ),
            (
                "content_line".into(),
                "typed-pins-order-icons-diagnostics-custom-v1".into(),
            ),
            ("explorer".into(), "async-watch-follow-scroll-v1".into()),
            (
                "git_status".into(),
                "worker-picker-explorer-live-buffer-stable-gutter-hunks-v4".into(),
            ),
            (
                "git_mutation".into(),
                "trusted-exact-enum-stage-confirmed-reset-v2".into(),
            ),
            ("license".into(), "GPL-3.0-or-later".into()),
            ("plugin_sdk_license".into(), "Apache-2.0".into()),
            ("release_signing_policy".into(), "sigstore-keyless".into()),
            ("syntax_registry".into(), "pinned-builtins-8-v1".into()),
            (
                "plugin_load_ownership".into(),
                "load-conditional-bounded-v1".into(),
            ),
            ("release_archive".into(), "deterministic-tar-zst-v1".into()),
            ("pty_screen".into(), "bounded-vt-subset-v1".into()),
            (
                "composable_search".into(),
                "typed-bounded-file-grep-command-plugin-v1".into(),
            ),
            (
                "terminal_emulation".into(),
                "bounded-xterm-tabs-normal-visual-directional-v2".into(),
            ),
        ]),
    })
}

#[cfg(test)]
mod tests {
    fn compatibility(required: &crate::api::ApiManifest) -> crate::api::ApiCompatibility {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("manifest.json");
        std::fs::write(&path, serde_json::to_vec(required).unwrap()).unwrap();
        crate::api::check_compatibility(&path, 1024 * 1024).unwrap()
    }

    #[test]
    fn compatibility_requires_a_regular_bounded_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("manifest.json");
        let bytes = serde_json::to_vec(&crate::api::manifest().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(crate::api::check_compatibility(&path, bytes.len()).is_ok());
        assert!(crate::api::check_compatibility(&path, bytes.len() - 1).is_err());
        assert!(crate::api::check_compatibility(std::path::Path::new("/dev/zero"), 1024).is_err());
        // FIFO creation is Unix-only; size and regular-file checks above are shared.
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).unwrap();
            nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
            assert!(crate::api::check_compatibility(&path, 1024).is_err());
        }
    }

    #[test]
    fn command_api_must_be_in_the_supported_range() {
        let mut required = crate::api::manifest().unwrap();
        assert!(compatibility(&required).compatible);
        required.command_api = 0;
        required.minimum_compatible_command_api = 0;
        assert!(!compatibility(&required).compatible);
        required.command_api = crate::api::COMMAND_API_VERSION + 1;
        assert!(!compatibility(&required).compatible);
    }

    #[test]
    fn persistence_schema_must_match_the_reader() {
        let mut required = crate::api::manifest().unwrap();
        for version in [
            0,
            crate::api::PERSISTENCE_SCHEMA_VERSION - 1,
            crate::api::PERSISTENCE_SCHEMA_VERSION + 1,
        ] {
            required.persistence_schema = version;
            assert!(!compatibility(&required).compatible);
        }
    }

    #[test]
    fn command_palette_is_a_supported_config_section() {
        let mut required = crate::api::manifest().unwrap();
        required.config_sections = vec!["command_palette".into()];
        assert!(compatibility(&required).compatible);
    }

    #[test]
    fn removed_installer_is_not_supported() {
        let mut required = crate::api::manifest().unwrap();
        assert!(!required.capabilities.contains_key("installer"));
        required
            .capabilities
            .insert("installer".into(), "user-local-atomic-v1".into());
        assert!(!compatibility(&required).compatible);
    }

    #[test]
    fn checked_in_release_contract_is_supported() {
        let compatibility = crate::api::check_compatibility(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("api/compat-v1.json"),
            1024 * 1024,
        )
        .unwrap();
        assert!(compatibility.compatible, "{:?}", compatibility.reasons);
    }
}
