#![cfg(unix)]
//! Live, read-only smoke of `dusty-git forge` against real forges.
//!
//! Ignored unless asked for twice: `DUSTY_FORGE_LIVE=1 cargo test --test forge_live -- --ignored
//! --nocapture`. Nothing here writes to a forge: only `detect`, lists, one pull request's detail,
//! threads, diff and timeline, CI runs with one job log, notifications, then the same detail
//! again from the cache and revalidated (`--refresh`, a 304 where the forge sends ETags).
//!
//! Hosts and repositories (override with the environment):
//! - github.com `DUSTY_LIVE_GITHUB_REPO` (default `blurskye/dust`), through the local `gh` login;
//! - github.com anonymously, `DUSTY_LIVE_GITHUB_PUBLIC_REPO` (default `cli/cli`): REST only, about
//!   25 of the address's 60 requests an hour;
//! - gitlab.com `DUSTY_LIVE_GITLAB_REPO` (default `gitlab-org/gitlab-runner`);
//! - codeberg.org `DUSTY_LIVE_CODEBERG_REPO` (default `forgejo/forgejo`);
//! - a private Forgejo instance, `DUSTY_LIVE_FORGEJO_HOST` with `DUSTY_LIVE_FORGEJO_REPO` (skipped
//!   unless both are set), which refuses every anonymous API call: without a credential
//!   (`DUSTY_FORGE_TOKEN` with `DUSTY_FORGE_HOST` set to that host, or a `tea` login with
//!   `DUSTY_LIVE_LOGINS=1`) it must report `SignInRequired`, with the ways to sign in.
//!
//! `DUSTY_GIT_BIN` runs a `dusty-git` built elsewhere (another machine) with this machine's
//! logins.
//!
//! Only GitHub uses the developer's own login (gh) unless `DUSTY_LIVE_LOGINS=1`; the other hosts
//! are read anonymously: read-only, every write capability `No("Sign in to …")`,
//! and the reads that need a sign-in (job logs on GitHub and GitLab, notifications) answer
//! "Sign in to …" without a request, which the tour reports as expected.
//! Tokens never reach argv or output: every output is checked against `gh auth token`.

use serde_json::Value;
use std::process::Command;
use std::time::Instant;

fn live() -> bool {
    if std::env::var("DUSTY_FORGE_LIVE").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set DUSTY_FORGE_LIVE=1 to read from real forges");
    false
}

struct Host {
    host: &'static str,
    repo: String,
    /// Whether the developer's own CLI logins and tokens may be used: always for GitHub (gh),
    /// for the others only with `DUSTY_LIVE_LOGINS=1`.
    logins: bool,
}

struct Live {
    directory: tempfile::TempDir,
    host: Host,
    secret: Option<String>,
}

struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
    millis: u128,
}

impl Outcome {
    fn object(&self) -> Option<Value> {
        serde_json::from_str(self.stdout.trim()).ok()
    }

    fn lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn error(&self) -> Value {
        self.stderr
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<Value>(line).ok())
            .map(|value| value["error"].clone())
            .unwrap_or(Value::Null)
    }
}

