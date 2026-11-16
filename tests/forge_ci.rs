#![cfg(unix)]
//! CI everywhere (FORGE.md F6) and the F3 speed items, against the fake forge: every scenario
//! runs for GitHub (`server.py --world actions`, `world_actions.py`), GitLab (`--world gitlab`),
//! Gitea and Forgejo (`--world gitea|forgejo`, recorded), each over direct HTTP and over the
//! forge's CLI (`gh`, `glab`, `tea` passthrough). Test names are `<forge>_<transport>::<scenario>`.
//!
//! Runs, jobs, steps (known, cut from log sections, unavailable), logs with ANSI kept, rerun,
//! cancel, dispatch, fork-run approval with its identity check, artifacts (list, browse,
//! download), version gates and the task fallback, check dedupe and rollup; then the speed
//! items measured on the fake's request log: an incremental `since` refresh merged into the
//! cached copy, alias batching, in-flight coalescing, ETag revalidation and the polling cadence.
//!
//! Each scenario runs in a child of this binary with its own `DUSTY_FORGE_STATE_DIR`, so the
//! process-wide stores (ETags, batches, flights, probed capabilities) start empty.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dust::forge::cache;
use dust::forge::cache::Cache;
use dust::forge::cache::CacheKey;
use dust::forge::capability::Cap;
use dust::forge::cli::CliTransport;
use dust::forge::credential::Credential;
use dust::forge::credential::CredentialSource;
use dust::forge::credential::Env;
use dust::forge::credential::Secret;
use dust::forge::detect;
use dust::forge::http::HttpTransport;
use dust::forge::model::logs;
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
use std::time::Duration;
use std::time::Instant;

const GITEA_HEAD: &str = "c92ad11000000000000000000000000000000000";
const FORK_SHA: &str = "ffffffffffffffffffffffffffffffffffffffff";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Forge {
    GitHub,
    GitLab,
    Gitea,
    Forgejo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Via {
    Http,
    Cli,
}

impl Forge {
    fn world(self) -> &'static str {
        match self {
            Forge::GitHub => "actions",
            Forge::GitLab => "gitlab",
            Forge::Gitea => "gitea",
            Forge::Forgejo => "forgejo",
        }
    }

    fn host(self) -> &'static str {
        match self {
            Forge::GitHub => "github.com",
            Forge::GitLab => "gitlab.ci.test",
            Forge::Gitea => "gitea.com",
            Forge::Forgejo => "code.forgejo.org",
        }
    }

    fn kind(self) -> ProviderKind {
        match self {
            Forge::GitHub => ProviderKind::GitHub,
            Forge::GitLab => ProviderKind::GitLab,
            Forge::Gitea => ProviderKind::Gitea,
            Forge::Forgejo => ProviderKind::Forgejo,
        }
    }

    fn repo(self) -> &'static str {
        match self {
            Forge::GitHub => "octo/orbit",
            Forge::GitLab => "orbit/team/orbit",
            Forge::Gitea | Forge::Forgejo => "orbit/orbit",
        }
    }

    /// The version the recorded Gitea/Forgejo answered.
    fn version(self) -> &'static str {
        match self {
            Forge::Forgejo => "17.0.0-26-363f3dc30b+gitea-1.22.0",
            _ => "29.0.0+dev-160-g55186ca39b",
        }
    }

    /// A run with a failed job, its jobs and an artifact.
    fn run(self) -> &'static str {
        match self {
            Forge::GitHub => "501",
            Forge::GitLab | Forge::Gitea => "91",
            Forge::Forgejo => "200481",
        }
    }

    /// `(artifact id, name, files inside)`.
    fn artifact(self) -> (&'static str, &'static str, &'static [&'static str]) {
        match self {
            Forge::GitHub => ("701", "coverage", &["coverage/index.html", "lcov.info"]),
            Forge::GitLab => (
                "44",
                "build",
                &["target/release/orbit", "coverage/lcov.info"],
            ),
            Forge::Gitea => ("7", "coverage", &["report.txt"]),
            Forge::Forgejo => ("45938", "forgejo-runner", &["report.txt"]),
        }
    }
}

macro_rules! expect {
    ($value:expr, $variant:path) => {
        match $value {
            $variant(inner) => inner,
            other => panic!("expected {}: {other:?}", stringify!($variant)),
        }
    };
}

/// Run `scenario` in a child of this binary with its own forge state directory.
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
    // Measurements the child printed (request counts) reach the parent's output.
    for line in stdout.lines() {
        if let Some(at) = line.find("forge_ci:") {
            println!("{}", &line[at..]);
        }
    }
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{name} failed in its isolated child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub struct Harness {
    directory: tempfile::TempDir,
    server: ForgeServer,
    cli: PathBuf,
    forge: Forge,
    via: Via,
}

impl Harness {
    pub fn new(forge: Forge, via: Via) -> Self {
        Self::with_version(forge, via, Some(forge.version()))
    }

    /// `version`: what `GET version` answers on Gitea/Forgejo (`None`: 404, version unknown).
    pub fn with_version(forge: Forge, via: Via, version: Option<&str>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut state = json!({ "glab_auth": { "*": "token" } });
        if let Some(version) = version
            && matches!(forge, Forge::Gitea | Forge::Forgejo)
        {
            state["probes"] =
                json!({ forge.host(): { "api/v1/version": { "body": { "version": version } } } });
        }
        std::fs::write(&path, state.to_string()).unwrap();
        let server = ForgeServer::start(&path, forge.world());
        let bin = directory.path().join("bin");
        let cli = match forge {
            Forge::GitHub => server.install_gh(&bin),
            Forge::GitLab => server.install_glab(&bin),
            Forge::Gitea | Forge::Forgejo => server.install_tea(
                &bin,
                json!([{"name": "work", "url": format!("https://{}", forge.host()),
                        "user": "maya", "default": true}]),
            ),
        };
        Self {
            directory,
            server,
            cli,
            forge,
            via,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    fn state(&self) -> Value {
        forge_fake::state(&self.state_path())
    }

    /// Under the fixture's state lock, so a change never races the server's own save.
    fn update(&self, change: impl FnOnce(&mut Value)) {
        forge_fake::update(&self.state_path(), change);
    }

    fn requests(&self) -> Vec<Value> {
        forge_fake::requests(&self.state_path())
            .into_iter()
            .filter(|request| request["path"] != "auth status" && request["path"] != "auth token")
            .collect()
    }

    fn clear(&self) {
        self.update(|state| state["requests"] = json!([]));
    }

    /// Requests whose path ends with `tail` (and the GraphQL ones when `tail` is `graphql`).
    fn count(&self, tail: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request["path"].as_str().unwrap_or("").ends_with(tail))
            .count()
    }

    fn paths(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| {
                format!(
                    "{} {}",
                    request["method"].as_str().unwrap_or(""),
                    request["path"].as_str().unwrap_or("")
                )
            })
            .collect()
    }

