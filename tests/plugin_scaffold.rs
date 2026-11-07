//! The public plugin scaffold CLI creates a guest without starting the editor.

#[test]
fn plugin_new_creates_a_guest_and_refuses_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_dust"))
            .current_dir(root.path())
            .args(["plugin", "new", "words"])
            .env("DUST_STATE_HOME", root.path().join("state"))
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "src/lib.rs",
        "wit/dust-plugin.wit",
        "manifest.toml",
        "README.md",
    ] {
        assert!(root.path().join("words").join(path).is_file(), "{path}");
    }
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Created")
    );
    let source = root.path().join("words/src/lib.rs");
    std::fs::write(&source, "user changes").unwrap();
    assert!(!run().status.success());
    assert_eq!(std::fs::read_to_string(source).unwrap(), "user changes");
}
