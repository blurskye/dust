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
    sidebar: bool,
}
impl Line {
    fn text(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            action: None,
            spans: Vec::new(),
            gutter: String::new(),
            sidebar: false,
        }
    }
    fn button(text: impl Into<String>, action: Action) -> Self {
        let mut line = Self::text(text, Style::Accent);
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
        rows.push(Line::text("Select an item · / search/filter", Style::Muted));
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
    rows.push(Line::button("[ Re-run jobs ]", Action::Rerun(id)));
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
    for job in array(run, "jobs") {
        let id = number(job, "databaseId");
        let expanded = workspace.expanded_jobs.contains(&id);
        rows.push(Line::button(
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
        ));
        if !expanded {
            continue;
        }
        for step in array(job, "steps") {
            let number = number(step, "number");
            let expanded = workspace.expanded_steps.contains(&(id, number));
            rows.push(Line::button(
                format!(
                    "  {} {} · {}",
                    if expanded { "▾" } else { "▸" },
                    string(step, "name"),
                    step["conclusion"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(string(step, "status"))
                ),
                Action::Step(id, number),
            ));
            if expanded {
                if error_or_loading(workspace, &Key::Log(id), &mut rows) {
                    continue;
                }
                let mut found = false;
                for line in workspace.raw(&Key::Log(id)).unwrap_or_default().lines() {
                    let mut parts = line.splitn(3, '\t');
                    let _job = parts.next();
                    let name = parts.next().unwrap_or("");
                    let text = parts.next().unwrap_or(line);
                    if name == string(step, "name") {
                        body(&mut rows, text, width);
                        found = true;
                    }
                }
                if !found {
                    rows.push(Line::text(
                        "No step log available yet. GitHub publishes job logs after completion.",
                        Style::Muted,
                    ));
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
    let left = (width / 4).clamp(24, 40);
    let body_height = height - 4;
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
            (1, body_height)
        } else if pane == 0 {
            (1, body_height / 3)
        } else {
            (1 + body_height / 3, body_height - body_height / 3)
        };
        super::super::render::border(
            &mut frame.grid,
            0,
            y,
            left,
            h,
            if pane == 0 { title } else { "Run history" },
            workspace.focus == pane,
        );
        let (items, selected): (Vec<String>, usize) = if workspace.page < 3 {
            (
                workspace
                    .items()
                    .iter()
                    .map(|item| format!("#{} {}", number(item, "number"), string(item, "title")))
                    .collect(),
                workspace.selected[workspace.page - 1],
            )
        } else if pane == 0 {
            (
                std::iter::once("All workflows".into())
                    .chain(
                        workspace
                            .workflows()
                            .iter()
                            .map(|workflow| string(workflow, "name").into()),
                    )
                    .collect(),
                workspace.workflow_selected,
            )
        } else {
            (
                workspace
                    .runs()
                    .iter()
                    .map(|run| {
                        format!(
                            "{} {} · {}",
                            if string(run, "conclusion") == "success" {
                                "✓"
                            } else if string(run, "status") == "completed" {
                                "✗"
                            } else {
                                "◌"
                            },
                            string(run, "displayTitle"),
                            string(run, "headBranch")
                        )
                    })
                    .collect(),
                workspace.run_selected,
            )
        };
        let visible = h.saturating_sub(2) as usize;
        let offset = selected.saturating_sub(visible.saturating_sub(1));
        for (i, text) in items.iter().enumerate().skip(offset).take(visible) {
            let row = y + 1 + (i - offset) as u16;
            let focused = workspace.focus == pane && i == selected;
            if focused {
                for x in 1..left - 1 {
                    frame.grid.set_cell(x, row, " ", Style::Selection);
                }
                frame.cursor = Some((2, row, SetCursorStyle::SteadyBlock));
            }
            frame.grid.put_text_clipped(
                2,
                row,
                text,
                if focused {
                    Style::Selection
                } else {
                    Style::Default
                },
                left - 1,
            );
        }
        if items.is_empty() {
            frame
                .grid
                .put_text_clipped(2, y + 1, "No entries", Style::Muted, left - 1);
        }
    }
    super::super::render::border(
        &mut frame.grid,
        left,
        1,
        width - left,
        body_height,
        "Details · Tab controls · Enter activate",
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
    workspace.detail_cursor = workspace.detail_cursor.min(rows.len().saturating_sub(1));
    let visible = body_height.saturating_sub(2) as usize;
    let main_count = rows.iter().take_while(|row| !row.sidebar).count();
    let in_sidebar = workspace.detail_cursor >= main_count && sidebar;
    if !in_sidebar {
        workspace.detail_scroll = workspace
            .detail_cursor
            .saturating_sub(visible.saturating_sub(1));
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
        let y = 2 + relative as u16;
        let selected = workspace.focus == 2 && i == workspace.detail_cursor;
        let style = if selected {
            Style::Selection
        } else {
            row.style
        };
        if selected {
            for at in x..right {
                frame.grid.set_cell(at, y, " ", Style::Selection);
            }
            frame.cursor = Some((x, y, SetCursorStyle::SteadyBlock));
        }
        let mut x = frame.grid.put_text_clipped(
            x,
            y,
            &row.gutter,
            if selected {
                Style::Selection
            } else {
                Style::Muted
            },
            right,
        );
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
                            Style::SelectionSyntax(*role)
                        } else {
                            Style::Syntax(*role)
                        }
                    })
                    .unwrap_or(style);
                let next = frame.grid.put_text_clipped(x, y, g, style, right);
                if next == x {
                    break;
                }
                x = next;
            }
            column += size;
        }
    }
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
    frame.grid.put_text_clipped(1,height-2,"[ / ] pages · Ctrl+h/l focus · Tab controls · Enter activate · / filter · r refresh · q close",Style::Muted,width-1);
    if let Some(form) = &app.form {
        super::super::render::render_form(&mut frame, form, width, height);
    }
    frame
}
