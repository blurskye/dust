//! Bounded Git CLI requests for history, previews and operations not owned by a Git worker.
use crate::process::{JobEvent, JobId, JobPolicy, JobRequest, JobStream, JobSupervisor};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Request {
    History,
    Stashes,
    Tags,
    Remotes,
    Preview,
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

impl Jobs {
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
    }

    pub fn cancel_preview(&mut self) {
        if let Some(id) = self.preview.take() {
            let _ = self.supervisor.cancel(id);
            self.pending.remove(&id);
        }
    }
    pub fn start(
        &mut self,
        root: &Path,
        request: Request,
        args: Vec<OsString>,
    ) -> Result<(), String> {
        if request == Request::Preview {
            self.cancel_preview();
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
        arguments.extend(args);
        let id = self
            .supervisor
            .start(JobRequest {
                program: crate::process::discover_executable("git")
                    .ok_or("Git is not installed or not on PATH")?,
                argv0: None,
                arguments,
                cwd: root.into(),
                environment: BTreeMap::from([
                    ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                    ("GIT_PAGER".into(), "cat".into()),
                ]),
                root_revision: 0,
                stdin: None,
                policy: JobPolicy {
                    maximum_output_bytes: 2 * 1024 * 1024,
                    timeout: Duration::from_secs(30),
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
                        let remaining = (2 * 1024 * 1024usize).saturating_sub(output.len());
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
                        } else if code == Some(0) {
                            let mut text = String::from_utf8_lossy(&pending.stdout).into_owned();
                            if pending.truncated {
                                text.push_str("\n[output truncated]\n");
                            }
                            Ok(text)
                        } else {
                            Err(String::from_utf8_lossy(&pending.stderr).trim().to_owned())
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
        replies
    }
}
