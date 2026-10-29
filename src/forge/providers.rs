//! Four INDEPENDENT provider adapters and the checked-in capability matrix (W-22).
//!
//! The contract: "GitHub, GitLab, Gitea and Forgejo are independent adapters with a checked-in
//! capability matrix recording operation, endpoint, supported server versions, required permission
//! and verification status. Capability parity is claimed per provider and version, never in
//! aggregate."
//!
//! There is deliberately NO aggregate question here: capability is only ever asked OF a provider,
//! because a single answer for "the forges" would be a parity claim the contract forbids.

use super::{Credential, ForgeRefusal, Provider};

/// A logical forge operation, named by what it does rather than by any provider's spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ForgeOperation {
    /// List reviews (pull requests or merge requests).
    Reviews,
    /// Read one review.
    Review,
    /// Approve the reviewed revision.
    ReviewApprove,
    /// Comment on a review, including inline comments.
    ReviewComment,
    /// Merge the reviewed revision.
    ReviewMerge,
    /// List issues.
    Issues,
    /// Read one issue.
    Issue,
    /// Comment on an issue.
    IssueComment,
    /// Read the checks for a revision.
    Checks,
    /// Read the identity the credential belongs to, so an account is only saved once the provider
    /// has CONFIRMED it.
    Identity,
}

impl ForgeOperation {
    pub const ALL: [Self; 10] = [
        Self::Reviews,
        Self::Review,
        Self::ReviewApprove,
        Self::ReviewComment,
        Self::ReviewMerge,
        Self::Issues,
        Self::Issue,
        Self::IssueComment,
        Self::Checks,
        Self::Identity,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reviews => "reviews",
            Self::Review => "review",
            Self::ReviewApprove => "review.approve",
            Self::ReviewComment => "review.comment",
            Self::ReviewMerge => "review.merge",
            Self::Issues => "issues",
            Self::Issue => "issue",
            Self::IssueComment => "issue.comment",
            Self::Checks => "checks",
            Self::Identity => "identity",
        }
    }
}

/// What an operation addresses: a number (a review or issue) or a revision (checks).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reference<'a> {
    Number(u64),
    Revision(&'a str),
}

/// How far a capability row has been taken. NOTHING here is server-verified: the suite cannot
/// reach a network, so every row says so rather than implying a live check happened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verification {
    /// Taken from the provider's own API documentation and exercised as a BUILT ADDRESS only.
    DocumentationOnly,
    /// Not even documented: the path is a guess and is recorded as such.
    Unverified,
}

/// One capability row: where the operation goes, what it needs, and how far it was verified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capability {
    /// The path appended to the provider's API base. `{account}`, `{repository}`, `{number}` and
    /// `{revision}` are substituted when an address is built.
    pub endpoint: &'static str,
    /// The server versions this row is claimed for.
    pub versions: &'static str,
    /// The permission the credential must hold.
    pub permission: &'static str,
    pub verification: Verification,
}

/// The provider's API base, which every endpoint hangs off.
#[must_use]
pub const fn api_base(provider: Provider) -> &'static str {
    match provider {
        Provider::GitHub => "https://api.github.com",
        Provider::GitLab => "https://gitlab.com",
        Provider::Gitea => "https://gitea.com",
        Provider::Forgejo => "https://codeberg.org",
    }
}

