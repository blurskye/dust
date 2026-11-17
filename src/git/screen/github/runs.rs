//! Actions: two panels, always. The run history on the left (section
//! tabs over two-line rows: the human title, then workflow · PR or branch · event · actor ·
//! age); the selected run on the right as one tree that follows the cursor: jobs, their steps
//! and each step's log inline, failing work opened at its first error. Find, error jumps,
//! zoom, attempts and artifacts live in that tree; rerun / cancel / dispatch / approve are
//! sheets. GitLab's words come from `Terms` (pipelines).
use super::super::shell::chrome::{self, Span, bold, st};
use super::super::shell::glyphs::{Glyph, glyph};
use super::super::shell::layout::{self, Mode, Rect};
use super::super::{FormAction, GitScreen};
use super::keymap::{Named, Step};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::host_state;
use crate::forge::records;
use crate::forge::transport::HostKey;
use crate::ui::{CellGrid, Frame, GitRole, GitSurface, Style};
use crossterm::cursor::SetCursorStyle;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A pull request's check, read as this tree reads a job (the Checks tab, `checks.rs`).
pub(super) mod check_job;

const PANE: GitSurface = GitSurface::Pane;
/// Below this width the screen shows the run history or the run, not both.
const SPLIT: u16 = 90;
const PAN: usize = 8;
/// Glyph-independent rows for key handling (the glyphs only matter when painting).
const KEYS: ExplorerIconMode = ExplorerIconMode::Unicode;

/// A one-line text input on the screen itself (run filter, log find).
#[derive(Default)]
pub(super) struct Input {
    pub(super) query: String,
    pub(super) editing: bool,
}

/// A row of the run tree, by identity: a refresh that reorders jobs keeps the cursor.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Node {
    Job(u64),
    /// A job's step by number.
    Step(u64, u64),
    /// A line of a step's log by index; step `0` is the whole job log (`f`).
    Line(u64, u64, usize),
    /// What stands where a log would be (loading, skipped, an error).
    Note(u64, u64),
    Artifacts,
    Artifact(u64),
    File(u64, usize),
}

/// What the Actions tab shows and where its cursors are.
#[derive(Default)]
pub(super) struct View {
    /// The run pane has focus (3, Tab, l, Enter).
    right: bool,
    /// The Workflows panel has focus (1); else the runs below it (2), unless `right`.
    workflows: bool,
    /// Below 90 columns the run replaces the list (Enter/Esc).
    open: bool,
    /// The last frame was too narrow for both panels.
    narrow: bool,
    /// The run pane takes the whole width (`z`).
    zoom: bool,
    /// Section tab: 0 all, 1 failing, 2 running, 3 mine.
    section: usize,
    /// The workflow / branch / event filter (`F`).
    picker: bool,
    rail: usize,
    branch: Option<String>,
    event: Option<String>,
    /// The workflow the list keeps when the forge lists none (Forgejo): its runs' names.
    workflow: Option<String>,
    filter: Input,
    /// A run opened from elsewhere (a pull request's check) stays shown though not listed.
    pinned: bool,
    /// The run attempt the tree below belongs to; another one starts from the defaults.
    tree_for: String,
    opened: BTreeSet<Node>,
    closed: BTreeSet<Node>,
    full: BTreeSet<u64>,
    cursor: Option<Node>,
    /// The cursor follows the first error until a key moves it.
    auto: bool,
    scroll: usize,
    attempts: bool,
    attempt_cursor: usize,
    pan: usize,
    /// Long log lines run past the right edge and pan with H L (`w`); by default they wrap
    /// under their text, past the line number.
    nowrap: bool,
    /// Jobs whose log was read while they ran: read once more when they finish.
    live: BTreeSet<u64>,
    /// The find's current match pulls the pan to it (until a pan or a cursor move).
    follow_match: bool,
    find: Input,
    find_current: usize,
    /// The last download's receipt and the run it came from: shown under that run only.
    receipt: Option<(u64, String)>,
    pending_dispatch: Option<u64>,
    /// The run whose `r` waits for its jobs: the sheet opens when they arrive.
    pub(super) pending_rerun: Option<u64>,
    dispatched: Option<(u64, String, BTreeSet<u64>)>,
    cancelling: BTreeSet<u64>,
    rows: usize,
    /// The finished job the open rerun sheet offers alone (`This job (lint)`): the one under
    /// the run cursor when `r` was pressed.
    pub(super) rerun_job: Option<u64>,
    /// Log bodies by what they were read from (`Workspace::log_body`).
    log_memo: std::sync::Mutex<std::collections::HashMap<u64, Body>>,
    /// `v`/`V`: the log line a selection started on; the cursor is its other end.
    visual: Option<Node>,
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
                // Running and queued share the pending mark; the colour tells
                // them apart, and `●` stays unread and a resolved thread.
                Self::Running | Self::Queued | Self::Waiting => Glyph::CheckPending,
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
    fn finished(self) -> bool {
        !matches!(self, Self::Running | Self::Queued | Self::Waiting)
    }
}

/// A run's mark and colour exactly as the Actions list draws it (`status`/`conclusion`), for
/// a check-suite notification's row.
pub(super) fn run_mark(run: &Value, mode: ExplorerIconMode) -> (&'static str, GitRole) {
    let state = State::of(run);
    (state.glyph(mode), state.role())
}

fn seconds(from: &str, to: &str) -> Option<i64> {
    // Forgejo writes `0001-01-01T00:00:00Z` for a time that never came.
    let from = records::epoch(from).filter(|at| *at > 0)?;
    Some(records::epoch(to).filter(|at| *at > 0)? - from).filter(|seconds| *seconds >= 0)
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

/// `2m`, `1h 5m`: how long something has been running.
fn elapsed(stamp: &str) -> Option<String> {
    let seconds = (records::now() - records::epoch(stamp)?).max(0);
    Some(if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    })
}

/// A job's (or step's) duration; a job without its own times spans its steps.
fn job_seconds(job: &Value) -> Option<i64> {
    seconds(string(job, "startedAt"), string(job, "completedAt")).or_else(|| {
        let steps = array(job, "steps");
        let start = steps
            .iter()
            .filter_map(|step| records::epoch(string(step, "startedAt")))
            .filter(|at| *at > 0)
            .min()?;
        let end = steps
            .iter()
            .filter_map(|step| records::epoch(string(step, "completedAt")))
            .filter(|at| *at > 0)
            .max()?;
        (end >= start && State::of(job).finished()).then_some(end - start)
    })
}

/// A read's error as UI copy: the provider's sentence, never an HTTP status echo.
/// A failed read in plain words and its one next key (`states::explain`): `Rate limited by
/// github.com · resets in 11m · R retry once it resets`, never the forge's raw `502 Bad
/// Gateway` or `error sending request`.
fn calm(ws: &Workspace, error: &str) -> String {
    ws.explain(error).line()
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
    let at = text
        .find("section_start:")
        .or_else(|| text.find("section_end:"))?;
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
    Pass,
    Command,
    Heading,
    /// The runner's own bookkeeping: a step's folded `Run …` preamble, its cleanup lines.
    Quiet,
}

struct LogLine {
    text: String,
    colours: Vec<(Range<usize>, logs::Sgr)>,
    tone: Tone,
    link: Option<(Range<usize>, String, u32)>,
    /// Where it last wrapped (`LogLine::parts`): a long log is laid out once per width, not on
    /// every frame and key.
    wrapped: std::sync::Mutex<Option<Wrapped>>,
}

/// One layout of a log line: the width and find query it was made for, and its parts.
struct Wrapped {
    room: usize,
    whole: bool,
    finding: String,
    parts: Vec<Range<usize>>,
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
            // The heading follows the marker's `\r`: without it the section's name runs into
            // the heading once the escapes are gone (`set_up_jobSet up job`).
            let shown = match raw.rsplit_once('\r') {
                Some((_, heading)) => logs::styled(heading).0,
                None => shown,
            };
            if !start && shown.trim().is_empty() {
                return None;
            }
            text = shown;
            colours.clear();
            if start {
                tone = Tone::Heading;
            }
        }
        let trimmed = text.trim();
        if tone == Tone::Text {
            let lower = trimmed.to_ascii_lowercase();
            if trimmed.starts_with("$ ") {
                tone = Tone::Command;
            } else if lower.starts_with("error")
                || lower.starts_with("fail")
                || lower.starts_with("fatal")
                || trimmed.starts_with("AssertionError")
                || lower.contains("panicked at")
                || lower.ends_with("... fail")
                || lower.ends_with("... failed")
                || lower.ends_with("... error")
            {
                tone = Tone::Error;
            } else if lower.ends_with("... ok")
                || lower == "ok"
                || lower.starts_with("ok ")
                || lower.starts_with("test result: ok")
                || trimmed.starts_with('✓')
            {
                tone = Tone::Pass;
            }
        }
        if tone == Tone::Text && RUNNER_CHATTER.contains(&trimmed) {
            tone = Tone::Quiet;
        }
        let link = link(&text);
        Some(Self {
            text,
            colours,
            tone,
            link,
            wrapped: std::sync::Mutex::default(),
        })
    }

    fn quiet(text: String) -> Self {
        Self {
            text,
            colours: Vec::new(),
            tone: Tone::Quiet,
            link: None,
            wrapped: std::sync::Mutex::default(),
        }
    }

    /// The line's rows in `room` columns, wrapped (`wrap`) or on one row. `whole` lets a test
    /// or summary line give up part of itself to stay on one row; `finding` keeps its matches
    /// whole.
    fn parts(&self, room: usize, wrap: bool, whole: bool, finding: &str) -> Vec<Range<usize>> {
        if !wrap || ruler(&self.text) {
            return std::iter::once(0..self.text.len()).collect();
        }
        let mut memo = self
            .wrapped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(memo) = memo.as_ref()
            && memo.room == room
            && memo.whole == whole
            && memo.finding == finding
        {
            return memo.parts.clone();
        }
        // Not while finding: a match in the part left out would not show.
        let parts = match one_row(&self.text, room).filter(|_| whole) {
            Some(range) => vec![range],
            None => wrap_points_keeping(&self.text, room.max(4), &matches(&self.text, finding)),
        };
        *memo = Some(Wrapped {
            room,
            whole,
            finding: finding.to_owned(),
            parts: parts.clone(),
        });
        parts
    }
}

/// Lines GitHub's runner adds round every job, read dim.
const RUNNER_CHATTER: [&str; 2] = ["Post job cleanup.", "Cleaning up orphan processes"];

/// A step's log lines. GitHub's runner opens each `run:` step with a group that repeats the
/// command and its shell (`Run python3 …`, `python3 …`, `shell: /usr/bin/bash -e {0}`): that
/// reads as one dim `$ python3 …` (`uses actions/checkout@v4` for an action) and the group's
/// body is hidden. The full log (`fold` false) keeps every line as the forge wrote it.
fn log_lines(payloads: &[&str], fold: bool) -> Vec<LogLine> {
    let mut out = Vec::new();
    let mut preamble = false;
    for payload in payloads {
        let text = logs::payload(payload);
        if !fold {
            out.extend(LogLine::new(payload));
            continue;
        }
        if preamble {
            if text.starts_with("##[endgroup]") {
                preamble = false;
            }
            continue;
        }
        if let Some(command) = text.strip_prefix("##[group]Run ") {
            let command = command.trim();
            let action = !command.contains(' ')
                && (command.contains('@') || command.starts_with("./.github/"));
            out.push(LogLine::quiet(if action {
                format!("uses {command}")
            } else {
                format!("$ {command}")
            }));
            preamble = true;
            continue;
        }
        out.extend(LogLine::new(payload));
    }
    out
}

/// A step opens by itself when it failed in a finished job, or is the one running now.
fn step_opens(job: State, step: State) -> bool {
    step == State::Failed && job != State::Running || step == State::Running
}

/// A line drawn only of one repeated rule character (`=====`, `-----`, `═════`).
fn ruler(text: &str) -> bool {
    let text = text.trim();
    let mut chars = text.chars();
    chars.next().is_some_and(|first| {
        "=-─═━_~*#".contains(first) && text.chars().count() >= 8 && chars.all(|c| c == first)
    })
}

/// A line that reads best on one row, as the range of it to draw: a summary
/// framed in rules (`==== 1 failed, 41 passed in 2.71s ====`) gives up rule characters, and a
/// test line that ends in its status (`tests/test_retry.py::test_backoff PASSED [ 50%]`) gives
/// up the start of its id, drawn as `…` before the rest. None when it fits or is neither.
fn one_row(text: &str, room: usize) -> Option<Range<usize>> {
    let line = text.trim_end();
    if line.width() <= room || room < 12 {
        return None;
    }
    let excess = line.width() - room;
    let first = line.chars().next()?;
    if "=-─═━_~*#".contains(first) {
        let lead = line.chars().take_while(|c| *c == first).count();
        let trail = line.chars().rev().take_while(|c| *c == first).count();
        let total = line.chars().count();
        if lead < 3 || trail < 3 || lead + trail >= total {
            return None;
        }
        let from_lead = excess.div_ceil(2).min(lead - 3);
        let from_trail = (excess - from_lead).min(trail - 3);
        if from_lead + from_trail < excess {
            return None;
        }
        let start = line.char_indices().nth(from_lead).map(|(at, _)| at)?;
        let end = line
            .char_indices()
            .nth(total - from_trail)
            .map_or(line.len(), |(at, _)| at);
        return Some(start..end);
    }
    let (id, rest) = line.split_once(' ')?;
    let mut words = rest.split_whitespace();
    let status = words.next()?;
    let tail = words.collect::<Vec<_>>().join(" ");
    let progress = tail.is_empty() || (tail.starts_with('[') && tail.ends_with("%]"));
    if !id.contains("::")
        || !progress
        || !["PASSED", "FAILED", "ERROR", "SKIPPED", "XFAIL", "XPASS"].contains(&status)
    {
        return None;
    }
    // One column for the `…`; at least a few characters of the id stay.
    let mut start = 0;
    for (at, _) in line.grapheme_indices(true) {
        start = at;
        if line[at..].width() < room {
            break;
        }
    }
    // The cut falls after a `/` or `::` when one follows (R10-GLCR-11): `…test_retry.py::…`
    // or `…test_retry_limit FAILED`, never `…try.py`.
    let boundary = id
        .match_indices('/')
        .map(|(at, _)| at + 1)
        .chain(id.match_indices("::").map(|(at, _)| at + 2))
        .filter(|at| *at >= start)
        .min();
    let start = boundary.unwrap_or(start);
    (start > 0 && start + 6 <= id.len()).then_some(start..line.len())
}

/// Where a log line wraps in `room` columns: after a space or `) . / : , _`, or before a
/// `(`, when one falls in the last 30% of the row; mid-word only when none does.
fn wrap_points(text: &str, room: usize) -> Vec<Range<usize>> {
    wrap_points_keeping(text, room, &[])
}

/// [`wrap_points`], but a hard break that would split one of `keep` (find matches) moves to
/// the match's start, so a match reads whole on one row whatever the width.
pub(super) fn wrap_points_keeping(
    text: &str,
    room: usize,
    keep: &[Range<usize>],
) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut used = 0;
    // The last break opportunity in the current row: its byte offset and the columns before it.
    let mut point: Option<(usize, usize)> = None;
    for (at, grapheme) in text.grapheme_indices(true) {
        let w = grapheme.width();
        if used + w > room && at > start {
            let soft = point.filter(|(offset, before)| *offset > start && *before * 10 >= room * 7);
            let cut = soft.map_or(at, |(offset, _)| offset);
            let cut = keep
                .iter()
                .find(|kept| kept.start > start && kept.start < cut && cut < kept.end)
                .map_or(cut, |kept| kept.start);
            out.push(start..cut);
            // The next row starts at a word, not at the spaces between.
            start = cut + text[cut..].len() - text[cut..].trim_start_matches(' ').len();
            used = text[start.min(at)..at].width();
            point = None;
        }
        if at < start {
            continue;
        }
        if grapheme == "(" {
            point = Some((at, used));
        }
        used += w;
        if matches!(grapheme, " " | ")" | "." | "/" | ":" | "," | "_") {
            point = Some((at + grapheme.len(), used));
        }
    }
    if start < text.len() || out.is_empty() {
        out.push(start..text.len());
    }
    out
}

/// The lines a running step shows (`f` shows them all).
const LIVE_TAIL: usize = 10;

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
    // Python's `File "path", line N`.
    if let Some(open) = text.find("File \"")
        && let Some(close) = text[open + 6..].find('"')
    {
        let path_end = open + 6 + close;
        let path = &text[open + 6..path_end];
        let rest = &text[path_end + 1..];
        if let Some(after) = rest.strip_prefix(", line ") {
            let digits = after
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after.len());
            if digits > 0 && path.contains('.') {
                let line = after[..digits].parse().unwrap_or(1);
                return Some((open + 6..path_end, path.to_owned(), line));
            }
        }
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
        7 => GitRole::Text,
        _ => GitRole::Muted,
    }
}

