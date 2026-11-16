//! Pull, undo and redo, history rewriting and tag push (DESIGN.md §5.11). Each is a plan the
//! Git tab shows in a confirm sheet, then runs as one `git` command with the arguments and
//! environment built here. Nothing in this module writes to the repository.
use git2::{Oid, Repository, RepositoryState};
use std::ffi::OsString;
use std::path::Path;

/// One `git` invocation: its arguments and the variables it adds to Git's environment.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Run {
    pub args: Vec<OsString>,
    pub env: Vec<(String, OsString)>,
}

impl Run {
    fn new<I: IntoIterator<Item = S>, S: Into<OsString>>(args: I) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
        }
    }

    fn with(mut self, key: &str, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// As a sheet shows it: `git reset --keep 1a2b3c4`, hashes shortened.
    pub fn display(&self) -> String {
        let words = self
            .args
            .iter()
            .filter(|arg| arg.to_str() != Some("-c") && !arg.to_string_lossy().starts_with("rebase.updateRefs"))
            .map(|arg| {
                let arg = arg.to_string_lossy();
                if arg.len() == 40 && arg.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    arg[..7].to_owned()
                } else {
                    arg.into_owned()
                }
            })
            .collect::<Vec<_>>();
        format!("git {}", words.join(" "))
    }
}

fn open(root: &Path) -> Result<Repository, String> {
    Repository::open(root).map_err(|error| error.message().to_owned())
}

fn failed(error: git2::Error) -> String {
    error.message().to_owned()
}

/// Refuses while a merge, rebase, cherry-pick, revert or bisect is paused.
fn settled(repository: &Repository) -> Result<(), String> {
    let name = match repository.state() {
        RepositoryState::Clean => return Ok(()),
        RepositoryState::Merge => "merge",
        RepositoryState::Revert | RepositoryState::RevertSequence => "revert",
        RepositoryState::CherryPick | RepositoryState::CherryPickSequence => "cherry-pick",
        RepositoryState::Bisect => "bisect",
        _ => "rebase",
    };
    Err(format!("A {name} is paused: continue or abort it first (C or X)"))
}

pub fn short(id: Oid) -> String {
    id.to_string()[..7].to_owned()
}

fn summary(commit: &git2::Commit) -> String {
    commit.summary().unwrap_or("").to_owned()
}

/// The current branch's short name; `None` on a detached HEAD.
fn branch(repository: &Repository) -> Result<Option<String>, String> {
    let head = repository.find_reference("HEAD").map_err(failed)?;
    Ok(head
        .symbolic_target()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned))
}

// ---------------------------------------------------------------------------------------
// Pull

/// How `git pull` joins the fetched branch to the local one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullMode {
    Merge,
    Rebase,
    FastForward,
}

impl PullMode {
    pub const ALL: [Self; 3] = [Self::Merge, Self::Rebase, Self::FastForward];

    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::FastForward => "fast-forward only",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.label() == label)
    }
}

/// `p`: the branch, its upstream and the mode its configuration asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pull {
    pub branch: String,
    pub upstream: String,
    pub mode: PullMode,
}

/// The pull the current branch is configured for: `branch.<name>.rebase`, then
/// `pull.rebase`, then `pull.ff = only`, as `git pull` reads them.
pub fn pull(root: &Path) -> Result<Pull, String> {
    let repository = open(root)?;
    settled(&repository)?;
    let branch = branch(&repository)?.ok_or("Pull needs a branch: HEAD is detached")?;
    let upstream = repository
        .branch_upstream_name(&format!("refs/heads/{branch}"))
        .map_err(|_| format!("{branch} has no upstream · u on the branch sets one"))?;
    let upstream = upstream.as_str().unwrap_or_default();
    let upstream = upstream
        .strip_prefix("refs/remotes/")
        .or_else(|| upstream.strip_prefix("refs/heads/"))
        .unwrap_or(upstream)
        .to_owned();
    let config = repository
        .config()
        .and_then(|mut config| config.snapshot())
        .map_err(failed)?;
    let rebase = |key: &str| {
        config.get_string(key).ok().map(|value| {
            !matches!(value.to_ascii_lowercase().as_str(), "false" | "no" | "off" | "0")
        })
    };
    let mode = match rebase(&format!("branch.{branch}.rebase")).or_else(|| rebase("pull.rebase")) {
        Some(true) => PullMode::Rebase,
        _ if config.get_string("pull.ff").is_ok_and(|value| value == "only") => {
            PullMode::FastForward
        }
        _ => PullMode::Merge,
    };
    Ok(Pull {
        branch,
        upstream,
        mode,
    })
}

