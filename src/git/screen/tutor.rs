//! The Git tutor: hands-on lessons, each on its own disposable practice repository.
//!
//! Every lesson builds its fixture (a practice repository with a local bare remote)
//! in a fresh directory under the session's TempDir and reopens the screen there, so
//! lessons can be skipped, repeated or revisited in any order. A lesson ends only when
//! the real repository or screen state shows the user did the thing. The user's own
//! screen is saved whole and restored by `:tutor quit`; nothing touches their repos.
//!
//! Commands: `:tutor git [N]`, `:tutor next`, `:tutor prev`, `:tutor reset`,
//! `:tutor N` and `:tutor quit`.
use super::*;
use git2::{Oid, Repository, RepositoryState};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const NAME: &str = "Dusty Git Tutor";
const EMAIL: &str = "tutor@example.invalid";
const GREET: &str = "src/greet.rs";
const USAGE: &str = "Tutor: tutor git [N] · tutor next · tutor prev · tutor reset · tutor N · tutor quit";
const NOT_RUNNING: &str = "The Git tutor is not running; :tutor git starts it";

type Setup = Result<Option<Oid>, String>;

/// One hands-on step. Instructions use the current keys and fit a narrow banner.
struct Lesson {
    title: &'static str,
    instruction: &'static str,
    /// Builds the starting state on a fresh practice repository. It may return a
    /// commit the check needs (a lost commit, an original branch tip).
    setup: fn(&Fixture) -> Setup,
    /// Arriving by finishing the previous lesson keeps that repository as it is;
    /// arriving any other way (skip, back, jump, reset) runs `setup`.
    carry: bool,
    /// Whether the real repository or screen state shows the lesson was done.
    done: fn(&mut Probe) -> bool,
}

struct Chapter {
    name: &'static str,
    lessons: &'static [Lesson],
}

const fn fresh(
    title: &'static str,
    instruction: &'static str,
    setup: fn(&Fixture) -> Setup,
    done: fn(&mut Probe) -> bool,
) -> Lesson {
    Lesson {
        title,
        instruction,
        setup,
        carry: false,
        done,
    }
}

const fn carried(
    title: &'static str,
    instruction: &'static str,
    setup: fn(&Fixture) -> Setup,
    done: fn(&mut Probe) -> bool,
) -> Lesson {
    Lesson {
        title,
        instruction,
        setup,
        carry: true,
        done,
    }
}

