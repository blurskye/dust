use std::collections::BTreeMap;

use crate::editor::CommandInvocation;

use crate::editor::input::KeyEvent;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum KeymapLayer {
    Global,
    PluginDefaults,
    Mode,
    Workspace,
    Buffer,
    Panel,
    Overlay,
}

#[derive(Clone, Debug, Default)]
struct TrieNode {
    binding: Option<Binding>,
    children: BTreeMap<KeyEvent, TrieNode>,
}

#[derive(Clone, Debug)]
enum Binding {
    Command(CommandInvocation),
    Disabled,
    PassThrough,
}

#[derive(Clone, Debug, Default)]
pub struct Keymap {
    root: TrieNode,
}

impl Keymap {
    pub fn insert(&mut self, sequence: &[KeyEvent], command: CommandInvocation) -> bool {
        let mut node = &mut self.root;
        for key in sequence {
            node = node.children.entry(key.clone()).or_default();
        }
        node.binding.replace(Binding::Command(command)).is_none()
    }

    pub fn insert_disabled(&mut self, sequence: &[KeyEvent]) -> bool {
        let mut node = &mut self.root;
        for key in sequence {
            node = node.children.entry(key.clone()).or_default();
        }
        node.binding.replace(Binding::Disabled).is_none()
    }

    pub fn insert_pass_through(&mut self, sequence: &[KeyEvent]) -> bool {
        let mut node = &mut self.root;
        for key in sequence {
            node = node.children.entry(key.clone()).or_default();
        }
        node.binding.replace(Binding::PassThrough).is_none()
    }

    fn lookup(&self, sequence: &[KeyEvent]) -> Lookup {
        let mut node = &self.root;
        for key in sequence {
            let Some(next) = node.children.get(key) else {
                return Lookup::Missing;
            };
            node = next;
        }
        match (&node.binding, node.children.is_empty()) {
            (Some(Binding::Disabled), _) => Lookup::Disabled,
            (Some(Binding::PassThrough), _) => Lookup::PassThrough,
            (Some(Binding::Command(command)), true) => Lookup::Exact(command.clone()),
            (Some(Binding::Command(command)), false) => Lookup::Ambiguous(command.clone()),
            (None, false) => Lookup::Prefix,
            (None, true) => Lookup::Missing,
        }
    }
}

enum Lookup {
    Missing,
    Disabled,
    PassThrough,
    Prefix,
    Exact(CommandInvocation),
    Ambiguous(CommandInvocation),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MappingResolution {
    Missing,
    Disabled,
    Pending,
    Ready(CommandInvocation),
    ReadyAndReplay(CommandInvocation, Vec<KeyEvent>),
    Ambiguous,
    /// No binding finished the sequence: the keys that started it, then the key that ended
    /// it (none after a timeout).
    Replay(Vec<KeyEvent>, Vec<KeyEvent>),
}

/// Compiled bindings contain no live key-sequence state.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeymapBindings {
    layers: BTreeMap<KeymapLayer, Keymap>,
}

