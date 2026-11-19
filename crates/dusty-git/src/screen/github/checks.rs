//! F-PRCHECKS: a pull request's checks in GitHub's Checks-tab layout. The
//! job list on the left, grouped by workflow (failing first, a long run of skipped checks
//! folded); the selected job's steps and log on the right, read by the run tree's own code
//! (`runs/check_job.rs`). A narrow reader stacks the list above the job. Moving in the list
//! selects; Enter (or `o`) opens the job's whole run in Actions, or a check that has no run
//! on the forge's own page.
use super::super::shell::glyphs::{Glyph, glyph};
use super::lines::Line;
use super::pr::{
    check_parts, check_rank, checks_summary, ci_mark, distinct, shown_duration, skipped_key,
    split_event, state_word,
};
use super::prose::{Rich, blank, heading, muted, role, strong};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::CiState;
use crate::forge::records::{self, CheckCounts, CheckRow, DetailRecord};
use crate::ui::GitRole;
use unicode_width::UnicodeWidthStr;

/// From this reader width the job list sits beside the job; below it, above.
pub(super) const BESIDE: usize = 90;

/// A check's identity in the reader: its name and its link (two checks may share a name).
fn identity(check: &CheckRow) -> String {
    format!("{}\u{0}{}", check.name, check.url)
}

/// The "details on the forge" link under a check with no run: the check's own action with a
/// mark, so the cursor keeps to the row it is on (the list row has the plain one).
const DETAILS: char = '\u{1}';

/// Where a step's open or closed choice is kept, beside the skipped folds.
fn step_key(job: u64, step: u64) -> String {
    format!("\u{0}step\u{0}{job}\u{0}{step}")
}

/// GitLab links a merge request's job as `…/-/jobs/N`: no run, but the job's own id.
fn gitlab_job(check: &CheckRow) -> Option<u64> {
    let (_, rest) = check.url.split_once("/-/jobs/")?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// A check's own name: no workflow before it, no event after it.
fn own_name(check: &CheckRow) -> &str {
    split_event(check_parts(check).1).0
}

/// The checks of one workflow and event, failing first.
struct Group<'a> {
    workflow: &'a str,
    event: Option<&'a str>,
    checks: Vec<(&'a CheckRow, &'a str)>,
}

impl Group<'_> {
    fn name(&self) -> String {
        match (self.workflow.is_empty(), self.event) {
            (true, Some(event)) => event.to_owned(),
            (true, None) => "Other".to_owned(),
            (false, Some(event)) => format!("{} · {event}", self.workflow),
            (false, None) => self.workflow.to_owned(),
        }
    }
}

/// The checks as the list shows them: grouped by workflow and event (every heading names
/// both, so no row repeats them), the group with a failure first, failing before running,
/// passing and skipped (the first screen answers "what broke").
fn groups(record: &DetailRecord) -> Vec<Group<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();
    for check in &record.checks {
        let (workflow, own) = check_parts(check);
        let (own, event) = split_event(own);
        match groups
            .iter_mut()
            .find(|group| group.workflow == workflow && group.event == event)
        {
            Some(group) => group.checks.push((check, own)),
            None => groups.push(Group {
                workflow,
                event,
                checks: vec![(check, own)],
            }),
        }
    }
    for group in &mut groups {
        group
            .checks
            .sort_by_key(|(check, _)| check_rank(check.state));
    }
    groups.sort_by_key(|group| {
        group
            .checks
            .iter()
            .map(|(check, _)| check_rank(check.state))
            .min()
            .unwrap_or(u8::MAX)
    });
    groups
}

/// What the Checks tab and the merge box say when the head has no checks (one wording).
pub(super) fn no_checks(record: &DetailRecord) -> String {
    match record
        .commits
        .last()
        .map(|commit| commit.sha.chars().take(7).collect::<String>())
    {
        Some(sha) if !sha.is_empty() => format!("No checks have run on {sha} yet"),
        _ => "No checks have run on this commit yet".to_owned(),
    }
}

