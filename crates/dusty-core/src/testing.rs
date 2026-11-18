//! Helpers for tests that run programs.

#[cfg(unix)]
use std::path::PathBuf;

/// Writes a script or program a test then runs, mode 755.
///
/// Tests run on many threads, and a file this process has open for writing cannot be
/// executed while a child another thread just forked still holds the inherited descriptor
/// ("Text file busy", ETXTBSY), however briefly the write lasted. So the file is written
/// under another name and put in place by `install`, whose descriptor dies with it.
#[cfg(unix)]
pub fn write_executable(path: impl AsRef<std::path::Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    let mut staging = path.as_os_str().to_owned();
    staging.push(".source");
    std::fs::write(&staging, contents).unwrap();
    let status = std::process::Command::new(install_program())
        .args(["-m", "755"])
        .arg(&staging)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "install {}", path.display());
    std::fs::remove_file(&staging).unwrap();
}

/// `install`, found by full path: some tests run in a child whose PATH they emptied or gave an
/// unreadable entry, so the PATH the tests were built with is searched as well.
#[cfg(unix)]
fn install_program() -> PathBuf {
    let runtime = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&runtime)
        .chain(std::env::split_paths(option_env!("PATH").unwrap_or("")))
        .chain(["/usr/bin".into(), "/bin".into()])
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("install"))
        .find(|candidate| candidate.is_file())
        .expect("install(1) for writing test executables")
}
