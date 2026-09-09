//! Extension-owned retained/private presenters; graph publication stays with WindowSession.
use super::{
    ExtensionSession,
    windows::{PluginUiInteraction, PluginWindowHost},
};
use crate::{
    editor::session::configuration::{
        ContributionCandidate, ContributionReceipt, WindowRuleSource,
    },
    editor::{
        EditorState,
        session::windows::{
            PluginWindowKey, PluginWindowReceipt, PluginWindowTransition, WindowSession,
        },
    },
    extensions::{
        PluginCatalogVersion, PluginContributionCatalog, PluginContributionIndex, PluginLoadId,
        PluginUiError, PluginUiErrorCode, PluginUiEvent, PluginUiRequest,
    },
    ui::{
        PluginSurfaceUiState, PrivateSurfaceUiState,
        window::{CompositorProjection, WindowGraph, WindowId},
    },
};

pub(in crate::editor::session) enum WindowUpdate<'a> {
    Ready(PluginUiEvent),
    Publish(WindowPublication<'a>, PluginWindowTransition),
}
pub(in crate::editor::session) struct WindowPublication<'a> {
    current: &'a mut PluginWindowHost,
    next: PluginWindowHost,
    key: PluginWindowKey,
    catalog: PluginCatalogVersion,
    event: PluginUiEvent,
}
impl WindowPublication<'_> {
    pub(in crate::editor::session) fn catalog(&self) -> &PluginCatalogVersion {
        &self.catalog
    }
    pub(in crate::editor::session) fn finish(
        mut self,
        receipt: Result<PluginWindowReceipt, String>,
        revision: Option<u64>,
    ) -> PluginUiEvent {
        let result = receipt.and_then(|receipt| {
            receipt
                .matches(&self.key)
                .then_some(())
                .ok_or_else(|| "foreign plugin window publication".into())
        });
        match result {
            Ok(()) => *self.current = self.next,
            Err(detail) => {
                self.event.handle = None;
                self.event.tree_revision = None;
                self.event.surface_revision = None;
                self.event.error = Some(PluginUiError {
                    code: PluginUiErrorCode::StaleGraph,
                    detail,
                });
            }
        }
        self.event.graph_revision = revision;
        self.event
    }
}
pub(in crate::editor::session) struct WindowRules<'a> {
    current: &'a mut PluginWindowHost,
    next: PluginWindowHost,
    expected: WindowRuleSource,
}
impl WindowRules<'_> {
    pub(in crate::editor::session) fn finish(
        self,
        accepted: ContributionReceipt,
    ) -> Result<(), String> {
        if !self.expected.accepts(&accepted) {
            return Err("plugin window rules do not match accepted configuration".into());
        }
        *self.current = self.next;
        Ok(())
    }
}
impl ExtensionSession {
    pub(in crate::editor::session) fn prepare_window_rules(
        &mut self,
        candidate: &ContributionCandidate,
    ) -> Result<(WindowRules<'_>, &PluginContributionCatalog), String> {
        let mut next = self.windows.clone();
        next.set_configured_rules(candidate.windows())?;
        Ok((
            WindowRules {
                current: &mut self.windows,
                next,
                expected: candidate.window_source(),
            },
            &self.plugin_contributions,
        ))
    }
    pub(in crate::editor::session) fn prepare_window(
        &mut self,
        index: &PluginContributionIndex,
        request: PluginUiRequest,
        editor: &EditorState,
        windows: &WindowSession,
    ) -> WindowUpdate<'_> {
        let rejected = |code, detail| {
            WindowUpdate::Ready(PluginWindowHost::rejected(
                request.token,
                windows.graph().revision(),
                code,
                detail,
            ))
        };
        let Some(index) = self
            .plugin_contributions
            .get(&index.owner)
            .filter(|current| {
                current.generation == index.generation && current.checksum == index.checksum
            })
        else {
            return rejected(
                PluginUiErrorCode::GenerationLost,
                "plugin UI request belongs to a retired catalog load".into(),
            );
        };
        if let Err(detail) = windows.require_editor_source(&editor.window_version()) {
            return rejected(PluginUiErrorCode::StaleGraph, detail);
        }
        if PluginWindowHost::is_retained_update(&request.kind) {
            return WindowUpdate::Ready(self.windows.process_retained(
                windows.graph(),
                index,
                request,
            ));
        }
        if let Some(event) = self
            .windows
            .admit_window_request(windows.graph(), index, &request)
        {
            return WindowUpdate::Ready(event);
        }
        let catalog = self.plugin_contributions.version();
        let mut draft = match windows.begin_plugin(
            &index.owner,
            index.generation,
            (&editor.window_version(), &catalog),
        ) {
            Ok(draft) => draft,
            Err(detail) => return rejected(PluginUiErrorCode::StaleGraph, detail),
        };
        let mut next = self.windows.clone();
        let event = next.process(&mut draft, index, request);
        if event.error.is_some() {
            return WindowUpdate::Ready(event);
        }
        let (transition, key) = draft.seal();
        WindowUpdate::Publish(
            WindowPublication {
                current: &mut self.windows,
                next,
                key,
                catalog,
                event,
            },
            transition,
        )
    }
    pub(in crate::editor::session) fn window_generations(&self) -> Vec<(String, u64)> {
        self.windows.generations()
    }
    pub(in crate::editor::session) fn remove_windows(&mut self, plugin: &str, load: u64) -> bool {
        self.windows.remove_generation(plugin, load)
    }
    pub(in crate::editor::session) fn window_context(&self, window: WindowId) -> Option<String> {
        self.windows
            .input_context(window, &self.plugin_contributions)
    }
    pub(in crate::editor::session) fn window_snapshots(&self) -> Vec<PluginSurfaceUiState> {
        self.windows.snapshots()
    }
    pub(in crate::editor::session) fn private_snapshots(
        &self,
        graph: &WindowGraph,
    ) -> Vec<PrivateSurfaceUiState> {
        self.windows.private_snapshots(graph)
    }
    pub(in crate::editor::session) fn window_interaction(
        &mut self,
        graph: &WindowGraph,
        interaction: PluginUiInteraction,
    ) -> Result<Option<String>, PluginUiError> {
        self.windows
            .interact_focused(graph, &self.plugin_contributions, interaction)
    }
    pub(in crate::editor::session) fn focus_window_row(
        &mut self,
        window: WindowId,
        row: usize,
    ) -> Result<(), PluginUiError> {
        self.windows.focus_node_at_row(window, row)
    }
    pub(in crate::editor::session) fn synchronize_windows(
        &mut self,
        projection: Option<&CompositorProjection>,
    ) {
        for (plugin, load, event) in self.windows.synchronize_private_geometry(projection) {
            self.queue_ui(plugin, PluginLoadId(load), [event]);
        }
    }
    pub(in crate::editor::session) fn private_action(
        &mut self,
        window: WindowId,
        x: u16,
        y: u16,
    ) -> Result<Option<String>, String> {
        let (plugin, load, event) = self
            .windows
            .private_action_at(window, x, y, &self.plugin_contributions)
            .map_err(|error| error.detail)?;
        let action = event.action.clone();
        self.push_action(plugin, PluginLoadId(load), event)?;
        Ok(action)
    }
}
#[cfg(test)]
impl ExtensionSession {
    pub(in crate::editor::session) fn fixture_window_clone_count() -> usize {
        PluginWindowHost::test_clone_count()
    }
}
