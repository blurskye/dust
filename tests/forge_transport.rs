#![cfg(unix)]
//! The GitHub contract on both transports (FORGE.md §8.2, F1): direct HTTP against the fake
//! forge server, and `gh api` passthrough through a fake `gh` that forwards to the same server.
//! Both must produce the same domain values and the same logical requests; neither may leak the
//! token into argv, logs or error text.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dust::forge::cli::CliTransport;
use dust::forge::credential::Credential;
use dust::forge::credential::CredentialSource;
use dust::forge::credential::Secret;
use dust::forge::http::HttpTransport;
use dust::forge::model::ItemId;
use dust::forge::model::ItemKind;
use dust::forge::model::MergeMethod;
use dust::forge::provider;
use dust::forge::provider::Conditional;
use dust::forge::provider::Fetched;
use dust::forge::provider::ForgeContext;
use dust::forge::provider::ProviderKind;
use dust::forge::provider::RepoRef;
use dust::forge::request::Filter;
use dust::forge::request::ForgeError;
use dust::forge::request::Page;
use dust::forge::request::Payload;
use dust::forge::request::Read;
use dust::forge::request::RunFilter;
use dust::forge::request::StateFilter;
use dust::forge::request::Write;
use dust::forge::transport;
use dust::forge::transport::Cli;
use dust::forge::transport::ConnectOptions;
use dust::forge::transport::Endpoints;
use dust::forge::transport::HostKey;
use dust::forge::transport::Transport;
use dust::forge::transport::TransportKind;
use forge_fake::FAKE_TOKEN;
use forge_fake::ForgeServer;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Via {
    Http,
    Gh,
}

struct World {
    directory: tempfile::TempDir,
    server: ForgeServer,
    gh: PathBuf,
}

