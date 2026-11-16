//! Fork-aware PR forms. Reading metadata never publishes a branch or creates a PR.
use super::*;
use crate::forge::pull_request::{
    CreatePullRequest, PullRequestPush, repository_from_url, valid_repository,
};

impl Workspace {
    /// `OWNER/NAME`; GitLab projects may sit in nested groups (`group/sub/project`).
    pub(super) fn valid_pr_repository(&self, name: &str) -> bool {
        if self.forge_kind() != Some(crate::forge::provider::ProviderKind::GitLab) {
            return valid_repository(name);
        }
        name.rsplit_once('/').is_some_and(|(groups, project)| {
            groups
                .split('/')
                .all(|group| valid_repository(&format!("{group}/{project}")))
        })
    }

    fn remotes(&self) -> Vec<(String, String)> {
        let Ok(repo) = git2::Repository::open(&self.root) else {
            return Vec::new();
        };
        let Ok(names) = repo.remotes() else {
            return Vec::new();
        };
        names
            .iter()
            .flatten()
            .filter_map(|name| {
                let remote = repo.find_remote(name).ok()?;
                Some((
                    name.into(),
                    repository_from_url(remote.pushurl().or(remote.url())?, &self.host)?,
                ))
            })
            .collect()
    }

    fn head_repository(&self) -> String {
        let remotes = self.remotes();
        if let Ok(repo) = git2::Repository::open(&self.root)
            && let Ok(config) = repo.config()
        {
            let branch = crate::git::git_branch_name(&self.root)
                .ok()
                .flatten()
                .unwrap_or_default();
            for name in [
                config
                    .get_string(&format!("branch.{branch}.pushRemote"))
                    .ok(),
                config.get_string("remote.pushDefault").ok(),
                config.get_string(&format!("branch.{branch}.remote")).ok(),
                Some("origin".into()),
            ]
            .into_iter()
            .flatten()
            {
                if let Some((_, repository)) = remotes.iter().find(|(remote, _)| remote == &name) {
                    return repository.clone();
                }
            }
        }
        remotes
            .first()
            .map_or_else(|| self.repo.clone(), |(_, repository)| repository.clone())
    }