    fn endpoints(&self) -> Endpoints {
        let host = self.forge.host();
        match self.forge {
            Forge::GitHub => {
                let mut endpoints = Endpoints::github(&HostKey::new(host));
                endpoints.override_api_base(&format!("{}/{host}/api/v3", self.server.url()));
                endpoints
            }
            Forge::GitLab => detect::endpoints_for(
                ProviderKind::GitLab,
                &HostKey::new(host),
                None,
                &Env::from_pairs([("DUSTY_FORGE_ORIGIN", self.server.origin())]),
            ),
            Forge::Gitea | Forge::Forgejo => detect::endpoints_for(
                self.forge.kind(),
                &HostKey::new(host),
                Some(&format!("{}/{host}/api/v1", self.server.url())),
                &Env::from_pairs::<&str, &str>([]),
            ),
        }
    }

    fn transport(&self) -> Arc<dyn Transport> {
        match (self.via, self.forge) {
            (Via::Http, forge) => Arc::new(
                HttpTransport::new(
                    self.endpoints(),
                    Credential {
                        token: Secret::new(FAKE_TOKEN),
                        source: match forge {
                            Forge::GitHub => CredentialSource::GhAuthToken,
                            Forge::GitLab => CredentialSource::Env("GITLAB_TOKEN"),
                            _ => CredentialSource::Env("GITEA_TOKEN"),
                        },
                        user: None,
                        obtained: Instant::now(),
                    },
                )
                .unwrap(),
            ),
            (Via::Cli, Forge::GitHub) => {
                Arc::new(CliTransport::new(Cli::Gh, self.cli.clone(), None))
            }
            (Via::Cli, Forge::GitLab) => {
                Arc::new(CliTransport::new(Cli::Glab, self.cli.clone(), None))
            }
            (Via::Cli, _) => Arc::new(CliTransport::new(
                Cli::Tea,
                self.cli.clone(),
                Some("work".into()),
            )),
        }
    }

    fn ctx(&self) -> ForgeContext {
        let mut options = ConnectOptions::default();
        match self.forge {
            Forge::GitHub => options.gh = Some(self.cli.clone()),
            Forge::GitLab => options.glab = Some(self.cli.clone()),
            Forge::Gitea | Forge::Forgejo => options.tea = Some(self.cli.clone()),
        }
        ForgeContext::new(
            self.directory.path().to_path_buf(),
            self.endpoints(),
            options,
            RepoRef::parse(self.forge.repo()),
            self.transport(),
        )
    }

    fn provider(&self) -> Arc<dyn Provider> {
        provider::build(self.forge.kind()).unwrap()
    }

    fn fetch(&self, ctx: &ForgeContext, read: &Read, cond: &Conditional) -> Fetched {
        self.provider()
            .read(ctx, read, cond, &AtomicBool::new(false))
            .unwrap_or_else(|error| panic!("{read:?}: {error:?}"))
    }

    fn try_read(&self, read: Read) -> Result<Payload, ForgeError> {
        match self.provider().read(
            &self.ctx(),
            &read,
            &Conditional::default(),
            &AtomicBool::new(false),
        )? {
            Fetched::Payload { value, .. } => Ok(value),
            Fetched::NotModified => panic!("{read:?} came back not modified"),
        }
    }

    fn read(&self, read: Read) -> Payload {
        self.try_read(read.clone())
            .unwrap_or_else(|error| panic!("{read:?}: {error:?}"))
    }

    fn write(&self, write: Write) -> Result<Receipt, ForgeError> {
        self.provider()
            .write(&self.ctx(), &write, &AtomicBool::new(false))
    }

    /// Learn the server version (Gitea/Forgejo) and seed the stateful worlds, then forget
    /// what that sent.
    fn warm(&self) {
        let _ = self.provider().capabilities(&self.ctx());
        if matches!(self.forge, Forge::GitHub | Forge::GitLab) {
            let _ = self.try_read(Read::Viewer);
        }
        self.clear();
    }

    fn assert_transport(&self) {
        let requests = self.requests();
        assert!(!requests.is_empty());
        let expected = match (self.via, self.forge) {
            (Via::Http, _) => "http",
            (Via::Cli, Forge::GitHub) => "gh",
            (Via::Cli, Forge::GitLab) => "glab",
            (Via::Cli, _) => "tea",
        };
        for request in &requests {
            if request["path"] == "porcelain" {
                continue;
            }
            assert_eq!(request["transport"], expected, "{request}");
            if self.via == Via::Http && request["path"] != "version" {
                assert_eq!(request["auth"], "valid", "{request}");
            }
        }
    }
}

fn runs(filter: RunFilter) -> Read {
    Read::Runs {
        filter,
        page: Page::first(20),
    }
}

fn run_ids(payload: Payload) -> Vec<String> {
    expect!(payload, Payload::Runs)
        .items
        .iter()
        .map(|run| run.id.0.clone())
        .collect()
}

fn notifications() -> Read {
    Read::Notifications {
        filter: NotifyFilter::default(),
        page: Page::first(50),
    }
}

// ---------------------------------------------------------------------------- runs and logs

