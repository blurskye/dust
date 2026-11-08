//! Tests for configuration loading and validation.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;

use crate::config::load::BUNDLED_CONFIG;

use crate::config::load::BUNDLED_THEME;

use crate::config::load::CapturedSource;

use crate::config::load::CapturedText;

use crate::config::load::ConfigurationSourceBundle;

use crate::config::load::SourceFingerprint;

use crate::config::load::WATCH_FINGERPRINT_BYTE_LIMIT;

use crate::config::load::capture_configuration_bundle_with;

use crate::config::load::capture_consistent_configuration_bundle_with;

use crate::config::load::captured_bundle_is_stable;

use crate::config::load::hash_source_bytes;

use crate::config::load::load_consistent_configuration;

use crate::config::load::load_theme;

use crate::config::load::setting_keys;

use crate::config::load::setting_spans;

use crate::config::AmbiguousWidth;
use crate::config::ExplorerIconMode;
use crate::config::LineNumbers;
use crate::config::load::source_fingerprint;
use crate::config::load_bundled_config;
use crate::config::load_config;
use crate::config::load_keymaps;

#[test]
fn bundled_theme_ports_validate_and_load_by_name_without_installed_files() {
    let mut backgrounds = BTreeSet::new();
    for name in [
        "dust",
        "catppuccin-latte",
        "catppuccin-frappe",
        "catppuccin-macchiato",
        "catppuccin-mocha",
        "gruvbox-dark",
        "nord",
        "tokyo-night",
        "tokyo-night-storm",
        "tokyo-night-moon",
        "tokyo-night-day",
        "rose-pine",
        "rose-pine-moon",
        "rose-pine-dawn",
    ] {
        let bundle = capture_configuration_bundle_with(name, |path| {
            let text = if path.file_name().is_some_and(|file| file == "config.toml") {
                CapturedText::Text(format!("[ui]\ntheme = \"{name}\"\n"))
            } else {
                CapturedText::Missing
            };
            captured(path, text)
        });
        let loaded = bundle.into_loaded();
        assert!(
            loaded.diagnostic.is_none(),
            "{name}: {:?}",
            loaded.diagnostic
        );
        assert_eq!(loaded.theme.name, name);
        loaded.theme.validate().unwrap();
        assert_eq!(
            loaded.theme_path,
            PathBuf::from(format!("<bundled:theme:{name}>"))
        );
        assert!(
            backgrounds.insert(format!("{:?}", loaded.theme.styles.default.background)),
            "themes need distinct backgrounds"
        );
    }
    let mut installed = crate::config::themes::bundled_named("nord").unwrap().theme;
    installed.styles.default.foreground = crate::config::ThemeColor::Red;
    for (theme_source, valid) in [
        (toml::to_string(&installed).unwrap(), true),
        ("invalid toml".into(), false),
    ] {
        let bundle = capture_configuration_bundle_with("nord", |path| {
            let text = match path.file_name().and_then(|name| name.to_str()) {
                Some("config.toml") => CapturedText::Text("[ui]\ntheme = \"nord\"\n".into()),
                Some("nord.toml") => CapturedText::Text(theme_source.clone()),
                _ => CapturedText::Missing,
            };
            captured(path, text)
        });
        let loaded = bundle.into_loaded();
        if valid {
            assert!(loaded.diagnostic.is_none());
            assert_eq!(loaded.theme, installed);
            assert_eq!(loaded.theme_path, crate::config::user_theme_path("nord"));
        } else {
            assert!(
                loaded.diagnostic.is_some(),
                "a malformed installed override must not silently load the bundled theme"
            );
        }
    }
}

#[test]
fn absent_file_uses_defaults_without_error() {
    let root = tempdir().expect("tempdir");
    let loaded = load_config(&root.path().join("missing.toml"));
    assert_eq!(loaded.config, load_bundled_config().config);
    assert!(loaded.diagnostic.is_none());
}

#[test]
fn inlay_hint_level_defaults_to_standard_and_accepts_maximum_testing_mode() {
    assert_eq!(
        load_bundled_config().config.lsp.inlay_hint_level,
        crate::config::InlayHintLevel::Standard
    );
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[lsp]\ninlay_hint_level = 'max'\n").expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert_eq!(
        loaded.config.lsp.inlay_hint_level,
        crate::config::InlayHintLevel::Max
    );
}

#[test]
fn valid_partial_config_merges_with_defaults() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "[editor]\nline_numbers = 'absolute'\ntab_width = 2\n",
    )
    .expect("write");
    let loaded = load_config(&path);
    assert_eq!(loaded.config.editor.line_numbers, LineNumbers::Absolute);
    assert_eq!(loaded.config.editor.tab_width, 2);
    assert_eq!(loaded.config.ui.explorer.width, 40);
    assert_eq!(
        loaded.config.ui.explorer.icon_mode,
        ExplorerIconMode::NerdFont
    );
    assert_eq!(
        loaded.config.input.text_commands.get("editor.insert"),
        Some(&"edit.insert_text".to_owned())
    );
    assert!(loaded.diagnostic.is_none());
    let source = loaded
        .explain("editor.tab_width")
        .expect("user value")
        .source;
    assert!(source.starts_with(&path.display().to_string()));
    assert!(source.contains(":bytes "));
    assert_eq!(
        loaded
            .explain("ui.explorer.width")
            .expect("inherited value")
            .source,
        "bundled defaults"
    );
}

#[test]
fn viewport_horizontal_margin_is_nested_bounded_and_has_no_flat_alias() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[editor.viewport]\nhorizontal_margin = 9\n").expect("write nested");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert_eq!(loaded.config.editor.viewport.horizontal_margin, 9);
    assert_eq!(loaded.config.editor.viewport.vertical_margin, 4);
    assert!(
        loaded
            .explain("editor.viewport.horizontal_margin")
            .expect("nested source")
            .source
            .starts_with(&path.display().to_string())
    );

    fs::write(&path, "[editor.viewport]\nvertical_margin = 7\n").expect("write vertical");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert_eq!(loaded.config.editor.viewport.vertical_margin, 7);

    fs::write(&path, "[editor.viewport]\nhorizontal_margin = 101\n").expect("write out of bounds");
    let loaded = load_config(&path);
    assert_eq!(loaded.config, load_bundled_config().config);
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|message| { message.contains("editor.viewport.horizontal_margin") })
    );

    // A removed setting is left out with a warning; the rest of the file still applies.
    fs::write(&path, "[editor]\nhorizontal_margin = 9\ntab_width = 3\n")
        .expect("write removed flat field");
    let loaded = load_config(&path);
    assert_eq!(loaded.config.editor.tab_width, 3);
    assert!(
        loaded
            .warning
            .as_deref()
            .is_some_and(|message| message.ends_with("config.toml: editor.horizontal_margin"))
    );
}

#[test]
fn unpublished_explorer_schema_leaves_out_removed_overlay_fields() {
    let root = tempdir().expect("tempdir");
    let removed = [
        ("width_percent", "22"),
        ("height_percent", "100"),
        ("minimum_height", "10"),
        ("indent_width", "2"),
        ("selected_marker", "\">\""),
        ("row_marker", "\" \""),
        ("border", "\"single\""),
    ];
    for (index, (field, value)) in removed.into_iter().enumerate() {
        let path = root.path().join(format!("config-{index}.toml"));
        fs::write(&path, format!("[ui.explorer]\n{field} = {value}\n")).expect("write");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(
            loaded
                .warning
                .as_deref()
                .is_some_and(|message| message.contains(field)),
            "missing warning for {field}"
        );
    }
}

#[test]
fn explorer_glyph_policy_rejects_oversized_segments() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[editor]\nambiguous_width = \"wide\"\n").expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert_eq!(loaded.config.editor.ambiguous_width, AmbiguousWidth::Wide);

    fs::write(
        &path,
        "[ui.explorer.glyphs.unicode]\ntree_vertical = \"界界界界界\"\n",
    )
    .expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_some());
    assert_eq!(loaded.config, load_bundled_config().config);
}

#[test]
fn a_bad_task_is_named_with_what_to_fix() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[tasks.demo]\nexecutable = \"python3\"\n").expect("write");
    let diagnostic = load_config(&path)
        .diagnostic
        .expect("relative executable is refused");
    assert!(
        diagnostic.contains("tasks.demo executable must be an absolute path"),
        "{diagnostic}"
    );
}

#[test]
fn migrated_config_spans_still_address_the_original_file() {
    let source = "[editor]\ntab_width = 3\n";
    let loaded =
        crate::config::load::parse_config_source(std::path::Path::new("config.toml"), source);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    let span = &loaded.user_setting_spans["editor.tab_width"];
    assert_eq!(&source[span.clone()], "3");
    assert!(!loaded.user_setting_spans.contains_key("schema_version"));
}

