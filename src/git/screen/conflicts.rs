//! Conflict resolution on the Git tab (DESIGN.md §5.12). Merge, pull, rebase, cherry-pick
//! and revert all stop in the same place: the operation banner names what is being applied
//! onto what and how many conflict hunks are left; the reader labels each hunk's sides
//! (`<<<<<<< ours · main`, `>>>>>>> theirs · feature`, and the base when diff3 or zdiff3
//! writes one); `o` `t` `b` take ours, theirs or both for the hunk under the cursor and
//! Enter takes the side the cursor is on, as lazygit's merge view does. The last pick in a
//! file stages it. Nothing here is read while a frame paints: `refresh_conflicts` runs when
//! the status refreshes and after each pick.
use super::github::keymap::{self, Key, Preset, To};
use super::preview::Preview;
use super::*;
use crate::git::GitFileStatus;

/// Reader row marks (`PreviewRow::marker`) for a conflicted file's source: ours, theirs and
/// base lines, and the marker lines themselves.
pub(super) const OURS: char = '<';
pub(super) const THEIRS: char = '>';
pub(super) const BASE: char = '|';
pub(super) const MARKER_ROW: char = '=';

/// Git's default `conflict-marker-size`.
const MARKER: usize = 7;
/// Larger files are not read for picks or counts.
const MAXIMUM_BYTES: u64 = 8 << 20;

/// One conflict in a file's text, as 0-based indices of its marker lines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Hunk {
    pub start: usize,
    /// `|||||||`: where diff3 and zdiff3 write the merge base.
    pub base: Option<usize>,
    pub middle: usize,
    pub end: usize,
}

impl Hunk {
    fn contains(&self, line: usize) -> bool {
        (self.start..=self.end).contains(&line)
    }

    /// The side `line` is on; `None` on a marker line.
    fn side(&self, line: usize) -> Option<Pick> {
        let ours_end = self.base.unwrap_or(self.middle);
        if line > self.start && line < ours_end {
            Some(Pick::Ours)
        } else if self.base.is_some_and(|base| line > base && line < self.middle) {
            Some(Pick::Base)
        } else if line > self.middle && line < self.end {
            Some(Pick::Theirs)
        } else {
            None
        }
    }
}

/// What a pick keeps of one hunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Pick {
    Ours,
    Theirs,
    /// Ours, then theirs (lazygit's "pick both hunks").
    Both,
    Base,
}

impl Pick {
    fn word(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Theirs => "theirs",
            Self::Both => "both",
            Self::Base => "the base",
        }
    }
}

/// `<<<<<<<` and its kin: exactly the marker, then nothing or a space and Git's label.
fn marker(line: &str, sign: char) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    line.chars().take_while(|c| *c == sign).count() == MARKER
        && line[MARKER..].chars().next().is_none_or(|c| c == ' ')
}

/// Every complete conflict in `text`, in order. A stray or unfinished marker is not one.
pub(super) fn hunks(text: &str) -> Vec<Hunk> {
    let mut found = Vec::new();
    let (mut start, mut base, mut middle) = (None, None, None);
    for (index, line) in text.lines().enumerate() {
        if marker(line, '<') {
            (start, base, middle) = (Some(index), None, None);
        } else if start.is_some() && middle.is_none() && base.is_none() && marker(line, '|') {
            base = Some(index);
        } else if start.is_some() && middle.is_none() && marker(line, '=') {
            middle = Some(index);
        } else if let (Some(first), Some(between)) = (start, middle)
            && marker(line, '>')
        {
            found.push(Hunk {
                start: first,
                base,
                middle: between,
                end: index,
            });
            (start, base, middle) = (None, None, None);
        }
    }
    found
}

