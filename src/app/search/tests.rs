//! Search behavior and application integration tests.

use crate::app::picker::PickerContent;
use crate::app::picker::PickerOrigin;
use crate::app::picker::PickerPayload;
use crate::app::search::*;
use crate::app::tests::*;
use crate::app::workspace::tests::wait_workspace_search;
use crate::editor::text::ByteOffset;
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

#[test]
fn search_moves_forward_wraps_and_repeats() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one two one"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.ui.search_query = Some("one".into());
    app.perform_search(true, 1);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(8)
    );
    app.perform_search(true, 1);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(0)
    );
    app.perform_search(false, 1);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(8)
    );
}

#[test]
fn help_filters_do_not_restart_the_suspended_live_grep() {
    let (_app_dirs, mut app) = app();
    fs::write(app.state.workspace_root.join("match.txt"), "needle\n").unwrap();
    app.invoke_mapped_command("workspace.search.prompt");
    app.picker_insert("needle");
    wait_workspace_search(&mut app);
    assert!(app.search.live());
    let sequence = app.search.sequence();
    app.open_contextual_help();
    assert!(!app.search.live());
    app.picker_insert("help");
    app.picker_backspace();
    assert!(app.search.help_is_open());
    assert_eq!(app.search.query(), "needle");
    assert_eq!(app.search.sequence(), sequence);
    assert_eq!(app.search.matches().len(), 1);
    app.close_picker();
    assert!(app.search.live());
    assert_eq!(app.ui.picker.as_ref().unwrap().query, "needle");
    app.accept_picker();
    wait_vfs(&mut app);
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor,
        ByteOffset(0)
    );
}

#[test]
fn grep_rows_draw_the_icon_location_and_highlighted_text_as_lazyvim_does() {
    let (_app_dirs, mut app) = app();
    fs::write(
        app.state.workspace_root.join("main.rs"),
        "\n    let needle = 1;\n",
    )
    .unwrap();
    app.invoke_mapped_command("workspace.search.prompt");
    app.picker_insert("needle");
    wait_workspace_search(&mut app);
    let picker = app.search.picker_projection().unwrap();
    assert_eq!(picker.surface, crate::ui::PickerSurfaceKind::Grep);
    let cells = &picker.rows[0].cells;
    let icon = crate::files::file_icon("main.rs").unwrap().glyph;
    assert_eq!(cells[0].text, format!("{icon} "));
    let texts: Vec<_> = cells[1..6].iter().map(|cell| cell.text.as_str()).collect();
    assert_eq!(texts, ["main.rs", ":", "2", ":", "9"]);
    assert_eq!(cells[3].style, crate::ui::Style::Accent);
    assert_eq!(cells[5].style, crate::ui::Style::Muted);
    let keyword = cells.iter().find(|cell| cell.text == "let").unwrap();
    assert!(matches!(keyword.style, crate::ui::Style::Syntax(_)));
}

