//! Dust's own record of each agent conversation, so any tab can be resumed whether or not
//! the agent can load its old sessions. One file per conversation: the first line describes
//! it, so listing reads only that, and the second line holds the transcript.

use serde::Deserialize;

use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread;
use std::time::Instant;

/// Saved transcripts must also fit the reader's byte limit.
const FILE_BYTE_LIMIT: u64 = 32 * 1024 * 1024;
const HEADER_BYTE_LIMIT: u64 = 64 * 1024;

/// One part of a conversation as the panel shows it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// The agent (re)started.
    Started {
        root: PathBuf,
    },
    User {
        text: String,
        context: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::agent::PromptPart>,
    },
    Agent(String),
    Thought(String),
    Tool {
        id: String,
        title: String,
        status: String,
    },
    Plan(Vec<(String, String)>),
    Notice(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Conversation {
    /// The file name without extension.
    pub id: String,
    /// The profile in `[agents.profiles]`.
    pub agent: String,
    pub root: PathBuf,
    /// The agent's own session id, for agents that can resume or load it.
    pub session: Option<String>,
    pub title: String,
    /// Seconds since the Unix epoch.
    pub updated: u64,
}

#[must_use]
pub fn directory() -> PathBuf {
    crate::config::user_state_directory().join("agents")
}

fn file(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!("{id}.jsonl"))
}

#[derive(Default)]
struct Saves {
    pending: BTreeMap<PathBuf, (Conversation, Vec<Entry>)>,
    order: VecDeque<PathBuf>,
    writing: bool,
    failures: BTreeMap<PathBuf, String>,
    notices: Vec<String>,
}

impl Saves {
    fn enqueue(&mut self, target: PathBuf, conversation: Conversation, entries: Vec<Entry>) {
        if !self.pending.contains_key(&target) {
            self.order.push_back(target.clone());
        }
        self.pending.insert(target, (conversation, entries));
    }

    fn pop_front(&mut self) -> (PathBuf, (Conversation, Vec<Entry>)) {
        let target = self.order.pop_front().unwrap();
        let snapshot = self.pending.remove(&target).unwrap();
        (target, snapshot)
    }
}

fn write_history(
    target: &Path,
    conversation: &Conversation,
    entries: &[Entry],
) -> Result<(), String> {
    let write = || -> Result<(), String> {
        let head = serde_json::to_string(conversation).map_err(|error| error.to_string())?;
        let body = serde_json::to_string(entries).map_err(|error| error.to_string())?;
        if head.len() as u64 + 1 > HEADER_BYTE_LIMIT {
            return Err("conversation header exceeds its size limit".into());
        }
        if head.len() as u64 + body.len() as u64 + 2 > FILE_BYTE_LIMIT {
            return Err("conversation exceeds its size limit".into());
        }
        let parent = target.parent().ok_or("history path has no parent")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        crate::files::atomic_write(target, format!("{head}\n{body}\n").as_bytes())
            .map_err(|error| error.to_string())
    };
    write().map_err(|error| format!("could not save agent history {}: {error}", target.display()))
}

struct Writer(Arc<(Mutex<Saves>, Condvar)>);

impl Writer {
    fn new() -> Self {
        let state = Arc::new((Mutex::new(Saves::default()), Condvar::new()));
        let worker = state.clone();
        thread::spawn(move || {
            let (lock, ready) = &*worker;
            loop {
                let (target, (conversation, entries)) = {
                    let mut saves = lock.lock().unwrap();
                    while saves.pending.is_empty() {
                        saves = ready.wait(saves).unwrap();
                    }
                    saves.writing = true;
                    saves.pop_front()
                };
                let result = write_history(&target, &conversation, &entries);
                let mut saves = lock.lock().unwrap();
                match result {
                    Ok(()) => {
                        saves.failures.remove(&target);
                    }
                    Err(error) => {
                        if saves.failures.get(&target) != Some(&error) {
                            saves.notices.push(error.clone());
                        }
                        saves.failures.insert(target, error);
                    }
                }
                saves.writing = false;
                ready.notify_all();
            }
        });
        Self(state)
    }

    fn save(&self, directory: PathBuf, conversation: Conversation, entries: Vec<Entry>) {
        let (lock, ready) = &*self.0;
        lock.lock()
            .unwrap()
            .enqueue(file(&directory, &conversation.id), conversation, entries);
        ready.notify_all();
    }

