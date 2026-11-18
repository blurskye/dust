//! One fake forge per provider with a local repository whose remote points at it, for tests
//! that go through detection and `connect` like the app does (the IDE picker, `dusty-git
//! forge`). Include after `forge_fake`: `#[path = "support/forge_world.rs"] mod forge_world;`.
#![allow(dead_code)]

use super::forge_fake;
use super::forge_fake::FAKE_TOKEN;
use super::forge_fake::ForgeServer;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Forge {
    GitHub,
    GitLab,
    Gitea,
    Forgejo,
}

pub const ALL: [Forge; 4] = [Forge::GitHub, Forge::GitLab, Forge::Gitea, Forge::Forgejo];

impl Forge {
    pub fn world(self) -> &'static str {
        match self {
            Forge::GitHub => "legacy",
            Forge::GitLab => "gitlab",
            Forge::Gitea => "gitea",
            Forge::Forgejo => "forgejo",
        }
    }

    pub fn host(self) -> &'static str {
        match self {
            Forge::GitHub => "github.com",
            Forge::GitLab => "gitlab.ci.test",
            Forge::Gitea => "gitea.com",
            Forge::Forgejo => "code.forgejo.org",
        }
    }

    pub fn repo(self) -> &'static str {
        match self {
            Forge::GitHub => "example/demo",
            Forge::GitLab => "orbit/team/orbit",
            Forge::Gitea | Forge::Forgejo => "orbit/orbit",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Forge::GitHub => "GitHub",
            Forge::GitLab => "GitLab",
            Forge::Gitea => "Gitea",
            Forge::Forgejo => "Forgejo",
        }
    }

    /// A pull (merge) request, an issue, a run and one of its jobs the world answers.
    pub fn pull(self) -> u64 {
        match self {
            Forge::GitHub => 9,
            Forge::GitLab => 13,
            Forge::Gitea => 1402,
            Forge::Forgejo => 1784,
        }
    }

    pub fn issue(self) -> u64 {
        match self {
            Forge::GitHub => 7,
            Forge::GitLab => 14,
            Forge::Gitea => 1393,
            Forge::Forgejo => 1792,
        }
    }

    pub fn run(self) -> &'static str {
        match self {
            Forge::GitHub | Forge::GitLab | Forge::Gitea => "91",
            Forge::Forgejo => "200481",
        }
    }

    pub fn job(self) -> &'static str {
        match self {
            Forge::GitHub | Forge::GitLab | Forge::Gitea => "42",
            Forge::Forgejo => "424039",
        }
    }

    /// The version probe the host answers (GitHub needs none: github.com is well known).
    fn probes(self) -> Value {
        match self {
            Forge::GitHub => json!({}),
            Forge::GitLab => json!({ "gitlab.ci.test": { "api/v4/version": {
                "body": { "version": "17.5.0", "revision": "fixture" } } } }),
            Forge::Gitea => json!({ "gitea.com": { "api/v1/version": {
                "body": { "version": "29.0.0+dev-160-g55186ca39b" } } } }),
            Forge::Forgejo => json!({ "code.forgejo.org": { "api/v1/version": {
                "body": { "version": "17.0.0-26-363f3dc30b+gitea-1.22.0" } } } }),
        }
    }
}

pub fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub struct World {
    pub directory: tempfile::TempDir,
    pub server: ForgeServer,
    pub forge: Forge,
    /// A clone-like repository whose `origin` is `https://<host>/<repo>.git`.
    pub repo: PathBuf,
}

impl World {
    pub fn new(forge: Forge) -> Self {
        Self::with_state(forge, json!({}))
    }

    pub fn with_state(forge: Forge, mut state: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        state["probes"] = forge.probes();
        state["glab_auth"] = json!({ "*": "token" });
        std::fs::write(&path, state.to_string()).unwrap();
        let server = ForgeServer::start(&path, forge.world());
        let repo = directory.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                &format!("https://{}/{}.git", forge.host(), forge.repo()),
            ],
        );
        for home in ["home", "config", "cache"] {
            std::fs::create_dir_all(directory.path().join(home)).unwrap();
        }
        Self {
            directory,
            server,
            forge,
            repo,
        }
    }

    pub fn state_path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    pub fn state(&self) -> Value {
        forge_fake::state(&self.state_path())
    }

    pub fn update(&self, change: impl FnOnce(&mut Value)) {
        forge_fake::update(&self.state_path(), change);
    }

    pub fn requests(&self) -> Vec<Value> {
        forge_fake::requests(&self.state_path())
    }

    /// Requests that reached the API (not probes, not CLI sign-in checks).
    pub fn api_requests(&self) -> Vec<Value> {
        self.requests()
            .into_iter()
            .filter(|request| {
                let path = request["path"].as_str().unwrap_or("");
                !path.ends_with("version") && !path.starts_with("auth ")
            })
            .collect()
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.directory.path().join("cache")
    }

    /// Point `command` at this world: the fake server, a token for direct HTTP, no CLIs (so
    /// porcelain takes the fetch path), private HOME/XDG/config and a private disk cache.
    pub fn configure(&self, command: &mut Command) {
        self.server.configure(command);
        let home = self.directory.path().join("home");
        let missing = self.directory.path().join("no-cli");
        command
            .env("DUSTY_FORGE_TOKEN", FAKE_TOKEN)
            .env("DUSTY_FORGE_GH", &missing)
            .env("DUSTY_FORGE_GLAB", &missing)
            .env("DUSTY_FORGE_TEA", &missing)
            .env("DUSTY_FORGE_CACHE_DIR", self.cache_dir())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("DUSTY_CONFIG_HOME", self.directory.path().join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
    }
}
