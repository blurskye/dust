//! Native Git selections use the existing checked mutation and bounded command workers.
use super::*;

const CHECK_STAGE: &str = "Couldn't check what is staged";
const STAGE_CHANGED: &str = "Files changed while you were deciding. Try staging again.";

#[derive(Clone)]
pub(super) struct BulkStage {
    paths: BTreeSet<PathBuf>,
    replacing: BTreeSet<PathBuf>,
    index: GitIndexFingerprint,
    head: Option<GitHeadId>,
    notice: Option<String>,
    pub(super) warning: String,
}

fn unmerged_paths(index: &git2::Index) -> Result<BTreeSet<PathBuf>, git2::Error> {
    let mut paths = BTreeSet::new();
    for conflict in index.conflicts()? {
        let conflict = conflict?;
        paths.extend(
            [conflict.ancestor, conflict.our, conflict.their]
                .into_iter()
                .flatten()
                .filter_map(|entry| crate::conflict::git_path(&entry.path)),
        );
    }
    Ok(paths)
}

/// Find staged blobs that would disappear because the worktree is back at HEAD or missing.
fn staged_only_paths(
    repo: &git2::Repository,
    index: &git2::Index,
    paths: &BTreeSet<PathBuf>,
    head: Option<GitHeadId>,
) -> Result<BTreeSet<PathBuf>, git2::Error> {
    let tree = head
        .map(|id| repo.find_commit(git2::Oid::from_bytes(&id.0)?)?.tree())
        .transpose()?;
    let root = repo
        .workdir()
        .ok_or_else(|| git2::Error::from_str("missing working tree"))?;
    let mut present_in_head = BTreeSet::new();
    let mut staged_only = BTreeSet::new();
    for path in paths {
        let Some(staged) = index.get_path(path, 0) else {
            continue;
        };
        let committed = match tree.as_ref().map(|tree| tree.get_path(path)).transpose() {
            Ok(committed) => committed,
            Err(error) if error.code() == git2::ErrorCode::NotFound => None,
            Err(error) => return Err(error),
        };
        if committed
            .as_ref()
            .is_some_and(|entry| entry.id() == staged.id)
        {
            continue;
        }
        match std::fs::symlink_metadata(root.join(path)) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                staged_only.insert(path.clone());
            }
            Err(_) => return Err(git2::Error::from_str("cannot read working tree")),
            Ok(_) if committed.is_some() => {
                present_in_head.insert(path.clone());
            }
            Ok(_) => {}
        }
    }
    if !present_in_head.is_empty() {
        // Two sides only: using the index here would obscure a reverted working copy.
        // Libgit2 compares worktree blob IDs with Git's built-in clean filters applied.
        let mut options = git2::DiffOptions::new();
        options
            .disable_pathspec_match(true)
            .ignore_filemode(true)
            .include_typechange(true);
        for path in &present_in_head {
            options.pathspec(path.as_path());
        }
        let diff = repo.diff_tree_to_workdir(tree.as_ref(), Some(&mut options))?;
        for delta in diff.deltas() {
            if delta.status() != git2::Delta::Deleted
                && let Some(path) = delta.old_file().path()
            {
                present_in_head.remove(path);
            }
        }
        staged_only.append(&mut present_in_head);
    }
    Ok(staged_only)
}

impl GitScreen {
    pub(super) fn stage_directory(&mut self, directory: &Path) -> Result<(), String> {
        self.allow_write()?;
        if self.filters[0].active() {
            return Err(
                "Clear the filter before toggling a whole directory, or select a matching file"
                    .into(),
            );
        }
        let snapshot = self.snapshot.as_ref().ok_or("Status is loading")?;
        if snapshot.truncated {
            return Err(
                "Status is incomplete; toggle individual files instead of a directory".into(),
            );
        }
        let paths = snapshot
            .entries
            .iter()
            .filter(|entry| entry.relative_path.starts_with(directory))
            .map(|entry| entry.relative_path.clone())
            .collect::<BTreeSet<_>>();
        if paths.is_empty() {
            return Err("This directory has no changed descendants".into());
        }
        self.stage_paths(&paths)
    }