/// `text` with `hunk` replaced by the side(s) `pick` keeps; every other byte, line endings
/// included, stays as it was.
pub(super) fn resolve(text: &str, hunk: &Hunk, pick: Pick) -> Result<String, String> {
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    if hunk.end >= lines.len() {
        return Err("The conflict is no longer in the file".into());
    }
    let ours = &lines[hunk.start + 1..hunk.base.unwrap_or(hunk.middle)];
    let theirs = &lines[hunk.middle + 1..hunk.end];
    let kept: Vec<&str> = match pick {
        Pick::Ours => ours.to_vec(),
        Pick::Theirs => theirs.to_vec(),
        Pick::Both => ours.iter().chain(theirs).copied().collect(),
        Pick::Base => {
            let base = hunk.base.ok_or(
                "Base version not shown · git config merge.conflictStyle zdiff3 adds it",
            )?;
            lines[base + 1..hunk.middle].to_vec()
        }
    };
    Ok(lines[..hunk.start]
        .iter()
        .chain(&kept)
        .chain(&lines[hunk.end + 1..])
        .copied()
        .collect())
}

/// A regular file's text, or `None` for a symlink, a special, binary or oversized file.
fn read_text(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAXIMUM_BYTES {
        return None;
    }
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    (!text.contains('\0')).then_some(text)
}

/// One still-conflicted file: hunks left on disk, hunks Git's merge of its three index
/// stages produced, and the 1-based line of its first conflict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FileConflicts {
    pub path: PathBuf,
    pub left: usize,
    pub total: usize,
    pub first: Option<usize>,
}

/// What the paused operation applies onto what, read from the repository when the status
/// refreshes. Each name is `None` when the repository does not say it plainly.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Context {
    /// The operation as the user started it: `merge`, `pull`, `rebase`, `cherry-pick`,
    /// `revert`, or the repository's own name for another.
    pub kind: String,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    pub base: Option<String>,
    /// `feature into main`, `feature onto main`, `a1b2c3d Shout the greeting onto main`.
    pub what: Option<String>,
    /// A rebase's `(commit, of)`.
    pub step: Option<(usize, usize)>,
    pub files: Vec<FileConflicts>,
}

fn short(id: git2::Oid) -> String {
    id.to_string()[..7].to_owned()
}

fn commit_line(repository: &git2::Repository, id: git2::Oid) -> String {
    let subject = repository
        .find_commit(id)
        .ok()
        .and_then(|commit| commit.summary().map(str::to_owned))
        .unwrap_or_default();
    format!("{} {subject}", short(id)).trim_end().to_owned()
}

/// A branch, then a remote-tracking branch, then a tag at `id`; never `skip` (the branch
/// being rebased or merged into).
fn name_at(repository: &git2::Repository, id: git2::Oid, skip: &str) -> Option<String> {
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        let mut names = repository
            .references_glob(&format!("{prefix}*"))
            .ok()?
            .flatten()
            .filter(|reference| reference.peel_to_commit().is_ok_and(|commit| commit.id() == id))
            .filter_map(|reference| reference.name().map(|name| name[prefix.len()..].to_owned()))
            .filter(|name| name != skip && !name.ends_with("/HEAD"))
            .collect::<Vec<_>>();
        names.sort();
        if let Some(name) = names.into_iter().next() {
            return Some(name);
        }
    }
    None
}

/// The first object id recorded in a metadata file such as `MERGE_HEAD` or `onto`.
fn recorded(path: &Path) -> Option<git2::Oid> {
    let text = std::fs::read_to_string(path).ok()?;
    git2::Oid::from_str(text.lines().next()?.trim()).ok()
}

