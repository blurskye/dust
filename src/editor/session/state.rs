use super::*;

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum DeferredExecution {
    InstallTool {
        id: String,
        source: Option<LspRuntimeSource>,
    },
    StartLsp(Option<String>),
    RestartLsp(String),
    StartDebug(crate::editor::session::debugger::DebugIntent),
    InstallRegistry {
        publisher: String,
        name: String,
        version: String,
    },
    PrepareRegistryArtifact,
    InstallPackage {
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    },
    RunTask(String),
    OpenTerminal(crate::editor::session::terminal::TerminalIntent),
    OpenDapTerminal {
        intent: crate::editor::session::terminal::TerminalIntent,
        reply: crate::debugger::DapTerminalReply,
    },
    RunTest {
        name: String,
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunLinter {
        name: String,
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunFormatter {
        name: String,
        source: crate::editor::session::tasks::FormatterSource,
    },
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingExecutionValidation {
    pub(in crate::editor::session) action: DeferredExecution,
    pub(in crate::editor::session) id: super::super::workspace::ValidationId,
}

pub(in crate::editor::session) fn compose_workspace_file_items(
    items: Vec<(PickerItem, PickerPayload)>,
) -> Result<Vec<(PickerItem, PickerPayload)>, crate::workspace::search_workflow::SearchWorkflowError>
{
    use crate::workspace::search_workflow::*;
    let mut broker = SearchWorkflowBroker::try_new(SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Files,
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    })?;
    broker.begin(SearchRequestId(1), SearchStreamGeneration(1), "")?;
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(1),
        generation: SearchStreamGeneration(1),
        sequence: 0,
        items: items
            .iter()
            .map(|(item, _)| picker_item_adapter(item))
            .collect(),
        complete: true,
    })?;
    let order = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?
        .items
        .into_iter()
        .map(|item| item.stable_key)
        .collect::<Vec<_>>();
    let mut map = items
        .into_iter()
        .map(|pair| (pair.0.id.0.clone(), pair))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|key| map.remove(&key))
        .collect())
}

pub(in crate::editor::session) fn compose_plugin_picker_items(
    projection: &crate::extensions::PluginPickerProjection,
) -> Result<Vec<(PickerItem, PickerPayload)>, crate::workspace::search_workflow::SearchWorkflowError>
{
    use crate::workspace::search_workflow::*;
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(projection.request),
        source: SearchSourceKind::PluginProvider {
            provider: projection.provider.clone(),
        },
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: projection.items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(
        SearchRequestId(projection.request),
        SearchStreamGeneration(projection.revision),
        &projection.query,
    )?;
    let workflow = projection
        .items
        .iter()
        .map(|item| plugin_item_adapter(&item.id, &item.primary, item.secondary.as_deref()))
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(projection.request),
        generation: SearchStreamGeneration(projection.revision),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let snapshot = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?;
    let mut source = projection
        .items
        .iter()
        .map(|item| (item.id.clone(), item))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(snapshot
        .items
        .into_iter()
        .filter_map(|ranked| {
            source.remove(&ranked.stable_key).map(|item| {
                let id = PickerItemId(format!(
                    "plugin-provider:{}:{}:{}",
                    projection.request, projection.revision, item.id
                ));
                (
                    PickerItem {
                        id,
                        label: item.primary.clone(),
                        detail: item.secondary.clone().unwrap_or_default(),
                        source: projection.provider.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::PluginProviderItem {
                        plugin: projection.plugin.clone(),
                        load: projection.load,
                        request: projection.request,
                        item: item.id.clone(),
                        action_command: projection.action_command.clone(),
                    },
                )
            })
        })
        .collect())
}

pub(in crate::editor::session) fn compose_command_picker_items(
    items: Vec<(PickerItem, PickerPayload)>,
) -> Result<Vec<(PickerItem, PickerPayload)>, crate::workspace::search_workflow::SearchWorkflowError>
{
    use crate::workspace::search_workflow::*;
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Commands,
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(SearchRequestId(1), SearchStreamGeneration(1), "")?;
    let workflow = items
        .iter()
        .map(|(item, payload)| {
            let mut item = item.clone();
            if let PickerPayload::Command(command) = payload {
                item.action = PickerAction::InvokeCommand {
                    command: command.clone(),
                }
            }
            picker_item_adapter(&item)
        })
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(1),
        generation: SearchStreamGeneration(1),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let order = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?
        .items
        .into_iter()
        .map(|item| item.stable_key)
        .collect::<Vec<_>>();
    let mut by_id = items
        .into_iter()
        .map(|pair| (pair.0.id.0.clone(), pair))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect())
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum PendingVfs {
    LspNavigation {
        range: crate::language::lsp::LspRange,
        encoding: PositionEncoding,
    },
    PersistedNavigation {
        location: PersistedLocation,
    },
    MatcherNavigation {
        source: MatcherSource,
    },
    WorkspaceSearchNavigation {
        range: std::ops::Range<usize>,
    },
    Completion {
        key: crate::editor::session::input::PromptCompletionKey,
    },
    CrashDelete,
}
