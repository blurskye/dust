//! Identity-bound local commits. Git owns commit creation (hooks and signing);
//! publication is a separately locked update, never a reset or checkout.
use git2::{Oid, Repository, RepositoryState};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Amend,
    Reword,
    Fixup(Oid),
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub kind: Kind,
    pub head: Oid,
    pub reference: Option<String>,
    pub message: String,
    root: PathBuf,
    gitdir: PathBuf,
    index_path: PathBuf,
    index: Vec<u8>,
    index_hash: [u8; 32],
    tree: Oid,
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn ready(repo: &Repository) -> Result<(), String> {
    if repo.workdir().is_none() || repo.state() != RepositoryState::Clean {
        return Err("Finish the current Git operation before rewriting history".into());
    }
    if repo.index().map_err(error)?.has_conflicts() {
        return Err("Resolve the index conflicts first".into());
    }
    Ok(())
}
fn identity(repo: &Repository) -> Result<(Oid, Option<String>), String> {
    let raw = repo.find_reference("HEAD").map_err(error)?;
    let reference = raw.symbolic_target().map(str::to_owned);
    if reference.as_ref().is_some_and(|name| !name.starts_with("refs/heads/")) {
        return Err("HEAD must name a local branch or be detached".into());
    }
    let head = repo.head().and_then(|head| head.peel_to_commit())
        .map_err(|_| "HEAD has no commit to rewrite".to_owned())?;
    let direct = match &reference {
        Some(name) => repo.find_reference(name).map_err(error)?.target(),
        None => raw.target(),
    };
    if direct != Some(head.id()) {
        return Err("Indirect HEAD references are not supported; use Git directly".into());
    }
    Ok((head.id(), reference))
}
impl Plan {
    pub fn capture(root: &Path, kind: Kind) -> Result<Self, String> {
        let repo = Repository::open(root).map_err(error)?;
        ready(&repo)?;
        let (head, reference) = identity(&repo)?;
        let commit = repo.find_commit(head).map_err(error)?;
        let mut index = repo.index().map_err(error)?;
        let index_path = index.path().ok_or("The repository has no on-disk index")?.to_path_buf();
        let bytes = fs::read(&index_path).map_err(error)?;
        let tree = if kind == Kind::Reword {
            commit.tree_id()
        } else {
            index.write_tree_to(&repo).map_err(error)?
        };
        if matches!(kind, Kind::Fixup(_)) && tree == commit.tree_id() {
            return Err("Stage changes before creating a fixup commit".into());
        }
        let message = match kind {
            Kind::Fixup(target) => {
                let target = repo.find_commit(target).map_err(error)?;
                if target.id() != head && !repo.graph_descendant_of(head, target.id()).map_err(error)? {
                    return Err("Select a commit in the current HEAD history".into());
                }
                format!("fixup! {}", target.summary().unwrap_or_default())
            }
            _ => std::str::from_utf8(commit.message_bytes())
                .map_err(|_| "The commit message is not UTF-8; use Git to edit it")?.to_owned(),
        };
        let plan = Self {
            kind, head, reference, message,
            root: repo.workdir().ok_or("No working tree")?.to_path_buf(),
            gitdir: repo.path().to_path_buf(), index_path,
            index_hash: Sha256::digest(&bytes).into(), index: bytes, tree,
        };
        plan.validate(&repo)?;
        Ok(plan)
    }
    fn validate(&self, repo: &Repository) -> Result<(), String> {
        ready(repo)?;
        if repo.path() != self.gitdir || identity(repo)? != (self.head, self.reference.clone()) {
            return Err("HEAD or its branch changed; cancel and reopen this action".into());
        }
        let bytes = fs::read(&self.index_path).map_err(error)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != self.index_hash {
            return Err("The index changed; cancel and reopen this action".into());
        }
        Ok(())
    }
}

