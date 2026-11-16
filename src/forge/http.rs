//! Direct REST/GraphQL over `reqwest` (FORGE.md §1.2): the hot path whenever a token could be
//! read. Redirects are followed by hand (≤ 5) so the token is only ever sent to the API's own
//! origin; a blob redirect (logs, artifacts) is fetched without it. Responses may be gzip
//! encoded (decoded before the body cap applies). Every request first asks `ratelimit.rs`
//! whether its credential is paused, and every response tells it the quota left.
//!
//! Without a credential (`HttpTransport::anonymous`, FORGE.md §2.5) the same client reads
//! public repositories: no auth header, every write and every GitHub GraphQL document refused
//! before it is sent, background work held back once half the (small, per-address) quota is
//! spent, and each response's quota recorded in the host state for the UI's chip.
//!
//! Not done: `gh config get http_unix_socket` (HTTP over a Unix socket). reqwest's blocking
//! client has no Unix-socket connector; a socket configured for gh is honoured by sending
//! through gh instead (`DUSTY_FORGE_TRANSPORT=cli`).

use crate::forge::credential::Credential;
use crate::forge::provider::ProviderKind;
use crate::forge::request::ForgeError;
use crate::forge::transport::ApiRequest;
use crate::forge::transport::ApiResponse;
use crate::forge::transport::Class;
use crate::forge::transport::Endpoints;
use crate::forge::transport::HTTP_BACKGROUND_BOUND;
use crate::forge::transport::HTTP_BOUND;
use crate::forge::transport::HostKey;
use crate::forge::transport::Method;
use crate::forge::transport::Target;
use crate::forge::transport::Transport;
use crate::forge::transport::TransportError;
use crate::forge::transport::TransportKind;
use std::io::Read;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

const MAX_REDIRECTS: usize = 5;

pub struct HttpTransport {
    endpoints: Endpoints,
    /// `None` is the anonymous read path.
    credential: Option<Credential>,
    fingerprint: crate::forge::ratelimit::Fingerprint,
    client: reqwest::blocking::Client,
}

impl HttpTransport {
    pub fn new(endpoints: Endpoints, credential: Credential) -> Result<Self, ForgeError> {
        let fingerprint =
            crate::forge::ratelimit::Fingerprint::token(&endpoints.api_base, &credential.token);
        Self::build(endpoints, Some(credential), fingerprint)
    }

    /// Reads without a credential; writes are refused before anything is sent.
    pub fn anonymous(endpoints: Endpoints) -> Result<Self, ForgeError> {
        let fingerprint = crate::forge::ratelimit::Fingerprint::anonymous(&endpoints.api_base);
        Self::build(endpoints, None, fingerprint)
    }

