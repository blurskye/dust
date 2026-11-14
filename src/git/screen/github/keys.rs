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

    fn reader_tab(&mut self, forward: bool) -> Result<Option<Form>, String> {
        if self.page == 1 {
            let tab = (self.issue_tab + if forward { 1 } else { issue::TABS.len() - 1 }) % issue::TABS.len();
            return self.activate(Action::PrTab(tab));
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
        let reader = self.focus == 2;
        let pull = self.page == 2;
        // Files keys (`v` viewed, `u` split) belong to the Files reader; from the list `v`
        // opens the saved views and `l` the labels as everywhere else.
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
            KeyCode::Char('m') => self.selected_number().and_then(|_| self.activate(Action::PrTab(pr::MERGE))),
            KeyCode::Char('r') if pull => self.selected_number().and_then(|id| self.activate(Action::Review(id))),
            KeyCode::Char('w') if pull => self.selected_number().and_then(|id| self.activate(Action::Worktree(id))),
            KeyCode::Char('d') if pull => self.selected_number().and_then(|_| self.activate(Action::PrTab(pr::FILES))),
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
            // The layout is a preference: too narrow a reader shows unified and says why in
            // the file heading, and the footer keeps its keys (no error replaces it).
            KeyCode::Char('u') if files => {
                self.split_diff = !self.split_diff;
                views::set_split_preference(&self.root, self.split_diff);
                self.detail_rows.clear();
                self.reveal_file = true;
                Ok(None)
            }
            _ => return None,
        };
        Some(result)
    }
}