/// The worker retains ownership through hooks, signing and publication, even if
/// its UI receiver goes away. Dropping a screen never kills a commit half-way.
#[derive(Default)]
pub struct Worker {
    pending: Option<Receiver<Result<Outcome, String>>>,
}
#[derive(Debug)]
pub struct Outcome {
    pub kind: Kind,
    pub head: Oid,
    pub recovery: String,
}
impl Worker {
    pub fn start(&mut self, plan: Plan, message: String) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("A commit is already running".into());
        }
        if message.trim().is_empty() || message.contains('\0') {
            return Err("Enter a nonempty commit message without NUL bytes".into());
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new().name("git-rewrite".into()).spawn(move || {
            let result = execute(plan, message);
            let _ = sender.send(result);
        }).map_err(error)?;
        self.pending = Some(receiver);
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Result<Outcome, String>> {
        match self.pending.as_ref()?.try_recv() {
            Ok(result) => { self.pending = None; Some(result) }
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                Some(Err("Commit worker stopped; inspect Git status and recovery refs".into()))
            }
            Err(TryRecvError::Empty) => None,
        }
    }
}

struct IndexLock { path: PathBuf, file: fs::File }
impl Drop for IndexLock {
    fn drop(&mut self) {
        let owned = crate::files::file_identity(&self.file).ok();
        let current = fs::File::open(&self.path).ok()
            .and_then(|file| crate::files::file_identity(&file).ok());
        if owned.is_some() && owned == current {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn execute(plan: Plan, message: String) -> Result<Outcome, String> {
    let repo = Repository::open(&plan.root).map_err(error)?;
    plan.validate(&repo)?;
    // Never delete somebody else's lock, even on failure.
    let lock_path = plan.index_path.with_file_name("index.lock");
    let lock_file = fs::OpenOptions::new().write(true).create_new(true).open(&lock_path)
        .map_err(|e| format!("Cannot lock the index: {e}"))?;
    let _index_lock = IndexLock { path: lock_path, file: lock_file };
    let mut transaction = repo.transaction().map_err(error)?;
    transaction.lock_ref("HEAD").map_err(error)?;
    if let Some(reference) = &plan.reference {
        transaction.lock_ref(reference).map_err(error)?;
    }
    plan.validate(&repo)?;
    let old = repo.find_commit(plan.head).map_err(error)?;
    let recovery = format!("refs/dust/rewrite/{}", plan.head);
    // Resolve commit/signing configuration against the original checkout.
    // includeIf.onbranch/gitdir must not silently disable signing in the private HEAD.
    let effective = run_git(&plan.root, super::command::environment(),
        ["config", "--null", "--list", "--includes"].into_iter().map(Into::into).collect(), None)?;
    let mut overrides = Vec::new();
    let mut has_hooks_path = false;
    for record in effective.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
        let record = std::str::from_utf8(record).map_err(|_| "Git configuration is not UTF-8; use Git directly")?;
        let (key, value) = record.split_once('\n').map_or((record, None), |(key, value)| (key, Some(value)));
        let lower = key.to_ascii_lowercase();
        if lower == "core.hookspath" {
            has_hooks_path = true;
        } else if ["commit.", "gpg.", "user.", "i18n."].iter().any(|prefix| lower.starts_with(*prefix))
            || matches!(lower.as_str(), "core.commentchar" | "core.commentstring")
        {
            overrides.push("-c".into());
            overrides.push(value.map_or_else(|| key.to_owned(), |value| format!("{key}={value}")).into());
        }
    }
    let hooks = if has_hooks_path {
        let path = run_git(&plan.root, super::command::environment(),
            ["config", "--null", "--path", "--get", "core.hooksPath"].into_iter().map(Into::into).collect(), None)?;
        let path = path.strip_suffix(&[0]).ok_or("Invalid Git hooks path response")?;
        PathBuf::from(std::str::from_utf8(path).map_err(|_| "Git hooks path is not UTF-8; use Git directly")?)
    } else { repo.commondir().join("hooks") };
    let resolved_hooks = if hooks.is_absolute() { hooks.clone() } else { plan.root.join(&hooks) };
    let reference_hook = resolved_hooks.join("reference-transaction");
    no_reference_hook(&reference_hook)?;
    // libgit2 publication cannot invoke Git's reference-transaction policy hook.
    // Refuse that configuration rather than silently bypassing a local policy.
    repo.reference(&recovery, plan.head, false, "dust: before local rewrite")
        .or_else(|e| {
            if repo.find_reference(&recovery).ok().and_then(|r| r.target()) == Some(plan.head) {
                repo.find_reference(&recovery)
            } else { Err(e) }
        }).map_err(error)?;

    // A private detached HEAD keeps commit hooks/signing in native Git while
    // preventing Git from publishing against a different HEAD mid-command.
    // Shared objects/config, original worktree, isolated index and HEAD.
    let private = tempfile::Builder::new().prefix("dust-rewrite-")
        .tempdir_in(repo.path()).map_err(error)?;
    let common = repo.commondir().canonicalize().map_err(error)?;
    fs::write(private.path().join("commondir"), common.to_str()
        .ok_or("The common Git directory is not UTF-8")?).map_err(error)?;
    fs::write(private.path().join("HEAD"), format!("{}\n", plan.head)).map_err(error)?;
    fs::create_dir(private.path().join("logs")).map_err(error)?;
    let worktree_config = repo.path().join("config.worktree");
    if worktree_config.exists() {
        fs::copy(worktree_config, private.path().join("config.worktree")).map_err(error)?;
    }
    let private_index = private.path().join("index");
    fs::write(&private_index, &plan.index).map_err(error)?;
    let mut environment = super::command::environment();
    environment.insert("GIT_DIR".into(), private.path().as_os_str().to_owned());
    environment.insert("GIT_WORK_TREE".into(), plan.root.as_os_str().to_owned());
    environment.insert("GIT_INDEX_FILE".into(), private_index.as_os_str().to_owned());
    environment.insert("GIT_EDITOR".into(), "true".into());
    // Resolve relative hooks paths as Git would from the real worktree. The
    // default path must not accidentally become the private gitdir's hooks.
    let mut args = overrides;
    args.extend(["-c".into(), {
        let mut option = std::ffi::OsString::from("core.hooksPath=");
        option.push(hooks);
        option
    }, "commit".into()]);
    match plan.kind {
        Kind::Amend => args.extend(["--amend".into(), "--allow-empty".into()]),
        Kind::Reword => args.extend(["--amend".into(), "--only".into(), "--allow-empty".into()]),
        Kind::Fixup(target) => args.push(format!("--fixup={target}").into()),
    }
    if !matches!(plan.kind, Kind::Fixup(_)) {
        args.push("--file=-".into());
    }
    args.push("--cleanup=verbatim".into());
    // Git owns pre-commit, prepare-commit-msg, commit-msg, post-commit,
    // post-rewrite and signing. Never disables verification or configured signing.
    let input = (!matches!(plan.kind, Kind::Fixup(_))).then(|| message.into_bytes());
    let command_result = run_git(&plan.root, environment, args, input);
    let private_repo = Repository::open(private.path()).map_err(error)?;
    let created = private_repo.head().and_then(|head| head.peel_to_commit()).map_err(error)?;
    // Keep completed objects reachable even if validation/publication fails.
    let result_ref = format!("refs/dust/rewrite-result/{}", created.id());
    no_reference_hook(&reference_hook).map_err(|e| format!("{e}; HEAD unchanged; prepared object {}", created.id()))?;
    repo.reference(&result_ref, created.id(), false, "dust: prepared local commit")
        .or_else(|e| {
            if repo.find_reference(&result_ref).ok().and_then(|r| r.target()) == Some(created.id()) {
                repo.find_reference(&result_ref)
            } else { Err(e) }
        }).map_err(error)?;
    command_result.map_err(|e| format!("{e} · HEAD unchanged; recovery {recovery}"))?;
    let reject = |why: &str| format!("{why}; HEAD unchanged. Prepared commit: {result_ref}");
    plan.validate(&repo).map_err(|e| reject(&e))?;
    if created.tree_id() != plan.tree {
        return Err(reject("A hook changed the intended commit tree; review its changes"));
    }
    let expected_parents = if matches!(plan.kind, Kind::Fixup(_)) {
        vec![plan.head]
    } else { old.parent_ids().collect::<Vec<_>>() };
    if created.parent_ids().collect::<Vec<_>>() != expected_parents {
        return Err(reject("Git produced unexpected commit parents"));
    }
    if !matches!(plan.kind, Kind::Fixup(_)) && created.author() != old.author() {
        return Err(reject("Git changed the original author metadata"));
    }
    let reference = plan.reference.as_deref().unwrap_or("HEAD");
    let committer = created.committer();
    let publication_error = |error: git2::Error| format!(
        "Cannot publish commit: {error}; inspect HEAD and {result_ref}; original {recovery}"
    );
    transaction.set_target(reference, created.id(), Some(&committer), "dust: local commit rewrite")
        .map_err(publication_error)?;
    // Normal branch publication already appends HEAD through libgit2. Keep
    // its HEAD lock, but never append that transition a second time.
    // With logAllRefUpdates=false, native Git still appends *existing* logs;
    // libgit2 suppresses all automatic logging, so own only those existing logs.
    if repo.config().map_err(publication_error)?.get_bool("core.logallrefupdates").ok() == Some(false) {
        let record = raw_reflog_record(plan.head, &created)
            .map_err(|e| format!("Cannot publish commit: {e}; inspect HEAD and {result_ref}; original {recovery}"))?;
        let names = if plan.reference.is_some() { vec![reference, "HEAD"] } else { vec!["HEAD"] };
        for name in names {
            if repo.reference_has_log(name).map_err(publication_error)? {
                let path = if name == "HEAD" {
                    repo.path().join("logs/HEAD")
                } else {
                    repo.commondir().join("logs").join(name)
                };
                // Preserve the entire historical byte prefix, including data
                // libgit2's reflog parser would skip or serializer would trim.
                // Real HEAD/branch/index locks remain held across every write.
                append_raw_reflog(&path, &record).map_err(|e| format!(
                    "Cannot publish commit: reflog {}: {e}; inspect HEAD and {result_ref}; original {recovery}", path.display()
                ))?;
            }
        }
    }
    transaction.commit().map_err(publication_error)?;
    // The real index is never rewritten or refreshed. Its staged tree is now
    // HEAD for amend/fixup; reword leaves the staged delta exactly as it was.
    Ok(Outcome { kind: plan.kind, head: created.id(), recovery })
}


/// Reuse Git's canonical committer bytes, including encoding and timezone. The
/// old OID is the captured actual HEAD, never the last (possibly partial) event.
fn raw_reflog_record(old: Oid, created: &git2::Commit<'_>) -> Result<Vec<u8>, String> {
    let mut fields = created.raw_header_bytes().split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"committer "));
    let committer = fields.next().ok_or("Missing committer header for reflog")?;
    if fields.next().is_some() || committer.is_empty()
        || committer.iter().any(|byte| matches!(*byte, 0 | b'\r' | b'\t'))
    {
        return Err("Unsupported committer header for reflog; use Git directly".into());
    }
    let mut record = format!("{old} {} ", created.id()).into_bytes();
    record.extend_from_slice(committer);
    record.extend_from_slice(b"\tdust: local commit rewrite\n");
    Ok(record)
}

