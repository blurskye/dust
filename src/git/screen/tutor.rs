//! The tutor operates on an owned temporary repository and a local bare remote.
//! The user's entire Git screen stays intact while the practice screen is open.
use super::*;
use git2::{Oid, Repository, RepositoryInitOptions, Signature};
use tempfile::TempDir;

const SOURCE: &str = "src/greet.rs";
const NOTES: &str = "Practice notes: save this in a stash, then pop it back.\n";

pub(super) struct Session {
    directory: TempDir,
    saved: Box<GitScreen>,
    initial: Oid,
    step: usize,
    saw_all: bool,
    saw_stash: bool,
    failed: bool,
}
impl Session {
    pub fn title(&self) -> String {
        if self.failed {
            return "Git tutor · practice setup failed".into();
        }
        if self.step == 6 {
            return "Git tutor · completed all six lessons".into();
        }
        format!(
            "Git tutor · {}/6 · {}",
            self.step + 1,
            [
                "Files",
                "Stage",
                "Commit and push",
                "Worktrees",
                "Stashes",
                "Conflict resolution"
            ][self.step]
        )
    }
    pub fn instruction(&self) -> &'static str {
        if self.failed {
            return "Read the error below. Use :tutor quit to return to your saved workspace.";
        }
        [
            "Press v for All files; h/l folds folders. Read the highlighted source, then v returns to Changes.",
            "Select src/greet.rs and press Space to stage it. i switches between working-tree and index previews.",
            "Press c, enter a message and Ctrl+Enter to commit. Then P and Enter push to the local practice remote.",
            "Press 2, L for Worktrees, then n. Use a new branch and the suggested path. Select that worktree and Enter.",
            "The new worktree has practice notes. Press 4, n and save a stash. Select it, then p and Enter to pop it back.",
            "Select src/greet.rs. Enter chooses a conflict side; Ctrl+Enter applies it. Space stages the result, then C and Enter finish the merge.",
            "You used the file tree, staging, commit/push, worktrees, stash/restore and a real merge conflict. q returns to your saved workspace.",
        ][self.step]
    }
    pub fn check_destination(&self, path: &Path) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or("Choose a practice worktree path")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if parent.starts_with(self.directory.path()) && path.file_name().is_some() {
            Ok(())
        } else {
            Err("Tutor worktrees must stay inside the disposable practice directory".into())
        }
    }
    fn advance(&mut self, screen: &mut GitScreen) -> Result<bool, String> {
        if self.failed {
            return Ok(false);
        }
        let mut repo = Repository::open(&screen.root).map_err(|error| error.to_string())?;
        let complete = match self.step {
            0 => {
                self.saw_all |= screen.all_files;
                self.saw_all && !screen.all_files
            }
            1 => {
                let head = repo
                    .head()
                    .and_then(|head| head.peel_to_tree())
                    .map_err(|error| error.to_string())?;
                repo.diff_tree_to_index(Some(&head), None, None)
                    .map_err(|error| error.to_string())?
                    .deltas()
                    .len()
                    > 0
            }
            2 => {
                let head = repo.head().ok().and_then(|head| head.target());
                let remote = Repository::open_bare(self.directory.path().join("remote.git"))
                    .map_err(|error| error.to_string())?;
                head != Some(self.initial)
                    && head.is_some()
                    && remote
                        .find_reference("refs/heads/practice")
                        .ok()
                        .and_then(|reference| reference.target())
                        == head
            }
            3 if repo.is_worktree() => {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(screen.root.join("notes.md"))
                    .map_err(|error| error.to_string())?;
                file.write_all(NOTES.as_bytes())
                    .map_err(|error| error.to_string())?;
                true
            }
            4 => {
                let mut count = 0;
                repo.stash_foreach(|_, _, _| {
                    count += 1;
                    true
                })
                .map_err(|error| error.to_string())?;
                self.saw_stash |= count > 0;
                if self.saw_stash
                    && count == 0
                    && std::fs::read_to_string(screen.root.join("notes.md"))
                        .ok()
                        .as_deref()
                        == Some(NOTES)
                {
                    prepare_conflict(&repo).map_err(|error| error.to_string())?;
                    true
                } else {
                    false
                }
            }
            5 => {
                repo.state() == git2::RepositoryState::Clean
                    && repo
                        .head()
                        .and_then(|head| head.peel_to_commit())
                        .is_ok_and(|commit| commit.parent_count() == 2)
            }
            _ => false,
        };
        if complete {
            self.step += 1;
            screen.refresh()?;
        }
        Ok(complete)
    }
}

