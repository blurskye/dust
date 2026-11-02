use std::{cell::Cell, collections::BTreeSet, fs, path::PathBuf};

use tempfile::tempdir;

use super::load::{
    BUNDLED_CONFIG, BUNDLED_THEME, CandidateAttempt, CapturedSource, CapturedText,
    ConfigurationSourceBundle, SourceFingerprint, WATCH_FINGERPRINT_BYTE_CEILING,
    capture_configuration_bundle_with, captured_bundle_is_stable, hash_source_bytes,
    load_consistent_candidate_with, load_theme, setting_keys, setting_spans, source_fingerprint,
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
            assert!(loaded.theme_path.is_absolute());
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
    assert_eq!(loaded.config.ui.command_line.width, 60);
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
            .explain("ui.command_line.width")
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

    fs::write(&path, "[editor]\nhorizontal_margin = 9\n").expect("write removed flat field");
    let loaded = load_config(&path);
    assert_eq!(loaded.config, load_bundled_config().config);
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|message| { message.contains("unknown field `horizontal_margin`") })
    );
}

#[test]
fn git_sign_policy_is_nested_strict_and_display_safe() {
    let root = tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(
            &path,
            "[editor.git_signs]\nenabled=false\nunchanged=\"│\"\nadded=\"A\"\nchanged=\"C\"\ndeleted=\"D\"\n",
        )
        .unwrap();
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert!(!loaded.config.editor.git_signs.enabled);
    for invalid in [
        "unchanged=\"\"",
        "added=\"\"",
        "changed=\"界\"",
        "deleted=\"DD\"",
    ] {
        fs::write(&path, format!("[editor.git_signs]\n{invalid}\n")).unwrap();
        assert!(load_config(&path).diagnostic.is_some());
    }
    fs::write(&path, "[editor.git_signs]\nunknown=\"x\"\n").unwrap();
    assert!(load_config(&path).diagnostic.is_some());
}

#[test]
fn indent_guide_policy_keeps_scope_highlighting_independently_configurable() {
    let root = tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "[editor.indent_guides]\nenabled=true\nactive_scope=false\ncharacter=\"│\"\n",
    )
    .unwrap();
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert!(loaded.config.editor.indent_guides.enabled);
    assert!(!loaded.config.editor.indent_guides.active_scope);
    assert_eq!(loaded.config.editor.indent_guides.character, "│");

    fs::write(&path, "[editor.indent_guides]\nactive_scope=\"yes\"\n").unwrap();
    assert!(load_config(&path).diagnostic.is_some());
    fs::write(&path, "[editor.indent_guides]\nunknown=true\n").unwrap();
    assert!(load_config(&path).diagnostic.is_some());
}

#[test]
fn explorer_watch_policy_is_nested_strict_and_bounded() {
    let root = tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "[ui.explorer.watch]\nenabled=false\npoll_interval_ms=1000\n",
    )
    .unwrap();
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert!(!loaded.config.ui.explorer.watch.enabled);
    for invalid in [
        "poll_interval_ms=49",
        "maximum_directories=0",
        "maximum_entries_per_directory=100001",
        "maximum_events=0",
        "maximum_path_bytes=12",
        "queue_capacity=0",
    ] {
        fs::write(&path, format!("[ui.explorer.watch]\n{invalid}\n")).unwrap();
        assert!(load_config(&path).diagnostic.is_some());
    }
}

#[test]
fn unpublished_explorer_schema_rejects_removed_overlay_fields() {
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
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains(field)),
            "missing rejection for {field}"
        );
    }
}

#[test]
fn explorer_glyph_policy_rejects_wide_panel_cells_and_oversized_segments() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[editor]\nambiguous_width = \"wide\"\n").expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none());
    assert_eq!(loaded.config.editor.ambiguous_width, AmbiguousWidth::Wide);

    fs::write(
        &path,
        "[ui.explorer.glyphs.ascii]\npanel_vertical = \"||\"\n",
    )
    .expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_some());
    assert_eq!(loaded.config, load_bundled_config().config);

    fs::write(
            &path,
            "[editor]\nambiguous_width = \"wide\"\n\n[ui.explorer.glyphs.unicode]\npanel_horizontal = \"·\"\n",
        )
        .expect("write");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_some());
    assert_eq!(loaded.config, load_bundled_config().config);

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

    fs::write(&path, "[ui]\nmade_up = true\n").expect("unknown nested key");
    let loaded = load_config(&path);
    assert_eq!(loaded.config, load_bundled_config().config);
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("unknown field `made_up`"))
    );
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

