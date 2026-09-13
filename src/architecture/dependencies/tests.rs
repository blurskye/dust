use super::*;

fn check(extra: &[(&str, &str)], service: &str) -> Result<(), ArchitectureError> {
    let fixture = tempfile::tempdir().unwrap();
    let base = [
        (
            "src/lib.rs",
            "extern crate self as local_crate; pub mod editor; pub mod language; pub mod facade;",
        ),
        (
            "src/editor/mod.rs",
            "pub mod state { pub struct EditorState { pub workspace_root: String } } pub use state::EditorState; pub mod text { pub struct TextBuffer; }",
        ),
        ("src/language/mod.rs", "pub mod lsp;"),
        (
            "src/facade.rs",
            "pub mod nested { pub use crate::editor::EditorState as Actor; } pub use nested::Actor; pub type Wrapped = Option<Box<Actor>>;",
        ),
        ("src/language/lsp.rs", service),
    ];
    let mut paths = BTreeSet::new();
    for (path, source) in base.into_iter().chain(extra.iter().copied()) {
        let path = fixture.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        paths.insert(path);
    }
    audit(fixture.path(), &paths.into_iter().collect::<Vec<_>>())
}

#[test]
fn canonical_services_reject_direct_local_alias_reexport_glob_and_macro_authority() {
    for source in [
        "pub fn bad(s: &mut crate::editor::EditorState) { s.workspace_root.clear(); }",
        "use crate::editor::EditorState as Shared; pub fn bad(s: &mut Shared) { s.workspace_root.clear(); }",
        "pub fn bad() { use crate::editor::EditorState as Shared; let mut s = Shared { workspace_root: String::new() }; s.workspace_root.clear(); }",
        "use crate::editor as model; pub fn bad(s: &mut model::EditorState) { s.workspace_root.clear(); }",
        "use crate::facade::nested::{self as model}; pub fn bad(s: &mut model::Actor) { s.workspace_root.clear(); }",
        "pub fn bad(_: crate::facade::Wrapped) {}",
        "pub fn outer() { mod nested { pub fn bad(s: &mut crate::editor::EditorState) { s.workspace_root.clear(); } } }",
        "const _: () = { mod nested { pub fn bad(s: &mut crate::editor::EditorState) { s.workspace_root.clear(); } } };",
        "type Local = crate::facade::nested::Actor; pub fn bad(s: &mut Local) { s.workspace_root.clear(); }",
        "macro_rules! ty { ($t:ty) => { $t } } pub fn bad(_: ty!(&mut crate::editor::EditorState)) {}",
        "pub fn bad(s: &mut ::local_crate::editor::EditorState) { s.workspace_root.clear(); }",
        "pub fn bad() { let mut s = crate::editor::EditorState { workspace_root: String::new() }; s.workspace_root.clear(); }",
    ] {
        let error = check(&[], source).expect_err(source);
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("forbidden authority crate::editor")),
            "{source}: {error}"
        );
    }
    let source = "use crate::facade::*; pub fn bad(s: &mut Actor) { s.workspace_root.clear(); }";
    let error = check(
        &[
            (
                "src/lib.rs",
                "extern crate self as local_crate; pub mod editor; pub mod language; pub mod facade; pub mod process;",
            ),
            ("src/process.rs", source),
        ],
        "pub fn service() {}",
    )
    .expect_err(source);
    assert!(
        matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard")),
        "{source}: {error}"
    );
}

#[test]
fn real_module_paths_cannot_launder_service_ownership() {
    for (declaration, path) in [
        (
            "#[path = \"../shared.rs\"] pub mod nested;",
            "src/shared.rs",
        ),
        (
            "pub mod nested { #[path = \"shared.rs\"] pub mod inner; }",
            "src/language/lsp/nested/shared.rs",
        ),
    ] {
        let error = check(
            &[(
                path,
                "pub fn bad(s: &mut crate::editor::EditorState) { s.workspace_root.clear(); }",
            )],
            declaration,
        )
        .unwrap_err();
        assert!(
            matches!(error, ArchitectureError::Dependency { detail, .. } if detail.contains("forbidden authority crate::editor"))
        );
    }
}

#[test]
fn legitimate_domain_clients_comments_strings_and_test_only_code_are_not_authority_edges() {
    check(
        &[],
        r#"
        use crate::editor::text::TextBuffer;
        pub fn process(_: &TextBuffer) { let _ = "crate::editor::EditorState"; }
        // crate::editor::EditorState is explanatory text, not a dependency.
        #[cfg(test)] fn fixture(_: &mut crate::editor::EditorState) {}
        #[cfg(all(unix, test))] mod tests { use crate::editor::EditorState; }
    "#,
    )
    .unwrap();
    // This is a public editor client, not a service, and remains intentionally legal.
    check(&[("src/facade.rs", "pub fn public_client(s: &mut crate::editor::EditorState) { s.workspace_root.clear(); }")], "pub fn service() {}").unwrap();
}

