//! Deep links (`dusty-git open <url|#n>`): a pull request, merge request, issue or run named
//! by its web address on GitHub, GitLab, Gitea or Forgejo, or by `#123` / `!123` /
//! `owner/repo#123`. The link waits until the repository resolves; another repository on the
//! same host opens the way a cross-repository reference does.
use super::*;
use crate::forge::provider::ProviderKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::screen) enum Kind {
    Pull,
    Issue,
    Run,
    /// `#123` where pull requests and issues share numbers: a pull request if there is one.
    Either,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::screen) struct Link {
    pub(in crate::screen) kind: Kind,
    pub(in crate::screen) number: u64,
    pub(in crate::screen) host: Option<String>,
    pub(in crate::screen) repo: Option<String>,
}

impl Link {
    /// The tab the link opens on (an `Either` starts on Pull requests).
    pub(in crate::screen) fn page(&self) -> usize {
        match self.kind {
            Kind::Issue => 1,
            Kind::Run => 3,
            Kind::Pull | Kind::Either => 2,
        }
    }
}

#[derive(Default)]
pub(super) struct Opening {
    pending: Option<Link>,
    /// `#n` opened as a pull request; an issue if the forge has no such pull request.
    probe: Option<u64>,
    /// The tab a link moved to after the host asked (`take_link_page`).
    moved: Option<usize>,
    /// The item a link opened: the list selects its row once the list has loaded.
    select: Option<(usize, u64)>,
}

fn number(text: &str) -> Result<u64, String> {
    text.parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| format!("`{text}` is not an item number"))
}

/// Parses a deep link; the forge is checked when the repository is known.
pub(in crate::screen) fn parse(text: &str) -> Result<Link, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Give a link or a number (#123)".into());
    }
    let short = |kind: Kind, digits: &str, repo: Option<String>| -> Result<Link, String> {
        Ok(Link {
            kind,
            number: number(digits)?,
            host: None,
            repo,
        })
    };
    if let Some(digits) = text.strip_prefix('#') {
        return short(Kind::Either, digits, None);
    }
    if let Some(digits) = text.strip_prefix('!') {
        return short(Kind::Pull, digits, None);
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return short(Kind::Either, text, None);
    }
    if !text.contains("://") {
        // `owner/repo#12` (GitLab `group/project!12`).
        if let Some(at) = text.rfind(['#', '!']) {
            let (repo, rest) = text.split_at(at);
            let kind = if rest.starts_with('!') {
                Kind::Pull
            } else {
                Kind::Either
            };
            if repo.contains('/') && !repo.starts_with('/') && !repo.ends_with('/') {
                return short(kind, &rest[1..], Some(repo.to_owned()));
            }
        }
        return Err(format!(
            "`{text}` is not a link, #number or owner/repo#number"
        ));
    }
    let url = reqwest::Url::parse(text).map_err(|_| format!("`{text}` is not a web address"))?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(format!("`{text}` is not a web address"));
    }
    let mut host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if let Some(port) = url.port() {
        host = format!("{host}:{port}");
    }
    let host = host.strip_prefix("www.").map(str::to_owned).unwrap_or(host);
    let segments = url
        .path_segments()
        .map(|parts| parts.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    let item = |kind: Kind, repo: &[&str], digits: &str| -> Result<Link, String> {
        Ok(Link {
            kind,
            number: number(digits)?,
            host: Some(host.clone()),
            repo: Some(repo.join("/")),
        })
    };
    // GitLab: `group/sub/project/-/merge_requests/12`, `/-/issues/12`, `/-/pipelines/34`.
    if let Some(dash) = segments.iter().position(|part| *part == "-")
        && dash >= 2
    {
        let repo = &segments[..dash];
        return match &segments[dash + 1..] {
            ["merge_requests", digits, ..] => item(Kind::Pull, repo, digits),
            ["issues", digits, ..] | ["work_items", digits, ..] => item(Kind::Issue, repo, digits),
            ["pipelines", digits, ..] => item(Kind::Run, repo, digits),
            _ => Err("That GitLab link is not a merge request, issue or pipeline".into()),
        };
    }
    // GitHub `owner/repo/pull/12`, Gitea and Forgejo `owner/repo/pulls/12`; issues and runs
    // the same way on all three.
    match segments.as_slice() {
        [owner, repo, "pull" | "pulls", digits, ..] => item(Kind::Pull, &[owner, repo], digits),
        [owner, repo, "issues", digits, ..] => item(Kind::Issue, &[owner, repo], digits),
        [owner, repo, "actions", "runs", digits, ..] => item(Kind::Run, &[owner, repo], digits),
        [_, _] => Err("That link names a repository; link to a pull request, issue or run".into()),
        _ => Err("That link is not a pull request, issue or run".into()),
    }
}

