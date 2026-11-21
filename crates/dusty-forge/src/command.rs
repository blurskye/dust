//! `dusty-git forge …`: the forge layer for other editors and scripts, JSON
//! only. Every top-level object carries `"schema": 1`; lists print one object per line (JSON
//! Lines); errors go to stderr as `{"schema":1,"error":{"kind","message","hint"}}` with a fixed
//! exit code. Reads share the TUI's on-disk cache: a fresh entry answers without a
//! request, a stale one is revalidated with its ETag.

use super::ForgeCommands;
use super::ForgeTarget;
use super::worker;
use super::worker::Served;
use crate::cache;
use crate::capability;
use crate::detect;
use crate::host_state;
use crate::model::ItemId;
use crate::model::ItemKind;
use crate::provider::ProviderKind;
use crate::provider::RepoRef;
use crate::request::Filter;
use crate::request::ForgeError;
use crate::request::NotifyFilter;
use crate::request::Page;
use crate::request::Payload;
use crate::request::Read;
use crate::request::RunFilter;
use crate::request::StateFilter;
use crate::request::Write;
use crate::transport::ConnectOptions;
use crate::transport::HostKey;
use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;
use serde_json::Value;
use serde_json::json;
use std::ffi::OsString;
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

pub const SCHEMA: u64 = 1;

pub const EXIT_OK: u8 = 0;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_CLI_MISSING: u8 = 3;
pub const EXIT_NOT_AUTHENTICATED: u8 = 4;
pub const EXIT_UNSUPPORTED: u8 = 5;
pub const EXIT_NETWORK: u8 = 6;
pub const EXIT_OTHER: u8 = 7;

#[derive(Parser, Debug)]
#[command(
    name = "dusty-git forge",
    about = "The forge of a repository as JSON: pull/merge requests, issues, CI, notifications",
    after_help = "Exit codes: 0 ok, 2 usage, 3 CLI missing, 4 not signed in, 5 unsupported, \
                  6 network or rate limited, 7 other. Errors are JSON on stderr."
)]
pub struct ForgeCli {
    /// Repository or a directory inside it.
    #[arg(long, global = true, value_name = "PATH", default_value = ".")]
    repo: PathBuf,
    /// Indent the JSON.
    #[arg(long, global = true)]
    pretty: bool,
    /// Ask the forge even when the cache holds a fresh answer.
    #[arg(long, global = true)]
    refresh: bool,
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand, Debug)]
enum Verb {
    /// Provider, endpoints, transport, sign-in state, capabilities and terms (never a token).
    Detect,
    /// Pull requests (JSON Lines of rows).
    Prs(ListArgs),
    /// Merge requests: `prs` under GitLab's name.
    Mrs(ListArgs),
    /// Issues (JSON Lines of rows).
    Issues(ListArgs),
    /// One pull or merge request.
    Pr { number: u64 },
    /// One issue.
    Issue { number: u64 },
    /// Review threads of a pull or merge request.
    Threads { number: u64 },
    /// The timeline of a pull request or issue.
    Timeline {
        number: u64,
        /// Read the issue instead of the pull or merge request.
        #[arg(long)]
        issue: bool,
    },
    /// The unified diff of a pull or merge request.
    Diff { number: u64 },
    /// CI runs, pipelines on GitLab (JSON Lines).
    Runs {
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        event: Option<String>,
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// One run with its jobs.
    Run { id: String },
    /// The log of one job.
    Log {
        #[arg(value_name = "JOB-ID")]
        job: String,
    },
    /// Artifacts of one run.
    Artifacts {
        #[arg(value_name = "RUN-ID")]
        run: String,
    },
    /// Notifications (GitLab to-dos), JSON Lines.
    Notifications {
        /// Include read ones.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// Comment on a pull request (or an issue with --issue).
    Comment {
        number: u64,
        /// The comment's Markdown; `-` reads stdin.
        #[arg(long, value_name = "F")]
        body_file: PathBuf,
        #[arg(long)]
        issue: bool,
    },
    /// Check a pull request out in this repository.
    Checkout { number: u64 },
    /// Open a pull request (or an issue with --issue) in the browser.
    Open {
        number: u64,
        #[arg(long)]
        issue: bool,
    },
    /// The on-disk cache shared with the TUI.
    Cache {
        #[arg(value_enum)]
        action: CacheAction,
    },
}

#[derive(clap::Args, Debug)]
struct ListArgs {
    #[arg(long, value_enum, default_value_t = State::Open)]
    state: State,
    #[arg(long, default_value = "")]
    query: String,
    #[arg(long, default_value_t = 1)]
    page: u32,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum State {
    Open,
    Closed,
    Merged,
    All,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CacheAction {
    Stats,
    Clear,
}

/// A failure as the CLI reports it.
#[derive(Debug)]
pub struct Failure {
    pub kind: &'static str,
    pub message: String,
    pub hint: String,
    pub code: u8,
}

impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            kind: "Usage",
            message: message.into(),
            hint: "dusty-git forge --help".into(),
            code: EXIT_USAGE,
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "schema": SCHEMA,
            "error": { "kind": self.kind, "message": self.message, "hint": self.hint },
        })
    }
}