pub fn runs_jobs_steps_logs(harness: &Harness) {
    harness.warm();
    match harness.forge {
        Forge::GitHub => {
            let all = expect!(harness.read(runs(RunFilter::default())), Payload::Runs);
            assert_eq!(
                all.items
                    .iter()
                    .map(|run| run.id.0.as_str())
                    .collect::<Vec<_>>(),
                ["503", "502", "501"]
            );
            assert_eq!(all.total, Some(3));
            assert!(all.items[1].needs_approval);
            assert_eq!(all.items[2].pr_number, Some(9));
            assert_eq!(all.items[2].attempt, 2);
            assert_eq!(
                run_ids(harness.read(runs(RunFilter {
                    workflow: Some(ItemId::new(31)),
                    ..RunFilter::default()
                }))),
                ["502", "501"]
            );
            assert_eq!(
                run_ids(harness.read(runs(RunFilter {
                    status: Some("action_required".into()),
                    ..RunFilter::default()
                }))),
                ["502"]
            );
            let detail = *expect!(
                harness.read(Read::Run {
                    id: ItemId::new(501),
                    attempt: None,
                }),
                Payload::Run
            );
            assert_eq!(detail.run.attempts, 2);
            assert!(matches!(&detail.jobs[0].steps, Steps::Known(steps) if steps.len() == 3));
            assert_eq!(detail.jobs[1].steps, Steps::FromLogSections);
            assert!(
                matches!(&detail.jobs[2].steps, Steps::Unavailable(reason) if reason.contains("never ran"))
            );
            assert_eq!(detail.failure_summary.as_ref().unwrap().job.0, "602");
            // An earlier attempt keeps its own jobs.
            let first = *expect!(
                harness.read(Read::Run {
                    id: ItemId::new(501),
                    attempt: Some(1),
                }),
                Payload::Run
            );
            assert_eq!(first.run.attempt, 1);
            assert_eq!(
                first
                    .jobs
                    .iter()
                    .map(|job| job.id.0.as_str())
                    .collect::<Vec<_>>(),
                ["611", "612"]
            );
            // A job without steps is cut by its `##[group]` sections; ANSI stays in the text.
            let log = expect!(harness.read(Read::JobLog(ItemId::new(602))), Payload::Log);
            assert_eq!(log.format, LogFormat::Plain);
            assert!(log.text.contains("\x1b[31mFAILED\x1b[0m"), "{:?}", log.text);
            assert_eq!(
                log.sections
                    .iter()
                    .map(|section| (section.name.as_str(), section.start, section.end))
                    .collect::<Vec<_>>(),
                [("Set up job", 0, 3), ("Run cargo test", 3, 6)]
            );
            assert_eq!(logs::steps(&log.sections).len(), 2);
            // A job with steps: each line goes to the step running at its time.
            let job = *expect!(harness.read(Read::Job(ItemId::new(601))), Payload::Job);
            let Steps::Known(steps) = &job.steps else {
                panic!("{:?}", job.steps)
            };
            let log = expect!(harness.read(Read::JobLog(ItemId::new(601))), Payload::Log);
            assert_eq!(
                logs::split(&log, steps)
                    .iter()
                    .map(|section| (section.step.unwrap(), section.start, section.end))
                    .collect::<Vec<_>>(),
                [(1, 0, 1), (2, 1, 5), (3, 5, 6)]
            );
            // Logs live on another origin: the token never follows the redirect.
            let blobs = harness.state()["blob_requests"].clone();
            assert!(
                blobs
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|blob| blob["authorization"] == "absent"),
                "{blobs}"
            );
            // CI, Release and the turned-off Nightly, with its state (§3.37).
            let workflows = expect!(harness.read(Read::Workflows), Payload::Workflows);
            assert_eq!(workflows.len(), 3);
            assert_eq!(workflows[2].state, "disabled_manually");
        }
        Forge::GitLab => {
            let all = expect!(harness.read(runs(RunFilter::default())), Payload::Runs);
            assert_eq!(
                all.items
                    .iter()
                    .map(|run| run.id.0.as_str())
                    .collect::<Vec<_>>(),
                // Newest update first: the running pipeline started minutes ago.
                ["89", "91", "90"]
            );
            // A merge request pipeline shows the merge request's source branch.
            assert_eq!(all.items[1].branch, "retry-uploads");
            assert_eq!(all.items[1].pr_number, Some(13));
            let workflows = expect!(harness.read(Read::Workflows), Payload::Workflows);
            assert_eq!(
                workflows
                    .iter()
                    .map(|workflow| workflow.id.0.as_str())
                    .collect::<Vec<_>>(),
                // Sources in the order the newest pipelines name them.
                ["push", "merge_request_event"]
            );
            // The workflow filter is a pipeline source; the status filter is translated.
            assert_eq!(
                run_ids(harness.read(runs(RunFilter {
                    workflow: Some(workflows[0].id.clone()),
                    ..RunFilter::default()
                }))),
                ["89", "90"]
            );
            assert_eq!(
                run_ids(harness.read(runs(RunFilter {
                    status: Some("failure".into()),
                    ..RunFilter::default()
                }))),
                ["91"]
            );
            assert_eq!(
                run_ids(harness.read(runs(RunFilter {
                    status: Some("in_progress".into()),
                    ..RunFilter::default()
                }))),
                ["89"]
            );
            let detail = *expect!(
                harness.read(Read::Run {
                    id: ItemId::new(91),
                    attempt: None,
                }),
                Payload::Run
            );
            assert_eq!(detail.run.branch, "retry-uploads");
            assert_eq!(detail.jobs.len(), 4);
            // A manual allow-failure job shows as skipped.
            assert_eq!(detail.jobs[3].conclusion, Some(CiState::Skipped));
            assert!(
                detail
                    .jobs
                    .iter()
                    .all(|job| job.steps == Steps::FromLogSections)
            );
            let log = expect!(harness.read(Read::JobLog(ItemId::new(42))), Payload::Log);
            assert_eq!(log.format, LogFormat::GitLabSections);
            assert!(log.text.contains("\x1b[31;1mERROR"), "{:?}", log.text);
            assert_eq!(
                log.sections
                    .iter()
                    .map(|section| (section.name.as_str(), section.start, section.end))
                    .collect::<Vec<_>>(),
                [("Preparing environment", 1, 4), ("$ cargo test", 4, 9)]
            );
            let steps = logs::steps(&log.sections);
            assert_eq!(
                steps
                    .iter()
                    .map(|step| (step.number, step.name.as_str()))
                    .collect::<Vec<_>>(),
                [(1, "Preparing environment"), (2, "$ cargo test")]
            );
            assert_eq!(
                steps[0].completed.as_ref().unwrap().as_str(),
                "2025-11-14T06:00:04Z"
            );
        }
        Forge::Gitea => {
            let all = expect!(harness.read(runs(RunFilter::default())), Payload::Runs);
            assert_eq!(all.items[0].id, ItemId::new("91"));
            assert_eq!(
                all.items[0].workflow.as_ref().unwrap().id,
                ItemId::new("ci.yml")
            );
            // The workflow filter takes the file name (or its numeric alias from the legacy view).
            assert_eq!(
                run_ids(harness.read(runs(RunFilter {
                    workflow: Some(ItemId::new("ci.yml")),
                    ..RunFilter::default()
                }))),
                ["91"]
            );
            assert!(
                run_ids(harness.read(runs(RunFilter {
                    workflow: Some(ItemId::new("release.yml")),
                    ..RunFilter::default()
                })))
                .is_empty()
            );
            let detail = *expect!(
                harness.read(Read::Run {
                    id: ItemId::new(91),
                    attempt: None,
                }),
                Payload::Run
            );
            assert!(matches!(detail.jobs[0].steps, Steps::Known(_)));
            let log = expect!(harness.read(Read::JobLog(ItemId::new(42))), Payload::Log);
            assert_eq!(log.format, LogFormat::Plain);
            assert!(!log.text.is_empty());
        }
        Forge::Forgejo => {
            let all = expect!(harness.read(runs(RunFilter::default())), Payload::Runs);
            assert_eq!(all.items[0].id, ItemId::new("200481"));
            let job = *expect!(harness.read(Read::Job(ItemId::new(424038))), Payload::Job);
            assert!(matches!(&job.steps, Steps::Known(steps) if steps.len() == 2));
            // A job log Forgejo answers with `500 {"message":""}` is a job that never ran.
            let never = expect!(
                harness.read(Read::JobLog(ItemId::new(424038))),
                Payload::Log
            );
            assert!(never.text.contains("never ran"), "{}", never.text);
            let log = expect!(
                harness.read(Read::JobLog(ItemId::new(424039))),
                Payload::Log
            );
            assert!(!log.text.contains("never ran"));
        }
    }
    harness.assert_transport();
}

