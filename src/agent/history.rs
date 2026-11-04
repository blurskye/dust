//! Dust's own record of each agent conversation, so any tab can be resumed whether or not
//! the agent can load its old sessions. One file per conversation: the first line describes
//! it, so listing reads only that, and the second line holds the transcript.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
    time::Instant,
};

/// Larger files are not read back.
const FILE_BYTE_CEILING: u64 = 32 * 1024 * 1024;

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
    writing: bool,
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
                    saves.pending.pop_first().unwrap()
                };
                if let (Ok(head), Ok(body)) = (
                    serde_json::to_string(&conversation),
                    serde_json::to_string(&entries),
                ) && let Some(parent) = target.parent()
                    && fs::create_dir_all(parent).is_ok()
                {
                    let _ =
                        crate::files::atomic_write(&target, format!("{head}\n{body}\n").as_bytes());
                }
                lock.lock().unwrap().writing = false;
                ready.notify_all();
            }
        });
        Self(state)
    }

    fn save(&self, directory: PathBuf, conversation: Conversation, entries: Vec<Entry>) {
        let (lock, ready) = &*self.0;
        lock.lock()
            .unwrap()
            .pending
            .insert(file(&directory, &conversation.id), (conversation, entries));
        ready.notify_all();
    }

    fn flush(&self, deadline: Instant) -> bool {
        let (lock, ready) = &*self.0;
        let mut saves = lock.lock().unwrap();
        while saves.writing || !saves.pending.is_empty() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            saves = ready.wait_timeout(saves, remaining).unwrap().0;
        }
        true
    }
}

static WRITER: OnceLock<Writer> = OnceLock::new();

/// Save in submission order; pending snapshots of the same conversation are coalesced.
pub fn save(directory: PathBuf, conversation: Conversation, entries: Vec<Entry>) {
    WRITER
        .get_or_init(Writer::new)
        .save(directory, conversation, entries);
}

/// Finish pending history writes within the editor's shared shutdown budget.
pub fn flush(deadline: Instant) -> bool {
    WRITER.get().is_none_or(|writer| writer.flush(deadline))
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
            BufReader::new(fs::File::open(entry.path()).ok()?)
                .read_line(&mut line)
                .ok()?;
            serde_json::from_str::<Conversation>(&line).ok()
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|item| std::cmp::Reverse(item.updated));
    found
}

#[must_use]
pub fn load(directory: &Path, id: &str) -> Option<(Conversation, Vec<Entry>)> {
    let path = file(directory, id);
    if fs::metadata(&path).ok()?.len() > FILE_BYTE_CEILING {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
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
            Entry::User { text: question, .. } => text.push_str(&format!("\n\nUser: {question}")),
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
            },
            Entry::Agent("still streaming".into()),
        ];
        writer.save(
            directory.path().into(),
            conversation.clone(),
            entries.clone(),
        );
        assert!(writer.flush(Instant::now() + std::time::Duration::from_secs(5)));
        assert_eq!(
            load(directory.path(), "ordered"),
            Some((conversation, entries))
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
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