/// The Git screen's chapters in teaching order.
static GIT: &[Chapter] = &[
    Chapter {
        name: "Panes",
        lessons: &[
            fresh(
                "Focus panes",
                "Press 3 Branches, 4 History, 5 Stashes, then 2 back to Changes.",
                panes,
                |p| {
                    let branches = p.saw(0, p.screen.focus == 1);
                    let history = p.saw(1, p.screen.focus == 2);
                    let stashes = p.saw(2, p.screen.focus == 3);
                    branches && history && stashes && p.screen.focus == 0
                },
            ),
            fresh(
                "Reader and Tab",
                "0 jumps to the reader on the right; Tab moves on to the next pane.",
                panes,
                |p| p.saw(0, p.screen.focus == 4) && p.screen.focus != 4,
            ),
            fresh(
                "Sub-tabs",
                "] switches sub-tabs. Open Tags in Branches (3) and Reflog in History (4).",
                panes,
                |p| {
                    let tags = p.saw(0, p.screen.subview == 3);
                    let reflog = p.saw(1, p.screen.history_view == 1);
                    tags && reflog
                },
            ),
        ],
    },
    Chapter {
        name: "Changes",
        lessons: &[
            fresh(
                "Flat and tree",
                "t shows Changes as a folder tree; h folds the folder you are on.",
                tree,
                |p| p.screen.changes_tree && !p.screen.collapsed.is_empty(),
            ),
            fresh(
                "All files",
                "F lists every tracked file, not only changes; F again goes back.",
                tree,
                |p| p.saw(0, p.screen.all_files) && !p.screen.all_files,
            ),
            fresh(
                "Filter",
                "/ filters the list: type guide, Enter keeps it, Esc clears it.",
                tree,
                |p| {
                    let filter = &p.screen.filters[0];
                    let (active, editing) = (filter.active(), filter.editing);
                    p.saw(0, active && !editing) && !active
                },
            ),
            fresh(
                "Split diff",
                "v opens the diff in the reader; s splits it, s again unifies it.",
                tree,
                |p| p.saw(0, p.screen.split_diff) && !p.screen.split_diff,
            ),
        ],
    },
    Chapter {
        name: "Staging",
        lessons: &[
            fresh(
                "Stage a file",
                "Select notes.md and press Space to stage the whole file.",
                two_files,
                |p| p.staged().iter().any(|path| path == "notes.md"),
            ),
            fresh(
                "Stage all",
                "a stages every change; a again unstages them all.",
                three_files,
                |p| {
                    let (staged, unstaged) = (p.staged().len(), p.unstaged().len());
                    p.saw(0, staged == 3 && unstaged == 0) && staged == 0
                },
            ),
            fresh(
                "Mark files",
                "x marks a file (V marks a run). Mark two, then Space stages them.",
                three_files,
                |p| {
                    let marked = p.screen.file_selection.marked.len();
                    p.saw(0, marked >= 2) && p.staged().len() == 2 && p.unstaged().len() == 1
                },
            ),
            fresh(
                "Stage a hunk",
                "On story.txt, v opens the diff; j to a change, Space stages its hunk.",
                two_hunks,
                |p| p.lines(true, "story.txt") > 0 && p.lines(false, "story.txt") > 0,
            ),
            fresh(
                "Stage a line",
                "On list.txt, v, then j to an added line; a stages just that line.",
                three_lines,
                |p| p.lines(true, "list.txt") == 1 && p.lines(false, "list.txt") > 0,
            ),
            fresh(
                "Stage a range",
                "On list.txt, v; V starts a range on a + line, j extends, a stages it.",
                four_lines,
                |p| p.lines(true, "list.txt") >= 2 && p.lines(false, "list.txt") > 0,
            ),
        ],
    },
    Chapter {
        name: "Commit",
        lessons: &[
            fresh(
                "Subject and body",
                "c opens the sheet: a subject, Enter, a body line, Ctrl+Enter commits.",
                staged,
                |p| {
                    p.moved()
                        && p.message()
                            .split_once("\n\n")
                            .is_some_and(|(_, body)| !body.trim().is_empty())
                }
            ),
            fresh(
                "Sign-off",
                "c, a subject, then Alt+s adds Signed-off-by; Ctrl+Enter commits.",
                staged,
                |p| p.moved() && p.message().contains("Signed-off-by:"),
            ),
            fresh(
                "Skip hooks",
                "A hook blocks commits here. c, a subject, Alt+n skips hooks, Ctrl+Enter.",
                hooked,
                |p| p.moved(),
            ),
            fresh(
                "Amend",
                "A file missed the last commit. c, Alt+a amends HEAD, Ctrl+Enter.",
                forgot,
                |p| p.amended() && p.staged().is_empty(),
            ),
        ],
    },
    Chapter {
        name: "History",
        lessons: &[
            fresh(
                "Commit files",
                "4 History, Enter on a commit lists its files; Esc goes back.",
                panes,
                |p| p.saw(0, p.screen.commit_open) && !p.screen.commit_open,
            ),
            fresh(
                "Amend HEAD",
                "A amends HEAD with the staged file; Ctrl+Enter confirms.",
                forgot,
                |p| p.amended() && p.staged().is_empty(),
            ),
            fresh(
                "Reword",
                "W rewords HEAD: fix the typo in 'Add greting', then Ctrl+Enter.",
                typo,
                |p| p.amended() && p.same_tree() && !p.subject(0).contains("greting"),
            ),
            fresh(
                "Fixup",
                "4, select 'Add greeting'; F records the staged fix for it; Enter.",
                fixup,
                |p| {
                    p.moved()
                        && p.subject(0).starts_with("fixup! Add greeting")
                        && p.commit(1).map(|commit| commit.id()) == p.start
                },
            ),
        ],
    },
    Chapter {
        name: "Branches",
        lessons: &[
            fresh(
                "New branch",
                "3 Branches, n, type a name, then Ctrl+Enter creates it.",
                plain,
                |p| p.branches() >= 2,
            ),
            fresh(
                "Checkout",
                "3, select feature, Enter, then Enter again to switch to it.",
                feature,
                |p| p.on_branch("feature"),
            ),
            fresh(
                "Rename",
                "3, select old-name, e, type a new name, Ctrl+Enter.",
                old_name,
                |p| p.branch("old-name").is_none() && p.branches() == 2,
            ),
            fresh(
                "Upstream",
                "3, select feature, u; origin/feature is picked (Up/Down); Ctrl+Enter.",
                pushed_feature,
                |p| {
                    p.repo
                        .find_branch("feature", git2::BranchType::Local)
                        .is_ok_and(|branch| branch.upstream().is_ok())
                },
            ),
            fresh(
                "Delete",
                "3, select done-topic, d, then Enter deletes it.",
                done_topic,
                |p| p.branch("done-topic").is_none(),
            ),
        ],
    },
    Chapter {
        name: "Merge",
        lessons: &[
            fresh(
                "Merge",
                "3, select topic, m, then Enter merges it. It will conflict.",
                diverged,
                |p| p.repo.state() == RepositoryState::Merge,
            ),
            carried(
                "Resolve",
                "2, select src/greet.rs; o or t keeps a side, Ctrl+Enter; Space stages.",
                merging,
                |p| p.repo.state() == RepositoryState::Merge && !p.conflicted(),
            ),
            carried(
                "Continue",
                "C, then Enter records the merge commit.",
                resolved,
                |p| p.clean() && p.commit(0).is_some_and(|commit| commit.parent_count() == 2),
            ),
        ],
    },
    Chapter {
        name: "Rebase",
        lessons: &[
            fresh(
                "Rebase",
                "3, select main, R, then Enter replays feature onto it. It stops.",
                rebase_ready,
                |p| p.rebasing(),
            ),
            carried(
                "Continue",
                "2, greet.txt: t keeps the commit, Ctrl+Enter, Space, then C, Enter.",
                rebase_stopped,
                |p| {
                    let subjects = p.subjects();
                    subjects.iter().any(|subject| subject == "Polish greet and farewell")
                        && subjects.iter().any(|subject| subject == "Shout the greeting")
                },
            ),
            carried(
                "Skip",
                "This commit conflicts too. S, then Enter skips it; the rest replays.",
                rebase_second,
                |p| {
                    let subjects = p.subjects();
                    let has = |name: &str| subjects.iter().any(|subject| subject == name);
                    p.clean()
                        && has("Polish greet and farewell")
                        && has("Shout the greeting")
                        && has("Add rebase notes")
                        && !has("Wave goodbye")
                },
            ),
            fresh(
                "Abort",
                "A rebase stopped here. X, then Enter aborts and restores feature.",
                rebase_stopped,
                |p| p.clean() && p.on_branch("feature") && p.head() == p.mark,
            ),
        ],
    },
    Chapter {
        name: "Reflog",
        lessons: &[fresh(
            "Rescue",
            "4, ] Reflog, select 'Precious work', n, a name, Ctrl+Enter.",
            lost,
            |p| {
                p.mark.is_some()
                    && p.repo.branches(Some(git2::BranchType::Local)).is_ok_and(|mut branches| {
                        branches.any(|branch| {
                            branch.is_ok_and(|(branch, _)| branch.get().target() == p.mark)
                        })
                    })
            },
        )],
    },
    Chapter {
        name: "Stash",
        lessons: &[
            fresh(
                "Save",
                "5 Stashes, n, a message, Ctrl+Enter puts your edits away.",
                dirty,
                |p| p.stashes() >= 1 && p.unstaged().is_empty(),
            ),
            carried(
                "Pop",
                "5, select the stash, p, then Enter restores and removes it.",
                stashed,
                |p| p.stashes() == 0 && p.screen.root.join("draft.md").is_file(),
            ),
            fresh(
                "Apply",
                "5, a, then Enter applies the selected stash and keeps it.",
                two_stashes,
                |p| p.stashes() == 2 && !p.unstaged().is_empty(),
            ),
            carried(
                "Drop",
                "5, d, then Enter drops the selected stash.",
                two_stashes,
                |p| p.stashes() == 1,
            ),
        ],
    },
    Chapter {
        name: "Worktrees",
        lessons: &[
            fresh(
                "New worktree",
                "2, ] Worktrees, n, Branch: review, keep the path, Ctrl+Enter.",
                plain,
                |p| p.worktrees() >= 1,
            ),
            carried(
                "Open",
                "2, ] Worktrees, select review, Enter opens it here.",
                review,
                |p| p.repo.is_worktree(),
            ),
            carried(
                "Remove",
                "2, ] Worktrees: Enter on practice, then d on review, Enter.",
                review,
                |p| !p.repo.is_worktree() && p.worktrees() == 0,
            ),
        ],
    },
    Chapter {
        name: "Submodules",
        lessons: &[
            fresh(
                "Init",
                "2, ] ] Submodules, select lib, u, then Enter checks it out.",
                submodule_off,
                |p| p.screen.root.join("lib/.git").exists() && p.screen.root.join("lib/lib.txt").is_file(),
            ),
            fresh(
                "Stage pointer",
                "lib has a new commit. 2, ] ] Submodules, Space stages its pointer.",
                submodule_moved,
                |p| {
                    let staged = p
                        .repo
                        .index()
                        .ok()
                        .and_then(|index| index.get_path(Path::new("lib"), 0).map(|entry| entry.id));
                    let committed = p
                        .repo
                        .head()
                        .and_then(|head| head.peel_to_tree())
                        .and_then(|tree| tree.get_path(Path::new("lib")))
                        .ok()
                        .map(|entry| entry.id());
                    staged.is_some() && staged != committed
                },
            ),
        ],
    },
    Chapter {
        name: "Tags and remotes",
        lessons: &[
            fresh(
                "Delete a tag",
                "3, ] ] Tags, select v0.1, d, then Enter deletes the local tag.",
                tagged,
                |p| p.repo.find_reference("refs/tags/v0.1").is_err(),
            ),
            fresh(
                "Fetch",
                "3, ] Remotes, select origin, f, then Enter fetches.",
                teammate,
                |p| {
                    p.mark.is_some()
                        && p.repo.refname_to_id("refs/remotes/origin/practice").ok() == p.mark
                },
            ),
            fresh(
                "Push",
                "P, then Enter pushes your commit to the practice remote.",
                ahead,
                |p| {
                    p.head().is_some()
                        && Repository::open_bare(p.remote)
                            .and_then(|remote| remote.refname_to_id("refs/heads/practice"))
                            .ok()
                            == p.head()
                },
            ),
        ],
    },
    Chapter {
        name: "Discard",
        lessons: &[fresh(
            "Discard a hunk",
            "v opens src/greet.rs, j to the debug line, d asks; Enter discards.",
            unwanted,
            |p| p.screen.root.join(GREET).is_file() && p.lines(false, GREET) == 0,
        )],
    },
    Chapter {
        name: "Help",
        lessons: &[fresh(
            "Keys",
            "? lists the keys of the focused pane; Esc closes it.",
            plain,
            |p| p.saw(0, p.screen.help) && !p.screen.help,
        )],
    },
];

