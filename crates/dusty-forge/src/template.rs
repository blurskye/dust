//! Pull and merge request templates and the remembered base branch.
//!
//! Templates are read from the **base ref locally** (`git show <base>:<path>`), never from the
//! API, so they cost no request and work offline. The base branch a head branch was opened or
//! checked out against is kept where `gh` keeps it, `branch.<head>.gh-merge-base`.

use crate::provider::ProviderKind;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

/// Single-file locations, in the order each forge looks them up. Directories of several
/// templates come after (`.github/PULL_REQUEST_TEMPLATE/*.md`, GitLab's
/// `.gitlab/merge_request_templates/*.md` whose default is `Default.md`).
fn files(kind: ProviderKind) -> &'static [&'static str] {
    match kind {
        ProviderKind::GitHub => &[
            ".github/pull_request_template.md",
            ".github/PULL_REQUEST_TEMPLATE.md",
            "pull_request_template.md",
            "PULL_REQUEST_TEMPLATE.md",
            "docs/pull_request_template.md",
            "docs/PULL_REQUEST_TEMPLATE.md",
        ],
        ProviderKind::GitLab => &[],
        ProviderKind::Gitea | ProviderKind::Forgejo => &[
            ".forgejo/pull_request_template.md",
            ".forgejo/PULL_REQUEST_TEMPLATE.md",
            ".gitea/pull_request_template.md",
            ".gitea/PULL_REQUEST_TEMPLATE.md",
            ".github/pull_request_template.md",
            ".github/PULL_REQUEST_TEMPLATE.md",
            "pull_request_template.md",
            "PULL_REQUEST_TEMPLATE.md",
            "docs/pull_request_template.md",
            "docs/PULL_REQUEST_TEMPLATE.md",
        ],
    }
}

fn directories(kind: ProviderKind) -> &'static [&'static str] {
    match kind {
        ProviderKind::GitHub => &[
            ".github/PULL_REQUEST_TEMPLATE",
            "PULL_REQUEST_TEMPLATE",
            "docs/PULL_REQUEST_TEMPLATE",
        ],
        ProviderKind::GitLab => &[".gitlab/merge_request_templates"],
        ProviderKind::Gitea | ProviderKind::Forgejo => &[
            ".forgejo/PULL_REQUEST_TEMPLATE",
            ".gitea/PULL_REQUEST_TEMPLATE",
            ".github/PULL_REQUEST_TEMPLATE",
        ],
    }
}