    fn build(
        endpoints: Endpoints,
        credential: Option<Credential>,
        fingerprint: crate::forge::ratelimit::Fingerprint,
    ) -> Result<Self, ForgeError> {
        Endpoints::check_scheme(&endpoints.api_base)?;
        if let Some(graphql) = &endpoints.graphql {
            Endpoints::check_scheme(graphql)?;
        }
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("dusty/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .gzip(true)
            .build()
            .map_err(|error| ForgeError::Failed(format!("HTTP client: {error}")))?;
        Ok(Self {
            fingerprint,
            endpoints,
            credential,
            client,
        })
    }

    pub fn credential(&self) -> Option<&Credential> {
        self.credential.as_ref()
    }

    pub fn fingerprint(&self) -> crate::forge::ratelimit::Fingerprint {
        self.fingerprint
    }

    /// The live credential check (§2.1): auth-status commands miss expired tokens. Records the
    /// account for the accounts popover.
    pub fn check_viewer(&self, cancel: &AtomicBool) -> Result<(), ForgeError> {
        let Some(credential) = &self.credential else {
            return Ok(());
        };
        let login = self.viewer(cancel)?;
        crate::forge::credential::accepted(&self.endpoints.host);
        crate::forge::host_state::observe_account(&self.endpoints, &credential.source, login);
        Ok(())
    }

    /// `GET /user` → the login (`login` on GitHub/Gitea, `username` on GitLab).
    pub fn viewer(&self, cancel: &AtomicBool) -> Result<Option<String>, ForgeError> {
        let response = self.send(
            &self.endpoints.host.clone(),
            &ApiRequest::get("user"),
            cancel,
        )?;
        if !response.success() {
            return Err(classify(&self.endpoints, &response));
        }
        let user: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
        Ok(user["login"]
            .as_str()
            .or_else(|| user["username"].as_str())
            .map(str::to_owned)
            .or_else(|| self.credential.as_ref()?.user.clone()))
    }

    fn url(&self, request: &ApiRequest) -> Result<String, TransportError> {
        let base =
            match &request.target {
                Target::Rest(path) => format!("{}/{path}", self.endpoints.api_base),
                Target::GraphQl => self.endpoints.graphql.clone().ok_or_else(|| {
                    TransportError::Insecure("this forge has no GraphQL API".into())
                })?,
                Target::Absolute(url) => url.clone(),
            };
        let mut url = reqwest::Url::parse(&base)
            .map_err(|_| TransportError::Insecure(format!("invalid forge URL {base}")))?;
        if !request.query.is_empty() {
            url.query_pairs_mut().extend_pairs(request.query.iter());
        }
        Ok(url.into())
    }

    fn authorization(&self) -> Option<(&'static str, String)> {
        let token = self.credential.as_ref()?.token.expose();
        Some(match self.endpoints.kind {
            ProviderKind::GitHub => ("Authorization", format!("Bearer {token}")),
            ProviderKind::GitLab => ("PRIVATE-TOKEN", token.to_owned()),
            ProviderKind::Gitea | ProviderKind::Forgejo => {
                ("Authorization", format!("token {token}"))
            }
        })
    }

    /// What an anonymous request may not do: anything but a GET, except a GitLab GraphQL
    /// query (GitLab answers those for public projects; GitHub's GraphQL needs a token).
    fn refuse_anonymous(&self, request: &ApiRequest) -> Option<TransportError> {
        let query = match (&request.target, &request.body) {
            (Target::GraphQl, Some(crate::forge::transport::Body::Json(body))) => body["query"]
                .as_str()
                .map(|document| !document.trim_start().starts_with("mutation")),
            _ => None,
        };
        let allowed = match request.target {
            Target::GraphQl => self.endpoints.kind == ProviderKind::GitLab && query == Some(true),
            _ => request.method == Method::Get,
        };
        (!allowed).then(|| TransportError::NeedsSignIn {
            host: self.endpoints.host.to_string(),
            hint: crate::forge::credential::login_hint(self.endpoints.kind, &self.endpoints.host),
        })
    }
}

fn origin(url: &reqwest::Url) -> (String, Option<String>, Option<u16>) {
    (
        url.scheme().to_owned(),
        url.host_str().map(str::to_ascii_lowercase),
        url.port_or_known_default(),
    )
}

impl Transport for HttpTransport {
    fn kind(&self) -> TransportKind {
        match &self.credential {
            Some(credential) => TransportKind::Http {
                credential_source: credential.source.clone(),
            },
            None => TransportKind::Anonymous,
        }
    }