fn scope(root: &std::path::Path) -> WorkspaceVersion {
    let identity = crate::files::RootIdentity::capture(root).unwrap();
    let state = crate::workspace::WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [("root".into(), identity)],
    )
    .unwrap();
    crate::app::workspace::Workspace::new(state, root.join("trust.json"), 1, false, false, 1)
        .version()
}
fn found() -> WorkspaceSearchMatch {
    WorkspaceSearchMatch {
        id: "one".into(),
        root: "/fixture".into(),
        canonical_path: "/fixture/file".into(),
        relative_path: "file".into(),
        line: 0,
        column: 0,
        end_line: 0,
        end_column: 6,
        range: 0..6,
        line_text: "needle".into(),
        snapshot: Arc::from(&b"needle\n"[..]),
    }
}
#[test]
fn preview_retries_full_queue_but_caches_permanent_failures() {
    use crate::files::RequestId;
    use crate::files::VfsError;
    let directory = tempfile::tempdir().unwrap();
    let source = scope(directory.path());
    let mut owner = Search::default();
    let prepare = |owner: &mut Search| {
        owner.prepare_preview(
            "file".into(),
            Some((0, 0)),
            "File".into(),
            "Loading".into(),
            source.clone(),
            1,
        )
    };
    let ticket = prepare(&mut owner).1.unwrap();
    owner.preview_failed(ticket, VfsError::QueueFull);
    let ticket = prepare(&mut owner)
        .1
        .expect("retry after temporary refusal");
    assert!(owner.track_preview_read(ticket, RequestId(1), 64));
    assert!(
        prepare(&mut owner).1.is_none(),
        "do not duplicate an accepted read"
    );
    assert!(owner.accept_preview(RequestId(1), &Ok(b"ready".to_vec()), source.clone(), 1));
    assert!(prepare(&mut owner).1.is_none());
    assert_eq!(
        owner.preview().as_ref().unwrap().as_text().unwrap().lines(),
        ["ready"]
    );

    owner.clear_preview();
    let ticket = prepare(&mut owner).1.unwrap();
    owner.preview_failed(ticket, VfsError::WorkerStopped);
    assert!(
        prepare(&mut owner).1.is_none(),
        "do not retry permanent failures every tick"
    );
}

#[test]
fn file_previews_follow_selected_lines_and_normalize_newlines() {
    use crate::files::RequestId;
    let directory = tempfile::tempdir().unwrap();
    let source = scope(directory.path());
    let mut owner = Search::default();
    let bytes = (0..1100)
        .map(|line| format!("line {line}\n"))
        .collect::<String>()
        .into_bytes();
    for (request, line) in [(1, 999), (2, 1050)] {
        let ticket = owner
            .prepare_preview(
                "file".into(),
                Some((line, 0)),
                "File".into(),
                "Loading".into(),
                source.clone(),
                1,
            )
            .1
            .expect("a different location in the same file needs its own preview");
        assert!(owner.track_preview_read(ticket, RequestId(request), bytes.len()));
        assert!(owner.accept_preview(RequestId(request), &Ok(bytes.clone()), source.clone(), 1));
        let text = owner.preview().as_ref().unwrap().as_text().unwrap();
        assert_eq!(text.highlight_line, Some(line));
        assert_eq!(text.first_line, line - 8);
        assert_eq!(text.lines()[line - text.first_line], format!("line {line}"));
        assert!(text.lines().len() <= 512);
        assert!(
            owner
                .prepare_preview(
                    "file".into(),
                    Some((line, 0)),
                    "File".into(),
                    "Loading".into(),
                    source.clone(),
                    1
                )
                .1
                .is_none()
        );
    }
    for bytes in [b"a\rb\rc".to_vec(), b"\xef\xbb\xbfa\r\nb\rc".to_vec()] {
        owner.clear_preview();
        let ticket = owner
            .prepare_preview(
                "file".into(),
                Some((0, 0)),
                "File".into(),
                "Loading".into(),
                source.clone(),
                1,
            )
            .1
            .unwrap();
        assert!(owner.track_preview_read(ticket, RequestId(3), 64));
        assert!(owner.accept_preview(RequestId(3), &Ok(bytes), source.clone(), 1));
        assert_eq!(
            owner.preview().as_ref().unwrap().as_text().unwrap().lines(),
            ["a", "b", "c"]
        );
    }
}

