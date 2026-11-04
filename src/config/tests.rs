use std::{cell::Cell, collections::BTreeSet, fs, path::PathBuf};

use tempfile::tempdir;

use super::load::{
    BUNDLED_CONFIG, BUNDLED_THEME, CapturedSource, CapturedText,
    ConfigurationSourceBundle, SourceFingerprint, WATCH_FINGERPRINT_BYTE_CEILING,
    capture_configuration_bundle_with, capture_consistent_configuration_bundle_with,
    captured_bundle_is_stable, hash_source_bytes, load_consistent_configuration, load_theme,
    setting_keys, setting_spans, source_fingerprint,
};
use super::{
    AmbiguousWidth, ExplorerIconMode, LineNumbers, load_bundled_config, load_config, load_keymaps,
};

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
    let mut installed = super::themes::bundled_named("nord").unwrap().theme;
    installed.styles.default.foreground = super::ThemeColor::Red;
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
            assert_eq!(loaded.theme_path, super::user_theme_path("nord"));
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
        super::InlayHintLevel::Standard
    );
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[lsp]\ninlay_hint_level = 'max'\n").expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert_eq!(
        loaded.config.lsp.inlay_hint_level,
        super::InlayHintLevel::Max
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
        (r#"command_labels."caf\u00e9""#, vec!["command_labels", "café"]),
        (r#"labels."a\\b\"c""#, vec!["labels", "a\\b\"c"]),
        (r#"labels.'a\b.c'"#, vec!["labels", "a\\b.c"]),
        (r#"labels."""#, vec!["labels", ""]),
        ("dashboard.action.0.label", vec!["dashboard", "action", "0", "label"]),
    ] {
        assert_eq!(super::config_key_segments(key).unwrap(), expected, "{key}");
    }
    for invalid in ["", "a..b", "a.", "\"unterminated", r#"a."\q""#] {
        assert!(super::config_key_segments(invalid).is_none(), "{invalid}");
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
            Some("dashboard.toml") => include_str!("../../assets/defaults/dashboard.toml")
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
    let dashboard =
        include_str!("../../assets/defaults/dashboard.toml").replace("D U S T", "CAPTURED DASH");
    let theme = include_str!("../../assets/themes/dust.toml")
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
        super::user_config_path(),
        CapturedText::Text("[ui]\ntheme='dust'\n".into()),
    );
    let missing = captured(super::user_keymaps_path(), CapturedText::Missing);
    let created = captured(
            super::user_keymaps_path(),
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
    assert_eq!(fingerprint, configuration_bundle("dust", 1, true).identity());
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
    assert_eq!(fingerprint, configuration_bundle("night", 1, true).identity());
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
    assert!(result.is_err(), "a transient capture must never be accepted");
    assert_eq!(loads.get(), super::load::CONFIGURATION_CONSISTENCY_ATTEMPTS);
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
        assert_eq!(fingerprint, configuration_bundle("dust", 1, final_valid).identity());
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
    assert_eq!(fingerprint, configuration_bundle("dust", 1, true).identity());
    let candidate = bundle.into_loaded();
    assert!(candidate.diagnostic.is_none(), "{:?}", candidate.diagnostic);
    assert_eq!(candidate.config.editor.tab_width, 3);
    assert_eq!(candidate.keymaps.entries.last().unwrap().id, "user.revision-1");
    assert_eq!(candidate.dashboard.identity, "dashboard-1");
    assert_eq!(candidate.theme.styles.default.foreground, super::ThemeColor::Rgb(0xc9, 0xd1, 0xd8));
}

#[test]
fn clean_consistent_load_never_fingerprints_user_sources() {
    let loaded = load_consistent_configuration(false, "unused-user-theme")
        .expect("bundled candidate");
    assert!(loaded.fingerprint.is_none());
    let candidate = loaded.candidate.expect("valid bundled candidate");
    assert_eq!(candidate.config, load_bundled_config().config);
    assert!(!candidate.user_configuration);
    assert_eq!(candidate.path, PathBuf::from("<bundled:config>"));
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

    let oversized = vec![b' '; WATCH_FINGERPRINT_BYTE_CEILING as usize + 1];
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
    let declared = toml::from_str::<toml::Value>(BUNDLED_CONFIG).expect("bundled source value");
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
    assert_eq!(loaded.dashboard.action.len(), 9);
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
        super::ThemeColor::Indexed(201)
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
    assert_eq!(shell.program, PathBuf::from("/bin/sh"));

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
fn global_keymaps_expand_to_every_command_surface_unless_the_surface_declares_the_keys() {
    let loaded = super::load::parse_keymaps(
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
        .map(|(suffix, context, mode)| (
            format!("user.global.files.{suffix}"),
            Some(*context),
            *mode,
            Some("workspace.file.find"),
        ))
        .collect::<BTreeSet<_>>();
    expected_bindings.insert((
        "user.explorer.files".into(), Some("explorer"), "", Some("explorer.refresh"),
    ));
    let bindings = loaded.entries.iter().filter(|entry| !entry.disable).map(|entry| (
        entry.id.clone(), entry.context.as_deref(), entry.mode.as_str(), entry.command.as_deref(),
    )).collect::<BTreeSet<_>>();
    assert_eq!(bindings, expected_bindings);
    let disables = loaded.entries.iter().filter(|entry| entry.disable)
        .map(|entry| entry.id.clone()).collect::<BTreeSet<_>>();
    assert_eq!(disables, surfaces.iter().map(|(suffix, _, _)|
        format!("builtin.global.quit.{suffix}")
    ).collect());
    let groups = loaded.groups.iter().map(|group| (
        group.id.clone(), group.context.as_deref(), group.mode.as_str(),
        group.prefix.as_deref(), group.label.as_deref(),
    )).collect::<BTreeSet<_>>();
    assert_eq!(groups, surfaces.iter().map(|(suffix, context, mode)| (
        format!("user.global.group.files.{suffix}"), Some(*context), *mode,
        Some("<leader> f"), Some("Files"),
    )).collect());
    assert_eq!(loaded.entries.len(), 14);
    assert_eq!(loaded.entries.len(), loaded.user_spans.len());
    assert_eq!(loaded.groups.len(), 7);
    assert_eq!(loaded.groups.len(), loaded.user_group_spans.len());
}

#[test]
fn highlight_groups_resolve_like_neovim() {
    use crate::language::syntax::Highlight;
    let group = |name| Highlight::named(name).expect(name);
    let colour = |theme: &super::ThemeConfig, name| {
        theme.highlight(group(name)).map(|style| style.foreground)
    };

    // Every bundled link lands on a group with a definition, so ports keep their colours.
    let mut unresolved = Vec::new();
    for (name, _) in super::themes::BUNDLED {
        let theme = super::themes::bundled_named(name).unwrap().theme;
        for (group, definition) in &theme.highlights {
            if let Some(link) = &definition.link
                && theme.resolve(link).is_none()
            {
                unresolved.push(format!("{name}: {group} links to undefined {link}"));
            }
        }
    }
    assert!(unresolved.is_empty(), "{}", unresolved.join("\n"));
    let tokyo = super::themes::bundled_named("tokyo-night").unwrap().theme;
    // `@function.method.call` links to `@function.method`, which links to `Function`.
    assert_eq!(
        colour(&tokyo, "@function.method.call"),
        Some(super::ThemeColor::Rgb(0x7a, 0xa2, 0xf7))
    );
    let keyword = tokyo.highlight(Highlight::KEYWORD).unwrap();
    assert!(keyword.attributes.contains(&super::ThemeAttribute::Italic));
    assert_eq!(
        keyword.foreground_256,
        Some(super::ThemeColor::Indexed(140))
    );

    let mut dust = super::themes::bundled_named("dust").unwrap().theme;
    assert!(dust.resolve("@missing.nonexistent").is_none());
    assert_eq!(dust.resolve("@function.custom.child"), dust.resolve("@function"));
    dust.highlights.insert(
        "@test.broken".into(),
        super::HighlightDefinition {
            link: Some("@missing.nonexistent".into()),
            ..Default::default()
        },
    );
    assert!(dust.resolve("@test.broken").is_none());
    dust.highlights.insert(
        "@function".into(),
        super::HighlightDefinition {
            link: Some("@function.method".into()),
            ..Default::default()
        },
    );
    dust.highlights.insert(
        "@function.method".into(),
        super::HighlightDefinition {
            link: Some("@function".into()),
            ..Default::default()
        },
    );
    // Nothing defines `@markup.heading.1`, so it takes `@markup.heading`, which Neovim links to `Title`.
    assert_eq!(
        colour(&dust, "@markup.heading.1"),
        Some(super::ThemeColor::Rgb(0x58, 0xa6, 0xff))
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
fn a_saved_bundled_theme_restores_from_this_build() {
    let (reply, receiver) = std::sync::mpsc::sync_channel(1);
    // Saved by an older build, with a style field that no longer exists.
    let saved = r##"{"bundled":true,"name":"tokyo-night","styles":{"syntax_comment":{"foreground":"#a9b1d6"}}}"##;
    super::themes::discover(super::themes::ThemeRead {
        source: super::themes::ThemeReadSource::Saved(saved.into()),
        cancelled: Default::default(),
        reply,
    });
    let catalog = receiver.recv().unwrap();
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
    assert_eq!(
        catalog.choices[0].theme,
        super::themes::bundled_named("tokyo-night").unwrap().theme
    );
}

#[test]
fn settings_catalog_lists_every_bundled_setting_once() {
    let catalog = super::settings_catalog();
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
                super::default_setting(table).is_some_and(|value| value.is_table()),
                "{table}"
            );
        }
        for choice in &setting.choices {
            let source = super::catalog::edited_source(
                "",
                &setting.key,
                Some(toml::Value::String(choice.clone())),
            )
            .unwrap();
            assert!(
                super::load::settings_from_source(&source).is_ok(),
                "{} = {choice} is offered but rejected",
                setting.key
            );
        }
    }
}

#[test]
fn a_setting_is_changed_in_place_and_reset_removes_it_but_keeps_its_comments() {
    let edited = |source: &str, key: &str, value: Option<toml::Value>| {
        super::catalog::edited_source(source, key, value).unwrap()
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
    super::write_user_setting(&path, "editor.tab_width", Some(toml::Value::Integer(2))).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[editor]\ntab_width = 2\n"
    );
    let rejected = super::write_user_setting(
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
fn deep_task_dependencies_are_validated_without_recursion() {
    let task: super::settings::TaskConfig = toml::from_str("executable = '/bin/true'").unwrap();
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

#[test]
fn settings_save_preserves_a_symlink() {
    let root = tempdir().unwrap();
    let target = root.path().join("dotfiles.toml");
    let path = root.path().join("config.toml");
    fs::write(&target, "# preserved\n[editor]\ntab_width = 4\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    super::write_user_setting(&path, "editor.tab_width", Some(toml::Value::Integer(3))).unwrap();
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
fn unknown_fields_under_dotted_task_names_preserve_valid_settings() {
    let (config, _, _, ignored) = super::load::settings_from_source(
        "[editor]\ntab_width=3\n[tasks.'build.all']\nexecutable='/bin/true'\nunknown=true\n",
    )
    .unwrap();
    assert_eq!(config.editor.tab_width, 3);
    assert!(config.tasks.contains_key("build.all"));
    assert_eq!(ignored, vec!["tasks.build.all.unknown"]);
}

#[test]
fn installed_dust_overrides_and_saved_namesakes_keep_their_colours() {
    let mut installed = load_bundled_config().theme;
    installed.styles.default.foreground = super::ThemeColor::Red;
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
    let choice = super::themes::ThemeChoice {
        theme: installed.clone(),
        path: "/themes/dust.toml".into(),
    };
    let (reply, receiver) = std::sync::mpsc::sync_channel(1);
    super::themes::discover(super::themes::ThemeRead {
        source: super::themes::ThemeReadSource::Saved(super::themes::snapshot(&choice).unwrap()),
        cancelled: Default::default(),
        reply,
    });
    assert_eq!(receiver.recv().unwrap().choices[0].theme, installed);
}

#[test]
fn terminal_highlights_keep_cterm_only_and_ansi_palette_colours() {
    use super::{HighlightDefinition, ThemeColor};
    use crate::language::syntax::Highlight;
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
        let mut theme: super::ThemeConfig = toml::from_str(BUNDLED_THEME).unwrap();
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
        let mut theme: super::ThemeConfig = toml::from_str(BUNDLED_THEME).unwrap();
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
        let error = super::migrate_config_source(source).unwrap_err();
        assert!(error.contains("schema_version must be an integer"), "{error}");
        fs::write(&path, source).unwrap();
        let loaded = load_config(&path);
        assert!(loaded.diagnostic.unwrap().contains("schema_version must be an integer"));
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
    }
    for source in ["", "schema_version = 0\n"] {
        let (migrated, changed) = super::migrate_config_source(source).unwrap();
        assert!(changed);
        assert!(migrated.contains("schema_version = 1"));
    }
    let current = "schema_version = 1\n";
    assert_eq!(super::migrate_config_source(current).unwrap(), (current.into(), false));
    assert!(super::migrate_config_source("schema_version = 999\n").is_err());
}

#[test]
fn config_migration_rejects_changes_since_the_source_was_read() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "# original\n").unwrap();
    let source = std::fs::read_to_string(&path).unwrap();
    let (migrated, _) = super::migrate_config_source(&source).unwrap();
    std::fs::write(&path, "# edited elsewhere\n").unwrap();
    assert!(matches!(
        super::apply_config_migration(&path, &source, &migrated),
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
    let (migrated, _) = super::migrate_config_source(&source).unwrap();
    super::apply_config_migration(&link, &source, &migrated).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(target).unwrap(), migrated);
}

#[test]
fn product_plan_terminal_profile_and_keymap_are_accepted() {
    let plan = include_str!("../../docs/product-plan.md");
    let profile = plan
        .split("Example profile in `config.toml`:")
        .nth(1)
        .unwrap()
        .split("```toml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let mapping = plan
        .split("Mapping in `keymaps.toml`:")
        .nth(1)
        .unwrap()
        .split("```toml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, profile).unwrap();
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    let path = directory.path().join("keymaps.toml");
    fs::write(&path, mapping).unwrap();
    let mappings = load_keymaps(&path);
    assert!(mappings.diagnostic.is_none(), "{:?}", mappings.diagnostic);
    let mut config = load_bundled_config();
    config.config = loaded.config;
    config.keymaps = mappings;
    crate::editor::validate_loaded_configuration(config).unwrap();
}
