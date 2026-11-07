//! Resolved bindings, reverse lookups and key-hint descriptions.

use crate::app::configuration::configuration_value;
use crate::app::configuration::display_key_sequence;
use crate::app::configuration::invocation_fingerprint;
use crate::editor::CommandInvocation;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use crate::editor::input::InputContextLayer;
use crate::editor::input::KeyEvent;
use crate::editor::input::KeymapBindings;
use crate::editor::input::KeymapSequence;
use crate::editor::input::MappingResolution;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeymapOperation {
    Command,
    Unmap,
    Disable,
    PassThrough,
}

impl KeymapOperation {
    pub(in crate::app) const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Unmap => "unmap",
            Self::Disable => "disable",
            Self::PassThrough => "pass_through",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeymapInspection {
    pub(crate) id: String,
    pub(crate) context: String,
    pub(crate) contribution_layer: InputContextLayer,
    pub(crate) contribution_owner: Option<String>,
    pub(crate) plugin_load: u64,
    pub(crate) operation: KeymapOperation,
    pub(crate) command: Option<String>,
    pub(crate) arguments: BTreeMap<String, CommandValue>,
    pub(crate) source_keys: String,
    pub(crate) keys: String,
    pub(crate) desc: Option<String>,
    pub(crate) source: String,
}

#[derive(Clone)]
pub(in crate::app) struct ReverseBinding {
    pub(in crate::app) id: String,
    pub(in crate::app) keys: String,
    pub(in crate::app) layer: InputContextLayer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedKeyGroup {
    pub(crate) id: String,
    pub(crate) context: String,
    pub(crate) contribution_layer: InputContextLayer,
    pub(crate) contribution_owner: Option<String>,
    pub(crate) plugin_load: u64,
    pub(crate) source_prefix: String,
    pub(crate) prefix: String,
    pub(crate) label: String,
    pub(crate) icon: Option<String>,
    pub(crate) order: i32,
    pub(crate) source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyHintMapping {
    pub(crate) id: String,
    pub(crate) operation: KeymapOperation,
    pub(crate) command: Option<String>,
    pub(crate) source: String,
    pub(crate) remaining_keys: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyHintRow {
    pub(crate) next_key: String,
    pub(crate) label: String,
    pub(crate) direct: bool,
    pub(crate) group_id: Option<String>,
    pub(crate) icon: Option<String>,
    pub(crate) order: i32,
    pub(crate) mappings: Vec<KeyHintMapping>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyHintProjection {
    pub(crate) context: String,
    pub(crate) prefix: String,
    pub(crate) rows: Vec<KeyHintRow>,
}

#[derive(Clone, Default)]
pub(crate) struct ResolvedKeymaps {
    pub(super) bindings: HashMap<String, KeymapBindings>,
    pub(in crate::app) reverse: HashMap<(String, String), ReverseBinding>,
    pub(in crate::app) inspections: Vec<KeymapInspection>,
    pub(in crate::app) groups: Vec<ResolvedKeyGroup>,
    pub(in crate::app) conflicts: Vec<String>,
}

/// What a lesson may tell the user to press.
///
/// `Unbound` is a real answer, not an absence: a command the user has unmapped must be reported
/// as unpressable rather than shown with a key the user cannot use.
///
/// `Bound` carries EVERY key that resolves in that context, sorted, because the resolved table
/// stores bindings in a `HashMap` and a command such as `command_line.open` legitimately answers
/// to more than one key. Naming a single arbitrary key would let a lesson teach a different key
/// from run to run and would hide the others.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) enum EffectiveBinding {
    Bound { keys: Vec<String>, command: String },
    Unbound { command: String },
}

impl EffectiveBinding {
    #[must_use]
    pub(in crate::app) fn is_bound(&self) -> bool {
        matches!(self, Self::Bound { .. })
    }

    /// What a lesson displays for this binding.
    #[must_use]
    pub(in crate::app) fn display(&self) -> String {
        match self {
            Self::Bound { keys, command } => format!("{}  {command}", keys.join(" ")),
            Self::Unbound { command } => format!("unbound  {command}"),
        }
    }
}

impl ResolvedKeymaps {
    pub(in crate::app) fn layers(
        &self,
        context: &str,
    ) -> impl Iterator<Item = &crate::editor::input::Keymap> {
        self.bindings
            .get(context)
            .into_iter()
            .flat_map(KeymapBindings::highest_first)
    }

    pub(crate) fn resolve(
        &self,
        sequence: &mut KeymapSequence,
        context: &str,
        key: KeyEvent,
    ) -> MappingResolution {
        self.bindings
            .get(context)
            .map_or(MappingResolution::Missing, |bindings| {
                sequence.feed(bindings, key)
            })
    }

    /// The effective binding a lesson must show: the key the user would actually press in this
    /// context, or plainly unbound. It reads the SAME resolved table `binding_for` reads, so a
    /// lesson can never teach a key the user cannot press and a second table cannot drift.
    #[must_use]
    pub(in crate::app) fn effective_binding(
        &self,
        context: &str,
        command: &str,
    ) -> EffectiveBinding {
        let mut keys = self
            .inspections
            .iter()
            .filter(|inspection| {
                inspection.context == context
                    && inspection.command.as_deref() == Some(command)
                    && self.mapping_reachable(inspection)
            })
            .map(|inspection| inspection.keys.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            EffectiveBinding::Unbound {
                command: command.to_owned(),
            }
        } else {
            EffectiveBinding::Bound {
                keys,
                command: command.to_owned(),
            }
        }
    }

    /// The key to SHOW for a command in a context.
    ///
    /// Choose the smallest key so hash iteration order cannot change the displayed hint.
    /// Use `effective_binding` when every key matters.
    #[must_use]
    pub(crate) fn binding_for(&self, context: &str, command: &str) -> Option<&str> {
        self.reverse
            .iter()
            .filter(|((mapped_context, fingerprint), _)| {
                mapped_context == context
                    && fingerprint
                        .strip_prefix(command)
                        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('|'))
            })
            .map(|(_, binding)| binding.keys.as_str())
            .min()
    }

    #[must_use]
    pub(crate) fn inspect(&self, context: &str, command: &str) -> Option<KeymapInspection> {
        // A command can have several bindings; report the shortest so hints stay stable.
        self.reverse
            .iter()
            .filter(|((mapped_context, fingerprint), _)| {
                mapped_context == context
                    && fingerprint
                        .strip_prefix(command)
                        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('|'))
            })
            .map(|(_, binding)| binding)
            .min_by(|left, right| {
                left.keys
                    .len()
                    .cmp(&right.keys.len())
                    .then_with(|| left.keys.cmp(&right.keys))
            })
            .and_then(|binding| self.inspect_id(&binding.id))
    }

    #[must_use]
    pub(crate) fn inspect_id(&self, id: &str) -> Option<KeymapInspection> {
        self.inspections
            .iter()
            .find(|inspection| inspection.id == id)
            .cloned()
    }

    #[must_use]
    pub(crate) fn inspect_key(&self, context: &str, keys: &str) -> Option<KeymapInspection> {
        self.inspections
            .iter()
            .find(|inspection| {
                inspection.context == context
                    && (inspection.keys == keys || inspection.source_keys == keys)
            })
            .cloned()
    }

    fn mapping_reachable(&self, inspection: &KeymapInspection) -> bool {
        let Ok(keys) = inspection
            .keys
            .split_whitespace()
            .map(KeyEvent::from_str)
            .collect::<Result<Vec<_>, _>>()
        else {
            return false;
        };
        let mut sequence = KeymapSequence::default();
        let mut resolution = MappingResolution::Missing;
        for (index, key) in keys.iter().enumerate() {
            resolution = self.resolve(&mut sequence, &inspection.context, key.clone());
            if index + 1 < keys.len()
                && !matches!(
                    resolution,
                    MappingResolution::Pending | MappingResolution::Ambiguous
                )
            {
                return false;
            }
        }
        if resolution == MappingResolution::Ambiguous {
            resolution = sequence.flush_timeout();
        }
        matches!(resolution, MappingResolution::Ready(invocation)
            if Some(invocation.id.as_str()) == inspection.command.as_deref()
                && invocation.arguments == inspection.arguments)
    }

    /// Projects reachable continuations from the already-resolved mapping roster.
    /// This is read-only presentation data, never an alternate dispatch table.
    #[must_use]
    pub(crate) fn hints_for_prefix(
        &self,
        context: &str,
        effective_prefix: &str,
        command_labels: &BTreeMap<String, String>,
    ) -> Option<KeyHintProjection> {
        let prefix = effective_prefix
            .split_whitespace()
            .map(KeyEvent::from_str)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if prefix.is_empty() {
            return None;
        }
        let mut candidates = BTreeMap::<String, Vec<(Vec<KeyEvent>, &KeymapInspection)>>::new();
        for inspection in &self.inspections {
            if inspection.context != context
                || matches!(
                    inspection.operation,
                    KeymapOperation::Disable
                        | KeymapOperation::Unmap
                        | KeymapOperation::PassThrough
                )
            {
                continue;
            }
            let sequence = inspection
                .keys
                .split_whitespace()
                .map(KeyEvent::from_str)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            if sequence.len() <= prefix.len()
                || !sequence.starts_with(&prefix)
                || !self.mapping_reachable(inspection)
            {
                continue;
            }
            let next_key = sequence[prefix.len()].to_string();
            candidates
                .entry(next_key)
                .or_default()
                .push((sequence, inspection));
        }
        let mut rows = Vec::new();
        for (next_key, mut mappings) in candidates {
            mappings.sort_by(|(left_keys, left), (right_keys, right)| {
                left_keys
                    .len()
                    .cmp(&right_keys.len())
                    .then_with(|| left.id.cmp(&right.id))
            });
            let group = self
                .groups
                .iter()
                .filter(|group| group.context == context)
                .filter_map(|group| {
                    let sequence = group
                        .prefix
                        .split_whitespace()
                        .map(KeyEvent::from_str)
                        .collect::<Result<Vec<_>, _>>()
                        .ok()?;
                    (sequence.len() == prefix.len() + 1
                        && sequence.starts_with(&prefix)
                        && sequence[prefix.len()].to_string() == next_key)
                        .then_some((sequence, group))
                })
                .min_by(|(left_keys, left), (right_keys, right)| {
                    left_keys
                        .len()
                        .cmp(&right_keys.len())
                        .then_with(|| left.order.cmp(&right.order))
                        .then_with(|| left.id.cmp(&right.id))
                });
            let direct_mapping = mappings
                .iter()
                .find(|(keys, _)| keys.len() == prefix.len() + 1)
                .map(|(_, inspection)| *inspection);
            let direct = direct_mapping.unwrap_or(mappings[0].1);
            let (label, group_id, icon, order) = group.map_or_else(
                || {
                    let label = direct
                        .desc
                        .clone()
                        .or_else(|| {
                            direct
                                .command
                                .as_ref()
                                .and_then(|command| command_labels.get(command))
                                .cloned()
                        })
                        .or_else(|| direct.command.clone())
                        .unwrap_or_else(|| direct.id.clone());
                    (label, None, None, i32::MAX)
                },
                |(_, group)| {
                    (
                        group.label.clone(),
                        Some(group.id.clone()),
                        group.icon.clone(),
                        group.order,
                    )
                },
            );
            let mapping_rows = mappings
                .into_iter()
                .map(|(keys, inspection)| KeyHintMapping {
                    id: inspection.id.clone(),
                    operation: inspection.operation,
                    command: inspection.command.clone(),
                    source: inspection.source.clone(),
                    remaining_keys: display_key_sequence(&keys[prefix.len()..]),
                })
                .collect();
            rows.push(KeyHintRow {
                next_key,
                label,
                direct: direct_mapping.is_some(),
                group_id,
                icon,
                order,
                mappings: mapping_rows,
            });
        }
        rows.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.next_key.cmp(&right.next_key))
                .then_with(|| left.label.cmp(&right.label))
        });
        (!rows.is_empty()).then_some(KeyHintProjection {
            context: context.into(),
            prefix: display_key_sequence(&prefix),
            rows,
        })
    }

    pub(in crate::app) fn binding_for_action(
        &self,
        context: &str,
        command: &str,
        arguments: &std::collections::BTreeMap<String, toml::Value>,
    ) -> Option<&str> {
        let mut invocation = CommandInvocation::new(command, InvocationSource::Keymap).ok()?;
        for (name, value) in arguments {
            invocation
                .arguments
                .insert(name.clone(), configuration_value(value).ok()?);
        }
        self.reverse
            .get(&(context.to_owned(), invocation_fingerprint(&invocation)))
            .map(|binding| binding.keys.as_str())
    }
}
