#![cfg(unix)]
//! How fast the forge tabs paint against a slow forge: the fake forge answers
//! every request after `delay_ms`, and the screen is driven headless, as a terminal would.
//!
//! - `speed_audit` (ignored; `--ignored --nocapture`) prints, for each forge, the time to a
//!   settled paint of a cold start, each tab, an opened pull request's four tabs, an issue and
//!   `j` through the list, then the same after a restart from the disk cache.
//! - `cached_paint_does_not_wait_on_the_network`: a restart paints the list, and the details of
//!   the row it opens on, from the cache while the forge is still answering, keys move the
//!   cursor at once, and the list says it is cached until the forge confirms it.
//!
//! - `warm_tabs_paint_from_the_cache`: the same for Notifications, Actions and a pull
//!   request's Commits and Checks on Gitea and Forgejo, "not found" answers included.
//! - `a_slow_read_on_one_tab_does_not_delay_another_tabs_paint`: a 2 s read on Actions does
//!   not hold up the Issues tab (the screen's reads run side by side).
//!
//! Each world runs in its own child process, as in `forge_features`.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use forge_world::{ALL, Forge, World};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_FORGE_SPEED_CHILD";
/// Per request, as a distant forge answers.
const LATENCY_MS: u64 = 300;
/// A paint is settled once nothing on screen changed for this long (over two round trips).
const QUIET: Duration = Duration::from_millis(900);
const DEADLINE: Duration = Duration::from_secs(60);

fn rerun_in_child(test: &str, world: &World, forge: Forge) -> String {
    rerun_in_child_with(test, world, forge, &[])
}

fn rerun_in_child_with(test: &str, world: &World, forge: Forge, env: &[(&str, &str)]) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.envs(env.iter().copied());
    command
        .args([
            test,
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
            "--include-ignored",
        ])
        .env(CHILD, forge.name())
        .env("DUSTY_SPEED_REPO", &world.repo)
        .env("DUSTY_SPEED_STATE", world.state_path());
    world.configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its {} child:\n{stdout}\n{}",
        forge.name(),
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// The forge this child runs against, and its repository.
fn child() -> Option<(Forge, PathBuf)> {
    let name = std::env::var(CHILD).ok()?;
    let forge = ALL.into_iter().find(|forge| forge.name() == name).unwrap();
    Some((
        forge,
        PathBuf::from(std::env::var_os("DUSTY_SPEED_REPO").unwrap()),
    ))
}

