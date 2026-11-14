//! Notifications (DESIGN.md §3.35, F-NOTIF; GitLab's To-dos): the forge's inbox of threads in
//! sections `All │ Mentions │ Review │ CI`, a reader card for the selected thread, `Enter` to
//! open its pull request, issue or run, `m` / `M` to mark read and `u` to unsubscribe. The
//! list is read the first time the page is painted, never while `<`/`>` pass over it, so a
//! visit costs one request.

use super::super::GitScreen;
use super::super::shell::chrome::{self, Span, bold, st};
use super::super::shell::glyphs::{Glyph, glyph};
use super::super::shell::layout::{self, Mode, Rect};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::{ItemKind, Notification, NotifyReason, NotifyTarget};
use crate::forge::records;
use crate::forge::request::{NotifyFilter, Page};
use crate::ui::{CellGrid, Frame, GitChip, GitRole, GitSurface};
use std::collections::HashSet;

pub(in crate::git::screen) const PAGE: usize = 4;
const SECTIONS: [&str; 4] = ["All", "Mentions", "Review", "CI"];
const PANE: GitSurface = GitSurface::Pane;

#[derive(Default)]
pub(super) struct Inbox {
    section: usize,
    selected: usize,
    /// The page has been painted once: its list may be read.
    wanted: bool,
    /// Threads marked read or unsubscribed here, ahead of the forge's next list.
    read: HashSet<String>,
    gone: HashSet<String>,
}

fn in_section(section: usize, notification: &Notification) -> bool {
    match section {
        1 => notification.reason == NotifyReason::Mentioned,
        2 => notification.reason == NotifyReason::ReviewRequested,
        3 => notification.reason == NotifyReason::CiActivity || matches!(notification.target, NotifyTarget::Run(_)),
        _ => true,
    }
}

fn reason_word(workspace: &Workspace, reason: &NotifyReason) -> String {
    match reason {
        NotifyReason::Mentioned => "Mention".into(),
        NotifyReason::ReviewRequested => workspace.requested_words().0.into(),
        NotifyReason::Assigned => "Assigned".into(),
        NotifyReason::Author => "Author".into(),
        NotifyReason::Comment => "Comment".into(),
        NotifyReason::StateChange => "State change".into(),
        NotifyReason::CiActivity => "CI activity".into(),
        NotifyReason::Subscribed => "Subscribed".into(),
        NotifyReason::Other(other) => {
            let mut word = other.replace('_', " ");
            if let Some(first) = word.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            word
        }
    }
}

impl Workspace {
    /// The forge's threads, as this session last saw them (marks applied).
    fn threads(&self) -> Vec<Notification> {
        let Some(value) = self.value(&Key::Notifications) else {
            return Vec::new();
        };
        let items = value.get("Notifications").map_or(Value::Null, |page| page["items"].clone());
        let mut threads: Vec<Notification> = serde_json::from_value(items).unwrap_or_default();
        threads.retain(|thread| !self.notify.gone.contains(&thread.id.0));
        for thread in &mut threads {
            if self.notify.read.contains(&thread.id.0) {
                thread.unread = false;
            }
        }
        threads
    }

    fn shown_threads(&self) -> Vec<Notification> {
        self.threads()
            .into_iter()
            .filter(|thread| in_section(self.notify.section, thread))
            .collect()
    }

    /// Unread threads, for the top tab; `None` until the page has been read once.
    pub(in crate::git::screen) fn unread_notifications(&self) -> Option<usize> {
        self.value(&Key::Notifications)?;
        Some(self.threads().iter().filter(|thread| thread.unread).count())
    }

    /// Whether the forge offers notifications to this session (a sign-in, and a forge with an
    /// inbox); the tab is skipped otherwise.
    pub(in crate::git::screen) fn notifications_available(&self) -> bool {
        self.capabilities().notifications.allowed()
    }

    pub(super) fn notification_reads(&mut self, refresh: bool) {
        if self.notify.wanted {
            let read = self.read(Read::Notifications {
                filter: NotifyFilter::default(),
                page: Page { number: 1, per_page: 50 },
            });
            self.enqueue(Key::Notifications, read, refresh);
        }
    }

    fn selected_thread(&self) -> Option<Notification> {
        let threads = self.shown_threads();
        let at = self.notify.selected.min(threads.len().saturating_sub(1));
        threads.into_iter().nth(at)
    }

