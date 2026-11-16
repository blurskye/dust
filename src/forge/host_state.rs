//! What Dusty knows about each forge host it talked to (FORGE.md §2.5, DESIGN §3.45 X-OFFLINE /
//! X-PROVIDER): signed in and how, browsed anonymously (read-only, with the quota left),
//! signed out, an instance that requires sign-in, CLI missing or too old, rate limited,
//! unreachable, unknown provider. The forge worker records observations here; the forge tabs
//! read them to paint the list-header notice and the accounts popover (`a`). Copy is fixed per
//! state and never includes raw stderr; nothing here holds a token.

use crate::forge::credential::CredentialSource;
use crate::forge::provider::ProviderKind;
use crate::forge::request::ForgeError;
use crate::forge::transport::Cli;
use crate::forge::transport::Endpoints;
use crate::forge::transport::HostKey;
use crate::forge::transport::TransportKind;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostState {
    Ready {
        transport: TransportKind,
        account: Option<String>,
    },
    /// No credential and no signed-in CLI: public repositories are read over direct HTTP and
    /// every write is refused (`Capabilities::read_only`). The quota is the last one the forge
    /// reported for anonymous requests (github.com: 60/hour per address); `hint` is the
    /// sign-in command that lifts both limits.
    Anonymous {
        remaining: Option<u64>,
        limit: Option<u64>,
        reset: Option<Instant>,
        hint: String,
    },
    /// The instance refuses API calls without a sign-in (Forgejo/Gitea `REQUIRE_SIGNIN_VIEW`),
    /// so not even public repositories can be read. `provider` is the guess from the refusal;
    /// `hints` are every way to sign in, the CLI login first.
    SignInRequired {
        provider: Option<ProviderKind>,
        hints: Vec<String>,
    },
    SignedOut {
        cli: Cli,
        hint: String,
    },
    CliMissing {
        cli: Cli,
        install_hint: String,
    },
    CliTooOld {
        cli: Cli,
        found: String,
        need: &'static str,
    },
    RateLimited {
        until: Instant,
    },
    Unreachable {
        error: String,
    },
    UnknownProvider,
    /// A provider Dusty recognised but cannot serve yet, or another host-wide refusal.
    Unknown(String),
}

/// How a line should be painted; the UI maps it onto its own styles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Normal,
    Muted,
    Warning,
    Error,
}

pub fn cli_for(kind: ProviderKind) -> Cli {
    match kind {
        ProviderKind::GitHub => Cli::Gh,
        ProviderKind::GitLab => Cli::Glab,
        ProviderKind::Gitea | ProviderKind::Forgejo => Cli::Tea,
    }
}

pub fn install_hint(cli: Cli) -> &'static str {
    match cli {
        Cli::Gh => "install GitHub CLI: https://cli.github.com",
        Cli::Glab => "install glab: https://gitlab.com/gitlab-org/cli",
        Cli::Tea => "install tea: https://gitea.com/gitea/tea",
    }
}

