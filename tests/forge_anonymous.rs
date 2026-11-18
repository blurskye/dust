#![cfg(unix)]
//! Anonymous reads: with no token and no forge CLI, public repositories on
//! GitHub, GitLab, Gitea and Forgejo are browsed read-only over direct HTTP. Writes answer
//! "not signed in" without a request, GitHub never sees a GraphQL request (its GraphQL API has
//! no anonymous access), the small quota is spent carefully (304s are free, background work
//! stops at half of it), and an instance that refuses every anonymous API call is
//! `SignInRequired`, not an unknown forge.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dusty::forge::capability::Cap;
use dusty::forge::host_state;
use dusty::forge::host_state::HostState;
use dusty::forge::http::HttpTransport;
use dusty::forge::model::ItemKind;
use dusty::forge::provider;
use dusty::forge::provider::Conditional;
use dusty::forge::provider::ForgeContext;
use dusty::forge::provider::ProviderKind;
use dusty::forge::provider::RepoRef;
use dusty::forge::request::ForgeError;
use dusty::forge::request::Payload;
use dusty::forge::request::Read;
use dusty::forge::request::Write;
use dusty::forge::transport::ConnectOptions;
use dusty::forge::transport::Endpoints;
use dusty::forge::transport::HostKey;
use dusty::forge::transport::TransportKind;
use forge_world::ALL;
use forge_world::Forge;
use forge_world::World;
use serde_json::Value;
use serde_json::json;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn describe(&self) -> String {
        format!(
            "exit {}\nstdout: {}\nstderr: {}",
            self.code, self.stdout, self.stderr
        )
    }

    fn object(&self) -> Value {
        serde_json::from_str(self.stdout.trim())
            .unwrap_or_else(|error| panic!("not one JSON object ({error}): {}", self.describe()))
    }

    fn lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn error(&self) -> Value {
        let line = self
            .stderr
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("no JSON error: {}", self.describe()));
        serde_json::from_str::<Value>(line).unwrap()["error"].clone()
    }
}

/// A world whose fake forge answers anonymous requests like a public forge (60 an hour).
fn anonymous_world(forge: Forge, anonymous: Value) -> World {
    World::with_state(forge, json!({ "anonymous": anonymous }))
}

/// `dusty-git forge <args>` with no token and no forge CLI anywhere (`PATH` holds only git).
fn forge(world: &World, args: &[&str]) -> Run {
    let bin = world.directory.path().join("git-only");
    if !bin.exists() {
        std::fs::create_dir_all(&bin).unwrap();
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("git"))
            .find(|path| path.is_file())
            .unwrap();
        std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_dusty-git"));
    command
        .arg("forge")
        .arg("--repo")
        .arg(&world.repo)
        .args(args)
        .current_dir(&world.repo)
        .stdin(Stdio::null());
    world.configure(&mut command);
    command
        .env_remove("DUSTY_FORGE_TOKEN")
        .env_remove("DUSTY_FORGE_ANONYMOUS")
        .env("DUSTY_FORGE_GH", "")
        .env("DUSTY_FORGE_GLAB", "")
        .env("DUSTY_FORGE_TEA", "")
        .env("PATH", &bin);
    let output = command.output().unwrap();
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn ok(run: &Run) {
    assert_eq!(run.code, 0, "{}", run.describe());
}

/// Runs `check` and records a failure instead of stopping, so one run reports every provider.
fn attempt(failures: &mut Vec<String>, label: String, check: impl FnOnce()) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check)) {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        failures.push(format!("{label}: {message}"));
    }
}

fn login_hint(forge: Forge) -> String {
    match forge {
        Forge::GitHub => format!("gh auth login --hostname {}", forge.host()),
        Forge::GitLab => format!("glab auth login --hostname {}", forge.host()),
        Forge::Gitea | Forge::Forgejo => format!("tea login add --url https://{}", forge.host()),
    }
}

