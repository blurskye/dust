//! GitLab's contract scenarios (F5) against `server.py --world gitlab`
//! (`tests/fixtures/forge/world_gitlab.py`): project `orbit/team/orbit` in a nested group,
//! merge requests !13 (failed pipeline, two diff threads, alex requested changes), !12 (merged),
//! !11 (draft, running pipeline), !10 (approved); issues #14, #9 and closed #7; pipelines 91, 90
//! and 89; To-dos 9001 (review requested), 9002 (mentioned) and 9003 (done).

use crate::forge_fake;
use crate::forge_fake::FAKE_TOKEN;
use crate::forge_fake::ForgeServer;
use dust::forge::capability::Cap;
use dust::forge::capability::GITLAB_TERMS;
use dust::forge::cli::CliTransport;
use dust::forge::credential::Credential;
use dust::forge::credential::CredentialSource;
use dust::forge::credential::Env;
use dust::forge::credential::Secret;
use dust::forge::detect;
use dust::forge::http::HttpTransport;
use dust::forge::model::CiState;
use dust::forge::model::ClosedReason;
use dust::forge::model::FileStatus;
use dust::forge::model::ItemId;
use dust::forge::model::ItemKind;
use dust::forge::model::Lifecycle;
use dust::forge::model::LogFormat;
use dust::forge::model::MergeMethod;
use dust::forge::model::Mergeable;
use dust::forge::model::NotifyReason;
use dust::forge::model::NotifyTarget;
use dust::forge::model::ReviewState;
use dust::forge::model::RunStatus;
use dust::forge::model::Side;
use dust::forge::model::Steps;
use dust::forge::model::TimelineEvent;
use dust::forge::provider;
use dust::forge::provider::Conditional;
use dust::forge::provider::Fetched;
use dust::forge::provider::ForgeContext;
use dust::forge::provider::Provider;
use dust::forge::provider::ProviderKind;
use dust::forge::provider::RepoRef;
use dust::forge::request::Filter;
use dust::forge::request::ForgeError;
use dust::forge::request::InlineDraft;
use dust::forge::request::NotifyFilter;
use dust::forge::request::Page;
use dust::forge::request::Payload;
use dust::forge::request::Read;
use dust::forge::request::Receipt;
use dust::forge::request::RerunScope;
use dust::forge::request::RunFilter;
use dust::forge::request::StateFilter;
use dust::forge::request::SubscriptionState;
use dust::forge::request::Verdict;
use dust::forge::request::Write;
use dust::forge::transport::Cli;
use dust::forge::transport::ConnectOptions;
use dust::forge::transport::HostKey;
use dust::forge::transport::Transport;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub const PROJECT: &str = "projects/orbit%2Fteam%2Forbit";
pub const HEAD: &str = "c92ad11000000000000000000000000000000000";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Via {
    Http,
    Glab,
}

pub struct Harness {
    directory: tempfile::TempDir,
    server: ForgeServer,
    glab: PathBuf,
    pub via: Via,
    pub host: String,
    ctx: ForgeContext,
    provider: Arc<dyn Provider>,
}