/// `45s`, `12m`, `3h`, `2d`.
pub fn age(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=172_799 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

impl HostState {
    /// The host-wide state an error implies; `None` for errors about one item (not found,
    /// conflict, permission on one repo), which stay with that item.
    pub fn from_error(kind: Option<ProviderKind>, error: &ForgeError) -> Option<Self> {
        let cli = cli_for(kind.unwrap_or(ProviderKind::GitHub));
        Some(match error {
            ForgeError::NotAuthenticated { hint, .. } => HostState::SignedOut {
                cli,
                hint: hint.clone(),
            },
            ForgeError::SignInRequired { hints, .. } => HostState::SignInRequired {
                provider: kind,
                hints: hints.clone(),
            },
            ForgeError::CliMissing(cli) => HostState::CliMissing {
                cli: *cli,
                install_hint: install_hint(*cli).into(),
            },
            ForgeError::CliTooOld { cli, found, needed } => HostState::CliTooOld {
                cli: *cli,
                found: found.clone(),
                need: needed,
            },
            ForgeError::RateLimited { until } => HostState::RateLimited { until: *until },
            ForgeError::Network(message) => HostState::Unreachable {
                error: crate::forge::cli::redact(message),
            },
            ForgeError::Timeout => HostState::Unreachable {
                error: "timed out".into(),
            },
            ForgeError::Unsupported {
                capability: "provider" | "transport",
                reason,
            } => {
                if reason.starts_with("Unknown site ") {
                    HostState::UnknownProvider
                } else {
                    HostState::Unknown(reason.clone())
                }
            }
            _ => return None,
        })
    }

    /// Reads can be sent (signed in, or anonymously).
    pub fn ready(&self) -> bool {
        matches!(self, HostState::Ready { .. } | HostState::Anonymous { .. })
    }

    /// Reads only: every write capability is `Cap::No("Sign in to …")`.
    pub fn read_only(&self) -> bool {
        matches!(self, HostState::Anonymous { .. })
    }

    /// The header chip for an anonymous host: `Anonymous · read-only · 52/60 requests left`.
    pub fn chip(&self) -> Option<String> {
        let HostState::Anonymous {
            remaining, limit, ..
        } = self
        else {
            return None;
        };
        let mut text = "Anonymous · read-only".to_owned();
        match (remaining, limit) {
            (Some(remaining), Some(limit)) => {
                text.push_str(&format!(" · {remaining}/{limit} requests left"))
            }
            (Some(remaining), None) => text.push_str(&format!(" · {remaining} requests left")),
            _ => {}
        }
        Some(text)
    }

    /// Less than a quarter of the anonymous quota is left.
    fn quota_low(&self) -> bool {
        matches!(
            self,
            HostState::Anonymous {
                remaining: Some(remaining),
                limit: Some(limit),
                ..
            } if remaining.saturating_mul(4) < *limit
        )
    }

    /// The popover's short state word.
    pub fn label(&self) -> &'static str {
        match self {
            HostState::Ready {
                transport: TransportKind::Http { .. },
                ..
            } => "Ready",
            HostState::Ready { .. } => "Ready · slower",
            HostState::Anonymous { .. } => "Anonymous · read-only",
            HostState::SignInRequired { .. } => "Instance requires sign-in",
            HostState::SignedOut { .. } => "Sign in required",
            HostState::CliMissing { .. } => "CLI missing",
            HostState::CliTooOld { .. } => "CLI too old",
            HostState::RateLimited { .. } => "Rate limited",
            HostState::Unreachable { .. } => "Offline",
            HostState::UnknownProvider => "Unknown site",
            HostState::Unknown(_) => "Unavailable",
        }
    }

    /// The one-row list-header notice (§2.5), or `None` when the list needs none. `cached` is
    /// the age of what the list is still showing. The forge screens word the states they act
    /// on themselves (`git::screen::github::states`); the keys here are theirs: `@` signs in
    /// (`a` is assignees and attempts there) and `R` retries.
    pub fn notice(&self, host: &str, cached: Option<Duration>) -> Option<(String, Tone)> {
        let cached_part = |text: &mut String| {
            if let Some(cached) = cached {
                text.push_str(&format!(" · updated {} ago", age(cached)));
            }
        };
        Some(match self {
            HostState::Ready { .. } => return None,
            HostState::Anonymous { reset, hint, .. } => {
                let mut text = self.chip().unwrap_or_default();
                let low = self.quota_low();
                if low && let Some(reset) = reset {
                    let left = reset
                        .saturating_duration_since(Instant::now())
                        .max(Duration::from_secs(1));
                    text.push_str(&format!(" · resets in {}", age(left)));
                }
                text.push_str(&format!(" · sign in: {hint}"));
                (
                    format!("○ {text}"),
                    if low { Tone::Warning } else { Tone::Muted },
                )
            }
            HostState::SignInRequired { .. } => {
                let mut text = format!("✕ {host} requires sign-in");
                cached_part(&mut text);
                text.push_str(" · @ sign in");
                (text, Tone::Error)
            }
            HostState::SignedOut { .. } => {
                let mut text = format!("✕ Sign in required for {host}");
                cached_part(&mut text);
                text.push_str(" · @ sign in");
                (text, Tone::Error)
            }
            HostState::CliMissing { cli, .. } => (
                format!(
                    "✕ {} is not installed · Git operations remain available",
                    cli.name()
                ),
                Tone::Error,
            ),
            HostState::CliTooOld { cli, found, need } => (
                format!(
                    "{} {found} is too old · {need} or newer needed",
                    cli.name()
                ),
                Tone::Warning,
            ),
            HostState::RateLimited { until } => {
                let left = until
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_secs(1));
                let mut text = format!("⚠ Rate limited by {host} · resumes in {}", age(left));
                if cached.is_some() {
                    text.push_str(" · showing earlier results");
                }
                (text, Tone::Warning)
            }
            HostState::Unreachable { .. } => {
                let mut text = format!("✕ Could not reach {host}");
                cached_part(&mut text);
                text.push_str(" · R retry");
                (text, Tone::Error)
            }
            HostState::UnknownProvider => (
                format!("Unknown site {host} · @ accounts"),
                Tone::Warning,
            ),
            HostState::Unknown(reason) => (
                format!("⚠ {reason} · Git operations remain available"),
                Tone::Warning,
            ),
        })
    }
}

