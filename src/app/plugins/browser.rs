//! Installed plugin rows and actions on the native picker.

use crate::app::App;
use crate::app::picker::{PickerOrigin, PickerPayload};
use crate::plugins::{PluginCapability, PluginStatus};
use crate::workspace::{PickerAction, PickerItem, PickerItemId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(in crate::app) enum PluginChoice {
    Details(String),
    Enable(String),
    Disable(String),
    Reload(String),
    Folder(String),
}

#[derive(Clone)]
pub(super) struct BrowserPlugin {
    pub version: String,
    pub capabilities: BTreeSet<PluginCapability>,
    pub folder: PathBuf,
    pub error: Option<String>,
}

fn permissions(capabilities: &BTreeSet<PluginCapability>) -> String {
    use PluginCapability::*;
    let mut words = Vec::new();
    for (allowed, label) in [
        (capabilities.contains(&ReadCurrentBuffer), "read buffers"),
        (capabilities.contains(&ProposeEdits), "edit buffers"),
        (
            capabilities
                .iter()
                .any(|c| matches!(c, ShowMessage | Input | PrivateCells)),
            "UI",
        ),
        (capabilities.contains(&Files), "files"),
        (capabilities.contains(&Process), "run processes"),
        (capabilities.contains(&Http), "network"),
    ] {
        if allowed {
            words.push(label);
        }
    }
    if words.is_empty() {
        "no buffer, UI or system access".into()
    } else {
        words.join(", ")
    }
}

fn status_label(status: Option<&PluginStatus>, disabled: bool, safe_mode: bool) -> String {
    if disabled {
        return "disabled".into();
    }
    match status {
        Some(PluginStatus::Active(_) | PluginStatus::Instantiated) => "loaded".into(),
        Some(PluginStatus::Loading) => "loading".into(),
        Some(PluginStatus::Indexed) => "indexed".into(),
        Some(PluginStatus::Failed(reason) | PluginStatus::ValidatedUntilHost(reason)) => {
            format!("failed: {reason}")
        }
        Some(PluginStatus::Disabled) => "disabled".into(),
        None if safe_mode => "disabled by safe mode".into(),
        None => "not loaded".into(),
    }
}

fn row(
    id: String,
    label: String,
    detail: String,
    payload: PickerPayload,
) -> (PickerItem, PickerPayload) {
    (
        PickerItem {
            id: PickerItemId(id),
            label,
            detail,
            source: String::new(),
            badge: String::new(),
            kind: crate::workspace::PickerItemKind::Entry,
            action: PickerAction::None,
        },
        payload,
    )
}

impl App {
    pub(in crate::app) fn open_plugin_browser(&mut self) {
        let policy = self.configuration.settings().plugins.clone();
        let store = if policy.store.is_absolute() {
            policy.store
        } else {
            crate::config::user_state_directory().join(policy.store)
        };
        self.extension.browser_configured.clear();
        for (id, entry) in policy.entries.iter().take(4096) {
            let result = crate::plugins::index_manifest_from_store(
                &store,
                &entry.manifest,
                &entry.component,
                crate::plugins::PluginLoadId(0),
                &self.plugin_limits(),
            );
            let metadata = match result {
                Ok((manifest, _, _, _)) if manifest.id == *id => BrowserPlugin {
                    version: manifest.version,
                    capabilities: manifest.capabilities,
                    folder: store
                        .join(&entry.manifest)
                        .parent()
                        .unwrap_or(&store)
                        .to_owned(),
                    error: None,
                },
                Ok(_) => BrowserPlugin {
                    version: "?".into(),
                    capabilities: BTreeSet::new(),
                    folder: store.clone(),
                    error: Some("manifest ID does not match configuration".into()),
                },
                Err(error) => BrowserPlugin {
                    version: "?".into(),
                    capabilities: BTreeSet::new(),
                    folder: store.clone(),
                    error: Some(error.to_string()),
                },
            };
            self.extension
                .browser_configured
                .insert(id.clone(), metadata);
        }
        self.present_plugin_browser();
    }

    fn browser_plugins(&self) -> BTreeMap<String, BrowserPlugin> {
        let mut plugins = self.extension.browser_packages.clone();
        plugins.extend(self.extension.browser_configured.clone());
        for index in self.extension.contributions().ordered() {
            plugins
                .entry(index.owner.clone())
                .or_insert_with(|| BrowserPlugin {
                    version: index.version.clone(),
                    capabilities: index.capabilities.clone(),
                    folder: PathBuf::from(&index.source)
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .to_owned(),
                    error: None,
                });
        }
        for id in self.extension.statuses().keys() {
            plugins.entry(id.clone()).or_insert_with(|| BrowserPlugin {
                version: "?".into(),
                capabilities: BTreeSet::new(),
                folder: PathBuf::new(),
                error: Some("manifest information unavailable".into()),
            });
        }
        plugins
    }

    fn plugin_disabled(&self, id: &str) -> bool {
        self.extension.disabled_plugins.contains(id)
            || (!self.extension.enabled_plugins.contains(id)
                && self
                    .configuration
                    .settings()
                    .plugins
                    .entries
                    .get(id)
                    .is_some_and(|entry| !entry.enabled))
    }

    pub(super) fn present_plugin_browser(&mut self) {
        let mut items = self
            .browser_plugins()
            .into_iter()
            .take(4096)
            .map(|(id, plugin)| {
                let status = if plugin.error.is_some() && !self.plugin_disabled(&id) {
                    "failed".into()
                } else {
                    status_label(
                        self.extension.statuses().get(&id),
                        self.plugin_disabled(&id),
                        self.workspace.safe_mode(),
                    )
                };
                // The full failure belongs in the status action, not a clipped list row.
                let status = status
                    .split_once(": ")
                    .map_or(status.as_str(), |(state, _)| state);
                row(
                    format!("plugin:{id}"),
                    format!("{id} {}", plugin.version),
                    format!(
                        "{status} · {}",
                        if plugin.error.is_some() {
                            "permissions unavailable".into()
                        } else {
                            permissions(&plugin.capabilities)
                        }
                    ),
                    PickerPayload::Plugin(PluginChoice::Details(id)),
                )
            })
            .collect::<Vec<_>>();
        let empty = items.is_empty();
        items.push(row(
            "registry".into(),
            self.message("plugin_browser_registry", &[]),
            String::new(),
            PickerPayload::Command("package.registry.search".into()),
        ));
        self.install_surface_picker_state(
            crate::ui::PickerSurfaceKind::Generic,
            self.message("plugin_picker_title", &[]),
            items,
            String::new(),
            true,
            PickerOrigin::Plugins,
        );
        self.search.set_picker_footer(self.message(
            if empty {
                "plugin_picker_empty"
            } else {
                "plugin_browser_footer"
            },
            &[],
        ));
        self.sync_picker_ui();
    }

    pub(in crate::app) fn refresh_plugin_browser(&mut self) {
        if self.search.plugin_browser_open() {
            self.present_plugin_browser();
        }
    }

    pub(in crate::app) fn accept_plugin_choice(&mut self, choice: PluginChoice) {
        match choice {
            PluginChoice::Details(id) => {
                let Some(plugin) = self.browser_plugins().remove(&id) else {
                    self.show_error("plugin is no longer installed");
                    return;
                };
                let disabled = self.plugin_disabled(&id)
                    || matches!(
                        self.extension.statuses().get(&id),
                        Some(PluginStatus::Disabled)
                    );
                let mut items = Vec::new();
                for (key, action) in [
                    (
                        if disabled {
                            "plugin_browser_enable"
                        } else {
                            "plugin_browser_disable"
                        },
                        if disabled {
                            PluginChoice::Enable(id.clone())
                        } else {
                            PluginChoice::Disable(id.clone())
                        },
                    ),
                    ("plugin_browser_reload", PluginChoice::Reload(id.clone())),
                    ("plugin_browser_folder", PluginChoice::Folder(id.clone())),
                ] {
                    items.push(row(
                        key.into(),
                        self.message(key, &[]),
                        String::new(),
                        PickerPayload::Plugin(action),
                    ));
                }
                items.push(row(
                    "back".into(),
                    self.message("plugin_browser_back", &[]),
                    String::new(),
                    PickerPayload::Command("plugin.list".into()),
                ));
                items.push(row("permissions".into(), self.message("plugin_browser_permissions", &[]), permissions(&plugin.capabilities), PickerPayload::Notice { title: format!("{id} permissions"), text: format!("{}\n\nProcess access runs programs with your ordinary user permissions; it is not an OS sandbox.\nFolder: {}", permissions(&plugin.capabilities), plugin.folder.display()) }));
                let status = status_label(
                    self.extension.statuses().get(&id),
                    disabled,
                    self.workspace.safe_mode(),
                );
                let reason = plugin.error.filter(|error| !status.contains(error));
                let text =
                    reason.map_or_else(|| status.clone(), |reason| format!("{status}\n{reason}"));
                items.push(row(
                    "status".into(),
                    self.message("plugin_browser_status", &[]),
                    status
                        .split_once(": ")
                        .map_or(status.as_str(), |(state, _)| state)
                        .into(),
                    PickerPayload::Notice {
                        title: format!("{id} status"),
                        text,
                    },
                ));
                self.install_picker(format!("{id} {}", plugin.version), items);
                self.search
                    .set_picker_footer(self.message("plugin_browser_session", &[]));
                self.sync_picker_ui();
            }
            PluginChoice::Disable(id) => {
                self.disable_browser_plugin(&id);
                self.present_plugin_browser();
            }
            PluginChoice::Enable(id) | PluginChoice::Reload(id) => {
                if self.workspace.safe_mode() {
                    self.show_error(self.message("safe_mode_blocked", &[]));
                    return;
                }
                if !self.browser_plugins().contains_key(&id) {
                    self.show_error("plugin is no longer installed");
                    return;
                }
                self.disable_browser_plugin(&id);
                self.extension.disabled_plugins.remove(&id);
                self.extension.enabled_plugins.insert(id.clone());
                let policy = self.configuration.settings().plugins.clone();
                if let Some(entry) = policy.entries.get(&id) {
                    let store = if policy.store.is_absolute() {
                        policy.store
                    } else {
                        crate::config::user_state_directory().join(policy.store)
                    };
                    if let Err(error) = self.extension.index_from_store(
                        id.clone(),
                        store,
                        entry.manifest.clone(),
                        entry.component.clone(),
                        self.plugin_limits(),
                    ) {
                        self.show_error(error);
                    }
                } else {
                    self.extension.package_refresh = true;
                }
                self.present_plugin_browser();
            }
            PluginChoice::Folder(id) => {
                if let Some(plugin) = self.browser_plugins().get(&id) {
                    if plugin.folder.is_dir() {
                        let folder = crate::files::canonicalize(&plugin.folder)
                            .unwrap_or_else(|_| plugin.folder.clone());
                        if let Some(id) = self
                            .workspace
                            .roots()
                            .iter()
                            .find(|root| root.identity.canonical_path == folder)
                            .map(|root| root.id)
                        {
                            self.activate_workspace_root(id);
                            self.open_explorer();
                        } else {
                            self.extension.browser_folder = Some(folder.clone());
                            self.add_workspace_root(folder);
                        }
                    } else {
                        self.show_error("plugin folder is unavailable");
                    }
                }
            }
        }
    }

    pub(super) fn finish_plugin_folder_open(&mut self) {
        if !self.workspace.root_operation_pending()
            && let Some(folder) = self.extension.browser_folder.take()
            && self.state.workspace_root == folder
        {
            self.open_explorer();
        }
    }

    pub(super) fn disable_browser_plugin(&mut self, id: &str) {
        let load = self.extension.load_id(id);
        if let Err(error) = self.extension.disable(id) {
            self.show_error(error);
            return;
        }
        self.extension.enabled_plugins.remove(id);
        if let Some(load) = load {
            self.remove_plugin_contributions(id, load);
        }
        self.commands.unregister_plugin(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_plugin(enabled: bool) -> (tempfile::TempDir, App) {
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("plugins");
        std::fs::create_dir(&store).unwrap();
        let core = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/plugins/completion/guest.core.wasm"
        ));
        let component = wit_component::ComponentEncoder::default()
            .module(core)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        std::fs::write(store.join("guest.wasm"), &component).unwrap();
        let manifest = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/plugins/completion/manifest.toml"
        ))
        .replace(&"0".repeat(64), &format!("{:x}", Sha256::digest(component)));
        std::fs::write(store.join("manifest.toml"), manifest).unwrap();
        let mut loaded = crate::config::load_bundled_config();
        loaded.config.plugins.store = store;
        loaded.config.plugins.entries.insert(
            "completion".into(),
            crate::config::PluginEntryConfig {
                manifest: "manifest.toml".into(),
                component: "guest.wasm".into(),
                enabled,
            },
        );
        let app = App::new(
            crate::app::startup::StartupTarget::Dashboard {
                root: root.path().into(),
            },
            loaded,
            false,
            root.path().join("state"),
            false,
            false,
            std::sync::Arc::new(crate::clock::SystemClock),
        )
        .unwrap();
        (root, app)
    }

    fn wait_loaded(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !matches!(
            app.extension.statuses().get("completion"),
            Some(PluginStatus::Active(_))
        ) {
            app.process_plugin_events();
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                app.extension.statuses()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn browser_enables_reloads_and_disables_a_configured_guest() {
        let (_root, mut app) = configured_plugin(false);
        app.open_plugin_browser();
        let item = app.search.picker().unwrap().selected_item().unwrap();
        assert_eq!(item.label, "completion 0.1.0");
        assert!(item.detail.contains("disabled"));
        assert!(item.detail.contains("read buffers, UI"));
        app.accept_picker();
        assert_eq!(
            app.search.picker().unwrap().selected_item().unwrap().label,
            "Enable for this session"
        );
        app.accept_picker();
        wait_loaded(&mut app);
        let first = app.extension.load_id("completion").unwrap();
        app.accept_plugin_choice(PluginChoice::Reload("completion".into()));
        wait_loaded(&mut app);
        assert_ne!(app.extension.load_id("completion"), Some(first));
        app.open_plugin_browser();
        app.search.picker_insert("comp");
        app.refresh_plugin_browser();
        assert_eq!(app.search.picker().unwrap().query(), "comp");
        app.accept_plugin_choice(PluginChoice::Disable("completion".into()));
        app.load_configured_plugins();
        assert!(matches!(
            app.extension.statuses().get("completion"),
            Some(PluginStatus::Disabled)
        ));
        assert!(app.extension.load_id("completion").is_none());
        assert!(
            !app.commands
                .specs()
                .any(|spec| spec.id.as_str() == "plugin.completion.enable")
        );
    }

    #[test]
    fn browser_keeps_failed_plugins_visible_with_their_reason() {
        let (root, mut app) = configured_plugin(true);
        std::fs::write(root.path().join("plugins/guest.wasm"), "not a component").unwrap();
        app.load_configured_plugins();
        app.open_plugin_browser();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !matches!(
            app.extension.statuses().get("completion"),
            Some(PluginStatus::Failed(_))
        ) {
            app.process_plugin_events();
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                app.extension.statuses()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let item = app.search.picker().unwrap().selected_item().unwrap();
        assert!(item.detail.starts_with("failed ·"), "{}", item.detail);
        let Some(PluginStatus::Failed(reason)) = app.extension.statuses().get("completion") else {
            panic!("expected failed plugin")
        };
        let reason = reason.clone();
        app.window_session.observe_size((160, 45));
        app.accept_plugin_choice(PluginChoice::Details("completion".into()));
        app.search.picker_insert("Status and failure reason");
        app.accept_picker();
        let text = app
            .input
            .documentation()
            .unwrap()
            .sections
            .iter()
            .flat_map(|section| &section.lines)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(&reason), "{text}");
        assert_eq!(text.matches(&reason).count(), 1, "{text}");
    }

    #[test]
    fn browser_folder_action_uses_the_workspace_and_explorer_owners() {
        let (root, mut app) = configured_plugin(false);
        app.state
            .open_text(crate::editor::text::TextBuffer::from_text("draft"), None)
            .unwrap();
        let buffer = app.state.active_buffer_id().unwrap();
        app.open_plugin_browser();
        app.accept_plugin_choice(PluginChoice::Folder("completion".into()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.state.workspace_root != root.path().join("plugins") {
            app.process_trust_events();
            app.process_plugin_events();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(app.state.workspace_root, root.path().join("plugins"));
        assert_eq!(app.state.buffers[buffer].text.to_string(), "draft");
        assert!(app.ui.explorer.is_some());
    }

    #[test]
    fn browser_enable_cannot_bypass_safe_mode() {
        let (_root, mut app) = configured_plugin(false);
        app.workspace.fixture_safe_mode(true);
        app.open_plugin_browser();
        app.accept_plugin_choice(PluginChoice::Enable("completion".into()));
        assert!(app.extension.load_id("completion").is_none());
        assert!(!app.extension.enabled_plugins.contains("completion"));
    }

    #[test]
    fn browser_explains_states_and_permissions() {
        assert_eq!(
            status_label(Some(&PluginStatus::Active(vec![])), false, false),
            "loaded"
        );
        assert_eq!(status_label(None, true, false), "disabled");
        assert_eq!(
            status_label(
                Some(&PluginStatus::Failed("wrong digest".into())),
                false,
                false
            ),
            "failed: wrong digest"
        );
        assert_eq!(status_label(None, false, true), "disabled by safe mode");
        assert_eq!(
            permissions(&BTreeSet::from([
                PluginCapability::ProposeEdits,
                PluginCapability::ShowMessage,
                PluginCapability::Files,
                PluginCapability::Process,
                PluginCapability::Http
            ])),
            "edit buffers, UI, files, run processes, network"
        );
    }
}