impl Workspace {
    /// The host of the remote the forge was found through. The forge may name itself on
    /// another host in its web addresses (`self.host`); links and remotes on either are ours.
    pub(super) fn remote_host(&self) -> Option<String> {
        crate::forge::detect::choose_remote(&self.root)
            .ok()
            .map(|remote| remote.host.to_string())
    }

    /// Opens `link` now, or once the repository resolves.
    pub(in crate::screen) fn follow(&mut self, link: Link) {
        self.deep_link = None;
        self.link.pending = Some(link);
        self.link.probe = None;
        self.apply_link();
    }

    /// The tab a link moved to since the last call (an `#n` that was an issue).
    pub(in crate::screen) fn take_link_page(&mut self) -> Option<usize> {
        self.link.moved.take()
    }

    fn link_error(&mut self, error: String) {
        self.notice = error;
        self.notice_error = true;
    }

    fn apply_link(&mut self) -> bool {
        if self.repo.is_empty() || self.host.is_empty() {
            return false;
        }
        let Some(link) = self.link.pending.take() else {
            return false;
        };
        let gitlab = self.forge_kind() == Some(ProviderKind::GitLab);
        if let Some(host) = &link.host
            && !host.eq_ignore_ascii_case(&self.host)
            && self
                .remote_host()
                .is_none_or(|remote| !host.eq_ignore_ascii_case(&remote))
        {
            self.link_error(format!(
                "That link is on {host}; this repository is on {}",
                self.host
            ));
            return true;
        }
        let kind = match link.kind {
            // GitLab numbers issues `#n` and merge requests `!n`.
            Kind::Either if gitlab => Kind::Issue,
            kind => kind,
        };
        let page = match kind {
            Kind::Issue => 1,
            Kind::Run => 3,
            _ => 2,
        };
        if let Some(repo) = link
            .repo
            .as_ref()
            .filter(|repo| !repo.eq_ignore_ascii_case(&self.repo))
        {
            if kind == Kind::Run {
                self.link_error(format!(
                    "That {} belongs to {repo}; open Dusty Git there",
                    self.terms().run_noun
                ));
                return true;
            }
            let target = inbox::Target {
                host: self.host.clone(),
                repo: repo.clone(),
                page,
                number: link.number,
            };
            if let Err(error) = self.activate(Action::OpenRemote(target)) {
                self.link_error(error);
            }
            self.link.moved = Some(page);
            return true;
        }
        if kind == Kind::Run {
            if self.page != 3 {
                self.page(3);
            }
            self.page = 3;
            self.run_id = Some(link.number);
            self.attempt = 0;
            self.last_list = 1;
            self.focus = 1;
            self.detail_cursor = 0;
            self.open_run(link.number);
            self.link.moved = Some(3);
            return true;
        }
        if let Err(error) = self.activate(Action::OpenItem(page, link.number)) {
            self.link_error(error);
            return true;
        }
        self.link.moved = Some(page);
        self.link.select = Some((page, link.number));
        if kind == Kind::Either {
            self.link.probe = Some(link.number);
        }
        true
    }