/// One row of the accounts popover.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Account {
    pub host: HostKey,
    pub kind: Option<ProviderKind>,
    pub state: HostState,
    pub source: Option<CredentialSource>,
    pub login: Option<String>,
    pub observed: Instant,
}

impl Account {
    pub fn new(host: HostKey, kind: Option<ProviderKind>, state: HostState) -> Self {
        Self {
            host,
            kind,
            state,
            source: None,
            login: None,
            observed: Instant::now(),
        }
    }

    /// `github.com · gh token · maya · direct` / `codeberg.org · tea login "cb" · maya · via tea
    /// (slower)`, then an instruction line for states the user can fix.
    pub fn lines(&self) -> Vec<(String, Tone)> {
        let host = self.host.to_string();
        let mut lines = Vec::new();
        match &self.state {
            HostState::Ready { transport, account } => {
                let source = match (transport, &self.source) {
                    (TransportKind::Http { credential_source }, _) => credential_source.to_string(),
                    (TransportKind::Cli(_), Some(source @ CredentialSource::TeaConfig(_))) => {
                        source.to_string()
                    }
                    (TransportKind::Cli(cli), _) => format!("{} sign-in", cli.name()),
                    (TransportKind::Anonymous, _) => "no sign-in".to_string(),
                };
                let account = account.clone().or_else(|| self.login.clone());
                let mut text = format!("✓ {host} · {source}");
                if let Some(account) = account {
                    text.push_str(&format!(" · {account}"));
                }
                match transport {
                    TransportKind::Http { .. } => text.push_str(" · direct"),
                    TransportKind::Cli(cli) => {
                        text.push_str(&format!(" · via {} (slower)", cli.name()))
                    }
                    TransportKind::Anonymous => text.push_str(" · read-only"),
                }
                lines.push((text, Tone::Normal));
            }
            HostState::Anonymous { hint, .. } => {
                let chip = self.state.chip().unwrap_or_default();
                lines.push((
                    format!("◌ {host} · {}", chip.to_ascii_lowercase()),
                    if self.state.quota_low() {
                        Tone::Warning
                    } else {
                        Tone::Normal
                    },
                ));
                lines.push((
                    format!("  sign in to write: {}", crate::forge::cli::redact(hint)),
                    Tone::Muted,
                ));
            }
            HostState::SignInRequired { hints, .. } => {
                lines.push((
                    format!("○ {host} · {}", self.state.label()),
                    Tone::Warning,
                ));
                for hint in hints {
                    lines.push((
                        format!("  {}", crate::forge::cli::redact(hint)),
                        Tone::Muted,
                    ));
                }
            }
            state => {
                lines.push((format!("○ {host} · {}", state.label()), Tone::Warning));
                let instruction = match state {
                    HostState::SignedOut { hint, .. } => Some(hint.clone()),
                    HostState::CliMissing { install_hint, .. } => Some(install_hint.clone()),
                    HostState::CliTooOld { cli, need, .. } => {
                        Some(format!("update {} to {need} or newer", cli.name()))
                    }
                    HostState::RateLimited { until } => Some(format!(
                        "Background updates paused · back in {}",
                        age(until
                            .saturating_duration_since(Instant::now())
                            .max(Duration::from_secs(1)))
                    )),
                    HostState::Unreachable { .. } => Some(format!("Can't reach {host} · r retry")),
                    HostState::UnknownProvider => {
                        lines.push((
                            "Tell Dusty Git what kind of site this is, in config.toml:".into(),
                            Tone::Muted,
                        ));
                        lines.push((
                            format!("[forge.hosts.\"{host}\"] provider = \"github\"   # or gitlab, gitea, forgejo"),
                            Tone::Muted,
                        ));
                        None
                    }
                    HostState::Unknown(reason) => Some(reason.clone()),
                    HostState::Ready { .. }
                    | HostState::Anonymous { .. }
                    | HostState::SignInRequired { .. } => None,
                };
                if let Some(instruction) = instruction {
                    lines.push((
                        format!("  {}", crate::forge::cli::redact(&instruction)),
                        Tone::Muted,
                    ));
                }
            }
        }
        lines
    }
}

