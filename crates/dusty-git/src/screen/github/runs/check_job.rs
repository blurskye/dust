//! One job of a pull request's check, read the way the run tree reads it (its steps, each
//! step's slice of the log, the same folds, tones and wraps), as reader rows for the pull
//! request's Checks tab (`checks.rs`). Any run's job, not only the Actions page's: nothing here
//! reads or moves the Actions tree's own state.
use super::super::find;
use super::super::lines::Line;
use super::super::prose::{self, Rich};
use super::*;
use crate::forge::model::CiState;

/// A job the forge links by no run: a GitLab merge request's job, read by its own id. Enough
/// of one for its log and the steps its log's sections make. A started job carries a start no
/// clock reads, so the log is read and no time is made up.
pub(in crate::screen::github) fn bare_job(id: u64, name: &str, state: CiState) -> Value {
    let (status, conclusion) = match state {
        CiState::Passed => ("completed", "success"),
        CiState::Failed => ("completed", "failure"),
        CiState::Cancelled => ("completed", "cancelled"),
        CiState::Skipped => ("completed", "skipped"),
        CiState::Pending => ("in_progress", ""),
        CiState::ActionRequired => ("waiting", ""),
        CiState::Unknown => ("completed", ""),
    };
    let started = if matches!(state, CiState::Skipped | CiState::ActionRequired) {
        ""
    } else {
        "started"
    };
    serde_json::json!({
        "databaseId": id,
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "startedAt": started,
    })
}

/// A finished job's time, or how long a running one has run: the run tree's right column.
pub(in crate::screen::github) fn job_time(job: &Value) -> Option<(String, GitRole)> {
    match State::of(job) {
        State::Running => Some((
            elapsed(string(job, "startedAt"))
                .map_or_else(|| "running".into(), |time| format!("running {time}")),
            GitRole::Modified,
        )),
        State::Queued | State::Waiting => Some(("queued".into(), GitRole::Muted)),
        _ => job_seconds(job).map(|time| (duration(time), GitRole::Muted)),
    }
}

/// The widest a job's or step's row runs: its time sits at this column on a wide pane, not
/// 150 columns from the name (the log below still takes the whole width).
pub(in crate::screen::github) const STEP_ROW: usize = 60;

/// A row of text pieces with `right` flush with `room` cells.
fn spread(left: Vec<(String, Style)>, right: Vec<(String, Style)>, room: usize) -> Line {
    let mut rich = Rich::new();
    for (text, style) in left {
        rich = rich.push(text, style);
    }
    let right_width = right.iter().map(|(text, _)| text.width()).sum::<usize>();
    if right_width > 0 {
        let left_width = rich.width();
        rich = rich.to_column(room.saturating_sub(right_width).max(left_width + 2));
        for (text, style) in right {
            rich = rich.push(text, style);
        }
    }
    rich.line()
}

impl Workspace {
    /// Whether a job failed by its own conclusion or, finished, by a failed step: a check
    /// whose job reads `✕ Run pytest` is failing whatever the check's summary says.
    pub(in crate::screen::github) fn job_failed(&self, job: &Value) -> bool {
        let state = State::of(job);
        state == State::Failed
            || state.finished()
                && self
                    .steps(job)
                    .iter()
                    .any(|step| step.state == State::Failed)
    }

    /// Whether a job's pane needs its log: a step there shows it, or the job lists no steps
    /// of its own (they come from the log). The run tree's own reading (`wanted_logs`).
    /// `open(step, default)` is whether a step shows its log; `default` is the tree's
    /// (a failing step of a finished job, the running step).
    pub(in crate::screen::github) fn job_wants_log(
        &self,
        job: &Value,
        open: &dyn Fn(u64, bool) -> bool,
    ) -> bool {
        if !self.capabilities().job_log.allowed() {
            return false;
        }
        let state = State::of(job);
        let done = state.finished() && string(job, "status") == "completed";
        if !done && state != State::Running
            || state == State::Skipped && array(job, "steps").is_empty()
        {
            return false;
        }
        array(job, "steps").is_empty()
            || self.steps(job).iter().any(|step| {
                step.state != State::Skipped && open(step.number, step_opens(state, step.state))
            })
    }

    /// A job's steps, each open one with its slice of the log under it, in `room` cells; a
    /// job without steps shows its whole log. Each step's own row comes with its number (the
    /// Checks tab makes Enter there open or close it); a skipped step has nothing to open.
    pub(in crate::screen::github) fn job_rows(
        &self,
        job: &Value,
        open: &dyn Fn(u64, bool) -> bool,
        room: usize,
        mode: ExplorerIconMode,
    ) -> Vec<(Option<u64>, Line)> {
        let mut rows = Vec::new();
        let state = State::of(job);
        let steps = self.steps(job);
        if steps.is_empty() {
            self.log_rows(&mut rows, job, None, 0, room);
            return rows;
        }
        for step in &steps {
            let skipped = step.state == State::Skipped;
            let shown = !skipped && open(step.number, step_opens(state, step.state));
            let fold = if skipped {
                "  ".to_owned()
            } else {
                format!(
                    "{} ",
                    glyph(
                        if shown {
                            Glyph::FoldOpen
                        } else {
                            Glyph::FoldClosed
                        },
                        mode
                    )
                )
            };
            let name = match step.state {
                State::Failed => prose::strong(GitRole::Text),
                State::Skipped => prose::role(GitRole::Muted),
                _ => prose::role(GitRole::Text),
            };
            let right = match step.state {
                State::Skipped => vec![("–".to_owned(), prose::role(GitRole::Muted))],
                State::Running => {
                    let time = step.started.as_deref().and_then(elapsed);
                    vec![(
                        time.map_or_else(|| "running".into(), |time| format!("running {time}")),
                        prose::role(GitRole::Modified),
                    )]
                }
                _ => step
                    .seconds
                    .map(|time| vec![(duration(time), prose::role(GitRole::Muted))])
                    .unwrap_or_default(),
            };
            let line = spread(
                vec![
                    (fold, prose::role(GitRole::Muted)),
                    (
                        format!("{} ", step.state.glyph(mode)),
                        prose::role(step.state.role()),
                    ),
                    (step.name.clone(), name),
                ],
                right,
                room.min(STEP_ROW),
            );
            rows.push(((!skipped).then_some(step.number), line));
            if shown {
                self.log_rows(&mut rows, job, Some(step), 2, room);
            }
        }
        rows
    }