#[test]
fn location_preview_loading_and_errors_remain_visible() {
    use crate::files::RequestId;
    use crate::files::VfsError;
    let directory = tempfile::tempdir().unwrap();
    let source = scope(directory.path());
    let mut owner = Search::default();
    let ticket = owner
        .prepare_preview(
            "file".into(),
            Some((999, 0)),
            "File".into(),
            "Loading".into(),
            source.clone(),
            1,
        )
        .1
        .unwrap();
    let text = owner.preview().as_ref().unwrap().as_text().unwrap();
    assert_eq!(text.first_line, 999);
    assert_eq!(text.highlight_line, Some(999));
    owner.preview_failed(ticket, VfsError::QueueFull);
    assert_eq!(
        owner
            .preview()
            .as_ref()
            .unwrap()
            .as_text()
            .unwrap()
            .first_line,
        999
    );
    for result in [Err(VfsError::WorkerStopped), Ok(vec![255]), Ok(vec![0; 65])] {
        let ticket = owner
            .prepare_preview(
                "file".into(),
                Some((999, 0)),
                "File".into(),
                "Loading".into(),
                source.clone(),
                1,
            )
            .1
            .unwrap();
        assert!(owner.track_preview_read(ticket, RequestId(1), 64));
        assert!(owner.accept_preview(RequestId(1), &result, source.clone(), 1));
        let text = owner.preview().as_ref().unwrap().as_text().unwrap();
        assert_eq!(text.lines().len(), 1);
        assert_eq!(text.highlight_line, Some(text.first_line));
        owner.clear_preview();
    }
}

#[test]
fn search_previews_keep_matches_beyond_the_line_character_limit() {
    for prefix in ["a".repeat(4095), "a".repeat(10_000), "é界".repeat(5_000)] {
        let mut item = found();
        item.range = prefix.len()..prefix.len() + 6;
        item.snapshot = Arc::from(format!("{prefix}needle{}\n", "z".repeat(10_000)).into_bytes());
        let mut owner = Search {
            matches: vec![item],
            ..Default::default()
        };
        owner.preview_match("one", 256 * 1024);
        let text = owner.preview().as_ref().unwrap().as_text().unwrap();
        assert_eq!(text.highlight_line, Some(0));
        assert_eq!(text.first_line, 0);
        assert!(text.lines()[0].contains("needle"));
        assert!(
            text.lines()[0]
                .chars()
                .take(24)
                .collect::<String>()
                .contains("needle")
        );
        assert!(text.lines()[0].chars().count() <= 4096);
    }
}