impl Context {
    /// `head` is the screen's validated head label (`rebase · main`, `rebase · detached
    /// HEAD`); a rebase never names its branch from unchecked metadata.
    fn read(root: &Path, operation: &str, branch: &str, head: &str, replay: Option<&PausedReplay>) -> Self {
        let mut context = Self {
            kind: operation.to_owned(),
            ..Self::default()
        };
        let Ok(repository) = git2::Repository::open(root) else {
            return context;
        };
        let here = if branch == "detached HEAD" {
            repository
                .head()
                .ok()
                .and_then(|head| head.target())
                .map_or_else(|| "HEAD".to_owned(), |id| format!("HEAD {}", short(id)))
        } else {
            branch.to_owned()
        };
        let git = repository.path();
        match operation {
            "merge" => {
                let first = std::fs::read_to_string(git.join("MERGE_MSG")).unwrap_or_default();
                // `git pull` records `Merge branch 'main' of <remote>`.
                if first.lines().next().is_some_and(|line| line.contains("' of ")) {
                    context.kind = "pull".into();
                }
                let incoming = recorded(&git.join("MERGE_HEAD"));
                context.theirs = incoming.map(|id| {
                    name_at(&repository, id, branch).unwrap_or_else(|| commit_line(&repository, id))
                });
                context.base = incoming
                    .zip(repository.head().ok().and_then(|head| head.target()))
                    .and_then(|(incoming, head)| repository.merge_base(head, incoming).ok())
                    .map(short);
                context.ours = Some(here);
                context.what = context
                    .theirs
                    .as_ref()
                    .zip(context.ours.as_ref())
                    .map(|(theirs, ours)| format!("{theirs} into {ours}"));
            }
            "rebase" => {
                let directory = ["rebase-merge", "rebase-apply"]
                    .map(|name| git.join(name))
                    .into_iter()
                    .find(|path| path.is_dir());
                let rebased = head.strip_prefix("rebase · ").unwrap_or(head);
                // Ours is what the commits are replayed onto: the upstream.
                context.ours = directory
                    .as_ref()
                    .and_then(|directory| recorded(&directory.join("onto")))
                    .map(|onto| name_at(&repository, onto, rebased).unwrap_or_else(|| short(onto)));
                // Theirs is the commit being replayed, only when its metadata checks out.
                context.theirs = replay.map(|replay| format!("{} {}", short(replay.id), replay.subject));
                context.base = replay.map(|replay| format!("parent of {}", short(replay.id)));
                // `rebase · feature onto main`: the head label as the top bar says it.
                context.what = (!head.is_empty()).then(|| match &context.ours {
                    Some(onto) => format!("{head} onto {onto}"),
                    None => head.to_owned(),
                });
                context.step = crate::git::conflict::rebase_progress(&repository);
            }
            "cherry-pick" | "revert" => {
                let head = if operation == "revert" { "REVERT_HEAD" } else { "CHERRY_PICK_HEAD" };
                let incoming = repository.refname_to_id(head).ok();
                context.ours = Some(here.clone());
                context.theirs = incoming.map(|id| {
                    let line = commit_line(&repository, id);
                    if operation == "revert" { format!("revert of {line}") } else { line }
                });
                context.base = incoming.map(|id| {
                    if operation == "revert" { short(id) } else { format!("parent of {}", short(id)) }
                });
                context.what = incoming.map(|id| {
                    let verb = if operation == "revert" { "on" } else { "onto" };
                    format!("{} {verb} {here}", commit_line(&repository, id))
                });
            }
            _ => {}
        }
        context.files = files(&repository, root);
        context
    }

    fn left(&self) -> usize {
        self.files.iter().map(|file| file.left).sum()
    }

    fn total(&self) -> usize {
        self.files.iter().map(|file| file.total).sum()
    }

    /// `rebase: 1 of 3 conflict hunks resolved in 2 files`: hunks, counted across the files
    /// still conflicted. `None` once no file is.
    pub(super) fn progress(&self) -> Option<String> {
        if self.files.is_empty() {
            return None;
        }
        let total = self.total();
        let files = self.files.len();
        Some(format!(
            "{}: {} of {total} conflict hunk{} resolved{}",
            self.kind,
            total - self.left(),
            if total == 1 { "" } else { "s" },
            if files > 1 { format!(" in {files} files") } else { String::new() }
        ))
    }

    /// `Rebase`, `Cherry-pick`, `Pull`: the banner's first word.
    pub(super) fn title(&self) -> String {
        let mut chars = self.kind.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }
}

/// Every conflicted path's hunk counts. The total comes from merging the index's three
/// stages again (as Git did when it stopped), so picks already made still count.
fn files(repository: &git2::Repository, root: &Path) -> Vec<FileConflicts> {
    let Ok(index) = repository.index() else {
        return Vec::new();
    };
    let Ok(conflicts) = index.conflicts() else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for conflict in conflicts.flatten().take(200) {
        let Some(path) = conflict
            .our
            .as_ref()
            .or(conflict.their.as_ref())
            .or(conflict.ancestor.as_ref())
            .and_then(|entry| crate::git::conflict::git_path(&entry.path))
        else {
            continue;
        };
        let found = read_text(&root.join(&path)).map(|text| hunks(&text)).unwrap_or_default();
        let merged = match (&conflict.ancestor, &conflict.our, &conflict.their) {
            (Some(base), Some(ours), Some(theirs)) => repository
                .merge_file_from_index(base, ours, theirs, None)
                .ok()
                .map(|result| hunks(&String::from_utf8_lossy(result.content())).len())
                .unwrap_or(0),
            _ => 0,
        };
        // A conflict without markers (binary, deleted on one side) is one to resolve.
        let (left, total) = if found.is_empty() && merged == 0 {
            (1, 1)
        } else {
            (found.len(), merged.max(found.len()))
        };
        files.push(FileConflicts {
            path,
            left,
            total,
            first: found.first().map(|hunk| hunk.start + 1),
        });
    }
    files
}

