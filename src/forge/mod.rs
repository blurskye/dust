//! The forge layer: providers for GitHub, GitLab, Gitea and Forgejo behind one
//! `Read`/`Write` model, and `ForgeJobs`, the single cancellable worker the Git screen and the
//! IDE send them through. The IDE's pull-request picker, review comments and checkout
//! go through the provider layer like everything else; replies arrive on a channel.

pub mod batch;
pub mod cache;
pub mod capability;
pub mod checkout;
pub mod cli;
pub mod command;
pub mod credential;
pub mod detect;
pub mod etag;
pub mod host_state;
pub mod http;
pub mod incremental;
pub mod inflight;
pub mod legacy;
pub mod merge;
pub mod model;
pub mod provider;
pub mod pull_request;
pub mod ratelimit;
pub mod records;
pub mod request;
pub mod sandbox;
pub mod template;
pub mod transport;
mod worker;
pub mod yaml_lite;
pub mod zip;

pub use cli::redact;
pub use worker::ForgeTarget;
pub use worker::Served;
pub use worker::cached_fresh;
pub use worker::cached_read;
pub use worker::connect;
pub use worker::connect_to;

use crate::forge::model::ItemKind;
use crate::forge::model::Side;
use crate::forge::provider::Conditional;
use crate::forge::provider::Fetched;
use crate::forge::provider::ForgeContext;
use crate::forge::provider::Provider;
use crate::forge::request::Filter;
use crate::forge::request::Page;
use crate::forge::request::Payload;
use crate::forge::request::Read;
use crate::forge::request::Write;
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
    /// A provider-layer read.
    Fetched(
        request::Read,
        Result<provider::Fetched, request::ForgeError>,
    ),
    /// A provider-layer write, porcelain included.
    Written(
        request::Write,
        Result<request::Receipt, request::ForgeError>,
    ),
}

#[derive(Debug)]
pub struct ForgeEvent {
    pub root: PathBuf,
    pub request_id: u64,
    pub reply: ForgeReply,
}

pub struct ForgeJobs {
    sender: Sender<ForgeEvent>,
    replies: Receiver<ForgeEvent>,
    worker: Option<thread::JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    name: &'static str,
}

impl Default for ForgeJobs {
    fn default() -> Self {
        let (sender, replies) = mpsc::channel();
        Self {
            sender,
            replies,
            worker: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            name: "dust-forge",
        }
    }
}

/// The IDE picker's page: one request on every provider.
const PICKER_PAGE: u32 = 100;

impl ForgeJobs {
    /// The low-priority worker that warms the cache; the interactive worker
    /// stays free for what the user is looking at.
    pub fn warm() -> Self {
        let mut jobs = Self::default();
        jobs.name = "dust-forge-warm";
        jobs
    }

    /// The warm worker: its reads are background work (held back sooner while anonymous).
    pub fn warming(&self) -> bool {
        self.name == "dust-forge-warm"
    }

    pub fn busy(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }

    pub fn try_reply(&self) -> Option<ForgeEvent> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return None;
        }
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
                .name(self.name.into())
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

    /// The open pull requests (merge requests on GitLab) of the repository's forge.
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
                let site =
                    checkout::Site::resolve(root, &ForgeTarget::remote(), commands.cancelled)
                        .map_err(|error| error.to_string())?;
                commands
                    .worktree_checkout(root, &site, number)
                    .map_err(|error| error.to_string())
            })())
        })
    }

    /// Review comments of the pull request for the current branch.
    pub fn comments(&mut self, root: PathBuf, request_id: u64) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            ForgeReply::Comments((|| {
                let (provider, ctx) = commands.connect(root)?;
                let number = commands.current_number(root, provider.as_ref(), &ctx)?;
                let threads = match commands.read(provider.as_ref(), &ctx, Read::Threads(number))? {
                    Payload::Threads(threads) => threads,
                    other => return Err(unexpected(&other)),
                };
                Ok(review_comments(&threads, root))
            })())
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
            ForgeReply::CommentTarget((|| {
                let (provider, ctx) = commands.connect(root)?;
                let head = commands.current_head(root, provider.as_ref(), &ctx)?;
                Ok(ReviewCommentTarget {
                    path,
                    line,
                    text,
                    head,
                })
            })())
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
            ForgeReply::Commented((|| {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                let (provider, ctx) = commands.connect(root)?;
                check_comment_head(
                    &head,
                    &commands.current_head(root, provider.as_ref(), &ctx)?,
                )?;
                commands.check_comment_text(root, &head.commit, &relative, &text)?;
                let write = Write::InlineComment {
                    number: head.number,
                    path: relative,
                    line: line as u64,
                    side: Side::Right,
                    head: head.commit.clone(),
                    body,
                };
                let receipt = provider
                    .write(&ctx, &write, commands.cancelled)
                    .map_err(|error| error.to_string())?;
                worker::invalidate(&ctx, &receipt.invalidates);
                Ok(())
            })())
        })
    }
}

impl Drop for ForgeJobs {
    fn drop(&mut self) {
        self.cancel();
    }
}