#[test]
fn preview_rejects_foreign_stale_oversized_and_duplicate_replies() {
    let directory = tempfile::tempdir().unwrap();
    let source = scope(directory.path());
    let mut owner = Search::default();
    let ticket = owner
        .prepare_preview(
            "file".into(),
            None,
            "title".into(),
            "loading".into(),
            source.clone(),
            1,
        )
        .1
        .unwrap();
    let mut other = Search::default();
    assert!(!other.track_preview_read(ticket, crate::files::RequestId(1), 8));
    let ticket = owner
        .prepare_preview(
            "second".into(),
            None,
            "second".into(),
            "loading".into(),
            source.clone(),
            1,
        )
        .1
        .unwrap();
    assert!(owner.track_preview_read(ticket, crate::files::RequestId(2), 8));
    assert_eq!(owner.clear_preview(), Some(crate::files::RequestId(2)));
    assert!(!owner.accept_preview(
        crate::files::RequestId(2),
        &Ok(vec![255; 64]),
        source.clone(),
        1
    ));
    assert!(owner.preview().is_none());
    let ticket = owner
        .prepare_preview(
            "third".into(),
            None,
            "third".into(),
            "loading".into(),
            source.clone(),
            1,
        )
        .1
        .unwrap();
    assert!(owner.track_preview_read(ticket, crate::files::RequestId(3), 8));
    assert!(owner.accept_preview(
        crate::files::RequestId(3),
        &Ok(vec![255; 9]),
        source.clone(),
        1
    ));
    assert_eq!(
        owner
            .preview()
            .as_ref()
            .unwrap()
            .as_text()
            .expect("text preview")
            .lines(),
        ["preview exceeds requested byte limit"]
    );
    assert!(!owner.accept_preview(
        crate::files::RequestId(3),
        &Ok(b"late".to_vec()),
        source.clone(),
        1
    ));
    let ticket = owner
        .prepare_preview(
            "fourth".into(),
            None,
            "fourth".into(),
            "loading".into(),
            source.clone(),
            1,
        )
        .1
        .unwrap();
    assert!(owner.track_preview_read(ticket, crate::files::RequestId(4), 8));
    assert!(owner.accept_preview(
        crate::files::RequestId(4),
        &Ok(b"valid".to_vec()),
        source.clone(),
        1
    ));
    assert_eq!(
        owner
            .preview()
            .as_ref()
            .unwrap()
            .as_text()
            .expect("text preview")
            .lines(),
        ["valid"]
    );
    let ticket = owner
        .prepare_preview(
            "fifth".into(),
            None,
            "fifth".into(),
            "loading".into(),
            source.clone(),
            1,
        )
        .1
        .unwrap();
    assert!(owner.track_preview_read(ticket, crate::files::RequestId(5), 8));
    assert!(owner.accept_preview(crate::files::RequestId(5), &Ok(vec![255; 8]), source, 2));
    assert!(owner.preview().is_none());
}
#[test]
fn matcher_previews_follow_the_source_location_within_one_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = scope(directory.path());
    let mut owner = Search::default();
    let ticket = owner
        .prepare_preview(
            "file.rs".into(),
            Some((900, 2)),
            "File".into(),
            "Loading".into(),
            source.clone(),
            1,
        )
        .1
        .unwrap();
    let bytes = (0..1000)
        .map(|line| format!("line {line}\n"))
        .collect::<String>()
        .into_bytes();
    assert!(owner.track_preview_read(ticket, crate::files::RequestId(1), bytes.len()));
    assert!(owner.accept_preview(crate::files::RequestId(1), &Ok(bytes), source.clone(), 1));
    let preview = owner.preview().as_ref().unwrap().as_text().unwrap();
    assert_eq!(preview.first_line, 892);
    assert_eq!(preview.highlight_line, Some(900));
    assert_eq!(preview.lines()[8], "line 900");
    assert!(
        owner
            .prepare_preview(
                "file.rs".into(),
                Some((900, 2)),
                "File".into(),
                "Loading".into(),
                source.clone(),
                1
            )
            .1
            .is_none()
    );
    assert!(
        owner
            .prepare_preview(
                "file.rs".into(),
                Some((950, 0)),
                "File".into(),
                "Loading".into(),
                source.clone(),
                1
            )
            .1
            .is_some()
    );
    assert!(
        owner
            .prepare_preview(
                "file.rs".into(),
                Some((950, 3)),
                "File".into(),
                "Loading".into(),
                source,
                1
            )
            .1
            .is_some()
    );
}