#[test]
fn migrated_owner_and_composition_globs_fail_closed_but_test_only_globs_do_not() {
    for source in [
        "pub mod session { pub mod input { use super::*; } }",
        "pub mod session { pub mod dispatch { use super::*; } }",
        "pub mod session { pub mod dispatch { pub use super::*; } }",
        "pub mod session { pub mod dispatch { pub fn nested() { use super::*; } } }",
        "pub mod session { pub mod dispatch { pub mod commands { use super::*; } } }",
        "pub mod session { pub mod dispatch { pub mod commands { pub use super::*; } } }",
    ] {
        let error = check(&[("src/editor/mod.rs", source)], "pub fn service() {}").unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard") && (detail.contains("imports and exports explicitly") || detail.contains("must name every export explicitly"))),
            "{source}: {error}"
        );
    }
    let remounted = check(
        &[
            (
                "src/editor/mod.rs",
                "#[path = \"session/commands/remounted.rs\"] pub mod remounted;",
            ),
            ("src/editor/session/commands/remounted.rs", "use super::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(remounted, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard"))
    );
    let remounted = check(
        &[
            (
                "src/editor/mod.rs",
                "#[path = \"session/dispatch/remounted.rs\"] pub mod remounted;",
            ),
            (
                "src/editor/session/dispatch/remounted.rs",
                "pub use super::*;",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(remounted, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard"))
    );
    check(
        &[(
            "src/editor/mod.rs",
            r#"
            pub mod session {
                pub mod dispatch {
                    #[cfg(test)] use super::*;
                    #[cfg(all(unix, test))] pub use super::*;
                    #[cfg(test)] mod tests { use super::*; }
                    pub mod commands {
                        #[cfg(test)] use super::*;
                        #[cfg(all(unix, test))] pub use super::*;
                        #[cfg(test)] mod tests { use super::*; }
                    }
                }
            }
            "#,
        )],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn configuration_globs_fail_by_logical_physical_and_block_scope_but_test_globs_do_not() {
    for source in [
        "use crate::*; pub fn invalid() {}",
        "pub fn invalid() { use crate::*; }",
    ] {
        let error = check(
            &[
                (
                    "src/lib.rs",
                    "extern crate self as local_crate; pub mod config; pub mod editor; pub mod language; pub mod facade;",
                ),
                ("src/config.rs", source),
            ],
            "pub fn service() {}",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("configuration") && detail.contains("imports and exports explicitly")),
            "{source}: {error}"
        );
    }
    let remounted = check(
        &[
            (
                "src/facade.rs",
                "#[path = \"config/remounted.rs\"] pub mod remounted;",
            ),
            ("src/config/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&remounted, ArchitectureError::Dependency { detail, .. } if detail.contains("configuration") && detail.contains("imports and exports explicitly")),
        "{remounted}"
    );
    check(
        &[
            (
                "src/lib.rs",
                "extern crate self as local_crate; pub mod config; pub mod editor; pub mod language; pub mod facade;",
            ),
            (
                "src/config.rs",
                "use std::path::Path; #[cfg(test)] use crate::*; pub fn valid(_: &Path) {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn configuration_cannot_import_higher_authority_or_launder_it_through_remounts() {
    let library = "extern crate self as local_crate; pub mod api; pub mod config; pub mod editor; pub mod language; pub mod facade;";
    let rejected = |result: Result<(), ArchitectureError>| {
        let error = result.unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("forbidden authority crate::api")),
            "{error}"
        );
    };
    for source in [
        "pub fn bad(_: crate::api::ApiManifest) {}",
        "use crate::api::ApiManifest; pub fn bad(_: ApiManifest) {}",
        "use crate::{api::{ApiManifest as Manifest}}; pub fn bad(_: Manifest) {}",
        "pub use crate::api::ApiManifest as Manifest;",
        "type Manifest = crate::api::ApiManifest; pub fn bad(_: Manifest) {}",
        "pub fn bad(_: fn(crate::api::ApiManifest)) {}",
        "macro_rules! authority { () => { crate::api::ApiManifest } } pub fn bad(_: authority!()) {}",
        "pub mod nested { pub fn bad(_: crate::api::ApiManifest) {} }",
        "#[cfg(any(unix, test))] pub fn bad(_: crate::api::ApiManifest) {}",
    ] {
        rejected(check(
            &[
                ("src/lib.rs", library),
                ("src/api.rs", "pub struct ApiManifest;"),
                ("src/config.rs", source),
            ],
            "pub fn service() {}",
        ));
    }
    rejected(check(
        &[
            (
                "src/lib.rs",
                "extern crate self as local_crate; pub mod api; #[path = \"shared.rs\"] pub mod config; pub mod editor; pub mod language; pub mod facade;",
            ),
            ("src/api.rs", "pub struct ApiManifest;"),
            ("src/shared.rs", "pub fn bad(_: crate::api::ApiManifest) {}"),
        ],
        "pub fn service() {}",
    ));
    rejected(check(
        &[
            ("src/lib.rs", library),
            ("src/api.rs", "pub struct ApiManifest;"),
            ("src/config.rs", "pub fn valid() {}"),
            (
                "src/facade.rs",
                "#[path = \"config/remounted.rs\"] pub mod remounted;",
            ),
            (
                "src/config/remounted.rs",
                "pub fn bad(_: crate::api::ApiManifest) {}",
            ),
        ],
        "pub fn service() {}",
    ));
    check(
        &[
            ("src/lib.rs", library),
            ("src/api.rs", "pub struct ApiManifest;"),
            (
                "src/config.rs",
                "#[cfg(test)] pub fn test_only(_: crate::api::ApiManifest) {} #[cfg(all(unix, test))] mod tests { pub fn nested(_: crate::api::ApiManifest) {} } pub fn valid() {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

fn check_editor_ui(extra: &[(&str, &str)]) -> Result<(), ArchitectureError> {
    let fixture = tempfile::tempdir().unwrap();
    let base = [
        (
            "src/lib.rs",
            "extern crate self as local_crate; pub mod config; pub mod editor; pub mod facade; pub mod files; pub mod language; pub mod ui; pub mod workspace;",
        ),
        (
            "src/editor/mod.rs",
            "pub mod command; pub mod input; pub mod session; pub mod state; pub mod text; pub mod transaction;",
        ),
        ("src/editor/command/mod.rs", "pub struct CommandInvocation;"),
        ("src/editor/input/mod.rs", "pub struct KeyEvent;"),
        ("src/editor/session/mod.rs", "pub struct SessionAuthority;"),
        ("src/editor/state/mod.rs", "pub struct EditorState;"),
        ("src/editor/text/mod.rs", "pub struct TextBuffer;"),
        (
            "src/editor/transaction/mod.rs",
            "pub struct EditorTransaction;",
        ),
        (
            "src/config.rs",
            "pub struct Config; pub fn load_bundled_config() -> Config { Config }",
        ),
        ("src/files.rs", "pub struct VfsWorker;"),
        (
            "src/facade.rs",
            "pub use crate::editor::state::EditorState as MutableEditor; pub use crate::files::VfsWorker as Worker; pub type WorkerAlias = Worker;",
        ),
        (
            "src/language.rs",
            "pub mod syntax { pub struct SyntaxRole; }",
        ),
        (
            "src/workspace.rs",
            "pub struct DiagnosticSeverity; pub struct FileSnapshot; pub struct ProposalState; pub struct WorkspaceEditProposal; pub mod git { pub struct GitRepositoryId; }",
        ),
        (
            "src/ui/mod.rs",
            "pub mod geometry; pub mod render; pub mod terminal; pub mod window;",
        ),
        ("src/ui/geometry.rs", "pub struct Geometry;"),
        ("src/ui/render.rs", "pub struct UiState;"),
        ("src/ui/terminal.rs", "pub struct TerminalSession;"),
        ("src/ui/window.rs", "pub struct WindowGraph;"),
    ];
    let mut paths = BTreeSet::new();
    for (path, source) in base.into_iter().chain(extra.iter().copied()) {
        let path = fixture.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        paths.insert(path);
    }
    audit(fixture.path(), &paths.into_iter().collect::<Vec<_>>())
}

#[test]
fn deterministic_editor_layers_reject_upward_authority_and_remount_laundering() {
    let rejected = |extra: &[(&str, &str)]| {
        let error = check_editor_ui(extra).unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("deterministic") && detail.contains("forbidden authority crate::files")),
            "{error}"
        );
    };
    for family in ["text", "state", "transaction", "command", "input"] {
        let path = format!("src/editor/{family}/mod.rs");
        rejected(&[(&path, "pub fn bad(_: &crate::files::VfsWorker) {}")]);
    }
    for source in [
        "use crate::files::VfsWorker as Worker; pub fn bad(_: &Worker) {}",
        "use crate::{files::{VfsWorker as Worker}}; pub fn bad(_: &Worker) {}",
        "pub use crate::files::VfsWorker as Worker;",
        "pub fn bad(_: &crate::facade::Worker) {}",
        "type Worker = crate::facade::WorkerAlias; pub fn bad(_: Worker) {}",
        "pub fn bad(_: fn(crate::files::VfsWorker)) {}",
        "pub fn bad(_: *mut crate::files::VfsWorker) {}",
        "pub fn bad(_: std::cell::RefCell<crate::files::VfsWorker>) {}",
        "macro_rules! ty { ($t:ty) => { $t } } pub fn bad(_: ty!(crate::files::VfsWorker)) {}",
        "pub mod nested { pub fn bad(_: &crate::files::VfsWorker) {} }",
        "#[cfg(any(unix, test))] pub fn bad(_: &crate::files::VfsWorker) {}",
        "pub fn bad(_: &::local_crate::files::VfsWorker) {}",
    ] {
        rejected(&[("src/editor/text/mod.rs", source)]);
    }
    rejected(&[
        (
            "src/editor/mod.rs",
            "pub mod command; pub mod input; pub mod session; pub mod state; #[path = \"../shared.rs\"] pub mod text; pub mod transaction;",
        ),
        (
            "src/shared.rs",
            "pub fn bad(_: &crate::files::VfsWorker) {}",
        ),
    ]);
    rejected(&[
        (
            "src/facade.rs",
            "#[path = \"editor/text/remounted.rs\"] pub mod remounted;",
        ),
        (
            "src/editor/text/remounted.rs",
            "pub fn bad(_: &crate::files::VfsWorker) {}",
        ),
    ]);

    check_editor_ui(&[
        (
            "src/editor/text/mod.rs",
            "pub struct TextBuffer; pub fn text(_: &TextBuffer) {} #[cfg(test)] pub fn fixture(_: &crate::files::VfsWorker) {}",
        ),
        (
            "src/editor/state/mod.rs",
            "pub struct EditorState; pub fn state(_: &crate::editor::text::TextBuffer) {}",
        ),
        (
            "src/editor/transaction/mod.rs",
            "pub struct EditorTransaction; pub fn transaction(_: &crate::editor::state::EditorState, _: &crate::workspace::WorkspaceEditProposal) {}",
        ),
        (
            "src/editor/command/mod.rs",
            "pub struct CommandInvocation; pub fn command(_: &crate::editor::state::EditorState, _: &crate::editor::transaction::EditorTransaction) {}",
        ),
        (
            "src/editor/input/mod.rs",
            "pub struct KeyEvent; pub fn input(_: &crate::editor::command::CommandInvocation) {}",
        ),
    ])
    .unwrap();
}

#[test]
fn ui_rejects_effect_owners_and_mutable_foreign_authority_with_dto_controls() {
    let effect_error = |extra: &[(&str, &str)]| {
        let error = check_editor_ui(extra).unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("deterministic Ui") && detail.contains("forbidden authority crate::files")),
            "{error}"
        );
    };
    for source in [
        "pub fn bad(_: &crate::files::VfsWorker) {}",
        "use crate::files::VfsWorker as Worker; pub fn bad(_: &Worker) {}",
        "pub fn bad(_: &crate::facade::Worker) {}",
        "type Worker = crate::facade::WorkerAlias; pub fn bad(_: Worker) {}",
        "pub fn bad(_: fn(crate::files::VfsWorker)) {}",
        "macro_rules! ty { ($t:ty) => { $t } } pub fn bad(_: ty!(crate::files::VfsWorker)) {}",
        "pub mod nested { pub fn bad(_: &crate::files::VfsWorker) {} }",
        "#[cfg(any(unix, test))] pub fn bad(_: &crate::files::VfsWorker) {}",
    ] {
        effect_error(&[("src/ui/render.rs", source)]);
    }
    let config_loader = check_editor_ui(&[(
        "src/ui/render.rs",
        "pub fn bad() { let _ = crate::config::load_bundled_config(); }",
    )])
    .unwrap_err();
    assert!(
        matches!(&config_loader, ArchitectureError::Dependency { detail, .. } if detail.contains("forbidden authority crate::config") && detail.contains("load_bundled_config")),
        "{config_loader}"
    );
    effect_error(&[
        (
            "src/ui/mod.rs",
            "pub mod geometry; #[path = \"../shared_ui.rs\"] pub mod render; pub mod terminal; pub mod window;",
        ),
        (
            "src/shared_ui.rs",
            "pub fn bad(_: &crate::files::VfsWorker) {}",
        ),
    ]);
    effect_error(&[
        (
            "src/facade.rs",
            "#[path = \"ui/render/remounted.rs\"] pub mod remounted;",
        ),
        (
            "src/ui/render/remounted.rs",
            "pub fn bad(_: &crate::files::VfsWorker) {}",
        ),
    ]);

    let mutable_error = |path: &str, source: &str, authority: &str| {
        let error = check_editor_ui(&[(path, source)]).unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("UI owner") && detail.contains("mutable authority") && detail.contains(authority)),
            "{source}: {error}"
        );
    };
    for source in [
        "pub fn bad(_: &mut crate::editor::state::EditorState) {}",
        "use crate::editor::state::EditorState as State; pub fn bad(_: &mut State) {}",
        "use crate::{editor::{state::EditorState as State}}; pub fn bad(_: &mut State) {}",
        "pub fn bad(_: &mut crate::facade::MutableEditor) {}",
        "type State = crate::facade::MutableEditor; pub fn bad(_: &mut State) {}",
        "pub fn bad(_: fn(&mut crate::editor::state::EditorState)) {}",
        "pub fn bad(_: *mut crate::editor::state::EditorState) {}",
        "pub fn bad(_: std::cell::RefCell<crate::editor::state::EditorState>) {}",
        "struct Local(crate::editor::state::EditorState); impl std::ops::Deref for Local { type Target = crate::editor::state::EditorState; fn deref(&self) -> &Self::Target { &self.0 } } impl std::ops::DerefMut for Local { fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 } }",
        "macro_rules! ty { ($t:ty) => { $t } } pub fn bad(_: ty!(&mut crate::editor::state::EditorState)) {}",
    ] {
        mutable_error("src/ui/render.rs", source, "EditorState");
    }
    mutable_error(
        "src/ui/render.rs",
        "pub fn bad(_: &mut crate::ui::window::WindowGraph) {}",
        "WindowGraph",
    );
    mutable_error(
        "src/ui/window.rs",
        "pub fn bad(_: &mut crate::ui::render::UiState) {}",
        "UiState",
    );
    mutable_error(
        "src/ui/geometry.rs",
        "pub fn bad(_: &mut crate::config::Config) {}",
        "Config",
    );
    mutable_error(
        "src/ui/render.rs",
        "pub mod nested { pub fn bad(_: &mut crate::editor::state::EditorState) {} }",
        "EditorState",
    );
    mutable_error(
        "src/ui/render.rs",
        "#[cfg(any(unix, test))] pub fn bad(_: &mut crate::editor::state::EditorState) {}",
        "EditorState",
    );
    let remounted_mutable = check_editor_ui(&[
        (
            "src/facade.rs",
            "#[path = \"ui/render/remounted.rs\"] pub mod remounted;",
        ),
        (
            "src/ui/render/remounted.rs",
            "pub fn bad(_: &mut crate::ui::window::WindowGraph) {}",
        ),
    ])
    .unwrap_err();
    assert!(
        matches!(&remounted_mutable, ArchitectureError::Dependency { detail, .. } if detail.contains("UI owner") && detail.contains("mutable authority") && detail.contains("WindowGraph")),
        "{remounted_mutable}"
    );

    check_editor_ui(&[
        (
            "src/ui/render.rs",
            "pub struct UiState; pub fn render(_: &mut UiState, _: &crate::editor::state::EditorState, _: &crate::config::Config, _: crate::language::syntax::SyntaxRole, _: crate::workspace::DiagnosticSeverity, _: crate::workspace::git::GitRepositoryId) {} #[cfg(test)] pub fn fixture(_: &crate::files::VfsWorker) {}",
        ),
        (
            "src/ui/window.rs",
            "pub struct WindowGraph; pub fn graph(_: &mut WindowGraph) {}",
        ),
        (
            "src/ui/terminal.rs",
            "pub struct TerminalSession; pub fn terminal(_: &mut TerminalSession, _: &crate::editor::input::KeyEvent) {}",
        ),
    ])
    .unwrap();
}

#[test]
fn deterministic_source_classification_uses_logical_and_physical_roots() {
    let root = std::path::Path::new("/fixture");
    for (logical, physical, expected) in [
        (
            "crate::editor::text",
            "editor/text",
            DeterministicSource::Text,
        ),
        (
            "crate::editor::state",
            "editor/state",
            DeterministicSource::State,
        ),
        (
            "crate::editor::transaction",
            "editor/transaction",
            DeterministicSource::Transaction,
        ),
        (
            "crate::editor::command",
            "editor/command",
            DeterministicSource::Command,
        ),
        (
            "crate::editor::input",
            "editor/input",
            DeterministicSource::Input,
        ),
        ("crate::ui", "ui", DeterministicSource::Ui),
    ] {
        let path = root.join("src").join(physical).join("child.rs");
        assert_eq!(deterministic_source(root, logical, &path), Some(expected));
        assert_eq!(
            deterministic_source(root, "crate::facade::remounted", &path),
            Some(expected)
        );
    }
}

#[test]
fn session_owners_cannot_receive_or_expose_mutable_foreign_authority() {
    let library = "extern crate self as local_crate; pub mod editor; pub mod facade; pub mod language; pub mod ui;";
    let editor = "pub mod command { pub struct CommandRegistry<T>(pub T); } pub mod state { pub struct EditorState; } pub mod session;";
    let session = "pub mod configuration { pub mod model { pub struct ResolvedConfiguration; } } pub mod notifications; pub mod windows { pub struct NavigationStamp; pub struct WindowGraphLoan; pub struct WindowSession; }";
    let rejected = |source: &str| {
        let error = check(
            &[
                ("src/lib.rs", library),
                ("src/editor/mod.rs", editor),
                ("src/editor/session/mod.rs", session),
                ("src/editor/session/notifications.rs", source),
                (
                    "src/facade.rs",
                    "pub use crate::editor::session::windows::WindowSession as MutableWindows;",
                ),
                (
                    "src/ui/mod.rs",
                    "pub mod render { pub struct UiState; } pub mod window { pub struct WindowGraph; }",
                ),
            ],
            "pub fn service() {}",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("session owner crate::editor::session::notifications") && detail.contains("mutable authority")),
            "{source}: {error}"
        );
    };
    for source in [
        "pub fn bad(_: &mut super::windows::WindowSession) {}",
        "use super::windows::WindowSession as Foreign; pub fn bad(_: &mut Foreign) {}",
        "use crate::editor::{session::{windows::WindowSession as Foreign}}; pub fn bad(_: &mut Foreign) {}",
        "type Foreign = super::windows::WindowSession; pub fn bad(_: &mut Foreign) {}",
        "pub fn bad(_: &mut crate::facade::MutableWindows) {}",
        "pub fn bad(_: &mut ::local_crate::editor::session::windows::WindowSession) {}",
        "pub fn bad(_: fn(&mut super::windows::WindowSession)) {}",
        "pub fn bad(_: *mut super::windows::WindowSession) {}",
        "pub fn bad(_: std::cell::Cell<super::windows::WindowSession>) {}",
        "pub fn bad(_: std::cell::RefCell<super::windows::WindowSession>) {}",
        "pub fn bad(_: std::cell::RefMut<'_, super::windows::WindowSession>) {}",
        "pub fn bad(_: std::sync::Mutex<super::windows::WindowSession>) {}",
        "pub fn bad(_: std::sync::RwLock<super::windows::WindowSession>) {}",
        "pub fn bad(_: std::cell::UnsafeCell<super::windows::WindowSession>) {}",
        "pub fn bad(_: &mut dyn AsMut<super::windows::WindowSession>) {}",
        "macro_rules! ty { ($t:ty) => { $t } } pub fn bad(_: ty!(&mut super::windows::WindowSession)) {}",
        "pub mod nested { pub fn bad(_: &mut super::super::windows::WindowSession) {} }",
        "#[cfg(any(unix, test))] pub fn bad(_: &mut super::windows::WindowSession) {}",
        "pub fn bad(_: &mut crate::editor::state::EditorState) {}",
        "pub fn bad(_: &mut crate::ui::render::UiState) {}",
        "pub fn bad(_: &mut crate::ui::window::WindowGraph) {}",
        "pub fn bad(_: &mut crate::editor::command::CommandRegistry<()>) {}",
        "pub fn bad(_: &mut super::configuration::model::ResolvedConfiguration) {}",
    ] {
        rejected(source);
    }
    rejected(
        "struct Local(super::windows::WindowSession); impl std::ops::Deref for Local { type Target = super::windows::WindowSession; fn deref(&self) -> &Self::Target { &self.0 } } impl std::ops::DerefMut for Local { fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 } }",
    );
    rejected(
        "struct Local(super::windows::WindowSession); impl std::borrow::Borrow<super::windows::WindowSession> for Local { fn borrow(&self) -> &super::windows::WindowSession { &self.0 } } impl std::borrow::BorrowMut<super::windows::WindowSession> for Local { fn borrow_mut(&mut self) -> &mut super::windows::WindowSession { &mut self.0 } }",
    );
    rejected(
        "struct Local(super::windows::WindowSession); impl std::ops::Index<usize> for Local { type Output = super::windows::WindowSession; fn index(&self, _: usize) -> &Self::Output { &self.0 } } impl std::ops::IndexMut<usize> for Local { fn index_mut(&mut self, _: usize) -> &mut Self::Output { &mut self.0 } }",
    );

    check(
        &[
            ("src/lib.rs", library),
            ("src/editor/mod.rs", editor),
            (
                "src/editor/session/mod.rs",
                "pub mod notifications; pub mod windows;",
            ),
            (
                "src/editor/session/notifications.rs",
                "pub fn immutable(_: &super::windows::WindowSession, _: &super::windows::NavigationStamp) {} struct ExecutionLane; pub fn loan(_: &mut ExecutionLane) {} #[cfg(test)] pub fn test_only(_: &mut super::windows::WindowSession) {} #[cfg(all(unix, test))] mod tests { pub fn nested(_: &mut super::super::windows::WindowSession) {} }",
            ),
            (
                "src/editor/session/windows.rs",
                "pub struct WindowSession; pub struct NavigationStamp; pub fn own(_: &mut WindowSession, _: &mut crate::ui::window::WindowGraph) {}",
            ),
            (
                "src/ui/mod.rs",
                "pub mod window { pub struct WindowGraph; }",
            ),
            (
                "src/facade.rs",
                "pub fn public_client(_: &mut crate::editor::state::EditorState) {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn session_owner_classification_uses_logical_and_physical_module_identity() {
    let root = std::path::Path::new("/fixture");
    for (source, kind, owner) in SESSION_OWNERS {
        let logical = format!("crate::editor::session::{source}");
        let physical = root.join("src/editor/session").join(source);
        assert_eq!(
            session_owner(root, &logical, &physical.with_extension("rs")),
            Some(*owner)
        );
        assert_eq!(
            session_owner(root, "crate::facade::remounted", &physical.join("child.rs")),
            Some(*owner)
        );
        assert_eq!(
            mutable_authority_owner(&format!("{logical}::{kind}")),
            Some(*owner)
        );
    }

    let library =
        "extern crate self as local_crate; pub mod editor; pub mod facade; pub mod language;";
    let target = "pub struct WindowSession;";
    for extra in [
        vec![
            ("src/editor/mod.rs", "pub mod session;"),
            (
                "src/editor/session/mod.rs",
                "#[path = \"../../shared.rs\"] pub mod notifications; pub mod windows;",
            ),
            (
                "src/shared.rs",
                "pub fn bad(_: &mut crate::editor::session::windows::WindowSession) {}",
            ),
            ("src/editor/session/windows.rs", target),
        ],
        vec![
            ("src/editor/mod.rs", "pub mod session;"),
            (
                "src/editor/session/mod.rs",
                "pub mod notifications; pub mod windows;",
            ),
            ("src/editor/session/notifications.rs", "pub fn valid() {}"),
            ("src/editor/session/windows.rs", target),
            (
                "src/facade.rs",
                "#[path = \"editor/session/notifications/remounted.rs\"] pub mod remounted;",
            ),
            (
                "src/editor/session/notifications/remounted.rs",
                "pub fn bad(_: &mut crate::editor::session::windows::WindowSession) {}",
            ),
        ],
    ] {
        let mut files = vec![("src/lib.rs", library)];
        files.extend(extra);
        let error = check(&files, "pub fn service() {}").unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("mutable authority") && detail.contains("WindowSession")),
            "{error}"
        );
    }
}

#[test]
fn filesystem_globs_fail_by_logical_physical_and_block_scope_but_test_globs_do_not() {
    for source in [
        "use crate::*; pub fn invalid() {}",
        "pub fn invalid() { use crate::*; }",
    ] {
        let error = check(
            &[
                (
                    "src/lib.rs",
                    "extern crate self as local_crate; pub mod editor; pub mod files; pub mod language; pub mod facade;",
                ),
                ("src/files.rs", source),
            ],
            "pub fn service() {}",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("filesystem") && detail.contains("imports and exports explicitly")),
            "{source}: {error}"
        );
    }
    let remounted = check(
        &[
            (
                "src/facade.rs",
                "#[path = \"files/remounted.rs\"] pub mod remounted;",
            ),
            ("src/files/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&remounted, ArchitectureError::Dependency { detail, .. } if detail.contains("filesystem") && detail.contains("imports and exports explicitly")),
        "{remounted}"
    );
    check(
        &[
            (
                "src/lib.rs",
                "extern crate self as local_crate; pub mod editor; pub mod files; pub mod language; pub mod facade;",
            ),
            (
                "src/files.rs",
                "use std::path::Path; #[cfg(test)] use crate::*; pub fn valid(_: &Path) {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn editor_command_globs_fail_by_logical_physical_and_block_scope_but_test_globs_do_not() {
    for source in [
        "use crate::*; pub fn invalid() {}",
        "pub fn invalid() { use crate::*; }",
    ] {
        let error = check(
            &[
                ("src/editor/mod.rs", "pub mod command;"),
                ("src/editor/command.rs", source),
            ],
            "pub fn service() {}",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("editor command") && detail.contains("imports and exports explicitly")),
            "{source}: {error}"
        );
    }
    let logical = check(
        &[
            (
                "src/editor/mod.rs",
                "#[path = \"../remounted.rs\"] pub mod command;",
            ),
            ("src/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&logical, ArchitectureError::Dependency { detail, .. } if detail.contains("editor command") && detail.contains("imports and exports explicitly")),
        "{logical}"
    );
    let physical = check(
        &[
            (
                "src/facade.rs",
                "#[path = \"editor/command/remounted.rs\"] pub mod remounted;",
            ),
            ("src/editor/command/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&physical, ArchitectureError::Dependency { detail, .. } if detail.contains("editor command") && detail.contains("imports and exports explicitly")),
        "{physical}"
    );
    check(
        &[
            (
                "src/editor/mod.rs",
                "pub mod command;",
            ),
            (
                "src/editor/command.rs",
                "use std::path::Path; #[cfg(test)] use crate::*; #[cfg(test)] mod tests { use super::*; } pub fn valid(_: &Path) {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn editor_state_and_transaction_globs_fail_by_every_scope_but_test_globs_do_not() {
    let rejected = |result: Result<(), ArchitectureError>| {
        let error = result.unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("editor command/state/transaction") && detail.contains("imports and exports explicitly")),
            "{error}"
        );
    };
    for owner in ["state", "transaction"] {
        let declaration = format!("pub mod {owner};");
        let path = format!("src/editor/{owner}.rs");
        for source in [
            "use crate::*; pub fn invalid() {}",
            "pub fn invalid() { use crate::*; }",
        ] {
            rejected(check(
                &[("src/editor/mod.rs", &declaration), (&path, source)],
                "pub fn service() {}",
            ));
        }

        let logical_declaration = format!("#[path = \"../remounted.rs\"] pub mod {owner};");
        rejected(check(
            &[
                ("src/editor/mod.rs", &logical_declaration),
                ("src/remounted.rs", "use crate::*;"),
            ],
            "pub fn service() {}",
        ));

        let physical_declaration =
            format!("#[path = \"editor/{owner}/remounted.rs\"] pub mod remounted;");
        let physical_path = format!("src/editor/{owner}/remounted.rs");
        rejected(check(
            &[
                ("src/facade.rs", &physical_declaration),
                (&physical_path, "use crate::*;"),
            ],
            "pub fn service() {}",
        ));

        check(
            &[
                ("src/editor/mod.rs", &declaration),
                (
                    &path,
                    "use std::path::Path; #[cfg(test)] use crate::*; #[cfg(test)] mod tests { use super::*; } pub fn valid(_: &Path) {}",
                ),
            ],
            "pub fn service() {}",
        )
        .unwrap();
    }
}

#[test]
fn workspace_globs_fail_by_logical_physical_and_block_scope_but_test_globs_do_not() {
    let library = "extern crate self as local_crate; pub mod editor; pub mod language; pub mod facade; pub mod workspace;";
    for source in [
        "use crate::*; pub fn invalid() {}",
        "pub fn invalid() { use crate::*; }",
    ] {
        let error = check(
            &[("src/lib.rs", library), ("src/workspace.rs", source)],
            "pub fn service() {}",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("workspace") && detail.contains("imports and exports explicitly")),
            "{source}: {error}"
        );
    }
    let logical = check(
        &[
            ("src/lib.rs", library),
            (
                "src/workspace.rs",
                "#[path = \"remounted.rs\"] pub mod remounted;",
            ),
            ("src/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&logical, ArchitectureError::Dependency { detail, .. } if detail.contains("workspace") && detail.contains("imports and exports explicitly")),
        "{logical}"
    );
    let physical = check(
        &[
            ("src/lib.rs", library),
            ("src/workspace.rs", "pub fn valid() {}"),
            (
                "src/facade.rs",
                "#[path = \"workspace/remounted.rs\"] pub mod remounted;",
            ),
            ("src/workspace/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&physical, ArchitectureError::Dependency { detail, .. } if detail.contains("workspace") && detail.contains("imports and exports explicitly")),
        "{physical}"
    );
    check(
        &[
            ("src/lib.rs", library),
            (
                "src/workspace.rs",
                "use std::path::Path; #[cfg(test)] use crate::*; #[cfg(test)] mod tests { use super::*; } pub fn valid(_: &Path) {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn extension_globs_fail_by_logical_physical_and_block_scope_but_test_globs_do_not() {
    let library = "extern crate self as local_crate; pub mod editor; pub mod extensions; pub mod language; pub mod facade;";
    for source in [
        "use crate::*; pub fn invalid() {}",
        "pub fn invalid() { use crate::*; }",
    ] {
        let error = check(
            &[("src/lib.rs", library), ("src/extensions.rs", source)],
            "pub fn service() {}",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("extension host") && detail.contains("imports and exports explicitly")),
            "{source}: {error}"
        );
    }
    let logical = check(
        &[
            (
                "src/lib.rs",
                "extern crate self as local_crate; pub mod editor; #[path = \"remounted.rs\"] pub mod extensions; pub mod language; pub mod facade;",
            ),
            ("src/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&logical, ArchitectureError::Dependency { detail, .. } if detail.contains("extension host") && detail.contains("imports and exports explicitly")),
        "{logical}"
    );
    let physical = check(
        &[
            ("src/lib.rs", library),
            ("src/extensions.rs", "pub fn valid() {}"),
            (
                "src/facade.rs",
                "#[path = \"extensions/remounted.rs\"] pub mod remounted;",
            ),
            ("src/extensions/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&physical, ArchitectureError::Dependency { detail, .. } if detail.contains("extension host") && detail.contains("imports and exports explicitly")),
        "{physical}"
    );
    check(
        &[
            ("src/lib.rs", library),
            (
                "src/extensions.rs",
                "use std::path::Path; #[cfg(test)] use crate::*; #[cfg(test)] mod tests { use super::*; } pub fn valid(_: &Path) {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn language_lsp_globs_fail_by_logical_physical_and_block_scope_but_test_globs_do_not() {
    let rejected = |result: Result<(), ArchitectureError>| {
        let error = result.unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("language/LSP") && detail.contains("imports and exports explicitly")),
            "{error}"
        );
    };
    for source in [
        "use crate::*; pub fn invalid() {}",
        "pub fn invalid() { use crate::*; }",
        "pub mod nested { use crate::*; }",
    ] {
        rejected(check(&[], source));
    }
    rejected(check(
        &[
            (
                "src/language/mod.rs",
                "#[path = \"../remounted.rs\"] pub mod lsp;",
            ),
            ("src/remounted.rs", "use crate::*;"),
        ],
        "pub fn unused() {}",
    ));
    rejected(check(
        &[
            (
                "src/facade.rs",
                "#[path = \"language/lsp/remounted.rs\"] pub mod remounted;",
            ),
            ("src/language/lsp/remounted.rs", "use crate::*;"),
        ],
        "pub fn valid() {}",
    ));
    check(
        &[],
        "use std::path::Path; #[cfg(test)] use crate::*; #[cfg(test)] mod tests { use super::*; } pub fn valid(_: &Path) {}",
    )
    .unwrap();
}

#[test]
fn tools_globs_fail_by_logical_physical_and_block_scope_but_test_globs_do_not() {
    let library = "extern crate self as local_crate; pub mod editor; pub mod language; pub mod facade; pub mod tools;";
    let rejected = |result: Result<(), ArchitectureError>| {
        let error = result.unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("tools") && detail.contains("imports and exports explicitly")),
            "{error}"
        );
    };
    for source in [
        "use crate::*; pub fn invalid() {}",
        "pub fn invalid() { use crate::*; }",
        "pub mod nested { use crate::*; }",
    ] {
        rejected(check(
            &[("src/lib.rs", library), ("src/tools.rs", source)],
            "pub fn service() {}",
        ));
    }
    rejected(check(
        &[
            (
                "src/lib.rs",
                "extern crate self as local_crate; pub mod editor; pub mod language; pub mod facade; #[path = \"remounted.rs\"] pub mod tools;",
            ),
            ("src/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    ));
    rejected(check(
        &[
            ("src/lib.rs", library),
            ("src/tools.rs", "pub fn valid() {}"),
            (
                "src/facade.rs",
                "#[path = \"tools/remounted.rs\"] pub mod remounted;",
            ),
            ("src/tools/remounted.rs", "use crate::*;"),
        ],
        "pub fn service() {}",
    ));
    check(
        &[
            ("src/lib.rs", library),
            (
                "src/tools.rs",
                "use std::path::Path; #[cfg(test)] use crate::*; #[cfg(test)] mod tests { use super::*; } pub fn valid(_: &Path) {}",
            ),
        ],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn ui_render_and_window_globs_fail_by_logical_physical_and_block_scope_but_tests_do_not() {
    let library = "extern crate self as local_crate; pub mod editor; pub mod language; pub mod facade; pub mod ui;";
    let rejected = |result: Result<(), ArchitectureError>| {
        let error = result.unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("UI render/window") && detail.contains("imports and exports explicitly")),
            "{error}"
        );
    };
    for family in ["render", "window"] {
        let primary = format!("src/ui/{family}.rs");
        let other = if family == "render" {
            "src/ui/window.rs"
        } else {
            "src/ui/render.rs"
        };
        for source in [
            "use crate::*; pub fn invalid() {}",
            "pub fn invalid() { use crate::*; }",
            "pub mod nested { use crate::*; }",
        ] {
            rejected(check(
                &[
                    ("src/lib.rs", library),
                    ("src/ui.rs", "pub mod render; pub mod window;"),
                    (&primary, source),
                    (other, "pub fn valid() {}"),
                ],
                "pub fn service() {}",
            ));
        }

        let logical_ui = if family == "render" {
            "#[path = \"remounted.rs\"] pub mod render; pub mod window;"
        } else {
            "pub mod render; #[path = \"remounted.rs\"] pub mod window;"
        };
        rejected(check(
            &[
                ("src/lib.rs", library),
                ("src/ui.rs", logical_ui),
                ("src/remounted.rs", "use crate::*;"),
                (other, "pub fn valid() {}"),
            ],
            "pub fn service() {}",
        ));

        let physical = format!("src/ui/{family}/remounted.rs");
        let physical_declaration =
            format!("#[path = \"ui/{family}/remounted.rs\"] pub mod remounted;");
        rejected(check(
            &[
                ("src/lib.rs", library),
                ("src/ui.rs", "pub mod render; pub mod window;"),
                (&primary, "pub fn valid() {}"),
                (other, "pub fn valid() {}"),
                ("src/facade.rs", &physical_declaration),
                (&physical, "use crate::*;"),
            ],
            "pub fn service() {}",
        ));

        let facade_declaration = format!("#[path = \"ui/{family}.rs\"] pub mod remounted;");
        rejected(check(
            &[
                ("src/lib.rs", library),
                ("src/ui.rs", "pub fn valid() {}"),
                ("src/facade.rs", &facade_declaration),
                (&primary, "use crate::*;"),
            ],
            "pub fn service() {}",
        ));

        check(
            &[
                ("src/lib.rs", library),
                ("src/ui.rs", "pub mod render; pub mod window;"),
                (
                    &primary,
                    "use std::path::Path; #[cfg(test)] use crate::*; #[cfg(test)] mod tests { use super::*; } pub fn valid(_: &Path) {}",
                ),
                (other, "pub fn valid() {}"),
            ],
            "pub fn service() {}",
        )
        .unwrap();
    }
}

#[test]
fn production_wildcard_imports_and_reexports_fail_globally_but_test_code_does_not() {
    for source in [
        "use crate::editor::*;",
        "pub fn nested() { use crate::editor::*; }",
        "pub mod nested { use crate::editor::*; }",
        "pub use crate::editor::*;",
        "pub fn nested() { pub use crate::editor::*; }",
        "pub(crate) use crate::editor::*;",
    ] {
        let error = check(&[("src/facade.rs", source)], "pub fn service() {}").unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard") && (detail.contains("imports and exports explicitly") || detail.contains("must name every export explicitly"))),
            "{source}: {error}"
        );
    }
    for source in ["use crate::editor::*;", "pub use crate::editor::*;"] {
        let error = check(
            &[
                (
                    "src/facade.rs",
                    "#[path = \"shared.rs\"] pub mod remounted;",
                ),
                ("src/shared.rs", source),
            ],
            "pub fn service() {}",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard")),
            "{source}: {error}"
        );
    }
    check(
        &[(
            "src/facade.rs",
            "use crate::editor::EditorState; #[cfg(test)] use crate::editor::*; #[cfg(test)] pub use crate::editor::*; #[cfg(test)] mod tests { use super::*; } pub fn client(_: &EditorState) {}",
        )],
        "pub fn service() {}",
    )
    .unwrap();
}

#[test]
fn unaccounted_module_paths_fail_closed() {
    let error = check(
        &[],
        "#[cfg_attr(unix, path = \"elsewhere.rs\")] pub mod nested;",
    )
    .unwrap_err();
    assert!(
        matches!(error, ArchitectureError::Dependency { detail, .. } if detail.contains("conditional module attributes"))
    );
}
