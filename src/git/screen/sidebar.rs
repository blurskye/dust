//! The Git sidebar's repository, reference and history tabs.
use super::*;

pub(super) struct ReflogEntry {
    pub id: String,
    pub label: String,
    pub key: PathBuf,
}
impl ReflogEntry {
    pub fn parse(text: &str) -> Vec<Self> {
        text.lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                let id = fields.next()?;
                let date = fields.next()?;
                let subject = fields.next()?;
                git2::Oid::from_str(id).ok()?;
                Some((id, date, subject))
            })
            .enumerate()
            .map(|(index, (id, date, subject))| Self {
                id: id.into(),
                label: format!("{} HEAD@{{{index}}} {subject}", &id[..7]),
                // A refresh can insert a new entry at HEAD@{0}. Preserve the selected event,
                // rather than silently inspecting whatever now occupies its old ordinal.
                key: format!("{id}\t{date}\t{subject}").into(),
            })
            .collect()
    }
}

impl GitScreen {
    pub(super) fn switch_sidebar_tab(&mut self, next: bool) {
        self.clear_file_selection();
        self.commit_open = false;
        self.commit = None;
        self.last_list = self.focus;
        self.notice.clear();
        match self.focus {
            0 => self.files_view = (self.files_view + if next { 1 } else { 2 }) % 3,
            1 => {
                let tabs = [0, 2, 3];
                let at = tabs
                    .iter()
                    .position(|view| *view == self.subview)
                    .unwrap_or(0);
                self.subview = tabs[(at + if next { 1 } else { 2 }) % tabs.len()];
            }
            2 => {
                self.history_view = 1 - self.history_view;
                if self.history_view == 1 {
                    let result = self.reload_reflog();
                    self.report(result);
                }
            }
            _ => return,
        }
        self.offsets[self.focus] = 0;
        self.preview_offset = 0;
        self.preview_column = 0;
        self.clamp();
        self.select_history();
        self.load_preview();
    }

