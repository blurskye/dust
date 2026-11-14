//! Porcelain per provider (FORGE.md §1.3, F7): checkout, a sibling worktree, linked branches,
//! Projects and open-in-browser.
//!
//! Checkout uses the forge's CLI where it owns the job (`gh pr checkout`, `glab mr checkout`)
//! and otherwise fetches the head ref every forge publishes (`refs/pull/N/head`,
//! `refs/merge-requests/N/head`) the t3code way: a fork's branch that collides with a local
//! name becomes `owner/branch`, an existing branch only moves with `--ff-only`, and the
//! upstream and `gh-merge-base` are written only after the switch succeeded.

use super::ForgeCommands;
use super::worker::ForgeTarget;
use crate::forge::cli::redact;
use crate::forge::detect;
use crate::forge::model::ItemKind;
use crate::forge::provider::Conditional;
use crate::forge::provider::Fetched;
use crate::forge::provider::ProviderKind;
use crate::forge::provider::RepoRef;
use crate::forge::request::ForgeError;
use crate::forge::request::Payload;
use crate::forge::request::Read;
use crate::forge::request::Receipt;
use crate::forge::request::Write;
use crate::forge::transport::Cli;
use crate::forge::transport::ConnectOptions;
use crate::forge::transport::Endpoints;
use crate::forge::transport::HostKey;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

/// What a checkout needs to know about a pull request; everything but the number is optional
/// so a forge that cannot be read still checks out as `pr-N`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PullHead {
    pub number: u64,
    /// The source branch on the head repository; empty when unknown.
    pub head_branch: String,
    /// `owner/name` of the head repository; `None` when it is the base repository or unknown.
    pub head_repo: Option<String>,
    pub base_repo: String,
    pub base_branch: String,
}

impl PullHead {
    fn cross_repository(&self) -> bool {
        self.head_repo
            .as_deref()
            .is_some_and(|head| !head.eq_ignore_ascii_case(&self.base_repo))
    }

    fn head_owner(&self) -> Option<&str> {
        let head = self.head_repo.as_deref()?;
        head.rsplit_once('/')
            .map(|(owner, _)| owner.rsplit('/').next().unwrap_or(owner))
    }
}

/// The ref each forge publishes for a pull request's head.
pub fn head_ref(kind: ProviderKind, number: u64) -> String {
    match kind {
        ProviderKind::GitLab => format!("refs/merge-requests/{number}/head"),
        _ => format!("refs/pull/{number}/head"),
    }
}

/// The pull request a `branch.<b>.merge` ref names (`refs/pull/N/head`,
/// `refs/merge-requests/N/head`).
pub fn number_of_ref(reference: &str) -> Option<u64> {
    let rest = reference
        .strip_prefix("refs/pull/")
        .or_else(|| reference.strip_prefix("refs/merge-requests/"))?;
    rest.strip_suffix("/head")?.parse().ok()
}

fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with("refs/")
        && !name.starts_with('-')
        && git2::Branch::name_is_valid(name).unwrap_or(false)
}

/// The local branch a pull request checks out to (t3code): its head branch, or `pr-N` when
/// that is unknown; a fork's branch becomes `owner/branch` when the plain name is the base's
/// default branch or a local branch that tracks something else.
pub fn local_branch(
    head: &PullHead,
    default_branch: Option<&str>,
    tracking: impl Fn(&str) -> Option<Option<String>>,
    expected_merge: &str,
) -> String {
    let name = if valid_branch(&head.head_branch) {
        head.head_branch.clone()
    } else {
        return format!("pr-{}", head.number);
    };
    if !head.cross_repository() {
        return name;
    }
    let collides = default_branch == Some(name.as_str())
        || name == head.base_branch
        || tracking(&name).is_some_and(|merge| merge.as_deref() != Some(expected_merge));
    match head.head_owner() {
        Some(owner) if collides => {
            let qualified = format!("{owner}/{name}");
            if valid_branch(&qualified) {
                qualified
            } else {
                format!("pr-{}", head.number)
            }
        }
        _ => name,
    }
}