#[test]
fn complete_bundled_keymap_layer_can_be_omitted_without_dropping_user_maps() {
    let config_path = PathBuf::from("config.toml");
    let keymaps_path = PathBuf::from("keymaps.toml");
    let bundle = ConfigurationSourceBundle {
            config: captured(
                config_path,
                CapturedText::Text(
                    "[input.keymaps]\nload_bundled_defaults=false\nleader=','\nlocal_leader=';'\n"
                        .into(),
                ),
            ),
            keymaps: captured(
                keymaps_path,
                CapturedText::Text(
                    "[[keymap]]\nid='user.test.opt-out'\ncontext='dashboard'\nkeys='<leader> z'\ncommand='health.open'\n"
                        .into(),
                ),
            ),
            dashboard: captured(PathBuf::from("dashboard.toml"), CapturedText::Missing),
            theme: captured(PathBuf::from("dust.toml"), CapturedText::Missing),
            selected_theme: "dust".into(),
        };
    let loaded = bundle.into_loaded();
    assert!(!loaded.config.input.keymaps.load_bundled_defaults);
    assert_eq!(loaded.keymaps.bundled_count, 0);
    assert_eq!(loaded.keymaps.bundled_group_count, 0);
    assert!(loaded.keymaps.groups.is_empty());
    assert_eq!(loaded.keymaps.entries.len(), 1);
    assert_eq!(
        loaded.keymaps.entries[0].keys.as_deref(),
        Some("<leader> z")
    );
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

fn candidate_attempt(theme: &str, valid: bool) -> CandidateAttempt {
    let mut loaded = load_bundled_config();
    loaded.config.ui.theme = theme.into();
    CandidateAttempt {
        candidate: if valid {
            Ok(loaded)
        } else {
            Err("synthetic malformed candidate".into())
        },
        theme: theme.into(),
    }
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
    let stable = load_consistent_candidate_with(
        true,
        "dust",
        || {
            loads.set(loads.get() + 1);
            if loads.get() == 1 {
                revision.set(1);
            }
            candidate_attempt("dust", true)
        },
        |_| revision.get(),
    )
    .expect("eventually stable");
    assert_eq!(loads.get(), 2);
    assert_eq!(stable.fingerprint, Some(1));
    assert!(stable.candidate.is_ok());

    let theme_revision = Cell::new(0_u8);
    let loads = Cell::new(0_usize);
    let switched = load_consistent_candidate_with(
        true,
        "dust",
        || {
            loads.set(loads.get() + 1);
            if loads.get() == 2 {
                theme_revision.set(1);
            }
            candidate_attempt("night", true)
        },
        |theme| (theme.to_owned(), theme_revision.get()),
    )
    .expect("new theme stabilizes");
    assert_eq!(loads.get(), 3);
    assert_eq!(switched.theme, "night");
    assert_eq!(switched.fingerprint, Some(("night".into(), 1)));
}

#[test]
fn consistent_loader_never_emits_discarded_malformed_or_stale_attempts() {
    for final_valid in [true, false] {
        let revision = Cell::new(0_u8);
        let loads = Cell::new(0_usize);
        let loaded = load_consistent_candidate_with(
            true,
            "dust",
            || {
                loads.set(loads.get() + 1);
                if loads.get() == 1 {
                    revision.set(1);
                    candidate_attempt("dust", !final_valid)
                } else {
                    candidate_attempt("dust", final_valid)
                }
            },
            |_| vec![revision.get(); 4],
        )
        .expect("second complete source set is stable");
        assert_eq!(loads.get(), 2);
        assert_eq!(loaded.candidate.is_ok(), final_valid);
        assert_eq!(loaded.fingerprint, Some(vec![1; 4]));
    }
}

#[test]
fn accepted_candidate_fingerprint_matches_all_four_loaded_surfaces() {
    let revision = Cell::new(0_u8);
    let loads = Cell::new(0_usize);
    let loaded = load_consistent_candidate_with(
        true,
        "dust",
        || {
            loads.set(loads.get() + 1);
            let snapshot = revision.get();
            let mut attempt = candidate_attempt("dust", true);
            let candidate = attempt.candidate.as_mut().expect("candidate");
            candidate.config.editor.tab_width = usize::from(snapshot) + 2;
            candidate.keymaps.path = format!("keymaps-{snapshot}").into();
            candidate.dashboard.identity = format!("dashboard-{snapshot}");
            candidate.theme_path = format!("theme-{snapshot}").into();
            if loads.get() == 1 {
                revision.set(1);
            }
            attempt
        },
        |_| vec![revision.get(); 4],
    )
    .expect("stable complete snapshot");
    let candidate = loaded.candidate.expect("accepted candidate");
    assert_eq!(loaded.fingerprint, Some(vec![1; 4]));
    assert_eq!(candidate.config.editor.tab_width, 3);
    assert_eq!(candidate.keymaps.path, PathBuf::from("keymaps-1"));
    assert_eq!(candidate.dashboard.identity, "dashboard-1");
    assert_eq!(candidate.theme_path, PathBuf::from("theme-1"));
}

#[test]
fn clean_consistent_load_never_fingerprints_user_sources() {
    let loads = Cell::new(0_usize);
    let loaded = load_consistent_candidate_with(
        false,
        "dust",
        || {
            loads.set(loads.get() + 1);
            candidate_attempt("dust", true)
        },
        |_| -> Vec<u8> { panic!("--clean must not fingerprint user sources") },
    )
    .expect("bundled candidate");
    assert_eq!(loads.get(), 1);
    assert!(loaded.fingerprint.is_none());
    assert!(loaded.candidate.is_ok());
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
fn recovery_retention_and_watcher_limits_reject_unsafe_candidates() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "[recovery]\nmaximum_retained_records=10001\nmaximum_age_days=36501\n",
    )
    .expect("unsafe recovery policy");
    let loaded = load_config(&path);
    assert_eq!(loaded.config, load_bundled_config().config);
    assert!(loaded.diagnostic.as_deref().is_some_and(|diagnostic| {
        diagnostic.contains("maximum_retained_records exceeds safe ceiling")
    }));

    fs::write(
        &path,
        "[config_watch]\npoll_interval_ms=24\ndebounce_ms=60001\n",
    )
    .expect("unsafe watcher policy");
    let loaded = load_config(&path);
    assert_eq!(loaded.config, load_bundled_config().config);
    assert!(
        loaded
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| { diagnostic.contains("poll_interval_ms must be between") })
    );
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
    assert!(loaded.config.input.keymaps.load_bundled_defaults);
    assert_eq!(loaded.config.input.keymaps.leader, "space");
    assert_eq!(loaded.config.input.keymaps.local_leader, "\\");
    assert!(loaded.config.editor.indent_guides.enabled);
    assert!(loaded.config.editor.indent_guides.active_scope);
    assert_eq!(loaded.config.editor.indent_guides.character, "│");
    assert_eq!(loaded.dashboard.action.len(), 9);
    assert!(
        loaded
            .dashboard
            .action
            .iter()
            .any(|action| action.command == "explorer.open")
    );
    let explained = loaded.explain("ui.command_line.width").expect("explain");
    assert_eq!(explained.value, "60");
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
fn buffer_line_policy_is_typed_bounded_and_strict() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "[ui.buffer_line]\nmaximum_name_width=48\ndirty_marker=' *'\n",
    )
    .expect("valid buffer line");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert_eq!(loaded.config.ui.buffer_line.maximum_name_width, 48);
    assert_eq!(loaded.config.ui.buffer_line.dirty_marker, " *");

    for invalid in [
        "maximum_name_width=3",
        "active_format='{title}'",
        "inactive_format='{dirty}'",
        "separator=\"bad\\nvalue\"",
        "pin_marker='界'",
        "extension_icons={RS='R'}",
    ] {
        fs::write(&path, format!("[ui.buffer_line]\n{invalid}\n")).expect("invalid buffer line");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(loaded.diagnostic.is_some());
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
fn terminal_panel_policy_is_typed_bounded_and_strict() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(
            &path,
            "[ui.terminal_panel]\nheight=9\nminimum_height=4\nmaximum_height=20\nminimum_workspace_height=2\nresize_step=3\ndivider='-'\ntab_separator=' | '\nnormal_mode_label='NORMAL'\nvisual_mode_label='VISUAL'\n",
        )
        .expect("valid terminal panel");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert_eq!(loaded.config.ui.terminal_panel.height, 9);
    assert_eq!(loaded.config.ui.terminal_panel.resize_step, 3);
    assert_eq!(loaded.config.ui.terminal_panel.tab_separator, " | ");

    for (index, invalid) in [
        "height=2",
        "height=9\nminimum_height=10",
        "maximum_height=201",
        "divider='界'",
        "normal_mode_label=''",
        "resize_step=0",
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(&path, format!("[ui.terminal_panel]\n{invalid}\n"))
            .expect("invalid terminal panel");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config, "case {index}");
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|message| { message.contains("ui.terminal_panel") })
        );
    }
}

