#![cfg(unix)]
//! Field-set parity for the legacy adapter: every `legacy::encode` shape
//! carries exactly the fields the old `gh … --json` calls requested, and for the same fixture
//! world its JSON has the same structure as what the old gh-CLI fixture
//! (`tests/fixtures/github_cli.py`) printed for that command, so `view.rs` reads it unchanged.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dusty::forge::credential::Credential;
use dusty::forge::credential::CredentialSource;
use dusty::forge::credential::Secret;
use dusty::forge::http::HttpTransport;
use dusty::forge::legacy;
use dusty::forge::legacy::Shape;
use dusty::forge::model::ItemId;
use dusty::forge::model::ItemKind;
use dusty::forge::provider;
use dusty::forge::provider::Conditional;
use dusty::forge::provider::Fetched;
use dusty::forge::provider::ForgeContext;
use dusty::forge::provider::ProviderKind;
use dusty::forge::provider::RepoRef;
use dusty::forge::request::Filter;
use dusty::forge::request::Page;
use dusty::forge::request::Payload;
use dusty::forge::request::Read;
use dusty::forge::request::RunFilter;
use dusty::forge::request::StateFilter;
use dusty::forge::transport::ConnectOptions;
use dusty::forge::transport::Endpoints;
use dusty::forge::transport::HostKey;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

struct Parity {
    directory: tempfile::TempDir,
    _server: forge_fake::ForgeServer,
    ctx: ForgeContext,
}

/// The same world on both sides: the fake server's legacy world and the old fixture.
fn world() -> Value {
    json!({"calls":[],"comments":["Parity comment"],"run_views":5})
}

impl Parity {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("server.json");
        std::fs::write(&state, world().to_string()).unwrap();
        let server = forge_fake::ForgeServer::start(&state, "legacy");
        let mut endpoints = Endpoints::github(&HostKey::new("github.com"));
        endpoints.override_api_base(&format!("{}/github.com/api/v3", server.url()));
        let transport = HttpTransport::new(
            endpoints.clone(),
            Credential {
                token: Secret::new(forge_fake::FAKE_TOKEN),
                source: CredentialSource::GhAuthToken,
                user: None,
                obtained: Instant::now(),
            },
        )
        .unwrap();
        let ctx = ForgeContext::new(
            directory.path().to_path_buf(),
            endpoints,
            ConnectOptions::default(),
            RepoRef::parse("example/demo"),
            Arc::new(transport),
        );
        Self {
            directory,
            _server: server,
            ctx,
        }
    }

    fn read(&self, read: Read) -> Payload {
        let provider = provider::build(ProviderKind::GitHub).unwrap();
        match provider
            .read(
                &self.ctx,
                &read,
                &Conditional::default(),
                &AtomicBool::new(false),
            )
            .unwrap()
        {
            Fetched::Payload { value, .. } => value,
            Fetched::NotModified => panic!("{read:?}"),
        }
    }

    /// What the old gh-CLI fixture printed for `gh <args>`.
    fn old(&self, args: &[&str]) -> String {
        let state = self.directory.path().join("old.json");
        std::fs::write(&state, world().to_string()).unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github_cli.py");
        let output = std::process::Command::new("python3")
            .arg(script)
            .args(args)
            .env("DUSTY_GITHUB_FIXTURE", &state)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn old_json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.old(args)).unwrap()
    }
}

fn keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {value}"))
        .keys()
        // The typed records' extension (`legacy::row_extension`) is not a gh field.
        .filter(|key| *key != "dusty")
        .cloned()
        .collect()
}

fn fields(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Every key the old fixture printed is present in the adapter's value with the same JSON
/// type, recursively (a null on either side stands for "absent in this world").
fn same_structure(old: &Value, new: &Value, at: &str) {
    if old.is_null() || new.is_null() {
        return;
    }
    assert_eq!(kind(old), kind(new), "{at}: old {old} vs adapter {new}");
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, value) in old {
                let next = format!("{at}.{key}");
                let other = new
                    .get(key)
                    .unwrap_or_else(|| panic!("{next} is missing from the adapter's value"));
                same_structure(value, other, &next);
            }
        }
        (Value::Array(old), Value::Array(new)) => {
            if let (Some(old), Some(new)) = (old.first(), new.first()) {
                same_structure(old, new, &format!("{at}[0]"));
            }
        }
        _ => {}
    }
}