    pub(super) fn stage_all(&mut self) -> Result<(), String> {
        let snapshot = self.snapshot.as_ref().ok_or("Status is loading")?;
        let paths = if self.filters[0].active() {
            let paths = self
                .rows(0)
                .iter()
                .filter(|row| !row.directory)
                .map(|row| row.value.clone())
                .collect::<BTreeSet<_>>();
            if paths.is_empty() {
                return Err("No files match the filter".into());
            }
            paths
        } else {
            snapshot
                .entries
                .iter()
                .map(|entry| entry.relative_path.clone())
                .collect()
        };
        if paths.is_empty() {
            return Ok(());
        }
        self.stage_bulk(&paths, true)
    }

    /// Directory and marked-file staging share the same exact-path safeguards as `a`.
    pub(super) fn stage_paths(&mut self, selection: &BTreeSet<PathBuf>) -> Result<(), String> {
        self.stage_bulk(selection, false)
    }

    fn stage_bulk(
        &mut self,
        selection: &BTreeSet<PathBuf>,
        toggle_all: bool,
    ) -> Result<(), String> {
        self.allow_write()?;
        let snapshot = self.snapshot.as_ref().ok_or("Status is loading")?;
        let mut entries = selection
            .iter()
            .filter_map(|path| memo::entry(&snapshot.entries, path))
            .collect::<Vec<_>>();
        if entries.is_empty() || entries.len() != selection.len() {
            return Err("The marked files changed; refresh and select them again".into());
        }
        let (index_before, head) = self.bulk_stage_state()?;
        let (repository, index) = self.stage_index()?;
        let conflicted = unmerged_paths(&index).map_err(|_| CHECK_STAGE)?;
        let conflicts_with = |path: &Path| {
            use std::ops::Bound::{Included, Unbounded};
            path.ancestors()
                .any(|ancestor| conflicted.contains(ancestor))
                || conflicted
                    .range::<Path, _>((Included(path), Unbounded))
                    .next()
                    .is_some_and(|conflict| conflict.starts_with(path))
        };
        entries.retain(|entry| {
            entry.status != GitFileStatus::Conflicted
                && !conflicts_with(&entry.relative_path)
                && entry
                    .original_path
                    .as_deref()
                    .is_none_or(|path| !conflicts_with(path))
        });
        let skipped = selection.len() - entries.len();
        let skip_notice = match skipped {
            0 => String::new(),
            1 => "1 file with conflicts was skipped: resolve it, then stage it on its own.".into(),
            n => format!(
                "{n} files with conflicts were skipped: resolve them, then stage them one at a time."
            ),
        };
        if entries.is_empty() {
            self.notice = skip_notice;
            return Ok(());
        }
        // Dirty submodules can look unstaged in a selection even when their pointer matches.
        let fully_staged = |entry: &GitStatusEntry| {
            entry.sources.staged
                && (!entry.sources.unstaged
                    || (!toggle_all && self.submodule_pointer_matches_index(&entry.relative_path)))
        };
        let all_staged = entries.iter().all(|entry| fully_staged(entry));
        let unstage = match if toggle_all { None } else { self.stage_intent } {
            Some(true) if all_staged => {
                if skipped != 0 {
                    self.notice = skip_notice;
                    return Ok(());
                }
                return Err("Already staged · u unstages them".into());
            }
            Some(true) => false,
            Some(false) if !entries.iter().any(|entry| entry.sources.staged) => {
                if skipped != 0 {
                    self.notice = skip_notice;
                    return Ok(());
                }
                return Err("Nothing staged here · s stages them".into());
            }
            Some(false) => true,
            None => all_staged,
        };
        let mut paths = BTreeSet::new();
        let mut count = 0;
        for entry in entries {
            let needs_change = if unstage {
                entry.sources.staged
            } else {
                !fully_staged(entry)
            };
            if !needs_change {
                continue;
            }
            count += 1;
            paths.insert(entry.relative_path.clone());
            if let Some(original) = &entry.original_path
                && (unstage || index.get_path(original, 0).is_some())
            {
                // The source and destination together are the rename shown by this row.
                paths.insert(original.clone());
            }
        }
        self.check_stage_paths(&index, &paths, unstage)?;
        let notice = (skipped != 0).then(|| {
            let verb = if unstage { "Unstaged" } else { "Staged" };
            let files = if count == 1 { "file" } else { "files" };
            format!("{verb} {count} {files}. {skip_notice}")
        });
        if unstage {
            self.check_bulk_stage_state(index_before, head)?;
            return self.write_stage_paths(paths, true, head.is_some(), notice);
        }

        let replacing =
            staged_only_paths(&repository, &index, &paths, head).map_err(|_| CHECK_STAGE)?;
        self.check_bulk_stage_state(index_before, head)?;
        if let Some(first) = replacing.first() {
            let more = if replacing.len() > 1 {
                format!(" (and {} more)", replacing.len() - 1)
            } else {
                String::new()
            };
            let warning = format!(
                "Staging the working copy would erase the staged version of {}{more}, which isn't saved anywhere else.",
                first.display()
            );
            self.form(
                "Replace staged changes?",
                FormAction::StageAnyway(Box::new(BulkStage {
                    paths,
                    replacing,
                    index: index_before,
                    head,
                    notice,
                    warning,
                })),
                vec![],
            );
            self.form.as_mut().expect("just opened").selected = 1; // Cancel.
            return Ok(());
        }
        self.write_stage_paths(paths, false, head.is_some(), notice)
    }

