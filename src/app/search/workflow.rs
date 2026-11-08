//! Search requests, result streams and selected actions.

use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::fuzzy_score;

pub(in crate::app) const MAX_QUERY_BYTES: usize = 4096;
pub(in crate::app) const MAX_ITEM_FIELD_BYTES: usize = 16 * 1024;
pub(in crate::app) const MAX_PREVIEW_BYTES: usize = 256 * 1024;
pub(in crate::app) const ABS_MAX_ITEMS: usize = 100_000;
pub(in crate::app) const ABS_MAX_BYTES: usize = 16 * 1024 * 1024;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::app) struct SearchWorkflowId(pub(in crate::app) u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::app) struct SearchRequestId(pub(in crate::app) u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::app) struct SearchStreamId(pub(in crate::app) u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::app) struct SearchPreviewRevision(pub(in crate::app) u64);
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) enum SearchSourceKind {
    #[expect(
        dead_code,
        reason = "File enumeration currently uses the file picker rather than a workflow stream."
    )]
    Files,
    Grep,
    Commands,
    PluginProvider {
        provider: String,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum SearchMatcherKind {
    Fuzzy,
    Literal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum SearchPresenterKind {
    Picker,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) enum SearchHostAction {
    OpenLocation { uri: String },
    InvokeBundledCommand { command: String },
    InvokeDeclaredProviderCommand,
    ApplyCodeAction { index: usize },
    None,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct SearchWorkflowSpec {
    pub(in crate::app) id: SearchWorkflowId,
    pub(in crate::app) source: SearchSourceKind,
    pub(in crate::app) matcher: SearchMatcherKind,
    pub(in crate::app) presenter: SearchPresenterKind,
    pub(in crate::app) maximum_items: usize,
    pub(in crate::app) maximum_bytes: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct SearchWorkflowItem {
    pub(in crate::app) stable_key: String,
    pub(in crate::app) title: String,
    pub(in crate::app) detail: String,
    pub(in crate::app) search_text: String,
    pub(in crate::app) action: SearchHostAction,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct SearchWorkflowChunk {
    pub(in crate::app) request: SearchRequestId,
    pub(in crate::app) stream_id: SearchStreamId,
    pub(in crate::app) sequence: u64,
    pub(in crate::app) items: Vec<SearchWorkflowItem>,
    pub(in crate::app) complete: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct SearchWorkflowSnapshot {
    pub(in crate::app) revision: u64,
    pub(in crate::app) request: SearchRequestId,
    pub(in crate::app) items: Vec<SearchWorkflowItem>,
    pub(in crate::app) complete: bool,
}
#[derive(Clone, Debug)]
struct Active {
    request: SearchRequestId,
    stream_id: SearchStreamId,
    next_sequence: u64,
    query: String,
    items: BTreeMap<String, SearchWorkflowItem>,
    order: Vec<String>,
    bytes: usize,
    complete: bool,
    preview_request: Option<(String, SearchPreviewRevision)>,
    preview: Option<(String, SearchPreviewRevision, String)>,
}
#[derive(Clone, Debug)]
pub(in crate::app) struct SearchWorkflowBroker {
    spec: SearchWorkflowSpec,
    active: Option<Active>,
    revision: u64,
}
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(in crate::app) enum SearchWorkflowError {
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
    pub(in crate::app) fn try_new(spec: SearchWorkflowSpec) -> Result<Self, SearchWorkflowError> {
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
    pub(in crate::app) fn begin(
        &mut self,
        request: SearchRequestId,
        stream_id: SearchStreamId,
        query: impl Into<String>,
    ) -> Result<(), SearchWorkflowError> {
        let query = query.into();
        if query.len() > MAX_QUERY_BYTES {
            return Err(SearchWorkflowError::Limit);
        }
        self.active = Some(Active {
            request,
            stream_id,
            next_sequence: 0,
            query,
            items: BTreeMap::new(),
            order: Vec::new(),
            bytes: 0,
            complete: false,
            preview_request: None,
            preview: None,
        });
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
    #[expect(
        dead_code,
        reason = "Retain the workflow revision query alongside its snapshot contract."
    )]
    pub(in crate::app) fn revision(&self) -> u64 {
        self.revision
    }
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "The picker owns current preview requests; retain the tested workflow contract."
        )
    )]
    pub(in crate::app) fn cancel(&mut self) {
        self.active = None;
        self.revision = self.revision.saturating_add(1)
    }
    pub(in crate::app) fn accept(
        &mut self,
        chunk: SearchWorkflowChunk,
    ) -> Result<(), SearchWorkflowError> {
        let next = self.active.as_ref().ok_or(SearchWorkflowError::Stale)?;
        let mut bytes = next.bytes;
        let sequence = next
            .next_sequence
            .checked_add(1)
            .ok_or(SearchWorkflowError::Limit)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(SearchWorkflowError::Limit)?;
        if next.request != chunk.request || next.stream_id != chunk.stream_id {
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
            bytes = bytes
                .checked_add(
                    item.stable_key.len()
                        + item.title.len()
                        + item.detail.len()
                        + item.search_text.len(),
                )
                .ok_or(SearchWorkflowError::Limit)?;
            if bytes > self.spec.maximum_bytes
                || next.items.len() + keys.len() > self.spec.maximum_items
            {
                return Err(SearchWorkflowError::Limit);
            }
        }
        let next = self.active.as_mut().expect("validated active stream");
        for item in chunk.items {
            next.order.push(item.stable_key.clone());
            next.items.insert(item.stable_key.clone(), item);
        }
        next.next_sequence = sequence;
        next.bytes = bytes;
        next.complete = chunk.complete;
        self.revision = revision;
        Ok(())
    }
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "The picker owns current preview requests; retain the tested workflow contract."
        )
    )]
    pub(in crate::app) fn request_preview(
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "The picker owns current preview requests; retain the tested workflow contract."
        )
    )]
    pub(in crate::app) fn accept_preview(
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
    pub(in crate::app) fn ordered_items(&self) -> Option<Vec<&SearchWorkflowItem>> {
        let active = self.active.as_ref()?;
        let q = active.query.to_lowercase();
        let mut items = active
            .order
            .iter()
            .filter_map(|key| active.items.get(key))
            .filter_map(|item| {
                let score = match self.spec.matcher {
                    SearchMatcherKind::Fuzzy => fuzzy_score(&q, &item.search_text, "")?,
                    SearchMatcherKind::Literal => {
                        -i64::try_from(item.search_text.to_lowercase().find(&q)?).ok()?
                    }
                };
                Some((item, score))
            })
            .collect::<Vec<_>>();
        if q.is_empty() {
            return Some(items.into_iter().map(|(item, _)| item).collect());
        }
        items.sort_by(|(a, a_score), (b, b_score)| {
            b_score
                .cmp(a_score)
                .then_with(|| a.title.cmp(&b.title))
                .then_with(|| a.stable_key.cmp(&b.stable_key))
        });
        Some(items.into_iter().map(|(item, _)| item).collect())
    }
    pub(in crate::app) fn snapshot_if_changed(&self, last: u64) -> Option<SearchWorkflowSnapshot> {
        if last == self.revision {
            return None;
        }
        let active = self.active.as_ref()?;
        let items = self.ordered_items()?.into_iter().cloned().collect();
        Some(SearchWorkflowSnapshot {
            revision: self.revision,
            request: active.request,
            items,
            complete: active.complete,
        })
    }
}
pub(in crate::app) fn picker_item_adapter(item: &PickerItem) -> SearchWorkflowItem {
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
pub(in crate::app) fn plugin_item_adapter(
    id: &str,
    primary: &str,
    secondary: Option<&str>,
) -> SearchWorkflowItem {
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
    use crate::app::search::workflow::*;
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
    fn empty_query_preserves_producer_order_in_every_projection() {
        let mut broker = broker();
        broker
            .begin(SearchRequestId(1), SearchStreamId(1), "")
            .unwrap();
        broker
            .accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                stream_id: SearchStreamId(1),
                sequence: 0,
                items: vec![item("20", "recent"), item("100", "earlier")],
                complete: true,
            })
            .unwrap();
        let ordered = broker
            .ordered_items()
            .unwrap()
            .iter()
            .map(|item| item.stable_key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ordered, vec!["20", "100"]);
        let snapshot = broker.snapshot_if_changed(0).unwrap();
        assert_eq!(
            snapshot
                .items
                .iter()
                .map(|item| item.stable_key.as_str())
                .collect::<Vec<_>>(),
            ordered
        );
    }

    #[test]
    fn workflow_matchers_filter_and_rank_search_text() {
        for (matcher, expected) in [
            (SearchMatcherKind::Fuzzy, vec!["literal", "subsequence"]),
            (SearchMatcherKind::Literal, vec!["literal"]),
        ] {
            let mut broker = broker();
            broker.spec.matcher = matcher;
            broker
                .begin(SearchRequestId(1), SearchStreamId(1), "AC")
                .unwrap();
            let mut subsequence = item("subsequence", "A");
            subsequence.search_text = "abc".into();
            let mut literal = item("literal", "Z");
            literal.search_text = "ac".into();
            broker
                .accept(SearchWorkflowChunk {
                    request: SearchRequestId(1),
                    stream_id: SearchStreamId(1),
                    sequence: 0,
                    items: vec![subsequence, literal],
                    complete: true,
                })
                .unwrap();
            let snapshot = broker.snapshot_if_changed(0).unwrap();
            assert_eq!(
                snapshot
                    .items
                    .iter()
                    .map(|item| item.stable_key.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn literal_workflow_ranks_by_substring_position() {
        let mut broker = broker();
        broker.spec.matcher = SearchMatcherKind::Literal;
        broker
            .begin(SearchRequestId(1), SearchStreamId(1), "ac")
            .unwrap();
        broker
            .accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                stream_id: SearchStreamId(1),
                sequence: 0,
                items: vec![item("late", "A ac"), item("early", "ac Z")],
                complete: true,
            })
            .unwrap();
        assert_eq!(broker.ordered_items().unwrap()[0].stable_key, "early");
    }

    #[test]
    fn bounded_streams_are_exact_and_idle() {
        let mut b = broker();
        b.begin(SearchRequestId(1), SearchStreamId(2), "x").unwrap();
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(9),
                stream_id: SearchStreamId(2),
                sequence: 0,
                items: vec![],
                complete: false
            }),
            Err(SearchWorkflowError::Stale)
        );
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                stream_id: SearchStreamId(2),
                sequence: 1,
                items: vec![],
                complete: false
            }),
            Err(SearchWorkflowError::Sequence)
        );
        assert_eq!(
            b.accept(SearchWorkflowChunk {
                request: SearchRequestId(1),
                stream_id: SearchStreamId(2),
                sequence: 0,
                items: vec![item("a", "xa"), item("a", "xb")],
                complete: false
            }),
            Err(SearchWorkflowError::Duplicate)
        );
        b.accept(SearchWorkflowChunk {
            request: SearchRequestId(1),
            stream_id: SearchStreamId(2),
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
                stream_id: SearchStreamId(2),
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
                SearchStreamId(1),
                "x".repeat(MAX_QUERY_BYTES + 1)
            ),
            Err(SearchWorkflowError::Limit)
        );
        b.begin(SearchRequestId(1), SearchStreamId(1), "").unwrap();
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
                stream_id: SearchStreamId(1),
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
            badge: String::new(),
            kind: crate::workspace::PickerItemKind::Entry,
            id: crate::workspace::PickerItemId("c".into()),
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
