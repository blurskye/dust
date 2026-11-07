//! Bounded interaction, document and chrome requests from components.

use crate::plugins::component::component_api::dust::plugin::host as wit;

#[derive(Clone, Debug)]
pub(crate) enum SurfaceRequest {
    Input(u64, wit::InputRequest, String),
    Select(u64, wit::SelectRequest, String),
    Notify(wit::NotifyRequest),
    Presenter(wit::PresenterKind, Option<String>),
    AnswerInput(u64, Option<String>),
    AnswerSelect(u64, Option<String>),
    Document(u64, wit::EditableDocument, String),
    Formatter(String, wit::Formatter, bool),
    Segment(wit::ChromeSegment),
    RemoveSegment(wit::SegmentLocation, String),
}

impl SurfaceRequest {
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Self::Input(_, request, handler) => {
                request.title.len() + request.initial.len() + handler.len()
            }
            Self::Select(_, request, handler) => {
                request.title.len()
                    + handler.len()
                    + request
                        .items
                        .iter()
                        .map(|item| {
                            item.id.len()
                                + item.label.len()
                                + item.detail.as_ref().map_or(0, String::len)
                        })
                        .sum::<usize>()
            }
            Self::Notify(request) => request.message.len(),
            Self::Presenter(_, handler) => handler.as_ref().map_or(0, String::len),
            Self::AnswerInput(_, value) | Self::AnswerSelect(_, value) => {
                value.as_ref().map_or(0, String::len)
            }
            Self::Document(_, document, handler) => {
                document.source.len()
                    + document.key.len()
                    + document.title.len()
                    + document.language.len()
                    + document.text.len()
                    + handler.len()
            }
            Self::Formatter(filetype, step, _) => {
                filetype.len() + step.name.len() + step.handler.len()
            }
            Self::Segment(segment) => {
                segment.name.len()
                    + segment.command.as_ref().map_or(0, String::len)
                    + segment
                        .text
                        .iter()
                        .map(|part| part.text.len() + part.style.len())
                        .sum::<usize>()
            }
            Self::RemoveSegment(_, name) => name.len(),
        }
    }
}

impl crate::plugins::component::ComponentHostState {
    pub(crate) fn queue_surface(&mut self, request: SurfaceRequest) -> Result<(), String> {
        use crate::plugins::manifest::PluginCapability;
        let edit = matches!(
            request,
            SurfaceRequest::Document(..) | SurfaceRequest::Formatter(..)
        );
        if edit {
            self.require_editor_capability(PluginCapability::ProposeEdits)?;
        } else if !self.capabilities.contains(&PluginCapability::Input)
            && !self.capabilities.contains(&PluginCapability::PrivateCells)
        {
            self.require_editor_capability(PluginCapability::ShowMessage)?;
        } else if !self.execution_available || self.confirmation_result_only {
            return Err("surface requests are unavailable in this callback".into());
        }
        match &request {
            SurfaceRequest::Select(_, request, _) if request.items.len() > 4096 => {
                return Err("select item limit exceeded".into());
            }
            SurfaceRequest::Segment(segment) if segment.text.len() > 64 => {
                return Err("segment text limit exceeded".into());
            }
            _ => {}
        }
        let bytes = request.bytes();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Surface(request),
            bytes,
        )
    }
}