impl From<ForgeError> for Failure {
    fn from(error: ForgeError) -> Self {
        let message = crate::redact(&error.to_string());
        let (kind, code, hint) = match &error {
            ForgeError::CliMissing(cli) => (
                "CliMissing",
                EXIT_CLI_MISSING,
                host_state::install_hint(*cli).to_owned(),
            ),
            ForgeError::CliTooOld { cli, needed, .. } => (
                "CliTooOld",
                EXIT_CLI_MISSING,
                format!("upgrade {} to {needed} or newer", cli.name()),
            ),
            ForgeError::NotAuthenticated { hint, .. } => {
                ("NotAuthenticated", EXIT_NOT_AUTHENTICATED, hint.clone())
            }
            ForgeError::SignInRequired { hints, .. } => {
                ("SignInRequired", EXIT_NOT_AUTHENTICATED, hints.join(" · "))
            }
            // One read that needs a credential while browsing anonymously (job logs).
            ForgeError::Unsupported {
                capability: "sign_in",
                reason,
            } => ("NotAuthenticated", EXIT_NOT_AUTHENTICATED, reason.clone()),
            ForgeError::Unsupported { reason, .. } => {
                ("Unsupported", EXIT_UNSUPPORTED, reason.clone())
            }
            ForgeError::RateLimited { until } => (
                "RateLimited",
                EXIT_NETWORK,
                format!(
                    "retry in {}s",
                    until
                        .saturating_duration_since(std::time::Instant::now())
                        .as_secs()
                ),
            ),
            ForgeError::Network(_) => ("Network", EXIT_NETWORK, String::new()),
            ForgeError::Timeout => ("Timeout", EXIT_NETWORK, String::new()),
            ForgeError::NotFound(_) => ("NotFound", EXIT_OTHER, String::new()),
            ForgeError::Forbidden(_) => ("Forbidden", EXIT_OTHER, String::new()),
            ForgeError::Conflict(_) => ("Conflict", EXIT_OTHER, String::new()),
            ForgeError::Cancelled => ("Cancelled", EXIT_OTHER, String::new()),
            ForgeError::BadResponse(_) => ("BadResponse", EXIT_OTHER, String::new()),
            ForgeError::Failed(_) => ("Failed", EXIT_OTHER, String::new()),
        };
        Self {
            kind,
            message,
            hint: crate::redact(&hint),
            code,
        }
    }
}

/// What a verb prints on success.
enum Output {
    Object(Value),
    Lines(Vec<Value>),
}

/// Run `dusty-git forge` with `args` (`args[0]` is the program name, then `forge`'s own
/// arguments) and return the exit code; stdout and stderr are written here.
pub fn main(args: Vec<OsString>) -> u8 {
    let cli = match ForgeCli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            use clap::error::ErrorKind;
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return EXIT_OK;
            }
            let message = error.render().to_string();
            let message = message
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("invalid arguments")
                .trim_start_matches("error: ")
                .to_owned();
            return fail(&Failure::usage(message), false);
        }
    };
    let pretty = cli.pretty;
    let cancel = AtomicBool::new(false);
    let result = run(&cli, &cancel);
    if let Some(cache) = cache::Cache::shared() {
        cache.flush();
    }
    match result {
        Ok(output) => {
            let mut stdout = std::io::stdout().lock();
            let text = match output {
                Output::Object(value) => render(&value, pretty),
                Output::Lines(lines) => lines
                    .iter()
                    .map(|line| render(line, false))
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if text.is_empty() {
                return EXIT_OK;
            }
            if writeln!(stdout, "{text}").is_err() {
                return EXIT_OTHER;
            }
            EXIT_OK
        }
        Err(failure) => fail(&failure, pretty),
    }
}

