//! CLI passthrough (FORGE.md §1.2): the same `ApiRequest`s sent through `gh api --include`
//! (and, from F4/F5, `glab api` / `tea api`) when no token can be read. The CLI attaches its
//! own credential, so Dusty never sees or passes a token here. Also the shared child-process
//! runner, error classification and `redact`.

use crate::forge::request::ForgeError;
use crate::forge::transport::ApiRequest;
use crate::forge::transport::ApiResponse;
use crate::forge::transport::CLI_BOUND;
use crate::forge::transport::Cli;
use crate::forge::transport::HostKey;
use crate::forge::transport::Target;
use crate::forge::transport::Transport;
use crate::forge::transport::TransportError;
use crate::forge::transport::TransportKind;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

pub struct CliTransport {
    cli: Cli,
    executable: PathBuf,
    /// tea login / glab alias for passthrough (`[forge.hosts.H].login`, F2).
    login: Option<String>,
}

impl CliTransport {
    pub fn new(cli: Cli, executable: PathBuf, login: Option<String>) -> Self {
        Self {
            cli,
            executable,
            login,
        }
    }
}

impl Transport for CliTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Cli(self.cli)
    }

    fn send(
        &self,
        host: &HostKey,
        request: &ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<ApiResponse, TransportError> {
        let fingerprint =
            crate::forge::ratelimit::Fingerprint::cli(host, self.cli, &self.executable);
        let limiter = crate::forge::ratelimit::global();
        limiter
            .check(fingerprint, request.class, Instant::now())
            .map_err(|until| TransportError::RateLimited { until })?;
        let (mut args, mut stdin, extra) = argv(self.cli, host, request, self.login.as_deref());
        // Authored text travels in a private file the child reads, removed when it exits.
        let _body = match stdin.take() {
            Some(bytes) if request.body_file => {
                let file = body_file(&bytes).map_err(|error| {
                    TransportError::Spawn(format!("cannot write the request body: {error}"))
                })?;
                point_at_file(self.cli, &mut args, file.path());
                Some(file)
            }
            other => {
                stdin = other;
                None
            }
        };
        let mut env = child_env();
        env.extend(extra.into_iter().map(|(key, value)| (key, value.into())));
        let spec = ProcessSpec {
            program: self.executable.clone(),
            args,
            cwd: std::env::temp_dir(),
            env,
            stdin,
            timeout: request.timeout,
            cap: request.body_cap + 256 * 1024,
        };
        let output = run_process(&spec, cancel)?;
        if let Some(response) = parse_included(&output.stdout) {
            let graphql = request.target == Target::GraphQl;
            if let Some(until) = limiter.observe(fingerprint, &response, graphql, Instant::now()) {
                return Err(TransportError::RateLimited { until });
            }
            return Ok(response);
        }
        let stderr = redact(&String::from_utf8_lossy(&output.stderr));
        if let ForgeError::RateLimited { .. } = classify(self.cli, &stderr, output.code) {
            let until = limiter.trip(fingerprint, None, Instant::now());
            return Err(TransportError::RateLimited { until });
        }
        Err(TransportError::Cli {
            cli: self.cli,
            code: output.code,
            stderr,
        })
    }

    fn graphql_batching(&self) -> bool {
        self.cli != Cli::Tea
    }
}

/// A CLI invocation: argv, stdin and extra environment.
pub type Invocation = (Vec<String>, Option<Vec<u8>>, Vec<(String, String)>);

/// argv, stdin and extra environment for one request. Bodies always travel on stdin.
pub fn argv(cli: Cli, host: &HostKey, request: &ApiRequest, login: Option<&str>) -> Invocation {
    let endpoint = match &request.target {
        Target::GraphQl => "graphql".to_owned(),
        _ => request.path_and_query(),
    };
    let accept = request
        .accept
        .header(crate::forge::provider::ProviderKind::GitHub);
    let mut args: Vec<String> = Vec::new();
    let mut env = Vec::new();
    match cli {
        Cli::Gh => {
            args.extend([
                "api".into(),
                endpoint,
                "--hostname".into(),
                host.to_string(),
                "--method".into(),
                request.method.as_str().into(),
                "--include".into(),
                "-H".into(),
                format!("Accept: {accept}"),
            ]);
        }
        Cli::Glab => {
            args.extend([
                "api".into(),
                endpoint,
                "--hostname".into(),
                host.to_string(),
                "-X".into(),
                request.method.as_str().into(),
                "--include".into(),
            ]);
            env.push(("GITLAB_HOST".into(), host.to_string()));
        }
        Cli::Tea => {
            args.extend(["api".into()]);
            if let Some(login) = login {
                args.extend(["--login".into(), login.into()]);
            }
            args.extend([
                "--method".into(),
                request.method.as_str().into(),
                "--include".into(),
                endpoint,
            ]);
        }
    }
    for (key, value) in &request.headers {
        args.extend(["-H".into(), format!("{key}: {value}")]);
    }
    let stdin = request.body.as_ref().map(|body| {
        match cli {
            Cli::Tea => args.extend(["-d".into(), "@-".into()]),
            _ => {
                args.extend(["--input".into(), "-".into()]);
                args.extend([
                    "-H".into(),
                    format!("Content-Type: {}", body.content_type()),
                ]);
            }
        }
        body.bytes()
    });
    (args, stdin, env)
}