/// Ask ONE provider what it can do. There is no aggregate form on purpose.
#[must_use]
pub fn capability(provider: Provider, operation: ForgeOperation) -> Option<Capability> {
    let documentation = Verification::DocumentationOnly;
    let row = |endpoint, versions, permission| Capability {
        endpoint,
        versions,
        permission,
        verification: documentation,
    };
    match (provider, operation) {
        (Provider::GitHub, ForgeOperation::Reviews) => {
            Some(row("/repos/{account}/{repository}/pulls", "REST 2022-11-28", "pull_requests:read"))
        }
        (Provider::GitHub, ForgeOperation::Review) => {
            Some(row("/repos/{account}/{repository}/pulls/{number}", "REST 2022-11-28", "pull_requests:read"))
        }
        (Provider::GitHub, ForgeOperation::ReviewApprove) => Some(row(
            "/repos/{account}/{repository}/pulls/{number}/reviews",
            "REST 2022-11-28",
            "pull_requests:write",
        )),
        (Provider::GitHub, ForgeOperation::ReviewComment) => Some(row(
            "/repos/{account}/{repository}/pulls/{number}/comments",
            "REST 2022-11-28",
            "pull_requests:write",
        )),
        (Provider::GitHub, ForgeOperation::ReviewMerge) => Some(row(
            "/repos/{account}/{repository}/pulls/{number}/merge",
            "REST 2022-11-28",
            "contents:write",
        )),
        (Provider::GitHub, ForgeOperation::Issues) => {
            Some(row("/repos/{account}/{repository}/issues", "REST 2022-11-28", "issues:read"))
        }
        (Provider::GitHub, ForgeOperation::Issue) => {
            Some(row("/repos/{account}/{repository}/issues/{number}", "REST 2022-11-28", "issues:read"))
        }
        (Provider::GitHub, ForgeOperation::IssueComment) => Some(row(
            "/repos/{account}/{repository}/issues/{number}/comments",
            "REST 2022-11-28",
            "issues:write",
        )),
        (Provider::GitHub, ForgeOperation::Identity) => {
            Some(row("/user", "REST 2022-11-28", "read:user"))
        }
        (Provider::GitHub, ForgeOperation::Checks) => Some(row(
            "/repos/{account}/{repository}/commits/{revision}/check-runs",
            "REST 2022-11-28",
            "checks:read",
        )),
        (Provider::GitLab, ForgeOperation::Reviews) => Some(row(
            "/api/v4/projects/{project}/merge_requests",
            "15 and later",
            "api",
        )),
        // GitLab has no single-review resource: reviews are merge requests plus notes and
        // approvals, so this operation is REFUSED rather than approximated by another endpoint.
        (Provider::GitLab, ForgeOperation::Review) => None,
        (Provider::GitLab, ForgeOperation::ReviewApprove) => Some(row(
            "/api/v4/projects/{project}/merge_requests/{number}/approve",
            "15 and later",
            "api",
        )),
        (Provider::GitLab, ForgeOperation::ReviewComment) => Some(row(
            "/api/v4/projects/{project}/merge_requests/{number}/notes",
            "15 and later",
            "api",
        )),
        (Provider::GitLab, ForgeOperation::ReviewMerge) => Some(row(
            "/api/v4/projects/{project}/merge_requests/{number}/merge",
            "15 and later",
            "api",
        )),
        (Provider::GitLab, ForgeOperation::Issues) => {
            Some(row("/api/v4/projects/{project}/issues", "15 and later", "api"))
        }
        (Provider::GitLab, ForgeOperation::Issue) => Some(row(
            "/api/v4/projects/{project}/issues/{number}",
            "15 and later",
            "api",
        )),
        (Provider::GitLab, ForgeOperation::IssueComment) => Some(row(
            "/api/v4/projects/{project}/issues/{number}/notes",
            "15 and later",
            "api",
        )),
        (Provider::GitLab, ForgeOperation::Identity) => Some(row("/api/v4/user", "15 and later", "read_user")),
        (Provider::GitLab, ForgeOperation::Checks) => Some(row(
            "/api/v4/projects/{project}/repository/commits/{revision}/statuses",
            "15 and later",
            "api",
        )),
        (Provider::Gitea, ForgeOperation::Reviews) => {
            Some(row("/api/v1/repos/{account}/{repository}/pulls", "1.20 and later", "read:repository"))
        }
        (Provider::Gitea, ForgeOperation::Review) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}",
            "1.20 and later",
            "read:repository",
        )),
        (Provider::Gitea, ForgeOperation::ReviewApprove) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}/reviews",
            "1.20 and later",
            "write:repository",
        )),
        (Provider::Gitea, ForgeOperation::ReviewComment) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}/reviews",
            "1.20 and later",
            "write:repository",
        )),
        (Provider::Gitea, ForgeOperation::ReviewMerge) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}/merge",
            "1.20 and later",
            "write:repository",
        )),
        (Provider::Gitea, ForgeOperation::Issues) => {
            Some(row("/api/v1/repos/{account}/{repository}/issues", "1.20 and later", "read:issue"))
        }
        (Provider::Gitea, ForgeOperation::Issue) => Some(row(
            "/api/v1/repos/{account}/{repository}/issues/{number}",
            "1.20 and later",
            "read:issue",
        )),
        (Provider::Gitea, ForgeOperation::IssueComment) => Some(row(
            "/api/v1/repos/{account}/{repository}/issues/{number}/comments",
            "1.20 and later",
            "write:issue",
        )),
        (Provider::Gitea, ForgeOperation::Identity) => {
            Some(row("/api/v1/user", "1.20 and later", "read:user"))
        }
        (Provider::Gitea, ForgeOperation::Checks) => Some(row(
            "/api/v1/repos/{account}/{repository}/commits/{revision}/statuses",
            "1.20 and later",
            "read:repository",
        )),
        (Provider::Forgejo, ForgeOperation::Reviews) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls",
            "7 and later",
            "read:repository",
        )),
        (Provider::Forgejo, ForgeOperation::Review) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}",
            "7 and later",
            "read:repository",
        )),
        (Provider::Forgejo, ForgeOperation::ReviewApprove) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}/reviews",
            "7 and later",
            "write:repository",
        )),
        (Provider::Forgejo, ForgeOperation::ReviewComment) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}/reviews",
            "7 and later",
            "write:repository",
        )),
        (Provider::Forgejo, ForgeOperation::ReviewMerge) => Some(row(
            "/api/v1/repos/{account}/{repository}/pulls/{number}/merge",
            "7 and later",
            "write:repository",
        )),
        (Provider::Forgejo, ForgeOperation::Issues) => {
            Some(row("/api/v1/repos/{account}/{repository}/issues", "7 and later", "read:issue"))
        }
        (Provider::Forgejo, ForgeOperation::Issue) => Some(row(
            "/api/v1/repos/{account}/{repository}/issues/{number}",
            "7 and later",
            "read:issue",
        )),
        (Provider::Forgejo, ForgeOperation::IssueComment) => Some(row(
            "/api/v1/repos/{account}/{repository}/issues/{number}/comments",
            "7 and later",
            "write:issue",
        )),
        (Provider::Forgejo, ForgeOperation::Identity) => {
            Some(row("/api/v1/user", "7 and later", "read:user"))
        }
        // Forgejo's commit statuses are reported per commit, not per combined check run.
        (Provider::Forgejo, ForgeOperation::Checks) => None,
    }
}

