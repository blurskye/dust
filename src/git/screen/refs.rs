//! Local reference actions. Forms own the selected name and object, never a filtered row index.
use super::*;

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Target {
    pub(super) name: String,
    pub(super) id: git2::Oid,
}

#[derive(Clone)]
pub(super) enum Action {
    RenameBranch(Target),
    DeleteBranch(Target),
    Upstream(Target),
    DeleteTag(Target),
    MergeBranch { target: Target, current: Target },
    RebaseBranch { target: Target, current: Target },
}
impl Action {
    fn target(&self) -> &Target {
        match self {
            Self::RenameBranch(target)
            | Self::DeleteBranch(target)
            | Self::Upstream(target)
            | Self::DeleteTag(target)
            | Self::MergeBranch { target, .. }
            | Self::RebaseBranch { target, .. } => target,
        }
    }
    fn reference(&self) -> String {
        let namespace = if matches!(self, Self::DeleteTag(_)) {
            "tags"
        } else {
            "heads"
        };
        format!("refs/{namespace}/{}", self.target().name)
    }
}

pub(super) fn head_target(repository: &git2::Repository) -> Result<Target, String> {
    let head = repository.head().map_err(|error| error.to_string())?;
    Ok(Target {
        name: head.name().ok_or("HEAD has no reference name")?.into(),
        id: head.target().ok_or("The current branch has no commit")?,
    })
}
fn current_branch(repository: &git2::Repository) -> Result<Target, String> {
    let head = head_target(repository)?;
    let name = head
        .name
        .strip_prefix("refs/heads/")
        .ok_or("Check out a local branch before merging or rebasing")?;
    Ok(Target {
        name: name.into(),
        id: head.id,
    })
}

impl GitScreen {
    pub(super) fn reference_form(&mut self, key: &KeyCode) -> Result<(), String> {
        let row = self
            .selected_row()
            .filter(|_| self.last_list == 1)
            .ok_or("Select a local reference")?;
        let name = row.value.to_str().ok_or("Reference name is not UTF-8")?;
        let repository = git2::Repository::open(&self.root).map_err(|error| error.to_string())?;
        let namespace = if self.subview == 3 { "tags" } else { "heads" };
        let reference = format!("refs/{namespace}/{name}");
        let id = repository
            .find_reference(&reference)
            .map_err(|error| error.to_string())?
            .target()
            .ok_or("Select a direct local reference")?;
        let target = Target {
            name: name.into(),
            id,
        };
        let (title, action, mut fields, effect) = match (self.subview, key) {
            (0, KeyCode::Char('m')) => {
                let current = current_branch(&repository)?;
                if current.name == target.name {
                    return Err("Select a different branch to merge into the current branch".into());
                }
                let mut field = Field::new(
                    "Merge into",
                    format!("{} @{}", current.name, &current.id.to_string()[..8]),
                );
                field.read_only = true;
                field.input.cursor = 0;
                (
                    "Merge selected branch?",
                    Action::MergeBranch { target, current },
                    vec![field],
                    "Merge the captured tip; stop on conflicts; no autostash",
                )
            }
            (0, KeyCode::Char('R')) => {
                let current = current_branch(&repository)?;
                if current.name == target.name {
                    return Err("Select a different branch to rebase onto".into());
                }
                let mut field = Field::new(
                    "Rewrite current branch",
                    format!("{} @{}", current.name, &current.id.to_string()[..8]),
                );
                field.read_only = true;
                field.input.cursor = 0;
                (
                    "Rebase current branch?",
                    Action::RebaseBranch { target, current },
                    vec![field],
                    "Replay onto captured tip; rewrite history; no autostash",
                )
            }
            (0, KeyCode::Char('e')) => (
                "Rename local branch",
                Action::RenameBranch(target),
                vec![Field::new("New name", name.into())],
                "No overwrite; keep index and working files",
            ),
            (0, KeyCode::Char('d')) => (
                "Delete local branch?",
                Action::DeleteBranch(target),
                Vec::new(),
                "No force: Git refuses unmerged or checked-out branches",
            ),
            (0, KeyCode::Char('u')) => {
                let refs = self.refs.as_ref().ok_or("References are loading")?;
                let mut choices = refs
                    .branches
                    .iter()
                    .filter(|branch| branch.remote)
                    .filter(|branch| {
                        repository
                            .find_reference(&format!("refs/remotes/{}", branch.name))
                            .is_ok_and(|reference| reference.target().is_some())
                    })
                    .map(|branch| branch.name.clone())
                    .collect::<Vec<_>>();
                choices.sort();
                choices.dedup();
                let first = choices
                    .first()
                    .ok_or("No remote-tracking branches; fetch first")?;
                let current = refs
                    .branches
                    .iter()
                    .find(|branch| !branch.remote && branch.name == name)
                    .and_then(|branch| branch.upstream.as_ref())
                    .filter(|upstream| choices.contains(upstream))
                    .unwrap_or(first)
                    .clone();
                let mut field = Field::new("Upstream", current);
                field.choices = choices;
                (
                    "Set selected branch upstream",
                    Action::Upstream(target),
                    vec![field],
                    "Local tracking config only; no fetch, push or checkout",
                )
            }
            (3, KeyCode::Char('d')) => (
                "Delete local tag?",
                Action::DeleteTag(target),
                Vec::new(),
                "Delete only this local tag; no remote deletion",
            ),
            _ => return Ok(()),
        };
        let confirmation = fields.iter().all(|field| field.read_only);
        let reference = if matches!(
            &action,
            Action::MergeBranch { .. } | Action::RebaseBranch { .. }
        ) {
            format!("{reference} @{}", &action.target().id.to_string()[..8])
        } else {
            reference
        };
        for (label, value) in [("Selected local ref", reference), ("Effect", effect.into())] {
            let mut field = Field::new(label, value);
            field.required = false;
            field.read_only = true;
            field.input.cursor = 0;
            fields.push(field);
        }
        self.notice.clear();
        self.form = Some(Form {
            title: title.into(),
            action: FormAction::Reference(action),
            selected: if confirmation { fields.len() } else { 0 },
            fields,
        });
        Ok(())
    }