/// `git pull` in a mode, with progress on stderr and no editor for a merge message.
pub fn pull_run(mode: PullMode) -> Run {
    let flags: &[&str] = match mode {
        PullMode::Merge => &["--no-rebase"],
        PullMode::Rebase => &["--rebase", "--autostash"],
        PullMode::FastForward => &["--ff-only"],
    };
    Run::new(["pull", "--progress"].iter().chain(flags).copied()).with("GIT_EDITOR", "true")
}

// ---------------------------------------------------------------------------------------
// Undo and redo

pub const UNDO_ACTION: &str = "[dusty undo]";
pub const REDO_ACTION: &str = "[dusty redo]";

/// What one reflog action moved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Move {
    /// A commit, reset, pull, merge or a whole rebase: HEAD went from one commit to another.
    Commit { from: Oid, to: Oid },
    /// A branch switch: from one branch (or detached commit) to another.
    Checkout { from: String, to: String },
}

/// `z` or `Z`: the reflog action to reverse (or replay) and how.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Undo {
    pub redo: bool,
    /// The reflog line of the action, as Git wrote it.
    pub entry: String,
    pub moved: Move,
}

impl Undo {
    /// Where HEAD is now and where it goes.
    pub fn head(&self) -> (String, String) {
        let (from, to) = match &self.moved {
            Move::Commit { from, to } => (short(*from), short(*to)),
            Move::Checkout { from, to } => (from.clone(), to.clone()),
        };
        if self.redo { (from, to) } else { (to, from) }
    }

    /// `git reset --keep` refuses rather than overwrite uncommitted changes; a branch switch
    /// switches back. The reflog entry is tagged so the next `z` walks further back.
    pub fn run(&self) -> Run {
        let action = if self.redo { REDO_ACTION } else { UNDO_ACTION };
        let run = match &self.moved {
            Move::Commit { from, to } => {
                let target = if self.redo { to } else { from };
                Run::new(["reset".into(), "--keep".into(), target.to_string()])
            }
            Move::Checkout { from, to } => {
                let target = if self.redo { to } else { from };
                Run::new(["checkout".into(), target.clone(), "--".into()])
            }
        };
        run.with("GIT_REFLOG_ACTION", action)
    }
}

/// A rebase's (or `pull --rebase`'s) step word: `start`, `pick`, `finish`, `abort`.
fn rebase_step(message: &str) -> Option<&str> {
    let head = message.split(':').next()?;
    if !(head.starts_with("rebase") || head.starts_with("pull")) {
        return None;
    }
    let open = head.rfind('(')?;
    head[open + 1..].strip_suffix(')')
}

/// The action `z` (or `Z`) reverses, walking HEAD's reflog as lazygit does: each
/// `[dusty undo]` entry skips one more action back, each `[dusty redo]` one less, and a
/// finished rebase counts as one action from its start to its end.
pub fn undo(root: &Path, redo: bool) -> Result<Undo, String> {
    let repository = open(root)?;
    settled(&repository)?;
    let reflog = repository.reflog("HEAD").map_err(failed)?;
    let mut counter = 0i32;
    let mut finish: Option<Oid> = None;
    for entry in reflog.iter() {
        let message = entry.message().unwrap_or("").to_owned();
        let (old, new) = (entry.id_old(), entry.id_new());
        let moved = if let Some(to) = finish {
            if rebase_step(&message) == Some("start") {
                finish = None;
                Some(Move::Commit { from: old, to })
            } else {
                None
            }
        } else if message.starts_with(UNDO_ACTION) {
            counter += 1;
            None
        } else if message.starts_with(REDO_ACTION) {
            counter -= 1;
            None
        } else if matches!(rebase_step(&message), Some("finish" | "abort")) {
            finish = Some(new);
            None
        } else if rebase_step(&message) == Some("start") {
            return Err("A rebase is in progress: continue or abort it first (C or X)".into());
        } else if let Some(rest) = message.strip_prefix("checkout: moving from ") {
            rest.split_once(" to ").map(|(from, to)| Move::Checkout {
                from: from.to_owned(),
                to: to.trim_end().to_owned(),
            })
        } else if ["commit", "reset: moving to", "pull", "merge", "cherry-pick", "revert"]
            .iter()
            .any(|prefix| message.starts_with(prefix))
        {
            Some(Move::Commit { from: old, to: new })
        } else {
            None
        };
        let Some(moved) = moved else { continue };
        let noop = match &moved {
            Move::Commit { from, to } => from == to,
            Move::Checkout { from, to } => from == to,
        };
        if noop {
            continue;
        }
        let fire = if redo {
            if counter <= 0 {
                return Err("Nothing to redo".into());
            }
            counter == 1
        } else {
            counter == 0
        };
        if fire {
            let undo = Undo {
                redo,
                entry: message.trim_end().to_owned(),
                moved,
            };
            if let Move::Commit { from, .. } = &undo.moved
                && from.is_zero()
                && !redo
            {
                return Err("The first commit has nothing before it to return to".into());
            }
            return Ok(undo);
        }
        counter -= 1;
    }
    Err(if redo { "Nothing to redo" } else { "Nothing to undo" }.into())
}

