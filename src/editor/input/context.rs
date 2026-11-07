//! Input contexts and their inherited keymap layers.

use std::collections::BTreeMap;

const MAXIMUM_CONTEXTS_PER_PLUGIN: usize = 64;
const MAXIMUM_DYNAMIC_CONTEXTS: usize = 1_024;
const PARENT_SURFACES: &[&str] = &["panel", "picker", "explorer", "terminal"];

const BUILTIN_SURFACES: &[&str] = &[
    "dashboard",
    "command_prompt",
    "search_prompt",
    "workspace_trust_prompt",
    "confirmation_prompt",
    "completion",
    "snippet",
    "documentation",
    "picker",
    "picker_normal",
    "theme_picker",
    "workspace_edit",
    "explorer",
    "explorer_search",
    "panel",
    "agent",
    "agent_normal",
    "terminal",
    "terminal_normal",
    "terminal_visual",
    "focus",
    "editor_mouse",
    "explorer_mouse",
    "terminal_mouse",
    "dashboard_mouse",
    "prompt_mouse",
    "tool_manager",
    "tool_manager_search",
    "tool_manager_mouse",
    "settings",
    "setup",
    "settings_search",
    "settings_edit",
    "settings_choice",
    "picker_mouse",
    "key_release",
    "register",
    "prefix",
    "replace",
    "character",
    "macro_record",
    "macro_replay",
];
const EDITOR_MODES: &[&str] = &[
    "normal",
    "insert",
    "replace",
    "visual_char",
    "visual_line",
    "visual_block",
    "multi",
    "operator_pending",
];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InputContextLayer {
    Bundled,
    Plugin,
    User,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputContextDescriptor {
    pub id: String,
    pub title: String,
    pub parent_surface: Option<String>,
    pub owner: Option<String>,
    pub load_id: u64,
    pub source: String,
    pub layer: InputContextLayer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputContextError {
    InvalidId(String),
    InvalidOwner(String),
    InvalidTitle,
    Unknown(String),
    ModeNotAllowed(String, String),
    Duplicate(String),
    InvalidParent(String),
    PluginLimit(String),
    GlobalLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputContextPolicy {
    pub maximum_contexts_per_plugin: usize,
    pub maximum_dynamic_contexts: usize,
}

impl Default for InputContextPolicy {
    fn default() -> Self {
        Self {
            maximum_contexts_per_plugin: MAXIMUM_CONTEXTS_PER_PLUGIN,
            maximum_dynamic_contexts: MAXIMUM_DYNAMIC_CONTEXTS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct InputContexts {
    contexts: BTreeMap<String, InputContextDescriptor>,
    policy: InputContextPolicy,
}

impl Default for InputContexts {
    fn default() -> Self {
        Self::bundled()
    }
}

impl InputContexts {
    #[must_use]
    pub fn bundled() -> Self {
        Self::bundled_with_policy(InputContextPolicy::default())
    }

    #[must_use]
    pub fn bundled_with_policy(policy: InputContextPolicy) -> Self {
        let mut contexts = BTreeMap::new();
        for id in BUILTIN_SURFACES {
            contexts.insert(
                (*id).into(),
                InputContextDescriptor {
                    id: (*id).into(),
                    title: id.replace('_', " "),
                    parent_surface: None,
                    owner: None,
                    load_id: 0,
                    source: "<bundled:input-contexts>".into(),
                    layer: InputContextLayer::Bundled,
                },
            );
        }
        for mode in EDITOR_MODES {
            let id = format!("editor.{mode}");
            contexts.insert(
                id.clone(),
                InputContextDescriptor {
                    id,
                    title: format!("editor {mode}"),
                    parent_surface: Some("editor".into()),
                    owner: None,
                    load_id: 0,
                    source: "<bundled:input-contexts>".into(),
                    layer: InputContextLayer::Bundled,
                },
            );
        }
        Self { contexts, policy }
    }

    pub fn resolve(&self, context: &str, mode: &str) -> Result<String, InputContextError> {
        let canonical = if context == "editor" {
            let mode = match mode {
                "visual" => "visual_char",
                "operator" => "operator_pending",
                mode => mode,
            };
            let candidate = format!("editor.{mode}");
            if !self.contexts.contains_key(&candidate) {
                return Err(InputContextError::Unknown(candidate));
            }
            candidate
        } else {
            if !mode.is_empty() {
                return Err(InputContextError::ModeNotAllowed(
                    context.into(),
                    mode.into(),
                ));
            }
            context.into()
        };
        self.contexts
            .contains_key(&canonical)
            .then_some(canonical.clone())
            .ok_or(InputContextError::Unknown(canonical))
    }

    pub fn register_plugin(
        &mut self,
        owner: &str,
        local_id: &str,
        title: &str,
        parent_surface: Option<String>,
        load_id: u64,
        source: String,
    ) -> Result<String, InputContextError> {
        if !valid_segment(owner, 64) {
            return Err(InputContextError::InvalidOwner(owner.into()));
        }
        if !valid_segment(local_id, 64) {
            return Err(InputContextError::InvalidId(local_id.into()));
        }
        if title.trim().is_empty()
            || title.len() > 256
            || !(1..=128).contains(&unicode_width::UnicodeWidthStr::width(title))
            || title.chars().any(char::is_control)
        {
            return Err(InputContextError::InvalidTitle);
        }
        let id = format!("plugin.{owner}.{local_id}");
        if id.len() > 128 {
            return Err(InputContextError::InvalidId(id));
        }
        if self.contexts.contains_key(&id) {
            return Err(InputContextError::Duplicate(id));
        }
        if self
            .contexts
            .values()
            .filter(|context| context.owner.as_deref() == Some(owner))
            .count()
            >= self.policy.maximum_contexts_per_plugin
        {
            return Err(InputContextError::PluginLimit(owner.into()));
        }
        if self
            .contexts
            .values()
            .filter(|context| context.layer == InputContextLayer::Plugin)
            .count()
            >= self.policy.maximum_dynamic_contexts
        {
            return Err(InputContextError::GlobalLimit);
        }
        if let Some(parent) = &parent_surface
            && !PARENT_SURFACES.contains(&parent.as_str())
        {
            return Err(InputContextError::InvalidParent(parent.clone()));
        }
        self.contexts.insert(
            id.clone(),
            InputContextDescriptor {
                id: id.clone(),
                title: title.into(),
                parent_surface,
                owner: Some(owner.into()),
                load_id,
                source,
                layer: InputContextLayer::Plugin,
            },
        );
        Ok(id)
    }

    pub fn unregister_plugin_load(&mut self, owner: &str, load_id: u64) {
        self.contexts.retain(|_, context| {
            context.owner.as_deref() != Some(owner) || context.load_id != load_id
        });
    }

    #[must_use]
    pub fn descriptors(&self) -> impl ExactSizeIterator<Item = &InputContextDescriptor> {
        self.contexts.values()
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<InputContextDescriptor> {
        self.contexts.values().cloned().collect()
    }
}

fn valid_segment(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

impl std::fmt::Display for InputContextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId(id) => write!(formatter, "invalid input context id `{id}`"),
            Self::InvalidOwner(owner) => write!(formatter, "invalid input context owner `{owner}`"),
            Self::InvalidTitle => write!(formatter, "input context title is invalid"),
            Self::Unknown(id) => write!(formatter, "unknown context `{id}`"),
            Self::ModeNotAllowed(context, mode) => {
                write!(
                    formatter,
                    "context `{context}` does not accept mode `{mode}`"
                )
            }
            Self::Duplicate(id) => write!(formatter, "duplicate input context `{id}`"),
            Self::InvalidParent(parent) => {
                write!(formatter, "invalid input context parent `{parent}`")
            }
            Self::PluginLimit(owner) => {
                write!(formatter, "plugin `{owner}` exceeds its context limit")
            }
            Self::GlobalLimit => write!(formatter, "global plugin context limit reached"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::input::context::*;

    #[test]
    fn bundled_registry_preserves_all_contexts_aliases_and_modes() {
        let registry = InputContexts::bundled();
        for surface in BUILTIN_SURFACES {
            assert_eq!(registry.resolve(surface, "").as_deref(), Ok(*surface));
        }
        for mode in EDITOR_MODES {
            assert_eq!(
                registry.resolve("editor", mode),
                Ok(format!("editor.{mode}"))
            );
        }
        assert_eq!(
            registry.resolve("editor", "visual").as_deref(),
            Ok("editor.visual_char")
        );
        assert_eq!(
            registry.resolve("editor", "operator").as_deref(),
            Ok("editor.operator_pending")
        );
    }

    #[test]
    fn plugin_context_limits_and_parent_kinds_are_injectable_and_deterministic() {
        let mut registry = InputContexts::bundled_with_policy(InputContextPolicy {
            maximum_contexts_per_plugin: 2,
            maximum_dynamic_contexts: 3,
        });
        registry
            .register_plugin("demo", "b", "B", Some("panel".into()), 1, "m".into())
            .expect("b");
        registry
            .register_plugin("demo", "a", "A", None, 2, "m".into())
            .expect("a");
        assert!(matches!(
            registry.register_plugin("demo", "c", "C", None, 3, "m".into()),
            Err(InputContextError::PluginLimit(_))
        ));
        registry
            .register_plugin("other", "a", "A", None, 1, "m".into())
            .expect("global final");
        assert!(matches!(
            registry.register_plugin("third", "a", "A", None, 1, "m".into()),
            Err(InputContextError::GlobalLimit)
        ));
        assert!(matches!(
            InputContexts::bundled().register_plugin(
                "demo",
                "x",
                "X",
                Some("dashboard".into()),
                1,
                "m".into()
            ),
            Err(InputContextError::InvalidParent(_))
        ));
        let ids = registry
            .snapshot()
            .into_iter()
            .map(|context| context.id)
            .collect::<Vec<_>>();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn plugin_contexts_are_namespaced_bounded_and_load_conditional() {
        let mut registry = InputContexts::bundled();
        let id = registry
            .register_plugin(
                "demo",
                "chat",
                "Chat",
                Some("panel".into()),
                7,
                "manifest.toml".into(),
            )
            .expect("context");
        assert_eq!(id, "plugin.demo.chat");
        assert_eq!(registry.resolve(&id, "").as_deref(), Ok(id.as_str()));
        assert!(matches!(
            registry.register_plugin("BAD", "x", "X", None, 1, "manifest".into()),
            Err(InputContextError::InvalidOwner(_))
        ));
        assert!(matches!(
            registry.register_plugin("demo", "chat", "Chat", None, 8, "manifest".into()),
            Err(InputContextError::Duplicate(_))
        ));
        registry.unregister_plugin_load("demo", 6);
        assert!(registry.resolve(&id, "").is_ok());
        registry.unregister_plugin_load("demo", 7);
        assert!(registry.resolve(&id, "").is_err());
    }
}
