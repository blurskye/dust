//! Native, immutable-target commit forms; hosts retain the usual write veto.
use super::*;
use crate::rewrite::{Kind, Plan};

impl GitScreen {
    pub(super) fn rewrite_key(&mut self, key: &KeyEvent) -> bool {
        if self.page != 0 || key.modifiers.control || key.modifiers.alt {
            return false;
        }
        let kind = match key.code {
            KeyCode::Char('A') => Kind::Amend,
            KeyCode::Char('W') => Kind::Reword,
            KeyCode::Char('F')
                if self.last_list == 2
                    && (self.focus == 2 || self.focus == 4 || self.commit_open)
                    && self.history_view == 0 =>
            {
                let target = self.selected_row().and_then(|row| {
                    row.value
                        .to_str()
                        .and_then(|id| git2::Oid::from_str(id).ok())
                });
                let Some(target) = target else {
                    self.notice = "Select a history commit for the fixup".into();
                    return true;
                };
                Kind::Fixup(target)
            }
            _ => return false,
        };
        let result = self.rewrite_form(kind);
        self.report(result);
        true
    }

    fn rewrite_form(&mut self, kind: Kind) -> Result<(), String> {
        self.allow_write()?;
        let history_branch = if self.last_list == 2 && self.history_view == 0 {
            Some(crate::history::history_branch(
                &self.root,
                self.history_reference()?,
            )?)
        } else {
            None
        };
        let plan = Plan::capture(&self.root, kind)?;
        if history_branch.is_some() && plan.reference != history_branch {
            return Err("The checked-out branch changed. Close this and open it again.".into());
        }
        let title = match kind {
            Kind::Amend => "Amend HEAD",
            Kind::Reword => "Reword HEAD",
            Kind::Fixup(_) => "Fixup",
        };
        let mut message = Field::new("Message", plan.message.clone());
        message.multiline = true;
        // Git's fixup convention must keep the selected target's subject intact.
        message.read_only = matches!(kind, Kind::Fixup(_));
        let mut fields = vec![message];
        let mut context = vec![(
            if plan.reference.is_some() {
                "Branch"
            } else {
                "Checkout"
            },
            plan.reference
                .as_deref()
                .map(|name| name.trim_start_matches("refs/heads/"))
                .unwrap_or("detached HEAD")
                .to_owned(),
        )];
        if kind != Kind::Reword {
            context.push(("Source", "Index".into()));
        }
        if let Kind::Fixup(target) = kind {
            context.push(("Target", target.to_string()[..8].to_owned()));
        }
        for (label, value) in context {
            let mut field = Field::new(label, value);
            field.read_only = true;
            field.input.cursor = 0;
            fields.push(field);
        }
        let selected = if matches!(kind, Kind::Fixup(_)) {
            fields.len()
        } else {
            0
        };
        self.form = Some(Form {
            title: format!("{title} · {}", &plan.head.to_string()[..8]),
            action: FormAction::Rewrite(Box::new(plan)),
            fields,
            selected,
        });
        self.notice.clear();
        Ok(())
    }

    pub(super) fn poll_rewrite(&mut self) -> bool {
        let Some(result) = self.rewrite.poll() else {
            return false;
        };
        self.accept_rewrite_result(result);
        let refreshed = self.refresh();
        self.finish_rewrite_refresh(refreshed);
        true
    }

    fn accept_rewrite_result(&mut self, result: Result<crate::rewrite::Outcome, String>) {
        self.busy = false;
        self.finish_reflog_form(result.is_ok());
        self.rewrite_diagnostic = result
            .as_ref()
            .err()
            .map(|error| error.replace(['\n', '\r'], " "));
        self.notice = match result {
            Ok(outcome) => format!(
                "{} {}",
                match outcome.kind {
                    Kind::Amend => "Amended",
                    Kind::Reword => "Reworded",
                    Kind::Fixup(_) => "Fixup",
                },
                &outcome.head.to_string()[..8]
            ),
            Err(error) => error.replace(['\n', '\r'], " "),
        };
    }

    fn finish_rewrite_refresh(&mut self, refreshed: Result<(), String>) {
        self.refreshing_after_write = refreshed.is_ok();
        self.report(refreshed);
    }