fn git(root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn text(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = git(root, arguments)?;
    let text = String::from_utf8(output).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `branch.<head>.gh-merge-base`, as `gh pr create` and `gh pr checkout` leave it.
pub fn merge_base(root: &Path, branch: &str) -> Option<String> {
    text(
        root,
        &["config", "--get", &format!("branch.{branch}.gh-merge-base")],
    )
}

/// Remember `base` for `branch`; only for a branch that exists here.
pub fn set_merge_base(root: &Path, branch: &str, base: &str) -> bool {
    if branch.is_empty() || base.is_empty() {
        return false;
    }
    let reference = format!("refs/heads/{branch}");
    if git(root, &["rev-parse", "--verify", "--quiet", &reference]).is_none() {
        return false;
    }
    git(
        root,
        &["config", &format!("branch.{branch}.gh-merge-base"), base],
    )
    .is_some()
}

/// The ref a new pull request of the current branch targets: the remembered merge base on
/// `remote`, else the remote's default branch, else the local default, else `HEAD`.
pub fn base_ref(root: &Path, remote: Option<&str>) -> String {
    let exists = |reference: &str| {
        git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{reference}^{{commit}}"),
            ],
        )
        .is_some()
    };
    let current = text(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let remembered = current
        .as_deref()
        .and_then(|branch| merge_base(root, branch));
    let mut candidates = Vec::new();
    if let Some(base) = &remembered {
        if let Some(remote) = remote {
            candidates.push(format!("refs/remotes/{remote}/{base}"));
        }
        candidates.push(format!("refs/heads/{base}"));
    }
    if let Some(remote) = remote
        && let Some(default) = text(
            root,
            &[
                "symbolic-ref",
                "--quiet",
                &format!("refs/remotes/{remote}/HEAD"),
            ],
        )
    {
        candidates.push(default);
    }
    for name in ["main", "master"] {
        if let Some(remote) = remote {
            candidates.push(format!("refs/remotes/{remote}/{name}"));
        }
        candidates.push(format!("refs/heads/{name}"));
    }
    candidates
        .into_iter()
        .find(|reference| exists(reference))
        .unwrap_or_else(|| "HEAD".into())
}

/// Files below `directory` at `base`, Markdown only, sorted.
fn listed(root: &Path, base: &str, directory: &str) -> Vec<String> {
    let Some(output) = git(
        root,
        &["ls-tree", "--name-only", &format!("{base}:{directory}")],
    ) else {
        return Vec::new();
    };
    let mut names = String::from_utf8_lossy(&output)
        .lines()
        .filter(|name| name.to_ascii_lowercase().ends_with(".md"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names
}

/// The template paths at `base`, in the order the forge would offer them: the single-file
/// locations, then each directory's entries (GitLab's `Default.md` first).
pub fn candidates(root: &Path, kind: ProviderKind, base: &str) -> Vec<String> {
    let mut found = Vec::new();
    for path in files(kind) {
        if git(root, &["cat-file", "-e", &format!("{base}:{path}")]).is_some() {
            found.push((*path).to_owned());
        }
    }
    for directory in directories(kind) {
        let mut names = listed(root, base, directory);
        if let Some(at) = names
            .iter()
            .position(|name| name.eq_ignore_ascii_case("default.md"))
        {
            let default = names.remove(at);
            names.insert(0, default);
        }
        found.extend(names.into_iter().map(|name| format!("{directory}/{name}")));
    }
    found
}

/// The first template at `base` (when several exist the first is the default; the others are
/// listed by `candidates`).
pub fn read(root: &Path, kind: ProviderKind, base: &str) -> Option<String> {
    let path = candidates(root, kind, base).into_iter().next()?;
    let bytes = git(root, &["show", &format!("{base}:{path}")])?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// `Read::PullRequestTemplate` for every provider: the remote the forge was chosen from, its
/// base ref, then the first template there.
pub fn for_context(root: &Path, kind: ProviderKind) -> Option<String> {
    let remote = crate::detect::choose_remote(root)
        .ok()
        .map(|remote| remote.remote);
    let base = base_ref(root, remote.as_deref());
    read(root, kind, &base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository(files: &[(&str, &str)]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        run(root, &["init", "-q", "-b", "main"]);
        run(root, &["config", "user.name", "Test"]);
        run(root, &["config", "user.email", "test@example.com"]);
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        directory
    }

    #[test]
    fn templates_come_from_the_base_ref_not_the_worktree() {
        let directory = repository(&[(".github/pull_request_template.md", "## Summary\n")]);
        let root = directory.path();
        run(root, &["checkout", "-q", "-b", "topic"]);
        std::fs::write(root.join(".github/pull_request_template.md"), "changed\n").unwrap();
        run(root, &["commit", "-q", "-am", "topic"]);
        assert_eq!(
            read(root, ProviderKind::GitHub, "main").as_deref(),
            Some("## Summary\n")
        );
        assert_eq!(base_ref(root, None), "refs/heads/main");
        assert_eq!(
            for_context(root, ProviderKind::GitHub).as_deref(),
            Some("## Summary\n")
        );
    }

    #[test]
    fn each_forge_finds_its_own_locations() {
        let directory = repository(&[
            (".github/PULL_REQUEST_TEMPLATE/bug.md", "bug\n"),
            (".github/PULL_REQUEST_TEMPLATE/feature.md", "feature\n"),
            (".gitlab/merge_request_templates/Bug.md", "gl bug\n"),
            (".gitlab/merge_request_templates/Default.md", "gl default\n"),
            (".forgejo/pull_request_template.md", "forgejo\n"),
            ("docs/notes.txt", "not a template\n"),
        ]);
        let root = directory.path();
        assert_eq!(
            candidates(root, ProviderKind::GitHub, "main"),
            [
                ".github/PULL_REQUEST_TEMPLATE/bug.md",
                ".github/PULL_REQUEST_TEMPLATE/feature.md"
            ]
        );
        assert_eq!(
            read(root, ProviderKind::GitLab, "main").as_deref(),
            Some("gl default\n")
        );
        assert_eq!(
            read(root, ProviderKind::Forgejo, "main").as_deref(),
            Some("forgejo\n")
        );
        assert_eq!(
            read(root, ProviderKind::Gitea, "main").as_deref(),
            Some("forgejo\n")
        );
        let plain = repository(&[("PULL_REQUEST_TEMPLATE.md", "root\n")]);
        for kind in [ProviderKind::GitHub, ProviderKind::Gitea] {
            assert_eq!(read(plain.path(), kind, "main").as_deref(), Some("root\n"));
        }
        assert_eq!(read(plain.path(), ProviderKind::GitLab, "main"), None);
        let none = repository(&[]);
        assert_eq!(read(none.path(), ProviderKind::GitHub, "main"), None);
    }

    #[test]
    fn merge_base_is_remembered_per_branch_like_gh() {
        let directory = repository(&[(".github/pull_request_template.md", "main\n")]);
        let root = directory.path();
        run(root, &["branch", "release"]);
        run(root, &["checkout", "-q", "release"]);
        std::fs::write(root.join(".github/pull_request_template.md"), "release\n").unwrap();
        run(root, &["commit", "-q", "-am", "release"]);
        run(root, &["checkout", "-q", "-b", "fix"]);
        assert!(!set_merge_base(root, "missing", "release"));
        assert!(set_merge_base(root, "fix", "release"));
        assert_eq!(merge_base(root, "fix").as_deref(), Some("release"));
        assert_eq!(base_ref(root, None), "refs/heads/release");
        assert_eq!(
            read(root, ProviderKind::GitHub, &base_ref(root, None)).as_deref(),
            Some("release\n")
        );
    }
}
