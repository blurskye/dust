//! The forge web URL for a line of a file, for `<leader>gB` (open) and `<leader>gY` (copy).

use std::path::Path;

/// The web URL of `file` at `line` (1-based) on the forge behind the `origin` remote (or the
/// only remote). The URL names the current branch, or the commit on a detached HEAD.
pub fn git_browse_url(file: &Path, line: usize) -> Result<String, String> {
    let repo = git2::Repository::discover(file).map_err(|_| "not inside a Git repository")?;
    let workdir = repo.workdir().ok_or("the repository has no working tree")?;
    let canonical = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let workdir = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
    let relative = canonical
        .strip_prefix(&workdir)
        .map_err(|_| "the file is outside the repository")?;
    let remote = repo
        .find_remote("origin")
        .or_else(|_| {
            let names = repo.remotes()?;
            match names.iter().flatten().collect::<Vec<_>>().as_slice() {
                [only] => repo.find_remote(only),
                _ => Err(git2::Error::from_str("no origin remote")),
            }
        })
        .map_err(|_| "the repository has no origin remote")?;
    let base = web_base(remote.url().ok_or("the remote URL is not UTF-8")?)
        .ok_or("the remote URL is not a web forge")?;
    let head = repo.head().map_err(|_| "the repository has no commits")?;
    let reference = match head.shorthand() {
        Some(branch) if head.is_branch() => branch.to_owned(),
        _ => head
            .peel_to_commit()
            .map_err(|_| "HEAD is not a commit")?
            .id()
            .to_string(),
    };
    let path = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(if base.contains("gitlab") {
        format!("{base}/-/blob/{reference}/{path}#L{line}")
    } else if base.contains("github") {
        format!("{base}/blob/{reference}/{path}#L{line}")
    } else {
        // Gitea, Forgejo and Codeberg.
        format!("{base}/src/branch/{reference}/{path}#L{line}")
    })
}

/// `git@host:owner/repo.git`, `ssh://git@host/owner/repo` or `https://host/owner/repo.git`
/// as `https://host/owner/repo`.
fn web_base(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/');
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    let (host, path) = if let Some(rest) = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("http://"))
        .or_else(|| remote.strip_prefix("ssh://"))
        .or_else(|| remote.strip_prefix("git://"))
    {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
        rest.split_once('/')?
    } else {
        let (user_host, path) = remote.split_once(':')?;
        (user_host.rsplit_once('@').map_or(user_host, |(_, host)| host), path)
    };
    // An ssh port is not the web port.
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("https://{host}/{path}"))
}

#[cfg(test)]
mod tests {
    use super::web_base;

    #[test]
    fn remote_urls_become_web_urls() {
        for remote in [
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "ssh://git@github.com:22/owner/repo",
            "https://github.com/owner/repo.git",
            "https://user@github.com/owner/repo/",
        ] {
            assert_eq!(
                web_base(remote).as_deref(),
                Some("https://github.com/owner/repo"),
                "{remote}"
            );
        }
        assert_eq!(web_base("/srv/git/repo.git"), None);
    }
}