struct ReflogLock {
    path: PathBuf,
    file: fs::File,
}
impl Drop for ReflogLock {
    fn drop(&mut self) {
        // Retain the owned handle through cleanup so its file identity cannot
        // be recycled into a replacement lock after an unlink.
        let owned = crate::files::file_identity(&self.file).ok();
        let current = fs::File::open(&self.path).ok()
            .and_then(|file| crate::files::file_identity(&file).ok());
        if owned.is_some() && owned == current {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Native logfile-lock convention, but copy history without parsing it. Each
/// individual replacement is atomic; multiple logs/refs are still not atomic.
fn append_raw_reflog(path: &Path, record: &[u8]) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let guard = ReflogLock {
        file: fs::OpenOptions::new().read(true).write(true).create_new(true).open(&lock_path)?,
        path: lock_path,
    };
    let mut destination = guard.file.try_clone()?;
    let copied = (|| -> std::io::Result<fs::File> {
        if !fs::symlink_metadata(path)?.file_type().is_file() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "reflog is not a regular file"));
        }
        let mut source = fs::File::open(path)?;
        let metadata = source.metadata()?;
        destination.set_permissions(metadata.permissions())?;
        let mut needs_separator = false;
        if metadata.len() != 0 {
            source.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            source.read_exact(&mut last)?;
            needs_separator = last[0] != b'\n';
            source.seek(SeekFrom::Start(0))?;
        }
        std::io::copy(&mut source, &mut destination)?;
        if needs_separator {
            destination.write_all(b"\n")?;
        }
        destination.write_all(record)?;
        destination.flush()?;
        destination.sync_all()?;
        Ok(source)
    })();
    drop(destination);
    // Keep both original source and owned lock handles alive through identity
    // checks and rename. Standard Windows File handles use delete sharing;
    // a platform replacement refusal remains a non-destructive I/O error.
    let source = copied?;
    let current_source = fs::File::open(path)?;
    let source_matches = crate::files::file_identity(&current_source)? == crate::files::file_identity(&source)?;
    drop(current_source);
    let current_lock = fs::File::open(&guard.path)?;
    let lock_matches = crate::files::file_identity(&current_lock)? == crate::files::file_identity(&guard.file)?;
    drop(current_lock);
    if !source_matches || !lock_matches {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "reflog or its lock changed"));
    }
    fs::rename(&guard.path, path)
}

