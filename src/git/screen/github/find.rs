//! Find is local to the currently rendered detail, not a GitHub query or an action draft.
use super::super::search::{SourceMatch, SourceSearch};
use super::*;
use std::sync::Arc;

/// A displayed prose fragment retains its rendered, unwrapped logical line.
/// Source and log rows use offset zero. Controls and identity rows have no source.
pub(super) struct Source {
    pub text: Arc<str>,
    pub offset: usize,
}
impl Source {
    pub fn new(text: &str) -> Self {
        Self {
            text: Arc::from(text),
            offset: 0,
        }
    }
}

#[derive(PartialEq, Eq)]
enum Content {
    Item(Key, u64, usize, String, Option<String>),
    Workflow(u64),
}
#[derive(PartialEq, Eq)]
struct Identity {
    host: String,
    repo: String,
    content: Content,
}
#[derive(Default)]
pub(super) struct DetailFind {
    identity: Option<Identity>,
    source: Vec<Arc<str>>,
    // Display row -> (logical source row, byte offset). Rebuilt on resize.
    fragments: Vec<Option<(usize, usize)>>,
    pub search: SourceSearch,
    // This native source caret survives clearing/replacing a query with no match.
    // It is not evidence that SourceSearch still has a selected occurrence.
    cursor: Option<(usize, usize)>,
    prompt: bool,
    reveal_horizontal: Option<usize>,
}
impl DetailFind {
    fn sync(&mut self, identity: Identity, rows: &[lines::Line]) -> Option<usize> {
        let mut source = Vec::new();
        self.fragments = rows
            .iter()
            .map(|row| {
                row.source.as_ref().map(|fragment| {
                    if fragment.offset == 0 {
                        source.push(fragment.text.clone());
                    }
                    (source.len() - 1, fragment.offset)
                })
            })
            .collect();
        let same_identity = self.identity.as_ref() == Some(&identity);
        if !same_identity || !source.starts_with(&self.source) {
            self.search = SourceSearch::default();
            self.cursor = None;
            self.prompt = false;
            self.reveal_horizontal = None;
        } else if source.len() > self.source.len() {
            // Newly loaded discussion must not move a match or dismiss a query.
            self.search.append_lines(
                source
                    .iter()
                    .enumerate()
                    .skip(self.source.len())
                    .map(|(row, text)| (row, text.as_ref())),
            );
        }
        self.identity = Some(identity);
        self.source = source;
        let (logical, byte) = self.cursor?;
        self.fragments
            .iter()
            .enumerate()
            .find_map(|(row, fragment)| {
                fragment
                    .filter(|(source, offset)| {
                        *source == logical
                            && *offset <= byte
                            && byte < *offset + rows[row].text.len()
                    })
                    .map(|_| row)
            })
    }

    fn source_row(&self, row: usize) -> Option<usize> {
        self.fragments
            .get(row)
            .copied()
            .flatten()
            .map(|(row, _)| row)
    }

    pub fn cursor_byte(&self, row: usize) -> Option<usize> {
        let (logical, offset) = self.fragments.get(row).copied().flatten()?;
        let (source, byte) = self.cursor?;
        (source == logical && byte >= offset).then(|| byte - offset)
    }

    pub fn row_matches(&self, row: usize) -> (&[SourceMatch], usize) {
        self.fragments
            .get(row)
            .copied()
            .flatten()
            .map_or((&[], 0), |(logical, offset)| {
                (self.search.row_matches(logical), offset)
            })
    }

    pub fn reveal_horizontal(&self) -> Option<usize> {
        self.reveal_horizontal
    }

    pub fn set_reveal_horizontal(&mut self, horizontal: Option<usize>) {
        self.reveal_horizontal = horizontal;
    }

    pub fn leave_match(&mut self) -> Option<usize> {
        self.search.leave_match();
        self.cursor = None;
        self.reveal_horizontal.take()
    }

    pub fn prompt_active(&self) -> bool {
        self.prompt
    }

    pub fn status(&self, cursor: usize, width: u16) -> String {
        let (position, count) = self.search.position(self.source_row(cursor));
        let position = if count == 0 {
            "No detail matches".into()
        } else {
            format!(
                "{}/{count}",
                position.map_or_else(|| "-".into(), |n| n.to_string())
            )
        };
        let query = super::super::render::ellipsize(&self.search.query, usize::from(width / 3));
        format!("Find /{query}/ · {position} · Esc clear · n/N")
    }

