//! The pending review (DESIGN.md §3.28, §3.30): inline comments held for the next review of a
//! pull request. They are kept under the forge state directory (never in the repository),
//! per host, repository and pull request, so a restart keeps them until the review is sent
//! or they are deleted. `V` in the Files reader marks the first line of a range; `c` on the
//! last line comments on the whole range.
use super::super::shell::glyphs::{Glyph, glyph};
use super::lines::Line;
use super::prose::{Rich, role, strong};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::Side;
use crate::forge::request::InlineDraft;
use crate::ui::{Emphasis, GitChip, GitRole, GitSurface, Style};
use unicode_width::UnicodeWidthStr;

pub(super) type Drafts = BTreeMap<u64, Vec<(InlineDraft, String)>>;

const SCHEMA: u64 = 1;

/// Where a range comment starts (`V` in the Files reader).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Anchor {
    pub(super) number: u64,
    pub(super) path: String,
    pub(super) line: u32,
    pub(super) side: &'static str,
}

#[derive(Default)]
pub(super) struct Review {
    /// The (host, repository) whose drafts `inline_drafts` holds.
    scope: Option<(String, String)>,
    /// What the state directory holds for that scope: only changes are written.
    saved: Drafts,
    pub(super) anchor: Option<Anchor>,
}

fn segment(text: &str) -> String {
    let mut escaped = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

/// `<state>/drafts/<host>/<repository>/`; `None` when there is no state directory.
pub(super) fn directory(host: &str, repo: &str) -> Option<PathBuf> {
    let env = crate::forge::credential::Env::process();
    // Unit tests never touch the real state directory.
    if cfg!(test) && env.get("DUSTY_FORGE_STATE_DIR").is_none() {
        return None;
    }
    let state = crate::forge::detect::hosts_path(&env)?.parent()?.to_path_buf();
    Some(state.join("drafts").join(segment(host)).join(segment(repo)))
}

fn side_name(side: Side) -> &'static str {
    side.as_github()
}

pub(super) fn encode(drafts: &[(InlineDraft, String)]) -> Value {
    serde_json::json!({
        "schema": SCHEMA,
        "drafts": drafts
            .iter()
            .map(|(draft, head)| {
                serde_json::json!({
                    "path": draft.path,
                    "line": draft.line,
                    "start_line": draft.start_line,
                    "side": side_name(draft.side),
                    "body": draft.body,
                    "head": head,
                })
            })
            .collect::<Vec<_>>(),
    })
}

pub(super) fn decode(value: &Value) -> Vec<(InlineDraft, String)> {
    if value["schema"].as_u64() != Some(SCHEMA) {
        return Vec::new();
    }
    value["drafts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|draft| {
            let path = draft["path"].as_str()?.to_owned();
            let body = draft["body"].as_str()?.to_owned();
            let line = draft["line"].as_u64()?;
            if path.is_empty() || body.trim().is_empty() || line == 0 {
                return None;
            }
            Some((
                InlineDraft {
                    path,
                    line,
                    start_line: draft["start_line"].as_u64(),
                    side: if draft["side"].as_str() == Some("LEFT") { Side::Left } else { Side::Right },
                    body,
                },
                draft["head"].as_str().unwrap_or_default().to_owned(),
            ))
        })
        .collect()
}

pub(super) fn load(host: &str, repo: &str) -> Drafts {
    directory(host, repo).map(|directory| load_from(&directory)).unwrap_or_default()
}

fn load_from(directory: &std::path::Path) -> Drafts {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Drafts::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                return None;
            }
            let number = path.file_stem()?.to_str()?.parse::<u64>().ok()?;
            let text = std::fs::read_to_string(&path).ok()?;
            let drafts = decode(&serde_json::from_str(&text).ok()?);
            (!drafts.is_empty()).then_some((number, drafts))
        })
        .collect()
}

fn save(host: &str, repo: &str, number: u64, drafts: &[(InlineDraft, String)]) -> Result<(), String> {
    match directory(host, repo) {
        Some(directory) => save_in(&directory, number, drafts),
        None => Ok(()),
    }
}