// ---------------------------------------------------------------------------------------
// History rewriting

/// What a History key does to the selected commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Edit {
    /// Into the commit below it (its parent), messages combined.
    Squash,
    /// Into the commit below it, keeping that commit's message.
    Fixup,
    Drop,
    /// Stop the rebase at the commit.
    Edit,
    Reword(String),
    /// Swap with the commit below (older).
    MoveDown,
    /// Swap with the commit above (newer).
    MoveUp,
    /// Fold the staged changes into the commit (a `fixup!` commit, then autosquash).
    Amend,
}

impl Edit {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Squash => "Squash into the commit below",
            Self::Fixup => "Fixup into the commit below",
            Self::Drop => "Drop",
            Self::Edit => "Edit (stop the rebase here)",
            Self::Reword(_) => "Reword",
            Self::MoveDown => "Move down",
            Self::MoveUp => "Move up",
            Self::Amend => "Amend with the staged changes",
        }
    }
}

/// A rebase of the current branch that rewrites one commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rewrite {
    pub edit: Edit,
    pub target: Oid,
    /// `1a2b3c4 subject` of the selected commit.
    pub commit: String,
    /// The other commit a squash, fixup or move involves.
    pub other: Option<String>,
    /// The commit the rebase replays onto; `None` rewrites from the root.
    pub base: Option<Oid>,
    /// The todo, oldest first, as `pick <hash>` lines.
    pub todo: Vec<String>,
    /// For a sheet: the todo with short hashes and subjects.
    pub steps: Vec<String>,
}

fn line(commit: &git2::Commit) -> String {
    format!("{} {}", short(commit.id()), summary(commit))
}

/// Plans `edit` on `target`, which must be on HEAD's first-parent line with no merge
/// commit between it and HEAD.
pub fn rewrite(root: &Path, target: Oid, edit: Edit) -> Result<Rewrite, String> {
    let repository = open(root)?;
    settled(&repository)?;
    let head = repository
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|_| "The branch has no commits yet")?;
    // Newest first, HEAD down to the target.
    let mut chain = Vec::new();
    let mut at = Some(head);
    while let Some(commit) = at {
        if commit.parent_count() > 1 {
            return Err(format!(
                "{} is a merge commit: rewriting across merges is not supported",
                short(commit.id())
            ));
        }
        let found = commit.id() == target;
        at = commit.parent(0).ok();
        chain.push(commit);
        if found {
            break;
        }
        if chain.len() > 100_000 {
            break;
        }
    }
    if chain.last().map(|commit| commit.id()) != Some(target) {
        return Err("The selected commit is not on the current branch".into());
    }
    let index = chain.len() - 1;
    let selected = &chain[index];
    let parent = selected.parent(0).ok();
    if parent.as_ref().is_some_and(|parent| parent.parent_count() > 1) {
        return Err("The commit below is a merge commit: rewriting across merges is not supported".into());
    }
    let reaches_parent = matches!(edit, Edit::Squash | Edit::Fixup | Edit::MoveDown);
    if reaches_parent && parent.is_none() {
        return Err("The first commit has no commit below it".into());
    }
    if edit == Edit::MoveUp && index == 0 {
        return Err("The newest commit has no commit above it".into());
    }
    if edit == Edit::Amend {
        let staged = repository
            .head()
            .and_then(|head| head.peel_to_tree())
            .and_then(|tree| {
                repository.diff_tree_to_index(Some(&tree), None, None)
            })
            .map_err(failed)?;
        if staged.deltas().count() == 0 {
            return Err("Nothing staged · s stages the selected change".into());
        }
    }
    // The commits the rebase replays, oldest first.
    let mut commits: Vec<&git2::Commit> = chain.iter().rev().collect();
    let base = if reaches_parent {
        let parent = parent.as_ref().expect("checked above");
        commits.insert(0, parent);
        parent.parent(0).ok().map(|commit| commit.id())
    } else {
        parent.as_ref().map(|commit| commit.id())
    };
    let at = if reaches_parent { 1 } else { 0 };
    let mut verbs = vec!["pick"; commits.len()];
    match &edit {
        Edit::Squash => verbs[at] = "squash",
        Edit::Fixup => verbs[at] = "fixup",
        Edit::Drop => verbs[at] = "drop",
        Edit::Edit => verbs[at] = "edit",
        Edit::Reword(_) => verbs[at] = "reword",
        Edit::MoveDown => commits.swap(0, 1),
        Edit::MoveUp => commits.swap(0, 1),
        Edit::Amend => {}
    }
    let other = match &edit {
        Edit::Squash | Edit::Fixup | Edit::MoveDown => parent.as_ref().map(line),
        Edit::MoveUp => Some(line(&chain[index - 1])),
        _ => None,
    };
    let todo = commits
        .iter()
        .zip(&verbs)
        .map(|(commit, verb)| format!("{verb} {}", commit.id()))
        .collect();
    let steps = commits
        .iter()
        .zip(&verbs)
        .map(|(commit, verb)| format!("{verb} {}", line(commit)))
        .collect();
    Ok(Rewrite {
        commit: line(selected),
        edit,
        target,
        other,
        base,
        todo,
        steps,
    })
}