/// EXTENSION POINT: forge chapters (Pull requests, Issues, Actions).
///
/// They run after the Git chapters; add them here once `src/forge` (FORGE.md) can
/// run against a fake forge server. A forge lesson's `setup` would start that server
/// inside the lesson directory and point the practice remote at it; its `done` would
/// read the provider's state through `p.screen`. Also lift the `<`/`>` refusal in
/// `GitScreen::event` ("Use :tutor quit before opening GitHub pages") for them.
static FORGE: &[Chapter] = &[];

fn track() -> impl Iterator<Item = (&'static Chapter, &'static Lesson)> {
    GIT.iter()
        .chain(FORGE)
        .flat_map(|chapter| chapter.lessons.iter().map(move |lesson| (chapter, lesson)))
}
fn total() -> usize {
    track().count()
}
fn lesson_at(index: usize) -> Option<(&'static Chapter, &'static Lesson)> {
    track().nth(index)
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Runs Git for a fixture with the tutor's identity, no signing and no user hooks.
fn run(directory: &Path, hooks: &Path, args: &[&str], check: bool) -> Result<String, String> {
    let output = Command::new("git")
        .env_clear()
        .envs(crate::git::command::environment())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .arg("-C")
        .arg(directory)
        .arg("-c")
        .arg(format!("user.name={NAME}"))
        .arg("-c")
        .arg(format!("user.email={EMAIL}"))
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.templateDir=", "-c", "advice.detachedHead=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("git: {error}"))?;
    if check && !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// One lesson's disposable directory: `practice/` (the screen's repository),
/// `remote.git` (its `origin`) and an empty `hooks/` directory.
struct Fixture {
    dir: PathBuf,
    root: PathBuf,
    remote: PathBuf,
    hooks: PathBuf,
}
impl Fixture {
    fn new(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(text)?;
        let dir = dir.canonicalize().map_err(text)?;
        let fixture = Self {
            root: dir.join("practice"),
            remote: dir.join("remote.git"),
            hooks: dir.join("hooks"),
            dir,
        };
        std::fs::create_dir(&fixture.hooks).map_err(text)?;
        let init = |args: &[&str]| run(&fixture.dir, &fixture.hooks, args, true);
        init(&["init", "--quiet", "--bare", "-b", "practice", "remote.git"])?;
        init(&["init", "--quiet", "-b", "practice", "practice"])?;
        let hooks = fixture.hooks.to_string_lossy().into_owned();
        for (key, value) in [
            ("user.name", NAME),
            ("user.email", EMAIL),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.hooksPath", hooks.as_str()),
            ("core.fsmonitor", "false"),
            ("core.autocrlf", "false"),
            ("push.default", "simple"),
            ("submodule.recurse", "false"),
        ] {
            fixture.git(&["config", key, value])?;
        }
        fixture.write(
            "README.md",
            "# Dusty Git practice\n\nThis repository and its remote are disposable.\n",
        )?;
        fixture.write(GREET, &greeting("Hello"))?;
        fixture.commit("Start Git practice")?;
        let remote = fixture.remote.to_string_lossy().into_owned();
        fixture.git(&["remote", "add", "origin", &remote])?;
        fixture.git(&["push", "--quiet", "-u", "origin", "practice"])?;
        Ok(fixture)
    }
    fn git(&self, args: &[&str]) -> Result<String, String> {
        run(&self.root, &self.hooks, args, true)
    }
    /// A step expected to stop (a conflicting merge or rebase).
    fn attempt(&self, args: &[&str]) -> Result<(), String> {
        run(&self.root, &self.hooks, args, false).map(drop)
    }
    fn write(&self, path: &str, source: &str) -> Result<(), String> {
        let path = self.root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(text)?;
        }
        std::fs::write(path, source).map_err(text)
    }
    fn commit(&self, message: &str) -> Result<Oid, String> {
        self.git(&["add", "-A"])?;
        self.git(&["commit", "--quiet", "-m", message])?;
        self.head()
    }
    fn head(&self) -> Result<Oid, String> {
        Oid::from_str(&self.git(&["rev-parse", "HEAD"])?).map_err(text)
    }
}