    fn bulk_stage_state(&self) -> Result<(GitIndexFingerprint, Option<GitHeadId>), String> {
        Ok((
            git_index_fingerprint(&self.root).map_err(|_| CHECK_STAGE)?,
            git_head_id(&self.root).map_err(|_| CHECK_STAGE)?,
        ))
    }

    fn check_bulk_stage_state(
        &self,
        index: GitIndexFingerprint,
        head: Option<GitHeadId>,
    ) -> Result<(), String> {
        if self.bulk_stage_state()? != (index, head) {
            return Err(STAGE_CHANGED.into());
        }
        Ok(())
    }

    fn stage_index(&self) -> Result<(git2::Repository, git2::Index), String> {
        let repo = git2::Repository::open(&self.root).map_err(|_| CHECK_STAGE)?;
        let index = repo.index().map_err(|_| CHECK_STAGE)?;
        Ok((repo, index))
    }

    pub(super) fn stage_anyway(&mut self, plan: BulkStage) -> Result<(), String> {
        self.check_bulk_stage_state(plan.index, plan.head)?;
        let (repository, index) = self.stage_index()?;
        self.check_stage_paths(&index, &plan.paths, false)?;
        let replacing = staged_only_paths(&repository, &index, &plan.paths, plan.head)
            .map_err(|_| CHECK_STAGE)?;
        if !replacing.is_subset(&plan.replacing) {
            return Err(STAGE_CHANGED.into());
        }
        self.check_bulk_stage_state(plan.index, plan.head)?;
        self.write_stage_paths(plan.paths, false, plan.head.is_some(), plan.notice)
    }

    fn check_stage_paths(
        &self,
        index: &git2::Index,
        paths: &BTreeSet<PathBuf>,
        unstage: bool,
    ) -> Result<(), String> {
        // Restoring the index cannot sweep up untracked files from a worktree directory.
        if unstage {
            return Ok(());
        }
        for path in paths {
            let directory = std::fs::symlink_metadata(self.root.join(path))
                .is_ok_and(|metadata| metadata.file_type().is_dir());
            let gitlink = index
                .get_path(path, 0)
                .is_some_and(|entry| entry.mode == 0o160000)
                || self
                    .refs
                    .as_ref()
                    .and_then(|refs| refs.submodules.as_ref().ok())
                    .is_some_and(|modules| modules.iter().any(|module| &module.path == path));
            if directory && !gitlink {
                // A literal pathspec still recurses into a directory. A file replaced
                // by a directory must not silently sweep in unmarked/new descendants.
                return Err(
                    "A selected file is now a directory; review its changed files before staging"
                        .into(),
                );
            }
        }

        Ok(())
    }

