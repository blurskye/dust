//! Pull requests through the forge's own CLI, which already owns sign-in, hosts and paging:
//! `gh` for GitHub and `tea` for Gitea and Forgejo. Each request runs the CLI (and `git`) on a
//! worker thread; replies arrive on a channel.

use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub branch: String,
    pub draft: bool,
}

/// A review comment on a line of the pull request's head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewComment {
    pub path: PathBuf,
    /// 1-based; `None` when the line no longer exists in the head.
    pub line: Option<u64>,
    pub author: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestHead {
    pub number: u64,
    pub commit: String,
}

#[derive(Clone, Debug)]
pub struct ReviewCommentTarget {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
    pub head: PullRequestHead,
}

#[derive(Debug)]
pub enum ForgeReply {
    List(Result<Vec<PullRequest>, String>),
    /// The forge could not be asked; these pull requests are already checked out here.
    Offline {
        error: String,
        local: Vec<(u64, PathBuf)>,
    },
    /// The worktree the pull request was checked out into.
    CheckedOut(Result<PathBuf, String>),
    Comments(Result<Vec<ReviewComment>, String>),
    CommentTarget(Result<ReviewCommentTarget, String>),
    Commented(Result<(), String>),
}

#[derive(Debug)]
pub struct ForgeEvent {
    pub root: PathBuf,
    pub request_id: u64,
    pub reply: ForgeReply,
}

/// The CLI that talks to the repository's forge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Forge {
    GitHub,
    Gitea,
}

pub struct ForgeJobs {
    sender: Sender<ForgeEvent>,
    replies: Receiver<ForgeEvent>,
    worker: Option<thread::JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
}

impl Default for ForgeJobs {
    fn default() -> Self {
        let (sender, replies) = mpsc::channel();
        Self {
            sender,
            replies,
            worker: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl ForgeJobs {
    pub fn try_reply(&self) -> Option<ForgeEvent> {
        self.replies.try_recv().ok()
    }

    #[cfg(test)]
    pub(crate) fn reply_for_test(&self, root: PathBuf, request_id: u64, reply: ForgeReply) {
        self.sender
            .send(ForgeEvent {
                root,
                request_id,
                reply,
            })
            .unwrap();
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn spawn(
        &mut self,
        root: PathBuf,
        request_id: u64,
        job: impl FnOnce(&Path, &ForgeCommands<'_>) -> ForgeReply + Send + 'static,
    ) -> Result<(), String> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Err("a forge request is already running".into());
        }
        self.cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = self.cancelled.clone();
        let sender = self.sender.clone();
        self.worker = Some(
            thread::Builder::new()
                .name("dust-forge".into())
                .spawn(move || {
                    let commands = ForgeCommands {
                        cancelled: &cancelled,
                    };
                    let reply = job(&root, &commands);
                    let _ = sender.send(ForgeEvent {
                        root,
                        request_id,
                        reply,
                    });
                })
                .map_err(|error| format!("cannot start forge request: {error}"))?,
        );
        Ok(())
    }

    pub fn list(&mut self, root: PathBuf, request_id: u64) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            match commands.open_pull_requests(root) {
                Ok(list) => ForgeReply::List(Ok(list)),
                Err(error) => ForgeReply::Offline {
                    error,
                    local: commands.local_pull_requests(root),
                },
            }
        })
    }