/// Build the address for an operation, refusing an operation this provider does not support rather
/// than sending it somewhere that merely looks similar.
pub fn address(
    provider: Provider,
    operation: ForgeOperation,
    account: &str,
    repository: &str,
    reference: Reference<'_>,
) -> Result<String, ForgeRefusal> {
    let base = super::ApiOrigin::parse(api_base(provider)).map_err(|_| {
        ForgeRefusal::UnreadableAddress(api_base(provider).to_owned())
    })?;
    address_on(&base, provider, operation, account, repository, reference)
}

/// The same, on an ACCOUNT's own API base, so a self-hosted forge is addressed rather than the
/// provider's public default.
pub fn address_on(
    base: &super::ApiOrigin,
    provider: Provider,
    operation: ForgeOperation,
    account: &str,
    repository: &str,
    reference: Reference<'_>,
) -> Result<String, ForgeRefusal> {
    let capability = capability(provider, operation)
        .ok_or(ForgeRefusal::UnsupportedOperation(operation.name()))?;
    let project = format!("{account}/{repository}");
    let mut path = capability.endpoint.to_owned();
    // GitLab addresses a project by its URL-encoded full path, so the separator is escaped rather
    // than left to look like a path segment.
    let project = if provider == Provider::GitLab {
        project.replace('/', "%2F")
    } else {
        project
    };
    path = path.replace("{project}", &project);
    path = path.replace("{account}", account);
    path = path.replace("{repository}", repository);
    match reference {
        Reference::Number(number) => {
            if matches!(operation, ForgeOperation::Checks) {
                return Err(ForgeRefusal::UnsupportedOperation(operation.name()));
            }
            path = path.replace("{number}", &number.to_string());
        }
        Reference::Revision(revision) => {
            if !matches!(operation, ForgeOperation::Checks) {
                return Err(ForgeRefusal::UnsupportedOperation(operation.name()));
            }
            path = path.replace("{revision}", revision);
        }
    }
    Ok(format!("{}{path}", base.base()))
}