impl Rewrite {
    fn rebase(&self, autosquash: bool) -> Run {
        let mut args: Vec<OsString> = ["-c", "rebase.updateRefs=false", "rebase", "-i", "--autostash"]
            .into_iter()
            .map(Into::into)
            .collect();
        args.push(if autosquash { "--autosquash" } else { "--no-autosquash" }.into());
        args.push(match self.base {
            Some(base) => base.to_string().into(),
            None => "--root".into(),
        });
        Run { args, env: Vec::new() }
    }

    /// The commands, in order: a rebase whose todo Dusty Git writes through
    /// `GIT_SEQUENCE_EDITOR`; an amend first commits the staged changes as `fixup!`.
    pub fn runs(&self) -> Vec<Run> {
        let editor = |run: Run| {
            let message = match &self.edit {
                Edit::Reword(message) => Some(message.clone()),
                _ => None,
            };
            match message {
                // The sequencer opens the editor once, for the reworded commit's message.
                Some(message) => run
                    .with("GIT_EDITOR", "printf '%s\\n' \"$DUSTY_MESSAGE\" >")
                    .with("DUSTY_MESSAGE", message),
                None => run.with("GIT_EDITOR", "true"),
            }
        };
        if self.edit == Edit::Amend {
            return vec![
                Run::new(["commit".into(), format!("--fixup={}", self.target)])
                    .with("GIT_EDITOR", "true"),
                self.rebase(true)
                    .with("GIT_SEQUENCE_EDITOR", "true")
                    .with("GIT_EDITOR", "true"),
            ];
        }
        let mut todo = self.todo.join("\n");
        todo.push('\n');
        vec![editor(
            self.rebase(false)
                .with("GIT_SEQUENCE_EDITOR", "printf '%s' \"$DUSTY_TODO\" >")
                .with("DUSTY_TODO", todo),
        )]
    }

