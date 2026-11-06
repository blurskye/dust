//! Startup configuration migration through the command line.

#[test]
fn migration_reads_regular_files_within_the_limit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "[editor]\ntab_width = 4\n").unwrap();
    let migrate = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_dust"))
            .arg("--migrate-config")
            .arg(&path)
            .output()
            .unwrap()
    };
    let output = migrate();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("tab_width = 4")
    );
    std::fs::File::create(&path)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    let output = migrate();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("exceeds migration limit")
    );
}

#[cfg(unix)]
#[test]
fn migration_rejects_special_files() {
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;
    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("config.toml");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let link = root.path().join("device.toml");
    std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
    for path in [
        fifo.as_path(),
        link.as_path(),
        std::path::Path::new("/dev/zero"),
        root.path(),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_dust"))
            .arg("--migrate-config")
            .arg(path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("must be a regular file")
        );
    }
}

#[cfg(unix)]
#[test]
fn startup_errors_escape_filesystem_terminal_controls() {
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;

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
    assert!(
        !error.contains('\x1b') && !error.contains('\x07'),
        "{error:?}"
    );
    assert!(error.contains("\\u{1b}]0;untrusted\\u{7}"), "{error}");
}
