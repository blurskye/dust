//! Recovery from a selected reflog event, always pinned to its full commit object ID.
use super::*;

impl GitScreen {
    pub(super) fn reflog_form(&mut self, key: &KeyCode) {
        let Some(entry) = self
            .selected_row()
            .and_then(|row| self.reflog.iter().find(|entry| entry.key == row.value))
        else {
            self.notice = "Select a reflog entry".into();
            return;
        };
        let id = entry.id.clone();
        let event = entry.label.clone();
        let message = self
            .commit
            .as_ref()
            .filter(|commit| commit.id == id && commit.message != "Loading commit…")
            .and_then(|commit| commit.message.lines().next())
            .filter(|message| !message.is_empty())
            .unwrap_or(&event)
            .to_owned();
        let (title, action, mut fields, effect) = match key {
            KeyCode::Char('n') => (
                "Create rescue branch at commit",
                FormAction::Branch(Some(id.clone())),
                vec![Field::new("Name", String::new())],
                "Keep HEAD, index and working files; do not check out",
            ),
            KeyCode::Char('o') => (
                "Check out commit with detached HEAD?",
                FormAction::CheckoutDetached(id.clone()),
                Vec::new(),
                "Detach HEAD and update the index and working files",
            ),
            KeyCode::Char('w') => (
                "Create detached worktree at commit",
                FormAction::DetachedWorktree(id.clone()),
                vec![Field::new(
                    "Path",
                    self.root
                        .parent()
                        .unwrap_or(&self.root)
                        .join(format!("reflog-{}", &id[..7]))
                        .display()
                        .to_string(),
                )],
                "New detached checkout; keep this worktree unchanged",
            ),
            _ => return,
        };
        for (label, value) in [
            ("Commit".to_owned(), id),
            (format!("Reflog event · {event}"), message),
            (
                if matches!(key, KeyCode::Char('o')) {
                    "Effect · Git refuses conflicting edits; no auto-stash"
                } else {
                    "Effect"
                }
                .to_owned(),
                effect.into(),
            ),
        ] {
            let mut field = Field::new(&label, value);
            field.required = false;
            field.read_only = true;
            field.input.cursor = 0;
            fields.push(field);
        }
        self.notice.clear();
        self.form = Some(Form {
            title: title.into(),
            action,
            selected: if matches!(key, KeyCode::Char('o')) {
                fields.len()
            } else {
                0
            },
            // Target and event are visible but cannot retarget an open recovery form.
            fields,
        });
    }

    pub(super) fn finish_reflog_form(&mut self, success: bool) {
        if std::mem::take(&mut self.reflog_submitting) && success {
            self.form = None;
        }
    }

    /// Anchor relative input to this checkout, not the process cwd. The branch worker owns
    /// destination occupancy and worktree checks; refuse symlink aliases before handing it a path.
    pub(super) fn reflog_worktree_path(&self, value: &str) -> Result<(PathBuf, String), String> {
        let path = Path::new(value);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let directory = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Choose a worktree destination with a directory name")?;
        // Administrative worktree names also appear in the worker's temporary ref.
        // Keep spaces and punctuation in the actual destination, not in that ref name.
        let name = directory
            .chars()
            .map(|ch| {
                if ch.is_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let temporary = format!("refs/heads/dust-worktree/{name}");
        if !git2::Reference::is_valid_name(&temporary) {
            return Err("Choose a worktree directory name valid for Git".into());
        }
        for ancestor in path.ancestors() {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) if metadata.is_symlink() => {
                    return Err("Choose a worktree path without symlinks".into());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let parent =
            crate::files::canonicalize(path.parent().ok_or("Choose a worktree parent directory")?)
                .map_err(|error| format!("Couldn't open the worktree's parent folder{}", super::words::io_reason(&error)))?;
        if !parent.is_dir() {
            return Err("Choose an existing worktree parent directory".into());
        }
        let repository = git2::Repository::open(&self.root).map_err(|error| error.to_string())?;
        if repository.find_reference(&temporary).is_ok() {
            return Err("A branch uses this worktree directory name; choose another path".into());
        }
        if repository.find_worktree(&name).is_ok() {
            return Err(format!(
                "A worktree named '{name}' already exists; choose another directory name"
            ));
        }
        Ok((parent.join(directory), name))
    }
}