#[test]
fn documentation_popup_policy_is_typed_and_bounded() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    for invalid in [
        "scroll_step=0",
        "maximum_sections=0",
        "maximum_lines=0",
        "maximum_bytes=255",
        "maximum_bytes=1048577",
    ] {
        fs::write(&path, format!("[ui.documentation_popup]\n{invalid}\n"))
            .expect("invalid documentation popup");
        let loaded = load_config(&path);
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("ui.documentation_popup"))
        );
        assert_eq!(
            loaded.config.ui.documentation_popup,
            load_bundled_config().config.ui.documentation_popup
        );
    }
}

#[test]
fn key_hint_policy_is_typed_bounded_and_transactional() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("config.toml");
    fs::write(
            &path,
            "[ui.key_hints]\nenabled=false\ndelay_ms=42\nanchor='bottom_left'\nmax_rows=4\nmax_columns=2\nmax_width=50\nhorizontal_padding=2\nvertical_padding=1\nrow_gap=1\ncolumn_gap=3\nshow_group_icons=false\nshow_command_titles=false\nfallback_label='Run'\nborder='single'\n",
        )
        .expect("valid key hints");
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    assert!(!loaded.config.ui.key_hints.enabled);
    assert_eq!(loaded.config.ui.key_hints.delay_ms, 42);
    assert_eq!(
        loaded.config.ui.key_hints.anchor,
        super::KeyHintAnchor::BottomLeft
    );
    assert_eq!(loaded.config.ui.key_hints.fallback_label, "Run");

    for invalid in [
        "delay_ms=60001",
        "max_rows=0",
        "max_columns=13",
        "max_width=11",
        "horizontal_padding=9",
        "vertical_padding=5",
        "row_gap=5",
        "column_gap=17",
        "fallback_label=''",
    ] {
        fs::write(&path, format!("[ui.key_hints]\n{invalid}\n")).expect("invalid key hints");
        let loaded = load_config(&path);
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("ui.key_hints")),
            "{invalid}: {:?}",
            loaded.diagnostic
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
    let ids = loaded
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"user.global.files.editor-normal"));
    assert!(ids.contains(&"user.global.files.dashboard"));
    assert!(ids.contains(&"user.global.files.terminal-normal"));
    assert!(
        !ids.contains(&"user.global.files.explorer"),
        "the explorer's own binding for the same keys wins"
    );
    assert!(ids.contains(&"user.explorer.files"));
    let editor = loaded
        .entries
        .iter()
        .find(|entry| entry.id == "user.global.files.editor-normal")
        .expect("editor copy");
    assert_eq!(editor.context.as_deref(), Some("editor"));
    assert_eq!(editor.mode, "normal");
    assert!(ids.contains(&"builtin.global.quit.editor-normal"));
    assert!(ids.contains(&"builtin.global.quit.explorer"));
    assert_eq!(loaded.entries.len(), loaded.user_spans.len());
    assert!(
        loaded
            .groups
            .iter()
            .any(|group| group.id == "user.global.group.files.explorer"
                && group.context.as_deref() == Some("explorer"))
    );
    assert_eq!(loaded.groups.len(), loaded.user_group_spans.len());
}