fn render(value: &Value, pretty: bool) -> String {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    };
    text.unwrap_or_default()
}

fn fail(failure: &Failure, pretty: bool) -> u8 {
    let _ = writeln!(
        std::io::stderr().lock(),
        "{}",
        render(&failure.json(), pretty)
    );
    failure.code
}

/// `value` as an object with `"schema": 1` first.
fn stamped(value: Value) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("schema".into(), json!(SCHEMA));
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                if key != "schema" {
                    object.insert(key, field);
                }
            }
        }
        other => {
            object.insert("data".into(), other);
        }
    }
    Value::Object(object)
}

/// The payload's own value, without the enum tag (`{"PullRequest": {…}}` → `{…}`).
fn untagged(payload: &Payload) -> Value {
    match serde_json::to_value(payload).unwrap_or(Value::Null) {
        Value::Object(mut fields) if fields.len() == 1 => {
            let key = fields.keys().next().cloned().unwrap_or_default();
            fields.remove(&key).unwrap_or(Value::Null)
        }
        other => other,
    }
}

fn to_value(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// The repository's work tree (a subdirectory or a file inside it is fine).
fn root_of(path: &Path) -> Result<PathBuf, Failure> {
    let repository = git2::Repository::discover(path).map_err(|_| {
        Failure::from(ForgeError::Failed(format!(
            "not a Git repository: {}",
            path.display()
        )))
    })?;
    Ok(repository
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf()))
}

struct Scope {
    root: PathBuf,
    host: HostKey,
    repo: Option<RepoRef>,
}

impl Scope {
    fn new(cli: &ForgeCli, host_wide: bool) -> Result<Self, Failure> {
        let root = root_of(&cli.repo)?;
        let (host, repo) = ForgeTarget::remote().resolve(&root)?;
        Ok(Self {
            root,
            host,
            repo: if host_wide { None } else { repo },
        })
    }

    fn header(&self, kind: Option<ProviderKind>, served: Served) -> serde_json::Map<String, Value> {
        let mut header = serde_json::Map::new();
        header.insert("schema".into(), json!(SCHEMA));
        header.insert("host".into(), json!(self.host.to_string()));
        header.insert(
            "repo".into(),
            self.repo
                .as_ref()
                .map_or(Value::Null, |repo| json!(repo.full())),
        );
        if let Some(kind) = kind {
            header.insert("provider".into(), json!(kind.name()));
        }
        header.insert("source".into(), to_value(&served));
        header
    }

    /// `read` through the shared cache: a fresh entry answers without connecting at all.
    fn read(
        &self,
        read: &Read,
        refresh: bool,
        cancel: &AtomicBool,
    ) -> Result<(Payload, Served, Option<ProviderKind>), Failure> {
        if !refresh
            && let Some(payload) = worker::cached_fresh(&self.host, self.repo.as_ref(), read)
        {
            let kind = detect::detect(&self.host, &ConnectOptions::from_env(), cancel)
                .ok()
                .map(|detected| detected.kind);
            return Ok((payload, Served::Cache, kind));
        }
        let (provider, ctx) =
            worker::connect_to(&self.root, &self.host, self.repo.clone(), cancel)?;
        let (payload, served) =
            worker::cached_read(provider.as_ref(), &ctx, read, refresh, cancel)?;
        Ok((payload, served, Some(ctx.endpoints.kind)))
    }
}

