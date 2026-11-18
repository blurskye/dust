#![cfg(unix)]
//! Credentials for every CLI: Dusty's own variable, then each forge's
//! variables (host-scoped where the CLI scopes them), then the CLIs' stores — glab's
//! `config.yml` and `glab auth status --show-token`, fj's `keys.json`, tea's `config.yml` (read
//! with the strict YAML subset) — then the CLI passthrough. A token is validated with a live
//! `GET /user` before it is used, and a rejected one falls back to the CLI.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dusty::forge::credential;
use dusty::forge::credential::CredentialCache;
use dusty::forge::credential::CredentialSource;
use dusty::forge::credential::CredentialState;
use dusty::forge::credential::Env;
use dusty::forge::credential::Stores;
use dusty::forge::detect;
use dusty::forge::host_state;
use dusty::forge::host_state::HostState;
use dusty::forge::provider::ProviderKind;
use dusty::forge::request::ForgeError;
use dusty::forge::transport;
use dusty::forge::transport::ApiRequest;
use dusty::forge::transport::Cli;
use dusty::forge::transport::ConnectOptions;
use dusty::forge::transport::Endpoints;
use dusty::forge::transport::Force;
use dusty::forge::transport::HostKey;
use dusty::forge::transport::TransportKind;
use forge_fake::FAKE_TOKEN;
use forge_fake::ForgeServer;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;

const GLAB_TOKEN: &str = "glpat-credfixture0123456789ab";
const GITEA_TOKEN: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f901234567";
const OTHER_TOKEN: &str = "f0e1d2c3b4a5968778695a4b3c2d1e0f98765432";

struct World {
    directory: tempfile::TempDir,
    server: ForgeServer,
    gh: PathBuf,
    glab: PathBuf,
    tea: PathBuf,
}

impl World {
    fn new(state: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        std::fs::write(&path, state.to_string()).unwrap();
        let server = ForgeServer::start(&path, "legacy");
        let bin = directory.path().join("bin");
        let gh = server.install_gh(&bin);
        let glab = server.install_glab(&bin);
        let tea = server.install_tea(
            &bin,
            json!([{"name": "work", "url": "https://tea.cred.test", "user": "maya", "default": true}]),
        );
        Self {
            directory,
            server,
            gh,
            glab,
            tea,
        }
    }

