//! Plugin-owned window drafts and checked publication.

use crate::app::windows::WindowCandidate;
use crate::app::windows::WindowState;
use crate::app::windows::Windows;
use crate::windows::ContentAdapterKind;
use crate::windows::ContentId;
use crate::windows::ContentTypeId;
use crate::windows::ViewId;
use crate::windows::WindowDescriptor;
use crate::windows::WindowError;
use crate::windows::WindowGraph;
use crate::windows::WindowId;
use crate::windows::WindowPlacement;
use crate::windows::WorkspaceTabId;
use std::sync::Arc;

/// Detached, plugin-load-scoped proposal. It has no mutable graph escape hatch.
#[derive(Clone)]
pub(in crate::app) struct PluginWindowDraft {
    origin: Arc<()>,
    epoch: u64,
    source: Option<crate::editor::EditorWindowVersion>,
    catalog: crate::plugins::PluginCatalogVersion,
    owner: String,
    load_id: u64,
    graph: WindowGraph,
}

pub(in crate::app) struct PluginWindowTransition {
    draft: PluginWindowDraft,
    key: Arc<()>,
}
pub(in crate::app) struct PluginWindowKey(Arc<()>);
pub(in crate::app) struct PluginWindowResult(Arc<()>);
impl PluginWindowResult {
    pub(in crate::app) fn matches(&self, key: &PluginWindowKey) -> bool {
        Arc::ptr_eq(&self.0, &key.0)
    }
}
impl PluginWindowDraft {
    pub(in crate::app) fn seal(self) -> (PluginWindowTransition, PluginWindowKey) {
        let key = Arc::new(());
        (
            PluginWindowTransition {
                draft: self,
                key: key.clone(),
            },
            PluginWindowKey(key),
        )
    }
}
impl Windows {
    pub(in crate::app) fn accept_plugin_transition(
        &mut self,
        transition: PluginWindowTransition,
        current: &crate::editor::EditorWindowVersion,
        catalog: &crate::plugins::PluginCatalogVersion,
        state: &mut crate::editor::EditorState,
    ) -> Result<PluginWindowResult, String> {
        self.commit_plugin(transition.draft, current, catalog, state)?;
        Ok(PluginWindowResult(transition.key))
    }

