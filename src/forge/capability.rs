//! What a forge can do and what it calls things (FORGE.md §4). A `Cap::No(reason)` action stays
//! visible but muted; pressing it shows the reason instead of sending a request.

use crate::forge::model::MergeMethod;
use crate::forge::provider::ProviderKind;
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum Cap {
    Yes,
    No(&'static str),
    /// Acts as `Yes` until a 404 flips it to `No` for that host and version (F2).
    Probe,
}

impl Cap {
    pub fn allowed(&self) -> bool {
        !matches!(self, Cap::No(_))
    }

    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Cap::No(reason) => Some(reason),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum StepsKind {
    Known,
    FromLogSections,
    Unavailable,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RunFilters {
    pub workflow: bool,
    pub branch: bool,
    pub event: bool,
    pub status: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Capabilities {
    pub pull_requests: Cap,
    pub issues: Cap,
    pub actions: Cap,
    pub notifications: Cap,
    pub cross_repo_search: Cap,
    pub review_threads: Cap,
    pub resolve_thread: Cap,
    pub reply_in_thread: Cap,
    pub inline_comment: Cap,
    pub batched_review: Cap,
    pub review_approve: Cap,
    pub review_request_changes: Cap,
    pub viewed_files: Cap,
    pub merge_methods: Vec<MergeMethod>,
    pub merge_pinned_head: Cap,
    pub delete_branch_on_merge: Cap,
    pub auto_merge: Cap,
    pub draft_pr: Cap,
    pub create_pr_cross_fork: Cap,
    pub update_branch: Cap,
    pub issue_not_planned: Cap,
    pub labels: Cap,
    pub assignees: Cap,
    pub milestones: Cap,
    pub reviewers: Cap,
    pub projects: Cap,
    pub subscribe: Cap,
    pub linked_issues: Cap,
    pub runs_list: Cap,
    pub run_jobs: Cap,
    pub job_steps: StepsKind,
    pub job_log: Cap,
    pub rerun_all: Cap,
    pub rerun_failed: Cap,
    pub rerun_job: Cap,
    pub cancel_run: Cap,
    pub dispatch: Cap,
    pub dispatch_inputs_schema: Cap,
    pub approve_fork_run: Cap,
    pub attempts: Cap,
    pub artifacts_list: Cap,
    pub artifacts_download: Cap,
    pub run_filters: RunFilters,
    pub etag: Cap,
    pub since_queries: Cap,
    pub graphql_batching: Cap,
    /// Any write at all (comment, edit, create, close/reopen, merge, rerun). `No` while the
    /// host is browsed anonymously; every field above that names a write is `No` then too.
    pub writes: Cap,
}

impl Capabilities {
    /// GitHub.com and GHES (§4.2): everything the UI offers today.
    pub fn github(graphql_batching: bool) -> Self {
        Self {
            pull_requests: Cap::Yes,
            issues: Cap::Yes,
            actions: Cap::Yes,
            notifications: Cap::Yes,
            cross_repo_search: Cap::Yes,
            review_threads: Cap::Yes,
            resolve_thread: Cap::Yes,
            reply_in_thread: Cap::Yes,
            inline_comment: Cap::Yes,
            batched_review: Cap::Yes,
            review_approve: Cap::Yes,
            review_request_changes: Cap::Yes,
            viewed_files: Cap::Yes,
            merge_methods: vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
            merge_pinned_head: Cap::Yes,
            delete_branch_on_merge: Cap::Yes,
            auto_merge: Cap::Yes,
            draft_pr: Cap::Yes,
            create_pr_cross_fork: Cap::Yes,
            update_branch: Cap::Yes,
            issue_not_planned: Cap::Yes,
            labels: Cap::Yes,
            assignees: Cap::Yes,
            milestones: Cap::Yes,
            reviewers: Cap::Yes,
            projects: Cap::Yes,
            subscribe: Cap::Yes,
            linked_issues: Cap::Yes,
            runs_list: Cap::Yes,
            run_jobs: Cap::Yes,
            job_steps: StepsKind::Known,
            job_log: Cap::Yes,
            rerun_all: Cap::Yes,
            rerun_failed: Cap::Yes,
            rerun_job: Cap::Yes,
            cancel_run: Cap::Yes,
            dispatch: Cap::Yes,
            dispatch_inputs_schema: Cap::Yes,
            approve_fork_run: Cap::Yes,
            attempts: Cap::Yes,
            artifacts_list: Cap::Yes,
            artifacts_download: Cap::Yes,
            run_filters: RunFilters {
                workflow: true,
                branch: true,
                event: true,
                status: true,
            },
            etag: Cap::Yes,
            since_queries: Cap::Yes,
            graphql_batching: if graphql_batching {
                Cap::Yes
            } else {
                Cap::No("via tea")
            },
            writes: Cap::Yes,
        }
    }

    /// The same forge browsed without a credential (FORGE.md §2.5): reads stay, every write
    /// becomes `Cap::No("Sign in to …")` and no merge method is offered. Reads that need a
    /// sign-in on this provider (notifications; GitHub job logs, viewed files, artifact
    /// downloads, GraphQL) are `No` as well.
    pub fn read_only(mut self, kind: ProviderKind) -> Self {
        const WRITE: Cap = Cap::No("Sign in to make changes");
        const REVIEW: Cap = Cap::No("Sign in to review");
        const COMMENT: Cap = Cap::No("Sign in to comment");
        const MERGE: Cap = Cap::No("Sign in to merge");
        const EDIT: Cap = Cap::No("Sign in to edit");
        const RUN: Cap = Cap::No("Sign in to run or cancel workflows");
        self.writes = WRITE;
        self.notifications = Cap::No("Sign in to see notifications");
        self.resolve_thread = match self.resolve_thread {
            Cap::No(reason) => Cap::No(reason),
            _ => REVIEW,
        };
        self.reply_in_thread = COMMENT;
        self.inline_comment = COMMENT;
        self.batched_review = REVIEW;
        self.review_approve = REVIEW;
        self.review_request_changes = REVIEW;
        self.merge_methods = Vec::new();
        self.merge_pinned_head = MERGE;
        self.delete_branch_on_merge = MERGE;
        self.auto_merge = MERGE;
        self.draft_pr = Cap::No("Sign in to open pull requests");
        self.create_pr_cross_fork = Cap::No("Sign in to open pull requests");
        self.update_branch = EDIT;
        self.issue_not_planned = match self.issue_not_planned {
            Cap::No(reason) => Cap::No(reason),
            _ => EDIT,
        };
        self.labels = EDIT;
        self.assignees = EDIT;
        self.milestones = EDIT;
        self.reviewers = EDIT;
        self.projects = match self.projects {
            Cap::No(reason) => Cap::No(reason),
            _ => EDIT,
        };
        self.subscribe = Cap::No("Sign in to subscribe");
        self.rerun_all = RUN;
        self.rerun_failed = RUN;
        self.rerun_job = RUN;
        self.cancel_run = RUN;
        self.dispatch = RUN;
        self.approve_fork_run = RUN;
        if kind == ProviderKind::GitHub {
            self.job_log = Cap::No("Sign in to read logs");
            self.viewed_files = Cap::No("Sign in to mark files viewed");
            self.artifacts_download = Cap::No("Sign in to download artifacts");
            self.linked_issues = Cap::No("Sign in to see linked issues");
            self.graphql_batching = Cap::No("GitHub GraphQL needs a sign-in");
            self.cross_repo_search = Cap::No("Sign in to search across repositories");
        }
        if kind == ProviderKind::GitLab {
            self.job_log = Cap::No("Sign in to read logs");
        }
        self
    }
}

/// What the forge calls things (§4.3); the UI never hard-codes "pull request".
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Terms {
    pub pr_singular: &'static str,
    pub pr_plural: &'static str,
    pub pr_glyph: &'static str,
    pub actions_tab: &'static str,
    pub run_noun: &'static str,
    pub workflow_noun: &'static str,
    pub approve_verb: &'static str,
    pub notifications_tab: &'static str,
}

pub static GITHUB_TERMS: Terms = Terms {
    pr_singular: "pull request",
    pr_plural: "Pull requests",
    pr_glyph: "#",
    actions_tab: "Actions",
    run_noun: "run",
    workflow_noun: "workflow",
    approve_verb: "Approve",
    notifications_tab: "Notifications",
};

pub static GITLAB_TERMS: Terms = Terms {
    pr_singular: "merge request",
    pr_plural: "Merge requests",
    pr_glyph: "!",
    actions_tab: "Pipelines",
    run_noun: "pipeline",
    workflow_noun: "pipeline",
    approve_verb: "Approve",
    notifications_tab: "To-dos",
};

pub static GITEA_TERMS: Terms = Terms {
    pr_singular: "pull request",
    pr_plural: "Pull requests",
    pr_glyph: "#",
    actions_tab: "Actions",
    run_noun: "run",
    workflow_noun: "workflow",
    approve_verb: "Approve",
    notifications_tab: "Notifications",
};

/// The words for a provider; GitHub's when the kind is not known yet.
pub fn terms_for(kind: Option<crate::forge::provider::ProviderKind>) -> &'static Terms {
    use crate::forge::provider::ProviderKind;
    match kind {
        Some(ProviderKind::GitLab) => &GITLAB_TERMS,
        Some(ProviderKind::Gitea | ProviderKind::Forgejo) => &GITEA_TERMS,
        Some(ProviderKind::GitHub) | None => &GITHUB_TERMS,
    }
}