    /// Check the pull request out into `<parent>/<repo>-pr-<number>` on branch `pr-<number>`,
    /// so the current worktree and its branch are left alone. An existing one is updated.
    pub fn checkout(
        &mut self,
        root: PathBuf,
        request_id: u64,
        number: u64,
        workspace: crate::workspace::WorkspaceState,
    ) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            ForgeReply::CheckedOut((|| {
                workspace
                    .authorize_execution(crate::workspace::ExecutionSource::ProjectLocal)
                    .map_err(|error| error.to_string())?;
                let target = commands.pr_worktree(root, number)?;
                commands.prepare_worktree(root, &target)?;
                let branch = format!("pr-{number}");
                commands.with_forge(root, |forge| match forge {
                    Forge::GitHub => commands.run(
                        &target,
                        "gh",
                        &["pr", "checkout", &number.to_string(), "--branch", &branch],
                    ),
                    // Gitea and Forgejo publish every pull request's head as refs/pull/N/head.
                    Forge::Gitea => {
                        commands.run(
                            &target,
                            "git",
                            &["fetch", "origin", &format!("refs/pull/{number}/head")],
                        )?;
                        commands.update_pr_branch(&target, &branch)
                    }
                })?;
                Ok(target)
            })())
        })
    }

    /// Review comments of the pull request for the current branch.
    pub fn comments(&mut self, root: PathBuf, request_id: u64) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            ForgeReply::Comments(commands.with_forge(root, |forge| match forge {
                Forge::GitHub => {
                    let number = commands.current_number(root, Forge::GitHub)?;
                    let output = commands.run(
                        root,
                        "gh",
                        &[
                            "api",
                            &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments"),
                            "--paginate",
                            "--jq",
                            ".[]",
                        ],
                    )?;
                    parse_comments(&output, root)
                }
                Forge::Gitea => {
                    let number = commands.current_number(root, Forge::Gitea)?;
                    let reviews = commands.tea_pages(
                        root,
                        &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews"),
                    )?;
                    let mut comments = Vec::new();
                    for review in reviews {
                        let Some(id) = review["id"].as_u64() else {
                            continue;
                        };
                        let items = commands.tea_pages(
                            root,
                            &format!(
                                "repos/{{owner}}/{{repo}}/pulls/{number}/reviews/{id}/comments"
                            ),
                        )?;
                        comments.extend(gitea_comments(&items, root));
                    }
                    Ok(comments)
                }
            }))
        })
    }

    /// Capture the pull request and head before a review draft is opened.
    pub fn comment_target(
        &mut self,
        root: PathBuf,
        request_id: u64,
        path: PathBuf,
        line: usize,
        text: String,
    ) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            ForgeReply::CommentTarget(
                commands
                    .with_forge(root, |forge| commands.current_head(root, forge))
                    .map(|head| ReviewCommentTarget {
                        path,
                        line,
                        text,
                        head,
                    }),
            )
        })
    }

    pub fn comment(
        &mut self,
        root: PathBuf,
        request_id: u64,
        target: ReviewCommentTarget,
        body: String,
    ) -> Result<(), String> {
        let ReviewCommentTarget {
            path,
            line,
            text,
            head,
        } = target;
        self.spawn(root, request_id, move |root, commands| {
            ForgeReply::Commented({
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                commands.with_forge(root, |forge| {
                    check_comment_head(&head, &commands.current_head(root, forge)?)?;
                    let number = head.number;
                    let commit = &head.commit;
                    commands.check_comment_text(root, commit, &relative, &text)?;
                    match forge {
                    Forge::GitHub => {
                        commands.run(
                            root,
                            "gh",
                            &[
                                "api",
                                "--method",
                                "POST",
                                &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments"),
                                "-f",
                                &format!("body={body}"),
                                "-f",
                                &format!("commit_id={commit}"),
                                "-f",
                                &format!("path={relative}"),
                                "-F",
                                &format!("line={line}"),
                                "-f",
                                "side=RIGHT",
                            ],
                        )
                        .map(|_| ())
                    }
                    Forge::Gitea => {
                        let review = serde_json::json!({
                            "commit_id": commit,
                            "event": "COMMENT",
                            "body": "",
                            "comments": [{ "path": relative, "body": body, "new_position": line }],
                        });
                        commands.run(
                            root,
                            "tea",
                            &[
                                "api",
                                "--method",
                                "POST",
                                &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews"),
                                "--data",
                                &review.to_string(),
                            ],
                        )
                        .map(|_| ())
                    }
                    }
                })
            })
        })
    }
}

impl Drop for ForgeJobs {
    fn drop(&mut self) {
        self.cancel();
    }
}

const FORGE_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

struct ForgeCommands<'a> {
    cancelled: &'a AtomicBool,
}

impl ForgeCommands<'_> {
    fn current_head(&self, root: &Path, forge: Forge) -> Result<PullRequestHead, String> {
        let (number, commit) = match forge {
            Forge::GitHub => {
                let output =
                    self.run(root, "gh", &["pr", "view", "--json", "number,headRefOid"])?;
                let view: Value =
                    serde_json::from_str(&output).map_err(|error| error.to_string())?;
                let number = view["number"]
                    .as_u64()
                    .ok_or("no pull request for this branch")?;
                let commit = view["headRefOid"]
                    .as_str()
                    .ok_or("pull request head is missing")?
                    .to_owned();
                (number, commit)
            }
            Forge::Gitea => {
                let number = self.current_number(root, forge)?;
                let output = self.run(
                    root,
                    "tea",
                    &["api", &format!("repos/{{owner}}/{{repo}}/pulls/{number}")],
                )?;
                let view: Value =
                    serde_json::from_str(&output).map_err(|error| error.to_string())?;
                let commit = view["head"]["sha"]
                    .as_str()
                    .ok_or("pull request head is missing")?
                    .to_owned();
                (number, commit)
            }
        };
        Ok(PullRequestHead { number, commit })
    }
}

fn check_comment_head(expected: &PullRequestHead, current: &PullRequestHead) -> Result<(), String> {
    if expected != current {
        return Err("the pull request or its head changed; start a new review comment".into());
    }
    Ok(())
}