// ---------------------------------------------------------------------------- writes

pub fn rerun_cancel_dispatch(harness: &Harness) {
    harness.warm();
    let forge = harness.forge;
    let run = ItemId::new(forge.run());
    match forge {
        Forge::GitHub => {
            harness
                .write(Write::Rerun {
                    run: run.clone(),
                    scope: RerunScope::Failed,
                    debug: true,
                })
                .unwrap();
            harness.write(Write::RerunJob(ItemId::new(601))).unwrap();
            harness.write(Write::CancelRun(ItemId::new(503))).unwrap();
            // A finished run cannot be cancelled: the forge's refusal comes back as an error.
            assert!(harness.write(Write::CancelRun(ItemId::new(502))).is_err());
            harness
                .write(Write::Dispatch {
                    workflow: ItemId::new(31),
                    reference: "main".into(),
                    inputs: vec![("target".into(), "staging".into())],
                })
                .unwrap();
            assert!(
                harness
                    .write(Write::Dispatch {
                        workflow: ItemId::new(32),
                        reference: "main".into(),
                        inputs: Vec::new(),
                    })
                    .is_err()
            );
            let world = harness.state()["actions"].clone();
            assert_eq!(
                world["reruns"],
                json!([{"run": 501, "scope": "rerun-failed-jobs", "debug": true}])
            );
            assert_eq!(
                world["dispatched"],
                json!([{"workflow": 31, "ref": "main", "inputs": {"target": "staging"}}])
            );
            // The dispatched run leads the list; the rerun is on its third attempt.
            let all = expect!(harness.read(runs(RunFilter::default())), Payload::Runs);
            assert_eq!(all.items[0].event, "workflow_dispatch");
            let rerun = all.items.iter().find(|item| item.id == run).unwrap();
            assert_eq!((rerun.attempt, rerun.status), (3, RunStatus::Queued));
            let cancelled = all.items.iter().find(|item| item.id.0 == "503").unwrap();
            assert_eq!(cancelled.conclusion, Some(CiState::Cancelled));
        }
        Forge::GitLab => {
            harness
                .write(Write::Rerun {
                    run: run.clone(),
                    scope: RerunScope::Failed,
                    debug: false,
                })
                .unwrap();
            // "Rerun all" is a new pipeline on the same ref.
            harness
                .write(Write::Rerun {
                    run: run.clone(),
                    scope: RerunScope::All,
                    debug: false,
                })
                .unwrap();
            harness.write(Write::RerunJob(ItemId::new(43))).unwrap();
            harness.write(Write::CancelRun(ItemId::new(89))).unwrap();
            harness
                .write(Write::Dispatch {
                    workflow: ItemId::new("web"),
                    reference: "main".into(),
                    inputs: vec![("DEPLOY".into(), "1".into())],
                })
                .unwrap();
            let writes = harness
                .requests()
                .into_iter()
                .filter(|request| request["method"] == "POST")
                .map(|request| {
                    let path = request["path"].as_str().unwrap_or("").to_owned();
                    let tail = path.rsplit_once("orbit/").map(|(_, tail)| tail.to_owned());
                    (tail.unwrap_or(path), request["body"].clone())
                })
                .collect::<Vec<_>>();
            assert_eq!(
                writes,
                [
                    ("pipelines/91/retry".to_owned(), json!({})),
                    (
                        "pipeline".to_owned(),
                        json!({"ref": "refs/merge-requests/13/head"})
                    ),
                    ("jobs/43/retry".to_owned(), json!({})),
                    ("pipelines/89/cancel".to_owned(), json!({})),
                    (
                        "pipeline".to_owned(),
                        json!({"ref": "main", "variables": [{"key": "DEPLOY", "value": "1"}]})
                    ),
                ]
            );
        }
        Forge::Gitea | Forge::Forgejo => {
            harness.update(|state| {
                state["answers"] = json!({
                    "POST repos/orbit/orbit/actions/runs/91/rerun-failed-jobs": {"status": 201, "body": {}},
                    "POST repos/orbit/orbit/actions/runs/91/jobs/42/rerun": {"status": 201, "body": {}},
                    "POST repos/orbit/orbit/actions/jobs/424039/rerun": {"status": 201, "body": {}},
                });
            });
            harness
                .write(Write::Rerun {
                    run: ItemId::new(91),
                    scope: RerunScope::All,
                    debug: false,
                })
                .unwrap();
            harness.write(Write::CancelRun(ItemId::new(91))).unwrap();
            harness
                .write(Write::Dispatch {
                    workflow: ItemId::new("ci.yml"),
                    reference: "main".into(),
                    inputs: vec![("target".into(), "staging".into())],
                })
                .unwrap();
            let failed = harness.write(Write::Rerun {
                run: ItemId::new(91),
                scope: RerunScope::Failed,
                debug: false,
            });
            if forge == Forge::Gitea {
                failed.unwrap();
                harness.write(Write::RerunJob(ItemId::new(42))).unwrap();
            } else {
                let error = failed.unwrap_err();
                assert!(
                    matches!(&error, ForgeError::Unsupported { reason, .. } if reason.contains("whole runs")),
                    "{error:?}"
                );
                harness.write(Write::RerunJob(ItemId::new(424039))).unwrap();
            }
            let posts = harness
                .requests()
                .into_iter()
                .filter(|request| request["method"] == "POST")
                .map(|request| request["path"].as_str().unwrap_or("").to_owned())
                .collect::<Vec<_>>();
            let mut expected = vec![
                "repos/orbit/orbit/actions/runs/91/rerun",
                "repos/orbit/orbit/actions/runs/91/cancel",
                "repos/orbit/orbit/actions/workflows/ci.yml/dispatches",
            ];
            if forge == Forge::Gitea {
                expected.push("repos/orbit/orbit/actions/runs/91/rerun-failed-jobs");
                expected.push("repos/orbit/orbit/actions/runs/91/jobs/42/rerun");
            } else {
                expected.push("repos/orbit/orbit/actions/jobs/424039/rerun");
            }
            assert_eq!(posts, expected);
        }
    }
    harness.assert_transport();
}