impl Workspace {
    fn check_record(&self, id: u64) -> Option<DetailRecord> {
        self.value(&Key::Item(2, id))
            .map(|value| self.checked(DetailRecord::from_value(value)))
    }

    /// The record with each check's state read from its job once the job is known: a
    /// finished job with a failed step failed, whatever the check's own conclusion says
    /// (the banner, the merge gate and the tab never say "passed" over `✕ Run pytest`).
    pub(super) fn checked(&self, mut record: DetailRecord) -> DetailRecord {
        for index in 0..record.checks.len() {
            if matches!(
                record.checks[index].state,
                CiState::Failed | CiState::Cancelled | CiState::Pending
            ) {
                continue;
            }
            let Some(job) = self.check_job(&record.checks[index]) else {
                continue;
            };
            if self.job_failed(&job) {
                record.checks[index].state = CiState::Failed;
                record.checks[index].word = "failure".into();
            }
        }
        record
    }

    /// The check the right pane shows: the one last selected, else the first listed.
    fn chosen_check<'a>(&self, id: u64, record: &'a DetailRecord) -> Option<&'a CheckRow> {
        self.checks_selected
            .get(&id)
            .and_then(|key| record.checks.iter().find(|check| identity(check) == *key))
            .or_else(|| {
                groups(record)
                    .first()
                    .and_then(|group| group.checks.first())
                    .map(|(check, _)| *check)
            })
    }

    /// The job behind a check: its run's job once the run is read (by id, else by name: a
    /// re-run gives the job a new id, and Gitea links a job by its place in the run), or a
    /// GitLab job by its own id.
    fn check_job(&self, check: &CheckRow) -> Option<Value> {
        let Some(run) = check.run else {
            return gitlab_job(check)
                .map(|id| runs::check_job::bare_job(id, own_name(check), check.state));
        };
        let jobs = array(self.value(&Key::Run(run, 0))?, "jobs");
        check
            .job
            .and_then(|id| jobs.iter().find(|job| number(job, "databaseId") == id))
            .or_else(|| {
                jobs.iter().find(|job| {
                    string(job, "name") == own_name(check) || string(job, "name") == check.name
                })
            })
            .cloned()
    }

    /// Whether a step of the pull request's job shows its log: its default, unless toggled.
    fn step_open(&self, id: u64, job: u64, step: u64, default: bool) -> bool {
        default != self.collapsed.contains(&(id, step_key(job, step)))
    }

    /// The selected check's run and job log, as the Checks tab reads them: (run, log).
    fn check_targets(&self) -> (Option<u64>, Option<u64>) {
        let id = self.number();
        if self.page != 2 || self.pr_tab != pr::CHECKS || id == 0 {
            return (None, None);
        }
        let Some(record) = self.check_record(id) else {
            return (None, None);
        };
        let Some(check) = self.chosen_check(id, &record) else {
            return (None, None);
        };
        let log = self.check_job(check).filter(|job| {
            let job_id = number(job, "databaseId");
            self.job_wants_log(job, &|step: u64, default: bool| {
                self.step_open(id, job_id, step, default)
            })
        });
        (check.run, log.map(|job| number(&job, "databaseId")))
    }

    /// The selected job's run, then its log once the run says which job it is.
    pub(super) fn check_reads(&mut self, refresh: bool) {
        let (run, log) = self.check_targets();
        if let Some(run) = run {
            self.enqueue(
                Key::Run(run, 0),
                self.read(Read::Run {
                    id: run.into(),
                    attempt: None,
                }),
                refresh,
            );
        }
        if let Some(job) = log {
            self.enqueue(Key::Log(job), self.read(Read::JobLog(job.into())), refresh);
        }
    }

    /// Whether a queued read is still the Checks tab's.
    pub(super) fn check_wants(&self, key: &Key) -> bool {
        let (run, log) = self.check_targets();
        match key {
            Key::Run(id, 0) => run == Some(*id),
            Key::Log(id) => log == Some(*id),
            _ => false,
        }
    }

    /// Moving in the job list selects: the job under the cursor is the one shown.
    pub(super) fn follow_check(&mut self) {
        if self.page != 2 || self.pr_tab != pr::CHECKS {
            return;
        }
        let Some(Action::Check(id, key)) = self
            .detail_rows
            .get(self.detail_cursor)
            .and_then(|row| row.action.clone())
        else {
            return;
        };
        let key = key.trim_end_matches(DETAILS).to_owned();
        if self.checks_selected.get(&id) != Some(&key) {
            self.checks_selected.insert(id, key);
            self.ensure(false);
        }
    }

    /// The selected job's row, where opening the tab puts the cursor.
    pub(super) fn selected_check_row(&self, rows: &[Line]) -> Option<usize> {
        let id = self.number();
        let record = self.check_record(id)?;
        let key = identity(self.chosen_check(id, &record)?);
        rows.iter()
            .position(|row| matches!(&row.action, Some(Action::Check(number, shown)) if *number == id && *shown == key))
    }

    /// Enter or `o` on a job: its whole run in Actions (Esc comes back), else the check's own
    /// page on the forge.
    pub(super) fn open_check(&mut self, id: u64, key: &str) -> Result<Option<Form>, String> {
        let key = key.trim_end_matches(DETAILS);
        let record = self
            .check_record(id)
            .ok_or("The pull request is still loading")?;
        let check = record
            .checks
            .iter()
            .find(|check| identity(check) == key)
            .ok_or("This check is no longer listed · R refreshes")?;
        self.checks_selected.insert(id, key.to_owned());
        if let Some(run) = check.run {
            return self.activate(Action::Run(run));
        }
        if check.url.is_empty() {
            return Err("This check has no details link".into());
        }
        let url = check.url.clone();
        self.cache.remove(&Key::Write);
        self.enqueue(
            Key::Write,
            Op::Write(
                self.target(),
                Write::OpenInBrowser {
                    kind: ItemKind::PullRequest,
                    number: id,
                    url: Some(url),
                },
            ),
            false,
        );
        self.notice_error = false;
        self.notice = "Opening the browser…".into();
        Ok(None)
    }

    /// The footer's word for Enter on the focused row of the Checks tab; empty where Enter
    /// does nothing.
    pub(super) fn check_enter(&self) -> &'static str {
        let Some(row) = self.detail_rows.get(self.detail_cursor) else {
            return "";
        };
        match &row.action {
            Some(Action::Check(id, key)) => {
                let key = key.trim_end_matches(DETAILS);
                let check = self.check_record(*id).and_then(|record| {
                    record
                        .checks
                        .into_iter()
                        .find(|check| identity(check) == key)
                });
                match check {
                    Some(check) if check.run.is_some() && self.terms().run_noun == "pipeline" => {
                        "open pipeline"
                    }
                    Some(check) if check.run.is_some() => "open run",
                    Some(check) if !check.url.is_empty() => "open in browser",
                    _ => "",
                }
            }
            Some(Action::Collapse(_, key)) if key.starts_with("\u{0}step\u{0}") => {
                if row.text.starts_with(glyph(Glyph::FoldOpen, self.icons)) {
                    "hide log"
                } else {
                    "show log"
                }
            }
            Some(Action::Collapse(..)) => "fold",
            _ => "",
        }
    }
}