    fn write_stage_paths(
        &mut self,
        paths: BTreeSet<PathBuf>,
        unstage: bool,
        has_head: bool,
        notice: Option<String>,
    ) -> Result<(), String> {
        let mut args: Vec<OsString> = vec!["--literal-pathspecs".into()];
        if unstage {
            if has_head {
                args.extend(["restore", "--source=HEAD", "--staged"].map(Into::into));
            } else {
                // An unborn index has no HEAD entries to restore. --cached never removes disk files.
                args.extend(["rm", "--cached", "--ignore-unmatch", "-r"].map(Into::into));
            }
        } else {
            args.extend(["add", "--all"].map(Into::into));
        }
        // Keep arbitrary/non-UTF8 names literal, and large selections below ARG_MAX.
        // JobRequest bounds stdin; no temporary pathspec or request files are needed.
        args.extend(["--pathspec-from-file=-", "--pathspec-file-nul"].map(Into::into));
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_os_str().as_encoded_bytes());
            input.push(0);
        }
        self.write_with_input(args, Some(input))?;
        // Shown when the write finishes, in place of its silent success.
        self.stage_notice = notice;
        Ok(())
    }

    pub(super) fn check_conflict_markers(
        &mut self,
        path: &Path,
        expected_index: GitIndexFingerprint,
    ) -> Result<Option<[u8; 32]>, String> {
        let warned = self.warned_markers.take();
        let (repo, index) = self.stage_index()?;
        if !(1..=3).any(|stage| index.get_path(path, stage).is_some()) {
            return Ok(None);
        }
        let file = self.root.join(path);
        if std::fs::symlink_metadata(&file)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(None);
        }
        let (hash, bytes) =
            worktree_hash(&file).map_err(|_| "Couldn't check for conflict markers")?;
        let size = repo
            .get_attr(
                path,
                "conflict-marker-size",
                git2::AttrCheckFlags::default(),
            )
            .map_err(|_| "Couldn't check for conflict markers")?
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|size| *size > 0);
        if conflicts::has_markers(&String::from_utf8_lossy(&bytes), size) {
            let capture = (path.to_path_buf(), hash, expected_index);
            if warned.as_ref() != Some(&capture) {
                self.warned_markers = Some(capture);
                return Err(format!(
                    "{} still has conflict markers. Press again to stage it anyway.",
                    path.display()
                ));
            }
        }
        // The staging worker must apply exactly the bytes that passed the check.
        Ok(Some(hash))
    }

    pub(super) fn clear_hunks(&mut self) {
        self.hunks.cancel();
        self.hunk_policy = None;
        self.hunk_snapshot = None;
        self.hunk_head = None;
        self.hunk_error = None;
    }

    pub(super) fn start_hunks(&mut self, row: &Row) {
        if self.last_list != 0 || self.files_view != 0 || row.directory || self.all_files {
            return;
        }
        let Some(status) = &self.snapshot else { return };
        let Some(entry) = status
            .entries
            .iter()
            .find(|entry| entry.relative_path == row.value)
        else {
            return;
        };
        if matches!(
            entry.status,
            GitFileStatus::Untracked | GitFileStatus::Conflicted
        ) {
            self.hunk_error =
                Some("Use s in the sidebar for untracked files or resolved conflicts".into());
            return;
        }
        if entry
            .original_path
            .as_ref()
            .is_some_and(|old| old != &row.value)
        {
            self.hunk_error =
                Some("Use s or u in the sidebar to stage or unstage both rename paths".into());
            return;
        }
        let policy = GitHunkPolicy {
            scope_root: self.root.clone(),
            repository_id: status.repository_id,
            status_revision: status.revision,
            root_revision: status.root_revision,
            file: row.value.clone(),
            layer: if self.preview_staged() {
                GitDiffLayer::Staged
            } else {
                GitDiffLayer::Unstaged
            },
            maximum_hunks: 10_000,
            maximum_lines: 100_000,
            maximum_bytes: 8 * 1024 * 1024,
            deadline: Duration::from_secs(5),
        };
        let head = match git_head_id(&self.root) {
            Ok(Some(id)) => GitHeadExpectation::At(id),
            Ok(None) => GitHeadExpectation::Unborn,
            Err(error) => {
                self.hunk_error = Some(error.to_string());
                return;
            }
        };
        match self.hunks.start(policy.clone()) {
            Ok(_) => {
                self.hunk_policy = Some(policy);
                self.hunk_head = Some(head);
            }
            Err(error) => self.hunk_error = Some(error.to_string()),
        }
    }

    fn diff_selection(&self) -> Result<(&GitHunkSnapshot, usize, usize), String> {
        if self.last_list != 0
            || self.files_view != 0
            || self.focus != 4
            || self.commit.is_some()
            || self.all_files
        {
            return Err("Select a working-tree or index diff".into());
        }
        if self.refreshing || self.jobs.preview_loading() {
            return Err(DIFF_REFRESHING.into());
        }
        if let Some(error) = &self.hunk_error {
            return Err(error.clone());
        }
        let snapshot = self
            .hunk_snapshot
            .as_ref()
            .ok_or("Hunks are loading; use s in the sidebar for a whole file")?;
        if self
            .selected_row()
            .is_none_or(|row| row.value != snapshot.file)
            || self
                .snapshot
                .as_ref()
                .is_none_or(|status| status.revision != snapshot.status_revision)
            || (snapshot.layer == GitDiffLayer::Staged) != self.preview_staged()
        {
            return Err("The diff selection changed; refresh before changing code".into());
        }
        let cursor = self
            .preview_cursor
            .ok_or("Move the cursor onto a diff line")?;
        let (hunk, line) = self
            .preview
            .captured_line(cursor, snapshot)
            .ok_or("The displayed hunk no longer matches Git; refresh before changing code")?;
        Ok((snapshot, hunk, line))
    }

    pub(super) fn start_line_range(&mut self) -> Result<(), String> {
        if self.preview_split {
            return Err("Use | for unified view before selecting an exact diff-line range".into());
        }
        if self.preview_anchor.take().is_some() {
            self.notice.clear();
            return Ok(());
        }
        let (snapshot, _, _) = self.diff_selection()?;
        let action = if snapshot.layer == GitDiffLayer::Staged {
            "unstage"
        } else {
            "stage"
        };
        self.preview_anchor = self.preview_cursor;
        self.notice =
            format!("Range started · j/k extend · a {action} selected diff lines · Esc cancel");
        Ok(())
    }

    /// Exact added/removed diff-line indices, never a partial cross-hunk range.
    fn selected_changed_diff_lines(
        &self,
        snapshot: &GitHunkSnapshot,
        hunk_index: usize,
        line_index: usize,
    ) -> Result<Vec<u32>, String> {
        let first = if let Some(anchor) = self.preview_anchor {
            let (anchor_hunk, anchor_line) = self
                .preview
                .captured_line(anchor, snapshot)
                .ok_or("The selected lines changed · Esc cancel")?;
            if anchor_hunk != hunk_index {
                return Err("Select a line range within one hunk; no changes were made".into());
            }
            anchor_line
        } else {
            line_index
        };
        let selected = (first.min(line_index)..=first.max(line_index))
            .filter(|&line| snapshot.hunks[hunk_index].lines[line].kind != GitDiffLineKind::Context)
            .map(|line| line as u32)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err("Select an added or removed line; context lines cannot be selected".into());
        }
        Ok(selected)
    }

    pub(super) fn stage_diff(&mut self, lines: bool) -> Result<(), String> {
        self.allow_write()?;
        let (snapshot, hunk_index, line_index) = self.diff_selection()?;
        let unstage = snapshot.layer == GitDiffLayer::Staged;
        match self.stage_intent {
            Some(true) if unstage => {
                return Err(
                    "This diff is staged · u unstages a hunk, ] shows the working tree".into(),
                );
            }
            Some(false) if !unstage => {
                return Err(
                    "This diff is not staged · ] shows the staged diff, where u unstages".into(),
                );
            }
            _ => {}
        }
        let action = if unstage { "unstage" } else { "stage" };
        if !lines && self.preview_anchor.is_some() {
            return Err(format!(
                "a {action}s the selected range; Esc cancels it before {} {action}s a whole hunk",
                if unstage { "u" } else { "s" }
            ));
        }
        let selected_lines = if lines {
            self.selected_changed_diff_lines(snapshot, hunk_index, line_index)?
        } else {
            Vec::new()
        };
        let hunk = snapshot.hunks[hunk_index].clone();
        let plan = GitChangePlanRequest {
            kind: match (unstage, lines) {
                (true, true) => GitMutationKind::UnstageLines,
                (true, false) => GitMutationKind::UnstageHunk,
                (false, true) => GitMutationKind::StageLines,
                (false, false) => GitMutationKind::StageHunk,
            },
            policy: self
                .hunk_policy
                .clone()
                .ok_or("Can't stage this hunk · r refresh")?,
            hunk_snapshot_revision: snapshot.revision,
            hunk_id: Some(hunk.id),
            selected: Some(hunk),
            selected_lines,
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            expected_head: self.hunk_head.ok_or("Can't stage this hunk · r refresh")?,
            expected_document_revision: None,
            unsaved_documents: Vec::new(),
            maximum_patch_bytes: 1024 * 1024,
        };
        self.mutation
            .start(GitMutationRequest::Plan(plan))
            .map_err(|error| error.to_string())?;
        self.busy = true;
        self.preview_anchor = None;
        self.notice.clear();
        Ok(())
    }

    pub(super) fn discard_diff(&mut self, lines: bool) -> Result<(), String> {
        self.allow_write()?;
        let (snapshot, hunk_index, line_index) = self.diff_selection()?;
        if snapshot.layer != GitDiffLayer::Unstaged {
            return Err("Discard requires an unstaged diff; the index is never discarded".into());
        }
        // An active preview range always wins over the whole-hunk binding. File
        // marks are deliberately not consulted: this is one captured code target.
        let exact = lines || self.preview_anchor.is_some();
        let selected_lines = if exact {
            self.selected_changed_diff_lines(snapshot, hunk_index, line_index)?
        } else {
            Vec::new()
        };
        let hunk = snapshot.hunks[hunk_index].clone();
        let scope = if exact {
            let first = selected_lines[0] + 1;
            let last = selected_lines[selected_lines.len() - 1] + 1;
            if first == last {
                let line = &hunk.lines[first as usize - 1];
                let source = match line.kind {
                    GitDiffLineKind::Add => format!("+{}", line.new_line.unwrap_or(0)),
                    GitDiffLineKind::Delete => format!("-{}", line.old_line.unwrap_or(0)),
                    GitDiffLineKind::Context => unreachable!("only changed lines are selected"),
                };
                format!("Hunk {} · diff line {first} ({source})", hunk_index + 1)
            } else {
                format!(
                    "Hunk {} · diff lines {first}–{last} ({} changes)",
                    hunk_index + 1,
                    selected_lines.len()
                )
            }
        } else {
            format!(
                "Hunk {}/{} · {}",
                hunk_index + 1,
                snapshot.hunks.len(),
                hunk.header.trim()
            )
        };
        let request = GitResetHunkRequest {
            policy: self
                .hunk_policy
                .clone()
                .ok_or("Can't stage this hunk · r refresh")?,
            hunk_snapshot_revision: snapshot.revision,
            hunk_id: hunk.id,
            selected: hunk,
            selected_lines,
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            maximum_patch_bytes: 1024 * 1024,
        };
        let fields = [
            ("File", request.policy.file.display().to_string()),
            ("Scope (index is kept)", scope),
        ]
        .into_iter()
        .map(|(label, value)| {
            let mut field = Field::new(label, value);
            field.read_only = true;
            field
        })
        .collect::<Vec<_>>();
        self.form = Some(Form {
            title: "Discard unstaged change?".into(),
            action: FormAction::Discard(Box::new(request)),
            selected: fields.len(),
            fields,
        });
        self.notice.clear();
        Ok(())
    }
}