impl World {
    fn new(state: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        std::fs::write(&path, state.to_string()).unwrap();
        let server = ForgeServer::start(&path, "legacy");
        let gh = server.install_gh(&directory.path().join("bin"));
        Self {
            directory,
            server,
            gh,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    fn state(&self) -> Value {
        forge_fake::state(&self.state_path())
    }

    fn update(&self, change: impl FnOnce(&mut Value)) {
        let mut state = self.state();
        change(&mut state);
        std::fs::write(self.state_path(), state.to_string()).unwrap();
    }

    fn requests(&self) -> Vec<Value> {
        forge_fake::requests(&self.state_path())
    }

    fn clear_requests(&self) {
        self.update(|state| state["requests"] = json!([]));
    }

    fn endpoints(&self, host: &str) -> Endpoints {
        let mut endpoints = Endpoints::github(&HostKey::new(host));
        endpoints.override_api_base(&format!("{}/{host}/api/v3", self.server.url()));
        endpoints
    }

    fn options(&self) -> ConnectOptions {
        ConnectOptions {
            gh: Some(self.gh.clone()),
            // Signed-out states, not anonymous reads (tests/forge_anonymous.rs).
            no_anonymous: true,
            ..ConnectOptions::default()
        }
    }

    fn transport(&self, via: Via, host: &str) -> Arc<dyn Transport> {
        match via {
            Via::Http => Arc::new(
                HttpTransport::new(
                    self.endpoints(host),
                    Credential {
                        token: Secret::new(FAKE_TOKEN),
                        source: CredentialSource::GhAuthToken,
                        user: None,
                        obtained: Instant::now(),
                    },
                )
                .unwrap(),
            ),
            Via::Gh => Arc::new(CliTransport::new(Cli::Gh, self.gh.clone(), None)),
        }
    }

    fn context(&self, via: Via, host: &str, repo: Option<&str>) -> ForgeContext {
        ForgeContext::new(
            self.directory.path().to_path_buf(),
            self.endpoints(host),
            self.options(),
            repo.and_then(RepoRef::parse),
            self.transport(via, host),
        )
    }
}

fn read(ctx: &ForgeContext, read: Read) -> Result<Payload, ForgeError> {
    let provider = provider::build(ProviderKind::GitHub).unwrap();
    match provider.read(ctx, &read, &Conditional::default(), &AtomicBool::new(false))? {
        Fetched::Payload { value, .. } => Ok(value),
        Fetched::NotModified => panic!("{read:?} was not modified"),
    }
}

fn write(ctx: &ForgeContext, write: Write) -> Result<dust::forge::request::Receipt, ForgeError> {
    provider::build(ProviderKind::GitHub)
        .unwrap()
        .write(ctx, &write, &AtomicBool::new(false))
}

/// The domain values every read of the contract produced, for comparing the two transports.
fn contract(world: &World, via: Via) -> Vec<String> {
    let ctx = world.context(via, "github.com", Some("example/demo"));
    let mut seen = Vec::new();

    let Payload::Repo(repo) = read(&ctx, Read::Repository).unwrap() else {
        panic!()
    };
    assert_eq!(
        (repo.full_name.as_str(), repo.default_branch.as_str()),
        ("example/demo", "main")
    );
    seen.push(format!("{repo:?}"));

    let filter = Filter {
        state: StateFilter::Open,
        query: String::new(),
    };
    let Payload::Rows(rows) = read(
        &ctx,
        Read::List {
            kind: ItemKind::Issue,
            filter: filter.clone(),
            page: Page::first(30),
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(rows.items.len(), 1);
    assert_eq!(rows.items[0].number, 7);
    assert_eq!(rows.items[0].title, "Preserve keyboard drafts");
    assert!(
        rows.items[0]
            .body_preview
            .as_deref()
            .unwrap()
            .starts_with("A real native issue")
    );
    assert_eq!(rows.total, Some(1));
    seen.push(format!("{rows:?}"));

    let Payload::Issue(issue) = read(&ctx, Read::Issue(7)).unwrap() else {
        panic!()
    };
    assert_eq!(issue.assignees[0].login, "bob");
    assert_eq!(issue.milestone.as_ref().unwrap().title, "v1");
    assert_eq!(issue.linked_prs[0].number, 9);
    seen.push(format!("{issue:?}"));

    let Payload::PullRequest(pull) = read(&ctx, Read::PullRequest(9)).unwrap() else {
        panic!()
    };
    assert_eq!(pull.head.sha, "a".repeat(40));
    assert_eq!(pull.head.branch, "feature/native");
    assert_eq!(pull.files.len(), 2);
    assert_eq!(pull.checks.len(), 1);
    assert_eq!(pull.checks[0].name, "test (linux)");
    assert_eq!(pull.linked_issues[0].number, 7);
    seen.push(format!("{pull:?}"));

    let Payload::Timeline(timeline) = read(&ctx, Read::Timeline(7)).unwrap() else {
        panic!()
    };
    assert_eq!(timeline.len(), 1);
    seen.push(format!("{timeline:?}"));

    // Two GraphQL pages: the provider follows `pageInfo` through an `after` cursor.
    world.update(|state| {
        state["review_threads"] = json!({"example/demo#9": [
            {"id":"THREAD_A","isResolved":false,"comments":{"nodes":[{"author":{"login":"reviewer"},"body":"First","path":"example.rs","line":1,"originalLine":1}]}},
            {"id":"THREAD_B","isResolved":true,"comments":{"nodes":[{"author":{"login":"reviewer"},"body":"Second","path":"src/nested/other.rs","line":1,"originalLine":1}]}}
        ]});
    });
    let Payload::Threads(threads) = read(&ctx, Read::Threads(9)).unwrap() else {
        panic!()
    };
    assert_eq!(
        threads
            .iter()
            .map(|thread| (thread.id.0.as_str(), thread.resolved))
            .collect::<Vec<_>>(),
        [("THREAD_A", Some(false)), ("THREAD_B", Some(true))]
    );
    seen.push(format!("{threads:?}"));

    let Payload::Viewed(viewed) = read(&ctx, Read::Viewed(9)).unwrap() else {
        panic!()
    };
    assert_eq!(viewed.len(), 2);
    seen.push(format!("{viewed:?}"));

    let Payload::Text(diff) = read(&ctx, Read::Diff(9)).unwrap() else {
        panic!()
    };
    assert!(diff.starts_with("diff --git a/example.rs"), "{diff}");
    seen.push(diff);

    let Payload::Workflows(workflows) = read(&ctx, Read::Workflows).unwrap() else {
        panic!()
    };
    assert_eq!(workflows[0].path, ".github/workflows/native.yml");
    seen.push(format!("{workflows:?}"));

    let Payload::Text(source) = read(
        &ctx,
        Read::WorkflowSource {
            id: ItemId::new(31),
            reference: Some("main".into()),
        },
    )
    .unwrap() else {
        panic!()
    };
    assert!(source.contains("workflow_dispatch"));

    let Payload::Runs(runs) = read(
        &ctx,
        Read::Runs {
            filter: RunFilter::default(),
            page: Page::first(20),
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(runs.items[0].id.0, "91");

    // The legacy world's run settles on its second view; read past that.
    world.update(|state| state["run_views"] = json!(5));
    let Payload::Run(run) = read(
        &ctx,
        Read::Run {
            id: ItemId::new(91),
            attempt: None,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(run.jobs.len(), 3);
    assert_eq!(
        run.run.web_url,
        "https://github.com/example/demo/actions/runs/91"
    );
    seen.push(format!("{:?}", run.jobs));

    let Payload::Log(log) = read(&ctx, Read::JobLog(ItemId::new(42))).unwrap() else {
        panic!()
    };
    assert!(
        log.text.contains("all native checks passed"),
        "{}",
        log.text
    );
    seen.push(log.text.clone());

    // Host-wide search: no repository term.
    let inbox = world.context(via, "github.com", None);
    let Payload::Rows(found) = read(
        &inbox,
        Read::Search {
            kind: ItemKind::PullRequest,
            filter,
            page: Page::first(30),
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(found.items[0].number, 9);
    seen.push(format!("{found:?}"));

    // Writes: a REST POST with a JSON body, a GraphQL mutation, a REST PUT.
    let receipt = write(
        &ctx,
        Write::Comment {
            kind: ItemKind::Issue,
            number: 7,
            body: format!("Sent {via:?} 界"),
        },
    )
    .unwrap();
    assert_eq!(
        receipt.web_url.as_deref(),
        Some("https://github.com/example/demo/issues/7#issuecomment-1")
    );
    write(
        &ctx,
        Write::Resolve {
            thread: ItemId::from("THREAD_A"),
            resolved: true,
        },
    )
    .unwrap();
    write(
        &ctx,
        Write::Merge {
            number: 9,
            method: MergeMethod::Squash,
            head: "a".repeat(40),
            delete_branch: false,
            title: None,
            message: None,
        },
    )
    .unwrap();
    let state = world.state();
    assert_eq!(state["comments"], json!([format!("Sent {via:?} 界")]));
    assert_eq!(state["thread_resolved"]["THREAD_A"], true);
    seen
}

fn assert_requests_went(world: &World, via: Via) {
    let requests = world.requests();
    assert!(!requests.is_empty());
    for request in &requests {
        if request["path"] == "auth token" {
            continue;
        }
        match via {
            Via::Http => {
                assert_eq!(request["transport"], "http", "{request}");
                assert_eq!(request["auth"], "valid", "{request}");
            }
            Via::Gh => {
                assert_eq!(request["transport"], "gh", "{request}");
                let argv = request["gh_argv"].as_array().unwrap();
                let words = argv
                    .iter()
                    .map(|word| word.as_str().unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(words[0], "api");
                assert!(words.contains(&"--include"), "{words:?}");
                assert!(
                    words
                        .windows(2)
                        .any(|pair| pair == ["--hostname", "github.com"]),
                    "{words:?}"
                );
                if request["method"] != "GET" {
                    // stdin, or a private file for authored text (`ApiRequest::body_file`).
                    assert!(
                        words.windows(2).any(|pair| pair[0] == "--input"
                            && (pair[1] == "-" || pair[1].contains("dusty-forge-body-"))),
                        "{words:?}"
                    );
                }
            }
        }
    }
}

fn logical(world: &World) -> Vec<(Value, Value, Value)> {
    world
        .requests()
        .into_iter()
        .filter(|request| request["path"] != "auth token")
        .map(|request| {
            (
                request["method"].clone(),
                request["path"].clone(),
                request["operation"].clone(),
            )
        })
        .collect()
}

#[test]
fn github_contract_matches_on_direct_http_and_gh_passthrough() {
    let http = World::new(json!({}));
    let over_http = contract(&http, Via::Http);
    assert_requests_went(&http, Via::Http);

    let gh = World::new(json!({}));
    let over_gh = contract(&gh, Via::Gh);
    assert_requests_went(&gh, Via::Gh);

    assert_eq!(
        over_http, over_gh,
        "both transports decode to the same domain values"
    );
    assert_eq!(
        logical(&http),
        logical(&gh),
        "both transports send the same requests"
    );
    for world in [&http, &gh] {
        let log = std::fs::read_to_string(world.state_path()).unwrap();
        assert!(!log.contains(FAKE_TOKEN), "the request log holds no token");
    }
}

#[test]
fn errors_classify_the_same_on_both_transports() {
    for via in [Via::Http, Via::Gh] {
        let world = World::new(json!({}));
        let ctx = world.context(via, "github.com", Some("example/demo"));
        world.update(|state| {
            state["fail"] = json!([
                {"path":"repos/example/demo/labels","status":404,"message":"Not Found","times":1},
                {"path":"repos/example/demo/milestones","status":403,"message":"Resource not accessible","times":1},
                {"path":"pulls/9/merge","status":405,"message":"Pull Request is not mergeable","times":1},
                {"path":"actions/workflows","status":502,"message":"Server Error","times":1},
                {"path":"repos/example/demo/assignees","status":403,"message":"API rate limit exceeded","times":1}
            ]);
        });
        assert!(
            matches!(read(&ctx, Read::Labels), Err(ForgeError::NotFound(_))),
            "{via:?}"
        );
        assert!(
            matches!(read(&ctx, Read::Milestones), Err(ForgeError::Forbidden(_))),
            "{via:?}"
        );
        let merge = write(
            &ctx,
            Write::Merge {
                number: 9,
                method: MergeMethod::Merge,
                head: "a".repeat(40),
                delete_branch: false,
                title: None,
                message: None,
            },
        )
        .unwrap_err();
        assert_eq!(
            merge.to_string(),
            "Conflict · Pull Request is not mergeable",
            "{via:?}"
        );
        assert!(
            matches!(read(&ctx, Read::Workflows), Err(ForgeError::Network(_))),
            "{via:?}"
        );
        // A GraphQL error with null data is a failure carrying the server's message.
        world.update(|state| state["fail_item_views"] = json!(1));
        let error = read(&ctx, Read::Issue(7)).unwrap_err();
        assert_eq!(
            error,
            ForgeError::Failed("Fixture refresh refused".into()),
            "{via:?}"
        );
        // Last: a rate limit pauses this credential (F2, FORGE.md §3.5), so later reads would
        // be refused locally.
        assert!(
            matches!(
                read(&ctx, Read::Collaborators),
                Err(ForgeError::RateLimited { .. })
            ),
            "{via:?}"
        );
    }
}

#[test]
fn no_token_reaches_argv_logs_or_error_text() {
    for via in [Via::Http, Via::Gh] {
        let world = World::new(json!({}));
        let ctx = world.context(via, "github.com", Some("example/demo"));
        // A server that echoes a token back in its error message.
        world.update(|state| {
            state["fail"] = json!([
                {"path":"repos/example/demo/labels","status":422,
                 "message":format!("token {FAKE_TOKEN} is not allowed"),"times":1},
                {"path":"repos/example/demo/milestones","status":500,
                 "message":format!("Authorization: Bearer {FAKE_TOKEN}"),"times":1}
            ]);
        });
        for error in [
            read(&ctx, Read::Labels).unwrap_err(),
            read(&ctx, Read::Milestones).unwrap_err(),
        ] {
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains(FAKE_TOKEN), "{via:?}: {shown}");
            assert!(shown.contains("<redacted>"), "{via:?}: {shown}");
        }
        // The world itself holds the echoing rules, so check only what the server logged.
        for request in world.requests() {
            assert!(
                !request.to_string().contains(FAKE_TOKEN),
                "{via:?}: {request}"
            );
        }
    }
    // The credential type never prints its secret.
    let credential = Credential {
        token: Secret::new(FAKE_TOKEN),
        source: CredentialSource::GhAuthToken,
        user: None,
        obtained: Instant::now(),
    };
    assert!(!format!("{credential:?} {}", credential.token).contains(FAKE_TOKEN));
    // gh's argv for a write carries the body on stdin and no token, whatever the body says.
    let request = dust::forge::transport::ApiRequest::new(
        dust::forge::transport::Method::Post,
        "repos/example/demo/issues/7/comments",
    )
    .json(json!({ "body": FAKE_TOKEN }));
    let (argv, stdin, env) =
        dust::forge::cli::argv(Cli::Gh, &HostKey::new("github.com"), &request, None);
    assert!(!argv.iter().any(|arg| arg.contains(FAKE_TOKEN)), "{argv:?}");
    assert!(argv.windows(2).any(|pair| pair == ["--input", "-"]));
    assert!(
        String::from_utf8(stdin.unwrap())
            .unwrap()
            .contains(FAKE_TOKEN)
    );
    assert!(env.iter().all(|(_, value)| !value.contains(FAKE_TOKEN)));
}

#[test]
fn passthrough_output_parses_as_an_http_response() {
    let world = World::new(json!({}));
    let output = std::process::Command::new(&world.gh)
        .args([
            "api",
            "repos/example/demo",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            "--include",
            "-H",
            "Accept: application/vnd.github+json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let response = dust::forge::cli::parse_included(&output.stdout).unwrap();
    assert_eq!(response.status, 200);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("application/json")
    );
    assert_eq!(response.json().unwrap()["full_name"], "example/demo");
    // An HTTP error still prints the response, so the status and body survive.
    let output = std::process::Command::new(&world.gh)
        .args([
            "api",
            "repos/example/demo/nothing",
            "--hostname",
            "github.com",
            "--include",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let response = dust::forge::cli::parse_included(&output.stdout).unwrap();
    assert_eq!(response.status, 404);
    assert!(String::from_utf8_lossy(&output.stderr).contains("(HTTP 404)"));
}

#[test]
fn a_rejected_token_falls_back_to_passthrough_once() {
    let world = World::new(json!({}));
    // A host of its own: the credential and transport caches are per host.
    let host = "ghe.fallback.test";
    let endpoints = world.endpoints(host);
    let options = world.options();
    let cancel = AtomicBool::new(false);
    let chosen = transport::for_host(&endpoints, &options, &cancel).unwrap();
    assert!(
        matches!(
            chosen.kind(),
            TransportKind::Http {
                credential_source: CredentialSource::GhAuthToken
            }
        ),
        "{:?}",
        chosen.kind()
    );
    world.update(|state| {
        state["fail"] = json!([{"transport":"http","path":"repos/example/demo","status":401,
            "message":"Bad credentials","times":1}]);
    });
    let ctx = ForgeContext::new(
        world.directory.path().to_path_buf(),
        endpoints.clone(),
        options.clone(),
        RepoRef::parse("example/demo"),
        chosen,
    );
    let Payload::Repo(repo) = read(&ctx, Read::Repository).unwrap() else {
        panic!()
    };
    assert_eq!(repo.full_name, "example/demo");
    assert_eq!(ctx.transport_kind(), TransportKind::Cli(Cli::Gh));
    let repository_reads = world
        .requests()
        .into_iter()
        .filter(|request| request["path"] == "repos/example/demo")
        .map(|request| request["transport"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(repository_reads, ["http", "gh"]);
    // The fallback happens once per context: a second 401 surfaces instead of looping.
    world.update(|state| {
        state["reject_http_auth"] = json!(true);
    });
    let ctx = ForgeContext::new(
        world.directory.path().to_path_buf(),
        endpoints.clone(),
        options.clone(),
        RepoRef::parse("example/demo"),
        world.transport(Via::Http, host),
    );
    world.clear_requests();
    read(&ctx, Read::Labels).unwrap();
    let transports = world
        .requests()
        .into_iter()
        .map(|request| request["transport"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(transports, ["http", "gh"]);
    let error = read(&ctx, Read::Repository);
    assert!(error.is_ok(), "the context stays on passthrough: {error:?}");
}

#[test]
fn a_signed_out_host_without_gh_is_a_state_not_a_crash() {
    let world = World::new(json!({"gh_auth": {"*": "missing"}}));
    let host = "ghe.signedout.test";
    let endpoints = world.endpoints(host);
    let forced = ConnectOptions {
        force: Some(transport::Force::Http),
        ..world.options()
    };
    let error = transport::for_host(&endpoints, &forced, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(
        matches!(&error, ForgeError::NotAuthenticated { hint, .. } if hint == &format!("gh auth login --hostname {host}")),
        "{error:?}"
    );
    // Without forcing, a missing token means gh passthrough.
    let chosen =
        transport::for_host(&endpoints, &world.options(), &AtomicBool::new(false)).unwrap();
    assert_eq!(chosen.kind(), TransportKind::Cli(Cli::Gh));
    let missing = ConnectOptions {
        no_anonymous: true,
        ..ConnectOptions::default()
    };
    let error = transport::for_host(
        &world.endpoints("ghe.nogh.test"),
        &missing,
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    assert_eq!(error, ForgeError::CliMissing(Cli::Gh));
    // By default the same host is browsed anonymously instead (FORGE.md §2.5).
    let anonymous = transport::for_host(
        &world.endpoints("ghe.anonymous.test"),
        &ConnectOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(anonymous.kind(), TransportKind::Anonymous);
    // So is a configured gh path that is not there (`DUSTY_FORGE_GH=/nowhere/gh`).
    let nowhere = ConnectOptions {
        gh: Some(world.directory.path().join("nowhere/gh")),
        ..ConnectOptions::default()
    };
    let anonymous = transport::for_host(
        &world.endpoints("ghe.nowhere.test"),
        &nowhere,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(anonymous.kind(), TransportKind::Anonymous);
}

#[test]
fn redirects_carry_the_token_only_to_the_api_origin() {
    let world = World::new(json!({}));
    let port = world.server.port;
    world.update(|state| {
        state["fail"] = json!([
            {"path":"actions/jobs/42/logs","status":302,"location":format!("http://localhost:{port}/_blob/foreign"),"times":1},
            {"path":"actions/jobs/43/logs","status":302,"location":format!("http://127.0.0.1:{port}/_blob/same"),"times":1}
        ]);
    });
    let ctx = world.context(Via::Http, "github.com", Some("example/demo"));
    for job in [42, 43] {
        let Payload::Log(log) = read(&ctx, Read::JobLog(ItemId::new(job))).unwrap() else {
            panic!()
        };
        assert_eq!(log.text, "blob payload\n");
    }
    let blobs = world.state()["blob_requests"].clone();
    assert_eq!(
        blobs,
        json!([
            {"path":"/_blob/foreign","authorization":"absent"},
            {"path":"/_blob/same","authorization":"present"}
        ])
    );
}

#[test]
fn host_keys_and_endpoints_follow_gh() {
    assert_eq!(HostKey::new("GitHub.COM.").to_string(), "github.com");
    let github = Endpoints::github(&HostKey::new("github.com"));
    if std::env::var_os("DUSTY_FORGE_API_BASE").is_none() {
        assert_eq!(github.api_base, "https://api.github.com");
        assert_eq!(
            github.graphql.as_deref(),
            Some("https://api.github.com/graphql")
        );
        let ghes = Endpoints::github(&HostKey::new("git.corp.example:8443"));
        assert_eq!(ghes.api_base, "https://git.corp.example:8443/api/v3");
        assert_eq!(
            ghes.graphql.as_deref(),
            Some("https://git.corp.example:8443/api/graphql")
        );
    }
    assert!(Endpoints::check_scheme("http://127.0.0.1:9/x").is_ok());
    assert!(Endpoints::check_scheme("http://example.com/x").is_err());
}
