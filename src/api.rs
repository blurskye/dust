use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};
pub const COMMAND_API_VERSION: u32 = 1;
pub const WIT_API_VERSION: &str = "0.1.0";
pub const CONFIG_SCHEMA_VERSION: u32 = 1;
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
#[must_use]
pub fn check_compatibility(path: &Path, maximum_bytes: usize) -> Result<ApiCompatibility, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > maximum_bytes as u64 {
        return Err("API fixture exceeds byte ceiling".into());
    }
    let required: ApiManifest =
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let current = manifest()?;
    let mut reasons = Vec::new();
    if required.command_api > current.command_api
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
    if required.persistence_schema > current.persistence_schema {
        reasons.push("persistence schema is newer".into())
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
    let mut registry = crate::editing::CommandRegistry::<crate::application::AppEffect>::default();
    crate::editing::register_builtin_commands(&mut registry)
        .map_err(|error| format!("built-in command inventory failed: {error}"))?;
    crate::application::register_app_commands(&mut registry)
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
            "history",
            "recovery",
            "config_watch",
            "runtime",
            "lsp",
            "observability",
            "persistence",
            "processes",
            "packages",
            "plugins",
            "debug",
            "tasks",
            "syntax",
            "terminals",
            "workspace",
            "messages",
            "command_labels",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        capabilities: BTreeMap::from([
            ("plugin_runtime".into(), "wasmtime-component-no-wasi".into()),
            (
                "package_network".into(),
                "official-https-no-redirect-v1".into(),
            ),
            ("license".into(), "GPL-3.0-or-later".into()),
            ("plugin_sdk_license".into(), "Apache-2.0".into()),
            ("release_signing_policy".into(), "sigstore-keyless".into()),
            ("syntax_registry".into(), "pinned-builtins-8-v1".into()),
            (
                "plugin_load_ownership".into(),
                "generation-conditional-bounded-v1".into(),
            ),
            ("release_archive".into(), "deterministic-tar-zst-v1".into()),
            ("installer".into(), "user-local-atomic-v1".into()),
            ("pty_screen".into(), "bounded-vt-subset-v1".into()),
        ]),
    })
}
