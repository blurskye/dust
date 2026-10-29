//! The shared forge surface and merge state (W-23).
//!
//! The contract: "Reviews, issues and checks share one forge surface whose header names provider,
//! host, repository and account; before an account is connected the surface offers connection
//! rather than presenting empty results. Lists support the filters applicable to each provider,
//! including authored, assigned and review-requested items." ... "Merge states the reviewed head
//! and the method the server supports, and unknown mergeability is shown as unknown."
//!
//! This is a STATE MODEL. No TUI surface renders it yet, and nothing here claims otherwise.

use super::accounts::ForgeAccount;
use super::Provider;
use super::providers::{Mergeability, mergeability};

/// The three things the one surface shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeList {
    Reviews,
    Issues,
    Checks,
}

impl ForgeList {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reviews => "reviews",
            Self::Issues => "issues",
            Self::Checks => "checks",
        }
    }

    pub const ALL: [Self; 3] = [Self::Reviews, Self::Issues, Self::Checks];
}

/// The list filters the contract names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListFilter {
    Authored,
    Assigned,
    ReviewRequested,
}

impl ListFilter {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Authored => "authored",
            Self::Assigned => "assigned",
            Self::ReviewRequested => "review-requested",
        }
    }

    pub const ALL: [Self; 3] = [Self::Authored, Self::Assigned, Self::ReviewRequested];
}

/// The header: WHOSE surface this is. Every field comes from the connected account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceHeader {
    pub provider: Provider,
    pub host: String,
    pub repository: String,
    pub account: String,
}

/// What the surface can say about a list.
///
/// There is deliberately NO variant that carries an empty list for a surface with no account: a
/// disconnected surface cannot express "no results", only "connect first".
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SurfaceAnswer {
    /// No account is connected: the surface OFFERS CONNECTION.
    NeedsConnection { providers: Vec<Provider> },
    /// This provider does not support that list or filter.
    Unsupported(&'static str),
    /// The request this surface would make, with the provider's OWN parameter names.
    Ready {
        provider: Provider,
        parameters: Vec<(String, String)>,
    },
}

/// The one surface for reviews, issues and checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgeSurface {
    /// Nothing is connected. There is no header and there are no lists.
    Disconnected { providers: Vec<Provider> },
    Connected { account: ForgeAccount },
}

impl ForgeSurface {
    /// A surface with no account, offering every provider it knows how to connect to.
    #[must_use]
    pub fn disconnected() -> Self {
        Self::Disconnected {
            providers: vec![
                Provider::GitHub,
                Provider::GitLab,
                Provider::Gitea,
                Provider::Forgejo,
            ],
        }
    }

    /// A surface bound to an account that the provider has already CONFIRMED.
    #[must_use]
    pub fn connected(account: ForgeAccount) -> Self {
        Self::Connected { account }
    }

    /// The header, which exists only once there is an account to name.
    #[must_use]
    pub fn header(&self) -> Option<SurfaceHeader> {
        match self {
            Self::Disconnected { .. } => None,
            Self::Connected { account } => Some(SurfaceHeader {
                provider: account.provider,
                host: account.host.clone(),
                repository: account.repository.clone(),
                account: account.account.clone(),
            }),
        }
    }

    /// Ask the surface for a list. A surface with no account ANSWERS WITH CONNECTION rather than
    /// with an empty collection, because an empty list would be a claim about a repository the
    /// editor has not even been granted access to.
    #[must_use]
    pub fn lists(&self, list: ForgeList, filter: Option<ListFilter>) -> SurfaceAnswer {
        let Self::Connected { account } = self else {
            let Self::Disconnected { providers } = self else {
                unreachable!()
            };
            return SurfaceAnswer::NeedsConnection {
                providers: providers.clone(),
            };
        };
        match list_support(account.provider, list) {
            false => SurfaceAnswer::Unsupported(list.name()),
            true => match filter {
                Some(filter) => match list_parameters(account.provider, filter) {
                    Some((name, kind)) => SurfaceAnswer::Ready {
                        provider: account.provider,
                        parameters: vec![(
                            name.to_owned(),
                            match kind {
                                FilterField::Account => account.account.clone(),
                                FilterField::Literal => "true".to_owned(),
                            },
                        )],
                    },
                    None => SurfaceAnswer::Unsupported(filter.name()),
                },
                None => SurfaceAnswer::Ready {
                    provider: account.provider,
                    parameters: Vec::new(),
                },
            },
        }
    }
}

/// Which side of the account a filter parameter takes its value from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilterField {
    Account,
    /// The provider expresses this filter as a flag rather than as an account.
    Literal,
}

/// Does this provider offer this list at all? Checks are the one that differs.
fn list_support(provider: Provider, list: ForgeList) -> bool {
    match (provider, list) {
        // Forgejo reports per-commit statuses rather than a combined check surface, exactly as its
        // capability row says.
        (Provider::Forgejo, ForgeList::Checks) => false,
        _ => true,
    }
}

/// The parameter a provider uses for a filter. `None` means that provider does not offer it - and
/// a filter that is not offered is refused rather than sent under a name the provider ignores.
fn list_parameters(
    provider: Provider,
    filter: ListFilter,
) -> Option<(&'static str, FilterField)> {
    match (provider, filter) {
        (Provider::GitHub, ListFilter::Authored) => Some(("author", FilterField::Account)),
        (Provider::GitHub, ListFilter::Assigned) => Some(("assignee", FilterField::Account)),
        (Provider::GitHub, ListFilter::ReviewRequested) => {
            Some(("review-requested", FilterField::Account))
        }
        (Provider::GitLab, ListFilter::Authored) => {
            Some(("author_username", FilterField::Account))
        }
        (Provider::GitLab, ListFilter::Assigned) => {
            Some(("assignee_username", FilterField::Account))
        }
        // GitLab has no documented username filter for "review requested", so it is refused.
        (Provider::GitLab, ListFilter::ReviewRequested) => None,
        (Provider::Gitea | Provider::Forgejo, ListFilter::Authored) => {
            Some(("created_by", FilterField::Account))
        }
        (Provider::Gitea | Provider::Forgejo, ListFilter::Assigned) => {
            Some(("assigned_by", FilterField::Account))
        }
        // Review-requested is expressed as a flag rather than an account on these two.
        (Provider::Gitea | Provider::Forgejo, ListFilter::ReviewRequested) => {
            Some(("review_requested", FilterField::Literal))
        }
    }
}

/// What the server said it supports for merging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// Read the method the server REPORTED. An unrecognised or absent method stays unknown rather
    /// than defaulting to a merge the server may not offer.
    #[must_use]
    pub fn reported(method: Option<&str>) -> Option<Self> {
        match method? {
            "merge" => Some(Self::Merge),
            "squash" => Some(Self::Squash),
            "rebase" => Some(Self::Rebase),
            _ => None,
        }
    }
}

/// The merge state of the REVIEWED revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeState {
    /// The head that was reviewed, named so a merge cannot be described without saying what it is
    /// merging.
    pub reviewed_head: String,
    /// The method the server supports, or nothing when it did not state one.
    pub method: Option<MergeMethod>,
    pub mergeability: Mergeability,
}

/// State the merge of a reviewed revision.
#[must_use]
pub fn merge_state(
    reviewed_head: &str,
    reported_method: Option<&str>,
    reported_mergeable: Option<bool>,
) -> MergeState {
    MergeState {
        reviewed_head: reviewed_head.to_owned(),
        method: MergeMethod::reported(reported_method),
        mergeability: mergeability(reported_mergeable),
    }
}