pub fn approve(harness: &Harness) {
    harness.warm();
    let identity = |sha: &str, branch: &str, owner: &str| RunIdentity {
        sha: sha.into(),
        branch: branch.into(),
        head_owner: owner.into(),
    };
    match harness.forge {
        Forge::GitHub => {
            let run = ItemId::new(502);
            // The head moved to another fork owner since the user looked: nothing is approved.
            let moved = harness
                .write(Write::ApproveRun {
                    run: run.clone(),
                    expected: identity(FORK_SHA, "patch-1", "someone-else"),
                })
                .unwrap_err();
            assert!(
                matches!(&moved, ForgeError::Conflict(message) if message.contains("fork owner")),
                "{moved:?}"
            );
            let sha = harness
                .write(Write::ApproveRun {
                    run: run.clone(),
                    expected: identity(&"0".repeat(40), "main", "forker"),
                })
                .unwrap_err();
            assert!(
                matches!(&sha, ForgeError::Conflict(message) if message.contains("head commit and branch")),
                "{sha:?}"
            );
            // A run that is not waiting for approval is refused before anything is sent.
            let waiting = harness
                .write(Write::ApproveRun {
                    run: ItemId::new(501),
                    expected: identity(FORK_SHA, "patch-1", "forker"),
                })
                .unwrap_err();
            assert!(matches!(waiting, ForgeError::Conflict(_)), "{waiting:?}");
            assert_eq!(harness.count("/approve"), 0, "{:?}", harness.paths());
            let receipt = harness
                .write(Write::ApproveRun {
                    run: run.clone(),
                    expected: identity(FORK_SHA, "patch-1", "Forker"),
                })
                .unwrap();
            assert_eq!(receipt.summary, "Approved");
            assert_eq!(harness.count("actions/runs/502/approve"), 1);
            let after = expect!(
                harness.read(runs(RunFilter {
                    status: Some("action_required".into()),
                    ..RunFilter::default()
                })),
                Payload::Runs
            );
            assert!(after.items.is_empty());
            harness.assert_transport();
        }
        Forge::Gitea => {
            harness.update(|state| {
                state["answers"] = json!({
                    "POST repos/orbit/orbit/actions/runs/91/approve": {"status": 200, "body": {}}
                });
            });
            let moved = harness
                .write(Write::ApproveRun {
                    run: ItemId::new(91),
                    expected: identity(&"0".repeat(40), "main", ""),
                })
                .unwrap_err();
            assert!(matches!(moved, ForgeError::Conflict(_)), "{moved:?}");
            assert_eq!(harness.count("/approve"), 0);
            harness
                .write(Write::ApproveRun {
                    run: ItemId::new(91),
                    expected: identity(GITEA_HEAD, "main", ""),
                })
                .unwrap();
            assert_eq!(harness.count("actions/runs/91/approve"), 1);
            harness.assert_transport();
        }
        Forge::GitLab | Forge::Forgejo => {
            let capabilities = harness.provider().capabilities(&harness.ctx());
            let reason = capabilities.approve_fork_run.reason().unwrap().to_owned();
            assert!(!reason.is_empty());
            let error = harness
                .write(Write::ApproveRun {
                    run: ItemId::new(harness.forge.run()),
                    expected: identity(GITEA_HEAD, "main", "maya"),
                })
                .unwrap_err();
            assert!(
                matches!(&error, ForgeError::Unsupported { reason: said, .. } if said.contains(reason.split(" · ").next().unwrap())),
                "{error:?} / {reason}"
            );
            assert!(harness.requests().is_empty(), "{:?}", harness.paths());
        }
    }
}

// ---------------------------------------------------------------------------- artifacts

pub fn artifacts(harness: &Harness) {
    harness.warm();
    let forge = harness.forge;
    let (id, name, files) = forge.artifact();
    let listed = expect!(
        harness.read(Read::Artifacts(ItemId::new(forge.run()))),
        Payload::Artifacts
    );
    let artifact = listed
        .iter()
        .find(|artifact| artifact.id.0 == id)
        .unwrap_or_else(|| panic!("{id} in {listed:?}"));
    assert_eq!(artifact.name, name);
    assert!(artifact.size > 0);
    if forge == Forge::GitHub {
        assert_eq!(listed.len(), 3);
        assert!(
            matches!(&artifact.download, ArtifactSource::Url(url) if url.ends_with("/701/zip"))
        );
    }
    if forge == Forge::GitLab {
        // Only the job with an archive is an artifact; the trace is not.
        assert_eq!(listed.len(), 1);
        assert_eq!(
            artifact.expires.as_ref().map(|time| time.as_str()),
            Some("2026-12-14T05:01:35Z")
        );
    }
    let inside = expect!(
        harness.read(Read::ArtifactFiles(ItemId::new(id))),
        Payload::ArtifactFiles
    );
    assert_eq!(
        inside
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        files
    );
    assert!(inside.iter().all(|file| file.size > 0));
    let text = expect!(
        harness.read(Read::ArtifactFile {
            artifact: ItemId::new(id),
            path: files[files.len() - 1].into(),
        }),
        Payload::Text
    );
    match forge {
        Forge::GitHub | Forge::GitLab => {
            assert!(text.starts_with("TN:\nSF:src/lib.rs\n"), "{text:?}")
        }
        Forge::Gitea | Forge::Forgejo => assert_eq!(text, format!("artifact {id}\n")),
    }
    let folder = harness.directory.path().join("downloads");
    let receipt = harness
        .write(Write::DownloadArtifact {
            artifact: ItemId::new(id),
            name: format!("../{name}"),
            directory: folder.clone(),
        })
        .unwrap();
    // The name cannot leave the folder; the archive is saved whole.
    let saved = folder.join(format!("_{name}.zip"));
    assert_eq!(receipt.summary, format!("Saved {}", saved.display()));
    let bytes = std::fs::read(&saved).unwrap();
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    assert!(std::fs::read_dir(&folder).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")
    }));
    if forge == Forge::GitHub {
        assert!(
            harness
                .try_read(Read::ArtifactFiles(ItemId::new(702)))
                .is_err()
        );
    }
    if matches!(forge, Forge::GitHub | Forge::Gitea | Forge::Forgejo) {
        // Archives redirect to storage on another origin, fetched without the token.
        let blobs = harness.state()["blob_requests"].clone();
        let blobs = blobs.as_array().unwrap();
        assert!(
            blobs
                .iter()
                .any(|blob| blob["path"] == format!("/_blob/artifact-{id}.zip")),
            "{blobs:?}"
        );
        assert!(blobs.iter().all(|blob| blob["authorization"] == "absent"));
    }
    harness.assert_transport();
}

// ---------------------------------------------------------------------------- capabilities

