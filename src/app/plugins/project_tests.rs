use crate::app::tests::*;

use crate::app::picker::project_tests::palette_row_for;


#[test]
fn palette_finds_plugin_titles_without_configured_labels_and_honors_overrides() {
    let (_app_dirs, mut app) = app();
    let mut manifest = crate::app::tests::plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x");
    manifest.commands.get_mut("run").unwrap().title = "Print Hello".into();
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "fixture".into()).unwrap();
    app.register_indexed_plugin_commands(&index).unwrap();
    assert!(
        !app.configuration
            .settings()
            .command_labels
            .contains_key("plugin.demo.run")
    );
    assert_eq!(
        palette_row_for(&mut app, "Print Hello"),
        "Plugins: Print Hello"
    );

    app.close_picker();
    let mut config = app.configuration.fixture_snapshot();
    config
        .settings
        .command_labels
        .insert("plugin.demo.run".into(), "Say Goodbye".into());
    app.configuration.install_fixture(config);
    assert_eq!(
        palette_row_for(&mut app, "Say Goodbye"),
        "Plugins: Say Goodbye"
    );
}