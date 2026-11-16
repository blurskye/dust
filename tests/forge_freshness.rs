//! Stale-while-revalidate, merging without moving the reader,
//! conditional requests and the warm worker (F3).
#![cfg(unix)]
use dust::editor::input::KeyCode;
use std::time::Duration;

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_screen.rs"]
mod forge_screen;
#[path = "support/records.rs"]
mod records;
use forge_screen::*;
use records::lists;

fn issue(id: u64, title: &str) -> serde_json::Value {
    serde_json::json!({"number": id, "title": title, "body": format!("Unique selected body {id}.")})
}

fn prime(ready: &str) {
    let mut screen = open();
    goto(&mut screen, 0, 1);
    wait(&mut screen, ready);
    settle(&mut screen, Duration::from_millis(400));
}

fn list_rows(screen: &mut dust::git::screen::GitScreen) -> Vec<String> {
    // Each record's title row with its `#N opened …` row.
    let lines = view(screen)
        .lines()
        .map(|line| line.chars().take(47).collect::<String>())
        .collect::<Vec<_>>();
    (0..lines.len())
        .filter(|&y| lines[y].contains("Navigation issue"))
        .map(|y| {
            format!(
                "{} {}",
                lines[y],
                lines.get(y + 1).map_or("", String::as_str)
            )
        })
        .collect()
}

#[test]
fn the_cursor_survives_a_background_merge() {
    if fixture_child(
        "the_cursor_survives_a_background_merge",
        serde_json::json!({"calls":[],"comments":["Merge fixture ready"],"issues":navigation_issues()}),
    ) {
        return;
    }
    prime("Merge fixture ready");
    age_cache(3600);
    // Since the last session: two issues were opened, one renamed and one closed.
    update_state(|state| {
        // Held until the cursor is on #6: the merge lands under a moved cursor.
        state["read_gates"] =
            serde_json::json!([{"prefix": ["issue", "list"], "name": "merge-list"}]);
        state["issues"] = serde_json::json!([
            issue(10, "Navigation issue 10"),
            issue(9, "Navigation issue 9"),
            issue(8, "Navigation issue 8"),
            issue(7, "Navigation issue 7"),
            issue(6, "Navigation issue 6"),
            issue(5, "Navigation issue 5 renamed"),
            issue(4, "Navigation issue 4"),
            issue(1, "Navigation issue 1"),
        ]);
    });
    let mut screen = open();
    goto(&mut screen, 0, 1);
    // The disk answers within a poll or a few: a busy machine can need more than one.
    wait_for(&mut screen, "the cached issues", |frame| {
        lists(frame, 2, "Navigation issue 2")
    });
    let cached = view(&mut screen);
    assert!(!cached.contains("Navigation issue 10"), "{cached}");
    assert!(cached.contains("updated 1h ago"), "{cached}");
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Char('j'));
    assert!(lists(&cursor_record(&mut screen), 6, "Navigation issue 6"));
    release("merge-list");
    wait_for(&mut screen, "#10", |frame| {
        lists(frame, 10, "Navigation issue 10")
    });
    wait(&mut screen, "updated just now");
    assert!(
        lists(&cursor_record(&mut screen), 6, "Navigation issue 6"),
        "the cursor stays on its item:\n{}",
        view(&mut screen)
    );
    let rows = list_rows(&mut screen);
    let text = view(&mut screen);
    assert!(lists(&text, 5, "Navigation issue 5 renamed"), "{text}");
    // The closed issue stays where it was, marked, until the list is left or refreshed.
    assert!(lists(&text, 2, "Navigation issue 2"), "{text}");
    assert!(text.contains("closed · "), "{text}");
    let position = |needle: &str| rows.iter().position(|row| row.contains(needle)).unwrap();
    assert!(position("#4 ") < position("#2 ") && position("#2 ") < position("#1 "));
    assert!(position("#10 ") < position("#9 ") && position("#9 ") < position("#8 "));
    // An explicit refresh shows exactly what the forge has.
    key(&mut screen, KeyCode::Char('r'));
    wait_for(&mut screen, "#2 gone after refresh", |frame| {
        !lists(frame, 2, "Navigation issue 2") && lists(frame, 10, "Navigation issue 10")
    });
    assert!(lists(&cursor_record(&mut screen), 6, "Navigation issue 6"));
}

#[test]
fn not_modified_changes_no_data() {
    if fixture_child(
        "not_modified_changes_no_data",
        serde_json::json!({"calls":[],"comments":["Etag fixture ready"],"issues":navigation_issues()}),
    ) {
        return;
    }
    prime("Etag fixture ready");
    let repo = entries(&cache_dir())
        .into_iter()
        .find(|path| path.parent().is_some_and(|parent| parent.ends_with("repo")))
        .expect("the repository is cached");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&repo).unwrap()).unwrap();
    assert!(
        stored["etag"]
            .as_str()
            .is_some_and(|etag| etag.starts_with('"')),
        "{stored}"
    );
    age_cache(7200);
    let aged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&repo).unwrap()).unwrap();
    let mut screen = open();
    goto(&mut screen, 0, 1);
    wait_for(&mut screen, "the cached issues", |frame| {
        lists(frame, 1, "Navigation issue 1")
    });
    let before = list_rows(&mut screen);
    assert!(!before.is_empty());
    wait_for(&mut screen, "a 304 for the repository", |_| {
        state()["not_modified"].as_u64().unwrap_or(0) >= 1
    });
    // The 304 renews the saved entry; the screen's age text can belong to another pane.
    wait_for(&mut screen, "the repository entry renewed", |_| {
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&repo).unwrap()).unwrap();
        saved["fetched_at"].as_u64() > aged["fetched_at"].as_u64()
    });
    assert_eq!(before, list_rows(&mut screen));
    let renewed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&repo).unwrap()).unwrap();
    assert_eq!(renewed["data"], aged["data"], "a 304 keeps the cached data");
    assert_eq!(renewed["etag"], aged["etag"]);
    assert!(renewed["fetched_at"].as_u64() > aged["fetched_at"].as_u64());
    let conditional = requests()
        .iter()
        .filter(|request| {
            request["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("repos/example/demo"))
        })
        .count();
    assert!(conditional >= 2, "{:?}", requests());
}

#[test]
fn the_warm_worker_fills_what_the_user_has_not_opened_yet() {
    if fixture_child(
        "the_warm_worker_fills_what_the_user_has_not_opened_yet",
        serde_json::json!({"calls":[],"comments":["Warm fixture ready"],"issues":navigation_issues()}),
    ) {
        return;
    }
    let mut screen = open();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Warm fixture ready");
    settle(&mut screen, Duration::from_millis(500));
    let lists = entries(&cache_dir())
        .into_iter()
        .filter(|path| path.parent().is_some_and(|parent| parent.ends_with("list")))
        .count();
    assert!(lists >= 2, "issues and pull requests are both cached");
    assert!(
        entries(&cache_dir()).iter().any(|path| path
            .parent()
            .is_some_and(|parent| parent.ends_with("workflows"))),
        "workflows are warmed too"
    );
    // The pull request tab now paints at once, already checked this session.
    update_state(|state| state["delay_ms"] = 1500.into());
    goto(&mut screen, 1, 2);
    screen.poll();
    let text = view(&mut screen);
    assert!(text.contains("updated just now"), "{text}");
}
