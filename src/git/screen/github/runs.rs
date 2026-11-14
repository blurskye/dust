//! Actions (DESIGN.md §3.37–§3.41): the runs table beside its filter rail and a run summary,
//! the run › jobs › steps › log drilldown, the full job log with find, pan and wrap, the
//! rerun / cancel / dispatch / approve sheets and a run's artifacts. GitLab's words come
//! from `Terms` (pipelines).
use super::super::shell::chrome::{self, Span, bold, st};
use super::super::shell::glyphs::{Glyph, glyph};
use super::super::shell::layout::{self, Mode, Rect};
use super::super::{FormAction, GitScreen};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::records;
use crate::ui::{CellGrid, Emphasis, Frame, GitRole, GitSurface, Style};
use crossterm::cursor::SetCursorStyle;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PANE: GitSurface = GitSurface::Pane;
/// The runs page's filter rail, borders included (20 interior cells).
const RAIL: u16 = 22;
/// Drilldown columns (Runs · Jobs · Steps), borders included, and the narrowest they shrink to.
const COLUMNS: [u16; 3] = [22, 22, 28];
const SHRUNK: u16 = 14;
/// The log keeps at least this much room before the columns left of focus shrink.
const LOG_ROOM: u16 = 80;
const PAN: usize = 8;

/// A one-line text input on the screen itself (run filter, log find).
#[derive(Default)]
pub(super) struct Input {
    pub(super) query: String,
    pub(super) editing: bool,
}

/// What the Actions tab shows and where its cursors are. Runs, jobs and steps are kept by
/// identity (database id, step number) so a refresh that reorders them keeps the selection.
#[derive(Default)]
pub(super) struct View {
    /// The run › jobs › steps › log drilldown is open.
    pub(super) drill: bool,
    /// Drilldown column with focus: 0 runs, 1 jobs, 2 steps, 3 log.
    pub(super) column: usize,
    /// Drilldown tab: 0 Jobs, 1 Artifacts, 2 Logs (the full job log).
    pub(super) tab: usize,
    rail: usize,
    branch: Option<String>,
    event: Option<String>,
    job: Option<u64>,
    step: Option<u64>,
    attempts: bool,
    attempt_cursor: usize,
    log_identity: String,
    log_cursor: usize,
    log_scroll: usize,
    pan: usize,
    wrap: bool,
    /// The find's current match pulls the pan to it (until h/l or a cursor move).
    follow_match: bool,
    filter: Input,
    find: Input,
    find_current: usize,
    artifact: usize,
    receipt: Option<String>,
    pending_dispatch: Option<u64>,
    dispatched: Option<(u64, String, BTreeSet<u64>)>,
    cancelling: BTreeSet<u64>,
    log_rows: usize,
    /// The runs page painted its summary pane last frame (it reads the failed job's log).
    summary: bool,
}

// ------------------------------------------------------------------------------- states

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Passed,
    Failed,
    Running,
    Queued,
    Waiting,
    Cancelled,
    Skipped,
    Unknown,
}

impl State {
    fn of(value: &Value) -> Self {
        let status = string(value, "status");
        let conclusion = string(value, "conclusion");
        if !conclusion.is_empty() || status == "completed" {
            return match conclusion.to_ascii_lowercase().as_str() {
                "success" | "passed" | "ok" => Self::Passed,
                "failure" | "failed" | "timed_out" | "startup_failure" | "error" => Self::Failed,
                "cancelled" | "canceled" => Self::Cancelled,
                "skipped" | "neutral" | "stale" => Self::Skipped,
                "action_required" | "manual" | "blocked" => Self::Waiting,
                _ => Self::Unknown,
            };
        }
        match status {
            "in_progress" | "running" => Self::Running,
            "waiting" => Self::Waiting,
            "queued" | "pending" | "requested" | "created" => Self::Queued,
            _ => Self::Unknown,
        }
    }
    fn glyph(self, mode: ExplorerIconMode) -> &'static str {
        glyph(
            match self {
                Self::Passed => Glyph::CheckPassed,
                Self::Failed => Glyph::CheckFailed,
                Self::Running => Glyph::Dot,
                Self::Queued | Self::Waiting => Glyph::CheckPending,
                Self::Cancelled | Self::Skipped => Glyph::CheckSkipped,
                Self::Unknown => Glyph::CheckUnknown,
            },
            mode,
        )
    }
    fn role(self) -> GitRole {
        match self {
            Self::Passed => GitRole::Added,
            Self::Failed => GitRole::Removed,
            Self::Running | Self::Waiting => GitRole::Modified,
            _ => GitRole::Muted,
        }
    }
    fn word(self) -> &'static str {
        match self {
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Running => "Running",
            Self::Queued => "Queued",
            Self::Waiting => "Waiting",
            Self::Cancelled => "Cancelled",
            Self::Skipped => "Skipped",
            Self::Unknown => "Unknown",
        }
    }
    fn finished(self) -> bool {
        !matches!(self, Self::Running | Self::Queued | Self::Waiting)
    }
}

fn seconds(from: &str, to: &str) -> Option<i64> {
    Some(records::epoch(to)? - records::epoch(from)?).filter(|seconds| *seconds >= 0)
}

fn duration(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    }
}

fn job_seconds(job: &Value) -> Option<i64> {
    seconds(string(job, "startedAt"), string(job, "completedAt"))
}

/// `2m`, `18m`, `3h`: the table's age column.
fn age(stamp: &str) -> String {
    let text = records::ago(stamp, records::now());
    if text == "just now" {
        "now".into()
    } else {
        text.trim_end_matches(" ago").to_owned()
    }
}

fn pull_event(event: &str) -> bool {
    matches!(
        event,
        "pull_request" | "pull_request_target" | "merge_request_event" | "merge_request"
    )
}

fn manual_event(event: &str) -> bool {
    matches!(event, "workflow_dispatch" | "web" | "api" | "manual")
}

fn capitalized(word: &str) -> String {
    let word = word.replace('_', " ");
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Lines the runner prints that are only section markers (GitLab's `section_end`, GitHub's
/// `##[endgroup]`) are dropped; a GitLab `section_start` keeps its heading text.
fn gitlab_marker(text: &str) -> Option<(String, bool)> {
    let at = text.find("section_start:").or_else(|| text.find("section_end:"))?;
    let start = text[at..].starts_with("section_start:");
    let mut rest = &text[at..];
    rest = rest.split_once(':').map_or("", |(_, rest)| rest);
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || "_.-:".contains(c)))
        .unwrap_or(rest.len());
    rest = &rest[name_end..];
    if rest.starts_with('[') {
        rest = rest.find(']').map_or("", |end| &rest[end + 1..]);
    }
    let shown = format!("{}{}", &text[..at], rest);
    Some((shown, start))
}

// ------------------------------------------------------------------------------- log rows

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tone {
    Text,
    Error,
    Command,
    Heading,
}

struct LogLine {
    text: String,
    colours: Vec<(Range<usize>, logs::Sgr)>,
    tone: Tone,
    link: Option<(Range<usize>, String, u32)>,
}

impl LogLine {
    fn new(payload: &str) -> Option<Self> {
        let raw = payload.trim_start_matches('\u{feff}');
        let heading = logs::line_kind(raw) == logs::LineKind::GroupHeading;
        let (mut text, mut colours) = logs::styled(raw);
        if text.is_empty() && logs::display(raw).is_empty() && raw.contains("##[endgroup]") {
            return None;
        }
        let mut tone = if heading { Tone::Heading } else { Tone::Text };
        if let Some((shown, start)) = gitlab_marker(&text) {
            if !start && shown.trim().is_empty() {
                return None;
            }
            text = shown;
            colours.clear();
            if start {
                tone = Tone::Heading;
            }
        }
        let trimmed = text.trim_start();
        if tone == Tone::Text {
            let lower = trimmed.to_ascii_lowercase();
            if trimmed.starts_with("$ ") {
                tone = Tone::Command;
            } else if lower.starts_with("error")
                || lower.starts_with("fail")
                || lower.starts_with("fatal")
                || trimmed.starts_with("AssertionError")
                || lower.contains("panicked at")
            {
                tone = Tone::Error;
            }
        }
        let link = link(&text);
        Some(Self {
            text,
            colours,
            tone,
            link,
        })
    }
}