fn registry() -> &'static Mutex<BTreeMap<HostKey, Account>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<HostKey, Account>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn update(host: &HostKey, kind: Option<ProviderKind>, change: impl FnOnce(&mut Account)) {
    let mut registry = registry().lock().unwrap_or_else(|error| error.into_inner());
    let account = registry.entry(host.clone()).or_insert_with(|| {
        Account::new(
            host.clone(),
            kind,
            HostState::Unknown("not contacted yet".into()),
        )
    });
    if kind.is_some() {
        account.kind = kind;
    }
    change(account);
    account.observed = Instant::now();
}

/// The live `GET /user` passed: who is signed in, and with which credential.
pub fn observe_account(endpoints: &Endpoints, source: &CredentialSource, login: Option<String>) {
    update(&endpoints.host, Some(endpoints.kind), |account| {
        account.source = Some(source.clone());
        if login.is_some() {
            account.login = login.clone();
        }
        account.state = HostState::Ready {
            transport: TransportKind::Http {
                credential_source: source.clone(),
            },
            account: login.or_else(|| account.login.clone()),
        };
    });
}

/// A transport was chosen for the host.
pub fn observe_transport(endpoints: &Endpoints, transport: TransportKind) {
    update(&endpoints.host, Some(endpoints.kind), |account| {
        if transport.anonymous() {
            if !matches!(account.state, HostState::Anonymous { .. }) {
                account.source = None;
                account.state = anonymous_state(endpoints);
            }
            return;
        }
        if let TransportKind::Cli(Cli::Tea) = transport
            && account.source.is_none()
        {
            account.source = endpoints_tea_login(endpoints);
        }
        account.state = HostState::Ready {
            account: account.login.clone(),
            transport,
        };
    });
}

/// An anonymous state seeded from what the rate limiter last saw for anonymous requests.
fn anonymous_state(endpoints: &Endpoints) -> HostState {
    let snapshot = crate::forge::ratelimit::global().snapshot(
        crate::forge::ratelimit::Fingerprint::anonymous(&endpoints.api_base),
    );
    let quota = snapshot.map(|snapshot| snapshot.quota).unwrap_or_default();
    HostState::Anonymous {
        remaining: quota.remaining,
        limit: quota.limit,
        reset: None,
        hint: crate::forge::credential::login_hint(endpoints.kind, &endpoints.host),
    }
}

