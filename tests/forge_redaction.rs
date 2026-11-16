#![cfg(unix)]
//! Tokens never leave the credential: not in argv, not in error text,
//! notices or the accounts popover, not in Debug output, not in `hosts.json`. `redact` knows
//! every forge's token shapes and, verbatim, every token Dusty has read.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dust::forge::cli;
use dust::forge::credential::Credential;
use dust::forge::credential::CredentialSource;
use dust::forge::credential::CredentialState;
use dust::forge::credential::Env;
use dust::forge::credential::Secret;
use dust::forge::credential::Stores;
use dust::forge::detect;
use dust::forge::detect::DetectInputs;
use dust::forge::detect::ForgeConfig;
use dust::forge::host_state;
use dust::forge::http::HttpTransport;
use dust::forge::provider::ForgeContext;
use dust::forge::provider::ProviderKind;
use dust::forge::redact;
use dust::forge::transport;
use dust::forge::transport::ApiRequest;
use dust::forge::transport::Cli;
use dust::forge::transport::ConnectOptions;
use dust::forge::transport::Endpoints;
use dust::forge::transport::HostKey;
use forge_fake::ForgeServer;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const GLAB_TOKEN: &str = "glpat-redactfixture0123456789";
const GITEA_TOKEN: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";

#[test]
fn every_token_shape_is_redacted() {
    for (text, secret) in [
        (
            "gh: token ghp_abcdefghijklmnopqrstuvwxyz012345 rejected",
            "ghp_abcdefghijklmnopqrstuvwxyz012345",
        ),
        (
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
        ),
        (
            "glab: glpat-abcdefghij0123456789 expired",
            "glpat-abcdefghij0123456789",
        ),
        ("PRIVATE-TOKEN: anything-at-all", "anything-at-all"),
        (
            "Authorization: token 0123456789abcdef0123456789abcdef01234567",
            "0123456789abcdef0123456789abcdef01234567",
        ),
        (
            "https://x/?token=0123456789abcdef0123456789abcdef01234567",
            "0123456789abcdef0123456789abcdef01234567",
        ),
        (
            "    token: s3cr3t-config-value-0001",
            "s3cr3t-config-value-0001",
        ),
        (
            "{\"token\": \"s3cr3t-json-value-000002\"}",
            "s3cr3t-json-value-000002",
        ),
        (
            "fatal: https://maya:hunter2hunter2@git.example/o/r.git",
            "hunter2hunter2",
        ),
    ] {
        let redacted = redact(text);
        assert!(!redacted.contains(secret), "{text:?} → {redacted:?}");
        assert!(redacted.contains("<redacted>"), "{text:?} → {redacted:?}");
    }
    // Commit hashes in ordinary text survive.
    let sha = "error: 0123456789abcdef0123456789abcdef01234567 is not a commit";
    assert_eq!(redact(sha), sha);
}

#[test]
fn a_token_dusty_has_read_is_redacted_verbatim_whatever_its_shape() {
    let odd = "plainwordsecret42";
    assert_eq!(redact(&format!("echo {odd}")), format!("echo {odd}"));
    let secret = Secret::new(odd);
    assert_eq!(redact(&format!("echo {odd}")), "echo <redacted>");
    assert_eq!(format!("{secret:?} · {secret}"), "<redacted> · <redacted>");
    let credential = Credential::new(GITEA_TOKEN, CredentialSource::FjKeys, Some("maya".into()));
    let state = CredentialState::Present(credential.clone());
    for text in [format!("{credential:?}"), format!("{state:?}")] {
        assert!(!text.contains(GITEA_TOKEN), "{text}");
    }
    let env = Env::from_pairs([
        ("GITLAB_TOKEN", GLAB_TOKEN),
        ("DUSTY_FORGE_TOKEN", "ghp_dustyfixture0123456789abcdef"),
    ]);
    let text = format!("{env:?}");
    assert!(
        text.contains("GITLAB_TOKEN") && !text.contains(GLAB_TOKEN) && !text.contains("ghp_dusty"),
        "{text}"
    );
}

struct World {
    directory: tempfile::TempDir,
    server: ForgeServer,
}

impl World {
    fn new(state: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        std::fs::write(&path, state.to_string()).unwrap();
        let server = ForgeServer::start(&path, "legacy");
        Self { directory, server }
    }

    fn requests(&self) -> Vec<Value> {
        forge_fake::requests(&self.directory.path().join("state.json"))
    }

    fn env(&self) -> Env {
        Env::from_pairs([("DUSTY_FORGE_ORIGIN", self.server.origin())])
    }

    fn endpoints(&self, kind: ProviderKind, host: &str) -> Endpoints {
        detect::endpoints_for(kind, &HostKey::new(host), None, &self.env())
    }
}

