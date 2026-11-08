//! Tools configuration records and validation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use serde::Serialize;

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
