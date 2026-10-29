//! The shared forge surface, its filters and its merge state. Nothing here reaches a network.

use super::accounts::ForgeAccount;
use super::Provider;
use super::providers::Mergeability;
use super::surface::{
    ForgeList, ForgeSurface, ListFilter, MergeMethod, SurfaceAnswer, SurfaceHeader, merge_state,
};
use super::*;

fn account(provider: Provider) -> ForgeAccount {
    ForgeAccount {
        provider,
        host: "forge.example".into(),
        api_base: ApiOrigin::parse("https://forge.example").unwrap(),
        account: "dust-dev".into(),
        repository: "dust-editor".into(),
    }
}

#[test]
fn a_disconnected_surface_offers_connection_and_cannot_return_an_empty_list() {
    let surface = ForgeSurface::disconnected();
    // There is no header, because there is no account to name.
    assert_eq!(surface.header(), None);
    for list in ForgeList::ALL {
        // THE ASSERTION THAT MATTERS: the answer is a CONNECTION OFFER, not a collection. An empty
        // list would be a claim about a repository the editor has not been granted access to.
        match surface.lists(list, None) {
            SurfaceAnswer::NeedsConnection { providers } => {
                assert_eq!(providers.len(), 4, "every provider is offered for connection");
            }
            other => panic!("a disconnected surface must offer connection, not {other:?}"),
        }
    }
    // A filter does not change the answer into a list either.
    assert!(matches!(
        surface.lists(ForgeList::Reviews, Some(ListFilter::Authored)),
        SurfaceAnswer::NeedsConnection { .. }
    ));
}

#[test]
fn the_header_names_the_account_the_surface_is_showing() {
    let surface = ForgeSurface::connected(account(Provider::GitLab));
    assert_eq!(
        surface.header(),
        Some(SurfaceHeader {
            provider: Provider::GitLab,
            host: "forge.example".into(),
            repository: "dust-editor".into(),
            account: "dust-dev".into(),
        }),
        "all four header fields come from the connected account"
    );
}

#[test]
fn the_same_filter_maps_to_each_provider_s_own_parameter() {
    let authored: Vec<SurfaceAnswer> = [Provider::GitHub, Provider::GitLab, Provider::Gitea]
        .into_iter()
        .map(|provider| {
            ForgeSurface::connected(account(provider))
                .lists(ForgeList::Reviews, Some(ListFilter::Authored))
        })
        .collect();
    let names: Vec<String> = authored
        .iter()
        .map(|answer| match answer {
            SurfaceAnswer::Ready { parameters, .. } => parameters[0].0.clone(),
            other => panic!("expected a prepared request, got {other:?}"),
        })
        .collect();
    assert_eq!(names, ["author", "author_username", "created_by"]);
    // The VALUE comes from the account, so the filter cannot be sent for someone else.
    for answer in &authored {
        if let SurfaceAnswer::Ready { parameters, .. } = answer {
            assert_eq!(parameters[0].1, "dust-dev");
        }
    }
    // And a filter that is a FLAG rather than an account is sent as one.
    match ForgeSurface::connected(account(Provider::Forgejo))
        .lists(ForgeList::Reviews, Some(ListFilter::ReviewRequested))
    {
        SurfaceAnswer::Ready { parameters, .. } => assert_eq!(parameters[0].1, "true"),
        other => panic!("expected a prepared request, got {other:?}"),
    }
}

#[test]
fn a_list_or_filter_a_provider_does_not_offer_is_refused_rather_than_approximated() {
    let gitlab = ForgeSurface::connected(account(Provider::GitLab));
    assert_eq!(
        gitlab.lists(ForgeList::Reviews, Some(ListFilter::ReviewRequested)),
        SurfaceAnswer::Unsupported("review-requested"),
        "GitLab has no documented review-requested filter, so it is refused"
    );
    let forgejo = ForgeSurface::connected(account(Provider::Forgejo));
    assert_eq!(
        forgejo.lists(ForgeList::Checks, None),
        SurfaceAnswer::Unsupported("checks"),
        "Forgejo reports per-commit statuses rather than a combined check surface"
    );
    // The control: the same list IS offered by a provider that has it.
    assert!(matches!(
        ForgeSurface::connected(account(Provider::GitHub)).lists(ForgeList::Checks, None),
        SurfaceAnswer::Ready { .. }
    ));
}

#[test]
fn a_merge_names_the_reviewed_head_and_a_method_that_was_actually_reported() {
    let stated = merge_state("abc123", Some("squash"), Some(true));
    assert_eq!(stated.reviewed_head, "abc123", "the reviewed head is named");
    assert_eq!(stated.method, Some(MergeMethod::Squash));
    assert_eq!(stated.mergeability, Mergeability::Mergeable);

    // A method the server did NOT state stays unknown rather than defaulting to a merge the server
    // may not offer, and unknown mergeability stays unknown rather than becoming unmergeable.
    let unstated = merge_state("abc123", None, None);
    assert_eq!(unstated.method, None);
    assert_eq!(unstated.mergeability, Mergeability::Unknown);
    let unrecognised = merge_state("abc123", Some("octopus"), Some(false));
    assert_eq!(
        unrecognised.method, None,
        "an unrecognised method is unknown, not assumed to be the default"
    );
    assert_eq!(unrecognised.mergeability, Mergeability::Conflicted);
}