pub fn version_gates(harness: &Harness) {
    match harness.forge {
        Forge::GitHub | Forge::GitLab => {
            harness.warm();
            let capabilities = harness.provider().capabilities(&harness.ctx());
            for cap in [
                &capabilities.runs_list,
                &capabilities.job_log,
                &capabilities.rerun_failed,
                &capabilities.cancel_run,
                &capabilities.dispatch,
                &capabilities.artifacts_list,
                &capabilities.artifacts_download,
                &capabilities.since_queries,
            ] {
                assert_eq!(*cap, Cap::Yes);
            }
            assert!(capabilities.run_filters.workflow && capabilities.run_filters.status);
            if harness.forge == Forge::GitLab {
                assert!(
                    capabilities
                        .approve_fork_run
                        .reason()
                        .unwrap()
                        .contains("merge request page")
                );
            } else {
                assert_eq!(capabilities.approve_fork_run, Cap::Yes);
            }
            assert!(harness.requests().is_empty());
        }
        Forge::Gitea | Forge::Forgejo => old_gitea(harness),
    }
}

/// Gitea 1.23 / Forgejo 8: tasks only; logs, artifacts, reruns and cancel are gated with the
/// version in the reason and nothing is sent for them.
fn old_gitea(current: &Harness) {
    let (version, words, running) = match current.forge {
        Forge::Gitea => ("1.23.4", "Gitea 1.24", "this server runs Gitea 1.23"),
        _ => (
            "8.0.3+gitea-1.22.0",
            "Forgejo 16",
            "this server runs Forgejo 8",
        ),
    };
    let harness = Harness::with_version(current.forge, current.via, Some(version));
    harness.warm();
    let capabilities = harness.provider().capabilities(&harness.ctx());
    assert_eq!(capabilities.runs_list, Cap::Yes);
    assert!(capabilities.job_log.reason().unwrap().contains(words));
    assert!(
        capabilities
            .artifacts_list
            .reason()
            .unwrap()
            .contains(words)
    );
    assert!(!capabilities.cancel_run.allowed());
    assert!(!capabilities.rerun_all.allowed());
    // The runs list falls back to `actions/tasks`; `actions/runs` is never asked.
    let listed = expect!(harness.read(runs(RunFilter::default())), Payload::Runs);
    assert!(!listed.items.is_empty());
    assert_eq!(harness.count("/actions/runs"), 0, "{:?}", harness.paths());
    assert!(harness.count("/actions/tasks") >= 1);
    let detail = *expect!(
        harness.read(Read::Run {
            id: listed.items[0].id.clone(),
            attempt: None,
        }),
        Payload::Run
    );
    let task = detail.jobs[0].id.clone();
    assert!(task.0.starts_with("task-"), "{task:?}");
    harness.clear();
    // A task has no log endpoint; a gated read or write sends nothing.
    let refused = [
        harness.try_read(Read::JobLog(task)).unwrap_err(),
        harness
            .try_read(Read::Artifacts(ItemId::new(91)))
            .unwrap_err(),
        harness
            .write(Write::CancelRun(ItemId::new(91)))
            .unwrap_err(),
        harness
            .write(Write::Rerun {
                run: ItemId::new(91),
                scope: RerunScope::All,
                debug: false,
            })
            .unwrap_err(),
        harness
            .write(Write::DownloadArtifact {
                artifact: ItemId::new(7),
                name: "coverage".into(),
                directory: harness.directory.path().to_path_buf(),
            })
            .unwrap_err(),
    ];
    for error in &refused {
        assert!(matches!(error, ForgeError::Unsupported { .. }), "{error:?}");
    }
    assert!(
        matches!(&refused[1], ForgeError::Unsupported { reason, .. } if reason.contains(words) && reason.contains(running)),
        "{:?}",
        refused[1]
    );
    assert!(harness.requests().is_empty(), "{:?}", harness.paths());

    // No version answer: the newest endpoint is probed once; its 404 is remembered and the
    // next refresh goes straight to the fallback.
    let unknown = Harness::with_version(current.forge, current.via, None);
    unknown.update(|state| {
        state["answers"] = json!({
            "GET repos/orbit/orbit/actions/runs": {"status": 404, "body": {"message": "404 Not Found"}}
        });
    });
    unknown.warm();
    assert_eq!(
        unknown.provider().capabilities(&unknown.ctx()).runs_list,
        Cap::Probe
    );
    let first = unknown.read(runs(RunFilter::default()));
    assert_eq!(unknown.count("/actions/runs"), 1, "{:?}", unknown.paths());
    unknown.clear();
    let second = unknown.read(runs(RunFilter::default()));
    assert_eq!(first, second);
    assert_eq!(unknown.count("/actions/runs"), 0, "{:?}", unknown.paths());
    assert_eq!(unknown.count("/actions/tasks"), 1);
}

// ---------------------------------------------------------------------------- checks

/// GitHub: a check that ran twice counts once (newest start), one name in two workflows is
/// prefixed, and the row's CI comes from the deduped checks, not GitHub's raw rollup.
pub fn check_dedupe_and_rollup(harness: &Harness) {
    harness.warm();
    let pull = *expect!(harness.read(Read::PullRequest(9)), Payload::PullRequest);
    let mut names = pull
        .checks
        .iter()
        .map(|check| (check.name.clone(), check.state))
        .collect::<Vec<_>>();
    names.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        names,
        [
            ("CI / lint".to_owned(), CiState::Passed),
            ("Docs / lint".to_owned(), CiState::Passed),
            ("test".to_owned(), CiState::Passed),
        ]
    );
    assert_eq!(checks::rollup(&pull.checks), Some(CiState::Passed));
    assert_eq!(pull.row.ci, Some(CiState::Passed));
    let quiet = *expect!(harness.read(Read::PullRequest(10)), Payload::PullRequest);
    assert!(quiet.checks.is_empty());
    assert_eq!(quiet.row.ci, None);
}

// ---------------------------------------------------------------------------- speed (§7.5)

/// The worker's refresh path (worker.rs `fetch`): prepare against the cache, read, merge.
fn refresh(harness: &Harness, cache: &Cache, read: &Read) -> (Payload, Option<String>) {
    let ctx = harness.ctx();
    let provider = harness.provider();
    let key = CacheKey::new(ctx.host(), ctx.repo.as_ref(), read);
    let (cond, base) = dust::forge::incremental::prepare(
        provider.as_ref(),
        &ctx,
        cache,
        &key,
        read,
        Conditional::default(),
    );
    let since = cond.since.as_ref().map(|since| since.as_str().to_owned());
    let fetched = harness.fetch(&ctx, read, &cond);
    let fetched = match base {
        Some(base) => dust::forge::incremental::merge(read, base, fetched),
        None => fetched,
    };
    let Fetched::Payload { value, .. } = fetched else {
        panic!("not modified")
    };
    cache.store(&key, &value, cache::ttl(read, Some(&value)), None, None);
    (value, since)
}