    /// A job's log (or one step's slice of it) as numbered rows wrapped to `room` cells, as
    /// the run tree draws it; what stands in for a log (loading, skipped, an error) is a row.
    #[allow(clippy::single_range_in_vec_init)] // a row that does not wrap is one range
    fn log_rows(
        &self,
        rows: &mut Vec<(Option<u64>, Line)>,
        job: &Value,
        step: Option<&StepRow>,
        depth: usize,
        room: usize,
    ) {
        let indent = " ".repeat(depth);
        // A log that would not load is a calm note in the log's place; the steps above stay.
        if State::of(job) != State::Running
            && matches!(
                self.cache.get(&Key::Log(number(job, "databaseId"))),
                Some(Err(_))
            )
        {
            rows.push((
                None,
                Rich::new()
                    .plain(&indent)
                    .push("Log unavailable · R retry", prose::role(GitRole::Muted))
                    .line(),
            ));
            return;
        }
        let (lines, skip) = match self.log_body(job, step, step.is_none()) {
            Body::Message(text, tone) => {
                rows.push((
                    None,
                    Rich::new()
                        .plain(&indent)
                        .push(text, prose::role(tone))
                        .line(),
                ));
                return;
            }
            Body::Lines { lines, skip } => (lines, skip),
        };
        let digits = lines.len().to_string().len().max(2);
        let gutter = depth + digits + 1;
        // One cell spare at the edge, as the reader keeps for its own marks.
        let text_room = room.saturating_sub(gutter + 1).max(8);
        for (index, line) in lines.iter().enumerate().skip(skip) {
            // The unwrapped line, so the reader's find and its cursor keep to it by identity.
            let logical: std::sync::Arc<str> = std::sync::Arc::from(line.text.as_str());
            let base = |at: usize| -> Style {
                let mut tone = match line.tone {
                    Tone::Error => GitRole::Removed,
                    Tone::Pass => GitRole::Added,
                    Tone::Quiet => GitRole::Muted,
                    _ => GitRole::Text,
                };
                let mut weight = matches!(line.tone, Tone::Command | Tone::Heading | Tone::Error);
                if let Some((_, sgr)) = line.colours.iter().find(|(range, _)| range.contains(&at)) {
                    if let Some(color) = sgr.color {
                        tone = sgr_role(color);
                    }
                    weight |= sgr.bold;
                }
                if line
                    .link
                    .as_ref()
                    .is_some_and(|(range, _, _)| range.contains(&at))
                {
                    tone = GitRole::Link;
                }
                if weight {
                    prose::strong(tone)
                } else {
                    prose::role(tone)
                }
            };
            let parts = if ruler(&line.text) {
                // A ruler is cut at the edge: nothing is lost.
                let end = line
                    .text
                    .char_indices()
                    .nth(text_room)
                    .map_or(line.text.len(), |(at, _)| at);
                vec![0..end]
            } else {
                // Every line wraps whole from its first token: never `…test_retry.py::…`.
                wrap_points(&line.text, text_room.max(4))
            };
            for (part, range) in parts.into_iter().enumerate() {
                // A test line drawn from inside its id (R9-07) says so.
                let elided = part == 0 && range.start > 0 && !ruler(&line.text);
                let mut text = String::new();
                let mut paint: Vec<(Range<usize>, Style)> = Vec::new();
                if elided {
                    text.push('…');
                    paint.push((0..text.len(), prose::role(GitRole::Muted)));
                }
                let offset = text.len();
                text.push_str(&line.text[range.clone()]);
                // One paint range per run of one style.
                for (at, grapheme) in line.text[range.clone()].grapheme_indices(true) {
                    let style = base(range.start + at);
                    let span = offset + at..offset + at + grapheme.len();
                    let joins = paint
                        .last()
                        .is_some_and(|(last, same)| *same == style && last.end == span.start);
                    if !joins {
                        paint.push((span, style));
                    } else if let Some((last, _)) = paint.last_mut() {
                        last.end = span.end;
                    }
                }
                let mut row = Line::text(text, Style::Default);
                row.paint = paint;
                row.pan = false;
                row.source = Some(find::Source {
                    text: logical.clone(),
                    offset: if elided { 0 } else { range.start },
                });
                // A dim rail of numbers, blank beside a wrapped line's continuation.
                row.gutter = if part == 0 {
                    format!("{indent}{:>digits$} ", index + 1)
                } else {
                    " ".repeat(gutter)
                };
                row.gutter_style = prose::role(GitRole::Border);
                rows.push((None, row));
            }
        }
    }
}
