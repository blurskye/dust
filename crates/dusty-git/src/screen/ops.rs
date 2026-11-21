//! Pull, undo and redo, history rewriting and tag push on the Git tab:
//! each opens a sheet that shows what will move and the command, then runs as a write job.
use super::*;
use crate::history::{self, Edit, PullMode, Run};

/// A network write (pull, tag push) may take minutes; a local rebase far less.
const NETWORK: Duration = Duration::from_secs(300);
const LOCAL: Duration = Duration::from_secs(120);

/// What a confirmed sheet runs.
#[derive(Clone)]
pub(super) enum Op {
    Pull,
    Undo(history::Undo),
    Rewrite(Box<history::Rewrite>),
    PushTag(String),
    /// `y` on a commit: what each choice of the menu copies.
    Copy(Vec<(String, String)>),
}

/// The commands of a confirmed sheet still to run, and the notice once they all succeed.
pub(super) struct Running {
    rest: Vec<Run>,
    timeout: Duration,
    done: String,
    /// The last progress line Git printed, while it runs.
    pub(super) progress: Option<String>,
}

impl Op {
    /// Whether confirming changes the repository or a remote (the copy menu does not).
    pub(super) fn writes(&self) -> bool {
        !matches!(self, Self::Copy(_))
    }

    /// The sheet's confirm button: the verb the title asks about.
    pub(super) fn verb(&self) -> &'static str {
        self.button()
            .trim_start_matches("[ ")
            .trim_end_matches(" ]")
    }

    /// The confirm button of the older painter, bracketed.
    pub(super) fn button(&self) -> &'static str {
        match self {
            Self::Pull => "[ Pull ]",
            Self::Undo(undo) if undo.redo => "[ Redo ]",
            Self::Undo(_) => "[ Undo ]",
            Self::PushTag(_) => "[ Push tag ]",
            Self::Copy(_) => "[ Copy ]",
            Self::Rewrite(plan) => match plan.edit {
                Edit::Squash => "[ Squash ]",
                Edit::Fixup => "[ Fixup ]",
                Edit::Drop => "[ Drop ]",
                Edit::Edit => "[ Edit ]",
                Edit::Reword(_) => "[ Reword ]",
                Edit::MoveDown | Edit::MoveUp => "[ Move ]",
                Edit::Amend => "[ Amend ]",
            },
        }
    }
}

fn read_only(label: &str, value: String) -> Field {
    let mut field = Field::new(label, value);
    field.read_only = true;
    field.input.cursor = 0;
    field
}

impl GitScreen {
    /// `p` pull, `z` `Z` undo and redo, `P` on a tag, and the History keys that rewrite the
    /// selected commit: `s` `f` `d` `e` `J` `K`, and `A` `W` on a commit below HEAD.
    pub(super) fn ops_key(&mut self, key: &KeyEvent) -> bool {
        if self.page != 0 || key.modifiers.control || key.modifiers.alt {
            return false;
        }
        let KeyCode::Char(code) = key.code else {
            return false;
        };
        let history =
            self.focus == 2 && self.last_list == 2 && self.history_view == 0 && !self.commit_open;
        // A commit is in hand on History and in its open file list.
        let commit = history
            || matches!(self.focus, 0 | 4)
                && self.last_list == 2
                && self.history_view == 0
                && self.commit.is_some()
                && self.preview_anchor.is_none();
        let result = match code {
            'y' if commit => self.copy_form(),
            'o' if commit => self.open_commit(),
            'z' | 'Z' => self.undo_form(code == 'Z'),
            'p' if self.focus != 3 => self.pull_form(),
            'P' if self.focus == 1 && self.subview == 3 => self.push_tag_form(),
            's' if history => self.history_form(Edit::Squash),
            'f' if history => self.history_form(Edit::Fixup),
            'd' if history => self.history_form(Edit::Drop),
            'e' if history => self.history_form(Edit::Edit),
            'J' if history => self.history_form(Edit::MoveDown),
            'K' if history => self.history_form(Edit::MoveUp),
            // HEAD keeps the native amend and reword sheets (`rewrite.rs`).
            'A' if history && !self.head_selected() => self.history_form(Edit::Amend),
            'W' if history && !self.head_selected() => {
                self.history_form(Edit::Reword(String::new()))
            }
            _ => return false,
        };
        self.report(result);
        true
    }

