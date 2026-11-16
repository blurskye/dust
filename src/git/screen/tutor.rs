//! The Git tutor: hands-on lessons, each on its own disposable practice repository.
//!
//! Every lesson builds its fixture (a practice repository with a local bare remote)
//! in a fresh directory under the session's TempDir and reopens the screen there, so
//! lessons can be skipped, repeated or revisited in any order. A lesson ends only when
//! the real repository or screen state shows the user did the thing. The user's own
//! screen is saved whole and restored by `:tutor quit`; nothing touches their repos.
//!
//! The forge track (`:tutor forge`) teaches the forge tabs the same way, against a fake
//! forge on loopback (`tutor/forge.rs`) with a practice token: nothing reaches a real forge
//! or reads the user's credentials.
//!
//! Commands: `:tutor git [N]`, `:tutor forge [N]`, `:tutor next`, `:tutor prev`,
//! `:tutor reset`, `:tutor N` and `:tutor quit`.
use super::*;
use crate::forge::sandbox;
use crate::ui::{CellGrid, Emphasis, GitRole, GitSurface, Style};
use forge::{Forge, Token, World};
use git2::{Oid, Repository, RepositoryState};
use std::process::{Command, Stdio};
use tempfile::TempDir;

mod conflicts;
mod forge;
mod forge_lessons;
use forge_lessons::FORGE;

const NAME: &str = "Dusty Git Tutor";
const EMAIL: &str = "tutor@example.invalid";
const GREET: &str = "src/greet.rs";
const USAGE: &str = "Tutor: tutor git [N] · tutor forge [N] · tutor next · tutor prev · tutor reset · tutor N · tutor quit";
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
    /// A forge lesson: the practice forge it runs against and the tab it starts on.
    forge: Option<Forge>,
    /// Only on a modal screen (the editor's vim modes): the lesson teaches them.
    vim: bool,
    /// The keys-sheet items it teaches (`Changes · Stage all · unstage all`): the coverage
    /// test holds every item of every sheet to a lesson.
    teaches: &'static [&'static str],
}
impl Lesson {
    const fn teaching(mut self, items: &'static [&'static str]) -> Self {
        self.teaches = items;
        self
    }
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
        forge: None,
        vim: false,
        teaches: &[],
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
        forge: None,
        vim: false,
        teaches: &[],
    }
}

/// A forge lesson on a fresh practice repository and practice forge.
const fn practice(
    title: &'static str,
    instruction: &'static str,
    forge: Forge,
    done: fn(&mut Probe) -> bool,
) -> Lesson {
    Lesson {
        title,
        instruction,
        setup: plain,
        carry: false,
        done,
        forge: Some(forge),
        vim: false,
        teaches: &[],
    }
}

/// A forge lesson that goes on where the previous one ended; `forge` is where it starts
/// when reached any other way.
const fn then(
    title: &'static str,
    instruction: &'static str,
    forge: Forge,
    done: fn(&mut Probe) -> bool,
) -> Lesson {
    Lesson {
        title,
        instruction,
        setup: plain,
        carry: true,
        done,
        forge: Some(forge),
        vim: false,
        teaches: &[],
    }
}

