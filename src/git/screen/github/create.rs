//! Create pull request: the sheet starts from the branch's commits (one
//! commit gives its subject and body; several give the branch's name and a list of
//! subjects), the base branch's template wins over the list, and the base branch is picked
//! from the remote's branches. `o` on a branch in the Git tab opens it for that branch. When
//! this account cannot write, the same fields open the forge's own form in the browser.
use super::*;
use crate::forge::provider::ProviderKind;

/// The longest body a prefilled web address carries; longer text is cut with a note.
const WEB_BODY: usize = 4000;

#[derive(Default)]
pub(super) struct Compose {
    /// The branch `o` asked for, until its sheet opens.
    head: Option<String>,
    /// `o` came before the repository resolved: the sheet opens once it does.
    waiting: bool,
    /// The open sheet's fields it does not show (same-repository targets, the head branch
    /// the note names, a publish choice an up-to-date branch does not need).
    hidden: Vec<(String, String)>,
}

/// The Create sheet's values in the order `review_pr` reads them; a Template picker, when
/// shown, comes after.
const PR_FIELDS: [&str; 8] = [
    "Title",
    "Description",
    "Base repository",
    "Base branch",
    "Head repository (fork)",
    "Head branch",
    "Publish branch",
    "Draft",
];
const OPEN_AS_DRAFT: &str = "Open as draft";
const PUSH: &str = "Push local branch";
const PUSHED: &str = "Already pushed";

/// Where the head branch stands against the head repository's copy of it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Published {
    /// The remote has it, with every local commit.
    Yes,
    /// The remote has it, but not these many local commits.
    Ahead(usize),
    /// Only this clone has it.
    Missing,
}

/// A commit on the head branch that the base does not have.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Commit {
    pub(super) subject: String,
    pub(super) body: String,
}

/// The commits of `head` not on `base`, oldest first (at most 50).
fn commits(repository: &git2::Repository, base: git2::Oid, head: git2::Oid) -> Vec<Commit> {
    let Ok(mut walk) = repository.revwalk() else {
        return Vec::new();
    };
    if walk.push(head).is_err() {
        return Vec::new();
    }
    let _ = walk.hide(base);
    let _ = walk.set_sorting(git2::Sort::TOPOLOGICAL);
    let mut found = walk
        .flatten()
        .take(50)
        .filter_map(|oid| repository.find_commit(oid).ok())
        .filter(|commit| commit.parent_count() < 2)
        .map(|commit| Commit {
            subject: commit.summary().unwrap_or_default().trim().to_owned(),
            body: commit.body().unwrap_or_default().trim().to_owned(),
        })
        .collect::<Vec<_>>();
    found.reverse();
    found
}

