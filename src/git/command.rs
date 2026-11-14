//! Environment for Git commands scoped to the selected repository.
use std::collections::BTreeMap;
use std::ffi::OsString;

pub(super) fn environment() -> BTreeMap<String, OsString> {
    let mut environment = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
        .collect::<BTreeMap<_, _>>();
    // Inherited directory/index overrides must not retarget the selected repository.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        environment.remove(key);
    }
    environment.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    environment.insert("GIT_PAGER".into(), "cat".into());
    environment
}
