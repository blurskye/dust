//! Forge transport, account and repository authority (W-22).
//!
//! The contract: "Forge access uses one bounded HTTPS transport. Requests are refused before
//! transmission when they would leave the configured API origin, and credentials are never
//! forwarded across a host redirect. Response bodies are bounded and error text is redacted.
//! Authentication uses an approved credential store, an explicitly session-only entry, or a named
//! environment variable; there is no plaintext fallback and no silent downgrade." and "Every
//! account binds an explicit provider, host, API base, account and repository, and every write
//! names both account and repository before sending; a mismatch between the bound repository and
//! the target is refused."
//!
//! Everything here is a DECISION made before anything is transmitted, so it is testable without a
//! socket and without a network: the transport that carries out an admitted request is a separate
//! concern.

/// The redirect chain is bounded: a server cannot walk the client across hosts without limit.
pub const MAX_REDIRECTS: usize = 3;
/// Response bodies are bounded, so a forge cannot exhaust the editor's memory.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Why a request was refused BEFORE it was transmitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgeRefusal {
    /// The address could not be read as a bounded HTTPS origin.
    UnreadableAddress(String),
    /// The transport is HTTPS-only: plaintext is refused rather than used as a fallback.
    InsecureScheme,
    /// The address carried credentials inline, which is a plaintext fallback.
    InlineCredential,
    /// The request would leave the configured API origin.
    OriginEscape,
    /// A redirect would leave the hosts this operation is allowed to follow.
    RedirectNotAllowed(String),
    /// The redirect chain exceeded its bound.
    RedirectLimit,
    /// A write named no account, no repository, or neither.
    UnboundWrite,
    /// The write's account or repository is not the one bound.
    BindingMismatch,
    /// The credential source is not one the contract approves.
    UnapprovedCredential,
    /// The response body exceeded its bound.
    ResponseTooLarge,
    /// The provider's reported identity is not the identity expected.
    IdentityMismatch,
    /// The transport could not complete, carrying a REDACTED message.
    Transport(String),
    /// The request method is not one a forge request may use.
    UnsupportedMethod(String),
    /// The provider does not support this operation, so it is refused rather than approximated.
    UnsupportedOperation(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scheme {
    Https,
    /// Reachable ONLY through the `#[cfg(test)]` `ApiOrigin::fixture` seam, so a local fixture can
    /// be addressed without weakening the production rule that a forge origin is HTTPS.
    Http,
}

/// Where a forge's API lives. Requests outside it are refused before transmission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiOrigin {
    scheme: Scheme,
    host: String,
    port: Option<u16>,
}

impl ApiOrigin {
    /// Read an API base as a bounded HTTPS origin. Plaintext, inline credentials and anything
    /// unreadable are refused here, once, rather than remembered as a fallback.
    pub fn parse(address: &str) -> Result<Self, ForgeRefusal> {
        Self::read(address, false)
    }

    /// A local fixture's origin. `#[cfg(test)]` only, so production code cannot construct a
    /// plaintext forge origin: `read(address, false)` refuses one.
    #[cfg(test)]
    pub(crate) fn fixture(address: &str) -> Result<Self, ForgeRefusal> {
        Self::read(address, true)
    }

    fn read(address: &str, allow_plaintext: bool) -> Result<Self, ForgeRefusal> {
        let (scheme, rest) = address
            .split_once("://")
            .ok_or_else(|| ForgeRefusal::UnreadableAddress(address.to_owned()))?;
        let scheme = if scheme.eq_ignore_ascii_case("https") {
            Scheme::Https
        } else if allow_plaintext && scheme.eq_ignore_ascii_case("http") {
            Scheme::Http
        } else {
            return Err(ForgeRefusal::InsecureScheme);
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return Err(ForgeRefusal::InlineCredential);
        }
        if authority.is_empty() {
            return Err(ForgeRefusal::UnreadableAddress(address.to_owned()));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .map_err(|_| ForgeRefusal::UnreadableAddress(address.to_owned()))?;
                (host, Some(port))
            }
            None => (authority, None),
        };
        if host.is_empty() {
            return Err(ForgeRefusal::UnreadableAddress(address.to_owned()));
        }
        Ok(Self {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
        })
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The origin as an address prefix, so an account's OWN api base can carry its addresses - a
    /// self-hosted forge is the reason the binding names a host and an API base separately.
    #[must_use]
    pub fn base(&self) -> String {
        let scheme = match self.scheme {
            Scheme::Https => "https",
            Scheme::Http => "http",
        };
        match self.port {
            Some(port) => format!("{scheme}://{}:{port}", self.host),
            None => format!("{scheme}://{}", self.host),
        }
    }

    /// Does this address stay inside the API origin? Compared on scheme, host AND port.
    pub fn contains(&self, address: &str) -> Result<Self, ForgeRefusal> {
        // A candidate is read with THIS origin's own allowance: a production origin is HTTPS and
        // therefore can never contain a plaintext candidate, while a `#[cfg(test)]` fixture origin
        // (the only kind that is plaintext) can contain its own fixture addresses.
        let other = Self::read(address, matches!(self.scheme, Scheme::Http))?;
        if other.scheme != self.scheme || other.host != self.host || other.port != self.port {
            return Err(ForgeRefusal::OriginEscape);
        }
        Ok(other)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    GitHub,
    GitLab,
    Gitea,
    Forgejo,
}

/// The authentication sources the contract approves. Nothing else is a credential source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialSource {
    /// An approved credential store.
    Store,
    /// An entry that lives only for this session.
    Session,
    /// A named environment variable.
    Environment,
}

/// A credential whose secret is never printed, logged or serialised with its text.
#[derive(Clone, Eq, PartialEq)]
pub struct Credential {
    source: CredentialSource,
    name: String,
    secret: String,
}