fn set_delay(millis: u64) {
    let state = PathBuf::from(std::env::var_os("DUSTY_SPEED_STATE").unwrap());
    forge_fake::update(&state, |state| state["delay_ms"] = json!(millis));
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    (0..48)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The frame without what changes on its own: ages (`updated 3s ago`) and the busy mark.
fn steady(frame: &str) -> String {
    frame
        .replace("just now", "")
        .replace(" · ⟳", "")
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .collect()
}

fn loading(frame: &str) -> bool {
    frame.contains("Loading") || frame.contains("loading details")
}

/// A list row: `#12 ` or `!12 `.
fn has_row(frame: &str) -> bool {
    frame.lines().any(|line| {
        let chars = line.chars().collect::<Vec<_>>();
        (0..chars.len()).any(|at| {
            matches!(chars[at], '#' | '!')
                && chars.get(at + 1).is_some_and(char::is_ascii_digit)
                && chars[at + 1..].iter().find(|c| !c.is_ascii_digit()) == Some(&' ')
        })
    })
}

/// Polls as a terminal would until `ready` holds and the screen then stays unchanged for
/// `QUIET`: the time from `start` to the last change, which is when the paint settled.
fn settled(
    screen: &mut GitScreen,
    start: Instant,
    what: &str,
    ready: impl Fn(&str) -> bool,
) -> Duration {
    let mut last = steady(&view(screen));
    let mut changed = start.elapsed();
    let mut quiet_since: Option<Instant> = None;
    loop {
        screen.poll();
        let frame = view(screen);
        let now = steady(&frame);
        if now != last {
            last = now;
            changed = start.elapsed();
            quiet_since = None;
        }
        if ready(&frame) && !loading(&frame) {
            let since = *quiet_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= QUIET {
                return changed;
            }
        } else {
            quiet_since = None;
        }
        assert!(start.elapsed() < DEADLINE, "{what} never settled:\n{frame}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The first poll whose frame satisfies `ready`, from `start`.
fn first(
    screen: &mut GitScreen,
    start: Instant,
    what: &str,
    ready: impl Fn(&str) -> bool,
) -> Duration {
    loop {
        screen.poll();
        let frame = view(screen);
        if ready(&frame) && !loading(&frame) {
            return start.elapsed();
        }
        assert!(start.elapsed() < DEADLINE, "{what} never painted:\n{frame}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn report(forge: Forge, phase: &str, label: &str, time: Duration) {
    println!(
        "SPEED\t{}\t{phase}\t{label}\t{}",
        forge.name(),
        time.as_millis()
    );
}

/// The first meaningful paint (`ready`, nothing loading) and the settled one, both from `start`.
fn measure(
    screen: &mut GitScreen,
    forge: Forge,
    phase: &str,
    label: &str,
    start: Instant,
    ready: impl Fn(&str) -> bool,
) {
    report(
        forge,
        phase,
        &format!("{label}: first paint"),
        first(screen, start, label, &ready),
    );
    report(
        forge,
        phase,
        &format!("{label}: settled"),
        settled(screen, start, label, &ready),
    );
}

/// One session through every tab: cold when the cache is empty, warm when a session filled it.
fn session(forge: Forge, repo: &Path, phase: &str) {
    let sigil = if forge == Forge::GitLab { '!' } else { '#' };
    // The test harness prints `test speed_audit ... ` without a newline first.
    println!();
    let start = Instant::now();
    let mut screen = GitScreen::open(repo).unwrap();
    screen.open_target("prs", None);
    measure(&mut screen, forge, phase, "start", start, has_row);
    // `j` through the list: the reader beside it follows the cursor.
    let mut moves = Vec::new();
    for _ in 0..4 {
        let start = Instant::now();
        key(&mut screen, KeyCode::Char('j'));
        let input = start.elapsed();
        moves.push(settled(
            &mut screen,
            start,
            "the next row's details",
            has_row,
        ));
        report(forge, phase, "j: key handled", input);
    }
    moves.sort();
    report(
        forge,
        phase,
        "j: details settled (median of 4)",
        moves[moves.len() / 2],
    );
    for (page, label) in [
        ("issues", "Issues"),
        ("actions", "Actions"),
        ("notifications", "Notifications"),
    ] {
        let start = Instant::now();
        screen.open_target(page, None);
        measure(
            &mut screen,
            forge,
            phase,
            &format!("tab {label}"),
            start,
            |_| true,
        );
    }
    let start = Instant::now();
    screen.open_target("prs", None);
    measure(
        &mut screen,
        forge,
        phase,
        "tab Pull requests",
        start,
        has_row,
    );
    // An opened pull request and each of its tabs.
    let number = format!("{sigil}{}", forge.pull());
    let start = Instant::now();
    screen.open_target("prs", Some(forge.pull()));
    measure(
        &mut screen,
        forge,
        phase,
        "PR Conversation",
        start,
        |frame| frame.contains(&number),
    );
    for tab in ["Commits", "Checks", "Files changed"] {
        let start = Instant::now();
        key(&mut screen, KeyCode::Char('L'));
        measure(
            &mut screen,
            forge,
            phase,
            &format!("PR {tab}"),
            start,
            |frame| frame.contains(&number),
        );
    }
    let number = format!("#{}", forge.issue());
    let start = Instant::now();
    screen.open_target("issues", Some(forge.issue()));
    measure(&mut screen, forge, phase, "issue", start, |frame| {
        frame.contains(&number)
    });
}

#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn speed_audit() {
    if let Some((forge, repo)) = child() {
        set_delay(LATENCY_MS);
        session(forge, &repo, "cold");
        // A restart: the same disk cache, a new screen.
        session(forge, &repo, "warm");
        return;
    }
    // `DUSTY_SPEED_FORGES=Gitea,Forgejo` measures only those.
    let only = std::env::var("DUSTY_SPEED_FORGES").unwrap_or_default();
    for forge in ALL
        .into_iter()
        .filter(|forge| only.is_empty() || only.split(',').any(|name| name == forge.name()))
    {
        let world = World::new(forge);
        let output = rerun_in_child("speed_audit", &world, forge);
        for line in output.lines().filter(|line| line.starts_with("SPEED\t")) {
            println!("{line}");
        }
    }
}

#[test]
fn cached_paint_does_not_wait_on_the_network() {
    let Some((_, repo)) = child() else {
        for forge in [Forge::GitHub, Forge::GitLab, Forge::Gitea] {
            let world = World::new(forge);
            rerun_in_child("cached_paint_does_not_wait_on_the_network", &world, forge);
        }
        return;
    };
    // A first session fills the disk cache: both lists and the rows they open on.
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("issues", None);
    settled(
        &mut screen,
        Instant::now(),
        "the first session's issues",
        has_row,
    );
    screen.open_target("prs", None);
    settled(
        &mut screen,
        Instant::now(),
        "the first session's list",
        has_row,
    );
    let cached = view(&mut screen);
    drop(screen);
    // The forge now takes far longer than a frame to answer anything.
    set_delay(5000);
    let start = Instant::now();
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    // Measured across polls: the disk may answer on the second, the forge not for 5 s.
    let painted = first(&mut screen, start, "the cached list", has_row);
    let frame = view(&mut screen);
    assert!(
        painted < Duration::from_millis(1500),
        "the first frame took {painted:?}:\n{frame}"
    );
    // The reader beside the list shows the cached row's details too.
    let title = cached
        .lines()
        .find(|line| line.contains("Pull request") || line.contains("Merge request"));
    assert!(title.is_some(), "{cached}");
    // Keys are handled at once while the refresh is in flight.
    let start = Instant::now();
    key(&mut screen, KeyCode::Char('j'));
    screen.poll();
    view(&mut screen);
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "`j` waited {:?}",
        start.elapsed()
    );
    // Another tab paints from the cache too, behind the list refresh still in flight. The
    // selected issue's conversation is a background read the disk answers a poll or two
    // later; the forge would take 5 s.
    let start = Instant::now();
    screen.open_target("issues", None);
    screen.poll();
    let issues = view(&mut screen);
    assert!(
        start.elapsed() < Duration::from_millis(500) && has_row(&issues),
        "the Issues tab waited {:?}:\n{issues}",
        start.elapsed()
    );
    let painted = first(&mut screen, start, "the cached issue", has_row);
    let issues = view(&mut screen);
    assert!(
        painted < Duration::from_millis(1500),
        "the Issues tab paints from the cache, not the forge ({painted:?}):\n{issues}"
    );
}

/// A restart paints Notifications, Actions and an opened pull request's Commits and Checks
/// from the disk cache on Gitea and Forgejo too, including the answers that were "not found"
/// (an expired job log, a run or commit diff the forge no longer has): nothing waits on the
/// forge, which now takes 5 s per request.
#[test]
fn warm_tabs_paint_from_the_cache() {
    let Some((forge, repo)) = child() else {
        for forge in [Forge::Gitea, Forge::Forgejo] {
            let world = World::new(forge);
            rerun_in_child("warm_tabs_paint_from_the_cache", &world, forge);
        }
        return;
    };
    let number = format!("#{}", forge.pull());
    #[allow(clippy::type_complexity)]
    let steps: [(&str, &dyn Fn(&mut GitScreen)); 5] = [
        ("Notifications", &|screen| {
            screen.open_target("notifications", None)
        }),
        ("Actions", &|screen| screen.open_target("actions", None)),
        ("PR", &|screen| {
            screen.open_target("prs", Some(forge.pull()))
        }),
        ("PR Commits", &|screen| key(screen, KeyCode::Char('L'))),
        ("PR Checks", &|screen| key(screen, KeyCode::Char('L'))),
    ];
    let ready = |label: &str, frame: &str| !label.starts_with("PR") || frame.contains(&number);
    // A first session fills the disk cache.
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    settled(
        &mut screen,
        Instant::now(),
        "the first session's list",
        has_row,
    );
    for (label, step) in &steps {
        step(&mut screen);
        settled(&mut screen, Instant::now(), label, |frame| {
            ready(label, frame)
        });
    }
    drop(screen);
    set_delay(5000);
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    first(&mut screen, Instant::now(), "the restarted list", has_row);
    for (label, step) in &steps {
        let start = Instant::now();
        step(&mut screen);
        let painted = first(&mut screen, start, label, |frame| ready(label, frame));
        let frame = view(&mut screen);
        assert!(
            painted < Duration::from_millis(1500),
            "{label} waited {painted:?} on the forge after a restart:\n{frame}"
        );
    }
}

/// A read that takes 2 s on one tab (Actions) does not hold up the next tab's first paint:
/// the screen runs its reads side by side (`lanes.rs`), so Issues is asked at once.
#[test]
fn a_slow_read_on_one_tab_does_not_delay_another_tabs_paint() {
    let Some((_, repo)) = child() else {
        for forge in [Forge::GitHub, Forge::Gitea] {
            let world = World::new(forge);
            // No cache warming: the Issues list must come from the network, not a prefetch.
            rerun_in_child_with(
                "a_slow_read_on_one_tab_does_not_delay_another_tabs_paint",
                &world,
                forge,
                &[("DUSTY_FORGE_WARM", "0")],
            );
        }
        return;
    };
    let state = PathBuf::from(std::env::var_os("DUSTY_SPEED_STATE").unwrap());
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    settled(&mut screen, Instant::now(), "the pull requests", has_row);
    forge_fake::update(&state, |state| {
        state["slow_paths"] = json!({"/actions/": 2000})
    });
    let opened = Instant::now();
    screen.open_target("actions", None);
    // The Actions reads are on their way (and will take 2 s).
    while opened.elapsed() < Duration::from_millis(100) {
        screen.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    screen.open_target("issues", None);
    let painted = first(
        &mut screen,
        start,
        "the Issues tab behind a slow Actions read",
        has_row,
    );
    let frame = view(&mut screen);
    assert!(
        painted < Duration::from_millis(1200),
        "Issues waited {painted:?} behind the slow Actions read:\n{frame}"
    );
    assert!(
        opened.elapsed() < Duration::from_millis(2000),
        "the Actions read was still in flight when Issues painted"
    );
    forge_fake::update(&state, |state| state["slow_paths"] = json!({}));
}

/// GitLab numbers issues and merge requests apart, and a timeline read names only the number.
/// After a restart that restores an issue from the disk cache without reading it again, its
/// timeline is read as the issue's (`issues/14/notes`), never guessed to be a merge request's.
#[test]
fn a_restored_gitlab_issue_reads_an_issues_timeline() {
    if let Some((forge, repo)) = child() {
        let number = format!("#{}", forge.issue());
        let mut screen = GitScreen::open(&repo).unwrap();
        screen.open_target("issues", Some(forge.issue()));
        settled(&mut screen, Instant::now(), "the issue", |frame| {
            frame.contains(&number)
        });
        return;
    }
    let forge = Forge::GitLab;
    let world = World::new(forge);
    let test = "a_restored_gitlab_issue_reads_an_issues_timeline";
    rerun_in_child(test, &world, forge);
    // The restart finds the issue on disk (fresh, so not read again) and its timeline missing.
    fn timelines(dir: &Path) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "timeline") {
                    std::fs::remove_dir_all(&path).unwrap();
                } else {
                    timelines(&path);
                }
            }
        }
    }
    timelines(&world.cache_dir());
    let paths = || {
        world.state()["requests"]
            .as_array()
            .map(|requests| {
                requests
                    .iter()
                    .map(|request| request["path"].as_str().unwrap_or_default().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let before = paths().len();
    rerun_in_child(test, &world, forge);
    let after = paths()[before..].to_vec();
    let issue = forge.issue();
    assert!(
        after
            .iter()
            .any(|path| path.contains(&format!("/issues/{issue}/notes"))),
        "the issue's timeline was read: {after:?}"
    );
    assert!(
        !after
            .iter()
            .any(|path| path.contains(&format!("/merge_requests/{issue}"))),
        "no merge request was guessed: {after:?}"
    );
}

/// Keys never wait for the disk (a cold page cache, a busy machine): with every disk cache read
/// taking 250 ms, `j` through a list restored from the cache is still handled at once, and the
/// rows' details still paint from the cache when their reads land.
#[test]
fn keys_never_wait_on_a_slow_disk_cache() {
    // Every cache read takes a second. Handling a key is under 1 ms; the bound leaves room
    // for a busy CI runner (macOS ones have taken 75 ms) and stays far below one read, so a
    // key that waited for the disk still fails.
    const READ_DELAY_MS: u64 = 1000;
    const BOUND: Duration = Duration::from_millis(READ_DELAY_MS * 2 / 5);
    if let Some((forge, repo)) = child() {
        let mut screen = GitScreen::open(&repo).unwrap();
        screen.open_target("prs", None);
        settled(&mut screen, Instant::now(), "the list", has_row);
        let slow = std::env::var("DUSTY_FORGE_CACHE_READ_DELAY_MS").is_ok();
        for row in 0..4 {
            let start = Instant::now();
            key(&mut screen, KeyCode::Char('j'));
            let handled = start.elapsed();
            if slow {
                assert!(
                    handled < BOUND,
                    "{}: `j` to row {} waited {handled:?} for the disk",
                    forge.name(),
                    row + 1
                );
            }
            // Filling: each row's details reach the disk. Slow: they still paint from it.
            settled(&mut screen, start, "the next row's details", has_row);
        }
        return;
    }
    let delay = READ_DELAY_MS.to_string();
    for forge in [Forge::Gitea, Forge::Forgejo] {
        let world = World::new(forge);
        let test = "keys_never_wait_on_a_slow_disk_cache";
        rerun_in_child(test, &world, forge);
        // A restart over the filled cache, the forge slow too so nothing races the disk.
        forge_fake::update(&world.state_path(), |state| state["delay_ms"] = json!(2000));
        rerun_in_child_with(
            test,
            &world,
            forge,
            &[("DUSTY_FORGE_CACHE_READ_DELAY_MS", delay.as_str())],
        );
    }
}