/// Labels each conflict's marker rows in a conflicted file's source preview and marks its
/// sides for the reader's bands; returns the row of the first conflict, for the cursor.
pub(super) fn decorate(preview: &mut Preview, text: &str, context: Option<&Context>) -> Option<usize> {
    let found = hunks(text);
    let lines = text.lines().collect::<Vec<_>>();
    let side = |pick: fn(&Context) -> Option<&String>| context.and_then(pick);
    for hunk in &found {
        label(preview, &lines, hunk.start, '<', "ours", side(|c| c.ours.as_ref()));
        if let Some(base) = hunk.base {
            label(preview, &lines, base, '|', "base", side(|c| c.base.as_ref()));
        }
        label(preview, &lines, hunk.middle, '=', "", None);
        label(preview, &lines, hunk.end, '>', "theirs", side(|c| c.theirs.as_ref()));
        for line in hunk.start + 1..hunk.end {
            if let Some(pick) = hunk.side(line)
                && let Some(row) = preview.rows.get_mut(line + 1)
            {
                row.marker = Some(match pick {
                    Pick::Ours => OURS,
                    Pick::Theirs => THEIRS,
                    _ => BASE,
                });
            }
        }
    }
    found.first().map(|hunk| hunk.start + 1)
}

/// One marker row's text: the marker, the side, and the repository's name for it, or Git's
/// own label (`HEAD`, `a1b2c3d (Shout)`) when the repository names no side.
fn label(preview: &mut Preview, lines: &[&str], line: usize, sign: char, side: &str, name: Option<&String>) {
    let Some(row) = preview.rows.get_mut(line + 1) else {
        return;
    };
    let own = lines[line].trim_end_matches('\r').get(MARKER..).unwrap_or_default().trim();
    let name = name.map_or(own, String::as_str);
    let marker = sign.to_string().repeat(MARKER);
    row.text = if side.is_empty() {
        marker
    } else if name.is_empty() {
        format!("{marker} {side}")
    } else {
        format!("{marker} {side} · {name}")
    };
    row.spans.clear();
    row.marker = Some(MARKER_ROW);
}

/// The confirm button and banner words for an action on `operation`: `Continue rebase`,
/// `Abort cherry-pick`, `Skip commit`.
pub(super) fn action_label(action: GitOperationAction, operation: &str) -> String {
    match action {
        GitOperationAction::Continue => format!("Continue {operation}"),
        GitOperationAction::Abort => format!("Abort {operation}"),
        GitOperationAction::Skip => "Skip commit".into(),
        GitOperationAction::Resolve => "Resolve".into(),
    }
}

/// The operation menu's choices (`M`, lazygit's `m`): skip only where Git has one.
fn menu_choices(operation: &str) -> Vec<String> {
    let mut choices = vec![action_label(GitOperationAction::Continue, operation)];
    if crate::git::conflict::skips(operation) {
        choices.push(action_label(GitOperationAction::Skip, operation));
    }
    choices.push(action_label(GitOperationAction::Abort, operation));
    choices
}

pub(super) fn menu_action(choice: &str) -> Option<GitOperationAction> {
    [GitOperationAction::Continue, GitOperationAction::Skip, GitOperationAction::Abort]
        .into_iter()
        .find(|action| choice.starts_with(&action_label(*action, "")[..4]))
}