/// A temporary file holding `bytes`, readable by this user only (tempfile creates 0600).
pub fn body_file(bytes: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix("dusty-forge-body-")
        .suffix(".json")
        .tempfile()?;
    std::io::Write::write_all(&mut file, bytes)?;
    std::io::Write::flush(&mut file)?;
    Ok(file)
}

/// Rewrite the stdin form of `argv` (`--input -`, tea's `-d @-`) to read `path` instead.
pub fn point_at_file(cli: Cli, args: &mut [String], path: &std::path::Path) {
    let path = path.to_string_lossy().into_owned();
    let (flag, stdin, file) = match cli {
        Cli::Tea => ("-d", "@-", format!("@{path}")),
        _ => ("--input", "-", path),
    };
    if let Some(at) = args
        .windows(2)
        .position(|pair| pair[0] == flag && pair[1] == stdin)
    {
        args[at + 1] = file;
    }
}

/// The `--include` form: a status line, headers, a blank line, then the body.
pub fn parse_included(output: &[u8]) -> Option<ApiResponse> {
    if !output.starts_with(b"HTTP/") {
        return None;
    }
    let (head, body) = split_head(output)?;
    let head = std::str::from_utf8(head).ok()?;
    let mut lines = head.lines();
    let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    Some(ApiResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

fn split_head(output: &[u8]) -> Option<(&[u8], &[u8])> {
    let crlf = output.windows(4).position(|window| window == b"\r\n\r\n");
    let lf = output.windows(2).position(|window| window == b"\n\n");
    match (crlf, lf) {
        (Some(crlf), Some(lf)) if lf + 1 < crlf => Some((&output[..lf], &output[lf + 2..])),
        (Some(crlf), _) => Some((&output[..crlf], &output[crlf + 4..])),
        (None, Some(lf)) => Some((&output[..lf], &output[lf + 2..])),
        (None, None) => Some((output, &[])),
    }
}

/// A CLI failure without an HTTP response (`gh: Not Found (HTTP 404)`, glab `404 Not Found`).
pub fn classify(cli: Cli, stderr: &str, code: Option<i32>) -> ForgeError {
    let text = redact(stderr);
    let lower = text.to_ascii_lowercase();
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| format!("{}: {line}", cli.name()))
        .unwrap_or_else(|| format!("{} failed ({code:?})", cli.name()));
    if lower.contains("rate limit") || lower.contains("abuse") || lower.contains("http 429") {
        ForgeError::RateLimited {
            until: Instant::now() + Duration::from_secs(30),
        }
    } else if lower.contains("http 401")
        || lower.contains("401 unauthorized")
        || lower.contains("auth login")
        || lower.contains("not logged")
    {
        ForgeError::NotAuthenticated {
            host: String::new(),
            hint: format!("{} auth login", cli.name()),
        }
    } else if lower.contains("http 404") || lower.contains("404 not found") {
        ForgeError::NotFound(line)
    } else if lower.contains("http 403") || lower.contains("403 forbidden") {
        ForgeError::Forbidden(line)
    } else if lower.contains("http 409") || lower.contains("http 405") {
        ForgeError::Conflict(line)
    } else if lower.contains("could not resolve")
        || lower.contains("connection refused")
        || lower.contains("dial tcp")
        || lower.contains("no such host")
    {
        ForgeError::Network(line)
    } else {
        ForgeError::Failed(line)
    }
}

fn redaction_patterns() -> &'static [(regex::Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (r"gh[pousr]_[A-Za-z0-9_]{16,}", "<redacted>"),
            (r"github_pat_[A-Za-z0-9_]{16,}", "<redacted>"),
            (
                r"gl(?:pat|rt|oas|dt|ptt|ft|cbt|imt|soat|agent|ffct)-[A-Za-z0-9_\-]{16,}",
                "<redacted>",
            ),
            (r"(?i)(token=)[0-9a-f]{40}", "${1}<redacted>"),
            (
                r"(?i)((?:private-token|authorization|x-gitea-token)\s*:\s*)[^\r\n]+",
                "${1}<redacted>",
            ),
            (
                r"(?i)(\b(?:bearer|token)\s+)[A-Za-z0-9_\-\.=]{20,}",
                "${1}<redacted>",
            ),
            // `token: …` / `"token": "…"` / `refresh_token=…` in echoed config or JSON.
            (
                r#"(?i)((?:\b|_)token"?\s*[:=]\s*"?)[A-Za-z0-9_\-\.]{16,}"#,
                "${1}<redacted>",
            ),
            // Credentials in URLs: `https://user:secret@host`.
            (
                r"(?i)(\b[a-z][a-z0-9+.\-]*://[^/\s:@]+:)[^@\s/]+@",
                "${1}<redacted>@",
            ),
        ]
        .into_iter()
        .map(|(pattern, replacement)| {
            (
                regex::Regex::new(pattern).expect("redaction pattern"),
                replacement,
            )
        })
        .collect()
    })
}