    fn state(&self) -> Value {
        forge_fake::state(&self.directory.path().join("state.json"))
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.directory.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn env(&self, pairs: &[(&str, &str)]) -> Env {
        let mut all: Vec<(String, String)> =
            vec![("DUSTY_FORGE_ORIGIN".into(), self.server.origin())];
        all.extend(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        Env::from_pairs(all)
    }

    fn endpoints(&self, kind: ProviderKind, host: &str) -> Endpoints {
        let env = self.env(&[("DUSTY_FORGE_API_BASE", &self.server.api_base())]);
        detect::endpoints_for(kind, &HostKey::new(host), None, &env)
    }

    fn options(&self, stores: Stores) -> ConnectOptions {
        ConnectOptions {
            gh: Some(self.gh.clone()),
            glab: Some(self.glab.clone()),
            tea: Some(self.tea.clone()),
            stores,
            // These tests check the signed-out states; anonymous reads have their own
            // (tests/forge_anonymous.rs).
            no_anonymous: true,
            ..ConnectOptions::default()
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.state()["requests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

fn lookup(endpoints: &Endpoints, options: &ConnectOptions, env: &Env) -> CredentialState {
    credential::lookup(endpoints, options, env, &AtomicBool::new(false))
}

fn source(state: &CredentialState) -> Option<CredentialSource> {
    match state {
        CredentialState::Present(credential) => Some(credential.source.clone()),
        _ => None,
    }
}

fn token(state: &CredentialState) -> Option<String> {
    match state {
        CredentialState::Present(credential) => Some(credential.token.expose().to_owned()),
        _ => None,
    }
}

#[test]
fn github_precedence_is_dusty_then_gh_variables_then_gh() {
    let world = World::new(json!({}));
    let options = world.options(Stores::default());
    let dotcom = world.endpoints(ProviderKind::GitHub, "github.com");
    let ghes = world.endpoints(ProviderKind::GitHub, "ghe.cred.test");
    let all = [
        ("DUSTY_FORGE_TOKEN", "ghp_dustyfixture0123456789abcdef"),
        ("GH_TOKEN", "ghp_ghtokenfixture0123456789abcd"),
        ("GH_ENTERPRISE_TOKEN", "ghp_enterprisefixture0123456789a"),
    ];
    let state = lookup(&dotcom, &options, &world.env(&all));
    assert_eq!(
        source(&state),
        Some(CredentialSource::Env("DUSTY_FORGE_TOKEN"))
    );
    // Scoped to another host, Dusty's variable steps aside.
    let scoped = [
        all[0],
        all[1],
        all[2],
        ("DUSTY_FORGE_HOST", "elsewhere.test"),
    ];
    assert_eq!(
        source(&lookup(&dotcom, &options, &world.env(&scoped))),
        Some(CredentialSource::Env("GH_TOKEN"))
    );
    assert_eq!(
        source(&lookup(&ghes, &options, &world.env(&scoped))),
        Some(CredentialSource::Env("GH_ENTERPRISE_TOKEN")),
        "GHES hosts read the enterprise variables, never GH_TOKEN"
    );
    let state = lookup(
        &ghes,
        &options,
        &world.env(&[("GH_TOKEN", "ghp_ghtokenfixture0123456789abcd")]),
    );
    assert_eq!(source(&state), Some(CredentialSource::GhAuthToken));
    assert_eq!(token(&state).as_deref(), Some(FAKE_TOKEN));
}

#[test]
fn gitlab_reads_its_variable_then_glab_config_then_glab_status() {
    let world = World::new(json!({"glab_auth": {"status.cred.test": GLAB_TOKEN}}));
    let config = world.file(
        "glab-cli/config.yml",
        &format!(
            "# glab config\ngit_protocol: ssh\nhosts:\n    gitlab.com:\n        token: \"\"\n    gl.cred.test:\n        token: {GLAB_TOKEN}\n        api_protocol: https\n        user: maya\n    alias.cred.test:\n        api_host: gl-api.cred.test\n        token: {GLAB_TOKEN}\n"
        ),
    );
    let options = world.options(Stores {
        glab_config: Some(config),
        ..Stores::default()
    });
    let gl = world.endpoints(ProviderKind::GitLab, "gl.cred.test");
    let state = lookup(&gl, &options, &world.env(&[]));
    assert_eq!(source(&state), Some(CredentialSource::GlabConfig));
    assert_eq!(token(&state).as_deref(), Some(GLAB_TOKEN));
    // GITLAB_TOKEN wins, but only for GITLAB_HOST when that is set.
    let env = world.env(&[
        ("GITLAB_TOKEN", "glpat-envfixture0123456789abcd"),
        ("GITLAB_HOST", "https://gl.cred.test"),
    ]);
    assert_eq!(
        source(&lookup(&gl, &options, &env)),
        Some(CredentialSource::Env("GITLAB_TOKEN"))
    );
    let other = world.endpoints(ProviderKind::GitLab, "other.cred.test");
    assert!(matches!(
        lookup(&other, &options, &env),
        CredentialState::Missing { ref hint, .. } if hint == "glab auth login --hostname other.cred.test"
    ));
    // The config entry also answers for its api_host.
    let api = world.endpoints(ProviderKind::GitLab, "gl-api.cred.test");
    assert_eq!(
        source(&lookup(&api, &options, &world.env(&[]))),
        Some(CredentialSource::GlabConfig)
    );
    // No config entry: `glab auth status --show-token`.
    let status = world.endpoints(ProviderKind::GitLab, "status.cred.test");
    let state = lookup(&status, &options, &world.env(&[]));
    assert_eq!(source(&state), Some(CredentialSource::GlabAuthStatus));
    assert_eq!(token(&state).as_deref(), Some(GLAB_TOKEN));
    match &state {
        CredentialState::Present(credential) => {
            assert_eq!(credential.user.as_deref(), Some("rowan"))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn gitea_and_forgejo_read_variables_fj_keys_and_tea_config() {
    let world = World::new(json!({}));
    let fj = world.file(
        "forgejo-cli/keys.json",
        &json!({"hosts": {"fj.cred.test": {"type": "Application", "name": "maya", "token": GITEA_TOKEN}}}).to_string(),
    );
    let tea = world.file(
        "tea/config.yml",
        &format!(
            "logins:\n  - name: old\n    url: https://tea.cred.test\n    token: {OTHER_TOKEN}\n    default: false\n  - name: work\n    url: https://tea.cred.test\n    token: {GITEA_TOKEN}\n    default: true\n    user: maya\npreferences:\n  editor: false\n"
        ),
    );
    let options = world.options(Stores {
        fj_keys: vec![fj],
        tea_config: Some(tea),
        ..Stores::default()
    });
    let fj_host = world.endpoints(ProviderKind::Forgejo, "fj.cred.test");
    let state = lookup(&fj_host, &options, &world.env(&[]));
    assert_eq!(source(&state), Some(CredentialSource::FjKeys));
    match &state {
        CredentialState::Present(credential) => {
            assert_eq!(credential.user.as_deref(), Some("maya"))
        }
        other => panic!("{other:?}"),
    }
    // tea: the default login of several for the same host.
    let tea_host = world.endpoints(ProviderKind::Gitea, "tea.cred.test");
    let state = lookup(&tea_host, &options, &world.env(&[]));
    assert_eq!(
        source(&state),
        Some(CredentialSource::TeaConfig("work".into()))
    );
    assert_eq!(token(&state).as_deref(), Some(GITEA_TOKEN));
    assert_eq!(
        CredentialSource::TeaConfig("work".into()).to_string(),
        "tea login \"work\""
    );
    // GITEA_SERVER_TOKEN needs GITEA_SERVER_URL naming the host.
    let unscoped = world.env(&[("GITEA_SERVER_TOKEN", OTHER_TOKEN)]);
    assert_eq!(
        source(&lookup(&tea_host, &options, &unscoped)),
        Some(CredentialSource::TeaConfig("work".into()))
    );
    let scoped = world.env(&[
        ("GITEA_SERVER_TOKEN", OTHER_TOKEN),
        ("GITEA_SERVER_URL", "https://tea.cred.test/"),
    ]);
    assert_eq!(
        source(&lookup(&tea_host, &options, &scoped)),
        Some(CredentialSource::Env("GITEA_SERVER_TOKEN"))
    );
    // FORGEJO_TOKEN applies to any Gitea-family host unless a URL scopes it.
    let forgejo = world.env(&[("FORGEJO_TOKEN", OTHER_TOKEN)]);
    assert_eq!(
        source(&lookup(&fj_host, &options, &forgejo)),
        Some(CredentialSource::Env("FORGEJO_TOKEN"))
    );
    let elsewhere = world.env(&[
        ("FORGEJO_TOKEN", OTHER_TOKEN),
        ("FORGEJO_URL", "https://elsewhere.test"),
    ]);
    assert_eq!(
        source(&lookup(&fj_host, &options, &elsewhere)),
        Some(CredentialSource::FjKeys)
    );
    // Nothing anywhere: the hint names tea.
    let none = world.endpoints(ProviderKind::Forgejo, "none.cred.test");
    assert!(matches!(
        lookup(&none, &options, &world.env(&[])),
        CredentialState::Missing { ref hint, .. } if hint == "tea login add --url https://none.cred.test"
    ));
}

#[test]
fn surprising_yaml_skips_the_store_instead_of_guessing() {
    let world = World::new(json!({}));
    for (name, text) in [
        (
            "anchor",
            format!(
                "logins:\n  - &a\n    name: work\n    url: https://tea.cred.test\n    token: {GITEA_TOKEN}\n"
            ),
        ),
        (
            "tab",
            format!(
                "logins:\n\t- name: work\n\t  url: https://tea.cred.test\n\t  token: {GITEA_TOKEN}\n"
            ),
        ),
        (
            "block",
            format!(
                "logins:\n  - name: work\n    url: https://tea.cred.test\n    token: |\n      {GITEA_TOKEN}\n"
            ),
        ),
        (
            "duplicate",
            format!(
                "logins:\n  - name: work\n    name: other\n    url: https://tea.cred.test\n    token: {GITEA_TOKEN}\n"
            ),
        ),
        (
            "flow",
            format!("logins: [{{name: work, url: https://tea.cred.test, token: {GITEA_TOKEN}}}]\n"),
        ),
    ] {
        let tea = world.file(&format!("{name}/config.yml"), &text);
        let options = world.options(Stores {
            tea_config: Some(tea.clone()),
            ..Stores::default()
        });
        let endpoints = world.endpoints(ProviderKind::Gitea, "tea.cred.test");
        assert!(
            matches!(
                lookup(&endpoints, &options, &world.env(&[])),
                CredentialState::Missing { .. }
            ),
            "{name}"
        );
        assert!(credential::tea_logins(&tea).is_empty(), "{name}");
    }
}

#[test]
fn a_token_is_validated_live_before_use_and_a_rejected_one_falls_back() {
    let world = World::new(json!({"tokens": [GLAB_TOKEN], "viewer": "maya"}));
    let config = world.file(
        "glab-cli/config.yml",
        &format!("hosts:\n    live.cred.test:\n        token: {GLAB_TOKEN}\n    stale.cred.test:\n        token: glpat-revokedfixture0123456789\n"),
    );
    let options = world.options(Stores {
        glab_config: Some(config),
        ..Stores::default()
    });
    let live = world.endpoints(ProviderKind::GitLab, "live.cred.test");
    let chosen = transport::for_host(&live, &options, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        chosen.kind(),
        TransportKind::Http {
            credential_source: CredentialSource::GlabConfig
        }
    );
    let user_check = world
        .requests()
        .into_iter()
        .find(|request| request["host"] == "live.cred.test")
        .unwrap();
    assert_eq!(
        (user_check["path"].as_str(), user_check["auth"].as_str()),
        (Some("user"), Some("valid"))
    );
    let account = host_state::account(&HostKey::new("live.cred.test")).unwrap();
    assert_eq!(account.login.as_deref(), Some("maya"));
    assert!(matches!(account.state, HostState::Ready { .. }));

    // A token the server refuses is not used: glab passthrough instead.
    let stale = world.endpoints(ProviderKind::GitLab, "stale.cred.test");
    let chosen = transport::for_host(&stale, &options, &AtomicBool::new(false)).unwrap();
    assert_eq!(chosen.kind(), TransportKind::Cli(Cli::Glab));
    let response = chosen
        .send(
            &HostKey::new("stale.cred.test"),
            &ApiRequest::get("user"),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(response.status, 200);
    let last = world.requests().pop().unwrap();
    assert_eq!(
        (last["transport"].as_str(), last["path"].as_str()),
        (Some("glab"), Some("user"))
    );
}

#[test]
fn tea_passthrough_uses_the_login_and_missing_or_old_clis_are_states() {
    let world = World::new(json!({}));
    // A login without a token: tea itself is signed in (keyring, OAuth), Dusty is not.
    let tea_config = world.file(
        "tea/config.yml",
        "logins:\n  - name: work\n    url: https://tea.cred.test\n    default: true\n",
    );
    let options = world.options(Stores {
        tea_config: Some(tea_config),
        ..Stores::default()
    });
    let endpoints = world.endpoints(ProviderKind::Forgejo, "tea.cred.test");
    let chosen = transport::for_host(&endpoints, &options, &AtomicBool::new(false)).unwrap();
    assert_eq!(chosen.kind(), TransportKind::Cli(Cli::Tea));
    let response = chosen
        .send(
            &HostKey::new("tea.cred.test"),
            &ApiRequest::get("user"),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(response.status, 200);
    let last = world.requests().pop().unwrap();
    assert_eq!(last["transport"], "tea");
    let argv: Vec<String> = serde_json::from_value(last["gh_argv"].clone()).unwrap();
    assert!(
        argv.windows(2).any(|pair| pair == ["--login", "work"]),
        "{argv:?}"
    );

    // No tea login for the host: signed out, with tea's command.
    let unknown = world.endpoints(ProviderKind::Gitea, "nologin.cred.test");
    let error = transport::for_host(&unknown, &options, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert_eq!(
        error,
        ForgeError::NotAuthenticated {
            host: "nologin.cred.test".into(),
            hint: "tea login add --url https://nologin.cred.test".into()
        }
    );
    // No glab at all.
    let gitlab = world.endpoints(ProviderKind::GitLab, "noglab.cred.test");
    let without = ConnectOptions {
        glab: None,
        ..world.options(Stores::default())
    };
    assert_eq!(
        transport::for_host(&gitlab, &without, &AtomicBool::new(false)).err(),
        Some(ForgeError::CliMissing(Cli::Glab))
    );
    // A glab older than 1.40.
    let old = world.directory.path().join("old/glab");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    forge_fake::write_executable(
        &old,
        format!(
            "#!/bin/sh\nFAKE_GLAB_VERSION=1.20.0 exec {} \"$@\"\n",
            world.glab.display()
        )
        .as_bytes(),
    );
    let outdated = ConnectOptions {
        glab: Some(old),
        ..world.options(Stores::default())
    };
    let old_host = world.endpoints(ProviderKind::GitLab, "oldglab.cred.test");
    let error = transport::for_host(&old_host, &outdated, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(
        matches!(&error, ForgeError::CliTooOld { cli: Cli::Glab, found, needed } if found == "1.20.0" && *needed == "1.40"),
        "{error:?}"
    );
    let state = HostState::from_error(Some(ProviderKind::GitLab), &error).unwrap();
    assert!(matches!(state, HostState::CliTooOld { .. }), "{state:?}");
}

#[test]
fn forced_http_without_a_credential_names_the_right_login_command() {
    let world = World::new(json!({"gh_auth": {"*": "missing"}}));
    let forced = ConnectOptions {
        force: Some(Force::Http),
        ..world.options(Stores::default())
    };
    for (kind, host, hint) in [
        (
            ProviderKind::GitHub,
            "ghe.forced.test",
            "gh auth login --hostname ghe.forced.test",
        ),
        (
            ProviderKind::GitLab,
            "gl.forced.test",
            "glab auth login --hostname gl.forced.test",
        ),
        (
            ProviderKind::Gitea,
            "tea.forced.test",
            "tea login add --url https://tea.forced.test",
        ),
    ] {
        let error = transport::for_host(
            &world.endpoints(kind, host),
            &forced,
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
        assert_eq!(
            error,
            ForgeError::NotAuthenticated {
                host: host.into(),
                hint: hint.into()
            },
            "{kind:?}"
        );
    }
}

#[test]
fn resolutions_are_cached_five_minutes_misses_ten_seconds_crashes_never() {
    let world = World::new(json!({}));
    let options = world.options(Stores::default());
    let endpoints = world.endpoints(ProviderKind::GitHub, "cache.cred.test");
    let cache = CredentialCache::new();
    let start = Instant::now();
    let present = credential::lookup(
        &endpoints,
        &options,
        &world.env(&[]),
        &AtomicBool::new(false),
    );
    assert!(matches!(present, CredentialState::Present(_)));
    cache.put(&endpoints, &options, &present, start);
    assert!(
        cache
            .get(&endpoints, &options, start + Duration::from_secs(299))
            .is_some()
    );
    assert!(
        cache
            .get(&endpoints, &options, start + Duration::from_secs(301))
            .is_none()
    );
    let missing = CredentialState::Missing {
        tried: vec![],
        hint: String::new(),
    };
    cache.put(&endpoints, &options, &missing, start);
    assert!(
        cache
            .get(&endpoints, &options, start + Duration::from_secs(9))
            .is_some()
    );
    assert!(
        cache
            .get(&endpoints, &options, start + Duration::from_secs(11))
            .is_none()
    );
    let crashes = CredentialCache::new();
    crashes.put(
        &endpoints,
        &options,
        &CredentialState::Crashed("boom".into()),
        start,
    );
    assert!(crashes.get(&endpoints, &options, start).is_none());
    assert_eq!(credential::PRESENT_TTL, Duration::from_secs(300));
    assert_eq!(credential::MISSING_TTL, Duration::from_secs(10));
}
