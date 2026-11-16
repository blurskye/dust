//! DESIGN.md §5.7: the keys of the Pull requests and Issues tabs. Anything not handled here
//! falls through to `Workspace::event`'s shared list/reader keys.
use super::*;

impl Workspace {
    fn selected_number(&self) -> Result<u64, String> {
        match self.number() {
            0 => Err(if self.page == 2 {
                format!("Select a {} first", self.terms().pr_singular)
            } else {
                "Select an issue first".into()
            }),
            id => Ok(id),
        }
    }

    /// The file and new-side line `e` opens (DESIGN.md §5.3): a diff line's own; a deleted
    /// line's replacement (the next new-side line of its file, else the one before); a review
    /// thread's line above it; a file or hunk heading's first line below.
    fn editor_location(&self) -> Option<(String, u32)> {
        let rows = &self.detail_rows;
        let at = self.detail_cursor.min(rows.len().checked_sub(1)?);
        let right = |row: &super::lines::Line| match &row.action {
            Some(Action::Inline { path, line, side: "RIGHT", .. }) => Some((path.clone(), *line)),
            _ => None,
        };
        if let Some(found) = right(&rows[at]) {
            return Some(found);
        }
        match &rows[at].action {
            Some(Action::Inline { path, .. }) => rows[at..]
                .iter()
                .filter_map(right)
                .find(|(file, _)| file == path)
                .or_else(|| rows[..at].iter().rev().filter_map(right).find(|(file, _)| file == path))
                .or_else(|| Some((path.clone(), 1))),
            Some(Action::SelectFile(_, path)) => Some((path.clone(), 1)),
            _ if rows[at].reply.is_some() => rows[..at].iter().rev().find_map(right),
            _ => rows[at..].iter().take_while(|row| !row.outline).find_map(right),
        }
    }

    fn reader_tab(&mut self, forward: bool) -> Result<Option<Form>, String> {
        // An issue is one Conversation page (§3.33): no tabs to step through.
        if self.page == 1 {
            let _ = forward;
            return Ok(None);
        }
        let current = self.pr_tab.min(pr::CYCLE - 1);
        let tab = (current + if forward { 1 } else { pr::CYCLE - 1 }) % pr::CYCLE;
        self.activate(Action::PrTab(tab))
    }

