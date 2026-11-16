//! The forge track's lessons (`:tutor forge`), each against the practice forge
//! (`forge.rs`). Lessons are short and in order: pull requests, a pull request, merging,
//! issues, Actions, notifications, then signing in. Each names the keys-sheet items it
//! teaches (`.teaching(&[...])`); `tutor::coverage` fails when an action has no lesson.
use super::*;
use serde_json::Value;

const ORBIT: World = World::Orbit;
const ACTIONS: World = World::Actions;
const SIGNED: Token = Token::Signed;

/// The practice forge answered a request (a read too) that passes `request`: its log entry
/// (`method`, `path`, `operation`, `variables`, `body`).
fn asked(p: &Probe, request: impl Fn(&Value) -> bool) -> bool {
    p.forge.is_some_and(|forge| forge.requests().iter().any(request))
}

/// The practice forge answered the same GraphQL `operation` with the same variables twice:
/// the screen read it again (a refresh), not just once more for another list.
fn read_twice(p: &Probe, operation: &str) -> bool {
    let Some(forge) = p.forge else {
        return false;
    };
    let mut seen = Vec::new();
    for entry in forge.requests().iter().filter(|entry| entry["operation"] == operation) {
        let variables = entry["variables"].to_string();
        if seen.contains(&variables) {
            return true;
        }
        seen.push(variables);
    }
    false
}

/// How many times the practice forge answered a read of `path` (`repos/OWNER/NAME/…`).
fn reads(p: &Probe, path: &str) -> usize {
    p.forge.map_or(0, |forge| {
        forge
            .requests()
            .iter()
            .filter(|entry| entry["method"] == "GET" && entry["path"].as_str().is_some_and(|at| at.ends_with(path)))
            .count()
    })
}

/// The open sheet's title.
fn sheet(p: &Probe) -> String {
    p.screen.form.as_ref().map(|form| form.title.clone()).unwrap_or_default()
}

/// The screen's and the forge tabs' notices.
fn said(p: &Probe, text: &str) -> bool {
    p.screen.notice.contains(text) || p.view().notice.contains(text)
}

/// A write to an item (`issues/19`, `pulls/17`) whose body sets `field`.
fn patched(p: &Probe, field: &str) -> bool {
    p.wrote(|method, path, body| {
        method == "PATCH" && (path.contains("/issues/") || path.contains("/pulls/")) && body.get(field).is_some()
    })
}

/// A write that changes an item's labels, assignees or milestone.
fn metadata(p: &Probe, name: &str) -> bool {
    p.wrote(|_, path, body| path.ends_with(&format!("/{name}")) || path.contains(&format!("/{name}/")) || body.get(name).is_some())
}

/// The practice forge has no web pages: `o` asks, then says so.
fn browsed(p: &mut Probe) -> bool {
    said(p, "no web pages")
}