    /// The commit `y` and `o` act on: the open commit, or the selected History row.
    fn commit_in_hand(&self) -> Result<git2::Oid, String> {
        let id = if matches!(self.focus, 0 | 4) {
            self.commit.as_ref().map(|commit| commit.id.clone())
        } else {
            self.selected_row()
                .and_then(|row| row.value.to_str().map(str::to_owned))
        };
        id.and_then(|id| git2::Oid::from_str(&id).ok())
            .ok_or_else(|| "Select a commit".into())
    }

    /// `y`: a menu of what to copy: the SHA, the short SHA, the subject, the forge URL.
    fn copy_form(&mut self) -> Result<(), String> {
        let id = self.commit_in_hand()?;
        let subject = git2::Repository::open(&self.root)
            .ok()
            .and_then(|repository| {
                let commit = repository.find_commit(id).ok()?;
                commit.summary().map(str::to_owned)
            })
            .unwrap_or_default();
        let mut items = vec![
            ("SHA".to_owned(), id.to_string()),
            ("Short SHA".to_owned(), history::short(id)),
            ("Subject".to_owned(), subject),
        ];
        if let Ok(url) = commit_url(&self.root, id) {
            items.push(("URL".to_owned(), url));
        }
        let mut field = Field::new("Copy", items[0].0.clone());
        field.choices = items.iter().map(|(label, _)| label.clone()).collect();
        let shown = items
            .iter()
            .map(|(label, value)| read_only(label, value.clone()))
            .collect::<Vec<_>>();
        let mut fields = vec![field];
        fields.extend(shown);
        self.op_form(
            format!("Copy from {}", history::short(id)),
            Op::Copy(items),
            fields,
            0,
        );
        Ok(())
    }

    /// `o`: the commit's page on the forge behind the default remote, in the browser.
    fn open_commit(&mut self) -> Result<(), String> {
        let url = commit_url(&self.root, self.commit_in_hand()?)?;
        crate::forge::checkout::open_url(&url)?;
        self.notice = format!("Opened {url}");
        Ok(())
    }

    fn head_selected(&self) -> bool {
        let head = git2::Repository::open(&self.root)
            .ok()
            .and_then(|repository| repository.head().ok()?.target())
            .map(|id| id.to_string());
        self.selected_row()
            .and_then(|row| row.value.to_str().map(str::to_owned))
            .is_none_or(|id| Some(id) == head)
    }

    fn op_form(&mut self, title: String, op: Op, fields: Vec<Field>, selected: usize) {
        self.form = Some(Form {
            title,
            action: FormAction::Run(Box::new(op)),
            fields,
            selected,
        });
        self.notice.clear();
    }

    pub(super) fn pull_form(&mut self) -> Result<(), String> {
        self.allow_write()?;
        let plan = history::pull(&self.root)?;
        let mut mode = Field::new("Mode", plan.mode.label().into());
        mode.choices = PullMode::ALL
            .iter()
            .map(|mode| mode.label().to_owned())
            .collect();
        let fields = vec![
            read_only("Branch", plan.branch.clone()),
            read_only("From", plan.upstream.clone()),
            mode,
        ];
        // Enter pulls in the configured mode; Tab reaches the mode, Up/Down changes it.
        self.op_form(
            format!("Pull {} into {}?", plan.upstream, plan.branch),
            Op::Pull,
            fields,
            3,
        );
        Ok(())
    }

    fn undo_form(&mut self, redo: bool) -> Result<(), String> {
        self.allow_write()?;
        let undo = history::undo(&self.root, redo)?;
        let (from, to) = undo.head();
        let fields = vec![
            read_only(if redo { "Redoes" } else { "Undoes" }, undo.entry.clone()),
            read_only("HEAD", format!("{from} → {to}")),
            read_only("Runs", undo.run().display()),
        ];
        let title = if redo {
            "Redo the undone action?"
        } else {
            "Undo the last action?"
        };
        self.op_form(title.into(), Op::Undo(undo), fields, 3);
        Ok(())
    }

