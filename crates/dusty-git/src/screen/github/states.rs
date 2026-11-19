//! The forge tabs' unhappy paths in plain words: what happened, in one
//! sentence that names the host, and the one key that moves on. Every failed read reaches the
//! screen as `ForgeError`'s Display; `explain` turns it into a headline and a hint, so no screen
//! prints an HTTP status echo, a server's JSON or a token, and every forge names its own way to
//! sign in (`gh` for GitHub, `glab` for GitLab, the token settings page for Gitea and Forgejo).

use super::{Key, Workspace};
use crate::forge::host_state::HostState;
use crate::forge::provider::ProviderKind;
use std::time::Duration;

/// Opens the accounts popover, which says how to sign in to each host. Free on every forge
/// screen and in every preset (`a` is assignees in a reader and attempts in a run).
pub(super) const SIGN_IN_KEY: char = '@';

/// One failure in words: the headline (what happened) and the hint (the next key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Words {
    pub(super) headline: String,
    pub(super) hint: String,
}

impl Words {
    fn new(headline: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            headline: headline.into(),
            hint: hint.into(),
        }
    }

    /// Headline and hint on one row.
    pub(super) fn line(&self) -> String {
        if self.hint.is_empty() {
            self.headline.clone()
        } else {
            format!("{} · {}", self.headline, self.hint)
        }
    }
}

/// How to sign in to this forge, in its own tool or page.
pub(super) fn sign_in_how(kind: Option<ProviderKind>, host: &str) -> String {
    let host = if host.is_empty() { "the forge" } else { host };
    match kind {
        Some(ProviderKind::GitHub) if host == "github.com" => "run gh auth login".into(),
        Some(ProviderKind::GitHub) => format!("run gh auth login --hostname {host}"),
        Some(ProviderKind::GitLab) if host == "gitlab.com" => "run glab auth login".into(),
        Some(ProviderKind::GitLab) => format!("run glab auth login --hostname {host}"),
        Some(ProviderKind::Gitea | ProviderKind::Forgejo) => {
            format!("make a token at https://{host}/user/settings/applications")
        }
        None => "@ shows how".into(),
    }
}

/// The CLI this forge signs in with when it isn't installed (`gh`, `glab`), so a sign-in
/// hint never says "run gh auth login" to someone without gh. Gitea and Forgejo sign in with a
/// token; the tutor's practice forge runs no CLI by design. Looked up at most every 10 s.
pub(super) fn missing_cli(kind: Option<ProviderKind>) -> Option<&'static str> {
    use crate::forge::transport::Cli;
    let cli = match kind {
        Some(ProviderKind::GitHub) => Cli::Gh,
        Some(ProviderKind::GitLab) => Cli::Glab,
        _ => return None,
    };
    (!installed(cli)).then_some(cli.name())
}

/// Whether `cli` is on the PATH (or named by its override); the tutor's sandbox counts as
/// installed, since it runs no CLI by design.
pub(super) fn installed(cli: crate::forge::transport::Cli) -> bool {
    crate::forge::sandbox::active() || probe(cli)
}

/// Unit tests decide, so they never depend on what the machine has installed.
#[cfg(any(test, feature = "test-support"))]
fn probe(_: crate::forge::transport::Cli) -> bool {
    !TEST_CLI_MISSING.with(std::cell::Cell::get)
}

#[cfg(not(any(test, feature = "test-support")))]
fn probe(cli: crate::forge::transport::Cli) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    static SEEN: OnceLock<Mutex<HashMap<&'static str, (Instant, bool)>>> = OnceLock::new();
    let Ok(mut seen) = SEEN.get_or_init(Default::default).lock() else {
        return true;
    };
    let fresh = seen
        .get(cli.name())
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(10))
        .map(|(_, found)| *found);
    fresh.unwrap_or_else(|| {
        let found = cli.executable().is_some();
        seen.insert(cli.name(), (Instant::now(), found));
        found
    })
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Whether this test thread's machine "has" no forge CLI.
    pub(super) static TEST_CLI_MISSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `gh isn't installed` · `install it or sign in with a token (@)`.
pub(super) fn cli_missing_words(cli: &str) -> Words {
    Words::new(
        format!("{cli} isn't installed"),
        format!("install it or sign in with a token ({SIGN_IN_KEY})"),
    )
}

