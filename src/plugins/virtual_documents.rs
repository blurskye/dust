//! Plugin document requests and validated text proposals.

#![forbid(unsafe_code)]

use crate::plugins::PluginContributionIndex;

use crate::plugins::PluginLoadId;

use crate::plugins::PluginVirtualDocumentContribution;
use std::collections::BTreeMap;
use std::time::Duration;
use std::time::Instant;

pub const VIRTUAL_DOCUMENT_REQUEST_QUEUE_LIMIT: usize = 64;
pub const VIRTUAL_DOCUMENT_KEY_BYTE_LIMIT: usize = 512;
pub const VIRTUAL_DOCUMENT_TITLE_BYTE_LIMIT: usize = 256;
pub const VIRTUAL_DOCUMENT_LANGUAGE_BYTE_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginVirtualDocumentErrorCode {
    QueueFull,
    UnknownSource,
    UnknownRequest,
    UnknownDocument,
    StaleLoad,
    StaleRevision,
    Cancelled,
    Expired,
    InvalidTitle,
    InvalidLanguage,
    InvalidUtf8Range,
    InvalidControl,
    ByteLimit,
    LineLimit,
    PatchLimit,
    ReadOnly,
    Malformed,
    Internal,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginVirtualDocumentError {
    pub code: PluginVirtualDocumentErrorCode,
    pub detail: String,
}
impl PluginVirtualDocumentError {
    pub fn new(code: PluginVirtualDocumentErrorCode, detail: impl std::fmt::Display) -> Self {
        Self {
            code,
            detail: detail
                .to_string()
                .chars()
                .filter(|c| !c.is_control())
                .take(1024)
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginVirtualDocumentRequestKind {
    Open,
    Refresh,
    CloseNotice,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginVirtualDocumentRequest {
    pub request: u64,
    pub document: u64,
    pub source: String,
    pub key: String,
    pub kind: PluginVirtualDocumentRequestKind,
    pub current_revision: Option<u64>,
    pub deadline_milliseconds: u32,
    pub cancellation_id: u64,
    pub maximum_document_bytes: u32,
    pub maximum_document_lines: u32,
    pub maximum_patch_bytes: u32,
    pub maximum_patches: u16,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginVirtualDocumentFull {
    pub expected_revision: Option<u64>,
    pub revision: u64,
    pub title: String,
    pub language: String,
    pub text: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginVirtualDocumentPatch {
    pub start: u32,
    pub end: u32,
    pub replacement: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginVirtualDocumentPatchSet {
    pub expected_revision: u64,
    pub revision: u64,
    pub patches: Vec<PluginVirtualDocumentPatch>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginVirtualDocumentProposalKind {
    Full(PluginVirtualDocumentFull),
    Patches(PluginVirtualDocumentPatchSet),
    CloseAcknowledged,
    Error(PluginVirtualDocumentError),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginVirtualDocumentProposal {
    pub request: u64,
    pub document: u64,
    pub cancellation_id: u64,
    pub kind: PluginVirtualDocumentProposalKind,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginVirtualDocumentSnapshot {
    pub plugin: String,
    pub load: PluginLoadId,
    pub source: String,
    pub document: u64,
    pub key: String,
    pub title: String,
    pub language: String,
    pub text: String,
    pub revision: u64,
    pub closed: bool,
}

#[derive(Clone, Debug)]
struct Owner {
    declaration: PluginVirtualDocumentContribution,
}
#[derive(Clone, Debug)]
struct Document {
    plugin: String,
    load: PluginLoadId,
    source: String,
    key: String,
    title: String,
    language: String,
    text: String,
    revision: u64,
    cancellation_id: u64,
    pending_request: Option<u64>,
    closed: bool,
}
#[derive(Clone, Debug)]
struct Pending {
    plugin: String,
    load: PluginLoadId,
    source: String,
    document: u64,
    kind: PluginVirtualDocumentRequestKind,
    deadline: Instant,
    deadline_milliseconds: u32,
    cancellation_id: u64,
    delivered: bool,
}
#[derive(Clone, Debug, Default)]
pub struct PluginVirtualDocumentBroker {
    owners: BTreeMap<(String, PluginLoadId, String), Owner>,
    documents: BTreeMap<u64, Document>,
    pending: BTreeMap<u64, Pending>,
    next_request: u64,
    next_document: u64,
    next_cancellation: u64,
}

/// Restricted delivery loan: cannot install sources or change document contents.
pub(crate) struct PluginVirtualRequestLane<'a> {
    broker: &'a mut PluginVirtualDocumentBroker,
}
impl PluginVirtualRequestLane<'_> {
    pub(crate) fn deliver(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        command: &str,
    ) -> Vec<PluginVirtualDocumentRequest> {
        self.broker.requests_for_command(plugin, load, command)
    }
}

impl PluginVirtualDocumentBroker {
    pub(crate) fn request_lane(&mut self) -> PluginVirtualRequestLane<'_> {
        PluginVirtualRequestLane { broker: self }
    }
    pub fn install(
        &mut self,
        index: &PluginContributionIndex,
    ) -> Result<(), PluginVirtualDocumentError> {
        for declaration in &index.virtual_documents {
            let key = (
                index.owner.clone(),
                PluginLoadId(index.plugin_load),
                declaration.id.clone(),
            );
            if self
                .owners
                .insert(
                    key,
                    Owner {
                        declaration: declaration.clone(),
                    },
                )
                .is_some()
            {
                return Err(err(
                    PluginVirtualDocumentErrorCode::UnknownSource,
                    "virtual document load already installed",
                ));
            }
        }
        Ok(())
    }
    pub fn is_opening(&self, plugin: &str, load: PluginLoadId, source: &str, key: &str) -> bool {
        let now = crate::clock::monotonic();
        self.documents.values().any(|document| {
            document.plugin == plugin
                && document.load == load
                && document.source == source
                && document.key == key
                && document.revision == 0
                && !document.closed
                && document
                    .pending_request
                    .and_then(|request| self.pending.get(&request))
                    .is_some_and(|pending| pending.deadline >= now)
        })
    }
    pub fn start(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        source: &str,
        key: String,
    ) -> Result<u64, PluginVirtualDocumentError> {
        validate_text(&key, VIRTUAL_DOCUMENT_KEY_BYTE_LIMIT, true)?;
        let owner = self
            .owners
            .get(&(plugin.into(), load, source.into()))
            .ok_or_else(|| {
                err(
                    PluginVirtualDocumentErrorCode::UnknownSource,
                    "source is not owned by this plugin load",
                )
            })?
            .clone();
        let now = crate::clock::monotonic();
        self.expire_requests(now);
        if let Some((document, _)) = self.documents.iter().find(|(_, document)| {
            document.plugin == plugin
                && document.load == load
                && document.source == source
                && document.key == key
                && !document.closed
        }) {
            return Ok(*document);
        }
        let active = self
            .documents
            .values()
            .filter(|d| d.plugin == plugin && d.load == load && d.source == source && !d.closed)
            .count();
        if active >= usize::from(owner.declaration.maximum_concurrent_documents) {
            return Err(err(
                PluginVirtualDocumentErrorCode::QueueFull,
                "concurrent document limit reached",
            ));
        }
        let document = self.alloc_document()?;
        let cancellation = self.alloc_cancellation()?;
        self.documents.insert(
            document,
            Document {
                plugin: plugin.into(),
                load,
                source: source.into(),
                key,
                title: String::new(),
                language: String::new(),
                text: String::new(),
                revision: 0,
                cancellation_id: cancellation,
                pending_request: None,
                closed: false,
            },
        );
        if let Err(error) = self.create_request(
            document,
            PluginVirtualDocumentRequestKind::Open,
            now,
            owner.declaration.deadline_milliseconds,
        ) {
            self.documents.remove(&document);
            return Err(error);
        }
        Ok(document)
    }
    fn expire_requests(&mut self, now: Instant) {
        let expired = self
            .pending
            .iter()
            .filter_map(|(id, pending)| (pending.deadline < now).then_some((*id, pending.document)))
            .collect::<Vec<_>>();
        for (request, document) in expired {
            self.pending.remove(&request);
            if self
                .documents
                .get(&document)
                .is_some_and(|doc| doc.revision == 0 || doc.closed)
            {
                self.documents.remove(&document);
            } else if let Some(doc) = self.documents.get_mut(&document) {
                doc.pending_request = None;
            }
        }
    }
    pub fn refresh(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        document: u64,
    ) -> Result<(), PluginVirtualDocumentError> {
        self.new_request(
            plugin,
            load,
            document,
            PluginVirtualDocumentRequestKind::Refresh,
        )
    }
    pub fn close(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        document: u64,
    ) -> Result<(), PluginVirtualDocumentError> {
        self.new_request(
            plugin,
            load,
            document,
            PluginVirtualDocumentRequestKind::CloseNotice,
        )?;
        if let Some(doc) = self.documents.get_mut(&document) {
            doc.closed = true;
        }
        Ok(())
    }
    fn new_request(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        document: u64,
        kind: PluginVirtualDocumentRequestKind,
    ) -> Result<(), PluginVirtualDocumentError> {
        self.expire_requests(crate::clock::monotonic());
        let doc = self.documents.get(&document).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::UnknownDocument,
                "unknown document",
            )
        })?;
        if doc.plugin != plugin || doc.load != load {
            return Err(err(
                PluginVirtualDocumentErrorCode::StaleLoad,
                "foreign document load",
            ));
        }
        if doc.pending_request.is_some() {
            return Err(err(
                PluginVirtualDocumentErrorCode::QueueFull,
                "document already has a pending request",
            ));
        }
        let deadline = self
            .owners
            .get(&(plugin.into(), load, doc.source.clone()))
            .ok_or_else(|| {
                err(
                    PluginVirtualDocumentErrorCode::UnknownSource,
                    "source load unavailable",
                )
            })?
            .declaration
            .deadline_milliseconds;
        self.create_request(document, kind, crate::clock::monotonic(), deadline)
    }
    fn create_request(
        &mut self,
        document: u64,
        kind: PluginVirtualDocumentRequestKind,
        now: Instant,
        deadline_ms: u32,
    ) -> Result<(), PluginVirtualDocumentError> {
        if self.pending.len() >= VIRTUAL_DOCUMENT_REQUEST_QUEUE_LIMIT {
            return Err(err(
                PluginVirtualDocumentErrorCode::QueueFull,
                "request queue full",
            ));
        }
        let request = self.alloc_request()?;
        let d = self.documents.get_mut(&document).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::UnknownDocument,
                "unknown document",
            )
        })?;
        d.pending_request = Some(request);
        self.pending.insert(
            request,
            Pending {
                plugin: d.plugin.clone(),
                load: d.load,
                source: d.source.clone(),
                document,
                kind,
                deadline: now + Duration::from_millis(u64::from(deadline_ms)),
                deadline_milliseconds: deadline_ms,
                cancellation_id: d.cancellation_id,
                delivered: false,
            },
        );
        Ok(())
    }
    pub fn requests_for_command(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        command: &str,
    ) -> Vec<PluginVirtualDocumentRequest> {
        let now = crate::clock::monotonic();
        self.expire_requests(now);
        let ids = self
            .pending
            .iter()
            .filter_map(|(id, p)| {
                let declared = self
                    .owners
                    .get(&(p.plugin.clone(), p.load, p.source.clone()))
                    .map(|owner| owner.declaration.command.as_str());
                (!p.delivered
                    && p.plugin == plugin
                    && p.load == load
                    && p.deadline >= now
                    && declared == Some(command))
                .then_some(*id)
            })
            .take(VIRTUAL_DOCUMENT_REQUEST_QUEUE_LIMIT)
            .collect::<Vec<_>>();
        let mut out = Vec::new();
        for id in ids {
            if let Some(p) = self.pending.get_mut(&id) {
                p.delivered = true;
                let d = &self.documents[&p.document];
                let o = &self.owners[&(p.plugin.clone(), p.load, p.source.clone())].declaration;
                out.push(PluginVirtualDocumentRequest {
                    request: id,
                    document: p.document,
                    source: p.source.clone(),
                    key: d.key.clone(),
                    kind: p.kind,
                    current_revision: (d.revision > 0).then_some(d.revision),
                    deadline_milliseconds: p.deadline_milliseconds,
                    cancellation_id: p.cancellation_id,
                    maximum_document_bytes: o.maximum_document_bytes,
                    maximum_document_lines: o.maximum_document_lines,
                    maximum_patch_bytes: o.maximum_patch_bytes,
                    maximum_patches: o.maximum_patches_per_request,
                });
            }
        }
        out
    }
    fn request_source(
        &self,
        plugin: &str,
        load: PluginLoadId,
        proposal: &PluginVirtualDocumentProposal,
    ) -> Result<&Pending, PluginVirtualDocumentError> {
        let pending = self.pending.get(&proposal.request).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::UnknownRequest,
                "unknown request",
            )
        })?;
        if pending.plugin != plugin || pending.load != load {
            return Err(err(
                PluginVirtualDocumentErrorCode::StaleLoad,
                "foreign request load",
            ));
        }
        if !pending.delivered {
            return Err(err(
                PluginVirtualDocumentErrorCode::UnknownRequest,
                "request was not delivered",
            ));
        }
        if pending.document != proposal.document {
            return Err(err(
                PluginVirtualDocumentErrorCode::UnknownDocument,
                "request/document mismatch",
            ));
        }
        if pending.cancellation_id != proposal.cancellation_id {
            return Err(err(
                PluginVirtualDocumentErrorCode::Cancelled,
                "stale cancellation ID",
            ));
        }
        if pending.deadline < crate::clock::monotonic() {
            return Err(err(
                PluginVirtualDocumentErrorCode::Expired,
                "request deadline expired",
            ));
        }
        if !self
            .owners
            .contains_key(&(plugin.into(), load, pending.source.clone()))
        {
            return Err(err(
                PluginVirtualDocumentErrorCode::UnknownSource,
                "source load unavailable",
            ));
        }
        let document = self.documents.get(&proposal.document).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::UnknownDocument,
                "unknown document",
            )
        })?;
        if document.closed
            && !matches!(
                proposal.kind,
                PluginVirtualDocumentProposalKind::CloseAcknowledged
                    | PluginVirtualDocumentProposalKind::Error(_)
            )
        {
            return Err(err(
                PluginVirtualDocumentErrorCode::Cancelled,
                "document closed",
            ));
        }
        Ok(pending)
    }
    pub(crate) fn check_proposal(
        &self,
        plugin: &str,
        load: PluginLoadId,
        proposal: &PluginVirtualDocumentProposal,
    ) -> Result<(), PluginVirtualDocumentError> {
        self.request_source(plugin, load, proposal).map(|_| ())
    }
    pub fn apply(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        proposal: PluginVirtualDocumentProposal,
    ) -> Result<PluginVirtualDocumentSnapshot, PluginVirtualDocumentError> {
        let pending = self.request_source(plugin, load, &proposal)?.clone();
        let owner = &self
            .owners
            .get(&(plugin.into(), load, pending.source.clone()))
            .ok_or_else(|| {
                err(
                    PluginVirtualDocumentErrorCode::UnknownSource,
                    "source load unavailable",
                )
            })?
            .declaration;
        let mut candidate = self
            .documents
            .get(&proposal.document)
            .ok_or_else(|| {
                err(
                    PluginVirtualDocumentErrorCode::UnknownDocument,
                    "unknown document",
                )
            })?
            .clone();
        if candidate.closed
            && !matches!(
                proposal.kind,
                PluginVirtualDocumentProposalKind::CloseAcknowledged
                    | PluginVirtualDocumentProposalKind::Error(_)
            )
        {
            return Err(err(
                PluginVirtualDocumentErrorCode::Cancelled,
                "document closed",
            ));
        }
        match proposal.kind {
            PluginVirtualDocumentProposalKind::Full(full) => {
                let expected = (candidate.revision > 0).then_some(candidate.revision);
                if full.expected_revision != expected
                    || full.revision
                        != candidate.revision.checked_add(1).ok_or_else(|| {
                            err(
                                PluginVirtualDocumentErrorCode::Internal,
                                "revision exhausted",
                            )
                        })?
                {
                    return Err(err(
                        PluginVirtualDocumentErrorCode::StaleRevision,
                        "full proposal revision mismatch",
                    ));
                }
                if full.title != owner.title {
                    return Err(err(
                        PluginVirtualDocumentErrorCode::InvalidTitle,
                        "title differs from static declaration",
                    ));
                }
                if full.language != owner.language {
                    return Err(err(
                        PluginVirtualDocumentErrorCode::InvalidLanguage,
                        "language differs from static declaration",
                    ));
                }
                validate_document(&full.text, owner)?;
                candidate.title = full.title;
                candidate.language = full.language;
                candidate.text = full.text;
                candidate.revision = full.revision;
            }
            PluginVirtualDocumentProposalKind::Patches(set) => {
                if candidate.revision == 0
                    || set.expected_revision != candidate.revision
                    || set.revision
                        != candidate.revision.checked_add(1).ok_or_else(|| {
                            err(
                                PluginVirtualDocumentErrorCode::Internal,
                                "revision exhausted",
                            )
                        })?
                {
                    return Err(err(
                        PluginVirtualDocumentErrorCode::StaleRevision,
                        "patch revision mismatch",
                    ));
                }
                if set.patches.is_empty()
                    || set.patches.len() > usize::from(owner.maximum_patches_per_request)
                {
                    return Err(err(
                        PluginVirtualDocumentErrorCode::PatchLimit,
                        "patch count invalid",
                    ));
                }
                let mut patch_bytes = 0usize;
                let mut previous = 0usize;
                for p in &set.patches {
                    let start = p.start as usize;
                    let end = p.end as usize;
                    if start > end
                        || start < previous
                        || end > candidate.text.len()
                        || !candidate.text.is_char_boundary(start)
                        || !candidate.text.is_char_boundary(end)
                    {
                        return Err(err(
                            PluginVirtualDocumentErrorCode::InvalidUtf8Range,
                            "patch range invalid",
                        ));
                    }
                    validate_text(&p.replacement, owner.maximum_patch_bytes as usize, true)?;
                    patch_bytes =
                        patch_bytes
                            .checked_add(p.replacement.len())
                            .ok_or_else(|| {
                                err(
                                    PluginVirtualDocumentErrorCode::PatchLimit,
                                    "patch bytes overflow",
                                )
                            })?;
                    previous = end;
                }
                if patch_bytes > owner.maximum_patch_bytes as usize {
                    return Err(err(
                        PluginVirtualDocumentErrorCode::PatchLimit,
                        "aggregate patch bytes exceeded",
                    ));
                }
                for p in set.patches.iter().rev() {
                    candidate
                        .text
                        .replace_range(p.start as usize..p.end as usize, &p.replacement);
                }
                validate_document(&candidate.text, owner)?;
                candidate.revision = set.revision;
            }
            PluginVirtualDocumentProposalKind::CloseAcknowledged => {
                if pending.kind != PluginVirtualDocumentRequestKind::CloseNotice {
                    return Err(err(
                        PluginVirtualDocumentErrorCode::Malformed,
                        "close acknowledgement for non-close request",
                    ));
                }
                candidate.closed = true;
            }
            PluginVirtualDocumentProposalKind::Error(error) => {
                self.pending.remove(&proposal.request);
                if candidate.revision == 0 || candidate.closed {
                    self.documents.remove(&proposal.document);
                } else if let Some(document) = self.documents.get_mut(&proposal.document) {
                    document.pending_request = None;
                }
                return Err(error);
            }
        }
        candidate.pending_request = None;
        self.documents.insert(proposal.document, candidate);
        self.pending.remove(&proposal.request);
        self.snapshot(proposal.document).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::Internal,
                "snapshot unavailable",
            )
        })
    }
    pub fn command_for_document(
        &self,
        plugin: &str,
        load: PluginLoadId,
        document: u64,
    ) -> Option<&str> {
        let doc = self.documents.get(&document)?;
        (doc.plugin == plugin && doc.load == load)
            .then(|| {
                self.owners
                    .get(&(doc.plugin.clone(), doc.load, doc.source.clone()))
                    .map(|owner| owner.declaration.command.as_str())
            })
            .flatten()
    }

    /// Withdraw a pending request without losing a published document.
    pub fn cancel_pending(&mut self, plugin: &str, load: PluginLoadId, document: u64) -> bool {
        let Some(doc) = self.documents.get_mut(&document) else {
            return false;
        };
        if doc.plugin != plugin || doc.load != load {
            return false;
        }
        let Some(request) = doc.pending_request.take() else {
            return false;
        };
        self.pending.remove(&request);
        if doc.revision == 0 || doc.closed {
            self.documents.remove(&document);
        }
        true
    }
    pub fn cancel(&mut self, plugin: &str, load: PluginLoadId, document: u64) -> bool {
        let Some(d) = self.documents.get(&document) else {
            return false;
        };
        if d.plugin != plugin || d.load != load {
            return false;
        }
        if let Some(request) = d.pending_request {
            self.pending.remove(&request);
        }
        self.documents.remove(&document);
        true
    }
    pub fn snapshot(&self, document: u64) -> Option<PluginVirtualDocumentSnapshot> {
        let d = self.documents.get(&document)?;
        Some(PluginVirtualDocumentSnapshot {
            plugin: d.plugin.clone(),
            load: d.load,
            source: d.source.clone(),
            document,
            key: d.key.clone(),
            title: d.title.clone(),
            language: d.language.clone(),
            text: d.text.clone(),
            revision: d.revision,
            closed: d.closed,
        })
    }
    pub fn remove_load(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        let keys = self
            .owners
            .keys()
            .filter(|(p, l, _)| p == plugin && *l == load)
            .cloned()
            .collect::<Vec<_>>();
        let changed = !keys.is_empty();
        for k in keys {
            self.owners.remove(&k);
        }
        let docs = self
            .documents
            .iter()
            .filter_map(|(id, d)| (d.plugin == plugin && d.load == load).then_some(*id))
            .collect::<Vec<_>>();
        for id in docs {
            self.documents.remove(&id);
        }
        self.pending
            .retain(|_, p| p.plugin != plugin || p.load != load);
        changed
    }
    pub fn reset_runtime(&mut self) {
        self.documents.clear();
        self.pending.clear();
    }
    fn alloc_request(&mut self) -> Result<u64, PluginVirtualDocumentError> {
        let id = self.next_request.max(1);
        self.next_request = id.checked_add(1).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::Internal,
                "request ID exhausted",
            )
        })?;
        Ok(id)
    }
    fn alloc_document(&mut self) -> Result<u64, PluginVirtualDocumentError> {
        let id = self.next_document.max(1);
        self.next_document = id.checked_add(1).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::Internal,
                "document ID exhausted",
            )
        })?;
        Ok(id)
    }
    fn alloc_cancellation(&mut self) -> Result<u64, PluginVirtualDocumentError> {
        let id = self.next_cancellation.max(1);
        self.next_cancellation = id.checked_add(1).ok_or_else(|| {
            err(
                PluginVirtualDocumentErrorCode::Internal,
                "cancellation ID exhausted",
            )
        })?;
        Ok(id)
    }
}
fn err(code: PluginVirtualDocumentErrorCode, detail: &str) -> PluginVirtualDocumentError {
    PluginVirtualDocumentError::new(code, detail)
}
fn validate_text(
    value: &str,
    maximum: usize,
    allow_newlines: bool,
) -> Result<(), PluginVirtualDocumentError> {
    if value.len() > maximum {
        return Err(err(
            PluginVirtualDocumentErrorCode::ByteLimit,
            "text byte limit exceeded",
        ));
    }
    if value
        .chars()
        .any(|c| c == '\u{7f}' || (c.is_control() && !(allow_newlines && (c == '\n' || c == '\t'))))
    {
        return Err(err(
            PluginVirtualDocumentErrorCode::InvalidControl,
            "text contains disallowed controls",
        ));
    }
    Ok(())
}
fn validate_document(
    text: &str,
    o: &PluginVirtualDocumentContribution,
) -> Result<(), PluginVirtualDocumentError> {
    validate_text(text, o.maximum_document_bytes as usize, true)?;
    if text
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        .saturating_add(1)
        > o.maximum_document_lines as usize
    {
        return Err(err(
            PluginVirtualDocumentErrorCode::LineLimit,
            "document line limit exceeded",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::plugins::virtual_documents::*;
    use crate::plugins::*;
    use std::collections::BTreeMap;
    fn index() -> PluginContributionIndex {
        PluginContributionIndex {
            capabilities: Default::default(),
            owner: "demo".into(),
            version: "1".into(),
            checksum: "0".repeat(64),
            plugin_load: 7,
            source: "manifest".into(),
            commands: BTreeMap::new(),
            contexts: vec![],
            keymaps: vec![],
            key_groups: vec![],
            content_types: vec![],
            subscriptions: vec![],
            providers: vec![],
            virtual_documents: vec![PluginVirtualDocumentContribution {
                id: "plugin.demo.virtual.help".into(),
                title: "Help".into(),
                language: "markdown".into(),
                mode: PluginVirtualDocumentMode::ReadOnly,
                maximum_concurrent_documents: 1,
                maximum_document_bytes: 128,
                maximum_document_lines: 8,
                maximum_patch_bytes: 32,
                maximum_patches_per_request: 4,
                deadline_milliseconds: 1000,
                command: "plugin.demo.run".into(),
                context: "buffer".into(),
                provenance: None,
            }],
            storage: None,
        }
    }
    #[test]
    fn full_patch_cancellation_id_and_idle_are_atomic() {
        let mut b = PluginVirtualDocumentBroker::default();
        b.install(&index()).unwrap();
        let d = b
            .start(
                "demo",
                PluginLoadId(7),
                "plugin.demo.virtual.help",
                "key".into(),
            )
            .unwrap();
        assert!(
            b.requests_for_command("demo", PluginLoadId(7), "plugin.demo.other")
                .is_empty()
        );
        assert_eq!(
            b.command_for_document("demo", PluginLoadId(7), d),
            Some("plugin.demo.run")
        );
        let r = b.requests_for_command("demo", PluginLoadId(7), "plugin.demo.run")[0].clone();
        let s = b
            .apply(
                "demo",
                PluginLoadId(7),
                PluginVirtualDocumentProposal {
                    request: r.request,
                    document: d,
                    cancellation_id: r.cancellation_id,
                    kind: PluginVirtualDocumentProposalKind::Full(PluginVirtualDocumentFull {
                        expected_revision: None,
                        revision: 1,
                        title: "Help".into(),
                        language: "markdown".into(),
                        text: "héllo\nworld".into(),
                    }),
                },
            )
            .unwrap();
        assert_eq!(s.revision, 1);
        b.refresh("demo", PluginLoadId(7), d).unwrap();
        let r = b.requests_for_command("demo", PluginLoadId(7), "plugin.demo.run")[0].clone();
        let stable = b.snapshot(d).unwrap();
        let bad = PluginVirtualDocumentProposal {
            request: r.request,
            document: d,
            cancellation_id: r.cancellation_id,
            kind: PluginVirtualDocumentProposalKind::Patches(PluginVirtualDocumentPatchSet {
                expected_revision: 1,
                revision: 2,
                patches: vec![PluginVirtualDocumentPatch {
                    start: 1,
                    end: 2,
                    replacement: "x".into(),
                }],
            }),
        };
        assert_eq!(
            b.apply("demo", PluginLoadId(7), bad).unwrap_err().code,
            PluginVirtualDocumentErrorCode::InvalidUtf8Range
        );
        assert_eq!(b.snapshot(d).unwrap(), stable);
        let good = PluginVirtualDocumentProposal {
            request: r.request,
            document: d,
            cancellation_id: r.cancellation_id,
            kind: PluginVirtualDocumentProposalKind::Patches(PluginVirtualDocumentPatchSet {
                expected_revision: 1,
                revision: 2,
                patches: vec![PluginVirtualDocumentPatch {
                    start: 0,
                    end: 3,
                    replacement: "Hi".into(),
                }],
            }),
        };
        assert_eq!(
            b.apply("demo", PluginLoadId(7), good).unwrap().text,
            "Hillo\nworld"
        );
        for _ in 0..256 {
            assert_eq!(b.snapshot(d).unwrap().revision, 2);
        }
        b.refresh("demo", PluginLoadId(7), d).unwrap();
        let late = b.requests_for_command("demo", PluginLoadId(7), "plugin.demo.run")[0].clone();
        assert!(b.cancel("demo", PluginLoadId(7), d));
        assert_eq!(
            b.apply(
                "demo",
                PluginLoadId(7),
                PluginVirtualDocumentProposal {
                    request: late.request,
                    document: d,
                    cancellation_id: late.cancellation_id,
                    kind: PluginVirtualDocumentProposalKind::CloseAcknowledged
                }
            )
            .unwrap_err()
            .code,
            PluginVirtualDocumentErrorCode::UnknownRequest
        );
    }
    #[test]
    fn reset_retains_declarations_and_stale_cleanup_preserves_replacement() {
        let mut b = PluginVirtualDocumentBroker::default();
        let seven = index();
        b.install(&seven).unwrap();
        let old = b
            .start(
                "demo",
                PluginLoadId(7),
                "plugin.demo.virtual.help",
                "old".into(),
            )
            .unwrap();
        b.reset_runtime();
        assert!(b.snapshot(old).is_none());
        assert!(
            b.start(
                "demo",
                PluginLoadId(7),
                "plugin.demo.virtual.help",
                "again".into()
            )
            .is_ok()
        );
        let mut eight = index();
        eight.plugin_load = 8;
        b.install(&eight).unwrap();
        let replacement = b
            .start(
                "demo",
                PluginLoadId(8),
                "plugin.demo.virtual.help",
                "new".into(),
            )
            .unwrap();
        assert!(b.remove_load("demo", PluginLoadId(7)));
        assert!(b.snapshot(replacement).is_some());
        assert_eq!(
            b.command_for_document("demo", PluginLoadId(8), replacement),
            Some("plugin.demo.run")
        );
    }

    #[test]
    fn hostile_ownership_limits_and_cleanup() {
        let mut b = PluginVirtualDocumentBroker::default();
        b.install(&index()).unwrap();
        assert_eq!(
            b.start(
                "other",
                PluginLoadId(7),
                "plugin.demo.virtual.help",
                "x".into()
            )
            .unwrap_err()
            .code,
            PluginVirtualDocumentErrorCode::UnknownSource
        );
        let d = b
            .start(
                "demo",
                PluginLoadId(7),
                "plugin.demo.virtual.help",
                "x".into(),
            )
            .unwrap();
        assert_eq!(
            b.start(
                "demo",
                PluginLoadId(7),
                "plugin.demo.virtual.help",
                "y".into()
            )
            .unwrap_err()
            .code,
            PluginVirtualDocumentErrorCode::QueueFull
        );
        let r = b.requests_for_command("demo", PluginLoadId(7), "plugin.demo.run")[0].clone();
        for (title, language, text, code) in [
            (
                "Wrong",
                "markdown",
                "ok",
                PluginVirtualDocumentErrorCode::InvalidTitle,
            ),
            (
                "Help",
                "rust",
                "ok",
                PluginVirtualDocumentErrorCode::InvalidLanguage,
            ),
            (
                "Help",
                "markdown",
                "bad\u{1}",
                PluginVirtualDocumentErrorCode::InvalidControl,
            ),
        ] {
            let e = b
                .apply(
                    "demo",
                    PluginLoadId(7),
                    PluginVirtualDocumentProposal {
                        request: r.request,
                        document: d,
                        cancellation_id: r.cancellation_id,
                        kind: PluginVirtualDocumentProposalKind::Full(PluginVirtualDocumentFull {
                            expected_revision: None,
                            revision: 1,
                            title: title.into(),
                            language: language.into(),
                            text: text.into(),
                        }),
                    },
                )
                .unwrap_err();
            assert_eq!(e.code, code);
        }
        assert!(!b.remove_load("demo", PluginLoadId(8)));
        assert!(b.remove_load("demo", PluginLoadId(7)));
        assert!(b.snapshot(d).is_none());
    }

    #[test]
    fn virtual_document_retries_deduplicate_and_release_cancelled_text() {
        let mut broker = PluginVirtualDocumentBroker::default();
        broker.install(&index()).unwrap();
        let load = PluginLoadId(7);
        let source = "plugin.demo.virtual.help";
        let document = broker.start("demo", load, source, "key".into()).unwrap();
        assert_eq!(
            broker.start("demo", load, source, "key".into()).unwrap(),
            document
        );
        assert_eq!(broker.pending.len(), 1);
        let request = broker
            .requests_for_command("demo", load, "plugin.demo.run")
            .pop()
            .unwrap();
        broker
            .apply(
                "demo",
                load,
                PluginVirtualDocumentProposal {
                    request: request.request,
                    document,
                    cancellation_id: request.cancellation_id,
                    kind: PluginVirtualDocumentProposalKind::Full(PluginVirtualDocumentFull {
                        expected_revision: None,
                        revision: 1,
                        title: "Help".into(),
                        language: "markdown".into(),
                        text: "old text".into(),
                    }),
                },
            )
            .unwrap();
        broker.refresh("demo", load, document).unwrap();
        let request = broker
            .requests_for_command("demo", load, "plugin.demo.run")
            .pop()
            .unwrap();
        let error = err(PluginVirtualDocumentErrorCode::Internal, "retry later");
        assert_eq!(
            broker
                .apply(
                    "demo",
                    load,
                    PluginVirtualDocumentProposal {
                        request: request.request,
                        document,
                        cancellation_id: request.cancellation_id,
                        kind: PluginVirtualDocumentProposalKind::Error(error.clone())
                    }
                )
                .unwrap_err(),
            error
        );
        assert_eq!(broker.snapshot(document).unwrap().text, "old text");
        broker.refresh("demo", load, document).unwrap();
        for pending in broker.pending.values_mut() {
            pending.deadline = crate::clock::monotonic() - Duration::from_secs(1);
        }
        broker.refresh("demo", load, document).unwrap();
        assert_eq!(broker.pending.len(), 1);
        assert!(broker.cancel_pending("demo", load, document));
        broker.close("demo", load, document).unwrap();
        let close = broker
            .requests_for_command("demo", load, "plugin.demo.run")
            .pop()
            .unwrap();
        let proposal = PluginVirtualDocumentProposal {
            request: close.request,
            document,
            cancellation_id: close.cancellation_id,
            kind: PluginVirtualDocumentProposalKind::Full(PluginVirtualDocumentFull {
                expected_revision: Some(1),
                revision: 2,
                title: "Help".into(),
                language: "markdown".into(),
                text: "must not reopen".into(),
            }),
        };
        assert_eq!(
            broker
                .apply("demo", load, proposal.clone())
                .unwrap_err()
                .code,
            PluginVirtualDocumentErrorCode::Cancelled
        );
        let error = err(PluginVirtualDocumentErrorCode::Internal, "close failed");
        assert_eq!(
            broker
                .apply(
                    "demo",
                    load,
                    PluginVirtualDocumentProposal {
                        kind: PluginVirtualDocumentProposalKind::Error(error.clone()),
                        ..proposal
                    }
                )
                .unwrap_err(),
            error
        );
        assert!(broker.documents.is_empty() && broker.pending.is_empty());
        for _ in 0..100 {
            let document = broker.start("demo", load, source, "key".into()).unwrap();
            broker.cancel("demo", load, document);
        }
        assert!(broker.documents.is_empty());
    }

    #[test]
    fn failed_virtual_request_reservation_does_not_leave_a_document() {
        let mut broker = PluginVirtualDocumentBroker::default();
        let mut index = index();
        index.virtual_documents[0].maximum_concurrent_documents = 100;
        broker.install(&index).unwrap();
        for key in 0..64 {
            broker
                .start(
                    "demo",
                    PluginLoadId(7),
                    "plugin.demo.virtual.help",
                    key.to_string(),
                )
                .unwrap();
        }
        assert!(
            broker
                .start(
                    "demo",
                    PluginLoadId(7),
                    "plugin.demo.virtual.help",
                    "overflow".into()
                )
                .is_err()
        );
        assert_eq!(broker.documents.len(), 64);
        assert!(broker.is_opening("demo", PluginLoadId(7), "plugin.demo.virtual.help", "0"));
        for pending in broker.pending.values_mut() {
            pending.deadline = crate::clock::monotonic() - Duration::from_secs(1);
        }
        assert!(
            broker
                .start(
                    "demo",
                    PluginLoadId(7),
                    "plugin.demo.virtual.help",
                    "overflow".into()
                )
                .is_ok()
        );
        assert_eq!(broker.documents.len(), 1);
    }
}
