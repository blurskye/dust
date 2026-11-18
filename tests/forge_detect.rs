#![cfg(unix)]
//! Forge discovery: config → `DUSTY_FORGE_PROVIDER` → known hosts →
//! `hosts.json` → CLI credential stores → unauthenticated version probes → "Unknown forge".
//! An unknown host is only claimed when a CLI holds a credential for it; probe results are kept
//! in `hosts.json` for 7 days and that file never holds a token. Everything runs against the
//! loopback fake forge (`DUSTY_FORGE_ORIGIN` points the probes at it).

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dusty::forge::credential::CredentialSource;
use dusty::forge::credential::Env;
use dusty::forge::credential::Stores;
use dusty::forge::detect;
use dusty::forge::detect::DetectInputs;
use dusty::forge::detect::DetectSource;
use dusty::forge::detect::Detected;
use dusty::forge::detect::ForgeConfig;
use dusty::forge::detect::HostsFile;
use dusty::forge::provider::ProviderKind;
use dusty::forge::request::ForgeError;
use dusty::forge::transport::Cli;
use dusty::forge::transport::ConnectOptions;
use dusty::forge::transport::HostKey;
use forge_fake::FAKE_TOKEN;
use forge_fake::ForgeServer;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

const GLAB_TOKEN: &str = "glpat-detectfixture0123456789";
const TEA_TOKEN: &str = "0123456789abcdef0123456789abcdef01234567";