    /// Keys on the Notifications page.
    pub(super) fn notification_key(&mut self, key: &KeyEvent) -> Result<Option<Form>, String> {
        let count = self.shown_threads().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.notify.selected = (self.notify.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.notify.selected = self.notify.selected.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => self.notify.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.notify.selected = count.saturating_sub(1),
            KeyCode::Char('[' | ']') => {
                let forward = key.code == KeyCode::Char(']');
                self.notify.section = (self.notify.section + if forward { 1 } else { SECTIONS.len() - 1 }) % SECTIONS.len();
                self.notify.selected = 0;
            }
            KeyCode::Char('r' | 'R') => {
                self.notice = "Refreshing notifications…".into();
                self.notice_error = false;
                self.notification_reads(true);
            }
            KeyCode::Char('m') => {
                let thread = self.selected_thread().ok_or("No notification selected")?;
                self.mark(Write::MarkRead(thread.id.clone()))?;
                self.notify.read.insert(thread.id.0.clone());
                self.notice = format!("✓ Marked read · {} stays in the inbox", target_words(&thread));
            }
            KeyCode::Char('M') => {
                let threads = self.threads();
                self.mark(Write::MarkAllRead)?;
                self.notify.read.extend(threads.into_iter().map(|thread| thread.id.0));
                self.notice = "✓ Marked every notification read".into();
            }
            KeyCode::Char('u') => {
                let thread = self.selected_thread().ok_or("No notification selected")?;
                self.mark(Write::Unsubscribe(thread.id.clone()))?;
                self.notify.gone.insert(thread.id.0.clone());
                self.notify.selected = self.notify.selected.min(count.saturating_sub(2));
                self.notice = format!("✓ Unsubscribed · {} stops notifying you", target_words(&thread));
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                let thread = self.selected_thread().ok_or("No notification selected")?;
                return self.open_thread(&thread);
            }
            _ => {}
        }
        Ok(None)
    }

    fn mark(&mut self, write: Write) -> Result<(), String> {
        if let Some(reason) = self.capabilities().notifications.reason() {
            return Err(reason.into());
        }
        self.enqueue(Key::Write, Op::Write(self.target(), write), false);
        self.notice_error = false;
        Ok(())
    }

    /// `Enter`: the thread's pull request or issue in its own tab (this repository), its run in
    /// Actions, or the browser for anything else.
    fn open_thread(&mut self, thread: &Notification) -> Result<Option<Form>, String> {
        let here = thread.repo.is_empty() || thread.repo.eq_ignore_ascii_case(&self.repo);
        match &thread.target {
            NotifyTarget::Item { kind, number } if here => {
                let page = if *kind == ItemKind::PullRequest { 2 } else { 1 };
                self.notify.read.insert(thread.id.0.clone());
                self.page(page);
                self.open_link(page, *number);
                Ok(None)
            }
            NotifyTarget::Item { kind, number } => {
                let page = if *kind == ItemKind::PullRequest { 2 } else { 1 };
                self.activate(Action::OpenRemote(inbox::Target {
                    host: self.host.clone(),
                    repo: thread.repo.clone(),
                    page,
                    number: *number,
                }))
            }
            NotifyTarget::Run(id) if here => {
                self.notify.read.insert(thread.id.0.clone());
                self.page(3);
                // A check suite names no run: Actions opens on the newest runs instead.
                match id.0.rsplit('/').next().and_then(|tail| tail.parse::<u64>().ok()) {
                    Some(run) => self.activate(Action::Run(run)),
                    None => Ok(None),
                }
            }
            _ => Err("This notification has no target to open here".into()),
        }
    }

    /// The receipt for a finished notification write.
    pub(super) fn notification_written(&mut self, write: &Write) -> Option<String> {
        match write {
            Write::MarkRead(_) | Write::MarkAllRead | Write::Unsubscribe(_) => {
                // Keep the local marks: the forge's list catches up on the next read.
                Some(match write {
                    Write::MarkRead(id) => match self.threads().iter().find(|thread| thread.id == *id) {
                        Some(thread) => format!("✓ Marked read · {} stays in the inbox", target_words(thread)),
                        None => "✓ Marked read".into(),
                    },
                    Write::MarkAllRead => "✓ Marked every notification read".into(),
                    _ => "✓ Unsubscribed".into(),
                })
            }
            _ => None,
        }
    }
}