#[test]
fn completed_stream_and_idle_snapshot_have_one_accepted_lifetime() {
    let directory = tempfile::tempdir().unwrap();
    let source = scope(directory.path());
    let mut owner = Search {
        stream_id: Some(1),
        scope: Some((source.clone(), 1)),
        intent: Some(Intent::Find),
        ..Search::default()
    };
    let mut broker = SearchWorkflowBroker::try_new(SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Grep,
        matcher: SearchMatcherKind::Literal,
        presenter: SearchPresenterKind::Picker,
        maximum_items: 2,
        maximum_bytes: 4096,
    })
    .unwrap();
    broker
        .begin(SearchRequestId(1), SearchStreamId(1), "")
        .unwrap();
    owner.workflow = Some(broker);
    assert!(
        owner
            .accept(WorkspaceSearchEvent::Batch {
                stream_id: 2,
                matches: vec![found()]
            })
            .is_none()
    );
    assert!(owner.matches().is_empty());
    assert!(matches!(
        owner.accept(WorkspaceSearchEvent::Batch {
            stream_id: 1,
            matches: vec![found()]
        }),
        Some(SearchUpdate::Batch)
    ));
    assert!(matches!(
        owner.accept(WorkspaceSearchEvent::Complete {
            stream_id: 1,
            summary: crate::files::WorkspaceSearchSummary::default(),
        }),
        Some(SearchUpdate::Complete { .. })
    ));
    assert!(!owner.pending());
    owner.preview_match("one", 64);
    let epoch = owner.preview_epoch.clone();
    let lines = owner
        .preview()
        .as_ref()
        .unwrap()
        .as_text()
        .expect("text preview")
        .lines()
        .as_ptr();
    for _ in 0..256 {
        owner.preview_match("one", 64);
        assert!(Arc::ptr_eq(&epoch, &owner.preview_epoch));
        assert_eq!(
            lines,
            owner
                .preview()
                .as_ref()
                .unwrap()
                .as_text()
                .expect("text preview")
                .lines()
                .as_ptr()
        );
    }
    for event in [
        WorkspaceSearchEvent::Batch {
            stream_id: 1,
            matches: vec![found()],
        },
        WorkspaceSearchEvent::Failed {
            stream_id: 1,
            error: "late failure".into(),
        },
        WorkspaceSearchEvent::Cancelled { stream_id: 1 },
        WorkspaceSearchEvent::Complete {
            stream_id: 1,
            summary: crate::files::WorkspaceSearchSummary::default(),
        },
    ] {
        assert!(owner.accept(event).is_none());
        assert!(owner.poll(source.clone(), 1).is_empty());
        assert_eq!(owner.matches().len(), 1);
        assert!(owner.workflow().is_some());
        assert!(Arc::ptr_eq(&epoch, &owner.preview_epoch));
    }
}
#[test]
#[ignore = "explicit search/preview ownership cost probe"]
fn search_owner_cost_probe() {
    let count = std::env::var("DUST_COST_ITERATIONS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1000)
        .clamp(1, 10000);
    let mut item = found();
    item.snapshot = Arc::from("0123456789abcdef\n".repeat(8000).into_bytes());
    item.line = 4096;
    item.range = 69632..69638;
    let mode = std::env::var("DUST_COST_CASE").unwrap_or("unchanged_snapshot".into());
    let max = 65536;
    let mut owner = Search {
        matches: vec![item],
        ..Default::default()
    };
    owner.preview_match("one", max);
    let before = (
        crate::windows::WindowGraph::test_clone_count(),
        crate::editor::EditorState::test_clone_count(),
        crate::editor::text::TextBuffer::test_materializations(),
    );
    eprintln!("DUST_ALLOC_START");
    let start = Instant::now();
    for n in 0..count {
        if mode == "changed_snapshot" {
            owner.matches[0].range = 69632 + (n % 2)..69638 + (n % 2);
        }
        std::hint::black_box(owner.preview_match("one", max));
    }
    let elapsed = start.elapsed();
    eprintln!("DUST_ALLOC_END");
    let after = crate::editor::text::TextBuffer::test_materializations();
    println!(
        "case={mode} iterations={count} elapsed_ns={} graph_copies={} editor_copies={} text_decodes={} text_displays={}",
        elapsed.as_nanos(),
        crate::windows::WindowGraph::test_clone_count() - before.0,
        crate::editor::EditorState::test_clone_count() - before.1,
        after.0 - before.2.0,
        after.1 - before.2.1
    );
    assert!(elapsed < Duration::from_secs(2));
}

#[test]
fn bounded_drain_publishes_only_the_final_accepted_store_disposition() {
    let directory = tempfile::tempdir().unwrap();
    let source = scope(directory.path());
    for failed in [false, true] {
        let mut workflow = SearchWorkflowBroker::try_new(SearchWorkflowSpec {
            id: SearchWorkflowId(1),
            source: SearchSourceKind::Grep,
            matcher: SearchMatcherKind::Literal,
            presenter: SearchPresenterKind::Picker,
            maximum_items: 2,
            maximum_bytes: 4096,
        })
        .unwrap();
        workflow
            .begin(SearchRequestId(1), SearchStreamId(1), "")
            .unwrap();
        let mut events = vec![WorkspaceSearchEvent::Batch {
            stream_id: 1,
            matches: vec![found()],
        }];
        events.push(if failed {
            WorkspaceSearchEvent::Failed {
                stream_id: 1,
                error: "fixture failure".into(),
            }
        } else {
            WorkspaceSearchEvent::Complete {
                stream_id: 1,
                summary: crate::files::WorkspaceSearchSummary::default(),
            }
        });
        let mut owner = Search {
            grep: WorkspaceSearchWorker::events_for_test(events),
            stream_id: Some(1),
            scope: Some((source.clone(), 1)),
            workflow: Some(workflow),
            intent: Some(Intent::Find),
            ..Search::default()
        };
        let updates = owner.poll(source.clone(), 1);
        assert_eq!(updates.len(), 1);
        assert!(!owner.pending());
        if failed {
            assert!(matches!(&updates[0], SearchUpdate::Failed(_)));
            assert!(owner.matches().is_empty());
        } else {
            assert!(matches!(&updates[0], SearchUpdate::Complete { .. }));
            assert_eq!(owner.matches().len(), 1);
            let snapshot = owner.matches()[0].snapshot.clone();
            owner.close_picker();
            assert!(owner.matches().is_empty());
            assert!(owner.workflow().is_none());
            assert_eq!(Arc::strong_count(&snapshot), 1);
        }
        assert!(owner.poll(source.clone(), 1).is_empty());
    }
}

#[test]
fn picker_epochs_scope_and_close_preserve_only_current_interaction() {
    use crate::ui::PickerSurfaceKind;
    use crate::workspace::PickerAction;
    use crate::workspace::PickerItem;
    use crate::workspace::PickerItemId;
    let root = tempfile::tempdir().unwrap();
    let source = scope(root.path());
    let mut owner = Search::default();
    let content = || PickerContent {
        surface: PickerSurfaceKind::Generic,
        origin: PickerOrigin::General,
        title: "Picker".into(),
        items: (0..45)
            .map(|n| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("row-{n:02}")),
                        label: format!("jk {n:02}"),
                        detail: String::new(),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(format!("command-{n}")),
                )
            })
            .collect(),
        query: String::new(),
        complete: true,
    };
    let epoch = owner
        .install_picker(content(), 100, source.clone(), 1)
        .unwrap();
    assert!(owner.owns_picker_epoch(&epoch));
    assert!(!Search::default().owns_picker_epoch(&epoch));
    assert_eq!(owner.picker_insert("jk"), Some("jk".into()));
    for _ in 0..17 {
        owner.picker_move(1, false);
    }
    assert_eq!(owner.picker_scroll(5), Some(13));
    assert_eq!(owner.picker_scroll(40), Some(13));
    assert_eq!(
        owner.picker().unwrap().selected_item().unwrap().id.0,
        "row-17"
    );
    assert!(!owner.picker_is_stale(source.clone(), 1));
    assert!(owner.picker_is_stale(source.clone(), 2));
    assert!(owner.picker_is_stale(scope(root.path()), 1));
    let next = owner
        .install_picker(content(), 100, source.clone(), 1)
        .unwrap();
    assert!(!owner.owns_picker_epoch(&epoch));
    let (_, ticket) = owner.prepare_preview(
        "file".into(),
        None,
        "File".into(),
        "Loading".into(),
        source,
        1,
    );
    assert!(owner.track_preview_read(ticket.unwrap(), crate::files::RequestId(9), 64));
    assert_eq!(owner.close_picker(), Some(crate::files::RequestId(9)));
    assert!(!owner.owns_picker_epoch(&next));
    assert!(owner.picker_projection().is_none());
    assert!(owner.preview().is_none());
    assert_eq!(owner.close_picker(), None);
    owner.open_live();
    owner.stream_id = Some(9);
    owner.grep = WorkspaceSearchWorker::events_for_test(vec![WorkspaceSearchEvent::Batch {
        stream_id: 9,
        matches: vec![found()],
    }]);
    owner
        .install_picker(content(), 100, scope(root.path()), 1)
        .unwrap();
    assert!(!owner.live());
    assert!(!owner.pending());
    assert!(owner.poll(scope(root.path()), 1).is_empty());
    assert!(owner.matches().is_empty());
}