const REMEMBERED_SECRETS: usize = 64;

fn remembered() -> &'static std::sync::Mutex<Vec<String>> {
    static SECRETS: OnceLock<std::sync::Mutex<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(Default::default)
}

/// Every token Dusty has read (`credential::Secret::new`) is also redacted verbatim, whatever
/// its shape (Gitea's 40-hex tokens look like commit hashes to a pattern).
pub fn remember_secret(token: &str) {
    let token = token.trim();
    if token.len() < 8 {
        return;
    }
    let mut secrets = remembered()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if secrets.iter().any(|known| known == token) {
        return;
    }
    if secrets.len() >= REMEMBERED_SECRETS {
        secrets.remove(0);
    }
    secrets.push(token.to_owned());
}

/// Strip anything token-shaped before text reaches a notice, the command log or a test log.
pub fn redact(text: &str) -> String {
    let mut text = text.to_owned();
    for secret in remembered()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
    {
        if text.contains(secret.as_str()) {
            text = text.replace(secret.as_str(), "<redacted>");
        }
    }
    for (pattern, replacement) in redaction_patterns() {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    text
}

/// `gh version 2.62.0 (…)`, `glab 1.50.0 (…)`, `glab version 1.36.0`, `Version: 0.10.1\tgolang…`
/// → (major, minor, patch).
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    text.split(|c: char| c.is_whitespace() || c == ',')
        .map(|word| word.trim_start_matches('v'))
        .find_map(|word| {
            let mut parts = word.split(['.', '-', '+']);
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next()?.parse().ok()?;
            let patch = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
            Some((major, minor, patch))
        })
}

/// The oldest CLI whose passthrough form §1.2 relies on.
pub fn minimum_version(cli: Cli) -> ((u32, u32, u32), &'static str) {
    match cli {
        Cli::Gh => ((2, 40, 0), "2.40"),
        Cli::Glab => ((1, 40, 0), "1.40"),
        // `tea api` arrived in 0.10.1; older tea has no passthrough at all.
        Cli::Tea => ((0, 10, 1), "0.10.1"),
    }
}

type Versions = std::collections::HashMap<PathBuf, (Option<String>, Instant)>;

fn versions() -> &'static std::sync::Mutex<Versions> {
    static VERSIONS: OnceLock<std::sync::Mutex<Versions>> = OnceLock::new();
    VERSIONS.get_or_init(Default::default)
}

/// `<cli> --version`, cached for 10 min per executable. `Ok(None)`: unreadable output (treated
/// as new enough); `Err(CliTooOld)` below the minimum; `Err(CliMissing)` when absent.
pub fn check_version(
    cli: Cli,
    executable: &std::path::Path,
    cancel: &AtomicBool,
) -> Result<Option<String>, ForgeError> {
    const TTL: Duration = Duration::from_secs(10 * 60);
    let cached = versions()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(executable)
        .filter(|(_, at)| at.elapsed() < TTL)
        .map(|(version, _)| version.clone());
    let found = match cached {
        Some(found) => found,
        None => {
            let spec = ProcessSpec {
                program: executable.to_path_buf(),
                args: vec!["--version".into()],
                cwd: std::env::temp_dir(),
                env: child_env(),
                stdin: None,
                timeout: Duration::from_secs(10),
                cap: 64 * 1024,
            };
            let output = match run_process(&spec, cancel) {
                Ok(output) => output,
                Err(TransportError::Spawn(_)) => return Err(ForgeError::CliMissing(cli)),
                Err(TransportError::Cancelled) => return Err(ForgeError::Cancelled),
                Err(_) => return Ok(None),
            };
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let found = parse_version(&text).map(|(a, b, c)| format!("{a}.{b}.{c}"));
            versions()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(executable.to_path_buf(), (found.clone(), Instant::now()));
            found
        }
    };
    let (minimum, needed) = minimum_version(cli);
    match found.as_deref().and_then(parse_version) {
        Some(version) if version < minimum => Err(ForgeError::CliTooOld {
            cli,
            found: found.unwrap_or_default(),
            needed,
        }),
        _ => Ok(found),
    }
}