    /// The footer's find context: the query and where the cursor is among its matches.
    /// The keys live in the footer's hints, so the position is what survives a narrow row.
    pub fn footer_status(&self, cursor: usize, width: u16) -> String {
        let (position, count) = self.search.position(self.source_row(cursor));
        let position = if count == 0 {
            "no matches".into()
        } else {
            format!("{}/{count}", position.map_or_else(|| "-".into(), |n| n.to_string()))
        };
        let query = super::super::render::ellipsize(&self.search.query, usize::from(width / 5).max(6));
        format!("/{query}/ · {position}")
    }
}
impl Workspace {
    fn detail_identity(&self) -> Identity {
        let content = if self.page < 3 {
            let file = if self.page == 2 && self.pr_tab == 3 {
                self.item()
                    .map(|item| self.selected_file(self.number(), item).to_owned())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            Content::Item(
                self.list_key(),
                self.number(),
                if self.page == 2 { self.pr_tab } else { self.issue_tab },
                file,
                if self.pr_tab == 1 {
                    self.selected_commit.clone()
                } else {
                    None
                },
            )
        } else {
            // Actions keeps its own log find (`runs.rs`); the reader find has nothing here.
            Content::Workflow(self.workflow_id())
        };
        Identity {
            host: self.host.clone(),
            repo: self.repo.clone(),
            content,
        }
    }

    pub(super) fn sync_detail_find(&mut self, rows: &[lines::Line]) {
        let identity = self.detail_identity();
        if let Some(row) = self.detail_find.sync(identity, rows) {
            self.detail_cursor = row;
        }
    }

    // Input may arrive between frames or while a refresh completes behind a form.
    // Reconcile against the actual cached source before stepping, not yesterday's rows.
    fn prepare_detail_find(&mut self) {
        let rows = lines::detail_lines(self, self.detail_width);
        self.sync_detail_find(&rows);
        self.detail_rows = rows;
    }

    pub(in crate::git::screen) fn find_detail(&mut self, query: String) -> Result<(), String> {
        self.prepare_detail_find();
        if !self.detail_find.prompt_active() {
            return Err("The detail changed while finding; press Esc and reopen find".into());
        }
        self.detail_find.prompt = false;
        self.detail_find.search = SourceSearch::in_lines(
            self.detail_find
                .source
                .iter()
                .enumerate()
                .map(|(row, text)| (row, text.as_ref())),
            query,
        );
        self.step_detail_match(false, true);
        Ok(())
    }

    fn step_detail_match(&mut self, backwards: bool, initial: bool) {
        let cursor = self.detail_find.source_row(self.detail_cursor);
        if self
            .detail_find
            .search
            .step(cursor, backwards, initial)
            .is_some()
            && let Some(found) = self.detail_find.search.current_match()
        {
            self.detail_find.cursor = Some((found.row, found.bytes.start));
            self.detail_find.reveal_horizontal = None;
            let rows = std::mem::take(&mut self.detail_rows);
            self.sync_detail_find(&rows);
            self.detail_rows = rows;
        }
    }

    pub(super) fn detail_find_key(&mut self, key: &KeyEvent) -> Option<Form> {
        if self.detail_find.search.active()
            || key.modifiers.control && key.code == KeyCode::Char('f')
        {
            self.prepare_detail_find();
        }
        if key.modifiers.control && !key.modifiers.alt && key.code == KeyCode::Char('f') {
            if self.focus < 2 {
                self.last_list = self.focus;
            }
            self.focus = 2;
            self.pending_pr = false;
            self.ready_form = None;
            self.detail_find.prompt = true;
            let mut field = Field::new(
                "Query · literal, case-sensitive",
                self.detail_find.search.query.clone(),
            );
            field.required = false;
            return Some(Form {
                title: "Find detail".into(),
                action: FormAction::SourceSearch,
                fields: vec![field],
                selected: 0,
            });
        }
        None
    }

    pub(super) fn step_or_clear_detail_find(&mut self, key: &KeyEvent) -> bool {
        if self.focus != 2
            || !self.detail_find.search.active()
            || key.modifiers.control
            || key.modifiers.alt
        {
            return false;
        }
        match key.code {
            KeyCode::Char('n' | 'N') => {
                self.step_detail_match(key.code == KeyCode::Char('N'), false)
            }
            KeyCode::Escape => self.detail_find.search = SourceSearch::default(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace() -> Workspace {
        let mut workspace = Workspace::new(PathBuf::new());
        workspace.host = "github.com".into();
        workspace.repo = "example/demo".into();
        workspace.page = 1;
        workspace.opened = Some((1, 7));
        workspace.focus = 2;
        workspace.cache.insert(
            workspace.list_key(),
            Ok(Reply {
                text: String::new(),
                value: json!([]),
            }),
        );
        workspace.cache.insert(Key::Item(1, 7), Ok(Reply {
            text: String::new(),
            value: json!({"number":7,"title":"Identity","body":"**before needle** after `needle` 界 e\u{301} 👩‍💻"}),
        }));
        workspace.prepare_detail_find();
        workspace.detail_find.prompt = true;
        workspace.find_detail("needle".into()).unwrap();
        workspace
    }

    #[test]
    fn local_reveal_is_consumed_once_and_never_crosses_source_identity_or_a_new_match() {
        let mut workspace = workspace();
        workspace.detail_find.set_reveal_horizontal(Some(81));
        assert_eq!(workspace.detail_find.leave_match(), Some(81));
        assert_eq!(workspace.detail_find.leave_match(), None);
        assert!(workspace.detail_find.cursor.is_none());
        workspace.step_detail_match(false, true);
        workspace.detail_find.set_reveal_horizontal(Some(33));
        workspace.step_detail_match(false, false);
        assert_eq!(workspace.detail_find.reveal_horizontal(), None);
        workspace.detail_find.set_reveal_horizontal(Some(99));
        workspace.host = "other.example.test".into();
        workspace.prepare_detail_find();
        assert_eq!(workspace.detail_find.reveal_horizontal(), None);
        assert!(workspace.detail_find.cursor.is_none());
        assert!(!workspace.detail_find.search.active());
    }

    #[test]
    fn unchanged_source_reflows_and_chrome_refreshes_without_losing_the_match() {
        let mut workspace = workspace();
        workspace.step_detail_match(false, false);
        let selected = workspace.detail_find.cursor;
        assert_eq!(workspace.detail_find.search.position(Some(0)), (Some(2), 2));
        for width in [35, 10, 100] {
            workspace.detail_width = width;
            workspace.prepare_detail_find();
            assert_eq!(workspace.detail_find.cursor, selected);
            let row = &workspace.detail_rows[workspace.detail_cursor];
            let byte = workspace
                .detail_find
                .cursor_byte(workspace.detail_cursor)
                .unwrap();
            assert!(row.text[byte..].starts_with('n'));
        }
        workspace
            .cache
            .get_mut(&Key::Item(1, 7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value["title"] = json!("Changed chrome");
        workspace.prepare_detail_find();
        assert_eq!(workspace.detail_find.cursor, selected);
        assert_eq!(workspace.detail_find.search.query, "needle");
        assert!(
            workspace.queue.is_empty(),
            "find must not schedule reads or writes"
        );
    }

    #[test]
    fn markup_only_refresh_keeps_visible_prose_matches_and_does_not_modify_raw_body_or_drafts() {
        let mut workspace = workspace();
        workspace.step_detail_match(false, false);
        let selected = workspace.detail_find.cursor;
        let source = workspace.detail_find.source.clone();
        let raw = "before **needle** after **needle** 界 e\u{301} 👩‍💻";
        workspace.save_draft(&Action::Comment(1, 7), vec!["**raw draft** `code`".into()]);
        let drafts = workspace.drafts.clone();
        workspace
            .cache
            .get_mut(&Key::Item(1, 7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value["body"] = json!(raw);
        for width in [10, 100, 35] {
            workspace.detail_width = width;
            workspace.prepare_detail_find();
            assert_eq!(workspace.detail_find.source, source);
            assert_eq!(workspace.detail_find.cursor, selected);
            assert_eq!(workspace.detail_find.search.query, "needle");
            let row = &workspace.detail_rows[workspace.detail_cursor];
            let byte = workspace
                .detail_find
                .cursor_byte(workspace.detail_cursor)
                .unwrap();
            assert!(row.text[byte..].starts_with('n'));
        }
        assert_eq!(workspace.item().unwrap()["body"], json!(raw));
        assert_eq!(workspace.drafts, drafts);
        assert!(workspace.queue.is_empty());
    }

    #[test]
    fn list_description_find_keeps_its_logical_cursor_when_only_metadata_arrives() {
        for page in [1, 2] {
            let mut workspace = workspace();
            workspace.page = page;
            workspace.opened = Some((page, 7));
            let loaded = workspace.cache.remove(&Key::Item(1, 7)).unwrap();
            let value = loaded.as_ref().unwrap().value.clone();
            workspace.cache.insert(
                workspace.list_key(),
                Ok(Reply {
                    text: String::new(),
                    value: json!([value]),
                }),
            );
            workspace.prepare_detail_find();
            workspace.detail_find.prompt = true;
            workspace.find_detail("needle".into()).unwrap();
            workspace.step_detail_match(false, false);
            let selected = workspace.detail_find.cursor;
            assert_eq!(workspace.detail_find.search.position(Some(0)), (Some(2), 2));
            workspace.detail_find.prompt = true;
            workspace.cache.insert(Key::Item(page, 7), loaded);
            workspace.prepare_detail_find();
            assert_eq!(workspace.detail_find.cursor, selected);
            assert_eq!(workspace.detail_find.search.query, "needle");
            assert!(
                workspace.detail_find.prompt_active(),
                "metadata must not cancel an open find form"
            );
            let row = &workspace.detail_rows[workspace.detail_cursor];
            let byte = workspace
                .detail_find
                .cursor_byte(workspace.detail_cursor)
                .unwrap();
            assert!(row.text[byte..].starts_with("needle"));
            assert!(workspace.queue.is_empty());
        }
    }

    #[test]
    fn another_query_or_state_is_a_new_find_identity_even_with_identical_prose() {
        for state in [false, true] {
            let mut workspace = workspace();
            if state {
                workspace.states[0] = "all".into();
            } else {
                workspace.queries[0] = "author:@me".into();
            }
            workspace.prepare_detail_find();
            assert!(!workspace.detail_find.search.active());
            assert!(workspace.detail_find.cursor.is_none());
        }
    }

    #[test]
    fn appended_source_preserves_query_cursor_and_prompt_and_adds_only_new_matches() {
        let mut workspace = workspace();
        workspace.step_detail_match(false, false);
        let selected = workspace.detail_find.cursor;
        workspace.detail_find.prompt = true;
        let body = string(workspace.item().unwrap(), "body").to_owned();
        workspace
            .cache
            .get_mut(&Key::Item(1, 7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value["body"] = json!(format!("{body}\n\nAn appended needle."));
        workspace.prepare_detail_find();
        assert_eq!(workspace.detail_find.cursor, selected);
        assert_eq!(workspace.detail_find.search.query, "needle");
        assert_eq!(workspace.detail_find.search.position(Some(0)), (Some(2), 3));
        assert!(workspace.detail_find.prompt_active());
        workspace.prepare_detail_find();
        assert_eq!(workspace.detail_find.search.position(Some(0)), (Some(2), 3));
        workspace.detail_find.prompt = false;
        workspace.step_detail_match(false, false);
        assert!(
            workspace.detail_rows[workspace.detail_cursor]
                .text
                .contains("appended needle")
        );
        assert!(workspace.queue.is_empty());
    }

    #[test]
    fn replaced_source_invalidates_matches_cursor_and_an_open_prompt_before_stepping() {
        let mut workspace = workspace();
        workspace.detail_find.prompt = true;
        workspace
            .cache
            .get_mut(&Key::Item(1, 7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value["body"] = json!("Entirely unrelated content");
        workspace.prepare_detail_find();
        assert!(!workspace.detail_find.search.active());
        assert!(workspace.detail_find.cursor.is_none());
        assert!(!workspace.detail_find.prompt_active());
        assert!(workspace.find_detail("needle".into()).is_err());
        assert!(
            !workspace.detail_find.search.active(),
            "stale prompt cannot apply to replacement text"
        );
        assert!(workspace.queue.is_empty());
    }

    #[test]
    fn identical_text_in_another_host_or_repository_is_not_the_same_find_context() {
        for host in [true, false] {
            let mut workspace = workspace();
            if host {
                workspace.host = "git.example.test".into();
            } else {
                workspace.repo = "other/demo".into();
            }
            workspace.prepare_detail_find();
            assert!(!workspace.detail_find.search.active());
            assert!(workspace.detail_find.cursor.is_none());
            assert!(!workspace.detail_find.prompt_active());
        }
    }
}