/// The variable Dusty Git reads a token from for this forge (`DUSTY_FORGE_TOKEN` works for
/// every forge).
pub(super) fn token_variable(kind: Option<ProviderKind>) -> &'static str {
    match kind {
        Some(ProviderKind::GitHub) => "GH_TOKEN",
        Some(ProviderKind::GitLab) => "GITLAB_TOKEN",
        Some(ProviderKind::Gitea) => "GITEA_TOKEN",
        Some(ProviderKind::Forgejo) => "FORGEJO_TOKEN",
        None => "DUSTY_FORGE_TOKEN",
    }
}

/// Where a token gets more scopes on this forge.
fn scope_fix(kind: Option<ProviderKind>, host: &str, scopes: &str) -> String {
    match kind {
        Some(ProviderKind::GitHub) => {
            let scopes = scopes.replace(", ", ",");
            if host == "github.com" {
                format!("run gh auth refresh -s {scopes}")
            } else {
                format!("run gh auth refresh -h {host} -s {scopes}")
            }
        }
        Some(ProviderKind::GitLab) => {
            format!("add it at https://{host}/-/user_settings/personal_access_tokens")
        }
        Some(ProviderKind::Gitea | ProviderKind::Forgejo) => {
            format!("add it at https://{host}/user/settings/applications")
        }
        None => "@ shows how".into(),
    }
}

/// `@ sign in · run gh auth login`.
pub(super) fn sign_in_hint(kind: Option<ProviderKind>, host: &str) -> String {
    format!("{SIGN_IN_KEY} sign in · {}", sign_in_how(kind, host))
}

/// `45s`, `12m`, `3h`, `2d`.
pub(super) fn span(duration: Duration) -> String {
    crate::forge::host_state::age(duration)
}

/// A failed read's error (`ForgeError`'s Display, perhaps wrapped by a notice) in words.
pub(super) fn explain(error: &str, kind: Option<ProviderKind>, host: &str) -> Words {
    let host = if host.is_empty() { "the forge" } else { host };
    let error = error
        .trim()
        .trim_start_matches("Refresh failed: ")
        .split(" — showing cached content")
        .next()
        .unwrap_or_default()
        .trim();
    let (head, detail) = error.split_once(" · ").unwrap_or((error, ""));
    let sign_in = || sign_in_hint(kind, host);
    if head.starts_with("Not signed in") {
        if crate::forge::credential::refused(host) {
            return Words::new(
                format!("{host} refused your token · it expired or was revoked"),
                sign_in(),
            );
        }
        if let Some(cli) = missing_cli(kind) {
            return cli_missing_words(cli);
        }
        return Words::new(format!("Not signed in to {host}"), sign_in());
    }
    if head.ends_with("requires sign-in") {
        return Words::new(format!("{host} shows nothing until you sign in"), sign_in());
    }
    if let Some(what) = head.strip_prefix("Sign in to ") {
        if crate::forge::credential::refused(host) {
            return Words::new(
                format!("{host} refused your token · it expired or was revoked"),
                sign_in(),
            );
        }
        return Words::new(format!("Sign in to {what}"), sign_in());
    }
    if head == "No permission" {
        if let Some(scopes) = detail.strip_prefix("needs scope ") {
            let plural = if scopes.contains(',') {
                "scopes"
            } else {
                "scope"
            };
            return Words::new(
                format!("Your token for {host} needs the {scopes} {plural}"),
                format!("{} · then R", scope_fix(kind, host, scopes)),
            );
        }
        return Words::new(
            format!("You don't have access to this on {host}"),
            format!("{SIGN_IN_KEY} accounts · R retry"),
        );
    }
    if head == "Rate limited" {
        let wait = detail
            .strip_prefix("retry in ")
            .and_then(|seconds| seconds.trim_end_matches('s').parse::<u64>().ok())
            .map(|seconds| span(Duration::from_secs(seconds)));
        return Words::new(
            match wait {
                Some(wait) => format!("Rate limited by {host} · resets in {wait}"),
                None => format!("Rate limited by {host}"),
            },
            "R retry once it resets",
        );
    }
    if head == "Network error" {
        let status: String = detail.chars().take_while(char::is_ascii_digit).collect();
        if status.len() == 3 && status.starts_with('5') {
            return Words::new(format!("{host} is having problems"), "R retry");
        }
        return Words::new(
            format!("Can't reach {host} · you may be offline"),
            "R retry",
        );
    }
    if head.starts_with("Timed out") {
        return Words::new(format!("{host} is taking too long to answer"), "R retry");
    }
    if head == "Unexpected response" {
        return Words::new(
            format!("{host} sent an answer Dusty Git can't read"),
            "R retry",
        );
    }
    if head == "Not found" {
        return Words::new(
            format!("Not found on {host} · deleted, moved or private"),
            format!("R retry · {SIGN_IN_KEY} accounts"),
        );
    }
    if head.starts_with("no Git remote points at a forge") {
        return Words::new(
            "No GitHub, GitLab, Gitea or Forgejo remote in this repository",
            "add one with git remote add origin <url> · then R",
        );
    }
    if (head == "Unsupported here" && detail.starts_with("Unknown site "))
        || head.starts_with("Unknown site ")
    {
        let text = if head == "Unsupported here" {
            detail
        } else {
            error
        };
        let unknown = text
            .trim_start_matches("Unknown site ")
            .split(" · ")
            .next()
            .unwrap_or(host);
        return Words::new(format!("Unknown site {unknown}"), "@ accounts");
    }
    if let Some(cli) = head.strip_suffix(" is not installed") {
        return cli_missing_words(cli.trim());
    }
    Words::new(plain(error), "R retry")
}