fn greeting(word: &str) -> String {
    format!("pub fn greet() -> &'static str {{\n    \"{word}\"\n}}\n")
}

fn plain(_: &Fixture) -> Setup {
    Ok(None)
}
/// Something in every pane: history, a second branch, a tag, a stash and a change.
fn panes(f: &Fixture) -> Setup {
    f.write("docs/guide.md", "# Guide\n\nRead me first.\n")?;
    f.commit("Add a guide")?;
    f.git(&["branch", "feature"])?;
    f.git(&["tag", "v0.1"])?;
    f.write("notes.md", "A stashed note\n")?;
    f.git(&["stash", "push", "--quiet", "--include-untracked", "-m", "Practice stash"])?;
    f.write(GREET, &greeting("Hello, Dusty Git!"))?;
    Ok(None)
}
fn tree(f: &Fixture) -> Setup {
    f.write("src/app/main.rs", "fn main() {}\n")?;
    f.write("src/app/view.rs", "pub fn view() {}\n")?;
    f.write("docs/guide.md", "# Guide\n")?;
    f.commit("Add the app")?;
    f.write("src/app/main.rs", "fn main() {\n    println!(\"practice\");\n}\n")?;
    f.write("src/app/view.rs", "pub fn view() -> u8 {\n    1\n}\n")?;
    f.write("docs/guide.md", "# Guide\n\nTry the tree view.\n")?;
    f.write(GREET, &greeting("Hello, tree!"))?;
    Ok(None)
}
fn two_files(f: &Fixture) -> Setup {
    f.write("notes.md", "Notes\n")?;
    f.commit("Add notes")?;
    f.write("notes.md", "Notes\n\nStage me.\n")?;
    f.write(GREET, &greeting("Leave me unstaged"))?;
    Ok(None)
}
fn three_files(f: &Fixture) -> Setup {
    for name in ["a.txt", "b.txt", "c.txt"] {
        f.write(name, "one\n")?;
    }
    f.commit("Add three files")?;
    for name in ["a.txt", "b.txt", "c.txt"] {
        f.write(name, "one\ntwo\n")?;
    }
    Ok(None)
}
fn two_hunks(f: &Fixture) -> Setup {
    let mut lines = (1..=30)
        .map(|line| format!("Line {line} of the story"))
        .collect::<Vec<_>>();
    f.write("story.txt", &(lines.join("\n") + "\n"))?;
    f.commit("Add a story")?;
    lines[2] = "Line 3 was rewritten".into();
    lines[26] = "Line 27 was rewritten".into();
    f.write("story.txt", &(lines.join("\n") + "\n"))?;
    Ok(None)
}
fn list(f: &Fixture, added: &[&str]) -> Setup {
    f.write("list.txt", "alpha\nomega\n")?;
    f.commit("Add a list")?;
    f.write("list.txt", &format!("alpha\n{}\nomega\n", added.join("\n")))?;
    Ok(None)
}
fn three_lines(f: &Fixture) -> Setup {
    list(f, &["beta", "gamma", "delta"])
}
fn four_lines(f: &Fixture) -> Setup {
    list(f, &["beta", "gamma", "delta", "epsilon"])
}
fn staged(f: &Fixture) -> Setup {
    f.write(GREET, &greeting("Hello, commit!"))?;
    f.git(&["add", GREET])?;
    Ok(None)
}
fn hooked(f: &Fixture) -> Setup {
    staged(f)?;
    let hook = f.hooks.join("pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'Practice hook: this commit is blocked' >&2\nexit 1\n",
    )
    .map_err(text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).map_err(text)?;
    }
    Ok(None)
}
fn forgot(f: &Fixture) -> Setup {
    f.write("src/farewell.rs", "pub fn farewell() {}\n")?;
    f.commit("Add farewell")?;
    f.write("docs/farewell.md", "Say goodbye politely.\n")?;
    f.git(&["add", "docs/farewell.md"])?;
    Ok(None)
}
fn typo(f: &Fixture) -> Setup {
    f.write("greeting.txt", "Hello\n")?;
    f.commit("Add greting")?;
    Ok(None)
}
fn fixup(f: &Fixture) -> Setup {
    f.write("greeting.txt", "Helo\n")?;
    f.commit("Add greeting")?;
    f.write("docs.md", "Docs\n")?;
    f.commit("Add docs")?;
    f.write("greeting.txt", "Hello\n")?;
    f.git(&["add", "greeting.txt"])?;
    Ok(None)
}
fn feature(f: &Fixture) -> Setup {
    f.git(&["branch", "feature"])?;
    Ok(None)
}
fn old_name(f: &Fixture) -> Setup {
    f.git(&["branch", "old-name"])?;
    Ok(None)
}
fn pushed_feature(f: &Fixture) -> Setup {
    f.git(&["branch", "feature"])?;
    f.git(&["push", "--quiet", "origin", "feature"])?;
    Ok(None)
}
fn done_topic(f: &Fixture) -> Setup {
    f.git(&["branch", "done-topic"])?;
    Ok(None)
}
fn diverged(f: &Fixture) -> Setup {
    f.git(&["switch", "--quiet", "-c", "topic"])?;
    f.write(GREET, &greeting("Hello from topic"))?;
    f.commit("Greet from topic")?;
    f.git(&["switch", "--quiet", "practice"])?;
    f.write(GREET, &greeting("Hello from practice"))?;
    f.commit("Greet from practice")?;
    Ok(None)
}
fn merging(f: &Fixture) -> Setup {
    diverged(f)?;
    f.attempt(&["merge", "--no-edit", "topic"])?;
    Ok(None)
}
fn resolved(f: &Fixture) -> Setup {
    merging(f)?;
    f.git(&["checkout", "--theirs", "--", GREET])?;
    f.git(&["add", GREET])?;
    Ok(None)
}
/// `feature` (checked out) replays three commits; the first two conflict with `main`.
fn rebase_ready(f: &Fixture) -> Setup {
    f.write("greet.txt", "Hello\n")?;
    f.write("farewell.txt", "Bye\n")?;
    f.commit("Add greet and farewell")?;
    f.git(&["switch", "--quiet", "-c", "feature"])?;
    f.write("greet.txt", "HELLO\n")?;
    f.commit("Shout the greeting")?;
    f.write("farewell.txt", "Bye, waving\n")?;
    f.commit("Wave goodbye")?;
    f.write("notes.txt", "Rebase practice\n")?;
    let tip = f.commit("Add rebase notes")?;
    f.git(&["switch", "--quiet", "-c", "main", "practice"])?;
    f.write("greet.txt", "Hello from main\n")?;
    f.write("farewell.txt", "Bye from main\n")?;
    f.commit("Polish greet and farewell")?;
    f.git(&["switch", "--quiet", "feature"])?;
    Ok(Some(tip))
}
fn rebase_stopped(f: &Fixture) -> Setup {
    let tip = rebase_ready(f)?;
    f.attempt(&["rebase", "main"])?;
    Ok(tip)
}
fn rebase_second(f: &Fixture) -> Setup {
    let tip = rebase_stopped(f)?;
    f.git(&["checkout", "--theirs", "--", "greet.txt"])?;
    f.git(&["add", "greet.txt"])?;
    f.attempt(&["rebase", "--continue"])?;
    Ok(tip)
}
fn lost(f: &Fixture) -> Setup {
    f.write("precious.txt", "Do not lose me\n")?;
    let lost = f.commit("Precious work")?;
    f.git(&["reset", "--quiet", "--hard", "HEAD~1"])?;
    Ok(Some(lost))
}
fn dirty(f: &Fixture) -> Setup {
    f.write(GREET, &greeting("Hello, stash!"))?;
    f.write("draft.md", "An untracked draft\n")?;
    Ok(None)
}
fn stashed(f: &Fixture) -> Setup {
    dirty(f)?;
    f.git(&["stash", "push", "--quiet", "--include-untracked", "-m", "Practice stash"])?;
    Ok(None)
}
fn two_stashes(f: &Fixture) -> Setup {
    f.write(GREET, &greeting("Hello, first stash"))?;
    f.git(&["stash", "push", "--quiet", "-m", "Greeting tweak"])?;
    f.write("idea.md", "An idea\n")?;
    f.git(&["stash", "push", "--quiet", "--include-untracked", "-m", "Idea"])?;
    Ok(None)
}
fn review(f: &Fixture) -> Setup {
    f.git(&["worktree", "add", "--quiet", "-b", "review", "../review"])?;
    Ok(None)
}
/// A committed `lib` submodule cloned from `library/` beside the practice repository.
fn library(f: &Fixture) -> Result<(), String> {
    let library = f.dir.join("library");
    run(&f.dir, &f.hooks, &["init", "--quiet", "-b", "main", "library"], true)?;
    std::fs::write(library.join("lib.txt"), "Library v1\n").map_err(text)?;
    run(&library, &f.hooks, &["add", "-A"], true)?;
    run(&library, &f.hooks, &["commit", "--quiet", "-m", "Library v1"], true)?;
    // File transport only for this local, disposable clone.
    let source = library.to_string_lossy().into_owned();
    f.git(&["-c", "protocol.file.allow=always", "submodule", "--quiet", "add", &source, "lib"])?;
    f.commit("Add the lib submodule")?;
    Ok(())
}
/// Deinitialized, but its Git directory stays in `.git/modules`, so `u` needs no clone.
fn submodule_off(f: &Fixture) -> Setup {
    library(f)?;
    f.git(&["submodule", "--quiet", "deinit", "--force", "--", "lib"])?;
    Ok(None)
}
fn submodule_moved(f: &Fixture) -> Setup {
    library(f)?;
    let lib = f.root.join("lib");
    std::fs::write(lib.join("lib.txt"), "Library v2\n").map_err(text)?;
    run(&lib, &f.hooks, &["commit", "--quiet", "-am", "Library v2"], true)?;
    Ok(None)
}
fn tagged(f: &Fixture) -> Setup {
    f.git(&["tag", "v0.1"])?;
    Ok(None)
}
/// The remote is one commit ahead of what this clone has fetched.
fn teammate(f: &Fixture) -> Setup {
    let base = f.head()?.to_string();
    f.write("team.md", "A teammate's change\n")?;
    let shared = f.commit("Teammate's change")?;
    f.git(&["push", "--quiet", "origin", "practice"])?;
    f.git(&["reset", "--quiet", "--hard", &base])?;
    f.git(&["update-ref", "refs/remotes/origin/practice", &base])?;
    Ok(Some(shared))
}
fn ahead(f: &Fixture) -> Setup {
    f.write("share.md", "Ready to share\n")?;
    f.commit("Ready to share")?;
    Ok(None)
}
fn unwanted(f: &Fixture) -> Setup {
    f.write(
        GREET,
        "pub fn greet() -> &'static str {\n    // debug: remove me\n    \"Hello\"\n}\n",
    )?;
    Ok(None)
}

