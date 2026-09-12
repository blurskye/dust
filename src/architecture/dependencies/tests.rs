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
        "use crate::facade::*; pub fn bad(s: &mut Actor) { s.workspace_root.clear(); }",
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
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("imports and exports explicitly") && detail.contains("production wildcards remain globally")),
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
        matches!(remounted, ArchitectureError::Dependency { detail, .. } if detail.contains("imports and exports explicitly"))
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
        matches!(remounted, ArchitectureError::Dependency { detail, .. } if detail.contains("imports and exports explicitly"))
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
fn production_wildcard_reexports_fail_globally_but_imports_and_test_code_remain_scoped() {
    for source in [
        "pub use crate::editor::*;",
        "pub fn nested() { pub use crate::editor::*; }",
    ] {
        let error = check(&[("src/facade.rs", source)], "pub fn service() {}").unwrap_err();
        assert!(
            matches!(&error, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard reexport") && detail.contains("must name every export explicitly")),
            "{source}: {error}"
        );
    }
    let remounted = check(
        &[
            (
                "src/facade.rs",
                "#[path = \"shared.rs\"] pub mod remounted;",
            ),
            ("src/shared.rs", "pub use crate::editor::*;"),
        ],
        "pub fn service() {}",
    )
    .unwrap_err();
    assert!(
        matches!(&remounted, ArchitectureError::Dependency { detail, .. } if detail.contains("production wildcard reexport")),
        "{remounted}"
    );
    check(
        &[(
            "src/facade.rs",
            "use crate::editor::*; #[cfg(test)] pub use crate::editor::*; pub fn client(_: &EditorState) {}",
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
