//! Opening files in a running Dust from programs it starts, such as lazygit's edit key.
//! Dust listens on a Unix socket named in `DUST_SERVER`; `dust --remote FILE` sends one
//! line per file, `<line>\t<absolute path>` with line 0 when none was given, and exits.

use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

/// The variable terminals started by Dust find the socket in.
pub const SERVER_VARIABLE: &str = "DUST_SERVER";
const REQUEST_BYTE_CEILING: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteOpen {
    pub path: PathBuf,
    /// 1-based.
    pub line: Option<usize>,
}

pub struct RemoteServer {
    path: PathBuf,
    requests: mpsc::Receiver<RemoteOpen>,
}

impl RemoteServer {
    /// Listen in `directory`, which should be private to the user.
    pub fn start(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join(format!("dust-{}.sock", std::process::id()));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let (sender, requests) = mpsc::channel();
        thread::Builder::new()
            .name("dust-remote".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    // A client that never writes must not hold up the next one.
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let reader = BufReader::new(stream.take(REQUEST_BYTE_CEILING));
                    for line in reader.lines().map_while(Result::ok) {
                        if let Some(request) = parse(&line)
                            && sender.send(request).is_err()
                        {
                            return;
                        }
                    }
                }
            })?;
        Ok(Self { path, requests })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn try_request(&self) -> Option<RemoteOpen> {
        self.requests.try_recv().ok()
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn parse(line: &str) -> Option<RemoteOpen> {
    let (number, path) = line.split_once('\t')?;
    let path = PathBuf::from(path);
    path.is_absolute().then(|| RemoteOpen {
        path,
        line: number.parse().ok().filter(|line| *line > 0),
    })
}

/// Ask the Dust at `server` to open `paths`; `line` applies to the first.
pub fn send(server: &Path, paths: &[PathBuf], line: Option<usize>) -> io::Result<()> {
    let mut stream = UnixStream::connect(server)?;
    let mut message = String::new();
    for (index, path) in paths.iter().enumerate() {
        let line = if index == 0 { line.unwrap_or(0) } else { 0 };
        message.push_str(&format!("{line}\t{}\n", std::path::absolute(path)?.display()));
    }
    stream.write_all(message.as_bytes())
}

/// Write a lazygit config whose edit keys open files in this Dust, and return the
/// `LG_CONFIG_FILE` value that layers it over the user's own lazygit config.
pub fn lazygit_config(directory: &Path, dust: &Path) -> io::Result<String> {
    let program = format!("\"{}\"", dust.display()).replace('\'', "''");
    let overlay = directory.join("lazygit.yml");
    fs::write(
        &overlay,
        format!(
            "# Written by Dust: lazygit's edit keys open files in the Dust that started it.\n\
             os:\n  \
             edit: '{program} --remote {{{{filename}}}}'\n  \
             editAtLine: '{program} --remote {{{{filename}}}} --line {{{{line}}}}'\n  \
             editAtLineAndWait: '{program} --remote {{{{filename}}}} --line {{{{line}}}}'\n"
        ),
    )?;
    let user = std::env::var("LG_CONFIG_FILE")
        .ok()
        .filter(|files| !files.is_empty())
        .or_else(|| {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
            let file = config.join("lazygit/config.yml");
            file.is_file().then(|| file.display().to_string())
        });
    Ok(match user {
        Some(files) => format!("{files},{}", overlay.display()),
        None => overlay.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sent_file_arrives_with_its_line() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let file = directory.path().join("a.rs");
        send(server.path(), &[file.clone(), PathBuf::from("/b.rs")], Some(12)).unwrap();
        let mut received = Vec::new();
        for _ in 0..200 {
            received.extend(server.try_request());
            if received.len() == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            received,
            [
                RemoteOpen { path: file, line: Some(12) },
                RemoteOpen { path: "/b.rs".into(), line: None },
            ]
        );
        assert!(parse("3\trelative.rs").is_none());
    }
}
