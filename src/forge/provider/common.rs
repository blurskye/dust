//! What more than one provider reads the same way: JSON fields, an Actions run's duration and
//! actor, REST paths and `Link`-following pages. Readers whose forge spells a field its own way
//! (GitLab's string iids, Gitea's zero dates) keep their own version beside these.

use super::ForgeContext;
use crate::forge::model::ItemId;
use crate::forge::model::Timestamp;
use crate::forge::request::ForgeError;
use crate::forge::transport::ApiRequest;
use crate::forge::transport::Target;
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

// ------------------------------------------------------------------------------ JSON fields

pub(super) fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// A string field, `None` when it is missing or empty.
pub(super) fn optional(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

pub(super) fn count(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or_default()
}

/// An id the forge sends as a string or a number.
pub(super) fn id_of(value: &Value, key: &str) -> ItemId {
    match &value[key] {
        Value::String(text) => ItemId::new(text),
        Value::Number(number) => ItemId::new(number),
        _ => ItemId::default(),
    }
}

pub(super) fn timestamp(value: &Value, key: &str) -> Timestamp {
    Timestamp::new(text(value, key))
}

pub(super) fn optional_time(value: &Value, key: &str) -> Option<Timestamp> {
    optional(value, key).map(Timestamp::new)
}

/// GitHub's `sort:` orders a forge has no parameter for, applied to the rows it returned
/// (`created-asc`, `comments-desc`, …). Unknown orders leave the forge's order.
pub(super) fn sort_rows(rows: &mut [crate::forge::model::ItemRow], order: &str) {
    let epoch = |stamp: Option<&Timestamp>| {
        stamp
            .and_then(|stamp| crate::forge::records::epoch(stamp.as_str()))
            .unwrap_or(0)
    };
    match order {
        "created-desc" => rows.sort_by_key(|row| std::cmp::Reverse(epoch(row.created.as_ref()))),
        "created-asc" => rows.sort_by_key(|row| epoch(row.created.as_ref())),
        "updated-desc" => rows.sort_by_key(|row| std::cmp::Reverse(epoch(Some(&row.updated)))),
        "updated-asc" => rows.sort_by_key(|row| epoch(Some(&row.updated))),
        "comments-desc" => rows.sort_by_key(|row| std::cmp::Reverse(row.comments)),
        "comments-asc" => rows.sort_by_key(|row| row.comments),
        _ => {}
    }
}

/// When a comment was edited: its update stamp, when that is later than its creation.
pub(super) fn edited(value: &Value, created: &str, updated: &str) -> Option<Timestamp> {
    let at = |key: &str| crate::forge::records::epoch(value[key].as_str()?);
    (at(updated)? > at(created)?).then(|| timestamp(value, updated))
}

// ------------------------------------------------------------------------------ people

/// An app or bot account: GitHub says so (`type`/`__typename` `Bot`), GitLab may (`bot`),
/// Gitea and Forgejo post Actions comments as user -2, and bot logins follow fixed patterns
/// (`name[bot]`, GitLab's `project_<id>_bot_<hash>` / `group_<id>_bot_<hash>` token users).
pub(super) fn is_bot(user: &Value) -> bool {
    if user["type"] == "Bot" || user["__typename"] == "Bot" || user["bot"] == true {
        return true;
    }
    if user["id"].as_i64() == Some(-2) {
        return true;
    }
    let login = user["login"]
        .as_str()
        .or_else(|| user["username"].as_str())
        .unwrap_or_default();
    // GitLab and Gitea name no bot in a note's or comment's author: an integration's
    // service account (`coderabbit-bot`, `renovate_bot`) says so in its login.
    login.ends_with("[bot]")
        || login.ends_with("-bot")
        || login.ends_with("_bot")
        || ["project_", "group_"].iter().any(|prefix| {
            login.strip_prefix(prefix).is_some_and(|rest| {
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                digits > 0 && rest[digits..].starts_with("_bot")
            })
        })
}

// ------------------------------------------------------------------------------ Actions runs

/// Seconds between two forge stamps, when both parse and run forward.
pub(super) fn span(value: &Value, from: &str, to: &str) -> Option<std::time::Duration> {
    let from = crate::forge::records::epoch(value[from].as_str()?)?;
    let to = crate::forge::records::epoch(value[to].as_str()?)?;
    (to >= from).then(|| std::time::Duration::from_secs((to - from) as u64))
}

/// The login a run's `triggering_actor` / `trigger_actor` / `actor` / `trigger_user` names.
pub(super) fn run_actor(value: &Value) -> String {
    ["triggering_actor", "trigger_actor", "actor", "trigger_user"]
        .iter()
        .find_map(|key| {
            value[key]["login"]
                .as_str()
                .filter(|login| !login.is_empty())
        })
        .unwrap_or_default()
        .to_owned()
}

// ------------------------------------------------------------------------------ requests

/// One percent-encoded path segment (label and workflow names may hold spaces or slashes;
/// GitLab's project id is the whole `group/sub/project` path in one segment).
pub(super) fn path_segment(text: &str) -> String {
    let mut escaped = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

/// `repos/<owner>/<name>[/<tail>]`, as GitHub and Gitea address a repository.
pub(super) fn repos_path(ctx: &ForgeContext, tail: &str) -> Result<String, ForgeError> {
    let repo = ctx.repo()?;
    Ok(if tail.is_empty() {
        format!("repos/{}/{}", repo.owner, repo.name)
    } else {
        format!("repos/{}/{}/{tail}", repo.owner, repo.name)
    })
}

/// A `Link` URL as a path under the API base when it is one (passthrough needs paths).
pub(super) fn link_target(ctx: &ForgeContext, url: &str) -> Target {
    match url.strip_prefix(&format!("{}/", ctx.endpoints.api_base)) {
        Some(path) => Target::Rest(path.to_owned()),
        None => Target::Absolute(url.to_owned()),
    }
}

/// Up to `limit` REST pages following `Link: rel="next"`; a page that is not a list counts as
/// one item.
pub(super) fn link_pages(
    ctx: &ForgeContext,
    first: ApiRequest,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<Value>, ForgeError> {
    let mut request = first;
    let mut all = Vec::new();
    for _ in 0..limit {
        if cancel.load(Ordering::Acquire) {
            return Err(ForgeError::Cancelled);
        }
        let response = ctx.ok(&request, cancel)?;
        match response.json()? {
            Value::Array(items) => all.extend(items),
            other => all.push(other),
        }
        let Some(next) = response.next_link() else {
            break;
        };
        request = ApiRequest {
            target: link_target(ctx, &next),
            query: Vec::new(),
            ..request
        };
    }
    Ok(all)
}