    fn take_errors(&self) -> Vec<String> {
        let (lock, _) = &*self.0;
        std::mem::take(&mut lock.lock().unwrap().notices)
    }

    fn flush(&self, directory: &Path, deadline: Instant) -> Result<(), String> {
        let (lock, ready) = &*self.0;
        let mut saves = lock.lock().unwrap();
        while saves.writing || !saves.pending.is_empty() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err("agent history writes did not finish before the exit deadline".into());
            };
            saves = ready.wait_timeout(saves, remaining).unwrap().0;
        }
        match saves
            .failures
            .iter()
            .find(|(target, _)| target.parent() == Some(directory))
        {
            Some((_, error)) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

static WRITER: OnceLock<Writer> = OnceLock::new();

/// Save in submission order; pending snapshots of the same conversation are coalesced.
pub fn save(directory: PathBuf, conversation: Conversation, entries: Vec<Entry>) {
    WRITER
        .get_or_init(Writer::new)
        .save(directory, conversation, entries);
}

/// Finish pending history writes within the editor's shared shutdown budget, and report a
/// failed save in `directory`. The writer is shared, so failures elsewhere are not this editor's.
pub fn flush(directory: &Path, deadline: Instant) -> Result<(), String> {
    WRITER
        .get()
        .map_or(Ok(()), |writer| writer.flush(directory, deadline))
}

/// New write failures for the session's notifications; failed saves still fail `flush`.
#[must_use]
pub fn take_errors() -> Vec<String> {
    WRITER.get().map_or_else(Vec::new, Writer::take_errors)
}

fn open_history(path: &Path) -> Option<fs::File> {
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW);
    }
    let file = options.open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// Every saved conversation, newest first.
