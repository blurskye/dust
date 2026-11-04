use crate::editor::session::documents::configured_file_bytes;
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

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use crate::editor::input::{InputContextLayer, KeyEvent, MappingResolution};
use crate::editor::text::{ByteOffset, ByteRange, TextBuffer};
use crate::editor::{CommandSource, CommandValue, InvocationSource};
use crate::files::{ExplorerWatchBatch, ExplorerWatchGeneration};
use crate::ui::{
    ContentTabDiagnostic, ContentTabDiagnosticSeverity, ContentTabKind, DocumentationMarkupKind,
    DocumentationPopupKind, ExplorerSegmentRole, ExplorerUiState, PickerSurfaceKind, PtyScreen,
    UiEvent, UiFold, VisualRowAnchor, VisualRowModel, active_pane_viewport, explorer_panel_width,
    render, snap_display_column,
};
use tempfile::tempdir;

use super::{
    InteractiveApplication, KeymapOperation, LspProjectionOwner, LspRuntimeSource,
    build_restored_layout, capture_persisted_layout, compile_keymaps,
    compile_keymaps_with_plugin_catalog, configuration_generation_is_newer,
    lsp_runtime_source_allowed_on_host, reload_keymaps, resolve_configuration,
    validate_configured_commands,
};
use crate::editor::{AppEffect, PickerKind, StartupTarget};
use crate::{
    extensions::PluginLoadId,
    persistence::{PersistedWindowPlacement, PersistedWindowRule},
    process::PtyId,
    tools::{ToolKind, ToolStatus, is_nixos_host},
    ui::window::{DockEdge, WindowPlacement, WindowRect},
    workspace::{DiagnosticSeverity, WorkspaceTrust},
};

#[test]
fn nix_workspace_fallback_is_reserved_for_nixos_hosts() {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("Rust analyzer catalog definition");
    assert_eq!(
        super::nix_workspace_shell_for_host(&definition, true, true),
        Some("rust-analyzer")
    );
    assert_eq!(
        super::nix_workspace_shell_for_host(&definition, true, false),
        None
    );
    assert_eq!(
        super::nix_workspace_shell_for_host(&definition, false, true),
        None
    );
}

#[test]
fn nix_lsp_launch_runs_the_processed_upstream_artifact_without_nix_evaluation() {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("Rust analyzer catalog definition");
    let launch = super::LspLaunch::NixWorkspace {
        nix: PathBuf::from("/home/user/.local/state/dust/tools/rust-analyzer/bin/rust-analyzer"),
        workspace_root: PathBuf::from("/home/user/.local/state/dust/tools/nix-lsp-workspace"),
        shell: "rust-analyzer".into(),
    };
    assert!(launch.arguments(&definition).is_empty());
    // The synthetic path has no profile, so environment resolution fails
    // closed rather than launching without compatibility.
    assert!(launch.environment(&definition).is_err());
}

pub(super) fn app() -> InteractiveApplication {
    app_with_optional_plugin_queue(None)
}

fn app_with_plugin_queue(plugin_queue_capacity: usize) -> InteractiveApplication {
    app_with_optional_plugin_queue(Some(plugin_queue_capacity))
}

fn app_with_optional_plugin_queue(plugin_queue_capacity: Option<usize>) -> InteractiveApplication {
    let root = tempdir().expect("tempdir").keep();
    let startup = StartupTarget::Dashboard { root: root.clone() };
    let mut loaded = crate::config::load_bundled_config();
    if let Some(plugin_queue_capacity) = plugin_queue_capacity {
        loaded.config.plugins.queue_capacity = plugin_queue_capacity;
    }
    let mut app = InteractiveApplication::new(
        startup,
        loaded,
        false,
        tempdir().expect("isolated state directory").keep(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .expect("application construction");
    app.window_session.observe_size((80, 24));
    app.notices.set_terminal_capabilities("test".into());
    app
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

fn wait_vfs(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.documents.has_work() {
        app.queue_startup_reads();
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "VFS completion timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

fn prepare_trust_identity(app: &mut InteractiveApplication) {
    let identity =
        crate::workspace::RootIdentity::capture(&app.state.workspace_root).expect("identity");
    let root = app.workspace.active_root().expect("root");
    app.workspace
        .fixture_identity(root, identity)
        .expect("replace identity");
    app.reconcile_workspace_trust();
}

fn wait_trust(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace.persist_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline, "trust persistence timed out");
        thread::yield_now();
    }
}
