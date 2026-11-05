//! Pull requests through the forge's own CLI, which already owns sign-in, hosts and paging:
//! `gh` for GitHub and `tea` for Gitea and Forgejo. Each request runs the CLI (and `git`) on a
//! worker thread; replies arrive on a channel.

use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

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
    pub generation: u64,
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
}

impl Default for ForgeJobs {
    fn default() -> Self {
        let (sender, replies) = mpsc::channel();
        Self { sender, replies }
    }
}

impl ForgeJobs {
    pub fn try_reply(&self) -> Option<ForgeEvent> {
        self.replies.try_recv().ok()
    }

    #[cfg(test)]
    pub(crate) fn reply_for_test(&self, root: PathBuf, generation: u64, reply: ForgeReply) {
        self.sender
            .send(ForgeEvent {
                root,
                generation,
                reply,
            })
            .unwrap();
    }

    fn spawn(
        &self,
        root: PathBuf,
        generation: u64,
        job: impl FnOnce(&Path) -> ForgeReply + Send + 'static,
    ) {
        let sender = self.sender.clone();
        thread::spawn(move || {
            let reply = job(&root);
            let _ = sender.send(ForgeEvent {
                root,
                generation,
                reply,
            });
        });
    }

    pub fn list(&self, root: PathBuf, generation: u64) {
        self.spawn(root, generation, move |root| {
            match open_pull_requests(root) {
                Ok(list) => ForgeReply::List(Ok(list)),
                Err(error) => ForgeReply::Offline {
                    error,
                    local: local_pull_requests(root),
                },
            }
        });
    }

    /// Check the pull request out into `<parent>/<repo>-pr-<number>` on branch `pr-<number>`,
    /// so the current worktree and its branch are left alone. An existing one is updated.
    pub fn checkout(
        &self,
        root: PathBuf,
        generation: u64,
        number: u64,
        workspace: crate::workspace::WorkspaceState,
    ) {
        self.spawn(root, generation, move |root| {
            ForgeReply::CheckedOut((|| {
                workspace
                    .authorize_execution(crate::workspace::ExecutionSource::ProjectLocal)
                    .map_err(|error| error.to_string())?;
                let target = pr_worktree(root, number)?;
                prepare_worktree(root, &target)?;
                let branch = format!("pr-{number}");
                with_forge(root, |forge| match forge {
                    Forge::GitHub => run(
                        &target,
                        "gh",
                        &["pr", "checkout", &number.to_string(), "--branch", &branch],
                    ),
                    // Gitea and Forgejo publish every pull request's head as refs/pull/N/head.
                    Forge::Gitea => {
                        run(
                            &target,
                            "git",
                            &["fetch", "origin", &format!("refs/pull/{number}/head")],
                        )?;
                        update_pr_branch(&target, &branch)
                    }
                })?;
                Ok(target)
            })())
        });
    }