impl KeymapBindings {
    pub(crate) fn layer_mut(&mut self, layer: KeymapLayer) -> &mut Keymap {
        self.layers.entry(layer).or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct KeymapSequence {
    pending: Vec<KeyEvent>,
    pending_exact: Option<CommandInvocation>,
}

impl KeymapSequence {
    pub(crate) fn feed(&mut self, bindings: &KeymapBindings, key: KeyEvent) -> MappingResolution {
        let fallback = self.pending_exact.take();
        self.pending.push(key);
        for (_, keymap) in bindings.layers.iter().rev() {
            match keymap.lookup(&self.pending) {
                Lookup::Disabled => {
                    self.pending.clear();
                    return MappingResolution::Disabled;
                }
                Lookup::PassThrough => continue,
                Lookup::Exact(command) => {
                    self.pending.clear();
                    return MappingResolution::Ready(command);
                }
                Lookup::Ambiguous(command) => {
                    self.pending_exact = Some(command);
                    return MappingResolution::Ambiguous;
                }
                Lookup::Prefix => return MappingResolution::Pending,
                Lookup::Missing => {}
            }
        }
        let mut rejected = std::mem::take(&mut self.pending);
        let last = rejected.pop().into_iter().collect::<Vec<_>>();
        if let Some(command) = fallback {
            MappingResolution::ReadyAndReplay(command, last)
        } else if rejected.is_empty() {
            MappingResolution::Missing
        } else {
            MappingResolution::Replay(rejected, last)
        }
    }

    #[must_use]
    pub fn pending_sequence(&self) -> &[KeyEvent] {
        &self.pending
    }

    pub fn flush_timeout(&mut self) -> MappingResolution {
        let pending = std::mem::take(&mut self.pending);
        match self.pending_exact.take() {
            Some(command) => MappingResolution::Ready(command),
            None if pending.is_empty() => MappingResolution::Missing,
            None => MappingResolution::Replay(pending, Vec::new()),
        }
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
        self.pending_exact = None;
    }
}

/// Standalone compatibility API; live sessions keep bindings and sequence apart.
#[derive(Clone, Debug, Default)]
pub struct KeymapResolver {
    bindings: KeymapBindings,
    sequence: KeymapSequence,
}

impl KeymapResolver {
    pub fn layer_mut(&mut self, layer: KeymapLayer) -> &mut Keymap {
        self.bindings.layer_mut(layer)
    }
    pub fn feed(&mut self, key: KeyEvent) -> MappingResolution {
        self.sequence.feed(&self.bindings, key)
    }
    #[must_use]
    pub fn pending_sequence(&self) -> &[KeyEvent] {
        self.sequence.pending_sequence()
    }
    pub fn flush_timeout(&mut self) -> MappingResolution {
        self.sequence.flush_timeout()
    }
    pub fn cancel(&mut self) {
        self.sequence.cancel();
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::{CommandInvocation, InvocationSource};

    use super::{KeymapLayer, KeymapResolver, MappingResolution};
    use crate::editor::input::KeyEvent;

    #[test]
    fn exact_prefix_waits_then_flushes() {
        let mut resolver = KeymapResolver::default();
        let short = CommandInvocation::new("editor.quit", InvocationSource::Keymap).expect("id");
        let long = CommandInvocation::new("health.open", InvocationSource::Keymap).expect("id");
        resolver
            .layer_mut(KeymapLayer::Mode)
            .insert(&[KeyEvent::character('g')], short);
        resolver
            .layer_mut(KeymapLayer::Mode)
            .insert(&[KeyEvent::character('g'), KeyEvent::character('g')], long);
        assert_eq!(
            resolver.feed(KeyEvent::character('g')),
            MappingResolution::Ambiguous
        );
        assert!(matches!(
            resolver.flush_timeout(),
            MappingResolution::Ready(command) if command.id.as_str() == "editor.quit"
        ));
    }

    #[test]
    fn unmap_suppresses_compiled_fallback() {
        let mut resolver = KeymapResolver::default();
        resolver
            .layer_mut(KeymapLayer::Mode)
            .insert_disabled(&[KeyEvent::character('x')]);
        assert_eq!(
            resolver.feed(KeyEvent::character('x')),
            MappingResolution::Disabled
        );
    }

    #[test]
    fn pass_through_reaches_lower_layer_and_failed_prefix_replays_last_key() {
        let mut resolver = KeymapResolver::default();
        let lower = CommandInvocation::new("editor.quit", InvocationSource::Keymap).expect("id");
        resolver
            .layer_mut(KeymapLayer::Global)
            .insert(&[KeyEvent::character('x')], lower);
        resolver
            .layer_mut(KeymapLayer::Overlay)
            .insert_pass_through(&[KeyEvent::character('x')]);
        assert!(matches!(
            resolver.feed(KeyEvent::character('x')),
            MappingResolution::Ready(command) if command.id.as_str() == "editor.quit"
        ));

        let mut prefix = KeymapResolver::default();
        let command = CommandInvocation::new("health.open", InvocationSource::Keymap).expect("id");
        prefix.layer_mut(KeymapLayer::Mode).insert(
            &[KeyEvent::character('g'), KeyEvent::character('g')],
            command,
        );
        assert_eq!(
            prefix.feed(KeyEvent::character('g')),
            MappingResolution::Pending
        );
        assert_eq!(
            prefix.feed(KeyEvent::character('x')),
            MappingResolution::Replay(vec![KeyEvent::character('g')], vec![KeyEvent::character('x')])
        );
    }

    #[test]
    fn higher_layer_wins() {
        let mut resolver = KeymapResolver::default();
        let global = CommandInvocation::new("editor.quit", InvocationSource::Keymap).expect("id");
        let overlay = CommandInvocation::new("health.open", InvocationSource::Keymap).expect("id");
        resolver
            .layer_mut(KeymapLayer::Global)
            .insert(&[KeyEvent::character('x')], global);
        resolver
            .layer_mut(KeymapLayer::Overlay)
            .insert(&[KeyEvent::character('x')], overlay);
        assert!(matches!(
            resolver.feed(KeyEvent::character('x')),
            MappingResolution::Ready(command) if command.id.as_str() == "health.open"
        ));
    }
}
