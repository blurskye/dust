use super::super::render::{ellipsize, footer, section, viewport};
use super::*;
use crate::config::{AmbiguousWidth, Highlight};
use crate::ui::{CellGrid, Frame, PickerTextPreview, Style};
use crossterm::cursor::SetCursorStyle;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct Line {
    text: String,
    style: Style,
    pub action: Option<Action>,
    spans: Vec<(Range<usize>, Highlight)>,
    gutter: String,
    gutter_style: Style,
    sidebar: bool,
    diff: Option<bool>,
}
impl Line {
    fn text(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            action: None,
            spans: Vec::new(),
            gutter: String::new(),
            gutter_style: Style::Muted,
            sidebar: false,
            diff: None,
        }
    }
    fn button(text: impl Into<String>, action: Action) -> Self {
        let text = text.into();
        let text = text
            .strip_prefix("[ ")
            .and_then(|text| text.strip_suffix(" ]"))
            .unwrap_or(&text)
            .to_owned();
        let expandable = matches!(
            action,
            Action::Job(_)
                | Action::Step(..)
                | Action::FullLog(_)
                | Action::Collapse(..)
                | Action::Commit(_)
                | Action::PrTab(_)
        );
        let mut line = Self::text(
            if expandable {
                text
            } else {
                format!("› {text}")
            },
            if expandable {
                Style::Default
            } else {
                Style::GitControl
            },
        );
        line.action = Some(action);
        line
    }
}
fn body(rows: &mut Vec<Line>, text: &str, width: usize) {
    let lines = crate::ui::render::wrap_plain_text(text, width.max(10), AmbiguousWidth::Narrow);
    let preview = PickerTextPreview::new(
        lines,
        0,
        None,
        Some(crate::language::syntax::SyntaxLanguage::Markdown),
    );
    for (index, line) in preview.lines().iter().enumerate() {
        let mut row = Line::text(line, Style::Default);
        row.spans = preview.line_spans(index).to_vec();
        rows.push(row);
    }
}
fn error_or_loading(workspace: &Workspace, key: &Key, rows: &mut Vec<Line>) -> bool {
    match workspace.cache.get(key) {
        Some(Ok(_)) => false,
        Some(Err(error)) => {
            rows.push(Line::text(error, Style::Error));
            rows.push(Line::text(
                "r refresh · authenticate with gh auth login if needed",
                Style::Muted,
            ));
            true
        }
        None => {
            rows.push(Line::text("Loading…", Style::Muted));
            true
        }
    }
}
fn metadata(workspace: &Workspace, item: &Value, sidebar: bool) -> Vec<Line> {
    let id = number(item, "number");
    let page = workspace.page;
    let mut rows = Vec::new();
    for (field, label, key) in [
        ("assignees", "Assignees", "login"),
        ("labels", "Labels", "name"),
        ("projects", "Projects", "title"),
    ] {
        rows.push(Line::button(
            format!("[ Edit {label} ]"),
            Action::Metadata(page, id, field),
        ));
        let names = array(
            item,
            if field == "projects" {
                "projectItems"
            } else {
                field
            },
        )
        .iter()
        .filter_map(|value| {
            value[key]
                .as_str()
                .or_else(|| value["project"][key].as_str())
        })
        .collect::<Vec<_>>()
        .join(", ");
        rows.push(Line::text(
            if names.is_empty() {
                "None".into()
            } else {
                names
            },
            Style::Muted,
        ));
        rows.push(Line::text("", Style::Default));
    }
    rows.push(Line::button(
        "[ Edit milestone ]",
        Action::Metadata(page, id, "milestone"),
    ));
    rows.push(Line::text(
        item["milestone"]["title"]
            .as_str()
            .unwrap_or("No milestone"),
        Style::Muted,
    ));
    if page == 2 {
        rows.push(Line::button(
            "[ Request reviewers ]",
            Action::Metadata(page, id, "reviewers"),
        ));
    }
    rows.push(Line::text("", Style::Default));
    rows.push(Line::button(
        "[ Notifications ]",
        Action::Subscription(page, id),
    ));
    rows.push(Line::text("Development", Style::Accent));
    if page == 1 {
        rows.push(Line::button(
            "[ Create linked branch ]",
            Action::Develop(id),
        ));
    }
    for linked in array(
        item,
        if page == 1 {
            "closedByPullRequestsReferences"
        } else {
            "closingIssuesReferences"
        },
    ) {
        rows.push(Line::button(
            format!("#{} {}", number(linked, "number"), string(linked, "title")),
            Action::OpenItem(if page == 1 { 2 } else { 1 }, number(linked, "number")),
        ));
    }
    for row in &mut rows {
        row.sidebar = sidebar;
    }
    rows
}
fn timeline(workspace: &Workspace, id: u64, rows: &mut Vec<Line>, width: usize) {
    if error_or_loading(workspace, &Key::Timeline(id), rows) {
        return;
    }
    let pages = workspace
        .value(&Key::Timeline(id))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for page in pages {
        for event in page.as_array().into_iter().flatten() {
            let kind = string(event, "event");
            rows.push(Line::text(
                format!(
                    "{} · {} · {}",
                    login(event),
                    kind,
                    string(event, "created_at")
                ),
                Style::Muted,
            ));
            if kind == "commented" || kind == "reviewed" {
                body(rows, string(event, "body"), width);
            } else if kind == "labeled" || kind == "unlabeled" {
                rows.push(Line::text(string(&event["label"], "name"), Style::Warning));
            } else if kind == "assigned" || kind == "unassigned" {
                rows.push(Line::text(
                    string(&event["assignee"], "login"),
                    Style::Default,
                ));
            } else if kind == "cross-referenced" {
                let issue = &event["source"]["issue"];
                if number(issue, "number") > 0 {
                    rows.push(Line::button(
                        format!("#{} {}", number(issue, "number"), string(issue, "title")),
                        Action::OpenItem(
                            if issue.get("pull_request").is_some() {
                                2
                            } else {
                                1
                            },
                            number(issue, "number"),
                        ),
                    ));
                }
            }
            rows.push(Line::text("", Style::Default));
        }
    }
}
fn threads(workspace: &Workspace, id: u64, rows: &mut Vec<Line>, width: usize) {
    if let Some(value) = workspace.value(&Key::Threads(id)) {
        for thread in array(
            &value["data"]["repository"]["pullRequest"]["reviewThreads"],
            "nodes",
        ) {
            let resolved = thread["isResolved"].as_bool().unwrap_or(false);
            rows.push(Line::text(
                if resolved {
                    "✓ Resolved conversation"
                } else {
                    "○ Unresolved conversation"
                },
                if resolved {
                    Style::Muted
                } else {
                    Style::Warning
                },
            ));
            for comment in array(&thread["comments"], "nodes") {
                rows.push(Line::text(
                    format!(
                        "{} · {}:{}",
                        login(comment),
                        string(comment, "path"),
                        number(comment, "line")
                    ),
                    Style::Muted,
                ));
                body(rows, string(comment, "body"), width);
            }
            rows.push(Line::button(
                if resolved {
                    "[ Reopen conversation ]"
                } else {
                    "[ Resolve conversation ]"
                },
                Action::Resolve(string(thread, "id").into(), !resolved),
            ));
        }
    }
}
fn files(workspace: &Workspace, id: u64, item: &Value, rows: &mut Vec<Line>) {
    if error_or_loading(workspace, &Key::Diff(id), rows) {
        return;
    }
    let preview = super::super::preview::Preview::git(
        workspace.raw(&Key::Diff(id)).unwrap_or_default(),
        None,
    );
    let width = preview.number_width();
    for file in array(item, "files") {
        let path = string(file, "path");
        let collapsed = workspace.collapsed.contains(&(id, path.into()));
        rows.push(Line::button(
            format!(
                "{} {}  +{} −{}",
                if collapsed { "▸" } else { "▾" },
                path,
                number(file, "additions"),
                number(file, "deletions")
            ),
            Action::Collapse(id, path.into()),
        ));
        rows.push(Line::button(
            if workspace.viewed.contains(&(id, path.into())) {
                "[ ✓ Viewed ]"
            } else {
                "[ Mark viewed ]"
            },
            Action::Viewed(id, path.into()),
        ));
        if collapsed {
            continue;
        }
        for source in preview
            .rows
            .iter()
            .filter(|row| row.path.as_deref() == Some(std::path::Path::new(path)))
        {
            let mut row = Line::text(&source.text, source.style);
            row.diff = match source.marker {
                Some('+') => Some(true),
                Some('-') => Some(false),
                _ => None,
            };
            row.spans = source.spans.clone();
            if source.marker.is_some() {
                row.gutter = format!(
                    "{:>width$} {:>width$} {}",
                    source.old.map(|v| v.to_string()).unwrap_or_default(),
                    source.new.map(|v| v.to_string()).unwrap_or_default(),
                    source.marker.unwrap_or(' ')
                );
                if let Some(line) = source.new.or(source.old) {
                    row.action = Some(Action::Inline {
                        number: id,
                        path: path.into(),
                        line,
                        side: if source.new.is_some() {
                            "RIGHT"
                        } else {
                            "LEFT"
                        },
                        head: string(item, "headRefOid").into(),
                    });
                }
            }
            rows.push(row);
        }
    }
    rows.push(Line::button("[ Review changes ]", Action::Review(id)));
}
fn issue_or_pr(workspace: &Workspace, width: usize, sidebar: bool) -> Vec<Line> {
    let id = workspace.number();
    let mut rows = Vec::new();
    if id == 0 {
        rows.push(Line::text(
            if workspace.page == 1 {
                "No issues match this view"
            } else {
                "No pull requests match this view"
            },
            Style::Default,
        ));
        rows.push(Line::text("", Style::Default));
        body(
            &mut rows,
            "Your repository is connected. Change the filter with / or create a new item with n. Ctrl+l focuses these controls.",
            width,
        );
        rows.push(Line::text("", Style::Default));
        if workspace.page == 1 {
            rows.push(Line::button("[ New issue ]", Action::CreateIssue));
        } else {
            rows.push(Line::button(
                "[ New pull request ]",
                Action::CreatePullRequest,
            ));
        }
        return rows;
    }
    if error_or_loading(workspace, &Key::Item(workspace.page, id), &mut rows) {
        return rows;
    }
    let item = workspace.item().unwrap();
    body(
        &mut rows,
        &format!("{} #{}", string(item, "title"), id),
        width,
    );
    rows.push(Line::text(
        format!(
            "{} · {} · {}",
            string(item, "state"),
            login(item),
            string(item, "createdAt")
        ),
        Style::Accent,
    ));
    rows.push(Line::button(
        "[ Edit title and description ]",
        Action::Edit(workspace.page, id),
    ));
    if workspace.page == 2 {
        rows.push(Line::text(
            format!(
                "{} → {} · {} commits · {} files",
                string(item, "headRefName"),
                string(item, "baseRefName"),
                array(item, "commits").len(),
                array(item, "files").len()
            ),
            Style::Muted,
        ));
        rows.push(Line::button("[ Checkout ]", Action::Checkout(id)));
        rows.push(Line::button("[ New worktree ]", Action::Worktree(id)));
        rows.push(Line::button(
            ["Conversation", "Commits", "Checks", "Files changed"]
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    if i == workspace.pr_tab {
                        format!("[{name}]")
                    } else {
                        (*name).into()
                    }
                })
                .collect::<Vec<_>>()
                .join("  "),
            Action::PrTab((workspace.pr_tab + 1) % 4),
        ));
    }
    rows.push(Line::text("", Style::Default));
    match if workspace.page == 1 {
        0
    } else {
        workspace.pr_tab
    } {
        0 => {
            body(&mut rows, string(item, "body"), width);
            rows.push(Line::text("", Style::Default));
            timeline(workspace, id, &mut rows, width);
            if workspace.page == 2 {
                for review in array(item, "reviews") {
                    rows.push(Line::text(
                        format!("{} · {}", login(review), string(review, "state")),
                        Style::Accent,
                    ));
                    body(&mut rows, string(review, "body"), width);
                }
                threads(workspace, id, &mut rows, width);
                rows.push(Line::button("[ Review changes ]", Action::Review(id)));
                let resolved = workspace.value(&Key::Threads(id)).is_some_and(|value| {
                    array(
                        &value["data"]["repository"]["pullRequest"]["reviewThreads"],
                        "nodes",
                    )
                    .iter()
                    .all(|thread| thread["isResolved"].as_bool() == Some(true))
                });
                let ready = string(item, "state") == "OPEN"
                    && string(item, "mergeStateStatus") == "CLEAN"
                    && !matches!(
                        string(item, "reviewDecision"),
                        "CHANGES_REQUESTED" | "REVIEW_REQUIRED"
                    )
                    && !item["isDraft"].as_bool().unwrap_or(false)
                    && resolved;
                if !resolved {
                    rows.push(Line::text(
                        "Resolve review conversations before merging",
                        Style::Warning,
                    ));
                }
                rows.push(Line::text(
                    format!(
                        "Merge: {} · {}",
                        string(item, "mergeStateStatus"),
                        string(item, "reviewDecision")
                    ),
                    if ready {
                        Style::GitSignAdded
                    } else {
                        Style::Warning
                    },
                ));
                if ready {
                    rows.push(Line::button(
                        "[ Merge pull request ]",
                        Action::Merge(id, string(item, "headRefOid").into()),
                    ));
                }
            }
            rows.push(Line::button(
                "[ Write / continue comment ]",
                Action::Comment(workspace.page, id),
            ));
            if string(item, "state") != "MERGED" {
                rows.push(Line::button(
                    if string(item, "state") == "OPEN" {
                        "[ Close ]"
                    } else {
                        "[ Reopen ]"
                    },
                    Action::State(workspace.page, id, string(item, "state") == "OPEN"),
                ));
            }
        }
        1 => {
            for commit in array(item, "commits") {
                rows.push(Line::button(
                    format!(
                        "{} {}",
                        string(commit, "oid").chars().take(8).collect::<String>(),
                        string(commit, "messageHeadline")
                    ),
                    Action::Commit(string(commit, "oid").into()),
                ));
                body(&mut rows, string(commit, "messageBody"), width);
                rows.push(Line::text(string(commit, "committedDate"), Style::Muted));
            }
            if let Some(sha) = &workspace.selected_commit {
                let key = Key::Commit(sha.clone());
                if !error_or_loading(workspace, &key, &mut rows) {
                    let preview = super::super::preview::Preview::git(
                        workspace.raw(&key).unwrap_or_default(),
                        None,
                    );
                    for source in preview.rows {
                        let mut row = Line::text(source.text, source.style);
                        row.diff = match source.marker {
                            Some('+') => Some(true),
                            Some('-') => Some(false),
                            _ => None,
                        };
                        row.spans = source.spans;
                        if let Some(marker) = source.marker {
                            row.gutter = format!(
                                "{:>4} {:>4} {marker}",
                                source.old.map(|n| n.to_string()).unwrap_or_default(),
                                source.new.map(|n| n.to_string()).unwrap_or_default()
                            );
                        }
                        rows.push(row);
                    }
                }
            }
        }
        2 => {
            for check in array(item, "statusCheckRollup") {
                let name = check["name"]
                    .as_str()
                    .or_else(|| check["context"].as_str())
                    .unwrap_or("Check");
                let status = check["conclusion"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| check["state"].as_str())
                    .unwrap_or(string(check, "status"));
                rows.push(Line::text(
                    format!("{status}  {name}"),
                    if matches!(status, "SUCCESS" | "NEUTRAL" | "SKIPPED") {
                        Style::GitSignAdded
                    } else {
                        Style::Warning
                    },
                ));
                let url = check["detailsUrl"]
                    .as_str()
                    .or_else(|| check["targetUrl"].as_str())
                    .unwrap_or("");
                if let Some(id) = url
                    .split("/actions/runs/")
                    .nth(1)
                    .and_then(|suffix| suffix.split('/').next())
                    .and_then(|id| id.parse().ok())
                {
                    rows.push(Line::button("[ Jobs and logs ]", Action::Run(id)));
                }
            }
        }
        _ => files(workspace, id, item, &mut rows),
    }
    rows.extend(metadata(workspace, item, sidebar));
    rows
}
fn status(value: &Value) -> (&'static str, Style) {
    match string(value, "conclusion") {
        "success" => ("✓", Style::GitSignAdded),
        "failure" | "timed_out" | "action_required" => ("×", Style::Error),
        "skipped" | "neutral" => ("−", Style::Muted),
        "cancelled" => ("⊘", Style::Muted),
        _ if string(value, "status") == "in_progress" => ("●", Style::Warning),
        _ => ("○", Style::Muted),
    }
}
fn log_ready(workspace: &Workspace, job: &Value, rows: &mut Vec<Line>) -> bool {
    if string(job, "status") != "completed" {
        rows.push(Line::text(
            "      Job running · GitHub has not published its downloadable log yet",
            Style::Muted,
        ));
        rows.push(Line::text(
            "      Status refreshes automatically; logs load when the job completes",
            Style::Muted,
        ));
        return false;
    }
    match workspace.cache.get(&Key::Log(number(job, "databaseId"))) {
        Some(Ok(_)) => true,
        Some(Err(error)) => {
            rows.push(Line::text(format!("      {error}"), Style::Error));
            rows.push(Line::text("      r retries loading the log", Style::Muted));
            false
        }
        None => {
            rows.push(Line::text("      Loading job log…", Style::Muted));
            false
        }
    }
}
fn log_line(rows: &mut Vec<Line>, text: &str) {
    let text = logs::display(text);
    let mut row = Line::text(text, Style::MarkdownCode(None));
    row.gutter = "    │ ".into();
    rows.push(row);
}
fn workflow_lines(workspace: &Workspace, width: usize) -> Vec<Line> {
    let mut rows = Vec::new();
    let workflow = workspace.workflow_id();
    if workspace.last_list == 0 || workspace.run_id.is_none() {
        if workflow == 0 {
            rows.push(Line::text("Workflows", Style::Accent));
            body(
                &mut rows,
                "Select a workflow to inspect its definition and dispatch inputs. Run history is a separate pane below it.",
                width,
            );
            return rows;
        }
        let key = Key::Workflow(workflow, String::new());
        if error_or_loading(workspace, &key, &mut rows) {
            return rows;
        }
        let yaml = workspace.raw(&key).unwrap_or_default();
        match workflow::Definition::parse(yaml) {
            Ok(definition) => {
                if definition.dispatch {
                    rows.push(Line::button(
                        "[ Run workflow… ]",
                        Action::Dispatch(workflow),
                    ));
                }
                rows.push(Line::text("Declared job dependencies", Style::Accent));
                for (job, needs) in definition.dependencies {
                    rows.push(Line::text(
                        format!(
                            "{job} ← {}",
                            if needs.is_empty() {
                                "start".into()
                            } else {
                                needs.join(", ")
                            }
                        ),
                        Style::Muted,
                    ));
                }
            }
            Err(error) => rows.push(Line::text(error, Style::Warning)),
        }
        rows.push(Line::text("Workflow definition", Style::Accent));
        let preview = PickerTextPreview::new(
            yaml.lines().map(Into::into).collect(),
            0,
            None,
            Some(crate::language::syntax::SyntaxLanguage::Yaml),
        );
        for (i, line) in preview.lines().iter().enumerate() {
            let mut row = Line::text(line, Style::Default);
            row.spans = preview.line_spans(i).to_vec();
            row.gutter = format!("{:>3} │ ", i + 1);
            rows.push(row);
        }
        return rows;
    }
    let id = workspace.run_id.unwrap();
    let key = Key::Run(id, workspace.attempt);
    if error_or_loading(workspace, &key, &mut rows) {
        return rows;
    }
    let run = workspace.value(&key).unwrap();
    rows.push(Line::text(string(run, "displayTitle"), Style::Accent));
    rows.push(Line::text(
        format!(
            "{} · {} · {} · {}",
            string(run, "workflowName"),
            string(run, "headBranch"),
            string(run, "status"),
            string(run, "conclusion")
        ),
        Style::Muted,
    ));
    if let Some(yaml) = workspace.raw(&Key::Workflow(
        number(run, "workflowDatabaseId"),
        string(run, "headSha").into(),
    )) && let Ok(definition) = workflow::Definition::parse(yaml)
    {
        rows.push(Line::text(
            "Job dependencies at this run's commit",
            Style::Accent,
        ));
        for (job, needs) in definition.dependencies {
            rows.push(Line::text(
                format!(
                    "{job} ← {}",
                    if needs.is_empty() {
                        "start".into()
                    } else {
                        needs.join(", ")
                    }
                ),
                Style::Muted,
            ));
        }
    }
    rows.push(Line::text("", Style::Default));
    if string(run, "status") == "completed" {
        rows.push(Line::button("[ Re-run jobs ]", Action::Rerun(id)));
    }
    if string(run, "status") != "completed" {
        rows.push(Line::button("[ Cancel run ]", Action::Cancel(id)));
    }
    for attempt in 1..=number(run, "attempt")
        .max(u64::from(workspace.attempt))
        .min(100)
    {
        rows.push(Line::button(
            format!("[ Attempt {attempt} ]"),
            Action::Attempt(attempt as u32),
        ));
    }
    rows.push(Line::text("", Style::Default));
    rows.push(Line::text(
        "JOBS  ·  Enter expands  ·  Tab jumps between controls",
        Style::Muted,
    ));
    for job in array(run, "jobs") {
        let id = number(job, "databaseId");
        let expanded = workspace.expanded_jobs.contains(&id);
        rows.push(Line::text("", Style::Default));
        let mut job_row = Line::button(
            format!(
                "{} {} · {}",
                if expanded { "▾" } else { "▸" },
                string(job, "name"),
                job["conclusion"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(string(job, "status"))
            ),
            Action::Job(id),
        );
        let (symbol, style) = status(job);
        job_row.gutter = format!("{symbol} ");
        job_row.gutter_style = style;
        rows.push(job_row);
        if !expanded {
            continue;
        }
        rows.push(Line::button(
            if workspace.full_logs.contains(&id) {
                "  ▾ Full job log · Enter returns to steps"
            } else {
                "  ▸ Full job log"
            },
            Action::FullLog(id),
        ));
        if workspace.full_logs.contains(&id) {
            if log_ready(workspace, job, &mut rows) {
                for line in workspace.raw(&Key::Log(id)).unwrap_or_default().lines() {
                    log_line(&mut rows, logs::unpack(line).1);
                }
            }
            continue;
        }
        for step in array(job, "steps") {
            let number = number(step, "number");
            let expanded = workspace.expanded_steps.contains(&(id, number));
            let mut step_row = Line::button(
                format!(
                    "{} {} · {}",
                    if expanded { "▾" } else { "▸" },
                    string(step, "name"),
                    step["conclusion"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(string(step, "status"))
                ),
                Action::Step(id, number),
            );
            let (symbol, style) = status(step);
            step_row.gutter = format!("  {symbol} ");
            step_row.gutter_style = style;
            rows.push(step_row);
            if expanded {
                if string(step, "conclusion") == "skipped" {
                    rows.push(Line::text("      Skipped · no commands ran", Style::Muted));
                    continue;
                }
                if !log_ready(workspace, job, &mut rows) {
                    continue;
                }
                let (lines, approximate) =
                    logs::step_lines(workspace.raw(&Key::Log(id)).unwrap_or_default(), step);
                if approximate && !lines.is_empty() {
                    rows.push(Line::text(
                        "      Logs matched by step timestamps · Full job log is also available",
                        Style::Muted,
                    ));
                }
                if lines.is_empty() {
                    rows.push(Line::text(
                        "      Step boundaries unavailable · expand Full job log above",
                        Style::Muted,
                    ));
                }
                for line in lines {
                    log_line(&mut rows, line);
                }
            }
        }
    }
    rows
}

pub(in crate::git::screen) fn frame(
    app: &mut super::super::GitScreen,
    width: u16,
    height: u16,
) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    if width < 64 || height < 20 {
        frame.grid.put_text_clipped(
            0,
            0,
            "Dusty Git needs at least 64 × 20",
            Style::Warning,
            width,
        );
        return frame;
    }
    let workspace = &mut app.github;
    super::super::render::navigation(
        &mut frame,
        workspace.page,
        width,
        &format!("{}/{}", workspace.host, workspace.repo),
    );
    let left = (if workspace.page == 3 {
        width / 3
    } else {
        width * 2 / 5
    })
    .clamp(24, 64)
    .min(width - 36);
    let body_height = height - 5;
    let workflow_height =
        (workspace.workflows().len() as u16 + 3).clamp(4, (body_height / 3).max(4));
    let right = width - 1;
    let sidebar = workspace.page < 3 && width - left >= 90;
    let meta_width = if sidebar { 26 } else { 0 };
    let content_right = right - meta_width;
    let title = if workspace.page == 1 {
        "Issues"
    } else if workspace.page == 2 {
        "Pull requests"
    } else {
        "Workflows"
    };
    let lists = if workspace.page == 3 { 2 } else { 1 };
    for pane in 0..lists {
        let (y, h) = if lists == 1 {
            (2, body_height)
        } else if pane == 0 {
            (2, workflow_height)
        } else {
            (2 + workflow_height, body_height - workflow_height)
        };
        section(
            &mut frame.grid,
            0,
            y,
            left,
            h,
            if pane == 0 { title } else { "Run history" },
            workspace.focus == pane,
        );
        let (items, selected): (Vec<(String, String, &'static str, Style)>, usize) = if workspace
            .page
            < 3
        {
            (
                workspace
                    .items()
                    .iter()
                    .map(|item| {
                        (
                            format!("#{} {}", number(item, "number"), string(item, "title")),
                            format!("{} · {}", login(item), string(item, "state").to_lowercase()),
                            if workspace.page == 1 { "○" } else { "⑂" },
                            Style::Accent,
                        )
                    })
                    .collect(),
                workspace.selected[workspace.page - 1],
            )
        } else if pane == 0 {
            (
                std::iter::once(("All workflows".into(), String::new(), "≡", Style::Muted))
                    .chain(workspace.workflows().iter().map(|workflow| {
                        (
                            string(workflow, "name").into(),
                            String::new(),
                            "◇",
                            Style::Muted,
                        )
                    }))
                    .collect(),
                workspace.workflow_selected,
            )
        } else {
            (
                workspace
                    .runs()
                    .iter()
                    .map(|run| {
                        let (symbol, style) = status(run);
                        (
                            string(run, "displayTitle").into(),
                            format!(
                                "{} · {} · #{}",
                                string(run, "workflowName"),
                                string(run, "headBranch"),
                                number(run, "databaseId")
                            ),
                            symbol,
                            style,
                        )
                    })
                    .collect(),
                workspace.run_selected,
            )
        };
        let query_height = u16::from(workspace.page < 3) * 2;
        if workspace.page < 3 {
            let at = workspace.page - 1;
            let query = if workspace.queries[at].is_empty() {
                format!(
                    "{} · {} items · / filter",
                    workspace.states[at],
                    items.len()
                )
            } else {
                format!("{} · {}", workspace.states[at], workspace.queries[at])
            };
            frame.grid.put_text_clipped(
                2,
                y + 1,
                &ellipsize(&query, usize::from(left - 4)),
                Style::GitSurface(Some(Highlight::COMMENT)),
                left - 2,
            );
        }
        let row_height = if workspace.page == 3 && pane == 0 {
            1
        } else {
            2
        };
        let visible = (h.saturating_sub(2 + query_height) / row_height).max(1) as usize;
        let offset = selected.saturating_sub(visible.saturating_sub(1));
        for (i, (title, subtitle, symbol, status_style)) in
            items.iter().enumerate().skip(offset).take(visible)
        {
            let row = y + 1 + query_height + (i - offset) as u16 * row_height;
            if row >= y + h - 1 {
                break;
            }
            let selected = i == selected;
            let focused = workspace.focus == pane && selected;
            if selected {
                for yy in row..(row + row_height).min(y + h - 1) {
                    for x in 1..left - 1 {
                        frame.grid.set_cell(
                            x,
                            yy,
                            " ",
                            if workspace.focus == pane {
                                Style::GitSelected(None)
                            } else {
                                Style::GitInactiveSelection(None)
                            },
                        );
                    }
                }
            }
            if focused {
                frame.cursor = Some((2, row, SetCursorStyle::SteadyBlock));
            }
            frame
                .grid
                .put_text_clipped(2, row, symbol, *status_style, left - 1);
            frame.grid.put_text_clipped(
                4,
                row,
                &ellipsize(title, usize::from(left - 6)),
                if selected {
                    if workspace.focus == pane {
                        Style::GitSelected(None)
                    } else {
                        Style::GitInactiveSelection(None)
                    }
                } else {
                    Style::GitSurface(None)
                },
                left - 2,
            );
            if row_height == 2 && row + 1 < y + h - 1 {
                frame.grid.put_text_clipped(
                    4,
                    row + 1,
                    &ellipsize(subtitle, usize::from(left - 6)),
                    if selected {
                        if workspace.focus == pane {
                            Style::GitSelected(Some(Highlight::COMMENT))
                        } else {
                            Style::GitInactiveSelection(Some(Highlight::COMMENT))
                        }
                    } else {
                        Style::GitSurface(Some(Highlight::COMMENT))
                    },
                    left - 2,
                );
            }
        }
        if items.is_empty() {
            frame.grid.put_text_clipped(
                2,
                y + 1 + query_height,
                "No entries",
                Style::Muted,
                left - 1,
            );
            if h > 5 {
                frame.grid.put_text_clipped(
                    2,
                    y + 3 + query_height,
                    if workspace.page == 3 {
                        "r refresh"
                    } else {
                        "n create · / filter"
                    },
                    Style::GitSurface(Some(Highlight::COMMENT)),
                    left - 1,
                );
            }
            if workspace.focus == pane {
                frame.cursor = Some((2, y + 1 + query_height, SetCursorStyle::SteadyBlock));
            }
        }
        viewport(
            &mut frame,
            left - 2,
            y + h - 1,
            offset,
            visible,
            items.len(),
        );
    }
    let detail_title = if workspace.page == 3 {
        if workspace.last_list == 1
            && let Some(id) = workspace.run_id
        {
            let name = workspace
                .value(&Key::Run(id, workspace.attempt))
                .map(|run| string(run, "workflowName"))
                .unwrap_or("Run");
            format!("{name} · #{id}")
        } else {
            "Workflows".into()
        }
    } else if workspace.number() > 0 {
        format!(
            "{} #{} · {}",
            if workspace.page == 1 { "Issue" } else { "PR" },
            workspace.number(),
            if workspace.page == 1 {
                "Conversation"
            } else {
                ["Conversation", "Commits", "Checks", "Files changed"][workspace.pr_tab]
            }
        )
    } else {
        "Overview".into()
    };
    super::super::render::border(
        &mut frame.grid,
        left,
        2,
        width - left,
        body_height,
        &detail_title,
        workspace.focus == 2,
    );
    let mut rows = Vec::new();
    if workspace.repo.is_empty() {
        error_or_loading(workspace, &Key::Repository, &mut rows);
    } else if workspace.page < 3 {
        if !error_or_loading(workspace, &Key::List(workspace.page), &mut rows) {
            rows = issue_or_pr(workspace, (content_right - left - 4) as usize, sidebar);
        }
    } else {
        rows = workflow_lines(workspace, (content_right - left - 4) as usize);
    }
    if workspace.focus == 2
        && let Some(action) = workspace
            .detail_rows
            .get(workspace.detail_cursor)
            .and_then(|row| row.action.as_ref())
        && let Some(index) = rows
            .iter()
            .position(|row| row.action.as_ref() == Some(action))
    {
        workspace.detail_cursor = index;
    }
    workspace.detail_cursor = workspace.detail_cursor.min(rows.len().saturating_sub(1));
    let visible = body_height.saturating_sub(2) as usize;
    let main_count = rows.iter().take_while(|row| !row.sidebar).count();
    let in_sidebar = workspace.detail_cursor >= main_count && sidebar;
    if !in_sidebar {
        let margin = (visible / 3).min(6);
        if workspace.detail_cursor < workspace.detail_scroll {
            workspace.detail_scroll = workspace.detail_cursor;
        } else if workspace.detail_cursor + margin >= workspace.detail_scroll + visible {
            workspace.detail_scroll =
                (workspace.detail_cursor + margin + 1).saturating_sub(visible);
        }
        workspace.detail_scroll = workspace
            .detail_scroll
            .min(main_count.saturating_sub(visible));
    }
    if sidebar {
        for y in 3..body_height + 1 {
            frame.grid.set_cell(content_right, y, "│", Style::Border);
        }
    }
    for (i, row) in rows.iter().enumerate() {
        let (x, right, relative) = if row.sidebar {
            (content_right + 1, width - 1, i - main_count)
        } else {
            if i < workspace.detail_scroll {
                continue;
            }
            (left + 2, content_right, i - workspace.detail_scroll)
        };
        if relative >= visible {
            continue;
        }
        let y = 3 + relative as u16;
        let selected = workspace.focus == 2 && i == workspace.detail_cursor;
        let style = if selected {
            Style::GitSelected(None)
        } else if i == 0 {
            Style::GitHeading
        } else if let Some(added) = row.diff {
            Style::GitDiff {
                added,
                syntax: None,
            }
        } else {
            row.style
        };
        if selected
            || i == 0
            || matches!(
                style,
                Style::MarkdownCode(_) | Style::GitDiff { .. } | Style::GitHeading
            )
        {
            for at in x..right {
                frame.grid.set_cell(at, y, " ", style);
            }
        }
        if selected {
            frame.cursor = Some((x, y, SetCursorStyle::SteadyBlock));
        }
        let mut x = frame
            .grid
            .put_text_clipped(x, y, &row.gutter, row.gutter_style, right);
        let mut column = 0;
        for (at, g) in row.text.grapheme_indices(true) {
            let size = g.width().max(1);
            if column >= workspace.horizontal || row.sidebar {
                let style = row
                    .spans
                    .iter()
                    .find(|(range, _)| range.contains(&at))
                    .map(|(_, role)| {
                        if selected {
                            Style::GitSelected(Some(*role))
                        } else if let Some(added) = row.diff {
                            Style::GitDiff {
                                added,
                                syntax: Some(*role),
                            }
                        } else {
                            Style::Syntax(*role)
                        }
                    })
                    .unwrap_or(style);
                let next = frame.grid.put_text_clipped(x, y, g, style, right);
                if next == x {
                    frame.grid.set_cell(right - 1, y, "›", Style::Muted);
                    break;
                }
                x = next;
            }
            column += size;
        }
    }
    if workspace.focus == 2 && frame.cursor.is_none() {
        frame.cursor = Some((left + 2, 3, SetCursorStyle::SteadyBlock));
    }
    viewport(
        &mut frame,
        width - 2,
        body_height + 1,
        workspace.detail_scroll,
        visible,
        main_count,
    );
    workspace.detail_rows = rows;
    let notice = if app.notice.is_empty() {
        &workspace.notice
    } else {
        &app.notice
    };
    frame.grid.put_text_clipped(
        1,
        height - 3,
        if workspace.writing() {
            "Sending confirmed operation to GitHub…"
        } else {
            notice
        },
        Style::Warning,
        width - 1,
    );
    footer(
        &mut frame,
        width,
        height,
        if workspace.page == 3 && workspace.focus == 2 {
            &[
                ("Tab", "next"),
                ("Enter", "open"),
                ("PgUp/Dn", "scroll"),
                ("h/l", "pan"),
                ("[ ]", "pages"),
                ("Ctrl+h", "list"),
            ]
        } else if workspace.page == 3 {
            &[
                ("Enter", "open"),
                ("Ctrl+j/k", "lists"),
                ("[ ]", "pages"),
                ("Tab", "controls"),
            ]
        } else if workspace.page == 2 && workspace.focus == 2 {
            &[
                ("H/L", "tabs"),
                ("Tab", "controls"),
                ("Enter", "open"),
                ("h/l", "pan"),
                ("[ ]", "pages"),
            ]
        } else {
            &[
                ("n", "create"),
                ("/", "filter"),
                ("Enter", "open"),
                ("[ ]", "pages"),
                ("Tab", "controls"),
            ]
        },
        false,
    );
    if let Some(form) = &app.form {
        super::super::render::render_form(&mut frame, form, width, height);
    }
    frame
}