    pub(super) fn restore_rewrite_diagnostic(&mut self) {
        if let Some(diagnostic) = &self.rewrite_diagnostic {
            self.notice.clone_from(diagnostic);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_with_draft() -> (tempfile::TempDir, GitScreen, git2::Oid) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let mut config = repo.config().unwrap();
        config
            .set_str("user.name", "Rewrite diagnostic test")
            .unwrap();
        config
            .set_str("user.email", "diagnostic@example.invalid")
            .unwrap();
        config.set_bool("commit.gpgsign", false).unwrap();
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Diagnostic", "diagnostic@example.invalid").unwrap();
        let head = repo
            .commit(Some("HEAD"), &signature, &signature, "Original", &tree, &[])
            .unwrap();
        let mut screen = GitScreen::open(directory.path()).unwrap();
        screen.rewrite_form(Kind::Reword).unwrap();
        let field = &mut screen.form.as_mut().unwrap().fields[0];
        field.input.input = "Retained rewrite draft".into();
        field.input.cursor = field.input.input.len();
        screen.busy = true;
        screen.reflog_submitting = true;
        (directory, screen, head)
    }

    fn ambiguous_publication(head: git2::Oid) -> String {
        format!(
            "Cannot publish commit: injected I/O error; inspect HEAD and refs/dusty/rewrite-result/{}; original refs/dusty/rewrite/{head}",
            "1".repeat(40)
        )
    }

    fn assert_retained(screen: &GitScreen, diagnostic: &str, head: git2::Oid) {
        assert_eq!(screen.notice, diagnostic);
        assert_eq!(screen.rewrite_diagnostic.as_deref(), Some(diagnostic));
        assert!(!screen.busy && !screen.reflog_submitting);
        let form = screen
            .form
            .as_ref()
            .expect("failed rewrite draft is retained");
        assert_eq!(form.fields[0].input.input, "Retained rewrite draft");
        let FormAction::Rewrite(plan) = &form.action else {
            panic!("rewrite identity replaced")
        };
        assert_eq!(plan.head, head);
    }

    #[test]
    fn rewrite_failure_survives_immediate_refresh_error_until_escape_acknowledges() {
        let (_directory, mut screen, head) = screen_with_draft();
        let diagnostic = ambiguous_publication(head);
        // Inject at the worker-result/refresh-result boundaries used by poll_rewrite.
        screen.accept_rewrite_result(Err(diagnostic.clone()));
        screen.finish_rewrite_refresh(Err("injected refresh startup failure".into()));
        assert_retained(&screen, &diagnostic, head);
        screen.report(Err("another immediate read error".into()));
        screen.notice = "a later read tried to replace the warning".into();
        let _ = screen.frame(120, 40);
        assert_retained(&screen, &diagnostic, head);
        screen.form_key(&KeyEvent::plain(KeyCode::Escape)).unwrap();
        assert!(screen.form.is_none() && screen.rewrite_diagnostic.is_none());
        assert!(screen.notice.is_empty());
    }

    #[test]
    fn rewrite_failure_survives_real_asynchronous_status_and_branch_errors() {
        let (_directory, mut screen, head) = screen_with_draft();
        let diagnostic = ambiguous_publication(head);
        screen.accept_rewrite_result(Err(diagnostic.clone()));
        let not_a_repository = tempfile::tempdir().unwrap();
        // Stop repository discovery even when the test's TMPDIR is inside a checkout.
        std::fs::write(
            not_a_repository.path().join(".git"),
            b"gitdir: missing-git-directory\n",
        )
        .unwrap();
        screen.root = not_a_repository.path().to_path_buf();
        let refreshed = screen.refresh();
        assert!(
            refreshed.is_ok(),
            "test must exercise asynchronous failures: {refreshed:?}"
        );
        screen.finish_rewrite_refresh(refreshed);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while screen.refreshing || screen.refs_loading {
            screen.poll();
            assert_retained(&screen, &diagnostic, head);
            assert!(
                std::time::Instant::now() < deadline,
                "follow-up reads did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // Other read jobs can finish after status/ref readiness; frame must retain it too.
        let _ = screen.frame(120, 40);
        assert_retained(&screen, &diagnostic, head);
        assert!(!screen.busy());
    }

    #[test]
    fn rewrite_success_and_hook_rejection_clear_submission_ownership() {
        let (_directory, mut screen, head) = screen_with_draft();
        screen.accept_rewrite_result(Err("commit-msg hook rejected message".into()));
        assert_retained(&screen, "commit-msg hook rejected message", head);
        // A subsequent accepted operation owns the worker and replaces the old result.
        screen.busy = true;
        screen.reflog_submitting = true;
        screen.accept_rewrite_result(Ok(crate::rewrite::Outcome {
            kind: Kind::Reword,
            head,
            recovery: format!("refs/dusty/rewrite/{head}"),
        }));
        assert!(!screen.busy && !screen.reflog_submitting);
        assert!(screen.form.is_none() && screen.rewrite_diagnostic.is_none());
        assert_eq!(
            screen.notice,
            format!("Reworded {}", &head.to_string()[..8])
        );
    }
}