    /// The notice once the rebase finished.
    pub fn done(&self) -> String {
        let commit = &self.commit;
        match &self.edit {
            Edit::Squash => format!("Squashed {commit} into the commit below"),
            Edit::Fixup => format!("Fixed {commit} up into the commit below"),
            Edit::Drop => format!("Dropped {commit}"),
            Edit::Edit => format!("Stopped at {commit} · amend it, then C continues, X aborts"),
            Edit::Reword(_) => format!("Reworded {commit}"),
            Edit::MoveDown => format!("Moved {commit} down"),
            Edit::MoveUp => format!("Moved {commit} up"),
            Edit::Amend => format!("Amended {commit} with the staged changes"),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Tag push

/// The remote a tag push goes to first: the branch's upstream remote, `origin`, or the only
/// remote.
pub fn default_remote(root: &Path) -> Result<String, String> {
    let repository = open(root)?;
    let remotes = repository.remotes().map_err(failed)?;
    let remotes = remotes.iter().flatten().map(str::to_owned).collect::<Vec<_>>();
    if remotes.is_empty() {
        return Err("No remotes to push the tag to".into());
    }
    let upstream = branch(&repository)?.and_then(|branch| {
        repository
            .branch_upstream_remote(&format!("refs/heads/{branch}"))
            .ok()
            .and_then(|remote| remote.as_str().map(str::to_owned))
    });
    Ok(upstream
        .filter(|remote| remotes.contains(remote))
        .or_else(|| remotes.iter().find(|remote| *remote == "origin").cloned())
        .unwrap_or_else(|| remotes[0].clone()))
}

/// `git push <remote> refs/tags/<tag>:refs/tags/<tag>`: the tag only, never a branch.
pub fn push_tag(remote: &str, tag: &str) -> Result<Run, String> {
    if remote.is_empty() || remote.starts_with('-') {
        return Err("Choose a remote".into());
    }
    let reference = format!("refs/tags/{tag}");
    if tag.starts_with('-') || !git2::Reference::is_valid_name(&reference) {
        return Err(format!("{tag} is not a valid tag name"));
    }
    Ok(Run::new([
        "push".into(),
        "--progress".into(),
        "--".into(),
        remote.to_owned(),
        format!("{reference}:{reference}"),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        git_env(root, args, &[])
    }

    fn git_env(root: &Path, args: &[&str], env: &[(String, OsString)]) -> String {
        let mut command = Command::new("git");
        command
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_REFLOG_ACTION");
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim_end().to_owned()
    }

    fn run(root: &Path, run: &Run) {
        let args = run.args.iter().map(|arg| arg.to_str().unwrap()).collect::<Vec<_>>();
        git_env(root, &args, &run.env);
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "--template=", "-b", "main"]);
        git(root, &["config", "user.name", "History"]);
        git(root, &["config", "user.email", "history@example.invalid"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        git(root, &["config", "core.autocrlf", "false"]);
        dir
    }

    fn commit(root: &Path, name: &str) -> Oid {
        fs::write(root.join(name), format!("{name}\n")).unwrap();
        git(root, &["add", "--", name]);
        git(root, &["commit", "-q", "-m", name]);
        head(root)
    }

    fn head(root: &Path) -> Oid {
        Oid::from_str(&git(root, &["rev-parse", "HEAD"])).unwrap()
    }

    fn subjects(root: &Path) -> String {
        git(root, &["log", "--format=%s"]).replace('\n', " ")
    }

    #[test]
    fn undo_walks_back_one_action_at_a_time_and_redo_walks_forward() {
        let dir = repo();
        let root = dir.path();
        let a = commit(root, "a");
        let b = commit(root, "b");
        let c = commit(root, "c");
        let first = undo(root, false).unwrap();
        assert_eq!(first.moved, Move::Commit { from: b, to: c });
        assert_eq!(first.entry, "commit: c");
        assert_eq!(first.head(), (short(c), short(b)));
        assert!(matches!(undo(root, true), Err(message) if message == "Nothing to redo"));
        run(root, &first.run());
        assert_eq!(head(root), b);
        let second = undo(root, false).unwrap();
        assert_eq!(second.moved, Move::Commit { from: a, to: b });
        run(root, &second.run());
        assert_eq!(head(root), a);
        assert!(undo(root, false).is_err(), "the first commit has nothing before it");
        let again = undo(root, true).unwrap();
        assert_eq!(again.head(), (short(a), short(b)));
        run(root, &again.run());
        assert_eq!(head(root), b);
        run(root, &undo(root, true).unwrap().run());
        assert_eq!(head(root), c);
        assert!(undo(root, true).is_err());
        // Undo after the redos starts from the newest action again.
        run(root, &undo(root, false).unwrap().run());
        assert_eq!(head(root), b);
    }

    #[test]
    fn undo_switches_back_and_treats_a_rebase_as_one_action() {
        let dir = repo();
        let root = dir.path();
        commit(root, "a");
        git(root, &["switch", "-q", "-c", "topic"]);
        let undo_switch = undo(root, false).unwrap();
        assert_eq!(
            undo_switch.moved,
            Move::Checkout { from: "main".into(), to: "topic".into() }
        );
        run(root, &undo_switch.run());
        assert_eq!(git(root, &["branch", "--show-current"]), "main");
        run(root, &undo(root, true).unwrap().run());
        assert_eq!(git(root, &["branch", "--show-current"]), "topic");
        commit(root, "b");
        let before = commit(root, "c");
        let plan = rewrite(root, before, Edit::MoveDown).unwrap();
        for step in plan.runs() {
            run(root, &step);
        }
        assert_eq!(subjects(root), "b c a");
        let rebase = undo(root, false).unwrap();
        assert!(rebase.entry.starts_with("rebase"), "{}", rebase.entry);
        run(root, &rebase.run());
        assert_eq!(head(root), before);
        assert_eq!(subjects(root), "c b a");
    }

    #[test]
    fn each_history_edit_rewrites_the_selected_commit() {
        let dir = repo();
        let root = dir.path();
        commit(root, "a");
        let b = commit(root, "b");
        commit(root, "c");
        let cases: [(Edit, &str); 6] = [
            (Edit::Drop, "c a"),
            (Edit::Squash, "c a"),
            (Edit::Fixup, "c a"),
            (Edit::MoveDown, "c a b"),
            (Edit::MoveUp, "b c a"),
            (Edit::Reword("bee\n\nbody".into()), "c bee a"),
        ];
        let start = head(root);
        for (edit, expected) in cases {
            git(root, &["reset", "-q", "--hard", &start.to_string()]);
            let plan = rewrite(root, b, edit.clone()).unwrap();
            for step in plan.runs() {
                run(root, &step);
            }
            assert_eq!(subjects(root), expected, "{edit:?}");
            if edit == Edit::Squash {
                let message = git(root, &["log", "-1", "--format=%B", "HEAD~1"]);
                assert!(message.contains('a') && message.contains('b'), "{message}");
            }
        }
        git(root, &["reset", "-q", "--hard", &start.to_string()]);
        fs::write(root.join("a"), "amended\n").unwrap();
        git(root, &["add", "a"]);
        fs::write(root.join("c"), "unstaged\n").unwrap();
        let plan = rewrite(root, b, Edit::Amend).unwrap();
        for step in plan.runs() {
            run(root, &step);
        }
        assert_eq!(subjects(root), "c b a");
        assert_eq!(git(root, &["show", "HEAD~1:a"]), "amended");
        assert_eq!(fs::read_to_string(root.join("c")).unwrap(), "unstaged\n");
        assert!(rewrite(root, head(root), Edit::MoveUp).is_err());
    }

    #[test]
    fn the_root_commit_rebases_from_the_root_and_merges_are_refused() {
        let dir = repo();
        let root = dir.path();
        let a = commit(root, "a");
        let b = commit(root, "b");
        let plan = rewrite(root, b, Edit::Squash).unwrap();
        assert_eq!(plan.base, None);
        assert!(plan.runs()[0].args.contains(&OsString::from("--root")));
        assert!(rewrite(root, a, Edit::Squash).is_err());
        git(root, &["switch", "-q", "-c", "side", &a.to_string()]);
        commit(root, "side");
        git(root, &["switch", "-q", "main"]);
        git(root, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        assert!(rewrite(root, b, Edit::Drop).unwrap_err().contains("merge"));
    }

    #[test]
    fn pull_reads_the_branch_then_the_pull_configuration() {
        let dir = repo();
        let root = dir.path();
        commit(root, "a");
        assert!(pull(root).unwrap_err().contains("no upstream"));
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(root, &["remote", "add", "origin", remote.path().to_str().unwrap()]);
        git(root, &["push", "-q", "-u", "origin", "main"]);
        let plan = pull(root).unwrap();
        assert_eq!((plan.branch.as_str(), plan.upstream.as_str()), ("main", "origin/main"));
        assert_eq!(plan.mode, PullMode::Merge);
        git(root, &["config", "pull.ff", "only"]);
        assert_eq!(pull(root).unwrap().mode, PullMode::FastForward);
        git(root, &["config", "pull.rebase", "true"]);
        assert_eq!(pull(root).unwrap().mode, PullMode::Rebase);
        git(root, &["config", "branch.main.rebase", "false"]);
        assert_eq!(pull(root).unwrap().mode, PullMode::FastForward);
        assert_eq!(default_remote(root).unwrap(), "origin");
        assert!(push_tag("origin", "-x").is_err());
        assert_eq!(
            push_tag("origin", "v1").unwrap().display(),
            "git push --progress -- origin refs/tags/v1:refs/tags/v1"
        );
    }
}