#[test]
fn parser_spans_cover_inline_tables_and_quoted_dotted_keys() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "editor = { tab_width = 3 }\n[command_labels]\n\"view.cursor.move\" = \"MOVE\"\n",
    )
    .expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert_eq!(loaded.config.editor.tab_width, 3);
    let editor = loaded.explain("editor.tab_width").expect("inline explain");
    assert!(editor.source.contains(":bytes "));
    let quoted = loaded
        .explain("command_labels.\"view.cursor.move\"")
        .expect("quoted key explain");
    assert_eq!(quoted.value, "\"MOVE\"");
    assert!(quoted.source.contains(":bytes "));
    for key in [
        r#"command_labels."view.cursor.\u006dove""#,
        "command_labels.'view.cursor.move'",
    ] {
        let escaped = loaded.explain(key).expect("TOML quoted key explain");
        assert_eq!(escaped.value, quoted.value);
        assert_eq!(escaped.source, quoted.source);
    }
}

#[test]
fn config_key_segments_decode_toml_quoted_keys() {
    for (key, expected) in [
        (
            r#"command_labels."caf\u00e9""#,
            vec!["command_labels", "café"],
        ),
        (r#"labels."a\\b\"c""#, vec!["labels", "a\\b\"c"]),
        (r#"labels.'a\b.c'"#, vec!["labels", "a\\b.c"]),
        (r#"labels."""#, vec!["labels", ""]),
        (
            "dashboard.action.0.label",
            vec!["dashboard", "action", "0", "label"],
        ),
    ] {
        assert_eq!(
            crate::config::config_key_segments(key).unwrap(),
            expected,
            "{key}"
        );
    }
    for invalid in ["", "a..b", "a.", "\"unterminated", r#"a."\q""#] {
        assert!(
            crate::config::config_key_segments(invalid).is_none(),
            "{invalid}"
        );
    }
}

#[test]
fn parser_spans_cover_theme_leaves_and_dashboard_action_tables() {
    let theme = "[styles.accent]\nforeground='#010203'\n";
    let theme_spans = setting_spans(theme);
    let span = &theme_spans["styles.accent.foreground"];
    assert_eq!(&theme[span.clone()], "'#010203'");

    let dashboard = "[[action]]\ncommand='file.new'\nlabel='New'\n";
    let dashboard_spans = setting_spans(dashboard);
    assert!(dashboard_spans.contains_key("action.0.command"));
    let span = &dashboard_spans["action.0.label"];
    assert_eq!(&dashboard[span.clone()], "'New'");
}

#[test]
fn malformed_or_invalid_config_is_nonfatal() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[editor]\ntab_width = 0\n").expect("write");
    let loaded = load_config(&path);
    assert_eq!(loaded.config, load_bundled_config().config);
    assert!(loaded.diagnostic.is_some());

    fs::write(
        &path,
        "made_up = 1\n[ui]\nmade_up = true\n[editor.debug_signs]\nexecution = \"x\"\n",
    )
    .expect("unknown keys");
    let loaded = load_config(&path);
    assert_eq!(loaded.config, load_bundled_config().config);
    assert_eq!(
        loaded.path, path,
        "the file is used, not the bundled defaults"
    );
    assert_eq!(loaded.diagnostic, None);
    let diagnostic = loaded.warning.unwrap_or_default();
    for key in ["made_up", "ui.made_up", "editor.debug_signs"] {
        assert!(
            diagnostic
                .split(": ")
                .last()
                .unwrap()
                .split(", ")
                .any(|found| found == key),
            "{diagnostic}"
        );
    }
}

#[test]
fn keymap_policy_is_typed_bounded_and_rejects_recursive_leaders() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    for (index, (field, value, expected)) in [
        ("leader", "''".to_owned(), "must not be empty"),
        (
            "local_leader",
            "'<localleader>'".to_owned(),
            "without leader tokens",
        ),
        (
            "leader",
            format!("'{}'", "a ".repeat(5).trim()),
            "at most 4 keys",
        ),
        ("local_leader", format!("'{}'", "x".repeat(65)), "64 bytes"),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(
            &path,
            format!("[input.keymaps]\n{field} = {value}\n# case {index}\n"),
        )
        .expect("write policy");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains(expected)),
            "missing `{expected}` in {:?}",
            loaded.diagnostic
        );
    }
}

fn captured(path: PathBuf, text: CapturedText) -> CapturedSource {
    let (length, content_hash) = match &text {
        CapturedText::Text(source) => (
            Some(source.len() as u64),
            Some(hash_source_bytes(source.as_bytes())),
        ),
        CapturedText::Missing | CapturedText::Error(_) => (None, None),
    };
    CapturedSource {
        identity: SourceFingerprint {
            path: path.clone(),
            modified: None,
            length,
            content_hash,
        },
        path,
        text,
    }
}

fn configuration_bundle(theme: &str, revision: u8, valid: bool) -> ConfigurationSourceBundle {
    capture_configuration_bundle_with(theme, |path| {
        let source = match path.file_name().and_then(|name| name.to_str()) {
            Some("config.toml") => format!(
                "[ui]\ntheme='{theme}'\n[editor]\ntab_width={}\n",
                if valid { usize::from(revision) + 2 } else { 0 },
            ),
            Some("keymaps.toml") => format!(
                "[[keymap]]\nid='user.revision-{revision}'\ncontext='dashboard'\nkeys='x'\ncommand='file.new'\n"
            ),
            Some("dashboard.toml") => include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/defaults/dashboard.toml"
            ))
            .replace("D U S T", &format!("dashboard-{revision}")),
            Some(_) => BUNDLED_THEME
                .replace("name = \"dust\"", &format!("name = \"{theme}\""))
                .replace("#c9d1d9", if revision == 0 { "#c9d1d9" } else { "#c9d1d8" }),
            None => panic!("unexpected captured path {}", path.display()),
        };
        captured(path, CapturedText::Text(source))
    })
}

#[test]
fn captured_bundle_identity_exposes_aba_and_uses_the_same_text_for_spans() {
    let config = "[ui]\ntheme='night'\n";
    let keymaps_b = "[[keymap]]\nid='user.test.captured-b'\ncontext='dashboard'\nkeys='b'\ncommand='file.new'\n";
    let dashboard = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/defaults/dashboard.toml"
    ))
    .replace("D U S T", "CAPTURED DASH");
    let theme = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/themes/dust.toml"
    ))
    .replace("name = \"dust\"", "name = \"night\"");
    let reads = Cell::new(0_usize);
    let bundle = capture_configuration_bundle_with("dust", |path| {
        reads.set(reads.get() + 1);
        let text = match path.file_name().and_then(|name| name.to_str()) {
            Some("config.toml") => config.to_owned(),
            Some("keymaps.toml") => keymaps_b.to_owned(),
            Some("dashboard.toml") => dashboard.clone(),
            Some("night.toml") => theme.clone(),
            other => panic!("unexpected captured path {other:?}"),
        };
        captured(path, CapturedText::Text(text))
    });
    assert_eq!(reads.get(), 4, "every represented source is captured once");
    assert_eq!(bundle.selected_theme, "night");
    assert!(bundle.theme.path.ends_with("night.toml"));

    // External pre/post observations can both be A, but a transient B read
    // remains visible in the captured identity and cannot be emitted as A.
    let mut observed_a = bundle.identity();
    let keymap_path = observed_a[1].path.clone();
    observed_a[1] = captured(
            keymap_path,
            CapturedText::Text(
                "[[keymap]]\nid='user.test.captured-a'\ncontext='dashboard'\nkeys='a'\ncommand='file.new'\n".into(),
            ),
        )
        .identity;
    assert_ne!(observed_a, bundle.identity());
    assert!(
        !captured_bundle_is_stable(&observed_a, &bundle.identity(), &observed_a),
        "A→B→A must reject the transient B bundle"
    );

    let loaded = bundle.into_loaded();
    assert_eq!(loaded.dashboard.identity, "CAPTURED DASH");
    let dashboard_span = &loaded.dashboard_spans["identity"];
    assert_eq!(&dashboard[dashboard_span.clone()], "\"CAPTURED DASH\"");
    let theme_span = &loaded.theme_spans["name"];
    assert_eq!(&theme[theme_span.clone()], "\"night\"");
    assert_eq!(
        loaded
            .keymaps
            .entries
            .last()
            .expect("user keymap")
            .keys
            .as_deref(),
        Some("b")
    );
}

