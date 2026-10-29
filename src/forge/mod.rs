//! Pull requests through the GitHub CLI (`gh`), which already owns sign-in, hosts and paging.
//! Each request runs `gh` (and `git`) on a worker thread; replies arrive on a channel.

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
    /// The worktree the pull request was checked out into.
    CheckedOut(Result<PathBuf, String>),
    Comments(Result<Vec<ReviewComment>, String>),
    Commented(Result<(), String>),
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
        self.spawn(move || {
            ForgeReply::List(
                run(
                    &root,
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
            )
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
                run(
                    &target,
                    "gh",
                    &["pr", "checkout", &number.to_string(), "--branch", &branch],
                )?;
                Ok(target)
            })())
        });
    }

    /// Review comments of the pull request for the current branch.
    pub fn comments(&self, root: PathBuf) {
        self.spawn(move || {
            ForgeReply::Comments((|| {
                let number = current_number(&root)?;
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
            })())
        });
    }

    /// Comment on `line` (1-based) of `path` in the pull request for the current branch.
    pub fn comment(&self, root: PathBuf, path: PathBuf, line: usize, body: String) {
        self.spawn(move || {
            ForgeReply::Commented((|| {
                let view = run(&root, "gh", &["pr", "view", "--json", "number,headRefOid"])?;
                let view: Value = serde_json::from_str(&view).map_err(|error| error.to_string())?;
                let number = view["number"].as_u64().ok_or("no pull request for this branch")?;
                let commit = view["headRefOid"].as_str().unwrap_or_default();
                let relative = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy();
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
            })())
        });
    }
}

fn current_number(root: &Path) -> Result<u64, String> {
    run(root, "gh", &["pr", "view", "--json", "number", "--jq", ".number"])?
        .trim()
        .parse()
        .map_err(|_| "no pull request for this branch".to_owned())
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
        .map_err(|error| format!("{program}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        let error = error.trim();
        Err(if error.is_empty() {
            format!("{program} failed ({})", output.status)
        } else {
            error.lines().last().unwrap_or(error).to_owned()
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
    fn pull_requests_get_a_sibling_worktree() {
        assert_eq!(
            pr_worktree(Path::new("/src/dust"), 12).unwrap(),
            PathBuf::from("/src/dust-pr-12")
        );
    }
}