/// What a path looked like when a file discard sheet opened. The sheet confirms exactly
/// this; any later change to the index entry or the bytes on disk refuses the discard.
#[derive(Clone, Debug, Eq, PartialEq)]
enum DiskIdentity {
    Missing,
    File { blob: git2::Oid, executable: bool },
    Symlink(PathBuf),
    Directory,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscardTarget {
    path: PathBuf,
    /// `(blob, mode)` of the stage-0 index entry; `None` for an untracked path, which is
    /// deleted rather than restored.
    index: Option<(git2::Oid, u32)>,
    disk: DiskIdentity,
}

/// A confirmed Changes-pane discard (`d`): the worktree side of each row is
/// restored from the index, and untracked files are deleted. The index is never touched.
#[derive(Clone, Debug)]
pub(super) struct FileDiscard {
    targets: Vec<DiscardTarget>,
    /// Rows the sheet named, for the receipt.
    restored: Vec<PathBuf>,
    deleted: Vec<PathBuf>,
    /// Marks to clear once the discard is done.
    marked: bool,
}

const INTENT_TO_ADD: u16 = 1 << 13;

fn disk_identity(path: &Path) -> Result<DiskIdentity, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DiskIdentity::Missing);
        }
        Err(error) => {
            return Err(format!(
                "Couldn't read {}{}",
                path.display(),
                super::words::io_reason(&error)
            ));
        }
    };
    let kind = metadata.file_type();
    Ok(if kind.is_symlink() {
        DiskIdentity::Symlink(std::fs::read_link(path).map_err(|error| error.to_string())?)
    } else if kind.is_file() {
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        DiskIdentity::File {
            blob: git2::Oid::hash_file(git2::ObjectType::Blob, path)
                .map_err(|error| error.message().to_owned())?,
            executable,
        }
    } else if kind.is_dir() {
        DiskIdentity::Directory
    } else {
        DiskIdentity::Other
    })
}