/// The Checks tab: the job list, then (beside it, or below on a narrow reader) the job.
pub(super) fn rows(
    workspace: &Workspace,
    id: u64,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    noun: &str,
    width: usize,
    mode: ExplorerIconMode,
) {
    let beside = width >= BESIDE;
    if record.checks.is_empty() {
        // The frame stays: an empty list beside a calm note, in the merge box's words.
        let (mark, _, _) = ci_mark(CiState::Unknown, mode);
        if beside {
            let mut row = Rich::new()
                .plain("  ")
                .push(format!("{mark} "), role(GitRole::Muted))
                .push("No checks yet", role(GitRole::Muted))
                .line();
            row.outline = true;
            rows.push(row);
        }
        rows.push(
            Rich::new()
                .plain("  ")
                .push(no_checks(record), role(GitRole::Muted))
                .line(),
        );
        return;
    }
    let chosen = workspace.chosen_check(id, record).map(identity);
    let start = rows.len();
    list(
        workspace,
        id,
        record,
        rows,
        noun,
        chosen.as_deref(),
        beside,
        width,
        mode,
    );
    if beside {
        for row in &mut rows[start..] {
            row.outline = true;
        }
    } else {
        rows.push(blank());
    }
    // One cell spare at the pane's edge.
    // Beside, the list is as wide as its names need (`paint::outline_fit`), so 21 checks
    // never crush into a narrow column beside an empty pane (R16 gitea-02).
    let pane = if beside {
        width.saturating_sub(super::paint::outline_fit(width, &rows[start..]) + 3)
    } else {
        width.saturating_sub(1)
    };
    if let Some(check) = record
        .checks
        .iter()
        .find(|check| Some(identity(check)) == chosen)
    {
        job(workspace, id, check, rows, pane, beside, mode);
    }
    // A row the cursor can rest on with no action of its own (a heading, the job's title,
    // its workflow line) keeps its place by its own text and order, so the job pane's
    // "Quality checks" line never reads as the list's heading of the same name.
    for row in &mut rows[start..] {
        if row.action.is_none() && row.source.is_none() {
            row.source = Some(super::find::Source {
                text: std::sync::Arc::from(row.text.as_str()),
                offset: 0,
            });
            // As `record_rows` would have left it without the anchor: muted text never pans.
            if row.diff.is_none()
                && row.gutter.is_empty()
                && row.paint.is_empty()
                && row.band.is_none()
                && row.style == crate::ui::Style::Muted
            {
                row.pan = false;
            }
        }
    }
}

