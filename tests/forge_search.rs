//! FORGE.md §7.4: search shows cached hits while typing and streams the forge's results in
//! below them (F3).
#![cfg(unix)]
use dust::editor::input::KeyCode;
use std::time::{Duration, Instant};

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_screen.rs"]
mod forge_screen;
use forge_screen::*;

/// Lines of the list pane only (the form sits beside it).
fn list(screen: &mut dust::git::screen::GitScreen) -> String {
    view(screen)
        .lines()
        .map(|line| line.chars().take(47).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn cached_search_hits_appear_before_remote_results() {
    if fixture_child(
        "cached_search_hits_appear_before_remote_results",
        serde_json::json!({
            "calls":[],"comments":["Search fixture ready"],"issues":navigation_issues(),
            "issue_queries":{"issue 5":[
                {"number":5,"title":"Navigation issue 5","body":"Unique selected body 5."},
                {"number":42,"title":"Remote only issue 42","body":"Only the forge knows."}
            ]}
        }),
    ) {
        return;
    }
    {
        let mut screen = open();
        goto(&mut screen, 0, 1);
        wait(&mut screen, "Search fixture ready");
        settle(&mut screen, Duration::from_millis(400));
    }
    update_state(|state| state["delay_ms"] = 1500.into());
    let mut screen = open();
    goto(&mut screen, 0, 1);
    screen.poll();
    assert!(list(&mut screen).contains("#1 Navigation issue 1"));
    key(&mut screen, KeyCode::Char('/'));
    let typed = Instant::now();
    paste(&mut screen, "issue 5");
    screen.poll();
    let local = list(&mut screen);
    let shown = typed.elapsed();
    println!("measured: local search hits {shown:?} after typing");
    assert!(local.contains("#5 Navigation issue 5"), "{local}");
    assert!(!local.contains("#1 Navigation issue 1"), "{local}");
    assert!(local.contains("From github.com · searching ⟳"), "{local}");
    assert!(!local.contains("Remote only issue 42"), "{local}");
    // Remote results arrive below the divider, without repeating the local hit.
    wait(&mut screen, "#42 Remote only issue 42");
    let merged = list(&mut screen);
    assert!(merged.contains("From github.com · 1 more"), "{merged}");
    assert_eq!(
        merged.matches("#5 Navigation issue 5").count(),
        1,
        "{merged}"
    );
    let divider = merged.find("From github.com").unwrap();
    assert!(merged.find("#5 Navigation issue 5").unwrap() < divider);
    assert!(merged.find("#42 Remote only issue 42").unwrap() > divider);
    // A newer query supersedes the one in flight; nothing of the old one is reported.
    paste(&mut screen, "x");
    let end = Instant::now() + Duration::from_millis(320);
    while Instant::now() < end {
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    paste(&mut screen, "y");
    wait(&mut screen, "From github.com · 7 more");
    let last = view(&mut screen);
    assert!(
        !last.contains("offline") && !last.contains("cancel"),
        "{last}"
    );
    // Closing the form brings the list back as it was.
    key(&mut screen, KeyCode::Escape);
    wait(&mut screen, "#1 Navigation issue 1");
    assert!(!view(&mut screen).contains("From github.com"));
}