/// `orbit #12`, `orbit run 91`: what a thread is about, short.
fn target_words(thread: &Notification) -> String {
    let repo = thread.repo.rsplit('/').next().unwrap_or(&thread.repo);
    match &thread.target {
        NotifyTarget::Item { number, .. } => format!("{repo} #{number}"),
        NotifyTarget::Run(id) if id.0.is_empty() => format!("{repo} run"),
        NotifyTarget::Run(id) => format!("{repo} run {}", id.0),
        NotifyTarget::Release(tag) => format!("{repo} {tag}"),
        NotifyTarget::Other(other) => format!("{repo} {other}"),
    }
}

fn target_glyph(thread: &Notification, mode: ExplorerIconMode) -> &'static str {
    match &thread.target {
        NotifyTarget::Item { kind: ItemKind::PullRequest, .. } => glyph(Glyph::PullRequest, mode),
        NotifyTarget::Item { .. } => glyph(Glyph::Issue, mode),
        NotifyTarget::Run(_) => glyph(Glyph::Actions, mode),
        _ => glyph(Glyph::Notification, mode),
    }
}

fn put(grid: &mut CellGrid, x: u16, y: u16, spans: &[Span], right: u16) -> u16 {
    chrome::put_spans(grid, x, y, spans, right)
}

/// The Notifications page: top bar, list, reader card and footer.
pub(in crate::git::screen) fn frame(app: &mut GitScreen, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    let mode = layout::mode(width, height);
    if mode == Mode::TooSmall {
        let text = format!("Dusty Git needs {}×{}", layout::MIN_WIDTH, layout::MIN_HEIGHT);
        frame.grid.put_text_clipped(width.saturating_sub(chrome::width(&text)) / 2, height / 2, &text, bold(GitRole::Modified, GitSurface::Canvas), width);
        return frame;
    }
    if !app.github.notify.wanted {
        app.github.notify.wanted = true;
        app.github.ensure(false);
    }
    chrome::fill(&mut frame.grid, Rect::new(0, 0, width, height), st(GitRole::Text, GitSurface::Canvas));
    super::super::shell::top_bar(app, &mut frame.grid, width);
    let icons = app.icon_mode;
    let workspace = &app.github;
    let body = Rect::new(0, 1, width, height.saturating_sub(2));
    let (list_rect, reader_rect) = if mode == Mode::Narrow {
        (body, None)
    } else {
        let left = (width * if mode == Mode::Wide { 44 } else { 40 } / 100).max(40);
        (Rect::new(0, 1, left, body.h), Some(Rect::new(left, 1, width - left, body.h)))
    };
    let threads = workspace.threads();
    let shown = workspace.shown_threads();
    let selected = workspace.notify.selected.min(shown.len().saturating_sub(1));
    list(&mut frame, workspace, list_rect, &threads, &shown, selected, icons);
    if let Some(rect) = reader_rect {
        reader(&mut frame, workspace, rect, shown.get(selected), icons);
    }
    let footer = Rect::new(0, height - 1, width, 1);
    let message = if !app.notice.is_empty() {
        Some((app.notice.as_str(), true))
    } else if !workspace.notice.is_empty() {
        Some((workspace.notice.as_str(), workspace.notice_error))
    } else {
        None
    };
    chrome::footer(
        &mut frame.grid,
        footer,
        &[("Enter", "open"), ("m", "mark read"), ("M", "mark all"), ("u", "unsubscribe"), ("[ ]", "sections"), ("r", "refresh")],
        &workspace.repo,
        message,
    );
    super::accounts::paint(&mut frame, &app.github, 0, width, height);
    frame
}

