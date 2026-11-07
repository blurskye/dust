//! Tests for cross-feature application workflows.

pub(in crate::app) mod driver;
mod input_workflows;
pub(super) mod window_workflows;

use std::collections::BTreeMap;

use std::collections::BTreeSet;

use std::fs;

use std::thread;

use std::time::Duration;

use std::time::Instant;

use tempfile::tempdir;

use crate::app::App;

use crate::app::startup::StartupTarget;

#[test]
fn nix_workspace_fallback_is_reserved_for_nixos_hosts() {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("Rust analyzer catalog definition");
    assert_eq!(
        crate::app::tools::nix_workspace_shell_for_host(&definition, true, true),
        Some("rust-analyzer")
    );
    assert_eq!(
        crate::app::tools::nix_workspace_shell_for_host(&definition, true, false),
        None
    );
    assert_eq!(
        crate::app::tools::nix_workspace_shell_for_host(&definition, false, true),
        None
    );
}

#[test]
fn nix_lsp_launch_runs_the_processed_upstream_artifact_without_nix_evaluation() {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("Rust analyzer catalog definition");
    let root = tempdir().expect("isolated tool directory");
    let launch = crate::app::language::LspLaunch::NixWorkspace {
        nix: root.path().join("rust-analyzer/bin/rust-analyzer"),
        workspace_root: root.path().join("nix-lsp-workspace"),
        shell: "rust-analyzer".into(),
    };
    assert!(launch.arguments(&definition).is_empty());
    // The synthetic path has no profile, so environment resolution fails
    // closed rather than launching without compatibility.
    assert!(launch.environment(&definition).is_err());
}

pub(super) fn app() -> ((tempfile::TempDir, tempfile::TempDir), App) {
    app_with_optional_plugin_queue(None)
}

pub(in crate::app) fn app_with_plugin_queue(
    plugin_queue_capacity: usize,
) -> ((tempfile::TempDir, tempfile::TempDir), App) {
    app_with_optional_plugin_queue(Some(plugin_queue_capacity))
}

fn app_with_optional_plugin_queue(
    plugin_queue_capacity: Option<usize>,
) -> ((tempfile::TempDir, tempfile::TempDir), App) {
    let root = tempdir().expect("tempdir");
    let state = tempdir().expect("isolated state directory");
    let startup = StartupTarget::Dashboard {
        root: root.path().into(),
    };
    let mut loaded = crate::config::load_bundled_config();
    if let Some(plugin_queue_capacity) = plugin_queue_capacity {
        loaded.config.plugins.queue_capacity = plugin_queue_capacity;
    }
    let mut app = App::new(
        startup,
        loaded,
        false,
        state.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .expect("application construction");
    app.window_session.observe_size((80, 24));
    app.health.set_terminal_capabilities("test".into());
    ((root, state), app)
}

#[test]
fn application_fixture_cleans_up_workspace_and_state_after_drop() {
    let (dirs, app) = app();
    let root = dirs.0.path().to_path_buf();
    let state = dirs.1.path().to_path_buf();
    assert!(root.is_dir());
    assert!(state.is_dir());
    drop(app);
    drop(dirs);
    assert!(!root.exists());
    assert!(!state.exists());
}

pub(in crate::app) fn plugin_manifest(
    owner: &str,
    mapping_id: &str,
    keys: &str,
) -> crate::plugins::PluginManifest {
    crate::plugins::PluginManifest {
        id: owner.into(),
        version: "1.0.0".into(),
        component_sha256: "0".repeat(64),
        capabilities: BTreeSet::new(),
        commands: [(
            "run".into(),
            crate::plugins::PluginCommandDeclaration {
                title: format!("Run {owner}"),
                description: String::new(),
                arguments: BTreeMap::new(),
                context: crate::plugins::PluginCommandContext::Application,
                activation: crate::plugins::PluginActivationBehavior::ActiveOnly,
            },
        )]
        .into_iter()
        .collect(),
        contributions: crate::plugins::PluginContributions {
            schema_version: 1,
            input_contexts: vec![crate::plugins::PluginInputContextContribution {
                id: format!("plugin.{owner}.chat"),
                title: format!("{owner} chat"),
                parent_surface: Some("panel".into()),
            }],
            keymaps: vec![crate::plugins::PluginKeymapContribution {
                id: mapping_id.into(),
                context: "dashboard".into(),
                mode: String::new(),
                keys: keys.into(),
                command: format!("plugin.{owner}.run"),
                arguments: BTreeMap::new(),
            }],
            key_groups: Vec::new(),
            content_types: vec![crate::plugins::PluginContentTypeContribution {
                id: format!("plugin.{owner}.panel"),
                title: format!("{owner} panel"),
                presenter: crate::plugins::PluginPresenterKind::RetainedDeclarative,
                default_placement: crate::plugins::PluginWindowPlacementKind::Floating,
                allowed_placements: [
                    crate::plugins::PluginWindowPlacementKind::Floating,
                    crate::plugins::PluginWindowPlacementKind::Hidden,
                ]
                .into_iter()
                .collect(),
                focusable: true,
                maximum_instances: 1,
                context: format!("plugin.{owner}.chat"),
                command: format!("plugin.{owner}.run"),
                private_cells: None,
                provenance: None,
            }],
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        },
    }
}

pub(in crate::app) fn write_plugin_manifest(
    root: &std::path::Path,
    manifest: &crate::plugins::PluginManifest,
) {
    fs::write(
        root.join("manifest.toml"),
        toml::to_string(manifest).expect("manifest TOML"),
    )
    .expect("manifest");
}

pub(in crate::app) fn wait_vfs(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.documents.has_work() {
        app.queue_startup_reads();
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "VFS completion timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

pub(in crate::app) fn wait_workspace_approval(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace_edits.proposal().is_some() {
        app.process_workspace_approval();
        assert!(
            Instant::now() < deadline,
            "workspace approval timed out: {:?}",
            app.ui.notification
        );
        thread::sleep(Duration::from_millis(2));
    }
}

pub(in crate::app) fn prepare_trust_identity(app: &mut App) {
    let identity =
        crate::files::RootIdentity::capture(&app.state.workspace_root).expect("identity");
    let root = app.workspace.active_root().expect("root");
    app.workspace
        .fixture_identity(root, identity)
        .expect("replace identity");
    app.reconcile_workspace_trust();
}

pub(in crate::app) fn wait_trust(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace.persist_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline, "trust persistence timed out");
        thread::yield_now();
    }
}