/// Probed capabilities persist in `hosts.json`, found through the process environment. A
/// scenario runs in a child of this test binary whose `DUSTY_FORGE_STATE_DIR` is a temporary
/// directory, so the user's state directory is never written (the crate forbids `set_var`).
pub fn isolated(name: &str, scenario: impl FnOnce()) {
    if std::env::var_os("DUSTY_FORGE_STATE_DIR").is_some() {
        scenario();
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env("DUSTY_FORGE_STATE_DIR", state.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{name} failed in its isolated child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Harness {
    pub fn new(via: Via) -> Self {
        // Each harness is its own host, so per-host memos (`@me`, probes) never leak across tests.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let host = format!(
            "gitlab-{}.contract.test",
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        std::fs::write(&path, json!({ "glab_auth": { "*": "token" } }).to_string()).unwrap();
        let server = ForgeServer::start(&path, "gitlab");
        let glab = server.install_glab(&directory.path().join("bin"));
        let endpoints = detect::endpoints_for(
            ProviderKind::GitLab,
            &HostKey::new(&host),
            None,
            &Env::from_pairs([("DUSTY_FORGE_ORIGIN", server.origin())]),
        );
        let transport: Arc<dyn Transport> = match via {
            Via::Http => Arc::new(
                HttpTransport::new(
                    endpoints.clone(),
                    Credential {
                        token: Secret::new(FAKE_TOKEN),
                        source: CredentialSource::Env("GITLAB_TOKEN"),
                        user: None,
                        obtained: Instant::now(),
                    },
                )
                .unwrap(),
            ),
            Via::Glab => Arc::new(CliTransport::new(Cli::Glab, glab.clone(), None)),
        };
        let ctx = ForgeContext::new(
            directory.path().to_path_buf(),
            endpoints,
            ConnectOptions {
                glab: Some(glab.clone()),
                ..ConnectOptions::default()
            },
            RepoRef::parse("orbit/team/orbit"),
            transport,
        );
        Self {
            directory,
            server,
            glab,
            via,
            host,
            ctx,
            provider: provider::build(ProviderKind::GitLab).unwrap(),
        }
    }

    fn state_path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    pub fn update(&self, change: impl FnOnce(&mut Value)) {
        let mut state = forge_fake::state(&self.state_path());
        change(&mut state);
        std::fs::write(self.state_path(), state.to_string()).unwrap();
    }

    /// Seed the world (it is built on the first request) and forget what that sent.
    pub fn seeded(&self, change: impl FnOnce(&mut Value)) {
        self.read(Read::Viewer).unwrap();
        self.update(|state| {
            change(&mut state["gitlab"]);
            state["requests"] = json!([]);
        });
    }

    pub fn requests(&self) -> Vec<Value> {
        forge_fake::requests(&self.state_path())
            .into_iter()
            .filter(|request| request["path"] != "auth status")
            .collect()
    }

    pub fn clear(&self) {
        self.update(|state| state["requests"] = json!([]));
    }

    /// `(method, path, body)` of every request that changes something.
    pub fn writes(&self) -> Vec<(String, String, Value)> {
        self.requests()
            .into_iter()
            .filter(|request| {
                request["method"] != "GET"
                    && !(request["path"] == "graphql"
                        && matches!(
                            request["operation"].as_str(),
                            Some("MergeRequestList" | "IssueList" | "MutationFields")
                        ))
            })
            .map(|request| {
                let body = if request["path"] == "graphql" {
                    json!({ "operation": request["operation"], "variables": request["variables"] })
                } else {
                    request["body"].clone()
                };
                (
                    request["method"].as_str().unwrap_or_default().to_owned(),
                    request["path"].as_str().unwrap_or_default().to_owned(),
                    body,
                )
            })
            .collect()
    }

    pub fn read(&self, read: Read) -> Result<Payload, ForgeError> {
        match self.provider.read(
            &self.ctx,
            &read,
            &Conditional::default(),
            &AtomicBool::new(false),
        )? {
            Fetched::Payload { value, .. } => Ok(value),
            Fetched::NotModified => panic!("{read:?} was not modified"),
        }
    }

    pub fn write(&self, write: Write) -> Result<Receipt, ForgeError> {
        self.provider
            .write(&self.ctx, &write, &AtomicBool::new(false))
    }

    pub fn rows(&self, kind: ItemKind, state: StateFilter, query: &str) -> Vec<u64> {
        let Payload::Rows(rows) = self
            .read(Read::List {
                kind,
                filter: Filter {
                    state,
                    query: query.into(),
                },
                page: Page::first(50),
            })
            .unwrap()
        else {
            panic!("rows")
        };
        rows.items.iter().map(|row| row.number).collect()
    }

    /// Every request went over the harness's transport, authenticated, and passthrough argv
    /// had glab's form without the token.
    pub fn assert_transport(&self) {
        let requests = self.requests();
        assert!(!requests.is_empty());
        for request in &requests {
            match self.via {
                Via::Http => {
                    assert_eq!(request["transport"], "http", "{request}");
                    assert_eq!(request["auth"], "valid", "{request}");
                }
                Via::Glab => {
                    assert_eq!(request["transport"], "glab", "{request}");
                    let words = request["gh_argv"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|word| word.as_str().unwrap().to_owned())
                        .collect::<Vec<_>>();
                    assert_eq!(words[0], "api");
                    assert!(words.iter().any(|word| word == "--include"), "{words:?}");
                    assert!(
                        words
                            .windows(2)
                            .any(|pair| pair[0] == "--hostname" && pair[1] == self.host),
                        "{words:?}"
                    );
                    if !request["body"].is_null() || request["path"] == "graphql" {
                        assert!(
                            words.windows(2).any(|pair| pair == ["--input", "-"]),
                            "{words:?}"
                        );
                    }
                    assert!(!words.join(" ").contains(FAKE_TOKEN), "{words:?}");
                }
            }
        }
        let _ = (&self.server, &self.glab);
    }
}

fn some<T>(payload: Payload, pick: impl FnOnce(Payload) -> Option<T>) -> T {
    let debug = format!("{payload:?}");
    pick(payload).unwrap_or_else(|| panic!("unexpected payload {debug}"))
}

// ------------------------------------------------------------------------------ rows

pub fn rows_and_terms(harness: &Harness) {
    assert_eq!(harness.provider.terms(), &GITLAB_TERMS);
    assert_eq!(
        (
            GITLAB_TERMS.pr_plural,
            GITLAB_TERMS.pr_glyph,
            GITLAB_TERMS.actions_tab,
            GITLAB_TERMS.notifications_tab
        ),
        ("Merge requests", "!", "Pipelines", "To-dos")
    );
    let rows = some(
        harness
            .read(Read::List {
                kind: ItemKind::PullRequest,
                filter: Filter::default(),
                page: Page::first(30),
            })
            .unwrap(),
        |payload| match payload {
            Payload::Rows(rows) => Some(rows),
            _ => None,
        },
    );
    assert_eq!(
        rows.items.iter().map(|row| row.number).collect::<Vec<_>>(),
        [13, 11, 10]
    );
    assert_eq!(rows.total, Some(3));
    let first = &rows.items[0];
    assert_eq!(first.title, "Retry flaky artifact uploads");
    assert_eq!(first.author.login, "maya");
    assert_eq!(first.kind, Some(ItemKind::PullRequest));
    assert_eq!(first.repo.as_deref(), Some("orbit/team/orbit"));
    assert_eq!(
        first.web_url,
        "https://gitlab.com/orbit/team/orbit/-/merge_requests/13"
    );
    assert_eq!(
        first
            .labels
            .iter()
            .map(|label| label.name.as_str())
            .collect::<Vec<_>>(),
        ["backend", "pipeline"]
    );
    assert_eq!(first.ci, Some(CiState::Failed));
    assert_eq!(first.review, Some(ReviewState::Requested));
    assert_eq!(
        (first.additions, first.deletions, first.changed_files),
        (Some(6), Some(1), Some(2))
    );
    assert_eq!(first.comments, 4);
    assert!(
        first
            .body_preview
            .as_deref()
            .unwrap()
            .starts_with("Uploads")
    );
    assert_eq!(rows.items[1].lifecycle, Lifecycle::Draft);
    assert_eq!(rows.items[1].ci, Some(CiState::Pending));
    assert_eq!(rows.items[2].review, Some(ReviewState::Approved));
    assert_eq!(rows.items[2].ci, Some(CiState::Passed));

    assert_eq!(
        harness.rows(ItemKind::Issue, StateFilter::Open, ""),
        [14, 9]
    );
    assert_eq!(harness.rows(ItemKind::Issue, StateFilter::Closed, ""), [7]);
}

/// GitHub search syntax (§3.6) becomes GitLab arguments; the world filters on them.
pub fn query_translation(harness: &Harness) {
    harness.update(|state| state["viewer"] = json!("alex"));
    let pulls = |state, query| harness.rows(ItemKind::PullRequest, state, query);
    assert_eq!(pulls(StateFilter::Open, "label:pipeline"), [13]);
    assert_eq!(pulls(StateFilter::Open, "-label:wip"), [13, 10]);
    assert_eq!(pulls(StateFilter::Open, "is:draft"), [11]);
    assert_eq!(pulls(StateFilter::Merged, ""), [12]);
    assert_eq!(pulls(StateFilter::Open, "is:merged"), [12]);
    assert_eq!(pulls(StateFilter::All, "author:@me"), [12]);
    assert_eq!(pulls(StateFilter::Open, "review-requested:alex"), [13, 10]);
    assert_eq!(pulls(StateFilter::Open, "artifact"), [13]);
    assert_eq!(pulls(StateFilter::Open, "milestone:v1.4"), [13]);
    let issues = |query| harness.rows(ItemKind::Issue, StateFilter::Open, query);
    assert_eq!(issues("label:bug"), [14, 9]);
    assert_eq!(issues("-label:pipeline"), [9]);
    assert_eq!(issues("assignee:maya"), [14]);

    let list = harness
        .requests()
        .into_iter()
        .filter(|request| request["operation"] == "MergeRequestList")
        .map(|request| request["variables"].clone())
        .collect::<Vec<_>>();
    assert_eq!(list[0]["labels"], json!(["pipeline"]));
    assert_eq!(list[1]["not"], json!({ "labels": ["wip"] }));
    assert_eq!(list[2]["draft"], json!(true));
    assert_eq!(list[5]["authorUsername"], "alex");
    assert_eq!(list[5]["state"], "all");
    assert_eq!(list[6]["reviewerUsername"], "alex");
    assert_eq!(list[7]["search"], "artifact");

    // Host-wide (the inbox): REST with `scope=all`, `@me` resolved through `GET /user`.
    let rows = some(
        harness
            .read(Read::Search {
                kind: ItemKind::PullRequest,
                filter: Filter {
                    state: StateFilter::Open,
                    query: "review-requested:@me".into(),
                },
                page: Page::first(30),
            })
            .unwrap(),
        |payload| match payload {
            Payload::Rows(rows) => Some(rows),
            _ => None,
        },
    );
    assert_eq!(
        rows.items.iter().map(|row| row.number).collect::<Vec<_>>(),
        [13, 10]
    );
    assert!(
        rows.items
            .iter()
            .all(|row| row.repo.as_deref() == Some("orbit/team/orbit"))
    );
    let search = harness
        .requests()
        .into_iter()
        .rfind(|request| request["path"] == "merge_requests")
        .unwrap();
    assert_eq!(search["query"]["scope"], "all");
    assert_eq!(search["query"]["reviewer_username"], "alex");
}

pub fn pagination(harness: &Harness) {
    let page = |number, per_page| {
        some(
            harness
                .read(Read::List {
                    kind: ItemKind::PullRequest,
                    filter: Filter::default(),
                    page: Page { number, per_page },
                })
                .unwrap(),
            |payload| match payload {
                Payload::Rows(rows) => Some(rows),
                _ => None,
            },
        )
    };
    let first = page(1, 2);
    assert_eq!(
        first.items.iter().map(|row| row.number).collect::<Vec<_>>(),
        [13, 11]
    );
    assert_eq!(
        first.next,
        Some(Page {
            number: 2,
            per_page: 2
        })
    );
    let second = page(2, 2);
    assert_eq!(
        second
            .items
            .iter()
            .map(|row| row.number)
            .collect::<Vec<_>>(),
        [10]
    );
    assert_eq!(second.next, None);

    // REST: `x-next-page` and `x-total`.
    let search = some(
        harness
            .read(Read::Search {
                kind: ItemKind::Issue,
                filter: Filter {
                    state: StateFilter::All,
                    query: String::new(),
                },
                page: Page {
                    number: 2,
                    per_page: 1,
                },
            })
            .unwrap(),
        |payload| match payload {
            Payload::Rows(rows) => Some(rows),
            _ => None,
        },
    );
    assert_eq!(
        search
            .items
            .iter()
            .map(|row| row.number)
            .collect::<Vec<_>>(),
        [9]
    );
    assert_eq!(
        search.next,
        Some(Page {
            number: 3,
            per_page: 1
        })
    );
    assert_eq!(search.total, Some(3));

    // A detail list longer than one page is followed to the end.
    harness.seeded(|world| {
        let discussions = (0..150)
            .map(|index| {
                json!({"id": format!("bulk{index}"), "individual_note": false, "notes": [{
                    "id": 90000 + index, "type": "DiffNote", "body": format!("note {index}"),
                    "author": {"id": 1003, "username": "sam"}, "created_at": "2026-11-14T05:00:00Z",
                    "system": false, "resolvable": true, "resolved": false,
                    "position": {"new_path": "README.md", "old_path": "README.md", "new_line": index + 1,
                                 "old_line": null, "head_sha": HEAD}}]})
            })
            .collect::<Vec<_>>();
        world["merge_requests"][3]["discussions"] = json!(discussions);
    });
    let threads = some(
        harness.read(Read::Threads(10)).unwrap(),
        |payload| match payload {
            Payload::Threads(threads) => Some(threads),
            _ => None,
        },
    );
    assert_eq!(threads.len(), 150);
    let pages = harness
        .requests()
        .iter()
        .filter(|request| request["path"].as_str().unwrap().ends_with("/discussions"))
        .map(|request| request["query"]["page"].as_str().unwrap_or("1").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(pages, ["1", "2"]);
}

// ------------------------------------------------------------------------------ details

pub fn merge_request_detail(harness: &Harness) {
    let pull = some(
        harness.read(Read::PullRequest(13)).unwrap(),
        |payload| match payload {
            Payload::PullRequest(pull) => Some(pull),
            _ => None,
        },
    );
    assert_eq!(pull.row.number, 13);
    assert_eq!(pull.head.sha, HEAD);
    assert_eq!(pull.head.branch, "retry-uploads");
    assert_eq!(pull.base.branch, "main");
    assert_eq!(pull.base.sha, "a".repeat(40));
    assert_eq!(pull.pinned_head, HEAD);
    assert_eq!(pull.mergeable, Mergeable::Blocked);
    assert_eq!(
        pull.files
            .iter()
            .map(|file| (
                file.path.as_str(),
                file.status,
                file.additions,
                file.deletions
            ))
            .collect::<Vec<_>>(),
        [
            ("src/lib.rs", FileStatus::Modified, 3, 1),
            ("ci/retry.rs", FileStatus::Added, 3, 0)
        ]
    );
    let mut checks = pull
        .checks
        .iter()
        .map(|check| (check.name.as_str(), check.workflow.as_deref(), check.state))
        .collect::<Vec<_>>();
    checks.sort_by_key(|check| check.0);
    assert_eq!(
        checks,
        [
            ("build", Some("build"), CiState::Passed),
            ("clippy", Some("test"), CiState::Passed),
            ("deploy", Some("deploy"), CiState::Skipped),
            ("test", Some("test"), CiState::Failed),
        ]
    );
    let test = pull
        .checks
        .iter()
        .find(|check| check.name == "test")
        .unwrap();
    let link = test.run_link.as_ref().unwrap();
    assert_eq!(
        (link.run.0.as_str(), link.job.as_ref().unwrap().0.as_str()),
        ("91", "42")
    );
    assert_eq!(pull.row.ci, Some(CiState::Failed));
    assert_eq!(
        pull.reviewers
            .iter()
            .map(|slot| (slot.reviewer.as_str(), slot.state))
            .collect::<Vec<_>>(),
        [
            ("alex", ReviewState::ChangesRequested),
            ("sam", ReviewState::Requested)
        ]
    );
    assert_eq!(pull.reviews.len(), 1);
    assert_eq!(pull.reviews[0].author.login, "alex");
    assert_eq!(pull.reviews[0].state, ReviewState::ChangesRequested);
    assert_eq!(pull.row.review, Some(ReviewState::ChangesRequested));
    assert_eq!(
        pull.linked_issues
            .iter()
            .map(|link| link.number)
            .collect::<Vec<_>>(),
        [14]
    );
    assert_eq!(pull.milestone.as_ref().unwrap().title, "v1.4");
    assert_eq!(pull.assignees[0].login, "maya");
    assert_eq!(pull.commits.len(), 1);
    assert_eq!(
        pull.commits[0].headline,
        "Retry artifact uploads three times"
    );
    assert_eq!(pull.commits[0].body, "Flaky store.");
    assert_eq!(pull.fingerprint.head, HEAD);
    assert_eq!(pull.subscribed, Some(true));

    let approved = some(
        harness.read(Read::PullRequest(10)).unwrap(),
        |payload| match payload {
            Payload::PullRequest(pull) => Some(pull),
            _ => None,
        },
    );
    assert_eq!(approved.reviews[0].author.login, "alex");
    assert_eq!(approved.reviews[0].state, ReviewState::Approved);
    assert_eq!(approved.row.review, Some(ReviewState::Approved));
    assert_eq!(approved.mergeable, Mergeable::Clean);

    let fingerprint = some(
        harness.read(Read::Fingerprint(13)).unwrap(),
        |payload| match payload {
            Payload::Fingerprint(fingerprint) => Some(fingerprint),
            _ => None,
        },
    );
    assert_eq!(fingerprint, pull.fingerprint);
    assert_eq!(
        harness
            .provider
            .read(
                &harness.ctx,
                &Read::Fingerprint(13),
                &Conditional {
                    fingerprint: Some(fingerprint),
                    ..Conditional::default()
                },
                &AtomicBool::new(false)
            )
            .unwrap(),
        Fetched::NotModified
    );
    assert_eq!(
        harness
            .read(Read::PullRequestForBranch("retry-uploads".into()))
            .unwrap(),
        Payload::MaybePullRequest(Some(13))
    );
    assert!(matches!(
        harness.read(Read::PullRequest(404)),
        Err(ForgeError::NotFound(_))
    ));
}

pub fn issue_detail(harness: &Harness) {
    let issue = some(
        harness.read(Read::Issue(14)).unwrap(),
        |payload| match payload {
            Payload::Issue(issue) => Some(issue),
            _ => None,
        },
    );
    assert_eq!(issue.row.title, "Artifact uploads fail on nested groups");
    assert_eq!(issue.row.kind, Some(ItemKind::Issue));
    assert!(issue.body.starts_with("`orbit/team/orbit`"));
    assert_eq!(issue.assignees[0].login, "maya");
    assert_eq!(issue.milestone.as_ref().unwrap().title, "v1.4");
    assert_eq!(
        issue
            .linked_prs
            .iter()
            .map(|link| link.number)
            .collect::<Vec<_>>(),
        [13]
    );
    assert_eq!(issue.linked_prs[0].kind, Some(ItemKind::PullRequest));
    assert_eq!(
        issue
            .row
            .labels
            .iter()
            .map(|label| label.name.as_str())
            .collect::<Vec<_>>(),
        ["bug", "pipeline"]
    );
}

pub fn threads_and_timeline(harness: &Harness) {
    let threads = some(
        harness.read(Read::Threads(13)).unwrap(),
        |payload| match payload {
            Payload::Threads(threads) => Some(threads),
            _ => None,
        },
    );
    assert_eq!(
        threads
            .iter()
            .map(|thread| (
                thread.id.0.as_str(),
                thread.path.as_deref().unwrap(),
                thread.line,
                thread.side,
                thread.resolved,
                thread.comments.len()
            ))
            .collect::<Vec<_>>(),
        [
            (
                "13/6a9c1750b37d513a43987b574953fceb50b03ce7",
                "src/lib.rs",
                Some(12),
                Side::Right,
                Some(false),
                2
            ),
            (
                "13/77d0c0ffee",
                "ci/retry.rs",
                Some(2),
                Side::Right,
                Some(false),
                1
            )
        ]
    );
    assert_eq!(threads[0].comments[0].author.login, "alex");
    assert_eq!(threads[0].commit.as_deref(), Some(HEAD));

    let timeline = some(
        harness.read(Read::Timeline(13)).unwrap(),
        |payload| match payload {
            Payload::Timeline(timeline) => Some(timeline),
            _ => None,
        },
    );
    let shape = timeline
        .iter()
        .map(|event| match event {
            TimelineEvent::Commented(comment) => {
                format!("comment {} {}", comment.author.login, comment.body)
            }
            TimelineEvent::Reviewed(review) => {
                format!("review {} {:?}", review.author.login, review.state)
            }
            TimelineEvent::Other { kind, by } => format!("other {} {kind}", by.actor.login),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shape,
        [
            "comment maya Ready for review.",
            "review alex ChangesRequested",
            "other sam added ~pipeline label",
            "comment alex Looks close; one naming nit.",
        ]
    );

    // An issue's timeline is read under `issues/`, once the issue was opened.
    harness.read(Read::Issue(14)).unwrap();
    let timeline = some(
        harness.read(Read::Timeline(14)).unwrap(),
        |payload| match payload {
            Payload::Timeline(timeline) => Some(timeline),
            _ => None,
        },
    );
    assert_eq!(timeline.len(), 1);
    assert!(
        harness
            .requests()
            .iter()
            .any(|request| request["path"] == format!("{PROJECT}/issues/14/notes"))
    );
}

pub fn diff_has_git_headers(harness: &Harness) {
    let diff = some(
        harness.read(Read::Diff(13)).unwrap(),
        |payload| match payload {
            Payload::Text(text) => Some(text),
            _ => None,
        },
    );
    assert!(
        diff.starts_with(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,4 +10,6 @@"
        ),
        "{diff}"
    );
    assert!(
        diff.contains(
            "diff --git a/ci/retry.rs b/ci/retry.rs\nnew file mode 100644\n--- /dev/null\n+++ b/ci/retry.rs\n@@ -0,0 +1,3 @@\n"
        ),
        "{diff}"
    );
    assert_eq!(
        diff.lines()
            .filter(|line| line.starts_with("diff --git"))
            .count(),
        2
    );
    let commit = some(
        harness.read(Read::CommitDiff(HEAD.into())).unwrap(),
        |payload| match payload {
            Payload::Text(text) => Some(text),
            _ => None,
        },
    );
    assert_eq!(commit, diff);
}

// ------------------------------------------------------------------------------ writes

pub fn comment_labels_and_state(harness: &Harness) {
    harness.seeded(|_| {});
    let receipt = harness
        .write(Write::Comment {
            kind: ItemKind::PullRequest,
            number: 13,
            body: "Thanks".into(),
        })
        .unwrap();
    assert!(
        receipt
            .web_url
            .as_deref()
            .unwrap()
            .ends_with("/orbit/team/orbit/-/merge_requests/13#note_10001"),
        "{receipt:?}"
    );
    harness
        .write(Write::SetLabels {
            kind: ItemKind::PullRequest,
            number: 13,
            add: vec!["bug".into()],
            remove: vec!["backend".into()],
        })
        .unwrap();
    harness
        .write(Write::SetState {
            kind: ItemKind::Issue,
            number: 9,
            open: false,
            reason: None,
        })
        .unwrap();
    assert_eq!(harness.rows(ItemKind::Issue, StateFilter::Open, ""), [14]);
    harness
        .write(Write::SetState {
            kind: ItemKind::Issue,
            number: 9,
            open: true,
            reason: None,
        })
        .unwrap();
    harness
        .write(Write::SetAssignees {
            kind: ItemKind::Issue,
            number: 9,
            add: vec!["sam".into()],
            remove: Vec::new(),
        })
        .unwrap();
    harness
        .write(Write::SetMilestone {
            kind: ItemKind::Issue,
            number: 9,
            milestone: Some("v1.4".into()),
        })
        .unwrap();
    harness
        .write(Write::SetReviewers {
            number: 10,
            add: vec!["sam".into()],
            remove: vec!["alex".into()],
        })
        .unwrap();
    harness
        .write(Write::Subscribe {
            kind: ItemKind::PullRequest,
            number: 13,
            node: ItemId::default(),
            state: SubscriptionState::Unsubscribed,
        })
        .unwrap();
    // Already unsubscribed: GitLab answers 304, which is not an error.
    harness
        .write(Write::Subscribe {
            kind: ItemKind::PullRequest,
            number: 13,
            node: ItemId::default(),
            state: SubscriptionState::Unsubscribed,
        })
        .unwrap();
    let created = harness
        .write(Write::CreateIssue {
            title: "Cache misses".into(),
            body: "Seen twice".into(),
            labels: vec!["bug".into()],
            assignees: Vec::new(),
        })
        .unwrap();
    assert_eq!(created.created_number, Some(15));

    let mr = format!("{PROJECT}/merge_requests");
    let issues = format!("{PROJECT}/issues");
    assert_eq!(
        harness.writes(),
        [
            (
                "POST".to_owned(),
                format!("{mr}/13/notes"),
                json!({"body": "Thanks"})
            ),
            (
                "PUT".to_owned(),
                format!("{mr}/13"),
                json!({"add_labels": "bug", "remove_labels": "backend"})
            ),
            (
                "PUT".to_owned(),
                format!("{issues}/9"),
                json!({"state_event": "close"})
            ),
            (
                "PUT".to_owned(),
                format!("{issues}/9"),
                json!({"state_event": "reopen"})
            ),
            (
                "PUT".to_owned(),
                format!("{issues}/9"),
                json!({"assignee_ids": [1003]})
            ),
            (
                "PUT".to_owned(),
                format!("{issues}/9"),
                json!({"milestone_id": 12})
            ),
            (
                "PUT".to_owned(),
                format!("{mr}/10"),
                json!({"reviewer_ids": [1003]})
            ),
            ("POST".to_owned(), format!("{mr}/13/unsubscribe"), json!({})),
            ("POST".to_owned(), format!("{mr}/13/unsubscribe"), json!({})),
            (
                "POST".to_owned(),
                issues.clone(),
                json!({"title": "Cache misses", "description": "Seen twice", "labels": "bug"})
            ),
        ]
    );

    let issue = some(
        harness.read(Read::Issue(9)).unwrap(),
        |payload| match payload {
            Payload::Issue(issue) => Some(issue),
            _ => None,
        },
    );
    assert_eq!(issue.assignees[0].login, "sam");
    assert_eq!(issue.milestone.as_ref().unwrap().title, "v1.4");
    assert_eq!(issue.row.lifecycle, Lifecycle::Open);
    let pull = some(
        harness.read(Read::PullRequest(13)).unwrap(),
        |payload| match payload {
            Payload::PullRequest(pull) => Some(pull),
            _ => None,
        },
    );
    let mut labels = pull
        .row
        .labels
        .iter()
        .map(|label| label.name.clone())
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, ["bug", "pipeline"]);
    assert_eq!(pull.subscribed, Some(false));

    harness
        .write(Write::SetMilestone {
            kind: ItemKind::Issue,
            number: 9,
            milestone: None,
        })
        .unwrap();
    assert_eq!(
        harness.writes().last().unwrap().2,
        json!({"milestone_id": 0})
    );
}

pub fn threads_reply_and_resolve(harness: &Harness) {
    harness.seeded(|_| {});
    let thread = ItemId::from("13/6a9c1750b37d513a43987b574953fceb50b03ce7");
    harness
        .write(Write::Reply {
            number: 13,
            thread: thread.clone(),
            body: "Renamed.".into(),
        })
        .unwrap();
    harness
        .write(Write::Resolve {
            thread,
            resolved: true,
        })
        .unwrap();
    harness
        .write(Write::InlineComment {
            number: 13,
            path: "src/lib.rs".into(),
            line: 11,
            side: Side::Right,
            head: HEAD.into(),
            body: "Why three?".into(),
        })
        .unwrap();
    let discussions = format!("{PROJECT}/merge_requests/13/discussions");
    assert_eq!(
        harness.writes(),
        [
            (
                "POST".to_owned(),
                format!("{discussions}/6a9c1750b37d513a43987b574953fceb50b03ce7/notes"),
                json!({"body": "Renamed."})
            ),
            (
                "PUT".to_owned(),
                format!("{discussions}/6a9c1750b37d513a43987b574953fceb50b03ce7"),
                json!({"resolved": true})
            ),
            (
                "POST".to_owned(),
                discussions.clone(),
                json!({"body": "Why three?", "position": {
                    "position_type": "text", "base_sha": "a".repeat(40), "start_sha": "a".repeat(40),
                    "head_sha": HEAD, "old_path": "src/lib.rs", "new_path": "src/lib.rs", "new_line": 11}})
            ),
        ]
    );
    let threads = some(
        harness.read(Read::Threads(13)).unwrap(),
        |payload| match payload {
            Payload::Threads(threads) => Some(threads),
            _ => None,
        },
    );
    assert_eq!(threads.len(), 3);
    assert_eq!(threads[0].resolved, Some(true));
    assert_eq!(threads[0].comments.len(), 3);
    assert_eq!(threads[2].line, Some(11));

    // A comment against a head that moved is refused before anything is sent.
    harness.clear();
    let moved = harness.write(Write::InlineComment {
        number: 13,
        path: "src/lib.rs".into(),
        line: 11,
        side: Side::Right,
        head: "0".repeat(40),
        body: "stale".into(),
    });
    assert!(matches!(moved, Err(ForgeError::Conflict(_))), "{moved:?}");
    assert!(harness.writes().is_empty());
}

fn drafts() -> Vec<InlineDraft> {
    vec![
        InlineDraft {
            path: "src/lib.rs".into(),
            line: 12,
            side: Side::Right,
            body: "Name the count".into(),
        },
        InlineDraft {
            path: "ci/retry.rs".into(),
            line: 2,
            side: Side::Right,
            body: "Back off?".into(),
        },
    ]
}

pub fn review_batches_drafts_then_approves(harness: &Harness) {
    harness.seeded(|_| {});
    let receipt = harness
        .write(Write::SubmitReview {
            number: 13,
            verdict: Verdict::Approve,
            body: "Looks good".into(),
            drafts: drafts(),
            head: Some(HEAD.into()),
        })
        .unwrap();
    assert_eq!(receipt.summary, "Approved");
    let mr = format!("{PROJECT}/merge_requests/13");
    let writes = harness.writes();
    assert_eq!(
        writes
            .iter()
            .map(|(method, path, _)| format!("{method} {}", path.trim_start_matches(&mr)))
            .collect::<Vec<_>>(),
        [
            "POST /draft_notes",
            "POST /draft_notes",
            "POST /draft_notes",
            "POST /draft_notes/bulk_publish",
            "POST /approve"
        ]
    );
    assert_eq!(writes[0].2["note"], "Name the count");
    assert_eq!(writes[0].2["position"]["new_line"], 12);
    assert_eq!(writes[0].2["position"]["head_sha"], HEAD);
    assert_eq!(writes[2].2, json!({"note": "Looks good"}));
    assert_eq!(writes[4].2, json!({"sha": HEAD}));

    let pull = some(
        harness.read(Read::PullRequest(13)).unwrap(),
        |payload| match payload {
            Payload::PullRequest(pull) => Some(pull),
            _ => None,
        },
    );
    assert!(
        pull.reviews
            .iter()
            .any(|review| review.author.login == "tester" && review.state == ReviewState::Approved)
    );
    let threads = some(
        harness.read(Read::Threads(13)).unwrap(),
        |payload| match payload {
            Payload::Threads(threads) => Some(threads),
            _ => None,
        },
    );
    assert_eq!(threads.len(), 4);

    // Approving a head that moved is refused before anything is sent.
    harness.clear();
    let moved = harness.write(Write::SubmitReview {
        number: 13,
        verdict: Verdict::Approve,
        body: String::new(),
        drafts: drafts(),
        head: Some("0".repeat(40)),
    });
    assert!(matches!(moved, Err(ForgeError::Conflict(_))), "{moved:?}");
    assert!(harness.writes().is_empty());
}

/// Before GitLab 15.9 there are no draft notes: the first 404 posts the rest one by one and the
/// capability turns off for the host.
pub fn review_without_draft_notes(harness: &Harness) {
    harness.seeded(|world| world["draft_notes"] = json!(false));
    assert_eq!(
        harness.provider.capabilities(&harness.ctx).batched_review,
        Cap::Probe
    );
    harness
        .write(Write::SubmitReview {
            number: 13,
            verdict: Verdict::Comment,
            body: "Two notes".into(),
            drafts: drafts(),
            head: Some(HEAD.into()),
        })
        .unwrap();
    let mr = format!("{PROJECT}/merge_requests/13");
    assert_eq!(
        harness
            .writes()
            .iter()
            .map(|(method, path, _)| format!("{method} {}", path.trim_start_matches(&mr)))
            .collect::<Vec<_>>(),
        [
            "POST /draft_notes",
            "POST /discussions",
            "POST /discussions",
            "POST /notes"
        ]
    );
    assert!(matches!(
        harness.provider.capabilities(&harness.ctx).batched_review,
        Cap::No(_)
    ));
}

pub fn request_changes_is_probed(harness: &Harness) {
    harness.seeded(|_| {});
    let receipt = harness
        .write(Write::SubmitReview {
            number: 13,
            verdict: Verdict::RequestChanges,
            body: String::new(),
            drafts: Vec::new(),
            head: None,
        })
        .unwrap();
    assert_eq!(receipt.summary, "Changes requested");
    let operations = harness
        .requests()
        .iter()
        .filter_map(|request| request["operation"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(operations, ["MutationFields", "MergeRequestRequestChanges"]);
    let mutation = harness.writes().pop().unwrap();
    assert_eq!(
        mutation.2["variables"],
        json!({"projectPath": "orbit/team/orbit", "iid": "13"})
    );
    assert_eq!(
        harness
            .provider
            .capabilities(&harness.ctx)
            .review_request_changes,
        Cap::Yes
    );

    // The probe is remembered for the host.
    harness.clear();
    harness
        .write(Write::SubmitReview {
            number: 10,
            verdict: Verdict::RequestChanges,
            body: String::new(),
            drafts: Vec::new(),
            head: None,
        })
        .unwrap();
    assert!(
        harness
            .requests()
            .iter()
            .all(|request| request["operation"] != "MutationFields")
    );
    let pull = some(
        harness.read(Read::PullRequest(10)).unwrap(),
        |payload| match payload {
            Payload::PullRequest(pull) => Some(pull),
            _ => None,
        },
    );
    assert_eq!(pull.row.review, Some(ReviewState::ChangesRequested));
}

pub fn request_changes_unavailable_sends_nothing(harness: &Harness) {
    harness.seeded(|world| world["graphql_mutations"] = json!(["mergeRequestAccept"]));
    let refused = harness.write(Write::SubmitReview {
        number: 13,
        verdict: Verdict::RequestChanges,
        body: "Please back off between tries".into(),
        drafts: drafts(),
        head: Some(HEAD.into()),
    });
    assert!(
        matches!(
            refused,
            Err(ForgeError::Unsupported {
                capability: "review_request_changes",
                ..
            })
        ),
        "{refused:?}"
    );
    assert!(harness.writes().is_empty(), "{:?}", harness.writes());
    assert!(matches!(
        harness
            .provider
            .capabilities(&harness.ctx)
            .review_request_changes,
        Cap::No(_)
    ));
}

pub fn merge_pins_head(harness: &Harness) {
    harness.seeded(|_| {});
    let head = format!("10{}", &HEAD[2..]);
    let moved = harness.write(Write::Merge {
        number: 10,
        method: MergeMethod::Squash,
        head: "0".repeat(40),
        delete_branch: true,
        title: None,
        message: None,
    });
    assert!(matches!(moved, Err(ForgeError::Conflict(_))), "{moved:?}");
    assert!(harness.writes().is_empty());

    let receipt = harness
        .write(Write::Merge {
            number: 10,
            method: MergeMethod::Squash,
            head: head.clone(),
            delete_branch: true,
            title: Some("Document approvals".into()),
            message: None,
        })
        .unwrap();
    assert_eq!(receipt.summary, "Merged");
    assert_eq!(
        harness.writes(),
        [(
            "PUT".to_owned(),
            format!("{PROJECT}/merge_requests/10/merge"),
            json!({"sha": head, "squash": true, "should_remove_source_branch": true,
                   "squash_commit_message": "Document approvals"})
        )]
    );
    assert_eq!(
        harness.rows(ItemKind::PullRequest, StateFilter::Merged, ""),
        [10, 12]
    );

    // The server refusing a moved head is the same conflict.
    harness.update(|state| {
        state["fail"] = json!([{"method": "PUT", "path": "merge_requests/13/merge", "status": 409,
                                "message": "SHA does not match HEAD of source branch"}])
    });
    let refused = harness.write(Write::Merge {
        number: 13,
        method: MergeMethod::Merge,
        head: HEAD.into(),
        delete_branch: false,
        title: None,
        message: None,
    });
    assert!(
        matches!(&refused, Err(ForgeError::Conflict(message)) if message.contains("moved")),
        "{refused:?}"
    );
}

pub fn merge_waits_for_running_pipeline(harness: &Harness) {
    harness.seeded(|_| {});
    let receipt = harness
        .write(Write::Merge {
            number: 11,
            method: MergeMethod::Merge,
            head: format!("11{}", &HEAD[2..]),
            delete_branch: false,
            title: None,
            message: None,
        })
        .unwrap();
    assert_eq!(receipt.summary, "Merges when the pipeline succeeds");
    let (_, path, body) = harness.writes().pop().unwrap();
    assert_eq!(path, format!("{PROJECT}/merge_requests/11/merge"));
    assert_eq!(body["merge_when_pipeline_succeeds"], true);
    assert_eq!(body["squash"], false);
}

// ------------------------------------------------------------------------------ pipelines

pub fn pipelines(harness: &Harness) {
    let runs = some(
        harness
            .read(Read::Runs {
                filter: RunFilter::default(),
                page: Page::first(20),
            })
            .unwrap(),
        |payload| match payload {
            Payload::Runs(runs) => Some(runs),
            _ => None,
        },
    );
    assert_eq!(
        runs.items
            .iter()
            .map(|run| run.id.0.as_str())
            .collect::<Vec<_>>(),
        ["91", "90", "89"]
    );
    assert_eq!(runs.total, Some(3));
    let failed = &runs.items[0];
    assert_eq!(failed.conclusion, Some(CiState::Failed));
    assert_eq!(failed.conclusion_raw.as_deref(), Some("failure"));
    assert_eq!(failed.pr_number, Some(13));
    assert_eq!(failed.status, RunStatus::Completed);
    assert_eq!(failed.number, 207);
    assert_eq!(runs.items[2].status, RunStatus::InProgress);
    assert_eq!(runs.items[2].conclusion, None);
    let main = some(
        harness
            .read(Read::Runs {
                filter: RunFilter {
                    branch: Some("main".into()),
                    ..RunFilter::default()
                },
                page: Page::first(20),
            })
            .unwrap(),
        |payload| match payload {
            Payload::Runs(runs) => Some(runs),
            _ => None,
        },
    );
    assert_eq!(main.items.len(), 1);
    assert_eq!(main.items[0].conclusion_raw.as_deref(), Some("success"));

    // F6: the workflows panel lists pipeline sources (one `.gitlab-ci.yml`), newest first.
    let workflows = some(
        harness.read(Read::Workflows).unwrap(),
        |payload| match payload {
            Payload::Workflows(workflows) => Some(workflows),
            _ => None,
        },
    );
    assert_eq!(
        workflows
            .iter()
            .map(|workflow| (workflow.id.0.as_str(), workflow.name.as_str()))
            .collect::<Vec<_>>(),
        [("merge_request_event", "Merge request"), ("push", "Push")]
    );
    assert!(
        workflows
            .iter()
            .all(|workflow| workflow.path == ".gitlab-ci.yml")
    );
    // A merge request pipeline shows its source branch, not `refs/merge-requests/N/head`.
    assert_eq!(failed.branch, "retry-uploads");
    let detail = some(
        harness
            .read(Read::Run {
                id: ItemId::new(91),
                attempt: None,
            })
            .unwrap(),
        |payload| match payload {
            Payload::Run(detail) => Some(detail),
            _ => None,
        },
    );
    assert_eq!(
        detail
            .jobs
            .iter()
            .map(|job| (
                job.name.as_str(),
                job.stage.as_deref().unwrap(),
                job.conclusion
            ))
            .collect::<Vec<_>>(),
        [
            ("test", "test", Some(CiState::Failed)),
            ("clippy", "test", Some(CiState::Passed)),
            ("build", "build", Some(CiState::Passed)),
            ("deploy", "deploy", Some(CiState::Skipped)),
        ]
    );
    assert_eq!(detail.failure_summary.as_ref().unwrap().job.0, "42");
    assert!(
        detail
            .jobs
            .iter()
            .all(|job| job.steps == Steps::FromLogSections)
    );

    let job = some(
        harness.read(Read::Job(ItemId::new(42))).unwrap(),
        |payload| match payload {
            Payload::Job(job) => Some(job),
            _ => None,
        },
    );
    assert_eq!(
        (job.name.as_str(), job.conclusion_raw.as_deref()),
        ("test", Some("failure"))
    );
    let log = some(
        harness.read(Read::JobLog(ItemId::new(42))).unwrap(),
        |payload| match payload {
            Payload::Log(log) => Some(log),
            _ => None,
        },
    );
    assert_eq!(log.format, LogFormat::GitLabSections);
    assert!(
        log.text.contains("section_start:1763100004:step_script"),
        "{}",
        log.text
    );
    assert!(!log.truncated);
    let source = some(
        harness
            .read(Read::WorkflowSource {
                id: ItemId::default(),
                reference: Some("main".into()),
            })
            .unwrap(),
        |payload| match payload {
            Payload::Text(text) => Some(text),
            _ => None,
        },
    );
    assert!(source.starts_with("stages:"));
    // F6: a pipeline's artifacts are its jobs' archives.
    let artifacts = some(
        harness.read(Read::Artifacts(ItemId::new(91))).unwrap(),
        |payload| match payload {
            Payload::Artifacts(artifacts) => Some(artifacts),
            _ => None,
        },
    );
    assert_eq!(
        artifacts
            .iter()
            .map(|artifact| (artifact.id.0.as_str(), artifact.name.as_str()))
            .collect::<Vec<_>>(),
        [("44", "build")]
    );
}

pub fn pipeline_writes(harness: &Harness) {
    harness.seeded(|_| {});
    harness
        .write(Write::Rerun {
            run: ItemId::new(91),
            scope: RerunScope::Failed,
            debug: false,
        })
        .unwrap();
    harness.write(Write::RerunJob(ItemId::new(43))).unwrap();
    harness.write(Write::CancelRun(ItemId::new(89))).unwrap();
    harness
        .write(Write::Dispatch {
            workflow: ItemId::default(),
            reference: "main".into(),
            inputs: vec![("DEPLOY".into(), "1".into())],
        })
        .unwrap();
    let again = harness
        .write(Write::Rerun {
            run: ItemId::new(90),
            scope: RerunScope::All,
            debug: false,
        })
        .unwrap();
    assert_eq!(again.summary, "New pipeline started");
    assert_eq!(
        harness.writes(),
        [
            (
                "POST".to_owned(),
                format!("{PROJECT}/pipelines/91/retry"),
                json!({})
            ),
            (
                "POST".to_owned(),
                format!("{PROJECT}/jobs/43/retry"),
                json!({})
            ),
            (
                "POST".to_owned(),
                format!("{PROJECT}/pipelines/89/cancel"),
                json!({})
            ),
            (
                "POST".to_owned(),
                format!("{PROJECT}/pipeline"),
                json!({"ref": "main", "variables": [{"key": "DEPLOY", "value": "1"}]})
            ),
            (
                "POST".to_owned(),
                format!("{PROJECT}/pipeline"),
                json!({"ref": "main"})
            ),
        ]
    );
    let runs = some(
        harness
            .read(Read::Runs {
                filter: RunFilter::default(),
                page: Page::first(20),
            })
            .unwrap(),
        |payload| match payload {
            Payload::Runs(runs) => Some(runs),
            _ => None,
        },
    );
    assert_eq!(runs.items.len(), 5);
    let cancelled = runs.items.iter().find(|run| run.id.0 == "89").unwrap();
    assert_eq!(cancelled.conclusion, Some(CiState::Cancelled));
}

// ------------------------------------------------------------------------------ To-dos

pub fn todos(harness: &Harness) {
    let pending = |all| {
        some(
            harness
                .read(Read::Notifications {
                    filter: NotifyFilter { all, since: None },
                    page: Page::first(50),
                })
                .unwrap(),
            |payload| match payload {
                Payload::Notifications(page) => Some(page),
                _ => None,
            },
        )
    };
    let page = pending(false);
    assert_eq!(
        page.items
            .iter()
            .map(|todo| todo.id.0.as_str())
            .collect::<Vec<_>>(),
        ["9001", "9002"]
    );
    let review = &page.items[0];
    assert_eq!(review.reason, NotifyReason::ReviewRequested);
    assert_eq!(
        review.target,
        NotifyTarget::Item {
            kind: ItemKind::PullRequest,
            number: 13
        }
    );
    assert_eq!(review.repo, "orbit/team/orbit");
    assert_eq!(review.title, "Retry flaky artifact uploads");
    assert!(review.unread);
    assert_eq!(page.items[1].reason, NotifyReason::Mentioned);
    assert_eq!(
        page.items[1].target,
        NotifyTarget::Item {
            kind: ItemKind::Issue,
            number: 14
        }
    );
    let all = pending(true);
    assert_eq!(all.items.len(), 3);
    assert!(!all.items[2].unread);
    assert_eq!(all.items[2].reason, NotifyReason::CiActivity);

    harness.clear();
    harness.write(Write::MarkRead(ItemId::new(9001))).unwrap();
    assert_eq!(pending(false).items.len(), 1);
    harness.write(Write::MarkAllRead).unwrap();
    assert!(pending(false).items.is_empty());
    assert_eq!(
        harness
            .writes()
            .iter()
            .map(|(method, path, _)| format!("{method} {path}"))
            .collect::<Vec<_>>(),
        ["POST todos/9001/mark_as_done", "POST todos/mark_as_done"]
    );
}

// ------------------------------------------------------------------------------ refusals

pub fn unsupported_writes_send_nothing(harness: &Harness) {
    let capabilities = harness.provider.capabilities(&harness.ctx);
    assert!(matches!(capabilities.update_branch, Cap::No(_)));
    assert!(matches!(capabilities.issue_not_planned, Cap::No(_)));
    assert!(matches!(capabilities.projects, Cap::No(_)));
    assert!(matches!(capabilities.approve_fork_run, Cap::No(_)));
    assert_eq!(
        capabilities.merge_methods,
        [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase]
    );
    // The workflow filter is the pipeline source (F6).
    assert!(capabilities.run_filters.workflow);
    let refused = [
        Write::SetProjects {
            kind: ItemKind::Issue,
            number: 14,
            add: vec!["Roadmap".into()],
            remove: Vec::new(),
        },
        Write::SetState {
            kind: ItemKind::Issue,
            number: 14,
            open: false,
            reason: Some(ClosedReason::NotPlanned),
        },
        Write::ApproveRun {
            run: ItemId::new(91),
            expected: dust::forge::request::RunIdentity {
                sha: HEAD.into(),
                branch: "main".into(),
                head_owner: "orbit".into(),
            },
        },
        Write::Unsubscribe(ItemId::new(9001)),
    ];
    for write in refused {
        let result = harness.write(write.clone());
        let Err(ForgeError::Unsupported { reason, .. }) = &result else {
            panic!("{write:?} → {result:?}");
        };
        assert!(!reason.is_empty());
    }
    assert!(harness.requests().is_empty(), "{:?}", harness.requests());
}

pub fn read_only_navigation_makes_no_writes(harness: &Harness) {
    let repo = some(
        harness.read(Read::Repository).unwrap(),
        |payload| match payload {
            Payload::Repo(repo) => Some(repo),
            _ => None,
        },
    );
    assert_eq!(
        (repo.full_name.as_str(), repo.default_branch.as_str()),
        ("orbit/team/orbit", "main")
    );
    assert!(repo.permissions.push && repo.permissions.admin);
    assert!(repo.delete_branch_on_merge);
    let viewer = some(
        harness.read(Read::Viewer).unwrap(),
        |payload| match payload {
            Payload::User(user) => Some(user),
            _ => None,
        },
    );
    assert_eq!(viewer.login, "tester");
    let labels = some(
        harness.read(Read::Labels).unwrap(),
        |payload| match payload {
            Payload::Labels(labels) => Some(labels),
            _ => None,
        },
    );
    assert_eq!(labels.len(), 4);
    assert_eq!(labels[0].color.as_deref(), Some("#D10069"));
    let milestones = some(
        harness.read(Read::Milestones).unwrap(),
        |payload| match payload {
            Payload::Milestones(milestones) => Some(milestones),
            _ => None,
        },
    );
    assert_eq!(milestones[0].title, "v1.4");
    let people = some(
        harness.read(Read::Collaborators).unwrap(),
        |payload| match payload {
            Payload::Users(users) => Some(users),
            _ => None,
        },
    );
    assert_eq!(people.len(), 4);
    for read in [
        Read::PullRequest(13),
        Read::Issue(14),
        Read::Threads(13),
        Read::Timeline(13),
        Read::Diff(13),
        Read::Viewed(13),
        Read::Run {
            id: ItemId::new(91),
            attempt: None,
        },
        Read::JobLog(ItemId::new(42)),
    ] {
        harness.read(read).unwrap();
    }
    harness.rows(ItemKind::PullRequest, StateFilter::Open, "");
    assert!(harness.writes().is_empty(), "{:?}", harness.writes());
    harness.assert_transport();
}

/// The nested group travels as one encoded project id; a fork's pipeline jobs are read in the
/// source project by numeric id.
pub fn nested_group_project_ids(harness: &Harness) {
    harness.read(Read::PullRequest(13)).unwrap();
    harness
        .read(Read::Runs {
            filter: RunFilter::default(),
            page: Page::first(5),
        })
        .unwrap();
    let paths = harness
        .requests()
        .iter()
        .map(|request| request["path"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        paths.iter().all(|path| !path.contains("orbit/team")),
        "{paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|path| path == &format!("{PROJECT}/merge_requests/13"))
    );
    assert!(
        paths
            .iter()
            .any(|path| path == "projects/6329679/pipelines/91/jobs")
    );
    let lists = harness
        .requests()
        .into_iter()
        .filter(|request| request["operation"] == "MergeRequestList")
        .count();
    assert_eq!(lists, 0);
    harness.rows(ItemKind::PullRequest, StateFilter::Open, "");
    let list = harness
        .requests()
        .into_iter()
        .find(|request| request["operation"] == "MergeRequestList")
        .unwrap();
    assert_eq!(list["variables"]["path"], "orbit/team/orbit");
}

// ------------------------------------------------------------------------------ transports

/// The same reads and writes over HTTP and `glab api` give the same values and requests.
pub fn transports_agree() {
    let snapshot = |harness: &Harness| {
        let mut seen = Vec::new();
        for read in [
            Read::Repository,
            Read::List {
                kind: ItemKind::PullRequest,
                filter: Filter::default(),
                page: Page::first(30),
            },
            Read::List {
                kind: ItemKind::Issue,
                filter: Filter::default(),
                page: Page::first(30),
            },
            Read::PullRequest(13),
            Read::Issue(14),
            Read::Threads(13),
            Read::Timeline(13),
            Read::Diff(13),
            Read::Runs {
                filter: RunFilter::default(),
                page: Page::first(30),
            },
            Read::Run {
                id: ItemId::new(91),
                attempt: None,
            },
            Read::JobLog(ItemId::new(42)),
            Read::Notifications {
                filter: NotifyFilter::default(),
                page: Page::first(30),
            },
        ] {
            seen.push(format!("{:?}", harness.read(read).unwrap()));
        }
        for write in [
            Write::Comment {
                kind: ItemKind::PullRequest,
                number: 13,
                body: "Same".into(),
            },
            Write::SubmitReview {
                number: 13,
                verdict: Verdict::Approve,
                body: String::new(),
                drafts: drafts(),
                head: Some(HEAD.into()),
            },
            Write::Merge {
                number: 10,
                method: MergeMethod::Merge,
                head: format!("10{}", &HEAD[2..]),
                delete_branch: false,
                title: None,
                message: None,
            },
        ] {
            seen.push(harness.write(write).unwrap().summary);
        }
        seen.push(format!(
            "{:?}",
            harness.read(Read::PullRequest(13)).unwrap()
        ));
        let logical = harness
            .requests()
            .iter()
            .map(|request| {
                format!(
                    "{} {} {} {}",
                    request["method"], request["path"], request["operation"], request["body"]
                )
            })
            .collect::<Vec<_>>();
        harness.assert_transport();
        (seen, logical)
    };
    let http = Harness::new(Via::Http);
    let glab = Harness::new(Via::Glab);
    let (http_seen, http_requests) = snapshot(&http);
    let (glab_seen, glab_requests) = snapshot(&glab);
    for (left, right) in http_seen.iter().zip(&glab_seen) {
        assert_eq!(left, right);
    }
    assert_eq!(http_seen.len(), glab_seen.len());
    assert_eq!(http_requests, glab_requests);
}