/// Whatever is left of an unknown error once anything raw is cut away: a server's JSON, a
/// stack, a status echo.
fn plain(error: &str) -> String {
    let cut = ["{", "Traceback", "\n", " at src/", "panicked"]
        .iter()
        .filter_map(|marker| error.find(marker))
        .min()
        .unwrap_or(error.len());
    let text = error[..cut].trim().trim_end_matches([':', '·', '—']).trim();
    if text.is_empty() {
        "Couldn't read this from the forge".into()
    } else {
        text.to_owned()
    }
}

/// The host a repository's forge remote names, read once per repository: before the first
/// successful read the workspace has no host yet, and a refusal still names the forge.
fn remote_host(root: &std::path::Path) -> Option<String> {
    static HOSTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Option<String>>>,
    > = std::sync::OnceLock::new();
    let mut hosts = HOSTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    hosts
        .entry(root.to_path_buf())
        .or_insert_with(|| {
            crate::forge::detect::choose_remote(root)
                .ok()
                .map(|remote| remote.host.to_string())
        })
        .clone()
}

impl Workspace {
    /// A failed read's error, in words, for this workspace's host and forge (the remote's
    /// host until the repository has been read).
    pub(super) fn explain(&self, error: &str) -> Words {
        if self.host.is_empty()
            && let Some(host) = remote_host(&self.root)
        {
            let key = crate::forge::transport::HostKey::new(&host);
            let kind = crate::forge::host_state::account(&key)
                .and_then(|account| account.kind)
                .or_else(|| crate::forge::detect::known_host(&key));
            return explain(error, kind, &host);
        }
        let words = explain(error, self.forge_kind(), &self.host);
        // Signed out, a 404 inside a repository the forge did show is the forge keeping that
        // part for signed-in users (gitea.com's pull list of an archived repository), not a
        // missing repository.
        if words.headline.starts_with("Not found")
            && self.read_only()
            && self.value(&Key::Repository).is_some()
        {
            return Words::new(
                format!("{} shows this only when signed in", self.host),
                format!("{SIGN_IN_KEY} sign in"),
            );
        }
        words
    }

    /// The list shows less than every open item: a typed query, a section's or a state.
    pub(super) fn filtered(&self) -> bool {
        let at = self.page.saturating_sub(1).min(1);
        !self.queries[at].trim().is_empty() || self.states[at] != "open"
    }

    /// Why a cached entry was not refreshed, for the freshness chip: `offline`, `rate limited`,
    /// `saving requests` (signed out, background reads keep half the quota for what is
    /// opened) or `not refreshed` (a server error).
    pub(super) fn stale_word(&self) -> &'static str {
        let state = self
            .accounts
            .state
            .as_ref()
            .map(|(_, state)| state.clone())
            .or_else(|| self.host_state());
        match state {
            Some(HostState::RateLimited { .. }) => "rate limited",
            Some(HostState::Anonymous { .. }) => "saving requests",
            Some(HostState::Unreachable { error })
                if error
                    .trim_start_matches("Network error · ")
                    .starts_with('5') =>
            {
                "not refreshed"
            }
            _ => "offline",
        }
    }

    /// How old the list on show is, for "showing data from 12m ago".
    pub(super) fn shown_age(&self) -> Option<Duration> {
        let meta = self.fresh.meta.get(&self.list_key())?;
        meta.fetched.elapsed().ok()
    }

    /// The notice after a refresh of something already on screen failed: the reason, how old
    /// what is shown is, and the one key.
    pub(super) fn stale_notice(&self, error: &str, age: Option<Duration>) -> String {
        let words = self.explain(error);
        let shown = match age {
            Some(age) if age.as_secs() >= 45 => format!("showing data from {} ago", span(age)),
            _ => "showing what was loaded".into(),
        };
        let headline = if words.headline.starts_with("Can't reach") {
            "Offline".to_owned()
        } else {
            words.headline
        };
        format!("{headline} · {shown} · {}", words.hint)
    }
}