/// The page of an item on the forge's website.
pub fn browser_url(endpoints: &Endpoints, repo: &RepoRef, kind: ItemKind, number: u64) -> String {
    let base = format!(
        "{}/{}",
        endpoints.web_base.trim_end_matches('/'),
        repo.full()
    );
    match (endpoints.kind, kind) {
        (ProviderKind::GitHub, ItemKind::PullRequest) => format!("{base}/pull/{number}"),
        (ProviderKind::GitLab, ItemKind::PullRequest) => {
            format!("{base}/-/merge_requests/{number}")
        }
        (ProviderKind::GitLab, ItemKind::Issue) => format!("{base}/-/issues/{number}"),
        (ProviderKind::Gitea | ProviderKind::Forgejo, ItemKind::PullRequest) => {
            format!("{base}/pulls/{number}")
        }
        (_, ItemKind::Issue) => format!("{base}/issues/{number}"),
    }
}

/// Hand `url` to `$BROWSER` (its first `:`-separated entry; `%s` is replaced, else the URL is
/// appended) or the OS opener. A launcher that exits at once must succeed; one still running
/// after a moment is left to run and reaped in the background.
pub fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("not a web address: {url}"));
    }
    let browser = std::env::var("BROWSER")
        .ok()
        .and_then(|value| value.split(':').next().map(str::trim).map(str::to_owned))
        .filter(|value| !value.is_empty());
    let (program, arguments) = match browser {
        Some(browser) => {
            let mut words = browser
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let program = words.remove(0);
            if words.iter().any(|word| word.contains("%s")) {
                for word in &mut words {
                    *word = word.replace("%s", url);
                }
            } else {
                words.push(url.to_owned());
            }
            (program, words)
        }
        None if cfg!(target_os = "macos") => ("open".to_owned(), vec![url.to_owned()]),
        None if cfg!(windows) => (
            "cmd".to_owned(),
            vec!["/C".into(), "start".into(), String::new(), url.to_owned()],
        ),
        None => ("xdg-open".to_owned(), vec![url.to_owned()]),
    };
    let mut child = std::process::Command::new(&program)
        .args(&arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start {program}: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{program} failed ({status})")),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Where a porcelain action goes: the forge kind (from detection, which needs no request on a
/// known or cached host), the repository and the local remote that points at it.
pub(super) struct Site {
    pub endpoints: Endpoints,
    pub host: HostKey,
    pub repo: RepoRef,
    pub remote: Option<String>,
    pub options: ConnectOptions,
}

impl Site {
    pub fn resolve(
        root: &Path,
        target: &ForgeTarget,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, ForgeError> {
        let (host, repo) = target.resolve(root)?;
        let repo =
            repo.ok_or_else(|| ForgeError::Failed("this action needs a repository".into()))?;
        let options = ConnectOptions::from_env();
        let detected = detect::detect(&host, &options, cancel)?;
        let remote = matching_remote(root, &host, &repo);
        Ok(Self {
            endpoints: detected.endpoints,
            host,
            repo,
            remote,
            options,
        })
    }

    fn kind(&self) -> ProviderKind {
        self.endpoints.kind
    }

    /// `host/owner/name`, the `--repo` form gh accepts.
    fn scope(&self) -> String {
        format!("{}/{}", self.host, self.repo.full())
    }

    /// The remote to fetch from: the local one pointing at the repository, else its URL.
    fn fetch_source(&self) -> String {
        self.remote.clone().unwrap_or_else(|| {
            format!(
                "{}/{}.git",
                self.endpoints.web_base.trim_end_matches('/'),
                self.repo.full()
            )
        })
    }

    fn cli(&self, cli: Cli) -> Option<PathBuf> {
        let path = match cli {
            Cli::Gh => self.options.gh.clone(),
            Cli::Glab => self.options.glab.clone(),
            Cli::Tea => self.options.tea.clone(),
        }?;
        path.is_file().then_some(path)
    }
}

/// The remote whose URL names `host`/`repo` (case-insensitive), preferring the forge's choice.
fn matching_remote(root: &Path, host: &HostKey, repo: &RepoRef) -> Option<String> {
    if let Ok(chosen) = detect::choose_remote(root)
        && &chosen.host == host
        && chosen.repo.full().eq_ignore_ascii_case(&repo.full())
    {
        return Some(chosen.remote);
    }
    let repository = git2::Repository::discover(root).ok()?;
    let names = repository.remotes().ok()?;
    names.iter().flatten().find_map(|name| {
        let remote = repository.find_remote(name).ok()?;
        let (found_host, found_repo) = remote.url().and_then(detect::parse_remote)?;
        (&found_host == host && found_repo.full().eq_ignore_ascii_case(&repo.full()))
            .then(|| name.to_owned())
    })
}

fn failed(error: String) -> ForgeError {
    ForgeError::Failed(redact(&error))
}

fn kind_word(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Issue => "issue",
        ItemKind::PullRequest => "pr",
    }
}

impl ForgeCommands<'_> {
    /// §1.3 porcelain for every provider.
    pub(super) fn porcelain(
        &self,
        root: &Path,
        target: &ForgeTarget,
        write: &Write,
    ) -> Result<Receipt, ForgeError> {
        let site = Site::resolve(root, target, self.cancelled)?;
        let receipt = |summary: String| Receipt {
            summary,
            invalidates: write.invalidates(),
            ..Receipt::default()
        };
        match write {
            Write::OpenInBrowser { kind, number, url } => {
                match (url, site.kind(), site.cli(Cli::Gh)) {
                    (Some(url), ..) => open_url(url).map_err(failed)?,
                    // gh knows the user's browser setup best (and tests observe it).
                    (None, ProviderKind::GitHub, Some(gh)) => {
                        let number = number.to_string();
                        self.run(
                            root,
                            &gh.to_string_lossy(),
                            &[
                                kind_word(*kind),
                                "view",
                                &number,
                                "--web",
                                "--repo",
                                &site.scope(),
                            ],
                        )
                        .map_err(failed)?;
                    }
                    (None, ..) => {
                        open_url(&browser_url(&site.endpoints, &site.repo, *kind, *number))
                            .map_err(failed)?
                    }
                }
                Ok(receipt("Opened in browser".into()))
            }
            Write::Checkout { number } => {
                let branch = self.checkout_into(root, &site, *number, None)?;
                Ok(receipt(format!("Checked out {branch}")))
            }
            Write::Worktree { number } => {
                let target = self.worktree_checkout(root, &site, *number)?;
                Ok(receipt(format!(
                    "Pull request worktree: {}",
                    target.display()
                )))
            }
            Write::Develop {
                number,
                branch,
                base,
            } => {
                let gh = self.github_cli(&site, "linked branches")?;
                let number = number.to_string();
                self.run(
                    root,
                    &gh.to_string_lossy(),
                    &[
                        "issue",
                        "develop",
                        &number,
                        "--name",
                        branch,
                        "--base",
                        base,
                        "--repo",
                        &site.scope(),
                    ],
                )
                .map_err(failed)?;
                Ok(receipt("Saved".into()))
            }
            Write::SetProjects {
                kind,
                number,
                add,
                remove,
            } => {
                let gh = self.github_cli(&site, "projects")?;
                let number = number.to_string();
                let scope = site.scope();
                let mut args = vec![kind_word(*kind), "edit", number.as_str()];
                for project in remove {
                    args.extend(["--remove-project", project.as_str()]);
                }
                for project in add {
                    args.extend(["--add-project", project.as_str()]);
                }
                args.extend(["--repo", scope.as_str()]);
                self.run(root, &gh.to_string_lossy(), &args)
                    .map_err(failed)?;
                Ok(receipt("Saved".into()))
            }
            _ => Err(ForgeError::Unsupported {
                capability: "porcelain",
                reason: "not a porcelain action".into(),
            }),
        }
    }

    fn github_cli(&self, site: &Site, capability: &'static str) -> Result<PathBuf, ForgeError> {
        if site.kind() != ProviderKind::GitHub {
            return Err(ForgeError::Unsupported {
                capability,
                reason: format!("{} has no {capability} here", site.kind().name()),
            });
        }
        site.cli(Cli::Gh).ok_or(ForgeError::CliMissing(Cli::Gh))
    }

    /// Check the pull request out into `<parent>/<repo>-pr-<number>` on branch `pr-<number>`
    /// (the IDE finds it again by that name); an existing worktree is updated.
    pub(super) fn worktree_checkout(
        &self,
        root: &Path,
        site: &Site,
        number: u64,
    ) -> Result<PathBuf, ForgeError> {
        let target = self.pr_worktree(root, number).map_err(failed)?;
        self.prepare_worktree(root, &target).map_err(failed)?;
        self.checkout_into(&target, site, number, Some(format!("pr-{number}")))?;
        Ok(target)
    }

    /// Check out pull request `number` in `root` and return the local branch. `forced` names
    /// the branch (worktrees); otherwise the t3code naming rule picks it.
    pub(super) fn checkout_into(
        &self,
        root: &Path,
        site: &Site,
        number: u64,
        forced: Option<String>,
    ) -> Result<String, ForgeError> {
        let head = self.pull_head(root, site, number);
        self.checkout_head(root, site, &head, forced)
    }

    fn checkout_head(
        &self,
        root: &Path,
        site: &Site,
        head: &PullHead,
        forced: Option<String>,
    ) -> Result<String, ForgeError> {
        let number = head.number;
        let cli = match site.kind() {
            ProviderKind::GitHub => site.cli(Cli::Gh).map(|gh| (gh, "pr")),
            ProviderKind::GitLab => site.cli(Cli::Glab).map(|glab| (glab, "mr")),
            ProviderKind::Gitea | ProviderKind::Forgejo => None,
        };
        let branch = match cli {
            Some((program, noun)) => {
                let number = number.to_string();
                let repo = match site.kind() {
                    ProviderKind::GitLab => format!(
                        "{}/{}",
                        site.endpoints.web_base.trim_end_matches('/'),
                        site.repo.full()
                    ),
                    _ => site.scope(),
                };
                let mut args = vec![noun, "checkout", number.as_str()];
                let flag = if noun == "mr" { "-b" } else { "--branch" };
                if let Some(branch) = &forced {
                    args.extend([flag, branch.as_str()]);
                }
                let repo_flag = if noun == "mr" { "-R" } else { "--repo" };
                args.extend([repo_flag, repo.as_str()]);
                self.run(root, &program.to_string_lossy(), &args)
                    .map_err(failed)?;
                self.run(root, "git", &["branch", "--show-current"])
                    .map_err(failed)?
                    .trim()
                    .to_owned()
            }
            None => self.fetch_checkout(root, site, head, forced)?,
        };
        // §3.7: gh-compatible base memory, after the checkout succeeded.
        if !head.base_branch.is_empty() {
            crate::forge::template::set_merge_base(root, &branch, &head.base_branch);
        }
        Ok(branch)
    }

    /// What the forge says about the pull request; a forge that cannot be read leaves only
    /// the number (the fetch still works for a public repository).
    fn pull_head(&self, root: &Path, site: &Site, number: u64) -> PullHead {
        let fallback = PullHead {
            number,
            base_repo: site.repo.full(),
            ..PullHead::default()
        };
        let Ok((provider, ctx)) =
            super::worker::connect_to(root, &site.host, Some(site.repo.clone()), self.cancelled)
        else {
            return fallback;
        };
        let read = Read::PullRequest(number);
        match crate::forge::inflight::read(
            provider.as_ref(),
            &ctx,
            &read,
            &Conditional::default(),
            self.cancelled,
        ) {
            Ok(Fetched::Payload {
                value: Payload::PullRequest(pull),
                ..
            }) => PullHead {
                number,
                head_branch: pull
                    .row
                    .head_branch
                    .clone()
                    .unwrap_or_else(|| pull.head.branch.clone()),
                head_repo: pull.head.repo.clone(),
                base_repo: site.repo.full(),
                base_branch: pull.base.branch.clone(),
            },
            _ => fallback,
        }
    }

    /// The fetch path: `git fetch <remote> <head ref>`, then create or fast-forward the local
    /// branch, then (only then) its upstream.
    fn fetch_checkout(
        &self,
        root: &Path,
        site: &Site,
        head: &PullHead,
        forced: Option<String>,
    ) -> Result<String, ForgeError> {
        let reference = head_ref(site.kind(), head.number);
        let source = site.fetch_source();
        self.run(
            root,
            "git",
            &["fetch", "--no-tags", "--", &source, &reference],
        )
        .map_err(failed)?;
        let commit = self
            .run(
                root,
                "git",
                &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
            )
            .map_err(failed)?
            .trim()
            .to_owned();
        let same_repo = !head.cross_repository() && valid_branch(&head.head_branch);
        let expected_merge = if same_repo {
            format!("refs/heads/{}", head.head_branch)
        } else {
            reference.clone()
        };
        let branch = match forced {
            Some(branch) => branch,
            None => {
                let default = site.remote.as_deref().and_then(|remote| {
                    self.run(
                        root,
                        "git",
                        &[
                            "symbolic-ref",
                            "--quiet",
                            "--short",
                            &format!("refs/remotes/{remote}/HEAD"),
                        ],
                    )
                    .ok()
                    .and_then(|name| {
                        name.trim()
                            .strip_prefix(&format!("{remote}/"))
                            .map(str::to_owned)
                    })
                });
                local_branch(
                    head,
                    default.as_deref(),
                    |name| self.tracking(root, name),
                    &expected_merge,
                )
            }
        };
        self.update_pr_branch(root, &branch, &commit)
            .map_err(failed)?;
        // The upstream only after the branch is where the pull request is (t3code).
        let remote = site.remote.clone().unwrap_or(source);
        self.run(
            root,
            "git",
            &["config", &format!("branch.{branch}.remote"), &remote],
        )
        .map_err(failed)?;
        self.run(
            root,
            "git",
            &["config", &format!("branch.{branch}.merge"), &expected_merge],
        )
        .map_err(failed)?;
        Ok(branch)
    }

    /// `Some(merge)` when local branch `name` exists (`merge` = its `branch.<name>.merge`).
    fn tracking(&self, root: &Path, name: &str) -> Option<Option<String>> {
        self.run(
            root,
            "git",
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ],
        )
        .ok()?;
        Some(
            self.run(
                root,
                "git",
                &["config", "--get", &format!("branch.{name}.merge")],
            )
            .ok()
            .map(|merge| merge.trim().to_owned()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(branch: &str, head_repo: Option<&str>) -> PullHead {
        PullHead {
            number: 12,
            head_branch: branch.into(),
            head_repo: head_repo.map(str::to_owned),
            base_repo: "orbit/orbit".into(),
            base_branch: "main".into(),
        }
    }

    #[test]
    fn fork_branches_that_collide_take_the_owner_prefix() {
        let none = |_: &str| None;
        let expected = "refs/pull/12/head";
        assert_eq!(
            local_branch(&head("fix", None), Some("main"), none, expected),
            "fix"
        );
        assert_eq!(
            local_branch(
                &head("fix", Some("orbit/orbit")),
                Some("main"),
                none,
                expected
            ),
            "fix"
        );
        assert_eq!(
            local_branch(
                &head("main", Some("maya/orbit")),
                Some("main"),
                none,
                expected
            ),
            "maya/main"
        );
        assert_eq!(
            local_branch(
                &head("trunk", Some("maya/orbit")),
                Some("trunk"),
                none,
                expected
            ),
            "maya/trunk"
        );
        let tracks_other = |_: &str| Some(Some("refs/heads/fix".to_owned()));
        assert_eq!(
            local_branch(
                &head("fix", Some("maya/orbit")),
                Some("main"),
                tracks_other,
                expected
            ),
            "maya/fix"
        );
        let tracks_this = |_: &str| Some(Some(expected.to_owned()));
        assert_eq!(
            local_branch(
                &head("fix", Some("maya/orbit")),
                Some("main"),
                tracks_this,
                expected
            ),
            "fix"
        );
        assert_eq!(
            local_branch(&head("main", Some("group/sub/orbit")), None, none, expected),
            "sub/main"
        );
        assert_eq!(local_branch(&head("", None), None, none, expected), "pr-12");
        assert_eq!(
            local_branch(&head("refs/pull/12/head", None), None, none, expected),
            "pr-12"
        );
        assert_eq!(
            local_branch(&head("bad..name", None), None, none, expected),
            "pr-12"
        );
    }

    fn git(root: &Path, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A commit on top of `parent` in `work`, published to the bare repository as `reference`.
    fn publish(work: &Path, parent: &str, message: &str, reference: &str) -> String {
        git(work, &["checkout", "-q", "--detach", parent]);
        git(work, &["commit", "-q", "--allow-empty", "-m", message]);
        let commit = git(work, &["rev-parse", "HEAD"]);
        git(
            work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{commit}:{reference}"),
            ],
        );
        commit
    }

    struct World {
        _directory: tempfile::TempDir,
        work: PathBuf,
        clone: PathBuf,
        base: String,
    }

    fn world() -> World {
        let directory = tempfile::tempdir().unwrap();
        let bare = directory.path().join("orbit.git");
        let work = directory.path().join("work");
        let clone = directory.path().join("clone");
        git(
            directory.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        git(
            directory.path(),
            &["init", "-q", "-b", "main", work.to_str().unwrap()],
        );
        git(&work, &["config", "user.name", "Test"]);
        git(&work, &["config", "user.email", "test@example.com"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&work, &["push", "-q", "origin", "main"]);
        let base = git(&work, &["rev-parse", "HEAD"]);
        git(
            directory.path(),
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        git(&clone, &["config", "user.name", "Test"]);
        git(&clone, &["config", "user.email", "test@example.com"]);
        World {
            _directory: directory,
            work,
            clone,
            base,
        }
    }

    fn site(kind: ProviderKind) -> Site {
        let host = HostKey::new("forge.example.test");
        Site {
            endpoints: Endpoints {
                kind,
                host: host.clone(),
                api_base: String::new(),
                graphql: None,
                web_base: "https://forge.example.test".into(),
            },
            host,
            repo: RepoRef::parse("orbit/orbit").unwrap(),
            remote: Some("origin".into()),
            options: ConnectOptions::default(),
        }
    }

    static CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static COMMANDS: ForgeCommands<'static> = ForgeCommands {
        cancelled: &CANCELLED,
    };

    fn config(root: &Path, key: &str) -> Option<String> {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(["config", "--get", key])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    #[test]
    fn same_repository_pull_requests_check_out_on_their_branch() {
        let world = world();
        let commit = publish(&world.work, &world.base, "feature", "refs/pull/9/head");
        let head = PullHead {
            number: 9,
            head_branch: "feature".into(),
            head_repo: Some("orbit/orbit".into()),
            base_repo: "orbit/orbit".into(),
            base_branch: "main".into(),
        };
        let branch = COMMANDS
            .checkout_head(&world.clone, &site(ProviderKind::Gitea), &head, None)
            .unwrap();
        assert_eq!(branch, "feature");
        assert_eq!(git(&world.clone, &["branch", "--show-current"]), "feature");
        assert_eq!(git(&world.clone, &["rev-parse", "HEAD"]), commit);
        assert_eq!(
            config(&world.clone, "branch.feature.remote").as_deref(),
            Some("origin")
        );
        assert_eq!(
            config(&world.clone, "branch.feature.merge").as_deref(),
            Some("refs/heads/feature")
        );
        assert_eq!(
            config(&world.clone, "branch.feature.gh-merge-base").as_deref(),
            Some("main")
        );
    }

    #[test]
    fn fork_pull_requests_take_the_owner_prefix_and_only_fast_forward() {
        let world = world();
        // GitLab publishes merge requests under their own namespace.
        let first = publish(
            &world.work,
            &world.base,
            "fork",
            "refs/merge-requests/10/head",
        );
        let head = PullHead {
            number: 10,
            head_branch: "main".into(),
            head_repo: Some("maya/orbit".into()),
            base_repo: "orbit/orbit".into(),
            base_branch: "main".into(),
        };
        let site = site(ProviderKind::GitLab);
        let branch = COMMANDS
            .checkout_head(&world.clone, &site, &head, None)
            .unwrap();
        assert_eq!(branch, "maya/main");
        assert_eq!(git(&world.clone, &["rev-parse", "HEAD"]), first);
        assert_eq!(
            git(&world.clone, &["rev-parse", "main"]),
            world.base,
            "the local default branch is untouched"
        );
        assert_eq!(
            config(&world.clone, "branch.maya/main.merge").as_deref(),
            Some("refs/merge-requests/10/head")
        );
        // The fork moves on: the existing branch fast-forwards.
        let second = publish(&world.work, &first, "fork 2", "refs/merge-requests/10/head");
        git(&world.clone, &["checkout", "-q", "main"]);
        let branch = COMMANDS
            .checkout_head(&world.clone, &site, &head, None)
            .unwrap();
        assert_eq!(branch, "maya/main");
        assert_eq!(git(&world.clone, &["rev-parse", "HEAD"]), second);
        // A local commit and a force-pushed fork diverge: nothing moves, the error says so.
        git(
            &world.clone,
            &["commit", "-q", "--allow-empty", "-m", "local"],
        );
        let local = git(&world.clone, &["rev-parse", "HEAD"]);
        git(&world.clone, &["checkout", "-q", "main"]);
        publish(
            &world.work,
            &world.base,
            "rewritten",
            "refs/merge-requests/10/head",
        );
        let error = COMMANDS
            .checkout_head(&world.clone, &site, &head, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("preserved"), "{error}");
        assert_eq!(git(&world.clone, &["rev-parse", "maya/main"]), local);
        assert_eq!(git(&world.clone, &["branch", "--show-current"]), "main");
    }

    #[test]
    fn upstreams_are_written_only_after_a_successful_checkout() {
        let world = world();
        let head = PullHead {
            number: 11,
            head_branch: "missing".into(),
            head_repo: None,
            base_repo: "orbit/orbit".into(),
            base_branch: "main".into(),
        };
        assert!(
            COMMANDS
                .checkout_head(&world.clone, &site(ProviderKind::Forgejo), &head, None)
                .is_err()
        );
        assert_eq!(config(&world.clone, "branch.missing.merge"), None);
        assert_eq!(config(&world.clone, "branch.missing.gh-merge-base"), None);
        // An unknown head (the forge could not be read) still checks out as `pr-N`.
        let commit = publish(&world.work, &world.base, "anonymous", "refs/pull/12/head");
        let head = PullHead {
            number: 12,
            base_repo: "orbit/orbit".into(),
            ..PullHead::default()
        };
        let branch = COMMANDS
            .checkout_head(&world.clone, &site(ProviderKind::Forgejo), &head, None)
            .unwrap();
        assert_eq!(branch, "pr-12");
        assert_eq!(git(&world.clone, &["rev-parse", "pr-12"]), commit);
        assert_eq!(
            config(&world.clone, "branch.pr-12.merge").as_deref(),
            Some("refs/pull/12/head")
        );
        assert_eq!(config(&world.clone, "branch.pr-12.gh-merge-base"), None);
    }

    #[test]
    fn head_refs_and_browser_urls_follow_the_provider() {
        assert_eq!(head_ref(ProviderKind::GitHub, 3), "refs/pull/3/head");
        assert_eq!(head_ref(ProviderKind::Forgejo, 3), "refs/pull/3/head");
        assert_eq!(
            head_ref(ProviderKind::GitLab, 3),
            "refs/merge-requests/3/head"
        );
        assert_eq!(number_of_ref("refs/pull/41/head"), Some(41));
        assert_eq!(number_of_ref("refs/merge-requests/7/head"), Some(7));
        assert_eq!(number_of_ref("refs/heads/main"), None);
        let repo = RepoRef::parse("group/sub/orbit").unwrap();
        let cases = [
            (
                ProviderKind::GitHub,
                "https://example.test/group/sub/orbit/pull/5",
                "/issues/5",
            ),
            (
                ProviderKind::GitLab,
                "https://example.test/group/sub/orbit/-/merge_requests/5",
                "/-/issues/5",
            ),
            (
                ProviderKind::Gitea,
                "https://example.test/group/sub/orbit/pulls/5",
                "/issues/5",
            ),
            (
                ProviderKind::Forgejo,
                "https://example.test/group/sub/orbit/pulls/5",
                "/issues/5",
            ),
        ];
        for (kind, pull, issue) in cases {
            let endpoints = Endpoints {
                kind,
                host: HostKey::new("example.test"),
                api_base: String::new(),
                graphql: None,
                web_base: "https://example.test".into(),
            };
            assert_eq!(
                browser_url(&endpoints, &repo, ItemKind::PullRequest, 5),
                pull
            );
            assert_eq!(
                browser_url(&endpoints, &repo, ItemKind::Issue, 5),
                format!("https://example.test/group/sub/orbit{issue}")
            );
        }
        assert!(open_url("file:///etc/passwd").is_err());
    }
}