#[test]
fn captured_bundle_identity_detects_missing_created_and_deleted_sources() {
    let config = captured(
        crate::config::user_config_path(),
        CapturedText::Text("[ui]\ntheme='dust'\n".into()),
    );
    let missing = captured(crate::config::user_keymaps_path(), CapturedText::Missing);
    let created = captured(
            crate::config::user_keymaps_path(),
            CapturedText::Text(
                "[[keymap]]\nid='user.test.created-x'\ncontext='dashboard'\nkeys='x'\ncommand='file.new'\n".into(),
            ),
        );
    assert_ne!(missing.identity, created.identity);
    assert_ne!(config.identity, missing.identity);
    assert!(!captured_bundle_is_stable(
        &missing.identity,
        &created.identity,
        &missing.identity
    ));
}

#[test]
fn consistent_loader_retries_same_theme_changes_and_theme_source_switches() {
    let revision = Cell::new(0_u8);
    let loads = Cell::new(0_usize);
    let (stable, fingerprint) = capture_consistent_configuration_bundle_with(
        "dust",
        |_| {
            loads.set(loads.get() + 1);
            if loads.get() == 1 {
                revision.set(1);
            }
            configuration_bundle("dust", revision.get(), true)
        },
        |_| configuration_bundle("dust", revision.get(), true).identity(),
    )
    .expect("eventually stable");
    assert_eq!(loads.get(), 2);
    assert_eq!(
        fingerprint,
        configuration_bundle("dust", 1, true).identity()
    );
    assert!(stable.into_loaded().diagnostic.is_none());

    let theme_revision = Cell::new(0_u8);
    let loads = Cell::new(0_usize);
    let (switched, fingerprint) = capture_consistent_configuration_bundle_with(
        "dust",
        |_| {
            loads.set(loads.get() + 1);
            if loads.get() == 2 {
                theme_revision.set(1);
            }
            configuration_bundle("night", theme_revision.get(), true)
        },
        |theme| configuration_bundle(theme, theme_revision.get(), true).identity(),
    )
    .expect("new theme stabilizes");
    assert_eq!(loads.get(), 3);
    assert_eq!(switched.selected_theme, "night");
    assert_eq!(
        fingerprint,
        configuration_bundle("night", 1, true).identity()
    );
}

#[test]
fn consistent_loader_rejects_transient_captures_even_when_before_and_after_match() {
    let loads = Cell::new(0_usize);
    let result = capture_consistent_configuration_bundle_with(
        "dust",
        |_| {
            loads.set(loads.get() + 1);
            configuration_bundle("dust", 1, true)
        },
        |_| configuration_bundle("dust", 0, true).identity(),
    );
    assert!(
        result.is_err(),
        "a transient capture must never be accepted"
    );
    assert_eq!(
        loads.get(),
        crate::config::load::CONFIGURATION_CONSISTENCY_ATTEMPTS
    );
}

#[test]
fn consistent_loader_never_emits_discarded_malformed_or_stale_attempts() {
    for final_valid in [true, false] {
        let revision = Cell::new(0_u8);
        let loads = Cell::new(0_usize);
        let (bundle, fingerprint) = capture_consistent_configuration_bundle_with(
            "dust",
            |_| {
                loads.set(loads.get() + 1);
                if loads.get() == 1 {
                    revision.set(1);
                    configuration_bundle("dust", 0, !final_valid)
                } else {
                    configuration_bundle("dust", revision.get(), final_valid)
                }
            },
            |_| configuration_bundle("dust", revision.get(), final_valid).identity(),
        )
        .expect("second complete source set is stable");
        assert_eq!(loads.get(), 2);
        assert_eq!(bundle.into_loaded().diagnostic.is_none(), final_valid);
        assert_eq!(
            fingerprint,
            configuration_bundle("dust", 1, final_valid).identity()
        );
    }
}

#[test]
fn accepted_candidate_fingerprint_matches_all_four_loaded_surfaces() {
    let revision = Cell::new(0_u8);
    let loads = Cell::new(0_usize);
    let (bundle, fingerprint) = capture_consistent_configuration_bundle_with(
        "dust",
        |_| {
            loads.set(loads.get() + 1);
            let snapshot = revision.get();
            let bundle = configuration_bundle("dust", snapshot, true);
            if loads.get() == 1 {
                revision.set(1);
            }
            bundle
        },
        |_| configuration_bundle("dust", revision.get(), true).identity(),
    )
    .expect("stable complete snapshot");
    assert_eq!(loads.get(), 2);
    assert_eq!(fingerprint, bundle.identity());
    assert_eq!(
        fingerprint,
        configuration_bundle("dust", 1, true).identity()
    );
    let candidate = bundle.into_loaded();
    assert!(candidate.diagnostic.is_none(), "{:?}", candidate.diagnostic);
    assert_eq!(candidate.config.editor.tab_width, 3);
    assert_eq!(
        candidate.keymaps.entries.last().unwrap().id,
        "user.revision-1"
    );
    assert_eq!(candidate.dashboard.identity, "dashboard-1");
    assert_eq!(
        candidate.theme.styles.default.foreground,
        crate::config::ThemeColor::Rgb(0xc9, 0xd1, 0xd8)
    );
}

#[test]
fn clean_consistent_load_never_fingerprints_user_sources() {
    let loaded =
        load_consistent_configuration(false, "unused-user-theme").expect("bundled candidate");
    assert!(loaded.fingerprint.is_none());
    let candidate = loaded.candidate.expect("valid bundled candidate");
    assert_eq!(candidate.config, load_bundled_config().config);
    assert!(!candidate.user_configuration);
    assert_eq!(candidate.path, PathBuf::from("<bundled:config>"));
}

#[test]
fn installed_plugin_discovery_uses_and_verifies_activation_digests() {
    use sha2::Digest;
    use sha2::Sha256;
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("activations")).unwrap();
    let manifest = crate::plugins::PluginManifest {
        id: "test-plugin".into(),
        version: "1.0.0".into(),
        component_sha256: "a".repeat(64),
        capabilities: Default::default(),
        commands: Default::default(),
        contributions: Default::default(),
    };
    let source = toml::to_string(&manifest).unwrap();
    let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
    assert_ne!(digest, manifest.component_sha256);
    for activation_sha256 in [Some(digest), None] {
        let package = crate::tools::LockedPackage {
            name: manifest.id.clone(),
            version: manifest.version.clone(),
            source: crate::tools::PackageSource::Path {
                path: root.path().into(),
            },
            content_sha256: manifest.component_sha256.clone(),
            activation_sha256,
            dependencies: vec![],
        };
        let path = root.path().join("activations").join(format!(
            "{}.toml",
            package
                .activation_sha256
                .as_ref()
                .unwrap_or(&package.content_sha256)
        ));
        let verifies_digest = package.activation_sha256.is_some();
        let lock = crate::tools::PackageLock {
            version: 1,
            packages: vec![package],
            reproducible: false,
        };
        fs::write(
            root.path().join("dust-packages.lock.json"),
            serde_json::to_vec(&lock).unwrap(),
        )
        .unwrap();
        fs::write(&path, &source).unwrap();
        assert_eq!(
            crate::tools::installed_plugins(root.path()),
            BTreeSet::from([manifest.id.clone()])
        );
        if verifies_digest {
            fs::write(&path, source.replace("1.0.0", "2.0.0")).unwrap();
            assert!(crate::tools::installed_plugins(root.path()).is_empty());
        }
    }
}

