//! Explicit fork-to-upstream PR creation, using the same bounded forge worker.

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct CreatePullRequest {
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

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct PullRequestPush {
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

/// Push only the reviewed commit to the reviewed remote, refusing if either moved since review.
pub(super) fn push_reviewed(
    commands: &super::ForgeCommands<'_>,
    root: &std::path::Path,
    request: &CreatePullRequest,
) -> Result<(), String> {
    let Some(push) = &request.push else {
        return Ok(());
    };
    let reference = format!("refs/heads/{}", request.head_branch);
    let current = commands.run(root, "git", &["rev-parse", "--verify", &reference])?;
    if current.trim() != push.commit {
        return Err("The local branch changed. Review the PR again before pushing".into());
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
    if repository_from_url(url, &request.host).as_deref() != Some(&request.head_repository) {
        return Err("The push remote no longer points to the selected head repository".into());
    }
    // Push only the reviewed commit and branch, not configured refspecs or other branches.
    commands
        .run(
            root,
            "git",
            &["push", "--", url, &format!("{}:{reference}", push.commit)],
        )
        .map(|_| ())
}