    pub(super) fn submit_reference(
        &mut self,
        action: Action,
        values: &[String],
    ) -> Result<(), String> {
        let repository = git2::Repository::open(&self.root).map_err(|error| error.to_string())?;
        let target = action.target();
        // Refuse a stale/replaced selection rather than deleting or renaming a different tip.
        if repository
            .find_reference(&action.reference())
            .ok()
            .and_then(|reference| reference.target())
            != Some(target.id)
        {
            return Err("Selected local ref changed; cancel and select it again".into());
        }
        let args = match &action {
            Action::MergeBranch { current, .. } => {
                let actual = current_branch(&repository)?;
                if actual.name != current.name || actual.id != current.id {
                    return Err("Current branch changed; cancel and select the merge again".into());
                }
                vec![
                    "merge".into(),
                    "--no-edit".into(),
                    "--no-autostash".into(),
                    "-m".into(),
                    format!("Merge branch '{}' into '{}'", target.name, current.name).into(),
                    "--".into(),
                    target.id.to_string().into(),
                ]
            }
            Action::RebaseBranch { current, .. } => {
                let actual = current_branch(&repository)?;
                if actual.name != current.name || actual.id != current.id {
                    return Err("Current branch changed; cancel and select the rebase again".into());
                }
                vec![
                    "-c".into(),
                    "rebase.updateRefs=false".into(),
                    "rebase".into(),
                    "--no-autostash".into(),
                    "--".into(),
                    target.id.to_string().into(),
                ]
            }
            Action::RenameBranch(_) => {
                let name = &values[0];
                if name.starts_with('-')
                    || !git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
                {
                    return Err("Choose a valid branch name".into());
                }
                vec![
                    "branch".into(),
                    "--move".into(),
                    "--".into(),
                    target.name.clone().into(),
                    name.clone().into(),
                ]
            }
            Action::DeleteBranch(_) => {
                // Git checks merge safety and every worktree at execution, not just our snapshot.
                vec![
                    "branch".into(),
                    "--delete".into(),
                    "--".into(),
                    target.name.clone().into(),
                ]
            }
            Action::Upstream(_) => {
                let upstream = format!("refs/remotes/{}", values[0]);
                if !repository
                    .find_reference(&upstream)
                    .is_ok_and(|reference| reference.target().is_some())
                {
                    return Err(
                        "Remote-tracking ref disappeared; choose an existing upstream".into(),
                    );
                }
                // Full refnames avoid local/remote ambiguity and do not split remote names on '/'.
                vec![
                    "branch".into(),
                    format!("--set-upstream-to={upstream}").into(),
                    "--".into(),
                    target.name.clone().into(),
                ]
            }
            Action::DeleteTag(_) => {
                vec![
                    "tag".into(),
                    "--delete".into(),
                    "--".into(),
                    target.name.clone().into(),
                ]
            }
        };
        self.write(args)
    }
}