/// The job list: the page's total, then each workflow and its checks.
#[allow(clippy::too_many_arguments)]
fn list(
    workspace: &Workspace,
    id: u64,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    noun: &str,
    chosen: Option<&str>,
    beside: bool,
    width: usize,
    mode: ExplorerIconMode,
) {
    let counts = record.check_counts();
    let (mark, _, tone) = ci_mark(counts.state().unwrap_or(CiState::Unknown), mode);
    // One failing check is named where it is: `1 failing · Quality checks / Python tests`.
    let failing = record
        .checks
        .iter()
        .filter(|check| matches!(check.state, CiState::Failed | CiState::Cancelled))
        .collect::<Vec<_>>();
    let summary = match failing.as_slice() {
        [check] => {
            let (workflow, own) = check_parts(check);
            let own = split_event(own).0;
            if workflow.is_empty() {
                format!("1 failing · {own}")
            } else {
                format!("1 failing · {workflow} / {own}")
            }
        }
        _ => checks_summary(&counts, noun),
    };
    rows.push(
        Rich::new()
            .push(format!("{mark} "), role(tone))
            .push(summary, strong(tone))
            .line(),
    );
    let groups = groups(record);
    let grouped = groups.len() > 1
        || groups
            .first()
            .is_some_and(|group| !group.workflow.is_empty() || group.event.is_some());
    let indent = if grouped { 2 } else { 0 };
    // Beside the job a row is its mark and name; above it, its state and time too.
    let room = if beside {
        super::paint::outline_most(width)
    } else {
        width.min(prose::MEASURE)
    };
    let timed = !beside
        && record
            .checks
            .iter()
            .any(|check| shown_duration(check).is_some());
    let tail = if beside {
        0
    } else {
        12 + if timed { 10 } else { 0 }
    };
    let longest = groups
        .iter()
        .flat_map(|group| group.checks.iter().map(|(_, own)| own.width()))
        .max()
        .unwrap_or(0);
    let name_width = if beside {
        room.saturating_sub(indent + 3).max(8)
    } else {
        longest.clamp(12, room.saturating_sub(indent + 3 + tail).max(12))
    };
    for group in &groups {
        if grouped {
            rows.push(blank());
            let mut counted = CheckCounts::default();
            for (check, _) in &group.checks {
                counted.add(check.state);
            }
            let note = if beside {
                String::new()
            } else {
                counted.summary()
            };
            rows.push(heading(&group.name(), &note));
        }
        // A long run of skipped checks folds into one row: Enter opens it.
        let skipped = group
            .checks
            .iter()
            .filter(|(check, _)| check.state == CiState::Skipped)
            .count();
        let key = skipped_key(group.workflow, group.event);
        let open = workspace.collapsed.contains(&(id, key.clone()));
        let fold = skipped > 3 && !open;
        let shown = group
            .checks
            .iter()
            .filter(|(check, _)| !fold || check.state != CiState::Skipped)
            .collect::<Vec<_>>();
        let names = distinct(
            &shown.iter().map(|(_, own)| *own).collect::<Vec<_>>(),
            name_width,
        );
        for ((check, _), name) in shown.iter().zip(&names) {
            let (mark, _, tone) = ci_mark(check.state, mode);
            let selected = chosen == Some(identity(check).as_str());
            let mut row = Rich::new()
                .plain(&" ".repeat(indent))
                .push(format!("{mark} "), role(tone))
                .push(
                    name,
                    if selected {
                        strong(GitRole::Focus)
                    } else {
                        role(GitRole::Text)
                    },
                );
            if !beside {
                row = row
                    .to_column(name_width + indent + 4)
                    .push(state_word(&check.word), role(tone));
                if timed {
                    row = row.to_column(name_width + indent + 16).push(
                        shown_duration(check)
                            .map(records::duration)
                            .unwrap_or_default(),
                        role(GitRole::Muted),
                    );
                }
            }
            let mut row = row.line();
            row.action = Some(Action::Check(id, identity(check)));
            rows.push(row);
        }
        if skipped > 3 {
            let (mark, _, tone) = ci_mark(CiState::Skipped, mode);
            let mut row = if open {
                Rich::new()
                    .plain(&" ".repeat(indent))
                    .push(
                        format!("{} ", glyph(Glyph::FoldOpen, mode)),
                        role(GitRole::Muted),
                    )
                    .push(format!("{skipped} skipped"), role(GitRole::Muted))
                    .line()
            } else {
                // The folded row names what it holds, so the list hides nothing it counts
                //: `▸ ⊘ linux, macos, windows +6`.
                let names = group
                    .checks
                    .iter()
                    .filter(|(check, _)| check.state == CiState::Skipped)
                    .map(|(_, own)| *own)
                    .collect::<Vec<_>>();
                let room = name_width.saturating_sub(2);
                let mut text = String::new();
                for (at, name) in names.iter().enumerate() {
                    let rest = names.len() - at - 1;
                    let next = if text.is_empty() {
                        (*name).to_owned()
                    } else {
                        format!("{text}, {name}")
                    };
                    let more = if rest > 0 {
                        format!(" +{rest}")
                    } else {
                        String::new()
                    };
                    if !text.is_empty() && format!("{next}{more}").width() > room {
                        text = format!("{text} +{}", names.len() - at);
                        break;
                    }
                    text = next;
                }
                Rich::new()
                    .plain(&" ".repeat(indent))
                    .push(
                        format!("{} ", glyph(Glyph::FoldClosed, mode)),
                        role(GitRole::Muted),
                    )
                    .push(format!("{mark} "), role(tone))
                    .push(
                        super::super::shell::chrome::ellipsize(&text, room),
                        role(GitRole::Muted),
                    )
                    .line()
            };
            row.action = Some(Action::Collapse(id, key));
            rows.push(row);
        }
    }
    let running = record
        .checks
        .iter()
        .filter(|check| matches!(check.state, CiState::Pending | CiState::ActionRequired))
        .count();
    if running > 0 {
        rows.push(blank());
        rows.push(muted(&format!("{running} running · R refreshes")));
    }
}