    fn push_tag_form(&mut self) -> Result<(), String> {
        self.allow_write()?;
        let tag = self
            .selected_row()
            .and_then(|row| row.value.to_str().map(str::to_owned))
            .ok_or("Select a tag to push")?;
        let remote = history::default_remote(&self.root)?;
        let mut field = Field::new("Remote", remote);
        field.choices = git2::Repository::open(&self.root)
            .and_then(|repository| repository.remotes())
            .map(|remotes| remotes.iter().flatten().map(str::to_owned).collect())
            .unwrap_or_default();
        let fields = vec![read_only("Tag", tag.clone()), field];
        self.op_form(format!("Push tag {tag}?"), Op::PushTag(tag), fields, 2);
        Ok(())
    }

    fn history_form(&mut self, edit: Edit) -> Result<(), String> {
        self.allow_write()?;
        if self.history_loading
            || self
                .history_target
                .as_ref()
                .is_some_and(|(root, _)| root != &self.root)
        {
            return Err("Open the selected repository and wait for its history".into());
        }
        let target = self
            .selected_row()
            .and_then(|row| {
                row.value
                    .to_str()
                    .and_then(|id| git2::Oid::from_str(id).ok())
            })
            .ok_or("Select a history commit")?;
        let plan = history::rewrite(&self.root, target, edit.clone())?;
        let mut fields = Vec::new();
        if matches!(edit, Edit::Reword(_)) {
            let message = git2::Repository::open(&self.root)
                .ok()
                .and_then(|repository| {
                    let commit = repository.find_commit(target).ok()?;
                    commit.message().map(|text| text.trim_end().to_owned())
                })
                .unwrap_or_default();
            let mut field = Field::new("Message", message);
            field.multiline = true;
            fields.push(field);
        }
        fields.push(read_only("Commit", plan.commit.clone()));
        if let Some(other) = &plan.other {
            let label = match edit {
                Edit::MoveDown | Edit::MoveUp => "Swaps with",
                _ => "Into",
            };
            fields.push(read_only(label, other.clone()));
        }
        if edit == Edit::Amend {
            fields.push(read_only("Source", "Index".into()));
        }
        let replays = plan.steps.len();
        let onto = plan
            .base
            .map_or_else(|| "the root".to_owned(), history::short);
        fields.push(read_only(
            "Rebases",
            format!(
                "{replays} commit{} onto {onto}",
                if replays == 1 { "" } else { "s" }
            ),
        ));
        let runs = plan.runs();
        fields.push(read_only(
            "Runs",
            runs.last().map(Run::display).unwrap_or_default(),
        ));
        let title = format!("{} · {}?", edit.title(), history::short(target));
        // A reword starts in its message; every other sheet on its confirm button.
        let selected = if matches!(edit, Edit::Reword(_)) {
            0
        } else {
            fields.len()
        };
        self.op_form(title, Op::Rewrite(Box::new(plan)), fields, selected);
        Ok(())
    }

    /// The confirmed sheet's commands, built from its fields.
    pub(super) fn submit_op(&mut self, op: Op, values: &[String]) -> Result<(), String> {
        let (runs, timeout, done) = match op {
            Op::Pull => {
                let mode = values
                    .get(2)
                    .and_then(|label| PullMode::parse(label))
                    .ok_or("Choose merge, rebase or fast-forward only with Up/Down")?;
                let plan = history::pull(&self.root)?;
                (
                    vec![history::pull_run(mode)],
                    NETWORK,
                    format!(
                        "Pulled {} into {} ({})",
                        plan.upstream,
                        plan.branch,
                        mode.label()
                    ),
                )
            }
            Op::Undo(undo) => {
                // Recheck HEAD and staged changes before running the confirmed plan.
                if history::undo(&self.root, undo.redo)? != undo {
                    return Err("The reflog changed; cancel and press the key again".into());
                }
                let (_, to) = undo.head();
                let verb = if undo.redo { "Redid" } else { "Undid" };
                (
                    vec![undo.run()],
                    LOCAL,
                    format!("{verb} {} · HEAD at {to}", undo.entry),
                )
            }
            Op::Rewrite(mut plan) => {
                if let Edit::Reword(message) = &mut plan.edit {
                    let text = values.first().map_or("", |text| text.trim());
                    if text.is_empty() {
                        return Err("Message is required".into());
                    }
                    *message = text.to_owned();
                }
                let fresh = history::rewrite(&self.root, plan.target, plan.edit.clone())?;
                if fresh.todo != plan.todo {
                    return Err("The branch changed; cancel and press the key again".into());
                }
                (plan.runs(), LOCAL, plan.done())
            }
            Op::Copy(items) => {
                let chosen = values.first().map_or("", String::as_str);
                let (label, text) = items
                    .into_iter()
                    .find(|(label, _)| label == chosen)
                    .ok_or("Choose what to copy with Up/Down")?;
                self.outcome = Some(ScreenOutcome::Copy(text));
                let what = match label.as_str() {
                    "Short SHA" => "short SHA",
                    "Subject" => "subject",
                    other => other,
                };
                self.notice = format!("Copied the {what}");
                return Ok(());
            }
            Op::PushTag(tag) => {
                let remote = values.get(1).map_or("", |remote| remote.trim());
                (
                    vec![history::push_tag(remote, &tag)?],
                    NETWORK,
                    format!("Pushed tag {tag} to {remote}"),
                )
            }
        };
        let mut runs = runs.into_iter();
        let first = runs.next().ok_or("Nothing to run")?;
        self.jobs
            .start_write_env(&self.root, first.args, &first.env, timeout)?;
        self.busy = true;
        self.notice = "Running Git…".into();
        self.running = Some(Running {
            rest: runs.collect(),
            timeout,
            done,
            progress: None,
        });
        Ok(())
    }

