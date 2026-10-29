//! The four adapters, the capability matrix and the four outcome states. Nothing here reaches a
//! network: addresses are BUILT and compared, which is exactly what an adapter can be asked about
//! without a server.

use super::providers::{
    ForgeOperation, ForgeOutcome, Mergeability, Reference, auth_header, address, capability,
    error_outcome, mergeability,
};
use super::*;

fn account() -> (&'static str, &'static str) {
    ("dust-dev", "dust-editor")
}

fn credential() -> Credential {
    Credential::new(CredentialSource::Store, "forge", "ghp_supersecret")
}

const PROVIDERS: [Provider; 4] = [Provider::GitHub, Provider::GitLab, Provider::Gitea, Provider::Forgejo];

#[test]
fn the_same_operation_builds_a_different_address_for_each_provider() {
    let mut addresses = Vec::new();
    for provider in PROVIDERS {
        let built = address(
            provider,
            ForgeOperation::Reviews,
            account().0,
            account().1,
            Reference::Number(1),
        )
        .expect("every provider lists reviews");
        addresses.push(built);
    }
    // Independence is observable: four DIFFERENT addresses, not one shape wearing four names.
    for (index, first) in addresses.iter().enumerate() {
        for second in addresses.iter().skip(index + 1) {
            assert_ne!(first, second, "two providers built the same address: {first}");
        }
    }
    assert!(addresses[0].contains("api.github.com/repos/dust-dev/dust-editor/pulls"));
    assert!(addresses[1].contains("/api/v4/projects/"));
    assert!(addresses[2].contains("gitea.com/api/v1/repos/"));
    assert!(addresses[3].contains("codeberg.org/api/v1/repos/"));
}

#[test]
fn gitlab_addresses_the_project_by_its_encoded_path() {
    // A provider-specific difference that a shared shape would have flattened.
    let gitlab = address(
        Provider::GitLab,
        ForgeOperation::Issues,
        "group/subgroup",
        "project",
        Reference::Number(1),
    )
    .expect("gitlab lists issues");
    assert!(
        gitlab.contains("group%2Fsubgroup%2Fproject"),
        "the GitLab project path is encoded: {gitlab}"
    );
    let github = address(
        Provider::GitHub,
        ForgeOperation::Issues,
        "group/subgroup",
        "project",
        Reference::Number(1),
    )
    .expect("github lists issues");
    assert!(!github.contains("%2F"), "GitHub uses path segments: {github}");
}

#[test]
fn each_provider_authenticates_in_its_own_way() {
    let credential = credential();
    let github = auth_header(Provider::GitHub, &credential);
    let gitlab = auth_header(Provider::GitLab, &credential);
    let gitea = auth_header(Provider::Gitea, &credential);
    let forgejo = auth_header(Provider::Forgejo, &credential);
    assert_eq!(github.0, "authorization");
    assert_eq!(github.1, "Bearer ghp_supersecret");
    assert_eq!(gitlab.0, "private-token", "GitLab names its own header");
    assert_eq!(gitlab.1, "ghp_supersecret");
    assert_eq!(gitea.1, "token ghp_supersecret");
    assert_eq!(forgejo.1, "token ghp_supersecret");
    // The three styles are genuinely different, not one style with a different prefix.
    assert_ne!(github, gitlab);
}

#[test]
fn an_operation_a_provider_does_not_support_is_refused_rather_than_approximated() {
    // GitLab has no single-review resource: notes and approvals are separate, so there is nothing
    // honest to build.
    assert_eq!(
        address(Provider::GitLab, ForgeOperation::Review, "a", "r", Reference::Number(1)),
        Err(ForgeRefusal::UnsupportedOperation("review"))
    );
    // Forgejo reports commit statuses per commit, not a combined check run.
    assert_eq!(
        address(Provider::Forgejo, ForgeOperation::Checks, "a", "r", Reference::Revision("abc")),
        Err(ForgeRefusal::UnsupportedOperation("checks"))
    );
    // And an operation addressed with the wrong KIND of reference is refused rather than built.
    assert_eq!(
        address(Provider::GitHub, ForgeOperation::Checks, "a", "r", Reference::Number(1)),
        Err(ForgeRefusal::UnsupportedOperation("checks"))
    );
}

