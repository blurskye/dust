//! A sealed forge environment for the Git tutor's practice forge (`src/git/screen/tutor`).
//!
//! While a sandbox is active, every forge setting the process would read from its environment
//! (`DUSTY_FORGE_*`, `GH_TOKEN` and the other tokens, `HOME` and the CLI sign-in stores) comes
//! from the sandbox's own variables instead; the user's `[forge]` config is ignored, no forge
//! CLI (gh, glab, tea) runs, the disk cache is off and no browser opens. So a practice lesson
//! reaches only its loopback fake forge and never sees the user's credentials. Entering and
//! leaving forget every credential, transport choice and detection made so far.
use crate::forge::transport::HostKey;
use std::collections::BTreeMap;
use std::sync::RwLock;

static SANDBOX: RwLock<Option<BTreeMap<String, String>>> = RwLock::new(None);

fn read() -> std::sync::RwLockReadGuard<'static, Option<BTreeMap<String, String>>> {
    SANDBOX.read().unwrap_or_else(|error| error.into_inner())
}

fn write() -> std::sync::RwLockWriteGuard<'static, Option<BTreeMap<String, String>>> {
    SANDBOX.write().unwrap_or_else(|error| error.into_inner())
}

/// Replace the forge environment with `vars` until `leave`.
pub fn enter(vars: BTreeMap<String, String>) {
    let previous = write().replace(vars);
    if let Some(previous) = previous {
        forget(&previous);
    }
    crate::forge::host_state::recheck();
}

/// Set (or with `None` remove) one variable of the active sandbox. Nothing is forgotten: a
/// new sign-in is noticed when the accounts popover re-checks (`r`), as outside.
pub fn set(name: &str, value: Option<&str>) {
    if let Some(vars) = write().as_mut() {
        match value {
            Some(value) => vars.insert(name.to_owned(), value.to_owned()),
            None => vars.remove(name),
        };
    }
}

/// Back to the process environment.
pub fn leave() {
    let previous = write().take();
    if let Some(previous) = previous {
        forget(&previous);
        crate::forge::host_state::recheck();
    }
}

fn forget(vars: &BTreeMap<String, String>) {
    if let Some(host) = vars.get("DUSTY_FORGE_HOST") {
        crate::forge::host_state::forget(&HostKey::new(host));
    }
}

pub fn active() -> bool {
    read().is_some()
}

/// The variable from the sandbox while one is active, else from the process environment.
pub fn var(name: &str) -> Option<String> {
    match &*read() {
        Some(vars) => vars.get(name).cloned(),
        None => std::env::var(name).ok(),
    }
}