/// Smart case: a query with capitals matches exactly, otherwise case is ignored.
pub(super) fn matches(text: &str, query: &str) -> Vec<Range<usize>> {
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

/// A step of a job: from the forge's step list, or from the log's own sections when the
/// forge lists none (GitLab, a job without `steps[]`).
struct StepRow {
    number: u64,
    name: String,
    state: State,
    seconds: Option<i64>,
    /// When it started: a running step reads `running 1m` as its job does (A3-05).
    started: Option<String>,
    value: Option<Value>,
    section: Option<Range<usize>>,
}

#[derive(Clone)]
enum Body {
    /// The lines, numbered from 1, of which the first `skip` are not shown (a running step
    /// shows its tail).
    Lines {
        lines: std::sync::Arc<Vec<std::sync::Arc<LogLine>>>,
        skip: usize,
    },
    Message(String, GitRole),
}

/// One row of the run tree.
struct Row {
    node: Node,
    depth: u16,
    /// `Some(open)` for a row that folds.
    fold: Option<bool>,
    state: Option<State>,
    left: Vec<Span>,
    right: Vec<Span>,
    /// A log line: its number and text.
    log: Option<(usize, std::sync::Arc<LogLine>)>,
}

impl Row {
    fn new(node: Node, depth: u16, left: Vec<Span>) -> Self {
        Self {
            node,
            depth,
            fold: None,
            state: None,
            left,
            right: Vec::new(),
            log: None,
        }
    }
    fn link(&self) -> Option<(String, u32)> {
        self.log
            .as_ref()
            .and_then(|(_, line)| line.link.as_ref())
            .map(|(_, path, line)| (path.clone(), *line))
    }
    fn error(&self) -> bool {
        self.log
            .as_ref()
            .is_some_and(|(_, line)| line.tone == Tone::Error)
    }
}

/// Job logs the forge failed to serve for one reason are said once, at the run's level, with
/// one `R retry`; each job keeps a muted `No log` (R10-GITEA-11).
fn say_gone_logs_once(rows: &mut Vec<Row>) {
    let reason = |row: &Row| -> Option<String> {
        let Node::Note(job, _) = row.node else {
            return None;
        };
        let text = &row.left.first()?.0;
        (job != 0 && row.log.is_none())
            .then(|| text.strip_suffix(" · R retry").map(str::to_owned))
            .flatten()
    };
    let failed = rows.iter().filter_map(reason).collect::<Vec<_>>();
    let Some(first) = failed.first().cloned() else {
        return;
    };
    if failed.len() < 2 || failed.iter().any(|text| *text != first) {
        return;
    }
    for row in rows.iter_mut() {
        if reason(row).is_some() {
            row.left = vec![("No log".into(), st(GitRole::Muted, PANE))];
        }
    }
    let mut lowered = first.chars();
    let lowered = lowered
        .next()
        .map(|c| c.to_lowercase().collect::<String>() + lowered.as_str())
        .unwrap_or_default();
    let text = format!("{} job logs: {lowered} · R retry", failed.len());
    rows.insert(
        0,
        Row::new(
            Node::Note(0, u64::MAX),
            0,
            vec![(text, st(GitRole::Removed, PANE))],
        ),
    );
}

fn job_of(node: &Node) -> Option<u64> {
    match node {
        Node::Job(job) | Node::Step(job, _) | Node::Line(job, _, _) | Node::Note(job, _) => {
            Some(*job)
        }
        _ => None,
    }
}

// ------------------------------------------------------------------------------- data

impl Workspace {
    fn actions_noun(&self) -> &'static str {
        self.terms().run_noun
    }

    pub(super) fn run_word(&self) -> &'static str {
        if self.terms().run_noun == "pipeline" {
            "Pipeline"
        } else {
            "Run"
        }
    }

    /// The signed-in login, when the session knows it (the Mine section).
    fn viewer_login(&self) -> Option<String> {
        if self.host.is_empty() {
            return None;
        }
        host_state::account(&HostKey::new(&self.host))
            .and_then(|account| account.login)
            .filter(|login| !login.is_empty())
    }

    fn in_section(&self, run: &Value, section: usize, login: Option<&str>) -> bool {
        match section {
            1 => State::of(run) == State::Failed,
            2 => !State::of(run).finished(),
            3 => login
                .is_some_and(|login| string(&run["dusty"], "actor").eq_ignore_ascii_case(login)),
            _ => true,
        }
    }

    /// The Actions tab's badge: failing runs among the newest, and
    /// whether any is running, as GitHub badges failing checks. Nothing until the runs are
    /// cached (this session or the disk's); nothing when all is quiet.
    pub(in crate::git::screen) fn actions_badge(&self) -> Option<(usize, bool)> {
        if self.repo.is_empty() || self.inbox.enabled {
            return None;
        }
        let runs = self.value(&Key::Runs(0))?.as_array()?;
        let failing = runs
            .iter()
            .filter(|run| State::of(run) == State::Failed)
            .count();
        let running = runs.iter().any(|run| !State::of(run).finished());
        (failing > 0 || running).then_some((failing, running))
    }

    /// The badge costs no request of its own: the newest runs come off the disk cache, and
    /// a cached read is kept fresh like any other.
    pub(super) fn actions_badge_read(&mut self) {
        if !self.capabilities().actions.allowed() {
            return;
        }
        let read = self.runs_read(0);
        if self.load_disk(&Key::Runs(0), &read) {
            self.enqueue(Key::Runs(0), read, false);
        }
    }

    /// The runs the branch, event and text filters keep, before the section tab.
    fn filtered_runs(&self) -> Vec<&Value> {
        let view = &self.actions;
        let query = view.filter.query.to_lowercase();
        self.runs()
            .into_iter()
            .filter(|run| {
                view.branch
                    .as_ref()
                    .is_none_or(|branch| string(run, "headBranch") == branch)
                    && view
                        .workflow
                        .as_ref()
                        .is_none_or(|workflow| self.run_workflow(run) == *workflow)
                    && view
                        .event
                        .as_ref()
                        .is_none_or(|event| self.event_kind(string(run, "event")) == *event)
                    && (query.is_empty()
                        || [
                            string(run, "displayTitle"),
                            &self.run_workflow(run),
                            string(run, "headBranch"),
                            string(&run["dusty"], "actor"),
                        ]
                        .iter()
                        .any(|text| text.to_lowercase().contains(&query))
                        || format!("#{}", run_number(run)).contains(&query))
            })
            .collect()
    }

    /// The runs the list shows: the filters, then the section tab.
    fn shown_runs(&self) -> Vec<&Value> {
        let login = self.viewer_login();
        let section = self.actions.section;
        self.filtered_runs()
            .into_iter()
            .filter(|run| self.in_section(run, section, login.as_deref()))
            .collect()
    }

    /// `All 10`, `Failing 1`, `Running 0`, `Mine 3` (Mine only when the login is known):
    /// every tab counted, 0 included, so the row never mixes counted and bare tabs (r15
    /// gitlab-01). The list pane paints only the state filters; `All` is the selected
    /// source above it, so section 0 is no chip.
    fn section_tabs(&self) -> Vec<String> {
        let login = self.viewer_login();
        let runs = self.filtered_runs();
        let count = |section| {
            runs.iter()
                .filter(|run| self.in_section(run, section, login.as_deref()))
                .count()
        };
        let tab = |name: &str, count: usize| format!("{name} {count}");
        let mut tabs = vec![
            tab("All", runs.len()),
            tab("Failing", count(1)),
            tab("Running", count(2)),
        ];
        if login.is_some() {
            tabs.push(tab("Mine", count(3)));
        }
        tabs
    }

    fn run_cursor(&self, runs: &[&Value]) -> usize {
        self.run_id
            .and_then(|id| runs.iter().position(|run| number(run, "databaseId") == id))
            .unwrap_or_else(|| self.run_selected.min(runs.len().saturating_sub(1)))
    }

    /// The selected run as best known: its detail for the chosen attempt, else its row.
    fn run_value(&self, id: u64) -> Option<&Value> {
        self.value(&Key::Run(id, self.attempt))
            .or_else(|| self.listed_run(id))
    }

    fn listed_run(&self, id: u64) -> Option<&Value> {
        self.runs()
            .into_iter()
            .find(|run| number(run, "databaseId") == id)
    }

    fn run_detail(&self) -> Option<&Value> {
        self.value(&Key::Run(self.run_id?, self.attempt))
    }

    fn jobs(&self) -> Vec<&Value> {
        self.run_detail()
            .map(|run| array(run, "jobs").iter().collect())
            .unwrap_or_default()
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
                    started: Some(string(step, "startedAt").to_owned())
                        .filter(|stamp| !stamp.is_empty()),
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
        // A failed job failed in its last section with an error line (cleanup sections run
        // after it), else in its last section; a running one runs its last.
        let lines = text.lines().collect::<Vec<_>>();
        let failed = (0..count)
            .rev()
            .find(|index| {
                let range = sections[*index].start.min(lines.len())..ends[*index].min(lines.len());
                lines[range]
                    .iter()
                    .any(|line| LogLine::new(line).is_some_and(|line| line.tone == Tone::Error))
            })
            .unwrap_or(count.saturating_sub(1));
        sections
            .into_iter()
            .enumerate()
            .map(|(index, section)| StepRow {
                number: index as u64 + 1,
                name: section.name.clone(),
                state: match job_state {
                    State::Failed if index != failed => State::Passed,
                    State::Running if index + 1 < count => State::Passed,
                    state => state,
                },
                seconds: section
                    .started
                    .as_ref()
                    .zip(section.completed.as_ref())
                    .and_then(|(from, to)| seconds(from.as_str(), to.as_str())),
                started: section.started.as_ref().map(|at| at.as_str().to_owned()),
                value: None,
                section: Some(section.start..ends[index].max(section.end)),
            })
            .collect()
    }

    /// Whether a tree node is open: what the user chose, else the default (failing and
    /// running jobs, failing steps).
    fn is_open(&self, node: &Node, default: bool) -> bool {
        self.actions.opened.contains(node) || default && !self.actions.closed.contains(node)
    }

    fn set_open(&mut self, node: Node, open: bool) {
        if open {
            self.actions.closed.remove(&node);
            self.actions.opened.insert(node);
        } else {
            self.actions.opened.remove(&node);
            self.actions.closed.insert(node);
        }
    }

    /// The jobs whose logs the tree shows now: open, finished jobs with an open step, their
    /// full log, or no step list of their own (their steps come from the log).
    pub(super) fn wanted_logs(&self) -> Vec<u64> {
        if self.page != 3 || !self.capabilities().job_log.allowed() {
            return Vec::new();
        }
        self.jobs()
            .into_iter()
            .filter(|job| {
                let id = number(job, "databaseId");
                let state = State::of(job);
                let done = state.finished() && string(job, "status") == "completed";
                if !done && state != State::Running
                    || state == State::Skipped && array(job, "steps").is_empty()
                    || !self.is_open(
                        &Node::Job(id),
                        matches!(state, State::Failed | State::Running),
                    )
                {
                    return false;
                }
                self.actions.full.contains(&id)
                    || array(job, "steps").is_empty()
                    || self.steps(job).iter().any(|step| {
                        step.state != State::Skipped
                            && self.is_open(
                                &Node::Step(id, step.number),
                                step_opens(state, step.state),
                            )
                    })
            })
            .map(|job| number(job, "databaseId"))
            .collect()
    }

    /// The logs to read again on the refresh tick: running jobs' (their tail follows the
    /// output), and once more those that finished since.
    pub(super) fn live_logs(&mut self) -> Vec<u64> {
        let running = self
            .wanted_logs()
            .into_iter()
            .filter(|id| {
                self.jobs()
                    .into_iter()
                    .any(|job| number(job, "databaseId") == *id && State::of(job) == State::Running)
            })
            .collect::<BTreeSet<_>>();
        let finished = self
            .actions
            .live
            .difference(&running)
            .copied()
            .collect::<Vec<_>>();
        self.actions.live = running.clone();
        running.into_iter().chain(finished).collect()
    }

    /// Opens a run from elsewhere (Enter on a check that links to its run).
    pub(super) fn open_run(&mut self, id: u64) {
        self.run_id = Some(id);
        self.actions.pinned = true;
        self.actions.workflows = false;
        self.actions.right = true;
        self.actions.open = true;
        self.ensure(false);
    }

    /// A finished run's artifacts are read for the tree's `Artifacts (N)` node.
    pub(super) fn wanted_artifacts(&self) -> Option<u64> {
        if self.page != 3 || self.capabilities().artifacts_list.reason().is_some() {
            return None;
        }
        let id = self.run_id?;
        let run = self.run_value(id)?;
        (State::of(run).finished() || self.actions.opened.contains(&Node::Artifacts)).then_some(id)
    }

    fn artifacts(&self) -> Vec<&Value> {
        self.run_id
            .and_then(|id| self.value(&Key::Artifacts(id)))
            .map(|value| array(value, "Artifacts").iter().collect())
            .unwrap_or_default()
    }

    /// Whether a tree node is an archive the forge no longer keeps.
    fn expired_artifact(&self, node: &Node) -> bool {
        let Node::Artifact(id) = node else {
            return false;
        };
        self.artifacts()
            .into_iter()
            .any(|artifact| artifact_id(artifact) == *id && expired(artifact))
    }

    /// The earlier attempts the attempts view lists, read for their outcome and time.
    pub(super) fn wanted_attempts(&self) -> Vec<u32> {
        if self.page != 3 || !self.actions.attempts {
            return Vec::new();
        }
        (1..self.latest_attempt())
            .filter_map(|attempt| u32::try_from(attempt).ok())
            .filter(|attempt| *attempt != self.attempt)
            .collect()
    }

    /// The open artifact's file list.
    pub(super) fn wanted_artifact_files(&self) -> Option<u64> {
        self.wanted_artifacts()?;
        if !self.actions.opened.contains(&Node::Artifacts) {
            return None;
        }
        // Every open archive is read in turn: the first one not read yet, else the first.
        let open = self
            .artifacts()
            .into_iter()
            .filter(|artifact| {
                self.actions
                    .opened
                    .contains(&Node::Artifact(artifact_id(artifact)))
                    && !expired(artifact)
            })
            .map(artifact_id)
            .collect::<Vec<_>>();
        open.iter()
            .find(|id| !self.cache.contains_key(&Key::ArtifactFiles(**id)))
            .or(open.first())
            .copied()
    }

    /// A job's log rows: one step's slice, or the whole job.
    #[allow(clippy::unnecessary_unwrap)] // `full` and the step are checked together
    /// [`Self::read_log_body`], read once per log arrival and step: the run tree is built on
    /// every key and frame, and a long log must not be parsed on each.
    fn log_body(&self, job: &Value, step: Option<&StepRow>, full: bool) -> Body {
        use std::hash::{Hash, Hasher};
        let id = number(job, "databaseId");
        let Some(Ok(reply)) = self.cache.get(&Key::Log(id)) else {
            return self.read_log_body(job, step, full);
        };
        let raw = reply.text.as_bytes();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (id, full, format!("{:?}", State::of(job))).hash(&mut hasher);
        (
            raw.as_ptr() as usize,
            raw.len(),
            &raw[raw.len().saturating_sub(256)..],
        )
            .hash(&mut hasher);
        if let Some(step) = step {
            let value = step.value.as_ref().map(ToString::to_string);
            (
                step.number,
                format!("{:?}", step.state),
                value,
                step.section.clone(),
            )
                .hash(&mut hasher);
        }
        let key = hasher.finish();
        let mut memo = self
            .actions
            .log_memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(body) = memo.get(&key) {
            return body.clone();
        }
        let body = self.read_log_body(job, step, full);
        // Bounded: the logs opened lately.
        if memo.len() >= 64 {
            memo.clear();
        }
        memo.insert(key, body.clone());
        body
    }

    fn read_log_body(&self, job: &Value, step: Option<&StepRow>, full: bool) -> Body {
        let id = number(job, "databaseId");
        let running = State::of(job) == State::Running;
        match State::of(job) {
            State::Queued | State::Waiting => {
                return Body::Message("Queued · nothing has run yet".into(), GitRole::Muted);
            }
            State::Skipped if full || step.is_none() => {
                return Body::Message("Skipped · no commands ran".into(), GitRole::Muted);
            }
            state
                if string(job, "startedAt").is_empty()
                    && state != State::Failed
                    && array(job, "steps").is_empty() =>
            {
                return Body::Message("Never started · no log".into(), GitRole::Muted);
            }
            _ => {}
        }
        if !full
            && let Some(step) = step
            && step.state == State::Skipped
        {
            return Body::Message("Skipped · no commands ran".into(), GitRole::Muted);
        }
        let raw = match self.cache.get(&Key::Log(id)) {
            Some(Ok(reply)) => reply.text.as_str(),
            // Not every forge serves a log before the job ends.
            Some(Err(_)) | None if running && !self.capabilities().job_log.allowed() => {
                return Body::Message(
                    "Running · the log appears when the job finishes".into(),
                    GitRole::Modified,
                );
            }
            Some(Err(_)) if running => {
                return Body::Message(
                    "Running · the log appears when the job finishes".into(),
                    GitRole::Modified,
                );
            }
            // A log the forge no longer has: GitHub's own words, calm, as in the Checks tab.
            Some(Err(error)) if self.explain(error).headline.starts_with("Not found") => {
                return Body::Message("This job's log has expired".into(), GitRole::Muted);
            }
            Some(Err(error)) => return Body::Message(calm(self, error), GitRole::Removed),
            None if !self.capabilities().job_log.allowed() => {
                return Body::Message(
                    self.capabilities()
                        .job_log
                        .reason()
                        .unwrap_or("Logs are unavailable")
                        .into(),
                    GitRole::Muted,
                );
            }
            None => return Body::Message("Loading log…".into(), GitRole::Muted),
        };
        if raw == crate::forge::provider::gitea::NEVER_RAN {
            return Body::Message(raw.to_owned(), GitRole::Muted);
        }
        let payloads: Vec<&str> = if let Some(step) = step.filter(|_| !full) {
            if let Some(value) = &step.value {
                // A slice by time (GitHub's job log names no steps) is as good as the stamps.
                let (lines, _) = logs::step_lines(raw, value);
                if lines.is_empty() && step.state == State::Running {
                    return Body::Message("Running · no output yet".into(), GitRole::Modified);
                }
                if lines.is_empty() {
                    return Body::Message(
                        "No lines in this step · f shows the full job log".into(),
                        GitRole::Muted,
                    );
                }
                lines
            } else {
                let all = raw
                    .lines()
                    .map(|line| logs::unpack(line).1)
                    .collect::<Vec<_>>();
                let range = step.section.clone().unwrap_or(0..all.len());
                let mut lines = all
                    .get(range.start.min(all.len())..range.end.min(all.len()))
                    .map(<[&str]>::to_vec)
                    .unwrap_or_default();
                // GitLab's `section_start` heading is the step's name, already on its row.
                if lines
                    .first()
                    .is_some_and(|line| line.contains("section_start:"))
                {
                    lines.remove(0);
                }
                lines
            }
        } else {
            raw.lines().map(|line| logs::unpack(line).1).collect()
        };
        let lines = log_lines(&payloads, !full);
        if lines.is_empty() {
            return Body::Message(
                if running {
                    "Running · no output yet"
                } else {
                    "The log is empty"
                }
                .into(),
                if running {
                    GitRole::Modified
                } else {
                    GitRole::Muted
                },
            );
        }
        // A running step shows the tail it is writing, as `tail -f` would.
        let following = running && !full && step.is_some_and(|step| step.state == State::Running);
        let skip = if following {
            lines.len().saturating_sub(LIVE_TAIL)
        } else {
            0
        };
        Body::Lines {
            lines: std::sync::Arc::new(lines.into_iter().map(std::sync::Arc::new).collect()),
            skip,
        }
    }

    fn push_log(&self, rows: &mut Vec<Row>, job: &Value, step: Option<&StepRow>, depth: u16) {
        let id = number(job, "databaseId");
        let at = step.map_or(0, |step| step.number);
        match self.log_body(job, step, step.is_none()) {
            Body::Message(text, role) => rows.push(Row::new(
                Node::Note(id, at),
                depth,
                vec![(text, st(role, PANE))],
            )),
            Body::Lines { lines, skip } => {
                for (index, line) in lines.iter().enumerate().skip(skip) {
                    let mut row = Row::new(Node::Line(id, at, index), depth, Vec::new());
                    row.log = Some((index + 1, std::sync::Arc::clone(line)));
                    rows.push(row);
                }
            }
        }
    }

    /// The run tree: jobs, their steps and open logs, then the run's artifacts.
    fn tree(&self, mode: ExplorerIconMode) -> Vec<Row> {
        let mut rows = Vec::new();
        for job in self.jobs() {
            let id = number(job, "databaseId");
            let state = State::of(job);
            let nothing_ran = state == State::Skipped && array(job, "steps").is_empty();
            let open = !nothing_ran
                && self.is_open(
                    &Node::Job(id),
                    matches!(state, State::Failed | State::Running),
                );
            let mut row = Row::new(
                Node::Job(id),
                0,
                vec![
                    (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
                    (
                        string(job, "name").to_owned(),
                        if state == State::Failed {
                            bold(GitRole::Text, PANE)
                        } else {
                            st(
                                if nothing_ran {
                                    GitRole::Muted
                                } else {
                                    GitRole::Text
                                },
                                PANE,
                            )
                        },
                    ),
                ],
            );
            row.fold = (!nothing_ran).then_some(open);
            row.state = Some(state);
            row.right = match state {
                // Forgejo's jobs carry no start: their first step's start says it.
                State::Running => elapsed(string(job, "startedAt"))
                    .or_else(|| {
                        array(job, "steps")
                            .iter()
                            .map(|step| string(step, "startedAt"))
                            .filter(|stamp| records::epoch(stamp).is_some_and(|at| at > 0))
                            .min()
                            .and_then(elapsed)
                    })
                    .map(|time| vec![(format!("running {time}"), st(GitRole::Modified, PANE))])
                    .unwrap_or_else(|| vec![("running".into(), st(GitRole::Modified, PANE))]),
                State::Queued | State::Waiting => vec![("queued".into(), st(GitRole::Muted, PANE))],
                _ => job_seconds(job)
                    .map(|time| vec![(duration(time), st(GitRole::Muted, PANE))])
                    .unwrap_or_default(),
            };
            rows.push(row);
            if !open {
                continue;
            }
            if self.actions.full.contains(&id) {
                self.push_log(&mut rows, job, None, 1);
                continue;
            }
            let steps = self.steps(job);
            if steps.is_empty() {
                self.push_log(&mut rows, job, None, 1);
                continue;
            }
            for step in &steps {
                let skipped = step.state == State::Skipped;
                let node = Node::Step(id, step.number);
                let step_open = !skipped && self.is_open(&node, step_opens(state, step.state));
                let mut row = Row::new(
                    node,
                    1,
                    vec![
                        (
                            format!("{} ", step.state.glyph(mode)),
                            st(step.state.role(), PANE),
                        ),
                        (
                            step.name.clone(),
                            match step.state {
                                State::Failed => bold(GitRole::Text, PANE),
                                State::Skipped => st(GitRole::Muted, PANE),
                                _ => st(GitRole::Text, PANE),
                            },
                        ),
                    ],
                );
                row.fold = (!skipped).then_some(step_open);
                row.state = Some(step.state);
                row.right = match step.state {
                    State::Skipped => vec![("–".into(), st(GitRole::Muted, PANE))],
                    State::Running => {
                        let time = step.started.as_deref().and_then(elapsed);
                        vec![(
                            time.map_or_else(|| "running".into(), |time| format!("running {time}")),
                            st(GitRole::Modified, PANE),
                        )]
                    }
                    _ => step
                        .seconds
                        .map(|time| vec![(duration(time), st(GitRole::Muted, PANE))])
                        .unwrap_or_default(),
                };
                rows.push(row);
                if step_open {
                    self.push_log(&mut rows, job, Some(step), 2);
                }
            }
        }
        self.push_artifacts(&mut rows, mode);
        say_gone_logs_once(&mut rows);
        rows
    }

    fn push_artifacts(&self, rows: &mut Vec<Row>, mode: ExplorerIconMode) {
        let Some(id) = self.run_id else {
            return;
        };
        if rows.is_empty() || self.capabilities().artifacts_list.reason().is_some() {
            return;
        }
        let artifacts = self.artifacts();
        let state = self.cache.get(&Key::Artifacts(id));
        if state.is_none()
            && !self.actions.opened.contains(&Node::Artifacts)
            && self.wanted_artifacts().is_none()
        {
            return;
        }
        let count = matches!(state, Some(Ok(_))).then_some(artifacts.len());
        let open = self.actions.opened.contains(&Node::Artifacts);
        let label = match count {
            None => "Artifacts".to_owned(),
            Some(0) => "No artifacts".to_owned(),
            Some(count) => format!("Artifacts ({count})"),
        };
        let mut row = Row::new(
            Node::Artifacts,
            0,
            vec![
                (
                    format!("{} ", glyph(Glyph::File, mode)),
                    st(GitRole::Muted, PANE),
                ),
                (
                    label,
                    st(
                        if count == Some(0) {
                            GitRole::Muted
                        } else {
                            GitRole::Text
                        },
                        PANE,
                    ),
                ),
            ],
        );
        row.fold = (count != Some(0)).then_some(open);
        rows.push(row);
        if !open {
            return;
        }
        let note = |text: String, role| Row::new(Node::Note(0, 0), 1, vec![(text, st(role, PANE))]);
        match state {
            None => rows.push(note("Loading artifacts…".into(), GitRole::Muted)),
            Some(Err(error)) => rows.push(note(calm(self, error), GitRole::Removed)),
            Some(Ok(_)) => {
                for artifact in artifacts {
                    let artifact_id = artifact_id(artifact);
                    let gone = expired(artifact);
                    let node = Node::Artifact(artifact_id);
                    let open = self.actions.opened.contains(&node);
                    let mut row = Row::new(
                        node,
                        1,
                        vec![(
                            string(artifact, "name").to_owned(),
                            st(if gone { GitRole::Muted } else { GitRole::Text }, PANE),
                        )],
                    );
                    row.fold = Some(open);
                    let (when, role) = expires(artifact);
                    let bytes = size(number(artifact, "size"));
                    // The forge's size is the archive's, not its files' (`zip 328 B`).
                    if !bytes.is_empty() {
                        row.right
                            .push((format!("zip {bytes} · "), st(GitRole::Muted, PANE)));
                    }
                    row.right.push((when, st(role, PANE)));
                    rows.push(row);
                    if !open {
                        continue;
                    }
                    let files_key = Key::ArtifactFiles(artifact_id);
                    // Under the archive, in its members' column.
                    let member_note = |text: String, role| {
                        Row::new(Node::Note(0, 0), 2, vec![(text, st(role, PANE))])
                    };
                    if gone {
                        rows.push(member_note(
                            "Expired · the forge no longer keeps it".into(),
                            GitRole::Removed,
                        ));
                        continue;
                    }
                    match self.cache.get(&files_key) {
                        None => {
                            rows.push(member_note("Reading the archive…".into(), GitRole::Muted))
                        }
                        Some(Err(error)) => {
                            rows.push(member_note(calm(self, error), GitRole::Removed))
                        }
                        Some(Ok(reply)) => {
                            for (index, file) in
                                array(&reply.value, "ArtifactFiles").iter().enumerate()
                            {
                                let mut row = Row::new(
                                    Node::File(artifact_id, index),
                                    2,
                                    // Body colour: a member opens nothing.
                                    vec![(
                                        string(file, "path").to_owned(),
                                        st(GitRole::Text, PANE),
                                    )],
                                );
                                row.right =
                                    vec![(size(number(file, "size")), st(GitRole::Muted, PANE))];
                                rows.push(row);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Where the cursor rests: the first error under an open failing step until a key moves
    /// it, else the remembered node.
    fn cursor_index(&self, rows: &[Row]) -> usize {
        if self.actions.auto
            && let Some(at) = rows
                .iter()
                .position(Row::error)
                .or_else(|| {
                    rows.iter().position(|row| {
                        matches!(row.node, Node::Step(..)) && row.state == Some(State::Failed)
                    })
                })
                .or_else(|| rows.iter().position(|row| row.state == Some(State::Failed)))
                .or_else(|| {
                    rows.iter().position(|row| {
                        matches!(row.node, Node::Step(..)) && row.state == Some(State::Running)
                    })
                })
        {
            return at;
        }
        self.actions
            .cursor
            .as_ref()
            .and_then(|node| rows.iter().position(|row| &row.node == node))
            .unwrap_or(0)
            .min(rows.len().saturating_sub(1))
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
            .map(|run| number(run, "attempt").max(number(&run["dusty"], "attempts")))
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

    /// `pull request`, `push`, `manual`, `schedule`: the event in words.
    fn event_word(&self, event: &str) -> String {
        if pull_event(event) {
            self.terms().pr_singular.to_owned()
        } else if manual_event(event) {
            "manual".into()
        } else {
            event.replace('_', " ")
        }
    }

    /// A listed run's second line, `Quality checks · #18 · maya · 9h ago`, in `room` columns.
    /// The pull request's number says the event; other events (`push`, `manual`) are named.
    /// Short of room the event goes first, then the actor, then the workflow is cut: the age
    /// stays.
    fn run_meta(&self, run: &Value, title: &str, room: usize) -> String {
        let event = string(run, "event");
        let word = self.event_word(event);
        let workflow = self.named_workflow(run);
        let workflow = workflow.as_str();
        // GitLab's pipelines are named after their source (`Merge request`, `Push`).
        let workflow =
            (!workflow.is_empty() && workflow != title && !workflow.eq_ignore_ascii_case(&word))
                .then(|| workflow.to_owned());
        // A title that already says the place or the event (GitLab's `topic-11 · push`) is
        // not repeated under it.
        let titled = |part: &str| {
            title
                .split(" · ")
                .any(|said| said.trim().eq_ignore_ascii_case(part))
        };
        let pull = self.pull_ref(run);
        let place = pull
            .clone()
            .or_else(|| {
                Some(string(run, "headBranch").to_owned())
                    .filter(|branch| !branch.is_empty() && !branch.starts_with("refs/"))
            })
            .filter(|place| !titled(place));
        let event = (pull.is_none()
            && !pull_event(event)
            && !word.is_empty()
            && !titled(&word)
            && !titled(event))
        .then_some(word);
        let actor =
            Some(string(&run["dusty"], "actor").to_owned()).filter(|actor| !actor.is_empty());
        let created = string(run, "createdAt");
        // Every row keeps its age slot, a running one too: the second line
        // has one schema, and the first line's `running 2m` is how long, not when.
        let age = (!created.is_empty()).then(|| records::ago(created, records::now()));
        let join = |workflow: &Option<String>, event: &Option<String>, actor: &Option<String>| {
            [workflow, &place, event, actor, &age]
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let mut line = join(&workflow, &event, &actor);
        for (event, actor) in [(None, actor.clone()), (None, None)] {
            if line.width() <= room {
                return line;
            }
            line = join(&workflow, &event, &actor);
        }
        if line.width() <= room {
            return line;
        }
        let Some(workflow) = workflow else {
            return line;
        };
        // The workflow name is cut to what the place and age leave.
        let rest = join(&None, &None, &None);
        let left = room.saturating_sub(rest.width() + 3);
        if left < 6 {
            return rest;
        }
        format!("{} · {rest}", chrome::ellipsize(&workflow, left))
    }

    /// `#18` (`!18` on GitLab) for a run a pull request started.
    fn pull_ref(&self, run: &Value) -> Option<String> {
        run["dusty"]["prNumber"]
            .as_u64()
            .filter(|_| {
                pull_event(string(run, "event")) || run["dusty"]["prNumber"].as_u64().is_some()
            })
            .map(|pr| format!("{}{pr}", self.terms().pr_glyph))
    }

    /// The jobs part of a run that lists none.
    fn no_jobs(&self) -> &'static str {
        match self.run_id.and_then(|id| self.run_value(id)).map(State::of) {
            Some(State::Skipped) => "Skipped · no jobs ran",
            Some(State::Cancelled) => "Cancelled · no jobs ran",
            Some(State::Waiting) => "Waiting · no jobs have started",
            Some(state) if state.finished() => "No jobs ran",
            _ => "No jobs yet",
        }
    }

    /// The run's duration: the forge's, else its jobs' span once its detail is loaded.
    fn run_seconds(&self, run: &Value) -> Option<i64> {
        if let Some(seconds) = run["dusty"]["durationSeconds"].as_i64() {
            return Some(seconds);
        }
        let id = number(run, "databaseId");
        let detail = self
            .value(&Key::Run(id, self.attempt))
            .or_else(|| self.value(&Key::Run(id, 0)))?;
        let jobs = array(detail, "jobs");
        let start = jobs
            .iter()
            .filter_map(|job| records::epoch(string(job, "startedAt")))
            .min()?;
        let end = jobs
            .iter()
            .filter_map(|job| records::epoch(string(job, "completedAt")))
            .max()?;
        (end >= start && jobs.iter().all(|job| string(job, "status") == "completed"))
            .then_some(end - start)
    }

    /// The right end of a run's row: its duration, or how long it has been running.
    fn run_time(&self, run: &Value) -> (String, GitRole) {
        match State::of(run) {
            State::Running => (
                elapsed(string(run, "createdAt"))
                    .map_or_else(|| "running".into(), |time| format!("running {time}")),
                GitRole::Modified,
            ),
            State::Queued => ("queued".into(), GitRole::Muted),
            State::Waiting => ("waiting".into(), GitRole::Modified),
            // A run that never started has no duration to show.
            State::Skipped => (String::new(), GitRole::Muted),
            _ => self
                .run_seconds(run)
                .map_or((String::new(), GitRole::Muted), |seconds| {
                    (duration(seconds), GitRole::Muted)
                }),
        }
    }

    // --------------------------------------------------------------------------- moves

    /// Keep a run selected once the runs arrive, and start each run attempt's tree from its
    /// defaults.
    pub(super) fn sync_actions(&mut self) {
        if self.page != 3 {
            return;
        }
        let runs = self.shown_runs();
        if !runs.is_empty() {
            let position = self
                .run_id
                .and_then(|id| runs.iter().position(|run| number(run, "databaseId") == id));
            match position {
                Some(at) => self.run_selected = at,
                None if self.actions.pinned && self.run_id.is_some() => {}
                None => {
                    let at = self.run_selected.min(runs.len() - 1);
                    let id = number(runs[at], "databaseId");
                    self.run_selected = at;
                    self.run_id = Some(id);
                    self.attempt = 0;
                    self.ensure(false);
                }
            }
        }
        let identity = format!("{}/{}", self.run_id.unwrap_or(0), self.attempt);
        if self.actions.tree_for != identity {
            let view = &mut self.actions;
            view.tree_for = identity;
            view.opened.clear();
            view.closed.clear();
            view.full.clear();
            view.cursor = None;
            view.auto = true;
            view.scroll = 0;
            view.pan = 0;
            view.attempts = false;
            view.find = Input::default();
            view.find_current = 0;
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
        self.actions.pinned = false;
        if self.run_id != Some(id) {
            self.run_id = Some(id);
            self.attempt = 0;
        }
        self.sync_actions();
        self.ensure(false);
    }

    fn choose_section(&mut self, section: usize) {
        self.actions.section = section;
        // The run opens afresh at its first error, even when it was the one shown.
        self.actions.tree_for.clear();
        self.actions.pinned = false;
        self.run_selected = 0;
        self.run_id = None;
        self.attempt = 0;
        self.sync_actions();
        self.ensure(false);
    }

    fn move_tree(&mut self, delta: isize) {
        let rows = self.tree(KEYS);
        if rows.is_empty() {
            return;
        }
        let at = self
            .cursor_index(&rows)
            .saturating_add_signed(delta)
            .min(rows.len() - 1);
        self.actions.cursor = Some(rows[at].node.clone());
        self.actions.auto = false;
        self.actions.follow_match = false;
    }

    /// Opens or closes the node under the cursor.
    fn toggle(&mut self, rows: &[Row], at: usize) {
        let Some(row) = rows.get(at) else {
            return;
        };
        let Some(open) = row.fold else {
            return;
        };
        let node = row.node.clone();
        self.set_open(node.clone(), !open);
        self.actions.cursor = Some(node);
        self.actions.auto = false;
        self.ensure(false);
    }

    /// `f`: the cursor's job shows its whole log (or its steps again).
    fn toggle_full(&mut self, rows: &[Row], at: usize) -> Result<(), String> {
        let job = rows
            .get(at)
            .and_then(|row| job_of(&row.node))
            .or_else(|| {
                rows.iter()
                    .find(|row| row.state == Some(State::Failed))
                    .and_then(|row| job_of(&row.node))
            })
            .ok_or("Select a job first")?;
        if !self.actions.full.remove(&job) {
            self.actions.full.insert(job);
        }
        self.set_open(Node::Job(job), true);
        self.ensure(false);
        let rows = self.tree(KEYS);
        let first_error = rows
            .iter()
            .find(|row| matches!(row.node, Node::Line(id, 0, _) if id == job) && row.error())
            .map(|row| row.node.clone());
        self.actions.cursor = Some(first_error.unwrap_or(Node::Job(job)));
        self.actions.auto = false;
        self.actions.find = Input::default();
        Ok(())
    }

    /// `n`/`N` without a find: the next or previous error line.
    fn jump_error(&mut self, forward: bool) -> Result<(), String> {
        let rows = self.tree(KEYS);
        let errors = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.error())
            .map(|(at, _)| at)
            .collect::<Vec<_>>();
        if errors.is_empty() {
            return Err("No errors in the open logs".into());
        }
        let at = self.cursor_index(&rows);
        let next = if forward {
            errors
                .iter()
                .copied()
                .find(|index| *index > at)
                .unwrap_or(errors[0])
        } else {
            errors
                .iter()
                .rev()
                .copied()
                .find(|index| *index < at)
                .unwrap_or(errors[errors.len() - 1])
        };
        self.actions.cursor = Some(rows[next].node.clone());
        self.actions.auto = false;
        Ok(())
    }

    fn rail_rows(&self) -> Vec<(usize, Option<String>)> {
        // GitLab's "workflows" are its pipeline sources (`push`, `web`), which the Event group
        // already lists in plain words (`Push`, `Manual`): one group, not two (A2-06).
        let pipelines = self.terms().run_noun == "pipeline";
        let mut rows = if pipelines {
            Vec::new()
        } else {
            vec![(0, None)]
        };
        rows.extend(
            self.workflows()
                .iter()
                .filter(|_| !pipelines)
                .map(|workflow| (0, Some(string(workflow, "name").to_owned()))),
        );
        // Forgejo lists no workflows: the runs name theirs (`quality`, `preview`).
        if self.workflows().is_empty() && !pipelines {
            let mut seen = Vec::<String>::new();
            for run in self.runs() {
                let name = self.run_workflow(run);
                if !name.is_empty() && !seen.contains(&name) {
                    seen.push(name);
                }
            }
            rows.extend(seen.into_iter().map(|name| (0, Some(name))));
        }
        rows.push((1, None));
        let mut branches = Vec::<String>::new();
        let mut events = Vec::<String>::new();
        for run in self.runs() {
            let branch = string(run, "headBranch");
            if !branch.is_empty()
                && !branch.starts_with("refs/")
                && !branches.iter().any(|known| known == branch)
            {
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
        let Some((section, value)) = rows
            .get(self.actions.rail.min(rows.len().saturating_sub(1)))
            .cloned()
        else {
            return;
        };
        match section {
            0 if self.workflows().is_empty() => self.actions.workflow = value,
            0 => {
                self.workflow_selected = self
                    .workflows()
                    .iter()
                    .position(|workflow| Some(string(workflow, "name")) == value.as_deref())
                    .map_or(0, |at| at + 1);
            }
            1 => self.actions.branch = value,
            _ => self.actions.event = value,
        }
        self.actions.picker = false;
        self.actions.pinned = false;
        self.actions.tree_for.clear();
        self.run_selected = 0;
        self.run_id = None;
        self.attempt = 0;
        self.ensure(false);
        self.sync_actions();
    }

    fn rail_chosen(&self, section: usize, value: &Option<String>) -> bool {
        match section {
            0 if self.workflows().is_empty() => self.actions.workflow == *value,
            0 => {
                self.workflow_selected
                    == value.as_ref().map_or(0, |name| {
                        self.workflows()
                            .iter()
                            .position(|workflow| string(workflow, "name") == name)
                            .map_or(usize::MAX, |at| at + 1)
                    })
            }
            1 => self.actions.branch == *value,
            _ => self.actions.event == *value,
        }
    }

    /// The branch and event the list is narrowed to, in words. The workflow is the
    /// Workflows panel's row and the runs panel's title, not a filter word.
    fn filter_words(&self) -> Vec<String> {
        let mut words = Vec::new();
        words.extend(self.actions.branch.clone());
        words.extend(self.actions.event.clone());
        words
    }

    // ---------------------------------------------------------------------- workflows

    /// Every run, whichever workflow is chosen: the All row's count and each workflow's
    /// latest state when its own runs were never read.
    fn all_runs(&self) -> Vec<&Value> {
        self.value(&Key::Runs(0))
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    }

    /// The Workflows panel's rows: All, then the forge's workflows (GitLab: its pipeline
    /// sources and schedules), else the workflows the runs name (a Forgejo without a list).
    fn flow_rows(&self) -> Vec<Flow> {
        let mut rows = vec![Flow::All];
        let listed = self.workflows().len();
        if listed > 0 {
            rows.extend((0..listed).map(Flow::Listed));
        } else if self.terms().run_noun != "pipeline" {
            let mut seen = Vec::<String>::new();
            for run in self.all_runs() {
                let name = self.run_workflow(run);
                if !name.is_empty() && !seen.contains(&name) {
                    seen.push(name);
                }
            }
            rows.extend(seen.into_iter().map(Flow::Named));
        }
        rows
    }

    /// The workflow the runs list is narrowed to, by name; `None` on All.
    fn chosen_flow(&self) -> Option<String> {
        let rows = self.flow_rows();
        rows.get(self.flow_cursor(&rows))
            .filter(|flow| **flow != Flow::All)
            .map(|flow| self.flow_name(flow))
    }

    fn flow_cursor(&self, rows: &[Flow]) -> usize {
        let wanted = if self.workflow_selected > 0 {
            Flow::Listed(self.workflow_selected - 1)
        } else if let Some(name) = &self.actions.workflow {
            Flow::Named(name.clone())
        } else {
            Flow::All
        };
        rows.iter().position(|row| *row == wanted).unwrap_or(0)
    }

    /// The runs panel follows the chosen row at once, its first run selected.
    fn choose_flow(&mut self, at: usize) {
        let rows = self.flow_rows();
        let Some(row) = rows.get(at).cloned() else {
            return;
        };
        if self.flow_cursor(&rows) == at {
            return;
        }
        match row {
            Flow::All => {
                self.workflow_selected = 0;
                self.actions.workflow = None;
            }
            Flow::Listed(index) => {
                self.workflow_selected = index + 1;
                self.actions.workflow = None;
            }
            Flow::Named(name) => {
                self.workflow_selected = 0;
                self.actions.workflow = Some(name);
            }
        }
        self.actions.pinned = false;
        self.actions.tree_for.clear();
        self.run_selected = 0;
        self.run_id = None;
        self.attempt = 0;
        self.ensure(false);
        self.sync_actions();
    }

    /// `Workflows`; GitLab's are what starts a pipeline.
    fn flows_title(&self) -> &'static str {
        if self.terms().run_noun == "pipeline" {
            "Sources"
        } else {
            "Workflows"
        }
    }

    fn flow_name(&self, flow: &Flow) -> String {
        match flow {
            Flow::All if self.terms().run_noun == "pipeline" => "All pipelines".into(),
            Flow::All => "All workflows".into(),
            Flow::Listed(index) => self
                .workflows()
                .get(*index)
                .map(|workflow| string(workflow, "name").to_owned())
                .unwrap_or_default(),
            Flow::Named(name) => name.clone(),
        }
    }

    /// Turned off on the forge (`disabled_manually`, `disabled_inactivity`, an inactive
    /// GitLab schedule).
    fn flow_off(&self, flow: &Flow) -> bool {
        matches!(flow, Flow::Listed(index) if self
            .workflows()
            .get(*index)
            .is_some_and(|workflow| string(workflow, "state").starts_with("disabled")))
    }

    /// The state of a workflow's latest run: from its own runs once read, else from every
    /// run's (None before either arrives, or when it has none).
    fn flow_state(&self, flow: &Flow) -> Option<State> {
        let latest = match flow {
            Flow::All => None,
            Flow::Listed(index) => {
                let workflow = self.workflows().get(*index).copied()?;
                let id = number(workflow, "id");
                match self.value(&Key::Runs(id)).and_then(Value::as_array) {
                    Some(runs) => runs.first(),
                    None => {
                        let name = string(workflow, "name");
                        self.all_runs().into_iter().find(|run| {
                            number(run, "workflowDatabaseId") == id
                                || self.run_workflow(run) == name
                        })
                    }
                }
            }
            Flow::Named(name) => self
                .all_runs()
                .into_iter()
                .find(|run| self.run_workflow(run) == *name),
        };
        latest.map(State::of)
    }

    /// Why `d` cannot dispatch the chosen workflow, when it cannot. GitLab runs the
    /// project's one pipeline from any row.
    fn dispatch_refusal(&self) -> Option<String> {
        if self.terms().run_noun == "pipeline" {
            return None;
        }
        let rows = self.flow_rows();
        match rows.get(self.flow_cursor(&rows))? {
            Flow::All => Some(format!(
                "Pick a {} first: press 1 for {}",
                self.terms().workflow_noun,
                self.flows_title()
            )),
            Flow::Listed(index) => {
                let workflow = self.workflows().get(*index).copied()?;
                let name = string(workflow, "name");
                if string(workflow, "state").starts_with("disabled") {
                    Some(format!(
                        "{name} is turned off · turn it on in the forge to run it"
                    ))
                } else if workflow["dispatchable"] == Value::Bool(false) {
                    Some(format!(
                        "{name} can't be run manually · no workflow_dispatch trigger"
                    ))
                } else {
                    None
                }
            }
            Flow::Named(_) => None,
        }
    }

    // --------------------------------------------------------------------------- keys

    /// Whether Actions types into its own input (a find or filter row), or Notifications
    /// into its `/` filter: every key is the input's.
    pub(in crate::git::screen) fn actions_typing(&self) -> bool {
        self.page == 3 && (self.actions.find.editing || self.actions.filter.editing)
            || self.notifications_typing()
    }

    /// A run's log lines are selected (`v`): the mode chip reads VISUAL.
    pub(in crate::git::screen) fn actions_selecting(&self) -> bool {
        self.page == 3 && self.actions.visual.is_some()
    }

    /// `[`/`]` switch the run history's sections rather than the top tabs.
    pub(in crate::git::screen) fn actions_brackets(&self) -> bool {
        self.page == 3
    }

    /// The run pane (or the filter popup) has the keys: `a` is attempts there, not accounts.
    pub(super) fn actions_drilled(&self) -> bool {
        self.actions.right || self.actions.picker
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
            self.actions.pinned = false;
            self.sync_actions();
        }
        true
    }

    /// Put the cursor on a match: the first at or after it (`None`), or the next/previous.
    fn reveal_match(&mut self, step: Option<bool>) {
        let rows = self.tree(KEYS);
        let query = self.actions.find.query.clone();
        let found = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.log.as_ref().map(|(_, line)| (index, line)))
            .flat_map(|(index, line)| {
                matches(&line.text, &query)
                    .into_iter()
                    .map(move |range| (index, range))
            })
            .collect::<Vec<_>>();
        if found.is_empty() {
            self.actions.find_current = 0;
            return;
        }
        let cursor = self.cursor_index(&rows);
        let current = match step {
            None => found
                .iter()
                .position(|(row, _)| *row >= cursor)
                .unwrap_or(0),
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
        let (row, range) = &found[current];
        self.actions.cursor = Some(rows[*row].node.clone());
        self.actions.auto = false;
        self.actions.follow_match = true;
        if self.actions.nowrap
            && let Some((_, line)) = &rows[*row].log
        {
            let start = line.text[..range.start].width();
            let end = start + line.text[range.clone()].width();
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
        // The shared key table (`keymap.rs`): `Space e` hides the left column. The filter
        // popup and the attempts list keep their own keys.
        if !self.actions.picker && !self.actions.attempts {
            match self.chord_key(key) {
                Step::Wait | Step::Cancel => return Some(Ok(None)),
                Step::Run(Named::ToggleSidebar) => {
                    self.toggle_sidebar();
                    if self.sidebar_hidden() && !self.actions.narrow {
                        self.focus_panel(2);
                    }
                    return Some(Ok(None));
                }
                // lazygit's Space: what Enter opens here.
                Step::Run(Named::Toggle) => {
                    return self.actions_key(&KeyEvent::plain(KeyCode::Enter), write_refusal);
                }
                Step::Pass => {}
            }
        }
        let plain = !key.modifiers.control && !key.modifiers.alt;
        if key.code == KeyCode::Char('q') && plain && !self.actions.picker {
            return None;
        }
        self.notice.clear();
        self.notice_error = false;
        if self.actions.picker {
            return Some(self.picker_key(key));
        }
        // Panels like the Git tab's: 1 Workflows, 2 runs, 3 the run.
        if plain && !self.actions.attempts {
            match key.code {
                KeyCode::Char(c @ '1'..='3') => {
                    if c == '3' && self.run_id.is_none() {
                        return Some(Err(format!("Select a {} first", self.actions_noun())));
                    }
                    self.focus_panel(usize::from(c as u8 - b'1'));
                    return Some(Ok(None));
                }
                // Long lines wrap by default, in every panel.
                KeyCode::Char('w') => {
                    self.actions.nowrap = !self.actions.nowrap;
                    self.actions.pan = 0;
                    return Some(Ok(None));
                }
                _ => {}
            }
        }
        let id = self.run_id;
        // Sheets and refresh work in both panels.
        if plain {
            let sheet = match key.code {
                KeyCode::Char('r') => id.map(Action::Rerun),
                KeyCode::Char('x') => id.map(Action::Cancel),
                KeyCode::Char('A') => id.map(Action::Approve),
                KeyCode::Char('d') => Some(Action::Dispatch(0)),
                _ => None,
            };
            if matches!(sheet, Some(Action::Dispatch(_)))
                && let Some(reason) = self.dispatch_refusal()
            {
                return Some(Err(reason));
            }
            if let Some(action) = sheet {
                if let Some(reason) = write_refusal {
                    return Some(Err(reason.into()));
                }
                return Some(self.activate(action));
            }
            if matches!(key.code, KeyCode::Char('r' | 'x' | 'A')) {
                return Some(Err(format!("Select a {} first", self.actions_noun())));
            }
            // `o`: the run on the forge's website, as `o` opens a pull request's page.
            if key.code == KeyCode::Char('o') && !self.actions.attempts {
                let Some(id) = id else {
                    return Some(Err(format!("Select a {} first", self.actions_noun())));
                };
                let url = self
                    .run_value(id)
                    .map(|run| string(run, "url").to_owned())
                    .unwrap_or_default();
                if !url.starts_with("https://") && !url.starts_with("http://") {
                    return Some(Err(format!("This {} has no web page", self.actions_noun())));
                }
                return Some(self.activate(Action::Web(3, id, url)));
            }
            if key.code == KeyCode::Char('R') {
                if self.writing() {
                    return Some(Err(
                        "Wait for the confirmed operation before refreshing".into()
                    ));
                }
                self.ensure(true);
                return Some(Ok(None));
            }
            if matches!(key.code, KeyCode::Char('[' | ']')) {
                let count = self.section_tabs().len();
                let forward = key.code == KeyCode::Char(']');
                let section = (self.actions.section.min(count - 1)
                    + if forward { 1 } else { count - 1 })
                    % count;
                self.choose_section(section);
                return Some(Ok(None));
            }
        }
        if self.actions.right {
            Some(self.run_key(key, write_refusal))
        } else if self.actions.workflows {
            Some(self.workflows_key(key))
        } else {
            Some(self.history_key(key))
        }
    }

    /// The Workflows panel: moving chooses, so the runs below follow at once (as GitHub's).
    fn workflows_key(&mut self, key: &KeyEvent) -> Result<Option<Form>, String> {
        if key.modifiers.control {
            return Ok(None);
        }
        let rows = self.flow_rows();
        let at = self.flow_cursor(&rows);
        let last = rows.len().saturating_sub(1);
        let to = match key.code {
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => last,
            KeyCode::PageDown => (at + 10).min(last),
            KeyCode::PageUp => at.saturating_sub(10),
            KeyCode::Char('j') | KeyCode::Down => (at + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => at.saturating_sub(1),
            // Esc goes back to every workflow.
            KeyCode::Escape => 0,
            KeyCode::Tab if key.modifiers.shift => {
                self.focus_panel(2);
                return Ok(None);
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
                self.focus_panel(1);
                return Ok(None);
            }
            // The runs' own keys take the focus there.
            KeyCode::Char('/' | 'F') => {
                self.focus_panel(1);
                return self.history_key(key);
            }
            _ => return Ok(None),
        };
        self.choose_flow(to);
        Ok(None)
    }

    /// Focus one panel: 0 Workflows, 1 runs, 2 the run. A hidden left column comes back
    /// for its panels; the run needs one selected.
    pub(super) fn focus_panel(&mut self, panel: usize) {
        if panel >= 2 {
            if self.run_id.is_some() {
                self.actions.workflows = false;
                self.actions.right = true;
                self.actions.open = true;
            }
            return;
        }
        if self.actions.right {
            self.leave_run();
        }
        self.actions.workflows = panel == 0;
        if self.sidebar_hidden() && !self.actions.narrow {
            self.toggle_sidebar();
        }
    }

    /// 0 Workflows, 1 runs, 2 the run.
    fn panel(&self) -> usize {
        if self.actions.right {
            2
        } else if self.actions.workflows {
            0
        } else {
            1
        }
    }

    fn picker_key(&mut self, key: &KeyEvent) -> Result<Option<Form>, String> {
        let count = self.rail_rows().len();
        let delta = match key.code {
            KeyCode::Char('j') | KeyCode::Down => 1,
            KeyCode::Char('k') | KeyCode::Up => -1,
            KeyCode::Home => isize::MIN / 2,
            KeyCode::End => isize::MAX / 2,
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.choose_rail();
                return Ok(None);
            }
            KeyCode::Escape | KeyCode::Char('F' | 'q') => {
                self.actions.picker = false;
                return Ok(None);
            }
            _ => 0,
        };
        self.actions.rail = self
            .actions
            .rail
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        Ok(None)
    }

    fn history_key(&mut self, key: &KeyEvent) -> Result<Option<Form>, String> {
        let delta = match key.code {
            KeyCode::Home | KeyCode::Char('g') => Some(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => Some(isize::MAX / 2),
            KeyCode::PageDown => Some(10),
            KeyCode::PageUp => Some(-10),
            KeyCode::Char('j') | KeyCode::Down => Some(1),
            KeyCode::Char('k') | KeyCode::Up => Some(-1),
            _ => None,
        };
        if key.modifiers.control {
            if matches!(key.code, KeyCode::Char('l')) {
                self.actions.right = true;
                self.actions.open = true;
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
            KeyCode::Char('F') => {
                self.actions.picker = true;
                self.actions.rail = 0;
            }
            KeyCode::Escape if self.actions.filter.active() => {
                self.actions.filter = Input::default();
                self.sync_actions();
            }
            KeyCode::Escape if !self.filter_words().is_empty() => {
                self.actions.branch = None;
                self.actions.event = None;
                self.choose_section(self.actions.section);
            }
            // Opened from Notifications without a run to show: Esc goes back there.
            KeyCode::Escape if self.notify.back => self.back_to_notifications(),
            KeyCode::Tab if key.modifiers.shift => self.focus_panel(0),
            KeyCode::Char('h') | KeyCode::Left => self.focus_panel(0),
            KeyCode::Enter | KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
                self.focus_panel(2)
            }
            _ => {
                if let Some(delta) = delta {
                    self.select_run(delta);
                }
            }
        }
        Ok(None)
    }

    /// The rows a `v` selection spans: from where it started to the cursor, either way.
    fn visual_range(&self, rows: &[Row], cursor: usize) -> Option<std::ops::RangeInclusive<usize>> {
        let anchor = self.actions.visual.as_ref()?;
        let start = rows.iter().position(|row| row.node == *anchor)?;
        Some(start.min(cursor)..=start.max(cursor))
    }

    fn run_key(
        &mut self,
        key: &KeyEvent,
        write_refusal: Option<&str>,
    ) -> Result<Option<Form>, String> {
        let rows = self.tree(KEYS);
        let at = self.cursor_index(&rows);
        if key.modifiers.control {
            match key.code {
                KeyCode::Char('s') => {
                    let job = rows
                        .get(at)
                        .and_then(|row| job_of(&row.node))
                        .or_else(|| self.jobs().first().map(|job| number(job, "databaseId")))
                        .ok_or("Select a job first")?;
                    if self.raw(&Key::Log(job)).is_none() {
                        return Err("Open a step's log first".into());
                    }
                    return self.activate(Action::SaveLog(job));
                }
                KeyCode::Char('h') => self.leave_run(),
                _ => {}
            }
            return Ok(None);
        }
        let page = self.actions.rows.max(2) as isize - 1;
        let delta = match key.code {
            KeyCode::Home | KeyCode::Char('g') => Some(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => Some(isize::MAX / 2),
            KeyCode::PageDown => Some(page),
            KeyCode::PageUp => Some(-page),
            KeyCode::Char('j') | KeyCode::Down => Some(1),
            KeyCode::Char('k') | KeyCode::Up => Some(-1),
            _ => None,
        };
        if self.actions.attempts {
            match key.code {
                KeyCode::Escape | KeyCode::Char('a') => self.actions.attempts = false,
                KeyCode::Enter | KeyCode::Char('l') => {
                    let latest = self.latest_attempt();
                    let chosen = latest
                        .saturating_sub(self.actions.attempt_cursor as u64)
                        .max(1);
                    self.actions.attempts = false;
                    return self.activate(Action::Attempt(if chosen == latest {
                        0
                    } else {
                        chosen as u32
                    }));
                }
                _ => {
                    if let Some(delta) = delta {
                        let count = self.latest_attempt() as usize;
                        self.actions.attempt_cursor = self
                            .actions
                            .attempt_cursor
                            .saturating_add_signed(delta)
                            .min(count.saturating_sub(1));
                    }
                }
            }
            return Ok(None);
        }
        // `v`/`V` select log lines from the cursor; `y` copies them, or the cursor's line.
        match key.code {
            KeyCode::Char('v' | 'V') => {
                self.actions.visual = if self.actions.visual.is_some() {
                    None
                } else {
                    let node = rows.get(at).filter(|row| row.log.is_some());
                    Some(node.ok_or("Select a log line first")?.node.clone())
                };
                return Ok(None);
            }
            KeyCode::Escape if self.actions.visual.is_some() => {
                self.actions.visual = None;
                return Ok(None);
            }
            KeyCode::Char('y') => {
                let range = self.visual_range(&rows, at).unwrap_or(at..=at);
                let lines = rows[range]
                    .iter()
                    .filter_map(|row| row.log.as_ref())
                    .map(|(_, line)| line.text.as_str())
                    .collect::<Vec<_>>();
                if lines.is_empty() {
                    return Err("Select a log line to copy".into());
                }
                self.notice = match lines.len() {
                    1 => "Copied the line".into(),
                    count => format!("Copied {count} lines"),
                };
                self.copied = Some(lines.join("\n"));
                self.actions.visual = None;
                return Ok(None);
            }
            _ => {}
        }
        match key.code {
            KeyCode::Escape => {
                if self.actions.find.active() {
                    self.actions.find = Input::default();
                } else if self.actions.zoom {
                    self.actions.zoom = false;
                } else if self.notify.back {
                    // Opened from Notifications, Esc goes back there.
                    self.back_to_notifications();
                } else if !self.go_back() {
                    // Opened from a pull request's check, Esc went back to it (R9-05).
                    self.leave_run();
                }
            }
            // Tab goes round the panels; Shift+Tab, h and ← back to the runs.
            KeyCode::Tab if !key.modifiers.shift => self.focus_panel(0),
            KeyCode::Tab | KeyCode::Char('h') | KeyCode::Left => self.leave_run(),
            KeyCode::Char('z') => self.actions.zoom = !self.actions.zoom,
            KeyCode::Char('f') => self.toggle_full(&rows, at)?,
            KeyCode::Char('a') => {
                let caps = self.capabilities();
                if let Some(reason) = caps.attempts.reason() {
                    return Err(reason.into());
                }
                if self.latest_attempt() <= 1 {
                    return Err(format!("This {} has one attempt", self.actions_noun()));
                }
                self.actions.attempts = true;
                self.actions.attempt_cursor =
                    (self.latest_attempt() - self.effective_attempt().max(1)) as usize;
                // Each earlier attempt's outcome and time are read for its row.
                self.ensure(false);
            }
            KeyCode::Char('/') => {
                self.actions.find = Input {
                    query: String::new(),
                    editing: true,
                };
                self.actions.find_current = 0;
            }
            KeyCode::Char('n' | 'N') => {
                let forward = key.code == KeyCode::Char('n');
                if self.actions.find.active() {
                    self.reveal_match(Some(forward));
                } else {
                    self.jump_error(forward)?;
                }
            }
            KeyCode::Char('H') => {
                self.actions.follow_match = false;
                self.actions.pan = self.actions.pan.saturating_sub(PAN);
            }
            KeyCode::Char('L') => {
                if self.actions.nowrap {
                    self.actions.follow_match = false;
                    self.actions.pan = (self.actions.pan + PAN).min(4096);
                }
            }
            KeyCode::Enter => match rows.get(at).map(|row| &row.node) {
                Some(Node::Line(..)) if rows[at].link().is_some() => {
                    return self.open_source(&rows, at);
                }
                // A log line folds the step it belongs to, the way back up the tree (r12
                // gitlab-08: the footer offers `Enter fold` on every row of the tree).
                Some(Node::Line(..)) => {
                    if let Some(step) = rows[..at].iter().rposition(|row| row.fold == Some(true)) {
                        self.toggle(&rows, step);
                    }
                }
                // Enter opens an archive's files (Space did, before Space led chords); an
                // expired one says why it cannot.
                Some(Node::Artifact(artifact)) if self.expired_artifact(&rows[at].node) => {
                    let artifact = *artifact;
                    return self.download(artifact, write_refusal);
                }
                _ => self.toggle(&rows, at),
            },
            // `s` saves (downloads) the archive under the cursor.
            KeyCode::Char('s') => match rows.get(at).map(|row| &row.node) {
                Some(Node::Artifact(artifact)) => {
                    let artifact = *artifact;
                    return self.download(artifact, write_refusal);
                }
                _ => return Err("Select an artifact to download".into()),
            },
            KeyCode::Char('l') | KeyCode::Right => self.toggle(&rows, at),
            _ => {
                if let Some(delta) = delta {
                    self.move_tree(delta);
                }
            }
        }
        self.ensure(false);
        Ok(None)
    }

    /// Focus goes back to the run history (below 90 columns the list comes back).
    fn leave_run(&mut self) {
        // In the list the run is no longer the one the pull request (or Notifications)
        // opened: no way back.
        self.return_to = None;
        self.notify.back = false;
        self.actions.visual = None;
        self.actions.right = false;
        self.actions.workflows = false;
        self.actions.open = false;
        self.actions.zoom = false;
        self.actions.attempts = false;
    }

    fn download(&mut self, id: u64, write_refusal: Option<&str>) -> Result<Option<Form>, String> {
        let artifacts = self.artifacts();
        let artifact = artifacts
            .into_iter()
            .find(|artifact| artifact_id(artifact) == id)
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
        let action = Action::Download(id, string(artifact, "name").to_owned());
        self.activate(action)
    }

    /// Enter on a log line with `path:line`: open that file of this repository.
    fn open_source(&mut self, rows: &[Row], at: usize) -> Result<Option<Form>, String> {
        let Some((path, line)) = rows.get(at).and_then(Row::link) else {
            return Ok(None);
        };
        Err(source_target(&self.root, &path, line))
    }

    // --------------------------------------------------------------------------- writes

    /// A finished write the Actions tab reports itself.
    pub(super) fn actions_written(&mut self, write: &Write, summary: &str) {
        match write {
            Write::DownloadArtifact {
                name, directory, ..
            } => {
                let size = self
                    .artifacts()
                    .iter()
                    .find(|artifact| string(artifact, "name") == name)
                    .map(|artifact| size(number(artifact, "size")))
                    .unwrap_or_default();
                let shown = directory
                    .strip_prefix(&self.root)
                    .map(|path| format!("./{}", path.join(format!("{name}.zip")).display()))
                    .unwrap_or_else(|_| {
                        directory.join(format!("{name}.zip")).display().to_string()
                    });
                let text = if size.is_empty() {
                    format!("Downloaded · {shown}")
                } else {
                    format!("Downloaded · {size} saved · {shown}")
                };
                self.actions.receipt = self.run_id.map(|run| (run, text));
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

    /// Opens pending sheets when their details arrive, and correlates a dispatched run
    /// once the runs list shows it.
    pub(super) fn poll_actions(&mut self) {
        if let Some(id) = self.actions.pending_rerun
            && self.ready_form.is_none()
        {
            if self.run_id != Some(id) || self.page != 3 {
                self.actions.pending_rerun = None;
            } else if self.cache.contains_key(&Key::Run(id, self.attempt))
                || self.cache.contains_key(&Key::Run(id, 0))
            {
                self.actions.pending_rerun = None;
                match self.activate(Action::Rerun(id)) {
                    Ok(form) => self.ready_form = form,
                    Err(error) => {
                        self.notice = error;
                        self.notice_error = true;
                    }
                }
            } else {
                self.enqueue(Key::Run(id, self.attempt), self.run_read(id), false);
            }
        }
        if let Some(workflow) = self.actions.pending_dispatch
            && self.ready_form.is_none()
            && self
                .cache
                .contains_key(&Key::Workflow(workflow, String::new()))
        {
            // The sheet opens with a ref alone when the definition could not be read.
            self.actions.pending_dispatch = None;
            match self.activate(Action::Dispatch(workflow)) {
                Ok(form) => self.ready_form = form,
                Err(error) => {
                    self.notice = error;
                    self.notice_error = true;
                }
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
                    self.notice = "Workflow run started".into();
                    self.run_id = found.first().copied();
                    self.actions.dispatched = None;
                }
                count => {
                    self.notice = format!("{count} runs started just now · pick yours");
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
        let path = values
            .first()
            .map(|path| path.trim())
            .filter(|path| !path.is_empty())
            .ok_or("A file name is required")?;
        let raw = self.raw(&Key::Log(job)).ok_or("The log is not loaded")?;
        let text = raw
            .lines()
            .filter_map(|line| LogLine::new(logs::unpack(line).1))
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let target = self.root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Couldn't create the folder{}",
                    crate::git::screen::words::io_reason(&error)
                )
            })?;
        }
        std::fs::write(&target, text.join("\n") + "\n").map_err(|error| {
            format!(
                "Couldn't save the log{}",
                crate::git::screen::words::io_reason(&error)
            )
        })?;
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
            .map(|c| {
                if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        format!("{}.log", safe.trim_matches('-'))
    }

    /// The run a sheet acts on: (`run #7 · Fix the CI regression`, as the header names it,
    /// run number, the run).
    pub(super) fn sheet_run(&self, id: u64) -> Option<(String, u64, &Value)> {
        let run = self.run_value(id)?;
        let title = self.run_title(run);
        let number = run_number(run);
        let mut object = format!("{} #{number}", self.actions_noun());
        if !title.is_empty() {
            object = format!("{object} · {title}");
        }
        Some((object, number, run))
    }

    /// A run's workflow under one label across the screen: the forge's listed name
    /// (`Quality checks`) when it lists the workflow, else the run's own (`quality`).
    /// The workflow a row or header names beside the run: none on GitLab, whose "workflow" is
    /// the pipeline's source (`Run pipeline`, `Push`) and the event word says it (A2-05).
    fn named_workflow(&self, run: &Value) -> String {
        if self.terms().run_noun == "pipeline" {
            String::new()
        } else {
            self.run_workflow(run)
        }
    }

    /// A run's title as the list, header and sheets show it: a squash merge's `(#16)` goes
    /// when the run already names that pull request (A2-09).
    pub(super) fn run_title(&self, run: &Value) -> String {
        let title = string(run, "displayTitle");
        if title.is_empty() {
            return self.run_workflow(run);
        }
        if let Some(pull) = self.pull_ref(run) {
            let number = pull.trim_start_matches(|c: char| !c.is_ascii_digit());
            for suffix in [format!(" (#{number})"), format!(" ({pull})")] {
                if let Some(kept) = title
                    .strip_suffix(suffix.as_str())
                    .filter(|kept| !kept.trim().is_empty())
                {
                    return kept.trim_end().to_owned();
                }
            }
        }
        title.to_owned()
    }

    /// `from #18 · Checks`: the pull request and tab a deep-linked run returns to.
    fn return_crumb(&self) -> Option<String> {
        let target = self.return_to.as_ref()?;
        let tab = match target.tab {
            super::pr::CONVERSATION => "Conversation",
            super::pr::COMMITS => "Commits",
            super::pr::CHECKS => self.ci_words().0,
            _ => "Files changed",
        };
        Some(format!("from {} · {tab}", target.label))
    }

    pub(super) fn run_workflow(&self, run: &Value) -> String {
        let id = number(run, "workflowDatabaseId");
        self.workflows()
            .into_iter()
            .find(|workflow| id != 0 && number(workflow, "id") == id)
            .map(|workflow| string(workflow, "name"))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| string(run, "workflowName"))
            .to_owned()
    }

    /// The jobs of a run that failed, by name.
    fn failed_jobs(&self, id: u64) -> Vec<String> {
        let detail = self
            .value(&Key::Run(id, self.attempt))
            .or_else(|| self.value(&Key::Run(id, 0)));
        detail
            .map(|run| array(run, "jobs"))
            .unwrap_or_default()
            .iter()
            .filter(|job| State::of(job) == State::Failed)
            .map(|job| string(job, "name").to_owned())
            .collect()
    }

    /// What a run sheet's confirm does, the line under its title: the consequence,
    /// read from what the sheet now holds (the rerun's chosen jobs, the dispatch's ref).
    pub(in super::super) fn sheet_note(
        &self,
        action: &Action,
        fields: &[super::super::Field],
    ) -> Option<String> {
        let value = |index: usize| {
            fields
                .get(index)
                .map(|field| field.input.input.trim().to_owned())
                .unwrap_or_default()
        };
        match action {
            Action::Cancel(_) => Some("Running jobs stop. Finished jobs keep their logs.".into()),
            Action::Approve(_) => {
                Some("Its jobs start on this repository's runners, with its secrets.".into())
            }
            Action::Rerun(id) => {
                let run = self.run_value(*id)?;
                let sha: String = string(run, "headSha").chars().take(7).collect();
                let commit = if sha.is_empty() {
                    "the same commit".to_owned()
                } else {
                    format!("the same commit ({sha})")
                };
                let (_, all) = self.run_jobs(*id);
                let failed = self.failed_jobs(*id);
                let first = value(0);
                let mut note = if let Some(name) = first
                    .strip_prefix("This job (")
                    .and_then(|rest| rest.strip_suffix(')'))
                {
                    format!("Reruns {name} alone on {commit}; the other jobs keep their results.")
                } else if value(0).starts_with("Failed") {
                    let names = failed.join(", ");
                    let plural = if failed.len() == 1 { "" } else { "s" };
                    format!(
                        "Reruns {} failed job{plural} ({names}) on {commit}.",
                        failed.len()
                    )
                } else if all > 0 {
                    format!(
                        "Reruns all {all} job{} on {commit}.",
                        if all == 1 { "" } else { "s" }
                    )
                } else {
                    format!("Reruns every job on {commit}.")
                };
                let caps = self.capabilities();
                if !failed.is_empty()
                    && !caps.rerun_failed.allowed()
                    && let Some(reason) = caps.rerun_failed.reason()
                {
                    // The sheet is the how: only the reason (`Forgejo reruns whole runs only`).
                    let reason = reason.split(" · ").next().unwrap_or(reason);
                    note = format!("{note}\n{}.", reason.trim_end_matches('.'));
                }
                Some(note)
            }
            Action::Dispatch(id) => {
                let what = if self.terms().run_noun == "pipeline" {
                    "a pipeline".to_owned()
                } else {
                    self.dispatch_name(*id)
                };
                let reference = value(0);
                let on = if reference.is_empty() {
                    String::new()
                } else {
                    format!(" on {reference}")
                };
                Some(if fields.len() > 1 {
                    format!("Starts {what}{on} with these inputs.")
                } else {
                    format!("Starts {what}{on}.")
                })
            }
            _ => self.feature_note(action, fields),
        }
    }

    /// The workflow a dispatch sheet names: the listed one, else a run's (Forgejo lists none).
    pub(super) fn dispatch_name(&self, id: u64) -> String {
        if let Some(workflow) = self
            .workflows()
            .into_iter()
            .find(|workflow| number(workflow, "id") == id)
        {
            return string(workflow, "name").to_owned();
        }
        self.runs()
            .into_iter()
            .find(|run| number(run, "workflowDatabaseId") == id)
            .map(|run| self.run_workflow(run))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.terms().workflow_noun.into())
    }

    /// The finished job under the run pane's cursor, with its name: GitHub's per-job re-run.
    pub(super) fn cursor_job(&self, id: u64) -> Option<(u64, String)> {
        if self.page != 3 || !self.actions.right || self.run_id != Some(id) {
            return None;
        }
        let rows = self.tree(KEYS);
        let job = job_of(&rows.get(self.cursor_index(&rows))?.node)?;
        let detail = self
            .value(&Key::Run(id, self.attempt))
            .or_else(|| self.value(&Key::Run(id, 0)))?;
        let found = array(detail, "jobs")
            .iter()
            .find(|entry| number(entry, "databaseId") == job)?;
        State::of(found)
            .finished()
            .then(|| (job, string(found, "name").to_owned()))
    }

    pub(super) fn run_jobs(&self, id: u64) -> (usize, usize) {
        let detail = self
            .value(&Key::Run(id, self.attempt))
            .or_else(|| self.value(&Key::Run(id, 0)));
        let jobs = detail.map(|run| array(run, "jobs")).unwrap_or_default();
        (
            jobs.iter()
                .filter(|job| State::of(job) == State::Failed)
                .count(),
            jobs.len(),
        )
    }

    pub(super) fn run_finished(&self, run: &Value) -> bool {
        string(run, "status") == "completed"
    }

    /// The workflow `d` dispatches: the filter's, else the selected run's.
    pub(super) fn dispatch_workflow(&self) -> Option<u64> {
        let chosen = self.workflow_id();
        if chosen != 0 {
            return Some(chosen);
        }
        let run = self.run_value(self.run_id?)?;
        Some(number(run, "workflowDatabaseId")).filter(|id| *id != 0)
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
    pub(super) fn active(&self) -> bool {
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
        return (String::new(), GitRole::Muted);
    };
    let left = at - records::now();
    if left <= 0 {
        ("expired".into(), GitRole::Removed)
    } else if left < 86_400 {
        (format!("expires in {}h", left / 3600), GitRole::Modified)
    } else {
        (format!("expires in {} days", left / 86_400), GitRole::Muted)
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

/// A row of the Workflows panel (GitLab: Sources).
#[derive(Clone, Debug, Eq, PartialEq)]
enum Flow {
    All,
    /// `workflows()[n]`.
    Listed(usize),
    /// A workflow only the runs name (a Forgejo without a workflow list).
    Named(String),
}

/// One list row: `left` from the start, `right` flush with the end, an optional second row.
struct Item {
    left: Vec<Span>,
    right: Vec<Span>,
    more: Option<Vec<Span>>,
}

impl Item {
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
            chrome::fill(
                grid,
                Rect::new(inner.x, y, inner.w, rows),
                st(GitRole::Text, surface),
            );
            // The accent edge, as in every other list (r12 IN-07 / gitea-03): no chevron
            // stacked on the run's state mark.
            chrome::selection_edge(grid, inner.x, y..y + rows, focused, surface, mode);
            cursor_y = Some(y);
        }
        let right_width = chrome::spans_width(&item.right);
        let end = inner
            .right()
            .saturating_sub(1 + right_width + u16::from(right_width > 0));
        chrome::put_spans(
            grid,
            inner.x + 1,
            y,
            &spans_on(
                &clip(item.left.clone(), end.saturating_sub(inner.x + 1)),
                surface,
            ),
            end.max(inner.x + 1),
        );
        if right_width > 0 && inner.x + 2 + right_width < inner.right() {
            chrome::put_spans(
                grid,
                inner.right() - 1 - right_width,
                y,
                &spans_on(&item.right, surface),
                inner.right(),
            );
        }
        if let Some(more) = &item.more
            && rows > 1
        {
            let room = inner.right().saturating_sub(inner.x + 2);
            chrome::put_spans(
                grid,
                inner.x + 1,
                y + 1,
                &spans_on(&clip(more.clone(), room), surface),
                inner.right() - 1,
            );
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

pub(in crate::git::screen) fn frame(app: &mut GitScreen, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    if layout::mode(width, height) == Mode::TooSmall {
        let text = format!(
            "Dusty Git needs {}×{}",
            layout::MIN_WIDTH,
            layout::MIN_HEIGHT
        );
        frame.grid.put_text_clipped(
            width.saturating_sub(chrome::width(&text)) / 2,
            height / 2,
            &text,
            bold(GitRole::Modified, GitSurface::Canvas),
            width,
        );
        return frame;
    }
    chrome::fill(
        &mut frame.grid,
        Rect::new(0, 0, width, height),
        st(GitRole::Text, GitSurface::Canvas),
    );
    super::super::shell::top_bar(app, &mut frame.grid, width);
    app.github.sync_actions();
    let mode = app.icon_mode;
    let footer_rect = Rect::new(0, height - 1, width, 1);
    let body = Rect::new(0, 1, width, height - 2);
    if app.github.repo.is_empty() || !app.github.cache.contains_key(&Key::Repository) {
        chrome::pane(
            &mut frame.grid,
            body,
            &[(
                app.github.terms().actions_tab.to_owned(),
                bold(GitRole::Text, PANE),
            )],
            "",
            true,
        );
        let (text, role) = match app.github.cache.get(&Key::Repository) {
            Some(Err(error)) => (calm(&app.github, error), GitRole::Removed),
            _ => ("Loading the repository…".to_owned(), GitRole::Muted),
        };
        frame
            .grid
            .put_text_clipped(2, 2, &text, st(role, PANE), width - 2);
    } else {
        page(&mut frame, &mut app.github, body, mode);
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

/// Workflows over their runs on the left, the run on the right; below 90 columns
/// the left column or the run; `z` gives the run the whole width, and `toggle_sidebar`
/// (`Space e`) hides the left column until it is toggled back.
fn page(frame: &mut Frame, ws: &mut Workspace, body: Rect, mode: ExplorerIconMode) {
    let narrow = body.w < SPLIT;
    ws.actions.narrow = narrow;
    if narrow && !ws.actions.open {
        ws.actions.right = false;
    }
    if narrow {
        if ws.actions.open && ws.run_id.is_some() {
            paint_run(frame, ws, body, true, mode);
        } else {
            paint_left(frame, ws, body, mode);
        }
        return;
    }
    if ws.sidebar_hidden() && ws.run_id.is_some() {
        // The left column is out of sight: the keys go to the run.
        ws.actions.right = true;
        ws.actions.workflows = false;
    }
    if ws.actions.zoom || ws.sidebar_hidden() {
        paint_run(frame, ws, body, true, mode);
        return;
    }
    // 40% from 120 columns (44–60, so run titles mostly fit; logs wrap in the run), else 40.
    let list_w = if body.w >= 120 {
        (body.w * 40 / 100).clamp(44, 60)
    } else {
        40.min(body.w / 2)
    };
    let right = ws.actions.right;
    paint_left(frame, ws, Rect::new(body.x, body.y, list_w, body.h), mode);
    paint_run(
        frame,
        ws,
        Rect::new(body.x + list_w, body.y, body.w - list_w, body.h),
        right,
        mode,
    );
}

/// The left column: Workflows on top (as tall as its rows, up to two fifths), the chosen
/// workflow's runs below.
fn paint_left(frame: &mut Frame, ws: &mut Workspace, rect: Rect, mode: ExplorerIconMode) {
    let rows = ws.flow_rows().len() as u16;
    let top = (rows + 2)
        .min(rect.h * 2 / 5)
        .max(4)
        .min(rect.h.saturating_sub(8))
        .max(3);
    let panel = ws.panel();
    paint_workflows(
        frame,
        ws,
        Rect::new(rect.x, rect.y, rect.w, top),
        panel == 0,
        mode,
    );
    paint_list(
        frame,
        ws,
        Rect::new(rect.x, rect.y + top, rect.w, rect.h - top),
        panel == 1,
        mode,
    );
}

/// `All workflows  42`, then each workflow with its latest run's state; one turned off is
/// muted with `(off)`.
fn paint_workflows(
    frame: &mut Frame,
    ws: &mut Workspace,
    rect: Rect,
    focused: bool,
    mode: ExplorerIconMode,
) {
    let title = format!("{} {}", glyph(Glyph::Workflow, mode), ws.flows_title());
    let chip = match ws.cache.get(&Key::Workflows) {
        Some(Err(_)) => "R retry".to_owned(),
        _ => String::new(),
    };
    chrome::pane(
        &mut frame.grid,
        rect,
        &[(title, bold(GitRole::Text, PANE))],
        &chip,
        focused,
    );
    let inner = rect.inner();
    if inner.h == 0 {
        return;
    }
    let rows = ws.flow_rows();
    let cursor = ws.flow_cursor(&rows);
    let blank = " ".repeat(usize::from(chrome::width(State::Passed.glyph(mode))) + 1);
    let items = rows
        .iter()
        .map(|flow| {
            let name = ws.flow_name(flow);
            if *flow == Flow::All {
                let count = ws.all_runs().len();
                return Item {
                    left: vec![
                        (blank.clone(), st(GitRole::Muted, PANE)),
                        (name, bold(GitRole::Text, PANE)),
                    ],
                    right: if count == 0 {
                        Vec::new()
                    } else {
                        vec![(count.to_string(), st(GitRole::Muted, PANE))]
                    },
                    more: None,
                };
            }
            let off = ws.flow_off(flow);
            let mark = match ws.flow_state(flow) {
                Some(state) if !off => (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
                _ if off => (
                    format!("{} ", State::Skipped.glyph(mode)),
                    st(GitRole::Muted, PANE),
                ),
                // Never run: a dim ring keeps the column whole.
                _ => (format!("◌{}", &blank[1..]), st(GitRole::Muted, PANE)),
            };
            let mut left = vec![
                mark,
                (
                    name,
                    if off {
                        st(GitRole::Muted, PANE)
                    } else {
                        st(GitRole::Text, PANE)
                    },
                ),
            ];
            if off {
                left.push((" (off)".into(), st(GitRole::Muted, PANE)));
            }
            Item {
                left,
                right: Vec::new(),
                more: None,
            }
        })
        .collect::<Vec<_>>();
    if let Some(row) = paint_items(&mut frame.grid, inner, &items, Some(cursor), focused, mode)
        && focused
        && !ws.actions.picker
    {
        frame.cursor = Some((inner.x, row, SetCursorStyle::SteadyBar));
    }
}

fn paint_list(
    frame: &mut Frame,
    ws: &mut Workspace,
    rect: Rect,
    focused: bool,
    mode: ExplorerIconMode,
) {
    let noun = ws.actions_noun();
    // `Runs · Quality checks` once a workflow is chosen; every one's are `Workflow runs`.
    let chosen = ws.chosen_flow();
    let title = match (chosen, noun == "pipeline") {
        (Some(name), true) => format!("{} Pipelines · {name}", glyph(Glyph::Workflow, mode)),
        (Some(name), false) => format!("{} Runs · {name}", glyph(Glyph::Workflow, mode)),
        (None, true) => format!("{} Pipelines", glyph(Glyph::Workflow, mode)),
        (None, false) => format!("{} Workflow runs", glyph(Glyph::Workflow, mode)),
    };
    let title = chrome::ellipsize(&title, usize::from(rect.w.saturating_sub(6)));
    let key = Key::Runs(ws.workflow_id());
    let room = rect.w.saturating_sub(chrome::width(&title) + 8);
    // One string at every width, or none (`updated just now`, never `now` when narrow).
    let chip = ws
        .freshness_chip(&key, false)
        .filter(|chip| chrome::width(chip) <= room)
        .unwrap_or_default();
    chrome::pane(
        &mut frame.grid,
        rect,
        &[(title, bold(GitRole::Text, PANE))],
        &chip,
        focused,
    );
    let inner = rect.inner();
    if inner.h < 3 {
        return;
    }
    let mut y = inner.y;
    let tabs = ws.section_tabs();
    let section = ws.actions.section.min(tabs.len() - 1);
    // Only the state filters: `All` is the source selected above, said once
    // with its count there. Section 0 (every run) is no chip; 1.. are `Failing`, `Running`,
    // `Mine`. Too wide, the counts go before the padding does, on every tab at once.
    let tabs = tabs[1..].to_vec();
    let active = section.checked_sub(1).unwrap_or(usize::MAX);
    let fitted = chrome::fit_tabs(&tabs, active.min(tabs.len() - 1), inner.w);
    chrome::sub_tabs(
        &mut frame.grid,
        inner.x,
        y,
        inner.right(),
        &fitted,
        active,
        focused,
    );
    y += 1;
    let words = ws.filter_words();
    let filter = &ws.actions.filter;
    if filter.active() || !words.is_empty() {
        let mut spans: Vec<Span> = Vec::new();
        if filter.active() {
            spans.push((
                format!("{} ", glyph(Glyph::Search, mode)),
                st(GitRole::Modified, PANE),
            ));
            spans.push((filter.query.clone(), bold(GitRole::Modified, PANE)));
            if filter.editing {
                spans.push(("▏".into(), st(GitRole::Modified, PANE)));
            }
        }
        for word in &words {
            if !spans.is_empty() {
                spans.push((" · ".into(), st(GitRole::Muted, PANE)));
            }
            spans.push((word.clone(), st(GitRole::Ref, PANE)));
        }
        spans.push(("  Esc clears".into(), st(GitRole::Muted, PANE)));
        chrome::put_spans(
            &mut frame.grid,
            inner.x + 1,
            y,
            &clip(spans, inner.w.saturating_sub(2)),
            inner.right() - 1,
        );
        if filter.editing && focused {
            frame.cursor = Some((
                inner.x + 3 + chrome::width(&filter.query),
                y,
                SetCursorStyle::SteadyBar,
            ));
        }
        y += 1;
    }
    for x in inner.x..inner.right() {
        frame.grid.set_cell(x, y, "─", st(GitRole::Border, PANE));
    }
    y += 1;
    let list = Rect::new(inner.x, y, inner.w, inner.bottom().saturating_sub(y));
    let runs = ws.shown_runs();
    if runs.is_empty() {
        let all = ws.runs().len();
        let (text, role) = match ws.cache.get(&key) {
            Some(Err(error)) => (calm(ws, error), GitRole::Removed),
            None => match ws.blocked(&key) {
                Some((headline, hint)) => (format!("{headline} · {hint}"), GitRole::Muted),
                None => ("Loading…".into(), GitRole::Muted),
            },
            Some(Ok(_)) if all == 0 => (format!("No {noun}s yet"), GitRole::Muted),
            Some(Ok(_)) => match section {
                1 => ("Nothing is failing".into(), GitRole::Added),
                2 => ("Nothing is running".into(), GitRole::Muted),
                3 => (format!("None of these {noun}s are yours"), GitRole::Muted),
                _ => (format!("No {noun}s match these filters"), GitRole::Muted),
            },
        };
        frame
            .grid
            .put_text_clipped(list.x + 1, list.y, &text, st(role, PANE), list.right() - 1);
        if focused && frame.cursor.is_none() {
            frame.cursor = Some((list.x + 1, list.y, SetCursorStyle::SteadyBlock));
        }
    } else {
        // One reserved, right-aligned time column for every row: titles
        // clip at a single column whether a row has `57m 25s`, `waiting` or nothing.
        let times = runs.iter().map(|run| ws.run_time(run)).collect::<Vec<_>>();
        let column = times
            .iter()
            .map(|(time, _)| chrome::width(time))
            .max()
            .unwrap_or(0);
        let items = runs
            .iter()
            .zip(times)
            .map(|(run, (time, role))| {
                let state = State::of(run);
                let title = ws.run_title(run);
                let meta = ws.run_meta(run, &title, usize::from(list.w.saturating_sub(4)));
                let pad = usize::from(column).saturating_sub(usize::from(chrome::width(&time)));
                let mark = format!("{} ", state.glyph(mode));
                let room = list
                    .w
                    .saturating_sub(2 + column + u16::from(column > 0) + chrome::width(&mark));
                let title = fit_title(&title, usize::from(room));
                Item {
                    left: vec![
                        (mark, st(state.role(), PANE)),
                        (title, bold(GitRole::Text, PANE)),
                    ],
                    right: if column == 0 {
                        Vec::new()
                    } else {
                        vec![(format!("{}{time}", " ".repeat(pad)), st(role, PANE))]
                    },
                    more: Some(vec![(format!("  {meta}"), st(GitRole::Muted, PANE))]),
                }
            })
            .collect::<Vec<_>>();
        let cursor = ws.run_cursor(&runs);
        if let Some(row) = paint_items(&mut frame.grid, list, &items, Some(cursor), focused, mode)
            && focused
            && !ws.actions.filter.editing
            && !ws.actions.picker
        {
            frame.cursor = Some((list.x, row, SetCursorStyle::SteadyBar));
        }
    }
    if ws.actions.picker {
        paint_picker(frame, ws, rect, mode);
    }
}

/// `F`: the workflow, branch and event filters as one popup over the list.
fn paint_picker(frame: &mut Frame, ws: &Workspace, rect: Rect, mode: ExplorerIconMode) {
    let rows = ws.rail_rows();
    let h = (rows.len() as u16 + 2).min(rect.h.saturating_sub(4)).max(3);
    // Over the list's whole width: no row's duration shows beside it.
    let w = rect.w.saturating_sub(4);
    let pane = Rect::new(rect.x + 2, rect.y + 3, w, h);
    // Its one-cell margin hides the list's cursor mark beside it (A3-07).
    for y in pane.y..pane.bottom() {
        for x in [rect.x + 1, pane.right()] {
            if x < rect.right().saturating_sub(1) {
                frame.grid.set_cell(x, y, " ", st(GitRole::Text, PANE));
            }
        }
    }
    // The footer says `Enter choose · Esc close`; the title does not repeat it cut (A2-04).
    chrome::pane(
        &mut frame.grid,
        pane,
        &[("Filter by".to_owned(), bold(GitRole::Text, PANE))],
        "",
        true,
    );
    let labels = [
        if ws.terms().run_noun == "pipeline" {
            "Source"
        } else {
            "Workflow"
        },
        "Branch",
        "Event",
    ];
    let mut last = usize::MAX;
    let items = rows
        .iter()
        .map(|(section, value)| {
            let label = if *section != last {
                labels[*section]
            } else {
                ""
            };
            last = *section;
            let text = value.clone().unwrap_or_else(|| "All".into());
            let chosen = ws.rail_chosen(*section, value);
            Item {
                left: vec![
                    (format!("{label:<9}"), st(GitRole::Muted, PANE)),
                    (
                        if chosen { "● " } else { "  " }.into(),
                        bold(GitRole::Focus, PANE),
                    ),
                    (
                        text,
                        st(
                            if *section == 1 && value.is_some() {
                                GitRole::Ref
                            } else {
                                GitRole::Text
                            },
                            PANE,
                        ),
                    ),
                ],
                right: Vec::new(),
                more: None,
            }
        })
        .collect::<Vec<_>>();
    let inner = pane.inner();
    let cursor = ws.actions.rail.min(rows.len().saturating_sub(1));
    if let Some(row) = paint_items(&mut frame.grid, inner, &items, Some(cursor), true, mode) {
        frame.cursor = Some((inner.x, row, SetCursorStyle::SteadyBar));
    }
}

/// The selected run: a two-line header, then one tree of jobs, steps and inline logs.
fn paint_run(
    frame: &mut Frame,
    ws: &mut Workspace,
    rect: Rect,
    focused: bool,
    mode: ExplorerIconMode,
) {
    let title = vec![(
        format!("{} {}", glyph(Glyph::Workflow, mode), ws.run_word()),
        bold(GitRole::Text, PANE),
    )];
    // Opened from a pull request's check, the title says where Esc goes (R9-05).
    let chip = if ws.actions.zoom {
        "z restores the list".to_owned()
    } else {
        ws.return_crumb().unwrap_or_default()
    };
    chrome::pane(&mut frame.grid, rect, &title, &chip, focused);
    let inner = rect.inner();
    if inner.h < 3 || inner.w < 8 {
        return;
    }
    let x = inner.x + 1;
    let right = inner.right().saturating_sub(1);
    let mut y = inner.y;
    let Some((id, run)) = ws
        .run_id
        .and_then(|id| ws.run_value(id).map(|run| (id, run.clone())))
    else {
        let (text, role) = match ws.cache.get(&Key::Runs(ws.workflow_id())) {
            None => match ws.blocked(&Key::Runs(ws.workflow_id())) {
                Some((headline, hint)) => (format!("{headline} · {hint}"), GitRole::Muted),
                None => ("Loading…".to_owned(), GitRole::Muted),
            },
            Some(Err(error)) => (calm(ws, error), GitRole::Muted),
            // Why `d` would refuse stays on screen here, not only in a toast that fades.
            Some(Ok(_)) => match ws.dispatch_refusal() {
                None => (
                    format!("No {} to show · d starts one", ws.actions_noun()),
                    GitRole::Muted,
                ),
                Some(reason) => (
                    format!("No {} to show · {reason}", ws.actions_noun()),
                    GitRole::Muted,
                ),
            },
        };
        frame
            .grid
            .put_text_clipped(x, y, &text, st(role, PANE), right);
        return;
    };
    // Header: `✕ <title>`, then the run's facts in one dim line.
    let state = State::of(&run);
    let name = ws.run_title(&run);
    chrome::put_spans(
        &mut frame.grid,
        x,
        y,
        &clip(
            vec![
                (format!("{} ", state.glyph(mode)), bold(state.role(), PANE)),
                (name, bold(GitRole::Text, PANE)),
            ],
            right.saturating_sub(x),
        ),
        right,
    );
    y += 1;
    // `Run #7 · #18 · fix/ci-regression · 9h ago · 7s · maya`: the number links the pull
    // request; the actor and the workflow come last, the first to be cut.
    let mut facts = Vec::<String>::new();
    facts.push(format!("{} #{}", ws.run_word(), run_number(&run)));
    if ws.capabilities().attempts.allowed() {
        let latest = ws.latest_attempt();
        let attempt = ws.effective_attempt().max(number(&run, "attempt")).max(1);
        if latest > 1 {
            facts.push(format!("attempt {attempt} of {latest}"));
        }
    }
    // The list row's order: the pull request or the branch, then the event.
    let event = string(&run, "event");
    let pull = ws.pull_ref(&run);
    let pulled = pull.is_some();
    if let Some(pull) = pull {
        facts.push(pull);
    }
    let branch = string(&run, "headBranch");
    // GitLab's merge-request pipelines run on `refs/merge-requests/18/head`: the number
    // before it already says which one.
    if !branch.is_empty() && !branch.starts_with("refs/") {
        facts.push(branch.to_owned());
    }
    if !pulled && !event.is_empty() && !pull_event(event) {
        facts.push(ws.event_word(event));
    }
    let listed = ws.listed_run(id);
    let field = |key: &str| {
        Some(string(&run, key))
            .filter(|text| !text.is_empty())
            .or_else(|| {
                listed
                    .map(|listed| string(listed, key))
                    .filter(|text| !text.is_empty())
            })
            .map(str::to_owned)
    };
    // A running run reads `running 2m` as its list row does, not `2m ago · running`; a
    // cancelled one says so (its glyph is a skipped run's too).
    let created = field("createdAt");
    match state {
        State::Running => facts.push(
            created
                .as_deref()
                .and_then(elapsed)
                .map_or_else(|| "running".into(), |time| format!("running {time}")),
        ),
        _ => {
            if state == State::Cancelled {
                facts.insert(1, "Cancelled".into());
            }
            if let Some(created) = &created {
                facts.push(records::ago(created, records::now()));
            }
            let (time, _) = ws.run_time(&run);
            if !time.is_empty() {
                facts.push(time);
            }
        }
    }
    let actor = [Some(&run), listed]
        .into_iter()
        .flatten()
        .map(|run| string(&run["dusty"], "actor"))
        .find(|actor| !actor.is_empty());
    if let Some(actor) = actor.map(str::to_owned) {
        facts.push(actor);
    }
    let workflow = ws.named_workflow(&run);
    let workflow = workflow.as_str();
    // GitLab's source names the event already (`Merge request`, `Push`).
    // Nor when the runs list beside it is already titled `Runs · Quality checks` (r16
    // notif-actions-11): the line keeps who started it instead.
    if !workflow.is_empty()
        && workflow != string(&run, "displayTitle")
        && !workflow.eq_ignore_ascii_case(&ws.event_word(event))
        && ws.chosen_flow().as_deref() != Some(workflow)
    {
        // Labelled, so the last token is not a bare unexplained word.
        facts.push(if ws.terms().run_noun == "pipeline" {
            workflow.to_owned()
        } else {
            format!("workflow {workflow}")
        });
    }
    // Short of room whole facts go from the end (the workflow, then the actor…): never a
    // word cut in half or a dangling `·…`.
    let room = usize::from(right.saturating_sub(x));
    while facts.len() > 1 && usize::from(chrome::width(&facts.join(" · "))) > room {
        facts.pop();
    }
    frame.grid.put_text_clipped(
        x,
        y,
        &chrome::ellipsize(&facts.join(" · "), room),
        st(GitRole::Muted, PANE),
        right,
    );
    y += 1;
    // Notes: approval, cancellation, the last download, the find row.
    if run["dusty"]["needsApproval"].as_bool() == Some(true) {
        chrome::put_spans(
            &mut frame.grid,
            x,
            y,
            &[
                (
                    format!(
                        "{} Waiting for approval · ",
                        glyph(Glyph::CheckPending, mode)
                    ),
                    st(GitRole::Modified, PANE),
                ),
                ("A".into(), bold(GitRole::Focus, PANE)),
                (
                    format!(" {}", ws.terms().approve_verb.to_lowercase()),
                    st(GitRole::Muted, PANE),
                ),
            ],
            right,
        );
        y += 1;
    }
    if ws.actions.cancelling.contains(&id) && !state.finished() {
        frame.grid.put_text_clipped(
            x,
            y,
            "Cancellation requested",
            bold(GitRole::Modified, PANE),
            right,
        );
        y += 1;
    }
    if let Some((_, receipt)) = ws.actions.receipt.as_ref().filter(|(run, _)| *run == id) {
        chrome::receipt(
            &mut frame.grid,
            Rect::new(inner.x, y, inner.w, 1),
            chrome::ReceiptTone::Ok,
            receipt,
            mode,
        );
        y += 1;
    }
    let find_row = ws.actions.find.active().then_some(y);
    if find_row.is_some() {
        y += 1;
    }
    for column in inner.x..inner.right() {
        frame
            .grid
            .set_cell(column, y, "─", st(GitRole::Border, PANE));
    }
    y += 1;
    let area = Rect::new(inner.x, y, inner.w, inner.bottom().saturating_sub(y));
    if area.h == 0 {
        return;
    }
    if ws.actions.attempts {
        paint_attempts(frame, ws, area, mode);
        return;
    }
    let message = match ws.cache.get(&Key::Run(id, ws.attempt)) {
        Some(Err(error)) => Some((calm(ws, error), GitRole::Removed)),
        None => Some(("Loading jobs…".to_owned(), GitRole::Muted)),
        Some(Ok(_)) => None,
    };
    let rows = if message.is_some() {
        Vec::new()
    } else {
        ws.tree(mode)
    };
    if let Some((text, role)) = message.or_else(|| {
        rows.is_empty()
            .then(|| (ws.no_jobs().to_owned(), GitRole::Muted))
    }) {
        frame
            .grid
            .put_text_clipped(x, area.y, &text, st(role, PANE), right);
        // A run where nothing ran left nothing: it reads as a cancelled run does (A3-02).
        let finished = ws
            .run_value(id)
            .is_some_and(|run| State::of(run).finished());
        if rows.is_empty()
            && finished
            && area.h > 1
            && ws.capabilities().artifacts_list.reason().is_none()
        {
            let line = format!("{} No artifacts", glyph(Glyph::File, mode));
            frame
                .grid
                .put_text_clipped(x, area.y + 1, &line, st(GitRole::Muted, PANE), right);
        }
        return;
    }
    let found = paint_tree(frame, ws, area, &rows, focused, mode);
    if let Some(row) = find_row {
        let find = &ws.actions.find;
        let text = format!(
            "{} {}{}",
            glyph(Glyph::Search, mode),
            find.query,
            if find.editing { "▏" } else { "" }
        );
        frame
            .grid
            .put_text_clipped(x, row, &text, bold(GitRole::Modified, PANE), right);
        let count = if find.query.is_empty() {
            String::new()
        } else if found == 0 {
            "no matches".into()
        } else {
            format!(
                "{}/{found} matches",
                ws.actions.find_current.min(found - 1) + 1
            )
        };
        frame.grid.put_text_clipped(
            right.saturating_sub(chrome::width(&count)),
            row,
            &count,
            st(
                if found == 0 && !find.query.is_empty() {
                    GitRole::Removed
                } else {
                    GitRole::Modified
                },
                PANE,
            ),
            right,
        );
        if find.editing && focused {
            frame.cursor = Some((
                x + 2 + chrome::width(&find.query),
                row,
                SetCursorStyle::SteadyBar,
            ));
        }
    }
}

fn paint_attempts(frame: &mut Frame, ws: &Workspace, area: Rect, mode: ExplorerIconMode) {
    let latest = ws.latest_attempt();
    let id = ws.run_id.unwrap_or(0);
    frame.grid.put_text_clipped(
        area.x + 1,
        area.y,
        "Attempts · Enter shows one · Esc closes",
        st(GitRole::Muted, PANE),
        area.right() - 1,
    );
    let items = (1..=latest)
        .rev()
        .map(|attempt| {
            let value = if attempt == latest {
                ws.value(&Key::Run(id, 0))
            } else {
                ws.value(&Key::Run(id, attempt as u32))
            };
            let mut left = vec![(format!("Attempt {attempt}"), bold(GitRole::Text, PANE))];
            let mut right = Vec::new();
            if let Some(run) = value {
                let state = State::of(run);
                left.insert(
                    0,
                    (format!("{} ", state.glyph(mode)), st(state.role(), PANE)),
                );
                if let Some(seconds) = ws.run_seconds(run) {
                    right.push((duration(seconds), st(GitRole::Muted, PANE)));
                }
            } else {
                left.insert(0, ("  ".into(), st(GitRole::Muted, PANE)));
            }
            Item {
                left,
                right,
                more: None,
            }
        })
        .collect::<Vec<_>>();
    let rows = Rect::new(area.x, area.y + 1, area.w, area.h.saturating_sub(1));
    if let Some(row) = paint_items(
        &mut frame.grid,
        rows,
        &items,
        Some(ws.actions.attempt_cursor),
        true,
        mode,
    ) {
        frame.cursor = Some((rows.x, row, SetCursorStyle::SteadyBar));
    }
}

/// The tree with its cursor, scroll, pan and find marks; returns the find's match count.
#[allow(clippy::single_range_in_vec_init)] // a row that does not wrap is one range
fn paint_tree(
    frame: &mut Frame,
    ws: &mut Workspace,
    area: Rect,
    rows: &[Row],
    focused: bool,
    mode: ExplorerIconMode,
) -> usize {
    let cursor = ws.cursor_index(rows);
    let right = area.right().saturating_sub(1);
    let x0 = area.x + 1;
    let digits = rows
        .iter()
        .filter_map(|row| row.log.as_ref().map(|(number, _)| number.to_string().len()))
        .max()
        .unwrap_or(2)
        .max(2) as u16;
    let indent = |depth: u16| x0 + depth * 2;
    let text_x = |row: &Row| indent(row.depth) + digits + 1;
    let room = |row: &Row| usize::from(right.saturating_sub(text_x(row)));
    ws.detail_width = rows
        .iter()
        .find(|row| row.log.is_some())
        .map_or(usize::from(area.w), room);
    let wrap = !ws.actions.nowrap;
    let whole = !ws.actions.find.active();
    let finding = if whole {
        String::new()
    } else {
        ws.actions.find.query.clone()
    };
    let chunks = |row: &Row| -> Vec<Range<usize>> {
        let Some((_, line)) = &row.log else {
            return vec![0..0];
        };
        line.parts(room(row), wrap, whole, &finding)
    };
    let heights = rows.iter().map(|row| chunks(row).len()).collect::<Vec<_>>();
    let total: usize = heights.iter().sum();
    let height = usize::from(area.h);
    let top: usize = heights[..cursor].iter().sum();
    let mut scroll = ws.actions.scroll;
    if ws.actions.auto {
        // The first error with its job and steps above it when they fit; else with its
        // step's header above it; else a third down.
        let job = rows[..=cursor]
            .iter()
            .rposition(|row| matches!(row.node, Node::Job(_)))
            .unwrap_or(0);
        let job_top: usize = heights[..job].iter().sum();
        let step = rows[..=cursor]
            .iter()
            .rposition(|row| matches!(row.node, Node::Step(..)))
            .unwrap_or(job);
        let step_top: usize = heights[..step].iter().sum();
        let fits = |from: usize| top + heights[cursor] <= from + height * 3 / 4;
        scroll = if fits(job_top) {
            job_top
        } else if fits(step_top) {
            step_top.saturating_sub(1).max(job_top)
        } else {
            top.saturating_sub(height / 3)
        };
    } else if top < scroll {
        scroll = top;
    } else {
        // An open node brings its first children into view with it (up to half the pane).
        let mut span = heights[cursor];
        if rows[cursor].fold == Some(true) {
            for (row, rows_high) in rows[cursor + 1..].iter().zip(&heights[cursor + 1..]) {
                if row.depth <= rows[cursor].depth
                    || span + rows_high > (height / 2).max(heights[cursor])
                {
                    break;
                }
                span += rows_high;
            }
        }
        // Artifacts are the tree's last node: in it, every archive comes into view when the
        // rest of the tree fits (A3-01).
        if matches!(
            rows[cursor].node,
            Node::Artifacts | Node::Artifact(_) | Node::File(..)
        ) && total - top <= height
        {
            span = span.max(total - top);
        }
        if top + span > scroll + height {
            scroll = (top + span).saturating_sub(height).min(top);
        }
    }
    scroll = scroll.min(total.saturating_sub(height));
    ws.actions.scroll = scroll;
    ws.actions.rows = height;
    // Find matches over the open logs.
    let query = ws.actions.find.query.clone();
    let found = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.log.as_ref().map(|(_, line)| (index, line)))
        .flat_map(|(index, line)| {
            matches(&line.text, &query)
                .into_iter()
                .map(move |range| (index, range))
        })
        .collect::<Vec<_>>();
    let current =
        (!found.is_empty()).then(|| found[ws.actions.find_current.min(found.len() - 1)].clone());
    // The current match stays in view at this frame's width (the pan was set at another).
    if !wrap
        && ws.actions.follow_match
        && let Some((index, range)) = current.as_ref().filter(|(index, _)| *index == cursor)
        && let Some((_, line)) = &rows[*index].log
    {
        let start = line.text[..range.start].width();
        let end = start + line.text[range.clone()].width();
        let room = room(&rows[*index]);
        if start < ws.actions.pan || end > ws.actions.pan + room {
            ws.actions.pan = start.saturating_sub(room / 3) / PAN * PAN;
        }
    }
    let pan = if wrap { 0 } else { ws.actions.pan };
    // A wrapped line's cursor sits on the part that holds the current match.
    let cursor_part = current
        .as_ref()
        .filter(|(line, _)| wrap && *line == cursor)
        .and_then(|(_, range)| {
            chunks(&rows[cursor])
                .iter()
                .position(|part| part.contains(&range.start))
        })
        .unwrap_or(0);
    let selection = ws.visual_range(rows, cursor);
    let mut visual = 0usize;
    for (index, row) in rows.iter().enumerate() {
        let parts = chunks(row);
        if visual + parts.len() <= scroll {
            visual += parts.len();
            continue;
        }
        let selected = index == cursor || selection.as_ref().is_some_and(|r| r.contains(&index));
        let surface = if selected {
            if focused {
                GitSurface::Cursor
            } else {
                GitSurface::CursorInactive
            }
        } else {
            PANE
        };
        for (part, range) in parts.into_iter().enumerate() {
            if visual < scroll {
                visual += 1;
                continue;
            }
            let y = area.y + (visual - scroll) as u16;
            visual += 1;
            if y >= area.bottom() {
                break;
            }
            if selected {
                chrome::fill(
                    &mut frame.grid,
                    Rect::new(area.x, y, area.w, 1),
                    st(GitRole::Text, surface),
                );
                if index == cursor && focused && part == cursor_part && !ws.actions.find.editing {
                    frame.cursor = Some((area.x, y, SetCursorStyle::SteadyBlock));
                }
            }
            let Some((number, line)) = &row.log else {
                // A job, step, artifact or note row.
                let mut at = indent(row.depth);
                if let Some(open) = row.fold {
                    at = frame.grid.put_text_clipped(
                        at,
                        y,
                        &format!(
                            "{} ",
                            glyph(
                                if open {
                                    Glyph::FoldOpen
                                } else {
                                    Glyph::FoldClosed
                                },
                                mode
                            )
                        ),
                        st(GitRole::Muted, surface),
                        right,
                    );
                } else if !matches!(row.node, Node::Note(..) | Node::File(..) | Node::Artifacts) {
                    // `No artifacts` has no fold mark and sits in its column, at the run's
                    // level, not under the last job.
                    at += 2;
                }
                let right_w = chrome::spans_width(&row.right);
                // A job's or step's time sits at most STEP_ROW columns in, by its name.
                let right = right.min(area.x + check_job::STEP_ROW as u16);
                let end = right.saturating_sub(right_w + u16::from(right_w > 0));
                chrome::put_spans(
                    &mut frame.grid,
                    at,
                    y,
                    &spans_on(&clip(row.left.clone(), end.saturating_sub(at)), surface),
                    end,
                );
                if right_w > 0 && at + right_w < right {
                    chrome::put_spans(
                        &mut frame.grid,
                        right - right_w,
                        y,
                        &spans_on(&row.right, surface),
                        right,
                    );
                }
                continue;
            };
            let x_text = text_x(row);
            if part == 0 {
                let label = format!("{number:>w$}", w = usize::from(digits));
                frame.grid.put_text_clipped(
                    indent(row.depth),
                    y,
                    &label,
                    st(
                        if selected {
                            GitRole::Focus
                        } else {
                            GitRole::Muted
                        },
                        surface,
                    ),
                    right,
                );
            }
            let line_matches = found
                .iter()
                .filter(|(at, _)| *at == index)
                .map(|(_, range)| range.clone())
                .collect::<Vec<_>>();
            let style_at = |at: usize| -> Style {
                if let Some((_, current_range)) =
                    current.as_ref().filter(|(line, _)| *line == index)
                    && current_range.contains(&at)
                {
                    return bold(GitRole::OnFocus, GitSurface::ChipActive);
                }
                if line_matches.iter().any(|range| range.contains(&at)) {
                    return Style::Search;
                }
                let mut role = match line.tone {
                    Tone::Error => GitRole::Removed,
                    Tone::Pass => GitRole::Added,
                    Tone::Quiet => GitRole::Muted,
                    _ => GitRole::Text,
                };
                let mut weight = matches!(line.tone, Tone::Command | Tone::Heading | Tone::Error);
                if let Some((_, sgr)) = line.colours.iter().find(|(range, _)| range.contains(&at)) {
                    if let Some(color) = sgr.color {
                        role = sgr_role(color);
                    }
                    weight |= sgr.bold;
                }
                if line
                    .link
                    .as_ref()
                    .is_some_and(|(range, _, _)| range.contains(&at))
                {
                    role = GitRole::Link;
                }
                if weight {
                    bold(role, surface)
                } else {
                    st(role, surface)
                }
            };
            let mut column = 0usize;
            let mut x = x_text;
            let mut clipped = false;
            // A test line drawn from inside its id (R9-07) says so.
            if part == 0
                && range.start > 0
                && !line.text[..range.start]
                    .chars()
                    .all(|c| "=-─═━_~*#".contains(c))
            {
                frame
                    .grid
                    .put_text_clipped(x, y, "…", st(GitRole::Muted, surface), right);
                x += 1;
            }
            for (at, grapheme) in line.text[range.clone()].grapheme_indices(true) {
                let at = at + range.start;
                let w = grapheme.width();
                if column < pan {
                    column += w;
                    continue;
                }
                if x + w as u16 > right {
                    clipped = true;
                    break;
                }
                if w > 0 {
                    frame
                        .grid
                        .put_text_clipped(x, y, grapheme, style_at(at), right);
                }
                x += w as u16;
                column += w;
            }
            if pan > 0 && line.text.width() > 0 {
                frame.grid.set_cell(
                    x_text - 1,
                    y,
                    glyph(Glyph::ClipLeft, mode),
                    st(GitRole::Muted, surface),
                );
            }
            // A ruler (`====`, `----`) is cut at the edge without a mark: nothing is lost.
            if clipped && !ruler(&line.text) {
                frame.grid.set_cell(
                    right - 1,
                    y,
                    glyph(Glyph::ClipRight, mode),
                    st(GitRole::Muted, surface),
                );
            }
            if index == cursor && part == cursor_part && focused && !ws.actions.find.editing {
                frame.cursor = Some((x_text, y, SetCursorStyle::SteadyBlock));
            }
        }
        if visual >= scroll + height {
            break;
        }
    }
    // What is below the pane's edge is counted on its bottom border (A3-01).
    let mut start = 0usize;
    let below = heights
        .iter()
        .filter(|rows_high| {
            let hidden = start >= scroll + height;
            start += **rows_high;
            hidden
        })
        .count();
    if below > 0 && area.w > 16 {
        let cue = format!(" {} {below} more ", glyph(Glyph::FoldOpen, mode));
        let cue_x = area.right().saturating_sub(chrome::width(&cue) + 1);
        frame.grid.put_text_clipped(
            cue_x,
            area.bottom(),
            &cue,
            st(GitRole::Muted, PANE),
            area.right(),
        );
    }
    found.len()
}

// ------------------------------------------------------------------------------- footer

fn footer(app: &GitScreen, frame: &mut Frame, rect: Rect) {
    let ws = &app.github;
    if let Some(form) = &app.form {
        let several = form.fields.len() > 1;
        let multiline = form.fields.iter().any(|field| field.multiline);
        let verb = match &form.action {
            FormAction::Github(Action::Rerun(_)) => "re-run jobs",
            FormAction::Github(Action::Cancel(_)) if ws.terms().run_noun == "pipeline" => {
                "cancel pipeline"
            }
            FormAction::Github(Action::Cancel(_)) => "cancel run",
            FormAction::Github(Action::Dispatch(_)) => "run workflow",
            FormAction::Github(Action::Approve(_)) => "approve",
            FormAction::Github(Action::Download(..)) => "download",
            FormAction::Github(Action::SaveLog(_)) => "save",
            _ => "confirm",
        };
        // The safe button (`Keep running`, `Cancel`) holds focus: Enter closes the sheet.
        let safe = form.selected == form.fields.len() + 1;
        let keep = if matches!(&form.action, FormAction::Github(Action::Cancel(_))) {
            "keep running"
        } else {
            "close"
        };
        // Esc says what it does: on the Cancel sheet it keeps the run, elsewhere it cancels
        // the sheet, as every other sheet's Esc does.
        let esc = if matches!(&form.action, FormAction::Github(Action::Cancel(_))) {
            "keep running"
        } else {
            "cancel"
        };
        let mut hints = if safe {
            vec![("Enter", keep), ("Tab", "switch"), ("Esc", esc)]
        } else {
            vec![
                (if multiline { "Ctrl+Enter" } else { "Enter" }, verb),
                ("Esc", esc),
            ]
        };
        if several {
            hints.extend([("Tab", "next field"), ("Shift+Tab", "previous")]);
        }
        let message = (!app.notice.is_empty()).then_some((app.notice.as_str(), true));
        chrome::footer(&mut frame.grid, rect, &hints, "", message);
        return;
    }
    let message = if ws.writing() {
        Some(("Sending…", false))
    } else if !app.notice.is_empty() {
        Some((app.notice.as_str(), true))
    } else if !ws.notice.is_empty() {
        Some((ws.notice.as_str(), ws.notice_error))
    } else {
        None
    };
    let view = &ws.actions;
    let back: Option<String>;
    let keys = ws.key_table();
    let sidebar = keys.keys(Named::ToggleSidebar).unwrap_or_default();
    // Each chord whole (`Space e sidebar`), not its tail beside a lone `Space …` at the far
    // edge (r16 notif-actions-06, gitlab-02).
    let chords = keys
        .after_leader()
        .into_iter()
        .map(|(key, label)| (format!("{} {key}", keys.leader_name()), label))
        .collect::<Vec<_>>();
    let hints: Vec<(&str, &str)> = if view.find.editing || view.filter.editing {
        vec![
            ("Enter", if view.find.editing { "find" } else { "filter" }),
            ("Esc", "cancel"),
        ]
    } else if view.picker {
        vec![("Enter", "choose"), ("j k", "move"), ("Esc", "close")]
    } else if ws.leader_pending() {
        // The leader waits for its chord: which keys finish it (no which-key sheet here).
        let mut hints = chords
            .iter()
            .map(|(key, label)| (key.as_str(), *label))
            .collect::<Vec<_>>();
        hints.push(("Esc", "cancel"));
        hints
    } else if !view.right && view.workflows {
        let mut hints = vec![
            ("j k", "choose"),
            ("Enter", "runs"),
            ("d", "dispatch"),
            ("Tab", "panels"),
        ];
        if !sidebar.is_empty() {
            hints.push((sidebar.as_str(), "sidebar"));
        }
        hints
    } else if !view.right {
        // The run under the cursor still going: `x cancel`, not `r rerun`.
        let running = ws
            .run_id
            .and_then(|id| ws.run_value(id))
            .is_some_and(|run| !State::of(run).finished());
        let rerun = if running {
            ("x", "cancel")
        } else {
            ("r", "re-run")
        };
        let mut hints = vec![
            ("Enter", "open"),
            ("[ ]", "sections"),
            ("/", "filter"),
            ("F", "narrow"),
            rerun,
        ];
        // Opened from Notifications: the way back reads second, so a narrow row keeps it.
        if ws.notify.back {
            hints.insert(1, ("Esc", "back"));
        }
        // The sidebar toggle ahead of `d` and `o`, so 120 columns keep it.
        if !sidebar.is_empty() {
            hints.push((sidebar.as_str(), "sidebar"));
        }
        hints.extend([("d", "dispatch"), ("o", "browser")]);
        hints
    } else if view.visual.is_some() {
        vec![("j k", "extend"), ("y", "copy"), ("Esc", "cancel")]
    } else if view.attempts {
        vec![("Enter", "show attempt"), ("j k", "move"), ("Esc", "close")]
    } else if view.find.active() {
        if view.nowrap {
            vec![
                ("n N", "matches"),
                ("w", "wrap"),
                ("H L", "pan"),
                ("Esc", "clear"),
            ]
        } else {
            vec![("n N", "matches"), ("w", "no wrap"), ("Esc", "clear")]
        }
    } else {
        let rows = ws.tree(KEYS);
        let at = ws.cursor_index(&rows);
        let row = rows.get(at);
        // (priority, key, label) in the order they read; the way back (`Esc list`) and the
        // way out of the full log (`f steps`) are kept first when the row is short.
        let mut hints: Vec<(u8, &str, &str)> = Vec::new();
        match row {
            Some(row) if row.link().is_some() => hints.push((1, "Enter", "open file")),
            // An expired archive can be neither downloaded nor opened: no hint offers it.
            Some(row) if ws.expired_artifact(&row.node) => {}
            Some(row) if matches!(row.node, Node::Artifact(_)) => {
                hints.extend([(1, "Enter", "files"), (6, "s", "download")])
            }
            Some(row) if row.fold == Some(true) => hints.push((1, "Enter", "fold")),
            Some(row) if row.fold == Some(false) => hints.push((1, "Enter", "unfold")),
            // A log line folds its step.
            Some(row) if matches!(row.node, Node::Line(..)) => {
                hints.extend([(1, "Enter", "fold"), (9, "v", "select")]);
            }
            _ => {}
        }
        let full = row
            .and_then(|row| job_of(&row.node))
            .is_some_and(|job| view.full.contains(&job));
        // Opened from a pull request's check, Esc goes back to it (R9-05).
        // Opened from Notifications, Esc goes back there.
        back = if ws.notify.back {
            Some("back".into())
        } else {
            ws.return_to
                .as_ref()
                .map(|target| format!("back to {}", target.label))
        };
        hints.extend([
            (2, "f", if full { "steps" } else { "full log" }),
            // Zoomed, the way back is as needed as `Esc list` (A2-02).
            if view.zoom {
                (0, "z", "unzoom")
            } else {
                (7, "z", "zoom")
            },
            (3, "n", "next error"),
        ]);
        // Offered where the forge is known to keep attempts, or this run has had several
        // (an unprobed Gitea may predate 1.27's attempts API).
        if matches!(
            ws.capabilities().attempts,
            crate::forge::capability::Cap::Yes
        ) || ws.latest_attempt() > 1
        {
            // Above `w`: attempts are the only way to a flaky step.
            hints.push((6, "a", "attempts"));
        }
        // Re-run what the cursor is on: its job, else the failed jobs (the sheet offers all).
        // A run still going cannot be re-run (`r` says so); its key is `x cancel` (r15
        // narrow pass: the footer offered `r rerun` on #215 while it ran).
        let running = ws
            .run_id
            .and_then(|id| ws.run_value(id))
            .is_some_and(|run| !State::of(run).finished());
        let rerun = match ws.run_id {
            _ if running => "cancel",
            Some(id) if ws.run_jobs(id).0 > 0 => "re-run failed",
            Some(id) if ws.cursor_job(id).is_some() => "re-run job",
            _ => "re-run",
        };
        // Signed out, a run can only be read: the key still says why when pressed.
        if !ws.read_only() {
            hints.push((4, if running { "x" } else { "r" }, rerun));
        }
        hints.extend([
            (5, "/", "find"),
            (8, "o", "browser"),
            (8, "w", if view.nowrap { "wrap" } else { "no wrap" }),
            (0, "Esc", back.as_deref().unwrap_or("list")),
        ]);
        if !sidebar.is_empty() {
            hints.push((9, sidebar.as_str(), "sidebar"));
        }
        // The width the row has after a mode chip (NORMAL), or the last hints (`Esc`) are cut.
        fit_hints(&hints, chrome::footer_rect(rect).w, message)
    };
    let context = if view.right || ws.leader_pending() {
        String::new()
    } else {
        view.filter.query.clone()
    };
    // Signed out: Actions' writes leave the footer, as the other tabs' do (pressing one
    // still says why).
    let mut hints = hints;
    if ws.read_only() {
        hints
            .retain(|hint| !matches!(*hint, ("x", "cancel") | ("r", "re-run") | ("d", "dispatch")));
    }
    chrome::footer_with(&mut frame.grid, rect, &hints, &context, message, usize::MAX);
}

/// The hints `chrome::footer_with` will show whole (`? keys` after them), chosen by priority
/// (0 first) until the width runs out, and kept in reading order.
fn fit_hints<'a>(
    hints: &[(u8, &'a str, &'a str)],
    width: u16,
    message: Option<(&str, bool)>,
) -> Vec<(&'a str, &'a str)> {
    let room = width.saturating_sub(1);
    let context = message
        .filter(|(text, _)| !text.is_empty())
        .map_or(0, |(text, error)| {
            chrome::width(text) + 2 + if error { 2 } else { 0 }
        });
    let mut used = 6 + context;
    let mut order = (0..hints.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| hints[*index].0);
    let mut kept = Vec::new();
    for index in order {
        let (_, key, label) = hints[index];
        let size = chrome::width(key) + chrome::width(label) + 4;
        if used + size > room {
            continue;
        }
        used += size;
        kept.push(index);
    }
    kept.sort_unstable();
    kept.into_iter()
        .map(|index| (hints[index].1, hints[index].2))
        .collect()
}

/// What the Git tutor's Actions lessons check (`src/git/screen/tutor`): read only.
pub(in crate::git::screen) struct TutorRuns {
    pub section: usize,
    pub run_open: bool,
    pub zoom: bool,
    pub full_log: bool,
    /// The cursor sits on an error line a key moved it to (not the first-error default).
    pub jumped_to_error: bool,
    /// The focused panel: 0 Workflows, 1 runs, 2 the run.
    pub panel: usize,
    /// Long log lines run past the edge (`w`), and how far they are panned (`H` `L`).
    pub nowrap: bool,
    pub pan: usize,
    /// The run list's `/` filter, and whether it is still being typed.
    pub filter: String,
    pub filtering: bool,
    /// The log's `/` find.
    pub find: String,
    /// The list is narrowed to a workflow, branch or event (`F`).
    pub narrowed: bool,
    /// The run tree lists an artifact's files.
    pub artifact_files: bool,
    /// An artifact was downloaded from this run.
    pub downloaded: bool,
}

impl Workspace {
    pub(super) fn tutor_runs(&self) -> TutorRuns {
        let rows = self.tree(KEYS);
        let at = self.cursor_index(&rows);
        TutorRuns {
            section: self.actions.section,
            run_open: self.actions.right,
            zoom: self.actions.zoom,
            full_log: !self.actions.full.is_empty(),
            jumped_to_error: !self.actions.auto && rows.get(at).is_some_and(Row::error),
            panel: self.panel(),
            nowrap: self.actions.nowrap,
            pan: self.actions.pan,
            filter: self.actions.filter.query.clone(),
            filtering: self.actions.filter.editing,
            find: self.actions.find.query.clone(),
            narrowed: self.workflow_selected != 0 || !self.filter_words().is_empty(),
            artifact_files: rows.iter().any(|row| matches!(row.node, Node::File(..))),
            downloaded: self.actions.receipt.is_some(),
        }
    }
}

/// A run title cut to `room` cells at a seam: whole ` · ` parts go from the end
/// first, then the cut falls after a `/` or a space, so `refs/merge-requests/13/…` never
/// keeps half a path segment or a dangling `·`.
fn fit_title(title: &str, room: usize) -> String {
    if title.width() <= room || room < 4 {
        return title.to_owned();
    }
    let mut parts = title.split(" · ").collect::<Vec<_>>();
    while parts.len() > 1 && parts.join(" · ").width() > room {
        parts.pop();
    }
    let kept = parts.join(" · ");
    if kept.width() <= room {
        return kept;
    }
    let cut = chrome::ellipsize(&kept, room);
    let body = cut.trim_end_matches('…');
    // A cut that ends a word keeps it (`Bound retries with exponential…`).
    if kept.starts_with(body) && kept[body.len()..].starts_with(' ') {
        return format!("{}…", body.trim_end());
    }
    match body.rfind(['/', ' ']) {
        Some(at) if at >= body.len() / 2 => format!("{}…", body[..=at].trim_end()),
        _ => cut,
    }
}

/// Where a log line's `path:line` opens: `open-at:LINE:PATH` (PATH relative to the
/// repository, so the editor or the standalone screen lands on that line), `open:FILE` for
/// a line 0, else why not. A runner's absolute path (`/home/runner/work/orbit/orbit/src/
/// queue.py`) is matched by its longest tail that is a file here.
fn source_target(root: &std::path::Path, path: &str, line: u32) -> String {
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for start in 0..parts.len() {
        let relative = parts[start..].join("/");
        let candidate = root.join(&relative);
        if candidate.is_file() {
            return if line > 0 {
                format!("open-at:{line}:{relative}")
            } else {
                format!("open:{}", candidate.display())
            };
        }
    }
    format!("{path} is not in this repository")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_failed_read_reads_in_plain_words_with_one_next_key() {
        let mut ws = workspace();
        // Each forge error as the worker reports it, and what the Actions tab says instead.
        let gateway = calm(&ws, "Network error · 502 Bad Gateway");
        assert_eq!(gateway, "github.com is having problems · R retry");
        let limited = calm(&ws, "Rate limited · retry in 711s");
        assert!(
            limited.starts_with("Rate limited by github.com · resets in "),
            "{limited}"
        );
        assert!(
            limited.ends_with(" · R retry once it resets") && !limited.contains("711s"),
            "{limited}"
        );
        let offline = calm(&ws, "Network error · error sending request");
        assert_eq!(
            offline,
            "Can't reach github.com · you may be offline · R retry"
        );
        // Painted where the runs list would be: the words, never the raw status line.
        let text = |ws: &mut Workspace| {
            let mut frame = Frame {
                grid: CellGrid::new(190, 40),
                cursor: None,
            };
            page(
                &mut frame,
                ws,
                Rect::new(0, 1, 190, 38),
                ExplorerIconMode::Unicode,
            );
            (0..40)
                .map(|y| frame.grid.row_text(y))
                .collect::<Vec<_>>()
                .join("\n")
        };
        for (error, words) in [
            ("Network error · 502 Bad Gateway", "is having problems"),
            ("Rate limited · retry in 711s", "Rate limited by github.com"),
        ] {
            ws.cache.insert(Key::Runs(0), Err(error.into()));
            let shown = text(&mut ws);
            assert!(shown.contains(words), "{words} in:\n{shown}");
            if error.starts_with("Network error") {
                let row = shown.lines().find(|line| line.contains(words)).unwrap();
                assert!(row.contains(&gateway), "{row}");
                assert_eq!(row.matches("R retry").count(), 1, "{row}");
            }
            assert!(
                !shown.contains("Bad Gateway") && !shown.contains("711s"),
                "{shown}"
            );
        }
    }

    #[test]
    fn a_row_says_its_place_and_event_once() {
        let ws = workspace();
        // GitLab names a pipeline with no commit title after its ref and source: the
        // second line keeps who and when, not `topic-11 · push` again.
        let run = json!({
            "displayTitle": "topic-11 · push",
            "event": "push",
            "headBranch": "topic-11",
            "createdAt": "2026-11-15T00:00:00Z",
            "dusty": { "actor": "maya" },
        });
        let meta = ws.run_meta(&run, &ws.run_title(&run), 80);
        assert!(
            meta.contains("maya") && !meta.contains("topic-11") && !meta.contains("push"),
            "{meta}"
        );
        // A commit title leaves both on the second line.
        let titled = json!({
            "displayTitle": "Bound retries",
            "event": "push",
            "headBranch": "topic-11",
            "createdAt": "2026-11-15T00:00:00Z",
            "dusty": { "actor": "maya" },
        });
        let meta = ws.run_meta(&titled, &ws.run_title(&titled), 80);
        assert!(
            meta.contains("topic-11") && meta.contains("push") && meta.contains("maya"),
            "{meta}"
        );
    }

    #[test]
    fn a_log_lines_file_and_line_open_at_that_line() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/queue.py"), "pass\n").unwrap();
        // A test failure's `src/queue.py:42`, and the runner's absolute copy of it.
        assert_eq!(
            source_target(root.path(), "src/queue.py", 42),
            "open-at:42:src/queue.py"
        );
        assert_eq!(
            source_target(root.path(), "/home/runner/work/orbit/orbit/src/queue.py", 7),
            "open-at:7:src/queue.py"
        );
        let whole = source_target(root.path(), "src/queue.py", 0);
        assert_eq!(
            whole,
            format!("open:{}", root.path().join("src/queue.py").display())
        );
        assert_eq!(
            source_target(root.path(), "lib/gone.py", 3),
            "lib/gone.py is not in this repository"
        );
        // From the log text itself, as Enter on the line reads it.
        let (_, path, at) =
            link("FAILED tests/test_queue.py::test_backoff - src/queue.py:42: AssertionError")
                .unwrap();
        assert_eq!(
            source_target(root.path(), &path, at),
            "open-at:42:src/queue.py"
        );
    }

    #[test]
    fn the_actions_tab_badges_failing_runs_and_a_running_one() {
        let mut workspace = workspace();
        assert_eq!(workspace.actions_badge(), Some((1, false)));
        let running = json!({"databaseId": 502, "status": "in_progress", "conclusion": null});
        let passed = json!({"databaseId": 503, "status": "completed", "conclusion": "success"});
        workspace.cache.insert(
            Key::Runs(0),
            Ok(Reply {
                text: String::new(),
                value: json!([running, passed.clone()]),
            }),
        );
        assert_eq!(workspace.actions_badge(), Some((0, true)));
        workspace.cache.insert(
            Key::Runs(0),
            Ok(Reply {
                text: String::new(),
                value: json!([passed]),
            }),
        );
        assert_eq!(workspace.actions_badge(), None, "all quiet: no badge");
    }

    #[test]
    fn a_title_cut_at_a_word_end_keeps_the_word() {
        let title = "Bound retries with exponential backoff";
        assert_eq!(fit_title(title, 31), "Bound retries with exponential…");
        assert_eq!(fit_title(title, 29), "Bound retries with…");
        assert_eq!(fit_title(title, 40), title);
    }

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
                               "startedAt": "2026-11-14T05:00:02Z", "completedAt": "2026-11-14T05:00:05Z"}]},
                    {"databaseId": 700 + attempt, "name": "lint", "status": "completed",
                    "conclusion": "success", "startedAt": "2026-11-14T05:00:01Z",
                    "completedAt": "2026-11-14T05:00:03Z",
                    "steps": [{"number": 1, "name": "Run lint", "status": "completed", "conclusion": "success",
                               "startedAt": "2026-11-14T05:00:01Z", "completedAt": "2026-11-14T05:00:03Z"}]}]})
        };
        let insert = |workspace: &mut Workspace, key: Key, value: Value, text: String| {
            workspace.cache.insert(key, Ok(Reply { text, value }));
        };
        insert(
            &mut workspace,
            Key::Repository,
            json!({"nameWithOwner": "octo/orbit"}),
            String::new(),
        );
        insert(
            &mut workspace,
            Key::Runs(0),
            json!([run(501, 2, "failure")]),
            String::new(),
        );
        insert(
            &mut workspace,
            Key::Run(501, 0),
            run(501, 2, "failure"),
            String::new(),
        );
        insert(
            &mut workspace,
            Key::Run(501, 1),
            run(501, 1, "failure"),
            String::new(),
        );
        // Both attempts print the same output: the tree belongs to the run and attempt.
        let log = "test\tUNKNOWN STEP\t2026-11-14T05:00:01.5Z setting up\n\
                   test\tUNKNOWN STEP\t2026-11-14T05:00:03.0Z running 3 tests\n\
                   test\tUNKNOWN STEP\t2026-11-14T05:00:04.0Z error: retries_uploads failed at src/lib.rs:41\n";
        insert(&mut workspace, Key::Log(602), Value::Null, log.into());
        insert(&mut workspace, Key::Log(601), Value::Null, log.into());
        workspace.run_id = Some(501);
        workspace.sync_actions();
        workspace
    }

    /// A run sheet names the run as the header does, says what it does, and a destructive one
    /// starts on its safe button; GitLab's say pipeline.
    /// R9-05: a run opened from a pull request's check goes back to it on Esc, to the same
    /// tab and line; the title says so (`from #18 · Checks`). Going to the list forgets it.
    #[test]
    fn a_run_opened_from_a_check_goes_back_to_the_pull_request() {
        let key = |code| KeyEvent {
            code,
            modifiers: Default::default(),
        };
        let mut workspace = workspace();
        workspace.page = 2;
        workspace.opened = Some((2, 18));
        workspace.pr_tab = super::super::pr::CHECKS;
        workspace.detail_cursor = 4;
        workspace.activate(Action::Run(501)).unwrap();
        assert_eq!(workspace.page, 3);
        assert_eq!(
            workspace
                .return_to
                .as_ref()
                .map(|target| target.label.as_str()),
            Some("#18")
        );
        assert_eq!(
            workspace.return_crumb().as_deref(),
            Some("from #18 · Checks")
        );
        // Zoom is undone first; then Esc goes back.
        workspace.actions_key(&key(KeyCode::Char('z')), None);
        workspace.actions_key(&key(KeyCode::Escape), None);
        assert_eq!(workspace.page, 3, "the first Esc only unzooms");
        workspace.actions_key(&key(KeyCode::Escape), None);
        assert_eq!(workspace.page, 2);
        assert_eq!(workspace.number(), 18);
        assert_eq!(workspace.pr_tab, super::super::pr::CHECKS);
        assert_eq!(workspace.detail_cursor, 4);
        assert!(workspace.return_to.is_none());
        // Tab to the list: the run is no longer the check's, and Esc stays in Actions.
        workspace.activate(Action::Run(501)).unwrap();
        workspace.actions_key(&key(KeyCode::Tab), None);
        assert!(workspace.return_to.is_none() && workspace.return_crumb().is_none());
        assert_eq!(workspace.page, 3);
        // From anywhere but a pull request (a notification) there is nothing to go back to.
        workspace.page = 4;
        workspace.activate(Action::Run(501)).unwrap();
        assert!(workspace.return_to.is_none());
    }

    /// A2-02: the run footer fills by priority until the width runs out, not to six.
    #[test]
    fn run_footer_fills_the_row_by_priority() {
        let hints = [
            (1, "Enter", "fold"),
            (2, "f", "full log"),
            (7, "z", "zoom"),
            (3, "n", "next error"),
            (6, "a", "attempts"),
            (4, "r", "rerun"),
            (5, "/", "find"),
            (8, "w", "no wrap"),
            (0, "Esc", "list"),
        ];
        let keys = |width| {
            fit_hints(&hints, width, None)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(keys(100), "Enter f z n a r / Esc");
        assert_eq!(keys(120), "Enter f z n a r / w Esc");
        assert_eq!(keys(60), "Enter f n Esc");
    }

    #[test]
    fn rerun_sheet_waits_for_jobs_before_offering_failed_jobs() {
        let mut workspace = workspace();
        let detail = workspace.cache.remove(&Key::Run(501, 0)).unwrap();
        if let Some(Ok(reply)) = workspace.cache.get_mut(&Key::Runs(0)) {
            reply.value[0].as_object_mut().unwrap().remove("jobs");
        }

        assert!(workspace.activate(Action::Rerun(501)).unwrap().is_none());
        assert_eq!(workspace.actions.pending_rerun, Some(501));
        assert!(workspace.notice.is_empty());

        workspace.cache.insert(Key::Run(501, 0), detail);
        workspace.poll_actions();

        assert_eq!(workspace.actions.pending_rerun, None);
        let rerun = workspace.ready_form.as_ref().expect("the jobs arrived");
        let jobs = rerun
            .fields
            .iter()
            .find(|field| field.label == "Jobs")
            .unwrap();
        assert!(
            jobs.choices
                .iter()
                .any(|choice| choice == "Failed jobs only (1)")
        );
    }

    #[test]
    fn run_sheets_name_the_run_and_start_on_the_safe_choice() {
        let mut workspace = workspace();
        let rerun = workspace.activate(Action::Rerun(501)).unwrap().unwrap();
        assert_eq!(rerun.title, "Re-run #10 · Retry flaky uploads");
        assert_eq!(rerun.selected, 0);
        let note = workspace
            .sheet_note(&Action::Rerun(501), &rerun.fields)
            .unwrap();
        assert_eq!(
            note,
            "Reruns 1 failed job (test) on the same commit (aaaaaaa)."
        );
        let mut all = rerun.fields;
        all[0].input.input = all[0].choices[1].clone();
        let note = workspace.sheet_note(&Action::Rerun(501), &all).unwrap();
        assert_eq!(note, "Reruns all 2 jobs on the same commit (aaaaaaa).");
        // The download sheet shows the path it writes and leaves the repository to the top bar.
        let download = workspace
            .activate(Action::Download(701, "coverage".into()))
            .unwrap()
            .unwrap();
        assert_eq!(download.title, "Download artifact · coverage");
        assert_eq!(download.fields[0].input.input, "./artifacts");
        // Still running: cancel opens on `Keep running`, so Enter cancels nothing.
        for key in [Key::Run(501, 0), Key::Run(501, 1)] {
            if let Some(Ok(reply)) = workspace.cache.get_mut(&key) {
                reply.value["status"] = json!("in_progress");
            }
        }
        if let Some(Ok(reply)) = workspace.cache.get_mut(&Key::Runs(0)) {
            reply.value[0]["status"] = json!("in_progress");
        }
        let cancel = workspace.activate(Action::Cancel(501)).unwrap().unwrap();
        assert_eq!(cancel.title, "Cancel run #10 · Retry flaky uploads?");
        assert_eq!(
            cancel.selected,
            cancel.fields.len() + 1,
            "focus starts on the safe button"
        );
        assert_eq!(
            workspace
                .sheet_note(&Action::Cancel(501), &cancel.fields)
                .as_deref(),
            Some("Running jobs stop. Finished jobs keep their logs.")
        );
        // GitLab names a pipeline, never its source (`Merge request`), in every sheet.
        workspace.host = "gitlab.com".into();
        let cancel = workspace.activate(Action::Cancel(501)).unwrap().unwrap();
        assert_eq!(cancel.title, "Cancel pipeline #10 · Retry flaky uploads?");
        assert_eq!(cancel.selected, 1);
        let error = workspace
            .activate(Action::Rerun(501))
            .err()
            .expect("a running pipeline refuses a rerun");
        assert_eq!(error, "Pipeline #10 is still running · x cancels it");
        let dispatch = workspace.activate(Action::Dispatch(31)).unwrap().unwrap();
        assert_eq!(dispatch.title, "Run pipeline");
        assert_eq!(dispatch.fields[0].label, "Branch or tag");
        for key in [Key::Run(501, 0), Key::Run(501, 1)] {
            if let Some(Ok(reply)) = workspace.cache.get_mut(&key) {
                reply.value["status"] = json!("completed");
                reply.value["workflowName"] = json!("Merge request");
            }
        }
        let rerun = workspace.activate(Action::Rerun(501)).unwrap().unwrap();
        assert_eq!(rerun.title, "Re-run pipeline #10 · Retry flaky uploads");
        assert!(
            rerun
                .fields
                .iter()
                .all(|field| field.label != "Debug logging")
        );
        // Nothing failed: one scope is no choice, and the note says what reruns (A3-04).
        for key in [Key::Run(501, 0), Key::Run(501, 1)] {
            if let Some(Ok(reply)) = workspace.cache.get_mut(&key) {
                reply.value["conclusion"] = json!("success");
                for job in reply.value["jobs"].as_array_mut().unwrap() {
                    job["conclusion"] = json!("success");
                }
            }
        }
        if let Some(Ok(reply)) = workspace.cache.get_mut(&Key::Runs(0)) {
            reply.value[0]["conclusion"] = json!("success");
        }
        let rerun = workspace.activate(Action::Rerun(501)).unwrap().unwrap();
        assert!(
            rerun.fields.iter().all(|field| field.label != "Jobs"),
            "{:?}",
            rerun
                .fields
                .iter()
                .map(|field| &field.label)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            workspace
                .sheet_note(&Action::Rerun(501), &rerun.fields)
                .as_deref(),
            Some("Reruns all 2 jobs on the same commit (aaaaaaa).")
        );
    }

    #[test]
    fn saving_a_log_hides_unknown_folder_errors() {
        let mut workspace = workspace();
        let directory = tempfile::tempdir().unwrap();
        workspace.root = directory.path().to_owned();
        assert_eq!(
            workspace.save_log(602, &["private\0/log.txt".into()]),
            Err("Couldn't create the folder".into())
        );
        std::fs::write(directory.path().join("occupied"), "").unwrap();
        assert_eq!(
            workspace.save_log(602, &["occupied/log.txt".into()]),
            Err("Couldn't create the folder · already exists".into())
        );
    }

    #[test]
    fn saving_a_log_hides_unknown_write_errors_and_clears_the_notice_on_success() {
        let mut workspace = workspace();
        let directory = tempfile::tempdir().unwrap();
        workspace.root = directory.path().to_owned();
        // Writing over a folder: Linux and macOS give an error the notice doesn't name,
        // Windows "access denied".
        let error = workspace.save_log(602, &[".".into()]).err();
        assert!(
            matches!(
                error.as_deref(),
                Some("Couldn't save the log" | "Couldn't save the log · permission denied")
            ),
            "{error:?}"
        );
        workspace.notice = "Couldn't save the log".into();
        workspace.notice_error = true;
        workspace.save_log(602, &["test.log".into()]).unwrap();
        assert_eq!(workspace.notice, "Saved 3 lines to test.log");
        assert!(!workspace.notice_error);
        assert!(
            std::fs::read_to_string(directory.path().join("test.log"))
                .unwrap()
                .contains("running 3 tests")
        );
    }

    #[test]
    fn logs_the_forge_lost_are_said_once_for_the_run() {
        let note = |job, text: &str| {
            Row::new(
                Node::Note(job, 0),
                1,
                vec![(text.to_owned(), st(GitRole::Removed, PANE))],
            )
        };
        let gone = "Not on the forge any more · R retry";
        let mut rows = vec![note(1, gone), note(2, gone)];
        say_gone_logs_once(&mut rows);
        let texts = rows
            .iter()
            .map(|row| row.left[0].0.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            [
                "2 job logs: not on the forge any more · R retry",
                "No log",
                "No log"
            ]
        );
        // One job, or two reasons: each says its own.
        let mut rows = vec![
            note(1, gone),
            note(2, "Couldn't read this from the forge · R retry"),
        ];
        say_gone_logs_once(&mut rows);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].left[0].0, gone);
    }

    fn nodes(workspace: &Workspace) -> Vec<Node> {
        workspace
            .tree(KEYS)
            .into_iter()
            .map(|row| row.node)
            .collect()
    }

    #[test]
    fn failing_work_opens_at_its_first_error_and_passing_jobs_stay_closed() {
        let workspace = workspace();
        let rows = workspace.tree(KEYS);
        let shown = nodes(&workspace);
        assert!(shown.contains(&Node::Step(602, 2)), "{shown:?}");
        assert!(
            !shown.contains(&Node::Step(702, 1)),
            "a passing job stays closed: {shown:?}"
        );
        assert_eq!(workspace.wanted_logs(), vec![602]);
        let at = workspace.cursor_index(&rows);
        let (_, line) = rows[at].log.as_ref().expect("the cursor is on a log line");
        assert!(
            line.text.starts_with("error: retries_uploads"),
            "{}",
            line.text
        );
        assert_eq!(rows[at].link(), Some(("src/lib.rs".into(), 41)));
    }

    #[test]
    fn another_attempt_is_another_tree_even_with_identical_output() {
        let mut workspace = workspace();
        workspace.set_open(Node::Job(702), true);
        assert!(nodes(&workspace).contains(&Node::Step(702, 1)));
        assert_eq!(workspace.effective_attempt(), 2);
        assert_eq!(workspace.latest_attempt(), 2);
        workspace.attempt = 1;
        workspace.sync_actions();
        assert_eq!(workspace.effective_attempt(), 1);
        let shown = nodes(&workspace);
        assert!(shown.contains(&Node::Step(601, 2)), "{shown:?}");
        assert!(
            !shown.contains(&Node::Step(701, 1)),
            "choices do not carry over: {shown:?}"
        );
        workspace.attempt = 0;
        workspace.cache.remove(&Key::Run(501, 0));
        assert_eq!(
            workspace.effective_attempt(),
            0,
            "a detail that has not arrived names no attempt"
        );
    }

    #[test]
    fn step_slices_follow_step_times_and_the_full_log_replaces_the_steps() {
        let mut workspace = workspace();
        let rows = workspace.tree(KEYS);
        let lines = rows
            .iter()
            .filter(|row| matches!(row.node, Node::Line(602, 2, _)))
            .count();
        assert_eq!(lines, 2, "lines from 05:00:02 to 05:00:05");
        assert!(
            !rows.iter().any(|row| row.node == Node::Note(602, 2)),
            "a slice by time is not remarked on"
        );
        let at = workspace.cursor_index(&rows);
        workspace.toggle_full(&rows, at).unwrap();
        let rows = workspace.tree(KEYS);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.node, Node::Step(602, _)))
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row.node, Node::Line(602, 0, _)))
                .count(),
            3
        );
        assert_eq!(
            workspace.actions.cursor,
            Some(Node::Line(602, 0, 2)),
            "the cursor lands on the error"
        );
    }

    #[test]
    fn find_counts_wrap_around_and_errors_jump() {
        let mut workspace = workspace();
        workspace.actions.find.query = "e".into();
        let rows = workspace.tree(KEYS);
        let total = rows
            .iter()
            .filter_map(|row| row.log.as_ref())
            .map(|(_, line)| matches(&line.text, "e").len())
            .sum::<usize>();
        assert!(total >= 3);
        workspace.reveal_match(None);
        workspace.actions.find_current = 0;
        workspace.reveal_match(Some(false));
        assert_eq!(workspace.actions.find_current, total - 1);
        workspace.reveal_match(Some(true));
        assert_eq!(workspace.actions.find_current, 0);
        assert_eq!(matches("Error ERROR error", "error").len(), 3);
        assert_eq!(matches("Error ERROR error", "Error").len(), 1);
        workspace.actions.find = Input::default();
        workspace.jump_error(true).unwrap();
        assert!(matches!(
            workspace.actions.cursor,
            Some(Node::Line(602, 2, _))
        ));
    }

    #[test]
    fn v_selects_log_lines_and_y_copies_them() {
        let key = KeyEvent::plain;
        let mut workspace = workspace();
        workspace.jump_error(true).unwrap();
        let rows = workspace.tree(KEYS);
        let at = workspace.cursor_index(&rows);
        let text = |index: usize| rows[index].log.as_ref().map(|(_, line)| line.text.clone());
        let error = text(at).unwrap();
        // The rows from the one above the error to the error, log lines only.
        let spanned = (at - 1..=at).filter_map(text).collect::<Vec<_>>();
        workspace.run_key(&key(KeyCode::Char('v')), None).unwrap();
        assert!(workspace.actions.visual.is_some());
        workspace.run_key(&key(KeyCode::Char('k')), None).unwrap();
        workspace.run_key(&key(KeyCode::Char('y')), None).unwrap();
        assert_eq!(workspace.copied.take(), Some(spanned.join("\n")));
        assert!(workspace.notice.starts_with("Copied"));
        assert!(workspace.actions.visual.is_none());
        // Without a selection `y` copies the cursor's line; Esc ends a selection first.
        workspace.run_key(&key(KeyCode::Char('j')), None).unwrap();
        workspace.run_key(&key(KeyCode::Char('y')), None).unwrap();
        assert_eq!(workspace.copied.take(), Some(error));
        assert_eq!(workspace.notice, "Copied the line");
        workspace.run_key(&key(KeyCode::Char('V')), None).unwrap();
        workspace.run_key(&key(KeyCode::Escape), None).unwrap();
        assert!(workspace.actions.visual.is_none());
    }

    #[test]
    fn a_log_is_parsed_once_and_laid_out_once_per_width() {
        let workspace = workspace();
        let lines = |rows: &[Row]| {
            rows.iter()
                .filter_map(|row| {
                    row.log
                        .as_ref()
                        .map(|(_, line)| std::sync::Arc::clone(line))
                })
                .collect::<Vec<_>>()
        };
        let (before, after) = (lines(&workspace.tree(KEYS)), lines(&workspace.tree(KEYS)));
        assert!(!before.is_empty());
        assert!(
            before
                .iter()
                .zip(&after)
                .all(|(a, b)| std::sync::Arc::ptr_eq(a, b))
        );
        let line = &before[0];
        assert_eq!(line.parts(6, true, true, ""), line.parts(6, true, true, ""));
        assert!(
            line.wrapped
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|memo| memo.room == 6)
        );
        assert_eq!(line.parts(6, false, true, "").len(), 1);
    }

    #[test]
    fn toggling_closes_a_default_and_reopens_it() {
        let mut workspace = workspace();
        let rows = workspace.tree(KEYS);
        let job = rows
            .iter()
            .position(|row| row.node == Node::Job(602))
            .unwrap();
        workspace.toggle(&rows, job);
        assert!(!nodes(&workspace).contains(&Node::Step(602, 2)));
        assert!(workspace.wanted_logs().is_empty());
        let rows = workspace.tree(KEYS);
        workspace.toggle(&rows, job);
        assert!(nodes(&workspace).contains(&Node::Step(602, 2)));
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // one kept range is the case under test
    fn log_lines_wrap_at_word_edges_and_rulers_do_not_wrap() {
        let text = "test_backoff_doubles (test_backoff.BackoffTest.test_backoff_doubles) ... ok";
        let parts = wrap_points(text, 66)
            .into_iter()
            .map(|range| &text[range])
            .collect::<Vec<_>>();
        assert_eq!(
            parts,
            [
                "test_backoff_doubles (test_backoff.BackoffTest.test_backoff_",
                "doubles) ... ok"
            ]
        );
        let text = "test_retry_limit (test_regression.RegressionTest.test_retry_limit) ... FAIL";
        let parts = wrap_points(text, 66)
            .into_iter()
            .map(|range| &text[range])
            .collect::<Vec<_>>();
        assert_eq!(
            parts,
            [
                "test_retry_limit (test_regression.RegressionTest.test_retry_limit)",
                "... FAIL"
            ]
        );
        // Nowhere to break in the last 30%: a hard break.
        let long = format!("{}needle end", "x".repeat(28));
        let parts = wrap_points_keeping(&long, 30, &[28..34])
            .into_iter()
            .map(|range| &long[range])
            .collect::<Vec<_>>();
        assert_eq!(
            parts,
            [&"x".repeat(28)[..], "needle end"],
            "a find match is not split"
        );
        let parts = wrap_points(
            "aaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            30,
        );
        assert_eq!(parts[0], 0..30);
        // R9-07: a status line keeps its status, a framed summary its words.
        let line = "tests/test_retry.py::test_backoff_caps_at_sixty PASSED";
        let range = one_row(line, 40).expect("one row");
        assert!(line[range.clone()].ends_with("caps_at_sixty PASSED") && line[range].width() < 40);
        assert_eq!(one_row(line, 80), None);
        assert_eq!(
            one_row("Collected 42 items in tests/test_retry.py::x", 20),
            None
        );
        // The cut falls at a separator, not inside the file's name (R10-GLCR-11).
        let line = "tests/test_retry.py::test_retry_limit_stops_after_five FAILED";
        let range = one_row(line, 50).expect("one row");
        assert_eq!(&line[range], "test_retry_limit_stops_after_five FAILED");
        let range = one_row(line, 58).expect("one row");
        assert_eq!(
            &line[range],
            "test_retry.py::test_retry_limit_stops_after_five FAILED"
        );
        let rule = "=========== 1 failed, 41 passed in 2.71s ===========";
        let range = one_row(rule, 40).expect("one row");
        assert_eq!(&rule[range], "===== 1 failed, 41 passed in 2.71s =====");
        assert!(ruler(
            "======================================================================"
        ));
        assert!(ruler("  ----------"));
        assert!(!ruler("--- a/file") && !ruler("== =="));
    }

    #[test]
    fn links_need_a_file_and_a_line() {
        assert_eq!(
            link("at tests/test_regression.py:6").map(|found| found.1),
            Some("tests/test_regression.py".into())
        );
        assert_eq!(
            link("File \"/home/runner/work/orbit/src/main.rs:12:5\"").map(|found| found.2),
            Some(12)
        );
        assert_eq!(
            link("  File \"/home/runner/work/orbit/checkout/tests/test_regression.py\", line 6, in test_retry_limit").map(|found| (found.1, found.2)),
            Some(("/home/runner/work/orbit/checkout/tests/test_regression.py".into(), 6))
        );
        assert!(link("12:30:45 started").is_none());
        assert!(link("version 2.330.0").is_none());
        assert!(
            link("http://example.invalid:8080/x").is_none()
                || link("http://example.invalid:8080/x")
                    .is_some_and(|found| !found.1.starts_with("//"))
        );
    }
}
