#![cfg(unix)]
//! How fast the Git tab is on a big repository (`tests/support/big_repo.py`).
//!
//! - `git_speed_audit` (ignored; `--ignored --nocapture`) builds 50k commits over 20k files,
//!   3k changes in the worktree, 300 branches, 50 stashes and a commit that edits 2k files (or
//!   uses `DUSTY_BIG_REPO`, a repository the script already built), and prints the time to the
//!   first paint, each list's `j`, History's `End`, a status refresh after touching one file,
//!   and opening the 2k-file commit; with `DUSTY_GIT_CACHE_DIR` set, also the first paint of a
//!   reopened tab over the warm cache.
//! - `a_big_repository_stays_fast`: the same steps on 5k commits and 2k changes, against bounds
//!   generous enough for a busy builder.

use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const WIDTH: u16 = 190;
const HEIGHT: u16 = 48;
/// A paint is settled once nothing on screen changed for this long.
const QUIET: Duration = Duration::from_millis(600);
const DEADLINE: Duration = Duration::from_secs(120);

struct Shape {
    commits: usize,
    files: usize,
    changes: usize,
    untracked: usize,
    branches: usize,
    stashes: usize,
    wide: usize,
}

const BIG: Shape = Shape {
    commits: 50_000,
    files: 20_000,
    changes: 3_000,
    untracked: 50,
    branches: 300,
    stashes: 50,
    wide: 2_000,
};

const CI: Shape = Shape {
    commits: 5_000,
    files: 4_000,
    changes: 1_980,
    untracked: 20,
    branches: 30,
    stashes: 5,
    wide: 500,
};