/// What a lesson's check can see: the screen, its repository and the lesson's marks.
struct Probe<'a> {
    screen: &'a GitScreen,
    repo: Repository,
    remote: &'a Path,
    /// HEAD when the lesson began.
    start: Option<Oid>,
    /// The commit the lesson's setup returned.
    mark: Option<Oid>,
    seen: &'a mut u32,
}
impl Probe<'_> {
    /// Remembers that a screen state happened during this lesson.
    fn saw(&mut self, bit: u32, now: bool) -> bool {
        if now {
            *self.seen |= 1 << bit;
        }
        *self.seen & (1 << bit) != 0
    }
    fn head(&self) -> Option<Oid> {
        self.repo.head().ok()?.target()
    }
    fn moved(&self) -> bool {
        self.head().is_some() && self.head() != self.start
    }
    fn commit(&self, back: usize) -> Option<git2::Commit<'_>> {
        let mut commit = self.repo.head().ok()?.peel_to_commit().ok()?;
        for _ in 0..back {
            commit = commit.parent(0).ok()?;
        }
        Some(commit)
    }
    fn subject(&self, back: usize) -> String {
        self.commit(back)
            .and_then(|commit| commit.summary().map(Into::into))
            .unwrap_or_default()
    }
    fn message(&self) -> String {
        self.commit(0)
            .and_then(|commit| commit.message().map(Into::into))
            .unwrap_or_default()
    }
    /// First-parent subjects from HEAD.
    fn subjects(&self) -> Vec<String> {
        let mut subjects = Vec::new();
        let mut commit = self.commit(0);
        while let Some(current) = commit.filter(|_| subjects.len() < 50) {
            subjects.push(current.summary().unwrap_or_default().to_owned());
            commit = current.parent(0).ok();
        }
        subjects
    }
    /// HEAD was replaced by a commit on the same parent.
    fn amended(&self) -> bool {
        let parent = |id: Oid| self.repo.find_commit(id).ok()?.parent_id(0).ok();
        self.moved()
            && self.start.and_then(parent).is_some()
            && self.head().and_then(parent) == self.start.and_then(parent)
    }
    fn same_tree(&self) -> bool {
        let tree = |id: Oid| self.repo.find_commit(id).ok().map(|commit| commit.tree_id());
        self.head().and_then(tree) == self.start.and_then(tree)
    }
    fn branch(&self, name: &str) -> Option<Oid> {
        self.repo
            .find_reference(&format!("refs/heads/{name}"))
            .ok()?
            .target()
    }
    fn branches(&self) -> usize {
        self.repo
            .branches(Some(git2::BranchType::Local))
            .map_or(0, Iterator::count)
    }
    fn on_branch(&self, name: &str) -> bool {
        self.repo
            .head()
            .is_ok_and(|head| head.name() == Some(format!("refs/heads/{name}").as_str()))
    }
    fn clean(&self) -> bool {
        self.repo.state() == RepositoryState::Clean
    }
    fn rebasing(&self) -> bool {
        matches!(
            self.repo.state(),
            RepositoryState::Rebase
                | RepositoryState::RebaseInteractive
                | RepositoryState::RebaseMerge
                | RepositoryState::ApplyMailboxOrRebase
        )
    }
    fn conflicted(&self) -> bool {
        self.repo.index().is_ok_and(|index| index.has_conflicts())
    }
    fn stashes(&mut self) -> usize {
        let mut count = 0;
        let _ = self.repo.stash_foreach(|_, _, _| {
            count += 1;
            true
        });
        count
    }
    fn worktrees(&self) -> usize {
        self.repo.worktrees().map_or(0, |names| names.len())
    }
    /// Changed paths and their changed-line counts, staged (HEAD → index) or not
    /// (index → working tree, untracked files included).
    fn changes(&self, staged: bool) -> Vec<(String, usize)> {
        let mut options = git2::DiffOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let diff = if staged {
            let tree = self.repo.head().and_then(|head| head.peel_to_tree()).ok();
            self.repo
                .diff_tree_to_index(tree.as_ref(), None, Some(&mut options))
        } else {
            self.repo.diff_index_to_workdir(None, Some(&mut options))
        };
        let Ok(diff) = diff else {
            return Vec::new();
        };
        (0..diff.deltas().len())
            .filter_map(|index| {
                let delta = diff.get_delta(index)?;
                let path = delta.new_file().path()?.to_string_lossy().into_owned();
                let lines = git2::Patch::from_diff(&diff, index)
                    .ok()
                    .flatten()
                    .and_then(|patch| patch.line_stats().ok())
                    .map_or(0, |(_, added, removed)| added + removed);
                Some((path, lines))
            })
            .collect()
    }
    fn staged(&self) -> Vec<String> {
        self.changes(true).into_iter().map(|(path, _)| path).collect()
    }
    fn unstaged(&self) -> Vec<String> {
        self.changes(false).into_iter().map(|(path, _)| path).collect()
    }
    fn lines(&self, staged: bool, path: &str) -> usize {
        self.changes(staged)
            .into_iter()
            .find(|(changed, _)| changed == path)
            .map_or(0, |(_, lines)| lines)
    }
}

