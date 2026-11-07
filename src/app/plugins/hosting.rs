//! Extension-owned retained/private presenters; graph publication stays with Windows.
use crate::app::configuration::AcceptedContributions;
use crate::app::configuration::ContributionCandidate;
use crate::app::configuration::WindowRuleSource;
use crate::app::plugins::Plugins;
use crate::app::plugins::panels::PluginUiInteraction;
use crate::app::plugins::panels::PluginWindowHost;
use crate::app::windows::PluginWindowKey;
use crate::app::windows::PluginWindowResult;
use crate::app::windows::PluginWindowTransition;
use crate::app::windows::Windows;
use crate::editor::EditorState;
use crate::plugins::PluginCatalogVersion;
use crate::plugins::PluginContributionCatalog;
use crate::plugins::PluginContributionIndex;
use crate::plugins::PluginLoadId;
use crate::plugins::PluginUiError;
use crate::plugins::PluginUiErrorCode;
use crate::plugins::PluginUiEvent;
use crate::plugins::PluginUiRequest;
use crate::ui::PluginSurfaceUiState;
use crate::ui::PrivateSurfaceUiState;
use crate::windows::CompositorProjection;
use crate::windows::WindowGraph;
use crate::windows::WindowId;

pub(in crate::app) enum WindowUpdate<'a> {
    Ready(PluginUiEvent),
    Publish(WindowPublication<'a>, Box<PluginWindowTransition>),
}
pub(in crate::app) struct WindowPublication<'a> {
    current: &'a mut PluginWindowHost,
    next: PluginWindowHost,
    key: PluginWindowKey,
    catalog: PluginCatalogVersion,
    event: PluginUiEvent,
}
impl WindowPublication<'_> {
    pub(in crate::app) fn catalog(&self) -> &PluginCatalogVersion {
        &self.catalog
    }
    pub(in crate::app) fn finish(
        mut self,
        accepted: Result<PluginWindowResult, String>,
        revision: Option<u64>,
    ) -> PluginUiEvent {
        let result = accepted.and_then(|accepted| {
            accepted
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
pub(in crate::app) struct WindowRules<'a> {
    current: &'a mut PluginWindowHost,
    next: PluginWindowHost,
    expected: WindowRuleSource,
}
impl WindowRules<'_> {
    pub(in crate::app) fn finish(self, accepted: AcceptedContributions) -> Result<(), String> {
        if !self.expected.accepts(&accepted) {
            return Err("plugin window rules do not match accepted configuration".into());
        }
        *self.current = self.next;
        Ok(())
    }
}
impl Plugins {
    pub(in crate::app) fn prepare_window_rules(
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
    pub(in crate::app) fn prepare_window(
        &mut self,
        index: &PluginContributionIndex,
        request: PluginUiRequest,
        editor: &EditorState,
        windows: &Windows,
        ambiguous_wide: bool,
    ) -> WindowUpdate<'_> {
        self.windows.set_ambiguous_width(ambiguous_wide);
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
                current.plugin_load == index.plugin_load && current.checksum == index.checksum
            })
        else {
            return rejected(
                PluginUiErrorCode::LoadUnavailable,
                "plugin UI request belongs to a unavailable catalog load".into(),
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
            .validate_window_request(windows.graph(), index, &request)
        {
            return WindowUpdate::Ready(event);
        }
        let catalog = self.plugin_contributions.version();
        let mut draft = match windows.begin_plugin(
            &index.owner,
            index.plugin_load,
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
            Box::new(transition),
        )
    }
    pub(in crate::app) fn window_loads(&self) -> Vec<(String, u64)> {
        self.windows.loads()
    }
    pub(in crate::app) fn remove_windows(&mut self, plugin: &str, load: u64) -> bool {
        self.windows.remove_load(plugin, load)
    }
    pub(in crate::app) fn window_context(&self, window: WindowId) -> Option<String> {
        self.windows
            .input_context(window, &self.plugin_contributions)
    }
    pub(in crate::app) fn window_snapshots(&self) -> Vec<PluginSurfaceUiState> {
        self.windows.snapshots()
    }
    pub(in crate::app) fn private_snapshots(
        &self,
        graph: &WindowGraph,
    ) -> Vec<PrivateSurfaceUiState> {
        self.windows.private_snapshots(graph)
    }
    pub(in crate::app) fn window_interaction(
        &mut self,
        graph: &WindowGraph,
        interaction: PluginUiInteraction,
    ) -> Result<Option<String>, PluginUiError> {
        self.windows
            .interact_focused(graph, &self.plugin_contributions, interaction)
    }
    pub(in crate::app) fn focus_window_row(
        &mut self,
        window: WindowId,
        row: usize,
    ) -> Result<(), PluginUiError> {
        self.windows.focus_node_at_row(window, row)
    }
    pub(in crate::app) fn synchronize_windows(
        &mut self,
        projection: Option<&CompositorProjection>,
        ambiguous_wide: bool,
    ) {
        self.windows.set_ambiguous_width(ambiguous_wide);
        for (plugin, load, event) in self.windows.synchronize_private_geometry(projection) {
            self.queue_ui(plugin, PluginLoadId(load), [event]);
        }
    }
    pub(in crate::app) fn private_action(
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
impl Plugins {
    pub(in crate::app) fn fixture_window_clone_count() -> usize {
        PluginWindowHost::test_clone_count()
    }
    pub(in crate::app) fn fixture_window_state(&self) -> String {
        format!("{:?}|{:?}", self.windows, self.ui)
    }
    pub(in crate::app) fn fixture_queued(&self) -> usize {
        self.manager.fixture_queued()
    }
    pub(in crate::app) fn fixture_delivery_state(&self) -> String {
        format!(
            "{:?}|{:?}|{:?}|{:?}|queued={}",
            self.windows,
            self.ui,
            self.events,
            self.results,
            self.manager.fixture_queued()
        )
    }
}