/// A branch name as a title: `feature/retry-backoff` reads `Retry backoff`, and a prefix
/// that says what the change does stays as its verb (`fix/backoff` reads `Fix backoff`).
pub(super) fn branch_title(branch: &str) -> String {
    let mut segments = branch
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let name = segments.pop().unwrap_or(branch);
    let words = name.replace(['-', '_'], " ");
    let mut words = words
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let verb = segments
        .last()
        .and_then(|prefix| match prefix.to_ascii_lowercase().as_str() {
            "fix" | "bugfix" | "hotfix" => Some("fix"),
            "refactor" => Some("refactor"),
            "revert" => Some("revert"),
            "test" | "tests" => Some("test"),
            "docs" | "doc" => Some("document"),
            _ => None,
        });
    if let Some(verb) = verb
        && words
            .first()
            .is_none_or(|first| !first.eq_ignore_ascii_case(verb))
    {
        words.insert(0, verb.to_owned());
    }
    let words = words.join(" ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The description a sheet starts from: what the commits say, then the template under it.
pub(super) fn describe(commits: &str, template: Option<String>) -> String {
    match template {
        Some(template) if commits.trim().is_empty() => template,
        Some(template) => format!("{}\n\n{template}", commits.trim_end()),
        None => commits.to_owned(),
    }
}

/// The title and description the commits suggest (`gh pr create --fill`).
pub(super) fn suggest(commits: &[Commit], branch: &str) -> (String, String) {
    match commits {
        [] => (String::new(), String::new()),
        [only] => (only.subject.clone(), only.body.clone()),
        many => {
            let title = branch_title(branch);
            let title = if title.is_empty() {
                many[0].subject.clone()
            } else {
                title
            };
            let body = many
                .iter()
                .map(|commit| format!("- {}", commit.subject))
                .collect::<Vec<_>>()
                .join("\n");
            (title, body)
        }
    }
}

/// Query text, percent-encoded (RFC 3986 unreserved characters stay).
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// What the forge's own "new pull request" page needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WebRequest {
    pub(super) host: String,
    pub(super) repo: String,
    /// The fork's owner when the head branch lives in another repository.
    pub(super) head_owner: Option<String>,
    pub(super) base: String,
    pub(super) head: String,
    pub(super) title: String,
    pub(super) body: String,
}

/// The prefilled web address: GitHub's compare view, GitLab's new merge request, Gitea and
/// Forgejo's compare view. The body is cut to a size every browser accepts.
pub(super) fn web_url(kind: ProviderKind, request: &WebRequest) -> String {
    let mut body = request.body.clone();
    if body.len() > WEB_BODY {
        let mut cut = WEB_BODY;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n\n…");
    }
    let (title, body) = (encode(&request.title), encode(&body));
    let origin = format!("https://{}/{}", request.host, request.repo);
    match kind {
        ProviderKind::GitLab => format!(
            "{origin}/-/merge_requests/new?merge_request%5Bsource_branch%5D={}&merge_request%5Btarget_branch%5D={}&merge_request%5Btitle%5D={title}&merge_request%5Bdescription%5D={body}",
            encode(&request.head),
            encode(&request.base),
        ),
        ProviderKind::GitHub => {
            let head = match &request.head_owner {
                Some(owner) => format!("{owner}:{}", request.head),
                None => request.head.clone(),
            };
            format!(
                "{origin}/compare/{}...{}?expand=1&title={title}&body={body}",
                encode_path(&request.base),
                encode_path(&head)
            )
        }
        _ => {
            let head = match &request.head_owner {
                Some(owner) => format!("{owner}:{}", request.head),
                None => request.head.clone(),
            };
            format!(
                "{origin}/compare/{}...{}?expand=1&title={title}&body={body}",
                encode_path(&request.base),
                encode_path(&head)
            )
        }
    }
}

/// A branch in a URL path: slashes and a fork's `owner:` stay, everything else outside the
/// unreserved set is escaped.
fn encode_path(text: &str) -> String {
    text.split('/')
        .map(|segment| segment.split(':').map(encode).collect::<Vec<_>>().join(":"))
        .collect::<Vec<_>>()
        .join("/")
}

fn value(fields: &[Field], label: &str) -> String {
    fields
        .iter()
        .find(|field| field.label == label)
        .map(|field| field.input.input.trim().to_owned())
        .unwrap_or_default()
}

fn set(fields: &mut [Field], label: &str, text: &str) {
    if let Some(field) = fields.iter_mut().find(|field| field.label == label) {
        field.input.input = text.to_owned();
        field.input.cursor = if field.multiline { 0 } else { text.len() };
    }
}

impl Workspace {
    fn pull_kind(&self) -> ProviderKind {
        self.forge_kind().unwrap_or(ProviderKind::GitHub)
    }

    /// The commit a base branch stands on: the base repository's remote-tracking branch, else
    /// any remote's, else the local branch.
    fn base_commit(
        &self,
        repository: &git2::Repository,
        base_repository: &str,
        base: &str,
    ) -> Option<git2::Oid> {
        let remotes = self.pr_remotes();
        let mut names = remotes
            .iter()
            .filter(|(_, repo)| repo.eq_ignore_ascii_case(base_repository))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        names.push("origin".into());
        names.extend(remotes.iter().map(|(name, _)| name.clone()));
        names
            .iter()
            .map(|remote| format!("refs/remotes/{remote}/{base}"))
            .chain([format!("refs/heads/{base}")])
            .find_map(|reference| repository.refname_to_id(&reference).ok())
    }

    pub(super) fn pr_remotes(&self) -> Vec<(String, String)> {
        let Ok(repo) = git2::Repository::open(&self.root) else {
            return Vec::new();
        };
        let Ok(names) = repo.remotes() else {
            return Vec::new();
        };
        let found_through = self.remote_host();
        let known = |host: &str| {
            host.eq_ignore_ascii_case(&self.host)
                || found_through
                    .as_deref()
                    .is_some_and(|found| host.eq_ignore_ascii_case(found))
        };
        names
            .iter()
            .flatten()
            .filter_map(|name| {
                let remote = repo.find_remote(name).ok()?;
                // GitLab projects may sit in nested groups (`orbit/team/orbit`).
                let url = crate::git::web_base(remote.pushurl().or(remote.url())?)?;
                let host = match url.port() {
                    Some(port) => format!("{}:{port}", url.host_str()?),
                    None => url.host_str()?.to_owned(),
                };
                let path = url.path().trim_matches('/');
                (known(&host) && self.valid_pr_repository(path))
                    .then(|| (name.to_owned(), path.to_owned()))
            })
            .collect()
    }

    /// The head branch's commits that the base lacks; `None` when either branch is not
    /// here (without the base, the whole history would read as the branch's work).
    pub(super) fn branch_commits(
        &self,
        base_repository: &str,
        base: &str,
        head: &str,
    ) -> Option<Vec<Commit>> {
        let repository = git2::Repository::open(&self.root).ok()?;
        let head = repository
            .refname_to_id(&format!("refs/heads/{head}"))
            .ok()?;
        let base = self.base_commit(&repository, base_repository, base)?;
        Some(commits(&repository, base, head))
    }

    /// Branch names the base may be: the base repository's remote branches, then local ones.
    fn base_choices(&self, base_repository: &str, default: &str) -> Vec<String> {
        let Ok(repository) = git2::Repository::open(&self.root) else {
            return Vec::new();
        };
        let remotes = self
            .pr_remotes()
            .into_iter()
            .filter(|(_, repo)| repo.eq_ignore_ascii_case(base_repository))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        let mut names = vec![default.to_owned()];
        if let Ok(branches) = repository.branches(None) {
            for (branch, kind) in branches.flatten() {
                let Some(name) = branch.name().ok().flatten() else {
                    continue;
                };
                let name = match kind {
                    git2::BranchType::Remote => match name.split_once('/') {
                        Some((remote, rest))
                            if remotes.iter().any(|known| known == remote) && rest != "HEAD" =>
                        {
                            rest
                        }
                        _ => continue,
                    },
                    git2::BranchType::Local => name,
                };
                names.push(name.to_owned());
            }
        }
        names.retain(|name| !name.is_empty());
        let mut seen = BTreeSet::new();
        names.retain(|name| seen.insert(name.clone()));
        names.truncate(200);
        names
    }

    /// Fills a fresh Create sheet from the head branch's commits and offers base branches.
    pub(super) fn compose_pr(&mut self, form: &mut Form) {
        let had_draft = self
            .drafts
            .contains_key(&self.draft_key(&Action::CreatePullRequest));
        let wanted = self.compose.head.take();
        let fields = &mut form.fields;
        let mut fresh = !had_draft;
        if let Some(branch) = &wanted
            && value(fields, "Head branch") != *branch
        {
            set(fields, "Head branch", branch);
            fresh = true;
        }
        let base_repository = value(fields, "Base repository");
        let base = value(fields, "Base branch");
        let head = value(fields, "Head branch");
        if let Some(field) = fields.iter_mut().find(|field| field.label == "Base branch") {
            field.choices = self.base_choices(&base_repository, &base);
            field.custom_choices = true;
        }
        if !fresh {
            return;
        }
        let commits = self
            .branch_commits(&base_repository, &base, &head)
            .unwrap_or_default();
        let (title, body) = suggest(&commits, &head);
        set(fields, "Title", &title);
        // One commit's body or the list of subjects, then the base branch's template.
        let template = super::pull_request::templates(&self.root, self.forge_kind())
            .into_iter()
            .next()
            .map(|(_, text)| text);
        set(fields, "Description", &describe(&body, template));
        if let Some(picker) = fields.iter_mut().find(|field| field.label == "Template")
            && let Some(first) = picker.choices.first().cloned()
        {
            picker.input.input = first;
        }
    }

    /// `o` on a branch: its open pull request when the list knows one, else the Create sheet
    /// for it.
    pub(in crate::git::screen) fn compose_from(
        &mut self,
        branch: String,
    ) -> Result<Option<Form>, String> {
        if branch.is_empty() {
            return Err("Select a branch first".into());
        }
        if self.repo.is_empty() {
            self.compose = Compose {
                head: Some(branch),
                waiting: true,
                ..Compose::default()
            };
            self.notice = format!(
                "Loading the repository to open a {}…",
                self.terms().pr_singular
            );
            self.notice_error = false;
            return Ok(None);
        }
        let open = self
            .items()
            .into_iter()
            .find(|row| {
                string(row, "headRefName") == branch
                    && !matches!(
                        string(row, "state"),
                        "CLOSED" | "MERGED" | "closed" | "merged"
                    )
            })
            .map(|row| number(row, "number"))
            .filter(|number| *number > 0);
        if let Some(number) = open {
            self.activate(Action::OpenItem(2, number))?;
            self.notice = format!("{}{number} is already open for {branch}", self.sigil(true));
            self.notice_error = false;
            return Ok(None);
        }
        self.compose.head = Some(branch);
        let form = self.activate(Action::CreatePullRequest);
        if !self.pending_pr {
            self.compose.head = None;
        }
        form
    }

    /// The branch `o` asked for is dropped once its sheet opened or was cancelled.
    pub(super) fn poll_compose(&mut self) -> bool {
        if self.compose.waiting {
            if self.repo.is_empty() || self.page != 2 {
                // Left the tab before the repository resolved: the request lapses.
                if self.page != 2 {
                    self.compose = Compose::default();
                }
                return false;
            }
            let branch = self.compose.head.take().unwrap_or_default();
            self.compose.waiting = false;
            match self.compose_from(branch) {
                Ok(Some(form)) => self.ready_form = Some(form),
                Ok(None) => {}
                Err(error) => {
                    self.notice = error;
                    self.notice_error = true;
                }
            }
            return true;
        }
        if self.compose.head.is_some() && !self.pending_pr {
            self.compose.head = None;
        }
        false
    }

    /// When this account cannot create pull requests: the same fields, sent to the forge's
    /// own form in the browser.
    pub(super) fn web_pr_form(&mut self) -> Result<Option<Form>, String> {
        if self.repo.is_empty() {
            return Err("The repository is not loaded yet".into());
        }
        let head = self
            .compose
            .head
            .take()
            .or_else(|| crate::git::git_branch_name(&self.root).ok().flatten())
            .unwrap_or_default();
        let default = self
            .value(&Key::Repository)
            .map(|repository| string(repository, "default_branch").to_owned())
            .filter(|branch| !branch.is_empty())
            .unwrap_or_else(|| "main".into());
        let commits = self
            .branch_commits(&self.repo.clone(), &default, &head)
            .unwrap_or_default();
        let (title, body) = suggest(&commits, &head);
        let template = super::pull_request::templates(&self.root, self.forge_kind())
            .into_iter()
            .next()
            .map(|(_, text)| text);
        let body = describe(&body, template);
        let mut base = actions::field("Base branch", &default, true, false);
        base.choices = self.base_choices(&self.repo.clone(), &default);
        base.custom_choices = true;
        let mut head_field = actions::field("Head branch", &head, true, false);
        if let Ok(repository) = git2::Repository::open(&self.root)
            && let Ok(branches) = repository.branches(Some(git2::BranchType::Local))
        {
            head_field.choices = branches
                .flatten()
                .filter_map(|(branch, _)| branch.name().ok().flatten().map(str::to_owned))
                .collect();
        }
        head_field.custom_choices = true;
        let title_text = format!("New {} in the browser", self.terms().pr_singular);
        Ok(Some(self.form(
            &title_text,
            Action::CreatePullRequestWeb,
            vec![
                actions::field("Title", &title, false, false),
                actions::field("Description", &body, false, true),
                base,
                head_field,
            ],
        )))
    }

    pub(super) fn submit_web_pr(&mut self, values: &[String]) -> Result<(), String> {
        let [title, body, base, head] = values else {
            return Err("Invalid pull request form".into());
        };
        let (base, head) = (base.trim(), head.trim());
        for branch in [base, head] {
            if !git2::Reference::is_valid_name(&format!("refs/heads/{branch}"))
                || branch.starts_with('-')
            {
                return Err("Choose a valid branch name".into());
            }
        }
        if base == head {
            return Err("Head and base must be different branches".into());
        }
        let owner = |repo: &str| {
            repo.rsplit_once('/')
                .map(|(owner, _)| owner.to_owned())
                .unwrap_or_default()
        };
        let fork = self
            .pr_remotes()
            .into_iter()
            .map(|(_, repo)| repo)
            .find(|repo| {
                !repo.eq_ignore_ascii_case(&self.repo) && self.head_remote_has(repo, head)
            });
        let request = WebRequest {
            host: self.host.clone(),
            repo: self.repo.clone(),
            head_owner: fork
                .map(|repo| owner(&repo))
                .filter(|owner| !owner.eq_ignore_ascii_case(&self.owner())),
            base: base.to_owned(),
            head: head.to_owned(),
            title: title.trim().to_owned(),
            body: body.clone(),
        };
        let url = web_url(self.pull_kind(), &request);
        self.drafts
            .remove(&self.draft_key(&Action::CreatePullRequestWeb));
        self.cache.remove(&Key::Write);
        self.enqueue(
            Key::Write,
            Op::Write(
                self.target(),
                Write::OpenInBrowser {
                    kind: crate::forge::model::ItemKind::PullRequest,
                    number: 0,
                    url: Some(url),
                },
            ),
            false,
        );
        self.notice_error = false;
        self.notice = "Opening the browser…".into();
        Ok(())
    }

    fn owner(&self) -> String {
        self.repo
            .rsplit_once('/')
            .map(|(owner, _)| owner.to_owned())
            .unwrap_or_default()
    }

    /// Whether `repo`'s remote has `branch` (a fork the head was pushed to).
    fn head_remote_has(&self, repo: &str, branch: &str) -> bool {
        let Ok(repository) = git2::Repository::open(&self.root) else {
            return false;
        };
        self.pr_remotes()
            .iter()
            .filter(|(_, known)| known.eq_ignore_ascii_case(repo))
            .any(|(remote, _)| {
                repository
                    .refname_to_id(&format!("refs/remotes/{remote}/{branch}"))
                    .is_ok()
            })
    }

    /// The line under the Create sheet's title: what goes where and how much; then, when the
    /// branch is not on the forge yet and will not be pushed, that it has to be.
    pub(super) fn create_note(&self, action: &Action, fields: &[Field]) -> Option<String> {
        let web = matches!(action, Action::CreatePullRequestWeb);
        let base_repository = if web {
            self.repo.clone()
        } else {
            self.pr_value(fields, "Base repository")
        };
        let (base, head) = (
            self.pr_value(fields, "Base branch"),
            self.pr_value(fields, "Head branch"),
        );
        if head.is_empty() || base.is_empty() {
            return None;
        }
        let head_repository = if web {
            String::new()
        } else {
            self.pr_value(fields, "Head repository (fork)")
        };
        // Across a fork each side names its owner, as the forges write it: `maya:fix/backoff → orbit-dev:main`.
        let owner = |repository: &str| {
            repository
                .rsplit_once('/')
                .map(|(owner, _)| owner.to_owned())
                .unwrap_or_default()
        };
        let mut note = if !head_repository.is_empty() && head_repository != base_repository {
            // A fork under the same owner names whole repositories instead.
            let (from, to) = match (owner(&head_repository), owner(&base_repository)) {
                (from, to) if from != to => (from, to),
                _ => (head_repository.clone(), base_repository.clone()),
            };
            format!("{from}:{head} → {to}:{base}")
        } else {
            format!("{head} → {base}")
        };
        match self
            .branch_commits(&base_repository, &base, &head)
            .map(|commits| commits.len())
        {
            None => {}
            Some(0) => note.push_str(" · no new commits"),
            Some(1) => note.push_str(" · 1 commit"),
            Some(50..) => note.push_str(" · 50+ commits"),
            Some(count) => note.push_str(&format!(" · {count} commits")),
        }
        if web {
            let reason = self
                .refusal(&Action::CreatePullRequest)
                .unwrap_or("Signed out");
            let reason = reason.split(" · ").next().unwrap_or(reason);
            note.push_str(&format!(
                "\n{} · the forge's own form opens with these filled in.",
                reason.trim_end_matches('.')
            ));
            return Some(note);
        }
        if self.pr_value(fields, "Publish branch") != PUSH {
            match self.published(&head_repository, &head) {
                Published::Yes => {}
                Published::Missing => {
                    note.push_str(&format!("\n{head} is not on {head_repository} yet"))
                }
                Published::Ahead(1) => {
                    note.push_str(&format!("\n1 commit is not on {head_repository} yet"))
                }
                Published::Ahead(count) => note.push_str(&format!(
                    "\n{count} commits are not on {head_repository} yet"
                )),
            }
        }
        Some(note)
    }

    /// A Create sheet value by its label: the field on the sheet, else the value it hides.
    fn pr_value(&self, fields: &[Field], label: &str) -> String {
        self.pr_raw(fields, label).trim().to_owned()
    }

    fn pr_raw(&self, fields: &[Field], label: &str) -> String {
        use crate::git::screen::shell::sheet::{is_toggle, toggled};
        if let Some(field) = fields
            .iter()
            .find(|field| field.label == label && !is_toggle(field))
        {
            return field.input.input.clone();
        }
        let toggle = |wanted: &dyn Fn(&Field) -> bool| {
            fields
                .iter()
                .find(|field| is_toggle(field) && wanted(field))
                .map(toggled)
        };
        match label {
            "Publish branch" => {
                if let Some(on) = toggle(&|field| field.label.starts_with("Push ")) {
                    return if on { PUSH } else { PUSHED }.into();
                }
            }
            "Draft" => {
                if let Some(on) = toggle(&|field| field.label == OPEN_AS_DRAFT) {
                    return on.to_string();
                }
            }
            _ => {}
        }
        self.compose
            .hidden
            .iter()
            .find(|(hidden, _)| hidden == label)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }

    /// Every value `review_pr` reads, in its order, from a sheet that shows only some.
    pub(in crate::git::screen) fn pr_values(&self, fields: &[Field]) -> Vec<String> {
        let mut values = PR_FIELDS
            .iter()
            .map(|label| self.pr_raw(fields, label))
            .collect::<Vec<_>>();
        if let Some(template) = fields.iter().find(|field| field.label == "Template") {
            values.push(template.input.input.clone());
        }
        values
    }

    /// Whether the head repository has the branch, and every local commit of it.
    fn published(&self, head_repository: &str, head: &str) -> Published {
        let Ok(repository) = git2::Repository::open(&self.root) else {
            return Published::Yes;
        };
        // A branch typed by name that is not here cannot be pushed from here.
        let Ok(local) = repository.refname_to_id(&format!("refs/heads/{head}")) else {
            return Published::Yes;
        };
        let remote = self
            .pr_remotes()
            .into_iter()
            .filter(|(_, repo)| repo.eq_ignore_ascii_case(head_repository))
            .find_map(|(remote, _)| {
                repository
                    .refname_to_id(&format!("refs/remotes/{remote}/{head}"))
                    .ok()
            });
        match remote {
            None => Published::Missing,
            Some(remote) => match repository.graph_ahead_behind(local, remote) {
                Ok((0, _)) | Err(_) => Published::Yes,
                Ok((ahead, _)) => Published::Ahead(ahead),
            },
        }
    }

    /// Lays a filled Create sheet out for this branch: Title, Description and the
    /// base branch; the repositories only for a fork; a push toggle only when the branch
    /// is not on the forge, or is behind the local one; `☐ Open as draft` last.
    pub(super) fn compact_pr(&mut self, form: &mut Form) {
        use crate::git::screen::shell::sheet::toggle;
        let fields = std::mem::take(&mut form.fields);
        let (base_repository, head_repository, head) = (
            value(&fields, "Base repository"),
            value(&fields, "Head repository (fork)"),
            value(&fields, "Head branch"),
        );
        let fork = !base_repository.eq_ignore_ascii_case(&head_repository);
        let published = self.published(&head_repository, &head);
        let remote = self
            .pr_remotes()
            .into_iter()
            .find(|(_, repo)| repo.eq_ignore_ascii_case(&head_repository))
            .map_or_else(|| "origin".to_owned(), |(remote, _)| remote);
        let mut hidden = Vec::new();
        let mut shown = Vec::new();
        let mut toggles = Vec::new();
        for field in fields {
            let text = field.input.input.clone();
            match field.label.as_str() {
                "Base repository" | "Head repository (fork)" if !fork => {
                    hidden.push((field.label, text))
                }
                "Head branch" if !head.is_empty() => hidden.push((field.label, text)),
                "Publish branch" => match published {
                    Published::Yes => hidden.push((field.label, PUSHED.into())),
                    Published::Ahead(count) => toggles.push(toggle(
                        &format!(
                            "Push {count} commit{} first",
                            if count == 1 { "" } else { "s" }
                        ),
                        text == PUSH,
                    )),
                    Published::Missing => {
                        toggles.push(toggle(&format!("Push to {remote} first"), text == PUSH))
                    }
                },
                "Draft" => toggles.push(toggle(OPEN_AS_DRAFT, text == "true")),
                _ => shown.push(field),
            }
        }
        shown.extend(toggles);
        form.fields = shown;
        form.selected = 0;
        self.compose.hidden = hidden;
    }
}

/// The footer's word for a feature sheet's submit key, matching its button.
pub(super) fn submit_verb(action: &Action) -> Option<&'static str> {
    match action {
        Action::CreatePullRequest => Some("review"),
        Action::CreatePullRequestWeb | Action::Browser(..) | Action::Web(..) => {
            Some("open in browser")
        }
        _ => None,
    }
}