/// The selected job: one breadcrumb beside the list (`Quality checks / Python tests · 3s ·
/// passed`; stacked under the list, a muted `Python tests · steps` heading, so a step named
/// like its job never reads as one more check, r16 prs-08), then its steps and log, or, for
/// a check with no run (a commit status, outside CI), where its details are.
#[allow(clippy::too_many_arguments)]
fn job(
    workspace: &Workspace,
    id: u64,
    check: &CheckRow,
    rows: &mut Vec<Line>,
    room: usize,
    beside: bool,
    mode: ExplorerIconMode,
) {
    let (workflow, own) = check_parts(check);
    let (own, _) = split_event(own);
    let (mark, _, tone) = ci_mark(check.state, mode);
    let found = workspace.check_job(check);
    if beside {
        let time = found
            .as_ref()
            .and_then(runs::check_job::job_time)
            .or_else(|| {
                shown_duration(check).map(|seconds| (records::duration(seconds), GitRole::Muted))
            });
        let mut title = Rich::new().push(format!("{mark} "), role(tone));
        if !workflow.is_empty() {
            title = title.push(format!("{workflow} / "), role(GitRole::Muted));
        }
        title = title.push(own, strong(GitRole::Text));
        if let Some((time, time_role)) =
            time.filter(|(time, _)| time != "running" && time != "queued")
        {
            title = title.sep().push(time, role(time_role));
        }
        title = title.sep().push(state_word(&check.word), role(tone));
        if check.required {
            title = title.sep().push("required", role(GitRole::Modified));
        }
        rows.push(title.line());
        rows.push(blank());
    } else if found.is_some() {
        rows.push(
            Rich::new()
                .push(own, strong(GitRole::Text))
                .sep()
                .push("steps", role(GitRole::Muted))
                .line(),
        );
    }
    if let Some(job) = found {
        let job_id = number(&job, "databaseId");
        let open = |step: u64, default: bool| workspace.step_open(id, job_id, step, default);
        for (step, mut line) in workspace.job_rows(&job, &open, room, mode) {
            if let Some(step) = step {
                line.action = Some(Action::Collapse(id, step_key(job_id, step)));
            }
            rows.push(line);
        }
        return;
    }
    if let Some(run) = check.run {
        match workspace.cache.get(&Key::Run(run, 0)) {
            // Calm, in the job's place: never the forge's status line. A run the forge
            // no longer has is GitHub's "logs … have expired"; a sign-in names the key.
            Some(Err(error)) => {
                let words = workspace.explain(error);
                rows.push(muted(&if words.headline.starts_with("Not found") {
                    // Short enough for the narrowest job pane (R16: it was cut mid-word at 62).
                    "This run's logs have expired".to_owned()
                } else {
                    format!("Steps and log unavailable · {}", words.line())
                }));
            }
            Some(Ok(_)) => rows.push(muted(&format!(
                "This job is not in the {}'s latest attempt",
                workspace.terms().run_noun
            ))),
            None => rows.push(muted(&format!(
                "Loading the {}…",
                workspace.terms().run_noun
            ))),
        }
        return;
    }
    // A commit status or outside CI: the forge has no run to read.
    rows.push(muted(&format!(
        "No {} here · details are on the check's own page",
        workspace.terms().run_noun
    )));
    if check.url.is_empty() {
        rows.push(muted("The forge gave no link for it."));
        return;
    }
    rows.push(blank());
    let mut row = Rich::new()
        .push(
            format!("View details {}", glyph(Glyph::Link, mode)),
            role(GitRole::Link),
        )
        .line();
    row.action = Some(Action::Check(id, format!("{}{DETAILS}", identity(check))));
    rows.push(row);
}
