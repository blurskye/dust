//! FORGE.md §7.2/§7.3/§7.5: stale-while-revalidate, merging without moving the reader,
//! conditional requests and the warm worker (F3).
#![cfg(unix)]
use dust::editor::input::KeyCode;
use std::time::Duration;

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_screen.rs"]
mod forge_screen;
use forge_screen::*;

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
    view(screen)
        .lines()
        .map(|line| line.chars().take(47).collect::<String>())
        .filter(|line| line.contains("Navigation issue"))
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
    screen.poll();
    let cached = view(&mut screen);
    assert!(cached.contains("#2 Navigation issue 2"), "{cached}");
    assert!(!cached.contains("Navigation issue 10"), "{cached}");
    assert!(cached.contains("cached 1h ago"), "{cached}");
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Char('j'));
    assert!(cursor_row(&mut screen).contains("#6 Navigation issue 6"));
    wait(&mut screen, "#10 Navigation issue 10");
    wait(&mut screen, "updated just now");
    assert!(
        cursor_row(&mut screen).contains("#6 Navigation issue 6"),
        "the cursor stays on its item:\n{}",
        view(&mut screen)
    );
    let rows = list_rows(&mut screen);
    let text = view(&mut screen);
    assert!(text.contains("#5 Navigation issue 5 renamed"), "{text}");
    // The closed issue stays where it was, marked, until the list is left or refreshed.
    assert!(text.contains("#2 Navigation issue 2"), "{text}");
    assert!(text.contains("closed · "), "{text}");
    let position = |needle: &str| rows.iter().position(|row| row.contains(needle)).unwrap();
    assert!(position("#4 ") < position("#2 ") && position("#2 ") < position("#1 "));
    assert!(position("#10 ") < position("#9 ") && position("#9 ") < position("#8 "));
    // An explicit refresh shows exactly what the forge has.
    key(&mut screen, KeyCode::Char('r'));
    wait_for(&mut screen, "#2 gone after refresh", |frame| {
        !frame.contains("#2 Navigation issue 2") && frame.contains("#10 Navigation issue 10")
    });
    assert!(cursor_row(&mut screen).contains("#6 Navigation issue 6"));
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
    screen.poll();
    let before = list_rows(&mut screen);
    assert!(!before.is_empty());
    wait_for(&mut screen, "a 304 for the repository", |_| {
        state()["not_modified"].as_u64().unwrap_or(0) >= 1
    });
    wait(&mut screen, "updated just now");
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