/// A lesson in the vim modes, on a fresh practice repository; a screen that is not modal
/// leaves it out.
const fn vim(
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
        forge: None,
        vim: true,
        teaches: &[],
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
            ).teaching(&["Navigate · Changes · Branches · History · Stashes"]),
            fresh(
                "Reader and Tab",
                "0 jumps to the reader on the right; Tab moves on to the next pane.",
                panes,
                |p| p.saw(0, p.screen.focus == 4) && p.screen.focus != 4,
            ).teaching(&["Navigate · Reader", "Navigate · Next pane"]),
            fresh(
                "Sub-tabs",
                "] switches sub-tabs. Open Tags in Branches (3) and Reflog in History (4).",
                panes,
                |p| {
                    let tags = p.saw(0, p.screen.subview == 3);
                    let reflog = p.saw(1, p.screen.history_view == 1);
                    tags && reflog
                },
            ).teaching(&["Navigate · Sub-tabs of this pane"]),
            fresh(
                "Hide the rail",
                "Space e hides the rail (lazygit keys: , e); the same keys show it again.",
                plain,
                |p| p.saw(0, p.screen.rail_hidden) && !p.screen.rail_hidden,
            ).teaching(&["View · Hide · show the rail", "Vim · toggle sidebar"]),
        ],
    },
    Chapter {
        name: "Help",
        lessons: &[
            fresh(
                "Keys",
                "? lists the focused pane's keys; Esc closes it. r rereads the repository.",
                plain,
                |p| p.saw(0, p.screen.help) && !p.screen.help,
            ).teaching(&[
                "View · Refresh",
                "View · Practice in a disposable repo",
                "View · Practice the forge tabs",
            ]),
        ],
    },
    Chapter {
        name: "Changes",
        lessons: &[
            fresh(
                "Flat and tree",
                "{Files:t} shows Changes as a folder tree; h folds the folder you are on.",
                tree,
                |p| p.screen.changes_tree && !p.screen.collapsed.is_empty(),
            ).teaching(&["Changes · Flat · tree", "View · Fold tree · pan diff"]),
            fresh(
                "All files",
                "F lists every tracked file, not only changes; F again goes back.",
                tree,
                |p| p.saw(0, p.screen.all_files) && !p.screen.all_files,
            ).teaching(&["Changes · Changed · all files"]),
            fresh(
                "Filter",
                "/ filters the list: type guide, Enter keeps it, Esc clears it.",
                tree,
                |p| {
                    let filter = &p.screen.filters[0];
                    let (active, editing) = (filter.active(), filter.editing);
                    p.saw(0, active && !editing) && !active
                },
            ).teaching(&["Navigate · Filter list · find in reader"]),
            fresh(
                "Find",
                "On echo.txt, {Files:v}; / finds: type hello, Enter. n jumps to the next match, N back.",
                echoes,
                |p| {
                    let search = &p.screen.preview.search;
                    let first = p.saw(0, search.active() && search.current == Some(0));
                    first && search.active() && search.current.is_some_and(|index| index >= 1)
                },
            ).teaching(&["Navigate · Next · previous match"]),
            fresh(
                "Split diff",
                "{Files:v} opens the diff in the reader; | splits it, | again unifies it.",
                tree,
                |p| p.saw(0, p.screen.split_diff) && !p.screen.split_diff,
            ).teaching(&["Changes · Open in reader", "View · Split · unified diff"]),
            fresh(
                "Open in the editor",
                "On greet.rs, {Files:v}; j to a line; e opens it in the editor, at that line.",
                edited_greet,
                |p| p.screen.notice.starts_with(EDITOR_NOTICE),
            ).teaching(&["Reader · Open the line in the editor"]),
        ],
    },
    Chapter {
        name: "Staging",
        lessons: &[
            fresh(
                "Stage a file",
                "Select notes.md and press {Files:s} to stage the whole file.",
                two_files,
                |p| p.staged().iter().any(|path| path == "notes.md"),
            ).teaching(&[
                "Changes · Stage · unstage · toggle",
                "Vim · toggle: stage · unstage",
            ]),
            fresh(
                "Stage all",
                "a stages every change; a again unstages them all.",
                three_files,
                |p| {
                    let (staged, unstaged) = (p.staged().len(), p.unstaged().len());
                    p.saw(0, staged == 3 && unstaged == 0) && staged == 0
                },
            ).teaching(&["Changes · Stage all · unstage all"]),
            fresh(
                "Mark files",
                "x marks a file ({Files:V} marks a run). Mark two, then {Files:s} stages them.",
                three_files,
                |p| {
                    let marked = p.screen.file_selection.marked.len();
                    p.saw(0, marked >= 2) && p.staged().len() == 2 && p.unstaged().len() == 1
                },
            ).teaching(&["Changes · Mark file", "Changes · Mark a range"]),
            fresh(
                "Stage a hunk",
                "On story.txt, {Files:v} opens the diff; j to a change, s stages its hunk.",
                two_hunks,
                |p| p.lines(true, "story.txt") > 0 && p.lines(false, "story.txt") > 0,
            ).teaching(&["Reader · Stage · unstage hunk · toggle"]),
            fresh(
                "Stage a line",
                "On list.txt, {Files:v}, then j to an added line; {Staging:a} stages just that line.",
                three_lines,
                |p| p.lines(true, "list.txt") == 1 && p.lines(false, "list.txt") > 0,
            ).teaching(&["Reader · Stage · unstage line or range"]),
            fresh(
                "Stage a range",
                "On list.txt, {Files:v}; {Staging:V} starts a range on a + line, j extends, {Staging:a} stages it.",
                four_lines,
                |p| p.lines(true, "list.txt") >= 2 && p.lines(false, "list.txt") > 0,
            ).teaching(&["Reader · Start · finish a range"]),
        ],
    },
    Chapter {
        name: "Discard",
        lessons: &[
            fresh(
                "Discard a hunk",
                "{Files:v} opens src/greet.rs, j to the debug line, d asks; Enter discards.",
                unwanted,
                |p| p.screen.root.join(GREET).is_file() && p.lines(false, GREET) == 0,
            ).teaching(&["Reader · Discard hunk (confirm)"]),
            fresh(
                "Discard a line",
                "On list.txt, {Files:v}; j to gamma; D asks to discard just that line; Enter.",
                three_lines,
                |p| p.lines(false, "list.txt") == 2 && p.lines(true, "list.txt") == 0,
            ).teaching(&["Reader · Discard line (confirm)"]),
            fresh(
                "Discard files",
                "x marks a.txt and b.txt; D discards the marked files (d just one); Enter.",
                three_files,
                |p| {
                    let left = p.unstaged();
                    left.len() == 1 && left.iter().any(|path| path == "c.txt")
                },
            ).teaching(&[
                "Changes · Discard (confirm)",
                "Changes · Discard marked (confirm)",
            ]),
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
            ).teaching(&["Changes · Commit staged", "Reader · Commit staged"]),
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
            ).teaching(&["History · Commit files"]),
            fresh(
                "File diffs",
                "4, Enter on 'Add the app'; J shows the next file, Enter or Tab reads it, / filters; Esc.",
                tree,
                |p| {
                    let open = p.screen.commit_open;
                    let reading = p.saw(0, open && p.screen.focus == 4);
                    let next = p.saw(1, open && p.screen.commit.as_ref().is_some_and(|commit| commit.selected >= 1));
                    reading && next && !open
                },
            ).teaching(&[
                "Commit files · Open the file's diff",
                "Commit files · Next · previous file",
                "Commit files · Filter files",
            ]),
            fresh(
                "Open a commit's line",
                "4, Enter on 'Add the app', Tab to its diff; j to a line; e opens it in the editor.",
                tree,
                |p| p.screen.commit_open && p.screen.notice.starts_with(EDITOR_NOTICE),
            ).teaching(&["Commit files · Open the line in the editor"]),
            fresh(
                "Full message",
                "4, select 'Explain the design'; m shows its whole message; Esc closes it.",
                long_message,
                |p| p.saw(0, p.screen.message_scroll.is_some()) && p.screen.message_scroll.is_none(),
            ).teaching(&["View · Full commit message"]),
            fresh(
                "Copy",
                "4, select 'Add a guide'; y offers SHA, short SHA, subject; Enter copies. o opens it on a forge.",
                panes,
                |p| p.saw(0, p.screen.notice.starts_with("Copied the")),
            ).teaching(&[
                "History · Copy SHA · short SHA · subject · URL",
                "Commit files · Copy SHA · short SHA · subject · URL",
                "History · Open the commit on the forge",
                "Commit files · Open the commit on the forge",
            ]),
            fresh(
                "Amend HEAD",
                "A amends HEAD with the staged file; Ctrl+Enter confirms.",
                forgot,
                |p| p.amended() && p.staged().is_empty(),
            ).teaching(&[
                "History · Amend with staged (confirm)",
                "Changes · Amend · reword · fixup",
            ]),
            fresh(
                "Reword",
                "W rewords HEAD: fix the typo in 'Add greting', then Ctrl+Enter.",
                typo,
                |p| p.amended() && p.same_tree() && !p.subject(0).contains("greting"),
            ).teaching(&["History · Reword"]),
            fresh(
                "Fixup",
                "4, select 'Add greeting'; F records the staged fix for it; Enter.",
                fixup,
                |p| {
                    p.moved()
                        && p.subject(0).starts_with("fixup! Add greeting")
                        && p.commit(1).map(|commit| commit.id()) == p.start
                },
            ).teaching(&["History · Commit staged as fixup!"]),
        ],
    },
    Chapter {
        name: "Stash",
        lessons: &[
            fresh(
                "Save",
                "5 Stashes, n, a message, Ctrl+Enter puts your edits away.",
                dirty,
                |p| p.stashes() >= 1 && p.unstaged().is_empty(),
            ).teaching(&["Stashes · New stash", "Changes · Stash all changes"]),
            carried(
                "Pop",
                "5, select the stash, {Stash:p}, then Enter restores and removes it.",
                stashed,
                |p| p.stashes() == 0 && p.screen.root.join("draft.md").is_file(),
            ).teaching(&["Stashes · Pop"]),
            fresh(
                "Apply",
                "5, {Stash:a}, then Enter applies the selected stash and keeps it.",
                two_stashes,
                |p| p.stashes() == 2 && !p.unstaged().is_empty(),
            ).teaching(&["Stashes · Apply"]),
            carried(
                "Drop",
                "5, d, then Enter drops the selected stash.",
                two_stashes,
                |p| p.stashes() == 1,
            ).teaching(&["Stashes · Drop (confirm)"]),
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
            ).teaching(&["Branches · New branch"]),
            fresh(
                "Checkout",
                "3, select feature, {Branches:Enter}, then Enter again to switch to it.",
                feature,
                |p| p.on_branch("feature"),
            ).teaching(&["Branches · Checkout"]),
            fresh(
                "Rename",
                "3, select old-name, {Branches:e}, type a new name, Ctrl+Enter.",
                old_name,
                |p| p.branch("old-name").is_none() && p.branches() == 2,
            ).teaching(&["Branches · Rename"]),
            fresh(
                "Upstream",
                "3, select feature, u; origin/feature is picked (Up/Down); Ctrl+Enter.",
                pushed_feature,
                |p| {
                    p.repo
                        .find_branch("feature", git2::BranchType::Local)
                        .is_ok_and(|branch| branch.upstream().is_ok())
                },
            ).teaching(&["Branches · Set upstream"]),
            fresh(
                "Delete",
                "3, select done-topic, d, then Enter deletes it.",
                done_topic,
                |p| p.branch("done-topic").is_none(),
            ).teaching(&["Branches · Delete (confirm)"]),
        ],
    },
    Chapter {
        name: "Merge",
        lessons: &[
            fresh(
                "Merge",
                "3, select topic, {Branches:m}, then Enter merges it. It will conflict.",
                diverged,
                |p| p.repo.state() == RepositoryState::Merge,
            ).teaching(&["Branches · Merge into current"]),
            carried(
                "Resolve",
                "2, select src/greet.rs; o or t keeps a side, Ctrl+Enter; s stages.",
                merging,
                |p| p.repo.state() == RepositoryState::Merge && !p.conflicted(),
            ).teaching(&["Operation · Take ours · theirs"]),
            carried(
                "Continue",
                "C, then Enter records the merge commit.",
                resolved,
                |p| p.clean() && p.commit(0).is_some_and(|commit| commit.parent_count() == 2),
            ).teaching(&["Operation · Continue"]),
        ],
    },
    Chapter {
        name: "Rebase",
        lessons: &[
            fresh(
                "Rebase",
                "3, select main, {Branches:R}, then Enter replays feature onto it. It stops.",
                rebase_ready,
                |p| p.rebasing(),
            ).teaching(&["Branches · Rebase current onto"]),
            carried(
                "Continue",
                "2, greet.txt: t keeps the commit, Ctrl+Enter, s, then C, Enter.",
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
            ).teaching(&["Operation · Skip commit"]),
            fresh(
                "Abort",
                "A rebase stopped here. X, then Enter aborts and restores feature.",
                rebase_stopped,
                |p| p.clean() && p.on_branch("feature") && p.head() == p.mark,
            ).teaching(&["Operation · Abort"]),
        ],
    },
    // Conflict resolution for every operation: merge, rebase, cherry-pick, pull.
    conflicts::CHAPTER,
    Chapter {
        name: "Worktrees",
        lessons: &[
            fresh(
                "New worktree",
                "2, ] Worktrees, n, Branch: review, keep the path, Ctrl+Enter.",
                plain,
                |p| p.worktrees() >= 1,
            ).teaching(&["Worktrees · New worktree"]),
            carried(
                "Open",
                "2, ] Worktrees, select review, {Worktrees:Enter} opens it here.",
                review,
                |p| p.repo.is_worktree(),
            ),
            carried(
                "Remove",
                "2, ] Worktrees: {Worktrees:Enter} on practice, then d on review, Enter.",
                review,
                |p| !p.repo.is_worktree() && p.worktrees() == 0,
            ).teaching(&["Worktrees · Remove a clean worktree"]),
        ],
    },
    Chapter {
        name: "Rewrite history",
        lessons: &[
            fresh(
                "Squash",
                "4, select 'Fix farewell typo'; s squashes it into the commit below; Enter.",
                farewell_typo,
                |p| {
                    let subjects = p.subjects();
                    p.clean()
                        && subjects.iter().any(|subject| subject == "Add farewell")
                        && !subjects.iter().any(|subject| subject == "Fix farewell typo")
                },
            ).teaching(&["History · Squash into the commit below"]),
            fresh(
                "Fixup",
                "4, select 'Fix farewell typo'; {Commits:f} folds it into the one below, message dropped; Enter.",
                farewell_typo,
                |p| {
                    let subjects = p.subjects();
                    p.clean()
                        && subjects.iter().any(|subject| subject == "Add farewell")
                        && !subjects.iter().any(|subject| subject == "Fix farewell typo")
                },
            ).teaching(&["History · Fixup into the commit below"]),
            fresh(
                "Drop",
                "4, select 'Add debug logging', d, then Enter drops it.",
                debug_commit,
                |p| {
                    let subjects = p.subjects();
                    p.clean()
                        && subjects.iter().any(|subject| subject == "Add docs")
                        && !subjects.iter().any(|subject| subject == "Add debug logging")
                },
            ).teaching(&["History · Drop (confirm)"]),
            fresh(
                "Move",
                "4, select 'First step'; {Commits:J} moves it below 'Second step'; Enter.",
                steps,
                |p| p.clean() && p.subject(0) == "Second step" && p.subject(1) == "First step",
            ).teaching(&["History · Move down · up"]),
            fresh(
                "Edit",
                "4, select 'Second step'; {Commits:e}, Enter stops the rebase there. C, Enter goes on.",
                steps,
                |p| p.saw(0, p.rebasing()) && p.clean() && p.subject(0) == "First step",
            ).teaching(&["History · Edit: stop the rebase here"]),
            fresh(
                "Reword an older commit",
                "4, select 'Add greting'; {Commits:W}, fix the typo, then Ctrl+Enter.",
                old_typo,
                |p| {
                    let subjects = p.subjects();
                    p.clean()
                        && subjects.first().is_some_and(|subject| subject == "Add docs")
                        && subjects.iter().any(|subject| subject.starts_with("Add gree"))
                        && !subjects.iter().any(|subject| subject.contains("greting"))
                },
            ),
            fresh(
                "Amend an older commit",
                "4, select 'Add farewell'; A folds the staged line into it; Enter.",
                missed,
                |p| {
                    p.clean()
                        && p.moved()
                        && p.staged().is_empty()
                        && p.subject(0) == "Add docs"
                        && p.subject(1) == "Add farewell"
                        && p.commit(1)
                            .and_then(|commit| {
                                let entry = commit.tree().ok()?.get_path(Path::new("src/farewell.rs")).ok()?;
                                let blob = p.repo.find_blob(entry.id()).ok()?;
                                Some(String::from_utf8_lossy(blob.content()).contains("See you"))
                            })
                            .unwrap_or(false)
                },
            ),
        ],
    },
    Chapter {
        name: "Undo",
        lessons: &[
            fresh(
                "Undo",
                "z shows what the last commit moves back to; Enter undoes it.",
                oops,
                |p| p.clean() && p.mark.is_some() && p.head() == p.mark,
            ).teaching(&["Sync · Undo · redo (confirm)"]),
            carried(
                "Redo",
                "{Commits:Z} shows the undone commit; Enter brings it back.",
                undone,
                |p| p.moved() && p.subject(0) == OOPS,
            ),
        ],
    },
    Chapter {
        name: "Reflog",
        lessons: &[
            fresh(
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
            ).teaching(&["Reflog · Rescue branch here"]),
            fresh(
                "Browse",
                "4, ] Reflog, select 'Precious work'; Enter lists its files; Esc goes back.",
                lost,
                |p| p.saw(0, p.screen.commit_open && p.screen.history_view == 1) && !p.screen.commit_open,
            ).teaching(&["Reflog · Browse files"]),
            fresh(
                "Detach",
                "4, ] Reflog, select 'Precious work'; o, Enter checks it out with a detached HEAD.",
                lost,
                |p| p.mark.is_some() && p.repo.head_detached().unwrap_or(false) && p.head() == p.mark,
            ).teaching(&["Reflog · Checkout detached"]),
            fresh(
                "Worktree",
                "4, ] Reflog, select 'Precious work'; w, keep the path, Ctrl+Enter opens a worktree there.",
                lost,
                |p| p.worktrees() >= 1,
            ).teaching(&["Reflog · Detached worktree"]),
        ],
    },
    Chapter {
        name: "Remotes and tags",
        lessons: &[
            fresh(
                "Fetch",
                "3, ] Remotes, select origin, f, then Enter fetches.",
                teammate,
                |p| {
                    p.mark.is_some()
                        && p.repo.refname_to_id("refs/remotes/origin/practice").ok() == p.mark
                },
            ).teaching(&["Remotes · Fetch (confirm)"]),
            fresh(
                "Track",
                "3, ] Remotes, select origin/topic; Enter, then Enter makes a local topic tracking it.",
                remote_topic,
                |p| {
                    p.repo
                        .find_branch("topic", git2::BranchType::Local)
                        .is_ok_and(|branch| branch.upstream().is_ok())
                },
            ).teaching(&["Remotes · Track as a local branch"]),
            fresh(
                "Pull",
                "A teammate pushed a commit. p shows the pull; Enter brings it in.",
                teammate,
                |p| p.mark.is_some() && p.head() == p.mark,
            ).teaching(&["Sync · Pull (confirm)"]),
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
            ).teaching(&["Sync · Push (confirm)"]),
            fresh(
                "Inspect a tag",
                "3, ] ] Tags, select v1.0; Enter reads its history in the reader.",
                release,
                |p| p.screen.focus == 4 && p.screen.last_list == 1 && p.screen.subview == 3,
            ).teaching(&["Tags · Inspect tag"]),
            fresh(
                "Delete a tag",
                "3, ] ] Tags, select v0.1, d, then Enter deletes the local tag.",
                tagged,
                |p| p.repo.find_reference("refs/tags/v0.1").is_err(),
            ).teaching(&["Tags · Delete local tag"]),
            fresh(
                "Push a tag",
                "3, ] ] Tags, select v1.0, P, then Enter pushes the tag alone.",
                release,
                |p| {
                    Repository::open_bare(p.remote)
                        .is_ok_and(|remote| remote.find_reference("refs/tags/v1.0").is_ok())
                },
            ).teaching(&["Tags · Push tag (confirm)"]),
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
            ).teaching(&["Submodules · Init · update"]),
            fresh(
                "Stage pointer",
                "lib has a new commit. 2, ] ] Submodules, s stages its pointer.",
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
            ).teaching(&["Submodules · Toggle pointer · stage it"]),
        ],
    },
    Chapter {
        name: "Vim mode",
        lessons: &[
            vim(
                "Mode chip",
                "The chip at the footer's left is the mode. {Files:v}, then {Staging:V} on a + line: VISUAL; Esc.",
                three_lines,
                |p| {
                    let mode = p.screen.input_mode();
                    p.saw(0, mode == Some(InputMode::Visual)) && mode == Some(InputMode::Normal)
                },
            ).teaching(&["Vim · VISUAL · select"]),
            vim(
                "Insert and Normal",
                "c opens the box in INSERT. Esc: NORMAL; i types again; Ctrl+Enter commits.",
                staged,
                |p| {
                    let open = p.screen.form.as_ref().is_some_and(|form| matches!(form.action, FormAction::Commit));
                    let mode = p.screen.input_mode();
                    let normal = p.saw(0, open && mode == Some(InputMode::Normal));
                    let again = p.saw(1, normal && open && mode == Some(InputMode::Insert));
                    again && p.moved()
                },
            ).teaching(&[
                "Vim · INSERT · type",
                "Vim · NORMAL · again closes",
            ]),
            vim(
                "Vim keys",
                "? opens the keys; its Vim group lists the modes and the leader. Esc closes it.",
                plain,
                |p| p.saw(0, p.screen.help) && !p.screen.help,
            ).teaching(&["Vim · Leader · which-key"]),
        ],
    },
];