// XDG overrides are Unix configuration conventions, including on macOS.
#[cfg(unix)]
#[test]
fn user_directories_honor_xdg_overrides() {
    if std::env::var_os("DUST_XDG_TEST_CHILD").is_some() {
        assert_eq!(
            crate::config::user_config_path(),
            PathBuf::from("xdg-config/dust/config.toml")
        );
        assert_eq!(
            crate::config::user_state_directory(),
            PathBuf::from("xdg-state/dust")
        );
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "config::tests::user_directories_honor_xdg_overrides",
        ])
        .env("DUST_XDG_TEST_CHILD", "1")
        .env_remove("DUST_CONFIG_HOME")
        .env_remove("DUST_STATE_HOME")
        .env("XDG_CONFIG_HOME", "xdg-config")
        .env("XDG_STATE_HOME", "xdg-state")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn configuration_worker_tracks_disk_sources_across_startup_and_acknowledgments() {
    use crate::config::ConfigurationWorker;
    use std::time::Duration;
    use std::time::Instant;

    if std::env::var_os("DUST_CONFIG_WORKER_TEST_CHILD").is_none() {
        let root = tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "config::tests::configuration_worker_tracks_disk_sources_across_startup_and_acknowledgments", "--nocapture"])
            .env("DUST_CONFIG_WORKER_TEST_CHILD", "1")
            .env("DUST_CONFIG_HOME", root.path())
            .env("DUST_STATE_HOME", root.path().join("state"))
            .status().unwrap();
        assert!(status.success());
        return;
    }

    fn next(worker: &ConfigurationWorker) -> crate::config::ConfigurationWorkerResult {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if let Some(result) = worker.try_result().unwrap() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "configuration change was not reloaded"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn no_reload(worker: &ConfigurationWorker) {
        let deadline = Instant::now() + Duration::from_millis(2300);
        while Instant::now() < deadline {
            assert!(worker.try_result().unwrap().is_none(), "duplicate reload");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let path = crate::config::user_config_path();
    fs::write(&path, "[editor]\ntab_width=2\n").unwrap();
    let loaded = crate::config::load_user_config();
    let unchanged = ConfigurationWorker::start_with_fingerprint(
        true,
        loaded.config.ui.theme.clone(),
        loaded.source_fingerprint.clone(),
    );
    no_reload(&unchanged);
    drop(unchanged);

    fs::write(&path, "[editor]\ntab_width=3\n").unwrap();
    let worker = ConfigurationWorker::start_with_fingerprint(
        true,
        loaded.config.ui.theme.clone(),
        loaded.source_fingerprint.clone(),
    );
    let initial = next(&worker);
    assert!(initial.automatic);
    assert_eq!(initial.candidate.unwrap().config.editor.tab_width, 3);

    fs::write(&path, "[ui]\ntheme='nord'\n").unwrap();
    worker.request_reload().unwrap();
    assert_eq!(next(&worker).candidate.unwrap().theme.name, "nord");
    worker.verify_watch().unwrap();
    no_reload(&worker);

    let theme_path = crate::config::user_theme_path("night");
    fs::create_dir_all(theme_path.parent().unwrap()).unwrap();
    fs::write(&theme_path, "malformed").unwrap();
    fs::write(&path, "[ui]\ntheme='night'\n").unwrap();
    worker.request_reload().unwrap();
    assert!(next(&worker).candidate.is_err());
    worker.verify_watch().unwrap();
    fs::write(
        &theme_path,
        BUNDLED_THEME.replace("name = \"dust\"", "name = \"night\""),
    )
    .unwrap();
    assert_eq!(next(&worker).candidate.unwrap().theme.name, "night");

    fs::write(&path, "[ui]\ntheme='night'\n[editor]\ntab_width=4\n").unwrap();
    worker.verify_watch().unwrap();
    worker.verify_watch().unwrap();
    assert_eq!(next(&worker).candidate.unwrap().config.editor.tab_width, 4);
    no_reload(&worker);
}

// Exercises Unix FIFOs, device files and symlinks.
#[cfg(unix)]
#[test]
fn configuration_reads_reject_fifos_and_devices_without_blocking() {
    let root = tempdir().unwrap();
    for name in [
        "config.toml",
        "keymaps.toml",
        "dashboard.toml",
        "theme.toml",
        "dust-packages.lock.json",
    ] {
        let path = root.path().join(name);
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
        assert!(source_fingerprint(path.clone()).content_hash.is_none());
        assert!(matches!(
            crate::config::load::capture_source(path).text,
            CapturedText::Error(_)
        ));
    }
    assert!(crate::tools::installed_plugins(root.path()).is_empty());
    let device = PathBuf::from("/dev/zero");
    assert!(source_fingerprint(device.clone()).content_hash.is_none());
    assert!(matches!(
        crate::config::load::capture_source(device).text,
        CapturedText::Error(_)
    ));

    let path = root.path().join("regular.toml");
    fs::write(&path, "[editor]\ntab_width=3\n").unwrap();
    let link = root.path().join("linked.toml");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(load_config(&link).config.editor.tab_width, 3);
}

#[test]
fn watcher_fingerprint_detects_same_length_content_and_sources_are_size_bounded() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[editor]\ntab_width=2\n").expect("first");
    let first = source_fingerprint(path.clone());
    fs::write(&path, "[editor]\ntab_width=3\n").expect("same-length replacement");
    let second = source_fingerprint(path.clone());
    assert_eq!(first.length, second.length);
    assert_ne!(first.content_hash, second.content_hash);

    let oversized = vec![b' '; WATCH_FINGERPRINT_BYTE_LIMIT as usize + 1];
    fs::write(&path, oversized).expect("oversized source");
    let loaded = load_config(&path);
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| { diagnostic.contains("configuration source exceeds") })
    );
    assert_eq!(loaded.config, load_bundled_config().config);
}

#[test]
fn bundled_assets_share_the_typed_configuration_loaders() {
    let loaded = load_bundled_config();
    // The platform's bundled overlay (windows.toml) counts as explicit too.
    let declared = crate::config::load::bundled_settings_value();
    let resolved = toml::Value::try_from(&loaded.config).expect("resolved value");
    assert_eq!(
        setting_keys(&declared),
        setting_keys(&resolved),
        "every typed bundled setting must be explicit in config.toml"
    );
    assert!(loaded.keymaps.entries.len() > 100);
    assert_eq!(loaded.keymaps.bundled_count, loaded.keymaps.entries.len());
    let bundled_ids = loaded
        .keymaps
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(bundled_ids.len(), loaded.keymaps.entries.len());
    assert!(bundled_ids.iter().all(|id| id.starts_with("builtin.")));
    assert!(!loaded.keymaps.groups.is_empty());
    assert_eq!(
        loaded.keymaps.bundled_group_count,
        loaded.keymaps.groups.len()
    );
    let bundled_group_ids = loaded
        .keymaps
        .groups
        .iter()
        .map(|group| group.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(bundled_group_ids.len(), loaded.keymaps.groups.len());
    assert!(
        bundled_group_ids
            .iter()
            .all(|id| id.starts_with("builtin."))
    );
    assert_eq!(loaded.config.input.keymaps.leader, "space");
    assert_eq!(loaded.config.input.keymaps.local_leader, "\\");
    assert!(loaded.config.editor.indent_guides.enabled);
    assert_eq!(loaded.dashboard.action.len(), 10);
    assert!(
        loaded
            .dashboard
            .action
            .iter()
            .any(|action| action.command == "plugin.list")
    );
    assert!(
        loaded
            .dashboard
            .action
            .iter()
            .any(|action| action.command == "explorer.open")
    );
    let explained = loaded.explain("ui.explorer.width").expect("explain");
    assert_eq!(explained.value, "40");
    assert_eq!(explained.source, "bundled defaults");
    assert_eq!(
        loaded.config.input.text_commands.get("command_prompt"),
        Some(&"command_line.insert".to_owned())
    );
}

#[test]
fn themes_are_typed_and_require_capability_fallbacks() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("theme.toml");
    let source = BUNDLED_THEME.replace(
            "[styles.accent]\nforeground = \"#58a6ff\"\nbackground = \"#0d1117\"\nforeground_256 = \"indexed:75\"\nbackground_256 = \"indexed:233\"\nforeground_16 = \"blue\"",
            "[styles.accent]\nforeground = \"#010203\"\nbackground = \"#0d1117\"\nbackground_256 = \"indexed:233\"",
        );
    fs::write(&path, source).expect("theme");
    let error = load_theme(&path).expect_err("missing fallbacks");
    assert!(error.contains("256-color fallback"));

    let source = BUNDLED_THEME.replace(
            "[styles.accent]\nforeground = \"#58a6ff\"\nbackground = \"#0d1117\"\nforeground_256 = \"indexed:75\"\nbackground_256 = \"indexed:233\"\nforeground_16 = \"blue\"",
            "[styles.accent]\nforeground = \"indexed:201\"\nbackground = \"#0d1117\"\nforeground_256 = \"indexed:201\"\nbackground_256 = \"indexed:233\"\nforeground_16 = \"cyan\"",
        );
    fs::write(&path, source).expect("theme");
    let theme = load_theme(&path).expect("valid typed theme");
    assert_eq!(
        theme.styles.accent.foreground,
        crate::config::ThemeColor::Indexed(201)
    );
}

#[test]
fn bundled_terminal_has_a_valid_configurable_default_profile() {
    let bundled = load_bundled_config().config;
    assert_eq!(bundled.terminals.default_profile, "shell");
    let shell = bundled
        .terminals
        .profiles
        .get("shell")
        .expect("bundled shell profile");
    assert_eq!(shell.program, PathBuf::from("$SHELL"));

    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[terminals]\ndefault_profile='missing'\n").expect("invalid default profile");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_some());
    assert_eq!(loaded.config, bundled);
}

#[test]
fn buffer_line_policy_is_typed_and_strict() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[ui.buffer_line]\nhide_when_single=false\n").expect("valid buffer line");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert!(!loaded.config.ui.buffer_line.hide_when_single);

    for invalid in ["maximum_name_width=48", "extension_icons={RS='R'}"] {
        fs::write(&path, format!("[ui.buffer_line]\n{invalid}\n")).expect("invalid buffer line");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(
            loaded.diagnostic.is_some() || loaded.warning.is_some(),
            "{invalid}"
        );
    }
}