    fn send(
        &self,
        _host: &HostKey,
        request: &ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<ApiResponse, TransportError> {
        let anonymous = self.credential.is_none();
        if anonymous && let Some(refused) = self.refuse_anonymous(request) {
            return Err(refused);
        }
        let _background = match request.class {
            Class::Background => Some(HTTP_BACKGROUND_BOUND.acquire(cancel)?),
            Class::Interactive => None,
        };
        let limiter = crate::forge::ratelimit::global();
        let now = std::time::Instant::now();
        if anonymous {
            limiter.check_anonymous(self.fingerprint, request.class, now)
        } else {
            limiter.check(self.fingerprint, request.class, now)
        }
        .map_err(|until| TransportError::RateLimited { until })?;
        let _permit = HTTP_BOUND.acquire(cancel)?;
        let first = self.url(request)?;
        Endpoints::check_scheme(&first)?;
        let api_origin = origin(
            &reqwest::Url::parse(&self.endpoints.api_base)
                .map_err(|_| TransportError::Insecure("invalid API base".into()))?,
        );
        let mut url = reqwest::Url::parse(&first)
            .map_err(|_| TransportError::Insecure(format!("invalid forge URL {first}")))?;
        let mut method = request.method;
        let mut body = request.body.as_ref();
        for _ in 0..=MAX_REDIRECTS {
            if cancel.load(Ordering::Acquire) {
                return Err(TransportError::Cancelled);
            }
            let reqwest_method = match method {
                Method::Get => reqwest::Method::GET,
                Method::Post => reqwest::Method::POST,
                Method::Patch => reqwest::Method::PATCH,
                Method::Put => reqwest::Method::PUT,
                Method::Delete => reqwest::Method::DELETE,
            };
            let mut builder = self
                .client
                .request(reqwest_method, url.clone())
                .timeout(request.timeout)
                .header("Accept", request.accept.header(self.endpoints.kind));
            if self.endpoints.kind == ProviderKind::GitHub {
                builder = builder.header("X-GitHub-Api-Version", "2022-11-28");
            }
            // Never across origins: blob storage gets the request without the token.
            if origin(&url) == api_origin
                && let Some((name, value)) = self.authorization()
            {
                builder = builder.header(name, value);
            }
            for (key, value) in &request.headers {
                builder = builder.header(key.as_str(), value.as_str());
            }
            if let Some(payload) = body {
                builder = builder
                    .header("Content-Type", payload.content_type())
                    .body(payload.bytes());
            }
            let response = builder.send().map_err(|error| {
                if error.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::Network(error.without_url().to_string())
                }
            })?;
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308)
                && let Some(location) = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
            {
                let next = url
                    .join(location)
                    .map_err(|_| TransportError::Network("invalid redirect".into()))?;
                Endpoints::check_scheme(next.as_str())?;
                if status == 303 || (matches!(status, 301 | 302) && method != Method::Get) {
                    method = Method::Get;
                    body = None;
                }
                url = next;
                continue;
            }
            let headers = response
                .headers()
                .iter()
                .filter_map(|(key, value)| {
                    Some((
                        key.as_str().to_ascii_lowercase(),
                        value.to_str().ok()?.to_owned(),
                    ))
                })
                .collect();
            let mut bytes = Vec::new();
            response
                .take(request.body_cap as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::TimedOut {
                        TransportError::Timeout
                    } else {
                        TransportError::Network(error.to_string())
                    }
                })?;
            if bytes.len() > request.body_cap {
                return Err(TransportError::TooLarge(request.body_cap));
            }
            let response = ApiResponse {
                status,
                headers,
                body: bytes,
            };
            // Only the API's own answers count against the credential, not a blob store's.
            if anonymous && origin(&url) == api_origin {
                crate::forge::host_state::observe_quota(&self.endpoints, &response);
            }
            if origin(&url) == api_origin
                && let Some(until) = limiter.observe(
                    self.fingerprint,
                    &response,
                    request.target == Target::GraphQl,
                    std::time::Instant::now(),
                )
            {
                return Err(TransportError::RateLimited { until });
            }
            return Ok(response);
        }
        Err(TransportError::Network("too many redirects".into()))
    }

    fn graphql_batching(&self) -> bool {
        self.endpoints.graphql.is_some()
            && (self.credential.is_some() || self.endpoints.kind == ProviderKind::GitLab)
    }
}

/// The scopes a 403 says the token lacks, joined `repo, workflow`: GitHub's
/// `X-Accepted-OAuth-Scopes` minus `X-OAuth-Scopes`, GitLab's `insufficient_scope` (body or
/// `WWW-Authenticate`), Gitea/Forgejo's `required=[…]` / `required scope(s): […]`.
pub fn missing_scopes(response: &ApiResponse, body: &serde_json::Value, message: &str) -> Option<String> {
    let split = |text: &str| -> Vec<String> {
        text.split([',', ' '])
            .map(|scope| scope.trim().trim_matches(['\'', '"']))
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let join = |scopes: Vec<String>| (!scopes.is_empty()).then(|| scopes.join(", "));
    if let Some(accepted) = response.header("x-accepted-oauth-scopes") {
        let held = split(response.header("x-oauth-scopes").unwrap_or_default());
        let needed: Vec<String> = split(accepted)
            .into_iter()
            .filter(|scope| !held.contains(scope))
            .collect();
        if let Some(scopes) = join(needed) {
            return Some(scopes);
        }
    }
    let insufficient = body["error"].as_str() == Some("insufficient_scope")
        || response
            .header("www-authenticate")
            .is_some_and(|value| value.contains("insufficient_scope"));
    if insufficient {
        let from_header = response.header("www-authenticate").and_then(|value| {
            let at = value.find("scope=\"")? + "scope=\"".len();
            value[at..].split('"').next().map(str::to_owned)
        });
        let scopes = body["scope"].as_str().map(str::to_owned).or(from_header);
        return join(split(&scopes.unwrap_or_default()));
    }
    scopes_in(message)
}

/// `… required=[read:repository] …`, `required scope(s): [a, b]` or GraphQL's `… one of the
/// following scopes: ['read:project']`: the bracketed list after the word that names it.
pub fn scopes_in(message: &str) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    let at = lower.find("required").or_else(|| lower.find("following scopes"))?;
    let rest = &message[at..];
    let open = rest.find('[')?;
    let close = rest[open..].find(']')? + open;
    let scopes: Vec<&str> = rest[open + 1..close]
        .split(',')
        .map(|scope| scope.trim().trim_matches(['\'', '"']))
        .filter(|scope| !scope.is_empty())
        .collect();
    (!scopes.is_empty()).then(|| scopes.join(", "))
}