#[test]
fn highlight_groups_resolve_like_neovim() {
    use crate::language::syntax::Highlight;
    let group = |name| Highlight::named(name).expect(name);
    let colour = |theme: &super::ThemeConfig, name| theme.highlight(group(name)).map(|style| style.foreground);

    // Every bundled link lands on a group with a definition, so ports keep their colours.
    for (name, _) in super::themes::BUNDLED {
        let theme = super::themes::bundled_named(name).unwrap().theme;
        for (group, definition) in &theme.highlights {
            if let Some(link) = &definition.link {
                assert!(
                    link.starts_with('@')
                        || theme.highlights.contains_key(link)
                        || Highlight::named(link).is_some_and(|linked| theme.highlight(linked).is_some()),
                    "{name}: {group} links to undefined {link}"
                );
            }
        }
    }
    let tokyo = super::themes::bundled_named("tokyo-night").unwrap().theme;
    // `@function.method.call` links to `@function.method`, which links to `Function`.
    assert_eq!(colour(&tokyo, "@function.method.call"), Some(super::ThemeColor::Rgb(0x7a, 0xa2, 0xf7)));
    let keyword = tokyo.highlight(Highlight::KEYWORD).unwrap();
    assert!(keyword.attributes.contains(&super::ThemeAttribute::Italic));
    assert_eq!(keyword.foreground_256, Some(super::ThemeColor::Indexed(140)));

    let mut dust = super::themes::bundled_named("dust").unwrap().theme;
    dust.highlights.insert("@function".into(), super::HighlightDefinition { link: Some("@function.method".into()), ..Default::default() });
    dust.highlights.insert("@function.method".into(), super::HighlightDefinition { link: Some("@function".into()), ..Default::default() });
    // Nothing defines `@markup.heading.1`, so it takes `@markup.heading`, which Neovim links to `Title`.
    assert_eq!(colour(&dust, "@markup.heading.1"), Some(super::ThemeColor::Rgb(0x58, 0xa6, 0xff)));
    // A theme that leaves a token type out keeps tree-sitter's colour.
    assert_eq!(dust.highlight(group("@lsp.type.derive")), None);
    // Links that loop colour nothing.
    assert_eq!(dust.highlight(Highlight::FUNCTION), None);
    // Groups without a background draw on the editor's.
    assert_eq!(dust.highlight(Highlight::STRING).unwrap().background, dust.styles.default.background);
}