/// The two tracks: the Git page's lessons and the forge tabs' lessons.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Track {
    Git,
    Forge,
}
impl Track {
    fn chapters(self) -> &'static [Chapter] {
        match self {
            Self::Git => GIT,
            Self::Forge => FORGE,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Git => "Git",
            Self::Forge => "Forge",
        }
    }
    /// The lessons in order; `vim` (a modal screen) adds the vim-mode lessons.
    fn lessons(self, vim: bool) -> impl Iterator<Item = (&'static Chapter, &'static Lesson)> {
        self.chapters()
            .iter()
            .flat_map(|chapter| chapter.lessons.iter().map(move |lesson| (chapter, lesson)))
            .filter(move |(_, lesson)| vim || !lesson.vim)
    }
    fn total(self, vim: bool) -> usize {
        self.lessons(vim).count()
    }
    fn lesson(self, index: usize, vim: bool) -> Option<(&'static Chapter, &'static Lesson)> {
        self.lessons(vim).nth(index)
    }
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
    /// A forge lesson's practice forge; it stops with the fixture.
    server: Option<forge::Server>,
}
impl Fixture {
    fn new(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(text)?;
        let dir = crate::files::canonicalize(dir).map_err(text)?;
        let fixture = Self {
            root: dir.join("practice"),
            remote: dir.join("remote.git"),
            hooks: dir.join("hooks"),
            dir,
            server: None,
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
    f.git(&[
        "stash",
        "push",
        "--quiet",
        "--include-untracked",
        "-m",
        "Practice stash",
    ])?;
    f.write(GREET, &greeting("Hello, Dusty Git!"))?;
    Ok(None)
}
fn tree(f: &Fixture) -> Setup {
    f.write("src/app/main.rs", "fn main() {}\n")?;
    f.write("src/app/view.rs", "pub fn view() {}\n")?;
    f.write("docs/guide.md", "# Guide\n")?;
    f.commit("Add the app")?;
    f.write(
        "src/app/main.rs",
        "fn main() {\n    println!(\"practice\");\n}\n",
    )?;
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
/// The same word on three changed lines, for find and `n`.
fn echoes(f: &Fixture) -> Setup {
    f.write("echo.txt", "start\n")?;
    f.commit("Add an echo")?;
    f.write(
        "echo.txt",
        "start\nhello one\nmiddle\nhello two\nhello three\n",
    )?;
    Ok(None)
}
/// HEAD has a body the History row does not show.
fn long_message(f: &Fixture) -> Setup {
    f.write("docs/design.md", "# Design\n")?;
    f.git(&["add", "docs/design.md"])?;
    f.git(&[
        "commit",
        "--quiet",
        "-m",
        "Explain the design",
        "-m",
        "The panes keep their places, so the keys you learn stay where they are.",
    ])?;
    Ok(None)
}
/// `origin/topic` exists; no local `topic` does.
fn remote_topic(f: &Fixture) -> Setup {
    f.git(&["branch", "topic"])?;
    f.git(&["push", "--quiet", "origin", "topic"])?;
    f.git(&["branch", "--quiet", "-D", "topic"])?;
    Ok(None)
}
/// One changed file for the reader.
fn edited_greet(f: &Fixture) -> Setup {
    f.write(GREET, &greeting("Hello, editor!"))?;
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
    f.git(&[
        "stash",
        "push",
        "--quiet",
        "--include-untracked",
        "-m",
        "Practice stash",
    ])?;
    Ok(None)
}
fn two_stashes(f: &Fixture) -> Setup {
    f.write(GREET, &greeting("Hello, first stash"))?;
    f.git(&["stash", "push", "--quiet", "-m", "Greeting tweak"])?;
    f.write("idea.md", "An idea\n")?;
    f.git(&[
        "stash",
        "push",
        "--quiet",
        "--include-untracked",
        "-m",
        "Idea",
    ])?;
    Ok(None)
}
fn review(f: &Fixture) -> Setup {
    f.git(&["worktree", "add", "--quiet", "-b", "review", "../review"])?;
    Ok(None)
}
/// A committed `lib` submodule cloned from `library/` beside the practice repository.
fn library(f: &Fixture) -> Result<(), String> {
    let library = f.dir.join("library");
    run(
        &f.dir,
        &f.hooks,
        &["init", "--quiet", "-b", "main", "library"],
        true,
    )?;
    std::fs::write(library.join("lib.txt"), "Library v1\n").map_err(text)?;
    run(&library, &f.hooks, &["add", "-A"], true)?;
    run(
        &library,
        &f.hooks,
        &["commit", "--quiet", "-m", "Library v1"],
        true,
    )?;
    // File transport only for this local, disposable clone.
    let source = library.to_string_lossy().into_owned();
    f.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "--quiet",
        "add",
        &source,
        "lib",
    ])?;
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
    run(
        &lib,
        &f.hooks,
        &["commit", "--quiet", "-am", "Library v2"],
        true,
    )?;
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
const OOPS: &str = "Commit too early";
/// HEAD is a commit the lesson undoes; the mark is the commit before it.
fn oops(f: &Fixture) -> Setup {
    let base = f.head()?;
    f.write("draft.md", "Not ready\n")?;
    f.commit(OOPS)?;
    Ok(Some(base))
}
/// The commit was just undone with `z` (its reflog entry says so), so `Z` redoes it.
fn undone(f: &Fixture) -> Setup {
    let base = f.head()?;
    oops(f)?;
    f.git(&[
        "update-ref",
        "-m",
        &format!("{}: updating HEAD", crate::git::history::UNDO_ACTION),
        "HEAD",
        &base.to_string(),
    ])?;
    f.git(&["reset", "--quiet", "--hard", "HEAD"])?;
    Ok(Some(base))
}
fn release(f: &Fixture) -> Setup {
    f.git(&["tag", "v1.0"])?;
    Ok(None)
}
fn farewell_typo(f: &Fixture) -> Setup {
    f.write(
        "src/farewell.rs",
        "pub fn farewell() -> &'static str {\n    \"Goodby\"\n}\n",
    )?;
    f.commit("Add farewell")?;
    f.write(
        "src/farewell.rs",
        "pub fn farewell() -> &'static str {\n    \"Goodbye\"\n}\n",
    )?;
    f.commit("Fix farewell typo")?;
    Ok(None)
}
fn debug_commit(f: &Fixture) -> Setup {
    f.write("src/debug.rs", "pub fn trace() { eprintln!(\"here\"); }\n")?;
    f.commit("Add debug logging")?;
    f.write("docs.md", "Docs\n")?;
    f.commit("Add docs")?;
    Ok(None)
}
fn steps(f: &Fixture) -> Setup {
    f.write("steps/two.md", "Second\n")?;
    f.commit("Second step")?;
    f.write("steps/one.md", "First\n")?;
    f.commit("First step")?;
    Ok(None)
}
fn old_typo(f: &Fixture) -> Setup {
    f.write("greeting.txt", "Hello\n")?;
    f.commit("Add greting")?;
    f.write("docs.md", "Docs\n")?;
    f.commit("Add docs")?;
    Ok(None)
}
fn missed(f: &Fixture) -> Setup {
    f.write("src/farewell.rs", "pub fn farewell() {}\n")?;
    f.commit("Add farewell")?;
    f.write("docs.md", "Docs\n")?;
    f.commit("Add docs")?;
    f.write("src/farewell.rs", "pub fn farewell() {}\n// See you\n")?;
    f.git(&["add", "src/farewell.rs"])?;
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
    /// A forge lesson's practice forge.
    forge: Option<&'a forge::Server>,
}
impl Probe<'_> {
    /// What the forge tabs show.
    fn view(&self) -> github::tutor::TutorView {
        self.screen.github.tutor_view()
    }
    /// The practice forge answered a write that passes `request(method, path, body)`.
    fn wrote(&self, request: impl Fn(&str, &str, &serde_json::Value) -> bool) -> bool {
        self.forge.is_some_and(|forge| forge.wrote(request))
    }
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
        let tree = |id: Oid| {
            self.repo
                .find_commit(id)
                .ok()
                .map(|commit| commit.tree_id())
        };
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
        self.changes(true)
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }
    fn unstaged(&self) -> Vec<String> {
        self.changes(false)
            .into_iter()
            .map(|(path, _)| path)
            .collect()
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

/// Builds lesson `index` in its own new directory and opens a screen on it. A forge
/// lesson also starts its practice forge and puts the forge settings in its sandbox.
fn build(
    base: &Path,
    track: Track,
    vim: bool,
    index: usize,
    attempt: usize,
) -> Result<(Fixture, Option<Oid>, GitScreen), String> {
    let (_, lesson) = track.lesson(index, vim).ok_or("No such lesson")?;
    let dir = base.join(format!("{:02}-{attempt}", index + 1));
    let built = Fixture::new(&dir).and_then(|mut fixture| {
        let mark = (lesson.setup)(&fixture)?;
        if let Some(practice) = &lesson.forge {
            forge::configure_repository(|args| fixture.git(args), practice)?;
            let server = forge::Server::start(&fixture.dir, practice.world)?;
            sandbox::enter(server.sandbox(practice.token == Token::Signed));
            fixture.server = Some(server);
        }
        let mut screen = GitScreen::open(&fixture.root)?;
        if let Some(practice) = &lesson.forge {
            screen.open_target(practice.page, practice.item);
            sign_in(practice);
        }
        Ok((fixture, mark, screen))
    });
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    built
}

/// A lesson that adds the practice token does it as it starts; the screen notices once
/// the accounts popover re-checks (`a`, `r`), as with a real sign-in.
fn sign_in(practice: &Forge) {
    if practice.token == Token::Added {
        sandbox::set("DUSTY_FORGE_TOKEN", Some(forge::TOKEN));
    }
}

/// The tutor banner on the forge tabs, which paint no banner of their own.
pub(super) struct Banner {
    title: String,
    completed: Option<String>,
    pub(super) instruction: String,
    hint: &'static str,
}

/// The finished lesson's `✓ Title` at the right end of the banner's title row (row `y`),
/// left out when the row is too narrow to keep it clear of the title.
pub(super) fn completed_mark(grid: &mut CellGrid, y: u16, mark: &str, title: &str, width: u16) {
    let (mark_width, title_width) = (shell::chrome::width(mark), shell::chrome::width(title));
    if 1 + title_width + 4 + mark_width + 1 > width {
        return;
    }
    let style = Style::Git {
        role: GitRole::Added,
        surface: GitSurface::Sheet,
        emphasis: Emphasis::None,
    };
    grid.put_text_clipped(width - 1 - mark_width, y, mark, style, width - 1);
}

/// Rows the banner takes above the footer.
pub(super) const BANNER_ROWS: u16 = 3;

/// `inner` (a forge page painted `BANNER_ROWS` shorter) with the banner between its body
/// and its footer, as the Git page lays it out.
pub(super) fn with_banner(inner: Frame, banner: Banner, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    let footer = height - BANNER_ROWS - 1;
    for y in 0..=footer {
        let to = if y == footer { height - 1 } else { y };
        for x in 0..width {
            let Some(cell) = inner.grid.get(x, y) else {
                continue;
            };
            if cell.continuation {
                continue;
            }
            let wide = inner
                .grid
                .get(x + 1, y)
                .is_some_and(|next| next.continuation);
            frame.grid.set_cell_with_width(
                x,
                to,
                &cell.symbol,
                cell.style,
                if wide { 2 } else { 1 },
            );
        }
    }
    let style = |role, emphasis| Style::Git {
        role,
        surface: GitSurface::Sheet,
        emphasis,
    };
    for y in footer..footer + BANNER_ROWS {
        frame.grid.fill_row(y, style(GitRole::Text, Emphasis::None));
    }
    let right = width.saturating_sub(1);
    frame.grid.put_text_clipped(
        1,
        footer,
        &banner.title,
        style(GitRole::Focus, Emphasis::Bold),
        right,
    );
    if let Some(completed) = &banner.completed {
        completed_mark(&mut frame.grid, footer, completed, &banner.title, width);
    }
    frame.grid.put_text_clipped(
        1,
        footer + 1,
        &shell::chrome::ellipsize(&banner.instruction, usize::from(width.saturating_sub(2))),
        style(GitRole::Text, Emphasis::None),
        right,
    );
    frame.grid.put_text_clipped(
        1,
        footer + 2,
        banner.hint,
        style(GitRole::Muted, Emphasis::None),
        right,
    );
    frame.cursor = inner
        .cursor
        .map(|(x, y, shape)| (x, if y >= footer { y + BANNER_ROWS } else { y }, shape));
    frame
}

/// What `e` says in the tutor instead of opening the editor: the practice files stay here.
pub(super) fn editor_notice(path: &str, line: usize) -> String {
    format!("{EDITOR_NOTICE} {path} at line {line}; in the tutor it only says where")
}
const EDITOR_NOTICE: &str = "e opens";

/// An instruction in the keys in force: `{Branches:m}` is the key that runs Dusty Git's `m`
/// in Branches (`M` in the lazygit preset); everything else reads as written.
pub(super) fn keyed(text: &str, keys: github::keymap::Keys) -> String {
    use github::keymap::Panel;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}').map(|close| open + close) else {
            rest = &rest[open..];
            break;
        };
        let inner = &rest[open + 1..close];
        let panel = inner.split_once(':').and_then(|(panel, key)| {
            let panel = match panel {
                "Files" => Panel::Files,
                "Worktrees" => Panel::Worktrees,
                "Branches" => Panel::Branches,
                "Commits" => Panel::Commits,
                "Stash" => Panel::Stash,
                "Staging" => Panel::Staging,
                _ => return None,
            };
            Some((panel, key))
        });
        match panel {
            Some((panel, key)) => {
                out.push_str(&keys.show(panel, key).unwrap_or_else(|| key.to_owned()))
            }
            None => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

pub(super) struct Session {
    directory: TempDir,
    track: Track,
    /// The screen is modal: the vim-mode lessons are part of the track.
    vim: bool,
    /// Leaves the forge sandbox when the session ends, however it ends.
    sandbox: forge::Sandbox,
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
    /// The lesson just finished, shown as a check mark on the banner's title row.
    completed: Option<&'static str>,
}
impl Session {
    pub fn title(&self) -> String {
        let (name, total) = (self.track.name(), self.track.total(self.vim));
        match self.track.lesson(self.lesson, self.vim) {
            None => format!("{name} tutor · all {total} lessons done"),
            Some(_) if self.failed => {
                format!("{name} tutor · {}/{total} · setup failed", self.lesson + 1)
            }
            Some((chapter, lesson)) => format!(
                "{name} tutor · {}/{total} · {} · {}",
                self.lesson + 1,
                chapter.name,
                lesson.title
            ),
        }
    }
    pub fn instruction(&self) -> &'static str {
        match self.track.lesson(self.lesson, self.vim) {
            None if self.track == Track::Forge => {
                "You practiced every forge lesson. q returns to your workspace."
            }
            None => {
                "You practiced every Git lesson. q returns to your workspace; :tutor forge goes on."
            }
            Some(_) if self.failed => {
                "Practice setup failed (see the notice). :tutor reset retries; :tutor next skips."
            }
            Some((_, lesson)) => lesson.instruction,
        }
    }
    pub fn hint(&self) -> &'static str {
        match self.track {
            Track::Git => "Practice repo · :tutor next skips · :tutor prev goes back · :tutor quit",
            Track::Forge => {
                "Practice forge on this computer · :tutor next skips · :tutor prev goes back · :tutor quit"
            }
        }
    }
    /// The forge tabs are open to this session: it is on the forge track.
    pub fn forge_pages(&self) -> bool {
        self.track == Track::Forge
    }
    /// The banner for a forge page `height` rows tall, when it leaves the page room.
    /// `✓ Title` for the lesson just finished, until the next lesson change.
    pub fn completed(&self) -> Option<String> {
        self.completed.map(|title| format!("✓ {title}"))
    }
    pub fn banner(&self, height: u16) -> Option<Banner> {
        (height >= 16).then(|| Banner {
            title: self.title(),
            completed: self.completed(),
            instruction: self.instruction().to_owned(),
            hint: self.hint(),
        })
    }
    pub fn check_destination(&self, path: &Path) -> Result<(), String> {
        let parent =
            crate::files::canonicalize(path.parent().ok_or("Choose a practice worktree path")?)
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
        // A running session counts the lessons it started with; a new one, this screen's.
        let vim = self
            .tutor
            .as_ref()
            .map_or(self.modal, |session| session.vim);
        let number = |track: Track, word: &str| {
            word.parse::<usize>()
                .ok()
                .filter(|number| (1..=track.total(vim)).contains(number))
                .map(|number| number - 1)
        };
        let running = self.tutor.as_ref().map(|session| session.track);
        let index = match words.as_slice() {
            ["tutor"] | ["tutor", "git"] => return self.start_tutor(Track::Git, 0),
            ["tutor", "forge"] => return self.start_tutor(Track::Forge, 0),
            ["tutor", name @ ("git" | "forge"), lesson] => {
                let track = if *name == "git" {
                    Track::Git
                } else {
                    Track::Forge
                };
                let index = number(track, lesson).ok_or(USAGE)?;
                // The running tutor's own track jumps to the lesson, as `:tutor N` does.
                if running != Some(track) {
                    return self.start_tutor(track, index);
                }
                index
            }
            ["tutor", "quit"] => return self.stop_tutor(),
            ["tutor", word] if running.is_none() => {
                return match number(Track::Git, word) {
                    Some(index) => self.start_tutor(Track::Git, index),
                    None => Err(NOT_RUNNING.into()),
                };
            }
            ["tutor", word] => {
                let track = running.unwrap_or(Track::Git);
                let current = self.tutor.as_ref().map_or(0, |session| session.lesson);
                match *word {
                    "next" | "skip" => (current + 1).min(track.total(vim)),
                    "prev" | "back" => current.saturating_sub(1),
                    "reset" | "again" => current,
                    other => number(track, other).ok_or(USAGE)?,
                }
            }
            _ => return Err(USAGE.into()),
        };
        if self.busy() {
            return Err("Wait for the practice operation before changing lessons".into());
        }
        self.enter_lesson(index, false)
    }
    fn start_tutor(&mut self, track: Track, index: usize) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the current operation before starting the tutor".into());
        }
        if let Some(session) = &self.tutor {
            return Err(format!(
                "The {} tutor is already running; :tutor 1 starts it over",
                session.track.name().to_lowercase()
            ));
        }
        if track == Track::Forge && sandbox::active() {
            return Err("The forge tutor is already running in another Git screen".into());
        }
        let directory = tempfile::Builder::new()
            .prefix("dusty-git-tutor-")
            .tempdir()
            .map_err(text)?;
        let base = crate::files::canonicalize(directory.path()).map_err(text)?;
        // From here on a forge lesson may have entered the sandbox; the guard leaves it
        // whether or not the first lesson builds.
        let guard = forge::Sandbox(track == Track::Forge);
        let (fixture, mark, mut next) = build(&base, track, self.modal, index, 1)?;
        next.icon_mode = self.icon_mode;
        self.carry_input(&mut next);
        self.form = None;
        let saved = Box::new(std::mem::replace(self, next));
        self.tutor = Some(Session {
            directory,
            track,
            vim: self.modal,
            sandbox: guard,
            base,
            saved,
            lesson: index,
            attempt: 1,
            start: head_of(&fixture.root),
            fixture,
            mark,
            seen: 0,
            failed: false,
            completed: None,
        });
        Ok(())
    }
    pub(super) fn stop_tutor(&mut self) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the practice operation before leaving the tutor".into());
        }
        let session = self.tutor.take().ok_or(NOT_RUNNING)?;
        let Session {
            directory,
            saved,
            fixture,
            sandbox,
            ..
        } = session;
        *self = *saved;
        // The practice forge stops, the forge settings return to the user's, and the
        // practice repositories go.
        drop(fixture);
        drop(sandbox);
        drop(directory);
        Ok(())
    }
    /// Moves to lesson `index`. Finishing the previous lesson (`carried`) keeps its
    /// repository for a lesson that continues it; everything else builds afresh.
    fn enter_lesson(&mut self, index: usize, carried: bool) -> Result<(), String> {
        let session = self.tutor.as_mut().ok_or(NOT_RUNNING)?;
        let (track, vim) = (session.track, session.vim);
        session.seen = 0;
        session.completed = None;
        let next_lesson = track.lesson(index, vim);
        let continues = next_lesson.is_none_or(|(_, lesson)| carried && lesson.carry);
        if continues {
            session.lesson = index.min(track.total(vim));
            session.failed = false;
            session.start = head_of(&self.root);
            if let Some(practice) = next_lesson.and_then(|(_, lesson)| lesson.forge.as_ref()) {
                sign_in(practice);
            }
            return Ok(());
        }
        session.attempt += 1;
        match build(&session.base, track, vim, index, session.attempt) {
            Ok((fixture, mark, mut next)) => {
                let mut session = self.tutor.take().ok_or(NOT_RUNNING)?;
                session.lesson = index;
                session.failed = false;
                session.start = head_of(&fixture.root);
                session.mark = mark;
                let mut previous = std::mem::replace(&mut session.fixture, fixture);
                next.icon_mode = self.icon_mode;
                self.carry_input(&mut next);
                next.tutor = Some(session);
                drop(std::mem::replace(self, next));
                // The finished lesson's practice forge, repositories (and worktrees) go with it.
                previous.server = None;
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
    /// What the learner did is recorded on every input, busy or not: a key pressed while a
    /// read is in flight still counts. The lesson only ends once the screen is idle.
    pub(super) fn advance_tutor(&mut self) -> bool {
        let busy = self.busy();
        let Some(mut session) = self.tutor.take() else {
            return false;
        };
        let lesson = session
            .track
            .lesson(session.lesson, session.vim)
            .filter(|_| !session.failed);
        let done = match (lesson, Repository::open(&self.root)) {
            (Some((_, lesson)), Ok(repo)) => (lesson.done)(&mut Probe {
                screen: self,
                repo,
                remote: &session.fixture.remote,
                start: session.start,
                mark: session.mark,
                seen: &mut session.seen,
                forge: session.fixture.server.as_ref(),
            }),
            _ => false,
        } && !busy;
        let next = session.lesson + 1;
        self.tutor = Some(session);
        if let Some((_, lesson)) = lesson.filter(|_| done) {
            // A sheet still open from the finished lesson describes state that has moved on.
            // Operation sheets close themselves (a continue that stops on the next conflict
            // included); this only guards sheets the learner opened along the way.
            self.form = None;
            match self.enter_lesson(next, true) {
                Ok(()) => {
                    self.notice.clear();
                    if let Some(session) = self.tutor.as_mut() {
                        session.completed = Some(lesson.title);
                    }
                }
                Err(error) => self.notice = error,
            }
        }
        done
    }
    pub(super) fn tutor_worktree(&mut self, path: &Path) -> Result<(), String> {
        let session = self.tutor.as_ref().ok_or(NOT_RUNNING)?;
        if !crate::files::canonicalize(path)
            .map_err(|error| error.to_string())?
            .starts_with(&session.base)
        {
            return Err("Tutor navigation stays inside the practice directory".into());
        }
        let mut next = GitScreen::open(path)?;
        next.icon_mode = self.icon_mode;
        self.carry_input(&mut next);
        next.tutor = self.tutor.take();
        *self = next;
        self.advance_tutor();
        Ok(())
    }
}

impl GitScreen {
    /// The tutor's lessons in order, as their banners name them (`Panes · Focus panes`):
    /// the Git track, or the forge track; `vim` includes the vim-mode lessons. Tests find a
    /// lesson's number by its name, so adding a lesson does not renumber them by hand.
    pub fn tutor_outline(forge: bool, vim: bool) -> Vec<String> {
        let track = if forge { Track::Forge } else { Track::Git };
        track
            .lessons(vim)
            .map(|(chapter, lesson)| format!("{} · {}", chapter.name, lesson.title))
            .collect()
    }
}

/// The tutor teaches every action (the user's rule): each item on every keys sheet, in
/// every context and both key presets, names the lesson that teaches it.
#[cfg(test)]
mod coverage {
    use super::*;

    /// Moving about, which every lesson does on the way: the only items without a lesson.
    const TRIVIAL: &[&str] = &[
        "Navigate · Move",
        "Navigate · First · last",
        "Navigate · Scroll",
        "Navigate · Back",
        "Navigate · Move focus",
        "Navigate · Back to the list",
        "Navigate · Load more",
        "Commit files · Back to History",
        "Navigate · Back to the run history",
        "Navigate · Back · into the run",
        // The list's legend, not keys.
        "List marks · Checks passed",
        "List marks · Checks failed",
        "List marks · Checks running",
    ];

    /// The Notifications page has no keys sheet yet; these are its footer's keys
    /// (`github/notifications.rs`).
    const NOTIFICATIONS: &[(&str, &str)] = &[
        ("Enter", "Notifications · open"),
        ("e", "Notifications · done · inbox"),
        ("s", "Notifications · save · unsave"),
        ("m", "Notifications · mark read"),
        ("/", "Notifications · filter"),
        ("[ ]", "Notifications · unread"),
        ("o", "Notifications · browser"),
        ("u", "Notifications · unsubscribe"),
        ("M", "Notifications · mark all"),
        ("Tab", "Notifications · sidebar"),
    ];

    /// The forge sheets' `Read-only` group, shown only while the host's state says signed out
    /// (`shell/help.rs`); a unit test cannot sign the shared host registry out.
    const SIGNED_OUT: &[(&str, &str)] = &[("@", "Read-only · Accounts · how to sign in")];

    /// `(context, key, item)` for every keys sheet: each Git pane and sub-tab, an open
    /// commit, a paused operation, Pull requests, Issues and Actions, with the default keys,
    /// the vim modes and the lazygit preset.
    fn sheets() -> Vec<(String, String, String)> {
        let dir = TempDir::new().unwrap();
        let fixture = Fixture::new(&dir.path().join("coverage")).unwrap();
        let mut out = Vec::new();
        for preset in ["default", "vim", "lazygit"] {
            let mut screen = GitScreen::open(&fixture.root).unwrap();
            if preset != "default" {
                let mut config = crate::config::load_bundled_config().config;
                if preset == "lazygit" {
                    config.ui.git.keys = crate::config::GitKeys::Lazygit;
                }
                screen.configure_input(&config);
            }
            let mut add = |screen: &GitScreen, context: &str| {
                for (key, item) in super::super::shell::help::rows(screen) {
                    out.push((format!("{context} ({preset})"), key, item));
                }
            };
            for (focus, files_view, subview, history_view, context) in [
                (0, 0, 0, 0, "Changes"),
                (0, 1, 0, 0, "Worktrees"),
                (0, 2, 0, 0, "Submodules"),
                (1, 0, 0, 0, "Branches"),
                (1, 0, 2, 0, "Remotes"),
                (1, 0, 3, 0, "Tags"),
                (2, 0, 0, 0, "History"),
                (2, 0, 0, 1, "Reflog"),
                (3, 0, 0, 0, "Stashes"),
                (4, 0, 0, 0, "Reader"),
            ] {
                screen.focus = focus;
                screen.last_list = focus.min(3);
                screen.files_view = files_view;
                screen.subview = subview;
                screen.history_view = history_view;
                add(&screen, context);
            }
            screen.focus = 2;
            screen.history_view = 0;
            screen.commit_open = true;
            add(&screen, "Commit files");
            screen.commit_open = false;
            screen.operation_bar = Some(crate::git::conflict::GitOperationBar {
                operation: "merge".into(),
                phase: String::new(),
                blockers: Vec::new(),
                actions: Vec::new(),
            });
            add(&screen, "Operation");
            screen.operation_bar = None;
            for (page, context) in [(2, "Pull requests"), (1, "Issues"), (3, "Actions")] {
                screen.page = page;
                add(&screen, context);
            }
        }
        out.extend(NOTIFICATIONS.iter().map(|(key, item)| {
            (
                "Notifications".to_owned(),
                (*key).to_owned(),
                (*item).to_owned(),
            )
        }));
        out.extend(SIGNED_OUT.iter().map(|(key, item)| {
            (
                "Signed out".to_owned(),
                (*key).to_owned(),
                (*item).to_owned(),
            )
        }));
        out
    }

    /// The lesson that teaches `item`: `Git 12 · Staging · Stage all`.
    fn lesson_for(item: &str) -> Option<String> {
        [Track::Git, Track::Forge].into_iter().find_map(|track| {
            track
                .lessons(true)
                .enumerate()
                .find_map(|(index, (chapter, lesson))| {
                    lesson.teaches.contains(&item).then(|| {
                        format!(
                            "{} {} · {} · {}",
                            track.name(),
                            index + 1,
                            chapter.name,
                            lesson.title
                        )
                    })
                })
        })
    }

    /// One row per item, with the keys and contexts it has: `| item | keys | contexts | lesson |`.
    fn table(rows: &[(String, String, String)]) -> (String, Vec<String>) {
        let mut items: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
        for (context, key, item) in rows {
            let context = context.split(" (").next().unwrap_or(context).to_owned();
            match items.iter_mut().find(|(known, ..)| known == item) {
                Some((_, keys, contexts)) => {
                    if !keys.contains(key) {
                        keys.push(key.clone());
                    }
                    if !contexts.contains(&context) {
                        contexts.push(context);
                    }
                }
                None => items.push((item.clone(), vec![key.clone()], vec![context])),
            }
        }
        let mut text = String::from("| Action | Keys | Where | Lesson |\n|---|---|---|---|\n");
        let mut missing = Vec::new();
        for (item, keys, contexts) in &items {
            let lesson = if TRIVIAL.contains(&item.as_str()) {
                "navigation (allowed)".to_owned()
            } else if let Some(lesson) = lesson_for(item) {
                lesson
            } else {
                missing.push(format!(
                    "{item}  [{}]  ({})",
                    keys.join(" / "),
                    contexts.join(", ")
                ));
                "**missing**".to_owned()
            };
            text.push_str(&format!(
                "| {item} | {} | {} | {lesson} |\n",
                keys.join(" / ").replace('|', "\\|"),
                contexts.join(", ")
            ));
        }
        (text.replace("| | |", "| \\| |"), missing)
    }

    #[test]
    fn every_action_on_every_keys_sheet_has_a_lesson() {
        let rows = sheets();
        let (text, missing) = table(&rows);
        // `DUSTY_TUTOR_COVERAGE=path` writes the table.
        if let Ok(path) = std::env::var("DUSTY_TUTOR_COVERAGE") {
            std::fs::write(path, &text).unwrap();
        }
        assert!(
            missing.is_empty(),
            "{} actions have no lesson; teach each in a lesson (`.teaching(&[...])`):\n{}",
            missing.len(),
            missing.join("\n")
        );
    }

    #[test]
    fn every_taught_item_is_on_a_keys_sheet() {
        let rows = sheets();
        let stale = [Track::Git, Track::Forge]
            .into_iter()
            .flat_map(|track| track.lessons(true))
            .flat_map(|(_, lesson)| lesson.teaches.iter())
            .filter(|item| !rows.iter().any(|(_, _, known)| known == *item))
            .collect::<Vec<_>>();
        assert!(
            stale.is_empty(),
            "lessons teach items no keys sheet lists: {stale:?}"
        );
    }
}