pub fn since_merges_without_full_reload(harness: &Harness) {
    harness.warm();
    let cache = Cache::at(harness.directory.path().join("cache"));
    let forge = harness.forge;
    let read = match forge {
        Forge::GitLab => runs(RunFilter::default()),
        _ => notifications(),
    };
    // The first refresh is a full one.
    let (base, since) = refresh(harness, &cache, &read);
    assert_eq!(since, None);
    match forge {
        Forge::GitHub => harness.update(|state| {
            let items = state["actions"]["notifications"].as_array_mut().unwrap();
            items[2]["unread"] = json!(true);
            items[2]["updated_at"] = json!("2099-01-01T00:00:00Z");
            items.push(json!({"id": "9004", "unread": true, "reason": "assign",
                              "updated_at": "2099-01-02T00:00:00Z",
                              "subject": {"title": "New", "type": "Issue", "url": null,
                                          "latest_comment_url": null}}));
        }),
        Forge::GitLab => harness.update(|state| {
            let pipeline = &mut state["gitlab"]["pipelines"][1];
            pipeline["updated_at"] = json!("2099-01-01T00:00:00Z");
            pipeline["status"] = json!("running");
        }),
        Forge::Gitea | Forge::Forgejo => {}
    }
    harness.clear();
    let (merged, since) = refresh(harness, &cache, &read);
    let since = since.expect("the second refresh is incremental");
    // One request, carrying the cached copy's time.
    let sent = harness.requests();
    let word = if forge == Forge::GitLab {
        "updated_after"
    } else {
        "since"
    };
    let lists = sent
        .iter()
        .filter(|request| request["query"].get(word).is_some())
        .collect::<Vec<_>>();
    assert_eq!(lists.len(), 1, "{sent:?}");
    assert_eq!(lists[0]["query"][word], since);
    match forge {
        Forge::GitHub => {
            assert_eq!(sent.len(), 1, "{sent:?}");
            let merged = expect!(merged, Payload::Notifications);
            assert_eq!(
                merged
                    .items
                    .iter()
                    .map(|item| item.id.0.as_str())
                    .collect::<Vec<_>>(),
                ["9004", "9003", "9001", "9002"]
            );
            let base = expect!(base, Payload::Notifications);
            assert_eq!(base.items.len(), 2);
        }
        Forge::GitLab => {
            assert_eq!(sent.len(), 1, "{sent:?}");
            let merged = expect!(merged, Payload::Runs);
            assert_eq!(
                merged
                    .items
                    .iter()
                    .map(|run| (run.id.0.as_str(), run.status))
                    .collect::<Vec<_>>(),
                [
                    ("89", RunStatus::InProgress),
                    ("91", RunStatus::Completed),
                    ("90", RunStatus::InProgress)
                ]
            );
            assert_eq!(merged.total, Some(3));
            // A merge request pipeline keeps the branch it resolved on the full refresh.
            assert_eq!(merged.items[1].branch, "retry-uploads");
            let _ = base;
        }
        Forge::Gitea | Forge::Forgejo => {
            // Nothing changed since: the cached list stands as it was.
            assert_eq!(merged, base);
        }
    }
    // Every tenth refresh is full again.
    for _ in 2..dust::forge::incremental::FULL_EVERY {
        refresh(harness, &cache, &read);
    }
    let (_, since) = refresh(harness, &cache, &read);
    assert_eq!(since, None);
}

/// N pull request details prefetched at once are one aliased GraphQL request; N overlapping
/// single fingerprint reads join one batch.
pub fn batched_prefetch(harness: &Harness) {
    harness.warm();
    let pulls = expect!(
        harness.read(Read::PullRequests(vec![9, 10, 11, 12, 404])),
        Payload::PullRequests
    );
    assert_eq!(
        pulls.iter().map(|pull| pull.row.number).collect::<Vec<_>>(),
        [9, 10, 11, 12]
    );
    let sent = harness.requests();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["operation"], "PullRequestDetailBatch");
    // The batch fills each item's own cache entry.
    let items = cache::items_of(&Payload::PullRequests(pulls.clone()));
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].0, Read::PullRequest(9));
    harness.clear();
    // The readers start together (the batch window is 50 ms), each with its own context.
    let start = &std::sync::Barrier::new(4);
    let prints = std::thread::scope(|scope| {
        let threads = [9u64, 10, 11, 12]
            .map(|number| {
                let ctx = harness.ctx();
                scope.spawn(move || {
                    start.wait();
                    expect!(
                        harness.read_with(&ctx, Read::Fingerprints(vec![number])),
                        Payload::Fingerprints
                    )
                })
            })
            .into_iter()
            .collect::<Vec<_>>();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    for (index, print) in prints.iter().enumerate() {
        assert_eq!(print.len(), 1);
        assert_eq!(print[0].0, [9, 10, 11, 12][index]);
    }
    assert_eq!(prints[3][0].1.head, FORK_SHA);
    let sent = harness.requests();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["operation"], "PullRequestFingerprintBatch");
}

impl Harness {
    fn read_with(&self, ctx: &ForgeContext, read: Read) -> Payload {
        match self.fetch(ctx, &read, &Conditional::default()) {
            Fetched::Payload { value, .. } => value,
            Fetched::NotModified => panic!("{read:?} came back not modified"),
        }
    }

    /// The list request of a runs read on this forge.
    fn runs_tail(&self) -> &'static str {
        match self.forge {
            Forge::GitHub => "/actions/runs",
            Forge::GitLab => "/pipelines",
            Forge::Gitea | Forge::Forgejo => "/actions/runs",
        }
    }
}