/// Where a provider states who a credential belongs to. It takes no account or repository,
/// because the question is about the credential rather than about a project.
pub fn identity_address(provider: Provider) -> Result<String, ForgeRefusal> {
    let base = super::ApiOrigin::parse(api_base(provider)).map_err(|_| {
        ForgeRefusal::UnreadableAddress(api_base(provider).to_owned())
    })?;
    identity_address_on(&base, provider)
}

/// Whose credential this is, asked OF THE ACCOUNT'S host rather than of a public default.
pub fn identity_address_on(
    base: &super::ApiOrigin,
    provider: Provider,
) -> Result<String, ForgeRefusal> {
    let capability = capability(provider, ForgeOperation::Identity)
        .ok_or(ForgeRefusal::UnsupportedOperation(ForgeOperation::Identity.name()))?;
    Ok(format!("{}{}", base.base(), capability.endpoint))
}

/// The login a provider reports, read from ITS OWN field. Providers differ here, so a shared guess
/// would silently read nothing and appear to confirm an identity it never saw.
#[must_use]
pub fn identity_login(provider: Provider, body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let field = match provider {
        Provider::GitHub | Provider::Gitea | Provider::Forgejo => "login",
        Provider::GitLab => "username",
    };
    value.get(field)?.as_str().map(str::to_owned)
}

/// The header a provider expects, and the value it expects it in. Providers differ here, and the
/// difference is the provider's own rather than a house style.
#[must_use]
pub fn auth_header(provider: Provider, credential: &Credential) -> (&'static str, String) {
    match provider {
        Provider::GitHub => ("authorization", format!("Bearer {}", credential.expose())),
        Provider::GitLab => ("private-token", credential.expose().to_owned()),
        Provider::Gitea | Provider::Forgejo => {
            ("authorization", format!("token {}", credential.expose()))
        }
    }
}

/// The four outcomes W-23 keeps apart: "Unsupported, denied, unknown and failed are four distinct
/// outcomes." They are distinct here because they arise from DIFFERENT causes, not because one
/// error type carries four strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgeOutcome {
    /// This provider does not offer the operation at all. Decided before a request is built.
    Unsupported(&'static str),
    /// The credential or the permission was rejected, carrying the provider's ACTUAL response.
    Denied(String),
    /// The provider did not state the fact being asked about.
    Unknown(String),
    /// The request failed, carrying the provider's own error text.
    Failed(String),
}

/// What a provider's error status means, keeping a denial apart from a failure and from an
/// operation the provider does not implement. A `404` is deliberately NOT "unsupported": it can
/// equally mean the repository or review does not exist, so it is not evidence about capability.
#[must_use]
pub fn error_outcome(status: u16, body: &str) -> ForgeOutcome {
    match status {
        401 | 403 => ForgeOutcome::Denied(body.to_owned()),
        405 | 501 => ForgeOutcome::Unsupported("the provider does not implement this operation"),
        400..=599 => ForgeOutcome::Failed(body.to_owned()),
        _ => ForgeOutcome::Unknown(format!(
            "the provider answered {status} without stating an outcome"
        )),
    }
}

/// Whether the reviewed revision can be merged, with "unknown" kept as its own answer rather than
/// folded into "no": the contract requires that "unknown mergeability is shown as unknown".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mergeability {
    Mergeable,
    Conflicted,
    Unknown,
}

#[must_use]
pub const fn mergeability(reported: Option<bool>) -> Mergeability {
    match reported {
        Some(true) => Mergeability::Mergeable,
        Some(false) => Mergeability::Conflicted,
        None => Mergeability::Unknown,
    }
}
