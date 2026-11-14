#![cfg(unix)]
//! The Gitea/Forgejo provider contract (FORGE.md §8.3, F4): one scenario suite, run for Gitea
//! (gitea.com, main) and Forgejo 17 (code.forgejo.org) over direct HTTP (`Authorization: token`)
//! and `tea api --include` passthrough (`tests/fixtures/forge/fake_tea.py`), against
//! `server.py --world gitea|forgejo`, which answers from the recorded fixtures. Test names are
//! `<family>_<transport>::<scenario>`; `<family>_transports_agree` checks that both transports
//! decode to the same domain values from the same logical requests. The IDE picker test proves
//! Codeberg pull requests list through the provider (FORGE.md §5.2).

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dust::forge::capability::Cap;
use dust::forge::capability::StepsKind;
use dust::forge::cli::CliTransport;
use dust::forge::credential::Credential;
use dust::forge::credential::CredentialSource;
use dust::forge::credential::Env;
use dust::forge::credential::Secret;
use dust::forge::detect;
use dust::forge::http::HttpTransport;
use dust::forge::model::*;
use dust::forge::provider;
use dust::forge::provider::Conditional;
use dust::forge::provider::Fetched;
use dust::forge::provider::ForgeContext;
use dust::forge::provider::Provider;
use dust::forge::provider::ProviderKind;
use dust::forge::provider::RepoRef;
use dust::forge::request::*;
use dust::forge::transport::Cli;
use dust::forge::transport::ConnectOptions;
use dust::forge::transport::Endpoints;
use dust::forge::transport::HostKey;
use dust::forge::transport::Transport;
use forge_fake::FAKE_TOKEN;
use forge_fake::ForgeServer;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

const HEAD: &str = "c92ad11000000000000000000000000000000000";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family {
    Gitea,
    Forgejo,
}