/// view.rs's `check_summary` buckets for each check (its word is `check_state`: the
/// conclusion, else the status-context state, else the status).
fn check_words(checks: &[Value]) -> Vec<&'static str> {
    let mut words = checks
        .iter()
        .map(|check| {
            let word = ["conclusion", "state", "status"]
                .iter()
                .find_map(|key| check[key].as_str().filter(|word| !word.is_empty()))
                .unwrap_or("");
            match word {
                "SUCCESS" => "passed",
                "NEUTRAL" | "SKIPPED" => "skipped",
                "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STALE" => {
                    "failed"
                }
                "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED"
                | "PENDING_DEPLOYMENT" => "pending",
                _ => "unknown",
            }
        })
        .collect::<Vec<_>>();
    words.sort();
    words
}

#[test]
fn list_search_detail_run_and_repository_shapes_carry_exactly_the_gh_fields() {
    let parity = Parity::new();
    let open = Filter {
        state: StateFilter::Open,
        query: String::new(),
    };
    let list = |kind| Read::List {
        kind,
        filter: open.clone(),
        page: Page::first(30),
    };
    let search = |kind| Read::Search {
        kind,
        filter: open.clone(),
        page: Page::first(30),
    };
    let cases = [
        (
            Shape::List(ItemKind::Issue),
            list(ItemKind::Issue),
            legacy::ISSUE_LIST_FIELDS,
            Some(vec![
                "issue", "list", "--state", "open", "--limit", "30", "--json",
            ]),
        ),
        (
            Shape::List(ItemKind::PullRequest),
            list(ItemKind::PullRequest),
            legacy::PULL_REQUEST_LIST_FIELDS,
            Some(vec![
                "pr", "list", "--state", "open", "--limit", "30", "--json",
            ]),
        ),
        (
            Shape::Search(ItemKind::Issue),
            search(ItemKind::Issue),
            legacy::ISSUE_SEARCH_FIELDS,
            None,
        ),
        (
            Shape::Search(ItemKind::PullRequest),
            search(ItemKind::PullRequest),
            legacy::PULL_REQUEST_SEARCH_FIELDS,
            None,
        ),
        (
            Shape::Detail,
            Read::Issue(7),
            legacy::ISSUE_VIEW_FIELDS,
            Some(vec!["issue", "view", "7", "--json"]),
        ),
        (
            Shape::Detail,
            Read::PullRequest(9),
            legacy::PULL_REQUEST_VIEW_FIELDS,
            Some(vec!["pr", "view", "9", "--json"]),
        ),
        (
            Shape::Repository,
            Read::Repository,
            legacy::REPOSITORY_FIELDS,
            Some(vec!["repo", "view", "--json"]),
        ),
        (
            Shape::Runs,
            Read::Runs {
                filter: RunFilter::default(),
                page: Page::first(20),
            },
            legacy::RUN_LIST_FIELDS,
            Some(vec!["run", "list", "--limit", "20", "--json"]),
        ),
        (
            Shape::Run,
            Read::Run {
                id: ItemId::new(91),
                attempt: None,
            },
            legacy::RUN_VIEW_FIELDS,
            Some(vec!["run", "view", "91", "--json"]),
        ),
    ];
    for (shape, read, expected, command) in cases {
        let (value, _) = legacy::encode(shape, &parity.read(read.clone()));
        let item = if value.is_array() { &value[0] } else { &value };
        assert_eq!(keys(item), fields(expected), "{shape:?} {read:?}");
        let Some(mut command) = command else {
            continue;
        };
        let joined = expected.join(",");
        command.push(&joined);
        let old = parity.old_json(&command);
        let old_item = if old.is_array() { &old[0] } else { &old };
        // The old fixture printed whole entries for views; gh itself printed only the fields.
        let old_item = Value::Object(
            old_item
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| expected.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        // `comments` rows: gh printed whole comment objects but view.rs only counts them, so
        // the adapter emits one empty object per comment; the count is the contract.
        let mut old_item = old_item;
        if let (Shape::List(_), Some(old_comments), Some(new_comments)) = (
            shape,
            old_item["comments"].as_array(),
            item["comments"].as_array(),
        ) {
            assert_eq!(old_comments.len(), new_comments.len(), "{shape:?} comments");
            old_item.as_object_mut().unwrap().remove("comments");
        }
        // List rows likewise feed only `check_summary`, so the adapter folds the rollup to one
        // `{conclusion}` per check; the multiset of check words is the contract.
        if let (Shape::List(_), Some(old_checks), Some(new_checks)) = (
            shape,
            old_item["statusCheckRollup"].as_array(),
            item["statusCheckRollup"].as_array(),
        ) {
            assert_eq!(
                check_words(old_checks),
                check_words(new_checks),
                "{shape:?} checks"
            );
            old_item
                .as_object_mut()
                .unwrap()
                .remove("statusCheckRollup");
        }
        same_structure(&old_item, item, &format!("{shape:?}"));
    }
}

#[test]
fn rest_and_graphql_page_shapes_match_the_old_gh_api_output() {
    let parity = Parity::new();
    let (timeline, _) = legacy::encode(
        Shape::Timeline,
        &parity.read(Read::Timeline {
            kind: ItemKind::PullRequest,
            number: 7,
        }),
    );
    let old = parity.old_json(&[
        "api",
        "repos/example/demo/issues/7/timeline",
        "--paginate",
        "--slurp",
    ]);
    same_structure(&old, &timeline, "timeline");
    assert_eq!(
        timeline[0].as_array().unwrap().len(),
        old[0].as_array().unwrap().len()
    );

    let (threads, _) = legacy::encode(Shape::Threads, &parity.read(Read::Threads(9)));
    let old = parity.old_json(&[
        "api",
        "graphql",
        "--paginate",
        "--slurp",
        "-f",
        "query=query($endCursor:String){repository{pullRequest{reviewThreads(first:100,after:$endCursor){nodes{id}}}}}",
        "-F",
        "owner=example",
        "-F",
        "name=demo",
        "-F",
        "number=9",
    ]);
    let flatten = |pages: &Value| {
        pages
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|page| {
                page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
    };
    let (old_threads, new_threads) = (flatten(&old), flatten(&threads));
    assert_eq!(old_threads.len(), new_threads.len());
    same_structure(
        &Value::Array(old_threads),
        &Value::Array(new_threads),
        "threads",
    );

    let (viewed, _) = legacy::encode(Shape::Viewed, &parity.read(Read::Viewed(9)));
    let old = parity.old_json(&[
        "api",
        "graphql",
        "-f",
        "query=query{repository{pullRequest{files(first:100){nodes{path viewerViewedState}}}}}",
    ]);
    same_structure(&old, &viewed, "viewed");

    let (workflows, _) = legacy::encode(Shape::Workflows, &parity.read(Read::Workflows));
    let old = parity.old_json(&["api", "repos/example/demo/actions/workflows"]);
    same_structure(&old, &workflows, "workflows");
}

#[test]
fn text_shapes_match_the_old_gh_output() {
    let parity = Parity::new();
    let (_, diff) = legacy::encode(Shape::Text, &parity.read(Read::Diff(9)));
    assert_eq!(diff.trim_end(), parity.old(&["pr", "diff", "9"]).trim_end());
    // `gh run view --log` lines are `job<TAB>step<TAB>line`; the REST log has only the line.
    let (_, log) = legacy::encode(Shape::Text, &parity.read(Read::JobLog(ItemId::new(42))));
    let old = parity.old(&["run", "view", "--job", "42", "--log"]);
    let columns = |text: &str| {
        // The old fixture `print`ed the log, adding one empty line at the end.
        text.lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let parts = line.splitn(3, '\t').collect::<Vec<_>>();
                assert_eq!(parts.len(), 3, "{line:?}");
                parts[2].to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(columns(&log), columns(&old));
}