impl Live {
    fn new(host: Host) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |arguments: &[&str]| {
            let status = Command::new("git")
                .current_dir(&repo)
                .args(arguments)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q", "-b", "main"]);
        git(&[
            "remote",
            "add",
            "origin",
            &format!("https://{}/{}.git", host.host, host.repo),
        ]);
        let secret = Command::new("gh")
            .args(["auth", "token", "--hostname", "github.com"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|token| !token.is_empty());
        Self {
            directory,
            host,
            secret,
        }
    }

    fn repo(&self) -> std::path::PathBuf {
        self.directory.path().join("repo")
    }

    fn forge(&self, args: &[&str]) -> Outcome {
        // `DUSTY_GIT_BIN` runs a binary built elsewhere (another machine) against this machine's
        // credentials.
        let binary = std::env::var_os("DUSTY_GIT_BIN")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_dusty-git").into());
        let mut command = Command::new(binary);
        command
            .arg("forge")
            .arg("--repo")
            .arg(self.repo())
            .args(args)
            // A private cache, so the first read is a real request and the second is not.
            .env("DUSTY_FORGE_CACHE_DIR", self.directory.path().join("cache"))
            .env("GIT_TERMINAL_PROMPT", "0");
        for name in [
            "DUSTY_FORGE_API_BASE",
            "DUSTY_FORGE_ORIGIN",
            "DUSTY_FORGE_STATE_DIR",
            "DUSTY_FORGE_ANONYMOUS",
        ] {
            command.env_remove(name);
        }
        if !self.host.logins {
            // No stored login: only a token given for this host on purpose.
            let missing = self.directory.path().join("no-cli");
            command
                .env("DUSTY_FORGE_GH", &missing)
                .env("DUSTY_FORGE_GLAB", &missing)
                .env("DUSTY_FORGE_TEA", &missing)
                .env_remove("GITLAB_TOKEN")
                .env_remove("GITEA_TOKEN")
                .env_remove("FORGEJO_TOKEN")
                .env_remove("GH_TOKEN")
                .env_remove("GITHUB_TOKEN")
                .env_remove("GH_ENTERPRISE_TOKEN");
            if std::env::var("DUSTY_FORGE_HOST").as_deref() != Ok(self.host.host) {
                command.env_remove("DUSTY_FORGE_TOKEN");
            }
        }
        let started = Instant::now();
        let output = command.output().unwrap();
        let outcome = Outcome {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            millis: started.elapsed().as_millis(),
        };
        if let Some(secret) = &self.secret {
            assert!(
                !outcome.stdout.contains(secret.as_str())
                    && !outcome.stderr.contains(secret.as_str()),
                "a token reached the output of {args:?}"
            );
        }
        outcome
    }

    /// Runs one verb and prints a one-line report; returns the outcome when it succeeded.
    fn step(
        &self,
        label: &str,
        args: &[&str],
        describe: impl Fn(&Outcome) -> String,
    ) -> Option<Outcome> {
        let outcome = self.forge(args);
        if outcome.code == 0 {
            println!(
                "  ok   {label:<28} {:>5} ms  {}",
                outcome.millis,
                describe(&outcome)
            );
            Some(outcome)
        } else {
            let error = outcome.error();
            let sign_in = error["kind"] == "NotAuthenticated"
                && error["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with("Sign in to "));
            println!(
                "  {} {label:<28} {:>5} ms  exit {} {} · {}",
                if sign_in { "sign" } else { "FAIL" },
                outcome.millis,
                outcome.code,
                error["kind"],
                error["message"]
            );
            None
        }
    }

    /// `detect`'s state chip and quota (anonymous hosts).
    fn chip(&self) -> String {
        let detected = self.forge(&["detect"]).object().unwrap_or(Value::Null);
        format!(
            "{} · {}",
            detected["state"]["chip"], detected["state"]["anonymous"]
        )
    }

    /// The whole read-only tour; returns the steps that failed (detect and the list are
    /// required; the rest is reported).
    fn smoke(&self) -> Vec<String> {
        let mut failed = Vec::new();
        println!("== {} {}", self.host.host, self.host.repo);
        let detect = self.forge(&["detect"]);
        let detected = detect.object().unwrap_or(Value::Null);
        println!(
            "  {}  provider={} source={} version={} transport={} viewer={} cli={} state={}",
            if detect.code == 0 { "ok  " } else { "FAIL" },
            detected["provider"],
            detected["source"],
            detected["server_version"],
            detected["transport"],
            detected["viewer"],
            detected["cli"],
            detected["state"]["label"],
        );
        let anonymous = detected["transport"] == "anonymous";
        if anonymous {
            println!(
                "  anon read_only={} writes={} job_log={} chip={} hint={}",
                detected["read_only"],
                detected["capabilities"]["writes"],
                detected["capabilities"]["job_log"],
                detected["state"]["chip"],
                detected["state"]["anonymous"]["hint"],
            );
            if detected["read_only"] != true
                || detected["capabilities"]["writes"]["No"].as_str().is_none()
            {
                failed.push("anonymous but not read-only".into());
            }
        }
        if detect.code != 0 {
            let error = detect.error();
            println!(
                "       exit {} {} · {}",
                detect.code, error["kind"], error["message"]
            );
            // Without a credential the host is read anonymously, so a failed
            // detection is a failure, not a skip.
            failed.push("detect".into());
        }
        let list = self.step("prs", &["prs", "--state", "all"], |outcome| {
            format!("{} rows", outcome.lines().len())
        });
        let Some(list) = list else {
            failed.push("prs".into());
            return failed;
        };
        let number = std::env::var("DUSTY_LIVE_PR")
            .ok()
            .or_else(|| list.lines().first().map(|row| row["number"].to_string()));
        if let Some(number) = number {
            let detail = |outcome: &Outcome| {
                let object = outcome.object().unwrap_or(Value::Null);
                format!(
                    "#{} {:?} source={}",
                    object["data"]["row"]["number"],
                    object["data"]["row"]["title"].as_str().unwrap_or(""),
                    object["source"]
                )
            };
            if self
                .step(&format!("pr {number}"), &["pr", &number], detail)
                .is_none()
            {
                failed.push(format!("pr {number}"));
            }
            let sized = |outcome: &Outcome| {
                let object = outcome.object().unwrap_or(Value::Null);
                match &object["data"] {
                    Value::Array(items) => format!("{} items", items.len()),
                    Value::String(text) => format!("{} bytes", text.len()),
                    other => format!("{}", other.to_string().len()),
                }
            };
            for verb in ["threads", "diff", "timeline"] {
                if self
                    .step(&format!("{verb} {number}"), &[verb, &number], sized)
                    .is_none()
                {
                    failed.push(format!("{verb} {number}"));
                }
            }
            // The same detail again: fresh in the cache, then revalidated.
            if self
                .step(&format!("pr {number} (again)"), &["pr", &number], detail)
                .is_none()
            {
                failed.push("pr again".into());
            }
            if self
                .step(
                    &format!("pr {number} --refresh"),
                    &["pr", &number, "--refresh"],
                    detail,
                )
                .is_none()
            {
                failed.push("pr --refresh".into());
            }
        }
        let issues = self.step("issues", &["issues"], |outcome| {
            format!("{} rows", outcome.lines().len())
        });
        let issue =
            issues.and_then(|issues| issues.lines().first().map(|row| row["number"].to_string()));
        if let Some(issue) = issue {
            let shown = |outcome: &Outcome| {
                let object = outcome.object().unwrap_or(Value::Null);
                format!(
                    "#{} {:?} source={}",
                    object["data"]["row"]["number"],
                    object["data"]["row"]["title"].as_str().unwrap_or(""),
                    object["source"]
                )
            };
            if self
                .step(&format!("issue {issue}"), &["issue", &issue], shown)
                .is_none()
            {
                failed.push(format!("issue {issue}"));
            }
        }
        if let Some(runs) = self.step("runs", &["runs"], |outcome| {
            format!("{} runs", outcome.lines().len())
        }) {
            let first = runs
                .lines()
                .first()
                .map(|run| run["id"].as_str().unwrap_or("").to_owned());
            if let Some(id) = first.filter(|id| !id.is_empty()) {
                let summary = |outcome: &Outcome| {
                    let object = outcome.object().unwrap_or(Value::Null);
                    format!(
                        "{} jobs, status={} source={}",
                        object["data"]["jobs"].as_array().map_or(0, Vec::len),
                        object["data"]["run"]["status"],
                        object["source"]
                    )
                };
                let detail = self.step(&format!("run {id}"), &["run", &id], summary);
                // Again: from the cache, then revalidated (a 304 where the forge sends ETags).
                self.step(&format!("run {id} (again)"), &["run", &id], summary);
                self.step(
                    &format!("run {id} --refresh"),
                    &["run", &id, "--refresh"],
                    summary,
                );
                let job = detail.and_then(|outcome| {
                    let object = outcome.object()?;
                    object["data"]["jobs"]
                        .as_array()?
                        .iter()
                        .find_map(|job| job["id"].as_str().map(str::to_owned))
                });
                if let Some(job) = job {
                    self.step(&format!("log {job}"), &["log", &job], |outcome| {
                        let object = outcome.object().unwrap_or(Value::Null);
                        format!(
                            "{} bytes, truncated={}",
                            object["data"]["text"].as_str().map_or(0, str::len),
                            object["data"]["truncated"]
                        )
                    });
                }
            }
        }
        self.step("notifications", &["notifications"], |outcome| {
            format!("{} unread", outcome.lines().len())
        });
        if anonymous {
            println!("  quota after the tour: {}", self.chip());
        }
        failed
    }
}

fn repo(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.to_owned())
}

#[test]
#[ignore = "reads real forges; DUSTY_FORGE_LIVE=1"]
fn github_com_through_gh() {
    if !live() {
        return;
    }
    let live = Live::new(Host {
        host: "github.com",
        repo: repo("DUSTY_LIVE_GITHUB_REPO", "blurskye/dust"),
        logins: true,
    });
    let failed = live.smoke();
    assert!(failed.is_empty(), "github.com: {failed:?}");
}

#[test]
#[ignore = "reads real forges; DUSTY_FORGE_LIVE=1"]
fn gitlab_com_nested_group() {
    if !live() {
        return;
    }
    let live = Live::new(Host {
        host: "gitlab.com",
        repo: repo("DUSTY_LIVE_GITLAB_REPO", "gitlab-org/gitlab-runner"),
        logins: std::env::var("DUSTY_LIVE_LOGINS").as_deref() == Ok("1"),
    });
    let failed = live.smoke();
    assert!(failed.is_empty(), "gitlab.com: {failed:?}");
}

#[test]
#[ignore = "reads real forges; DUSTY_FORGE_LIVE=1"]
fn codeberg_org() {
    if !live() {
        return;
    }
    let live = Live::new(Host {
        host: "codeberg.org",
        repo: repo("DUSTY_LIVE_CODEBERG_REPO", "forgejo/forgejo"),
        logins: std::env::var("DUSTY_LIVE_LOGINS").as_deref() == Ok("1"),
    });
    let failed = live.smoke();
    assert!(failed.is_empty(), "codeberg.org: {failed:?}");
}

#[test]
#[ignore = "reads real forges; DUSTY_FORGE_LIVE=1"]
fn github_com_anonymous() {
    if !live() {
        return;
    }
    let live = Live::new(Host {
        host: "github.com",
        repo: repo("DUSTY_LIVE_GITHUB_PUBLIC_REPO", "cli/cli"),
        logins: false,
    });
    let failed = live.smoke();
    assert!(failed.is_empty(), "github.com anonymously: {failed:?}");
}

#[test]
#[ignore = "reads real forges; DUSTY_FORGE_LIVE=1"]
fn forgejo_private_instance() {
    if !live() {
        return;
    }
    let (Ok(host), Ok(private_repo)) = (
        std::env::var("DUSTY_LIVE_FORGEJO_HOST"),
        std::env::var("DUSTY_LIVE_FORGEJO_REPO"),
    ) else {
        println!(
            "== private Forgejo skipped (set DUSTY_LIVE_FORGEJO_HOST and DUSTY_LIVE_FORGEJO_REPO)"
        );
        return;
    };
    let host: &'static str = Box::leak(host.into_boxed_str());
    let logins = std::env::var("DUSTY_LIVE_LOGINS").as_deref() == Ok("1");
    let token = std::env::var("DUSTY_FORGE_HOST").as_deref() == Ok(host);
    let live = Live::new(Host {
        host,
        repo: private_repo,
        logins,
    });
    if logins || token {
        let failed = live.smoke();
        assert!(failed.is_empty(), "{host}: {failed:?}");
        return;
    }
    // No credential: the instance refuses anonymous API calls, which is a state with the ways
    // to sign in, not an unknown forge.
    println!("== {host} (no credential)");
    for args in [&["detect"][..], &["prs"]] {
        let outcome = live.forge(args);
        let error = outcome.error();
        let detected = outcome.object().unwrap_or(Value::Null);
        println!(
            "  {:?}: exit {} kind={} provider={} sign_in_required={} state={} message={} hint={}",
            args,
            outcome.code,
            error["kind"],
            detected["provider"],
            detected["sign_in_required"],
            detected["state"]["label"],
            error["message"],
            error["hint"],
        );
        assert_eq!(outcome.code, 4, "{args:?}: {}", outcome.stderr);
        assert_eq!(
            error["kind"], "SignInRequired",
            "{args:?}: {}",
            outcome.stderr
        );
        let hint = error["hint"].as_str().unwrap_or_default();
        assert!(hint.contains("tea login add"), "{hint}");
        assert!(hint.contains("FORGEJO_TOKEN"), "{hint}");
        if args == ["detect"] {
            assert!(
                matches!(detected["provider"].as_str(), Some("Forgejo" | "Gitea")),
                "{detected}"
            );
            assert_eq!(detected["sign_in_required"], true, "{detected}");
            assert_eq!(detected["state"]["label"], "Instance requires sign-in");
        }
    }
}

#[test]
fn live_tests_stay_off_without_the_switch() {
    // Guards the gate itself: without the variable nothing here touches the network.
    if std::env::var("DUSTY_FORGE_LIVE").as_deref() != Ok("1") {
        assert!(!live());
    }
}