/// The identity of one path, refusing what a file discard must never touch.
fn discard_target(root: &Path, index: &git2::Index, path: &Path) -> Result<DiscardTarget, String> {
    if !path
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!("{} is not a repository path", path.display()));
    }
    let entry = index.get_path(path, 0);
    if let Some(entry) = &entry {
        if entry.mode == 0o160000 {
            return Err(format!(
                "{} is a submodule; discard its changes inside the submodule",
                path.display()
            ));
        }
        if entry.flags_extended & INTENT_TO_ADD != 0 {
            return Err(format!(
                "{} is marked intent-to-add; stage or unstage it before discarding",
                path.display()
            ));
        }
    }
    let disk = disk_identity(&root.join(path))?;
    let refusal = match disk {
        DiskIdentity::Directory => Some("is now a directory; review its files before discarding"),
        DiskIdentity::Other => Some("has an unsupported path type"),
        DiskIdentity::Missing if entry.is_none() => Some("no longer exists; refresh with r"),
        _ => None,
    };
    if let Some(refusal) = refusal {
        return Err(format!("{} {refusal}", path.display()));
    }
    Ok(DiscardTarget {
        path: path.to_path_buf(),
        index: entry.map(|entry| (entry.id, entry.mode)),
        disk,
    })
}

/// Recheck every captured identity, then restore tracked paths from the index and delete
/// untracked ones. Nothing is written unless every path still matches the sheet.
fn apply_file_discard(root: &Path, plan: &FileDiscard) -> Result<(), String> {
    let repository = git2::Repository::open(root).map_err(|error| error.message().to_owned())?;
    let mut index = repository
        .index()
        .map_err(|error| error.message().to_owned())?;
    for target in &plan.targets {
        let now = discard_target(root, &index, &target.path)?;
        if &now != target {
            return Err(format!(
                "{} changed since this sheet opened; nothing was discarded",
                target.path.display()
            ));
        }
    }
    let restore = plan
        .targets
        .iter()
        .filter(|target| target.index.is_some())
        .collect::<Vec<_>>();
    if !restore.is_empty() {
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout
            .force()
            .update_index(false)
            .disable_pathspec_match(true);
        for target in &restore {
            checkout.path(target.path.as_path());
        }
        repository
            .checkout_index(Some(&mut index), Some(&mut checkout))
            .map_err(|error| {
                format!(
                    "Couldn't discard changes{}",
                    super::words::git_reason(error.message())
                )
            })?;
    }
    for target in plan.targets.iter().filter(|target| target.index.is_none()) {
        std::fs::remove_file(root.join(&target.path)).map_err(|error| {
            format!(
                "Couldn't delete {}{}",
                target.path.display(),
                super::words::io_reason(&error)
            )
        })?;
    }
    Ok(())
}