fn head_of(root: &Path) -> Option<Oid> {
    Repository::open(root).ok()?.head().ok()?.target()
}

/// Builds lesson `index` in its own new directory and opens a screen on it.
fn build(base: &Path, index: usize, attempt: usize) -> Result<(Fixture, Option<Oid>, GitScreen), String> {
    let (_, lesson) = lesson_at(index).ok_or("No such lesson")?;
    let dir = base.join(format!("{:02}-{attempt}", index + 1));
    let built = Fixture::new(&dir).and_then(|fixture| {
        let mark = (lesson.setup)(&fixture)?;
        let screen = GitScreen::open(&fixture.root)?;
        Ok((fixture, mark, screen))
    });
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    built
}

pub(super) struct Session {
    directory: TempDir,
    /// The canonical TempDir path every practice path must stay inside.
    base: PathBuf,
    saved: Box<GitScreen>,
    /// Index into the lesson track; the track's length means every lesson is done.
    lesson: usize,
    attempt: usize,
    fixture: Fixture,
    start: Option<Oid>,
    mark: Option<Oid>,
    seen: u32,
    failed: bool,
}
impl Session {
    pub fn title(&self) -> String {
        let total = total();
        match lesson_at(self.lesson) {
            None => format!("Git tutor · all {total} lessons done"),
            Some(_) if self.failed => {
                format!("Git tutor · {}/{total} · setup failed", self.lesson + 1)
            }
            Some((chapter, lesson)) => format!(
                "Git tutor · {}/{total} · {} · {}",
                self.lesson + 1,
                chapter.name,
                lesson.title
            ),
        }
    }
    pub fn instruction(&self) -> &'static str {
        match lesson_at(self.lesson) {
            None => "You practiced every Git lesson. q returns to your workspace.",
            Some(_) if self.failed => {
                "Practice setup failed (see the notice). :tutor reset retries; :tutor next skips."
            }
            Some((_, lesson)) => lesson.instruction,
        }
    }
    pub fn hint(&self) -> &'static str {
        "Practice repo · :tutor next skips · :tutor prev goes back · :tutor quit"
    }
    pub fn check_destination(&self, path: &Path) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or("Choose a practice worktree path")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if parent.starts_with(&self.base) && path.file_name().is_some() {
            Ok(())
        } else {
            Err("Tutor worktrees must stay inside the disposable practice directory".into())
        }
    }
}