fn run(cli: &ForgeCli, cancel: &AtomicBool) -> Result<Output, Failure> {
    let list = |kind: ItemKind, args: &ListArgs| Read::List {
        kind,
        filter: Filter {
            state: match args.state {
                State::Open => StateFilter::Open,
                State::Closed => StateFilter::Closed,
                State::Merged => StateFilter::Merged,
                State::All => StateFilter::All,
            },
            query: args.query.clone(),
        },
        page: Page {
            number: args.page.max(1),
            ..Page::default()
        },
    };
    let read = match &cli.verb {
        Verb::Detect => return detect_verb(cli, cancel),
        Verb::Cache { action } => return cache_verb(*action),
        Verb::Comment {
            number,
            body_file,
            issue,
        } => {
            let body = if body_file == Path::new("-") {
                let mut body = String::new();
                std::io::stdin()
                    .read_to_string(&mut body)
                    .map_err(|error| Failure::usage(format!("cannot read stdin: {error}")))?;
                body
            } else {
                std::fs::read_to_string(body_file).map_err(|error| {
                    Failure::usage(format!("cannot read {}: {error}", body_file.display()))
                })?
            };
            if body.trim().is_empty() {
                return Err(Failure::usage("the comment is empty"));
            }
            let kind = if *issue {
                ItemKind::Issue
            } else {
                ItemKind::PullRequest
            };
            return write_verb(
                cli,
                Write::Comment {
                    kind,
                    number: *number,
                    body,
                },
                cancel,
            );
        }
        Verb::Checkout { number } => {
            return write_verb(cli, Write::Checkout { number: *number }, cancel);
        }
        Verb::Open { number, issue } => {
            let kind = if *issue {
                ItemKind::Issue
            } else {
                ItemKind::PullRequest
            };
            return write_verb(
                cli,
                Write::OpenInBrowser {
                    kind,
                    number: *number,
                    url: None,
                },
                cancel,
            );
        }
        Verb::Prs(args) | Verb::Mrs(args) => list(ItemKind::PullRequest, args),
        Verb::Issues(args) => list(ItemKind::Issue, args),
        Verb::Pr { number } => Read::PullRequest(*number),
        Verb::Issue { number } => Read::Issue(*number),
        Verb::Threads { number } => Read::Threads(*number),
        Verb::Timeline { number, issue } => Read::Timeline {
            kind: if *issue {
                ItemKind::Issue
            } else {
                ItemKind::PullRequest
            },
            number: *number,
        },
        Verb::Diff { number } => Read::Diff(*number),
        Verb::Runs {
            branch,
            event,
            page,
        } => Read::Runs {
            filter: RunFilter {
                branch: branch.clone(),
                event: event.clone(),
                ..RunFilter::default()
            },
            page: Page {
                number: (*page).max(1),
                ..Page::default()
            },
        },
        Verb::Run { id } => Read::Run {
            id: ItemId::new(id),
            attempt: None,
        },
        Verb::Log { job } => Read::JobLog(ItemId::new(job)),
        Verb::Artifacts { run } => Read::Artifacts(ItemId::new(run)),
        Verb::Notifications { all, page } => Read::Notifications {
            filter: NotifyFilter {
                all: *all,
                since: None,
            },
            page: Page {
                number: (*page).max(1),
                ..Page::default()
            },
        },
    };
    let host_wide = matches!(read, Read::Notifications { .. });
    let scope = Scope::new(cli, host_wide)?;
    let (payload, served, kind) = scope.read(&read, cli.refresh, cancel)?;
    match payload {
        Payload::Rows(page) => Ok(Output::Lines(
            page.items
                .iter()
                .map(|row| stamped(to_value(row)))
                .collect(),
        )),
        Payload::Runs(page) => Ok(Output::Lines(
            page.items
                .iter()
                .map(|run| stamped(to_value(run)))
                .collect(),
        )),
        Payload::Notifications(page) => Ok(Output::Lines(
            page.items
                .iter()
                .map(|notification| stamped(to_value(notification)))
                .collect(),
        )),
        payload => {
            let mut object = scope.header(kind, served);
            object.insert("data".into(), untagged(&payload));
            Ok(Output::Object(Value::Object(object)))
        }
    }
}

fn write_verb(cli: &ForgeCli, write: Write, cancel: &AtomicBool) -> Result<Output, Failure> {
    let root = root_of(&cli.repo)?;
    let commands = ForgeCommands { cancelled: cancel };
    let receipt = worker::write_through(&commands, &root, &ForgeTarget::remote(), &write)?;
    Ok(Output::Object(stamped(json!({
        "ok": true,
        "summary": receipt.summary,
        "web_url": receipt.web_url,
        "created_number": receipt.created_item.map(|(_, number)| number),
        "created_kind": receipt.created_item.map(|(kind, _)| kind),
    }))))
}