/// `o` on a local branch in the Git tab's Branches pane.
impl super::super::GitScreen {
    pub(in crate::git::screen) fn branch_pull_request(&mut self) -> Result<(), String> {
        let branch = self
            .selected_row()
            .map(|row| row.value.to_string_lossy().into_owned())
            .ok_or("Select a branch first")?;
        // The Git track has no forge; the forge track's practice forge has pull requests.
        if self.tutor.is_some() && !crate::forge::sandbox::active() {
            return Err("Pull requests are not part of the tutor".into());
        }
        self.clear_file_selection();
        self.page = 2;
        self.github.page(2);
        let form = self.github.compose_from(branch);
        self.page = self.github.current_page();
        if let Some(form) = form? {
            self.form = Some(form);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str, body: &str) -> Commit {
        Commit {
            subject: subject.into(),
            body: body.into(),
        }
    }

    #[test]
    fn one_commit_gives_its_subject_and_body() {
        let (title, body) = suggest(
            &[commit("Retry with backoff", "Waits 2s, 4s, 8s.")],
            "feature/retry",
        );
        assert_eq!(title, "Retry with backoff");
        assert_eq!(body, "Waits 2s, 4s, 8s.");
    }

    #[test]
    fn several_commits_give_the_branch_and_a_list() {
        let (title, body) = suggest(
            &[commit("Add retry", ""), commit("Test retry", "")],
            "feature/retry-backoff",
        );
        assert_eq!(title, "Retry backoff");
        assert_eq!(body, "- Add retry\n- Test retry");
        assert_eq!(suggest(&[], "x"), (String::new(), String::new()));
    }

    #[test]
    fn a_branch_name_keeps_the_verb_its_prefix_names() {
        assert_eq!(branch_title("fix/backoff"), "Fix backoff");
        assert_eq!(branch_title("hotfix/fix-crash"), "Fix crash");
        assert_eq!(branch_title("feature/retry-backoff"), "Retry backoff");
        assert_eq!(branch_title("maya/docs/api_guide"), "Document api guide");
        assert_eq!(branch_title("cleanup"), "Cleanup");
    }

    #[test]
    fn the_template_follows_what_the_commits_say() {
        assert_eq!(
            describe("- Add retry", Some("## Why".into())),
            "- Add retry\n\n## Why"
        );
        assert_eq!(describe("", Some("## Why".into())), "## Why");
        assert_eq!(describe("Body", None), "Body");
    }

    #[test]
    fn web_addresses_match_each_forge() {
        let request = WebRequest {
            host: "example.test".into(),
            repo: "team/app".into(),
            head_owner: None,
            base: "main".into(),
            head: "feature/retry".into(),
            title: "Retry & backoff".into(),
            body: "Line one\nline two".into(),
        };
        assert_eq!(
            web_url(ProviderKind::GitHub, &request),
            "https://example.test/team/app/compare/main...feature/retry?expand=1&title=Retry%20%26%20backoff&body=Line%20one%0Aline%20two"
        );
        assert_eq!(
            web_url(ProviderKind::GitLab, &request),
            "https://example.test/team/app/-/merge_requests/new?merge_request%5Bsource_branch%5D=feature%2Fretry&merge_request%5Btarget_branch%5D=main&merge_request%5Btitle%5D=Retry%20%26%20backoff&merge_request%5Bdescription%5D=Line%20one%0Aline%20two"
        );
        let fork = WebRequest {
            head_owner: Some("me".into()),
            ..request.clone()
        };
        assert!(
            web_url(ProviderKind::Gitea, &fork).starts_with(
                "https://example.test/team/app/compare/main...me:feature/retry?expand=1"
            )
        );
        let long = WebRequest {
            body: "x".repeat(WEB_BODY * 2),
            ..request
        };
        assert!(web_url(ProviderKind::Forgejo, &long).len() < WEB_BODY + 400);
    }
}
