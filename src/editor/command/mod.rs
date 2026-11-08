//! Command declarations and dispatch.

mod builtins;
mod wrappers;

pub use wrappers::{CommandWrapperAction, PendingCommandId};

pub use builtins::register_builtin_commands;
pub(crate) use builtins::visual_ranges;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use std::fmt;

use std::num::NonZeroU64;

use std::sync::Arc;

use thiserror::Error;

use crate::editor::BufferId;

use crate::editor::EditorState;

use crate::editor::EditorTransaction;

use crate::editor::ViewId;

const BUILTIN_NAMESPACES: &[&str] = &[
    "editor",
    "input",
    "startup",
    "session",
    "workspace",
    "agent",
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
    "multi",
    "pane",
    "surface",
    "dashboard",
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
    "settings",
    "theme",
    "keymap",
    "health",
    "history",
    "command_line",
    "command_palette",
    "recent",
    "recovery",
    "application",
    "window",
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
    Number(serde_json::Number),
    List(Vec<CommandValue>),
    Record(CommandArguments),
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

    pub fn as_list(&self) -> Result<&[CommandValue], CommandError> {
        match self {
            Self::List(value) => Ok(value),
            _ => Err(CommandError::InvalidArguments("expected list".into())),
        }
    }

    pub fn as_record(&self) -> Result<&CommandArguments, CommandError> {
        match self {
            Self::Record(value) => Ok(value),
            _ => Err(CommandError::InvalidArguments("expected record".into())),
        }
    }

    pub fn as_number(&self) -> Result<serde_json::Number, CommandError> {
        match self {
            Self::Number(value) => Ok(value.clone()),
            Self::Integer(value) => Ok((*value).into()),
            _ => Err(CommandError::InvalidArguments("expected number".into())),
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
    Number,
    List,
    Record,
}

impl ArgumentType {
    fn accepts(self, value: &CommandValue) -> bool {
        matches!(
            (self, value),
            (Self::String, CommandValue::String(_))
                | (Self::Bool, CommandValue::Bool(_))
                | (Self::Integer | Self::Number, CommandValue::Integer(_))
                | (Self::Number, CommandValue::Number(_))
                | (Self::List, CommandValue::List(_))
                | (Self::Record, CommandValue::Record(_))
        )
    }
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
            let valid = field.kind.accepts(value);
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

/// Access is checked before running either a wrapper or the command handler.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CommandAccess {
    Edit,
    Ui,
    Files,
    Process,
    Network,
    HostOnly,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CommandAuthority {
    #[default]
    Host,
    Plugin {
        initiator: String,
        grants: BTreeSet<CommandAccess>,
    },
}

impl CommandAuthority {
    pub(crate) fn require(&self, access: CommandAccess) -> Result<(), CommandError> {
        if let Self::Plugin { initiator, grants } = self
            && (access == CommandAccess::HostOnly || !grants.contains(&access))
        {
            return Err(CommandError::EffectDenied {
                initiator: initiator.clone(),
                access,
            });
        }
        Ok(())
    }

    fn check(&self, command: &CommandSpec) -> Result<(), CommandError> {
        if let Self::Plugin { grants, .. } = self
            && (command.access.contains(&CommandAccess::HostOnly)
                || !command.access.is_subset(grants))
        {
            return Err(CommandError::AccessDenied(command.id.clone()));
        }
        Ok(())
    }

    pub(crate) fn restrict(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Host, _) => other.clone(),
            (_, Self::Host) => self.clone(),
            (Self::Plugin { initiator, grants }, Self::Plugin { grants: other, .. }) => {
                Self::Plugin {
                    initiator: initiator.clone(),
                    grants: grants.intersection(other).copied().collect(),
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: String,
    pub description: String,
    pub arguments: ArgumentSchema,
    pub result: Option<ArgumentType>,
    pub access: BTreeSet<CommandAccess>,
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
            result: None,
            access: BTreeSet::from([CommandAccess::HostOnly]),
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
    Repeat,
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
    pub value: Option<CommandValue>,
    pub pending: Option<PendingCommandId>,
    pub authority: CommandAuthority,
}

impl<E> CommandOutcome<E> {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            transaction: None,
            effects: Vec::new(),
            message: None,
            follow_up: Vec::new(),
            value: None,
            pending: None,
            authority: CommandAuthority::Host,
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
    wrappers: Vec<Arc<wrappers::CommandWrapper<E>>>,
}

pub struct CommandRegistry<E> {
    commands: BTreeMap<CommandId, RegisteredCommand<E>>,
    pending: RefCell<wrappers::PendingCommands<E>>,
}

impl<E> Default for CommandRegistry<E> {
    fn default() -> Self {
        Self {
            commands: BTreeMap::new(),
            pending: RefCell::new(wrappers::PendingCommands::default()),
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
                wrappers: Vec::new(),
            },
        );
        Ok(())
    }

    pub(crate) fn set_access(
        &mut self,
        id: &CommandId,
        access: BTreeSet<CommandAccess>,
    ) -> Result<(), CommandError> {
        self.commands
            .get_mut(id)
            .ok_or_else(|| CommandError::Unknown(id.clone()))?
            .spec
            .access = access;
        Ok(())
    }

    pub fn check_access(
        &self,
        id: &CommandId,
        authority: &CommandAuthority,
    ) -> Result<(), CommandError> {
        let command = self
            .commands
            .get(id)
            .ok_or_else(|| CommandError::Unknown(id.clone()))?;
        authority.check(&command.spec)
    }

    pub fn execute(
        &self,
        state: &EditorState,
        invocation: &CommandInvocation,
    ) -> Result<CommandOutcome<E>, CommandError> {
        let authority = if invocation.source == InvocationSource::Plugin {
            CommandAuthority::Plugin {
                initiator: String::new(),
                grants: BTreeSet::new(),
            }
        } else {
            CommandAuthority::Host
        };
        self.execute_with_authority(state, invocation, authority)
    }

    pub fn unregister_plugin(&mut self, owner: &str) {
        self.cancel_plugin_commands(owner);
        self.commands.retain(|_, registered| {
            registered.wrappers.retain(|wrapper| wrapper.owner != owner);
            !matches!(&registered.spec.source, CommandSource::Plugin(candidate) if candidate == owner)
        });
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

/// Whether this requirement is satisfied right now, using the dispatcher's own check so help
/// cannot advertise a command the dispatcher would refuse.
impl ContextRequirement {
    #[must_use]
    pub fn satisfied(&self, state: &EditorState, invocation: &CommandInvocation) -> bool {
        validate_context(state, invocation, *self).is_ok()
    }
}
fn validate_context(
    state: &EditorState,
    invocation: &CommandInvocation,
    requirement: ContextRequirement,
) -> Result<(), CommandError> {
    let view = invocation.target.view.or(state.active_view());
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
    #[error("access to command `{0}` was refused")]
    AccessDenied(CommandId),
    #[error("plugin `{initiator}` lacks {access:?} permission for this operation")]
    EffectDenied {
        initiator: String,
        access: CommandAccess,
    },
    #[error("unknown or completed pending command")]
    UnknownPending,
    #[error("only the waiting wrapper can continue this command")]
    WrongWrapper,
    #[error("call-next may only be used once, before the wrapped command returns")]
    AlreadyContinued,
    #[error("a command may have at most 16 wrappers")]
    TooManyWrappers,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::editor::command::CommandError;

    use crate::editor::CommandInvocation;

    use crate::editor::command::CommandOutcome;

    use crate::editor::command::CommandRegistry;

    use crate::editor::command::CommandSource;

    use crate::editor::command::CommandSpec;

    use crate::editor::command::ContextRequirement;

    use crate::editor::EditorState;
    use crate::editor::command::InvocationSource;

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