#[test]
fn capability_is_only_ever_asked_of_one_provider() {
    // The observable form of "never in aggregate": the same question has no single answer, and the
    // absent cell is absent rather than filled in from a neighbour.
    assert!(capability(Provider::GitLab, ForgeOperation::Review).is_none());
    assert!(capability(Provider::GitHub, ForgeOperation::Review).is_some());
    assert!(capability(Provider::Forgejo, ForgeOperation::Checks).is_none());
    assert!(capability(Provider::Gitea, ForgeOperation::Checks).is_some());
}

#[test]
fn the_matrix_and_the_adapters_agree_in_every_cell() {
    // Anti-drift: every cell either builds the address the matrix records, or is refused, so the
    // matrix cannot quietly become a wish.
    let mut built = 0;
    for provider in PROVIDERS {
        for operation in ForgeOperation::ALL {
            let row = capability(provider, operation);
            let reference = if matches!(operation, ForgeOperation::Checks) {
                Reference::Revision("deadbeef")
            } else {
                Reference::Number(7)
            };
            let address = address(provider, operation, account().0, account().1, reference);
            match row {
                Some(row) => {
                    let built_address = address.expect("a recorded capability builds");
                    assert!(
                        built_address.ends_with(row.endpoint.replace("{project}", &format!(
                            "{}%2F{}",
                            account().0,
                            account().1
                        ))
                        .replace("{account}", account().0)
                        .replace("{repository}", account().1)
                        .replace("{number}", "7")
                        .replace("{revision}", "deadbeef")
                        .as_str()),
                        "{provider:?} {operation:?} built {built_address} but the matrix records {}",
                        row.endpoint
                    );
                    assert!(!row.versions.is_empty(), "a version claim is stated");
                    assert!(!row.permission.is_empty(), "a permission claim is stated");
                    built += 1;
                }
                None => assert_eq!(
                    address,
                    Err(ForgeRefusal::UnsupportedOperation(operation.name())),
                    "an unsupported cell is refused"
                ),
            }
        }
    }
    assert!(built >= 30, "the matrix covers the four providers' operations: {built}");
}

#[test]
fn no_capability_row_is_claimed_as_server_verified_yet() {
    // The honest gap, recorded rather than implied: passing tests prove the ADDRESS is built from
    // the documented path, and nothing here has spoken to a real server. A row may only stop
    // saying `DocumentationOnly` when something actually verified it.
    for provider in PROVIDERS {
        for operation in ForgeOperation::ALL {
            if let Some(row) = capability(provider, operation) {
                assert_eq!(
                    row.verification,
                    super::providers::Verification::DocumentationOnly,
                    "{provider:?} {operation:?} claims more verification than it has"
                );
            }
        }
    }
}

#[test]
fn the_four_outcomes_arise_from_four_different_causes() {
    // A single catch-all error type cannot pass this: each state comes from its own cause.
    assert_eq!(
        error_outcome(403, "resource not accessible by token"),
        ForgeOutcome::Denied("resource not accessible by token".into()),
        "a rejected credential is DENIED, and reports the provider's actual response"
    );
    assert_ne!(
        error_outcome(403, "denied"),
        ForgeOutcome::Failed("denied".into()),
        "a denial is not a failure"
    );
    assert_eq!(
        error_outcome(501, "not implemented"),
        ForgeOutcome::Unsupported("the provider does not implement this operation")
    );
    assert_eq!(
        error_outcome(500, "internal error"),
        ForgeOutcome::Failed("internal error".into())
    );
    // A 404 is NOT capability evidence, because it equally means "no such review".
    assert_eq!(
        error_outcome(404, "not found"),
        ForgeOutcome::Failed("not found".into())
    );
    // UNKNOWN has its own cause: a fact the provider does not state.
    assert_eq!(mergeability(None), Mergeability::Unknown);
    assert_eq!(mergeability(Some(true)), Mergeability::Mergeable);
    assert_eq!(mergeability(Some(false)), Mergeability::Conflicted);
    assert_ne!(
        Mergeability::Unknown,
        Mergeability::Conflicted,
        "unknown mergeability is shown as unknown, not as unmergeable"
    );
}