fn build(dir: &Path, shape: &Shape) {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/big_repo.py");
    let started = Instant::now();
    let status = Command::new("python3")
        .arg(script)
        .arg(dir)
        .args(["--commits", &shape.commits.to_string()])
        .args(["--files", &shape.files.to_string()])
        .args(["--changes", &shape.changes.to_string()])
        .args(["--untracked", &shape.untracked.to_string()])
        .args(["--branches", &shape.branches.to_string()])
        .args(["--stashes", &shape.stashes.to_string()])
        .args(["--wide", &shape.wide.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "big_repo.py failed");
    eprintln!("built the repository in {:?}", started.elapsed());
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(WIDTH, HEIGHT);
    (0..HEIGHT)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Poll and paint, as the terminal loop does, until `ready`; the time from `start`.
fn until(
    screen: &mut GitScreen,
    start: Instant,
    what: &str,
    ready: impl Fn(&str) -> bool,
) -> Duration {
    loop {
        screen.poll();
        let frame = view(screen);
        if ready(&frame) {
            return start.elapsed();
        }
        assert!(start.elapsed() < DEADLINE, "{what} never painted:\n{frame}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Until nothing changes for `QUIET`; the time from `start` to the last change.
fn settled(screen: &mut GitScreen, start: Instant) -> Duration {
    let mut last = view(screen);
    let mut changed = start.elapsed();
    let mut quiet = Instant::now();
    while quiet.elapsed() < QUIET {
        screen.poll();
        let frame = view(screen);
        if frame != last {
            last = frame;
            changed = start.elapsed();
            quiet = Instant::now();
        }
        assert!(start.elapsed() < DEADLINE, "never settled:\n{last}");
        std::thread::sleep(Duration::from_millis(2));
    }
    changed
}

/// One keystroke as the terminal loop sees it: the key, a poll and a paint.
fn stroke(screen: &mut GitScreen, code: KeyCode) -> (Duration, Duration) {
    let start = Instant::now();
    key(screen, code);
    let handled = start.elapsed();
    screen.poll();
    screen.frame(WIDTH, HEIGHT);
    (handled, start.elapsed())
}

#[derive(Default)]
struct Times(Vec<(String, Duration)>);

impl Times {
    fn add(&mut self, label: &str, time: Duration) {
        println!("GITSPEED\t{label}\t{}", time.as_micros());
        self.0.push((label.into(), time));
    }

    fn get(&self, label: &str) -> Duration {
        self.0
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, time)| *time)
            .unwrap()
    }
}

/// `j` 20 times in the focused list: the slowest key and the slowest key-to-paint.
fn walk(screen: &mut GitScreen, times: &mut Times, list: &str) {
    let mut handled = Vec::new();
    let mut painted = Vec::new();
    for _ in 0..20 {
        let (key, paint) = stroke(screen, KeyCode::Char('j'));
        handled.push(key);
        painted.push(paint);
    }
    handled.sort();
    painted.sort();
    times.add(
        &format!("{list} j: key handled (max of 20)"),
        *handled.last().unwrap(),
    );
    times.add(
        &format!("{list} j: key to paint (median of 20)"),
        painted[painted.len() / 2],
    );
    times.add(
        &format!("{list} j: key to paint (max of 20)"),
        *painted.last().unwrap(),
    );
}

fn audit(root: &Path, shape: &Shape) -> Times {
    let mut times = Times::default();
    let total = shape.changes + shape.untracked;
    let newest = format!("commit {}", shape.commits);
    // Open: the Changes count and the newest commit on screen.
    let start = Instant::now();
    let mut screen = GitScreen::open(root).unwrap();
    times.add("open: GitScreen::open", start.elapsed());
    let count = format!("{total} · tree");
    // The tab paints progressively: the frame at once, History and branches from their own
    // commands, Changes when the status (the slowest read) answers.
    let (mut frame_at, mut history_at, mut branches_at) = (None, None, None);
    let both = loop {
        screen.poll();
        let frame = view(&mut screen);
        let now = start.elapsed();
        frame_at.get_or_insert(now);
        if history_at.is_none() && frame.contains(&newest) {
            history_at = Some(now);
        }
        if branches_at.is_none()
            && frame.match_indices("Local ").any(|(at, found)| {
                frame[at + found.len()..]
                    .starts_with(|digit: char| digit.is_ascii_digit() && digit != '0')
            })
        {
            branches_at = Some(now);
        }
        if frame.contains(&count) && history_at.is_some() {
            break now;
        }
        assert!(now < DEADLINE, "the first paint never came:\n{frame}");
        std::thread::sleep(Duration::from_millis(1));
    };
    times.add("open: first frame", frame_at.unwrap());
    times.add(
        "open: History painted (first meaningful paint)",
        history_at.unwrap(),
    );
    times.add(
        "open: branches painted",
        branches_at.unwrap_or(Duration::MAX / 4),
    );
    times.add("open: first paint (changes and history)", both);
    times.add("open: settled", settled(&mut screen, start));

    // `j` through each list.
    key(&mut screen, KeyCode::Char('2'));
    walk(&mut screen, &mut times, "Changes");
    key(&mut screen, KeyCode::Char('3'));
    settled(&mut screen, Instant::now());
    walk(&mut screen, &mut times, "Branches");
    // History follows the selected branch: back to `main`, the top row.
    key(&mut screen, KeyCode::Home);
    key(&mut screen, KeyCode::Char('4'));
    settled(&mut screen, Instant::now());
    walk(&mut screen, &mut times, "History");
    settled(&mut screen, Instant::now());

    // History to its end: the root commit.
    let start = Instant::now();
    let (handled, _) = stroke(&mut screen, KeyCode::End);
    times.add("History End: key handled", handled);
    // `commit 1` and not `commit 12…`.
    let root_commit = |frame: &str| {
        frame.match_indices("commit 1").any(|(at, found)| {
            !frame[at + found.len()..].starts_with(|next: char| next.is_ascii_digit())
        })
    };
    let reached = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            screen.poll();
            let frame = view(&mut screen);
            if root_commit(&frame) {
                break Some(start.elapsed());
            }
            if Instant::now() > deadline {
                println!("GITSPEED-NOTE\tHistory End stops short of the root:\n{frame}");
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    times.add(
        "History End: root commit painted",
        reached.unwrap_or(Duration::MAX / 4),
    );
    key(&mut screen, KeyCode::Home);
    settled(&mut screen, Instant::now());

    // The commit that edits `wide` files, 10 below the tip.
    for _ in 0..9 {
        stroke(&mut screen, KeyCode::Char('j'));
    }
    settled(&mut screen, Instant::now());
    let wide = shape.wide.to_string();
    let start = Instant::now();
    let (handled, _) = stroke(&mut screen, KeyCode::Char('j'));
    times.add(
        &format!("open the {wide}-file commit: key handled"),
        handled,
    );
    times.add(
        &format!("open the {wide}-file commit: file list painted"),
        until(&mut screen, start, "the wide commit", |frame| {
            frame
                .lines()
                .any(|line| line.contains("Commit files") && line.contains(&format!(" {wide} ")))
        }),
    );
    times.add(
        &format!("open the {wide}-file commit: settled"),
        settled(&mut screen, start),
    );
    // Enter moves into its files; `j` walks them (each loads that file's patch).
    let (_, paint) = stroke(&mut screen, KeyCode::Enter);
    times.add(
        &format!("open the {wide}-file commit: Enter into its files"),
        paint,
    );
    settled(&mut screen, Instant::now());
    let mut painted = Vec::new();
    for _ in 0..20 {
        painted.push(stroke(&mut screen, KeyCode::Char('j')).1);
    }
    painted.sort();
    times.add(
        &format!("{wide}-file commit j: key to paint (median of 20)"),
        painted[painted.len() / 2],
    );
    times.add(
        &format!("{wide}-file commit j: key to paint (max of 20)"),
        *painted.last().unwrap(),
    );

    // A status refresh after touching one file, back in Changes (which shows the count):
    // Esc goes from the commit's files to History, then to Changes.
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('2'));
    settled(&mut screen, Instant::now());
    std::fs::write(root.join("new/touched.txt"), "touched\n").unwrap();
    let after = format!("{} · tree", total + 1);
    let start = Instant::now();
    let (handled, _) = stroke(&mut screen, KeyCode::Char('r'));
    times.add("refresh after touching a file: key handled", handled);
    times.add(
        "refresh after touching a file: count painted",
        until(&mut screen, start, "the refreshed count", |frame| {
            frame.contains(&after)
        }),
    );
    times.add(
        "refresh after touching a file: settled",
        settled(&mut screen, start),
    );
    println!("GITSPEED-FRAME\n{}", view(&mut screen));

    // Reopened over the warm cache (`DUSTY_GIT_CACHE_DIR`, `src/git/screen/warm.rs`): the
    // last status, History and branches paint before git answers.
    if std::env::var_os("DUSTY_GIT_CACHE_DIR").is_some() {
        drop(screen);
        // The cache is written on a thread after the refresh settled.
        std::thread::sleep(Duration::from_millis(500));
        let start = Instant::now();
        let mut screen = GitScreen::open(root).unwrap();
        times.add("reopen, warm cache: GitScreen::open", start.elapsed());
        times.add(
            "reopen, warm cache: first paint (changes and history)",
            until(&mut screen, start, "the warm paint", |frame| {
                (frame.contains(&count) || frame.contains(&after)) && frame.contains(&newest)
            }),
        );
        times.add("reopen, warm cache: settled", settled(&mut screen, start));
    }
    let _ = std::fs::remove_file(root.join("new/touched.txt"));
    times
}

#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn git_speed_audit() {
    let built;
    let root = match std::env::var_os("DUSTY_BIG_REPO") {
        Some(root) => PathBuf::from(root),
        None => {
            built = tempfile::tempdir().unwrap();
            build(built.path(), &BIG);
            built.path().to_owned()
        }
    };
    audit(&root, &BIG);
}

#[test]
fn a_big_repository_stays_fast() {
    let dir = tempfile::tempdir().unwrap();
    build(dir.path(), &CI);
    let times = audit(dir.path(), &CI);
    // Bounds for a busy shared builder or hosted runner (macOS ones measured 63 ms for a
    // Branches key), several times what the tab takes on a quiet machine.
    for (label, bound) in [
        ("open: first paint (changes and history)", 3_000),
        ("Changes j: key to paint (median of 20)", 100),
        ("History j: key to paint (median of 20)", 100),
        ("Branches j: key to paint (median of 20)", 100),
        ("refresh after touching a file: count painted", 3_000),
    ] {
        let time = times.get(label);
        assert!(
            time < Duration::from_millis(bound),
            "{label}: {time:?} (bound {bound} ms)"
        );
    }
}