/// An anonymous response's quota headers, for the chip (`52/60 requests left`). GitHub's
/// separate search quota is left out.
pub fn observe_quota(endpoints: &Endpoints, response: &crate::forge::transport::ApiResponse) {
    if response
        .header("x-ratelimit-resource")
        .is_some_and(|resource| resource.contains("search"))
    {
        return;
    }
    let quota = crate::forge::ratelimit::Quota::of(response);
    if quota.remaining.is_none() && quota.limit.is_none() {
        return;
    }
    update(&endpoints.host, Some(endpoints.kind), |account| {
        if matches!(
            account.state,
            HostState::Unreachable { .. } | HostState::RateLimited { .. } | HostState::Unknown(_)
        ) {
            account.state = anonymous_state(endpoints);
        }
        if let HostState::Anonymous {
            remaining,
            limit,
            reset,
            ..
        } = &mut account.state
        {
            *remaining = quota.remaining.or(*remaining);
            *limit = quota.limit.or(*limit);
            if let Some(reset_in) = quota.reset_in {
                *reset = Some(Instant::now() + reset_in);
            }
        }
    });
}

fn endpoints_tea_login(endpoints: &Endpoints) -> Option<CredentialSource> {
    let stores =
        crate::forge::credential::Stores::from_env(&crate::forge::credential::Env::process());
    let login =
        crate::forge::credential::tea_login(stores.tea_config.as_deref()?, &endpoints.host)?;
    Some(CredentialSource::TeaConfig(login.name))
}

/// An error from connecting or sending; item-level errors leave the host's state alone.
pub fn observe_error(endpoints: &Endpoints, error: &ForgeError) {
    observe_host_error(&endpoints.host, Some(endpoints.kind), error);
}

pub fn observe_host_error(host: &HostKey, kind: Option<ProviderKind>, error: &ForgeError) {
    if let Some(state) = HostState::from_error(kind, error) {
        update(host, kind, |account| {
            if !keeps_anonymous(&account.state, &state) {
                account.state = state;
            }
        });
    }
}

/// While a host is browsed anonymously, a refused write or a 401 on one endpoint (GitLab
/// labels, a job log) does not sign it out, and background work held back by the reserve
/// does not make it rate limited; only a spent quota does.
fn keeps_anonymous(current: &HostState, next: &HostState) -> bool {
    match (current, next) {
        (HostState::Anonymous { .. }, HostState::SignedOut { .. }) => true,
        (HostState::Anonymous { remaining, .. }, HostState::RateLimited { .. }) => {
            remaining.is_none_or(|remaining| remaining > 0)
        }
        _ => false,
    }
}

/// A response arrived: a host that was unreachable or paused is back.
pub fn observe_response(endpoints: &Endpoints, transport: &TransportKind) {
    let stale = registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&endpoints.host)
        .is_none_or(|account| {
            matches!(
                account.state,
                HostState::Unreachable { .. }
                    | HostState::RateLimited { .. }
                    | HostState::Unknown(_)
            )
        });
    if stale {
        observe_transport(endpoints, transport.clone());
    }
}

pub fn accounts() -> Vec<Account> {
    registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .values()
        .cloned()
        .collect()
}

pub fn account(host: &HostKey) -> Option<Account> {
    registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(host)
        .cloned()
}

/// The account observed most recently (the host of a read whose target the UI did not know).
pub fn latest() -> Option<Account> {
    registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .values()
        .max_by_key(|account| account.observed)
        .cloned()
}

/// Drop a host from the accounts list (the tutor's practice forge, once left).
pub fn forget(host: &HostKey) {
    registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(host);
}