/// An Operation help row's key in the keys in force: lazygit takes the side under the
/// cursor with Space and opens the menu with `m`.
pub(super) fn operation_key(app: &GitScreen, key: &str) -> String {
    if app.github.key_table().preset != Preset::Lazygit {
        return key.to_owned();
    }
    let lazygit = |dusty: Key| {
        keymap::LAZYGIT_MERGING
            .iter()
            .chain(keymap::LAZYGIT_OPERATION)
            .find(|(_, to, _)| *to == To::Key(dusty))
            .map(|(pressed, ..)| pressed.label())
    };
    match key {
        "Enter" => lazygit(Key::Enter),
        "M" => lazygit(Key::Char('M')),
        _ => None,
    }
    .unwrap_or_else(|| key.to_owned())
}

impl GitScreen {
    /// Re-read the paused operation's sides and hunk counts.
    pub(super) fn refresh_conflicts(&mut self) {
        self.conflict = self.operation_bar.as_ref().map(|bar| {
            Context::read(
                &self.root,
                &bar.operation,
                &self.branch,
                &self.head_label,
                self.paused_replay.as_ref(),
            )
        });
    }

    /// The selected Changes row when it is a conflicted file.
    fn conflicted_row(&self) -> Option<PathBuf> {
        if self.last_list != 0 || self.files_view != 0 || self.all_files {
            return None;
        }
        let row = self.selected_row().filter(|row| !row.directory)?;
        self.snapshot
            .as_ref()?
            .entries
            .iter()
            .any(|entry| entry.relative_path == row.value && entry.status == GitFileStatus::Conflicted)
            .then_some(row.value)
    }

    /// The reader is on a conflicted file's text, where the hunk keys pick.
    pub(super) fn conflict_reader(&self) -> bool {
        self.focus == 4 && !self.preview.patch && self.conflicted_row().is_some()
    }

    /// The footer's keys for a conflict, already in the keys in force; `None` elsewhere.
    pub(super) fn conflict_hints(&self) -> Option<Vec<(&'static str, &'static str)>> {
        if self.preview_anchor.is_some() || self.form.is_some() {
            return None;
        }
        let lazygit = self.github.key_table().preset == Preset::Lazygit;
        if self.conflict_reader() {
            let take = if lazygit { "Space" } else { "Enter" };
            return Some(vec![("o", "ours"), ("t", "theirs"), ("b", "both"), (take, "this side"), ("e", "edit")]);
        }
        if self.focus == 0 && !lazygit && self.file_selection.marked.is_empty() && self.conflicted_row().is_some() {
            return Some(vec![
                ("0", "pick hunks"),
                ("o t", "whole file"),
                ("e", "edit"),
                ("s", "stage"),
                ("M", "continue · abort"),
            ]);
        }
        None
    }

    /// lazygit's keys for its merge view (Space picks, `b` both, `e` edits) on a conflicted
    /// file in the reader, and `m` for the operation menu while one is paused.
    pub(super) fn conflict_lazygit(&self, key: Key) -> Option<KeyEvent> {
        let reader = self.conflict_reader().then_some(keymap::LAZYGIT_MERGING);
        let paused = (self.operation_bar.is_some() && self.message_scroll.is_none())
            .then_some(keymap::LAZYGIT_OPERATION);
        reader
            .into_iter()
            .chain(paused)
            .flatten()
            .find(|(pressed, ..)| *pressed == key)
            .and_then(|(_, to, _)| match to {
                To::Key(dusty) => Some(dusty.event()),
                _ => None,
            })
    }

    /// The conflict keys. In the reader on a conflicted file: `o` `t` `b` take ours, theirs
    /// or both for the hunk under the cursor, Enter the side it is on. In Changes, `e` opens
    /// a conflicted file at its first conflict. `M` opens the operation menu. `false`
    /// leaves the key to the rest of the screen.
    pub(super) fn conflict_key(&mut self, key: &KeyEvent) -> bool {
        if self.page != 0 || key.modifiers.control || key.modifiers.alt || self.form.is_some() {
            return false;
        }
        let pick = match key.code {
            KeyCode::Char('o') => Some(Pick::Ours),
            KeyCode::Char('t') => Some(Pick::Theirs),
            KeyCode::Char('b') => Some(Pick::Both),
            KeyCode::Enter => None,
            KeyCode::Char('e') if self.focus == 0 && self.tutor.is_none() => {
                let Some(relative) = self.conflicted_row() else {
                    return false;
                };
                let line = read_text(&self.root.join(&relative))
                    .and_then(|text| hunks(&text).first().map(|hunk| hunk.start + 1))
                    .unwrap_or(1);
                self.outcome = Some(ScreenOutcome::OpenFileAt(self.root.join(relative), line));
                return true;
            }
            KeyCode::Char('M') if self.operation_bar.is_some() && self.message_scroll.is_none() => {
                let result = self.operation_menu();
                self.report(result);
                return true;
            }
            _ => return false,
        };
        if !self.conflict_reader() || self.preview_anchor.is_some() {
            return false;
        }
        let result = self.pick_hunk(pick);
        self.report(result);
        true
    }