impl GitScreen {
    pub fn in_tutor(&self) -> bool {
        self.tutor.is_some()
    }
    pub fn resume(&mut self) {
        let result = self.refresh();
        self.report(result);
    }
    /// `:tutor …` commands.
    pub(super) fn tutor_command(&mut self, command: &str) -> Result<(), String> {
        // The command sheet closes either way; a refusal is reported as a notice.
        self.form = None;
        let words = command.split_whitespace().collect::<Vec<_>>();
        let number = |word: &str| {
            word.parse::<usize>()
                .ok()
                .filter(|number| (1..=total()).contains(number))
                .map(|number| number - 1)
        };
        let index = match words.as_slice() {
            ["tutor"] | ["tutor", "git"] => return self.start_tutor(0),
            ["tutor", "git", lesson] => {
                return self.start_tutor(number(*lesson).ok_or(USAGE)?);
            }
            ["tutor", "quit"] => return self.stop_tutor(),
            ["tutor", word] if self.tutor.is_none() => {
                return match number(*word) {
                    Some(index) => self.start_tutor(index),
                    None => Err(NOT_RUNNING.into()),
                };
            }
            ["tutor", word] => {
                let current = self.tutor.as_ref().map_or(0, |session| session.lesson);
                match *word {
                    "next" | "skip" => (current + 1).min(total()),
                    "prev" | "back" => current.saturating_sub(1),
                    "reset" | "again" => current,
                    other => number(other).ok_or(USAGE)?,
                }
            }
            _ => return Err(USAGE.into()),
        };
        if self.busy() {
            return Err("Wait for the practice operation before changing lessons".into());
        }
        self.enter_lesson(index, false)
    }
    fn start_tutor(&mut self, index: usize) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the current operation before starting the tutor".into());
        }
        if self.tutor.is_some() {
            return Err("The Git tutor is already running; :tutor 1 starts it over".into());
        }
        let directory = tempfile::Builder::new()
            .prefix("dusty-git-tutor-")
            .tempdir()
            .map_err(text)?;
        let base = directory.path().canonicalize().map_err(text)?;
        let (fixture, mark, mut next) = build(&base, index, 1)?;
        next.icon_mode = self.icon_mode;
        self.form = None;
        let saved = Box::new(std::mem::replace(self, next));
        self.tutor = Some(Session {
            directory,
            base,
            saved,
            lesson: index,
            attempt: 1,
            start: head_of(&fixture.root),
            fixture,
            mark,
            seen: 0,
            failed: false,
        });
        Ok(())
    }
    pub(super) fn stop_tutor(&mut self) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the practice operation before leaving the tutor".into());
        }
        let session = self.tutor.take().ok_or(NOT_RUNNING)?;
        let Session {
            directory, saved, ..
        } = session;
        *self = *saved;
        drop(directory);
        Ok(())
    }
    /// Moves to lesson `index`. Finishing the previous lesson (`carried`) keeps its
    /// repository for a lesson that continues it; everything else builds afresh.
    fn enter_lesson(&mut self, index: usize, carried: bool) -> Result<(), String> {
        let session = self.tutor.as_mut().ok_or(NOT_RUNNING)?;
        session.seen = 0;
        let continues = lesson_at(index).is_none_or(|(_, lesson)| carried && lesson.carry);
        if continues {
            session.lesson = index.min(total());
            session.failed = false;
            session.start = head_of(&self.root);
            return Ok(());
        }
        session.attempt += 1;
        match build(&session.base, index, session.attempt) {
            Ok((fixture, mark, mut next)) => {
                let mut session = self.tutor.take().ok_or(NOT_RUNNING)?;
                session.lesson = index;
                session.failed = false;
                session.start = head_of(&fixture.root);
                session.mark = mark;
                let previous = std::mem::replace(&mut session.fixture, fixture);
                next.icon_mode = self.icon_mode;
                next.tutor = Some(session);
                drop(std::mem::replace(self, next));
                // The finished lesson's repositories (and worktrees) go with it.
                let _ = std::fs::remove_dir_all(&previous.dir);
                Ok(())
            }
            Err(error) => {
                session.lesson = index;
                session.failed = true;
                Err(format!("Tutor: {error}"))
            }
        }
    }
    /// Checks the current lesson against the real state; moves on when it is done.
    pub(super) fn advance_tutor(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        let Some(mut session) = self.tutor.take() else {
            return false;
        };
        let lesson = lesson_at(session.lesson).filter(|_| !session.failed);
        let done = match (lesson, Repository::open(&self.root)) {
            (Some((_, lesson)), Ok(repo)) => (lesson.done)(&mut Probe {
                screen: self,
                repo,
                remote: &session.fixture.remote,
                start: session.start,
                mark: session.mark,
                seen: &mut session.seen,
            }),
            _ => false,
        };
        let next = session.lesson + 1;
        self.tutor = Some(session);
        if let Some((_, lesson)) = lesson.filter(|_| done) {
            // A sheet still open from the finished lesson describes state that has moved on.
            // Operation sheets close themselves (a continue that stops on the next conflict
            // included); this only guards sheets the learner opened along the way.
            self.form = None;
            let result = self.enter_lesson(next, true);
            self.notice = match result {
                Ok(()) => format!("✓ {} done", lesson.title),
                Err(error) => error,
            };
        }
        done
    }
    pub(super) fn tutor_worktree(&mut self, path: &Path) -> Result<(), String> {
        let session = self.tutor.as_ref().ok_or(NOT_RUNNING)?;
        if !path
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(&session.base)
        {
            return Err("Tutor navigation stays inside the practice directory".into());
        }
        let mut next = GitScreen::open(path)?;
        next.icon_mode = self.icon_mode;
        next.tutor = self.tutor.take();
        *self = next;
        self.advance_tutor();
        Ok(())
    }
}