    /// Review comments of the pull request for the current branch.
    pub fn comments(&self, root: PathBuf, generation: u64) {
        self.spawn(root, generation, move |root| {
            ForgeReply::Comments(with_forge(root, |forge| match forge {
                Forge::GitHub => {
                    let number = current_number(root, Forge::GitHub)?;
                    let output = run(
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
                    let number = current_number(root, Forge::Gitea)?;
                    let reviews = run(
                        root,
                        "tea",
                        &[
                            "api",
                            &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews"),
                        ],
                    )?;
                    let reviews: Vec<Value> =
                        serde_json::from_str(&reviews).map_err(|error| error.to_string())?;
                    let mut comments = Vec::new();
                    for review in reviews {
                        let Some(id) = review["id"].as_u64() else {
                            continue;
                        };
                        let output = run(
                            root,
                            "tea",
                            &[
                                "api",
                                &format!(
                                    "repos/{{owner}}/{{repo}}/pulls/{number}/reviews/{id}/comments"
                                ),
                            ],
                        )?;
                        comments.extend(parse_gitea_comments(&output, root)?);
                    }
                    Ok(comments)
                }
            }))
        });
    }

    /// Capture the pull request and head before a review draft is opened.
    pub fn comment_target(
        &self,
        root: PathBuf,
        generation: u64,
        path: PathBuf,
        line: usize,
        text: String,
    ) {
        self.spawn(root, generation, move |root| {
            ForgeReply::CommentTarget(with_forge(root, |forge| current_head(root, forge))
                .map(|head| ReviewCommentTarget { path, line, text, head }))
        });
    }

    pub fn comment(
        &self,
        root: PathBuf,
        generation: u64,
        target: ReviewCommentTarget,
        body: String,
    ) {
        let ReviewCommentTarget { path, line, text, head } = target;
        self.spawn(root, generation, move |root| {
            ForgeReply::Commented({
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                with_forge(root, |forge| {
                    check_comment_head(&head, &current_head(root, forge)?)?;
                    let number = head.number;
                    let commit = &head.commit;
                    check_comment_text(root, commit, &relative, &text)?;
                    match forge {
                    Forge::GitHub => {
                        run(
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
                            "event": "COMMENT",
                            "body": "",
                            "comments": [{ "path": relative, "body": body, "new_position": line }],
                        });
                        run(
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
        });
    }
}

fn current_head(root: &Path, forge: Forge) -> Result<PullRequestHead, String> {
    let (number, commit) = match forge {
        Forge::GitHub => {
            let output = run(root, "gh", &["pr", "view", "--json", "number,headRefOid"])?;
            let view: Value = serde_json::from_str(&output).map_err(|error| error.to_string())?;
            let number = view["number"].as_u64().ok_or("no pull request for this branch")?;
            let commit = view["headRefOid"].as_str().ok_or("pull request head is missing")?.to_owned();
            (number, commit)
        }
        Forge::Gitea => {
            let number = current_number(root, forge)?;
            let output = run(root, "tea", &["api", &format!("repos/{{owner}}/{{repo}}/pulls/{number}")])?;
            let view: Value = serde_json::from_str(&output).map_err(|error| error.to_string())?;
            let commit = view["head"]["sha"].as_str().ok_or("pull request head is missing")?.to_owned();
            (number, commit)
        }
    };
    Ok(PullRequestHead { number, commit })
}

fn check_comment_head(expected: &PullRequestHead, current: &PullRequestHead) -> Result<(), String> {
    if expected != current {
        return Err("the pull request or its head changed; start a new review comment".into());
    }
    Ok(())
}

fn check_comment_text(root: &Path, commit: &str, path: &str, text: &str) -> Result<(), String> {
    let head = run(root, "git", &["show", &format!("{commit}:{path}")])?;
    if head.replace("\r\n", "\n").replace('\r', "\n") != text {
        return Err(
            "the buffer differs from the pull request head; comment on an unchanged buffer".into(),
        );
    }
    Ok(())
}

fn open_pull_requests(root: &Path) -> Result<Vec<PullRequest>, String> {
    with_forge(root, |forge| match forge {
        Forge::GitHub => run(
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
        Forge::Gitea => tea_list(root),
    })
}

fn tea_list(root: &Path) -> Result<Vec<PullRequest>, String> {
    run(
        root,
        "tea",
        &[
            "pulls",
            "list",
            "--state",
            "open",
            "--limit",
            "200",
            "--output",
            "json",
            "--fields",
            "index,title,author,head",
        ],
    )
    .and_then(|output| parse_tea_list(&output))
}

/// GitHub remotes go to `gh`; every other host is taken to be Gitea or Forgejo.
fn forge(root: &Path) -> Result<Forge, String> {
    let url = run(root, "git", &["remote", "get-url", "origin"])
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
fn with_forge<T>(root: &Path, job: impl Fn(Forge) -> Result<T, String>) -> Result<T, String> {
    let first = forge(root)?;
    let other = match first {
        Forge::GitHub => Forge::Gitea,
        Forge::Gitea => Forge::GitHub,
    };
    job(first).or_else(|error| job(other).map_err(|_| error))
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

fn current_number(root: &Path, forge: Forge) -> Result<u64, String> {
    match forge {
        Forge::GitHub => run(
            root,
            "gh",
            &["pr", "view", "--json", "number", "--jq", ".number"],
        )?
        .trim()
        .parse()
        .map_err(|_| "no pull request for this branch".to_owned()),
        Forge::Gitea => {
            let branch = run(root, "git", &["branch", "--show-current"])?;
            let branch = branch.trim();
            // A pull request checked out by Dust is on `pr-<number>`.
            if let Some(number) = branch.strip_prefix("pr-").and_then(|n| n.parse().ok()) {
                return Ok(number);
            }
            tea_list(root)?
                .into_iter()
                .find(|pr| pr.branch == branch)
                .map(|pr| pr.number)
                .ok_or_else(|| "no pull request for this branch".to_owned())
        }
    }
}

/// Pull requests already checked out in a worktree on a `pr-<number>` branch.
fn local_pull_requests(root: &Path) -> Vec<(u64, PathBuf)> {
    let Ok(output) = run(root, "git", &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let mut local = Vec::new();
    let mut path = None;
    for line in output.lines() {
        if let Some(worktree) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(worktree));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/pr-")
            && let (Ok(number), Some(path)) = (branch.parse(), path.take())
        {
            local.push((number, path));
        }
    }
    local
}

fn pr_worktree(root: &Path, number: u64) -> Result<PathBuf, String> {
    if let Some((_, path)) = local_pull_requests(root)
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

fn prepare_worktree(root: &Path, target: &Path) -> Result<(), String> {
    if !target.try_exists().map_err(|error| error.to_string())? {
        let path = target.to_str().ok_or("worktree path is not UTF-8")?;
        return run(root, "git", &["worktree", "add", "--detach", path]).map(|_| ());
    }
    let source = git2::Repository::discover(root).map_err(|error| error.to_string())?;
    let repository = git2::Repository::open(target).map_err(|error| error.to_string())?;
    let common = source
        .commondir()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if repository
        .commondir()
        .canonicalize()
        .map_err(|error| error.to_string())?
        != common
    {
        return Err("pull request checkout belongs to another repository".into());
    }
    let target = target.canonicalize().map_err(|error| error.to_string())?;
    let registered = source
        .worktrees()
        .map_err(|error| error.to_string())?
        .iter()
        .flatten()
        .any(|name| {
            source.find_worktree(name).is_ok_and(|worktree| {
                worktree.validate().is_ok()
                    && worktree
                        .path()
                        .canonicalize()
                        .is_ok_and(|path| path == target)
            })
        });
    let common_repository = git2::Repository::open(&common).map_err(|error| error.to_string())?;
    let main = common_repository
        .workdir()
        .and_then(|path| path.canonicalize().ok());
    if !registered && main.as_ref() != Some(&target) {
        return Err("pull request checkout is not a registered worktree".into());
    }
    Ok(())
}

fn update_pr_branch(root: &Path, branch: &str) -> Result<String, String> {
    let repository = git2::Repository::open(root).map_err(|error| error.to_string())?;
    if repository
        .find_branch(branch, git2::BranchType::Local)
        .is_ok()
    {
        run(root, "git", &["checkout", branch])?;
        run(root, "git", &["merge", "--ff-only", "FETCH_HEAD"])
            .map_err(|error| format!("{error}; PR branch preserved, reconcile it before updating"))
    } else {
        run(root, "git", &["checkout", "-b", branch, "FETCH_HEAD"])
    }
}

fn run(cwd: &Path, program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("{program} is not installed"),
            _ => format!("{program}: {error}"),
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        let error = error.trim();
        // The first line says what went wrong; the rest is usually advice on flags.
        Err(match error.lines().find(|line| !line.trim().is_empty()) {
            Some(line) => format!("{program}: {}", line.trim()),
            None => format!("{program} failed ({})", output.status),
        })
    }
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

/// `tea … --output json` prints rows keyed by field name, every value a string.
fn parse_tea_list(output: &str) -> Result<Vec<PullRequest>, String> {
    let items: Vec<Value> = serde_json::from_str(output).map_err(|error| error.to_string())?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let title = item["title"].as_str().unwrap_or_default().to_owned();
            Some(PullRequest {
                number: item["index"].as_str()?.trim().parse().ok()?,
                draft: ["WIP:", "[WIP]"]
                    .iter()
                    .any(|prefix| title.starts_with(prefix)),
                title,
                author: item["author"].as_str().unwrap_or_default().to_owned(),
                branch: item["head"].as_str().unwrap_or_default().to_owned(),
            })
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

/// A Gitea review's comments; `position` is the line in the head, 0 once it is gone.
fn parse_gitea_comments(output: &str, root: &Path) -> Result<Vec<ReviewComment>, String> {
    let items: Vec<Value> = serde_json::from_str(output).map_err(|error| error.to_string())?;
    Ok(items
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
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let list = r#"[{"index":"12","title":"WIP: Faster","author":"ana","head":"fast"}]"#;
        assert_eq!(
            parse_tea_list(list).unwrap(),
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
        let parsed = parse_gitea_comments(comments, Path::new("/r")).unwrap();
        assert_eq!(parsed[0].line, Some(4));
        assert_eq!(parsed[1].line, None);
    }

    #[test]
    fn remote_hosts_pick_the_forge_cli() {
        assert_eq!(remote_host("git@github.com:a/b.git"), "github.com");
        assert_eq!(remote_host("https://github.com/a/b"), "github.com");
        assert_eq!(
            remote_host("ssh://git@forgejo.example.org/a/b.git"),
            "forgejo.example.org"
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

    fn repository(root: &Path) {
        run(root, "git", &["init"]).unwrap();
        run(root, "git", &["config", "user.name", "Test"]).unwrap();
        run(root, "git", &["config", "user.email", "test@example.com"]).unwrap();
        run(root, "git", &["commit", "--allow-empty", "-m", "initial"]).unwrap();
    }

    #[test]
    fn pull_requests_reuse_sibling_worktrees_from_any_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        repository(&root);
        let first = pr_worktree(&root, 1).unwrap();
        assert_eq!(first, directory.path().join("repo-pr-1"));
        prepare_worktree(&root, &first).unwrap();
        run(&first, "git", &["checkout", "-b", "pr-1"]).unwrap();
        let second = pr_worktree(&first, 2).unwrap();
        assert_eq!(second, directory.path().join("repo-pr-2"));
        prepare_worktree(&first, &second).unwrap();
        assert_eq!(pr_worktree(&second, 1).unwrap(), first);
        prepare_worktree(&second, &first).unwrap();
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
        let before = run(&target, "git", &["rev-parse", "HEAD"]).unwrap();
        assert!(prepare_worktree(&root, &target).is_err());
        assert_eq!(run(&target, "git", &["rev-parse", "HEAD"]).unwrap(), before);
    }

    #[test]
    fn updating_pr_branches_preserves_local_commits_and_fast_forwards() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        repository(root);
        let base = run(root, "git", &["rev-parse", "HEAD"]).unwrap();
        run(root, "git", &["fetch", ".", base.trim()]).unwrap();
        update_pr_branch(root, "pr-7").unwrap();
        run(root, "git", &["commit", "--allow-empty", "-m", "local"]).unwrap();
        let local = run(root, "git", &["rev-parse", "HEAD"]).unwrap();
        update_pr_branch(root, "pr-7").unwrap();
        assert_eq!(run(root, "git", &["rev-parse", "HEAD"]).unwrap(), local);
        run(root, "git", &["checkout", "--detach", base.trim()]).unwrap();
        run(root, "git", &["commit", "--allow-empty", "-m", "remote"]).unwrap();
        let remote = run(root, "git", &["rev-parse", "HEAD"]).unwrap();
        run(root, "git", &["fetch", ".", remote.trim()]).unwrap();
        assert!(update_pr_branch(root, "pr-7").is_err());
        assert_eq!(run(root, "git", &["rev-parse", "pr-7"]).unwrap(), local);
        run(root, "git", &["fetch", ".", local.trim()]).unwrap();
        run(root, "git", &["branch", "pr-8", base.trim()]).unwrap();
        update_pr_branch(root, "pr-8").unwrap();
        assert_eq!(run(root, "git", &["rev-parse", "HEAD"]).unwrap(), local);
    }
    #[test]
    fn untrusted_checkout_is_rejected_before_running_commands() {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [(
                "workspace".into(),
                crate::workspace::RootIdentity {
                    canonical_path: root.path().into(),
                    filesystem_identity: None,
                },
            )],
        )
        .unwrap();
        let jobs = ForgeJobs::default();
        jobs.checkout(root.path().into(), 0, 1, workspace);
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
        let original = PullRequestHead { number: 7, commit: "abc".into() };
        assert!(check_comment_head(&original, &original).is_ok());
        assert!(check_comment_head(&original, &PullRequestHead { number: 8, ..original.clone() }).is_err());
        assert!(check_comment_head(&original, &PullRequestHead { commit: "def".into(), ..original.clone() }).is_err());
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
        assert!(check_comment_text(root.path(), &commit, "file.txt", "one\ntwo\n").is_ok());
        assert!(
            check_comment_text(root.path(), &commit, "file.txt", "inserted\none\ntwo\n").is_err()
        );
    }
}