    /// The list follows the item a link opened, so the row and the reader agree. The wish
    /// lapses once another item is open or the list has loaded without that row.
    fn select_linked(&mut self) -> bool {
        let Some((page, number)) = self.link.select else {
            return false;
        };
        if self.inbox.enabled || self.page != page || self.opened != Some((page, number)) {
            self.link.select = None;
            return false;
        }
        if self.value(&self.list_key()).is_none() {
            return false;
        }
        self.link.select = None;
        let Some(row) = self
            .items()
            .iter()
            .position(|item| item["number"].as_u64() == Some(number))
        else {
            return false;
        };
        let changed = self.selected[page - 1] != row;
        self.selected[page - 1] = row;
        changed
    }

    /// From `poll`: a waiting link opens once the repository resolves, and an `#n` that is
    /// not a pull request opens as an issue.
    pub(super) fn poll_link(&mut self) -> bool {
        let mut changed = self.link.pending.is_some() && self.apply_link();
        if let Some(number) = self.link.probe {
            if self.opened != Some((2, number)) || self.page != 2 {
                self.link.probe = None;
            } else {
                match self.cache.get(&Key::Item(2, number)) {
                    Some(Ok(_)) => self.link.probe = None,
                    Some(Err(_)) => {
                        self.link.probe = None;
                        self.cache.remove(&Key::Item(2, number));
                        self.notice.clear();
                        self.notice_error = false;
                        if let Err(error) = self.activate(Action::OpenItem(1, number)) {
                            self.link_error(error);
                        }
                        self.link.moved = Some(1);
                        self.link.select = Some((1, number));
                        changed = true;
                    }
                    None => {}
                }
            }
        }
        changed | self.select_linked()
    }
}

impl Workspace {
    /// The features' share of `poll`: drafts saved, links followed, a branch's sheet settled.
    pub(super) fn poll_features(&mut self) -> bool {
        let mut changed = self.sync_drafts();
        changed |= self.poll_link();
        changed |= self.poll_compose();
        changed
    }

    /// `activate`'s first look: the sheets that live in `create.rs` and `drafts.rs`.
    pub(super) fn activate_feature(
        &mut self,
        action: &Action,
    ) -> Option<Result<Option<Form>, String>> {
        match action {
            // Signed out: the forge's own form, prefilled, instead of a refusal.
            Action::CreatePullRequest
                if !self.repo.is_empty()
                    && !self.inbox.enabled
                    && self.refusal(action).is_some() =>
            {
                Some(self.web_pr_form())
            }
            Action::CreatePullRequestWeb => Some(self.web_pr_form()),
            Action::InlineRange { .. } | Action::Draft(..) => Some(self.draft_form(action)),
            Action::CommitSuggestion(..) => Some(self.suggestion_form(action)),
            // Enter or `o` on a pull request's check (`checks.rs`).
            Action::Check(id, key) => Some(self.open_check(*id, key)),
            Action::MergeMethod(_) => {
                self.cycle_merge_method();
                Some(Ok(None))
            }
            _ => None,
        }
    }

    /// `submit`'s first look, after a scoped action is unwrapped.
    pub(super) fn submit_feature(
        &mut self,
        action: &Action,
        values: &[String],
    ) -> Option<Result<(), String>> {
        match action {
            Action::CreatePullRequestWeb => Some(self.submit_web_pr(values)),
            _ => self.submit_draft(action, values),
        }
    }

    /// The line under a feature sheet's title.
    pub(in crate::screen) fn feature_note(
        &self,
        action: &Action,
        fields: &[Field],
    ) -> Option<String> {
        match action {
            Action::CreatePullRequest | Action::CreatePullRequestWeb => {
                self.create_note(action, fields)
            }
            Action::CommitSuggestion(..) => self.suggestion_note(action),
            _ => self.draft_note(action),
        }
    }
}