    pub(super) fn create_pr_form(&mut self) -> Result<Option<Form>, String> {
        let head = self.head_repository();
        let Some(reply) = self.cache.get(&Key::PullRequestDefaults) else {
            self.pending_pr = true;
            let read = self.read(Read::RepositoryNamed(head));
            self.enqueue(Key::PullRequestDefaults, read, false);
            self.notice = "Loading fork and upstream repositories…".into();
            return Ok(None);
        };
        let info = &reply.as_ref().map_err(Clone::clone)?.value;
        let base_info = if info["parent"]["full_name"].is_string() {
            &info["parent"]
        } else {
            info
        };
        let base = string(base_info, "full_name");
        if !self.valid_pr_repository(base) {
            return Err("GitHub returned an invalid repository".into());
        }
        let branch = crate::git::git_branch_name(&self.root)
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let mut repositories = vec![base.to_owned(), head.clone(), self.repo.clone()];
        repositories.extend(self.remotes().into_iter().map(|(_, repository)| repository));
        repositories.sort();
        repositories.dedup();
        let mut base_field = actions::field("Base repository", base, true, false);
        base_field.choices = repositories.clone();
        base_field.custom_choices = true;
        let mut head_field = actions::field("Head repository (fork)", &head, true, false);
        head_field.choices = repositories;
        head_field.custom_choices = true;
        let mut head_branch = actions::field("Head branch", &branch, true, false);
        if let Ok(repo) = git2::Repository::open(&self.root)
            && let Ok(branches) = repo.branches(Some(git2::BranchType::Local))
        {
            head_branch.choices = branches
                .flatten()
                .filter_map(|(branch, _)| branch.name().ok().flatten().map(str::to_owned))
                .collect();
        }
        head_branch.custom_choices = true;
        self.pending_pr = false;
        self.notice.clear();
        // The description starts from the base branch's pull request template; when
        // the repository keeps several, a Template picker swaps between them.
        let templates = templates(&self.root, self.forge_kind());
        let description = templates
            .first()
            .map(|(_, text)| text.clone())
            .unwrap_or_default();
        let picker = (templates.len() > 1).then(|| {
            let names = templates
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>();
            let mut field = actions::choice(
                "Template",
                &names[0],
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            field.custom_choices = false;
            field
        });
        let title = if self.forge_kind() == Some(crate::forge::provider::ProviderKind::GitLab) {
            "Create merge request"
        } else {
            "Create pull request"
        };
        let mut form = self.form(
            title,
            Action::CreatePullRequest,
            [
                actions::field("Title", "", true, false),
                actions::field("Description", &description, false, true),
                base_field,
                actions::field(
                    "Base branch",
                    string(base_info, "default_branch"),
                    true,
                    false,
                ),
                head_field,
                head_branch,
                // Shown as toggles when needed (create.rs `compact_pr`): a branch the forge
                // lacks is pushed first unless unticked; a pull request opens ready.
                actions::choice(
                    "Publish branch",
                    "Push local branch",
                    &["Already pushed", "Push local branch"],
                ),
                actions::choice("Draft", "false", &["true", "false"]),
            ]
            .into_iter()
            .chain(picker)
            .collect(),
        );
        // The branch's commits fill a fresh sheet; the base branch is picked.
        self.compose_pr(&mut form);
        self.compact_pr(&mut form);
        Ok(Some(form))
    }

    pub(in crate::git::screen) fn review_pr(
        &mut self,
        values: Vec<String>,
    ) -> Result<Form, String> {
        self.save_draft(&Action::CreatePullRequest, values.clone());
        // The Template picker (last, when shown) only seeded the description.
        let values = if values.len() == 9 {
            values[..8].to_vec()
        } else {
            values
        };
        let [
            title,
            body,
            base_repository,
            base_branch,
            head_repository,
            head_branch,
            publish,
            draft,
        ] = values.as_slice()
        else {
            return Err("Invalid PR form".into());
        };
        for repository in [base_repository, head_repository] {
            if !self.valid_pr_repository(repository) {
                return Err("Repository must be OWNER/NAME on the displayed GitHub host".into());
            }
        }
        for branch in [base_branch, head_branch] {
            if !git2::Reference::is_valid_name(&format!("refs/heads/{branch}"))
                || branch.starts_with('-')
            {
                return Err("Choose a valid branch name, without an owner prefix".into());
            }
        }
        if base_repository == head_repository && base_branch == head_branch {
            return Err("Head and base must be different branches or repositories".into());
        }
        let push = if publish == "Push local branch" {
            let remote = self.remotes().into_iter().find(|(_, repository)| repository == head_repository)
                .map(|(remote, _)| remote).ok_or("No push remote points to the head repository; add one or publish the branch first")?;
            let repo = git2::Repository::open(&self.root).map_err(|error| error.to_string())?;
            let commit = repo
                .find_branch(head_branch, git2::BranchType::Local)
                .map_err(|_| "Head branch is not local; choose an existing local branch to push")?
                .get()
                .peel_to_commit()
                .map_err(|error| error.to_string())?
                .id()
                .to_string();
            Some(PullRequestPush { remote, commit })
        } else {
            None
        };
        let request = CreatePullRequest {
            host: self.host.clone(),
            base_repository: base_repository.clone(),
            base_branch: base_branch.clone(),
            head_repository: head_repository.clone(),
            head_branch: head_branch.clone(),
            title: title.clone(),
            body: body.clone(),
            draft: draft == "true",
            push,
        };
        let publication = request.push.as_ref().map_or_else(
            || "Use the published branch; do not push".into(),
            |push| {
                format!(
                    "Push {} to {} (no force push)",
                    &push.commit[..8],
                    push.remote
                )
            },
        );
        let mut fields = vec![
            actions::field(
                "From",
                &format!("{}/{} : {}", request.host, head_repository, head_branch),
                false,
                false,
            ),
            actions::field(
                "Into",
                &format!("{}/{} : {}", request.host, base_repository, base_branch),
                false,
                false,
            ),
            actions::field(
                "Publish · committed changes only",
                &publication,
                false,
                false,
            ),
            actions::field("Title", title, false, false),
            actions::field("Description", body, false, true),
        ];
        for field in &mut fields {
            field.read_only = true;
            field.input.cursor = 0;
        }
        let title = if request.draft {
            "Review draft pull request"
        } else {
            "Review pull request"
        };
        Ok(self.form(title, Action::ConfirmPullRequest(Box::new(request)), fields))
    }

    pub(super) fn poll_pr_form(&mut self) {
        // Retried until the defaults are in memory: a disk miss that came back after the
        // turn's budget never sent the forge read, so waiting for the entry alone stalls.
        if self.pending_pr {
            self.pending_pr = false;
            match self.create_pr_form() {
                Ok(form) => self.ready_form = form,
                Err(error) => self.notice = error,
            }
        }
    }

    pub fn take_form(&mut self) -> Option<Form> {
        self.ready_form.take()
    }
}

/// The base branch's pull request templates (path, text), the forge's default first.
pub(super) fn templates(
    root: &std::path::Path,
    kind: Option<crate::forge::provider::ProviderKind>,
) -> Vec<(String, String)> {
    use crate::forge::template;
    let kind = kind.unwrap_or(crate::forge::provider::ProviderKind::GitHub);
    let remote = crate::forge::detect::choose_remote(root)
        .ok()
        .map(|remote| remote.remote);
    let base = template::base_ref(root, remote.as_deref());
    let Ok(repository) = git2::Repository::open(root) else {
        return Vec::new();
    };
    template::candidates(root, kind, &base)
        .into_iter()
        .filter_map(|path| {
            let blob = repository
                .revparse_single(&format!("{base}:{path}"))
                .ok()?
                .peel_to_blob()
                .ok()?;
            let text = String::from_utf8_lossy(blob.content()).into_owned();
            (!text.trim().is_empty()).then_some((path, text))
        })
        .take(16)
        .collect()
}

/// A new Template choice replaces the description while it is still an untouched template
/// (or empty); text the author wrote is never thrown away.
pub(in crate::git::screen) fn template_chosen(workspace: &Workspace, form: &mut Form) {
    let (root, kind) = (workspace.root.as_path(), workspace.forge_kind());
    if !matches!(form.action, FormAction::Github(Action::CreatePullRequest)) {
        return;
    }
    let Some(chosen) = form
        .fields
        .iter()
        .find(|field| field.label == "Template")
        .map(|field| field.input.input.clone())
    else {
        return;
    };
    let templates = templates(root, kind);
    let Some((_, text)) = templates.iter().find(|(path, _)| *path == chosen) else {
        return;
    };
    if let Some(description) = form
        .fields
        .iter_mut()
        .find(|field| field.label == "Description")
    {
        let current = description.input.input.trim_end().to_owned();
        if current.trim().is_empty()
            || templates
                .iter()
                .any(|(_, template)| template.trim() == current.trim())
        {
            description.input.input = text.clone();
            description.input.cursor = 0;
        } else if let Some(kept) = templates.iter().find_map(|(_, template)| {
            current
                .strip_suffix(template.trim_end())
                .filter(|_| !template.trim().is_empty())
        }) {
            // The commits' text above an untouched template stays; only the template swaps.
            description.input.input = format!("{}{text}", kept);
            description.input.cursor = 0;
        }
    }
}