const FORGE_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct ForgeCommands<'a> {
    cancelled: &'a AtomicBool,
}

fn unexpected(payload: &Payload) -> String {
    let name = format!("{payload:?}");
    let name = name.split(['(', ' ', '{']).next().unwrap_or_default();
    format!("the forge answered with {name}")
}

/// Every comment of every thread, at its line in the head; base-side comments have none.
fn review_comments(threads: &[model::ReviewThread], root: &Path) -> Vec<ReviewComment> {
    threads
        .iter()
        .flat_map(|thread| {
            thread.comments.iter().map(move |comment| {
                let position = comment.position.as_ref();
                let path = thread
                    .path
                    .clone()
                    .or_else(|| position.map(|position| position.path.clone()))
                    .unwrap_or_default();
                let line = match thread.side {
                    Side::Left => None,
                    Side::Right => position.and_then(|position| position.line).or(thread.line),
                };
                ReviewComment {
                    path: root.join(path),
                    line: line.filter(|line| *line > 0),
                    author: comment.author.login.clone(),
                    body: comment.body.clone(),
                }
            })
        })
        .collect()
}

impl ForgeCommands<'_> {
    fn connect(&self, root: &Path) -> Result<(Arc<dyn Provider>, ForgeContext), String> {
        worker::connect(root, &ForgeTarget::remote(), self.cancelled)
            .map_err(|error| error.to_string())
    }

    /// One uncached read: the IDE acts on what it is told (heads, threads).
    fn read(
        &self,
        provider: &dyn Provider,
        ctx: &ForgeContext,
        read: Read,
    ) -> Result<Payload, String> {
        match worker::fetch_through(provider, ctx, &read, Conditional::default(), self.cancelled) {
            Ok(Fetched::Payload { value, .. }) => Ok(value),
            Ok(Fetched::NotModified) => Err("the forge answered 304 to a full read".into()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// The picker's list: the first page of open pull requests, any provider.
    fn open_pull_requests(&self, root: &Path) -> Result<Vec<PullRequest>, String> {
        let (provider, ctx) = self.connect(root)?;
        self.pull_requests(provider.as_ref(), &ctx)
    }

    fn pull_requests(
        &self,
        provider: &dyn Provider,
        ctx: &ForgeContext,
    ) -> Result<Vec<PullRequest>, String> {
        let read = Read::List {
            kind: ItemKind::PullRequest,
            filter: Filter::default(),
            page: Page::first(PICKER_PAGE),
        };
        let rows = match self.read(provider, ctx, read)? {
            Payload::Rows(rows) => rows.items,
            other => return Err(unexpected(&other)),
        };
        Ok(rows
            .into_iter()
            .map(|row| PullRequest {
                number: row.number,
                draft: row.draft || row.lifecycle == model::Lifecycle::Draft,
                title: row.title,
                author: row.author.login,
                branch: row.head_branch.unwrap_or_default(),
            })
            .collect())
    }

    /// The pull request of the current branch: `pr-N` (Dust's own checkout), a branch whose
    /// upstream is `refs/pull/N/head` or `refs/merge-requests/N/head` (a fork checkout), else
    /// the forge's open pull request for the upstream branch name.
    fn current_number(
        &self,
        root: &Path,
        provider: &dyn Provider,
        ctx: &ForgeContext,
    ) -> Result<u64, String> {
        const NONE: &str = "no pull request for this branch";
        let branch = self.run(root, "git", &["branch", "--show-current"])?;
        let branch = branch.trim();
        if branch.is_empty() {
            return Err(NONE.into());
        }
        if let Some(number) = branch.strip_prefix("pr-").and_then(|n| n.parse().ok()) {
            return Ok(number);
        }
        let merge = self
            .run(
                root,
                "git",
                &["config", "--get", &format!("branch.{branch}.merge")],
            )
            .ok()
            .map(|merge| merge.trim().to_owned());
        if let Some(number) = merge.as_deref().and_then(checkout::number_of_ref) {
            return Ok(number);
        }
        let name = merge
            .as_deref()
            .and_then(|merge| merge.strip_prefix("refs/heads/"))
            .unwrap_or(branch);
        match self.read(provider, ctx, Read::PullRequestForBranch(name.into()))? {
            Payload::MaybePullRequest(Some(number)) => Ok(number),
            Payload::MaybePullRequest(None) => Err(NONE.into()),
            other => Err(unexpected(&other)),
        }
    }

    fn current_head(
        &self,
        root: &Path,
        provider: &dyn Provider,
        ctx: &ForgeContext,
    ) -> Result<PullRequestHead, String> {
        let number = self.current_number(root, provider, ctx)?;
        let pull = match self.read(provider, ctx, Read::PullRequest(number))? {
            Payload::PullRequest(pull) => pull,
            other => return Err(unexpected(&other)),
        };
        let commit = [&pull.head.sha, &pull.pinned_head]
            .into_iter()
            .find(|sha| !sha.is_empty())
            .ok_or("pull request head is missing")?
            .clone();
        Ok(PullRequestHead { number, commit })
    }

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
}

fn check_comment_head(expected: &PullRequestHead, current: &PullRequestHead) -> Result<(), String> {
    if expected != current {
        return Err("the pull request or its head changed; start a new review comment".into());
    }
    Ok(())
}

impl ForgeCommands<'_> {
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

    /// Put `branch` at `commit`: create it there, or fast-forward an existing one. A branch
    /// that diverged is left where it is, and so is the current checkout.
    fn update_pr_branch(&self, root: &Path, branch: &str, commit: &str) -> Result<String, String> {
        let reference = format!("refs/heads/{branch}");
        let exists = self
            .run(
                root,
                "git",
                &["rev-parse", "--verify", "--quiet", &reference],
            )
            .is_ok();
        if !exists {
            return self.run(
                root,
                "git",
                &["checkout", "--no-track", "-b", branch, commit],
            );
        }
        let contains = |ancestor: &str, descendant: &str| {
            self.run(
                root,
                "git",
                &["merge-base", "--is-ancestor", ancestor, descendant],
            )
            .is_ok()
        };
        if !contains(&reference, commit) && !contains(commit, &reference) {
            return Err(format!(
                "{branch} and the pull request diverged; PR branch preserved, reconcile it before updating"
            ));
        }
        self.run(root, "git", &["checkout", branch])?;
        self.run(root, "git", &["merge", "--ff-only", commit])
            .map_err(|error| format!("{error}; PR branch preserved, reconcile it before updating"))
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

/// A bare host name (`[forge.hosts]` keys, `ForgeTarget::host`), lower-cased.
fn valid_host(host: &str) -> Result<String, String> {
    if host.is_empty()
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
        || host.contains(['/', '\\', '@', '?', '#', ':'])
    {
        return Err("Invalid forge host".into());
    }
    let url = reqwest::Url::parse(&format!("https://{host}")).map_err(|_| "Invalid forge host")?;
    let host = url
        .host_str()
        .ok_or("Invalid forge host")?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("Invalid forge host".into());
    }
    Ok(host)
}

#[cfg(test)]
mod tests {
    use crate::forge::*;

    static CANCELLED: AtomicBool = AtomicBool::new(false);
    static COMMANDS: ForgeCommands<'static> = ForgeCommands {
        cancelled: &CANCELLED,
    };

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
                    // Renamed into place so the test never reads a half-written pid.
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
        // Bounds a stuck worker; the sleeping child would outlive it if cancel failed.
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
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
        COMMANDS
            .update_pr_branch(root, "pr-7", base.trim())
            .unwrap();
        COMMANDS
            .run(root, "git", &["commit", "--allow-empty", "-m", "local"])
            .unwrap();
        let local = COMMANDS.run(root, "git", &["rev-parse", "HEAD"]).unwrap();
        COMMANDS
            .update_pr_branch(root, "pr-7", base.trim())
            .unwrap();
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
        assert!(
            COMMANDS
                .update_pr_branch(root, "pr-7", remote.trim())
                .is_err()
        );
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
        COMMANDS
            .update_pr_branch(root, "pr-8", local.trim())
            .unwrap();
        assert_eq!(
            COMMANDS.run(root, "git", &["rev-parse", "HEAD"]).unwrap(),
            local
        );
    }

    #[test]
    fn forge_hosts_are_validated_and_normalised() {
        assert_eq!(valid_host("GitHub.COM.").unwrap(), "github.com");
        for host in [
            "",
            " ",
            "github.com/path",
            "github.com:443",
            "user@github.com",
            "x\n.test",
            "https://github.com",
        ] {
            assert!(valid_host(host).is_err(), "{host:?}");
        }
    }

    #[test]
    fn review_threads_become_head_line_comments() {
        use crate::forge::model::*;
        let comment = |body: &str, line: Option<u64>| Comment {
            author: User::login("ana"),
            body: body.into(),
            position: line.map(|line| CommentPosition {
                path: "src/a.rs".into(),
                line: Some(line),
                original_line: None,
                commit: None,
            }),
            ..Comment::default()
        };
        let threads = [
            ReviewThread {
                path: Some("src/a.rs".into()),
                line: Some(3),
                comments: vec![comment("nit", None), comment("reply", Some(4))],
                ..ReviewThread::default()
            },
            ReviewThread {
                path: None,
                side: Side::Left,
                comments: vec![comment("old side", Some(9))],
                ..ReviewThread::default()
            },
            ReviewThread {
                path: Some("gone.rs".into()),
                line: Some(0),
                comments: vec![comment("outdated", None)],
                ..ReviewThread::default()
            },
        ];
        let comments = review_comments(&threads, Path::new("/r"));
        assert_eq!(
            comments
                .iter()
                .map(|comment| (
                    comment.path.to_string_lossy().into_owned(),
                    comment.line,
                    comment.body.as_str(),
                    comment.author.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                ("/r/src/a.rs".to_owned(), Some(3), "nit", "ana"),
                ("/r/src/a.rs".to_owned(), Some(4), "reply", "ana"),
                ("/r/src/a.rs".to_owned(), None, "old side", "ana"),
                ("/r/gone.rs".to_owned(), None, "outdated", "ana"),
            ]
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
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