#[test]
fn semantic_window_rules_are_typed_bounded_and_transactional() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(
            &path,
            "[windows]\nrule=[{content_type='builtin.problems',placement={kind='floating',x=4,y=3,width=50,height=12}}]\n",
        )
        .expect("valid windows");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert_eq!(loaded.config.windows.rules.len(), 1);
    assert_eq!(
        loaded.config.windows.rules[0].content_type,
        "builtin.problems"
    );
    let explanation = loaded.explain("windows.rule").expect("rule provenance");
    assert!(
        explanation
            .contributors
            .iter()
            .any(|source| source.contains("config.toml")),
        "{explanation:?}"
    );

    for invalid in [
        "[windows]\nrule=[{content_type='unknown.surface',placement={kind='hidden'}}]\n",
        "[windows]\nrule=[{content_type='builtin.picker',placement={kind='hidden'}},{content_type='builtin.picker',placement={kind='automatic'}}]\n",
        "[windows]\nrule=[{content_type='builtin.picker',placement={kind='fullscreen'}},{content_type='builtin.debug',placement={kind='fullscreen'}}]\n",
        "[windows]\nrule=[{content_type='builtin.output',placement={kind='floating',x=0,y=0,width=0,height=4}}]\n",
        "[windows]\nrule=[{content_type='builtin.logs',placement={kind='docked',edge='middle',size=4}}]\n",
    ] {
        fs::write(&path, invalid).expect("invalid windows");
        let loaded = load_config(&path);
        assert!(loaded.diagnostic.is_some(), "accepted {invalid}");
        assert_eq!(loaded.config, load_bundled_config().config);
    }
}

#[test]
fn terminal_panel_height_is_bounded() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[ui.terminal_panel]\nheight=9\n").expect("valid terminal panel");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert_eq!(loaded.config.ui.terminal_panel.height, 9);

    for invalid in ["height=2", "height=201", "resize_step=3"] {
        fs::write(&path, format!("[ui.terminal_panel]\n{invalid}\n"))
            .expect("invalid terminal panel");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config, "{invalid}");
        assert!(
            loaded.diagnostic.is_some() || loaded.warning.is_some(),
            "{invalid}"
        );
    }
}

#[test]
fn key_hint_policy_is_typed_bounded_and_transactional() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[ui.key_hints]\nenabled=false\ndelay_ms=42\n").expect("valid key hints");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert!(!loaded.config.ui.key_hints.enabled);
    assert_eq!(loaded.config.ui.key_hints.delay_ms, 42);

    for invalid in ["delay_ms=60001", "max_rows=4"] {
        fs::write(&path, format!("[ui.key_hints]\n{invalid}\n")).expect("invalid key hints");
        let loaded = load_config(&path);
        assert!(
            loaded.diagnostic.is_some() || loaded.warning.is_some(),
            "{invalid}"
        );
        assert_eq!(
            loaded.config.ui.key_hints,
            load_bundled_config().config.ui.key_hints
        );
    }
}

#[test]
fn keymaps_parse_typed_arguments_and_fail_transactionally() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("keymaps.toml");
    fs::write(
            &path,
            "[[keymap]]\nid='user.test.typed-quit'\ncontext='editor'\nmode='normal'\nkeys='space q'\ncommand='editor.quit'\narguments={force=true}\n\n[[key_group]]\nid='user.test.files-group'\ncontext='editor'\nmode='normal'\nprefix='<leader> f'\nlabel='Files'\nicon='F'\norder=7\n",
        )
        .expect("write");
    let loaded = load_keymaps(&path);
    assert!(loaded.diagnostic.is_none());
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.user_spans.len(), 1);
    assert!(
        loaded.user_spans[0].end > loaded.user_spans[0].start,
        "span was {:?}",
        loaded.user_spans[0]
    );
    assert_eq!(loaded.entries[0].id, "user.test.typed-quit");
    assert_eq!(loaded.entries[0].context.as_deref(), Some("editor"));
    assert_eq!(loaded.entries[0].keys.as_deref(), Some("space q"));
    assert_eq!(loaded.entries[0].arguments["force"].as_bool(), Some(true));
    assert_eq!(loaded.groups.len(), 1);
    assert_eq!(loaded.user_group_spans.len(), 1);
    assert_eq!(loaded.groups[0].id, "user.test.files-group");
    assert_eq!(loaded.groups[0].prefix.as_deref(), Some("<leader> f"));
    assert_eq!(loaded.groups[0].label.as_deref(), Some("Files"));
    assert_eq!(loaded.groups[0].icon.as_deref(), Some("F"));
    assert_eq!(loaded.groups[0].order, 7);

    fs::write(
        &path,
        "[[keymap]]\ncontext='editor'\nmode='normal'\nkeys='q'\ncommand='editor.quit'\n",
    )
    .expect("id-less declaration");
    let loaded = load_keymaps(&path);
    assert!(loaded.entries.is_empty());
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("missing field `id`"))
    );

    fs::write(&path, "[[keymap]\n").expect("malformed");
    let loaded = load_keymaps(&path);
    assert!(loaded.entries.is_empty());
    assert!(loaded.diagnostic.is_some());

    fs::write(
            &path,
            "[[keymap]]\nid='user.test.removed-rhs'\ncontext='editor'\nmode='normal'\nkeys='x'\nreplacement_keys='dd'\n",
        )
        .expect("unsupported recursive mapping");
    let loaded = load_keymaps(&path);
    assert!(loaded.entries.is_empty());
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("unknown field `replacement_keys`"))
    );

    fs::write(&path, "[[key_gruop]]\nid='user.test.typo'\n").expect("unknown root keymap field");
    let loaded = load_keymaps(&path);
    assert!(loaded.groups.is_empty());
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("unknown field `key_gruop`"))
    );
}

#[test]
fn global_keymaps_respect_surface_overrides_with_equivalent_key_spellings() {
    for (global, local) in [
        ("z esc", "z escape"),
        ("<leader> control-a", "<leader> ctrl-a"),
    ] {
        let loaded = crate::config::load::parse_keymaps(
            &format!(
                "[[keymap]]\nid='user.global.alias'\ncontext='global'\nkeys='{global}'\ncommand='explorer.refresh'\n\n[[keymap]]\nid='user.explorer.alias'\ncontext='explorer'\nkeys='{local}'\ncommand='explorer.refresh'\n"
            ),
            PathBuf::from("keymaps.toml"),
        );
        assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
        assert!(
            !loaded
                .entries
                .iter()
                .any(|entry| entry.id == "user.global.alias.explorer")
        );
        assert!(
            loaded
                .entries
                .iter()
                .any(|entry| entry.id == "user.explorer.alias")
        );
        assert!(
            loaded
                .entries
                .iter()
                .any(|entry| entry.id == "user.global.alias.editor-normal")
        );
    }
}