    fn operation_menu(&mut self) -> Result<(), String> {
        let bar = self.operation_bar.clone().ok_or("No Git operation is paused")?;
        let choices = menu_choices(&bar.operation);
        let mut field = Field::new("Action (Up/Down selects)", choices[0].clone());
        field.choices = choices;
        let title = self
            .conflict
            .as_ref()
            .map_or_else(|| bar.operation.clone(), Context::title);
        self.notice.clear();
        self.form = Some(Form {
            title: format!("{title} options"),
            action: FormAction::OperationMenu,
            fields: vec![field],
            selected: 0,
        });
        Ok(())
    }

    /// Takes a side of the conflict under the reader's cursor (`None`: the side the cursor
    /// is on) and writes the file; the file's last conflict also stages it.
    fn pick_hunk(&mut self, pick: Option<Pick>) -> Result<(), String> {
        self.allow_write()?;
        let relative = self.conflicted_row().ok_or("Select a conflicted file")?;
        let name = relative.display().to_string();
        let path = self.root.join(&relative);
        let text = read_text(&path)
            .ok_or_else(|| format!("{name} is not a text file to pick in; e opens it"))?;
        let found = hunks(&text);
        if found.is_empty() {
            return Err(format!("{name} has no conflict markers left; s stages it"));
        }
        self.normalize_preview_cursor();
        let line = self
            .preview_cursor
            .and_then(|cursor| self.preview.rows.get(cursor))
            .and_then(|row| row.new)
            .map(|number| number as usize - 1);
        let hunk = line
            .and_then(|line| found.iter().find(|hunk| hunk.contains(line)))
            .copied()
            .ok_or("Move the cursor into a conflict, then o t or b picks a side")?;
        // The reader must still show this text: an edit since it loaded moves the conflicts.
        let markers = [Some(hunk.start), hunk.base, Some(hunk.middle), Some(hunk.end)];
        let shown = markers.into_iter().flatten().all(|index| {
            self.preview.rows.get(index + 1).is_some_and(|row| {
                row.new == Some(index as u32 + 1) && row.marker == Some(MARKER_ROW)
            })
        });
        if !shown {
            self.load_preview();
            return Err(format!("{name} changed since it was shown; check the conflict and pick again"));
        }
        let pick = match pick {
            Some(pick) => pick,
            None => line
                .and_then(|line| hunk.side(line))
                .ok_or("On a marker line: o t or b picks; on a side, Enter takes it")?,
        };
        let resolved = resolve(&text, &hunk, pick)?;
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
        };
        #[cfg(not(unix))]
        let executable = false;
        crate::files::atomic_write_with_executable(&path, resolved.as_bytes(), executable)
            .map_err(|error| {
                let reason = match &error {
                    crate::files::VfsError::Io { source, .. }
                    | crate::files::VfsError::RootIo { source, .. } => super::words::io_reason(source),
                    _ => String::new(),
                };
                format!("Couldn't save {name}{reason}")
            })?;
        let left = found.len() - 1;
        self.notice = if left > 0 {
            format!(
                "Took {} · {left} conflict{} left in {name}",
                pick.word(),
                if left == 1 { "" } else { "s" }
            )
        } else {
            match crate::git::conflict::stage_resolved(&self.root, &relative) {
                Ok(()) => format!("Took {} · {name} is resolved and staged", pick.word()),
                Err(_) => format!("Used {} · {name} is resolved but couldn't be staged", pick.word()),
            }
        };
        self.refresh_conflicts();
        if left == 0
            && self.conflict.as_ref().is_some_and(|context| context.files.iter().all(|file| file.path == relative))
            && let Some(bar) = &self.operation_bar
        {
            self.notice = format!(
                "{} · every conflict is resolved; C continues the {}",
                self.notice, bar.operation
            );
        }
        self.load_preview();
        self.refresh()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF3: &str = "one\n<<<<<<< HEAD\nmain\n||||||| base\nbase\n=======\nfeature\n>>>>>>> feature\nend\n";