/// The popover's sign-in with no CLI on PATH: the token variable, never a CLI it can't run.
fn token_hint(forge: Forge) -> String {
    match forge {
        Forge::GitHub if forge.host() == "github.com" => "GH_TOKEN=<token>".into(),
        Forge::GitHub => "GH_ENTERPRISE_TOKEN=<token>".into(),
        Forge::GitLab => "GITLAB_TOKEN=<token>".into(),
        Forge::Gitea | Forge::Forgejo => {
            format!("FORGEJO_TOKEN=<token> FORGEJO_URL=https://{}", forge.host())
        }
    }
}

/// Every request that reached the API carried no credential, and none was a write.
fn assert_anonymous_reads_only(world: &World) {
    for request in world.api_requests() {
        assert_eq!(request["auth"], "missing", "{request}");
        let method = request["method"].as_str().unwrap_or("GET");
        let graphql_query = request["path"] == "graphql"
            && !request["operation"]
                .as_str()
                .is_some_and(|operation| operation.starts_with("Mutation"));
        assert!(
            matches!(method, "GET" | "HEAD") || graphql_query,
            "a write reached the forge: {request}"
        );
    }
}

#[test]
fn lists_and_details_load_without_a_credential_on_every_provider() {
    let mut failures = Vec::new();
    for forge_kind in ALL {
        let world = anonymous_world(forge_kind, json!({ "limit": 60 }));
        let world = &world;
        let name = forge_kind.name();
        let pull = forge_kind.pull().to_string();
        let issue = forge_kind.issue().to_string();
        let label = |verb: &str| format!("{name} {verb}");

        attempt(&mut failures, label("detect"), || {
            let run = forge(world, &["detect"]);
            ok(&run);
            let detected = run.object();
            assert_eq!(detected["provider"], name, "{}", run.stdout);
            assert_eq!(detected["transport"], "anonymous", "{}", run.stdout);
            assert_eq!(detected["read_only"], true);
            assert_eq!(detected["sign_in_required"], false);
            assert!(detected["viewer"].is_null(), "{}", run.stdout);
            let caps = &detected["capabilities"];
            assert_eq!(
                caps["writes"],
                json!({ "No": "Sign in to make changes" }),
                "{caps}"
            );
            assert_eq!(
                caps["inline_comment"],
                json!({ "No": "Sign in to comment" })
            );
            assert_eq!(
                caps["notifications"],
                json!({ "No": "Sign in to see notifications" })
            );
            assert_eq!(caps["merge_methods"], json!([]));
            let state = &detected["state"];
            assert_eq!(state["label"], "Anonymous · read-only", "{state}");
            assert_eq!(state["ready"], true);
            assert_eq!(state["read_only"], true);
            let chip = state["chip"].as_str().unwrap_or_default();
            assert!(chip.starts_with("Anonymous · read-only · "), "{chip}");
            assert!(chip.ends_with("/60 requests left"), "{chip}");
            assert_eq!(state["anonymous"]["limit"], 60, "{state}");
            assert_eq!(
                state["anonymous"]["hint"],
                token_hint(forge_kind),
                "{state}"
            );
        });
        attempt(&mut failures, label("prs"), || {
            let run = forge(world, &["prs"]);
            ok(&run);
            let rows = run.lines();
            assert!(
                rows.iter().any(|row| row["number"] == forge_kind.pull()),
                "{}",
                run.stdout
            );
        });
        attempt(&mut failures, label("issues"), || {
            let run = forge(world, &["issues"]);
            ok(&run);
            assert!(!run.lines().is_empty(), "{}", run.stdout);
        });
        attempt(&mut failures, label("pr"), || {
            let run = forge(world, &["pr", &pull]);
            ok(&run);
            let object = run.object();
            assert_eq!(
                object["data"]["row"]["number"],
                forge_kind.pull(),
                "{}",
                run.stdout
            );
        });
        attempt(&mut failures, label("issue"), || {
            let run = forge(world, &["issue", &issue]);
            ok(&run);
            assert_eq!(
                run.object()["data"]["row"]["number"],
                forge_kind.issue(),
                "{}",
                run.stdout
            );
        });
        attempt(&mut failures, label("threads"), || {
            let run = forge(world, &["threads", &pull]);
            ok(&run);
            assert!(run.object()["data"].is_array(), "{}", run.stdout);
        });
        attempt(&mut failures, label("timeline"), || {
            let run = forge(world, &["timeline", &pull]);
            ok(&run);
            assert!(!run.object()["data"].is_null(), "{}", run.stdout);
        });
        attempt(&mut failures, label("diff"), || {
            let run = forge(world, &["diff", &pull]);
            ok(&run);
            let object = run.object();
            assert!(
                object["data"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("diff --git"),
                "{}",
                run.stdout
            );
        });
        if matches!(forge_kind, Forge::GitHub | Forge::GitLab) {
            attempt(&mut failures, label("runs"), || {
                let run = forge(world, &["runs"]);
                ok(&run);
                assert!(!run.lines().is_empty(), "{}", run.stdout);
            });
            // Job logs need a sign-in on both; the answer says so without a request.
            attempt(&mut failures, label("log"), || {
                let before = world.api_requests().len();
                let run = forge(world, &["log", forge_kind.job()]);
                assert_eq!(run.code, 4, "{}", run.describe());
                let error = run.error();
                assert_eq!(error["kind"], "NotAuthenticated", "{error}");
                assert!(
                    error["message"]
                        .as_str()
                        .unwrap_or_default()
                        .starts_with("Sign in to read logs"),
                    "{error}"
                );
                assert!(
                    // No CLI on PATH: the sign-in it names is the token variable.
                    error["hint"]
                        .as_str()
                        .unwrap_or_default()
                        .contains(&token_hint(forge_kind)),
                    "{error}"
                );
                assert_eq!(world.api_requests().len(), before, "a log read was sent");
            });
        }
        attempt(&mut failures, label("anonymous only"), || {
            assert_anonymous_reads_only(world);
            assert_eq!(
                world.state()["anonymous_refused"].as_u64().unwrap_or(0),
                0,
                "{}",
                world.state()["requests"]
            );
        });
        if forge_kind == Forge::GitHub {
            attempt(&mut failures, label("no GraphQL"), || {
                let graphql = world
                    .requests()
                    .into_iter()
                    .filter(|request| request["path"] == "graphql")
                    .collect::<Vec<_>>();
                assert!(
                    graphql.is_empty(),
                    "GitHub GraphQL sent anonymously: {graphql:?}"
                );
            });
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[test]
fn writes_are_refused_without_a_request() {
    let mut failures = Vec::new();
    for forge_kind in ALL {
        let world = anonymous_world(forge_kind, json!({ "limit": 60 }));
        let world = &world;
        attempt(&mut failures, forge_kind.name().into(), || {
            let body = world.directory.path().join("body.md");
            std::fs::write(&body, "Looks right.").unwrap();
            let run = forge(
                world,
                &[
                    "comment",
                    &forge_kind.pull().to_string(),
                    "--body-file",
                    body.to_str().unwrap(),
                ],
            );
            assert_eq!(run.code, 4, "{}", run.describe());
            let error = run.error();
            assert_eq!(error["kind"], "NotAuthenticated", "{error}");
            assert_eq!(error["hint"], login_hint(forge_kind), "{error}");
            let writes = world
                .requests()
                .into_iter()
                .filter(|request| {
                    !matches!(request["method"].as_str(), Some("GET" | "HEAD") | None)
                })
                .collect::<Vec<_>>();
            assert!(writes.is_empty(), "a write was sent: {writes:?}");
            assert_eq!(world.state()["anonymous_refused"].as_u64().unwrap_or(0), 0);
        });
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// In-process GitHub context on the fake server, browsed anonymously.
fn github_context(world: &World, host: &str) -> (Endpoints, ForgeContext) {
    let mut endpoints = Endpoints::github(&HostKey::new(host));
    endpoints.override_api_base(&format!("{}/{host}/api/v3", world.server.url()));
    let transport = HttpTransport::anonymous(endpoints.clone()).unwrap();
    let ctx = ForgeContext::new(
        world.directory.path().to_path_buf(),
        endpoints.clone(),
        ConnectOptions::default(),
        RepoRef::parse("example/demo"),
        Arc::new(transport),
    );
    (endpoints, ctx)
}

#[test]
fn github_writes_and_sign_in_reads_send_nothing_in_process() {
    let world = anonymous_world(Forge::GitHub, json!({ "limit": 60 }));
    let (_, ctx) = github_context(&world, "writes.anonymous.test");
    assert!(ctx.anonymous());
    assert_eq!(ctx.transport_kind(), TransportKind::Anonymous);
    let github = provider::build(ProviderKind::GitHub).unwrap();
    let cancel = AtomicBool::new(false);
    let writes = [
        Write::Comment {
            kind: ItemKind::PullRequest,
            number: 9,
            body: "hi".into(),
        },
        Write::SetState {
            kind: ItemKind::Issue,
            number: 7,
            open: false,
            reason: None,
        },
    ];
    for write in &writes {
        let error = github.write(&ctx, write, &cancel).unwrap_err();
        assert!(
            matches!(&error, ForgeError::NotAuthenticated { hint, .. } if hint == "gh auth login --hostname writes.anonymous.test"),
            "{error:?}"
        );
    }
    for read in [
        Read::Viewer,
        Read::JobLog(dusty::forge::model::ItemId::new("42")),
    ] {
        let error = github
            .read(&ctx, &read, &Conditional::default(), &cancel)
            .unwrap_err();
        assert!(
            error.to_string().starts_with("Sign in to "),
            "{read:?}: {error}"
        );
    }
    let caps = github.capabilities(&ctx);
    assert_eq!(caps.writes, Cap::No("Sign in to make changes"));
    assert_eq!(caps.job_log, Cap::No("Sign in to read logs"));
    assert_eq!(
        caps.graphql_batching,
        Cap::No("GitHub GraphQL needs a sign-in")
    );
    assert!(
        world.api_requests().is_empty(),
        "{:?}",
        world.api_requests()
    );
}

/// The quota is respected: once the forge says it is spent nothing more is sent, a 304 costs
/// nothing, and the warm worker (background) stops at half the quota and sends no prefetch.
#[test]
fn the_rate_budget_is_respected() {
    // Two requests left: a pull request detail needs six, so it stops at the quota instead of
    // spending past it.
    let world = anonymous_world(Forge::GitHub, json!({ "limit": 60, "remaining": 2 }));
    let run = forge(&world, &["pr", "9"]);
    assert_ne!(run.code, 0, "{}", run.describe());
    assert_eq!(run.error()["kind"], "RateLimited", "{}", run.describe());
    let state = world.state();
    assert_eq!(state["anonymous"]["remaining"], 0, "{state}");
    assert_eq!(state["anonymous"]["charged"], 2, "{}", state["requests"]);
    assert_eq!(
        state["anonymous_refused"].as_u64().unwrap_or(0),
        0,
        "a request went past the quota: {}",
        state["requests"]
    );

    // A revalidation that answers 304 is free.
    let world = anonymous_world(Forge::GitHub, json!({ "limit": 60 }));
    ok(&forge(&world, &["issue", "7"]));
    let charged = world.state()["anonymous"]["charged"].as_u64().unwrap();
    let again = forge(&world, &["issue", "7", "--refresh"]);
    ok(&again);
    assert_eq!(again.object()["source"], "revalidated", "{}", again.stdout);
    let state = world.state();
    assert_eq!(
        state["anonymous"]["charged"].as_u64().unwrap(),
        charged,
        "{state}"
    );
    assert!(state["not_modified"].as_u64().unwrap_or(0) >= 1);

    // In process: background work stops at half the anonymous quota.
    let world = anonymous_world(Forge::GitHub, json!({ "limit": 60, "remaining": 30 }));
    let host = "warm.anonymous.test";
    let (_, ctx) = github_context(&world, host);
    let github = provider::build(ProviderKind::GitHub).unwrap();
    let cancel = AtomicBool::new(false);
    let read =
        |ctx: &ForgeContext, read: Read| github.read(ctx, &read, &Conditional::default(), &cancel);
    read(&ctx, Read::Issue(7)).unwrap();
    // 29 of 60 left, and the host's chip says so.
    let account = host_state::account(&HostKey::new(host)).unwrap();
    let HostState::Anonymous {
        remaining, limit, ..
    } = &account.state
    else {
        panic!("{:?}", account.state);
    };
    assert_eq!((*remaining, *limit), (Some(29), Some(60)));
    assert_eq!(
        account.state.chip().as_deref(),
        Some("Anonymous · read-only · 29/60 requests left")
    );
    let sent = world.api_requests().len();
    ctx.set_background(true);
    // A background batch of pull requests sends nothing at all anonymously.
    match read(&ctx, Read::PullRequests(vec![9])).unwrap() {
        dusty::forge::provider::Fetched::Payload {
            value: Payload::PullRequests(pulls),
            ..
        } => assert!(pulls.is_empty()),
        other => panic!("{other:?}"),
    }
    // Under half the quota left: any other background read is held back, unsent.
    let held = read(&ctx, Read::Issue(7)).unwrap_err();
    assert!(matches!(held, ForgeError::RateLimited { .. }), "{held:?}");
    assert_eq!(world.api_requests().len(), sent, "background work was sent");
    // The user's own reads still go out.
    ctx.set_background(false);
    read(&ctx, Read::Issue(7)).unwrap();
    assert_eq!(world.api_requests().len(), sent + 1);
}

/// An instance that refuses every anonymous API call (`REQUIRE_SIGNIN_VIEW`) is a state with
/// the ways to sign in, not "Unknown forge".
#[test]
fn a_sign_in_required_instance_is_a_state_not_an_unknown_forge() {
    let refusal = json!({ "status": 403, "body": { "message": "Only signed in user is allowed to call APIs." } });
    let world = anonymous_world(Forge::Forgejo, json!({ "sign_in_required": true }));
    world.update(|state| {
        state["probes"] = json!({ "code.forgejo.org": {
            "api/v1/version": refusal, "api/forgejo/v1/version": refusal } });
    });
    let run = forge(&world, &["detect"]);
    assert_eq!(run.code, 4, "{}", run.describe());
    let detected = run.object();
    assert_eq!(detected["provider"], "Forgejo", "{}", run.stdout);
    assert_eq!(detected["sign_in_required"], true);
    assert_eq!(
        detected["error"]["kind"], "SignInRequired",
        "{}",
        run.stdout
    );
    let state = &detected["state"];
    assert_eq!(state["label"], "Instance requires sign-in", "{state}");
    assert_eq!(state["ready"], false);
    assert_eq!(state["sign_in_required"]["provider"], "Forgejo");
    let hints = state["sign_in_required"]["hints"].to_string();
    for hint in [
        "tea login add --url https://code.forgejo.org",
        "fj -H code.forgejo.org auth login",
        "FORGEJO_TOKEN=",
    ] {
        assert!(hints.contains(hint), "{hint}: {hints}");
    }
    let list = forge(&world, &["prs"]);
    assert_eq!(list.code, 4, "{}", list.describe());
    let error = list.error();
    assert_eq!(error["kind"], "SignInRequired", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("requires sign-in"),
        "{error}"
    );
    assert!(
        error["hint"].as_str().unwrap().contains("tea login add"),
        "{error}"
    );
    assert!(
        world.api_requests().is_empty(),
        "detection already knew: {:?}",
        world.api_requests()
    );

    // A host detection knows as Gitea whose API then refuses: the API's 403 says the same.
    let world = anonymous_world(Forge::Gitea, json!({ "sign_in_required": true }));
    let list = forge(&world, &["prs"]);
    assert_eq!(list.code, 4, "{}", list.describe());
    assert_eq!(
        list.error()["kind"],
        "SignInRequired",
        "{}",
        list.describe()
    );
    assert!(
        list.error()["hint"]
            .as_str()
            .unwrap()
            .contains("FORGEJO_TOKEN=")
    );
}
