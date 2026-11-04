#[cfg(unix)]
#[test]
fn startup_errors_escape_filesystem_terminal_controls() {
    use nix::{sys::stat::Mode, unistd::mkfifo};

    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("pipe\x1b]0;untrusted\x07");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dust"))
        .args(["--clean", "--print-startup"])
        .arg(fifo)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(!error.contains('\x1b') && !error.contains('\x07'), "{error:?}");
    assert!(error.contains("\\u{1b}]0;untrusted\\u{7}"), "{error}");
}