/// The one-row list header notice for a host-level state, or `None` to use the registry's own
/// (`Anonymous · read-only · …`, CLI missing). `cached` is how old the list on show is; `width`
/// drops the tail parts whole rather than cutting one.
pub(super) fn host_notice(
    state: &HostState,
    host: &str,
    cached: Option<Duration>,
) -> Option<(Vec<String>, crate::forge::host_state::Tone)> {
    use crate::forge::host_state::Tone;
    let host = if host.is_empty() { "the forge" } else { host };
    let shown = cached.map(|age| {
        if age.as_secs() < 45 {
            "showing what was loaded".to_owned()
        } else {
            format!("showing data from {} ago", span(age))
        }
    });
    let with_shown = |mut parts: Vec<String>| {
        if let Some(shown) = &shown {
            parts.insert(1, shown.clone());
        }
        parts
    };
    Some(match state {
        HostState::SignedOut { cli, .. } => {
            let refused = crate::forge::credential::refused(host);
            if !refused && !installed(*cli) {
                let words = cli_missing_words(cli.name());
                return Some((
                    with_shown(vec![format!("✕ {}", words.headline), words.hint]),
                    Tone::Error,
                ));
            }
            let headline = if refused {
                format!("✕ {host} refused your token")
            } else {
                format!("✕ Not signed in to {host}")
            };
            (
                with_shown(vec![headline, format!("{SIGN_IN_KEY} sign in")]),
                Tone::Error,
            )
        }
        HostState::CliMissing { cli, .. } => {
            let words = cli_missing_words(cli.name());
            (
                with_shown(vec![format!("✕ {}", words.headline), words.hint]),
                Tone::Error,
            )
        }
        HostState::SignInRequired { .. } => (
            with_shown(vec![
                format!("✕ {host} needs a sign-in"),
                format!("{SIGN_IN_KEY} sign in"),
            ]),
            Tone::Error,
        ),
        HostState::RateLimited { until } => {
            let left = until
                .saturating_duration_since(std::time::Instant::now())
                .max(Duration::from_secs(1));
            (
                with_shown(vec![
                    format!("⚠ Rate limited · resets in {}", span(left)),
                    "R retry then".into(),
                ]),
                Tone::Warning,
            )
        }
        HostState::Unreachable { error } => {
            let status: String = error
                .trim_start_matches("Network error · ")
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            // With the last copy on screen the trouble is a warning, not an error: the reader
            // still has everything it had.
            let (mark, tone) = if cached.is_some() {
                ("⚠", Tone::Warning)
            } else {
                ("✕", Tone::Error)
            };
            let headline = if status.len() == 3 && status.starts_with('5') {
                format!("{mark} {host} is having problems")
            } else if cached.is_some() {
                format!("{mark} Offline")
            } else {
                format!("{mark} Can't reach {host}")
            };
            (with_shown(vec![headline, "R retry".into()]), tone)
        }
        HostState::UnknownProvider => (
            vec![
                format!("⚠ Unknown site {host}"),
                format!("{SIGN_IN_KEY} accounts"),
            ],
            Tone::Warning,
        ),
        _ => return None,
    })
}