/// `r` in the accounts popover: forget credentials, transport choices, detection and pauses so
/// the next request discovers everything again.
pub fn recheck() {
    crate::forge::credential::clear_cache();
    crate::forge::transport::forget_selections();
    crate::forge::detect::forget();
    crate::forge::ratelimit::global().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_use_fixed_copy() {
        let signed_out = HostState::SignedOut {
            cli: Cli::Glab,
            hint: "glab auth login --hostname gitlab.com".into(),
        };
        assert_eq!(
            signed_out
                .notice("gitlab.com", Some(Duration::from_secs(12 * 60)))
                .unwrap()
                .0,
            "✕ Sign in required for gitlab.com · updated 12m ago · @ sign in"
        );
        assert_eq!(
            HostState::CliMissing {
                cli: Cli::Glab,
                install_hint: String::new()
            }
            .notice("gitlab.com", None)
            .unwrap()
            .0,
            "✕ glab is not installed · Git operations remain available"
        );
        assert_eq!(
            HostState::Unreachable { error: "x".into() }
                .notice("codeberg.org", Some(Duration::from_secs(120)))
                .unwrap()
                .0,
            "✕ Could not reach codeberg.org · updated 2m ago · R retry"
        );
        assert_eq!(
            HostState::UnknownProvider
                .notice("git.example", None)
                .unwrap()
                .0,
            "Unknown site git.example · @ accounts"
        );
        let limited = HostState::RateLimited {
            until: Instant::now() + Duration::from_secs(42),
        }
        .notice("github.com", Some(Duration::from_secs(5)))
        .unwrap()
        .0;
        assert!(
            limited.starts_with("⚠ Rate limited by github.com · resumes in 4"),
            "{limited}"
        );
        assert!(limited.ends_with("· showing earlier results"));
        assert!(
            HostState::Ready {
                transport: TransportKind::Cli(Cli::Gh),
                account: None
            }
            .notice("github.com", None)
            .is_none()
        );
    }

    #[test]
    fn item_errors_leave_the_host_state_alone() {
        assert_eq!(
            HostState::from_error(None, &ForgeError::NotFound("x".into())),
            None
        );
        assert_eq!(
            HostState::from_error(
                Some(ProviderKind::GitLab),
                &ForgeError::Unsupported {
                    capability: "provider",
                    reason: "Unknown site h · @ accounts".into()
                }
            ),
            Some(HostState::UnknownProvider)
        );
        assert!(matches!(
            HostState::from_error(
                Some(ProviderKind::GitLab),
                &ForgeError::NotAuthenticated {
                    host: "gitlab.com".into(),
                    hint: "glab auth login".into()
                }
            ),
            Some(HostState::SignedOut { cli: Cli::Glab, .. })
        ));
    }

    #[test]
    fn unreachable_account_uses_a_fixed_retry_instruction() {
        let account = Account::new(
            HostKey::new("offline.test"),
            None,
            HostState::Unreachable { error: "Network error · 502 Bad Gateway".into() },
        );
        assert_eq!(account.lines()[1], ("  Can't reach offline.test · r retry".into(), Tone::Muted));
    }

    #[test]
    fn unknown_account_explains_site_configuration_on_two_lines() {
        let account = Account::new(HostKey::new("unknown.test"), None, HostState::UnknownProvider);
        assert_eq!(account.lines(), vec![
            ("○ unknown.test · Unknown site".into(), Tone::Warning),
            ("Tell Dusty Git what kind of site this is, in config.toml:".into(), Tone::Muted),
            ("[forge.hosts.\"unknown.test\"] provider = \"github\"   # or gitlab, gitea, forgejo".into(), Tone::Muted),
        ]);
    }

    #[test]
    fn popover_rows_name_source_account_and_transport() {
        let mut account = Account::new(
            HostKey::new("github.com"),
            Some(ProviderKind::GitHub),
            HostState::Ready {
                transport: TransportKind::Http {
                    credential_source: CredentialSource::GhAuthToken,
                },
                account: Some("maya".into()),
            },
        );
        assert_eq!(
            account.lines()[0].0,
            "✓ github.com · gh token · maya · direct"
        );
        account.host = HostKey::new("codeberg.org");
        account.source = Some(CredentialSource::TeaConfig("cb".into()));
        account.state = HostState::Ready {
            transport: TransportKind::Cli(Cli::Tea),
            account: Some("maya".into()),
        };
        assert_eq!(
            account.lines()[0].0,
            "✓ codeberg.org · tea login \"cb\" · maya · via tea (slower)"
        );
        account.state = HostState::SignedOut {
            cli: Cli::Glab,
            hint: "glab auth login --hostname gitlab.com".into(),
        };
        let lines = account.lines();
        assert_eq!(lines[1].0, "  glab auth login --hostname gitlab.com");
    }
}