#[allow(clippy::too_many_arguments)]
fn list(
    frame: &mut Frame,
    workspace: &Workspace,
    rect: Rect,
    threads: &[Notification],
    shown: &[Notification],
    selected: usize,
    mode: ExplorerIconMode,
) {
    let terms = workspace.terms();
    let unread = threads.iter().filter(|thread| thread.unread).count();
    let title: Vec<Span> = vec![
        (format!("{} ", glyph(Glyph::Notification, mode)), st(GitRole::Focus, PANE)),
        (terms.notifications_tab.to_owned(), bold(GitRole::Text, PANE)),
    ];
    let count = match workspace.value(&Key::Notifications) {
        Some(_) => format!("{unread} unread"),
        None => "…".into(),
    };
    chrome::pane(&mut frame.grid, rect, &title, &count, true);
    let inner = rect.inner();
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    let mut y = inner.y;
    // Sections, each counted from the same list.
    let tabs = SECTIONS
        .iter()
        .enumerate()
        .map(|(index, name)| format!("{name} {}", threads.iter().filter(|thread| in_section(index, thread)).count()))
        .collect::<Vec<_>>();
    let tabs = chrome::fit_tabs(&tabs, workspace.notify.section, right.saturating_sub(x));
    chrome::sub_tabs(&mut frame.grid, x, y, right, &tabs, workspace.notify.section, true);
    y += 2;
    if let Some((text, _)) = workspace.host_notice(workspace.value(&Key::Notifications).is_some()) {
        frame.grid.put_text_clipped(x, y, &chrome::ellipsize(&text, usize::from(right - x)), st(GitRole::Muted, PANE), right);
        y += 2;
    }
    let bottom = inner.bottom();
    if workspace.value(&Key::Notifications).is_none() {
        let text = match workspace.cache.get(&Key::Notifications) {
            Some(Err(error)) => format!("✕ {error}"),
            _ if !workspace.notifications_available() => {
                workspace.capabilities().notifications.reason().unwrap_or("Notifications are not available here").to_owned()
            }
            _ => "Loading…".to_owned(),
        };
        frame.grid.put_text_clipped(x, y, &chrome::ellipsize(&text, usize::from(right - x)), st(GitRole::Muted, PANE), right);
        return;
    }
    if shown.is_empty() {
        let text = if threads.is_empty() {
            format!("Inbox zero · nothing new in {}", workspace.terms().notifications_tab.to_lowercase())
        } else {
            format!("Nothing in {} · [ ] other sections", SECTIONS[workspace.notify.section])
        };
        frame.grid.put_text_clipped(x, y, &text, st(GitRole::Muted, PANE), right);
        return;
    }
    let now = records::now();
    let per = 3u16;
    let visible = usize::from(bottom.saturating_sub(y) / per).max(1);
    let first = selected.saturating_sub(visible.saturating_sub(1));
    for (index, thread) in shown.iter().enumerate().skip(first).take(visible) {
        let current = index == selected;
        let surface = if current { GitSurface::Cursor } else { PANE };
        if current {
            chrome::fill(&mut frame.grid, Rect::new(inner.x, y, inner.w, 2), st(GitRole::Text, surface));
            frame.grid.set_cell(inner.x, y, "▎", st(GitRole::Focus, surface));
            frame.grid.set_cell(inner.x, y + 1, "▎", st(GitRole::Focus, surface));
            frame.cursor = Some((inner.x, y, crossterm::cursor::SetCursorStyle::SteadyBar));
        }
        let dot = if thread.unread { ("● ", st(GitRole::Focus, surface)) } else { ("  ", st(GitRole::Muted, surface)) };
        let title_style = if thread.unread { bold(GitRole::Text, surface) } else { st(GitRole::Muted, surface) };
        let used = 2;
        put(
            &mut frame.grid,
            x,
            y,
            &[
                (dot.0.to_owned(), dot.1),
                (chrome::ellipsize(&thread.title, usize::from(right.saturating_sub(x + used))), title_style),
            ],
            right,
        );
        let when = records::ago(&thread.updated.0, now);
        let mut meta: Vec<Span> = vec![
            (format!("{} ", target_glyph(thread, mode)), st(GitRole::Ref, surface)),
            (target_words(thread), st(GitRole::Ref, surface)),
            (" · ".into(), st(GitRole::Muted, surface)),
            (
                reason_word(workspace, &thread.reason),
                st(
                    match thread.reason {
                        NotifyReason::Mentioned | NotifyReason::ReviewRequested => GitRole::Modified,
                        _ => GitRole::Muted,
                    },
                    surface,
                ),
            ),
        ];
        if !when.is_empty() {
            meta.push((" · ".into(), st(GitRole::Muted, surface)));
            meta.push((when, st(GitRole::Muted, surface)));
        }
        put(&mut frame.grid, x + 2, y + 1, &meta, right);
        y += per;
        if y + 1 >= bottom {
            break;
        }
    }
}