#[test]
fn echoed_tokens_never_reach_error_text_or_host_states() {
    let world = World::new(json!({
        "tokens": [GLAB_TOKEN, GITEA_TOKEN],
        "answers": {
            "GET projects/1": {"status": 422, "body": {"message": format!("token {GLAB_TOKEN} lacks api scope")}},
            "GET repos/o/r": {"status": 500, "body": {"message": format!("bad token {GITEA_TOKEN}")}}
        }
    }));
    for (kind, host, token, path) in [
        (
            ProviderKind::GitLab,
            "gl.redact.test",
            GLAB_TOKEN,
            "projects/1",
        ),
        (
            ProviderKind::Forgejo,
            "fj.redact.test",
            GITEA_TOKEN,
            "repos/o/r",
        ),
    ] {
        let endpoints = world.endpoints(kind, host);
        let http = HttpTransport::new(
            endpoints.clone(),
            Credential::new(token, CredentialSource::Env("DUSTY_FORGE_TOKEN"), None),
        )
        .unwrap();
        http.check_viewer(&AtomicBool::new(false)).unwrap();
        let ctx = ForgeContext::new(
            world.directory.path().to_path_buf(),
            endpoints,
            ConnectOptions::default(),
            None,
            Arc::new(http),
        );
        let response = ctx
            .send(&ApiRequest::get(path), &AtomicBool::new(false))
            .unwrap();
        let error = dust::forge::http::classify(&world.endpoints(kind, host), &response);
        let text = format!("{error} {error:?}");
        assert!(!text.contains(token), "{kind:?}: {text}");
        let account = host_state::account(&HostKey::new(host)).unwrap();
        let lines = format!("{:?}", account.lines());
        assert!(!lines.contains(token), "{lines}");
        assert!(
            lines.contains("$DUSTY_FORGE_TOKEN") && lines.contains("rowan"),
            "{lines}"
        );
    }
    // Over HTTP the token travels in a header only, never in the URL.
    for request in world.requests() {
        let path = request["path"].as_str().unwrap_or_default();
        assert!(
            !path.contains(GLAB_TOKEN) && !path.contains(GITEA_TOKEN),
            "{path}"
        );
        assert!(
            request["query"]
                .as_object()
                .is_none_or(|query| query.values().all(|value| {
                    let value = value.as_str().unwrap_or_default();
                    value != GLAB_TOKEN && value != GITEA_TOKEN
                })),
            "{request}"
        );
    }
}

#[test]
fn cli_passthrough_never_puts_a_token_in_argv() {
    let world = World::new(json!({}));
    let bin = world.directory.path().join("bin");
    let glab = world.server.install_glab(&bin);
    let tea = world.server.install_tea(
        &bin,
        json!([{"name": "work", "url": "https://tea.redact.test", "default": true}]),
    );
    let request = ApiRequest::get("user").header("X-Test", "1");
    for (cli, args) in [
        (
            Cli::Gh,
            cli::argv(Cli::Gh, &HostKey::new("gh.redact.test"), &request, None),
        ),
        (
            Cli::Glab,
            cli::argv(Cli::Glab, &HostKey::new("gl.redact.test"), &request, None),
        ),
        (
            Cli::Tea,
            cli::argv(
                Cli::Tea,
                &HostKey::new("tea.redact.test"),
                &request,
                Some("work"),
            ),
        ),
    ] {
        let (argv, _, env) = args;
        let joined = format!("{argv:?} {env:?}");
        for word in [
            "token",
            "PRIVATE-TOKEN",
            "Authorization",
            GLAB_TOKEN,
            GITEA_TOKEN,
        ] {
            assert!(!joined.contains(word), "{cli:?}: {joined}");
        }
    }
    // And end to end: what the fake CLIs were given.
    let options = ConnectOptions {
        glab: Some(glab),
        tea: Some(tea),
        stores: Stores {
            tea_config: Some(write(
                &world,
                "tea/config.yml",
                "logins:\n  - name: work\n    url: https://tea.redact.test\n    default: true\n",
            )),
            ..Stores::default()
        },
        ..ConnectOptions::default()
    };
    for (kind, host) in [
        (ProviderKind::GitLab, "gl.redact.test"),
        (ProviderKind::Gitea, "tea.redact.test"),
    ] {
        let chosen = transport::passthrough(&world.endpoints(kind, host), &options).unwrap();
        chosen
            .send(&HostKey::new(host), &request, &AtomicBool::new(false))
            .unwrap();
    }
    let argvs: Vec<String> = world
        .requests()
        .iter()
        .filter(|request| request["transport"] != "http")
        .map(|request| request["gh_argv"].to_string())
        .collect();
    assert_eq!(argvs.len(), 2, "{argvs:?}");
    for argv in argvs {
        assert!(!argv.to_ascii_lowercase().contains("token"), "{argv}");
    }
}

fn write(world: &World, name: &str, text: &str) -> std::path::PathBuf {
    let path = world.directory.path().join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn hosts_json_never_holds_a_token_or_a_login() {
    let world = World::new(json!({"glab_auth": {"*": GLAB_TOKEN}}));
    let glab = world
        .server
        .install_glab(&world.directory.path().join("bin"));
    let config = write(
        &world,
        "glab-cli/config.yml",
        &format!(
            "hosts:\n    claimed.redact.test:\n        token: {GLAB_TOKEN}\n        user: maya\n"
        ),
    );
    let hosts = world.directory.path().join("state/hosts.json");
    let inputs = DetectInputs {
        config: ForgeConfig::default(),
        env: world.env(),
        options: ConnectOptions {
            glab: Some(glab),
            stores: Stores {
                glab_config: Some(config),
                ..Stores::default()
            },
            ..ConnectOptions::default()
        },
        hosts_file: Some(hosts.clone()),
        probe: true,
    };
    for host in ["claimed.redact.test", "status.redact.test"] {
        let found =
            detect::detect_with(&HostKey::new(host), &inputs, &AtomicBool::new(false)).unwrap();
        assert_eq!(found.kind, ProviderKind::GitLab, "{host}");
    }
    let text = std::fs::read_to_string(&hosts).unwrap();
    assert!(
        text.contains("claimed.redact.test") && text.contains("status.redact.test"),
        "{text}"
    );
    for secret in [GLAB_TOKEN, "maya", "rowan", "token"] {
        assert!(
            !text.contains(secret),
            "hosts.json holds {secret:?}: {text}"
        );
    }
}
