//! Bounded downloads: hosts are NAMED, bounds are bounded, and nothing is extracted or executed.

use super::download::{DownloadPlan, DownloadRefusal};
use super::*;

#[test]
fn a_wildcard_or_empty_host_list_is_refused_rather_than_honoured() {
    assert_eq!(
        DownloadPlan::new(
            "https://objects.example/log.txt",
            &["*.example.com"],
            1024
        ),
        Err(DownloadRefusal::WildcardHost("*.example.com".into())),
        "a wildcard is a claim to follow anywhere, not an explicit name"
    );
    assert_eq!(
        DownloadPlan::new("https://objects.example/log.txt", &[], 1024),
        Err(DownloadRefusal::NoAllowedHosts)
    );
    assert_eq!(
        DownloadPlan::new("http://objects.example/log.txt", &["objects.example"], 1024),
        Err(DownloadRefusal::UnreadableAddress(
            "http://objects.example/log.txt".into()
        )),
        "a download is HTTPS like every other forge request"
    );
    assert_eq!(
        DownloadPlan::new(
            "https://objects.example/log.txt",
            &["objects.example"],
            MAX_RESPONSE_BYTES + 1
        ),
        Err(DownloadRefusal::BoundTooLarge),
        "a plan cannot ask for more than the transport will ever hand over"
    );
}

#[test]
fn a_plan_names_exactly_the_hosts_it_was_given_and_nothing_else() {
    let plan = DownloadPlan::new(
        "https://objects.example/artifact.tar.gz",
        &["objects.example", "cdn.example"],
        4096,
    )
    .expect("an explicit plan is admitted");
    assert_eq!(plan.allowed_hosts(), ["objects.example", "cdn.example"]);
    assert_eq!(plan.max_bytes, 4096);
    // The transport takes the SAME list, so a redirect to anything else is refused there rather
    // than here - one rule, one place.
    let without_named_host = DownloadPlan::new(
        "https://objects.example/artifact.tar.gz",
        &["objects.example"],
        4096,
    )
    .unwrap();
    assert_eq!(
        without_named_host.allowed_hosts(),
        ["objects.example"],
        "a host that was not named is not followed"
    );
}
