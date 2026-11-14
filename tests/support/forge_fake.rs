//! The fake forge for integration tests (FORGE.md §8.2, F1): `tests/fixtures/forge/server.py`
//! serves REST v3 + GraphQL on loopback, and `tests/fixtures/forge/fake_gh.py` installed as `gh`
//! forwards `gh api` passthrough to it. Include with
//! `#[path = "support/forge_fake.rs"] mod forge_fake;`.
#![allow(dead_code)]

use serde_json::Value;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;

/// The only token the fake server accepts over direct HTTP; `gh auth token` prints it.
pub const FAKE_TOKEN: &str = "ghp_fixture0123456789abcdefABCDEF0123";

/// Environment that would let a test reach a real forge or skip the fake `gh`.
pub const AMBIENT_ENV: [&str; 25] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "DUSTY_FORGE_GRAPHQL_URL",
    "DUSTY_FORGE_TRANSPORT",
    "DUSTY_FORGE_GH",
    "DUSTY_FORGE_OFFLINE",
    "GH_REPO",
    // F2: the other CLIs' and Dusty's own credential and routing variables.
    "GH_HOST",
    "GITLAB_TOKEN",
    "GITLAB_HOST",
    "GL_HOST",
    "GITEA_SERVER_URL",
    "GITEA_SERVER_TOKEN",
    "GITEA_TOKEN",
    "FORGEJO_TOKEN",
    "FORGEJO_URL",
    "DUSTY_FORGE_TOKEN",
    "DUSTY_FORGE_HOST",
    "DUSTY_FORGE_PROVIDER",
    "DUSTY_FORGE_REMOTE",
    "DUSTY_FORGE_GLAB",
    "DUSTY_FORGE_TEA",
    // F7: anonymous reads off.
    "DUSTY_FORGE_ANONYMOUS",
];

pub struct ForgeServer {
    child: Child,
    pub port: u16,
    pub state: PathBuf,
}

impl ForgeServer {
    /// `world` is `legacy` (tests/fixtures/github_cli.py's world), `inbox` or `recorded`.
    pub fn start(state: &Path, world: &str) -> Self {
        Self::start_with(state, world, None)
    }

    pub fn start_with(state: &Path, world: &str, fixtures: Option<&Path>) -> Self {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forge/server.py");
        let mut command = Command::new("python3");
        command
            .arg(script)
            .arg("--state")
            .arg(state)
            .arg("--world")
            .arg(world);
        if let Some(fixtures) = fixtures {
            command.arg("--fixtures").arg(fixtures);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("python3 runs the fake forge server");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("fake forge server printed no port: {line:?}"));
        Self {
            child,
            port,
            state: state.to_path_buf(),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// `DUSTY_FORGE_API_BASE`: the logical host is the first path segment.
    pub fn api_base(&self) -> String {
        format!("{}/{{host}}/api/v3", self.url())
    }

    /// The fake `gh` as `<bin>/gh`, finding this server through a sidecar file.
    pub fn install_gh(&self, bin: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(&gh, include_bytes!("../fixtures/forge/fake_gh.py")).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            bin.join("fake_gh.json"),
            serde_json::json!({ "server": self.url() }).to_string(),
        )
        .unwrap();
        gh
    }

    /// `DUSTY_FORGE_ORIGIN`: where version probes and GitLab/Gitea API bases go.
    pub fn origin(&self) -> String {
        format!("{}/{{host}}", self.url())
    }

    /// `DUSTY_FORGE_STATE_DIR`: `hosts.json` next to the state file, never the user's.
    pub fn state_dir(&self) -> PathBuf {
        self.state.with_extension("forge-state")
    }

    /// The fake `glab` as `<bin>/glab` (signed in per the server's `glab_auth` world).
    pub fn install_glab(&self, bin: &Path) -> PathBuf {
        self.install(
            bin,
            "glab",
            include_bytes!("../fixtures/forge/fake_glab.py"),
            serde_json::json!({}),
        )
    }

    /// The fake `tea` as `<bin>/tea`, knowing `logins` (`[{name, url, user, default}]`).
    pub fn install_tea(&self, bin: &Path, logins: Value) -> PathBuf {
        self.install(
            bin,
            "tea",
            include_bytes!("../fixtures/forge/fake_tea.py"),
            serde_json::json!({ "logins": logins }),
        )
    }

    fn install(&self, bin: &Path, name: &str, script: &[u8], mut sidecar: Value) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(bin).unwrap();
        let path = bin.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        sidecar["server"] = Value::String(self.url());
        std::fs::write(bin.join(format!("fake_{name}.json")), sidecar.to_string()).unwrap();
        path
    }

    /// Point a child process at this server and away from ambient credentials.
    pub fn configure<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        for name in AMBIENT_ENV {
            command.env_remove(name);
        }
        command
            .env("DUSTY_FORGE_API_BASE", self.api_base())
            .env("DUSTY_FORGE_ORIGIN", self.origin())
            .env("DUSTY_FORGE_STATE_DIR", self.state_dir())
    }
}

impl Drop for ForgeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn state(path: &Path) -> Value {
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(value) = serde_json::from_str(&text)
        {
            return value;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("fake forge state {} is unreadable", path.display());
}

/// Every request the server saw, in order (`{transport, method, host, path, operation, call, ...}`).
pub fn requests(path: &Path) -> Vec<Value> {
    state(path)["requests"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The gh-CLI form of each logical request (what `github_cli.py` logged as `calls`).
pub fn calls(path: &Path) -> Vec<Vec<String>> {
    requests(path)
        .into_iter()
        .filter_map(|request| serde_json::from_value(request["call"].clone()).ok())
        .collect()
}

/// One logical list load spans a first page and its `--after` continuations; its limit is the
/// last page's. `matches` picks the list calls of interest.
pub fn list_load_limits(calls: &[Vec<String>], matches: impl Fn(&[String]) -> bool) -> Vec<usize> {
    let mut limits: Vec<usize> = Vec::new();
    for call in calls.iter().filter(|call| matches(call)) {
        let Some(limit) = call
            .iter()
            .position(|arg| arg == "--limit")
            .and_then(|at| call.get(at + 1))
            .and_then(|limit| limit.parse().ok())
        else {
            continue;
        };
        if call.iter().any(|arg| arg == "--after") && !limits.is_empty() {
            *limits.last_mut().unwrap() = limit;
        } else {
            limits.push(limit);
        }
    }
    limits
}