fn identity(repo: &Repository, hooks: &Path) -> Result<(), git2::Error> {
    let mut config = repo.config()?;
    config.set_str("user.name", "Dusty Git Tutor")?;
    config.set_str("user.email", "tutor@example.invalid")?;
    config.set_bool("commit.gpgsign", false)?;
    config.set_bool("core.fsmonitor", false)?;
    config.set_str("core.hooksPath", &hooks.to_string_lossy())
}
fn prepare() -> Result<(TempDir, PathBuf, Oid), String> {
    let directory = tempfile::Builder::new()
        .prefix("dusty-git-tutor-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let root = directory.path().join("practice");
    let hooks = directory.path().join("hooks");
    std::fs::create_dir(&hooks).map_err(|error| error.to_string())?;
    let remote_path = directory.path().join("remote.git");
    let remote = Repository::init_bare(&remote_path).map_err(|error| error.to_string())?;
    identity(&remote, &hooks).map_err(|error| error.to_string())?;
    let repo = Repository::init_opts(&root, RepositoryInitOptions::new().initial_head("practice"))
        .map_err(|error| error.to_string())?;
    identity(&repo, &hooks).map_err(|error| error.to_string())?;
    std::fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    std::fs::write(
        root.join(SOURCE),
        "pub fn greet() -> &'static str {\n    \"Hello\"\n}\n",
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(
        root.join("README.md"),
        "# Dusty Git practice\n\nThis repository and its remote are disposable.\n",
    )
    .map_err(|error| error.to_string())?;
    let initial = (|| -> Result<Oid, git2::Error> {
        let mut index = repo.index()?;
        index.add_path(Path::new(SOURCE))?;
        index.add_path(Path::new("README.md"))?;
        index.write()?;
        let tree = repo.find_tree(index.write_tree()?)?;
        let signature = Signature::now("Dusty Git Tutor", "tutor@example.invalid")?;
        let initial = repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Start Git practice",
            &tree,
            &[],
        )?;
        repo.branch("main", &repo.find_commit(initial)?, false)?;
        repo.remote("origin", &remote_path.to_string_lossy())?;
        let mut config = repo.config()?;
        config.set_str("branch.practice.remote", "origin")?;
        config.set_str("branch.practice.merge", "refs/heads/practice")?;
        Ok(initial)
    })()
    .map_err(|error| error.to_string())?;
    std::fs::write(
        root.join(SOURCE),
        "pub fn greet() -> &'static str {\n    \"Hello, Dusty Git!\"\n}\n",
    )
    .map_err(|error| error.to_string())?;
    Ok((directory, root, initial))
}
fn prepare_conflict(repo: &Repository) -> Result<(), git2::Error> {
    let parent = repo.head()?.peel_to_commit()?;
    let signature = Signature::now("Dusty Git Tutor", "tutor@example.invalid")?;
    let version = |reference: &str, text: &str| -> Result<Oid, git2::Error> {
        let mut index = git2::Index::new()?;
        index.read_tree(&parent.tree()?)?;
        let mut entry = index
            .get_path(Path::new(SOURCE), 0)
            .ok_or_else(|| git2::Error::from_str("Practice source is missing"))?;
        entry.id = repo.blob(text.as_bytes())?;
        index.add(&entry)?;
        let tree = repo.find_tree(index.write_tree_to(repo)?)?;
        repo.commit(
            Some(reference),
            &signature,
            &signature,
            "Practice conflicting greeting",
            &tree,
            &[&parent],
        )
    };
    let ours = "pub fn greet() -> &'static str {\n    \"Hello from our branch\"\n}\n";
    let theirs = "pub fn greet() -> &'static str {\n    \"Hello from their branch\"\n}\n";
    let other = version("refs/heads/tutor-conflict", theirs)?;
    let current = version("HEAD", ours)?;
    let mut index = repo.index()?;
    index.read_tree(&repo.find_commit(current)?.tree()?)?;
    index.write()?;
    std::fs::write(
        repo.workdir()
            .ok_or_else(|| git2::Error::from_str("Practice worktree is missing"))?
            .join(SOURCE),
        ours,
    )
    .map_err(|error| git2::Error::from_str(&error.to_string()))?;
    repo.merge(&[&repo.find_annotated_commit(other)?], None, None)
}

impl GitScreen {
    pub fn in_tutor(&self) -> bool {
        self.tutor.is_some()
    }
    pub fn resume(&mut self) {
        let result = self.refresh();
        self.report(result);
    }
    pub(super) fn start_tutor(&mut self) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the current operation before starting the tutor".into());
        }
        if self.tutor.is_some() {
            return Err("The Git tutor is already running".into());
        }
        let (directory, root, initial) = prepare()?;
        let next = GitScreen::open(&root)?;
        self.form = None;
        let saved = Box::new(std::mem::replace(self, next));
        self.tutor = Some(Session {
            directory,
            saved,
            initial,
            step: 0,
            saw_all: false,
            saw_stash: false,
            failed: false,
        });
        Ok(())
    }
    pub(super) fn stop_tutor(&mut self) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the practice operation before leaving the tutor".into());
        }
        let session = self.tutor.take().ok_or("The Git tutor is not running")?;
        let Session {
            directory, saved, ..
        } = session;
        *self = *saved;
        drop(directory);
        Ok(())
    }
    pub(super) fn advance_tutor(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        let Some(mut session) = self.tutor.take() else {
            return false;
        };
        let changed = match session.advance(self) {
            Ok(changed) => changed,
            Err(error) => {
                session.failed = true;
                self.notice = format!("Tutor: {error}");
                true
            }
        };
        self.tutor = Some(session);
        changed
    }
    pub(super) fn tutor_worktree(&mut self, path: &Path) -> Result<(), String> {
        let session = self.tutor.as_ref().ok_or("The Git tutor is not running")?;
        if !path
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(session.directory.path())
        {
            return Err("Tutor navigation stays inside the practice directory".into());
        }
        let mut next = GitScreen::open(path)?;
        next.tutor = self.tutor.take();
        *self = next;
        self.advance_tutor();
        Ok(())
    }
}