    #[test]
    fn hunks_find_each_conflict_with_its_optional_base() {
        let text = format!("{DIFF3}<<<<<<< HEAD\r\nx\r\n=======\r\ny\r\n>>>>>>> a1b2c3d (Shout)\r\n");
        let found = hunks(&text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], Hunk { start: 1, base: Some(3), middle: 5, end: 7 });
        assert_eq!(found[1], Hunk { start: 9, base: None, middle: 11, end: 13 });
        assert_eq!(found[0].side(2), Some(Pick::Ours));
        assert_eq!(found[0].side(4), Some(Pick::Base));
        assert_eq!(found[0].side(6), Some(Pick::Theirs));
        assert_eq!(found[0].side(5), None);
        // A longer run of signs or a half conflict is not a conflict.
        assert!(hunks("<<<<<<<< x\n=======\n>>>>>>> y\n").is_empty());
        assert!(hunks("<<<<<<< x\n=======\n").is_empty());
    }

    #[test]
    fn resolve_keeps_the_picked_side_and_every_other_byte() {
        let hunk = hunks(DIFF3)[0];
        assert_eq!(resolve(DIFF3, &hunk, Pick::Ours).unwrap(), "one\nmain\nend\n");
        assert_eq!(resolve(DIFF3, &hunk, Pick::Theirs).unwrap(), "one\nfeature\nend\n");
        assert_eq!(resolve(DIFF3, &hunk, Pick::Both).unwrap(), "one\nmain\nfeature\nend\n");
        assert_eq!(resolve(DIFF3, &hunk, Pick::Base).unwrap(), "one\nbase\nend\n");
        let crlf = "<<<<<<< HEAD\r\na\r\n=======\r\nb\r\n>>>>>>> x\r\ntail";
        let hunk = hunks(crlf)[0];
        assert_eq!(resolve(crlf, &hunk, Pick::Theirs).unwrap(), "b\r\ntail");
        assert!(resolve(crlf, &hunk, Pick::Base).is_err());
    }

    #[test]
    fn decorate_labels_each_side_and_marks_its_rows() {
        let mut preview = Preview::source(Path::new("greet.txt"), DIFF3);
        let context = Context {
            kind: "merge".into(),
            ours: Some("main".into()),
            theirs: Some("feature".into()),
            base: Some("1234567".into()),
            ..Context::default()
        };
        assert_eq!(decorate(&mut preview, DIFF3, Some(&context)), Some(2));
        let text = |row: usize| preview.rows[row].text.clone();
        assert_eq!(text(2), "<<<<<<< ours · main");
        assert_eq!(text(4), "||||||| base · 1234567");
        assert_eq!(text(6), "=======");
        assert_eq!(text(8), ">>>>>>> theirs · feature");
        assert_eq!(preview.rows[3].marker, Some(OURS));
        assert_eq!(preview.rows[5].marker, Some(BASE));
        assert_eq!(preview.rows[7].marker, Some(THEIRS));
        assert_eq!(preview.rows[1].marker, None);
        // Without a context, Git's own label names the side.
        let mut plain = Preview::source(Path::new("greet.txt"), DIFF3);
        decorate(&mut plain, DIFF3, None);
        assert_eq!(plain.rows[2].text, "<<<<<<< ours · HEAD");
    }

    #[test]
    fn menu_choices_name_the_operation_and_skip_only_a_replay() {
        assert_eq!(menu_choices("merge"), ["Continue merge", "Abort merge"]);
        assert_eq!(menu_choices("rebase"), ["Continue rebase", "Skip commit", "Abort rebase"]);
        assert_eq!(menu_action("Skip commit"), Some(GitOperationAction::Skip));
        assert_eq!(menu_action("Abort cherry-pick"), Some(GitOperationAction::Abort));
        assert_eq!(menu_action("Continue merge"), Some(GitOperationAction::Continue));
    }
}