/// `dusty-git open <link>` and the editor's `workspace.git.open.link`.
impl super::super::GitScreen {
    pub fn open_link(&mut self, text: &str) -> Result<(), String> {
        if self.tutor.is_some() {
            return Err("Links open outside the tutor".into());
        }
        let link = parse(text)?;
        let page = link.page();
        self.clear_file_selection();
        self.page = page;
        self.github.page(page);
        self.github.follow(link);
        if let Some(page) = self.github.take_link_page() {
            self.page = page;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(kind: Kind, number: u64, host: Option<&str>, repo: Option<&str>) -> Link {
        Link {
            kind,
            number,
            host: host.map(str::to_owned),
            repo: repo.map(str::to_owned),
        }
    }

    #[test]
    fn short_forms_name_numbers() {
        assert_eq!(parse("#12").unwrap(), link(Kind::Either, 12, None, None));
        assert_eq!(parse(" 12 ").unwrap(), link(Kind::Either, 12, None, None));
        assert_eq!(parse("!12").unwrap(), link(Kind::Pull, 12, None, None));
        assert_eq!(
            parse("team/app#4").unwrap(),
            link(Kind::Either, 4, None, Some("team/app"))
        );
        assert_eq!(
            parse("group/sub/app!4").unwrap(),
            link(Kind::Pull, 4, None, Some("group/sub/app"))
        );
        assert!(parse("#0").is_err());
        assert!(parse("#x").is_err());
        assert!(parse("").is_err());
        assert!(parse("app#4").is_err());
    }

    #[test]
    fn web_addresses_of_every_forge() {
        assert_eq!(
            parse("https://github.com/example/demo/pull/9/files?diff=split#r1").unwrap(),
            link(Kind::Pull, 9, Some("github.com"), Some("example/demo"))
        );
        assert_eq!(
            parse("https://www.github.com/example/demo/issues/7").unwrap(),
            link(Kind::Issue, 7, Some("github.com"), Some("example/demo"))
        );
        assert_eq!(
            parse("https://github.com/example/demo/actions/runs/91/job/42").unwrap(),
            link(Kind::Run, 91, Some("github.com"), Some("example/demo"))
        );
        assert_eq!(
            parse("https://gitlab.ci.test/orbit/team/orbit/-/merge_requests/13/diffs").unwrap(),
            link(
                Kind::Pull,
                13,
                Some("gitlab.ci.test"),
                Some("orbit/team/orbit")
            )
        );
        assert_eq!(
            parse("https://gitlab.ci.test/orbit/team/orbit/-/issues/14").unwrap(),
            link(
                Kind::Issue,
                14,
                Some("gitlab.ci.test"),
                Some("orbit/team/orbit")
            )
        );
        assert_eq!(
            parse("https://gitlab.ci.test/orbit/team/orbit/-/pipelines/5").unwrap(),
            link(
                Kind::Run,
                5,
                Some("gitlab.ci.test"),
                Some("orbit/team/orbit")
            )
        );
        assert_eq!(
            parse("https://gitea.com/orbit/orbit/pulls/1402").unwrap(),
            link(Kind::Pull, 1402, Some("gitea.com"), Some("orbit/orbit"))
        );
        assert_eq!(
            parse("https://code.forgejo.org/orbit/orbit/actions/runs/200481/jobs/0").unwrap(),
            link(
                Kind::Run,
                200481,
                Some("code.forgejo.org"),
                Some("orbit/orbit")
            )
        );
        assert_eq!(
            parse("http://localhost:3000/me/app/issues/2").unwrap(),
            link(Kind::Issue, 2, Some("localhost:3000"), Some("me/app"))
        );
        assert!(parse("https://github.com/example/demo").is_err());
        assert!(parse("https://github.com/example/demo/wiki/Home").is_err());
        assert!(parse("ftp://github.com/example/demo/pull/9").is_err());
    }
}