fn detect_verb(cli: &ForgeCli, cancel: &AtomicBool) -> Result<Output, Failure> {
    let root = root_of(&cli.repo)?;
    let remote = detect::choose_remote(&root)?;
    let options = ConnectOptions::from_env();
    let detected = detect::detect(&remote.host, &options, cancel)?;
    let mut object = serde_json::Map::new();
    object.insert("schema".into(), json!(SCHEMA));
    object.insert("host".into(), json!(remote.host.to_string()));
    object.insert("repo".into(), json!(remote.repo.full()));
    object.insert("remote".into(), json!(remote.remote));
    object.insert("provider".into(), json!(detected.kind.name()));
    object.insert("source".into(), json!(detected.source.to_string()));
    object.insert("server_version".into(), json!(detected.server_version));
    object.insert("cli".into(), json!(detected.cli.map(|cli| cli.name())));
    object.insert("login".into(), json!(detected.login));
    object.insert("endpoints".into(), to_value(&detected.endpoints));
    object.insert("sign_in_required".into(), json!(detected.sign_in_required));
    object.insert(
        "terms".into(),
        to_value(capability::terms_for(Some(detected.kind))),
    );
    let connected = worker::connect_to(&root, &remote.host, Some(remote.repo.clone()), cancel);
    let mut failure = None;
    match &connected {
        Ok((provider, ctx)) => {
            object.insert("transport".into(), json!(ctx.transport_kind().to_string()));
            object.insert("transport_kind".into(), to_value(&ctx.transport_kind()));
            object.insert("capabilities".into(), to_value(&provider.capabilities(ctx)));
            object.insert("read_only".into(), json!(ctx.anonymous()));
            if ctx.anonymous() {
                // No credential to prove; one cheap public read proves the host answers and
                // fills in the quota left.
                object.insert("viewer".into(), Value::Null);
                if let Err(error) =
                    worker::cached_read(provider.as_ref(), ctx, &Read::Repository, false, cancel)
                {
                    failure = Some(Failure::from(error));
                }
            } else {
                // `/user` proves the credential (auth status misses env tokens).
                match worker::cached_read(provider.as_ref(), ctx, &Read::Viewer, false, cancel) {
                    Ok((Payload::User(user), _)) => {
                        object.insert("viewer".into(), json!(user.login));
                    }
                    Ok(_) => {}
                    Err(error) => failure = Some(Failure::from(error)),
                }
            }
        }
        Err(error) => failure = Some(Failure::from(error.clone())),
    }
    let account = host_state::account(&remote.host);
    object.insert(
        "state".into(),
        match &account {
            Some(account) => {
                let mut state = json!({
                    "label": account.state.label(),
                    "ready": account.state.ready(),
                    "read_only": account.state.read_only(),
                    "chip": account.state.chip(),
                    "lines": account.lines().into_iter().map(|(line, _)| line).collect::<Vec<_>>(),
                });
                match &account.state {
                    host_state::HostState::Anonymous {
                        remaining,
                        limit,
                        reset,
                        hint,
                    } => {
                        state["anonymous"] = json!({
                            "remaining": remaining,
                            "limit": limit,
                            "reset_in": reset.map(|reset| reset
                                .saturating_duration_since(std::time::Instant::now())
                                .as_secs()),
                            "hint": hint,
                        });
                    }
                    host_state::HostState::SignInRequired { provider, hints } => {
                        state["sign_in_required"] = json!({
                            "provider": provider.map(|kind| kind.name()),
                            "hints": hints,
                        });
                    }
                    _ => {}
                }
                state
            }
            None => Value::Null,
        },
    );
    if let Some(failure) = failure {
        object.insert("error".into(), failure.json()["error"].clone());
        // The description still goes to stdout; the exit code says what is wrong.
        let _ = writeln!(
            std::io::stdout().lock(),
            "{}",
            render(&Value::Object(object), cli.pretty)
        );
        return Err(failure);
    }
    Ok(Output::Object(Value::Object(object)))
}