/// Parts joined with ` · `, dropping whole parts from the end (never the first) to fit.
pub(super) fn fit(parts: &[String], width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut keep = parts.len();
    loop {
        let text = parts[..keep].join(" · ");
        if keep <= 1 || text.width() <= width {
            return text;
        }
        keep -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: [&str; 6] = [
        "{",
        "Network error",
        "Bad credentials",
        "401",
        "Error(",
        "ghp_",
    ];

    fn words(error: &str, kind: ProviderKind, host: &str) -> String {
        let line = explain(error, Some(kind), host).line();
        for raw in RAW {
            assert!(!line.contains(raw), "{line:?} shows {raw:?}");
        }
        line
    }

    #[test]
    fn each_forge_names_its_own_way_to_sign_in() {
        // Hosts no other test signs in to: a refused token is remembered per process.
        let error = "Not signed in · run gh auth login";
        assert_eq!(
            words(error, ProviderKind::GitHub, "ghe.states.test"),
            "Not signed in to ghe.states.test · @ sign in · run gh auth login --hostname ghe.states.test"
        );
        assert!(
            words(error, ProviderKind::GitLab, "gitlab.example")
                .ends_with("run glab auth login --hostname gitlab.example")
        );
        assert!(
            words(error, ProviderKind::Gitea, "gitea.com")
                .ends_with("make a token at https://gitea.com/user/settings/applications")
        );
        assert!(
            words(error, ProviderKind::Forgejo, "codeberg.org")
                .ends_with("make a token at https://codeberg.org/user/settings/applications")
        );
    }

    /// Without its CLI, a forge never says "run gh auth login": it names the missing tool and
    /// the token way in. Token forges read the same either way.
    #[test]
    fn a_missing_cli_reads_differently_from_signed_out() {
        TEST_CLI_MISSING.with(|missing| missing.set(true));
        let error = "Not signed in · run gh auth login";
        assert_eq!(
            words(error, ProviderKind::GitHub, "ghe.missing.test"),
            "gh isn't installed · install it or sign in with a token (@)"
        );
        assert_eq!(
            words(error, ProviderKind::GitLab, "gitlab.missing.test"),
            "glab isn't installed · install it or sign in with a token (@)"
        );
        assert_eq!(
            words(
                "glab is not installed",
                ProviderKind::GitLab,
                "gl.missing.test"
            ),
            "glab isn't installed · install it or sign in with a token (@)"
        );
        assert!(
            words(error, ProviderKind::Gitea, "gitea.missing.test")
                .ends_with("make a token at https://gitea.missing.test/user/settings/applications")
        );
        let signed_out = HostState::SignedOut {
            cli: crate::forge::transport::Cli::Gh,
            hint: String::new(),
        };
        let (parts, _) = host_notice(&signed_out, "github.com", None).unwrap();
        assert_eq!(
            parts,
            [
                "✕ gh isn't installed",
                "install it or sign in with a token (@)"
            ]
        );
        TEST_CLI_MISSING.with(|missing| missing.set(false));
        let (parts, _) = host_notice(&signed_out, "github.com", None).unwrap();
        assert_eq!(parts[0], "✕ Not signed in to github.com");
    }

    #[test]
    fn a_missing_scope_names_the_scopes_and_where_to_add_them() {
        assert_eq!(
            words(
                "No permission · needs scope repo, workflow",
                ProviderKind::GitHub,
                "github.com"
            ),
            "Your token for github.com needs the repo, workflow scopes · run gh auth refresh -s repo,workflow · then R"
        );
        assert!(words("No permission · needs scope api", ProviderKind::GitLab, "gitlab.com")
            .contains("needs the api scope · add it at https://gitlab.com/-/user_settings/personal_access_tokens"));
    }

    #[test]
    fn network_rate_and_server_errors_read_as_sentences() {
        assert_eq!(
            words(
                "Rate limited · retry in 720s",
                ProviderKind::GitHub,
                "github.com"
            ),
            "Rate limited by github.com · resets in 12m · R retry once it resets"
        );
        assert_eq!(
            words(
                "Network error · 502 Bad Gateway",
                ProviderKind::GitLab,
                "gitlab.com"
            ),
            "gitlab.com is having problems · R retry"
        );
        assert_eq!(
            explain(
                "Network error · 502 Bad Gateway",
                Some(ProviderKind::GitLab),
                "gitlab.com"
            ),
            Words::new("gitlab.com is having problems", "R retry")
        );
        assert_eq!(
            words(
                "Refresh failed: Network error · error sending request — showing cached content",
                ProviderKind::Gitea,
                "gitea.com"
            ),
            "Can't reach gitea.com · you may be offline · R retry"
        );
        assert_eq!(
            words(
                "Unexpected response · {\"message\":\"x\"}",
                ProviderKind::GitHub,
                "github.com"
            ),
            "github.com sent an answer Dusty Git can't read · R retry"
        );
        assert_eq!(plain("boom {\"message\":\"x\"}"), "boom");
    }

    #[test]
    fn unknown_site_notice_keeps_configuration_in_accounts() {
        let error = crate::forge::detect::unknown_provider(&crate::forge::transport::HostKey::new(
            "unknown.test",
        ));
        assert_eq!(
            explain(&error.to_string(), None, "").line(),
            "Unknown site unknown.test · @ accounts"
        );
    }

    #[test]
    fn notices_drop_whole_parts_to_fit() {
        let parts = [
            "✕ Offline".to_owned(),
            "showing data from 12m ago".into(),
            "R retry".into(),
        ];
        assert_eq!(
            fit(&parts, 80),
            "✕ Offline · showing data from 12m ago · R retry"
        );
        assert_eq!(fit(&parts, 40), "✕ Offline · showing data from 12m ago");
        assert_eq!(fit(&parts, 5), "✕ Offline");
    }
}