#[test]
fn a_saved_bundled_theme_restores_from_this_build() {
    let (reply, receiver) = std::sync::mpsc::sync_channel(1);
    // Saved by an older build, with a style field that no longer exists.
    let saved = r##"{"name":"tokyo-night","styles":{"syntax_comment":{"foreground":"#a9b1d6"}}}"##;
    super::themes::discover(super::themes::ThemeRead {
        source: super::themes::ThemeReadSource::Saved(saved.into()),
        cancelled: Default::default(),
        reply,
    });
    let catalog = receiver.recv().unwrap();
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
    assert_eq!(catalog.choices[0].theme, super::themes::bundled_named("tokyo-night").unwrap().theme);
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
    assert!(missing.is_empty(), "settings missing from settings.toml: {missing:?}");
    let stale: Vec<&&str> = listed.iter().filter(|key| !bundled.contains(**key)).collect();
    assert!(stale.is_empty(), "settings.toml lists unknown settings: {stale:?}");
    let unique: BTreeSet<&&str> = listed.iter().collect();
    assert_eq!(unique.len(), listed.len(), "settings.toml lists a setting twice");
    for section in &catalog.sections {
        for key in &section.common {
            assert!(catalog.setting(key).is_some(), "{} lists unknown {key}", section.title);
        }
    }
    for setting in catalog.sections.iter().flat_map(|section| &section.settings) {
        assert!(!setting.about.is_empty(), "{} needs a description", setting.key);
        if let Some(table) = &setting.choices_from {
            assert!(super::default_setting(table).is_some_and(|value| value.is_table()), "{table}");
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
fn a_setting_is_changed_in_place_and_reset_removes_it_with_empty_tables() {
    let edited = |source: &str, key: &str, value: Option<toml::Value>| {
        super::catalog::edited_source(source, key, value).unwrap()
    };
    let source = "# my settings\n[editor]\n# wrap long lines\nwrap = false # for now\n";
    let wrapped = edited(source, "editor.wrap", Some(toml::Value::Boolean(true)));
    assert_eq!(wrapped, "# my settings\n[editor]\n# wrap long lines\nwrap = true # for now\n");
    let added = edited(&wrapped, "editor.git_signs.enabled", Some(toml::Value::Boolean(false)));
    assert!(added.starts_with(&wrapped), "{added}");
    assert!(added.contains("enabled = false"), "{added}");
    let reset = edited(&added, "editor.git_signs.enabled", None);
    assert_eq!(reset, wrapped);
    let emptied = edited(&reset, "editor.wrap", None);
    assert!(emptied.contains("# my settings") && !emptied.contains("wrap"), "{emptied}");
    let file = tempdir().unwrap();
    let path = file.path().join("dust").join("config.toml");
    super::write_user_setting(&path, "editor.tab_width", Some(toml::Value::Integer(2))).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "[editor]\ntab_width = 2\n");
    let rejected =
        super::write_user_setting(&path, "editor.tab_width", Some(toml::Value::String("x".into())));
    assert!(rejected.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "[editor]\ntab_width = 2\n");
}
