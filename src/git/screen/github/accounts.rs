//! Host states in the forge tabs (FORGE.md §2.5, DESIGN §3.45 X-OFFLINE / X-PROVIDER): one
//! notice row under the list title while the host is signed out, missing its CLI, rate limited,
//! unreachable or unknown, and the accounts popover (`a`) listing every host this session has
//! talked to. `r` in the popover forgets credentials, transport choices, detection and pauses,
//! then reloads.

use super::super::render::{ellipsize, section, surface};
use super::{Form, Workspace};
use crate::editor::input::{KeyCode, KeyEvent};
use crate::forge::host_state::{self, Account, HostState, Tone};
use crate::forge::request::ForgeError;
use crate::forge::transport::HostKey;
use crate::ui::{Frame, Style};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(super) struct Accounts {
    /// The host the last host-level error was about, and its state.
    state: Option<(String, HostState)>,
    /// The last reply that reached the forge, for "cached 2m ago".
    last_success: Option<Instant>,
    pub(super) open: bool,
}

impl Accounts {
    /// A reply arrived: a host-level error becomes the notice; anything the forge answered
    /// (even "not found") clears it.
    pub(super) fn observe(&mut self, host: &str, error: Option<&ForgeError>) {
        let Some(error) = error else {
            self.state = None;
            self.last_success = Some(Instant::now());
            return;
        };
        if matches!(error, ForgeError::Cancelled) {
            return;
        }
        // The registry knows the host and its forge when the workspace does not yet (the very
        // first read failed before any repository was known).
        let account = (!host.is_empty())
            .then(|| host_state::account(&HostKey::new(host)))
            .flatten()
            .or_else(host_state::latest);
        let kind = account.as_ref().and_then(|account| account.kind);
        match HostState::from_error(kind, error) {
            Some(state) => {
                let host = if host.is_empty() {
                    account.map(|account| account.host.to_string()).unwrap_or_default()
                } else {
                    host.to_owned()
                };
                self.state = Some((host, state));
            }
            None => {
                self.state = None;
                self.last_success = Some(Instant::now());
            }
        }
    }

    /// The notice row, or `None` while the host is fine.
    pub(super) fn notice(&self, showing_cached: bool) -> Option<(String, Tone)> {
        let (host, state) = self.state.as_ref()?;
        let host = if host.is_empty() { "the forge" } else { host };
        let cached = self
            .last_success
            .filter(|_| showing_cached)
            .map(|at| at.elapsed());
        state.notice(host, cached)
    }

    /// Popover rows: every host this process has observed, the current one first.
    fn lines(&self, current: &str) -> Vec<(String, Tone)> {
        let mut accounts = host_state::accounts();
        accounts.sort_by_key(|account| (account.host.to_string() != current, account.host.to_string()));
        if accounts.is_empty()
            && let Some((host, state)) = &self.state
        {
            accounts.push(Account::new(HostKey::new(host), None, state.clone()));
        }
        let mut lines: Vec<(String, Tone)> = accounts.iter().flat_map(Account::lines).collect();
        if lines.is_empty() {
            lines.push(("No forge contacted yet".into(), Tone::Muted));
        }
        lines
    }
}

fn style(tone: Tone) -> Style {
    match tone {
        Tone::Normal => Style::Default,
        Tone::Muted => Style::Muted,
        Tone::Warning => Style::Warning,
        Tone::Error => Style::Error,
    }
}

impl Workspace {
    /// The host notice row: a host-level error, else the anonymous read-only chip
    /// (`○ Anonymous · read-only · 52/60 requests left · sign in: gh auth login`). Anonymous
    /// reads succeed, so no error sets that state; the registry knows it.
    pub(super) fn host_notice(&self, showing_cached: bool) -> Option<(String, Tone)> {
        self.accounts.notice(showing_cached).or_else(|| {
            self.host_state()
                .filter(HostState::read_only)
                .and_then(|state| state.notice(&self.host, None))
        })
    }

    /// The instance hides even reads from anonymous visitors: its sign-in commands (one per
    /// way in), for the full-pane empty state instead of an error dump.
    pub(super) fn sign_in_hints(&self) -> Option<(String, Vec<String>)> {
        let from_error = self.accounts.state.as_ref().and_then(|(host, state)| match state {
            HostState::SignInRequired { hints, .. } => Some((host.clone(), hints.clone())),
            _ => None,
        });
        from_error.or_else(|| match self.host_state()? {
            HostState::SignInRequired { hints, .. } => Some((self.host.clone(), hints)),
            _ => None,
        })
    }

    /// Keys while the popover is open (it is modal), and `a` to open it.
    pub(super) fn accounts_key(&mut self, key: &KeyEvent) -> Option<Result<Option<Form>, String>> {
        if key.modifiers.control || key.modifiers.alt {
            return self.accounts.open.then_some(Ok(None));
        }
        if !self.accounts.open {
            if key.code == KeyCode::Char('a') && self.focus < 2 {
                self.accounts.open = true;
                return Some(Ok(None));
            }
            return None;
        }
        match key.code {
            KeyCode::Escape | KeyCode::Char('a' | 'q') => self.accounts.open = false,
            KeyCode::Char('r') => {
                host_state::recheck();
                self.accounts.state = None;
                self.accounts.open = false;
                self.notice.clear();
                self.notice_error = false;
                self.ensure(true);
            }
            _ => {}
        }
        Some(Ok(None))
    }
}

/// The notice over the list pane's state row, and the popover over everything but forms.
pub(super) fn paint(frame: &mut Frame, workspace: &Workspace, left: u16, width: u16, height: u16) {
    let showing_cached = workspace.cache.values().any(Result::is_ok);
    if left > 4
        && let Some((text, tone)) = workspace.host_notice(showing_cached)
    {
        // Pages 1–2: over the state/count row; page 3: over the first workflow row.
        let row = 3;
        let focused = workspace.focus == 0;
        for x in 1..left - 1 {
            frame.grid.set_cell(x, row, " ", surface(focused, None));
        }
        frame.grid.put_text_clipped(
            2,
            row,
            &ellipsize(&text, usize::from(left - 4)),
            style(tone),
            left - 2,
        );
    }
    if !workspace.accounts.open {
        return;
    }
    let mut lines = workspace.accounts.lines(&workspace.host);
    lines.push((String::new(), Tone::Muted));
    lines.push(("r re-check · Esc close".into(), Tone::Muted));
    let widest = lines.iter().map(|(text, _)| text.width()).max().unwrap_or(0) as u16;
    let w = (widest + 4).clamp(36, width.saturating_sub(4).max(36)).min(width);
    let h = (lines.len() as u16 + 2).min(height.saturating_sub(4));
    let x = width.saturating_sub(w + 1);
    let y = 2;
    section(&mut frame.grid, x, y, w, h, "Accounts", true);
    for (index, (text, tone)) in lines.iter().enumerate().take(usize::from(h.saturating_sub(2))) {
        frame.grid.put_text_clipped(
            x + 2,
            y + 1 + index as u16,
            &ellipsize(text, usize::from(w.saturating_sub(4))),
            style(*tone),
            x + w - 2,
        );
    }
}
