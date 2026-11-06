use crate::app::documents::configured_file_bytes;
mod clipboard;
mod configuration;
mod contributions;
mod driver;
mod editing;
mod extensions;
mod input_workflows;
mod language;
mod language_features;
pub(super) mod project;
mod surfaces;
mod viewport;
mod window_workflows;

use std::collections::BTreeMap;

use std::collections::BTreeSet;

use std::collections::HashMap;

use std::fs;

use std::path::PathBuf;

use std::thread;

use std::time::Duration;

use std::time::Instant;

use crate::editor::input::InputContextLayer;

use crate::editor::input::KeyEvent;

use crate::editor::input::MappingResolution;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::editor::CommandSource;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use crate::files::ExplorerWatchBatch;
use crate::files::ExplorerWatchGeneration;
use crate::ui::ContentTabDiagnostic;
use crate::ui::ContentTabDiagnosticSeverity;
use crate::ui::ContentTabKind;
use crate::ui::DocumentationMarkupKind;
use crate::ui::DocumentationPopupKind;
use crate::ui::ExplorerSegmentRole;
use crate::ui::ExplorerUiState;
use crate::ui::PickerSurfaceKind;
use crate::ui::PtyScreen;
use crate::ui::UiEvent;
use crate::ui::UiFold;
use crate::ui::VisualRowAnchor;
use crate::ui::VisualRowModel;
use crate::ui::active_pane_viewport;
use crate::ui::explorer_panel_width;
use crate::ui::render;
use crate::ui::snap_display_column;
use tempfile::tempdir;

use crate::app::App;

use crate::app::KeymapOperation;

use crate::app::LspProjectionOwner;

use crate::app::LspRuntimeSource;

use crate::app::build_restored_layout;

use crate::app::capture_persisted_layout;

use crate::app::compile_keymaps;

use crate::app::compile_keymaps_with_plugin_catalog;

use crate::app::configuration_generation_is_newer;

use crate::app::lsp_runtime_source_allowed_on_host;

use crate::app::reload_keymaps;

use crate::app::resolve_configuration;

use crate::app::validate_configured_commands;
use crate::app::AppEffect;
use crate::app::PickerKind;
use crate::app::startup::StartupTarget;
use crate::extensions::PluginLoadId;
use crate::persistence::PersistedWindowPlacement;
use crate::persistence::PersistedWindowRule;
use crate::process::PtyId;
use crate::tools::ToolKind;
use crate::tools::ToolStatus;
use crate::tools::is_nixos_host;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use crate::workspace::DiagnosticSeverity;
use crate::workspace::WorkspaceTrust;

#[test]
fn nix_workspace_fallback_is_reserved_for_nixos_hosts() {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("Rust analyzer catalog definition");
    assert_eq!(
        crate::app::nix_workspace_shell_for_host(&definition, true, true),
        Some("rust-analyzer")
    );
    assert_eq!(
        crate::app::nix_workspace_shell_for_host(&definition, true, false),
        None
    );
    assert_eq!(
        crate::app::nix_workspace_shell_for_host(&definition, false, true),
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
    let launch = crate::app::LspLaunch::NixWorkspace {
        nix: root.path().join("rust-analyzer/bin/rust-analyzer"),
        workspace_root: root.path().join("nix-lsp-workspace"),
        shell: "rust-analyzer".into(),
    };
    assert!(launch.arguments(&definition).is_empty());
    // The synthetic path has no profile, so environment resolution fails
    // closed rather than launching without compatibility.
    assert!(launch.environment(&definition).is_err());
}

pub(super) fn app() -> (
    (tempfile::TempDir, tempfile::TempDir),
    App,
) {
    app_with_optional_plugin_queue(None)
}

fn app_with_plugin_queue(
    plugin_queue_capacity: usize,
) -> (
    (tempfile::TempDir, tempfile::TempDir),
    App,
) {
    app_with_optional_plugin_queue(Some(plugin_queue_capacity))
}

fn app_with_optional_plugin_queue(
    plugin_queue_capacity: Option<usize>,
) -> (
    (tempfile::TempDir, tempfile::TempDir),
    App,
) {
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
    app.notices.set_terminal_capabilities("test".into());
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

fn plugin_manifest(owner: &str, mapping_id: &str, keys: &str) -> crate::extensions::PluginManifest {
    crate::extensions::PluginManifest {
        id: owner.into(),
        version: "1.0.0".into(),
        component_sha256: "0".repeat(64),
        capabilities: BTreeSet::new(),
        commands: [(
            "run".into(),
            crate::extensions::PluginCommandDeclaration {
                title: format!("Run {owner}"),
                description: String::new(),
                arguments: BTreeMap::new(),
                context: crate::extensions::PluginCommandContext::Application,
                activation: crate::extensions::PluginActivationBehavior::ActiveOnly,
            },
        )]
        .into_iter()
        .collect(),
        contributions: crate::extensions::PluginContributions {
            schema_version: 1,
            input_contexts: vec![crate::extensions::PluginInputContextContribution {
                id: format!("plugin.{owner}.chat"),
                title: format!("{owner} chat"),
                parent_surface: Some("panel".into()),
            }],
            keymaps: vec![crate::extensions::PluginKeymapContribution {
                id: mapping_id.into(),
                context: "dashboard".into(),
                mode: String::new(),
                keys: keys.into(),
                command: format!("plugin.{owner}.run"),
                arguments: BTreeMap::new(),
            }],
            key_groups: Vec::new(),
            content_types: vec![crate::extensions::PluginContentTypeContribution {
                id: format!("plugin.{owner}.panel"),
                title: format!("{owner} panel"),
                presenter: crate::extensions::PluginPresenterKind::RetainedDeclarative,
                default_placement: crate::extensions::PluginWindowPlacementKind::Floating,
                allowed_placements: [
                    crate::extensions::PluginWindowPlacementKind::Floating,
                    crate::extensions::PluginWindowPlacementKind::Hidden,
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

fn write_plugin_manifest(root: &std::path::Path, manifest: &crate::extensions::PluginManifest) {
    fs::write(
        root.join("manifest.toml"),
        toml::to_string(manifest).expect("manifest TOML"),
    )
    .expect("manifest");
}

fn wait_vfs(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.documents.has_work() {
        app.queue_startup_reads();
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "VFS completion timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

fn wait_workspace_approval(app: &mut App) {
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

fn prepare_trust_identity(app: &mut App) {
    let identity =
        crate::workspace::RootIdentity::capture(&app.state.workspace_root).expect("identity");
    let root = app.workspace.active_root().expect("root");
    app.workspace
        .fixture_identity(root, identity)
        .expect("replace identity");
    app.reconcile_workspace_trust();
}

fn wait_trust(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace.persist_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline, "trust persistence timed out");
        thread::yield_now();
    }
}
