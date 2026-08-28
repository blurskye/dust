use super::*;

pub(in crate::editor::session) fn make_logs(
    config: &crate::config::Config,
    persist: bool,
) -> StructuredLogs {
    let policy = &config.observability;
    StructuredLogs::new(
        LogPolicy {
            maximum_records: policy.maximum_records,
            maximum_field_bytes: policy.maximum_field_bytes,
            maximum_file_bytes: policy.maximum_file_bytes,
            rotations: policy.rotations,
            sensitive_keys: policy.sensitive_keys.clone(),
        },
        (persist && policy.enabled)
            .then(|| crate::config::user_state_directory().join(&policy.file_name)),
        policy.queue_capacity,
    )
}
