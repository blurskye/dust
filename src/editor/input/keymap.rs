//! Key sequence matching and layered bindings.

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
            (Some(Binding::Disabled), empty) => Lookup::Disabled { prefix: !empty },
            (Some(Binding::PassThrough), empty) => Lookup::PassThrough { prefix: !empty },
            (Some(Binding::Command(command)), true) => Lookup::Exact(command.clone()),
            (Some(Binding::Command(command)), false) => Lookup::Ambiguous(command.clone()),
            (None, false) => Lookup::Prefix,
            (None, true) => Lookup::Missing,
        }
    }
}

enum Lookup {
    Missing,
    Disabled { prefix: bool },
    PassThrough { prefix: bool },
    Prefix,
    Exact(CommandInvocation),
    Ambiguous(CommandInvocation),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MappingResolution {
    Missing,
    Disabled,
    DisabledAndReplay(Vec<KeyEvent>),
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

    pub(crate) fn highest_first(&self) -> impl Iterator<Item = &Keymap> {
        self.layers.values().rev()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct KeymapSequence {
    pending: Vec<KeyEvent>,
    // None as the command means that this exact sequence is disabled.
    pending_exact: Option<(Option<CommandInvocation>, usize)>,
}

impl KeymapSequence {
    pub(crate) fn feed(&mut self, bindings: &KeymapBindings, key: KeyEvent) -> MappingResolution {
        self.feed_layers(bindings.highest_first(), key)
    }

    pub(crate) fn feed_layers<'a>(
        &mut self,
        layers: impl IntoIterator<Item = &'a Keymap>,
        key: KeyEvent,
    ) -> MappingResolution {
        self.pending.push(key);
        let mut prefix = false;
        let mut disabled = false;
        for keymap in layers {
            match keymap.lookup(&self.pending) {
                Lookup::Disabled {
                    prefix: continuation,
                } => {
                    disabled = true;
                    prefix |= continuation;
                }
                Lookup::PassThrough {
                    prefix: continuation,
                } => prefix |= continuation,
                Lookup::Exact(_) if disabled => {}
                Lookup::Ambiguous(_) if disabled => prefix = true,
                Lookup::Exact(command) if !prefix => {
                    self.cancel();
                    return MappingResolution::Ready(command);
                }
                Lookup::Exact(command) | Lookup::Ambiguous(command) => {
                    self.pending_exact = Some((Some(command), self.pending.len()));
                    return MappingResolution::Ambiguous;
                }
                Lookup::Prefix => prefix = true,
                Lookup::Missing => {}
            }
        }
        if disabled {
            self.pending_exact = Some((None, self.pending.len()));
        }
        if prefix {
            return if self.pending_exact.is_some() {
                MappingResolution::Ambiguous
            } else {
                MappingResolution::Pending
            };
        }
        if self.pending_exact.is_some() {
            return self.flush_timeout();
        }
        let mut rejected = std::mem::take(&mut self.pending);
        let last = rejected.pop().into_iter().collect::<Vec<_>>();
        if rejected.is_empty() {
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
        let mut pending = std::mem::take(&mut self.pending);
        match self.pending_exact.take() {
            Some((Some(command), consumed)) if consumed < pending.len() => {
                MappingResolution::ReadyAndReplay(command, pending.split_off(consumed))
            }
            Some((Some(command), _)) => MappingResolution::Ready(command),
            Some((None, consumed)) if consumed < pending.len() => {
                MappingResolution::DisabledAndReplay(pending.split_off(consumed))
            }
            Some((None, _)) => MappingResolution::Disabled,
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
    temporary: Vec<Keymap>,
}

impl KeymapResolver {
    pub fn layer_mut(&mut self, layer: KeymapLayer) -> &mut Keymap {
        self.bindings.layer_mut(layer)
    }
    /// Changing layers cancels an unfinished sequence rather than interpreting it twice.
    pub fn push_layer(&mut self, layer: Keymap) {
        self.sequence.cancel();
        self.temporary.push(layer);
    }

    pub fn pop_layer(&mut self) -> Option<Keymap> {
        self.sequence.cancel();
        self.temporary.pop()
    }

    pub fn feed(&mut self, key: KeyEvent) -> MappingResolution {
        self.sequence.feed_layers(
            self.temporary
                .iter()
                .rev()
                .chain(self.bindings.highest_first()),
            key,
        )
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
    use crate::editor::CommandInvocation;
    use crate::editor::InvocationSource;

    use crate::editor::input::keymap::KeymapLayer;

    use crate::editor::input::keymap::KeymapResolver;

    use crate::editor::input::KeyEvent;
    use crate::editor::input::keymap::MappingResolution;

    #[test]
    fn temporary_layers_override_and_pop_back_to_the_previous_layer() {
        let mut resolver = KeymapResolver::default();
        let base = CommandInvocation::new("base.key", InvocationSource::Test).unwrap();
        let first = CommandInvocation::new("first.key", InvocationSource::Test).unwrap();
        let second = CommandInvocation::new("second.key", InvocationSource::Test).unwrap();
        let key = KeyEvent::character('x');
        resolver
            .layer_mut(KeymapLayer::Mode)
            .insert(std::slice::from_ref(&key), base.clone());
        for command in [&first, &second] {
            let mut layer = super::Keymap::default();
            layer.insert(std::slice::from_ref(&key), command.clone());
            resolver.push_layer(layer);
            assert_eq!(
                resolver.feed(key.clone()),
                MappingResolution::Ready(command.clone())
            );
        }
        assert!(resolver.pop_layer().is_some());
        assert_eq!(resolver.feed(key.clone()), MappingResolution::Ready(first));
        assert!(resolver.pop_layer().is_some());
        assert_eq!(resolver.feed(key), MappingResolution::Ready(base));
        assert!(resolver.pop_layer().is_none());
    }

    #[test]
    fn popping_a_layer_cancels_its_prefix_and_keeps_lower_bindings() {
        let mut resolver = KeymapResolver::default();
        let command = CommandInvocation::new("base.key", InvocationSource::Test).unwrap();
        let g = KeyEvent::character('g');
        resolver
            .layer_mut(KeymapLayer::Mode)
            .insert(std::slice::from_ref(&g), command.clone());
        let mut layer = super::Keymap::default();
        layer.insert(&[g.clone(), g.clone()], command.clone());
        resolver.push_layer(layer);
        assert_eq!(resolver.feed(g.clone()), MappingResolution::Ambiguous);
        resolver.pop_layer();
        assert!(resolver.pending_sequence().is_empty());
        assert_eq!(resolver.feed(g), MappingResolution::Ready(command));
    }

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
    fn disabled_exact_preserves_longer_mappings_across_layers() {
        for longer_layer in [KeymapLayer::Global, KeymapLayer::Overlay] {
            let mut resolver = KeymapResolver::default();
            let command = CommandInvocation::new("health.open", InvocationSource::Test).unwrap();
            let keys = [KeyEvent::character('g'), KeyEvent::character('c')];
            resolver
                .layer_mut(KeymapLayer::Global)
                .insert(&keys, command.clone());
            resolver
                .layer_mut(KeymapLayer::Overlay)
                .insert_disabled(&keys);
            resolver.layer_mut(longer_layer).insert(
                &[keys[0].clone(), keys[1].clone(), KeyEvent::character('c')],
                command.clone(),
            );
            assert_eq!(resolver.feed(keys[0].clone()), MappingResolution::Pending);
            assert_eq!(resolver.feed(keys[1].clone()), MappingResolution::Ambiguous);
            assert_eq!(
                resolver.feed(KeyEvent::character('c')),
                MappingResolution::Ready(command)
            );
            resolver.feed(keys[0].clone());
            assert_eq!(resolver.feed(keys[1].clone()), MappingResolution::Ambiguous);
            assert_eq!(resolver.flush_timeout(), MappingResolution::Disabled);
        }
    }

    #[test]
    fn disabled_prefix_suppresses_text_and_shorter_fallbacks_but_replays_suffixes() {
        for timeout in [false, true] {
            let mut resolver = KeymapResolver::default();
            let command = CommandInvocation::new("health.open", InvocationSource::Test).unwrap();
            let g = KeyEvent::character('g');
            let c = KeyEvent::character('c');
            resolver
                .layer_mut(KeymapLayer::Global)
                .insert(std::slice::from_ref(&g), command.clone());
            resolver
                .layer_mut(KeymapLayer::Overlay)
                .insert_disabled(&[g.clone(), c.clone()]);
            resolver
                .layer_mut(KeymapLayer::Global)
                .insert(&[g.clone(), c.clone(), c.clone(), c.clone()], command);
            assert_eq!(resolver.feed(g), MappingResolution::Ambiguous);
            assert_eq!(resolver.feed(c.clone()), MappingResolution::Ambiguous);
            assert_eq!(resolver.feed(c.clone()), MappingResolution::Ambiguous);
            let result = if timeout {
                resolver.flush_timeout()
            } else {
                resolver.feed(KeyEvent::character('x'))
            };
            let suffix = if timeout {
                vec![c]
            } else {
                vec![c, KeyEvent::character('x')]
            };
            assert_eq!(result, MappingResolution::DisabledAndReplay(suffix));
        }
    }

    #[test]
    fn pass_through_preserves_its_continuation_and_lower_timeout_fallback() {
        for timeout in [false, true] {
            let mut resolver = KeymapResolver::default();
            let short = CommandInvocation::new("editor.quit", InvocationSource::Test).unwrap();
            let long = CommandInvocation::new("health.open", InvocationSource::Test).unwrap();
            let g = KeyEvent::character('g');
            resolver
                .layer_mut(KeymapLayer::Global)
                .insert(std::slice::from_ref(&g), short.clone());
            resolver
                .layer_mut(KeymapLayer::Overlay)
                .insert_pass_through(std::slice::from_ref(&g));
            resolver
                .layer_mut(KeymapLayer::Overlay)
                .insert(&[g.clone(), g.clone()], long.clone());
            assert_eq!(resolver.feed(g.clone()), MappingResolution::Ambiguous);
            if timeout {
                assert_eq!(resolver.flush_timeout(), MappingResolution::Ready(short));
            } else {
                assert_eq!(resolver.feed(g), MappingResolution::Ready(long));
            }
        }
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
            MappingResolution::Replay(
                vec![KeyEvent::character('g')],
                vec![KeyEvent::character('x')]
            )
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
    #[test]
    fn shorter_mapping_survives_longer_prefixes_and_replays_the_suffix() {
        for timeout in [false, true] {
            let mut resolver = KeymapResolver::default();
            let short = CommandInvocation::new("editor.quit", InvocationSource::Test).unwrap();
            let long = CommandInvocation::new("health.open", InvocationSource::Test).unwrap();
            resolver
                .layer_mut(KeymapLayer::Mode)
                .insert(&[KeyEvent::character('g')], short.clone());
            resolver.layer_mut(KeymapLayer::Mode).insert(
                &[
                    KeyEvent::character('g'),
                    KeyEvent::character('h'),
                    KeyEvent::character('i'),
                ],
                long,
            );
            assert_eq!(
                resolver.feed(KeyEvent::character('g')),
                MappingResolution::Ambiguous
            );
            assert_eq!(
                resolver.feed(KeyEvent::character('h')),
                MappingResolution::Ambiguous
            );
            let result = if timeout {
                resolver.flush_timeout()
            } else {
                resolver.feed(KeyEvent::character('x'))
            };
            let suffix = if timeout {
                vec![KeyEvent::character('h')]
            } else {
                vec![KeyEvent::character('h'), KeyEvent::character('x')]
            };
            assert_eq!(result, MappingResolution::ReadyAndReplay(short, suffix));
        }
    }

    #[test]
    fn higher_layer_prefix_keeps_a_lower_exact_mapping_as_timeout_fallback() {
        let mut resolver = KeymapResolver::default();
        let short = CommandInvocation::new("editor.quit", InvocationSource::Test).unwrap();
        resolver
            .layer_mut(KeymapLayer::Global)
            .insert(&[KeyEvent::character('j')], short.clone());
        resolver.layer_mut(KeymapLayer::Mode).insert(
            &[KeyEvent::character('j'), KeyEvent::character('k')],
            short.clone(),
        );
        assert_eq!(
            resolver.feed(KeyEvent::character('j')),
            MappingResolution::Ambiguous
        );
        assert_eq!(resolver.flush_timeout(), MappingResolution::Ready(short));
    }
}
