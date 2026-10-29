//! The one bounded transport a forge request travels through.
//!
//! W-22: "Forge access uses one bounded HTTPS transport. Requests are refused before transmission
//! when they would leave the configured API origin, and credentials are never forwarded across a
//! host redirect. Response bodies are bounded and error text is redacted."
//!
//! Redirects are decided HERE, by the policy in this module's parent, rather than by the HTTP
//! client: following them is what carries a credential to another host, so the decision must be
//! ours and must be visible.

use std::io::Read;
use std::time::Duration;

use super::providers::auth_header;
use super::{
    ApiOrigin, Credential, ForgeRefusal, MAX_REDIRECTS, MAX_RESPONSE_BYTES, PlannedRequest, Provider,
    redact, redirect,
};

/// A response the caller may read: bounded, and never larger than the contract allows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeResponse {
    pub status: u16,
    pub body: String,
}

/// The single bounded transport.
pub struct ForgeTransport {
    client: reqwest::blocking::Client,
    redirects: usize,
}

impl ForgeTransport {
    /// The production transport: rustls, bounded redirects decided by our policy, and timeouts so a
    /// forge cannot hold the editor open.
    pub fn new() -> Result<Self, ForgeRefusal> {
        Self::with_limits(Duration::from_secs(20), Duration::from_secs(120), MAX_REDIRECTS)
    }

    /// The same transport with explicit limits, so a test can fail fast rather than wait out a
    /// production timeout.
    pub fn with_limits(
        connect: Duration,
        total: Duration,
        redirects: usize,
    ) -> Result<Self, ForgeRefusal> {
        let client = reqwest::blocking::Client::builder()
            // OURS, not the client's: a redirect is exactly where a credential could leave.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect)
            .timeout(total)
            .build()
            .map_err(|error| ForgeRefusal::Transport(error.to_string()))?;
        Ok(Self { client, redirects })
    }

    /// Send an ADMITTED request. The origin containment check runs again here, before anything is
    /// transmitted, so an address that escaped the policy cannot leave through this door.
    pub fn send(
        &self,
        provider: Provider,
        method: &str,
        planned: &PlannedRequest,
        origin: &ApiOrigin,
        allowed_hosts: &[&str],
    ) -> Result<ForgeResponse, ForgeRefusal> {
        origin.contains(&planned.address)?;
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| ForgeRefusal::UnsupportedMethod(method.to_owned()))?;
        let secrets: Vec<&str> = planned.credential.as_ref().map(Credential::expose).into_iter().collect();
        let mut hop = planned.clone();
        for redirects_so_far in 0..=self.redirects {
            let mut request = self.client.request(method.clone(), &hop.address);
            if let Some(credential) = &hop.credential {
                // The PROVIDER's own header: GitLab names its token header differently from the
                // others, and sending a bearer token there would be a silent protocol downgrade.
                let (name, value) = auth_header(provider, credential);
                request = request.header(name, value);
            }
            let response = request.send().map_err(|error| {
                ForgeRefusal::Transport(redact(&error.to_string(), &secrets))
            })?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        ForgeRefusal::Transport("a redirect named no location".to_owned())
                    })?
                    .to_owned();
                // A relative location resolves against the address that produced it.
                let location = super::resolve_location(&hop.address, &location)?;
                // OUR decision, including whether the credential survives the hop.
                hop = redirect(
                    origin,
                    &location,
                    hop.credential.as_ref(),
                    allowed_hosts,
                    redirects_so_far,
                )?;
                continue;
            }
            // Bounded AS IT IS READ: one byte past the bound is enough to refuse it.
            let mut body = Vec::new();
            let mut bounded = response.take(MAX_RESPONSE_BYTES as u64 + 1);
            bounded
                .read_to_end(&mut body)
                .map_err(|error| ForgeRefusal::Transport(redact(&error.to_string(), &secrets)))?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(ForgeRefusal::ResponseTooLarge);
            }
            return Ok(ForgeResponse {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        Err(ForgeRefusal::RedirectLimit)
    }
}