/// The environment of every forge child: no prompts, pagers, colours or debug output.
pub fn child_env() -> BTreeMap<String, OsString> {
    let mut environment: BTreeMap<String, OsString> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
        .collect();
    environment.remove("GH_DEBUG");
    environment.remove("DEBUG");
    for (key, value) in [
        ("GH_PROMPT_DISABLED", "1"),
        ("GLAB_NO_PROMPT", "1"),
        ("NO_COLOR", "1"),
        ("PAGER", "cat"),
        ("GH_PAGER", "cat"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", "/bin/false"),
        ("SSH_ASKPASS", "/bin/false"),
        ("SSH_ASKPASS_REQUIRE", "force"),
    ] {
        environment.insert(key.into(), value.into());
    }
    environment
}

pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, OsString>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub cap: usize,
}

#[derive(Debug)]
pub struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run a child on the job supervisor: bounded output, timeout, killed on cancel.
pub fn run_process(
    spec: &ProcessSpec,
    cancel: &AtomicBool,
) -> Result<ProcessOutput, TransportError> {
    use crate::process::JobEvent;
    use crate::process::JobPolicy;
    use crate::process::JobRequest;
    use crate::process::JobStream;
    use crate::process::JobSupervisor;
    if cancel.load(Ordering::Acquire) {
        return Err(TransportError::Cancelled);
    }
    if !spec.program.is_file() {
        return Err(TransportError::Spawn(format!(
            "{} is not installed",
            spec.program.display()
        )));
    }
    // Every forge child (passthrough, `gh auth token`, `glab auth status`, version checks)
    // shares the ≤ 4 bound of §3.5.
    let _permit = CLI_BOUND.acquire(cancel)?;
    let mut jobs =
        JobSupervisor::try_new(64).map_err(|error| TransportError::Spawn(error.to_string()))?;
    jobs.start(JobRequest {
        program: spec.program.clone(),
        argv0: None,
        arguments: spec.args.iter().map(Into::into).collect(),
        cwd: spec.cwd.clone(),
        environment: spec.env.clone(),
        root_revision: 0,
        stdin: spec.stdin.clone(),
        policy: JobPolicy {
            // The supervisor refuses line caps above 16 MiB; a passthrough body is one line
            // of at most `LARGE_BODY_CAP`, so the clamp only drops the header slack.
            // ... and output above 256 MiB.
            maximum_output_bytes: spec.cap.min(256 * 1024 * 1024),
            maximum_line_bytes: spec.cap.min(16 * 1024 * 1024),
            timeout: spec.timeout,
            ..JobPolicy::default()
        },
    })
    .map_err(|error| TransportError::Spawn(error.to_string()))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        for event in jobs.poll() {
            match event {
                JobEvent::Output {
                    stream,
                    bytes,
                    truncated,
                    ..
                } => {
                    if truncated {
                        return Err(TransportError::TooLarge(spec.cap));
                    }
                    match stream {
                        JobStream::Stdout => stdout.extend(bytes),
                        JobStream::Stderr => stderr.extend(bytes),
                    }
                }
                JobEvent::Exited {
                    code, timed_out, ..
                } => {
                    if timed_out {
                        return Err(TransportError::Timeout);
                    }
                    return Ok(ProcessOutput {
                        code,
                        stdout,
                        stderr,
                    });
                }
                JobEvent::Failed { message, .. } => return Err(TransportError::Spawn(message)),
                JobEvent::Started { .. } => {}
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::transport::Method;

    #[test]
    fn included_output_parses_status_headers_and_body() {
        let output = b"HTTP/2.0 404 Not Found\nContent-Type: application/json\r\nEtag: \"x\"\r\n\r\n{\"message\":\"Not Found\"}";
        let response = parse_included(output).unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.header("ETag"), Some("\"x\""));
        assert_eq!(response.body, b"{\"message\":\"Not Found\"}");
        let empty = parse_included(b"HTTP/1.1 204 No Content\r\nX-A: b\r\n\r\n").unwrap();
        assert_eq!((empty.status, empty.body.len()), (204, 0));
        assert!(parse_included(b"gh: not logged in").is_none());
    }

    #[test]
    fn gh_argv_carries_bodies_on_stdin_and_never_a_token() {
        let request = ApiRequest::new(Method::Post, "repos/o/r/issues/7/comments")
            .json(serde_json::json!({"body": "multi\nline"}));
        let (args, stdin, _) = argv(Cli::Gh, &HostKey::new("github.com"), &request, None);
        assert_eq!(&args[..2], ["api", "repos/o/r/issues/7/comments"]);
        for flag in ["--include", "--input", "--hostname"] {
            assert!(args.iter().any(|arg| arg == flag), "{flag}: {args:?}");
        }
        assert_eq!(stdin.unwrap(), br#"{"body":"multi\nline"}"#);
        let graphql =
            ApiRequest::graphql("query Viewer { viewer { login } }", serde_json::json!({}));
        let (args, stdin, _) = argv(Cli::Gh, &HostKey::new("github.com"), &graphql, None);
        assert_eq!(args[1], "graphql");
        assert!(
            String::from_utf8(stdin.unwrap())
                .unwrap()
                .contains("Viewer")
        );
        let (tea, _, _) = argv(
            Cli::Tea,
            &HostKey::new("codeberg.org"),
            &request,
            Some("cb"),
        );
        assert!(tea.iter().any(|arg| arg == "--include"));
        assert!(tea.windows(2).any(|pair| pair == ["-d", "@-"]));
    }

    #[test]
    fn authored_bodies_travel_in_a_private_file_over_every_cli() {
        let request = ApiRequest::new(Method::Post, "repos/o/r/pulls")
            .json(serde_json::json!({"title": "t", "body": "long\ndescription"}))
            .body_file();
        let file = body_file(&request.body.as_ref().unwrap().bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only the user reads the body: {mode:o}");
        }
        let written = String::from_utf8(std::fs::read(file.path()).unwrap()).unwrap();
        assert!(written.contains(r#""body":"long\ndescription""#), "{written}");
        let path = file.path().to_string_lossy().into_owned();
        for (cli, flag, value) in [
            (Cli::Gh, "--input", path.clone()),
            (Cli::Glab, "--input", path.clone()),
            (Cli::Tea, "-d", format!("@{path}")),
        ] {
            let (mut args, stdin, _) = argv(cli, &HostKey::new("h.test"), &request, None);
            assert!(stdin.is_some());
            point_at_file(cli, &mut args, file.path());
            assert!(
                args.windows(2).any(|pair| pair[0] == flag && pair[1] == value),
                "{cli:?}: {args:?}"
            );
            assert!(!args.iter().any(|arg| arg == "-" || arg == "@-"), "{args:?}");
        }
        drop(file);
        assert!(!std::path::Path::new(&path).exists(), "the file goes with the request");
    }

    #[test]
    fn tokens_are_redacted_everywhere() {
        let text = "Authorization: Bearer abc.def\nghp_0123456789abcdefABCDEF0123456789abcd \
                    github_pat_11ABCDEFG0123456789_abcdefghijk glpat-0123456789abcdefghij \
                    token=0123456789abcdef0123456789abcdef01234567 PRIVATE-TOKEN: secret-value";
        let redacted = redact(text);
        for leak in [
            "abc.def",
            "ghp_0123",
            "github_pat_11",
            "glpat-0123",
            "0123456789abcdef0123456789abcdef01234567",
            "secret-value",
        ] {
            assert!(!redacted.contains(leak), "{leak} leaked: {redacted}");
        }
        assert_eq!(redact("nothing to hide"), "nothing to hide");
    }

    #[test]
    fn cli_errors_classify_into_fixed_variants() {
        assert!(matches!(
            classify(Cli::Gh, "gh: Not Found (HTTP 404)", Some(1)),
            ForgeError::NotFound(_)
        ));
        assert!(matches!(
            classify(Cli::Glab, "404 Not Found", Some(1)),
            ForgeError::NotFound(_)
        ));
        assert!(matches!(
            classify(
                Cli::Gh,
                "To get started with GitHub CLI, please run:  gh auth login",
                Some(4)
            ),
            ForgeError::NotAuthenticated { .. }
        ));
        assert!(matches!(
            classify(Cli::Gh, "gh: API rate limit exceeded (HTTP 403)", Some(1)),
            ForgeError::RateLimited { .. }
        ));
    }
}
