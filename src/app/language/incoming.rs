//! Method-specific command entry points for incoming language-server events.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::commands::register;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandInvocation;
use crate::editor::CommandRegistry;
use crate::editor::CommandValue;
use crate::editor::ContextRequirement;
use crate::editor::InvocationSource;
use crate::language::lsp::LspEvent;
use crate::language::lsp::OwnedLspEvent;

const METHODS: &[&str] = &[
    "textDocument/publishDiagnostics",
    "$/progress",
    "experimental/serverStatus",
    "window/logMessage",
    "window/showMessage",
    "client/registerCapability",
    "client/unregisterCapability",
    "workspace/inlayHint/refresh",
    "workspace/applyEdit",
    "textDocument/hover",
    "textDocument/completion",
    "completionItem/resolve",
    "textDocument/signatureHelp",
    "textDocument/definition",
    "textDocument/declaration",
    "textDocument/typeDefinition",
    "textDocument/implementation",
    "textDocument/references",
    "textDocument/documentSymbol",
    "workspace/symbol",
    "textDocument/rename",
    "textDocument/formatting",
    "textDocument/rangeFormatting",
    "textDocument/codeAction",
    "workspace/executeCommand",
    "textDocument/diagnostic",
    "textDocument/semanticTokens/full",
    "textDocument/semanticTokens/full/delta",
    "textDocument/inlayHint",
    "textDocument/codeLens",
    "codeLens/resolve",
    "textDocument/documentHighlight",
    "textDocument/foldingRange",
    "textDocument/documentColor",
    "textDocument/selectionRange",
];

pub(in crate::app) fn command_id(method: &str) -> String {
    format!(
        "lsp.incoming.{}",
        method
            .replace('$', "dollar")
            .replace('/', ".")
            .to_ascii_lowercase()
    )
}

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for &method in METHODS {
        register(
            registry,
            &command_id(method),
            &format!("Handle incoming LSP {method}"),
            ContextRequirement::Application,
            ArgumentSchema::default()
                .field("event", ArgumentType::Integer, true)
                .field("method", ArgumentType::String, true)
                .field("server", ArgumentType::String, true),
            move |invocation| {
                if invocation.arguments["method"].as_str()? != method {
                    return Err(CommandError::InvalidArguments(
                        "LSP method does not match its entry point".into(),
                    ));
                }
                Ok(AppEffect::LspIncoming(
                    invocation.arguments["event"].as_integer()?,
                ))
            },
        )?;
        registry.set_access(
            &crate::editor::CommandId::new(command_id(method))?,
            [crate::editor::CommandAccess::Edit].into(),
        )?;
    }
    Ok(())
}

fn method(event: &LspEvent) -> Option<(&str, &str)> {
    match event {
        LspEvent::Diagnostics { server, .. } => Some(("textDocument/publishDiagnostics", server)),
        LspEvent::Progress { server, .. } => Some(("$/progress", server)),
        LspEvent::ServerStatus { server, .. } => Some(("experimental/serverStatus", server)),
        LspEvent::Log { server, method, .. } => {
            method.as_deref().map(|method| (method, server.as_str()))
        }
        LspEvent::CapabilitiesChanged { server, method, .. } => Some((method, server)),
        LspEvent::InlayHintsRefresh { server } => Some(("workspace/inlayHint/refresh", server)),
        LspEvent::ApplyEditRequest { server, .. } => Some(("workspace/applyEdit", server)),
        LspEvent::Result { server, method, .. }
        | LspEvent::RequestFailed { server, method, .. } => Some((method, server)),
        _ => None,
    }
}

impl App {
    pub(in crate::app) fn route_lsp_event(&mut self, owned: OwnedLspEvent) {
        if !self.language.owns_event_context(&owned.context) {
            self.language.reject_reply(
                owned.reply,
                "requesting language-server session is no longer current".into(),
            );
            return;
        }
        let Some((method, server)) = method(&owned.event) else {
            self.handle_lsp_event(owned);
            return;
        };
        let Some(id) = self.language.incoming_id.checked_add(1) else {
            self.language
                .reject_reply(owned.reply, "incoming LSP event ids exhausted".into());
            self.show_error("incoming LSP event ids exhausted");
            return;
        };
        let invocation = CommandInvocation::new(&command_id(method), InvocationSource::Startup)
            .expect("registered LSP method command")
            .with_argument("event", CommandValue::Integer(id))
            .with_argument("method", CommandValue::String(method.into()))
            .with_argument("server", CommandValue::String(server.into()));
        self.language.incoming_id = id;
        self.language.incoming.insert(id, owned);
        self.invoke_checked(invocation);
        self.finish_incoming_lsp_commands();
    }

    pub(in crate::app) fn finish_incoming_lsp_commands(&mut self) {
        let finished: Vec<_> = self
            .language
            .incoming
            .keys()
            .copied()
            .filter(|id| !self.incoming_command_pending(*id))
            .collect();
        for id in finished {
            if let Some(owned) = self.language.incoming.remove(&id) {
                self.language.reject_reply(
                    owned.reply,
                    "incoming LSP handler did not apply the request".into(),
                );
            }
        }
    }

    pub(super) fn handle_incoming_lsp_command(&mut self, id: i64) {
        if let Some(owned) = self.language.incoming.remove(&id) {
            self.handle_lsp_event(owned);
        }
    }
}
