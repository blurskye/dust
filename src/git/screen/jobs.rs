//! Bounded Git CLI requests for history, previews and operations not owned by a Git worker.
use crate::process::{JobEvent, JobId, JobPolicy, JobRequest, JobStream, JobSupervisor};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Git jobs in flight across the process; the forge's cache-warming worker waits while this
/// is non-zero so it never competes with the user's Git work (FORGE.md §7.2).
static RUNNING: AtomicUsize = AtomicUsize::new(0);

pub(super) fn running() -> usize {
    RUNNING.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Request {
    History,
    Numstat,
    /// `diff --cached --numstat`: what the next commit contains.
    StagedNumstat,
    /// `diff --cached`: the commit sheet's staged patch.
    StagedPatch,
    Reflog,
    Stashes,
    Tags,
    Remotes,
    Files,
    Preview,
    CommitInfo,
    Write,
}

struct Pending {
    request: Request,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

pub(super) struct Jobs {
    supervisor: JobSupervisor,
    pending: HashMap<JobId, Pending>,
    preview: Option<JobId>,
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            supervisor: JobSupervisor::new(256),
            pending: HashMap::new(),
            preview: None,
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.pending.clear();
        self.count();
    }
}

impl Jobs {
    fn count(&self) {
        RUNNING.store(self.pending.len(), Ordering::Relaxed);
    }

    pub fn preview_loading(&self) -> bool {
        self.pending
            .values()
            .any(|pending| matches!(pending.request, Request::Preview | Request::CommitInfo))
    }

    pub fn cancel_reads(&mut self) {
        let ids = self
            .pending
            .iter()
            .filter_map(|(id, pending)| (pending.request != Request::Write).then_some(*id))
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.supervisor.cancel(id);
            self.pending.remove(&id);
        }
        self.preview = None;
        self.count();
    }

    pub fn cancel_preview(&mut self) {
        if let Some(id) = self.preview.take() {
            let _ = self.supervisor.cancel(id);
            self.pending.remove(&id);
        }
        self.count();
    }
    pub fn start(
        &mut self,
        root: &Path,
        request: Request,
        args: Vec<OsString>,
    ) -> Result<(), String> {
        self.start_with_input(root, request, args, None)
    }

    pub fn start_with_input(
        &mut self,
        root: &Path,
        request: Request,
        args: Vec<OsString>,
        stdin: Option<Vec<u8>>,
    ) -> Result<(), String> {
        self.start_full(root, request, args, stdin, &[], Duration::from_secs(30))
    }

    /// A write that adds variables to Git's environment (a rebase's todo, a reflog tag)
    /// and may run for minutes (a pull or a push over the network).
    pub fn start_write_env(
        &mut self,
        root: &Path,
        args: Vec<OsString>,
        env: &[(String, OsString)],
        timeout: Duration,
    ) -> Result<(), String> {
        self.start_full(root, Request::Write, args, None, env, timeout)
    }

    /// The last line a running write printed on stderr (Git's progress), if any.
    pub fn write_progress(&self) -> Option<String> {
        let pending = self.pending.values().find(|pending| pending.request == Request::Write)?;
        let text = String::from_utf8_lossy(&pending.stderr);
        text.split(['\r', '\n'])
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(str::to_owned)
    }

    fn start_full(
        &mut self,
        root: &Path,
        request: Request,
        args: Vec<OsString>,
        stdin: Option<Vec<u8>>,
        extra: &[(String, OsString)],
        timeout: Duration,
    ) -> Result<(), String> {
        if request == Request::Preview {
            self.cancel_preview();
        }
        if matches!(
            request,
            Request::History
                | Request::Numstat
                | Request::StagedNumstat
                | Request::StagedPatch
                | Request::Reflog
                | Request::CommitInfo
        ) {
            let ids = self
                .pending
                .iter()
                .filter_map(|(id, pending)| (pending.request == request).then_some(*id))
                .collect::<Vec<_>>();
            for id in ids {
                let _ = self.supervisor.cancel(id);
                self.pending.remove(&id);
            }
        }
        let mut arguments: Vec<OsString> = [
            "--no-pager",
            "-c",
            "color.ui=false",
            "-c",
            "core.quotepath=false",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if request != Request::Write {
            // Optional-lock suppression alone does not prevent diff's stat refresh
            // with all fsmonitor configurations. Reads must leave the index alone.
            arguments.extend(
                ["-c", "diff.autoRefreshIndex=false"]
                    .into_iter()
                    .map(Into::into),
            );
        }
        arguments.extend(args);
        let mut environment = crate::git::command::environment();
        if request != Request::Write {
            // Even a content-clean diff can refresh stat data in the index.
            environment.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
        }
        for (key, value) in extra {
            environment.insert(key.clone(), value.clone());
        }
        let id = self
            .supervisor
            .start(JobRequest {
                program: crate::process::discover_executable("git")
                    .ok_or("Git is not installed or not on PATH")?,
                argv0: None,
                arguments,
                cwd: root.into(),
                environment,
                root_revision: 0,
                stdin,
                policy: JobPolicy {
                    maximum_output_bytes: output_limit(request),
                    timeout,
                    ..JobPolicy::default()
                },
            })
            .map_err(|e| e.to_string())?;
        self.pending.insert(
            id,
            Pending {
                request,
                stdout: Vec::new(),
                stderr: Vec::new(),
                truncated: false,
            },
        );
        if request == Request::Preview {
            self.preview = Some(id);
        }
        self.count();
        Ok(())
    }

    pub fn poll(&mut self) -> Vec<(Request, Result<String, String>)> {
        let mut replies = Vec::new();
        for event in self.supervisor.poll() {
            match event {
                JobEvent::Output {
                    job,
                    stream,
                    bytes,
                    truncated,
                } => {
                    if let Some(pending) = self.pending.get_mut(&job) {
                        pending.truncated |= truncated;
                        let output = if stream == JobStream::Stdout {
                            &mut pending.stdout
                        } else {
                            &mut pending.stderr
                        };
                        let remaining = output_limit(pending.request).saturating_sub(output.len());
                        output.extend(bytes.iter().take(remaining));
                    }
                }
                JobEvent::Exited {
                    job,
                    code,
                    timed_out,
                } => {
                    if let Some(pending) = self.pending.remove(&job) {
                        let result = if timed_out {
                            Err("Git command timed out".into())
                        } else if pending.truncated
                            && matches!(
                                pending.request,
                                Request::Files | Request::CommitInfo | Request::Reflog
                            )
                        {
                            Err("Git response exceeds the preview limit".into())
                        } else if code == Some(0) {
                            let mut text = String::from_utf8_lossy(&pending.stdout).into_owned();
                            if pending.truncated {
                                text.push_str("\n[output truncated]\n");
                            }
                            Ok(text)
                        } else {
                            let stderr = String::from_utf8_lossy(&pending.stderr);
                            let message = if stderr.trim().is_empty() {
                                String::from_utf8_lossy(&pending.stdout).trim().to_owned()
                            } else {
                                stderr.trim().to_owned()
                            };
                            Err(if message.is_empty() {
                                "Git command failed".into()
                            } else {
                                message
                            })
                        };
                        replies.push((pending.request, result));
                    }
                }
                JobEvent::Failed { job, message } => {
                    if let Some(pending) = self.pending.remove(&job) {
                        replies.push((pending.request, Err(message)));
                    }
                }
                JobEvent::Started { .. } => {}
            }
        }
        self.count();
        replies
    }
}

/// How much of a request's output is kept. History may be read whole (`End` on a 50,000-commit
/// log is about 6 MiB); everything else is a preview-sized read.
fn output_limit(request: Request) -> usize {
    if request == Request::History { 64 << 20 } else { 2 << 20 }
}

#[cfg(test)]
mod tests {
    use super::{Jobs, Request};
    use std::fs::{self, File};
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    fn git(root: &Path, args: &[&str]) -> Vec<u8> {
        let output =
            Command::new(crate::process::discover_executable("git").expect("Git installed"))
                .current_dir(root)
                .env_clear()
                .envs(crate::git::command::environment())
                .env("GIT_OPTIONAL_LOCKS", "1")
                .args(args)
                .output()
                .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn committed_file() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "--template="]);
        git(root, &["config", "user.name", "Jobs test"]);
        git(root, &["config", "user.email", "jobs@example.invalid"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        git(root, &["config", "core.autocrlf", "false"]);
        git(root, &["config", "core.fsmonitor", "false"]);
        let hooks = root.join("empty-hooks");
        fs::create_dir(&hooks).unwrap();
        git(root, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        let file = root.join("tracked.txt");
        fs::write(&file, b"committed\n").unwrap();
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000))
            .unwrap();
        git(root, &["add", "--", "tracked.txt"]);
        git(root, &["commit", "-m", "fixture"]);
        dir
    }

    fn complete(jobs: &mut Jobs, request: Request) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let replies = jobs.poll();
            if !replies.is_empty() {
                assert_eq!(replies.len(), 1);
                let (completed, result) = replies.into_iter().next().unwrap();
                assert_eq!(completed, request);
                return result.expect("Git job must complete successfully");
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {request:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn preview_diff_preserves_stat_dirty_index_bytes() {
        let dir = committed_file();
        let root = dir.path();
        let file = root.join("tracked.txt");
        let original_modified = fs::metadata(&file).unwrap().modified().unwrap();
        // A whole-minute difference is deterministic even on coarse timestamp filesystems.
        let modified = original_modified + Duration::from_secs(60);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
        assert_eq!(fs::read(&file).unwrap(), b"committed\n");
        let index = root.join(".git/index");
        let before = fs::read(&index).unwrap();
        let args = [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--",
            "tracked.txt",
        ];
        let mut jobs = Jobs::default();
        jobs.start(
            root,
            Request::Preview,
            args.into_iter().map(Into::into).collect(),
        )
        .unwrap();
        assert_eq!(complete(&mut jobs, Request::Preview), "");
        assert_eq!(fs::read(&index).unwrap(), before);
        assert_eq!(fs::read(&file).unwrap(), b"committed\n");

        // Positive control: the same diff with optional locks enabled refreshes
        // this fixture's index, so the byte comparison above is not vacuous.
        assert!(git(root, &args).is_empty());
        assert_ne!(fs::read(&index).unwrap(), before);
    }

    #[test]
    fn write_add_stages_changed_file_without_changing_worktree() {
        let dir = committed_file();
        let root = dir.path();
        let file = root.join("tracked.txt");
        let index = root.join(".git/index");
        let before = fs::read(&index).unwrap();
        let changed = b"changed worktree\n";
        fs::write(&file, changed).unwrap();
        let mut jobs = Jobs::default();
        jobs.start(
            root,
            Request::Write,
            ["add", "--", "tracked.txt"]
                .into_iter()
                .map(Into::into)
                .collect(),
        )
        .unwrap();
        assert_eq!(complete(&mut jobs, Request::Write), "");
        assert_ne!(fs::read(&index).unwrap(), before);
        assert_eq!(git(root, &["show", ":tracked.txt"]), changed);
        assert_eq!(git(root, &["show", "HEAD:tracked.txt"]), b"committed\n");
        assert_eq!(fs::read(&file).unwrap(), changed);
    }
}
