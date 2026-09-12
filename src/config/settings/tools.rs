use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRegistryConfig {
    pub base_url: String,
    pub offline: bool,
    pub redirect_limit: u8,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub maximum_metadata_bytes: usize,
    pub maximum_artifact_bytes: usize,
    pub maximum_cache_bytes: u64,
    pub maximum_cache_entries: usize,
    pub maximum_search_results: usize,
    pub queue_capacity: usize,
}
impl Default for PackageRegistryConfig {
    fn default() -> Self {
        Self {
            base_url: "https://dust.blurskye.dev/registry/v1".into(),
            offline: false,
            redirect_limit: 0,
            connect_timeout_ms: 5_000,
            request_timeout_ms: 30_000,
            maximum_metadata_bytes: 1024 * 1024,
            maximum_artifact_bytes: 64 * 1024 * 1024,
            maximum_cache_bytes: 512 * 1024 * 1024,
            maximum_cache_entries: 4096,
            maximum_search_results: 100,
            queue_capacity: 8,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackagesConfig {
    pub registry: PackageRegistryConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntryConfig {
    pub manifest: PathBuf,
    pub component: PathBuf,
    pub enabled: bool,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginsConfig {
    pub store: PathBuf,
    pub queue_capacity: usize,
    pub maximum_component_bytes: usize,
    pub maximum_commands: usize,
    pub fuel: u64,
    pub epoch_deadline_ticks: u64,
    pub maximum_memory_bytes: usize,
    pub maximum_table_elements: u32,
    pub maximum_instances: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, PluginEntryConfig>,
}
impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            store: PathBuf::from("packages/store"),
            queue_capacity: 8,
            maximum_component_bytes: 64 * 1024 * 1024,
            maximum_commands: 256,
            fuel: 10_000_000,
            epoch_deadline_ticks: 10,
            maximum_memory_bytes: 64 * 1024 * 1024,
            maximum_table_elements: 10_000,
            maximum_instances: 16,
            entries: BTreeMap::new(),
        }
    }
}