/// The forge tabs' chapters (`:tutor forge`), each against the practice forge.
pub(super) static FORGE: &[Chapter] = &[
    Chapter {
        name: "Pull requests",
        lessons: &[
            practice(
                "Open the tab",
                "> moves to the next top tab, Pull requests; < moves back.",
                Forge::new(ORBIT, SIGNED, "git"),
                |p| p.view().page == 2,
            )
            .teaching(&["Navigate · Previous · next top tab", "Navigate · Pull requests · Issues · Actions"]),
            then(
                "Sections",
                "] cycles the list's sections; stop on Review, what waits for you.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| p.view().section.as_deref() == Some("Review"),
            )
            .teaching(&["Navigate · Sections (list) · tabs (reader)"]),
            then(
                "Filter",
                "/ opens the filter: type retry in Search, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| {
                    let view = p.view();
                    view.page == 2 && view.query.contains("retry")
                },
            )
            .teaching(&["Filter · Filter query"]),
            then(
                "Open · Closed",
                "s shows the closed pull requests; s again, the open ones.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| {
                    let state = p.view().state;
                    p.saw(0, state == "closed") && state == "open"
                },
            )
            .teaching(&["Filter · Open · Closed"]),
            then(
                "Menus",
                "F opens the filter menus: Down picks an Author, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| p.view().query.contains("author:"),
            )
            .teaching(&["Filter · Author · Label · Sort menus"]),
            then(
                "Save a section",
                "S saves this filter as a section: name it Mine, Ctrl+Enter. D, Tab, Down, Ctrl+Enter deletes it.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| {
                    let mine = p.view().views.iter().any(|name| name == "Mine");
                    p.saw(0, mine) && !mine
                },
            )
            .teaching(&["Filter · Save · delete a section"]),
            then(
                "Views",
                "v lists the views, built in and saved: Down picks one, Ctrl+Enter shows it.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| {
                    let open = sheet(p).starts_with("Repository views");
                    p.saw(0, open) && !open && p.view().section.is_some()
                },
            )
            .teaching(&["Filter · Saved views (list and reader)"]),
            then(
                "Inbox",
                "i lists what waits for you in every repository; i again, this one.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| {
                    let inbox = p.view().inbox;
                    p.saw(0, inbox) && !inbox
                },
            )
            .teaching(&["Filter · Cross-repository inbox"]),
            practice(
                "Refresh",
                "R reads the list again from the forge.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| read_twice(p, "PullRequestList"),
            )
            .teaching(&["Filter · Refresh"]),
            then(
                "Browser",
                "o opens the pull request in your browser: Ctrl+Enter. The practice forge has no pages.",
                Forge::new(ORBIT, SIGNED, "prs"),
                browsed,
            )
            .teaching(&["Filter · Open in the browser"]),
        ],
    },
    Chapter {
        name: "Pull request",
        lessons: &[
            practice(
                "Open one",
                "j and k pick a pull request; Enter opens it in the reader.",
                Forge::new(ORBIT, SIGNED, "prs"),
                |p| {
                    let view = p.view();
                    view.page == 2 && view.reader
                },
            )
            .teaching(&["Navigate · Open · follow link"]),
            then(
                "List and reader",
                "2 moves to the list, 3 back to the reader.",
                Forge::new(ORBIT, SIGNED, "prs").item(17),
                |p| {
                    let reader = p.view().reader;
                    p.saw(0, !reader) && reader
                },
            )
            .teaching(&["Navigate · List · reader"]),
            then(
                "Tabs",
                "In the reader ] moves to the next tab, [ back. Go to Checks.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| p.view().pr_tab == "Checks",
            ),
            then(
                "Links",
                "Tab moves the reader's cursor to the next link; Enter would follow it.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| p.view().link,
            )
            .teaching(&["Navigate · Reader · next link"]),
            then(
                "Find",
                "Ctrl+F finds text in the reader: type Python, then Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| !p.view().find.is_empty(),
            )
            .teaching(&["Navigate · Find in the reader"]),
            then(
                "Hide the list",
                "Space e hides the list so the reader takes the width; Space e brings it back.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| {
                    let hidden = p.view().list_hidden;
                    p.saw(0, hidden) && !hidden
                },
            )
            .teaching(&["Navigate · Hide · show the list"]),
            then(
                "Comment",
                "C comments on the pull request: write a line, then Ctrl+Enter posts it.",
                Forge::new(ORBIT, SIGNED, "prs").item(17),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/comments") && path.contains("/issues/")),
            )
            .teaching(&["Pull request · Comment"]),
            then(
                "Edit",
                "e edits the title and description: change the title, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(17),
                |p| patched(p, "title"),
            )
            .teaching(&["Pull request · Edit title and description"]),
            then(
                "Labels",
                "l edits the labels: type bug, Ctrl+Enter. a edits the assignees: type rowan, Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(17),
                |p| metadata(p, "labels") && metadata(p, "assignees"),
            )
            .teaching(&["Pull request · Labels · assignees"]),
            then(
                "Close",
                "x closes the pull request, X reopens it: Ctrl+Enter confirms each.",
                Forge::new(ORBIT, SIGNED, "prs").item(17),
                |p| {
                    let state = |to: &'static str| move |method: &str, path: &str, body: &Value| {
                        method == "PATCH" && path.contains("/pulls/") && body["state"] == to
                    };
                    p.wrote(state("closed")) && p.wrote(state("open"))
                },
            )
            .teaching(&["Pull request · Close · reopen"]),
            then(
                "Files",
                "d jumps to Files changed: the changed files and their diff.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| p.view().pr_tab == "Files changed",
            )
            .teaching(&["Pull request · Files"]),
            then(
                "Choose a file",
                "f picks a changed file: Down to tests/test_queue.py, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| p.view().file.as_deref() == Some("tests/test_queue.py"),
            )
            .teaching(&["Files changed tab · Choose a file"]),
            then(
                "Wrap",
                "w pans long diff lines sideways instead of wrapping them; w again wraps.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| {
                    let unwrapped = p.view().unwrapped;
                    p.saw(0, unwrapped) && !unwrapped
                },
            )
            .teaching(&["Files changed tab · Wrap · pan long lines"]),
            then(
                "Viewed",
                "v marks the file viewed, as GitHub's Viewed box does.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| asked(p, |entry| entry["operation"] == "MarkFileViewed"),
            )
            .teaching(&["Files changed tab · Mark the file viewed"]),
            then(
                "Open in the editor",
                "j to a + line; e opens it in the editor, at that line (in Commits too).",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| p.screen.notice.starts_with(EDITOR_NOTICE),
            )
            .teaching(&["Files changed tab · Open the line in the editor (and in Commits)"]),
            then(
                "Line comment",
                "Enter on a diff line comments on it: write a line, Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| {
                    p.wrote(|method, path, body| {
                        method == "POST" && path.contains("/pulls/") && path.ends_with("/comments") && body.get("start_line").is_none()
                    })
                },
            )
            .teaching(&["Files changed tab · Comment on the line"]),
            then(
                "Range comment",
                "V starts a range, j extends it, C comments on it for your review: Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| {
                    // Beside a pending review the comment joins it; Review sends them together.
                    said(p, "Added to your review")
                        || p.wrote(|method, path, body| {
                            method == "POST" && path.contains("/pulls/") && path.ends_with("/comments") && body.get("start_line").is_some()
                        })
                },
            )
            .teaching(&["Files changed tab · Comment on a range"]),
            then(
                "Review",
                "r reviews: Down picks Approve, Tab, write a line, Ctrl+Enter sends it.",
                Forge::new(ORBIT, SIGNED, "prs").item(17),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/reviews")),
            )
            .teaching(&["Pull request · Review"]),
            practice(
                "Check out",
                "c checks the pull request's branch out here: Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| p.repo.head().ok().and_then(|head| head.shorthand().map(str::to_owned)).is_some_and(|branch| branch != "practice"),
            )
            .teaching(&["Pull request · Check out · reply in the reader"]),
            then(
                "Worktree",
                "w checks it out in a worktree of its own instead: Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").item(13),
                |p| p.repo.worktrees().is_ok_and(|names| !names.is_empty()),
            )
            .teaching(&["Pull request · Review in a worktree"]),
            practice(
                "From a branch",
                "On Git, 3 goes to Branches; o on fix/backoff opens its pull request, or a new one.",
                Forge::new(ORBIT, SIGNED, "git").branch("fix/backoff"),
                |p| {
                    let view = p.view();
                    view.page == 2 && (view.opened == Some(13) || sheet(p).starts_with("Create pull request"))
                },
            )
            .teaching(&["Branches · Its pull request, or open one"]),
            practice(
                "Create",
                "n opens a pull request from your branch: type a title, Ctrl+Enter, Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "prs").branch("feat/quiet-mode"),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/pulls")),
            )
            .teaching(&["Pull request · Create"]),
        ],
    },
    Chapter {
        name: "Merge",
        lessons: &[practice(
            "Merge",
            "#14 is approved. m goes to the merge box that ends its Conversation; Enter, then Ctrl+Enter merges.",
            Forge::new(ORBIT, SIGNED, "prs").item(14),
            |p| p.wrote(|method, path, _| method == "PUT" && path.ends_with("/merge")),
        )
        .teaching(&["Pull request · Merge readiness"])],
    },
    Chapter {
        name: "Issues",
        lessons: &[
            practice(
                "Open one",
                "> twice goes to Issues; Enter opens the selected issue.",
                Forge::new(ORBIT, SIGNED, "git"),
                |p| {
                    let view = p.view();
                    view.page == 1 && view.reader
                },
            ),
            then(
                "Comment",
                "C comments: write a line, then Ctrl+Enter posts it.",
                Forge::new(ORBIT, SIGNED, "issues").item(19),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/comments")),
            )
            .teaching(&["Issue · Comment"]),
            then(
                "Edit",
                "e edits the title and description: change the title, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "issues").item(19),
                |p| patched(p, "title"),
            )
            .teaching(&["Issue · Edit title and description"]),
            then(
                "Labels",
                "l edits the labels: after bug type ,native, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "issues").item(19),
                |p| metadata(p, "labels"),
            )
            .teaching(&["Issue · Labels"]),
            then(
                "Assignees",
                "a edits the assignees: type rowan, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "issues").item(19),
                |p| metadata(p, "assignees"),
            )
            .teaching(&["Issue · Assignees"]),
            then(
                "Milestone",
                "m sets the milestone: type v1, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "issues").item(19),
                |p| patched(p, "milestone"),
            )
            .teaching(&["Issue · Milestone"]),
            then(
                "Close",
                "x closes the issue: pick a reason, Ctrl+Enter. X reopens it: Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "issues").item(19),
                |p| {
                    let state = |to: &'static str| move |method: &str, path: &str, body: &Value| {
                        method == "PATCH" && path.contains("/issues/") && body["state"] == to
                    };
                    p.wrote(state("closed")) && p.wrote(state("open"))
                },
            )
            .teaching(&["Issue · Close (with a reason) · reopen"]),
            then(
                "New issue",
                "n opens a new issue: type a title, then Ctrl+Enter.",
                Forge::new(ORBIT, SIGNED, "issues"),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/issues")),
            )
            .teaching(&["Issue · New issue"]),
        ],
    },
    Chapter {
        name: "Actions",
        lessons: &[
            practice(
                "Run history",
                "Actions lists the runs, newest first. ] shows only the Failing ones.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.view().runs.section == 1,
            )
            .teaching(&["Navigate · All · Failing · Running · Mine"]),
            then(
                "Open a run",
                "Enter opens the run: its jobs, with the failing step unfolded.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.view().runs.run_open,
            )
            .teaching(&["Navigate · Open · fold the job or step"]),
            then(
                "Panels",
                "1 focuses Workflows, 2 the runs, 3 the run; Tab goes round them. End on the run.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let panel = p.view().runs.panel;
                    p.saw(0, panel == 0) && p.saw(1, panel == 1) && panel == 2
                },
            )
            .teaching(&["Navigate · Workflows · runs · run"]),
            then(
                "Errors",
                "n jumps to the next error line in the log, N to the one before.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.view().runs.jumped_to_error,
            )
            .teaching(&["Log · Next · previous error or match"]),
            then(
                "Full log",
                "f shows the job's full log; f again goes back to the steps.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let full = p.view().runs.full_log;
                    p.saw(0, full) && !full
                },
            )
            .teaching(&["Log · Full job log · steps"]),
            then(
                "Wrap",
                "w lets long log lines run past the edge; w again wraps them.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let nowrap = p.view().runs.nowrap;
                    p.saw(0, nowrap) && !nowrap
                },
            )
            .teaching(&["Log · Long lines wrap · run past the edge"]),
            then(
                "Pan",
                "w unwraps the lines; L pans them 8 columns right, H back.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let runs = p.view().runs;
                    p.saw(0, runs.nowrap && runs.pan > 0) && runs.nowrap && runs.pan == 0
                },
            )
            .teaching(&["Log · Pan 8 columns, unwrapped"]),
            then(
                "Find in the log",
                "/ finds in the log: type test_regression.py, then Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.view().runs.find.contains("test_regression"),
            ),
            then(
                "Open file:line",
                "Enter on that file:line opens it in the editor; this practice repo lacks it.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| said(p, "is not in this repository"),
            )
            .teaching(&["Log · Open file:line"]),
            then(
                "Save the log",
                "Ctrl+S saves the job's log to a file: Ctrl+Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| said(p, "Saved "),
            )
            .teaching(&["Log · Save the log"]),
            then(
                "Zoom",
                "z zooms the run to the whole screen; Esc unzooms it.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let zoom = p.view().runs.zoom;
                    p.saw(0, zoom) && !zoom
                },
            )
            .teaching(&["Log · Zoom the run · Esc returns"]),
            then(
                "Left column",
                "Space e hides the left column; Space e brings it back.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let hidden = p.view().list_hidden;
                    p.saw(0, hidden) && !hidden
                },
            )
            .teaching(&["Navigate · Hide · show the left column"]),
            then(
                "Filter runs",
                "2, then [ for All; / filters the runs (in a log, it finds): type Release, Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let runs = p.view().runs;
                    runs.section == 0 && runs.filter.contains("Release") && !runs.filtering
                },
            )
            .teaching(&["Navigate · Filter runs · find in the logs"]),
            then(
                "Attempts",
                "Enter opens Release 0.4.0; a lists its attempts: Down to Attempt 1, Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.view().attempt == 1,
            )
            .teaching(&["Log · Attempts"]),
            then(
                "Narrow",
                "2, then F narrows the runs by workflow, branch or event: Down, Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.view().runs.narrowed,
            )
            .teaching(&["Navigate · Narrow by workflow · branch · event"]),
            then(
                "Run workflow",
                "d runs the selected workflow by hand: Ctrl+Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/dispatches")),
            )
            .teaching(&["Actions · Run workflow"]),
            practice(
                "Refresh",
                "R reads the workflows and runs again from the forge.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| reads(p, "actions/workflows") >= 2,
            )
            .teaching(&["Navigate · Refresh"]),
            then(
                "Cancel",
                "The newest run is still running: x cancels it, Ctrl+Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/cancel")),
            )
            .teaching(&["Actions · Cancel"]),
            then(
                "Approve",
                "j to the fork's run that waits for approval; A approves it, Ctrl+Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| p.wrote(|method, path, _| method == "POST" && path.ends_with("/approve")),
            )
            .teaching(&["Actions · Approve a waiting run"]),
            then(
                "Rerun",
                "] to Failing, then r reruns the run: pick all or failed jobs, then Ctrl+Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    p.wrote(|method, path, _| {
                        method == "POST" && path.contains("actions/runs/") && path.contains("/rerun")
                    })
                },
            )
            .teaching(&["Actions · Re-run jobs"]),
            then(
                "Artifacts",
                "In the run, Enter on an artifact lists its files; s downloads it, Ctrl+Enter.",
                Forge::new(ACTIONS, SIGNED, "actions"),
                |p| {
                    let runs = p.view().runs;
                    p.saw(0, runs.artifact_files) && runs.downloaded
                },
            )
            .teaching(&["Actions · An artifact's files · download it"]),
        ],
    },
    Chapter {
        name: "Notifications",
        lessons: &[
            practice(
                "Inbox",
                "< from Git wraps round to Notifications, your inbox.",
                Forge::new(ORBIT, SIGNED, "git"),
                |p| p.view().page == github::notifications::PAGE,
            ),
            then(
                "Mark read",
                "j and k pick a thread; m marks it read. M would mark them all.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| {
                    p.wrote(|method, path, _| {
                        (method == "PATCH" && path.starts_with("notifications/threads/"))
                            || (method == "PUT" && path == "notifications")
                    })
                },
            )
            .teaching(&["Notifications · mark read", "Notifications · mark all"]),
            then(
                "Unread",
                "] shows only the unread threads; [ shows them all again.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| {
                    let unread = p.view().unread_only;
                    p.saw(0, unread) && !unread
                },
            )
            .teaching(&["Notifications · unread"]),
            then(
                "Filter",
                "/ filters the threads: type queue, then Enter. Esc clears the filter.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| {
                    let view = p.view();
                    view.thread_filter.contains("queue") && !view.thread_filtering
                },
            )
            .teaching(&["Notifications · filter"]),
            then(
                "Sidebar",
                "Tab gives the keys to the sidebar's views; Tab again gives them back.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| {
                    let side = p.view().sidebar;
                    p.saw(0, side) && !side
                },
            )
            .teaching(&["Notifications · sidebar"]),
            then(
                "Save",
                "s saves the thread for later; s again takes it out of Saved.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| {
                    let saved = said(p, "✓ Saved");
                    p.saw(0, saved) && said(p, "from Saved")
                },
            )
            .teaching(&["Notifications · save · unsave"]),
            then(
                "Done",
                "e marks the thread done, out of the inbox; e on a done one brings it back.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| said(p, "✓ Done"),
            )
            .teaching(&["Notifications · done · inbox"]),
            then(
                "Unsubscribe",
                "u unsubscribes you from the thread: it stops notifying you.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| said(p, "✓ Unsubscribed"),
            )
            .teaching(&["Notifications · unsubscribe"]),
            then(
                "Browser",
                "o opens the thread in your browser: Ctrl+Enter. The practice forge has no pages.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                browsed,
            )
            .teaching(&["Notifications · browser"]),
            then(
                "Open",
                "Enter opens the thread's pull request or issue in its tab.",
                Forge::new(ORBIT, SIGNED, "notifications"),
                |p| {
                    let view = p.view();
                    matches!(view.page, 1 | 2) && view.opened.is_some()
                },
            )
            .teaching(&["Notifications · open"]),
        ],
    },
    Chapter {
        name: "Sign in",
        lessons: &[
            practice(
                "Accounts · how to sign in [@]",
                "Signed out, so you can only read. @ opens Accounts to sign in; Esc closes it.",
                Forge::new(World::Anonymous, Token::Missing, "prs"),
                |p| {
                    let open = p.view().accounts_open;
                    p.saw(0, open) && !open
                },
            )
            // `@` is on the signed-in forge and Actions sheets (Navigate) and the signed-out ones.
            .teaching(&["Navigate · Accounts · how to sign in", "Read-only · Accounts · how to sign in"]),
            then(
                "Commenting while signed out",
                "C on a pull request starts a comment; signed out, it asks you to sign in.",
                Forge::new(World::Anonymous, Token::Missing, "prs"),
                |p| p.screen.notice.starts_with("Sign in") || p.view().notice.starts_with("Sign in"),
            ),
            then(
                "Sign in",
                "A practice token is now set, as GH_TOKEN would be. @, then r re-checks.",
                Forge::new(World::Anonymous, Token::Added, "prs"),
                |p| {
                    let view = p.view();
                    p.saw(0, view.accounts_open) && !view.accounts_open && view.signed_in
                },
            ),
        ],
    },
];
