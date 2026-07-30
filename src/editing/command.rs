use std::{collections::BTreeMap, fmt, num::NonZeroU64, sync::Arc};

use thiserror::Error;

use crate::editing::{BufferId, EditorState, EditorTransaction, ViewId};

const BUILTIN_NAMESPACES: &[&str] = &[
    "editor",
    "input",
    "startup",
    "session",
    "workspace",
    "file",
    "buffer",
    "view",
    "selection",
    "edit",
    "register",
    "mark",
    "macro",
    "jump",
    "search",
    "replace",
    "mode",
    "pane",
    "surface",
    "tab",
    "overlay",
    "panel",
    "picker",
    "explorer",
    "terminal",
    "job",
    "task",
    "syntax",
    "diagnostic",
    "completion",
    "lsp",
    "dap",
    "plugin",
    "package",
    "tools",
    "config",
    "keymap",
    "health",
    "history",
    "command_line",
    "command_palette",
    "recent",
    "recovery",
    "application",
    "crash",
    "format",
    "lint",
    "navigation",
    "snippet",
    "test",
];

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CommandId(String);

impl CommandId {
    pub fn new(value: impl Into<String>) -> Result<Self, CommandError> {
        let value = value.into();
        let valid = value.len() <= 128
            && value.split('.').count() >= 2
            && value.split('.').all(|segment| {
                !segment.is_empty()
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            });
        if !valid {
            return Err(CommandError::InvalidId(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn namespace(&self) -> &str {
        self.0
            .split('.')
            .next()
            .expect("validated command has namespace")
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandValue {
    String(String),
    Bool(bool),
    Integer(i64),
}

impl CommandValue {
    pub fn as_str(&self) -> Result<&str, CommandError> {
        match self {
            Self::String(value) => Ok(value),
            _ => Err(CommandError::InvalidArguments("expected string".into())),
        }
    }

    pub fn as_bool(&self) -> Result<bool, CommandError> {
        match self {
            Self::Bool(value) => Ok(*value),
            _ => Err(CommandError::InvalidArguments("expected boolean".into())),
        }
    }

    pub fn as_integer(&self) -> Result<i64, CommandError> {
        match self {
            Self::Integer(value) => Ok(*value),
            _ => Err(CommandError::InvalidArguments("expected integer".into())),
        }
    }
}

pub type CommandArguments = BTreeMap<String, CommandValue>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentType {
    String,
    Bool,
    Integer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArgumentField {
    pub kind: ArgumentType,
    pub required: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArgumentSchema {
    pub fields: BTreeMap<String, ArgumentField>,
}

impl ArgumentSchema {
    #[must_use]
    pub fn field(mut self, name: &str, kind: ArgumentType, required: bool) -> Self {
        self.fields
            .insert(name.to_owned(), ArgumentField { kind, required });
        self
    }

    fn validate(&self, arguments: &CommandArguments) -> Result<(), CommandError> {
        for (name, value) in arguments {
            let field = self.fields.get(name).ok_or_else(|| {
                CommandError::InvalidArguments(format!("unknown argument `{name}`"))
            })?;
            let valid = matches!(
                (field.kind, value),
                (ArgumentType::String, CommandValue::String(_))
                    | (ArgumentType::Bool, CommandValue::Bool(_))
                    | (ArgumentType::Integer, CommandValue::Integer(_))
            );
            if !valid {
                return Err(CommandError::InvalidArguments(format!(
                    "argument `{name}` has the wrong type"
                )));
            }
        }
        for (name, field) in &self.fields {
            if field.required && !arguments.contains_key(name) {
                return Err(CommandError::InvalidArguments(format!(
                    "missing required argument `{name}`"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextRequirement {
    #[default]
    Application,
    Workspace,
    View,
    Buffer,
    Selection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandSource {
    Builtin,
    Plugin(String),
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: String,
    pub description: String,
    pub arguments: ArgumentSchema,
    pub context: ContextRequirement,
    pub source: CommandSource,
}

impl CommandSpec {
    pub fn builtin(id: &str, title: &str) -> Result<Self, CommandError> {
        Ok(Self {
            id: CommandId::new(id)?,
            title: title.into(),
            description: String::new(),
            arguments: ArgumentSchema::default(),
            context: ContextRequirement::Application,
            source: CommandSource::Builtin,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CommandTarget {
    pub view: Option<ViewId>,
    pub buffer: Option<BufferId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationSource {
    Keymap,
    CommandLine,
    Macro,
    Plugin,
    Picker,
    Test,
    Startup,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandInvocation {
    pub id: CommandId,
    pub arguments: CommandArguments,
    pub source: InvocationSource,
    pub target: CommandTarget,
    pub count: Option<NonZeroU64>,
    pub register: Option<char>,
}

impl CommandInvocation {
    pub fn new(id: &str, source: InvocationSource) -> Result<Self, CommandError> {
        Ok(Self {
            id: CommandId::new(id)?,
            arguments: CommandArguments::new(),
            source,
            target: CommandTarget::default(),
            count: None,
            register: None,
        })
    }

    #[must_use]
    pub fn with_argument(mut self, name: &str, value: CommandValue) -> Self {
        self.arguments.insert(name.into(), value);
        self
    }
}

#[derive(Clone, Debug)]
pub struct CommandOutcome<E> {
    pub transaction: Option<EditorTransaction>,
    pub effects: Vec<E>,
    pub message: Option<String>,
    pub follow_up: Vec<CommandInvocation>,
}

impl<E> CommandOutcome<E> {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            transaction: None,
            effects: Vec::new(),
            message: None,
            follow_up: Vec::new(),
        }
    }
}

type Handler<E> = Arc<
    dyn Fn(&EditorState, &CommandInvocation) -> Result<CommandOutcome<E>, CommandError>
        + Send
        + Sync,
>;

struct RegisteredCommand<E> {
    spec: CommandSpec,
    handler: Handler<E>,
}

pub struct CommandRegistry<E> {
    commands: BTreeMap<CommandId, RegisteredCommand<E>>,
}

impl<E> Default for CommandRegistry<E> {
    fn default() -> Self {
        Self {
            commands: BTreeMap::new(),
        }
    }
}

impl<E> CommandRegistry<E> {
    pub fn register<F>(&mut self, spec: CommandSpec, handler: F) -> Result<(), CommandError>
    where
        F: Fn(&EditorState, &CommandInvocation) -> Result<CommandOutcome<E>, CommandError>
            + Send
            + Sync
            + 'static,
    {
        validate_ownership(&spec)?;
        if self.commands.contains_key(&spec.id) {
            return Err(CommandError::Duplicate(spec.id));
        }
        self.commands.insert(
            spec.id.clone(),
            RegisteredCommand {
                spec,
                handler: Arc::new(handler),
            },
        );
        Ok(())
    }

    pub fn execute(
        &self,
        state: &EditorState,
        invocation: &CommandInvocation,
    ) -> Result<CommandOutcome<E>, CommandError> {
        let command = self
            .commands
            .get(&invocation.id)
            .ok_or_else(|| CommandError::Unknown(invocation.id.clone()))?;
        command.spec.arguments.validate(&invocation.arguments)?;
        validate_context(state, invocation, command.spec.context)?;
        (command.handler)(state, invocation)
    }

    pub fn unregister_plugin(&mut self, owner: &str) {
        self.commands.retain(|_,registered|!matches!(&registered.spec.source,CommandSource::Plugin(candidate) if candidate==owner));
    }

    pub fn specs(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands.values().map(|registered| &registered.spec)
    }

    pub fn validate_invocation_shape(
        &self,
        invocation: &CommandInvocation,
    ) -> Result<(), CommandError> {
        let command = self
            .commands
            .get(&invocation.id)
            .ok_or_else(|| CommandError::Unknown(invocation.id.clone()))?;
        command.spec.arguments.validate(&invocation.arguments)
    }
}

fn validate_ownership(spec: &CommandSpec) -> Result<(), CommandError> {
    let namespace = spec.id.namespace();
    match &spec.source {
        CommandSource::Builtin => {
            const PLUGIN_HOST_COMMANDS: [&str; 3] =
                ["plugin.validate", "plugin.list", "plugin.disable"];
            if !BUILTIN_NAMESPACES.contains(&namespace)
                || (namespace == "plugin" && !PLUGIN_HOST_COMMANDS.contains(&spec.id.as_str()))
            {
                return Err(CommandError::ReservedNamespace(spec.id.clone()));
            }
        }
        CommandSource::Plugin(owner) => {
            let expected = format!("plugin.{owner}.");
            if !spec.id.as_str().starts_with(&expected) {
                return Err(CommandError::ReservedNamespace(spec.id.clone()));
            }
        }
    }
    Ok(())
}

fn validate_context(
    state: &EditorState,
    invocation: &CommandInvocation,
    requirement: ContextRequirement,
) -> Result<(), CommandError> {
    let view = invocation.target.view.or(state.active_view);
    let buffer = invocation.target.buffer.or_else(|| {
        view.and_then(|view| state.views.get(view))
            .map(|view| view.buffer)
    });
    let available = match requirement {
        ContextRequirement::Application => true,
        ContextRequirement::Workspace => !state.workspace_root.as_os_str().is_empty(),
        ContextRequirement::View => view.is_some_and(|view| state.views.contains_key(view)),
        ContextRequirement::Buffer => {
            buffer.is_some_and(|buffer| state.buffers.contains_key(buffer))
        }
        ContextRequirement::Selection => view
            .and_then(|view| state.views.get(view))
            .is_some_and(|view| view.selection.is_some()),
    };
    if available {
        Ok(())
    } else {
        Err(CommandError::Unavailable(format!(
            "requires {requirement:?} context"
        )))
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("invalid command ID `{0}`")]
    InvalidId(String),
    #[error("command `{0}` is already registered")]
    Duplicate(CommandId),
    #[error("command `{0}` uses a reserved namespace")]
    ReservedNamespace(CommandId),
    #[error("unknown command `{0}`")]
    Unknown(CommandId),
    #[error("invalid command arguments: {0}")]
    InvalidArguments(String),
    #[error("command is unavailable: {0}")]
    Unavailable(String),
    #[error("command failed: {0}")]
    Failed(String),
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CommandError, CommandInvocation, CommandOutcome, CommandRegistry, CommandSource,
        CommandSpec, ContextRequirement, InvocationSource,
    };
    use crate::editing::EditorState;

    #[test]
    fn rejects_duplicates_and_plugin_namespace_theft() {
        let mut registry = CommandRegistry::<()>::default();
        let spec = CommandSpec::builtin("editor.quit", "Quit").expect("spec");
        registry
            .register(spec.clone(), |_, _| Ok(CommandOutcome::empty()))
            .expect("first registration");
        assert!(matches!(
            registry.register(spec, |_, _| Ok(CommandOutcome::empty())),
            Err(CommandError::Duplicate(_))
        ));

        let mut stolen = CommandSpec::builtin("editor.fake", "Fake").expect("shape");
        stolen.source = CommandSource::Plugin("evil.pkg".into());
        assert!(matches!(
            registry.register(stolen, |_, _| Ok(CommandOutcome::empty())),
            Err(CommandError::ReservedNamespace(_))
        ));
    }

    #[test]
    fn enforces_required_context() {
        let mut registry = CommandRegistry::<()>::default();
        let mut spec = CommandSpec::builtin("buffer.close", "Close").expect("spec");
        spec.context = ContextRequirement::Buffer;
        registry
            .register(spec, |_, _| Ok(CommandOutcome::empty()))
            .expect("register");
        let state = EditorState::new(PathBuf::from("/tmp"));
        let invocation =
            CommandInvocation::new("buffer.close", InvocationSource::Test).expect("invocation");
        assert!(matches!(
            registry.execute(&state, &invocation),
            Err(CommandError::Unavailable(_))
        ));
    }
}
