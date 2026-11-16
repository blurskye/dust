#![cfg(unix)]
//! `dusty-git forge <verb>` (FORGE.md §5.3, F7): every verb against the fake forge of each
//! provider (GitHub, GitLab, Gitea, Forgejo), through detection and the shared on-disk cache
//! exactly as other editors and scripts run it. Output is JSON only with `"schema": 1` in every
//! top-level object, errors are JSON on stderr with a fixed exit code, and the token never shows.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use forge_fake::FAKE_TOKEN;
use forge_world::ALL;
use forge_world::Forge;
use forge_world::World;
use forge_world::git;
use serde_json::Value;
use serde_json::json;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

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

    /// The single top-level object on stdout.
    fn object(&self) -> Value {
        let value: Value = serde_json::from_str(self.stdout.trim()).unwrap_or_else(|error| {
            panic!(
                "stdout is not one JSON object ({error}): {}",
                self.describe()
            )
        });
        assert_eq!(value["schema"], 1, "{}", self.describe());
        value
    }

    /// JSON Lines on stdout, each stamped with the schema.
    fn lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let value: Value = serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("not a JSON line ({error}): {line}"));
                assert_eq!(value["schema"], 1, "{line}");
                value
            })
            .collect()
    }

    /// The error object on stderr.
    fn error(&self) -> Value {
        let line = self
            .stderr
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("no JSON error on stderr: {}", self.describe()));
        let value: Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["schema"], 1, "{line}");
        for key in ["kind", "message", "hint"] {
            assert!(value["error"][key].is_string(), "{key} missing: {line}");
        }
        value["error"].clone()
    }
}

fn forge(world: &World, args: &[&str]) -> Run {
    forge_with(world, args, None, |_| {})
}

fn forge_with(
    world: &World,
    args: &[&str],
    stdin: Option<&str>,
    extra: impl FnOnce(&mut Command),
) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dusty-git"));
    command
        .arg("forge")
        .arg("--repo")
        .arg(&world.repo)
        .args(args)
        .current_dir(&world.repo)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    world.configure(&mut command);
    extra(&mut command);
    let mut child = command.spawn().unwrap();
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let run = Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    assert!(
        [FAKE_TOKEN, REFUSED_TOKEN]
            .iter()
            .all(|token| !run.stdout.contains(token) && !run.stderr.contains(token)),
        "the token leaked: {}",
        run.describe()
    );
    run
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

fn ok(run: &Run) {
    assert_eq!(run.code, 0, "{}", run.describe());
}