#[test]
fn streaming_picker_keeps_query_selection_and_epoch_but_not_foreign_actions() {
    use crate::ui::PickerSurfaceKind;
    use crate::workspace::PickerAction;
    use crate::workspace::PickerItem;
    use crate::workspace::PickerItemId;
    let root = tempfile::tempdir().unwrap();
    let source = scope(root.path());
    let mut owner = Search::default();
    let content = |origin, ids: &[&str], query: &str| PickerContent {
        surface: PickerSurfaceKind::Generic,
        origin,
        title: "stream".into(),
        items: ids
            .iter()
            .map(|id| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId((*id).into()),
                        label: (*id).into(),
                        detail: String::new(),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command((*id).into()),
                )
            })
            .collect(),
        query: query.into(),
        complete: false,
    };
    for origin in [
        PickerOrigin::Files,
        PickerOrigin::Grep,
        PickerOrigin::PluginProvider(1),
    ] {
        owner.close_picker();
        let epoch = owner
            .install_picker(content(origin, &[], ""), 3, source.clone(), 1)
            .unwrap();
        owner.picker_insert("jack");
        owner.set_picker_surface(PickerSurfaceKind::Problems);
        owner.set_picker_footer("Retained footer".into());
        owner
            .install_picker(
                content(origin, &["jack-a", "jack-b", "other"], "jack"),
                3,
                source.clone(),
                1,
            )
            .unwrap();
        assert!(owner.owns_picker_epoch(&epoch));
        owner.picker_move(1, false);
        assert_eq!(
            owner.picker().unwrap().selected_item().unwrap().id.0,
            "jack-b"
        );
        owner
            .install_picker(
                content(
                    origin,
                    &["jack-c", "jack-b", "jack-b", "jack-a", "unreachable"],
                    "jack",
                ),
                3,
                source.clone(),
                1,
            )
            .unwrap();
        assert!(owner.owns_picker_epoch(&epoch));
        assert_eq!(owner.picker().unwrap().query(), "jack");
        assert_eq!(owner.picker_surface(), PickerSurfaceKind::Problems);
        assert_eq!(
            owner.picker_projection().unwrap().footer.as_deref(),
            Some("Retained footer")
        );
        assert_eq!(
            owner.picker().unwrap().selected_item().unwrap().id.0,
            "jack-b"
        );
        assert!(
            matches!(owner.selected_picker_payload(), Some(PickerPayload::Command(id)) if id == "jack-b")
        );
        owner
            .install_picker(content(origin, &["fresh"], ""), 3, source.clone(), 2)
            .unwrap();
        assert!(!owner.owns_picker_epoch(&epoch));
        assert_eq!(owner.picker().unwrap().query(), "");
        assert_eq!(owner.picker_surface(), PickerSurfaceKind::Generic);
        assert!(
            matches!(owner.selected_picker_payload(), Some(PickerPayload::Command(id)) if id == "fresh")
        );
        owner.close_picker();
        assert!(owner.selected_picker_payload().is_none());
    }
}