#[must_use]
pub fn list(directory: &Path) -> Vec<Conversation> {
    let mut found = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .filter_map(|entry| {
            let mut line = String::new();
            BufReader::new(open_history(&entry.path())?.take(HEADER_BYTE_LIMIT + 1))
                .read_line(&mut line)
                .ok()?;
            if line.len() as u64 > HEADER_BYTE_LIMIT {
                return None;
            }
            serde_json::from_str::<Conversation>(&line).ok()
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|item| std::cmp::Reverse(item.updated));
    found
}

#[must_use]
pub fn load(directory: &Path, id: &str) -> Option<(Conversation, Vec<Entry>)> {
    let source = open_history(&file(directory, id))?;
    if source.metadata().ok()?.len() > FILE_BYTE_LIMIT {
        return None;
    }
    let mut text = String::new();
    source
        .take(FILE_BYTE_LIMIT + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > FILE_BYTE_LIMIT {
        return None;
    }
    let (head, body) = text.split_once('\n')?;
    Some((
        serde_json::from_str(head).ok()?,
        serde_json::from_str(body.trim_end()).ok()?,
    ))
}

/// The conversation as plain text for an agent that starts fresh: the last `limit`
/// characters of what was asked and answered.
#[must_use]
pub fn recap(entries: &[Entry], limit: usize) -> String {
    let mut text = String::new();
    for entry in entries {
        match entry {
            Entry::User {
                text: question,
                context,
                attachments,
            } => {
                text.push_str(&format!("\n\nUser: {question}"));
                for label in context {
                    text.push_str(&format!("\n[context: {label}]"));
                }
                for part in attachments {
                    match part {
                        crate::agent::PromptPart::Text(content) => {
                            text.push('\n');
                            text.push_str(content);
                        }
                        crate::agent::PromptPart::File {
                            path,
                            text: content,
                        } => {
                            text.push_str(&format!("\n[file: {}]", path.display()));
                            if let Some(content) = content {
                                text.push('\n');
                                text.push_str(content);
                            }
                        }
                    }
                }
            }
            Entry::Agent(answer) => text.push_str(&format!("\n\nAssistant: {answer}")),
            Entry::Tool { title, .. } => text.push_str(&format!("\n[tool: {title}]")),
            _ => {}
        }
    }
    let count = text.chars().count();
    let text = if count > limit {
        text.chars().skip(count - limit).collect()
    } else {
        text
    };
    format!(
        "This continues an earlier conversation in this workspace. The agent that had it \
         is gone, so here is how it went (latest part):{text}\n\nThe new question follows."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_history_saves_are_fair_and_coalesce_in_place() {
        let conversation = Conversation {
            id: "a".into(),
            agent: "fixture".into(),
            root: "/work".into(),
            session: None,
            title: "question".into(),
            updated: 1,
        };
        let mut saves = Saves::default();
        saves.enqueue("b".into(), conversation.clone(), Vec::new());
        saves.enqueue("a".into(), conversation.clone(), Vec::new());
        let latest = vec![Entry::Agent("latest".into())];
        saves.enqueue("b".into(), conversation.clone(), latest.clone());
        assert_eq!(saves.pending.len(), 2);
        let (target, (_, entries)) = saves.pop_front();
        assert_eq!(target, Path::new("b"));
        assert_eq!(entries, latest);
        // Repeated updates to a just-written conversation go behind existing work.
        saves.enqueue("b".into(), conversation, Vec::new());
        assert_eq!(saves.pop_front().0, Path::new("a"));
        assert_eq!(saves.pop_front().0, Path::new("b"));
        assert!(saves.pending.is_empty());
        assert!(saves.order.is_empty());
    }

    #[test]
    fn failed_history_saves_are_reported_until_a_successful_retry() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("blocked");
        fs::write(&blocked, "not a directory").unwrap();
        let conversation = Conversation {
            id: "failed".into(),
            agent: "fixture".into(),
            root: directory.path().into(),
            session: None,
            title: "question".into(),
            updated: 1,
        };
        let entries = vec![Entry::Agent("answer".into())];
        let writer = Writer::new();
        writer.save(blocked.clone(), conversation.clone(), entries.clone());
        let error = writer
            .flush(&blocked, Instant::now() + std::time::Duration::from_secs(5))
            .unwrap_err();
        assert!(error.contains("could not save agent history"));
        assert!(error.contains("blocked"));
        assert_eq!(
            writer.take_errors().as_slice(),
            std::slice::from_ref(&error)
        );
        assert!(writer.take_errors().is_empty());
        assert_eq!(writer.flush(&blocked, Instant::now()), Err(error));
        assert_eq!(writer.flush(directory.path(), Instant::now()), Ok(()));
        fs::remove_file(&blocked).unwrap();
        writer.save(blocked.clone(), conversation.clone(), entries.clone());
        writer
            .flush(&blocked, Instant::now() + std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(load(&blocked, "failed"), Some((conversation, entries)));
    }

    #[test]
    fn oversized_history_saves_preserve_the_previous_readable_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let conversation = Conversation {
            id: "large".into(),
            agent: "fixture".into(),
            root: directory.path().into(),
            session: None,
            title: "question".into(),
            updated: 1,
        };
        let writer = Writer::new();
        let entries = vec![Entry::Agent("saved answer".into())];
        writer.save(
            directory.path().into(),
            conversation.clone(),
            entries.clone(),
        );
        writer
            .flush(
                directory.path(),
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap();
        writer.save(
            directory.path().into(),
            conversation.clone(),
            vec![Entry::Agent("x".repeat(FILE_BYTE_LIMIT as usize))],
        );
        let error = writer
            .flush(
                directory.path(),
                Instant::now() + std::time::Duration::from_secs(10),
            )
            .unwrap_err();
        assert!(error.contains("conversation exceeds its size limit"));
        assert_eq!(
            load(directory.path(), "large"),
            Some((conversation.clone(), entries.clone()))
        );
        let mut oversized_header = conversation.clone();
        oversized_header.title = "x".repeat(HEADER_BYTE_LIMIT as usize);
        writer.save(directory.path().into(), oversized_header, entries.clone());
        let error = writer
            .flush(
                directory.path(),
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap_err();
        assert!(error.contains("header exceeds its size limit"));
        assert_eq!(
            load(directory.path(), "large"),
            Some((conversation, entries))
        );
    }

    #[test]
    fn queued_history_saves_keep_the_newest_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let writer = Writer::new();
        let mut conversation = Conversation {
            id: "ordered".into(),
            agent: "fixture".into(),
            root: directory.path().into(),
            session: Some("s".into()),
            title: "old".into(),
            updated: 1,
        };
        for revision in 0..50 {
            conversation.updated = revision;
            writer.save(
                directory.path().into(),
                conversation.clone(),
                vec![Entry::Agent("x".repeat(256 * 1024))],
            );
        }
        conversation.title = "newest".into();
        conversation.updated = 50;
        let entries = vec![
            Entry::User {
                text: "question".into(),
                context: Vec::new(),
                attachments: Vec::new(),
            },
            Entry::Agent("still streaming".into()),
        ];
        writer.save(
            directory.path().into(),
            conversation.clone(),
            entries.clone(),
        );
        writer
            .flush(
                directory.path(),
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            load(directory.path(), "ordered"),
            Some((conversation, entries))
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn history_reads_reject_oversized_headers_and_transcripts() {
        let directory = tempfile::tempdir().unwrap();
        let conversation = Conversation {
            id: "large".into(),
            agent: "fixture".into(),
            root: directory.path().into(),
            session: None,
            title: "x".repeat(HEADER_BYTE_LIMIT as usize),
            updated: 1,
        };
        let path = file(directory.path(), &conversation.id);
        fs::write(
            &path,
            format!("{}\n[]\n", serde_json::to_string(&conversation).unwrap()),
        )
        .unwrap();
        assert!(list(directory.path()).is_empty());
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(FILE_BYTE_LIMIT + 1)
            .unwrap();
        assert!(load(directory.path(), &conversation.id).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn history_reads_reject_special_files_and_symlinks() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        symlink("/dev/zero", file(directory.path(), "device")).unwrap();
        nix::unistd::mkfifo(
            &file(directory.path(), "pipe"),
            nix::sys::stat::Mode::S_IRUSR,
        )
        .unwrap();
        fs::create_dir(file(directory.path(), "directory")).unwrap();
        assert!(list(directory.path()).is_empty());
        for id in ["device", "pipe", "directory"] {
            assert!(load(directory.path(), id).is_none());
        }
    }

    #[test]
    fn history_round_trips_attachments_and_recaps_legacy_context() {
        use crate::agent::PromptPart;
        let directory = tempfile::tempdir().unwrap();
        let conversation = Conversation {
            id: "attached".into(),
            agent: "fixture".into(),
            root: directory.path().into(),
            session: None,
            title: "explain".into(),
            updated: 1,
        };
        let entries = vec![Entry::User {
            text: "explain".into(),
            context: vec!["main.rs (1 lines selected)".into()],
            attachments: vec![
                PromptPart::File {
                    path: "/work/main.rs".into(),
                    text: Some("unsaved fragment".into()),
                },
                PromptPart::Text("Diagnostics:\nunsaved error".into()),
                PromptPart::File {
                    path: "/work/other.rs".into(),
                    text: None,
                },
            ],
        }];
        write_history(&file(directory.path(), "attached"), &conversation, &entries).unwrap();
        let (_, loaded) = load(directory.path(), "attached").unwrap();
        assert_eq!(loaded, entries);
        let summary = recap(&loaded, 1000);
        for expected in [
            "User: explain",
            "main.rs (1 lines selected)",
            "/work/main.rs",
            "unsaved fragment",
            "Diagnostics:\nunsaved error",
            "/work/other.rs",
        ] {
            assert!(summary.contains(expected), "missing {expected}: {summary}");
        }
        let legacy: Vec<Entry> =
            serde_json::from_str(r#"[{"user":{"text":"old question","context":["old context"]}}]"#)
                .unwrap();
        assert!(recap(&legacy, 1000).contains("[context: old context]"));
        assert!(matches!(&legacy[0], Entry::User { attachments, .. } if attachments.is_empty()));
    }

    #[test]
    fn a_saved_conversation_lists_and_loads_back() {
        let directory = tempfile::tempdir().unwrap();
        let conversation = Conversation {
            id: "1-1".into(),
            agent: "opencode".into(),
            root: "/work".into(),
            session: Some("ses_1".into()),
            title: "why".into(),
            updated: 5,
        };
        let entries = vec![
            Entry::User {
                text: "why".into(),
                context: Vec::new(),
                attachments: Vec::new(),
            },
            Entry::Agent("because".into()),
        ];
        save(
            directory.path().to_owned(),
            conversation.clone(),
            entries.clone(),
        );
        let mut loaded = None;
        for _ in 0..200 {
            loaded = load(directory.path(), "1-1");
            if loaded.is_some() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(loaded, Some((conversation.clone(), entries.clone())));
        assert_eq!(list(directory.path()), [conversation]);
        assert!(recap(&entries, 1000).contains("User: why\n\nAssistant: because"));
        assert!(recap(&entries, 7).ends_with(":because\n\nThe new question follows."));
    }
}
