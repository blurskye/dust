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
    Commented(Result<(), String>),
}

/// The CLI that talks to the repository's forge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Forge {
    GitHub,
    Gitea,
}

pub struct ForgeJobs {
    sender: Sender<ForgeReply>,
    replies: Receiver<ForgeReply>,
}

impl Default for ForgeJobs {
    fn default() -> Self {
        let (sender, replies) = mpsc::channel();
        Self { sender, replies }
    }
}

impl ForgeJobs {
    pub fn try_reply(&self) -> Option<ForgeReply> {
        self.replies.try_recv().ok()
    }

    fn spawn(&self, job: impl FnOnce() -> ForgeReply + Send + 'static) {
        let sender = self.sender.clone();
        thread::spawn(move || {
            let _ = sender.send(job());
        });
    }

    pub fn list(&self, root: PathBuf) {
        self.spawn(move || match open_pull_requests(&root) {
            Ok(list) => ForgeReply::List(Ok(list)),
            Err(error) => ForgeReply::Offline {
                error,
                local: local_pull_requests(&root),
            },
        });
    }

    /// Check the pull request out into `<parent>/<repo>-pr-<number>` on branch `pr-<number>`,
    /// so the current worktree and its branch are left alone. An existing one is updated.
    pub fn checkout(&self, root: PathBuf, number: u64) {
        self.spawn(move || {
            ForgeReply::CheckedOut((|| {
                let target = pr_worktree(&root, number)?;
                if !target.is_dir() {
                    let path = target.to_string_lossy().into_owned();
                    run(&root, "git", &["worktree", "add", "--detach", &path])?;
                }
                let branch = format!("pr-{number}");
                with_forge(&root, |forge| match forge {
                    Forge::GitHub => run(
                        &target,
                        "gh",
                        &["pr", "checkout", &number.to_string(), "--branch", &branch],
                    ),
                    // Gitea and Forgejo publish every pull request's head as refs/pull/N/head.
                    Forge::Gitea => {
                        run(&target, "git", &["fetch", "origin", &format!("refs/pull/{number}/head")])?;
                        run(&target, "git", &["checkout", "-B", &branch, "FETCH_HEAD"])
                    }
                })?;
                Ok(target)
            })())
        });
    }

    /// Review comments of the pull request for the current branch.
    pub fn comments(&self, root: PathBuf) {
        self.spawn(move || {
            ForgeReply::Comments(with_forge(&root, |forge| match forge {
                Forge::GitHub => {
                    let number = current_number(&root, Forge::GitHub)?;
                    let output = run(
                        &root,
                        "gh",
                        &[
                            "api",
                            &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments"),
                            "--paginate",
                            "--jq",
                            ".[]",
                        ],
                    )?;
                    parse_comments(&output, &root)
                }
                Forge::Gitea => {
                    let number = current_number(&root, Forge::Gitea)?;
                    let reviews = run(
                        &root,
                        "tea",
                        &["api", &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews")],
                    )?;
                    let reviews: Vec<Value> =
                        serde_json::from_str(&reviews).map_err(|error| error.to_string())?;
                    let mut comments = Vec::new();
                    for review in reviews {
                        let Some(id) = review["id"].as_u64() else {
                            continue;
                        };
                        let output = run(
                            &root,
                            "tea",
                            &[
                                "api",
                                &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews/{id}/comments"),
                            ],
                        )?;
                        comments.extend(parse_gitea_comments(&output, &root)?);
                    }
                    Ok(comments)
                }
            }))
        });
    }

    /// Comment on `line` (1-based) of `path` in the pull request for the current branch.
    pub fn comment(&self, root: PathBuf, path: PathBuf, line: usize, body: String) {
        self.spawn(move || {
            ForgeReply::Commented((|| {
                let relative = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().into_owned();
                with_forge(&root, |forge| match forge {
                    Forge::GitHub => {
                        let view = run(&root, "gh", &["pr", "view", "--json", "number,headRefOid"])?;
                        let view: Value =
                            serde_json::from_str(&view).map_err(|error| error.to_string())?;
                        let number =
                            view["number"].as_u64().ok_or("no pull request for this branch")?;
                        let commit = view["headRefOid"].as_str().unwrap_or_default();
                        run(
                            &root,
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
                        let number = current_number(&root, Forge::Gitea)?;
                        let review = serde_json::json!({
                            "event": "COMMENT",
                            "body": "",
                            "comments": [{ "path": relative, "body": body, "new_position": line }],
                        });
                        run(
                            &root,
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
                })
            })())
        });
    }
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
        Forge::GitHub => run(root, "gh", &["pr", "view", "--json", "number", "--jq", ".number"])?
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
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/pr-") {
            if let (Ok(number), Some(path)) = (branch.parse(), path.take()) {
                local.push((number, path));
            }
        }
    }
    local
}

fn pr_worktree(root: &Path, number: u64) -> Result<PathBuf, String> {
    let name = root
        .file_name()
        .ok_or("the workspace has no directory name")?
        .to_string_lossy();
    let parent = root.parent().ok_or("the workspace has no parent directory")?;
    Ok(parent.join(format!("{name}-pr-{number}")))
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
            author: item["author"]["login"].as_str().unwrap_or_default().to_owned(),
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
                draft: ["WIP:", "[WIP]"].iter().any(|prefix| title.starts_with(prefix)),
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
                line: item["line"].as_u64(),
                author: item["user"]["login"].as_str().unwrap_or_default().to_owned(),
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
            author: item["user"]["login"].as_str().unwrap_or_default().to_owned(),
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
        assert_eq!(remote_host("ssh://git@forgejo.example.org/a/b.git"), "forgejo.example.org");
        assert_eq!(remote_host("ssh://git@codeberg.org:2222/a/b"), "codeberg.org");
        assert!(remote_is_path("/srv/git/a.git"));
        assert!(remote_is_path("../a.git"));
        assert!(remote_is_path("file:///srv/git/a.git"));
        assert!(!remote_is_path("git@github.com:a/b.git"));
    }

    #[test]
    fn pull_requests_get_a_sibling_worktree() {
        assert_eq!(
            pr_worktree(Path::new("/src/dust"), 12).unwrap(),
            PathBuf::from("/src/dust-pr-12")
        );
    }
}
