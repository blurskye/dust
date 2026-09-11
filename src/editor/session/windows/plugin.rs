use super::{WindowCandidate, WindowCore, WindowSession};
use crate::ui::window::{
    ContentAdapterKind, ContentId, ContentTypeId, ViewId, WindowDescriptor, WindowError,
    WindowGraph, WindowId, WindowPlacement, WorkspaceTabId,
};
use std::sync::Arc;

/// Detached, generation-scoped proposal. It has no mutable graph escape hatch.
#[derive(Clone)]
pub(in crate::editor::session) struct PluginWindowDraft {
    origin: Arc<()>,
    epoch: u64,
    source: Option<crate::editor::EditorWindowVersion>,
    catalog: crate::extensions::PluginCatalogVersion,
    owner: String,
    generation: u64,
    graph: WindowGraph,
}

pub(in crate::editor::session) struct PluginWindowTransition {
    draft: PluginWindowDraft,
    key: Arc<()>,
}
pub(in crate::editor::session) struct PluginWindowKey(Arc<()>);
pub(in crate::editor::session) struct PluginWindowReceipt(Arc<()>);
impl PluginWindowReceipt {
    pub(in crate::editor::session) fn matches(&self, key: &PluginWindowKey) -> bool {
        Arc::ptr_eq(&self.0, &key.0)
    }
}
impl PluginWindowDraft {
    pub(in crate::editor::session) fn seal(self) -> (PluginWindowTransition, PluginWindowKey) {
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
impl WindowSession {
    pub(in crate::editor::session) fn accept_plugin_transition(
        &mut self,
        transition: PluginWindowTransition,
        current: &crate::editor::EditorWindowVersion,
        catalog: &crate::extensions::PluginCatalogVersion,
    ) -> Result<PluginWindowReceipt, String> {
        self.commit_plugin(transition.draft, current, catalog)?;
        Ok(PluginWindowReceipt(transition.key))
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn begin_plugin_fixture(
        &self,
        plugin: &str,
        generation: u64,
    ) -> PluginWindowDraft {
        self.plugin_draft(
            plugin,
            generation,
            None,
            crate::extensions::PluginCatalogVersion::default(),
        )
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn commit_plugin_fixture(
        &mut self,
        draft: PluginWindowDraft,
    ) -> Result<(), String> {
        self.publish_plugin(draft)
    }
    pub(in crate::editor::session) fn begin_plugin(
        &self,
        plugin: &str,
        generation: u64,
        versions: (
            &crate::editor::EditorWindowVersion,
            &crate::extensions::PluginCatalogVersion,
        ),
    ) -> Result<PluginWindowDraft, String> {
        self.require_editor_source(versions.0)?;
        self.epoch
            .checked_add(1)
            .ok_or("window owner revision is exhausted")?;
        Ok(self.plugin_draft(
            plugin,
            generation,
            Some(versions.0.clone()),
            versions.1.clone(),
        ))
    }
    fn plugin_draft(
        &self,
        plugin: &str,
        generation: u64,
        source: Option<crate::editor::EditorWindowVersion>,
        catalog: crate::extensions::PluginCatalogVersion,
    ) -> PluginWindowDraft {
        PluginWindowDraft {
            origin: self.origin.clone(),
            epoch: self.epoch,
            source,
            catalog,
            owner: format!("plugin.{plugin}"),
            generation,
            graph: self.core.graph.clone(),
        }
    }
    pub(in crate::editor::session) fn commit_plugin(
        &mut self,
        draft: PluginWindowDraft,
        current: &crate::editor::EditorWindowVersion,
        catalog: &crate::extensions::PluginCatalogVersion,
    ) -> Result<(), String> {
        if draft.source.as_ref() != Some(current) || &draft.catalog != catalog {
            return Err("plugin window editor or catalog source is stale".into());
        }
        self.publish_plugin(draft)
    }
    fn publish_plugin(&mut self, draft: PluginWindowDraft) -> Result<(), String> {
        if !Arc::ptr_eq(&self.origin, &draft.origin) || self.epoch != draft.epoch {
            return Err("window candidate is stale or belongs to another session".into());
        }
        let mut core = WindowCore {
            graph: draft.graph,
            editor: self.core.editor.clone(),
            native: self.core.native.clone(),
        };
        core.native.invalidate();
        self.commit(WindowCandidate {
            dashboard: None,
            origin: draft.origin,
            epoch: draft.epoch,
            core,
        })
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn from_test_graph(graph: WindowGraph) -> Self {
        graph.validate().expect("valid owned initial graph fixture");
        let mut session = Self::default();
        session.core.graph = graph;
        session
    }
}
impl PluginWindowDraft {
    pub(in crate::editor::session) fn read(&self) -> &WindowGraph {
        &self.graph
    }
    pub(in crate::editor::session) fn revision(&self) -> Option<u64> {
        self.graph.revision()
    }
    pub(in crate::editor::session) fn active_tab(&self) -> Option<WorkspaceTabId> {
        self.graph.active_tab()
    }
    pub(in crate::editor::session) fn window(&self, id: WindowId) -> Option<&WindowDescriptor> {
        self.graph.window(id)
    }
    pub(in crate::editor::session) fn validate(&self) -> Result<(), WindowError> {
        self.graph.validate()
    }
    fn require_owner(&self, owner: &str, generation: u64) -> Result<(), WindowError> {
        if owner == self.owner && generation == self.generation {
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
        self.require_owner(&window.owner, window.generation)?;
        if self.graph.active_tab() != Some(window.tab) {
            return Err(WindowError::WindowOutsideActiveTab);
        }
        Ok(())
    }
    pub(in crate::editor::session) fn create_content_with_contract(
        &mut self,
        kind: ContentTypeId,
        contract: crate::ui::window::ContentContract,
        title: String,
        dirty: bool,
        owner: &str,
        generation: u64,
    ) -> Result<ContentId, WindowError> {
        self.require_owner(owner, generation)?;
        if !kind.as_str().starts_with("plugin.")
            || !matches!(
                contract.adapter,
                ContentAdapterKind::DeclarativeUi | ContentAdapterKind::PrivateCellSurface
            )
        {
            return Err(WindowError::InvalidContentType(kind.as_str().to_owned()));
        }
        self.graph
            .create_content_with_contract(kind, contract, title, dirty, owner, generation)
    }
    pub(in crate::editor::session) fn create_view(
        &mut self,
        content: ContentId,
        owner: &str,
        generation: u64,
    ) -> Result<ViewId, WindowError> {
        self.require_owner(owner, generation)?;
        let existing = self
            .graph
            .content(content)
            .ok_or(WindowError::UnknownContent)?;
        self.require_owner(&existing.owner, existing.generation)?;
        self.graph.create_view(content, owner, generation)
    }
    pub(in crate::editor::session) fn open_window(
        &mut self,
        tab: WorkspaceTabId,
        view: ViewId,
        placement: WindowPlacement,
        focusable: bool,
        owner: &str,
        generation: u64,
    ) -> Result<WindowId, WindowError> {
        self.require_owner(owner, generation)?;
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
        self.require_owner(&existing.owner, existing.generation)?;
        self.graph
            .open_window(tab, view, placement, focusable, owner, generation)
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn open_tiled_window(
        &mut self,
        tab: WorkspaceTabId,
        view: ViewId,
        focusable: bool,
        owner: &str,
        generation: u64,
        insertion: (Option<WindowId>, crate::ui::window::TileAxis, bool, u16),
    ) -> Result<WindowId, WindowError> {
        self.require_owner(owner, generation)?;
        if self.graph.active_tab() != Some(tab) {
            return Err(WindowError::WindowOutsideActiveTab);
        }
        let existing = self.graph.view(view).ok_or(WindowError::UnknownView)?;
        self.require_owner(&existing.owner, existing.generation)?;
        let window = self.graph.open_window(
            tab,
            view,
            WindowPlacement::Tiled,
            focusable,
            owner,
            generation,
        )?;
        self.graph
            .tile(window, insertion.0, insertion.1, insertion.2, insertion.3)?;
        Ok(window)
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn tile(
        &mut self,
        window: WindowId,
        insertion: (Option<WindowId>, crate::ui::window::TileAxis, bool, u16),
    ) -> Result<(), WindowError> {
        self.require_window(window)?;
        self.graph
            .tile(window, insertion.0, insertion.1, insertion.2, insertion.3)
    }
    pub(in crate::editor::session) fn place(
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
    pub(in crate::editor::session) fn focus(
        &mut self,
        window: WindowId,
    ) -> Result<(), WindowError> {
        self.require_window(window)?;
        self.graph.focus(window)
    }
    pub(in crate::editor::session) fn close_window(
        &mut self,
        window: WindowId,
    ) -> Result<(), WindowError> {
        self.require_window(window)?;
        self.graph.close_window(window)
    }
    pub(in crate::editor::session) fn remove_view(
        &mut self,
        view: ViewId,
    ) -> Result<(), WindowError> {
        let existing = self.graph.view(view).ok_or(WindowError::UnknownView)?;
        self.require_owner(&existing.owner, existing.generation)?;
        self.graph.remove_view(view)
    }
    pub(in crate::editor::session) fn remove_content(
        &mut self,
        content: ContentId,
    ) -> Result<(), WindowError> {
        let existing = self
            .graph
            .content(content)
            .ok_or(WindowError::UnknownContent)?;
        self.require_owner(&existing.owner, existing.generation)?;
        self.graph.remove_content(content)
    }
}