impl Credential {
    #[must_use]
    pub fn new(source: CredentialSource, name: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            source,
            name: name.into(),
            secret: secret.into(),
        }
    }

    #[must_use]
    pub fn source(&self) -> CredentialSource {
        self.source
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The secret, for the one place that builds a header. Never for a message.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.secret
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Credential")
            .field("source", &self.source)
            .field("name", &self.name)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Redact every secret from a message before it can be stored, shown or logged.
#[must_use]
pub fn redact(message: &str, secrets: &[&str]) -> String {
    let mut redacted = message.to_owned();
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        redacted = redacted.replace(secret, "<redacted>");
    }
    redacted
}

/// A response body is only usable up to its bound.
pub fn bound_response(body: &str, bound: usize) -> Result<&str, ForgeRefusal> {
    if body.len() > bound {
        return Err(ForgeRefusal::ResponseTooLarge);
    }
    Ok(body)
}

/// A provider identity, as the provider itself reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub host: String,
    pub account: String,
}

/// An account is only saved once the provider has CONFIRMED the identity it was asked about.
pub fn verify_identity(expected: &Identity, reported: &Identity) -> Result<(), ForgeRefusal> {
    if expected != reported {
        return Err(ForgeRefusal::IdentityMismatch);
    }
    Ok(())
}

/// What an account binds: provider, host, API base, account and repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountBinding {
    pub provider: Provider,
    pub host: String,
    pub api_base: ApiOrigin,
    pub account: String,
    pub repository: String,
}

/// A write that names the account and repository it targets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteTarget<'a> {
    pub address: &'a str,
    pub account: Option<&'a str>,
    pub repository: Option<&'a str>,
}

/// A request that passed every pre-transmission rule, with the credential it may carry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedRequest {
    pub address: String,
    pub credential: Option<Credential>,
}

/// Decide whether a WRITE may be sent. Every write names both account and repository before
/// sending, the binding must match, and the address must stay inside the API origin.
pub fn plan_write(
    binding: &AccountBinding,
    target: &WriteTarget<'_>,
    credential: Option<&Credential>,
) -> Result<PlannedRequest, ForgeRefusal> {
    let (Some(account), Some(repository)) = (target.account, target.repository) else {
        return Err(ForgeRefusal::UnboundWrite);
    };
    if account != binding.account || repository != binding.repository {
        return Err(ForgeRefusal::BindingMismatch);
    }
    binding.api_base.contains(target.address)?;
    if let Some(credential) = credential {
        ensure_approved(credential.source)?;
    }
    Ok(PlannedRequest {
        address: target.address.to_owned(),
        credential: credential.cloned(),
    })
}

/// Resolve a redirect `Location` against the address that produced it. Servers routinely send a
/// RELATIVE location, and a relative location is the same origin by definition, so it must not be
/// mistaken for a cross-host escape or an unreadable address.
pub(crate) fn resolve_location(base: &str, location: &str) -> Result<String, ForgeRefusal> {
    if location.contains("://") {
        return Ok(location.to_owned());
    }
    let scheme_end =
        base.find("://").ok_or_else(|| ForgeRefusal::UnreadableAddress(base.to_owned()))?;
    let scheme = &base[..scheme_end];
    let after_scheme = &base[scheme_end + 3..];
    let authority_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if let Some(rest) = location.strip_prefix("//") {
        return Ok(format!("{scheme}://{rest}"));
    }
    if location.starts_with('/') {
        return Ok(format!("{scheme}://{authority}{location}"));
    }
    let path = &after_scheme[authority_end..];
    let directory = match path.rfind('/') {
        Some(index) => &path[..=index],
        None => "/",
    };
    Ok(format!("{scheme}://{authority}{directory}{location}"))
}

/// Decide what a redirect may carry. A credential is NEVER forwarded across a host change, and a
/// host this operation is not allowed to follow is refused outright.
pub fn redirect(
    from: &ApiOrigin,
    to: &str,
    credentialed: Option<&Credential>,
    allowed_hosts: &[&str],
    redirects_so_far: usize,
) -> Result<PlannedRequest, ForgeRefusal> {
    if redirects_so_far >= MAX_REDIRECTS {
        return Err(ForgeRefusal::RedirectLimit);
    }
    // Read the target with THIS origin's own allowance, so a fixture origin can be redirected
    // within itself while a production origin still refuses a plaintext target outright.
    let target = ApiOrigin::read(to, matches!(from.scheme, Scheme::Http))?;
    if target.host == from.host && target.port == from.port {
        // Same host: the origin still contains it, so the credential stays with the request.
        return Ok(PlannedRequest {
            address: to.to_owned(),
            credential: credentialed.cloned(),
        });
    }
    let allowed = allowed_hosts
        .iter()
        .any(|host| host.eq_ignore_ascii_case(&target.host));
    if !allowed {
        return Err(ForgeRefusal::RedirectNotAllowed(target.host));
    }
    // Allowed to follow, and the credential is DROPPED rather than forwarded to another host.
    Ok(PlannedRequest {
        address: to.to_owned(),
        credential: None,
    })
}

fn ensure_approved(source: CredentialSource) -> Result<(), ForgeRefusal> {
    match source {
        CredentialSource::Store | CredentialSource::Session | CredentialSource::Environment => Ok(()),
    }
}

pub mod accounts;
pub mod download;
pub mod providers;
pub mod review;
pub mod surface;
pub mod worktree;
mod transport;
pub use transport::{ForgeResponse, ForgeTransport};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod transport_tests;
#[cfg(test)]
mod provider_tests;
#[cfg(test)]
mod account_tests;
#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod worktree_tests;
#[cfg(test)]
mod download_tests;
#[cfg(test)]
mod surface_tests;
