#![cfg(unix)]
//! The provider contract (FORGE.md §8.3): one scenario suite per provider, generated for each
//! transport by `contract!`, against the stateful fake forge. Test names are
//! `<provider>_<transport>::<scenario>`; `<provider>_transports_agree` checks that direct HTTP and
//! CLI passthrough produce the same domain values and the same logical requests.
//!
//! GitLab (F5) runs over HTTP (`PRIVATE-TOKEN`) and `glab api` passthrough
//! (`tests/fixtures/forge/fake_glab.py`) against `server.py --world gitlab`.

#[path = "support/forge_fake.rs"]
mod forge_fake;

#[path = "forge_contract/gitlab.rs"]
mod gitlab;

/// `contract!(module: harness; scenario, …)` runs every scenario against a fresh harness.
macro_rules! contract {
    ($module:ident: $harness:expr; $($scenario:ident),* $(,)?) => {
        mod $module {
            $(
                #[test]
                fn $scenario() {
                    super::gitlab::isolated(
                        concat!(stringify!($module), "::", stringify!($scenario)),
                        || super::gitlab::$scenario(&$harness),
                    );
                }
            )*
        }
    };
}

contract!(gitlab_http: super::gitlab::Harness::new(super::gitlab::Via::Http);
    rows_and_terms,
    query_translation,
    pagination,
    merge_request_detail,
    issue_detail,
    threads_and_timeline,
    diff_has_git_headers,
    comment_labels_and_state,
    threads_reply_and_resolve,
    review_batches_drafts_then_approves,
    review_without_draft_notes,
    request_changes_is_probed,
    request_changes_unavailable_sends_nothing,
    merge_pins_head,
    merge_waits_for_running_pipeline,
    pipelines,
    pipeline_writes,
    todos,
    unsupported_writes_send_nothing,
    read_only_navigation_makes_no_writes,
    nested_group_project_ids,
    conversation_timelines,
);

contract!(gitlab_glab: super::gitlab::Harness::new(super::gitlab::Via::Glab);
    rows_and_terms,
    query_translation,
    pagination,
    merge_request_detail,
    issue_detail,
    threads_and_timeline,
    diff_has_git_headers,
    comment_labels_and_state,
    threads_reply_and_resolve,
    review_batches_drafts_then_approves,
    review_without_draft_notes,
    request_changes_is_probed,
    request_changes_unavailable_sends_nothing,
    merge_pins_head,
    merge_waits_for_running_pipeline,
    pipelines,
    pipeline_writes,
    todos,
    unsupported_writes_send_nothing,
    read_only_navigation_makes_no_writes,
    nested_group_project_ids,
    conversation_timelines,
);

#[test]
fn gitlab_transports_agree() {
    gitlab::isolated("gitlab_transports_agree", gitlab::transports_agree);
}