#[test]
fn picker_rows_and_actions_share_first_wins_bounds_and_replacement() {
    use crate::ui::PickerSurfaceKind;
    use crate::workspace::PickerAction;
    use crate::workspace::PickerItem;
    use crate::workspace::PickerItemId;
    let root = tempfile::tempdir().unwrap();
    let source = scope(root.path());
    let mut owner = Search::default();
    let row = |id: &str, value: &str| {
        (
            PickerItem {
                badge: String::new(),
                kind: crate::workspace::PickerItemKind::Entry,
                id: PickerItemId(id.into()),
                label: value.into(),
                detail: String::new(),
                source: String::new(),
                action: PickerAction::None,
            },
            PickerPayload::Command(value.into()),
        )
    };
    let content = |items| PickerContent {
        surface: PickerSurfaceKind::Generic,
        origin: PickerOrigin::General,
        title: "owned".into(),
        items,
        query: String::new(),
        complete: true,
    };
    owner
        .install_picker(
            content(vec![
                row("same", "first"),
                row("same", "wrong"),
                row("overflow", "overflow"),
            ]),
            1,
            source.clone(),
            1,
        )
        .unwrap();
    assert_eq!(
        owner.picker().unwrap().selected_item().unwrap().label,
        "first"
    );
    assert!(
        matches!(owner.take_picker_payload(), Some(PickerPayload::Command(command)) if command == "first")
    );
    owner.picker_move(1, false);
    assert!(owner.take_picker_payload().is_none());
    owner
        .install_picker(content(vec![row("same", "replacement")]), 1, source, 1)
        .unwrap();
    assert!(
        matches!(owner.selected_picker_payload(), Some(PickerPayload::Command(command)) if command == "replacement")
    );
    owner.close_picker();
    assert!(owner.take_picker_payload().is_none());
    assert!(owner.selected_picker_payload().is_none());
}