struct World {
    directory: tempfile::TempDir,
    server: ForgeServer,
    gh: PathBuf,
    glab: PathBuf,
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
        Self {
            directory,
            server,
            gh,
            glab,
        }
    }

    fn state(&self) -> Value {
        forge_fake::state(&self.directory.path().join("state.json"))
    }

    fn hosts_file(&self) -> PathBuf {
        self.directory.path().join("state/hosts.json")
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.directory.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn options(&self, stores: Stores) -> ConnectOptions {
        ConnectOptions {
            gh: Some(self.gh.clone()),
            glab: Some(self.glab.clone()),
            stores,
            ..ConnectOptions::default()
        }
    }

    fn inputs(&self, config: &str, env: &[(&str, &str)], stores: Stores) -> DetectInputs {
        let mut pairs: Vec<(String, String)> =
            vec![("DUSTY_FORGE_ORIGIN".into(), self.server.origin())];
        pairs.extend(
            env.iter()
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        DetectInputs {
            config: ForgeConfig::parse(config),
            env: Env::from_pairs(pairs),
            options: self.options(stores),
            hosts_file: Some(self.hosts_file()),
            probe: true,
        }
    }

    fn detect(&self, host: &str, inputs: &DetectInputs) -> Result<Detected, ForgeError> {
        detect::detect_with(&HostKey::new(host), inputs, &AtomicBool::new(false))
    }

    fn probes(&self) -> u64 {
        self.state()["probe_count"].as_u64().unwrap_or(0)
    }

    fn requests_for(&self, host: &str) -> Vec<Value> {
        self.state()["requests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request["host"] == host)
            .collect()
    }
}

fn signed_out_gh() -> Value {
    json!({"gh_auth": {"*": "missing"}})
}

#[test]
fn config_beats_env_beats_known_hosts() {
    let world = World::new(signed_out_gh());
    let config = "[forge.hosts.\"git.corp.example:8443\"]\nprovider = \"gitlab\"\napi_base = \"https://git.corp.example:8443/gitlab/api/v4\"\n[forge.hosts.\"gitlab.com\"]\nprovider = \"github\"\n";
    let inputs = world.inputs(config, &[], Stores::default());
    let corp = world.detect("git.corp.example:8443", &inputs).unwrap();
    assert_eq!(
        (corp.kind, &corp.source),
        (ProviderKind::GitLab, &DetectSource::Config)
    );
    assert_eq!(
        corp.endpoints.api_base,
        "https://git.corp.example:8443/gitlab/api/v4"
    );
    assert_eq!(
        corp.endpoints.graphql.as_deref(),
        Some("https://git.corp.example:8443/gitlab/api/graphql")
    );
    assert_eq!(corp.endpoints.web_base, "https://git.corp.example:8443");
    assert_eq!(corp.cli, Some(Cli::Glab));
    // Config overrides even a well-known host.
    assert_eq!(
        world.detect("gitlab.com", &inputs).unwrap().kind,
        ProviderKind::GitHub
    );

    // DUSTY_FORGE_PROVIDER: any host when DUSTY_FORGE_HOST is unset, else only that one.
    let env = world.inputs(
        "",
        &[("DUSTY_FORGE_PROVIDER", "forgejo")],
        Stores::default(),
    );
    let any = world.detect("code.internal.test", &env).unwrap();
    assert_eq!(
        (any.kind, &any.source),
        (ProviderKind::Forgejo, &DetectSource::Env)
    );
    assert_eq!(any.endpoints.graphql, None);
    assert!(
        any.endpoints
            .api_base
            .ends_with("/code.internal.test/api/v1"),
        "{}",
        any.endpoints.api_base
    );
    let scoped = world.inputs(
        "",
        &[
            ("DUSTY_FORGE_PROVIDER", "gitea"),
            ("DUSTY_FORGE_HOST", "gitea.internal.test"),
        ],
        Stores::default(),
    );
    assert_eq!(
        world.detect("gitea.internal.test", &scoped).unwrap().kind,
        ProviderKind::Gitea
    );
    assert_eq!(
        world.detect("github.com", &scoped).unwrap().source,
        DetectSource::KnownHost
    );

    // Well-known hosts need no probe and no credential.
    let plain = world.inputs("", &[], Stores::default());
    for (host, kind) in [
        ("github.com", ProviderKind::GitHub),
        ("acme.ghe.com", ProviderKind::GitHub),
        ("gitlab.com", ProviderKind::GitLab),
        ("codeberg.org", ProviderKind::Forgejo),
        ("next.forgejo.org", ProviderKind::Forgejo),
        ("gitea.com", ProviderKind::Gitea),
    ] {
        let found = world.detect(host, &plain).unwrap();
        assert_eq!(
            (found.kind, &found.source),
            (kind, &DetectSource::KnownHost),
            "{host}"
        );
    }
    assert_eq!(
        world.probes(),
        0,
        "no probe for configured, env or known hosts"
    );
    assert!(
        !world.hosts_file().exists(),
        "nothing to cache for them either"
    );
}

#[test]
fn an_unknown_host_is_claimed_only_through_a_cli_credential() {
    let world = World::new(json!({"gh_auth": {"*": "missing", "ghe.corp.test": "token"}}));
    let glab_config = world.file(
        "config/glab-cli/config.yml",
        &format!("git_protocol: ssh\nhosts:\n    gitlab.corp.test:\n        token: {GLAB_TOKEN}\n        api_host: gitlab.corp.test\n        user: maya\n"),
    );
    let fj = world.file(
        "data/forgejo-cli/keys.json",
        &json!({"hosts": {"fj.corp.test": {"type": "Application", "name": "maya", "token": TEA_TOKEN}}}).to_string(),
    );
    let tea = world.file(
        "config/tea/config.yml",
        &format!("logins:\n  - name: work\n    url: https://tea.corp.test\n    token: {TEA_TOKEN}\n    default: true\n    user: maya\n"),
    );
    let stores = Stores {
        glab_config: Some(glab_config),
        fj_keys: vec![fj],
        tea_config: Some(tea),
    };
    let inputs = world.inputs("", &[], stores);

    let gitlab = world.detect("gitlab.corp.test", &inputs).unwrap();
    assert_eq!(gitlab.kind, ProviderKind::GitLab);
    assert_eq!(
        gitlab.source,
        DetectSource::Credential(CredentialSource::GlabConfig.to_string())
    );
    assert!(
        gitlab
            .endpoints
            .api_base
            .ends_with("/gitlab.corp.test/api/v4")
    );

    let forgejo = world.detect("fj.corp.test", &inputs).unwrap();
    assert_eq!(forgejo.kind, ProviderKind::Forgejo);
    assert_eq!(forgejo.source, DetectSource::Credential("fj keys".into()));

    // tea serves Gitea and Forgejo; with no version to read it stays Gitea, keeps the login.
    let gitea = world.detect("tea.corp.test", &inputs).unwrap();
    assert_eq!(gitea.kind, ProviderKind::Gitea);
    assert_eq!(gitea.login.as_deref(), Some("work"));
    assert_eq!(
        gitea.source,
        DetectSource::Credential("tea login \"work\"".into())
    );

    let ghes = world.detect("ghe.corp.test", &inputs).unwrap();
    assert_eq!(
        (ghes.kind, &ghes.source),
        (
            ProviderKind::GitHub,
            &DetectSource::Credential("gh token".into())
        )
    );

    // No CLI holds anything for this host and it answers no probe: not GitHub by default.
    let error = world.detect("nobody.corp.test", &inputs).unwrap_err();
    assert_eq!(
        error,
        ForgeError::Unsupported {
            capability: "provider",
            reason: "Unknown site nobody.corp.test · @ accounts".into(),
        }
    );
    assert!(
        error
            .to_string()
            .ends_with("Unknown site nobody.corp.test · @ accounts"),
        "{error}"
    );
    let probed: Vec<String> = world
        .requests_for("nobody.corp.test")
        .iter()
        .filter_map(|request| request["path"].as_str().map(str::to_owned))
        .collect();
    assert!(
        !probed.is_empty(),
        "the unknown host was probed: {}",
        world.state()["requests"]
    );
    assert!(
        world
            .requests_for("nobody.corp.test")
            .iter()
            .filter(|request| request["transport"] == "http")
            .all(|request| request["auth"] == "missing"),
        "probes never carry a credential"
    );
}

#[test]
fn glab_auth_status_claims_a_host_without_a_config_file() {
    let world = World::new(
        json!({"gh_auth": {"*": "missing"}, "glab_auth": {"gl.status.test": GLAB_TOKEN}}),
    );
    let inputs = world.inputs("", &[], Stores::default());
    let found = world.detect("gl.status.test", &inputs).unwrap();
    assert_eq!(found.kind, ProviderKind::GitLab);
    assert_eq!(
        found.source,
        DetectSource::Credential("glab auth status".into())
    );
}

#[test]
fn version_probes_tell_the_forges_apart() {
    let world = World::new(json!({
        "gh_auth": {"*": "missing"},
        "probes": {
            "forgejo.test": {"api/v1/version": {"body": {"version": "11.0.1+gitea-1.22.0"}}},
            "gitea.test": {"api/v1/version": {"body": {"version": "1.22.6"}}},
            "private-forgejo.test": {
                "api/v1/version": {"status": 403, "body": {"message": "token is required", "url": "https://private-forgejo.test/api/swagger"}},
                "api/forgejo/v1/version": {"status": 403, "body": {"message": "token is required"}}
            },
            "private-gitea.test": {
                "api/v1/version": {"status": 401, "body": {"message": "token is required", "url": "https://private-gitea.test/api/swagger"}}
            },
            "gitlab.test": {"api/v4/version": {"status": 401, "body": {"message": "401 Unauthorized"}}},
            "open-gitlab.test": {"api/v4/version": {"body": {"version": "17.5.1-ee", "revision": "abc"}}},
            "ghes.test": {"api/v3/meta": {"body": {"installed_version": "3.14.2", "verifiable_password_authentication": false}}}
        }
    }));
    let inputs = world.inputs("", &[], Stores::default());
    for (host, kind, version) in [
        (
            "forgejo.test",
            ProviderKind::Forgejo,
            Some("11.0.1+gitea-1.22.0"),
        ),
        ("gitea.test", ProviderKind::Gitea, Some("1.22.6")),
        ("private-forgejo.test", ProviderKind::Forgejo, None),
        ("private-gitea.test", ProviderKind::Gitea, None),
        ("gitlab.test", ProviderKind::GitLab, None),
        ("open-gitlab.test", ProviderKind::GitLab, Some("17.5.1-ee")),
        ("ghes.test", ProviderKind::GitHub, Some("3.14.2")),
    ] {
        let found = world.detect(host, &inputs).unwrap();
        assert_eq!(
            (found.kind, found.server_version.as_deref(), &found.source),
            (kind, version, &DetectSource::Probe),
            "{host}"
        );
    }
    assert_eq!(detect::PROBE_TIMEOUT.as_secs(), 3);
    // A Gitea probe never falls through to the GitLab path.
    let paths: Vec<String> = world
        .requests_for("gitea.test")
        .iter()
        .filter(|request| request["transport"] == "http")
        .filter_map(|request| request["path"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(paths, ["version"], "{paths:?}");
}

#[test]
fn hosts_json_caches_probes_for_seven_days_without_secrets() {
    let world = World::new(json!({
        "gh_auth": {"*": "missing"},
        "probes": {"cached.test": {"api/v1/version": {"body": {"version": "10.0.0+gitea-1.22.0"}}}}
    }));
    let inputs = world.inputs(
        "",
        &[("FORGEJO_TOKEN", TEA_TOKEN), ("GH_TOKEN", FAKE_TOKEN)],
        Stores::default(),
    );
    let first = world.detect("cached.test", &inputs).unwrap();
    assert_eq!(first.source, DetectSource::Probe);
    let probes = world.probes();
    let second = world.detect("cached.test", &inputs).unwrap();
    assert_eq!(
        (second.kind, &second.source),
        (ProviderKind::Forgejo, &DetectSource::Cache)
    );
    assert_eq!(
        second.server_version.as_deref(),
        Some("10.0.0+gitea-1.22.0")
    );
    assert_eq!(world.probes(), probes, "the cache answered without probing");

    let text = std::fs::read_to_string(world.hosts_file()).unwrap();
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["schema"], 1);
    let record = &file["hosts"]["cached.test"];
    assert_eq!(record["kind"], "forgejo");
    assert!(
        record["api_base"]
            .as_str()
            .unwrap()
            .ends_with("/cached.test/api/v1")
    );
    assert_eq!(record["web_base"], "https://cached.test");
    assert_eq!(record["server_version"], "10.0.0+gitea-1.22.0");
    assert!(record["probed_at"].as_u64().unwrap() > 0);
    assert!(record["capabilities_observed"].is_object());
    for secret in [TEA_TOKEN, FAKE_TOKEN, "rowan"] {
        assert!(!text.contains(secret), "hosts.json holds {secret:?}");
    }

    // Older than 7 days: probed again.
    let mut stale = HostsFile::load(&world.hosts_file());
    let eight_days = 8 * 24 * 60 * 60;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let record = stale.hosts.get_mut("cached.test").unwrap();
    record.probed_at = now - eight_days;
    record
        .capabilities_observed
        .insert("actions".into(), "no".into());
    stale.save(&world.hosts_file()).unwrap();
    assert!(stale.fresh(&HostKey::new("cached.test"), now).is_none());
    let again = world.detect("cached.test", &inputs).unwrap();
    assert_eq!(again.source, DetectSource::Probe);
    assert!(world.probes() > probes);
    let reprobed = HostsFile::load(&world.hosts_file());
    assert!(reprobed.fresh(&HostKey::new("cached.test"), now).is_some());
    assert_eq!(
        reprobed.hosts["cached.test"].capabilities_observed["actions"], "no",
        "a re-probe keeps what capability probes learned"
    );

    // A newer schema or garbage reads as empty instead of failing.
    std::fs::write(world.hosts_file(), "{\"schema\": 2, \"hosts\": {}}").unwrap();
    assert!(HostsFile::load(&world.hosts_file()).hosts.is_empty());
    std::fs::write(world.hosts_file(), "not json").unwrap();
    assert!(HostsFile::load(&world.hosts_file()).hosts.is_empty());
}

#[test]
fn offline_detection_never_probes() {
    let world = World::new(json!({
        "gh_auth": {"*": "missing"},
        "probes": {"offline.test": {"api/v1/version": {"body": {"version": "1.22.0"}}}}
    }));
    let mut inputs = world.inputs("", &[], Stores::default());
    inputs.probe = false;
    assert!(matches!(
        world.detect("offline.test", &inputs),
        Err(ForgeError::Unsupported {
            capability: "provider",
            ..
        })
    ));
    assert_eq!(world.probes(), 0);
}

#[test]
fn remotes_pick_the_forge_host() {
    let directory = tempfile::tempdir().unwrap();
    let repository = git2::Repository::init(directory.path()).unwrap();
    repository
        .remote("origin", "git@gitlab.com:group/sub/orbit.git")
        .unwrap();
    repository
        .remote("fork", "https://codeberg.org/maya/orbit.git")
        .unwrap();
    let chosen = detect::choose_remote_with(directory.path(), None).unwrap();
    assert_eq!(
        (chosen.host.to_string(), chosen.repo.full()),
        ("gitlab.com".into(), "group/sub/orbit".into())
    );
    let preferred = detect::choose_remote_with(directory.path(), Some("fork")).unwrap();
    assert_eq!(preferred.host.to_string(), "codeberg.org");
    assert_eq!(
        ForgeConfig::parse("[forge]\nremote = \"fork\"\n")
            .remote
            .as_deref(),
        Some("fork")
    );
}

/// F5: a nested GitLab group keeps every segment over SSH, `ssh://` with a Git port (dropped:
/// it is not the web port) and HTTPS with a port (kept), and travels as one encoded project id.
#[test]
fn nested_gitlab_groups_survive_every_remote_form() {
    use dusty::forge::provider;
    use dusty::forge::provider::Conditional;
    use dusty::forge::provider::ForgeContext;
    use dusty::forge::request::Read;
    for (remote, host) in [
        (
            "git@gitlab.example.com:group/sub/project.git",
            "gitlab.example.com",
        ),
        (
            "ssh://git@gitlab.example.com:2222/group/sub/project.git",
            "gitlab.example.com",
        ),
        (
            "https://gitlab.example.com:8443/group/sub/project.git",
            "gitlab.example.com:8443",
        ),
        (
            "https://gitlab.example.com/group/sub/project",
            "gitlab.example.com",
        ),
    ] {
        let (parsed_host, repo) =
            detect::parse_remote(remote).unwrap_or_else(|| panic!("{remote}"));
        assert_eq!(parsed_host.to_string(), host, "{remote}");
        assert_eq!(
            (repo.owner.as_str(), repo.name.as_str()),
            ("group/sub", "project"),
            "{remote}"
        );
        let endpoints = detect::endpoints_for(
            ProviderKind::GitLab,
            &parsed_host,
            None,
            &Env::from_pairs::<&str, &str>([]),
        );
        assert_eq!(endpoints.api_base, format!("https://{host}/api/v4"));
        assert_eq!(
            endpoints.graphql.as_deref(),
            Some(format!("https://{host}/api/graphql").as_str())
        );
        let ctx = ForgeContext::offline(endpoints, Some(repo));
        let plan = provider::build(ProviderKind::GitLab).unwrap().plan(
            &ctx,
            &Read::PullRequest(7),
            &Conditional::default(),
        );
        assert_eq!(
            plan.requests[0].path_and_query(),
            "projects/group%2Fsub%2Fproject/merge_requests/7"
        );
    }
}