    #[cfg(test)]
    pub(in crate::app) fn begin_plugin_fixture(
        &self,
        plugin: &str,
        load_id: u64,
    ) -> PluginWindowDraft {
        self.plugin_draft(
            plugin,
            load_id,
            None,
            crate::plugins::PluginCatalogVersion::default(),
        )
    }
    #[cfg(test)]
    pub(in crate::app) fn commit_plugin_fixture(
        &mut self,
        draft: PluginWindowDraft,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        self.publish_plugin(draft, state)
    }
    pub(in crate::app) fn begin_plugin(
        &self,
        plugin: &str,
        load_id: u64,
        versions: (
            &crate::editor::EditorWindowVersion,
            &crate::plugins::PluginCatalogVersion,
        ),
    ) -> Result<PluginWindowDraft, String> {
        self.require_editor_source(versions.0)?;
        self.epoch
            .checked_add(1)
            .ok_or("window owner revision is exhausted")?;
        Ok(self.plugin_draft(
            plugin,
            load_id,
            Some(versions.0.clone()),
            versions.1.clone(),
        ))
    }
    fn plugin_draft(
        &self,
        plugin: &str,
        load_id: u64,
        source: Option<crate::editor::EditorWindowVersion>,
        catalog: crate::plugins::PluginCatalogVersion,
    ) -> PluginWindowDraft {
        PluginWindowDraft {
            origin: self.origin.clone(),
            epoch: self.epoch,
            source,
            catalog,
            owner: format!("plugin.{plugin}"),
            load_id,
            graph: self.core.graph.clone(),
        }
    }
    pub(in crate::app) fn commit_plugin(
        &mut self,
        draft: PluginWindowDraft,
        current: &crate::editor::EditorWindowVersion,
        catalog: &crate::plugins::PluginCatalogVersion,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        if draft.source.as_ref() != Some(current) || &draft.catalog != catalog {
            return Err("plugin window editor or catalog source is stale".into());
        }
        self.publish_plugin(draft, state)
    }
    fn publish_plugin(
        &mut self,
        draft: PluginWindowDraft,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&self.origin, &draft.origin) || self.epoch != draft.epoch {
            return Err("window candidate is stale or belongs to another session".into());
        }
        let mut core = WindowState {
            graph: draft.graph,
            source: self.core.source.clone(),
            scratch_label: self.core.scratch_label.clone(),
            native: self.core.native.clone(),
        };
        core.native.invalidate();
        self.commit(
            WindowCandidate {
                dashboard: None,
                origin: draft.origin,
                epoch: draft.epoch,
                core,
            },
            state,
        )
    }
    #[cfg(test)]
    pub(in crate::app) fn from_test_graph(graph: WindowGraph) -> Self {
        graph.validate().expect("valid owned initial graph fixture");
        let mut session = Self::default();
        session.core.graph = graph;
        session
    }
}
impl PluginWindowDraft {
    pub(in crate::app) fn read(&self) -> &WindowGraph {
        &self.graph
    }
    pub(in crate::app) fn revision(&self) -> Option<u64> {
        self.graph.revision()
    }
    pub(in crate::app) fn active_tab(&self) -> Option<WorkspaceTabId> {
        self.graph.active_tab()
    }
    pub(in crate::app) fn window(&self, id: WindowId) -> Option<&WindowDescriptor> {
        self.graph.window(id)
    }
    pub(in crate::app) fn validate(&self) -> Result<(), WindowError> {
        self.graph.validate()
    }
    fn require_owner(&self, owner: &str, load_id: u64) -> Result<(), WindowError> {
        if owner == self.owner && load_id == self.load_id {
            Ok(())
        } else {
            Err(WindowError::InvalidOwner)
        }
    }
    fn require_window(&self, window: WindowId) -> Result<(), WindowError> {
        let window = self
            .graph
            .window(window)
            .ok_or(WindowError::UnknownWindow)?;
        self.require_owner(&window.owner, window.instance_id)?;
        if self.graph.active_tab() != Some(window.tab) {
            return Err(WindowError::WindowOutsideActiveTab);
        }
        Ok(())
    }
    pub(in crate::app) fn create_content_with_contract(
        &mut self,
        kind: ContentTypeId,
        contract: crate::windows::ContentContract,
        title: String,
        dirty: bool,
        owner: &str,
        load_id: u64,
    ) -> Result<ContentId, WindowError> {
        self.require_owner(owner, load_id)?;
        if !kind.as_str().starts_with("plugin.")
            || !matches!(
                contract.adapter,
                ContentAdapterKind::DeclarativeUi | ContentAdapterKind::PrivateCellSurface
            )
        {
            return Err(WindowError::InvalidContentType(kind.as_str().to_owned()));
        }
        self.graph
            .create_content_with_contract(kind, contract, title, dirty, owner, load_id)
    }
    pub(in crate::app) fn create_view(
        &mut self,
        content: ContentId,
        owner: &str,
        load_id: u64,
    ) -> Result<ViewId, WindowError> {
        self.require_owner(owner, load_id)?;
        let existing = self
            .graph
            .content(content)
            .ok_or(WindowError::UnknownContent)?;
        self.require_owner(&existing.owner, existing.instance_id)?;
        self.graph.create_view(content, owner, load_id)
    }
    pub(in crate::app) fn open_window(
        &mut self,
        tab: WorkspaceTabId,
        view: ViewId,
        placement: WindowPlacement,
        focusable: bool,
        owner: &str,
        load_id: u64,
    ) -> Result<WindowId, WindowError> {
        self.require_owner(owner, load_id)?;
        if self.graph.active_tab() != Some(tab) {
            return Err(WindowError::WindowOutsideActiveTab);
        }
        if matches!(
            placement,
            WindowPlacement::Tiled | WindowPlacement::Overlay(_)
        ) {
            return Err(WindowError::InvalidPlacement);
        }
        let existing = self.graph.view(view).ok_or(WindowError::UnknownView)?;
        self.require_owner(&existing.owner, existing.instance_id)?;
        self.graph
            .open_window(tab, view, placement, focusable, owner, load_id)
    }
    pub(in crate::app) fn place(
        &mut self,
        window: WindowId,
        placement: WindowPlacement,
    ) -> Result<(), WindowError> {
        self.require_window(window)?;
        if matches!(
            placement,
            WindowPlacement::Tiled | WindowPlacement::Overlay(_)
        ) {
            return Err(WindowError::InvalidPlacement);
        }
        self.graph.place(window, placement)
    }
    pub(in crate::app) fn focus(&mut self, window: WindowId) -> Result<(), WindowError> {
        let target = self
            .graph
            .window(window)
            .ok_or(WindowError::UnknownWindow)?;
        self.require_owner(&target.owner, target.instance_id)?;
        self.graph.focus(window)
    }
    pub(in crate::app) fn close_window(&mut self, window: WindowId) -> Result<(), WindowError> {
        self.require_window(window)?;
        self.graph.close_window(window)
    }
    pub(in crate::app) fn remove_view(&mut self, view: ViewId) -> Result<(), WindowError> {
        let existing = self.graph.view(view).ok_or(WindowError::UnknownView)?;
        self.require_owner(&existing.owner, existing.instance_id)?;
        self.graph.remove_view(view)
    }
    pub(in crate::app) fn remove_content(&mut self, content: ContentId) -> Result<(), WindowError> {
        let existing = self
            .graph
            .content(content)
            .ok_or(WindowError::UnknownContent)?;
        self.require_owner(&existing.owner, existing.instance_id)?;
        self.graph.remove_content(content)
    }
}