fn save_in(directory: &std::path::Path, number: u64, drafts: &[(InlineDraft, String)]) -> Result<(), String> {
    let file = directory.join(format!("{number}.json"));
    if drafts.is_empty() {
        return match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    std::io::Write::write_all(&mut temporary, encode(drafts).to_string().as_bytes())
        .map_err(|error| error.to_string())?;
    temporary.persist(&file).map_err(|error| error.error.to_string())?;
    Ok(())
}

/// `line 14` or `lines 12–14`.
pub(super) fn anchor_text(draft: &InlineDraft) -> String {
    match draft.range() {
        Some((start, end)) => format!("lines {start}–{end}"),
        None => format!("line {}", draft.line),
    }
}

/// `1 pending comment`, `2 pending comments`: GitHub's words for the comments a review holds
/// until it is submitted (§3.30a).
pub(super) fn pending(count: usize) -> String {
    plural(count, "pending comment")
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// Words wrapped to `width` cells; a word longer than the row is cut.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut rows = Vec::new();
    for paragraph in text.lines() {
        let mut row = String::new();
        for word in paragraph.split_whitespace() {
            let mut word = word.to_owned();
            while word.width() > width {
                let mut used = 0;
                let cut = word
                    .char_indices()
                    .find(|(_, ch)| {
                        used += unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(0);
                        used > width
                    })
                    .map_or(word.len(), |(at, _)| at.max(1));
                if !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                }
                rows.push(word[..cut].to_owned());
                word = word[cut..].to_owned();
            }
            if row.is_empty() {
                row = word;
            } else if row.width() + 1 + word.width() <= width {
                row.push(' ');
                row.push_str(&word);
            } else {
                rows.push(std::mem::replace(&mut row, word));
            }
        }
        rows.push(row);
    }
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

impl Workspace {
    /// The drafts waiting for the review of `number`.
    pub(super) fn review_drafts(&self, number: u64) -> &[(InlineDraft, String)] {
        self.inline_drafts.get(&number).map(Vec::as_slice).unwrap_or_default()
    }

    /// From `poll`: loads the drafts of the repository on screen and writes what changed.
    pub(super) fn sync_drafts(&mut self) -> bool {
        if self.host.is_empty() || self.repo.is_empty() {
            return false;
        }
        let scope = (self.host.clone(), self.repo.clone());
        if self.review.scope.as_ref() == Some(&scope) {
            self.flush_drafts();
            return false;
        }
        let first = self.review.scope.is_none();
        self.flush_drafts();
        let loaded = load(&scope.0, &scope.1);
        let mut drafts = loaded.clone();
        if first {
            // Drafts made before the repository first resolved stay with it.
            for (number, held) in std::mem::take(&mut self.inline_drafts) {
                drafts.insert(number, held);
            }
        }
        self.inline_drafts = drafts;
        self.review.saved = loaded;
        self.review.scope = Some(scope);
        self.review.anchor = None;
        self.flush_drafts();
        true
    }

    fn flush_drafts(&mut self) {
        let Some((host, repo)) = self.review.scope.clone() else {
            return;
        };
        if self.inline_drafts == self.review.saved {
            return;
        }
        let numbers = self
            .inline_drafts
            .keys()
            .chain(self.review.saved.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        for number in numbers {
            let now = self.inline_drafts.get(&number).map(Vec::as_slice).unwrap_or_default();
            if self.review.saved.get(&number).map(Vec::as_slice).unwrap_or_default() == now {
                continue;
            }
            if save(&host, &repo, number, now).is_err() {
                self.notice = "Couldn't save review drafts".into();
                self.notice_error = true;
            }
        }
        self.review.saved = self.inline_drafts.clone();
        self.inline_drafts.retain(|_, drafts| !drafts.is_empty());
        self.review.saved.retain(|_, drafts| !drafts.is_empty());
    }

    fn files_reader(&self) -> bool {
        self.page == 2 && self.pr_tab == pr::FILES && self.focus == 2
    }

    /// Files reader keys for pending comments: `V` marks a range, Esc drops it, `c` (or `C`,
    /// §3.30a) on a pending comment edits it and on the range's last line comments on the range.
    pub(super) fn review_key(&mut self, key: &KeyEvent) -> Option<Result<Option<Form>, String>> {
        if !self.files_reader() || key.modifiers.control || key.modifiers.alt {
            return None;
        }
        let row = self.detail_rows.get(self.detail_cursor).and_then(|row| row.action.clone());
        match key.code {
            KeyCode::Escape if self.review.anchor.is_some() => {
                self.review.anchor = None;
                self.notice.clear();
                self.notice_error = false;
                Some(Ok(None))
            }
            KeyCode::Char('V') => {
                let Some(Action::Inline { number, path, line, side, .. }) = row else {
                    return Some(Err("Move to a line of the diff to start a range".into()));
                };
                let anchor = Anchor { number, path, line, side };
                if self.review.anchor.as_ref() == Some(&anchor) {
                    self.review.anchor = None;
                    self.notice.clear();
                } else {
                    self.notice = format!("Range from line {line} · j/k extend, C comments · Esc cancels");
                    self.review.anchor = Some(anchor);
                }
                self.notice_error = false;
                Some(Ok(None))
            }
            KeyCode::Char('c' | 'C') => match row {
                Some(action @ Action::Draft(..)) => Some(self.activate(action)),
                Some(Action::Inline { number, path, line, side, head }) if self.review.anchor.is_some() => {
                    let anchor = self.review.anchor.clone()?;
                    if anchor.number != number || anchor.path != path || anchor.side != side {
                        return Some(Err("A range stays in one file and on one side · Esc cancels it".into()));
                    }
                    if anchor.line == line {
                        self.review.anchor = None;
                        return Some(self.activate(Action::Inline { number, path, line, side, head }));
                    }
                    Some(self.activate(Action::InlineRange {
                        number,
                        path,
                        start: anchor.line.min(line),
                        line: anchor.line.max(line),
                        side,
                        head,
                    }))
                }
                // With a range open, `c` belongs to the range: a thread under the cursor is
                // not where it ends, and replying there would leave the range dangling.
                _ if self.review.anchor.is_some() => {
                    Some(Err("Move to a line of the diff to end the range · Esc cancels it".into()))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The sheet for a range comment or for editing a draft.
    pub(super) fn draft_form(&mut self, action: &Action) -> Result<Option<Form>, String> {
        match action {
            Action::InlineRange { path, start, line, .. } => {
                if let Some(reason) = self.capabilities().inline_comment.reason().filter(|_| self.read_only()) {
                    return Err(reason.into());
                }
                if let Some(reason) = self.capabilities().batched_review.reason() {
                    return Err(format!("{reason} · comment on one line with c"));
                }
                let fields = vec![actions::field("Comment", "", true, true)];
                Ok(Some(self.form(&format!("Comment on {path}:{start}–{line}"), action.clone(), fields)))
            }
            Action::Draft(number, index) => {
                let (draft, _) = self
                    .review_drafts(*number)
                    .get(*index)
                    .cloned()
                    .ok_or("That draft was already sent or deleted")?;
                // The saved text, not an older typed value for the same slot.
                self.drafts.remove(&self.draft_key(action));
                let fields = vec![
                    actions::field("Comment", &draft.body, true, true),
                    actions::choice("Pending", "Keep", &["Keep", "Delete"]),
                ];
                Ok(Some(self.form(&format!("Pending comment on {}:{}", draft.path, draft_lines(&draft)), action.clone(), fields)))
            }
            _ => Ok(None),
        }
    }

    /// Saves a range comment or an edited draft; `None` for every other action.
    pub(super) fn submit_draft(&mut self, action: &Action, values: &[String]) -> Option<Result<(), String>> {
        let result = match action {
            Action::InlineRange { number, path, start, line, side, head } => {
                let body = values.first().map(|body| body.trim()).unwrap_or_default();
                if body.is_empty() {
                    return Some(Err("Comment is required".into()));
                }
                self.inline_drafts.entry(*number).or_default().push((
                    InlineDraft {
                        path: path.clone(),
                        line: u64::from(*line),
                        start_line: Some(u64::from(*start)),
                        side: if *side == "LEFT" { Side::Left } else { Side::Right },
                        body: body.to_owned(),
                    },
                    head.clone(),
                ));
                self.review.anchor = None;
                self.notice = format!("Added to your review · {}", pending(self.review_drafts(*number).len()));
                self.notice_transient = true;
                Ok(())
            }
            Action::Draft(number, index) => {
                let delete = values.get(1).map(String::as_str) == Some("Delete")
                    || values.first().is_none_or(|body| body.trim().is_empty());
                let Some(drafts) = self.inline_drafts.get_mut(number).filter(|drafts| *index < drafts.len()) else {
                    return Some(Err("That draft was already sent or deleted".into()));
                };
                if delete {
                    drafts.remove(*index);
                } else {
                    drafts[*index].0.body = values[0].trim().to_owned();
                }
                let left = drafts.len();
                self.notice = match (delete, left) {
                    (true, 0) => "Pending comment deleted · no review pending".into(),
                    (true, left) => format!("Pending comment deleted · {} · r review", pending(left)),
                    (false, _) => "Pending comment updated".into(),
                };
                Ok(())
            }
            _ => return None,
        };
        self.drafts.remove(&self.draft_key(action));
        self.notice_error = false;
        self.flush_drafts();
        Some(result)
    }

    /// The line under a sheet's title for draft sheets.
    pub(super) fn draft_note(&self, action: &Action) -> Option<String> {
        match action {
            Action::InlineRange { number, .. } => {
                let waiting = self.review_drafts(*number).len();
                Some(if waiting == 0 {
                    "Starts a review · held as pending until you submit it (r)".into()
                } else {
                    format!("Joins your pending review · {} already", pending(waiting))
                })
            }
            Action::Draft(..) => Some("Only you see pending comments until the review is submitted".into()),
            Action::Review(number) => {
                let drafts = self.review_drafts(*number);
                (!drafts.is_empty()).then(|| format!("Submits {} as one review", pending(drafts.len())))
            }
            _ => None,
        }
    }
}

/// `4` or `2–4`: the line or lines a draft is on.
pub(super) fn draft_lines(draft: &InlineDraft) -> String {
    match draft.range() {
        Some((start, end)) => format!("{start}–{end}"),
        None => draft.line.to_string(),
    }
}

/// `2 drafts` for the file heading; empty when there are none.
pub(super) fn file_count(workspace: &Workspace, id: u64, path: &str) -> usize {
    workspace.review_drafts(id).iter().filter(|(draft, _)| draft.path == path).count()
}

/// The rows of the Files diff between the range's first line and the cursor, tinted.
pub(super) fn mark_range(workspace: &Workspace, id: u64, path: &str, rows: &mut [Line], diff_start: usize) {
    let Some(anchor) = workspace.review.anchor.as_ref().filter(|anchor| anchor.number == id && anchor.path == path) else {
        return;
    };
    let cursor = match workspace.detail_rows.get(workspace.detail_cursor).and_then(|row| row.action.as_ref()) {
        Some(Action::Inline { line, side, path: at, .. }) if *side == anchor.side && at == path => *line,
        _ => anchor.line,
    };
    let (low, high) = (anchor.line.min(cursor), anchor.line.max(cursor));
    for row in rows.iter_mut().skip(diff_start) {
        if let Some(Action::Inline { line, side, .. }) = &row.action
            && *side == anchor.side
            && (low..=high).contains(line)
        {
            row.band = Some(GitSurface::CursorInactive);
            for band in &mut row.bands {
                band.1 = GitSurface::CursorInactive;
            }
            if row.gutter.is_empty() {
                continue;
            }
            row.gutter_style = Style::Git { role: GitRole::Focus, surface: GitSurface::CursorInactive, emphasis: Emphasis::Bold };
        }
    }
}

/// Draft cards under the lines they are about (after any review thread there); drafts whose
/// line is not in this diff follow it.
pub(super) fn place(workspace: &Workspace, id: u64, path: &str, rows: &mut Vec<Line>, diff_start: usize, width: usize, mode: ExplorerIconMode) {
    let rail = Style::Git { role: GitRole::Modified, surface: GitSurface::Code, emphasis: Emphasis::None };
    let mut placed: Vec<(usize, usize, Vec<Line>)> = Vec::new();
    let mut trailing = Vec::new();
    for (index, (draft, _)) in workspace.review_drafts(id).iter().enumerate() {
        if draft.path != path {
            continue;
        }
        let side = if draft.side == Side::Left { "LEFT" } else { "RIGHT" };
        let mut card = Vec::new();
        let header = Rich::new()
            .push(format!("{} ", glyph(Glyph::Comment, mode)), Style::Git { role: GitRole::Modified, surface: GitSurface::Code, emphasis: Emphasis::None })
            // GitHub's `Pending` label: the comment waits for the review to be submitted.
            .chip("Pending", Style::Git { role: GitRole::Modified, surface: GitSurface::Chip(GitChip::Modified), emphasis: Emphasis::Bold })
            .push(" ", Style::Git { role: GitRole::Muted, surface: GitSurface::Code, emphasis: Emphasis::None })
            .push(anchor_text(draft), Style::Git { role: GitRole::Muted, surface: GitSurface::Code, emphasis: Emphasis::None })
            .line();
        card.push(header);
        let measure = width.saturating_sub(6).min(super::prose::MEASURE);
        let prose = |card: &mut Vec<Line>, text: &str| {
            for text in wrap(text, measure) {
                card.push(Rich::new().push(text, Style::Git { role: GitRole::Text, surface: GitSurface::Code, emphasis: Emphasis::None }).line());
            }
        };
        match super::suggestion::split(&draft.body) {
            // A suggested change reads as a diff against the lines on screen (§3.30a).
            Some(parts) => {
                let start = draft.start_line.unwrap_or(draft.line);
                let original = (start..=draft.line)
                    .filter_map(|line| {
                        rows[diff_start..].iter().find_map(|row| match (&row.action, &row.source) {
                            (Some(Action::Inline { line: at, side: s, .. }), Some(source)) if u64::from(*at) == line && *s == side => {
                                Some(source.text.to_string())
                            }
                            _ => None,
                        })
                    })
                    .collect::<Vec<_>>();
                for part in parts {
                    match part {
                        super::suggestion::Part::Text(text) => prose(&mut card, &text),
                        super::suggestion::Part::Suggestion(lines) => super::suggestion::rows(&mut card, &original, &lines, width.saturating_sub(4), ""),
                    }
                }
            }
            None => prose(&mut card, &draft.body),
        }
        for row in &mut card {
            row.band = Some(row.band.filter(|band| matches!(band, GitSurface::AddedBand | GitSurface::RemovedBand)).unwrap_or(GitSurface::Code));
            row.gutter = "  ▍ ".into();
            row.gutter_style = rail;
            row.pan = false;
            row.action = Some(Action::Draft(id, index));
        }
        let line = draft.line as u32;
        let target = rows[diff_start..]
            .iter()
            .rposition(|row| matches!(&row.action, Some(Action::Inline { line: at, side: s, .. }) if *at == line && *s == side))
            .map(|at| {
                // After the review threads already under that line.
                let mut at = diff_start + at + 1;
                while rows.get(at).is_some_and(|row| row.band == Some(GitSurface::Code) && !matches!(row.action, Some(Action::Inline { .. }))) {
                    at += 1;
                }
                at
            });
        match target {
            Some(at) => placed.push((at, index, card)),
            None => trailing.extend(card),
        }
    }
    placed.sort_by_key(|(at, index, _)| std::cmp::Reverse((*at, *index)));
    for (at, _, card) in placed {
        rows.splice(at..at, card);
    }
    if !trailing.is_empty() {
        rows.push(super::prose::blank());
        rows.extend(trailing);
    }
}

/// `› Review changes · 2 pending`.
pub(super) fn review_row(workspace: &Workspace, id: u64) -> Line {
    let mut review = Rich::new()
        .push("› ", role(GitRole::Focus))
        .push("Review changes", role(GitRole::Link));
    let waiting = workspace.review_drafts(id).len();
    if waiting > 0 {
        review = review.sep().push(format!("{waiting} pending"), strong(GitRole::Modified));
    }
    let mut review = review.line();
    review.action = Some(Action::Review(id));
    review
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(line: u64, start: Option<u64>, body: &str) -> (InlineDraft, String) {
        (
            InlineDraft { path: "src/lib.rs".into(), line, start_line: start, side: Side::Right, body: body.into() },
            "a".repeat(40),
        )
    }

    #[test]
    fn drafts_round_trip_through_their_file_format() {
        let drafts = vec![draft(14, None, "Name this"), draft(22, Some(18), "Split this block\nin two")];
        assert_eq!(decode(&encode(&drafts)), drafts);
        // An unknown schema reads as nothing rather than as wrong drafts.
        let mut other = encode(&drafts);
        other["schema"] = 9.into();
        assert!(decode(&other).is_empty());
    }

    #[test]
    fn a_range_reads_as_lines_and_a_single_line_as_line() {
        assert_eq!(anchor_text(&draft(22, Some(18), "x").0), "lines 18–22");
        assert_eq!(anchor_text(&draft(22, None, "x").0), "line 22");
        assert_eq!(anchor_text(&draft(22, Some(22), "x").0), "line 22");
    }

    #[test]
    fn draft_bodies_wrap_on_words() {
        assert_eq!(wrap("one two three four", 9), vec!["one two", "three", "four"]);
        assert_eq!(wrap("first\n\nsecond", 20), vec!["first", "", "second"]);
        assert_eq!(wrap("abcdefghijkl", 8), vec!["abcdefgh", "ijkl"]);
    }

    #[test]
    fn drafts_are_saved_per_pull_request_and_survive_a_reload() {
        let state = tempfile::tempdir().unwrap();
        let directory = state.path().join("drafts/github.com").join(segment("example/demo"));
        let drafts = vec![draft(14, None, "Name this"), draft(22, Some(18), "Range")];
        save_in(&directory, 9, &drafts).unwrap();
        assert_eq!(load_from(&directory).get(&9), Some(&drafts));
        assert!(directory.ends_with("example%2Fdemo"));
        let file = directory.join("9.json");
        assert!(file.is_file(), "drafts live under the state directory: {file:?}");
        // Sending or deleting the last draft removes the file.
        save_in(&directory, 9, &[]).unwrap();
        assert!(!file.exists());
        assert!(load_from(&directory).is_empty());
    }
}