impl ForgeCommands<'_> {
    fn check_comment_text(
        &self,
        root: &Path,
        commit: &str,
        path: &str,
        text: &str,
    ) -> Result<(), String> {
        let head = self.run_bytes(root, "git", &["show", &format!("{commit}:{path}")])?;
        let head = crate::editor::text::TextBuffer::from_bytes(&head)
            .map_err(|error| error.to_string())?;
        if head.to_string() != text {
            return Err(
                "the buffer differs from the pull request head; comment on an unchanged buffer"
                    .into(),
            );
        }
        Ok(())
    }

    fn open_pull_requests(&self, root: &Path) -> Result<Vec<PullRequest>, String> {
        self.with_forge(root, |forge| match forge {
            Forge::GitHub => self
                .run(
                    root,
                    "gh",
                    &[
                        "pr",
                        "list",
                        "--limit",
                        "200",
                        "--json",
                        "number,title,author,headRefName,isDraft",
                    ],
                )
                .and_then(|output| parse_list(&output)),
            Forge::Gitea => self.tea_list(root),
        })
    }

    fn tea_list(&self, root: &Path) -> Result<Vec<PullRequest>, String> {
        let items = self.tea_pages(root, "repos/{owner}/{repo}/pulls?state=open")?;
        Ok(gitea_pull_requests(&items))
    }

    fn tea_pages(&self, root: &Path, endpoint: &str) -> Result<Vec<Value>, String> {
        let mut items = Vec::new();
        let mut bytes = 0;
        for page in 1_u64.. {
            let separator = if endpoint.contains('?') { '&' } else { '?' };
            let output = self.run(
                root,
                "tea",
                &["api", &format!("{endpoint}{separator}limit=50&page={page}")],
            )?;
            bytes += output.len();
            if bytes > FORGE_OUTPUT_BYTES {
                return Err("forge response exceeds the output limit".into());
            }
            let batch: Vec<Value> =
                serde_json::from_str(&output).map_err(|error| error.to_string())?;
            // Do not assume the server honored our requested page size.
            if batch.is_empty() {
                return Ok(items);
            }
            items.extend(batch);
        }
        Err("forge pagination exhausted".into())
    }

    /// GitHub remotes go to `gh`; every other host is taken to be Gitea or Forgejo.
    fn forge(&self, root: &Path) -> Result<Forge, String> {
        let url = self
            .run(root, "git", &["remote", "get-url", "origin"])
            .map_err(|_| "this repository has no `origin` remote".to_owned())?;
        if remote_is_path(url.trim()) {
            return Err("`origin` is a local path, so it has no pull requests".to_owned());
        }
        Ok(if remote_host(url.trim()).contains("github") {
            Forge::GitHub
        } else {
            Forge::Gitea
        })
    }

    /// Run `job` with the CLI the remote's host suggests, then with the other one: a GitHub
    /// Enterprise host need not say "github", and either CLI may be the one signed in. The first
    /// CLI's error is the one reported.
    fn with_forge<T>(
        &self,
        root: &Path,
        job: impl Fn(Forge) -> Result<T, String>,
    ) -> Result<T, String> {
        let first = self.forge(root)?;
        let other = match first {
            Forge::GitHub => Forge::Gitea,
            Forge::Gitea => Forge::GitHub,
        };
        job(first).or_else(|error| job(other).map_err(|_| error))
    }
}

/// A remote on this machine: a path or `file://`. Host forms always hold a colon.
fn remote_is_path(url: &str) -> bool {
    url.starts_with("file://") || !url.contains(':')
}

/// The host of a remote URL: `https://host/…`, `ssh://user@host:port/…` or `user@host:path`.
fn remote_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split_once('@').map_or(rest, |(_, rest)| rest);
    rest.split(['/', ':']).next().unwrap_or_default()
}

