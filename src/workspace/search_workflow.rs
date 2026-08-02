use super::{PickerAction, PickerItem};

pub const MAX_QUERY_BYTES: usize = 4096;
pub const MAX_ITEM_FIELD_BYTES: usize = 16 * 1024;
pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;
pub const ABS_MAX_ITEMS: usize = 100_000;
pub const ABS_MAX_BYTES: usize = 16 * 1024 * 1024;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SearchWorkflowId(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SearchRequestId(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SearchStreamGeneration(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SearchPreviewRevision(pub u64);
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchSourceKind {
    Files,
    Grep,
    Commands,
    PluginProvider { provider: String },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMatcherKind {
    Fuzzy,
    Literal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchPresenterKind {
    Picker,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchHostAction {
    OpenLocation { uri: String },
    InvokeBundledCommand { command: String },
    InvokeDeclaredProviderCommand,
    ApplyCodeAction { index: usize },
    None,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchWorkflowSpec {
    pub id: SearchWorkflowId,
    pub source: SearchSourceKind,
    pub matcher: SearchMatcherKind,
    pub presenter: SearchPresenterKind,
    pub maximum_items: usize,
    pub maximum_bytes: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchWorkflowItem {
    pub stable_key: String,
    pub title: String,
    pub detail: String,
    pub search_text: String,
    pub action: SearchHostAction,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchWorkflowChunk {
    pub request: SearchRequestId,
    pub generation: SearchStreamGeneration,
    pub sequence: u64,
    pub items: Vec<SearchWorkflowItem>,
    pub complete: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchWorkflowSnapshot {
    pub revision: u64,
    pub request: SearchRequestId,
    pub items: Vec<SearchWorkflowItem>,
    pub complete: bool,
}
#[derive(Clone, Debug)]
struct Active {
    request: SearchRequestId,
    generation: SearchStreamGeneration,
    next_sequence: u64,
    query: String,
    items: BTreeMap<String, SearchWorkflowItem>,
    bytes: usize,
    complete: bool,
    preview_request: Option<(String, SearchPreviewRevision)>,
    preview: Option<(String, SearchPreviewRevision, String)>,
}
#[derive(Clone, Debug)]
pub struct SearchWorkflowBroker {
    spec: SearchWorkflowSpec,
    active: Option<Active>,
    revision: u64,
}
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SearchWorkflowError {
    #[error("invalid workflow spec")]
    InvalidSpec,
    #[error("stale request")]
    Stale,
    #[error("invalid sequence")]
    Sequence,
    #[error("duplicate item key")]
    Duplicate,
    #[error("workflow limit exceeded")]
    Limit,
    #[error("stream complete")]
    Complete,
    #[error("invalid preview")]
    Preview,
}
impl SearchWorkflowBroker {
    pub fn try_new(spec: SearchWorkflowSpec) -> Result<Self, SearchWorkflowError> {
        if spec.maximum_items == 0
            || spec.maximum_items > ABS_MAX_ITEMS
            || spec.maximum_bytes == 0
            || spec.maximum_bytes > ABS_MAX_BYTES
        {
            return Err(SearchWorkflowError::InvalidSpec);
        }
        Ok(Self {
            spec,
            active: None,
            revision: 0,
        })
    }
    pub fn begin(
        &mut self,
        request: SearchRequestId,
        generation: SearchStreamGeneration,
        query: impl Into<String>,
    ) -> Result<(), SearchWorkflowError> {
        let query = query.into();
        if query.len() > MAX_QUERY_BYTES {
            return Err(SearchWorkflowError::Limit);
        }
        self.active = Some(Active {
            request,
            generation,
            next_sequence: 0,
            query,
            items: BTreeMap::new(),
            bytes: 0,
            complete: false,
            preview_request: None,
            preview: None,
        });
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn cancel(&mut self) {
        self.active = None;
        self.revision = self.revision.saturating_add(1)
    }
    pub fn accept(&mut self, chunk: SearchWorkflowChunk) -> Result<(), SearchWorkflowError> {
        let mut next = self.active.clone().ok_or(SearchWorkflowError::Stale)?;
        if next.request != chunk.request || next.generation != chunk.generation {
            return Err(SearchWorkflowError::Stale);
        }
        if next.complete {
            return Err(SearchWorkflowError::Complete);
        }
        if chunk.sequence != next.next_sequence {
            return Err(SearchWorkflowError::Sequence);
        }
        let mut keys = BTreeSet::new();
        for item in &chunk.items {
            if item.stable_key.len() > MAX_ITEM_FIELD_BYTES
                || item.title.len() > MAX_ITEM_FIELD_BYTES
                || item.detail.len() > MAX_ITEM_FIELD_BYTES
                || item.search_text.len() > MAX_ITEM_FIELD_BYTES
            {
                return Err(SearchWorkflowError::Limit);
            }
            if item.stable_key.is_empty()
                || !keys.insert(&item.stable_key)
                || next.items.contains_key(&item.stable_key)
            {
                return Err(SearchWorkflowError::Duplicate);
            }
            next.bytes = next
                .bytes
                .checked_add(
                    item.stable_key.len()
                        + item.title.len()
                        + item.detail.len()
                        + item.search_text.len(),
                )
                .ok_or(SearchWorkflowError::Limit)?;
            if next.bytes > self.spec.maximum_bytes
                || next.items.len() + keys.len() > self.spec.maximum_items
            {
                return Err(SearchWorkflowError::Limit);
            }
        }
        for item in chunk.items {
            next.items.insert(item.stable_key.clone(), item);
        }
        next.next_sequence += 1;
        next.complete = chunk.complete;
        self.active = Some(next);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
    pub fn request_preview(
        &mut self,
        key: &str,
        revision: SearchPreviewRevision,
    ) -> Result<(), SearchWorkflowError> {
        let next = self.active.as_mut().ok_or(SearchWorkflowError::Preview)?;
        if !next.items.contains_key(key)
            || next
                .preview_request
                .as_ref()
                .is_some_and(|(_, r)| *r >= revision)
        {
            return Err(SearchWorkflowError::Preview);
        }
        next.preview_request = Some((key.into(), revision));
        next.preview = None;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
    pub fn accept_preview(
        &mut self,
        key: &str,
        revision: SearchPreviewRevision,
        text: String,
    ) -> Result<(), SearchWorkflowError> {
        if text.len() > MAX_PREVIEW_BYTES {
            return Err(SearchWorkflowError::Limit);
        }
        let next = self.active.as_mut().ok_or(SearchWorkflowError::Preview)?;
        if next.preview_request.as_ref() != Some(&(key.into(), revision)) {
            return Err(SearchWorkflowError::Preview);
        }
        next.preview = Some((key.into(), revision, text));
        next.preview_request = None;
        self.revision += 1;
        Ok(())
    }
    pub fn snapshot_if_changed(&self, last: u64) -> Option<SearchWorkflowSnapshot> {
        if last == self.revision {
            return None;
        }
        let active = self.active.as_ref()?;
        let q = active.query.to_lowercase();
        let mut items = active
            .items
            .values()
            .filter(|i| q.is_empty() || i.search_text.to_lowercase().contains(&q))
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by(|a, b| {
            let ar = a.search_text.to_lowercase().find(&q).unwrap_or(usize::MAX);
            let br = b.search_text.to_lowercase().find(&q).unwrap_or(usize::MAX);
            ar.cmp(&br)
                .then_with(|| a.title.cmp(&b.title))
                .then_with(|| a.stable_key.cmp(&b.stable_key))
        });
        Some(SearchWorkflowSnapshot {
            revision: self.revision,
            request: active.request,
            items,
            complete: active.complete,
        })
    }
}
pub fn picker_item_adapter(item: &PickerItem) -> SearchWorkflowItem {
    SearchWorkflowItem {
        stable_key: item.id.0.clone(),
        title: item.label.clone(),
        detail: item.detail.clone(),
        search_text: format!("{} {}", item.label, item.detail),
        action: match &item.action {
            PickerAction::OpenLocation { uri, .. } => {
                SearchHostAction::OpenLocation { uri: uri.clone() }
            }
            PickerAction::InvokeCommand { command } => SearchHostAction::InvokeBundledCommand {
                command: command.clone(),
            },
            PickerAction::ApplyCodeAction { index } => {
                SearchHostAction::ApplyCodeAction { index: *index }
            }
            PickerAction::None => SearchHostAction::None,
        },
    }
}
pub fn plugin_item_adapter(id: &str, primary: &str, secondary: Option<&str>) -> SearchWorkflowItem {
    SearchWorkflowItem {
        stable_key: id.into(),
        title: primary.into(),
        detail: secondary.unwrap_or_default().into(),
        search_text: format!("{} {}", primary, secondary.unwrap_or_default()),
        action: SearchHostAction::InvokeDeclaredProviderCommand,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn item(k: &str, t: &str) -> SearchWorkflowItem {
        plugin_item_adapter(k, t, None)
    }
    fn broker() -> SearchWorkflowBroker {
        SearchWorkflowBroker::try_new(SearchWorkflowSpec {
            id: SearchWorkflowId(1),
            source: SearchSourceKind::PluginProvider {
                provider: "p".into(),
            },
            matcher: SearchMatcherKind::Fuzzy,
            presenter: SearchPresenterKind::Picker,
            maximum_items: 2,
            maximum_bytes: 64,
        })
        .unwrap()
    }
    #[test]
    fn bounded_streams_are_exact_and_idle() {
        let mut b = broker();
        b.begin(SearchRequestId(1), SearchStreamGeneration(2), "x")
            .unwrap();
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(9),
                generation: SearchStreamGeneration(2),
                sequence: 0,
                items: vec![],
                complete: false
            }),
            Err(SearchWorkflowError::Stale)
        );
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                generation: SearchStreamGeneration(2),
                sequence: 1,
                items: vec![],
                complete: false
            }),
            Err(SearchWorkflowError::Sequence)
        );
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                generation: SearchStreamGeneration(2),
                sequence: 0,
                items: vec![item("a", "xa"), item("a", "xb")],
                complete: false
            }),
            Err(SearchWorkflowError::Duplicate)
        );
        b.accept(SearchWorkflowChunk {
            request: SearchRequestId(1),
            generation: SearchStreamGeneration(2),
            sequence: 0,
            items: vec![item("b", "xb"), item("a", "xa")],
            complete: true,
        })
        .unwrap();
        let s = b.snapshot_if_changed(0).unwrap();
        assert_eq!(s.items[0].stable_key, "a");
        for _ in 0..256 {
            assert!(b.snapshot_if_changed(s.revision).is_none())
        }
        assert_eq!(
            b.accept_preview("missing", SearchPreviewRevision(1), "x".into()),
            Err(SearchWorkflowError::Preview)
        );
        b.request_preview("a", SearchPreviewRevision(1)).unwrap();
        b.accept_preview("a", SearchPreviewRevision(1), "x".into())
            .unwrap();
        assert_eq!(
            b.accept_preview("a", SearchPreviewRevision(1), "y".into()),
            Err(SearchWorkflowError::Preview)
        );
        b.cancel();
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                generation: SearchStreamGeneration(2),
                sequence: 1,
                items: vec![],
                complete: true
            }),
            Err(SearchWorkflowError::Stale)
        );
    }
    #[test]
    fn specifications_queries_fields_and_previews_are_bounded_atomically() {
        let spec = SearchWorkflowSpec {
            id: SearchWorkflowId(1),
            source: SearchSourceKind::Commands,
            matcher: SearchMatcherKind::Literal,
            presenter: SearchPresenterKind::Picker,
            maximum_items: 0,
            maximum_bytes: 1,
        };
        assert_eq!(
            SearchWorkflowBroker::try_new(spec).unwrap_err(),
            SearchWorkflowError::InvalidSpec
        );
        let mut b = broker();
        assert_eq!(
            b.begin(
                SearchRequestId(1),
                SearchStreamGeneration(1),
                "x".repeat(MAX_QUERY_BYTES + 1)
            ),
            Err(SearchWorkflowError::Limit)
        );
        b.begin(SearchRequestId(1), SearchStreamGeneration(1), "")
            .unwrap();
        let revision = b.revision;
        let huge = SearchWorkflowItem {
            stable_key: "k".into(),
            title: "x".repeat(MAX_ITEM_FIELD_BYTES + 1),
            detail: String::new(),
            search_text: String::new(),
            action: SearchHostAction::None,
        };
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                generation: SearchStreamGeneration(1),
                sequence: 0,
                items: vec![huge],
                complete: false
            }),
            Err(SearchWorkflowError::Limit)
        );
        assert_eq!(b.revision, revision);
    }

    #[test]
    fn adapters_preserve_bounded_authority() {
        let p = plugin_item_adapter("a", "A", Some("B"));
        assert_eq!(p.action, SearchHostAction::InvokeDeclaredProviderCommand);
        let picker = PickerItem {
            id: super::super::PickerItemId("c".into()),
            label: "C".into(),
            detail: "D".into(),
            source: "commands".into(),
            action: PickerAction::InvokeCommand {
                command: "builtin.open".into(),
            },
        };
        assert_eq!(
            picker_item_adapter(&picker).action,
            SearchHostAction::InvokeBundledCommand {
                command: "builtin.open".into()
            }
        );
    }
}