impl Family {
    fn world(self) -> &'static str {
        match self {
            Family::Gitea => "gitea",
            Family::Forgejo => "forgejo",
        }
    }

    fn host(self) -> &'static str {
        match self {
            Family::Gitea => "gitea.com",
            Family::Forgejo => "code.forgejo.org",
        }
    }

    fn version(self) -> &'static str {
        match self {
            Family::Gitea => "29.0.0+dev-160-g55186ca39b",
            Family::Forgejo => "17.0.0-26-363f3dc30b+gitea-1.22.0",
        }
    }

    fn kind(self) -> ProviderKind {
        match self {
            Family::Gitea => ProviderKind::Gitea,
            Family::Forgejo => ProviderKind::Forgejo,
        }
    }

    /// The recorded pull request and issue.
    fn pull(self) -> u64 {
        match self {
            Family::Gitea => 1402,
            Family::Forgejo => 1784,
        }
    }

    fn issue(self) -> u64 {
        match self {
            Family::Gitea => 1393,
            Family::Forgejo => 1792,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Via {
    Http,
    Tea,
}

pub struct Harness {
    directory: tempfile::TempDir,
    server: ForgeServer,
    tea: PathBuf,
    family: Family,
    via: Via,
}

macro_rules! expect {
    ($value:expr, $variant:path) => {
        match $value {
            $variant(inner) => inner,
            other => panic!("expected {}: {other:?}", stringify!($variant)),
        }
    };
}

fn probes(host: &str, version: &str) -> Value {
    json!({ host: { "api/v1/version": { "body": { "version": version } } } })
}

impl Harness {
    pub fn new(family: Family, via: Via) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let state = json!({ "probes": probes(family.host(), family.version()) });
        std::fs::write(&path, state.to_string()).unwrap();
        let server = ForgeServer::start(&path, family.world());
        let tea = server.install_tea(
            &directory.path().join("bin"),
            json!([{"name": "work", "url": format!("https://{}", family.host()),
                    "user": "maya", "default": true}]),
        );
        Self {
            directory,
            server,
            tea,
            family,
            via,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    fn update(&self, change: impl FnOnce(&mut Value)) {
        let mut state = forge_fake::state(&self.state_path());
        change(&mut state);
        std::fs::write(self.state_path(), state.to_string()).unwrap();
    }

    fn requests(&self) -> Vec<Value> {
        forge_fake::requests(&self.state_path())
    }

    fn clear_requests(&self) {
        self.update(|state| state["requests"] = json!([]));
    }

    /// `(method, path, sorted query)` of every request since the last clear, transport aside.
    fn logical(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| {
                let mut query = request["query"]
                    .as_object()
                    .map(|query| {
                        query
                            .iter()
                            .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or("")))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                query.sort();
                format!(
                    "{} {}?{}",
                    request["method"].as_str().unwrap_or(""),
                    request["path"].as_str().unwrap_or(""),
                    query.join("&")
                )
            })
            .collect()
    }

    fn paths(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| request["path"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    fn endpoints(&self) -> Endpoints {
        let host = self.family.host();
        detect::endpoints_for(
            self.family.kind(),
            &HostKey::new(host),
            Some(&format!("{}/{host}/api/v1", self.server.url())),
            &Env::from_pairs::<&str, &str>([]),
        )
    }

    fn transport(&self) -> Arc<dyn Transport> {
        match self.via {
            Via::Http => Arc::new(
                HttpTransport::new(
                    self.endpoints(),
                    Credential {
                        token: Secret::new(FAKE_TOKEN),
                        source: CredentialSource::Env("GITEA_TOKEN"),
                        user: None,
                        obtained: Instant::now(),
                    },
                )
                .unwrap(),
            ),
            Via::Tea => Arc::new(CliTransport::new(
                Cli::Tea,
                self.tea.clone(),
                Some("work".into()),
            )),
        }
    }

    fn ctx(&self) -> ForgeContext {
        ForgeContext::new(
            self.directory.path().to_path_buf(),
            self.endpoints(),
            ConnectOptions {
                tea: Some(self.tea.clone()),
                ..ConnectOptions::default()
            },
            RepoRef::parse("orbit/orbit"),
            self.transport(),
        )
    }

    fn provider(&self) -> Arc<dyn Provider> {
        provider::build(self.family.kind()).unwrap()
    }

    fn fetch(&self, read: Read, cond: &Conditional) -> Result<Fetched, ForgeError> {
        self.provider()
            .read(&self.ctx(), &read, cond, &AtomicBool::new(false))
    }

    fn read(&self, read: Read) -> Result<Payload, ForgeError> {
        match self.fetch(read.clone(), &Conditional::default())? {
            Fetched::Payload { value, .. } => Ok(value),
            Fetched::NotModified => panic!("{read:?} came back not modified"),
        }
    }

    fn write(&self, write: Write) -> Result<Receipt, ForgeError> {
        self.provider()
            .write(&self.ctx(), &write, &AtomicBool::new(false))
    }

    /// Learn the server version so later gates decide without a request.
    fn warm(&self) {
        let _ = self.provider().capabilities(&self.ctx());
        self.clear_requests();
    }

    fn assert_transport(&self) {
        let requests = self.requests();
        assert!(!requests.is_empty());
        for request in &requests {
            match self.via {
                Via::Http => {
                    assert_eq!(request["transport"], "http", "{request}");
                    if request["path"] != "version" {
                        assert_eq!(request["auth"], "valid", "{request}");
                    }
                }
                Via::Tea => {
                    assert_eq!(request["transport"], "tea", "{request}");
                    let argv = request["gh_argv"].as_array().unwrap();
                    let words = argv
                        .iter()
                        .map(|word| word.as_str().unwrap())
                        .collect::<Vec<_>>();
                    assert_eq!(words[0], "api");
                    assert!(words.contains(&"--include"), "{words:?}");
                    assert!(
                        words.windows(2).any(|pair| pair == ["--login", "work"]),
                        "{words:?}"
                    );
                }
            }
        }
    }
}

fn list(kind: ItemKind, query: &str, per_page: u32) -> Read {
    Read::List {
        kind,
        filter: Filter {
            state: StateFilter::Open,
            query: query.into(),
        },
        page: Page::first(per_page),
    }
}

// ---------------------------------------------------------------------------- scenarios

pub fn rows_and_terms(harness: &Harness) {
    let provider = harness.provider();
    assert_eq!(provider.terms().pr_plural, "Pull requests");
    let pulls = expect!(
        harness.read(list(ItemKind::PullRequest, "", 20)).unwrap(),
        Payload::Rows
    );
    assert!(!pulls.items.is_empty());
    assert!(
        pulls
            .items
            .iter()
            .all(|row| row.kind == Some(ItemKind::PullRequest))
    );
    let first = &pulls.items[0];
    match harness.family {
        Family::Gitea => {
            assert_eq!(pulls.total, Some(4));
            assert_eq!(pulls.items.len(), 4);
            assert_eq!(first.number, 1402);
            // `WIP:` is Gitea's draft.
            assert!(first.draft);
            assert_eq!(first.lifecycle, Lifecycle::Draft);
            assert_eq!(first.repo.as_deref(), Some("orbit/orbit"));
        }
        Family::Forgejo => {
            assert_eq!(pulls.total, Some(17));
            assert_eq!(first.number, 1793);
            assert!(!first.draft);
        }
    }
    assert!(first.web_url.starts_with("https://"));
    assert!(!first.author.login.is_empty());
    let paths = harness.paths();
    assert!(
        paths.contains(&"repos/orbit/orbit/pulls".to_owned()),
        "{paths:?}"
    );
    let issues = expect!(
        harness.read(list(ItemKind::Issue, "", 10)).unwrap(),
        Payload::Rows
    );
    assert!(
        issues
            .items
            .iter()
            .all(|row| row.kind == Some(ItemKind::Issue))
    );
    assert_eq!(
        issues.items[0].number,
        match harness.family {
            Family::Gitea => 1411,
            Family::Forgejo => 1792,
        }
    );
    harness.assert_transport();
}

pub fn query_translation(harness: &Harness) {
    harness.clear_requests();
    // Not recorded: only the request matters.
    let _ = harness.read(list(ItemKind::PullRequest, "label:bug author:maya fix", 20));
    let request = harness.requests().pop().unwrap();
    assert_eq!(request["path"], "repos/orbit/orbit/issues");
    assert_eq!(request["query"]["type"], "pulls");
    assert_eq!(request["query"]["labels"], "bug");
    assert_eq!(request["query"]["created_by"], "maya");
    assert_eq!(request["query"]["q"], "fix");
    assert_eq!(request["query"]["state"], "open");
    let found = expect!(
        harness
            .read(Read::Search {
                kind: ItemKind::PullRequest,
                filter: Filter {
                    state: StateFilter::Open,
                    query: "org:orbit".into(),
                },
                page: Page::first(10),
            })
            .unwrap(),
        Payload::Rows
    );
    assert_eq!(found.items.len(), 10);
    assert!(found.items.iter().all(|row| row.repo.is_some()));
    let request = harness.requests().pop().unwrap();
    assert_eq!(request["path"], "repos/issues/search");
    assert_eq!(request["query"]["owner"], "orbit");
    assert_eq!(request["query"]["type"], "pulls");
}

pub fn pull_request_detail(harness: &Harness) {
    let pull = *expect!(
        harness
            .read(Read::PullRequest(harness.family.pull()))
            .unwrap(),
        Payload::PullRequest
    );
    assert_eq!(pull.row.number, harness.family.pull());
    assert_eq!(pull.fingerprint.head, pull.head.sha);
    assert_eq!(pull.pinned_head, pull.head.sha);
    assert!(!pull.checks.is_empty());
    for check in &pull.checks {
        assert!(
            check
                .raw_state
                .as_deref()
                .is_some_and(|state| state.chars().all(|letter| letter.is_ascii_uppercase()))
        );
        let url = check.details_url.as_deref().unwrap();
        assert!(
            url.starts_with(&format!(
                "https://{}/orbit/orbit/actions/runs/",
                harness.family.host()
            )),
            "{url}"
        );
        assert!(check.run_link.is_some());
    }
    assert!(pull.files.iter().all(|file| file.viewed.is_some()));
    match harness.family {
        Family::Gitea => {
            assert_eq!(pull.head.branch, "enhance/stable-host-workdir");
            assert_eq!(pull.files.len(), 10);
            assert_eq!(pull.commits.len(), 5);
            assert_eq!(pull.checks.len(), 5);
            // The combined status: four jobs passed, the title lint was skipped.
            let states: Vec<CiState> = pull.checks.iter().map(|check| check.state).collect();
            assert_eq!(
                states
                    .iter()
                    .filter(|state| **state == CiState::Passed)
                    .count(),
                4
            );
            assert_eq!(
                states
                    .iter()
                    .filter(|state| **state == CiState::Skipped)
                    .count(),
                1
            );
            assert_eq!(pull.row.ci, Some(CiState::Passed));
            assert_eq!(pull.mergeable, Mergeable::Draft);
            assert_eq!(
                pull.checks[0].run_link.as_ref().unwrap().run,
                ItemId::new("1204760")
            );
            assert!(pull.reviewers.iter().any(|slot| slot.reviewer == "leo"));
        }
        Family::Forgejo => {
            assert_eq!(pull.head.branch, "docker-socket");
            assert_eq!(pull.files.len(), 1);
            assert_eq!(pull.commits.len(), 1);
            assert_eq!(pull.mergeable, Mergeable::Clean);
            assert!(
                pull.reviews
                    .iter()
                    .all(|review| review.state == ReviewState::Commented)
            );
        }
    }
    // The fingerprint read confirms nothing changed.
    let fingerprint = expect!(
        harness
            .read(Read::Fingerprint(harness.family.pull()))
            .unwrap(),
        Payload::Fingerprint
    );
    assert_eq!(fingerprint, pull.fingerprint);
    let again = harness
        .fetch(
            Read::Fingerprint(harness.family.pull()),
            &Conditional {
                fingerprint: Some(fingerprint),
                ..Conditional::default()
            },
        )
        .unwrap();
    assert_eq!(again, Fetched::NotModified);
    harness.assert_transport();
}

pub fn issue_timeline_and_threads(harness: &Harness) {
    let issue = *expect!(
        harness.read(Read::Issue(harness.family.issue())).unwrap(),
        Payload::Issue
    );
    assert_eq!(issue.row.number, harness.family.issue());
    assert_eq!(issue.row.kind, Some(ItemKind::Issue));
    assert!(!issue.body.is_empty());
    let timeline = expect!(
        harness.read(Read::Timeline(harness.family.pull())).unwrap(),
        Payload::Timeline
    );
    assert!(
        timeline
            .iter()
            .any(|event| matches!(event, TimelineEvent::Renamed { .. }))
    );
    let reviewed = timeline
        .iter()
        .filter_map(|event| match event {
            TimelineEvent::Reviewed(review) => Some(review.state),
            _ => None,
        })
        .collect::<Vec<_>>();
    let threads = expect!(
        harness.read(Read::Threads(harness.family.pull())).unwrap(),
        Payload::Threads
    );
    assert!(threads.iter().all(|thread| !thread.resolvable));
    match harness.family {
        Family::Gitea => {
            assert_eq!(reviewed, [ReviewState::ChangesRequested]);
            assert!(
                timeline
                    .iter()
                    .any(|event| matches!(event, TimelineEvent::Commented(_)))
            );
            assert!(threads.is_empty(), "{threads:?}");
        }
        Family::Forgejo => {
            assert_eq!(reviewed, [ReviewState::Commented, ReviewState::Commented]);
            assert!(timeline.iter().any(|event| matches!(
                event,
                TimelineEvent::Labeled { .. } | TimelineEvent::Unlabeled { .. }
            )));
            assert_eq!(threads.len(), 1);
            let thread = &threads[0];
            assert_eq!(thread.path.as_deref(), Some("internal/app/cmd/exec.go"));
            assert_eq!(thread.line, Some(384));
            assert_eq!(thread.side, Side::Right);
            assert_eq!(thread.resolved, Some(false));
            assert!(thread.id.0.starts_with("R384@"), "{}", thread.id);
        }
    }
}

pub fn diffs(harness: &Harness) {
    let diff = expect!(
        harness.read(Read::Diff(harness.family.pull())).unwrap(),
        Payload::Text
    );
    assert!(diff.starts_with("diff --git"), "{diff:.80}");
    let pull = *expect!(
        harness
            .read(Read::PullRequest(harness.family.pull()))
            .unwrap(),
        Payload::PullRequest
    );
    let commit = expect!(
        harness
            .read(Read::CommitDiff(pull.head.sha.clone()))
            .unwrap(),
        Payload::Text
    );
    assert!(
        commit.starts_with("diff --git") || commit.starts_with("From "),
        "{commit:.80}"
    );
}

pub fn repository_and_branch_lookup(harness: &Harness) {
    let repo = expect!(harness.read(Read::Repository).unwrap(), Payload::Repo);
    assert_eq!(repo.full_name, "orbit/orbit");
    assert_eq!(repo.default_branch, "main");
    assert!(repo.has_ci);
    match harness.family {
        Family::Gitea => {
            assert_eq!(repo.merge_methods, [MergeMethod::Squash]);
            assert_eq!(repo.default_merge_method, Some(MergeMethod::Merge));
            let found = harness
                .read(Read::PullRequestForBranch(
                    "enhance/macos-vm-executor".into(),
                ))
                .unwrap();
            assert_eq!(found, Payload::MaybePullRequest(Some(1210)));
            // 404 on the direct lookup, then the open list: the PR's head lives in a fork.
            let fork = harness
                .read(Read::PullRequestForBranch(
                    "enhance/stable-host-workdir".into(),
                ))
                .unwrap();
            assert_eq!(fork, Payload::MaybePullRequest(None));
        }
        Family::Forgejo => {
            assert_eq!(repo.default_merge_method, Some(MergeMethod::Squash));
            assert!(repo.merge_methods.contains(&MergeMethod::RebaseMerge));
            let fork = harness
                .read(Read::PullRequestForBranch("docker-socket".into()))
                .unwrap();
            assert_eq!(fork, Payload::MaybePullRequest(None));
        }
    }
    let viewer = expect!(harness.read(Read::Viewer).unwrap(), Payload::User);
    assert_eq!(viewer.login, "maya");
}

pub fn notifications(harness: &Harness) {
    let page = expect!(
        harness
            .read(Read::Notifications {
                filter: NotifyFilter::default(),
                page: Page::first(50),
            })
            .unwrap(),
        Payload::Notifications
    );
    assert_eq!(page.items.len(), 1);
    let notification = &page.items[0];
    assert_eq!(notification.id, ItemId::new("9001"));
    assert!(notification.unread);
    assert_eq!(
        notification.target,
        NotifyTarget::Item {
            kind: ItemKind::PullRequest,
            number: 13
        }
    );
    assert_eq!(notification.reason, NotifyReason::Subscribed);
    let request = harness.requests().pop().unwrap();
    assert_eq!(request["path"], "notifications");
    assert_eq!(request["query"]["status-types"], "unread");
    harness.write(Write::MarkRead(ItemId::new("9001"))).unwrap();
    let request = harness.requests().pop().unwrap();
    assert_eq!(request["method"], "PATCH");
    assert_eq!(request["path"], "notifications/threads/9001");
    assert_eq!(request["query"]["to-status"], "read");
}

pub fn actions(harness: &Harness) {
    let runs = expect!(
        harness
            .read(Read::Runs {
                filter: RunFilter::default(),
                page: Page::first(20),
            })
            .unwrap(),
        Payload::Runs
    );
    assert!(!runs.items.is_empty());
    match harness.family {
        Family::Gitea => {
            let workflows = expect!(harness.read(Read::Workflows).unwrap(), Payload::Workflows);
            assert_eq!(workflows.len(), 1);
            let run = &runs.items[0];
            assert_eq!(run.id, ItemId::new("91"));
            assert_eq!(run.number, 12);
            assert_eq!(run.conclusion, Some(CiState::Failed));
            assert_eq!(run.workflow.as_ref().unwrap().id, ItemId::new("ci.yml"));
            let detail = *expect!(
                harness
                    .read(Read::Run {
                        id: ItemId::new("91"),
                        attempt: None,
                    })
                    .unwrap(),
                Payload::Run
            );
            assert_eq!(detail.jobs.len(), 1);
            assert_eq!(detail.jobs[0].id, ItemId::new("42"));
            assert!(matches!(detail.jobs[0].steps, Steps::Known(_)));
            assert!(detail.failure_summary.is_some());
            let job = *expect!(
                harness.read(Read::Job(ItemId::new("42"))).unwrap(),
                Payload::Job
            );
            assert_eq!(job.name, "test");
            let log = expect!(
                harness.read(Read::JobLog(ItemId::new("42"))).unwrap(),
                Payload::Log
            );
            assert!(!log.text.is_empty());
            let artifacts = expect!(
                harness.read(Read::Artifacts(ItemId::new("91"))).unwrap(),
                Payload::Artifacts
            );
            assert_eq!(artifacts.len(), 1);
        }
        Family::Forgejo => {
            harness.clear_requests();
            // No workflow list on Forgejo: empty, and nothing asked.
            let workflows = expect!(harness.read(Read::Workflows).unwrap(), Payload::Workflows);
            assert!(workflows.is_empty());
            assert!(
                !harness
                    .paths()
                    .iter()
                    .any(|path| path.contains("workflows")),
                "{:?}",
                harness.paths()
            );
            let run = &runs.items[0];
            assert_eq!(run.id, ItemId::new("200481"));
            assert_eq!(run.number, 24108);
            assert_eq!(run.conclusion, Some(CiState::Failed));
            assert_eq!(run.conclusion_raw.as_deref(), Some("failure"));
            assert_eq!(run.event, "schedule");
            assert_eq!(
                run.workflow.as_ref().unwrap().id,
                ItemId::new("test-multi-platform.yml")
            );
            assert!(runs.items.iter().any(|run| run.pr_number.is_some()));
            let detail = *expect!(
                harness
                    .read(Read::Run {
                        id: ItemId::new("200481"),
                        attempt: None,
                    })
                    .unwrap(),
                Payload::Run
            );
            assert_eq!(detail.jobs.len(), 9);
            assert_eq!(
                detail.failure_summary.as_ref().unwrap().line,
                "arm64 failed"
            );
            let job = *expect!(
                harness.read(Read::Job(ItemId::new("424038"))).unwrap(),
                Payload::Job
            );
            assert!(matches!(&job.steps, Steps::Known(steps) if steps.len() == 2));
            // Forgejo answers a job that never ran with `500 {"message":""}`.
            let never = expect!(
                harness.read(Read::JobLog(ItemId::new("424038"))).unwrap(),
                Payload::Log
            );
            assert!(never.text.contains("never ran"), "{}", never.text);
            let log = expect!(
                harness.read(Read::JobLog(ItemId::new("424039"))).unwrap(),
                Payload::Log
            );
            assert!(!log.text.is_empty() && !log.text.contains("never ran"));
            let artifacts = expect!(
                harness
                    .read(Read::Artifacts(ItemId::new("200481")))
                    .unwrap(),
                Payload::Artifacts
            );
            assert_eq!(artifacts.len(), 1);
            assert!(matches!(artifacts[0].download, ArtifactSource::Url(_)));
        }
    }
}

pub fn writes(harness: &Harness) {
    harness.update(|state| {
        state["answers"] = json!({
            "GET repos/orbit/orbit/issues/13": {"body": {"number": 13, "labels": [], "assignees": []}}
        });
    });
    let comment = harness
        .write(Write::Comment {
            kind: ItemKind::PullRequest,
            number: 13,
            body: "Looks good to me.".into(),
        })
        .unwrap();
    assert!(comment.web_url.is_some());
    harness
        .write(Write::SetLabels {
            kind: ItemKind::PullRequest,
            number: 13,
            add: vec!["77".into()],
            remove: Vec::new(),
        })
        .unwrap();
    harness
        .write(Write::SetState {
            kind: ItemKind::PullRequest,
            number: 13,
            open: false,
            reason: None,
        })
        .unwrap();
    harness
        .write(Write::SubmitReview {
            number: 13,
            verdict: Verdict::Approve,
            body: "Ship it".into(),
            drafts: Vec::new(),
            head: Some(HEAD.into()),
        })
        .unwrap();
    harness
        .write(Write::InlineComment {
            number: 13,
            path: "src/lib.rs".into(),
            line: 12,
            side: Side::Right,
            head: HEAD.into(),
            body: "nit: rename".into(),
        })
        .unwrap();
    harness
        .write(Write::Reply {
            number: 13,
            thread: ItemId::new(format!("R12@{HEAD}:src/lib.rs")),
            body: "Done.".into(),
        })
        .unwrap();
    let merge = |head: &str| {
        harness.write(Write::Merge {
            number: 13,
            method: MergeMethod::Squash,
            head: head.into(),
            delete_branch: true,
            title: None,
            message: None,
        })
    };
    merge(HEAD).unwrap();
    let moved = merge("0000000000000000000000000000000000000000").unwrap_err();
    assert!(matches!(moved, ForgeError::Conflict(_)), "{moved:?}");
    harness
        .write(Write::Dispatch {
            workflow: ItemId::new("ci.yml"),
            reference: "main".into(),
            inputs: vec![("target".into(), "staging".into())],
        })
        .unwrap();
    harness
        .write(Write::Rerun {
            run: ItemId::new("91"),
            scope: RerunScope::All,
            debug: false,
        })
        .unwrap();
    harness.write(Write::CancelRun(ItemId::new("91"))).unwrap();
    let sent = harness
        .requests()
        .iter()
        .filter(|request| request["method"] != "GET")
        .map(|request| {
            (
                request["method"].as_str().unwrap().to_owned(),
                request["body"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        sent.contains(&("PUT".into(), json!({"labels": [77]}))),
        "{sent:?}"
    );
    assert!(sent.contains(&(
        "POST".into(),
        json!({"event": "COMMENT", "body": "", "commit_id": HEAD,
               "comments": [{"path": "src/lib.rs", "new_position": 12, "body": "Done."}]})
    )));
    harness.assert_transport();
}

pub fn degradations(harness: &Harness) {
    harness.warm();
    let capabilities = harness.provider().capabilities(&harness.ctx());
    assert!(!capabilities.resolve_thread.allowed());
    assert!(
        capabilities
            .resolve_thread
            .reason()
            .unwrap()
            .contains("no API to resolve")
    );
    assert!(!capabilities.projects.allowed());
    assert!(!capabilities.issue_not_planned.allowed());
    assert_eq!(capabilities.viewed_files, Cap::Yes);
    assert_eq!(capabilities.job_steps, StepsKind::Known);
    match harness.family {
        Family::Gitea => {
            assert_eq!(capabilities.cancel_run, Cap::Yes);
            assert_eq!(capabilities.approve_fork_run, Cap::Yes);
            assert_eq!(capabilities.rerun_failed, Cap::Yes);
        }
        Family::Forgejo => {
            assert_eq!(capabilities.rerun_all, Cap::Yes);
            assert!(
                capabilities
                    .rerun_failed
                    .reason()
                    .unwrap()
                    .contains("whole runs")
            );
            assert!(!capabilities.approve_fork_run.allowed());
            assert!(!capabilities.attempts.allowed());
        }
    }
    let mut refused = vec![
        Write::Resolve {
            thread: ItemId::new("R12:src/lib.rs"),
            resolved: true,
        },
        Write::SetProjects {
            kind: ItemKind::Issue,
            number: 13,
            add: vec!["Roadmap".into()],
            remove: Vec::new(),
        },
        Write::SetState {
            kind: ItemKind::Issue,
            number: 13,
            open: false,
            reason: Some(ClosedReason::NotPlanned),
        },
    ];
    if harness.family == Family::Forgejo {
        refused.push(Write::Rerun {
            run: ItemId::new("91"),
            scope: RerunScope::Failed,
            debug: false,
        });
        refused.push(Write::ApproveRun {
            run: ItemId::new("91"),
            expected: RunIdentity {
                sha: HEAD.into(),
                branch: "main".into(),
                head_owner: "maya".into(),
            },
        });
    }
    for write in refused {
        let error = harness.write(write.clone()).unwrap_err();
        assert!(
            matches!(error, ForgeError::Unsupported { .. }),
            "{write:?}: {error:?}"
        );
        assert!(
            error.to_string().starts_with("Unsupported here · "),
            "{error}"
        );
    }
    assert!(harness.requests().is_empty(), "{:?}", harness.logical());
}

/// Both transports decode the same values from the same requests.
fn transports_agree(family: Family) {
    let reads = [
        Read::Repository,
        list(ItemKind::PullRequest, "", 20),
        list(ItemKind::Issue, "", 10),
        Read::PullRequest(family.pull()),
        Read::Timeline(family.pull()),
        Read::Threads(family.pull()),
        Read::Runs {
            filter: RunFilter::default(),
            page: Page::first(20),
        },
        Read::Notifications {
            filter: NotifyFilter::default(),
            page: Page::first(50),
        },
    ];
    let run = |via| {
        let harness = Harness::new(family, via);
        let values = reads
            .iter()
            .map(|read| harness.read(read.clone()).unwrap())
            .collect::<Vec<_>>();
        harness.assert_transport();
        (values, harness.logical())
    };
    let (http, http_requests) = run(Via::Http);
    let (tea, tea_requests) = run(Via::Tea);
    assert_eq!(
        http, tea,
        "both transports decode to the same domain values"
    );
    assert_eq!(
        http_requests, tea_requests,
        "both transports send the same requests"
    );
}

macro_rules! contract {
    ($module:ident: $family:expr, $via:expr; $($scenario:ident),* $(,)?) => {
        mod $module {
            $(
                #[test]
                fn $scenario() {
                    let harness = super::Harness::new($family, $via);
                    super::$scenario(&harness);
                }
            )*
        }
    };
}

contract!(gitea_http: super::Family::Gitea, super::Via::Http;
    rows_and_terms, query_translation, pull_request_detail, issue_timeline_and_threads, diffs,
    repository_and_branch_lookup, notifications, actions, writes, degradations);
contract!(gitea_tea: super::Family::Gitea, super::Via::Tea;
    rows_and_terms, query_translation, pull_request_detail, issue_timeline_and_threads, diffs,
    repository_and_branch_lookup, notifications, actions, writes, degradations);
contract!(forgejo_http: super::Family::Forgejo, super::Via::Http;
    rows_and_terms, query_translation, pull_request_detail, issue_timeline_and_threads, diffs,
    repository_and_branch_lookup, notifications, actions, writes, degradations);
contract!(forgejo_tea: super::Family::Forgejo, super::Via::Tea;
    rows_and_terms, query_translation, pull_request_detail, issue_timeline_and_threads, diffs,
    repository_and_branch_lookup, notifications, actions, writes, degradations);

#[test]
fn gitea_transports_agree() {
    transports_agree(Family::Gitea);
}

#[test]
fn forgejo_transports_agree() {
    transports_agree(Family::Forgejo);
}

// ---------------------------------------------------------------------------- child processes

/// Tests that write the forge state directory (viewed files, `hosts.json`) or read the process
/// environment run again in a child process with their own environment.
const CHILD: &str = "DUSTY_GITEA_CONTRACT_CHILD";

fn rerun_in_child(test: &str, configure: impl FnOnce(&mut std::process::Command)) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([test, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1");
    for name in forge_fake::AMBIENT_ENV {
        command.env_remove(name);
    }
    configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn viewed_files_stay_on_this_computer() {
    if std::env::var_os(CHILD).is_none() {
        let state = tempfile::tempdir().unwrap();
        rerun_in_child("viewed_files_stay_on_this_computer", |command| {
            command.env("DUSTY_FORGE_STATE_DIR", state.path());
        });
        let file = state
            .path()
            .join("viewed/code.forgejo.org/orbit/orbit/1784.json");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["viewed"], json!(["internal/app/cmd/exec.go"]));
        return;
    }
    let harness = Harness::new(Family::Forgejo, Via::Http);
    harness.warm();
    let receipt = harness
        .write(Write::SetViewed {
            number: 1784,
            node: ItemId::default(),
            path: "internal/app/cmd/exec.go".into(),
            viewed: true,
        })
        .unwrap();
    assert_eq!(receipt.summary, "Saved on this computer");
    assert!(harness.requests().is_empty(), "nothing goes to the forge");
    let viewed = expect!(harness.read(Read::Viewed(1784)).unwrap(), Payload::Viewed);
    assert_eq!(viewed, [("internal/app/cmd/exec.go".to_owned(), true)]);
    let pull = *expect!(
        harness.read(Read::PullRequest(1784)).unwrap(),
        Payload::PullRequest
    );
    assert_eq!(pull.files[0].viewed, Some(true));
}

/// The IDE's pull-request picker on a Codeberg checkout lists through the provider: once with
/// a token over HTTP, once through tea with the login named in `[forge.hosts."codeberg.org"]`.
#[test]
fn ide_picker_lists_codeberg_pull_requests() {
    if std::env::var_os(CHILD).is_some() {
        let root = PathBuf::from(std::env::var_os("DUSTY_GITEA_REPO").unwrap());
        let mut jobs = dust::forge::ForgeJobs::default();
        jobs.list(root, 1).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let reply = loop {
            if let Some(event) = jobs.try_reply() {
                break event.reply;
            }
            assert!(Instant::now() < deadline, "the picker never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let list = match reply {
            dust::forge::ForgeReply::List(Ok(list)) => list,
            other => panic!("the picker failed: {other:?}"),
        };
        assert_eq!(list.len(), 10, "{list:?}");
        let pull = list.iter().find(|pull| pull.number == 1784).unwrap();
        assert_eq!(pull.branch, "docker-socket");
        assert!(!pull.title.is_empty() && !pull.author.is_empty());
        return;
    }
    for via in [Via::Http, Via::Tea] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let state = json!({ "probes": probes("codeberg.org", "15.0.0+gitea-1.22.0") });
        std::fs::write(&path, state.to_string()).unwrap();
        // Codeberg runs Forgejo; the recorded Forgejo repository stands in for it.
        let server = ForgeServer::start(&path, "forgejo");
        let tea = server.install_tea(
            &directory.path().join("bin"),
            json!([{"name": "other", "url": "https://gitea.example", "default": true},
                   {"name": "cb", "url": "https://codeberg.org", "user": "maya", "default": false}]),
        );
        let repo = directory.path().join("orbit");
        let repository = git2::Repository::init(&repo).unwrap();
        repository
            .remote("origin", "https://codeberg.org/orbit/orbit.git")
            .unwrap();
        let home = directory.path().join("home");
        let config = directory.path().join("config");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            "[forge.hosts.\"codeberg.org\"]\nlogin = \"cb\"\n",
        )
        .unwrap();
        rerun_in_child("ide_picker_lists_codeberg_pull_requests", |command| {
            server.configure(command);
            command
                .env("DUSTY_GITEA_REPO", &repo)
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("XDG_DATA_HOME", home.join(".local/share"))
                .env("XDG_STATE_HOME", home.join(".local/state"))
                .env("DUST_CONFIG_HOME", &config)
                .env("DUSTY_FORGE_TEA", &tea)
                .env("DUSTY_FORGE_GH", directory.path().join("bin/no-gh"));
            if via == Via::Http {
                command.env("FORGEJO_TOKEN", FAKE_TOKEN);
            }
        });
        let requests = forge_fake::requests(&path);
        let list = requests
            .iter()
            .find(|request| request["path"] == "repos/orbit/orbit/pulls")
            .unwrap_or_else(|| panic!("no pulls request: {requests:?}"));
        assert_eq!(list["host"], "codeberg.org");
        assert_eq!(list["query"]["state"], "open");
        match via {
            Via::Http => {
                assert_eq!(list["transport"], "http");
                assert_eq!(list["auth"], "valid");
            }
            Via::Tea => {
                assert_eq!(list["transport"], "tea");
                let argv = list["gh_argv"].as_array().unwrap();
                assert!(
                    argv.windows(2)
                        .any(|pair| pair[0] == "--login" && pair[1] == "cb"),
                    "{argv:?}"
                );
            }
        }
    }
}
