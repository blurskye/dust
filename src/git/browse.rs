//! The forge web URL for a line of a file, for `<leader>gB` (open) and `<leader>gY` (copy).

use std::path::Path;

/// The web URL of `file` at `line` (1-based) on the forge behind the `origin` remote (or the
/// only remote). The URL names the current branch, or the commit on a detached HEAD.
pub fn git_browse_url(file: &Path, line: usize) -> Result<String, String> {
    let repo = git2::Repository::discover(file).map_err(|_| "not inside a Git repository")?;
    let workdir = repo.workdir().ok_or("the repository has no working tree")?;
    let canonical = crate::files::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let workdir = crate::files::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
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
    let reference = encode_component(reference.as_bytes());
    let path = relative
        .components()
        .map(|part| encode_component(part.as_os_str().as_encoded_bytes()))
        .collect::<Vec<_>>()
        .join("/");
    let host = base.host_str().unwrap_or_default();
    Ok(if host.contains("gitlab") {
        format!("{base}/-/blob/{reference}/{path}#L{line}")
    } else if host.contains("github") {
        format!("{base}/blob/{reference}/{path}#L{line}")
    } else {
        // Gitea, Forgejo and Codeberg distinguish branch and commit routes.
        let kind = if head.is_branch() { "branch" } else { "commit" };
        format!("{base}/src/{kind}/{reference}/{path}#L{line}")
    })
}

fn encode_component(value: &[u8]) -> String {
    let mut encoded = String::new();
    for &byte in value {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Convert Git transports to HTTPS; keep HTTP(S) schemes and web ports intact.
fn web_base(remote: &str) -> Option<reqwest::Url> {
    let remote = remote.trim().trim_end_matches('/');
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    let (scheme, authority, path) = if let Some((scheme, rest)) = remote.split_once("://") {
        if !matches!(scheme, "http" | "https" | "ssh" | "git") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        (scheme, authority, path)
    } else {
        let (authority, path) = remote.split_once(':')?;
        ("ssh", authority, path)
    };
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() || path.is_empty() {
        return None;
    }
    let is_web = matches!(scheme, "http" | "https");
    let web_scheme = if is_web { scheme } else { "https" };
    let mut base = reqwest::Url::parse(&format!("{web_scheme}://{authority}/{path}")).ok()?;
    if !is_web {
        // A Git transport port is not the web port.
        base.set_port(None).ok()?;
    }
    Some(base)
}

#[cfg(test)]
mod tests {
    use crate::git::browse::web_base;

    fn repository_with_file(name: &std::path::Path) -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join(name), "text").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(name).unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("Test", "test@example.invalid").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "test", &tree, &[])
                .unwrap();
        }
        (dir, repo)
    }

    #[test]
    fn browse_urls_escape_file_and_branch_components() {
        let name = "notes#draft %.rs";
        let (dir, repo) = repository_with_file(std::path::Path::new(name));
        let file = dir.path().join(name);
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature/a#b", &commit, false).unwrap();
        repo.set_head("refs/heads/feature/a#b").unwrap();
        repo.remote("origin", "git@github.com:owner/repo.git")
            .unwrap();
        assert_eq!(
            crate::git::browse::git_browse_url(&file, 3).unwrap(),
            "https://github.com/owner/repo/blob/feature%2Fa%23b/notes%23draft%20%25.rs#L3"
        );
    }

    #[cfg(unix)]
    #[test]
    fn browse_urls_preserve_non_utf8_filenames() {
        use std::os::unix::ffi::OsStrExt;

        // Exercise arbitrary Unix filename bytes without requiring filesystem support.
        let name = std::ffi::OsStr::from_bytes(b"notes\xFF.rs");
        assert_eq!(
            crate::git::browse::encode_component(name.as_encoded_bytes()),
            "notes%FF.rs"
        );
    }

    #[test]
    fn browse_urls_detect_forges_from_the_host_only() {
        let (dir, repo) = repository_with_file(std::path::Path::new("file.rs"));
        let oid = repo.head().unwrap().target().unwrap();
        repo.set_head_detached(oid).unwrap();
        for (remote, route) in [
            ("https://github.com/owner/gitlab-tools.git", "blob"),
            ("https://gitlab.com/github/repo.git", "-/blob"),
            ("https://codeberg.org/gitlab/github-tools.git", "src/commit"),
        ] {
            repo.remote("origin", remote).unwrap();
            let base = remote.strip_suffix(".git").unwrap();
            assert_eq!(
                crate::git::browse::git_browse_url(&dir.path().join("file.rs"), 2).unwrap(),
                format!("{base}/{route}/{oid}/file.rs#L2")
            );
            repo.remote_delete("origin").unwrap();
        }
    }

    #[test]
    fn browse_urls_distinguish_branches_from_detached_commits() {
        let (dir, repo) = repository_with_file(std::path::Path::new("file.rs"));
        let oid = repo.head().unwrap().target().unwrap();
        repo.branch("feature", &repo.find_commit(oid).unwrap(), false)
            .unwrap();
        for remote in [
            "https://codeberg.org/owner/repo.git",
            "http://gitea.example:3000/owner/repo.git",
            "https://forgejo.example:8443/owner/repo.git",
        ] {
            repo.remote("origin", remote).unwrap();
            let base = remote.strip_suffix(".git").unwrap();
            repo.set_head("refs/heads/feature").unwrap();
            assert_eq!(
                crate::git::browse::git_browse_url(&dir.path().join("file.rs"), 4).unwrap(),
                format!("{base}/src/branch/feature/file.rs#L4")
            );
            repo.set_head_detached(oid).unwrap();
            assert_eq!(
                crate::git::browse::git_browse_url(&dir.path().join("file.rs"), 4).unwrap(),
                format!("{base}/src/commit/{oid}/file.rs#L4")
            );
            repo.remote_delete("origin").unwrap();
        }
    }

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
                web_base(remote).as_ref().map(reqwest::Url::as_str),
                Some("https://github.com/owner/repo"),
                "{remote}"
            );
        }
        for (remote, expected) in [
            (
                "http://forge.example:3000/owner/repo.git",
                "http://forge.example:3000/owner/repo",
            ),
            (
                "https://forge.example:8443/owner/repo.git",
                "https://forge.example:8443/owner/repo",
            ),
            (
                "http://user:password@forge.example:3000/owner/repo.git",
                "http://forge.example:3000/owner/repo",
            ),
            (
                "ssh://git@forge.example:2222/owner/repo.git",
                "https://forge.example/owner/repo",
            ),
            (
                "git://forge.example:9418/owner/repo.git",
                "https://forge.example/owner/repo",
            ),
            (
                "http://[::1]:3000/owner/repo.git",
                "http://[::1]:3000/owner/repo",
            ),
            (
                "ssh://git@[::1]:2222/owner/repo.git",
                "https://[::1]/owner/repo",
            ),
        ] {
            assert_eq!(web_base(remote).unwrap().as_str(), expected, "{remote}");
        }
        assert_eq!(web_base("/srv/git/repo.git"), None);
    }
}