    /// A write finished: start the sheet's next command, if it has one and this one
    /// succeeded. `true` while the sheet keeps running.
    pub(super) fn next_op_step(&mut self, result: &Result<String, String>) -> bool {
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        if result.is_err() || running.rest.is_empty() {
            return false;
        }
        let next = running.rest.remove(0);
        let timeout = running.timeout;
        running.progress = None;
        match self
            .jobs
            .start_write_env(&self.root, next.args, &next.env, timeout)
        {
            Ok(()) => true,
            Err(error) => {
                self.running = None;
                self.busy = false;
                self.notice = error;
                let refreshed = self.refresh();
                self.report(refreshed);
                false
            }
        }
    }

    /// The notice for a finished sheet: its receipt, or where a stopped rebase or merge
    /// waits. `None` when no sheet was running.
    pub(super) fn finish_op(&mut self, result: &Result<String, String>) -> Option<String> {
        let running = self.running.take()?;
        Some(match result {
            Ok(_) => running.done,
            Err(error) => {
                let state = git2::Repository::open(&self.root)
                    .map_or(git2::RepositoryState::Clean, |repository| {
                        repository.state()
                    });
                if state != git2::RepositoryState::Clean {
                    // Git has no skip for a merge (a pull that merges stops as one).
                    let skip = if state == git2::RepositoryState::Merge {
                        ""
                    } else {
                        "S skips, "
                    };
                    format!("Stopped on a conflict · resolve it, then C continues, {skip}X aborts")
                } else {
                    error.replace(['\r', '\n'], " ")
                }
            }
        })
    }

    /// While a sheet's command runs, the footer shows Git's last progress line.
    pub(super) fn op_progress(&mut self) -> bool {
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        let line = self.jobs.write_progress();
        if line.is_some() && line != running.progress {
            running.progress = line;
            return true;
        }
        false
    }

    /// The footer's line while Git runs.
    pub(super) fn busy_line(&self) -> &str {
        self.running
            .as_ref()
            .and_then(|running| running.progress.as_deref())
            .unwrap_or("Git operation running…")
    }
}

/// The web page of commit `id` on the forge behind the default remote: GitHub's and
/// Gitea's `/commit/<sha>`, GitLab's `/-/commit/<sha>`.
pub(super) fn commit_url(root: &Path, id: git2::Oid) -> Result<String, String> {
    let remote =
        history::default_remote(root).map_err(|_| "No remote to open the commit on".to_owned())?;
    let repository = git2::Repository::open(root).map_err(|error| error.message().to_owned())?;
    let remote = repository
        .find_remote(&remote)
        .map_err(|error| error.message().to_owned())?;
    let base = remote
        .url()
        .and_then(crate::web_base)
        .ok_or("The remote is not a web forge")?;
    let base = base.as_str().trim_end_matches('/').to_owned();
    let host = reqwest::Url::parse(&base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(if host.contains("gitlab") {
        format!("{base}/-/commit/{id}")
    } else {
        format!("{base}/commit/{id}")
    })
}