impl Search {
    pub(in crate::app) fn fixture_search_completion(
        &mut self,
        matches: Vec<WorkspaceSearchMatch>,
        truncated: bool,
    ) {
        let stream_id = self.stream_id.expect("accepted search");
        self.grep = WorkspaceSearchWorker::events_for_test(vec![
            WorkspaceSearchEvent::Batch { stream_id, matches },
            WorkspaceSearchEvent::Complete {
                stream_id,
                summary: crate::files::WorkspaceSearchSummary {
                    truncated,
                    ..Default::default()
                },
            },
        ]);
    }
}

#[test]
fn crlf_previews_use_the_normalized_match_offset() {
    let mut source = b"a\r\n".repeat(300_000);
    source.extend_from_slice(b"needle\r\n");
    let mut found = found();
    found.snapshot = Arc::from(source);
    found.range = 600_000..600_006;
    found.line = 300_000;
    let preview = crate::app::picker::preview::search_picker_preview(&found, 1024);
    let text = preview.as_text().unwrap();
    assert!(text.lines().iter().any(|line| line == "needle"));
    assert_eq!(text.highlight_line, Some(300_000));
}

#[test]
fn rejected_live_query_clears_previous_results() {
    let root = tempfile::tempdir().unwrap();
    let source = scope(root.path());
    let mut owner = Search::default();
    owner.open_live();
    owner.set_query("needle".into());
    owner.stream_id = Some(9);
    owner.scope = Some((source.clone(), 1));
    owner.matches.push(found());
    owner.grep = WorkspaceSearchWorker::events_for_test(vec![WorkspaceSearchEvent::Batch {
        stream_id: 9,
        matches: vec![found()],
    }]);
    let query = "a".repeat(65_537);
    owner.set_query(query.clone());
    let policy = WorkspaceSearchPolicy {
        roots: vec![root.path().into()],
        query: query.clone(),
        regex: false,
        smart_case: true,
        whole_word: false,
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        include_hidden: false,
        include_ignored: false,
        maximum_files: 10,
        maximum_results: 10,
        maximum_file_bytes: 1024,
        maximum_total_bytes: 4096,
        maximum_preview_bytes: 100,
        batch_size: 10,
        event_queue_capacity: 8,
    };
    assert!(owner.start(policy, source.clone(), 1, None).is_err());
    assert!(owner.live());
    assert_eq!(owner.query(), query);
    assert!(!owner.pending());
    assert!(owner.matches().is_empty());
    assert!(owner.workflow().is_none());
    assert!(owner.poll(source, 1).is_empty());
}