/// Identical reads that overlap share one request.
///
/// The forge holds the runs list until three readers are waiting on the first one's fetch,
/// so they overlap by construction: no response delay to outlast, and a count of this one
/// flight's followers, not the process-wide `joined`, which the other forges' copies of this
/// test (running at the same time) also move.
pub fn concurrent_reads_coalesce(harness: &Harness) {
    harness.warm();
    harness.update(|state| {
        state["read_gates"] = json!([{"path": harness.runs_tail(), "name": "coalesce"}]);
    });
    let read = runs(RunFilter::default());
    let results = std::thread::scope(|scope| {
        let threads = (0..4)
            .map(|_| {
                let read = read.clone();
                scope.spawn(move || {
                    let ctx = harness.ctx();
                    dust::forge::inflight::read(
                        harness.provider().as_ref(),
                        &ctx,
                        &read,
                        &Conditional::default(),
                        &AtomicBool::new(false),
                    )
                    .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let ctx = harness.ctx();
        let deadline = Instant::now() + Duration::from_secs(20);
        while dust::forge::inflight::followers(&ctx, &read, &Conditional::default()) < 3 {
            assert!(Instant::now() < deadline, "the readers never overlapped");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::write(
            format!("{}.coalesce", harness.state_path().display()),
            "release",
        )
        .unwrap();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(
        harness.count(harness.runs_tail()),
        1,
        "{:?}",
        harness.paths()
    );
}

/// A second read of an unchanged list is a `304` the transport turns back into the answer.
pub fn etag_revalidates(harness: &Harness) {
    harness.warm();
    let read = runs(RunFilter::default());
    let first = harness.read(read.clone());
    let revalidated = dust::forge::etag::revalidated();
    harness.clear();
    let second = harness.read(read);
    assert_eq!(first, second);
    assert_eq!(harness.count(harness.runs_tail()), 1);
    assert_eq!(harness.state()["not_modified"], json!(1));
    assert_eq!(dust::forge::etag::revalidated() - revalidated, 1);
    let sent = harness
        .requests()
        .into_iter()
        .find(|request| {
            request["path"]
                .as_str()
                .unwrap_or("")
                .ends_with(harness.runs_tail())
        })
        .unwrap();
    assert_eq!(sent["method"], "GET");
}

/// 45 s while anything runs, 60 s once everything finished, nothing for non-CI screens.
pub fn polling_cadence(harness: &Harness) {
    harness.warm();
    let all = harness.read(runs(RunFilter::default()));
    let pending = expect!(all.clone(), Payload::Runs)
        .items
        .iter()
        .any(|run| run.status != RunStatus::Completed);
    assert_eq!(
        cache::poll_interval(&all),
        Some(Duration::from_secs(if pending { 45 } else { 60 }))
    );
    let finished = harness.read(Read::Run {
        id: ItemId::new(harness.forge.run()),
        attempt: None,
    });
    assert_eq!(
        cache::poll_interval(&finished),
        Some(Duration::from_secs(60))
    );
    match harness.forge {
        Forge::GitHub => {
            assert!(pending);
            let running = harness.read(Read::Run {
                id: ItemId::new(503),
                attempt: None,
            });
            assert_eq!(
                cache::poll_interval(&running),
                Some(Duration::from_secs(45))
            );
        }
        Forge::GitLab => {
            assert!(pending);
            let main = harness.read(runs(RunFilter {
                branch: Some("main".into()),
                ..RunFilter::default()
            }));
            assert_eq!(cache::poll_interval(&main), Some(Duration::from_secs(60)));
        }
        Forge::Gitea | Forge::Forgejo => {}
    }
    assert_eq!(cache::poll_interval(&harness.read(notifications())), None);
}

/// Requests one refresh of an Actions screen costs before and after F3's speed items: four
/// pull request details, the runs list read by both workers at once, then again unchanged.
pub fn typical_refresh(harness: &Harness) {
    harness.warm();
    let numbers = [9u64, 10, 11, 12];
    let read = runs(RunFilter::default());
    // Before: one request per detail, no coalescing (each reader its own fetch, full bodies).
    for number in numbers {
        harness.read(Read::PullRequest(number));
    }
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let read = read.clone();
            scope.spawn(move || harness.read(read));
        }
    });
    let before = harness.requests().len();
    let before_bodies = before - harness.state()["not_modified"].as_u64().unwrap_or(0) as usize;
    // After: one aliased detail request, coalesced runs, then a 304 for the unchanged list.
    let fresh = &Harness::new(harness.forge, harness.via);
    fresh.warm();
    fresh.update(|state| state["delay_ms"] = json!(200));
    fresh.read(Read::PullRequests(numbers.to_vec()));
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let read = read.clone();
            scope.spawn(move || {
                let ctx = fresh.ctx();
                dust::forge::inflight::read(
                    fresh.provider().as_ref(),
                    &ctx,
                    &read,
                    &Conditional::default(),
                    &AtomicBool::new(false),
                )
                .unwrap()
            });
        }
    });
    fresh.read(read.clone());
    let after = fresh.requests().len();
    let after_bodies = after - fresh.state()["not_modified"].as_u64().unwrap_or(0) as usize;
    println!(
        "forge_ci: {:?}/{:?} typical refresh: before {before} requests ({before_bodies} with a body), after {after} requests ({after_bodies} with a body)",
        harness.forge, harness.via
    );
    assert_eq!(before, 6);
    assert_eq!(after, 3);
    assert_eq!(after_bodies, 2);
}

// ---------------------------------------------------------------------------- the matrix

macro_rules! matrix {
    ($module:ident: $forge:expr, $via:expr; $($scenario:ident),* $(,)?) => {
        mod $module {
            $(
                #[test]
                fn $scenario() {
                    super::isolated(
                        concat!(stringify!($module), "::", stringify!($scenario)),
                        || super::$scenario(&super::Harness::new($forge, $via)),
                    );
                }
            )*
        }
    };
}

matrix!(github_http: super::Forge::GitHub, super::Via::Http;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    check_dedupe_and_rollup, since_merges_without_full_reload, batched_prefetch,
    concurrent_reads_coalesce, etag_revalidates, polling_cadence, typical_refresh);
matrix!(github_gh: super::Forge::GitHub, super::Via::Cli;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    check_dedupe_and_rollup, since_merges_without_full_reload, batched_prefetch,
    concurrent_reads_coalesce, etag_revalidates, polling_cadence, typical_refresh);
matrix!(gitlab_http: super::Forge::GitLab, super::Via::Http;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    since_merges_without_full_reload, concurrent_reads_coalesce, etag_revalidates,
    polling_cadence);
matrix!(gitlab_glab: super::Forge::GitLab, super::Via::Cli;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    since_merges_without_full_reload, concurrent_reads_coalesce, etag_revalidates,
    polling_cadence);
matrix!(gitea_http: super::Forge::Gitea, super::Via::Http;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    since_merges_without_full_reload, concurrent_reads_coalesce, etag_revalidates,
    polling_cadence);
matrix!(gitea_tea: super::Forge::Gitea, super::Via::Cli;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    since_merges_without_full_reload, concurrent_reads_coalesce, etag_revalidates,
    polling_cadence);
matrix!(forgejo_http: super::Forge::Forgejo, super::Via::Http;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    since_merges_without_full_reload, concurrent_reads_coalesce, etag_revalidates,
    polling_cadence);
matrix!(forgejo_tea: super::Forge::Forgejo, super::Via::Cli;
    runs_jobs_steps_logs, rerun_cancel_dispatch, approve, artifacts, version_gates,
    since_merges_without_full_reload, concurrent_reads_coalesce, etag_revalidates,
    polling_cadence);