/// One error per response (§3.5); the copy shown to the user comes from `ForgeError`'s Display.
pub fn classify(endpoints: &Endpoints, response: &ApiResponse) -> ForgeError {
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
    let message = body["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            String::from_utf8_lossy(&response.body)
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect()
        });
    let message = crate::forge::cli::redact(&message);
    if let Some(trigger) = crate::forge::ratelimit::trigger(response, false) {
        return ForgeError::RateLimited {
            until: std::time::Instant::now()
                + trigger
                    .wait
                    .unwrap_or(crate::forge::ratelimit::FIRST_BACKOFF),
        };
    }
    // Forgejo/Gitea `REQUIRE_SIGNIN_VIEW`: every API call without a sign-in is refused.
    if matches!(response.status, 401 | 403)
        && message
            .to_ascii_lowercase()
            .contains("only signed in user is allowed")
    {
        return ForgeError::SignInRequired {
            host: endpoints.host.to_string(),
            hints: crate::forge::credential::sign_in_hints(endpoints.kind, &endpoints.host),
        };
    }
    match response.status {
        401 => ForgeError::NotAuthenticated {
            host: endpoints.host.to_string(),
            hint: crate::forge::credential::login_hint(endpoints.kind, &endpoints.host),
        },
        403 => ForgeError::Forbidden(match missing_scopes(response, &body, &message) {
            Some(scopes) => format!("needs scope {scopes}"),
            None => message,
        }),
        404 | 410 => ForgeError::NotFound(message),
        405 | 409 | 422 => ForgeError::Conflict(message),
        500..=599 => ForgeError::Network(format!("{} {message}", response.status)),
        _ => ForgeError::Failed(format!("{} {message}", response.status)),
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    fn response(headers: &[(&str, &str)], body: &str) -> ApiResponse {
        ApiResponse {
            status: 403,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn scopes(response: &ApiResponse) -> Option<String> {
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
        let message = body["message"].as_str().unwrap_or_default().to_owned();
        missing_scopes(response, &body, &message)
    }

    #[test]
    fn a_403_names_the_scopes_the_token_lacks() {
        let github = response(
            &[("x-oauth-scopes", "read:org"), ("x-accepted-oauth-scopes", "repo, workflow")],
            r#"{"message":"Resource not accessible by personal access token"}"#,
        );
        assert_eq!(scopes(&github).as_deref(), Some("repo, workflow"));
        let held = response(&[("x-oauth-scopes", "repo"), ("x-accepted-oauth-scopes", "repo")], "{}");
        assert_eq!(scopes(&held), None);
        let gitlab = response(
            &[("www-authenticate", r#"Bearer realm="", error="insufficient_scope", scope="api""#)],
            r#"{"error":"insufficient_scope","scope":"api read_api"}"#,
        );
        assert_eq!(scopes(&gitlab).as_deref(), Some("api, read_api"));
        let gitea = response(
            &[],
            r#"{"message":"token does not have at least one of required scope(s), required=[read:repository], token scope=read:user"}"#,
        );
        assert_eq!(scopes(&gitea).as_deref(), Some("read:repository"));
        assert_eq!(
            scopes_in("The 'title' field requires one of the following scopes: ['read:project']").as_deref(),
            Some("read:project")
        );
        assert_eq!(scopes(&response(&[], r#"{"message":"Must have admin rights"}"#)), None);
    }
}