fn cache_verb(action: CacheAction) -> Result<Output, Failure> {
    let Some(cache) = cache::Cache::shared() else {
        return Ok(Output::Object(stamped(json!({
            "root": Value::Null,
            "entries": 0,
            "bytes": 0,
            "hosts": {},
        }))));
    };
    let root = cache.root().to_path_buf();
    match action {
        CacheAction::Clear => {
            let (entries, bytes, _) = walk(&root);
            cache.clear().map_err(|error| {
                Failure::from(ForgeError::Failed(format!(
                    "cannot clear the cache: {error}"
                )))
            })?;
            Ok(Output::Object(stamped(json!({
                "root": root,
                "cleared": true,
                "entries": entries,
                "bytes": bytes,
            }))))
        }
        CacheAction::Stats => {
            let (entries, bytes, hosts) = walk(&root);
            Ok(Output::Object(stamped(json!({
                "root": root,
                "entries": entries,
                "bytes": bytes,
                "hosts": hosts,
            }))))
        }
    }
}

/// Entries (JSON files other than indexes) and bytes below the cache root, per host directory.
fn walk(root: &Path) -> (u64, u64, serde_json::Map<String, Value>) {
    fn visit(directory: &Path, entries: &mut u64, bytes: &mut u64) {
        let Ok(children) = std::fs::read_dir(directory) else {
            return;
        };
        for child in children.flatten() {
            let path = child.path();
            let Ok(metadata) = child.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                visit(&path, entries, bytes);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && path.file_name().is_some_and(|name| name != "index.json")
            {
                *entries += 1;
                *bytes += metadata.len();
            }
        }
    }
    let mut hosts = serde_json::Map::new();
    let (mut entries, mut bytes) = (0, 0);
    if let Ok(children) = std::fs::read_dir(root) {
        for child in children.flatten() {
            let path = child.path();
            if !path.is_dir() {
                continue;
            }
            let (mut count, mut size) = (0, 0);
            visit(&path, &mut count, &mut size);
            entries += count;
            bytes += size;
            hosts.insert(
                child.file_name().to_string_lossy().into_owned(),
                json!({ "entries": count, "bytes": size }),
            );
        }
    }
    (entries, bytes, hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(error: ForgeError) -> u8 {
        Failure::from(error).code
    }

    #[test]
    fn errors_map_onto_fixed_exit_codes() {
        use crate::transport::Cli;
        assert_eq!(exit(ForgeError::CliMissing(Cli::Glab)), EXIT_CLI_MISSING);
        assert_eq!(
            exit(ForgeError::NotAuthenticated {
                host: "gitlab.com".into(),
                hint: "glab auth login".into(),
            }),
            EXIT_NOT_AUTHENTICATED
        );
        assert_eq!(
            exit(ForgeError::Unsupported {
                capability: "x",
                reason: "no".into(),
            }),
            EXIT_UNSUPPORTED
        );
        assert_eq!(exit(ForgeError::Network("down".into())), EXIT_NETWORK);
        assert_eq!(
            exit(ForgeError::RateLimited {
                until: std::time::Instant::now(),
            }),
            EXIT_NETWORK
        );
        assert_eq!(exit(ForgeError::NotFound("pr".into())), EXIT_OTHER);
        let json = Failure::from(ForgeError::NotAuthenticated {
            host: "gitlab.com".into(),
            hint: "glab auth login".into(),
        })
        .json();
        assert_eq!(json["schema"], 1);
        assert_eq!(json["error"]["kind"], "NotAuthenticated");
        assert_eq!(json["error"]["hint"], "glab auth login");
    }

    #[test]
    fn usage_errors_are_json_with_exit_code_two() {
        let args = |list: &[&str]| {
            std::iter::once("forge")
                .chain(list.iter().copied())
                .map(OsString::from)
                .collect::<Vec<_>>()
        };
        assert!(ForgeCli::try_parse_from(args(&["prs", "--state", "open"])).is_ok());
        assert!(ForgeCli::try_parse_from(args(&["pr", "nope"])).is_err());
        assert_eq!(main(args(&["frobnicate"])), EXIT_USAGE);
        assert_eq!(main(args(&["pr"])), EXIT_USAGE);
        assert_eq!(main(args(&["prs", "--state", "sideways"])), EXIT_USAGE);
    }

    #[test]
    fn payloads_print_without_their_tag_and_with_the_schema() {
        let payload = Payload::Text("diff --git a b".into());
        assert_eq!(untagged(&payload), json!("diff --git a b"));
        let row = stamped(json!({ "number": 3, "schema": 9 }));
        assert_eq!(row, json!({ "schema": 1, "number": 3 }));
    }
}