fn reader(frame: &mut Frame, workspace: &Workspace, rect: Rect, thread: Option<&Notification>, mode: ExplorerIconMode) {
    let title: Vec<Span> = vec![("Thread".into(), bold(GitRole::Text, PANE))];
    chrome::pane(&mut frame.grid, rect, &title, "", false);
    let inner = rect.inner();
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    let mut y = inner.y;
    let Some(thread) = thread else {
        frame.grid.put_text_clipped(x, y, "Select a notification to see what it is about", st(GitRole::Muted, PANE), right);
        return;
    };
    let width = usize::from(right.saturating_sub(x));
    for line in crate::ui::render::wrap_plain_text(&thread.title, width.max(10), crate::config::AmbiguousWidth::Narrow).into_iter().take(2) {
        frame.grid.put_text_clipped(x, y, &line, bold(GitRole::Text, PANE), right);
        y += 1;
    }
    let chip = if thread.unread { (" Unread ", GitChip::Focus, GitRole::Focus) } else { (" Read ", GitChip::Muted, GitRole::Muted) };
    let at = frame.grid.put_text_clipped(x, y, chip.0, st(chip.2, GitSurface::Chip(chip.1)), right);
    put(
        &mut frame.grid,
        at + 1,
        y,
        &[
            (format!("{} ", target_glyph(thread, mode)), st(GitRole::Ref, PANE)),
            (target_words(thread), st(GitRole::Ref, PANE)),
        ],
        right,
    );
    y += 2;
    let now = records::now();
    let column = 12usize;
    let field = |grid: &mut CellGrid, y: u16, name: &str, value: &str, role: GitRole| {
        let at = grid.put_text_clipped(x, y, &format!("{name:<column$}"), st(GitRole::Muted, PANE), right);
        grid.put_text_clipped(at, y, value, st(role, PANE), right);
    };
    let kind = match &thread.target {
        NotifyTarget::Item { kind: ItemKind::PullRequest, .. } => workspace.terms().pr_singular.to_owned(),
        NotifyTarget::Item { .. } => "issue".into(),
        NotifyTarget::Run(_) => workspace.terms().run_noun.to_owned(),
        NotifyTarget::Release(_) => "release".into(),
        NotifyTarget::Other(other) => other.clone(),
    };
    field(&mut frame.grid, y, "Repository", &thread.repo, GitRole::Ref);
    y += 1;
    field(&mut frame.grid, y, "About", &kind, GitRole::Text);
    y += 1;
    field(&mut frame.grid, y, "Why", &reason_word(workspace, &thread.reason), GitRole::Modified);
    y += 1;
    let when = records::ago(&thread.updated.0, now);
    if !when.is_empty() {
        field(&mut frame.grid, y, "Updated", &when, GitRole::Muted);
        y += 1;
    }
    y += 1;
    frame.grid.put_text_clipped(x, y, "Next step", bold(GitRole::Text, PANE), right);
    y += 1;
    let here = thread.repo.is_empty() || thread.repo.eq_ignore_ascii_case(&workspace.repo);
    let open = match &thread.target {
        NotifyTarget::Item { kind, number } => {
            let tab = if *kind == ItemKind::PullRequest { workspace.terms().pr_plural } else { "Issues" };
            if here { format!("Open #{number} in {tab}") } else { format!("Open #{number} from {}", thread.repo) }
        }
        NotifyTarget::Run(id) => format!("Open {} {} in {}", workspace.terms().run_noun, id.0, workspace.terms().actions_tab),
        _ => "Open in the browser".into(),
    };
    let readable = workspace.notifications_available();
    let mut actions: Vec<(String, &str, bool)> = vec![(open, "Enter", true)];
    if thread.unread {
        actions.push(("Mark as read".into(), "m", readable));
    }
    actions.push(("Unsubscribe from this thread".into(), "u", readable));
    for (label, key, enabled) in actions {
        let role = if enabled { GitRole::Link } else { GitRole::Muted };
        put(
            &mut frame.grid,
            x,
            y,
            &[
                ("› ".into(), st(GitRole::Focus, PANE)),
                (label, st(role, PANE)),
                ("  ".into(), st(GitRole::Text, PANE)),
                (key.to_owned(), bold(GitRole::Focus, PANE)),
            ],
            right,
        );
        y += 1;
        if y >= inner.bottom() {
            return;
        }
    }
}