/// The first `path/to/file.ext:LINE` in a line, as a byte range, the path and the line.
fn link(text: &str) -> Option<(Range<usize>, String, u32)> {
    let path_char = |c: char| c.is_ascii_alphanumeric() || "_./-".contains(c);
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < text.len() {
        let Some(colon) = text[at..].find(':').map(|found| at + found) else {
            break;
        };
        let start = text[..colon]
            .char_indices()
            .rev()
            .take_while(|(_, c)| path_char(*c))
            .last()
            .map_or(colon, |(index, _)| index);
        let digits = text[colon + 1..]
            .find(|c: char| !c.is_ascii_digit())
            .map_or(text.len(), |end| colon + 1 + end);
        let path = &text[start..colon];
        let file = path.rsplit('/').next().unwrap_or("");
        if digits > colon + 1
            && file.contains('.')
            && !file.starts_with('.')
            && !file.ends_with('.')
            && file.rsplit('.').next().is_some_and(|ext| {
                (1..=6).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
            })
            && !path.chars().all(|c| c.is_ascii_digit() || c == '.')
            && (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
        {
            let line = text[colon + 1..digits].parse().unwrap_or(1);
            return Some((start..digits, path.to_owned(), line));
        }
        at = colon + 1;
    }
    None
}

fn sgr_role(color: u8) -> GitRole {
    match color % 8 {
        1 => GitRole::Removed,
        2 => GitRole::Added,
        3 => GitRole::Modified,
        4 => GitRole::Link,
        5 => GitRole::Ref,
        6 => GitRole::Focus,
        7 if color < 8 => GitRole::Text,
        7 => GitRole::Text,
        _ => GitRole::Muted,
    }
}

/// Smart case: a query with capitals matches exactly, otherwise case is ignored.
fn matches(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = !query.chars().any(char::is_uppercase);
    let (haystack, needle) = if fold {
        (text.to_lowercase(), query.to_lowercase())
    } else {
        (text.to_owned(), query.to_owned())
    };
    if haystack.len() != text.len() {
        // Lowercasing changed byte lengths; fall back to an exact search.
        return text
            .match_indices(query)
            .map(|(at, found)| at..at + found.len())
            .collect();
    }
    haystack
        .match_indices(&needle)
        .map(|(at, found)| at..at + found.len())
        .collect()
}

/// A step of the selected job: from the forge's step list, or from the log's own sections
/// when the forge lists none (GitLab, a job without `steps[]`).
struct StepRow {
    number: u64,
    name: String,
    state: State,
    seconds: Option<i64>,
    value: Option<Value>,
    section: Option<Range<usize>>,
}

enum Body {
    Lines {
        lines: Vec<LogLine>,
        note: Option<String>,
    },
    Message(String, GitRole),
}

// ------------------------------------------------------------------------------- data

impl Workspace {
    fn actions_noun(&self) -> &'static str {
        self.terms().run_noun
    }

    fn run_word(&self) -> &'static str {
        if self.terms().run_noun == "pipeline" {
            "Pipeline"
        } else {
            "Run"
        }
    }

    /// The runs the table shows: the workflow's runs, then the local branch, event and text
    /// filters.
    fn shown_runs(&self) -> Vec<&Value> {
        let view = &self.actions;
        let query = view.filter.query.to_lowercase();
        self.runs()
            .into_iter()
            .filter(|run| {
                view.branch
                    .as_ref()
                    .is_none_or(|branch| string(run, "headBranch") == branch)
                    && view
                        .event
                        .as_ref()
                        .is_none_or(|event| self.event_kind(string(run, "event")) == *event)
                    && (query.is_empty()
                        || [
                            string(run, "displayTitle"),
                            string(run, "workflowName"),
                            string(run, "headBranch"),
                        ]
                        .iter()
                        .any(|text| text.to_lowercase().contains(&query))
                        || format!("#{}", run_number(run)).contains(&query))
            })
            .collect()
    }

    fn run_cursor(&self, runs: &[&Value]) -> usize {
        self.run_id
            .and_then(|id| runs.iter().position(|run| number(run, "databaseId") == id))
            .unwrap_or_else(|| self.run_selected.min(runs.len().saturating_sub(1)))
    }

    /// The selected run as best known: its detail for the chosen attempt, else its row.
    fn run_value(&self, id: u64) -> Option<&Value> {
        self.value(&Key::Run(id, self.attempt)).or_else(|| {
            self.runs()
                .into_iter()
                .find(|run| number(run, "databaseId") == id)
        })
    }

    fn run_detail(&self) -> Option<&Value> {
        self.value(&Key::Run(self.run_id?, self.attempt))
    }

    fn jobs(&self) -> Vec<&Value> {
        self.run_detail()
            .map(|run| array(run, "jobs").iter().collect())
            .unwrap_or_default()
    }

    fn job_index(&self, jobs: &[&Value]) -> usize {
        self.actions
            .job
            .and_then(|id| jobs.iter().position(|job| number(job, "databaseId") == id))
            .or_else(|| {
                jobs.iter()
                    .position(|job| State::of(job) == State::Failed)
            })
            .unwrap_or(0)
    }

    fn selected_job(&self) -> Option<&Value> {
        let jobs = self.jobs();
        jobs.get(self.job_index(&jobs)).copied()
    }

    fn steps(&self, job: &Value) -> Vec<StepRow> {
        let listed = array(job, "steps");
        if !listed.is_empty() {
            return listed
                .iter()
                .map(|step| StepRow {
                    number: number(step, "number"),
                    name: string(step, "name").to_owned(),
                    state: State::of(step),
                    seconds: job_seconds(step),
                    value: Some(step.clone()),
                    section: None,
                })
                .collect();
        }
        let Some(raw) = self.raw(&Key::Log(number(job, "databaseId"))) else {
            return Vec::new();
        };
        let text = raw
            .lines()
            .map(|line| logs::unpack(line).1)
            .collect::<Vec<_>>()
            .join("\n");
        let format = if text.contains("section_start:") {
            crate::forge::model::LogFormat::GitLabSections
        } else {
            crate::forge::model::LogFormat::Plain
        };
        let sections = crate::forge::model::logs::sections(&text, format)
            .into_iter()
            .filter(|section| section.depth == 0)
            .collect::<Vec<_>>();
        let job_state = State::of(job);
        let count = sections.len();
        // A step's share runs to the next section: output after `##[endgroup]` is still its own.
        let total = text.lines().count();
        let ends = (0..count)
            .map(|index| sections.get(index + 1).map_or(total, |next| next.start))
            .collect::<Vec<_>>();
        sections
            .into_iter()
            .enumerate()
            .map(|(index, section)| StepRow {
                number: index as u64 + 1,
                name: section.name.clone(),
                state: if job_state == State::Failed && index + 1 < count {
                    State::Passed
                } else {
                    job_state
                },
                seconds: section
                    .started
                    .as_ref()
                    .zip(section.completed.as_ref())
                    .and_then(|(from, to)| seconds(from.as_str(), to.as_str())),
                value: None,
                section: Some(section.start..ends[index].max(section.end)),
            })
            .collect()
    }

    fn step_index(&self, steps: &[StepRow]) -> usize {
        self.actions
            .step
            .and_then(|number| steps.iter().position(|step| step.number == number))
            .or_else(|| steps.iter().position(|step| step.state == State::Failed))
            .unwrap_or(0)
    }

    /// The job whose log the screen needs now: the drilldown's selected, finished job when
    /// its steps, its log or its full log are on screen.
    pub(super) fn wanted_log(&self) -> Option<u64> {
        if self.page != 3 || !self.actions.drill || self.actions.tab == 1 {
            return None;
        }
        let job = self.selected_job()?;
        if !State::of(job).finished() || string(job, "status") != "completed" {
            return None;
        }
        (self.actions.tab == 2 || self.actions.column >= 2 || array(job, "steps").is_empty())
            .then(|| number(job, "databaseId"))
    }

    /// The failed job whose log gives the summary pane its first failure line.
    pub(super) fn summary_log(&self) -> Option<u64> {
        if self.page != 3 || self.actions.drill || !self.actions.summary {
            return None;
        }
        let job = self
            .jobs()
            .into_iter()
            .find(|job| State::of(job) == State::Failed && string(job, "status") == "completed")?;
        self.capabilities().job_log.allowed().then(|| number(job, "databaseId"))
    }

    /// Opens a run's drilldown (Enter on a check that links to its run).
    pub(super) fn open_run(&mut self, id: u64) {
        self.run_id = Some(id);
        self.open_drill();
    }

    pub(super) fn wanted_artifacts(&self) -> Option<u64> {
        (self.page == 3 && self.actions.drill && self.actions.tab == 1)
            .then_some(self.run_id)
            .flatten()
    }

    fn artifacts(&self) -> Vec<&Value> {
        self.run_id
            .and_then(|id| self.value(&Key::Artifacts(id)))
            .map(|value| array(value, "Artifacts").iter().collect())
            .unwrap_or_default()
    }

    pub(super) fn wanted_artifact_files(&self) -> Option<u64> {
        self.wanted_artifacts()?;
        let artifacts = self.artifacts();
        let artifact = artifacts.get(self.actions.artifact.min(artifacts.len().saturating_sub(1)))?;
        (!expired(artifact)).then(|| artifact_id(artifact))
    }

    /// The log pane's rows for the selected job and step (or the whole job).
    fn log_body(&self, job: &Value, step: Option<&StepRow>, full: bool) -> Body {
        let id = number(job, "databaseId");
        match State::of(job) {
            State::Running => {
                return Body::Message("Job running · its log appears when it finishes".into(), GitRole::Modified);
            }
            State::Queued | State::Waiting => {
                return Body::Message("Job queued · nothing has run yet".into(), GitRole::Muted);
            }
            _ => {}
        }
        if !full && let Some(step) = step && step.state == State::Skipped {
            return Body::Message("Skipped · no commands ran".into(), GitRole::Muted);
        }
        let raw = match self.cache.get(&Key::Log(id)) {
            Some(Ok(reply)) => reply.text.as_str(),
            Some(Err(error)) => return Body::Message(error.clone(), GitRole::Removed),
            None if !self.capabilities().job_log.allowed() => {
                return Body::Message(
                    self.capabilities().job_log.reason().unwrap_or("Logs are unavailable").into(),
                    GitRole::Muted,
                );
            }
            None => return Body::Message("Loading log…".into(), GitRole::Muted),
        };
        let mut note = None;
        let payloads: Vec<&str> = if full || step.is_none() {
            raw.lines().map(|line| logs::unpack(line).1).collect()
        } else {
            let step = step.expect("checked");
            if let Some(value) = &step.value {
                let (lines, approximate) = logs::step_lines(raw, value);
                if approximate && !lines.is_empty() {
                    let clock = |key| {
                        let stamp = string(value, key);
                        stamp.get(11..19).filter(|_| stamp.len() >= 19).map(str::to_owned)
                    };
                    note = Some(match (clock("startedAt"), clock("completedAt")) {
                        (Some(from), Some(to)) => format!("Approximate step slice · {from}–{to} UTC"),
                        _ => "Approximate step slice".into(),
                    });
                }
                if lines.is_empty() {
                    return Body::Message(
                        "Step boundaries unavailable · f shows the full job log".into(),
                        GitRole::Muted,
                    );
                }
                lines
            } else {
                let all = raw.lines().map(|line| logs::unpack(line).1).collect::<Vec<_>>();
                let range = step.section.clone().unwrap_or(0..all.len());
                all.get(range.start.min(all.len())..range.end.min(all.len()))
                    .map(<[&str]>::to_vec)
                    .unwrap_or_default()
            }
        };
        let lines = payloads.into_iter().filter_map(LogLine::new).collect::<Vec<_>>();
        if lines.is_empty() {
            return Body::Message("The log is empty".into(), GitRole::Muted);
        }
        Body::Lines { lines, note }
    }

    /// The log on screen now, with the identity its cursor and find belong to.
    fn current_log(&self) -> Option<(String, Body)> {
        let job = self.selected_job()?;
        let full = self.actions.tab == 2;
        let steps = self.steps(job);
        let step = (!full).then(|| steps.get(self.step_index(&steps))).flatten();
        let identity = format!(
            "{}/{}/{}/{}/{}",
            self.run_id.unwrap_or(0),
            self.effective_attempt(),
            number(job, "databaseId"),
            step.map_or(0, |step| step.number),
            full
        );
        Some((identity, self.log_body(job, step, full)))
    }

    /// The attempt on screen: the chosen one, else the latest the run reports.
    fn effective_attempt(&self) -> u64 {
        if self.attempt > 0 {
            u64::from(self.attempt)
        } else {
            self.run_detail().map_or(0, |run| number(run, "attempt"))
        }
    }

    fn latest_attempt(&self) -> u64 {
        let id = self.run_id.unwrap_or(0);
        self.value(&Key::Run(id, 0))
            .map(|run| number(run, "attempt").max(number(run, "attempts")))
            .or_else(|| self.run_value(id).map(|run| number(run, "attempt")))
            .unwrap_or(1)
            .max(self.effective_attempt())
            .max(1)
    }

    fn event_kind(&self, event: &str) -> String {
        if pull_event(event) {
            capitalized(self.terms().pr_singular)
        } else if manual_event(event) {
            "Manual".into()
        } else {
            capitalized(event)
        }
    }

    /// `PR #18`, `MR !18`, `Push`, `Manual`.
    fn event_cell(&self, run: &Value) -> (String, bool) {
        let event = string(run, "event");
        if pull_event(event) {
            let short = if self.terms().pr_glyph == "!" { "MR" } else { "PR" };
            return match run["prNumber"].as_u64() {
                Some(pr) => (format!("{short} {}{pr}", self.terms().pr_glyph), true),
                None => (short.into(), false),
            };
        }
        (self.event_kind(event), false)
    }

    /// The run's duration: the forge's, else its jobs' span once its detail is loaded.
    fn run_seconds(&self, run: &Value) -> Option<i64> {
        if let Some(seconds) = run["durationSeconds"].as_i64() {
            return Some(seconds);
        }
        let id = number(run, "databaseId");
        let detail = self.value(&Key::Run(id, self.attempt)).or_else(|| self.value(&Key::Run(id, 0)))?;
        let jobs = array(detail, "jobs");
        let start = jobs
            .iter()
            .filter_map(|job| records::epoch(string(job, "startedAt")))
            .min()?;
        let end = jobs
            .iter()
            .filter_map(|job| records::epoch(string(job, "completedAt")))
            .max()?;
        (end >= start && jobs.iter().all(|job| string(job, "status") == "completed")).then_some(end - start)
    }

    fn failed_job_name(&self, run: &Value) -> Option<String> {
        let id = number(run, "databaseId");
        let detail = self.value(&Key::Run(id, 0))?;
        array(detail, "jobs")
            .iter()
            .find(|job| State::of(job) == State::Failed)
            .map(|job| string(job, "name").to_owned())
    }

    // --------------------------------------------------------------------------- moves

    /// Keep a run selected once the runs arrive, so its summary loads.
    pub(super) fn sync_actions(&mut self) {
        if self.page != 3 {
            return;
        }
        let runs = self.shown_runs();
        if runs.is_empty() {
            return;
        }
        let at = self.run_cursor(&runs);
        let id = number(runs[at], "databaseId");
        let changed = self.run_id.is_none()
            || !self.actions.drill && self.run_id != Some(id);
        if changed {
            self.run_selected = at;
            self.run_id = Some(id);
            self.ensure(false);
        }
    }

    fn select_run(&mut self, delta: isize) {
        let runs = self.shown_runs();
        if runs.is_empty() {
            return;
        }
        let at = self
            .run_cursor(&runs)
            .saturating_add_signed(delta)
            .min(runs.len() - 1);
        let id = number(runs[at], "databaseId");
        self.run_selected = at;
        if self.run_id != Some(id) {
            self.run_id = Some(id);
            self.attempt = 0;
            self.actions.job = None;
            self.actions.step = None;
            self.actions.attempts = false;
            self.actions.artifact = 0;
        }
        self.ensure(false);
    }

    fn move_job(&mut self, delta: isize) {
        let jobs = self.jobs();
        if jobs.is_empty() {
            return;
        }
        let at = self.job_index(&jobs).saturating_add_signed(delta).min(jobs.len() - 1);
        let id = number(jobs[at], "databaseId");
        if self.actions.job != Some(id) {
            self.actions.job = Some(id);
            self.actions.step = None;
        }
        self.ensure(false);
    }

    fn move_step(&mut self, delta: isize) {
        let Some(job) = self.selected_job() else {
            return;
        };
        let steps = self.steps(job);
        if steps.is_empty() {
            return;
        }
        let at = self.step_index(&steps).saturating_add_signed(delta).min(steps.len() - 1);
        self.actions.step = Some(steps[at].number);
    }

    fn move_log(&mut self, delta: isize) {
        self.actions.follow_match = false;
        let count = match self.current_log() {
            Some((_, Body::Lines { lines, .. })) => lines.len(),
            _ => 0,
        };
        self.actions.log_cursor = self
            .actions
            .log_cursor
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
    }

    fn open_drill(&mut self) {
        if self.run_id.is_none() {
            return;
        }
        let view = &mut self.actions;
        view.drill = true;
        view.column = 1;
        view.tab = 0;
        view.job = None;
        view.step = None;
        view.attempts = false;
        view.find = Input::default();
        self.ensure(false);
    }

    fn rail_rows(&self) -> Vec<(usize, Option<String>)> {
        let mut rows = vec![(0, None)];
        rows.extend(
            self.workflows()
                .iter()
                .map(|workflow| (0, Some(string(workflow, "name").to_owned()))),
        );
        rows.push((1, None));
        let mut branches = Vec::<String>::new();
        let mut events = Vec::<String>::new();
        for run in self.runs() {
            let branch = string(run, "headBranch");
            if !branch.is_empty() && !branches.iter().any(|known| known == branch) {
                branches.push(branch.to_owned());
            }
            let event = self.event_kind(string(run, "event"));
            if !event.is_empty() && !events.contains(&event) {
                events.push(event);
            }
        }
        rows.extend(branches.into_iter().map(|branch| (1, Some(branch))));
        rows.push((2, None));
        rows.extend(events.into_iter().map(|event| (2, Some(event))));
        rows
    }

    fn choose_rail(&mut self) {
        let rows = self.rail_rows();
        let Some((section, value)) = rows.get(self.actions.rail.min(rows.len().saturating_sub(1))).cloned()
        else {
            return;
        };
        match section {
            0 => {
                self.workflow_selected = self
                    .workflows()
                    .iter()
                    .position(|workflow| Some(string(workflow, "name")) == value.as_deref())
                    .map_or(0, |at| at + 1);
                self.actions.branch = None;
                self.actions.event = None;
            }
            1 => self.actions.branch = value,
            _ => self.actions.event = value,
        }
        self.run_selected = 0;
        self.run_id = None;
        self.attempt = 0;
        self.ensure(false);
        self.sync_actions();
    }

    // --------------------------------------------------------------------------- keys

    /// Whether Actions types into its own input (a find or filter row): every key is its.
    pub(in crate::git::screen) fn actions_typing(&self) -> bool {
        self.page == 3 && (self.actions.find.editing || self.actions.filter.editing)
    }

    /// `[`/`]` switch the drilldown's tabs rather than the top tabs.
    pub(in crate::git::screen) fn actions_brackets(&self) -> bool {
        self.page == 3 && self.actions.drill
    }

    fn typing(&mut self, key: &KeyEvent) -> bool {
        let finding = self.actions.find.editing;
        let input = if finding {
            &mut self.actions.find
        } else if self.actions.filter.editing {
            &mut self.actions.filter
        } else {
            return false;
        };
        match key.code {
            KeyCode::Escape => {
                *input = Input::default();
            }
            KeyCode::Enter => {
                input.editing = false;
                if input.query.is_empty() {
                    *input = Input::default();
                }
            }
            KeyCode::Backspace => {
                input.query.pop();
            }
            KeyCode::Char(c) if !key.modifiers.control && !key.modifiers.alt => input.query.push(c),
            _ => return true,
        }
        if finding {
            self.reveal_match(None);
        } else {
            self.run_selected = 0;
            self.sync_actions();
        }
        true
    }

    /// Put the cursor on a match: the first at or after it (`None`), or the next/previous.
    pub(super) fn actions_drilled(&self) -> bool {
        self.actions.drill
    }

    fn reveal_match(&mut self, step: Option<bool>) {
        let Some((_, Body::Lines { lines, .. })) = self.current_log() else {
            return;
        };
        let query = self.actions.find.query.clone();
        let found = lines
            .iter()
            .enumerate()
            .flat_map(|(index, line)| matches(&line.text, &query).into_iter().map(move |range| (index, range)))
            .collect::<Vec<_>>();
        if found.is_empty() {
            self.actions.find_current = 0;
            return;
        }
        let cursor = self.actions.log_cursor;
        let current = match step {
            None => found.iter().position(|(line, _)| *line >= cursor).unwrap_or(0),
            Some(forward) => {
                let now = self.actions.find_current.min(found.len() - 1);
                if forward {
                    (now + 1) % found.len()
                } else {
                    (now + found.len() - 1) % found.len()
                }
            }
        };
        self.actions.find_current = current;
        let (line, range) = &found[current];
        self.actions.log_cursor = *line;
        self.actions.follow_match = true;
        if !self.actions.wrap {
            let start = lines[*line].text[..range.start].width();
            let end = start + lines[*line].text[range.clone()].width();
            let room = self.detail_width.max(20);
            if start < self.actions.pan || end > self.actions.pan + room {
                self.actions.pan = start.saturating_sub(room / 3) / PAN * PAN;
            }
        }
    }

    pub(super) fn actions_key(
        &mut self,
        key: &KeyEvent,
        write_refusal: Option<&str>,
    ) -> Option<Result<Option<Form>, String>> {
        if self.typing(key) {
            return Some(Ok(None));
        }
        let plain = !key.modifiers.control && !key.modifiers.alt;
        if key.code == KeyCode::Char('q') && plain {
            return None;
        }
        self.notice.clear();
        self.notice_error = false;
        let id = self.run_id;
        // Sheets and refresh work everywhere on the tab.
        if plain {
            let sheet = match key.code {
                KeyCode::Char('r') => id.map(Action::Rerun),
                KeyCode::Char('x') => id.map(Action::Cancel),
                KeyCode::Char('A') => id.map(Action::Approve),
                KeyCode::Char('d') => Some(Action::Dispatch(0)),
                _ => None,
            };
            if let Some(action) = sheet {
                if let Some(reason) = write_refusal {
                    return Some(Err(reason.into()));
                }
                return Some(self.activate(action));
            }
            if matches!(key.code, KeyCode::Char('r' | 'x' | 'A')) {
                return Some(Err(format!("Select a {} first", self.actions_noun())));
            }
            if key.code == KeyCode::Char('R') {
                if self.writing() {
                    return Some(Err("Wait for the confirmed operation before refreshing".into()));
                }
                self.ensure(true);
                return Some(Ok(None));
            }
        }
        if !self.actions.drill {
            return Some(self.runs_page_key(key));
        }
        Some(self.drill_key(key, write_refusal))
    }

    fn runs_page_key(&mut self, key: &KeyEvent) -> Result<Option<Form>, String> {
        let control = key.modifiers.control;
        let delta = |code: &KeyCode| match code {
            KeyCode::Home => Some(isize::MIN / 2),
            KeyCode::End => Some(isize::MAX / 2),
            KeyCode::PageDown => Some(20),
            KeyCode::PageUp => Some(-20),
            KeyCode::Char('j') | KeyCode::Down => Some(1),
            KeyCode::Char('k') | KeyCode::Up => Some(-1),
            _ => None,
        };
        if control {
            match key.code {
                KeyCode::Char('j' | 'l') => self.focus = 1,
                KeyCode::Char('k' | 'h') => self.focus = 0,
                _ => {}
            }
            return Ok(None);
        }
        match key.code {
            KeyCode::Char('/') => {
                self.actions.filter = Input {
                    query: String::new(),
                    editing: true,
                };
            }
            KeyCode::Escape if !self.actions.filter.query.is_empty() => {
                self.actions.filter = Input::default();
            }
            KeyCode::Char('1') => self.focus = 0,
            KeyCode::Char('2') => self.focus = 1,
            KeyCode::Tab => self.focus = 1 - self.focus.min(1),
            KeyCode::Char('h') | KeyCode::Left if self.focus == 1 => self.focus = 0,
            KeyCode::Char('l') | KeyCode::Right if self.focus == 0 => self.focus = 1,
            KeyCode::Enter | KeyCode::Char(' ') if self.focus == 0 => self.choose_rail(),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.open_drill(),
            _ => {
                if let Some(delta) = delta(&key.code) {
                    if self.focus == 0 {
                        let count = self.rail_rows().len();
                        self.actions.rail = self
                            .actions
                            .rail
                            .saturating_add_signed(delta)
                            .min(count.saturating_sub(1));
                    } else {
                        self.select_run(delta);
                    }
                }
            }
        }
        self.focus = self.focus.min(1);
        self.last_list = self.focus;
        Ok(None)
    }

    fn drill_key(&mut self, key: &KeyEvent, write_refusal: Option<&str>) -> Result<Option<Form>, String> {
        let control = key.modifiers.control;
        let view_tab = self.actions.tab;
        let log_focus = view_tab == 2 || view_tab == 0 && self.actions.column == 3;
        if control {
            match key.code {
                KeyCode::Char('s') if log_focus => {
                    let job = self.selected_job().ok_or("Select a job first")?;
                    return self.activate(Action::SaveLog(number(job, "databaseId")));
                }
                KeyCode::Char('l') if view_tab == 0 => self.actions.column = (self.actions.column + 1).min(3),
                KeyCode::Char('h') if view_tab == 0 => self.actions.column = self.actions.column.saturating_sub(1),
                _ => {}
            }
            self.ensure(false);
            return Ok(None);
        }
        let delta = match key.code {
            KeyCode::Home | KeyCode::Char('g') => Some(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => Some(isize::MAX / 2),
            KeyCode::PageDown => Some(self.actions.log_rows.max(2) as isize - 1),
            KeyCode::PageUp => Some(1 - self.actions.log_rows.max(2) as isize),
            KeyCode::Char('j') | KeyCode::Down => Some(1),
            KeyCode::Char('k') | KeyCode::Up => Some(-1),
            _ => None,
        };
        match key.code {
            KeyCode::Escape => {
                if self.actions.find.active() {
                    self.actions.find = Input::default();
                } else if self.actions.attempts {
                    self.actions.attempts = false;
                } else if view_tab != 0 {
                    self.actions.tab = 0;
                } else if self.actions.column > 1 {
                    self.actions.column -= 1;
                } else {
                    self.actions.drill = false;
                    self.focus = 1;
                }
            }
            KeyCode::Char('[' | ']') => {
                let forward = key.code == KeyCode::Char(']');
                self.actions.tab = (view_tab + if forward { 1 } else { 2 }) % 3;
                self.actions.attempts = false;
                self.actions.find = Input::default();
            }
            KeyCode::Char('f') => {
                if view_tab == 2 {
                    self.actions.tab = 0;
                    self.actions.column = 3;
                } else {
                    self.actions.tab = 2;
                }
                self.actions.find = Input::default();
            }
            KeyCode::Char('a') if view_tab != 1 => {
                let caps = self.capabilities();
                if let Some(reason) = caps.attempts.reason() {
                    return Err(reason.into());
                }
                if self.latest_attempt() <= 1 {
                    return Err(format!("This {} has one attempt", self.actions_noun()));
                }
                self.actions.tab = 0;
                self.actions.attempts = true;
                self.actions.column = 1;
                self.actions.attempt_cursor = (self.latest_attempt() - self.effective_attempt().max(1)) as usize;
            }
            KeyCode::Char('/') if log_focus => {
                self.actions.find = Input {
                    query: String::new(),
                    editing: true,
                };
                self.actions.find_current = 0;
            }
            KeyCode::Char('n' | 'N') if log_focus && self.actions.find.active() => {
                self.reveal_match(Some(key.code == KeyCode::Char('n')));
            }
            KeyCode::Char('w') if log_focus => {
                self.actions.wrap = !self.actions.wrap;
                self.actions.pan = 0;
            }
            KeyCode::Tab if view_tab == 0 => {
                self.actions.column = if key.modifiers.shift {
                    self.actions.column.saturating_sub(1)
                } else {
                    (self.actions.column + 1) % 4
                };
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if log_focus && self.actions.pan > 0 && !self.actions.wrap {
                    self.actions.follow_match = false;
                    self.actions.pan = self.actions.pan.saturating_sub(PAN);
                } else if view_tab == 0 {
                    self.actions.column = self.actions.column.saturating_sub(1);
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if log_focus {
                    if !self.actions.wrap {
                        self.actions.follow_match = false;
                        self.actions.pan = (self.actions.pan + PAN).min(4096);
                    }
                } else if view_tab == 0 {
                    self.actions.column += 1;
                }
            }
            KeyCode::Enter => match (view_tab, self.actions.column) {
                (1, _) => {
                    let artifacts = self.artifacts();
                    let artifact = artifacts
                        .get(self.actions.artifact.min(artifacts.len().saturating_sub(1)))
                        .ok_or("No artifact selected")?;
                    if let Some(reason) = self.capabilities().artifacts_download.reason() {
                        return Err(reason.into());
                    }
                    if expired(artifact) {
                        return Err("This artifact has expired".into());
                    }
                    if let Some(reason) = write_refusal {
                        return Err(reason.into());
                    }
                    let action = Action::Download(artifact_id(artifact), string(artifact, "name").to_owned());
                    return self.activate(action);
                }
                (0, 1) if self.actions.attempts => {
                    let latest = self.latest_attempt();
                    let chosen = latest.saturating_sub(self.actions.attempt_cursor as u64).max(1);
                    self.actions.attempts = false;
                    self.actions.job = None;
                    self.actions.step = None;
                    return self.activate(Action::Attempt(if chosen == latest { 0 } else { chosen as u32 }));
                }
                (0, 3) | (2, _) => return self.open_source(),
                (0, column) => self.actions.column = column + 1,
                _ => {}
            },
            _ => {
                if let Some(delta) = delta {
                    match (view_tab, self.actions.column) {
                        (1, _) => {
                            let count = self.artifacts().len();
                            self.actions.artifact = self
                                .actions
                                .artifact
                                .saturating_add_signed(delta)
                                .min(count.saturating_sub(1));
                        }
                        (2, _) | (0, 3) => self.move_log(delta),
                        (0, 0) => self.select_run(delta),
                        (0, 1) if self.actions.attempts => {
                            let count = self.latest_attempt() as usize;
                            self.actions.attempt_cursor = self
                                .actions
                                .attempt_cursor
                                .saturating_add_signed(delta)
                                .min(count.saturating_sub(1));
                        }
                        (0, 1) => self.move_job(delta),
                        _ => self.move_step(delta),
                    }
                }
            }
        }
        self.actions.column = self.actions.column.min(3);
        self.ensure(false);
        Ok(None)
    }

    /// Enter on a log line with `path:line`: open that file of this repository.
    fn open_source(&mut self) -> Result<Option<Form>, String> {
        let Some((_, Body::Lines { lines, .. })) = self.current_log() else {
            return Ok(None);
        };
        let Some((_, path, line)) = lines
            .get(self.actions.log_cursor)
            .and_then(|row| row.link.clone())
        else {
            return Ok(None);
        };
        let parts = path.split('/').filter(|part| !part.is_empty()).collect::<Vec<_>>();
        for start in 0..parts.len() {
            let candidate = self.root.join(parts[start..].join("/"));
            if candidate.is_file() {
                let _ = line;
                return Err(format!("open:{}", candidate.display()));
            }
        }
        Err(format!("{path} is not in this repository"))
    }

    // --------------------------------------------------------------------------- writes

    /// A finished write the Actions tab reports itself.
    pub(super) fn actions_written(&mut self, write: &Write, summary: &str) {
        match write {
            Write::DownloadArtifact { name, directory, .. } => {
                let size = self
                    .artifacts()
                    .iter()
                    .find(|artifact| string(artifact, "name") == name)
                    .map(|artifact| size(number(artifact, "size")))
                    .unwrap_or_default();
                let shown = directory
                    .strip_prefix(&self.root)
                    .map(|path| format!("./{}", path.join(format!("{name}.zip")).display()))
                    .unwrap_or_else(|_| directory.join(format!("{name}.zip")).display().to_string());
                self.actions.receipt = Some(if size.is_empty() {
                    format!("Downloaded · {shown}")
                } else {
                    format!("Downloaded · {size} saved · {shown}")
                });
                let _ = summary;
            }
            Write::CancelRun(run) => {
                if let Ok(id) = run.0.parse::<u64>() {
                    self.actions.cancelling.insert(id);
                }
            }
            _ => {}
        }
    }

    pub(super) fn note_dispatch(&mut self, workflow: u64, reference: &str) {
        let known = self
            .runs()
            .iter()
            .map(|run| number(run, "databaseId"))
            .collect();
        self.actions.dispatched = Some((workflow, reference.to_owned(), known));
    }

    pub(super) fn begin_dispatch(&mut self, workflow: u64) {
        self.actions.pending_dispatch = Some(workflow);
    }

    pub(super) fn pending_dispatch(&self) -> Option<u64> {
        self.actions.pending_dispatch
    }

    /// Opens a dispatch sheet whose workflow definition just arrived, and correlates a
    /// dispatched run once the runs list shows it.
    pub(super) fn poll_actions(&mut self) {
        if let Some(workflow) = self.actions.pending_dispatch
            && self.ready_form.is_none()
        {
            match self.cache.get(&Key::Workflow(workflow, String::new())) {
                Some(Ok(_)) => {
                    self.actions.pending_dispatch = None;
                    match self.activate(Action::Dispatch(workflow)) {
                        Ok(form) => self.ready_form = form,
                        Err(error) => {
                            self.notice = error;
                            self.notice_error = true;
                        }
                    }
                }
                Some(Err(_)) => {
                    // The sheet opens with a ref alone (`activate` reads the error).
                    self.actions.pending_dispatch = None;
                    match self.activate(Action::Dispatch(workflow)) {
                        Ok(form) => self.ready_form = form,
                        Err(error) => {
                            self.notice = error;
                            self.notice_error = true;
                        }
                    }
                }
                None => {}
            }
        }
        if let Some((workflow, reference, known)) = &self.actions.dispatched
            && !self.writing()
        {
            let found = self
                .runs()
                .into_iter()
                .filter(|run| {
                    let id = number(run, "databaseId");
                    !known.contains(&id)
                        && string(run, "headBranch") == reference
                        && (number(run, "workflowDatabaseId") == *workflow || *workflow == 0)
                })
                .map(|run| number(run, "databaseId"))
                .collect::<Vec<_>>();
            match found.len() {
                0 => {}
                1 => {
                    self.notice = "Dispatched · 1 matching run found after refresh".into();
                    self.run_id = found.first().copied();
                    self.actions.dispatched = None;
                }
                count => {
                    self.notice = format!("Dispatched · ambiguous: {count} runs match · pick one");
                    self.actions.dispatched = None;
                }
            }
        }
        let finished = self
            .actions
            .cancelling
            .iter()
            .copied()
            .filter(|id| {
                self.value(&Key::Run(*id, 0))
                    .is_some_and(|run| string(run, "status") == "completed")
            })
            .collect::<Vec<_>>();
        for id in finished {
            self.actions.cancelling.remove(&id);
        }
    }

    /// Ctrl+s: writes the full job log to a file under the repository.
    pub(super) fn save_log(&mut self, job: u64, values: &[String]) -> Result<(), String> {
        let path = values.first().map(|path| path.trim()).filter(|path| !path.is_empty()).ok_or("A file name is required")?;
        let raw = self.raw(&Key::Log(job)).ok_or("The log is not loaded")?;
        let text = raw
            .lines()
            .filter_map(|line| LogLine::new(logs::unpack(line).1))
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let target = self.root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("Could not create the folder: {error}"))?;
        }
        std::fs::write(&target, text.join("\n") + "\n").map_err(|error| format!("Could not save the log: {error}"))?;
        self.notice = format!("Saved {} lines to {path}", text.len());
        self.notice_error = false;
        Ok(())
    }

    /// The default file for Ctrl+s: `<job>.log`.
    pub(super) fn log_file_name(&self, job: u64) -> String {
        let name = self
            .jobs()
            .into_iter()
            .find(|value| number(value, "databaseId") == job)
            .map(|value| string(value, "name").to_owned())
            .unwrap_or_else(|| format!("job-{job}"));
        let safe = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' })
            .collect::<String>();
        format!("{}.log", safe.trim_matches('-'))
    }

    /// The run a sheet acts on: (workflow name, run number, state, its jobs).
    pub(super) fn sheet_run(&self, id: u64) -> Option<(String, u64, &Value)> {
        let run = self.run_value(id)?;
        Some((string(run, "workflowName").to_owned(), run_number(run), run))
    }

    pub(super) fn run_jobs(&self, id: u64) -> (usize, usize) {
        let detail = self.value(&Key::Run(id, self.attempt)).or_else(|| self.value(&Key::Run(id, 0)));
        let jobs = detail.map(|run| array(run, "jobs")).unwrap_or_default();
        (
            jobs.iter().filter(|job| State::of(job) == State::Failed).count(),
            jobs.len(),
        )
    }

    pub(super) fn run_finished(&self, run: &Value) -> bool {
        string(run, "status") == "completed"
    }

    /// The workflow `d` dispatches: the rail's, else the selected run's.
    pub(super) fn dispatch_workflow(&self) -> Option<u64> {
        let chosen = self.workflow_id();
        if chosen != 0 {
            return Some(chosen);
        }
        let run = self.run_value(self.run_id?)?;
        Some(number(run, "workflowDatabaseId")).filter(|id| *id != 0)
    }

    pub(super) fn workflow_name(&self, id: u64) -> String {
        self.workflows()
            .into_iter()
            .find(|workflow| number(workflow, "id") == id)
            .map(|workflow| string(workflow, "name").to_owned())
            .unwrap_or_else(|| "workflow".into())
    }

    /// Branches the dispatch sheet offers: the default branch, then the runs' branches.
    pub(super) fn dispatch_refs(&self) -> Vec<String> {
        let mut refs = Vec::<String>::new();
        if let Some(branch) = self
            .value(&Key::Repository)
            .and_then(|value| value["defaultBranchRef"]["name"].as_str())
        {
            refs.push(branch.to_owned());
        }
        for run in self.runs() {
            let branch = string(run, "headBranch");
            if !branch.is_empty() && !refs.iter().any(|known| known == branch) && refs.len() < 8 {
                refs.push(branch.to_owned());
            }
        }
        refs
    }
}