#[test]
fn every_read_verb_answers_with_the_schema_on_every_provider() {
    let mut failures = Vec::new();
    for forge_kind in ALL {
        let world = World::new(forge_kind);
        let world = &world;
        let pull = forge_kind.pull().to_string();
        let issue = forge_kind.issue().to_string();
        let name = forge_kind.name();
        let label = |verb: &str| format!("{name} {verb}");

        attempt(&mut failures, label("detect"), || {
            let run = forge(world, &["detect"]);
            ok(&run);
            let detected = run.object();
            assert_eq!(detected["provider"], name, "{}", run.stdout);
            assert_eq!(detected["host"], forge_kind.host());
            assert_eq!(detected["repo"], forge_kind.repo());
            assert_eq!(detected["remote"], "origin");
            assert!(detected["transport"].is_string(), "{}", run.stdout);
            assert!(detected["capabilities"].is_object(), "{}", run.stdout);
            assert!(detected["terms"].is_object(), "{}", run.stdout);
            assert!(detected["viewer"].is_string(), "{}", run.stdout);
            assert_eq!(
                detected["endpoints"]["web_base"],
                format!("https://{}", forge_kind.host())
            );
            assert!(detected.get("error").is_none(), "{}", run.stdout);
        });

        let list_verbs: &[&str] = if forge_kind == Forge::GitLab {
            &["prs", "mrs"]
        } else {
            &["prs"]
        };
        for &verb in list_verbs {
            attempt(&mut failures, label(verb), || {
                let run = forge(world, &[verb]);
                ok(&run);
                let rows = run.lines();
                assert!(
                    rows.iter().any(|row| row["number"] == forge_kind.pull()),
                    "{}",
                    run.stdout
                );
                assert!(rows.iter().all(|row| row["title"].is_string()));
            });
        }
        // The Gitea/Forgejo recordings hold only the default filters.
        let filters = matches!(forge_kind, Forge::GitHub | Forge::GitLab);
        if filters {
            attempt(&mut failures, label("prs --state all --page 1"), || {
                let run = forge(world, &["prs", "--state", "all", "--page", "1"]);
                ok(&run);
                run.lines();
            });
        }
        attempt(&mut failures, label("issues"), || {
            let run = forge(world, &["issues"]);
            ok(&run);
            assert!(!run.lines().is_empty(), "{}", run.stdout);
        });
        attempt(&mut failures, label("pr"), || {
            let run = forge(world, &["pr", &pull]);
            ok(&run);
            let object = run.object();
            assert_eq!(object["provider"], name);
            assert_eq!(object["host"], forge_kind.host());
            assert_eq!(object["repo"], forge_kind.repo());
            assert_eq!(object["source"], "network");
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
            let text = object["data"].as_str().unwrap_or_default();
            assert!(text.contains("diff --git"), "{}", run.stdout);
        });
        attempt(&mut failures, label("runs"), || {
            let run = forge(world, &["runs"]);
            ok(&run);
            assert!(!run.lines().is_empty(), "{}", run.stdout);
        });
        if filters {
            attempt(&mut failures, label("runs --branch main"), || {
                let run = forge(world, &["runs", "--branch", "main", "--event", "push"]);
                ok(&run);
                run.lines();
            });
        }
        attempt(&mut failures, label("run"), || {
            let run = forge(world, &["run", forge_kind.run()]);
            ok(&run);
            assert!(run.object()["data"].is_object(), "{}", run.stdout);
        });
        attempt(&mut failures, label("log"), || {
            let run = forge(world, &["log", forge_kind.job()]);
            ok(&run);
            assert!(run.object()["data"].is_object(), "{}", run.stdout);
        });
        attempt(&mut failures, label("artifacts"), || {
            let run = forge(world, &["artifacts", forge_kind.run()]);
            ok(&run);
            assert!(!run.object()["data"].is_null(), "{}", run.stdout);
        });
        attempt(&mut failures, label("notifications"), || {
            let run = forge(world, &["notifications"]);
            ok(&run);
            run.lines();
        });
        attempt(&mut failures, label("pretty"), || {
            let run = forge(world, &["issue", &issue, "--pretty"]);
            ok(&run);
            assert!(run.stdout.contains("\n  \"schema\": 1"), "{}", run.stdout);
            run.object();
        });
        attempt(&mut failures, label("reads only"), || {
            let writes = world
                .requests()
                .into_iter()
                .filter(|request| {
                    !matches!(request["method"].as_str(), Some("GET" | "HEAD") | None)
                        && request["path"] != "graphql"
                })
                .collect::<Vec<_>>();
            assert!(writes.is_empty(), "reads wrote: {writes:?}");
        });
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The second run is served by the shared disk cache without a request; `--refresh` asks
/// again; `cache stats|clear` see and drop the entries.
#[test]
fn second_reads_come_from_the_shared_cache() {
    let mut failures = Vec::new();
    for forge_kind in ALL {
        let world = World::new(forge_kind);
        let world = &world;
        let pull = forge_kind.pull().to_string();
        attempt(&mut failures, forge_kind.name().into(), || {
            let first = forge(world, &["pr", &pull]);
            ok(&first);
            assert_eq!(first.object()["source"], "network");
            let before = world.api_requests().len();
            assert!(before > 0);
            let second = forge(world, &["pr", &pull]);
            ok(&second);
            let cached = second.object();
            assert_eq!(cached["source"], "cache", "{}", second.stdout);
            assert_eq!(cached["provider"], forge_kind.name());
            assert_eq!(cached["data"], first.object()["data"]);
            assert_eq!(
                world.api_requests().len(),
                before,
                "a fresh cache entry was asked again: {:?}",
                &world.api_requests()[before..]
            );
            let refreshed = forge(world, &["pr", &pull, "--refresh"]);
            ok(&refreshed);
            let source = refreshed.object()["source"].clone();
            assert!(source == "network" || source == "revalidated", "{source}");
            assert!(world.api_requests().len() > before);

            let stats = forge(world, &["cache", "stats"]);
            ok(&stats);
            let stats = stats.object();
            assert!(stats["entries"].as_u64().unwrap() > 0, "{stats}");
            assert!(stats["bytes"].as_u64().unwrap() > 0, "{stats}");
            assert!(!stats["hosts"].as_object().unwrap().is_empty(), "{stats}");
            assert!(
                Path::new(stats["root"].as_str().unwrap()).starts_with(world.cache_dir()),
                "{stats}"
            );
            let cleared = forge(world, &["cache", "clear"]);
            ok(&cleared);
            assert_eq!(cleared.object()["cleared"], true);
            let empty = forge(world, &["cache", "stats"]);
            ok(&empty);
            assert_eq!(empty.object()["entries"], 0, "{}", empty.stdout);
            let again = forge(world, &["pr", &pull]);
            ok(&again);
            assert_eq!(again.object()["source"], "network");
        });
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

const REFUSED_TOKEN: &str = "glpat-refused0123456789abcdef";

/// No token and no forge CLI anywhere: `PATH` holds only `git`.
fn without_credentials(world: &World, command: &mut Command) {
    let bin = world.directory.path().join("git-only");
    if !bin.exists() {
        std::fs::create_dir_all(&bin).unwrap();
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("git"))
            .find(|path| path.is_file())
            .unwrap();
        std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    }
    command
        .env_remove("DUSTY_FORGE_TOKEN")
        .env("DUSTY_FORGE_GH", "")
        .env("DUSTY_FORGE_GLAB", "")
        .env("DUSTY_FORGE_TEA", "")
        .env("PATH", &bin);
}

#[test]
fn errors_are_json_on_stderr_with_fixed_exit_codes() {
    let world = World::new(Forge::GitHub);
    for args in [
        &["pr"][..],
        &["frobnicate"],
        &["prs", "--state", "sideways"],
        &["pr", "nine"],
        &["comment", "9", "--body-file", "/nonexistent/body.md"],
    ] {
        let run = forge(&world, args);
        assert_eq!(run.code, 2, "{args:?}: {}", run.describe());
        assert_eq!(run.error()["kind"], "Usage", "{args:?}");
        assert!(run.stdout.is_empty(), "{args:?}: {}", run.stdout);
    }
    let help = forge(&world, &["--help"]);
    assert_eq!(help.code, 0, "{}", help.describe());
    assert!(help.stdout.contains("Exit codes"), "{}", help.stdout);

    // No token and no gh, with anonymous reads turned off: the CLI is missing (3), with the
    // install hint. (Anonymous reads: tests/forge_anonymous.rs.)
    let no_cli = forge_with(&world, &["pr", "9"], None, |command| {
        without_credentials(&world, command);
        command.env("DUSTY_FORGE_ANONYMOUS", "0");
    });
    assert_eq!(no_cli.code, 3, "{}", no_cli.describe());
    let error = no_cli.error();
    assert_eq!(error["kind"], "CliMissing");
    assert!(
        error["hint"].as_str().unwrap().contains("install"),
        "{error}"
    );

    // A token the forge refuses, with direct HTTP required (no CLI to fall back to): not
    // signed in (4), and the token is not repeated.
    let refused = forge_with(&world, &["pr", "9"], None, |command| {
        without_credentials(&world, command);
        command
            .env("DUSTY_FORGE_TOKEN", REFUSED_TOKEN)
            .env("DUSTY_FORGE_TRANSPORT", "http");
    });
    assert_eq!(refused.code, 4, "{}", refused.describe());
    assert_eq!(refused.error()["kind"], "NotAuthenticated");

    // Nothing listening: 6.
    let offline = forge_with(&world, &["issue", "7"], None, |command| {
        command
            .env("DUSTY_FORGE_API_BASE", "http://127.0.0.1:9/{host}/api/v3")
            .env("DUSTY_FORGE_ORIGIN", "http://127.0.0.1:9/{host}");
    });
    assert_eq!(offline.code, 6, "{}", offline.describe());
    assert!(
        matches!(
            offline.error()["kind"].as_str(),
            Some("Network" | "Timeout")
        ),
        "{}",
        offline.stderr
    );

    // No repository: 7.
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_dusty-git"));
    command
        .args(["forge", "--repo"])
        .arg(directory.path())
        .arg("prs")
        .env("GIT_CEILING_DIRECTORIES", directory.path());
    world.configure(&mut command);
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&output.stderr).contains("\"schema\":1"));

    // Gitea and Forgejo: a task without a log is unsupported (5), a missing pull request is
    // not found (7).
    for forge_kind in [Forge::Gitea, Forge::Forgejo] {
        let world = World::new(forge_kind);
        let unsupported = forge(&world, &["log", "task-1"]);
        assert_eq!(unsupported.code, 5, "{}", unsupported.describe());
        assert_eq!(unsupported.error()["kind"], "Unsupported");
        let missing = forge(&world, &["pr", "5"]);
        assert_eq!(missing.code, 7, "{}", missing.describe());
        assert_eq!(missing.error()["kind"], "NotFound", "{}", missing.stderr);
    }

    // Detection still describes the host when signing in fails, and exits with the code.
    let gitlab = World::new(Forge::GitLab);
    let detected = forge_with(&gitlab, &["detect"], None, |command| {
        without_credentials(&gitlab, command);
        command
            .env("DUSTY_FORGE_TOKEN", REFUSED_TOKEN)
            .env("DUSTY_FORGE_TRANSPORT", "http");
    });
    assert_eq!(detected.code, 4, "{}", detected.describe());
    let object = detected.object();
    assert_eq!(object["provider"], "GitLab");
    assert_eq!(object["error"]["kind"], "NotAuthenticated");
}

/// `comment N --body-file F` posts through the provider's own comment endpoint.
#[test]
fn comments_are_posted_from_a_body_file() {
    let mut failures = Vec::new();
    for forge_kind in ALL {
        let world = World::new(forge_kind);
        let world = &world;
        let number = forge_kind.pull();
        let repo = forge_kind.repo();
        if matches!(forge_kind, Forge::Gitea | Forge::Forgejo) {
            world.update(|state| {
                state["answers"][format!("POST repos/{repo}/issues/{number}/comments")] = json!({
                    "status": 201,
                    "body": { "id": 77, "body": "Looks right.",
                              "html_url": format!("https://{}/{repo}/pulls/{number}#issuecomment-77",
                                                  forge_kind.host()) },
                });
            });
        }
        attempt(&mut failures, forge_kind.name().into(), || {
            let body = world.directory.path().join("body.md");
            std::fs::write(&body, "Looks right.").unwrap();
            let run = forge(
                world,
                &[
                    "comment",
                    &number.to_string(),
                    "--body-file",
                    body.to_str().unwrap(),
                ],
            );
            ok(&run);
            let receipt = run.object();
            assert_eq!(receipt["ok"], true, "{}", run.stdout);
            assert!(receipt["summary"].is_string(), "{}", run.stdout);
            let posted = world
                .requests()
                .into_iter()
                .filter(|request| request["method"] == "POST")
                .collect::<Vec<_>>();
            assert!(
                posted
                    .iter()
                    .any(|request| request["body"]["body"] == "Looks right."),
                "{posted:?}"
            );
            if forge_kind == Forge::GitLab {
                assert!(posted.iter().any(|request| {
                    request["path"]
                        .as_str()
                        .unwrap_or("")
                        .ends_with(&format!("merge_requests/{number}/notes"))
                }));
            } else {
                assert!(posted.iter().any(|request| {
                    request["path"]
                        .as_str()
                        .unwrap_or("")
                        .ends_with(&format!("issues/{number}/comments"))
                }));
            }
            // An empty body is a usage error and sends nothing.
            std::fs::write(&body, "  \n").unwrap();
            let before = world.requests().len();
            let empty = forge(
                world,
                &[
                    "comment",
                    &number.to_string(),
                    "--body-file",
                    body.to_str().unwrap(),
                ],
            );
            assert_eq!(empty.code, 2, "{}", empty.describe());
            assert_eq!(world.requests().len(), before);
        });
    }
    // The body can come from stdin.
    let world = World::new(Forge::GitHub);
    let run = forge_with(
        &world,
        &["comment", "7", "--issue", "--body-file", "-"],
        Some("From stdin."),
        |_| {},
    );
    ok(&run);
    assert!(
        world.state()["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|comment| comment == "From stdin.")
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// `open N` hands the provider's own web URL to `$BROWSER`.
#[test]
fn open_hands_the_provider_url_to_the_browser() {
    let mut failures = Vec::new();
    for forge_kind in ALL {
        let world = World::new(forge_kind);
        let world = &world;
        attempt(&mut failures, forge_kind.name().into(), || {
            let log = world.directory.path().join("browser.log");
            let browser = world.directory.path().join("browser.sh");
            forge_fake::write_executable(
                &browser,
                format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\n", log.display()).as_bytes(),
            );
            let before = world.requests().len();
            for args in [
                vec!["open".to_owned(), forge_kind.pull().to_string()],
                vec![
                    "open".to_owned(),
                    forge_kind.issue().to_string(),
                    "--issue".into(),
                ],
            ] {
                let args = args.iter().map(String::as_str).collect::<Vec<_>>();
                let run = forge_with(world, &args, None, |command| {
                    command.env("BROWSER", &browser);
                });
                ok(&run);
                assert_eq!(run.object()["ok"], true);
            }
            let base = format!("https://{}/{}", forge_kind.host(), forge_kind.repo());
            let (pull, issue) = (forge_kind.pull(), forge_kind.issue());
            let expected = match forge_kind {
                Forge::GitHub => [
                    format!("{base}/pull/{pull}"),
                    format!("{base}/issues/{issue}"),
                ],
                Forge::GitLab => [
                    format!("{base}/-/merge_requests/{pull}"),
                    format!("{base}/-/issues/{issue}"),
                ],
                Forge::Gitea | Forge::Forgejo => [
                    format!("{base}/pulls/{pull}"),
                    format!("{base}/issues/{issue}"),
                ],
            };
            let opened = std::fs::read_to_string(&log).unwrap_or_default();
            assert_eq!(opened.lines().collect::<Vec<_>>(), expected, "{opened}");
            let writes = world.requests()[before..]
                .iter()
                .filter(|request| request["method"] != "GET" && request["path"] != "graphql")
                .cloned()
                .collect::<Vec<_>>();
            assert!(writes.is_empty(), "opening wrote: {writes:?}");
        });
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The ref a forge publishes a pull/merge request's head under.
fn pull_ref(forge_kind: Forge) -> String {
    match forge_kind {
        Forge::GitLab => format!("refs/merge-requests/{}/head", forge_kind.pull()),
        _ => format!("refs/pull/{}/head", forge_kind.pull()),
    }
}

/// `checkout N` without gh/glab fetches the PR ref from a local bare "forge" (the remote's
/// https URL is rewritten to it with `url.<bare>.insteadOf`), checks the head branch out, sets
/// its upstream and remembers the base in `branch.<b>.gh-merge-base`.
#[test]
fn checkout_fetches_the_pull_request_ref() {
    let mut failures = Vec::new();
    for forge_kind in ALL {
        let world = World::new(forge_kind);
        let world = &world;
        attempt(&mut failures, forge_kind.name().into(), || {
            let bare = world.directory.path().join("forge.git");
            let output = Command::new("git")
                .args(["init", "-q", "--bare", "-b", "main"])
                .arg(&bare)
                .output()
                .unwrap();
            assert!(output.status.success());
            let bare_path = bare.to_str().unwrap();
            let repo = &world.repo;
            git(repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
            git(repo, &["push", "-q", bare_path, "HEAD:refs/heads/main"]);
            git(repo, &["checkout", "-q", "-b", "scratch"]);
            std::fs::write(repo.join("change.txt"), "pull request\n").unwrap();
            git(repo, &["add", "change.txt"]);
            git(repo, &["commit", "-q", "-m", "pull request"]);
            let head = git(repo, &["rev-parse", "HEAD"]);
            git(
                repo,
                &[
                    "push",
                    "-q",
                    bare_path,
                    &format!("HEAD:{}", pull_ref(forge_kind)),
                ],
            );
            git(repo, &["checkout", "-q", "main"]);
            git(repo, &["branch", "-q", "-D", "scratch"]);
            let url = format!("https://{}/{}.git", forge_kind.host(), forge_kind.repo());
            let insteadof = format!("url.{bare_path}.insteadOf");
            let checkout = |number: u64| {
                forge_with(world, &["checkout", &number.to_string()], None, |command| {
                    command
                        .env("GIT_CONFIG_COUNT", "1")
                        .env("GIT_CONFIG_KEY_0", &insteadof)
                        .env("GIT_CONFIG_VALUE_0", &url);
                })
            };
            let run = checkout(forge_kind.pull());
            ok(&run);
            let receipt = run.object();
            let summary = receipt["summary"].as_str().unwrap_or_default().to_owned();
            let branch = summary
                .strip_prefix("Checked out ")
                .unwrap_or_else(|| panic!("{summary}"))
                .to_owned();
            assert_eq!(git(repo, &["branch", "--show-current"]), branch);
            assert_eq!(git(repo, &["rev-parse", "HEAD"]), head);
            match forge_kind {
                Forge::GitHub => assert_eq!(branch, "feature/native"),
                Forge::GitLab => assert_eq!(branch, "retry-uploads"),
                Forge::Forgejo => assert_eq!(branch, "docker-socket"),
                Forge::Gitea => assert!(!branch.is_empty()),
            }
            assert_eq!(
                git(repo, &["config", &format!("branch.{branch}.remote")]),
                "origin"
            );
            assert!(!git(repo, &["config", &format!("branch.{branch}.merge")]).is_empty());
            assert!(
                !git(repo, &["config", &format!("branch.{branch}.gh-merge-base")]).is_empty(),
                "the base branch is remembered"
            );
            // Checking out again fast-forwards (here: nothing to do) and keeps the branch.
            let again = checkout(forge_kind.pull());
            ok(&again);
            assert_eq!(git(repo, &["rev-parse", "HEAD"]), head);
            // A pull request the forge does not publish fails and writes no upstream.
            git(repo, &["checkout", "-q", "main"]);
            let missing = checkout(forge_kind.issue());
            assert_ne!(missing.code, 0, "{}", missing.describe());
            missing.error();
            let config = git(repo, &["config", "--list", "--local"]);
            assert!(
                !config.contains(&format!("/{}/head", forge_kind.issue())),
                "{config}"
            );
        });
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
