//! Executable file checks.

use std::path::Path;
pub(crate) fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        rustix::fs::accessat(
            rustix::fs::CWD,
            path,
            rustix::fs::Access::EXEC_OK,
            rustix::fs::AtFlags::EACCESS,
        )
        .is_ok()
    }
    #[cfg(not(unix))]
    true
}
use std::fs;

/// The extensions Windows tries, in order, for a bare command name, lowercased.
#[cfg(windows)]
pub(crate) fn path_extensions() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|value| value.starts_with('.'))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Search PATH without invoking a shell; Windows also searches PATHEXT.
pub fn discover_executable(executable: &str) -> Option<std::path::PathBuf> {
    let candidate = Path::new(executable);
    let find = |path: std::path::PathBuf| {
        // Windows cannot start an extensionless file, such as the `sh` shim npm writes
        // beside `tool.cmd`, so a bare name only matches through PATHEXT there.
        #[cfg(windows)]
        if path.extension().is_none() {
            return path_extensions().into_iter().find_map(|extension| {
                let mut name = path.as_os_str().to_owned();
                name.push(extension);
                let candidate = std::path::PathBuf::from(name);
                executable_file(&candidate).then_some(candidate)
            });
        }
        executable_file(&path).then_some(path)
    };
    if candidate.components().count() > 1 {
        return find(std::path::absolute(candidate).ok()?);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .find_map(|directory| find(std::path::absolute(directory.join(executable)).ok()?))
}

pub(crate) fn terminal_executable(
    program: &Path,
    cwd: &Path,
) -> std::io::Result<std::path::PathBuf> {
    if program == Path::new("$SHELL") {
        let defaults: toml::Value = toml::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/defaults/shells.toml"
        )))
        .expect("bundled shells");
        #[cfg(unix)]
        {
            if let Some(shell) = std::env::var_os("SHELL").filter(|value| !value.is_empty()) {
                let path = std::path::PathBuf::from(shell);
                if executable_file(&path) {
                    return Ok(path);
                }
            }
            for shell in defaults["unix"].as_array().expect("Unix shells") {
                if let Some(path) = discover_executable(shell.as_str().expect("shell name")) {
                    return Ok(path);
                }
            }
        }
        #[cfg(windows)]
        {
            for shell in defaults["windows"].as_array().expect("Windows shells") {
                let name = shell.as_str().expect("shell name");
                if name.eq_ignore_ascii_case("cmd.exe")
                    && let Some(path) = std::env::var_os("ComSpec")
                        .map(std::path::PathBuf::from)
                        .filter(|path| executable_file(path))
                {
                    return Ok(path);
                }
                if let Some(path) = discover_executable(name) {
                    return Ok(path);
                }
            }
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no default shell is available",
        ));
    }
    if program.is_absolute() {
        return Ok(program.into());
    }
    if program.components().count() == 1
        && let Some(path) = program.to_str().and_then(discover_executable)
    {
        return Ok(path);
    }
    Ok(cwd.join(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_is_an_existing_executable() {
        let cwd = std::env::current_dir().unwrap();
        let shell = terminal_executable(Path::new("$SHELL"), &cwd).unwrap();
        assert!(shell.is_absolute());
        assert!(executable_file(&shell));
    }

    #[test]
    fn explicit_relative_program_stays_relative_to_the_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            terminal_executable(Path::new("./local-tool"), &cwd).unwrap(),
            cwd.join("./local-tool")
        );
    }
}