impl Input {
    fn active(&self) -> bool {
        self.editing || !self.query.is_empty()
    }
}

fn run_number(run: &Value) -> u64 {
    run["number"]
        .as_u64()
        .filter(|number| *number > 0)
        .unwrap_or_else(|| number(run, "databaseId"))
}

fn artifact_id(artifact: &Value) -> u64 {
    artifact["id"]
        .as_u64()
        .or_else(|| artifact["id"].as_str().and_then(|id| id.parse().ok()))
        .unwrap_or(0)
}

fn expired(artifact: &Value) -> bool {
    artifact["expires"]
        .as_str()
        .and_then(records::epoch)
        .is_some_and(|at| at <= records::now())
}

fn size(bytes: u64) -> String {
    if bytes == 0 {
        String::new()
    } else if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn expires(artifact: &Value) -> (String, GitRole) {
    let Some(at) = artifact["expires"].as_str().and_then(records::epoch) else {
        return ("—".into(), GitRole::Muted);
    };
    let left = at - records::now();
    if left <= 0 {
        ("expired".into(), GitRole::Removed)
    } else if left < 86_400 {
        (format!("in {}h", left / 3600), GitRole::Modified)
    } else {
        (format!("in {} days", left / 86_400), GitRole::Text)
    }
}

// ------------------------------------------------------------------------------- painting

/// A style moved onto another background; chips keep their own.
fn on(style: Style, surface: GitSurface) -> Style {
    match style {
        Style::Git {
            role,
            surface: own,
            emphasis,
        } => Style::Git {
            role,
            surface: if matches!(own, GitSurface::Pane | GitSurface::Canvas) {
                surface
            } else {
                own
            },
            emphasis,
        },
        other => other,
    }
}

fn spans_on(spans: &[Span], surface: GitSurface) -> Vec<Span> {
    spans
        .iter()
        .map(|(text, style)| (text.clone(), on(*style, surface)))
        .collect()
}

/// One list row: `left` from the start, `right` flush with the end.
struct Item {
    left: Vec<Span>,
    right: Vec<Span>,
    /// A wrapped name's second row.
    more: Option<Vec<Span>>,
}

impl Item {
    fn new(left: Vec<Span>, right: Vec<Span>) -> Self {
        Self {
            left,
            right,
            more: None,
        }
    }
    fn height(&self) -> u16 {
        1 + u16::from(self.more.is_some())
    }
}

/// Rows with a cursor that scrolls into view; returns the cursor's row.
fn paint_items(
    grid: &mut CellGrid,
    inner: Rect,
    items: &[Item],
    cursor: Option<usize>,
    focused: bool,
    mode: ExplorerIconMode,
) -> Option<u16> {
    if inner.h == 0 || inner.w < 3 {
        return None;
    }
    let mut offset = 0;
    if let Some(cursor) = cursor {
        while offset < cursor
            && items[offset..=cursor.min(items.len().saturating_sub(1))]
                .iter()
                .map(Item::height)
                .sum::<u16>()
                > inner.h
        {
            offset += 1;
        }
    }
    let mut y = inner.y;
    let mut cursor_y = None;
    for (index, item) in items.iter().enumerate().skip(offset) {
        if y >= inner.bottom() {
            break;
        }
        let selected = cursor == Some(index);
        let surface = if selected {
            if focused {
                GitSurface::Cursor
            } else {
                GitSurface::CursorInactive
            }
        } else {
            PANE
        };
        let rows = item.height().min(inner.bottom() - y);
        if selected {
            chrome::fill(grid, Rect::new(inner.x, y, inner.w, rows), st(GitRole::Text, surface));
            grid.put_text_clipped(
                inner.x,
                y,
                glyph(Glyph::FoldClosed, mode),
                bold(GitRole::Focus, surface),
                inner.right(),
            );
            cursor_y = Some(y);
        }
        let right_width = chrome::spans_width(&item.right);
        let end = inner.right().saturating_sub(right_width + u16::from(right_width > 0));
        chrome::put_spans(grid, inner.x + 1, y, &spans_on(&item.left, surface), end.max(inner.x + 1));
        if right_width > 0 && inner.x + 2 + right_width < inner.right() {
            chrome::put_spans(
                grid,
                inner.right() - right_width,
                y,
                &spans_on(&item.right, surface),
                inner.right(),
            );
        }
        if let Some(more) = &item.more
            && rows > 1
        {
            chrome::put_spans(grid, inner.x + 1, y + 1, &spans_on(more, surface), inner.right());
        }
        y += rows;
    }
    cursor_y
}

/// Clip `spans` to `cells` columns with an ellipsis.
fn clip(spans: Vec<Span>, cells: u16) -> Vec<Span> {
    let mut out = Vec::new();
    let mut used = 0u16;
    for (text, style) in spans {
        let w = chrome::width(&text);
        if used + w <= cells {
            used += w;
            out.push((text, style));
        } else {
            let room = usize::from(cells.saturating_sub(used));
            if room > 0 {
                out.push((chrome::ellipsize(&text, room), style));
            }
            break;
        }
    }
    out
}

/// A name that wraps onto a second row rather than truncating (§3.38 step rows).
fn wrap_name(name: &str, cells: usize) -> (String, Option<String>) {
    if name.width() <= cells || cells < 4 {
        return (name.to_owned(), None);
    }
    let mut first = String::new();
    let mut used = 0;
    let mut split = 0;
    for (at, grapheme) in name.grapheme_indices(true) {
        let w = grapheme.width();
        if used + w > cells {
            break;
        }
        used += w;
        first.push_str(grapheme);
        split = at + grapheme.len();
        if grapheme == " " {
            // Remember the last space as a word break.
        }
    }
    let (head, tail) = match first.rfind(' ').filter(|at| *at > cells / 3) {
        Some(space) => (name[..space].to_owned(), name[space + 1..].to_owned()),
        None => (name[..split].to_owned(), name[split..].to_owned()),
    };
    (head, Some(chrome::ellipsize(&tail, cells)))
}

fn pane_title(text: &str, role: GitRole) -> Vec<Span> {
    vec![(text.to_owned(), bold(role, PANE))]
}

pub(in crate::git::screen) fn frame(app: &mut GitScreen, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    if layout::mode(width, height) == Mode::TooSmall {
        let text = format!("Dusty Git needs {}×{}", layout::MIN_WIDTH, layout::MIN_HEIGHT);
        frame.grid.put_text_clipped(
            width.saturating_sub(chrome::width(&text)) / 2,
            height / 2,
            &text,
            bold(GitRole::Modified, GitSurface::Canvas),
            width,
        );
        return frame;
    }
    chrome::fill(&mut frame.grid, Rect::new(0, 0, width, height), st(GitRole::Text, GitSurface::Canvas));
    super::super::shell::top_bar(app, &mut frame.grid, width);
    app.github.sync_actions();
    let mode = app.icon_mode;
    let footer_rect = Rect::new(0, height - 1, width, 1);
    if app.github.repo.is_empty() || !app.github.cache.contains_key(&Key::Repository) {
        let body = Rect::new(0, 1, width, height - 2);
        chrome::pane(&mut frame.grid, body, &pane_title(app.github.terms().actions_tab, GitRole::Text), "", true);
        let (text, role) = match app.github.cache.get(&Key::Repository) {
            Some(Err(error)) => (error.clone(), GitRole::Removed),
            _ => ("Loading the repository…".to_owned(), GitRole::Muted),
        };
        frame.grid.put_text_clipped(2, 2, &text, st(role, PANE), width - 2);
    } else if app.github.actions.drill {
        drill(&mut frame, &mut app.github, width, height, mode);
    } else {
        runs_page(&mut frame, &mut app.github, width, height, mode);
    }
    footer(app, &mut frame, footer_rect);
    super::accounts::paint(&mut frame, &app.github, 0, width, height);
    if let Some(form) = &app.form {
        let lay = layout::layout(width, height, false, [false; 4]);
        if !super::super::shell::sheet::paint(app, &mut frame, &lay) {
            super::super::render::render_form(&mut frame, form, width, height);
        }
    }
    frame
}

// ------------------------------------------------------------------------------- runs page

fn runs_page(frame: &mut Frame, ws: &mut Workspace, width: u16, height: u16, mode: ExplorerIconMode) {
    let body = Rect::new(0, 1, width, height - 2);
    let narrow = width < 100;
    let (rail, table, summary) = if narrow {
        if ws.focus == 0 {
            (Some(body), None, None)
        } else {
            (None, Some(body), None)
        }
    } else if width >= 120 {
        let side = (width * 32 / 100).clamp(38, 64);
        (
            Some(Rect::new(0, 1, RAIL, body.h)),
            Some(Rect::new(RAIL, 1, width - RAIL - side, body.h)),
            Some(Rect::new(width - side, 1, side, body.h)),
        )
    } else {
        (
            Some(Rect::new(0, 1, RAIL, body.h)),
            Some(Rect::new(RAIL, 1, width - RAIL, body.h)),
            None,
        )
    };
    if let Some(rect) = rail {
        paint_rail(frame, ws, rect, mode);
    }
    if let Some(rect) = table {
        paint_table(frame, ws, rect, summary.is_none(), mode);
    }
    if let Some(rect) = summary {
        paint_summary(frame, ws, rect, mode);
    }
    if ws.actions.summary != summary.is_some() {
        ws.actions.summary = summary.is_some();
        ws.ensure(false);
    }
}

fn paint_rail(frame: &mut Frame, ws: &Workspace, rect: Rect, mode: ExplorerIconMode) {
    let rows = ws.rail_rows();
    let focused = ws.focus == 0;
    let all_runs = ws.value(&Key::Runs(0)).and_then(Value::as_array);
    let count_of = |name: &str| {
        all_runs.map(|runs| {
            runs.iter()
                .filter(|run| string(run, "workflowName") == name)
                .count()
        })
    };
    let titles = [
        (Glyph::Actions, if ws.terms().run_noun == "pipeline" { "Sources" } else { "Workflows" }),
        (Glyph::Branch, "Branches"),
        (Glyph::Search, "Events"),
    ];
    let wanted = (0..3)
        .map(|section| rows.iter().filter(|(at, _)| *at == section).count() as u16 + 2)
        .collect::<Vec<_>>();
    let mut heights = wanted.clone();
    let total: u16 = heights.iter().sum();
    if total > rect.h {
        let mut spare = rect.h.saturating_sub(9);
        for height in heights.iter_mut() {
            let take = (*height - 3).min(spare / 2 + 1).min(spare);
            *height = 3 + take;
            spare -= take;
        }
        let used: u16 = heights.iter().sum();
        heights[2] += rect.h.saturating_sub(used);
    } else {
        heights[2] += rect.h - total;
    }
    let selected_rail = ws.actions.rail.min(rows.len().saturating_sub(1));
    let mut y = rect.y;
    let mut flat = 0usize;
    for section in 0..3 {
        let pane = Rect::new(rect.x, y, rect.w, heights[section].min(rect.bottom().saturating_sub(y)));
        y += pane.h;
        let (icon, title) = titles[section];
        let section_rows = rows
            .iter()
            .filter(|(at, _)| *at == section)
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let here = selected_rail >= flat && selected_rail < flat + section_rows.len();
        chrome::pane(
            &mut frame.grid,
            pane,
            &[(format!("{} {title}", glyph(icon, mode)), bold(GitRole::Text, PANE))],
            "",
            focused && here,
        );
        let chosen = |value: &Option<String>| match section {
            0 => {
                ws.workflow_selected
                    == value.as_ref().map_or(0, |name| {
                        ws.workflows()
                            .iter()
                            .position(|workflow| string(workflow, "name") == name)
                            .map_or(usize::MAX, |at| at + 1)
                    })
            }
            1 => ws.actions.branch == *value,
            _ => ws.actions.event == *value,
        };
        let items = section_rows
            .iter()
            .map(|value| {
                let label = value.clone().unwrap_or_else(|| match section {
                    0 if ws.terms().run_noun == "pipeline" => "All sources".into(),
                    0 => "All workflows".into(),
                    1 => "All branches".into(),
                    _ => "All events".into(),
                });
                let mark = if chosen(value) { "● " } else { "  " };
                let role = if section == 1 && value.is_some() { GitRole::Ref } else { GitRole::Text };
                let count = match (section, value) {
                    (0, None) => all_runs.map(|runs| runs.len().to_string()),
                    (0, Some(name)) => count_of(name).map(|count| count.to_string()),
                    _ => None,
                };
                let room = pane.w.saturating_sub(6 + count.as_ref().map_or(0, |c| chrome::width(c) + 1));
                Item::new(
                    vec![
                        (mark.to_owned(), bold(GitRole::Focus, PANE)),
                        (chrome::ellipsize(&label, usize::from(room)), st(role, PANE)),
                    ],
                    count.map(|count| vec![(count, st(GitRole::Muted, PANE))]).unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let cursor = (here).then(|| selected_rail - flat);
        let inner = pane.inner();
        if let Some(row) = paint_items(&mut frame.grid, inner, &items, cursor, focused, mode)
            && focused
        {
            frame.cursor = Some((inner.x, row, SetCursorStyle::SteadyBar));
        }
        flat += section_rows.len();
    }
}

fn paint_table(frame: &mut Frame, ws: &mut Workspace, rect: Rect, compact: bool, mode: ExplorerIconMode) {
    let focused = ws.focus == 1;
    let runs = ws.shown_runs();
    let all = ws.runs().len();
    let running = runs.iter().filter(|run| State::of(run) == State::Running).count();
    let failed = runs.iter().filter(|run| State::of(run) == State::Failed).count();
    let noun = ws.actions_noun();
    let mut count = format!("{} {noun}{}", runs.len(), if runs.len() == 1 { "" } else { "s" });
    if runs.len() != all {
        count = format!("{} of {all} {noun}s", runs.len());
    }
    if running > 0 {
        count.push_str(&format!(" · {running} running"));
    }
    if failed > 0 {
        count.push_str(&format!(" · {failed} failed"));
    }
    let title = if noun == "pipeline" { "Pipelines" } else { "Workflow runs" };
    chrome::pane(
        &mut frame.grid,
        rect,
        &[(format!("{} {title}", glyph(Glyph::Actions, mode)), bold(GitRole::Text, PANE))],
        &count,
        focused,
    );
    let inner = rect.inner();
    let mut y = inner.y;
    let filter = &ws.actions.filter;
    if filter.active() {
        let text = format!("{} {}{}", glyph(Glyph::Search, mode), filter.query, if filter.editing { "▏" } else { "" });
        frame.grid.put_text_clipped(inner.x + 1, y, &text, bold(GitRole::Modified, PANE), inner.right());
        if filter.editing {
            frame.cursor = Some((inner.x + 3 + chrome::width(&filter.query), y, SetCursorStyle::SteadyBar));
        }
        y += 1;
    }
    let key = Key::Runs(ws.workflow_id());
    if runs.is_empty() {
        let (text, role) = match ws.cache.get(&key) {
            Some(Err(error)) => (error.clone(), GitRole::Removed),
            None => ("Loading…".into(), GitRole::Muted),
            Some(Ok(_)) if all > 0 => ("No runs match these filters".into(), GitRole::Muted),
            Some(Ok(_)) => (format!("No {noun}s yet"), GitRole::Muted),
        };
        frame.grid.put_text_clipped(inner.x + 1, y + 1, &text, bold(role, PANE), inner.right());
        if focused && frame.cursor.is_none() {
            frame.cursor = Some((inner.x + 1, y + 1, SetCursorStyle::SteadyBlock));
        }
        return;
    }
    // Columns: mark+glyph, #n, workflow, branch, event, age, time.
    let number_w = runs.iter().map(|run| format!("#{}", run_number(run)).len()).max().unwrap_or(2).max(3) as u16;
    let event_w = 7u16;
    let age_w = 4u16;
    let time_w = 7u16;
    let fixed = 2 + 1 + number_w + 1 + event_w + 1 + age_w + 1 + time_w + 1;
    let flex = inner.w.saturating_sub(fixed + 2);
    let workflow_w = (flex * 45 / 100).max(8);
    let branch_w = flex.saturating_sub(workflow_w + 1);
    let columns = [
        (2u16, ""),
        (number_w, if noun == "pipeline" { "Pipe" } else { "Run" }),
        (workflow_w, if noun == "pipeline" { "Source" } else { "Workflow" }),
        (branch_w, "Branch"),
        (event_w, "Event"),
        (age_w, "Age"),
        (time_w, "Time"),
    ];
    let mut x = inner.x + 1;
    for (w, label) in columns {
        if w == 0 {
            continue;
        }
        frame.grid.put_text_clipped(x, y, label, bold(GitRole::Text, PANE), inner.right());
        x += w + 1;
    }
    y += 1;
    for column in inner.x..inner.right() {
        frame.grid.set_cell(column, y, "─", st(GitRole::Border, PANE));
    }
    y += 1;
    let list = Rect::new(inner.x, y, inner.w, inner.bottom().saturating_sub(y));
    let items = runs
        .iter()
        .map(|run| {
            let state = State::of(run);
            let mut workflow = string(run, "workflowName").to_owned();
            if workflow.is_empty() {
                workflow = string(run, "displayTitle").to_owned();
            }
            let mut spans: Vec<Span> = vec![
                (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
                (format!("{:<w$} ", format!("#{}", run_number(run)), w = usize::from(number_w)), st(GitRole::Text, PANE)),
            ];
            let failed_job = compact.then(|| ws.failed_job_name(run)).flatten();
            let mut cell = clip(vec![(workflow, st(GitRole::Text, PANE))], workflow_w);
            if let Some(job) = &failed_job {
                // Narrow: the summary is gone, so the row names what failed.
                cell = clip(
                    vec![
                        (string(run, "workflowName").to_owned(), st(GitRole::Text, PANE)),
                        (format!(" · {job} "), st(GitRole::Muted, PANE)),
                        (glyph(Glyph::CheckFailed, mode).to_owned(), st(GitRole::Removed, PANE)),
                    ],
                    workflow_w + if branch_w > 12 { branch_w / 2 } else { 0 },
                );
            }
            let cell_w = chrome::spans_width(&cell);
            let target = if failed_job.is_some() && branch_w > 12 { workflow_w + branch_w / 2 } else { workflow_w };
            spans.extend(cell);
            spans.push((" ".repeat(usize::from(target.saturating_sub(cell_w)) + 1), st(GitRole::Text, PANE)));
            let branch_room = if target > workflow_w { branch_w - (target - workflow_w) } else { branch_w };
            let branch = chrome::ellipsize(string(run, "headBranch"), usize::from(branch_room));
            spans.push((format!("{branch:<w$} ", w = usize::from(branch_room)), st(GitRole::Ref, PANE)));
            let (event, link) = ws.event_cell(run);
            spans.push((
                format!("{:<w$} ", chrome::ellipsize(&event, usize::from(event_w)), w = usize::from(event_w)),
                st(if link { GitRole::Link } else { GitRole::Text }, PANE),
            ));
            spans.push((format!("{:<w$} ", age(string(run, "createdAt")), w = usize::from(age_w)), st(GitRole::Muted, PANE)));
            let (time, role) = match state {
                State::Running => ("Running".to_owned(), GitRole::Modified),
                State::Queued => ("Queued".to_owned(), GitRole::Muted),
                State::Waiting => ("Waiting".to_owned(), GitRole::Modified),
                _ => match ws.run_seconds(run) {
                    Some(seconds) => (duration(seconds), GitRole::Text),
                    None => ("—".to_owned(), GitRole::Muted),
                },
            };
            spans.push((time, st(role, PANE)));
            Item::new(spans, Vec::new())
        })
        .collect::<Vec<_>>();
    let cursor = ws.run_cursor(&runs);
    if let Some(row) = paint_items(&mut frame.grid, list, &items, Some(cursor), focused, mode)
        && focused
        && !ws.actions.filter.editing
    {
        frame.cursor = Some((list.x, row, SetCursorStyle::SteadyBar));
    }
}

/// `(glyph word)` spans for a state.
fn state_spans(state: State, mode: ExplorerIconMode) -> Vec<Span> {
    vec![(format!("{} {}", state.glyph(mode), state.word()), bold(state.role(), PANE))]
}

fn rule(grid: &mut CellGrid, inner: Rect, y: u16) {
    for x in inner.x..inner.right() {
        grid.set_cell(x, y, "─", st(GitRole::Border, PANE));
    }
}

fn paint_summary(frame: &mut Frame, ws: &Workspace, rect: Rect, mode: ExplorerIconMode) {
    let Some(id) = ws.run_id else {
        chrome::pane(&mut frame.grid, rect, &pane_title(ws.run_word(), GitRole::Text), "", false);
        return;
    };
    let Some(run) = ws.run_value(id) else {
        chrome::pane(&mut frame.grid, rect, &pane_title(ws.run_word(), GitRole::Text), "", false);
        return;
    };
    let state = State::of(run);
    let title = vec![
        (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
        (format!("{} #{} · ", ws.run_word(), run_number(run)), bold(GitRole::Text, PANE)),
        (state.word().to_owned(), bold(state.role(), PANE)),
    ];
    chrome::pane(&mut frame.grid, rect, &title, "", false);
    let inner = rect.inner();
    let right = inner.right().saturating_sub(1);
    let x = inner.x + 1;
    let mut y = inner.y;
    let line = |grid: &mut CellGrid, y: u16, spans: Vec<Span>| {
        if y < inner.bottom() {
            chrome::put_spans(grid, x, y, &spans, right);
        }
    };
    line(&mut frame.grid, y, vec![(string(run, "displayTitle").to_owned(), bold(GitRole::Text, PANE))]);
    y += 1;
    line(&mut frame.grid, y, vec![
        (string(run, "workflowName").to_owned(), st(GitRole::Text, PANE)),
        (" · ".into(), st(GitRole::Muted, PANE)),
        (string(run, "headBranch").to_owned(), st(GitRole::Ref, PANE)),
    ]);
    y += 1;
    let (event, _) = ws.event_cell(run);
    let event = if pull_event(string(run, "event")) {
        event.replacen("PR", &capitalized(ws.terms().pr_singular), 1).replacen("MR", &capitalized(ws.terms().pr_singular), 1)
    } else {
        event
    };
    let mut second = vec![
        (format!("{} ", glyph(Glyph::Branch, mode)), st(GitRole::Muted, PANE)),
        (event, st(GitRole::Text, PANE)),
        (format!(" · {}", records::ago(string(run, "createdAt"), records::now())), st(GitRole::Muted, PANE)),
    ];
    if let Some(seconds) = ws.run_seconds(run) {
        second.push((format!(" · {}", duration(seconds)), st(GitRole::Muted, PANE)));
    }
    line(&mut frame.grid, y, second);
    y += 1;
    if ws.actions.cancelling.contains(&id) && !state.finished() {
        line(&mut frame.grid, y, vec![("Cancellation requested".into(), bold(GitRole::Modified, PANE))]);
        y += 1;
    }
    if run["needsApproval"].as_bool() == Some(true) {
        line(&mut frame.grid, y, vec![
            (format!("{} Waiting for approval · ", glyph(Glyph::CheckPending, mode)), st(GitRole::Modified, PANE)),
            ("A".into(), bold(GitRole::Focus, PANE)),
            (format!(" {}", ws.terms().approve_verb.to_lowercase()), st(GitRole::Muted, PANE)),
        ]);
        y += 1;
    }
    let key = Key::Run(id, ws.attempt);
    let detail = match ws.cache.get(&key) {
        Some(Ok(reply)) => &reply.value,
        Some(Err(error)) => {
            line(&mut frame.grid, y + 1, vec![(error.clone(), st(GitRole::Removed, PANE))]);
            line(&mut frame.grid, y + 2, vec![("R retry".into(), st(GitRole::Muted, PANE))]);
            return;
        }
        None => {
            line(&mut frame.grid, y + 1, vec![("Loading jobs…".into(), st(GitRole::Muted, PANE))]);
            return;
        }
    };
    let jobs = array(detail, "jobs");
    rule(&mut frame.grid, inner, y);
    y += 1;
    let passed = jobs.iter().filter(|job| State::of(job) == State::Passed).count();
    let failed = jobs.iter().filter(|job| State::of(job) == State::Failed).count();
    let mut head = vec![("Jobs".to_owned(), bold(GitRole::Text, PANE))];
    if passed > 0 {
        head.push((format!(" · {passed} passed"), bold(GitRole::Text, PANE)));
    }
    if failed > 0 {
        head.push((format!(" · {failed} failed"), bold(GitRole::Text, PANE)));
    }
    if jobs.is_empty() {
        head.push((" · none yet".into(), st(GitRole::Muted, PANE)));
    }
    line(&mut frame.grid, y, head);
    y += 1;
    rule(&mut frame.grid, inner, y);
    y += 1;
    let word_at = right.saturating_sub(18);
    for job in jobs.iter().take(usize::from(inner.bottom().saturating_sub(y)).min(8)) {
        let state = State::of(job);
        line(&mut frame.grid, y, vec![
            (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
            (chrome::ellipsize(string(job, "name"), usize::from(word_at.saturating_sub(x + 3))), st(GitRole::Text, PANE)),
        ]);
        frame.grid.put_text_clipped(word_at, y, state.word(), st(state.role(), PANE), right);
        if let Some(seconds) = job_seconds(job) {
            let text = duration(seconds);
            frame.grid.put_text_clipped(right.saturating_sub(chrome::width(&text)), y, &text, st(GitRole::Text, PANE), right);
        }
        y += 1;
        if state == State::Failed {
            let steps = array(job, "steps");
            let ok = steps.iter().filter(|step| State::of(step) == State::Passed).count();
            let bad = steps.iter().filter(|step| State::of(step) == State::Failed).count();
            if !steps.is_empty() {
                line(&mut frame.grid, y, vec![(format!("  {ok} passed · {bad} failed"), st(GitRole::Muted, PANE))]);
                y += 1;
            }
        }
    }
    let Some(failed_job) = jobs.iter().find(|job| State::of(job) == State::Failed) else {
        return;
    };
    let steps = ws.steps(failed_job);
    if y + 3 < inner.bottom() && !steps.is_empty() {
        rule(&mut frame.grid, inner, y);
        y += 1;
        line(&mut frame.grid, y, vec![
            (string(failed_job, "name").to_owned(), bold(GitRole::Text, PANE)),
            (" · Steps".into(), bold(GitRole::Text, PANE)),
        ]);
        y += 1;
        rule(&mut frame.grid, inner, y);
        y += 1;
        for step in steps.iter() {
            if y + 3 >= inner.bottom() {
                break;
            }
            line(&mut frame.grid, y, vec![
                (format!("{} ", step.state.glyph(mode)), st(step.state.role(), PANE)),
                (chrome::ellipsize(&step.name, usize::from(right.saturating_sub(x + 12))), st(
                    if step.state == State::Skipped { GitRole::Muted } else { GitRole::Text },
                    PANE,
                )),
            ]);
            let text = if step.state == State::Skipped {
                "Skipped".into()
            } else {
                step.seconds.map(duration).unwrap_or_default()
            };
            frame.grid.put_text_clipped(right.saturating_sub(chrome::width(&text)), y, &text, st(GitRole::Muted, PANE), right);
            y += 1;
        }
    }
    // The first failure line, with its source when the log names one.
    let log = ws.raw(&Key::Log(number(failed_job, "databaseId")));
    let failure = log.and_then(|raw| {
        let lines = raw.lines().filter_map(|line| LogLine::new(logs::unpack(line).1)).collect::<Vec<_>>();
        let first = lines.iter().position(|line| line.tone == Tone::Error)?;
        let link = lines[first..].iter().chain(lines[..first].iter().rev()).find_map(|line| line.link.clone());
        Some((lines[first].text.trim().to_owned(), link.map(|(_, path, line)| format!("{path}:{line}"))))
    });
    if y + 1 < inner.bottom() {
        rule(&mut frame.grid, inner, y);
        y += 1;
        match failure {
            Some((text, link)) => {
                for part in crate::ui::render::wrap_plain_text(&text, usize::from(right.saturating_sub(x)).max(10), crate::config::AmbiguousWidth::Narrow)
                    .into_iter()
                    .take(3)
                {
                    line(&mut frame.grid, y, vec![(part, st(GitRole::Removed, PANE))]);
                    y += 1;
                }
                if let Some(link) = link {
                    line(&mut frame.grid, y, vec![
                        (format!("{} ", glyph(Glyph::Link, mode)), st(GitRole::Link, PANE)),
                        (link, st(GitRole::Link, PANE)),
                    ]);
                }
            }
            None => {
                let text = detail["failureSummary"]["line"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{} failed", string(failed_job, "name")));
                line(&mut frame.grid, y, vec![(text, bold(GitRole::Removed, PANE))]);
                line(&mut frame.grid, y + 1, vec![
                    ("Enter".into(), bold(GitRole::Focus, PANE)),
                    (" opens its log".into(), st(GitRole::Muted, PANE)),
                ]);
            }
        }
    }
}

// ------------------------------------------------------------------------------- drilldown

fn breadcrumb(frame: &mut Frame, ws: &Workspace, y: u16, width: u16, mode: ExplorerIconMode) {
    let surface = GitSurface::Canvas;
    let Some(id) = ws.run_id else {
        return;
    };
    let run = ws.run_value(id);
    let mut spans: Vec<Span> = Vec::new();
    let sep = |spans: &mut Vec<Span>| spans.push((" › ".into(), st(GitRole::Muted, surface)));
    if let Some(run) = run {
        let (event, link) = ws.event_cell(run);
        spans.push((format!("{} ", glyph(Glyph::Branch, mode)), st(GitRole::Muted, surface)));
        if link {
            spans.push((
                event.replacen("PR", &capitalized(ws.terms().pr_singular), 1).replacen("MR", &capitalized(ws.terms().pr_singular), 1),
                st(GitRole::Link, surface),
            ));
        } else {
            spans.push((string(run, "headBranch").to_owned(), st(GitRole::Ref, surface)));
        }
        spans.push((" · ".into(), st(GitRole::Muted, surface)));
        spans.push((string(run, "displayTitle").to_owned(), st(GitRole::Text, surface)));
        sep(&mut spans);
        let state = State::of(run);
        spans.push((format!("{} #{}", ws.run_word(), run_number(run)), st(GitRole::Link, surface)));
        spans.push((format!(" {}", state.glyph(mode)), st(state.role(), surface)));
        let attempt = ws.effective_attempt();
        if ws.latest_attempt() > 1 && attempt > 0 {
            spans.push((format!(" · attempt {attempt} of {}", ws.latest_attempt()), st(GitRole::Muted, surface)));
        }
    }
    if ws.actions.tab != 1
        && let Some(job) = ws.selected_job()
    {
        sep(&mut spans);
        spans.push((string(job, "name").to_owned(), st(GitRole::Link, surface)));
        if ws.actions.tab == 2 {
            sep(&mut spans);
            spans.push(("Full job log".into(), st(GitRole::Text, surface)));
        } else if ws.actions.column >= 2 {
            let steps = ws.steps(job);
            if let Some(step) = steps.get(ws.step_index(&steps)) {
                sep(&mut spans);
                spans.push((step.name.clone(), st(GitRole::Link, surface)));
            }
        }
    }
    if ws.actions.tab == 1 {
        sep(&mut spans);
        spans.push(("Artifacts".into(), st(GitRole::Text, surface)));
    }
    // Tabs on the right: Jobs N │ Artifacts N │ Logs.
    let jobs = ws.jobs().len();
    let artifacts = ws.run_id.and_then(|id| ws.value(&Key::Artifacts(id))).map(|value| array(value, "Artifacts").len());
    let tabs = vec![
        format!("Jobs {jobs}"),
        artifacts.map_or_else(|| "Artifacts".to_owned(), |count| format!("Artifacts {count}")),
        "Logs".to_owned(),
    ];
    let tabs_w = tabs.iter().map(|tab| chrome::width(tab) + 3).sum::<u16>();
    // The tail (run › job › step) is kept longest: the title gives way first, then the
    // branch and title, then the tabs.
    let head = if run.is_some() && spans.len() > 4 { 5 } else { 0 };
    let fits = |spans: &[Span], right: u16| chrome::spans_width(spans) + 1 <= right;
    let mut show_tabs = width > tabs_w + 30;
    let mut crumb_right = if show_tabs { width - tabs_w - 2 } else { width - 1 };
    if !fits(&spans, crumb_right) && head > 0 {
        let total = usize::from(chrome::spans_width(&spans));
        let title_w = spans[3].0.width();
        let room = usize::from(crumb_right.saturating_sub(1)).saturating_sub(total - title_w);
        if room >= 8 {
            spans[3].0 = chrome::ellipsize(&spans[3].0, room);
        } else {
            spans.drain(..head);
            if !fits(&spans, crumb_right) {
                show_tabs = false;
                crumb_right = width - 1;
            }
        }
    }
    let mut x = 1;
    for (text, style) in &spans {
        x = frame.grid.put_text_clipped(x, y, text, *style, crumb_right);
    }
    if show_tabs {
        chrome::sub_tabs(&mut frame.grid, width - tabs_w - 1, y, width, &tabs, ws.actions.tab, false);
    }
}

fn drill(frame: &mut Frame, ws: &mut Workspace, width: u16, height: u16, mode: ExplorerIconMode) {
    breadcrumb(frame, ws, 1, width, mode);
    let body = Rect::new(0, 2, width, height.saturating_sub(3));
    match ws.actions.tab {
        1 => artifacts_tab(frame, ws, body, mode),
        2 => {
            paint_log(frame, ws, body, true, mode);
        }
        _ => {
            let narrow = width < 100;
            let column = ws.actions.column.min(3);
            let mut widths = COLUMNS;
            if narrow {
                if column == 3 {
                    paint_log(frame, ws, body, true, mode);
                    return;
                }
                let w = (width / 2).clamp(24, 36);
                let rect = Rect::new(0, body.y, w, body.h);
                paint_column(frame, ws, column, rect, true, mode);
                paint_log(frame, ws, Rect::new(w, body.y, width - w, body.h), false, mode);
                return;
            }
            let mut log = width.saturating_sub(widths.iter().sum::<u16>());
            for at in 0..column.min(3) {
                if log >= LOG_ROOM {
                    break;
                }
                let give = widths[at] - SHRUNK;
                widths[at] = SHRUNK;
                log += give;
            }
            let mut x = 0;
            for (at, w) in widths.iter().enumerate() {
                paint_column(frame, ws, at, Rect::new(x, body.y, *w, body.h), column == at, mode);
                x += w;
            }
            paint_log(frame, ws, Rect::new(x, body.y, width - x, body.h), column == 3, mode);
        }
    }
}

fn paint_column(frame: &mut Frame, ws: &mut Workspace, at: usize, rect: Rect, focused: bool, mode: ExplorerIconMode) {
    let inner = rect.inner();
    let room = usize::from(inner.w.saturating_sub(2));
    let (title, items, cursor): (Vec<Span>, Vec<Item>, Option<usize>) = match at {
        0 => {
            let runs = ws.shown_runs();
            let cursor = ws.run_cursor(&runs);
            let items = runs
                .iter()
                .enumerate()
                .map(|(index, run)| {
                    let state = State::of(run);
                    let mut item = Item::new(
                        vec![
                            (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
                            (
                                chrome::ellipsize(&format!("#{} {}", run_number(run), string(run, "workflowName")), room.saturating_sub(2)),
                                if index == cursor { bold(state.role(), PANE) } else { st(GitRole::Text, PANE) },
                            ),
                        ],
                        Vec::new(),
                    );
                    if index == cursor && room > 12 {
                        let (event, _) = ws.event_cell(run);
                        let duration = ws.run_seconds(run).map(duration);
                        item.more = Some(vec![
                            (
                                chrome::ellipsize(
                                    &format!(
                                        "  {} · {}{}",
                                        string(run, "headBranch"),
                                        event,
                                        duration.map(|d| format!(" · {d}")).unwrap_or_default()
                                    ),
                                    room,
                                ),
                                st(GitRole::Muted, PANE),
                            ),
                        ]);
                    }
                    item
                })
                .collect();
            (vec![(format!("{} {}s", glyph(Glyph::FoldClosed, mode), ws.run_word()), bold(GitRole::Text, PANE))], items, Some(cursor))
        }
        1 if ws.actions.attempts => {
            let latest = ws.latest_attempt();
            let id = ws.run_id.unwrap_or(0);
            let items = (1..=latest)
                .rev()
                .map(|attempt| {
                    let value = if attempt == latest {
                        ws.value(&Key::Run(id, 0))
                    } else {
                        ws.value(&Key::Run(id, attempt as u32))
                    };
                    let (state, when) = value.map_or((State::Unknown, String::new()), |run| {
                        let state = State::of(run);
                        (state, ws.run_seconds(run).map(duration).unwrap_or_default())
                    });
                    let mut left = vec![
                        (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
                        (format!("Attempt {attempt}"), bold(state.role(), PANE)),
                    ];
                    if value.is_some() {
                        left.push((format!(" · {}", state.word()), st(state.role(), PANE)));
                    }
                    if !when.is_empty() {
                        left.push((format!(" · {when}"), st(GitRole::Muted, PANE)));
                    }
                    Item::new(clip(left, room as u16), Vec::new())
                })
                .collect();
            (vec![("Attempts".to_owned(), bold(GitRole::Text, PANE))], items, Some(ws.actions.attempt_cursor))
        }
        1 => {
            let jobs = ws.jobs();
            let cursor = ws.job_index(&jobs);
            let items = jobs
                .iter()
                .map(|job| {
                    let state = State::of(job);
                    let time = job_seconds(job).map(duration).unwrap_or_default();
                    let name_room = room.saturating_sub(time.width() + 3);
                    Item::new(
                        vec![
                            (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
                            (chrome::ellipsize(string(job, "name"), name_room), st(if state == State::Failed { GitRole::Removed } else { GitRole::Text }, PANE)),
                        ],
                        if time.is_empty() { Vec::new() } else { vec![(time, st(GitRole::Muted, PANE))] },
                    )
                })
                .collect::<Vec<_>>();
            let mut title = vec![("Jobs".to_owned(), bold(GitRole::Text, PANE))];
            if ws.latest_attempt() > 1 && ws.attempt > 0 && u64::from(ws.attempt) < ws.latest_attempt() {
                title.push((format!(" · attempt {}", ws.attempt), st(GitRole::Muted, PANE)));
            }
            (title, items, (!jobs.is_empty()).then_some(cursor))
        }
        _ => {
            let job = ws.selected_job().cloned();
            let steps = job.as_ref().map(|job| ws.steps(job)).unwrap_or_default();
            let cursor = ws.step_index(&steps);
            let items = steps
                .iter()
                .map(|step| {
                    let time = if step.state == State::Skipped {
                        "Skipped".to_owned()
                    } else {
                        step.seconds.map(duration).unwrap_or_default()
                    };
                    let name_room = room.saturating_sub(time.width() + 3);
                    let (head, tail) = wrap_name(&step.name, name_room);
                    let role = match step.state {
                        State::Failed => GitRole::Removed,
                        State::Skipped => GitRole::Muted,
                        _ => GitRole::Text,
                    };
                    let mut item = Item::new(
                        vec![(format!("{} ", step.state.glyph(mode)), st(step.state.role(), PANE)), (head, st(role, PANE))],
                        if time.is_empty() { Vec::new() } else { vec![(time, st(GitRole::Muted, PANE))] },
                    );
                    item.more = tail.map(|tail| vec![(format!("  {tail}"), st(role, PANE))]);
                    item
                })
                .collect::<Vec<_>>();
            (vec![("Steps".to_owned(), bold(GitRole::Text, PANE))], items, (!steps.is_empty()).then_some(cursor))
        }
    };
    chrome::pane(&mut frame.grid, rect, &title, "", focused);
    if items.is_empty() {
        let text = match at {
            1 => match ws.run_id.map(|id| ws.cache.get(&Key::Run(id, ws.attempt))) {
                Some(Some(Err(error))) => error.clone(),
                Some(Some(Ok(_))) => "No jobs yet".into(),
                _ => "Loading jobs…".into(),
            },
            2 if ws.selected_job().is_some_and(|job| array(job, "steps").is_empty() && State::of(job).finished()) => {
                "Steps come from the log…".into()
            }
            2 => "No steps".into(),
            _ => "Nothing here".into(),
        };
        frame.grid.put_text_clipped(inner.x + 1, inner.y, &text, st(GitRole::Muted, PANE), inner.right());
        if focused {
            frame.cursor = Some((inner.x + 1, inner.y, SetCursorStyle::SteadyBlock));
        }
        return;
    }
    if let Some(row) = paint_items(&mut frame.grid, inner, &items, cursor, focused, mode)
        && focused
    {
        frame.cursor = Some((inner.x, row, SetCursorStyle::SteadyBar));
    }
}

/// The step log (or the full job log): header, find row, numbered lines with SGR colours,
/// error and command tones, links, find matches, pan marks and wrap.
fn paint_log(frame: &mut Frame, ws: &mut Workspace, rect: Rect, focused: bool, mode: ExplorerIconMode) {
    let full = ws.actions.tab == 2;
    let job = ws.selected_job().cloned();
    let steps = job.as_ref().map(|job| ws.steps(job)).unwrap_or_default();
    let step = steps.get(ws.step_index(&steps));
    let name = if full {
        job.as_ref().map(|job| string(job, "name").to_owned()).unwrap_or_default()
    } else {
        step.map(|step| step.name.clone()).unwrap_or_default()
    };
    let title = vec![
        (
            format!("{} {} log", if full { "$" } else { glyph(Glyph::FoldClosed, mode) }, if full { "Job" } else { "Step" }),
            bold(GitRole::Text, PANE),
        ),
        (if name.is_empty() { String::new() } else { format!(" · {name}") }, bold(GitRole::Text, PANE)),
    ];
    let button = if full { "[ Step log ]" } else { "[ Full job ]" };
    chrome::pane(&mut frame.grid, rect, &title, "", focused);
    if rect.w > chrome::width(button) + 20 {
        frame.grid.put_text_clipped(
            rect.right().saturating_sub(chrome::width(button) + 2),
            rect.y,
            button,
            bold(GitRole::Focus, PANE),
            rect.right() - 1,
        );
    }
    let inner = rect.inner();
    let x0 = inner.x + 1;
    let right = inner.right();
    let mut y = inner.y;
    let Some(job) = job else {
        let text = match ws.run_id.map(|id| ws.cache.get(&Key::Run(id, ws.attempt))) {
            Some(Some(Err(error))) => error.clone(),
            Some(Some(Ok(_))) => "No jobs".into(),
            _ => "Loading…".into(),
        };
        frame.grid.put_text_clipped(x0, y, &text, st(GitRole::Muted, PANE), right);
        return;
    };
    let body = if full {
        ws.log_body(&job, None, true)
    } else {
        ws.log_body(&job, step, false)
    };
    let state = State::of(&job);
    let run_label = ws
        .run_id
        .and_then(|id| ws.run_value(id))
        .map(|run| format!("{} #{}", ws.run_word(), run_number(run)))
        .unwrap_or_default();
    let mut header: Vec<Span> = if full {
        vec![(run_label, st(GitRole::Text, PANE))]
    } else {
        vec![
            (string(&job, "name").to_owned(), st(GitRole::Text, PANE)),
            (format!(" · {run_label}"), st(GitRole::Text, PANE)),
        ]
    };
    header.push((" · ".into(), st(GitRole::Muted, PANE)));
    header.push((state.word().to_owned(), bold(state.role(), PANE)));
    if let Body::Lines { lines, .. } = &body
        && full
    {
        header.push((format!(" · {} lines", lines.len()), st(GitRole::Muted, PANE)));
    }
    chrome::put_spans(&mut frame.grid, x0, y, &header, right);
    y += 1;
    let (lines, note) = match body {
        Body::Message(text, role) => {
            frame.grid.put_text_clipped(x0, y + 1, &text, st(role, PANE), right);
            if focused {
                frame.cursor = Some((x0, y + 1, SetCursorStyle::SteadyBlock));
            }
            ws.actions.log_rows = 0;
            return;
        }
        Body::Lines { lines, note } => (lines, note),
    };
    // A new log (another run, attempt, job or step) starts at its first error.
    let identity = ws.current_log().map(|(identity, _)| identity).unwrap_or_default();
    if ws.actions.log_identity != identity {
        ws.actions.log_identity = identity;
        ws.actions.log_cursor = lines.iter().position(|line| line.tone == Tone::Error).unwrap_or(0);
        ws.actions.log_scroll = 0;
        ws.actions.pan = 0;
        ws.actions.find = Input::default();
        ws.actions.find_current = 0;
    }
    ws.actions.log_cursor = ws.actions.log_cursor.min(lines.len() - 1);
    let query = ws.actions.find.query.clone();
    let found = lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| matches(&line.text, &query).into_iter().map(move |range| (index, range)))
        .collect::<Vec<_>>();
    if ws.actions.find.active() {
        let text = format!(
            "{} {}{}",
            glyph(Glyph::Search, mode),
            ws.actions.find.query,
            if ws.actions.find.editing { "▏" } else { "" }
        );
        frame.grid.put_text_clipped(x0, y, &text, bold(GitRole::Modified, PANE), right);
        let count = if query.is_empty() {
            String::new()
        } else if found.is_empty() {
            "no matches".into()
        } else {
            format!("{}/{} matches", ws.actions.find_current.min(found.len() - 1) + 1, found.len())
        };
        frame.grid.put_text_clipped(
            right.saturating_sub(chrome::width(&count) + 1),
            y,
            &count,
            st(if found.is_empty() && !query.is_empty() { GitRole::Removed } else { GitRole::Modified }, PANE),
            right,
        );
        if ws.actions.find.editing {
            frame.cursor = Some((x0 + 2 + chrome::width(&ws.actions.find.query), y, SetCursorStyle::SteadyBar));
        }
        y += 1;
    }
    if let Some(note) = &note {
        frame.grid.put_text_clipped(x0, y, note, st(GitRole::Muted, PANE), right);
        y += 1;
    }
    let digits = lines.len().to_string().len().max(3) as u16;
    let text_x = x0 + 1 + digits + 1;
    let room = usize::from(right.saturating_sub(text_x));
    ws.detail_width = room;
    let rows = inner.bottom().saturating_sub(y);
    ws.actions.log_rows = usize::from(rows);
    if rows == 0 || room < 4 {
        return;
    }
    // Visual rows per line (wrap) and a scroll that keeps the cursor in view.
    let chunks = |line: &LogLine| -> Vec<Range<usize>> {
        if !ws.actions.wrap {
            return vec![0..line.text.len()];
        }
        let mut out = Vec::new();
        let mut start = 0;
        let mut used = 0;
        for (at, grapheme) in line.text.grapheme_indices(true) {
            let w = grapheme.width();
            if used + w > room && at > start {
                out.push(start..at);
                start = at;
                used = 0;
            }
            used += w;
        }
        out.push(start..line.text.len());
        out
    };
    let cursor = ws.actions.log_cursor;
    let mut scroll = ws.actions.log_scroll.min(cursor);
    loop {
        let visible: usize = lines[scroll..=cursor].iter().map(|line| chunks(line).len()).sum();
        if visible <= usize::from(rows) || scroll == cursor {
            break;
        }
        scroll += 1;
    }
    ws.actions.log_scroll = scroll;
    let current = (!found.is_empty()).then(|| found[ws.actions.find_current.min(found.len() - 1)].clone());
    // The current match stays in view at this frame's width (the pan was set at another).
    if !ws.actions.wrap
        && ws.actions.follow_match
        && let Some((line, range)) = current.as_ref().filter(|(line, _)| *line == cursor)
    {
        let text = &lines[*line].text;
        let start = text[..range.start].width();
        let end = start + text[range.clone()].width();
        if start < ws.actions.pan || end > ws.actions.pan + room {
            ws.actions.pan = start.saturating_sub(room / 3) / PAN * PAN;
        }
    }
    let pan = if ws.actions.wrap { 0 } else { ws.actions.pan };
    let mut row = y;
    for (index, line) in lines.iter().enumerate().skip(scroll) {
        if row >= inner.bottom() {
            break;
        }
        let selected = index == cursor;
        let surface = if selected {
            if focused { GitSurface::Cursor } else { GitSurface::CursorInactive }
        } else {
            PANE
        };
        let line_matches = found.iter().filter(|(at, _)| *at == index).map(|(_, range)| range.clone()).collect::<Vec<_>>();
        for (part, range) in chunks(line).into_iter().enumerate() {
            if row >= inner.bottom() {
                break;
            }
            if selected {
                chrome::fill(&mut frame.grid, Rect::new(inner.x, row, inner.w, 1), st(GitRole::Text, surface));
            }
            if part == 0 {
                if selected {
                    frame.grid.put_text_clipped(inner.x, row, glyph(Glyph::FoldClosed, mode), bold(GitRole::Focus, surface), right);
                }
                let number = format!("{:>w$}", index + 1, w = usize::from(digits));
                frame.grid.put_text_clipped(x0 + 1, row, &number, st(if selected { GitRole::Focus } else { GitRole::Muted }, surface), right);
            }
            let style_at = |at: usize| -> Style {
                if let Some((_, current_range)) = current.as_ref().filter(|(line, _)| *line == index)
                    && current_range.contains(&at)
                {
                    return bold(GitRole::OnFocus, GitSurface::ChipActive);
                }
                if line_matches.iter().any(|range| range.contains(&at)) {
                    return Style::Search;
                }
                let mut role = match line.tone {
                    Tone::Error => GitRole::Removed,
                    _ => GitRole::Text,
                };
                let mut weight = matches!(line.tone, Tone::Command | Tone::Heading);
                if let Some((_, sgr)) = line.colours.iter().find(|(range, _)| range.contains(&at)) {
                    if let Some(color) = sgr.color {
                        role = sgr_role(color);
                    }
                    weight |= sgr.bold;
                }
                if line.link.as_ref().is_some_and(|(range, _, _)| range.contains(&at)) {
                    role = GitRole::Link;
                }
                if weight {
                    bold(role, surface)
                } else {
                    st(role, surface)
                }
            };
            let mut column = 0usize;
            let mut x = text_x;
            let mut clipped_right = false;
            for (at, grapheme) in line.text[range.clone()].grapheme_indices(true) {
                let at = at + range.start;
                let w = grapheme.width();
                if column + w <= pan {
                    column += w;
                    continue;
                }
                if column < pan {
                    // A wide grapheme cut by the pan edge.
                    column += w;
                    continue;
                }
                if x + w as u16 > right {
                    clipped_right = true;
                    break;
                }
                if w > 0 {
                    frame.grid.put_text_clipped(x, row, grapheme, style_at(at), right);
                }
                x += w as u16;
                column += w;
            }
            if pan > 0 && line.text.width() > 0 {
                frame.grid.set_cell(text_x - 1, row, glyph(Glyph::ClipLeft, mode), st(GitRole::Muted, surface));
            }
            if clipped_right {
                frame.grid.set_cell(right - 1, row, glyph(Glyph::ClipRight, mode), st(GitRole::Muted, surface));
            }
            if selected && part == 0 && focused && !ws.actions.find.editing {
                frame.cursor = Some((text_x, row, SetCursorStyle::SteadyBar));
            }
            row += 1;
        }
    }
}

fn artifacts_tab(frame: &mut Frame, ws: &mut Workspace, body: Rect, mode: ExplorerIconMode) {
    let narrow = body.w < 100;
    let left_w = if narrow { body.w } else { (body.w * 45 / 100).max(44) };
    let list = Rect::new(body.x, body.y, left_w, body.h);
    let artifacts = ws.artifacts();
    let count = artifacts.len().to_string();
    let run = ws.run_id.and_then(|id| ws.run_value(id));
    let title = vec![(
        format!("{} {} #{} · Artifacts", glyph(Glyph::File, mode), ws.run_word(), run.map(run_number).unwrap_or(0)),
        bold(GitRole::Text, PANE),
    )];
    chrome::pane(&mut frame.grid, list, &title, &count, true);
    let inner = list.inner();
    let x = inner.x + 1;
    let mut y = inner.y;
    if let Some(run) = run {
        let state = State::of(run);
        let mut spans = vec![(format!("{} #{} · ", string(run, "workflowName"), run_number(run)), st(GitRole::Text, PANE))];
        spans.extend(state_spans(state, mode));
        spans.push((" · ".into(), st(GitRole::Muted, PANE)));
        spans.push((string(run, "headBranch").to_owned(), st(GitRole::Ref, PANE)));
        chrome::put_spans(&mut frame.grid, x, y, &spans, inner.right());
        y += 2;
    }
    let caps = ws.capabilities();
    let key = ws.run_id.map(Key::Artifacts);
    let message = if let Some(reason) = caps.artifacts_list.reason() {
        Some((reason.to_owned(), GitRole::Muted))
    } else {
        match key.as_ref().and_then(|key| ws.cache.get(key)) {
            Some(Err(error)) => Some((error.clone(), GitRole::Removed)),
            None => Some(("Loading artifacts…".to_owned(), GitRole::Muted)),
            Some(Ok(_)) if artifacts.is_empty() => Some((format!("This {} uploaded no artifacts", ws.actions_noun()), GitRole::Muted)),
            _ => None,
        }
    };
    if let Some((text, role)) = message {
        frame.grid.put_text_clipped(x, y, &text, st(role, PANE), inner.right());
        frame.cursor = Some((x, y, SetCursorStyle::SteadyBlock));
        return;
    }
    let size_w = 9u16;
    let expires_w = 12u16;
    let name_w = inner.w.saturating_sub(size_w + expires_w + 5);
    let mut header_x = x;
    for (label, w) in [("Name", name_w), ("Size", size_w), ("Expires", expires_w)] {
        frame.grid.put_text_clipped(header_x, y, label, bold(GitRole::Text, PANE), inner.right());
        header_x += w + 1;
    }
    y += 1;
    let cursor = ws.actions.artifact.min(artifacts.len() - 1);
    let items = artifacts
        .iter()
        .map(|artifact| {
            let (when, role) = expires(artifact);
            let gone = expired(artifact);
            let name = chrome::ellipsize(string(artifact, "name"), usize::from(name_w.saturating_sub(2)));
            Item::new(
                vec![
                    (format!("{} ", glyph(Glyph::File, mode)), st(GitRole::Muted, PANE)),
                    (format!("{name:<w$}", w = usize::from(name_w.saturating_sub(2))), st(if gone { GitRole::Muted } else { GitRole::Text }, PANE)),
                    (format!(" {:>w$} ", size(number(artifact, "size")), w = usize::from(size_w - 1)), st(GitRole::Text, PANE)),
                    (format!(" {when}"), st(role, PANE)),
                ],
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    let rows = Rect::new(inner.x, y, inner.w, inner.bottom().saturating_sub(y));
    if let Some(row) = paint_items(&mut frame.grid, rows, &items, Some(cursor), true, mode) {
        frame.cursor = Some((inner.x, row, SetCursorStyle::SteadyBar));
    }
    if narrow {
        return;
    }
    let reader = Rect::new(body.x + left_w, body.y, body.w - left_w, body.h);
    let artifact = artifacts[cursor];
    let name = string(artifact, "name").to_owned();
    chrome::pane(
        &mut frame.grid,
        reader,
        &[(format!("{} Artifact · {name}", glyph(Glyph::File, mode)), bold(GitRole::Text, PANE))],
        "",
        false,
    );
    let inner = reader.inner();
    let x = inner.x + 1;
    let right = inner.right().saturating_sub(1);
    let mut y = inner.y;
    let files_key = Key::ArtifactFiles(artifact_id(artifact));
    let files = ws.value(&files_key).map(|value| array(value, "ArtifactFiles").to_vec()).unwrap_or_default();
    let mut summary = vec![("Archive".to_owned(), st(GitRole::Muted, PANE))];
    let bytes = size(number(artifact, "size"));
    if !bytes.is_empty() {
        summary.push((format!(" · {bytes}"), st(GitRole::Muted, PANE)));
    }
    if !files.is_empty() {
        summary.push((format!(" · {} file{}", files.len(), if files.len() == 1 { "" } else { "s" }), st(GitRole::Muted, PANE)));
    }
    chrome::put_spans(&mut frame.grid, x, y, &summary, right);
    y += 2;
    frame.grid.put_text_clipped(x, y, "Files", bold(GitRole::Text, PANE), right);
    y += 1;
    if expired(artifact) {
        frame.grid.put_text_clipped(x + 2, y, "Expired · the forge no longer keeps it", st(GitRole::Removed, PANE), right);
        y += 1;
    } else {
        match ws.cache.get(&files_key) {
            Some(Err(error)) => {
                frame.grid.put_text_clipped(x + 2, y, error, st(GitRole::Removed, PANE), right);
                y += 1;
            }
            None => {
                frame.grid.put_text_clipped(x + 2, y, "Reading the archive…", st(GitRole::Muted, PANE), right);
                y += 1;
            }
            Some(Ok(_)) => {
                let shown = usize::from(inner.bottom().saturating_sub(y + 6)).max(1);
                for file in files.iter().take(shown) {
                    let path = string(file, "path");
                    let size = size(number(file, "size"));
                    frame.grid.put_text_clipped(x + 2, y, path, st(GitRole::Link, PANE), right.saturating_sub(10));
                    frame.grid.put_text_clipped(right.saturating_sub(chrome::width(&size)), y, &size, st(GitRole::Muted, PANE), right);
                    y += 1;
                }
                if files.len() > shown {
                    frame.grid.put_text_clipped(x + 2, y, &format!("+{} more", files.len() - shown), st(GitRole::Muted, PANE), right);
                    y += 1;
                }
            }
        }
    }
    y += 1;
    frame.grid.put_text_clipped(x, y, "Download", bold(GitRole::Text, PANE), right);
    y += 1;
    let label_end = frame.grid.put_text_clipped(x, y, "Destination  ", st(GitRole::Text, PANE), right);
    frame.grid.put_text_clipped(label_end, y, &format!("./artifacts/{name}.zip"), st(GitRole::Link, PANE), right);
    y += 2;
    if let Some(receipt) = &ws.actions.receipt
        && receipt.contains(&format!("{name}.zip"))
    {
        chrome::receipt(&mut frame.grid, Rect::new(inner.x, y, inner.w, 1), chrome::ReceiptTone::Ok, receipt, mode);
    } else {
        let mut x = frame.grid.put_text_clipped(x, y, "Enter", bold(GitRole::Focus, PANE), right);
        x = frame.grid.put_text_clipped(x, y, " download", st(GitRole::Muted, PANE), right);
        let _ = x;
    }
}

// ------------------------------------------------------------------------------- footer

fn footer(app: &GitScreen, frame: &mut Frame, rect: Rect) {
    let ws = &app.github;
    if let Some(form) = &app.form {
        let several = form.fields.len() > 1;
        let multiline = form.fields.iter().any(|field| field.multiline);
        let verb = match &form.action {
            FormAction::Github(Action::Rerun(_)) => "rerun",
            FormAction::Github(Action::Cancel(_)) => "cancel run",
            FormAction::Github(Action::Dispatch(_)) => "dispatch",
            FormAction::Github(Action::Approve(_)) => "approve",
            FormAction::Github(Action::Download(..)) => "download",
            FormAction::Github(Action::SaveLog(_)) => "save",
            _ => "confirm",
        };
        let mut hints = vec![(if multiline { "Ctrl+Enter" } else { "Enter" }, verb), ("Esc", "back")];
        if several {
            hints.extend([("Tab", "next field"), ("Shift+Tab", "previous")]);
        }
        let message = (!app.notice.is_empty()).then(|| (app.notice.as_str(), true));
        chrome::footer(&mut frame.grid, rect, &hints, "", message);
        return;
    }
    let message = if ws.writing() {
        Some(("Sending to the forge…", false))
    } else if !app.notice.is_empty() {
        Some((app.notice.as_str(), true))
    } else if !ws.notice.is_empty() {
        Some((ws.notice.as_str(), ws.notice_error))
    } else {
        None
    };
    let view = &ws.actions;
    let log_focus = view.drill && (view.tab == 2 || view.tab == 0 && view.column == 3);
    let hints: Vec<(&str, &str)> = if view.find.editing || view.filter.editing {
        vec![("Enter", if view.find.editing { "find" } else { "filter" }), ("Esc", "cancel")]
    } else if !view.drill {
        if ws.focus == 0 {
            vec![("Enter", "choose"), ("l", ws.actions_noun()), ("/", "filter"), ("d", "dispatch")]
        } else {
            vec![("Enter", "open"), ("r", "rerun"), ("x", "cancel"), ("d", "dispatch"), ("/", "filter")]
        }
    } else if view.tab == 1 {
        vec![("Enter", "download"), ("j k", "move"), ("[ ]", "tabs"), ("Esc", "back")]
    } else if log_focus && view.find.active() {
        vec![("n/N", "matches"), ("h/l", "pan"), ("w", "wrap"), ("Esc", "clear")]
    } else if log_focus {
        let link = matches!(ws.current_log(), Some((_, Body::Lines { lines, .. })) if lines.get(view.log_cursor).is_some_and(|line| line.link.is_some()));
        let mut hints = Vec::new();
        if link {
            hints.push(("Enter", "open source"));
        }
        hints.extend([("/", "find"), (if view.tab == 2 { "f" } else { "f" }, if view.tab == 2 { "step log" } else { "full job" })]);
        if view.tab == 2 {
            hints.extend([("h/l", "pan"), ("w", "wrap"), ("Ctrl+s", "save")]);
        } else {
            hints.extend([("a", "attempts"), ("r", "rerun")]);
        }
        hints.push(("Esc", "back"));
        hints
    } else if view.attempts {
        vec![("Enter", "show attempt"), ("j k", "move"), ("Esc", "close")]
    } else {
        vec![("Enter", "open"), ("f", "full job"), ("a", "attempts"), ("r", "rerun"), ("Esc", "back")]
    };
    let context = if view.drill || ws.focus == 0 {
        String::new()
    } else {
        view.filter.query.clone()
    };
    chrome::footer(&mut frame.grid, rect, &hints, &context, message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace() -> Workspace {
        let mut workspace = Workspace::new(PathBuf::new());
        workspace.page = 3;
        workspace.repo = "octo/orbit".into();
        workspace.host = "github.com".into();
        let run = |id: u64, attempt: u64, conclusion: &str| {
            json!({"databaseId": id, "number": 10, "displayTitle": "Retry flaky uploads",
                "workflowName": "CI", "status": "completed", "conclusion": conclusion,
                "headBranch": "retry-uploads", "headSha": "a".repeat(40), "event": "pull_request",
                "attempt": attempt, "workflowDatabaseId": 31, "createdAt": "2026-11-14T05:00:00Z",
                "jobs": [{"databaseId": 600 + attempt, "name": "test", "status": "completed",
                    "conclusion": conclusion, "startedAt": "2026-11-14T05:00:01Z",
                    "completedAt": "2026-11-14T05:00:05Z",
                    "steps": [{"number": 1, "name": "Set up job", "status": "completed", "conclusion": "success",
                               "startedAt": "2026-11-14T05:00:01Z", "completedAt": "2026-11-14T05:00:02Z"},
                              {"number": 2, "name": "Run tests", "status": "completed", "conclusion": conclusion,
                               "startedAt": "2026-11-14T05:00:02Z", "completedAt": "2026-11-14T05:00:05Z"}]}]})
        };
        let insert = |workspace: &mut Workspace, key: Key, value: Value, text: String| {
            workspace.cache.insert(key, Ok(Reply { text, value }));
        };
        insert(&mut workspace, Key::Repository, json!({"nameWithOwner": "octo/orbit"}), String::new());
        insert(&mut workspace, Key::Runs(0), json!([run(501, 2, "failure")]), String::new());
        insert(&mut workspace, Key::Run(501, 0), run(501, 2, "failure"), String::new());
        insert(&mut workspace, Key::Run(501, 1), run(501, 1, "failure"), String::new());
        // Both attempts print the same output: identity comes from run, attempt, job and step.
        let log = "test\tUNKNOWN STEP\t2026-11-14T05:00:01.5Z setting up\n\
                   test\tUNKNOWN STEP\t2026-11-14T05:00:03.0Z running 3 tests\n\
                   test\tUNKNOWN STEP\t2026-11-14T05:00:04.0Z error: retries_uploads failed at src/lib.rs:41\n";
        insert(&mut workspace, Key::Log(602), Value::Null, log.into());
        insert(&mut workspace, Key::Log(601), Value::Null, log.into());
        workspace.run_id = Some(501);
        workspace.actions.drill = true;
        workspace.actions.column = 3;
        workspace
    }

    fn identity(workspace: &Workspace) -> String {
        workspace.current_log().unwrap().0
    }

    #[test]
    fn log_identity_is_run_attempt_job_and_step_never_the_text() {
        let mut workspace = workspace();
        let latest = identity(&workspace);
        assert_eq!(latest, "501/2/602/2/false", "the failed step is chosen first");
        workspace.attempt = 1;
        let earlier = identity(&workspace);
        assert_eq!(earlier, "501/1/601/2/false");
        assert_ne!(latest, earlier, "identical output in another attempt is another log");
        workspace.actions.step = Some(1);
        assert_eq!(identity(&workspace), "501/1/601/1/false");
        workspace.actions.tab = 2;
        assert_eq!(identity(&workspace), "501/1/601/0/true");
    }

    #[test]
    fn effective_attempt_comes_from_the_run_not_from_matching_output() {
        let mut workspace = workspace();
        assert_eq!(workspace.effective_attempt(), 2);
        assert_eq!(workspace.latest_attempt(), 2);
        workspace.attempt = 1;
        assert_eq!(workspace.effective_attempt(), 1);
        assert_eq!(workspace.latest_attempt(), 2);
        // A detail that has not arrived reports no attempt rather than guessing one.
        workspace.attempt = 0;
        workspace.cache.remove(&Key::Run(501, 0));
        assert_eq!(workspace.effective_attempt(), 0);
    }

    #[test]
    fn step_slices_follow_step_times_and_errors_link_their_source() {
        let workspace = workspace();
        let Some((_, Body::Lines { lines, note })) = workspace.current_log() else {
            panic!("log");
        };
        assert_eq!(lines.len(), 2, "lines from 05:00:02 to 05:00:05");
        assert!(note.unwrap().starts_with("Approximate step slice · 05:00:02–05:00:05"));
        assert_eq!(lines[1].tone, Tone::Error);
        let (range, path, line) = lines[1].link.clone().unwrap();
        assert_eq!((path.as_str(), line), ("src/lib.rs", 41));
        assert_eq!(&lines[1].text[range], "src/lib.rs:41");
    }

    #[test]
    fn find_counts_and_steps_wrap_around() {
        let mut workspace = workspace();
        workspace.actions.tab = 2;
        workspace.actions.find.query = "e".into();
        let Some((_, Body::Lines { lines, .. })) = workspace.current_log() else {
            panic!("log");
        };
        let total = lines.iter().map(|line| matches(&line.text, "e").len()).sum::<usize>();
        assert!(total > 3);
        workspace.reveal_match(None);
        assert_eq!(workspace.actions.find_current, 0);
        workspace.reveal_match(Some(false));
        assert_eq!(workspace.actions.find_current, total - 1);
        workspace.reveal_match(Some(true));
        assert_eq!(workspace.actions.find_current, 0);
        assert_eq!(matches("Error ERROR error", "error").len(), 3);
        assert_eq!(matches("Error ERROR error", "Error").len(), 1);
    }

    #[test]
    fn links_need_a_file_and_a_line() {
        assert_eq!(link("at tests/test_regression.py:6").map(|found| found.1), Some("tests/test_regression.py".into()));
        assert_eq!(link("File \"/home/runner/work/orbit/src/main.rs:12:5\"").map(|found| found.2), Some(12));
        assert!(link("12:30:45 started").is_none());
        assert!(link("version 2.330.0").is_none());
        assert!(link("http://example.invalid:8080/x").is_none() || link("http://example.invalid:8080/x").is_some_and(|found| !found.1.starts_with("//")));
    }
}
