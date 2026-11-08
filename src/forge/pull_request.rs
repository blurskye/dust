//! Explicit fork-to-upstream PR creation, using the same bounded forge worker.
use super::{ForgeJobs, ForgeReply};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CreatePullRequest {
    pub host: String,
    pub base_repository: String,
    pub base_branch: String,
    pub head_repository: String,
    pub head_branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub push: Option<PullRequestPush>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestPush {
    pub remote: String,
    pub commit: String,
}

pub(crate) fn valid_repository(name: &str) -> bool {
    let parts = name.split('/').collect::<Vec<_>>();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && !matches!(*part, "." | "..")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}

pub(crate) fn repository_from_url(url: &str, host: &str) -> Option<String> {
    let url = crate::git::web_base(url)?;
    let name = url.path().trim_matches('/');
    (url.host_str()?.eq_ignore_ascii_case(host) && valid_repository(name)).then(|| name.into())
}

impl ForgeJobs {
    pub(crate) fn create_pull_request(
        &mut self,
        root: PathBuf,
        request_id: u64,
        request: CreatePullRequest,
    ) -> Result<(), String> {
        self.spawn(root, request_id, move |root, commands| {
            ForgeReply::Workspace((|| {
                if let Some(push) = &request.push {
                    let reference = format!("refs/heads/{}", request.head_branch);
                    let current =
                        commands.run(root, "git", &["rev-parse", "--verify", &reference])?;
                    if current.trim() != push.commit {
                        return Err(
                            "The local branch changed. Review the PR again before pushing".into(),
                        );
                    }
                    let urls = commands.run(
                        root,
                        "git",
                        &["remote", "get-url", "--push", "--all", &push.remote],
                    )?;
                    let urls = urls.lines().collect::<Vec<_>>();
                    let [url] = urls.as_slice() else {
                        return Err("Choose a remote with exactly one push URL".into());
                    };
                    if repository_from_url(url, &request.host).as_deref()
                        != Some(&request.head_repository)
                    {
                        return Err(
                            "The push remote no longer points to the selected head repository"
                                .into(),
                        );
                    }
                    // Push only the reviewed commit and branch, not configured refspecs or other branches.
                    commands.run(
                        root,
                        "git",
                        &["push", "--", url, &format!("{}:{reference}", push.commit)],
                    )?;
                }
                let (owner, repository) = request
                    .head_repository
                    .split_once('/')
                    .ok_or("Invalid head repository")?;
                let mut args = vec![
                    "api".into(),
                    "--hostname".into(),
                    request.host.clone(),
                    format!("repos/{}/pulls", request.base_repository),
                    "--method".into(),
                    "POST".into(),
                    "-f".into(),
                    format!("title={}", request.title),
                    "-f".into(),
                    format!("body={}", request.body),
                    "-f".into(),
                    format!("base={}", request.base_branch),
                    "-f".into(),
                    format!("head={owner}:{}", request.head_branch),
                    "-F".into(),
                    format!("draft={}", request.draft),
                ];
                if request.head_repository != request.base_repository {
                    // Unlike `gh pr create --head`, REST also disambiguates organization-owned forks.
                    args.extend(["-f".into(), format!("head_repo={repository}")]);
                }
                commands
                    .run(
                        root,
                        "gh",
                        &args.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                    .map_err(|error| {
                        if request.push.is_some() {
                            format!("Branch pushed, but PR creation failed: {error}")
                        } else {
                            error
                        }
                    })
            })())
        })
    }
}