impl ForgeCommands<'_> {
    fn current_number(&self, root: &Path, forge: Forge) -> Result<u64, String> {
        match forge {
            Forge::GitHub => self
                .run(
                    root,
                    "gh",
                    &["pr", "view", "--json", "number", "--jq", ".number"],
                )?
                .trim()
                .parse()
                .map_err(|_| "no pull request for this branch".to_owned()),
            Forge::Gitea => {
                let branch = self.run(root, "git", &["branch", "--show-current"])?;
                let branch = branch.trim();
                // A pull request checked out by Dust is on `pr-<number>`.
                if let Some(number) = branch.strip_prefix("pr-").and_then(|n| n.parse().ok()) {
                    return Ok(number);
                }
                self.tea_list(root)?
                    .into_iter()
                    .find(|pr| pr.branch == branch)
                    .map(|pr| pr.number)
                    .ok_or_else(|| "no pull request for this branch".to_owned())
            }
        }
    }

    /// Pull requests already checked out in a worktree on a `pr-<number>` branch.
    fn local_pull_requests(&self, root: &Path) -> Vec<(u64, PathBuf)> {
        let Ok(output) = self.run_bytes(root, "git", &["worktree", "list", "--porcelain", "-z"])
        else {
            return Vec::new();
        };
        parse_local_pull_requests(&output)
    }

    fn pr_worktree(&self, root: &Path, number: u64) -> Result<PathBuf, String> {
        if let Some((_, path)) = self
            .local_pull_requests(root)
            .into_iter()
            .find(|(pr, _)| *pr == number)
        {
            return Ok(path);
        }
        let repository = git2::Repository::discover(root).map_err(|error| error.to_string())?;
        let common =
            git2::Repository::open(repository.commondir()).map_err(|error| error.to_string())?;
        let root = common
            .workdir()
            .ok_or("the repository has no main worktree")?;
        let name = root
            .file_name()
            .ok_or("the workspace has no directory name")?
            .to_string_lossy();
        let parent = root
            .parent()
            .ok_or("the workspace has no parent directory")?;
        Ok(parent.join(format!("{name}-pr-{number}")))
    }

    fn prepare_worktree(&self, root: &Path, target: &Path) -> Result<(), String> {
        if !target.try_exists().map_err(|error| error.to_string())? {
            let path = target.to_str().ok_or("worktree path is not UTF-8")?;
            return self
                .run(root, "git", &["worktree", "add", "--detach", path])
                .map(|_| ());
        }
        let source = git2::Repository::discover(root).map_err(|error| error.to_string())?;
        let repository = git2::Repository::open(target).map_err(|error| error.to_string())?;
        let common =
            crate::files::canonicalize(source.commondir()).map_err(|error| error.to_string())?;
        if crate::files::canonicalize(repository.commondir()).map_err(|error| error.to_string())?
            != common
        {
            return Err("pull request checkout belongs to another repository".into());
        }
        let target = crate::files::canonicalize(target).map_err(|error| error.to_string())?;
        let registered = source
            .worktrees()
            .map_err(|error| error.to_string())?
            .iter()
            .flatten()
            .any(|name| {
                source.find_worktree(name).is_ok_and(|worktree| {
                    worktree.validate().is_ok()
                        && crate::files::canonicalize(worktree.path())
                            .is_ok_and(|path| path == target)
                })
            });
        let common_repository =
            git2::Repository::open(&common).map_err(|error| error.to_string())?;
        let main = common_repository
            .workdir()
            .and_then(|path| crate::files::canonicalize(path).ok());
        if !registered && main.as_ref() != Some(&target) {
            return Err("pull request checkout is not a registered worktree".into());
        }
        Ok(())
    }

    fn update_pr_branch(&self, root: &Path, branch: &str) -> Result<String, String> {
        let repository = git2::Repository::open(root).map_err(|error| error.to_string())?;
        if repository
            .find_branch(branch, git2::BranchType::Local)
            .is_ok()
        {
            self.run(root, "git", &["checkout", branch])?;
            self.run(root, "git", &["merge", "--ff-only", "FETCH_HEAD"])
                .map_err(|error| {
                    format!("{error}; PR branch preserved, reconcile it before updating")
                })
        } else {
            self.run(root, "git", &["checkout", "-b", branch, "FETCH_HEAD"])
        }
    }

    fn run(&self, cwd: &Path, program: &str, arguments: &[&str]) -> Result<String, String> {
        self.run_bytes(cwd, program, arguments)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    fn run_bytes(&self, cwd: &Path, program: &str, arguments: &[&str]) -> Result<Vec<u8>, String> {
        use crate::process::JobEvent;
        use crate::process::JobPolicy;
        use crate::process::JobRequest;
        use crate::process::JobStream;
        use crate::process::JobSupervisor;
        if self.cancelled.load(Ordering::Acquire) {
            return Err("forge request cancelled".into());
        }
        let executable = crate::tools::discover_executable(program)
            .ok_or_else(|| format!("{program} is not installed"))?;
        let executable = std::path::absolute(executable).map_err(|error| error.to_string())?;
        let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
            .collect();
        for (key, value) in [
            ("GH_PROMPT_DISABLED", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_ASKPASS", "/bin/false"),
            ("SSH_ASKPASS", "/bin/false"),
            // Force the failing askpass helper instead of consulting the controlling
            // terminal, without replacing the user's SSH command or identity options.
            ("SSH_ASKPASS_REQUIRE", "force"),
        ] {
            environment.insert(key.into(), value.into());
        }
        let mut jobs = JobSupervisor::try_new(64).map_err(|error| error.to_string())?;
        jobs.start(JobRequest {
            program: executable,
            argv0: None,
            arguments: arguments
                .iter()
                .map(|argument| (*argument).into())
                .collect(),
            cwd: cwd.into(),
            environment,
            root_revision: 0,
            stdin: None,
            policy: JobPolicy {
                maximum_output_bytes: FORGE_OUTPUT_BYTES,
                maximum_line_bytes: FORGE_OUTPUT_BYTES,
                timeout: Duration::from_secs(60),
                ..JobPolicy::default()
            },
        })
        .map_err(|error| error.to_string())?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err("forge request cancelled".into());
            }
            for event in jobs.poll() {
                match event {
                    JobEvent::Output {
                        stream,
                        bytes,
                        truncated,
                        ..
                    } => {
                        if truncated {
                            return Err(format!("{program}: output exceeds the forge limit"));
                        }
                        match stream {
                            JobStream::Stdout => stdout.extend(bytes),
                            JobStream::Stderr => stderr.extend(bytes),
                        }
                    }
                    JobEvent::Exited {
                        code, timed_out, ..
                    } => {
                        if timed_out {
                            return Err(format!("{program}: forge command timed out"));
                        }
                        if code == Some(0) {
                            return Ok(stdout);
                        }
                        let error = String::from_utf8_lossy(&stderr);
                        return Err(match error.lines().find(|line| !line.trim().is_empty()) {
                            Some(line) => format!("{program}: {}", line.trim()),
                            None => format!("{program} failed ({code:?})"),
                        });
                    }
                    JobEvent::Failed { message, .. } => {
                        return Err(format!("{program}: {message}"));
                    }
                    JobEvent::Started { .. } => {}
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn parse_local_pull_requests(output: &[u8]) -> Vec<(u64, PathBuf)> {
    let mut local = Vec::new();
    let mut path = None;
    for field in output.split(|byte| *byte == 0) {
        if field.is_empty() {
            path = None;
        } else if let Some(worktree) = field.strip_prefix(b"worktree ") {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                path = Some(PathBuf::from(std::ffi::OsStr::from_bytes(worktree)));
            }
            #[cfg(not(unix))]
            {
                path = std::str::from_utf8(worktree).ok().map(PathBuf::from);
            }
        } else if let Some(branch) = field.strip_prefix(b"branch refs/heads/pr-")
            && let (Ok(number), Some(path)) = (
                std::str::from_utf8(branch).unwrap_or_default().parse(),
                path.take(),
            )
        {
            local.push((number, path));
        }
    }
    local
}

fn parse_list(output: &str) -> Result<Vec<PullRequest>, String> {
    let items: Vec<Value> = serde_json::from_str(output).map_err(|error| error.to_string())?;
    Ok(items
        .iter()
        .map(|item| PullRequest {
            number: item["number"].as_u64().unwrap_or_default(),
            title: item["title"].as_str().unwrap_or_default().to_owned(),
            author: item["author"]["login"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            branch: item["headRefName"].as_str().unwrap_or_default().to_owned(),
            draft: item["isDraft"].as_bool().unwrap_or_default(),
        })
        .collect())
}

/// `gh api --jq '.[]'` prints one comment object per line.
fn parse_comments(output: &str, root: &Path) -> Result<Vec<ReviewComment>, String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let item: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
            Ok(ReviewComment {
                path: root.join(item["path"].as_str().unwrap_or_default()),
                line: if item["side"].as_str() == Some("LEFT") {
                    None
                } else {
                    item["line"].as_u64()
                },
                author: item["user"]["login"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                body: item["body"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

fn gitea_pull_requests(items: &[Value]) -> Vec<PullRequest> {
    items
        .iter()
        .map(|item| {
            let title = item["title"].as_str().unwrap_or_default().to_owned();
            PullRequest {
                number: item["number"].as_u64().unwrap_or_default(),
                draft: item["draft"].as_bool().unwrap_or_default()
                    || ["WIP:", "[WIP]"]
                        .iter()
                        .any(|prefix| title.starts_with(prefix)),
                title,
                author: item["user"]["login"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                branch: item["head"]["ref"].as_str().unwrap_or_default().to_owned(),
            }
        })
        .collect()
}

/// A Gitea review's comments; `position` is the line in the head, 0 once it is gone.
fn gitea_comments(items: &[Value], root: &Path) -> Vec<ReviewComment> {
    items
        .iter()
        .map(|item| ReviewComment {
            path: root.join(item["path"].as_str().unwrap_or_default()),
            line: item["position"].as_u64().filter(|line| *line > 0),
            author: item["user"]["login"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            body: item["body"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::forge::*;

    static CANCELLED: AtomicBool = AtomicBool::new(false);
    static COMMANDS: ForgeCommands<'static> = ForgeCommands {
        cancelled: &CANCELLED,
    };

    #[test]
    fn gh_output_is_parsed() {
        let list = r#"[{"number":7,"title":"Fix it","author":{"login":"ana"},"headRefName":"fix","isDraft":false}]"#;
        assert_eq!(
            parse_list(list).unwrap(),
            vec![PullRequest {
                number: 7,
                title: "Fix it".into(),
                author: "ana".into(),
                branch: "fix".into(),
                draft: false,
            }]
        );
        let comments = "{\"path\":\"src/a.rs\",\"line\":3,\"user\":{\"login\":\"bo\"},\"body\":\"nit\"}\n\
                        {\"path\":\"b.rs\",\"line\":null,\"user\":{\"login\":\"cy\"},\"body\":\"old\"}\n";
        let parsed = parse_comments(comments, Path::new("/r")).unwrap();
        assert_eq!(parsed[0].path, PathBuf::from("/r/src/a.rs"));
        assert_eq!(parsed[0].line, Some(3));
        assert_eq!(parsed[1].line, None);
    }

    #[test]
    fn base_side_review_comments_have_no_head_location() {
        let comments = "{\"path\":\"a.rs\",\"line\":10,\"side\":\"LEFT\"}\n{\"path\":\"a.rs\",\"line\":11,\"side\":\"RIGHT\"}\n";
        let parsed = parse_comments(comments, Path::new("/r")).unwrap();
        assert_eq!(parsed[0].line, None);
        assert_eq!(parsed[1].line, Some(11));
    }

    #[test]
    fn tea_output_is_parsed() {
        let list =
            r#"[{"number":12,"title":"WIP: Faster","user":{"login":"ana"},"head":{"ref":"fast"}}]"#;
        assert_eq!(
            gitea_pull_requests(&serde_json::from_str::<Vec<Value>>(list).unwrap()),
            vec![PullRequest {
                number: 12,
                title: "WIP: Faster".into(),
                author: "ana".into(),
                branch: "fast".into(),
                draft: true,
            }]
        );
        let comments = r#"[{"path":"a.rs","position":4,"user":{"login":"bo"},"body":"nit"},
                           {"path":"b.rs","position":0,"user":{"login":"cy"},"body":"old"}]"#;
        let parsed = gitea_comments(
            &serde_json::from_str::<Vec<Value>>(comments).unwrap(),
            Path::new("/r"),
        );
        assert_eq!(parsed[0].line, Some(4));
        assert_eq!(parsed[1].line, None);
    }

    #[cfg(unix)]
    #[test]
    fn gitea_cli_pages_reviews_comments_and_matches_late_branches() {
        use std::os::unix::fs::PermissionsExt;
        const FIXTURE: &str = "DUST_FORGE_FIXTURE_DIR";
        if let Some(directory) = std::env::var_os(FIXTURE) {
            let root = PathBuf::from(directory).join("repo");
            assert_eq!(COMMANDS.current_number(&root, Forge::Gitea).unwrap(), 205);
            let wait = |jobs: &ForgeJobs| {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(event) = jobs.try_reply() {
                        break event.reply;
                    }
                    assert!(std::time::Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(2));
                }
            };
            let mut jobs = ForgeJobs::default();
            jobs.comments(root.clone(), 1).unwrap();
            let ForgeReply::Comments(Ok(comments)) = wait(&jobs) else {
                panic!("comments failed")
            };
            assert_eq!(comments.len(), 4);
            assert!(
                comments
                    .iter()
                    .all(|comment| comment.body == "nit" && comment.line == Some(1))
            );
            let mut jobs = ForgeJobs::default();
            let target = ReviewCommentTarget {
                path: root.join("file.txt"),
                line: 1,
                text: "one\n".into(),
                head: COMMANDS.current_head(&root, Forge::Gitea).unwrap(),
            };
            jobs.comment(root.clone(), 1, target, "nit".into()).unwrap();
            assert!(matches!(wait(&jobs), ForgeReply::Commented(Ok(()))));
            let log = std::fs::read_to_string(root.parent().unwrap().join("requests")).unwrap();
            assert!(log.contains("/reviews?limit=50&page=2"));
            assert!(log.contains("/reviews/2/comments?limit=50&page=2"));
            let posted = log
                .lines()
                .find_map(|line| line.split_once("--data ").map(|(_, data)| data))
                .unwrap();
            let posted: Value = serde_json::from_str(posted).unwrap();
            assert_eq!(
                posted["commit_id"],
                std::env::var("DUST_FORGE_HEAD").unwrap()
            );
            assert_eq!(posted["comments"][0]["new_position"], 1);
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        repository(&root);
        COMMANDS
            .run(&root, "git", &["branch", "-m", "feature"])
            .unwrap();
        COMMANDS
            .run(
                &root,
                "git",
                &["remote", "add", "origin", "https://forge.example/o/r"],
            )
            .unwrap();
        std::fs::write(root.join("file.txt"), "\u{feff}one\r\n").unwrap();
        COMMANDS.run(&root, "git", &["add", "file.txt"]).unwrap();
        COMMANDS
            .run(&root, "git", &["commit", "-m", "text"])
            .unwrap();
        let commit = COMMANDS.run(&root, "git", &["rev-parse", "HEAD"]).unwrap();
        let tea = directory.path().join("tea");
        std::fs::write(
            &tea,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$DUST_FORGE_FIXTURE_DIR/requests"
case "$*" in
  'api repos/{owner}/{repo}/pulls?state=open&limit=50&page='*)
    page=${2##*page=}
    if [ "$page" -le 4 ]; then
      printf '['
      i=1
      while [ "$i" -le 50 ]; do
        [ "$i" -eq 1 ] || printf ','
        printf '{"number":%s,"head":{"ref":"other-%s"}}' "$i" "$i"
        i=$((i+1))
      done
      printf ']'
    elif [ "$page" -eq 5 ]; then
      printf '[{"number":205,"head":{"ref":"feature"}}]'
    else printf '[]'; fi ;;
  'api repos/{owner}/{repo}/pulls/205/reviews?limit=50&page='*)
    page=${2##*page=}
    if [ "$page" -le 2 ]; then printf '[{"id":%s}]' "$page"; else printf '[]'; fi ;;
  'api repos/{owner}/{repo}/pulls/205/reviews/'*'/comments?limit=50&page='*)
    page=${2##*page=}
    if [ "$page" -le 2 ]; then
      printf '[{"path":"file.txt","position":1,"body":"nit"}]'
    else printf '[]'; fi ;;
  'api repos/{owner}/{repo}/pulls/205') printf '{"head":{"sha":"%s"}}' "$DUST_FORGE_HEAD" ;;
  'api --method POST repos/{owner}/{repo}/pulls/205/reviews --data '*) printf '{}' ;;
  *) printf 'unexpected tea request: %s' "$*" >&2; exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&tea, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![directory.path().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "forge::tests::gitea_cli_pages_reviews_comments_and_matches_late_branches",
                "--nocapture",
            ])
            .env(FIXTURE, directory.path())
            .env("DUST_FORGE_HEAD", commit.trim())
            .env("PATH", std::env::join_paths(paths).unwrap())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn remote_hosts_pick_the_forge_cli() {
        assert_eq!(remote_host("git@github.com:a/b.git"), "github.com");
        assert_eq!(remote_host("https://github.com/a/b"), "github.com");
        assert_eq!(
            remote_host("ssh://git@git.example.org/a/b.git"),
            "git.example.org"
        );
        assert_eq!(
            remote_host("ssh://git@codeberg.org:2222/a/b"),
            "codeberg.org"
        );
        assert!(remote_is_path("/srv/git/a.git"));
        assert!(remote_is_path("../a.git"));
        assert!(remote_is_path("file:///srv/git/a.git"));
        assert!(!remote_is_path("git@github.com:a/b.git"));
    }

    #[cfg(unix)]
    #[test]
    fn forge_commands_disable_prompts_and_bound_output() {
        const FIXTURE: &str = "DUST_FORGE_SSH_FIXTURE";
        if std::env::var_os(FIXTURE).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "forge::tests::forge_commands_disable_prompts_and_bound_output",
                    "--nocapture",
                ])
                .env(FIXTURE, "1")
                .env("GIT_SSH_COMMAND", "ssh -i /custom/key -p 2222")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = COMMANDS.run(root.path(), "/bin/sh", &["-c",
            "printf '%s\\n' \"$GIT_TERMINAL_PROMPT\" \"$GIT_SSH_COMMAND\" \"$SSH_ASKPASS_REQUIRE\" \"$GH_PROMPT_DISABLED\"; test ! -t 0 && test -n \"$SSH_ASKPASS\" && ! \"$SSH_ASKPASS\" ignored"
        ]).unwrap();
        assert_eq!(output, "0\nssh -i /custom/key -p 2222\nforce\n1\n");
        let error = COMMANDS
            .run(
                root.path(),
                "/bin/sh",
                &["-c", "head -c 16777217 /dev/zero"],
            )
            .unwrap_err();
        assert!(error.contains("output exceeds"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn forge_jobs_refuse_concurrent_requests_and_cancel_running_children() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().to_path_buf();
        let mut jobs = ForgeJobs::default();
        jobs.spawn(path.clone(), 1, |root, commands| {
            ForgeReply::List(
                commands
                    // The rename publishes the PID whole; `> child` creates the file before writing it.
                    .run(
                        root,
                        "/bin/sh",
                        &[
                            "-c",
                            "echo $$ > child.tmp && mv child.tmp child; exec sleep 30",
                        ],
                    )
                    .map(|_| Vec::new()),
            )
        })
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !path.join("child").exists() {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            jobs.list(path.clone(), 1)
                .unwrap_err()
                .contains("already running")
        );
        let pid: i32 = std::fs::read_to_string(path.join("child"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        jobs.cancel();
        loop {
            if let Some(event) = jobs.try_reply() {
                assert!(
                    matches!(event.reply, ForgeReply::List(Err(error)) if error.contains("cancelled"))
                );
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "cancelled child was not reaped"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn repository(root: &Path) {
        COMMANDS.run(root, "git", &["init"]).unwrap();
        COMMANDS
            .run(root, "git", &["config", "user.name", "Test"])
            .unwrap();
        COMMANDS
            .run(root, "git", &["config", "user.email", "test@example.com"])
            .unwrap();
        COMMANDS
            .run(root, "git", &["commit", "--allow-empty", "-m", "initial"])
            .unwrap();
    }

    #[test]
    fn pull_requests_reuse_sibling_worktrees_from_any_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        repository(&root);
        let first = COMMANDS.pr_worktree(&root, 1).unwrap();
        assert_eq!(first, directory.path().join("repo-pr-1"));
        COMMANDS.prepare_worktree(&root, &first).unwrap();
        COMMANDS
            .run(&first, "git", &["checkout", "-b", "pr-1"])
            .unwrap();
        let second = COMMANDS.pr_worktree(&first, 2).unwrap();
        assert_eq!(second, directory.path().join("repo-pr-2"));
        COMMANDS.prepare_worktree(&first, &second).unwrap();
        assert_eq!(COMMANDS.pr_worktree(&second, 1).unwrap(), first);
        COMMANDS.prepare_worktree(&second, &first).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_pr_worktrees_preserve_quoted_and_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        repository(&root);
        let target = directory.path().join("quoted\\path\nworktree");
        let output = std::process::Command::new("git")
            .current_dir(&root)
            .args(["worktree", "add", "-b", "pr-9"])
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            COMMANDS.local_pull_requests(&root),
            vec![(9, target.clone())]
        );
        assert_eq!(COMMANDS.pr_worktree(&root, 9).unwrap(), target);
        // Exercise arbitrary Unix bytes independently of Git's locale-dependent path creation.
        let output = b"worktree /repo/quoted\\path\n\xff\0branch refs/heads/pr-10\0\0";
        let expected = PathBuf::from(std::ffi::OsStr::from_bytes(b"/repo/quoted\\path\n\xff"));
        assert_eq!(parse_local_pull_requests(output), vec![(10, expected)]);
    }

    #[test]
    fn foreign_checkout_directories_are_rejected_before_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let target = directory.path().join("repo-pr-7");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&target).unwrap();
        repository(&root);
        repository(&target);
        let before = COMMANDS
            .run(&target, "git", &["rev-parse", "HEAD"])
            .unwrap();
        assert!(COMMANDS.prepare_worktree(&root, &target).is_err());
        assert_eq!(
            COMMANDS
                .run(&target, "git", &["rev-parse", "HEAD"])
                .unwrap(),
            before
        );
    }

    #[test]
    fn updating_pr_branches_preserves_local_commits_and_fast_forwards() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        repository(root);
        let base = COMMANDS.run(root, "git", &["rev-parse", "HEAD"]).unwrap();
        COMMANDS
            .run(root, "git", &["fetch", ".", base.trim()])
            .unwrap();
        COMMANDS.update_pr_branch(root, "pr-7").unwrap();
        COMMANDS
            .run(root, "git", &["commit", "--allow-empty", "-m", "local"])
            .unwrap();
        let local = COMMANDS.run(root, "git", &["rev-parse", "HEAD"]).unwrap();
        COMMANDS.update_pr_branch(root, "pr-7").unwrap();
        assert_eq!(
            COMMANDS.run(root, "git", &["rev-parse", "HEAD"]).unwrap(),
            local
        );
        COMMANDS
            .run(root, "git", &["checkout", "--detach", base.trim()])
            .unwrap();
        COMMANDS
            .run(root, "git", &["commit", "--allow-empty", "-m", "remote"])
            .unwrap();
        let remote = COMMANDS.run(root, "git", &["rev-parse", "HEAD"]).unwrap();
        COMMANDS
            .run(root, "git", &["fetch", ".", remote.trim()])
            .unwrap();
        assert!(COMMANDS.update_pr_branch(root, "pr-7").is_err());
        assert_eq!(
            COMMANDS.run(root, "git", &["rev-parse", "pr-7"]).unwrap(),
            local
        );
        COMMANDS
            .run(root, "git", &["fetch", ".", local.trim()])
            .unwrap();
        COMMANDS
            .run(root, "git", &["branch", "pr-8", base.trim()])
            .unwrap();
        COMMANDS.update_pr_branch(root, "pr-8").unwrap();
        assert_eq!(
            COMMANDS.run(root, "git", &["rev-parse", "HEAD"]).unwrap(),
            local
        );
    }
    #[test]
    fn untrusted_checkout_is_rejected_before_running_commands() {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [(
                "workspace".into(),
                crate::files::RootIdentity {
                    canonical_path: root.path().into(),
                    filesystem_identity: None,
                },
            )],
        )
        .unwrap();
        let mut jobs = ForgeJobs::default();
        jobs.checkout(root.path().into(), 0, 1, workspace).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(reply) = jobs.try_reply() {
                let ForgeReply::CheckedOut(Err(error)) = reply.reply else {
                    panic!("trust rejection")
                };
                assert!(error.contains("trust"), "{error}");
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn review_drafts_require_the_original_pull_request_and_head() {
        let original = PullRequestHead {
            number: 7,
            commit: "abc".into(),
        };
        assert!(check_comment_head(&original, &original).is_ok());
        assert!(
            check_comment_head(
                &original,
                &PullRequestHead {
                    number: 8,
                    ..original.clone()
                }
            )
            .is_err()
        );
        assert!(
            check_comment_head(
                &original,
                &PullRequestHead {
                    commit: "def".into(),
                    ..original.clone()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn line_comments_require_the_pull_request_head_text() {
        let root = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(root.path()).unwrap();
        std::fs::write(root.path().join("file.txt"), "one\ntwo\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        let commit = repo
            .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap()
            .to_string();
        assert!(
            COMMANDS
                .check_comment_text(root.path(), &commit, "file.txt", "one\ntwo\n")
                .is_ok()
        );
        assert!(
            COMMANDS
                .check_comment_text(root.path(), &commit, "file.txt", "inserted\none\ntwo\n")
                .is_err()
        );
    }
    #[test]
    fn line_comments_accept_unchanged_bom_and_mixed_line_endings() {
        let root = tempfile::tempdir().unwrap();
        repository(root.path());
        std::fs::write(root.path().join("bom.txt"), "\u{feff}one\r\ntwo\rthree\n").unwrap();
        COMMANDS
            .run(root.path(), "git", &["add", "bom.txt"])
            .unwrap();
        COMMANDS
            .run(root.path(), "git", &["commit", "-m", "BOM"])
            .unwrap();
        assert!(
            COMMANDS
                .check_comment_text(root.path(), "HEAD", "bom.txt", "one\ntwo\nthree\n")
                .is_ok()
        );
        assert!(
            COMMANDS
                .check_comment_text(root.path(), "HEAD", "bom.txt", "one\ntwo\nchanged\n")
                .is_err()
        );
    }
}