    pub(super) fn worktree_rows(&self) -> Vec<Row> {
        self.refs
            .as_ref()
            .map(|refs| {
                refs.worktrees
                    .iter()
                    .map(|worktree| {
                        Row::plain(
                            format!(
                                "{}  {}{}{}{}",
                                worktree.branch.as_deref().unwrap_or("detached"),
                                worktree
                                    .path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy(),
                                if worktree.dirty { " *" } else { "" },
                                if worktree.locked { " [locked]" } else { "" },
                                worktree
                                    .operation
                                    .as_ref()
                                    .map(|operation| format!(" [{operation}]"))
                                    .unwrap_or_default(),
                            ),
                            worktree.path.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn remote_rows(&self) -> Vec<Row> {
        let mut rows = std::collections::BTreeMap::new();
        for remote in &self.remotes {
            let Some((name, rest)) = remote.split_once(char::is_whitespace) else {
                continue;
            };
            rows.entry(PathBuf::from(name))
                .or_insert_with(|| Row::plain(format!("{name}  {}", rest.trim()), name.into()));
        }
        if let Some(refs) = &self.refs {
            for branch in refs.branches.iter().filter(|branch| branch.remote) {
                rows.insert(
                    branch.name.clone().into(),
                    Row::plain(format!("  {}", branch.name), branch.name.clone().into()),
                );
            }
        }
        rows.into_values().collect()
    }

    pub(super) fn remote_branch(&self, row: &Row) -> bool {
        self.refs.as_ref().is_some_and(|refs| {
            refs.branches
                .iter()
                .any(|branch| branch.remote && Path::new(&branch.name) == row.value)
        })
    }

    pub(super) fn submodule_rows(&self) -> Vec<Row> {
        self.refs
            .as_ref()
            .and_then(|refs| refs.submodules.as_ref().ok())
            .map(|modules| {
                modules
                    .iter()
                    .map(|module| {
                        let entry = self.submodule_status(&module.path);
                        let staged = entry.is_some_and(|entry| entry.sources.staged);
                        let pointer_changed = module.initialized && module.head != module.index;
                        let unstaged = pointer_changed
                            || module.dirty
                            || entry.is_some_and(|entry| entry.sources.unstaged);
                        let mut state = Vec::new();
                        if staged {
                            state.push("staged pointer");
                        }
                        if pointer_changed {
                            state.push("unstaged pointer");
                        }
                        if module.dirty {
                            state.push("child files modified");
                        }
                        if !module.initialized {
                            state.push("uninitialized");
                        }
                        let status_known = self
                            .snapshot
                            .as_ref()
                            .is_some_and(|snapshot| !snapshot.truncated || entry.is_some());
                        if !status_known {
                            state.push("parent status unavailable");
                        } else if state.is_empty() {
                            state.push("clean");
                        }
                        // Match Changes: a check is staged, never merely a clean checkout.
                        let marker = match (staged, unstaged, module.initialized) {
                            (true, true, _) => "◐",
                            (true, false, _) => "✓",
                            (_, _, false) => "−",
                            _ => " ",
                        };
                        let code = self.submodule_change_code(module);
                        Row::plain(
                            format!(
                                "{marker} {code} {}  {}",
                                module.path.display(),
                                state.join(" · "),
                            ),
                            module.path.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn submodule_status(&self, path: &Path) -> Option<&GitStatusEntry> {
        self.snapshot
            .as_ref()?
            .entries
            .iter()
            .find(|entry| entry.relative_path == path)
    }

    pub(super) fn submodule_change_code(&self, module: &GitSubmoduleEntry) -> String {
        let entry = self.submodule_status(&module.path);
        let known = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.truncated || entry.is_some());
        let mut code = entry
            .map(GitStatusEntry::change_code)
            .unwrap_or_else(|| (if known { "  " } else { "? " }).into())
            .chars()
            .collect::<Vec<_>>();
        // Raw parent diffs can omit untracked child files. Inventory still knows
        // they are dirty: merge that evidence into Y without losing X (or UU/D).
        if code[1] == ' ' && (module.dirty || module.initialized && module.head != module.index) {
            code[1] = 'M';
        }
        code.into_iter().collect()
    }

    /// Child file dirtiness cannot be staged in the parent. Only a new checkout revision
    /// remains actionable after its gitlink was staged, so the next Space must unstage it.
    pub(super) fn submodule_pointer_matches_index(&self, path: &Path) -> bool {
        self.refs
            .as_ref()
            .and_then(|refs| refs.submodules.as_ref().ok())
            .and_then(|modules| modules.iter().find(|module| module.path == path))
            .is_some_and(|module| module.initialized && module.head == module.index)
    }

    pub(super) fn submodule_preview(&mut self, path: &Path) {
        let Some(module) = self
            .refs
            .as_ref()
            .and_then(|refs| refs.submodules.as_ref().ok())
            .and_then(|modules| modules.iter().find(|module| module.path == path))
        else {
            return;
        };
        let entry = self.submodule_status(path);
        let index_state = if entry.is_some_and(|entry| entry.sources.staged) {
            "staged pointer · Space unstages when checkout matches index"
        } else if self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.truncated && entry.is_none())
        {
            "parent status unavailable"
        } else {
            "no staged pointer"
        };
        self.preview = format!("{}\n\nParent index: {}\nCheckout pointer: {}\nCheckout: {}\nWorking files: {}\nExpected index commit: {}\nActual submodule HEAD: {}\n\nEnter opens this submodule as a Git workspace.\nSpace stages or unstages its commit pointer, not its working files.\nu initializes / updates to the expected commit after confirmation.",
            module.path.display(),
            index_state,
            if !module.initialized { "not initialized" } else if module.head != module.index { "unstaged revision · Space stages this pointer" } else { "matches index" },
            if module.initialized { "initialized" } else { "not initialized" },
            if module.dirty { "modified; open the submodule to review and commit them" } else { "clean" },
            module.index.as_deref().unwrap_or("none"),
            module.head.as_deref().unwrap_or("none"),
        ).into();
    }

    pub(super) fn open_sidebar_repository(&mut self, row: Row) -> ScreenOutcome {
        if self.files_view == 1 {
            if self.tutor.is_some() {
                let result = self.tutor_worktree(&row.value);
                self.report(result);
                return ScreenOutcome::Continue;
            }
            return ScreenOutcome::OpenRepository(row.value);
        }
        let path = self.root.join(&row.value);
        let allowed = crate::files::canonicalize(&path)
            .ok()
            .zip(crate::files::canonicalize(&self.root).ok())
            .is_some_and(|(path, root)| path.starts_with(root));
        if allowed && git2::Repository::open(&path).is_ok_and(|repo| repo.workdir().is_some()) {
            ScreenOutcome::OpenRepository(path)
        } else {
            self.notice = "Submodule is not initialized; u reviews its initialization".into();
            ScreenOutcome::Continue
        }
    }

    pub(super) fn toggle_submodule(&mut self, path: PathBuf) -> Result<(), String> {
        self.allow_write()?;
        let module = self
            .refs
            .as_ref()
            .and_then(|refs| refs.submodules.as_ref().ok())
            .and_then(|modules| modules.iter().find(|module| module.path == path))
            .ok_or("Select a listed submodule")?;
        let entry = self.snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .entries
                .iter()
                .find(|entry| entry.relative_path == path)
        });
        let unstage = entry.is_some_and(|entry| {
            entry.sources.staged && (module.head == module.index || !entry.sources.unstaged)
        });
        if !unstage && !module.initialized {
            return Err("Initialize or open the submodule before staging its revision".into());
        }
        if !unstage && module.head == module.index {
            return Err("Commit changes inside the submodule before staging its revision".into());
        }
        let mut args: Vec<OsString> = vec!["--literal-pathspecs".into()];
        if unstage {
            if git_head_id(&self.root)
                .map_err(|error| error.to_string())?
                .is_some()
            {
                args.extend(["restore", "--source=HEAD", "--staged", "--"].map(Into::into));
            } else {
                args.extend(["rm", "--cached", "--"].map(Into::into));
            }
        } else {
            args.extend(["add", "--"].map(Into::into));
        }
        args.push(path.into_os_string());
        self.write(args)
    }

    pub(super) fn initialize_submodule(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let module = self
            .refs
            .as_ref()
            .and_then(|refs| refs.submodules.as_ref().ok())
            .and_then(|modules| modules.iter().find(|module| module.path == row.value));
        let Some(module) = module else { return };
        if module.dirty {
            self.notice =
                "Commit or save the submodule's working changes before updating its checkout"
                    .into();
            return;
        }
        self.form(
            &format!(
                "Initialize / update {} to its index commit?",
                row.value.display()
            ),
            FormAction::Confirm(vec![
                "--literal-pathspecs".into(),
                "submodule".into(),
                "update".into(),
                "--init".into(),
                "--".into(),
                row.value.into_os_string(),
            ]),
            Vec::new(),
        );
    }

    pub(super) fn remove_sidebar_worktree(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let Some(worktree) = self.refs.as_ref().and_then(|refs| {
            refs.worktrees
                .iter()
                .find(|worktree| worktree.path == row.value)
        }) else {
            return;
        };
        if worktree.main {
            self.notice = "The main worktree cannot be removed".into();
            return;
        }
        self.form(
            &format!(
                "Remove clean, unlocked worktree {}?",
                worktree.path.display()
            ),
            FormAction::RemoveWorktree(worktree.name.clone()),
            Vec::new(),
        );
    }

    pub(super) fn fetch_sidebar_remote(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let selected = row.value.to_string_lossy();
        let name = self
            .remotes
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| selected == *name || selected.starts_with(&format!("{name}/")))
            .max_by_key(|name| name.len())
            .map(str::to_owned);
        if let Some(name) = name {
            self.form(
                &format!("Fetch from {name}?"),
                FormAction::Confirm(vec!["fetch".into(), "--".into(), name.into()]),
                Vec::new(),
            );
        }
    }

    pub(super) fn reload_reflog(&mut self) -> Result<(), String> {
        if self.reflog.is_empty() {
            self.preview = "Loading reflog…".into();
        }
        self.reflog_error = None;
        let result = self.run(
            Request::Reflog,
            [
                "log",
                "-g",
                "-300",
                "--format=%H%x09%gD%x09%gs",
                "--date=raw",
                "HEAD",
                "--",
            ]
            .map(Into::into)
            .into(),
        );
        self.reflog_loading = result.is_ok();
        result
    }
}