    /// `Some` when the key belongs to the forge tabs.
    pub(super) fn forge_key(
        &mut self,
        key: &KeyEvent,
        write_refusal: Option<&str>,
    ) -> Option<Result<Option<Form>, String>> {
        // Review drafts and line ranges in Files (`drafts.rs`).
        if let Some(result) = self.review_key(key) {
            return Some(result);
        }
        let reader = self.focus == 2;
        let pull = self.page == 2;
        // Files keys (`v` viewed, `w` wrap) belong to the Files reader; from the list `v`
        // opens the saved views, `l` the labels and `w` a worktree as everywhere else.
        let files = pull && self.pr_tab == pr::FILES && reader;
        let row = self.detail_rows.get(self.detail_cursor);
        let result = match key.code {
            KeyCode::Char('[' | ']') => {
                let forward = key.code == KeyCode::Char(']');
                if reader {
                    self.reader_tab(forward)
                } else {
                    self.cycle_section(forward).map(|()| None)
                }
            }
            KeyCode::Char('H' | 'L') => self.reader_tab(key.code == KeyCode::Char('L')),
            KeyCode::Char('2') => {
                self.focus = 0;
                self.last_list = 0;
                self.return_inbox_list();
                Ok(None)
            }
            KeyCode::Char('3' | '0') => {
                self.last_list = 0;
                self.focus = 2;
                Ok(None)
            }
            KeyCode::Char('R') => {
                if self.writing() {
                    return Some(Err("Wait for the confirmed operation before refreshing".into()));
                }
                self.notice.clear();
                self.notice_error = false;
                self.ensure(true);
                Ok(None)
            }
            KeyCode::Char('C') => self
                .selected_number()
                .and_then(|id| self.activate(Action::Comment(self.page, id))),
            KeyCode::Char('x' | 'X') => {
                let close = key.code == KeyCode::Char('x');
                self.selected_number()
                    .and_then(|id| self.activate(Action::State(self.page, id, close)))
            }
            KeyCode::Char('l') if !files => self
                .selected_number()
                .and_then(|id| self.activate(Action::Metadata(self.page, id, "labels"))),
            KeyCode::Char('a') => self
                .selected_number()
                .and_then(|id| self.activate(Action::Metadata(self.page, id, "assignees"))),
            KeyCode::Char('M') if !pull => self
                .selected_number()
                .and_then(|id| self.activate(Action::Metadata(1, id, "milestone"))),
            KeyCode::Char('m') if !pull => self
                .selected_number()
                .and_then(|id| self.activate(Action::Metadata(1, id, "milestone"))),
            // The merge box ends the Conversation; `m` goes there from anywhere.
            KeyCode::Char('m') => self.selected_number().and_then(|_| {
                let result = self.activate(Action::PrTab(pr::CONVERSATION));
                self.reveal_merge = true;
                self.reading = false;
                result
            }),
            KeyCode::Char('r') if pull => self.selected_number().and_then(|id| self.activate(Action::Review(id))),
            // Long lines wrap by default; `w` pans them sideways instead, and back. The
            // cursor keeps its source line (the reading position follows its bytes).
            KeyCode::Char('w') if files => {
                self.diff_nowrap = !self.diff_nowrap;
                self.horizontal = 0;
                Ok(None)
            }
            KeyCode::Char('w') if pull => self.selected_number().and_then(|id| self.activate(Action::Worktree(id))),
            KeyCode::Char('d') if pull => self.selected_number().and_then(|_| self.activate(Action::PrTab(pr::FILES))),
            // On a check `o` opens what Enter opens (its run, else its page on the forge);
            // elsewhere it opens the pull request in the browser.
            KeyCode::Char('o')
                if pull && reader && self.pr_tab == pr::CHECKS && matches!(row.and_then(|row| row.action.as_ref()), Some(Action::Check(..))) =>
            {
                let action = row.and_then(|row| row.action.clone()).expect("a check row");
                self.activate(action)
            }
            KeyCode::Char('v') if files => {
                let Ok(id) = self.selected_number() else {
                    return Some(Err("Select a pull request first".into()));
                };
                if let Some(reason) = write_refusal {
                    return Some(Err(reason.into()));
                }
                let path = self
                    .item()
                    .map(|item| self.selected_file(id, item).to_owned())
                    .unwrap_or_default();
                if path.is_empty() {
                    Err("No changed file is selected".into())
                } else {
                    self.activate(Action::Viewed(id, path))
                }
            }
            // `e` opens the line under the cursor in the editor (DESIGN.md §5.3): the
            // checkout's copy of the file, at the new side's line. In Commits it does so on a
            // commit's diff; on the commit list `e` edits the description, as elsewhere.
            KeyCode::Char('e') if pull && reader && matches!(self.pr_tab, pr::FILES | pr::COMMITS) => {
                match self.editor_location() {
                    Some(_) if self.foreign_inbox_item() => {
                        Err("Remote inbox item: open its local repository to edit its files".into())
                    }
                    Some((path, line)) => Err(format!("open-at:{line}:{path}")),
                    None if self.pr_tab == pr::FILES => Err("No changed line under the cursor".into()),
                    None => return None,
                }
            }
            KeyCode::Char('c') if pull => {
                // In the reader `c` answers the thread or line under the cursor; elsewhere
                // it checks the branch out (§5.7).
                let answer = row
                    .filter(|_| reader)
                    .and_then(|row| {
                        row.reply
                            .as_ref()
                            .or_else(|| row.action.as_ref().filter(|action| matches!(action, Action::Inline { .. })))
                    })
                    .cloned();
                match answer {
                    Some(action) => self.activate(action),
                    None => self.selected_number().and_then(|id| self.activate(Action::Checkout(id))),
                }
            }
            _ => return None,
        };
        Some(result)
    }
}