impl GitScreen {
    /// `d` in Changes: the marked set if any, else the selected row. `D` is the marked set.
    pub(super) fn discard_files_form(&mut self, marked_only: bool) -> Result<(), String> {
        self.allow_write()?;
        if self.all_files {
            return Err("F returns to changed files before discarding".into());
        }
        let marked = !self.file_selection.marked.is_empty();
        let selection = if marked {
            self.finish_file_range();
            self.file_selection.marked.clone()
        } else if marked_only {
            return Err("No marked files · x marks files, then D discards them".into());
        } else {
            let row = self
                .selected_row()
                .filter(|_| self.last_list == 0)
                .ok_or("Select a changed file")?;
            if row.directory {
                return Err(
                    "Select a file, or mark files with x; a directory is not discarded whole"
                        .into(),
                );
            }
            BTreeSet::from([row.value])
        };
        let snapshot = self.snapshot.as_ref().ok_or("Status is loading")?;
        let entries = snapshot
            .entries
            .iter()
            .filter(|entry| selection.contains(&entry.relative_path))
            .collect::<Vec<_>>();
        if entries.is_empty() || entries.len() != selection.len() {
            return Err(if marked {
                "The marked files changed; refresh and select them again".into()
            } else {
                "Select a changed file".into()
            });
        }
        let repository = git2::Repository::open(&self.root).map_err(|error| error.to_string())?;
        let index = repository.index().map_err(|error| error.to_string())?;
        let mut targets = Vec::new();
        let (mut restored, mut deleted, mut kept) = (Vec::new(), Vec::new(), Vec::new());
        let mut rows = Vec::new();
        for entry in entries {
            let path = &entry.relative_path;
            if entry.status == GitFileStatus::Conflicted {
                return Err(format!(
                    "{} is conflicted; resolve it with o/t or Enter instead of discarding",
                    path.display()
                ));
            }
            if !entry.sources.unstaged {
                kept.push(path.clone());
                continue;
            }
            let target = discard_target(&self.root, &index, path)?;
            if target.index.is_none() {
                // Not in the index (untracked, or re-created after a staged delete): the
                // worktree change IS the file, so discarding it deletes it.
                targets.push(target);
                deleted.push(path.clone());
                rows.push(("Delete", format!("{} · untracked", path.display())));
                continue;
            }
            targets.push(target);
            let code = entry.change_code();
            // An unstaged rename's source is still in the index: restore it too, so the
            // row's whole worktree change is undone. A staged rename's source is not.
            let mut label = format!("{} · {}", path.display(), code.trim());
            if let Some(original) = entry
                .original_path
                .as_ref()
                .filter(|original| *original != path)
            {
                if index.get_path(original, 0).is_some() {
                    targets.push(discard_target(&self.root, &index, original)?);
                }
                label = format!("{label} · renamed from {}", original.display());
            }
            if entry.sources.staged {
                label.push_str(" · staged part kept");
            }
            restored.push(path.clone());
            rows.push(("Restore", label));
        }
        if targets.is_empty() {
            return Err(match kept.as_slice() {
                [path] => format!(
                    "{} has only staged changes; nothing in the worktree to discard (u unstages)",
                    path.display()
                ),
                _ => {
                    "The marked files have only staged changes; nothing in the worktree to discard"
                        .into()
                }
            });
        }
        let files = restored.len() + deleted.len();
        let title = match (restored.as_slice(), deleted.as_slice()) {
            ([path], []) => format!("Discard changes in {}?", path.display()),
            ([], [path]) => format!("Delete untracked file {}?", path.display()),
            _ => format!("Discard changes in {files} files?"),
        };
        let mut fields = Vec::new();
        if files == 1 {
            let (_, row) = rows.remove(0);
            fields.push(("Target", row));
        } else {
            fields.push(("Target", format!("{files} files")));
            const SHOWN: usize = 8;
            let more = rows.len().saturating_sub(SHOWN);
            fields.extend(rows.into_iter().take(SHOWN));
            if more > 0 {
                fields.push(("More", format!("{more} more file(s)")));
            }
        }
        if !restored.is_empty() {
            fields.push(("Scope", "Worktree only; the index keeps its copy.".into()));
        }
        if !deleted.is_empty() {
            fields.push(("Untracked", "Deleted from disk; Git keeps no copy.".into()));
        }
        if !kept.is_empty() {
            fields.push((
                "Kept",
                format!("{} staged-only file(s) have nothing to discard", kept.len()),
            ));
        }
        let fields = fields
            .into_iter()
            .map(|(label, value)| {
                let mut field = Field::new(label, value);
                field.read_only = true;
                field.input.cursor = 0;
                field
            })
            .collect::<Vec<_>>();
        self.form = Some(Form {
            title,
            action: FormAction::DiscardFiles(Box::new(FileDiscard {
                targets,
                restored,
                deleted,
                marked,
            })),
            // The cursor rests on the primary Discard button.
            selected: fields.len(),
            fields,
        });
        self.notice.clear();
        Ok(())
    }

    /// Submit a confirmed file discard: its captured targets, never the current rows.
    pub(super) fn discard_files(&mut self, plan: &FileDiscard) -> Result<(), String> {
        apply_file_discard(&self.root, plan)?;
        let receipt = match (plan.restored.as_slice(), plan.deleted.as_slice()) {
            ([path], []) => format!("Discarded worktree changes in {}", path.display()),
            ([], [path]) => format!("Deleted untracked file {}", path.display()),
            (restored, deleted) => {
                let mut text = format!(
                    "Discarded changes in {} files",
                    restored.len() + deleted.len()
                );
                if !deleted.is_empty() {
                    text.push_str(&format!(" · {} untracked deleted", deleted.len()));
                }
                text
            }
        };
        if plan.marked {
            self.clear_file_selection();
        }
        self.preview_anchor = None;
        self.receipt = Some((ReceiptTone::Ok, receipt));
        // Like a Git write: no further write until the refreshed status lands.
        let result = self.refresh();
        self.refreshing_after_write = result.is_ok();
        result
    }
}