#[test]
fn global_keymaps_expand_to_every_command_surface_unless_the_surface_declares_the_keys() {
    let loaded = crate::config::load::parse_keymaps(
        r#"
[[keymap]]
id = "user.global.files"
context = "global"
keys = "<leader> f f"
command = "workspace.file.find"

[[keymap]]
id = "user.explorer.files"
context = "explorer"
keys = "<leader>  f f"
command = "explorer.refresh"

[[keymap]]
id = "builtin.global.quit"
disable = true

[[key_group]]
id = "user.global.group.files"
context = "global"
prefix = "<leader> f"
label = "Files"
"#,
        PathBuf::from("keymaps.toml"),
    );
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    let surfaces = [
        ("editor-normal", "editor", "normal"),
        ("explorer", "explorer", ""),
        ("dashboard", "dashboard", ""),
        ("terminal-normal", "terminal_normal", ""),
        ("tool-manager", "tool_manager", ""),
        ("panel", "panel", ""),
        ("agent-normal", "agent_normal", ""),
    ];
    let mut expected_bindings = surfaces
        .iter()
        .filter(|(_, context, _)| *context != "explorer")
        .map(|(suffix, context, mode)| {
            (
                format!("user.global.files.{suffix}"),
                Some(*context),
                *mode,
                Some("workspace.file.find"),
            )
        })
        .collect::<BTreeSet<_>>();
    expected_bindings.insert((
        "user.explorer.files".into(),
        Some("explorer"),
        "",
        Some("explorer.refresh"),
    ));
    let bindings = loaded
        .entries
        .iter()
        .filter(|entry| !entry.disable)
        .map(|entry| {
            (
                entry.id.clone(),
                entry.context.as_deref(),
                entry.mode.as_str(),
                entry.command.as_deref(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(bindings, expected_bindings);
    let disables = loaded
        .entries
        .iter()
        .filter(|entry| entry.disable)
        .map(|entry| entry.id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        disables,
        surfaces
            .iter()
            .map(|(suffix, _, _)| format!("builtin.global.quit.{suffix}"))
            .collect()
    );
    let groups = loaded
        .groups
        .iter()
        .map(|group| {
            (
                group.id.clone(),
                group.context.as_deref(),
                group.mode.as_str(),
                group.prefix.as_deref(),
                group.label.as_deref(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        groups,
        surfaces
            .iter()
            .map(|(suffix, context, mode)| (
                format!("user.global.group.files.{suffix}"),
                Some(*context),
                *mode,
                Some("<leader> f"),
                Some("Files"),
            ))
            .collect()
    );
    assert_eq!(loaded.entries.len(), 14);
    assert_eq!(loaded.entries.len(), loaded.user_spans.len());
    assert_eq!(loaded.groups.len(), 7);
    assert_eq!(loaded.groups.len(), loaded.user_group_spans.len());
}

#[test]
fn highlight_groups_resolve_like_neovim() {
    use crate::config::Highlight;
    let group = |name| Highlight::named(name).expect(name);
    let colour = |theme: &crate::config::ThemeConfig, name| {
        theme.highlight(group(name)).map(|style| style.foreground)
    };

    // Every bundled link lands on a group with a definition, so ports keep their colours.
    let mut unresolved = Vec::new();
    for (name, _) in crate::config::themes::BUNDLED {
        let theme = crate::config::themes::bundled_named(name).unwrap().theme;
        for (group, definition) in &theme.highlights {
            if let Some(link) = &definition.link
                && theme.resolve(link).is_none()
            {
                unresolved.push(format!("{name}: {group} links to undefined {link}"));
            }
        }
    }
    assert!(unresolved.is_empty(), "{}", unresolved.join("\n"));
    let tokyo = crate::config::themes::bundled_named("tokyo-night")
        .unwrap()
        .theme;
    // `@function.method.call` links to `@function.method`, which links to `Function`.
    assert_eq!(
        colour(&tokyo, "@function.method.call"),
        Some(crate::config::ThemeColor::Rgb(0x7a, 0xa2, 0xf7))
    );
    let keyword = tokyo.highlight(Highlight::KEYWORD).unwrap();
    assert!(
        keyword
            .attributes
            .contains(&crate::config::ThemeAttribute::Italic)
    );
    assert_eq!(
        keyword.foreground_256,
        Some(crate::config::ThemeColor::Indexed(140))
    );

    let mut dust = crate::config::themes::bundled_named("dust").unwrap().theme;
    assert!(dust.resolve("@missing.nonexistent").is_none());
    assert_eq!(
        dust.resolve("@function.custom.child"),
        dust.resolve("@function")
    );
    dust.highlights.insert(
        "@test.broken".into(),
        crate::config::HighlightDefinition {
            link: Some("@missing.nonexistent".into()),
            ..Default::default()
        },
    );
    assert!(dust.resolve("@test.broken").is_none());
    dust.highlights.insert(
        "@function".into(),
        crate::config::HighlightDefinition {
            link: Some("@function.method".into()),
            ..Default::default()
        },
    );
    dust.highlights.insert(
        "@function.method".into(),
        crate::config::HighlightDefinition {
            link: Some("@function".into()),
            ..Default::default()
        },
    );
    // Nothing defines `@markup.heading.1`, so it takes `@markup.heading`, which Neovim links to `Title`.
    assert_eq!(
        colour(&dust, "@markup.heading.1"),
        Some(crate::config::ThemeColor::Rgb(0x58, 0xa6, 0xff))
    );
    // A theme that leaves a token type out keeps tree-sitter's colour.
    assert_eq!(dust.highlight(group("@lsp.type.derive")), None);
    // Links that loop colour nothing.
    assert_eq!(dust.highlight(Highlight::FUNCTION), None);
    // Groups without a background draw on the editor's.
    assert_eq!(
        dust.highlight(Highlight::STRING).unwrap().background,
        dust.styles.default.background
    );
}

#[test]
fn theme_defined_language_captures_are_selected_and_inherit_in_other_themes() {
    use crate::config::Highlight;
    use crate::language::syntax::SyntaxLanguage;
    use crate::language::syntax::highlight;
    let fallback = crate::config::themes::bundled_named("dust").unwrap().theme;
    let expected = fallback.highlight(Highlight::FUNCTION).unwrap().clone();
    // Existing syntax spans must honor a theme loaded later without another edit.
    let spans = highlight(SyntaxLanguage::Rust, "fn custom() {}\n");
    let mut value = serde_json::to_value(&fallback).unwrap();
    value["highlights"]["@function.rust"] = serde_json::json!({ "fg": "#123456" });
    let theme: crate::config::ThemeConfig = serde_json::from_value(value).unwrap();
    let group = Highlight::named("@function.rust").unwrap();
    assert!(spans.iter().any(|span| span.role == group));
    assert_eq!(
        theme.highlight(group).unwrap().foreground,
        crate::config::ThemeColor::Rgb(0x12, 0x34, 0x56)
    );
    assert_eq!(fallback.highlight(group), Some(&expected));
    // Use a unique group so parallel syntax tests cannot register it before the cache.
    let late = Highlight::register("@function.late_theme_test").unwrap();
    assert_eq!(fallback.highlight(late), Some(&expected));
}

#[test]
fn a_saved_bundled_theme_restores_from_this_build() {
    let (reply, receiver) = std::sync::mpsc::sync_channel(1);
    // Saved by an older build, with a style field that no longer exists.
    let saved = r##"{"bundled":true,"name":"tokyo-night","styles":{"syntax_comment":{"foreground":"#a9b1d6"}}}"##;
    crate::config::themes::discover(crate::config::themes::ThemeRead {
        source: crate::config::themes::ThemeReadSource::Saved(saved.into()),
        cancelled: Default::default(),
        reply,
    });
    let catalog = receiver.recv().unwrap();
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
    assert_eq!(
        catalog.choices[0].theme,
        crate::config::themes::bundled_named("tokyo-night")
            .unwrap()
            .theme
    );
}

#[test]
fn rebinding_a_shortcut_writes_one_entry_and_reset_removes_it() {
    let root = tempdir().unwrap();
    let path = root.path().join("keymaps.toml");
    fs::write(&path, "# my keys\n").unwrap();
    let declared = crate::config::declared_keymaps(Some(&path));
    let save = declared
        .iter()
        .find(|keymap| keymap.entry.id == "builtin.global.search-keymaps")
        .unwrap();
    assert!(!save.user);
    crate::config::write_user_keymap(&path, &save.entry, "<leader> s K").unwrap();
    crate::config::write_user_keymap(&path, &save.entry, "ctrl-g").unwrap();
    let source = fs::read_to_string(&path).unwrap();
    assert!(source.starts_with("# my keys\n"), "{source}");
    assert_eq!(source.matches("[[keymap]]").count(), 1, "{source}");
    let declared = crate::config::declared_keymaps(Some(&path));
    let rebound: Vec<_> = declared
        .iter()
        .filter(|keymap| keymap.entry.id == "builtin.global.search-keymaps")
        .collect();
    assert_eq!(rebound.len(), 1);
    assert!(rebound[0].user);
    assert_eq!(rebound[0].entry.keys.as_deref(), Some("ctrl-g"));
    assert_eq!(
        rebound[0].entry.command.as_deref(),
        Some("settings.shortcuts")
    );
    // Arguments are copied, so the replacement does the same thing.
    let moving = declared
        .iter()
        .find(|keymap| keymap.entry.id == "builtin.settings.move.k")
        .unwrap();
    crate::config::write_user_keymap(&path, &moving.entry, "ctrl-k").unwrap();
    let moved = crate::config::declared_keymaps(Some(&path))
        .into_iter()
        .find(|keymap| keymap.entry.id == "builtin.settings.move.k")
        .unwrap();
    assert_eq!(moved.entry.arguments, moving.entry.arguments);
    crate::config::remove_user_keymap(&path, "builtin.global.search-keymaps").unwrap();
    crate::config::remove_user_keymap(&path, "builtin.settings.move.k").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "# my keys\n");
}

#[test]
fn every_setup_choice_sets_a_real_setting_to_a_valid_value() {
    let bundled = setting_keys(&toml::from_str::<toml::Value>(BUNDLED_CONFIG).unwrap());
    for page in &crate::config::setup_catalog().pages {
        if let Some(key) = &page.key {
            assert!(
                bundled.contains(key),
                "setup page {} sets unknown {key}",
                page.title
            );
        }
        assert!(
            page.choices_from.is_some() || !page.choices.is_empty(),
            "setup page {} has nothing to choose",
            page.title
        );
        assert!(
            ["layout", "terminal", "agent", "leader"].contains(&page.preview.as_str()),
            "setup page {} shows an unknown preview {:?}",
            page.title,
            page.preview
        );
        for choice in &page.choices {
            for (key, value) in &choice.settings {
                assert!(bundled.contains(key), "{} sets unknown {key}", choice.label);
                let source =
                    crate::config::catalog::edited_source("", key, Some(value.clone())).unwrap();
                crate::config::load::settings_from_source(&source)
                    .unwrap_or_else(|error| panic!("{}: {key}: {error}", choice.label));
            }
        }
    }
}

#[test]
fn settings_catalog_lists_every_bundled_setting_once() {
    let catalog = crate::config::settings_catalog();
    let bundled = setting_keys(&toml::from_str::<toml::Value>(BUNDLED_CONFIG).unwrap());
    let in_collection = |key: &str| {
        catalog
            .collections
            .iter()
            .any(|prefix| key == prefix || key.starts_with(&format!("{prefix}.")))
    };
    let listed: Vec<&str> = catalog
        .sections
        .iter()
        .flat_map(|section| &section.settings)
        .map(|setting| setting.key.as_str())
        .collect();
    let missing: Vec<&String> = bundled
        .iter()
        .filter(|key| !listed.contains(&key.as_str()) && !in_collection(key))
        .collect();
    assert!(
        missing.is_empty(),
        "settings missing from settings.toml: {missing:?}"
    );
    let stale: Vec<&&str> = listed
        .iter()
        .filter(|key| !bundled.contains(**key))
        .collect();
    assert!(
        stale.is_empty(),
        "settings.toml lists unknown settings: {stale:?}"
    );
    let unique: BTreeSet<&&str> = listed.iter().collect();
    assert_eq!(
        unique.len(),
        listed.len(),
        "settings.toml lists a setting twice"
    );
    for section in &catalog.sections {
        for key in &section.common {
            assert!(
                catalog.setting(key).is_some(),
                "{} lists unknown {key}",
                section.title
            );
        }
    }
    for setting in catalog
        .sections
        .iter()
        .flat_map(|section| &section.settings)
    {
        assert!(
            !setting.about.is_empty(),
            "{} needs a description",
            setting.key
        );
        if let Some(table) = &setting.choices_from {
            assert!(
                crate::config::default_setting(table).is_some_and(|value| value.is_table()),
                "{table}"
            );
        }
        for choice in &setting.choices {
            let source = crate::config::catalog::edited_source(
                "",
                &setting.key,
                Some(toml::Value::String(choice.clone())),
            )
            .unwrap();
            assert!(
                crate::config::load::settings_from_source(&source).is_ok(),
                "{} = {choice} is offered but rejected",
                setting.key
            );
        }
    }
}

#[test]
fn resetting_the_last_inline_setting_preserves_parent_comments() {
    for tail in ["", "wrap = true\n"] {
        let source = format!("# keep this\neditor = {{ tab_width = 2 }} # explanation\n{tail}");
        let reset =
            crate::config::catalog::edited_source(&source, "editor.tab_width", None).unwrap();
        assert_eq!(reset, format!("# keep this\n# explanation\n{tail}"));
    }
}

#[test]
fn a_setting_is_changed_in_place_and_reset_removes_it_but_keeps_its_comments() {
    let edited = |source: &str, key: &str, value: Option<toml::Value>| {
        crate::config::catalog::edited_source(source, key, value).unwrap()
    };
    let source = "# my settings\n[editor]\n# wrap long lines\nwrap = false # for now\n";
    let wrapped = edited(source, "editor.wrap", Some(toml::Value::Boolean(true)));
    assert_eq!(
        wrapped,
        "# my settings\n[editor]\n# wrap long lines\nwrap = true # for now\n"
    );
    let added = edited(
        &wrapped,
        "editor.git_signs.enabled",
        Some(toml::Value::Boolean(false)),
    );
    assert!(added.starts_with(&wrapped), "{added}");
    assert!(added.contains("enabled = false"), "{added}");
    let reset = edited(&added, "editor.git_signs.enabled", None);
    assert_eq!(reset, wrapped);
    // Reset keeps the setting's comments: under the table header when it was the last key,
    // above the next key otherwise.
    let emptied = edited(&reset, "editor.wrap", None);
    assert_eq!(
        emptied,
        "# my settings\n[editor]\n# wrap long lines\n# for now\n"
    );
    let next = edited(
        "[editor]\n# width comment\ntab_width = 4\nwrap = true\n",
        "editor.tab_width",
        None,
    );
    assert_eq!(next, "[editor]\n# width comment\nwrap = true\n");
    let plain = edited("[editor]\ntab_width = 4\n", "editor.tab_width", None);
    assert_eq!(plain, "");
    let file = tempdir().unwrap();
    let path = file.path().join("dust").join("config.toml");
    crate::config::write_user_setting(&path, "editor.tab_width", Some(toml::Value::Integer(2)))
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[editor]\ntab_width = 2\n"
    );
    let rejected = crate::config::write_user_setting(
        &path,
        "editor.tab_width",
        Some(toml::Value::String("x".into())),
    );
    assert!(rejected.is_err());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[editor]\ntab_width = 2\n"
    );
}

#[test]
fn settings_save_rejects_growth_past_the_configuration_byte_limit() {
    let root = tempdir().unwrap();
    let path = root.path().join("config.toml");
    let mut source = "[editor]\ntab_width = 2\n#".to_owned();
    source.extend(std::iter::repeat_n(
        ' ',
        crate::config::load::WATCH_FINGERPRINT_BYTE_LIMIT as usize - source.len() - 1,
    ));
    source.push('\n');
    fs::write(&path, &source).unwrap();
    let error = crate::config::write_user_setting(
        &path,
        "editor.tab_width",
        Some(toml::Value::Integer(32)),
    )
    .unwrap_err();
    assert!(error.contains("byte limit"), "{error}");
    assert_eq!(fs::read_to_string(path).unwrap(), source);
}

#[test]
fn deep_task_dependencies_are_validated_without_recursion() {
    let task: crate::config::settings::TaskConfig =
        toml::from_str(&format!("executable = '{}'", true_executable())).unwrap();
    let mut config = load_bundled_config().config;
    for index in 0..50_000 {
        let mut task = task.clone();
        if index < 49_999 {
            task.dependencies.push(format!("t{:05}", index + 1));
        }
        config.tasks.insert(format!("t{index:05}"), task);
    }
    assert!(config.validate().is_ok());
    config
        .tasks
        .get_mut("t49999")
        .unwrap()
        .dependencies
        .push("t00000".into());
    assert!(config.validate().is_err());
}

// Creates a Unix symlink without requiring Windows developer privileges.
#[cfg(unix)]
#[test]
fn settings_save_preserves_a_symlink() {
    let root = tempdir().unwrap();
    let target = root.path().join("dotfiles.toml");
    let path = root.path().join("config.toml");
    fs::write(&target, "# preserved\n[editor]\ntab_width = 4\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    crate::config::write_user_setting(&path, "editor.tab_width", Some(toml::Value::Integer(3)))
        .unwrap();
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(target)
            .unwrap()
            .contains("tab_width = 3")
    );
}

#[test]
fn unknown_fields_in_window_rule_arrays_preserve_valid_settings() {
    for (rules, expected_count) in [
        (
            "[[windows.rule]]\ncontent_type='builtin.problems'\nplacement={kind='hidden'}\nmisspelled=true\n",
            1,
        ),
        (
            "[windows]\nrule=[{content_type='builtin.problems',placement={kind='hidden'},misspelled=true}]\n",
            1,
        ),
        (
            "[[windows.rule]]\ncontent_type='builtin.problems'\nplacement={kind='hidden'}\n[[windows.rule]]\ncontent_type='builtin.output'\nplacement={kind='hidden'}\nmisspelled=true\n",
            2,
        ),
    ] {
        let source = format!("[editor]\ntab_width=3\n{rules}");
        let (config, _, _, ignored) = crate::config::load::settings_from_source(&source).unwrap();
        assert_eq!(config.editor.tab_width, 3);
        assert_eq!(config.windows.rules.len(), expected_count);
        assert_eq!(config.windows.rules[0].content_type, "builtin.problems");
        assert_eq!(ignored.len(), 1);
        assert!(ignored[0].ends_with("misspelled"));
    }
}

#[test]
fn unknown_fields_in_window_rules_do_not_remove_valid_fields_from_other_variants() {
    let source = "[[windows.rule]]\ncontent_type='builtin.problems'\nplacement={kind='floating',x=1,y=2,width=30,height=10}\n[[windows.rule]]\ncontent_type='builtin.output'\nplacement={kind='docked',edge='bottom',size=5,x=99}\n";
    let (config, _, _, ignored) = crate::config::load::settings_from_source(source).unwrap();
    assert_eq!(config.windows.rules.len(), 2);
    assert_eq!(
        config.windows.rules[0].placement,
        crate::config::WindowPlacementConfig::Floating {
            x: 1,
            y: 2,
            width: 30,
            height: 10,
        }
    );
    assert_eq!(
        config.windows.rules[1].placement,
        crate::config::WindowPlacementConfig::Docked {
            edge: "bottom".into(),
            size: 5,
        }
    );
    assert_eq!(ignored.len(), 1);
}

#[test]
fn unknown_fields_under_dotted_task_names_preserve_valid_settings() {
    let (config, _, _, ignored) = crate::config::load::settings_from_source(&format!(
        "[editor]\ntab_width=3\n[tasks.'build.all']\nexecutable='{}'\nunknown=true\n",
        true_executable()
    ))
    .unwrap();
    assert_eq!(config.editor.tab_width, 3);
    assert!(config.tasks.contains_key("build.all"));
    assert_eq!(ignored, vec!["tasks.build.all.unknown"]);
}

#[test]
fn installed_dust_overrides_and_saved_namesakes_keep_their_colours() {
    let mut installed = load_bundled_config().theme;
    installed.styles.default.foreground = crate::config::ThemeColor::Red;
    let theme_source = toml::to_string(&installed).unwrap();
    let bundle = capture_configuration_bundle_with("dust", |path| {
        captured(
            path.clone(),
            if path.file_name().is_some_and(|name| name == "dust.toml") {
                CapturedText::Text(theme_source.clone())
            } else {
                CapturedText::Missing
            },
        )
    });
    assert_eq!(bundle.into_loaded().theme, installed);
    let choice = crate::config::themes::ThemeChoice {
        theme: installed.clone(),
        path: "/themes/dust.toml".into(),
    };
    let (reply, receiver) = std::sync::mpsc::sync_channel(1);
    crate::config::themes::discover(crate::config::themes::ThemeRead {
        source: crate::config::themes::ThemeReadSource::Saved(
            crate::config::themes::snapshot(&choice).unwrap(),
        ),
        cancelled: Default::default(),
        reply,
    });
    assert_eq!(receiver.recv().unwrap().choices[0].theme, installed);
}

#[test]
fn terminal_highlights_prefer_explicit_cterm_over_rgb_for_ansi16() {
    use crate::config::Highlight;
    use crate::config::HighlightDefinition;
    use crate::config::ThemeColor;
    for (index, expected) in [(1, ThemeColor::DarkRed), (196, ThemeColor::Red)] {
        let mut theme: crate::config::ThemeConfig = toml::from_str(BUNDLED_THEME).unwrap();
        theme.highlights.insert(
            "@function".into(),
            HighlightDefinition {
                fg: Some(ThemeColor::Rgb(0, 0, 255)),
                bg: Some(ThemeColor::Rgb(0, 255, 0)),
                ctermfg: Some(index),
                ctermbg: Some(index),
                ..Default::default()
            },
        );
        let style = theme.highlight(Highlight::FUNCTION).unwrap();
        assert_eq!(style.foreground, ThemeColor::Rgb(0, 0, 255));
        assert_eq!(style.background, ThemeColor::Rgb(0, 255, 0));
        assert_eq!(style.foreground_256, Some(ThemeColor::Indexed(index)));
        assert_eq!(style.background_256, Some(ThemeColor::Indexed(index)));
        assert_eq!(style.foreground_16, Some(expected));
        assert_eq!(style.background_16, Some(expected));
    }
}

#[test]
fn markdown_code_recomputes_both_background_fallbacks() {
    use crate::config::Highlight;
    use crate::config::HighlightDefinition;
    use crate::config::ThemeColor;
    for background in [ThemeColor::DarkRed, ThemeColor::Indexed(9)] {
        let mut theme: crate::config::ThemeConfig = toml::from_str(BUNDLED_THEME).unwrap();
        theme.styles.default.foreground = ThemeColor::Rgb(255, 255, 255);
        theme.styles.default.background = ThemeColor::Rgb(0, 0, 0);
        theme.highlights.insert(
            "@function".into(),
            HighlightDefinition {
                bg: Some(background),
                ..Default::default()
            },
        );
        let style = theme.markdown_code(Some(Highlight::FUNCTION));
        assert_eq!(style.background, ThemeColor::Rgb(20, 20, 20));
        assert_eq!(style.background_256, Some(ThemeColor::Indexed(233)));
        assert_eq!(style.background_16, Some(ThemeColor::Black));
    }
}

#[test]
fn terminal_highlights_keep_cterm_only_and_ansi_palette_colours() {
    use crate::config::Highlight;
    use crate::config::HighlightDefinition;
    use crate::config::ThemeColor;
    for (index, expected) in [
        ThemeColor::Black,
        ThemeColor::DarkRed,
        ThemeColor::DarkGreen,
        ThemeColor::DarkYellow,
        ThemeColor::DarkBlue,
        ThemeColor::DarkMagenta,
        ThemeColor::DarkCyan,
        ThemeColor::Grey,
        ThemeColor::DarkGrey,
        ThemeColor::Red,
        ThemeColor::Green,
        ThemeColor::Yellow,
        ThemeColor::Blue,
        ThemeColor::Magenta,
        ThemeColor::Cyan,
        ThemeColor::White,
    ]
    .into_iter()
    .enumerate()
    {
        let mut theme: crate::config::ThemeConfig = toml::from_str(BUNDLED_THEME).unwrap();
        theme.highlights.insert(
            "@function".into(),
            HighlightDefinition {
                ctermfg: Some(index as u8),
                ctermbg: Some(index as u8),
                ..Default::default()
            },
        );
        let style = theme.highlight(Highlight::FUNCTION).unwrap();
        assert_eq!(style.foreground_256, Some(ThemeColor::Indexed(index as u8)));
        assert_eq!(style.background_16, Some(expected));
        let mut theme: crate::config::ThemeConfig = toml::from_str(BUNDLED_THEME).unwrap();
        theme.highlights.insert(
            "@function".into(),
            HighlightDefinition {
                fg: Some(ThemeColor::Indexed(index as u8)),
                ..Default::default()
            },
        );
        assert_eq!(
            theme.highlight(Highlight::FUNCTION).unwrap().foreground_16,
            Some(expected)
        );
    }
}

#[test]
fn config_migration_rejects_noninteger_schema_versions() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    for source in [
        "schema_version = \"999\"\n",
        "schema_version = 1.0\n",
        "schema_version = true\n",
        "schema_version = []\n",
        "[schema_version]\n",
    ] {
        let error = crate::config::migrate_config_source(source).unwrap_err();
        assert!(
            error.contains("schema_version must be an integer"),
            "{error}"
        );
        fs::write(&path, source).unwrap();
        let loaded = load_config(&path);
        assert!(
            loaded
                .diagnostic
                .unwrap()
                .contains("schema_version must be an integer")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
    }
    for source in ["", "schema_version = 0\n"] {
        let (migrated, changed) = crate::config::migrate_config_source(source).unwrap();
        assert!(changed);
        assert!(migrated.contains("schema_version = 1"));
    }
    let current = "schema_version = 1\n";
    assert_eq!(
        crate::config::migrate_config_source(current).unwrap(),
        (current.into(), false)
    );
    assert!(crate::config::migrate_config_source("schema_version = 999\n").is_err());
}

#[test]
fn config_migration_rejects_changes_since_the_source_was_read() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "# original\n").unwrap();
    let source = std::fs::read_to_string(&path).unwrap();
    let (migrated, _) = crate::config::migrate_config_source(&source).unwrap();
    std::fs::write(&path, "# edited elsewhere\n").unwrap();
    assert!(matches!(
        crate::config::apply_config_migration(&path, &source, &migrated),
        Err(crate::files::VfsError::ExternalModification(_))
    ));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "# edited elsewhere\n"
    );
}

#[cfg(unix)]
#[test]
fn config_migration_preserves_symlinks_and_updates_their_target() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("dotfiles.toml");
    let link = directory.path().join("config.toml");
    std::fs::write(&target, "# original\n").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let source = std::fs::read_to_string(&link).unwrap();
    let (migrated, _) = crate::config::migrate_config_source(&source).unwrap();
    crate::config::apply_config_migration(&link, &source, &migrated).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(target).unwrap(), migrated);
}

/// An absolute executable path in the host's form; validation only checks its shape.
fn true_executable() -> String {
    crate::files::absolute_test_path("/bin/true")
        .display()
        .to_string()
}
