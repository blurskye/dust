//! The Git tab's warm start: the last status, the head of History and the branches of each
//! repository, kept on disk so a reopened tab paints them at once and swaps in what git says
//! when it answers (the forge's rule: paint from the cache, then fetch fresh).
//!
//! An entry is keyed by the repository's path, its HEAD and its index's modification time.
//! When HEAD or the index moved, History and the branches still paint from the cache, but the
//! status reads as loading: a status from another HEAD would list the wrong changes.
//!
//! Off unless `DUSTY_GIT_CACHE_DIR` names a folder or a binary calls `enable` (then
//! `$XDG_CACHE_HOME/dusty/git`); `DUSTY_GIT_CACHE=0` turns it off.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Each write's temporary file has its own name.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// The format; an entry of another version is ignored.
const VERSION: u32 = 2;
/// History lines kept: more than any screen shows.
const HISTORY_LINES: usize = 200;
/// The revision of a status read from the disk cache: never one the status worker gives, so
/// the rows kept for it are never taken for a live status's (`memo::rows_inputs`).
pub(super) const REVISION: u64 = u64::MAX;

/// Keeps each repository's last status, History head and branches in
/// `$XDG_CACHE_HOME/dusty/git` (the `dusty` and `dusty-git` binaries call it).
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

fn folder() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DUSTY_GIT_CACHE_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if !ENABLED.load(Ordering::Relaxed) || std::env::var("DUSTY_GIT_CACHE").as_deref() == Ok("0") {
        return None;
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("dusty").join("git"))
}

/// The entry's file: FNV-1a of the path, stable across builds.
fn file(folder: &Path, root: &Path) -> PathBuf {
    let hash = root
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    folder.join(format!("{hash:016x}.json"))
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct Key {
    root: PathBuf,
    head: Option<String>,
    /// Nanoseconds since the epoch.
    index: Option<u64>,
}

impl Key {
    fn of(repository: &git2::Repository, root: &Path) -> Self {
        let head = repository
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|id| id.to_string());
        let index = std::fs::metadata(repository.path().join("index"))
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|time| u64::try_from(time.as_nanos()).ok());
        Self {
            root: root.to_owned(),
            head,
            index,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Warm {
    version: u32,
    key: Key,
    snapshot: GitStatusSnapshot,
    numstat: Vec<(PathBuf, Option<(u32, u32)>)>,
    /// `git log`'s first lines, as History reads them.
    history: String,
    refs: Option<GitBranchList>,
    stashes: Vec<StashEntry>,
    tags: Vec<String>,
    remotes: Vec<String>,
}

/// The first `HISTORY_LINES` lines of History's `git log`.
pub(super) fn history_head(text: &str) -> String {
    let end = text
        .match_indices('\n')
        .nth(HISTORY_LINES - 1)
        .map_or(text.len(), |(at, _)| at + 1);
    text[..end].to_owned()
}

impl GitScreen {
    /// Paints `root`'s cached state, if any, before git answers.
    pub(super) fn load_warm(&mut self, repository: &git2::Repository) {
        let Some(folder) = folder() else { return };
        let Ok(bytes) = std::fs::read(file(&folder, &self.root)) else {
            return;
        };
        let Ok(warm) = serde_json::from_slice::<Warm>(&bytes) else {
            return;
        };
        if warm.version != VERSION || warm.key.root != self.root {
            return;
        }
        if warm.key == Key::of(repository, &self.root) {
            let mut snapshot = warm.snapshot;
            snapshot.revision = REVISION;
            snapshot.request_id = GitStatusRequestId(u64::MAX);
            self.snapshot = Some(snapshot);
            self.numstat = warm.numstat.into_iter().collect();
        }
        if !warm.history.is_empty() {
            let (history, info) = shell::parse_history(&warm.history);
            self.set_history(history, info);
        }
        self.warm_history = warm.history;
        self.refs = warm.refs;
        self.stashes = warm.stashes;
        self.tags = warm.tags;
        self.remotes = warm.remotes;
    }

    /// Writes what the screen shows to the cache once git has answered for all of it, on a
    /// thread; again only when something changed.
    pub(super) fn save_warm(&mut self) {
        // History's head is HEAD's (`warm_history` is kept only then), whatever branch shows now.
        if self.refreshing
            || self.history_loading
            || self.refs_loading
            || self.warm_history.is_empty()
        {
            return;
        }
        let Some(snapshot) = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.revision != REVISION)
        else {
            return;
        };
        let Some(folder) = folder() else { return };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        (
            snapshot.revision,
            snapshot.entries.len(),
            self.numstat.len(),
            self.warm_history.len(),
        )
            .hash(&mut hasher);
        self.refs
            .as_ref()
            .map(|refs| refs.request_id.0)
            .hash(&mut hasher);
        (&self.stashes, &self.tags, &self.remotes).hash(&mut hasher);
        let fingerprint = hasher.finish();
        if self.warm_saved == Some(fingerprint) {
            return;
        }
        self.warm_saved = Some(fingerprint);
        let root = self.root.clone();
        let warm = Warm {
            version: VERSION,
            key: Key {
                root: root.clone(),
                head: None,
                index: None,
            },
            snapshot: snapshot.clone(),
            numstat: self
                .numstat
                .iter()
                .map(|(path, stat)| (path.clone(), *stat))
                .collect(),
            history: self.warm_history.clone(),
            refs: self.refs.clone(),
            stashes: self.stashes.clone(),
            tags: self.tags.clone(),
            remotes: self.remotes.clone(),
        };
        let _ = std::thread::Builder::new()
            .name("dusty-git-warm".into())
            .spawn(move || {
                let Ok(repository) = git2::Repository::open(&root) else {
                    return;
                };
                let mut warm = warm;
                // After git answered: a status may have refreshed the index's stat data.
                warm.key = Key::of(&repository, &root);
                let Ok(bytes) = serde_json::to_vec(&warm) else {
                    return;
                };
                if std::fs::create_dir_all(&folder).is_err() {
                    return;
                }
                let target = file(&folder, &root);
                let temporary = target.with_extension(format!(
                    "tmp.{}.{}",
                    std::process::id(),
                    WRITES.fetch_add(1, Ordering::Relaxed)
                ));
                if std::fs::write(&temporary, bytes).is_ok()
                    && std::fs::rename(&temporary, &target).is_err()
                {
                    let _ = std::fs::remove_file(&temporary);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_head_keeps_whole_lines() {
        let text = (0..300).map(|line| format!("{line}\n")).collect::<String>();
        let head = history_head(&text);
        assert_eq!(head.lines().count(), HISTORY_LINES);
        assert!(head.ends_with("199\n"));
        assert_eq!(history_head("a\nb"), "a\nb");
    }
}