fn no_reference_hook(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => Ok(()),
        Err(error) => Err(format!("Cannot inspect reference-transaction hook: {error}")),
        Ok(_) => Err("This repository has a reference-transaction hook; use Git directly so its publication policy is honored".into()),
    }
}

fn run_git(
    root: &Path,
    environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    arguments: Vec<std::ffi::OsString>,
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    use crate::process::{JobEvent, JobPolicy, JobRequest, JobStream, JobSupervisor};
    let mut jobs = JobSupervisor::try_new(16).map_err(error)?;
    jobs.start(JobRequest {
        program: crate::process::discover_executable("git").ok_or("Git is not installed or not on PATH")?,
        argv0: None, arguments, cwd: root.into(), environment, root_revision: 0,
        stdin: input,
        policy: JobPolicy {
            maximum_output_bytes: 64 * 1024,
            maximum_line_bytes: 64 * 1024,
            timeout: Duration::from_secs(120),
            ..JobPolicy::default()
        },
    }).map_err(error)?;
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    let mut truncated_output = false;
    loop {
        for event in jobs.poll() {
            match event {
                JobEvent::Output { stream, bytes, truncated, .. } => {
                    let target = if stream == JobStream::Stdout { &mut output } else { &mut diagnostics };
                    let remaining = (64 * 1024usize).saturating_sub(target.len());
                    truncated_output |= truncated || bytes.len() > remaining;
                    target.extend(bytes.into_iter().take(remaining));
                }
                JobEvent::Exited { code, timed_out, .. } => {
                    return if code == Some(0) && !timed_out && !truncated_output { Ok(output) } else {
                        Err(format!("Git command failed (exit {code:?}, timeout {timed_out}, output limit {truncated_output}): {} {}",
                            String::from_utf8_lossy(&diagnostics).trim(), String::from_utf8_lossy(&output).trim()))
                    };
                }
                JobEvent::Failed { message, .. } => return Err(message),
                JobEvent::Started { .. } => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
