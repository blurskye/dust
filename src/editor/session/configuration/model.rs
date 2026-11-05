use super::{
    BTreeMap, BTreeSet, CommandInvocation, CommandValue, FromStr, HashMap, InputContextLayer,
    InvocationSource, KeyEvent, KeymapBindings, KeymapSequence, MappingResolution, PathBuf,
    configuration_value, display_key_sequence, invocation_fingerprint,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeymapOperation {
    Command,
    Unmap,
    Disable,
    PassThrough,
}

impl KeymapOperation {
    pub(in crate::editor::session) const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Unmap => "unmap",
            Self::Disable => "disable",
            Self::PassThrough => "pass_through",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeymapInspection {
    pub id: String,
    pub context: String,
    pub contribution_layer: InputContextLayer,
    pub contribution_owner: Option<String>,
    pub contribution_generation: u64,
    pub operation: KeymapOperation,
    pub command: Option<String>,
    pub arguments: BTreeMap<String, CommandValue>,
    pub source_keys: String,
    pub keys: String,
    pub desc: Option<String>,
    pub source: String,
}

#[derive(Clone)]
pub(in crate::editor::session) struct ReverseBinding {
    pub(in crate::editor::session) id: String,
    pub(in crate::editor::session) keys: String,
    pub(in crate::editor::session) layer: InputContextLayer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedKeyGroup {
    pub id: String,
    pub context: String,
    pub contribution_layer: InputContextLayer,
    pub contribution_owner: Option<String>,
    pub contribution_generation: u64,
    pub source_prefix: String,
    pub prefix: String,
    pub label: String,
    pub icon: Option<String>,
    pub order: i32,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyHintMapping {
    pub id: String,
    pub operation: KeymapOperation,
    pub command: Option<String>,
    pub source: String,
    pub remaining_keys: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyHintRow {
    pub next_key: String,
    pub label: String,
    pub direct: bool,
    pub group_id: Option<String>,
    pub icon: Option<String>,
    pub order: i32,
    pub mappings: Vec<KeyHintMapping>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyHintProjection {
    pub context: String,
    pub prefix: String,
    pub rows: Vec<KeyHintRow>,
}

#[derive(Clone, Default)]
pub struct ResolvedKeymaps {
    pub(super) bindings: HashMap<String, KeymapBindings>,
    pub(in crate::editor::session) reverse: HashMap<(String, String), ReverseBinding>,
    pub(in crate::editor::session) inspections: Vec<KeymapInspection>,
    pub(in crate::editor::session) groups: Vec<ResolvedKeyGroup>,
    pub(in crate::editor::session) conflicts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationSources {
    pub settings: PathBuf,
    pub keymaps: PathBuf,
    pub dashboard: PathBuf,
    pub theme: PathBuf,
}

#[derive(Clone)]
pub struct ResolvedConfiguration {
    pub settings: crate::config::Config,
    pub dashboard: crate::config::DashboardConfig,
    pub theme: crate::config::ThemeConfig,
    pub raw_keymaps: crate::config::LoadedKeymaps,
    pub base_keymaps: crate::config::LoadedKeymaps,
    pub raw_windows: crate::config::WindowsConfig,
    pub deferred_plugin_keymaps: bool,
    pub expected_plugin_indexes: BTreeSet<String>,
    pub keymaps: ResolvedKeymaps,
    pub sources: ConfigurationSources,
    pub user_setting_keys: BTreeSet<String>,
    pub user_setting_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub dashboard_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub theme_spans: BTreeMap<String, std::ops::Range<usize>>,
}

pub(in crate::editor::session) fn resolved_toml_value(
    mut value: toml::Value,
    segments: &[String],
) -> Option<toml::Value> {
    for segment in segments {
        value = if let Some(table) = value.as_table() {
            table.get(segment)?.clone()
        } else {
            value
                .as_array()?
                .get(segment.parse::<usize>().ok()?)?
                .clone()
        };
    }
    Some(value)
}

impl ResolvedConfiguration {
    #[must_use]
    pub fn explain(&self, key: &str) -> Option<crate::config::ConfigExplanation> {
        let (value, source, span, layered) = if let Some(key) = key.strip_prefix("theme.") {
            let segments = crate::config::config_key_segments(key)?;
            let canonical = segments.join(".");
            let value = resolved_toml_value(toml::Value::try_from(&self.theme).ok()?, &segments)?;
            (
                value,
                &self.sources.theme,
                self.theme_spans.get(&canonical),
                false,
            )
        } else if let Some(key) = key.strip_prefix("dashboard.") {
            let segments = crate::config::config_key_segments(key)?;
            let canonical = segments.join(".");
            let value =
                resolved_toml_value(toml::Value::try_from(&self.dashboard).ok()?, &segments)?;
            (
                value,
                &self.sources.dashboard,
                self.dashboard_spans.get(&canonical),
                false,
            )
        } else {
            let segments = crate::config::config_key_segments(key)?;
            let value =
                resolved_toml_value(toml::Value::try_from(&self.settings).ok()?, &segments)?;
            let user_key = (1..=segments.len())
                .rev()
                .map(|length| segments[..length].join("."))
                .find(|ancestor| self.user_setting_keys.contains(ancestor));
            let source = if user_key.is_some() {
                &self.sources.settings
            } else {
                // Inherited values continue to identify the bundled declaration.
                static BUNDLED_SETTINGS: std::sync::LazyLock<PathBuf> =
                    std::sync::LazyLock::new(|| PathBuf::from("<bundled:config>"));
                &BUNDLED_SETTINGS
            };
            let span = user_key
                .as_ref()
                .and_then(|key| self.user_setting_spans.get(key));
            (value, source, span, true)
        };
        let mut final_source = source.display().to_string();
        if let Some(span) = span {
            final_source = format!("{final_source}:bytes {}..{}", span.start, span.end);
        }
        let bundled = final_source.starts_with("<bundled:");
        Some(crate::config::ConfigExplanation {
            key: key.into(),
            value: value.to_string(),
            source: if bundled {
                "bundled defaults".into()
            } else {
                final_source.clone()
            },
            contributors: if bundled {
                vec!["bundled defaults".into()]
            } else if layered {
                vec!["bundled defaults".into(), final_source]
            } else {
                vec![final_source]
            },
        })
    }
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
pub(in crate::editor::session) enum EffectiveBinding {
    Bound { keys: Vec<String>, command: String },
    Unbound { command: String },
}

impl EffectiveBinding {
    /// Every key that resolves here, in a stable order. Test-facing: the reference page shows
    /// `display`, and asserting the whole set is what makes the ordering checkable.
    #[cfg(test)]
    #[must_use]
    pub(in crate::editor::session) fn keys(&self) -> &[String] {
        match self {
            Self::Bound { keys, .. } => keys,
            Self::Unbound { .. } => &[],
        }
    }

    /// The first resolvable key, for prose that must name one. Stable, because `keys` is sorted.
    #[cfg(test)]
    #[must_use]
    pub(in crate::editor::session) fn primary(&self) -> Option<&str> {
        self.keys().first().map(String::as_str)
    }

    #[must_use]
    pub(in crate::editor::session) fn is_bound(&self) -> bool {
        matches!(self, Self::Bound { .. })
    }

    /// What a lesson displays for this binding.
    #[must_use]
    pub(in crate::editor::session) fn display(&self) -> String {
        match self {
            Self::Bound { keys, command } => format!("{}  {command}", keys.join(" ")),
            Self::Unbound { command } => format!("unbound  {command}"),
        }
    }
}

impl ResolvedKeymaps {
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

    #[cfg(test)]
    pub(in crate::editor::session) fn has_bindings(&self) -> bool {
        !self.bindings.is_empty()
    }

    /// The effective binding a lesson must show: the key the user would actually press in this
    /// context, or plainly unbound. It reads the SAME resolved table `binding_for` reads, so a
    /// lesson can never teach a key the user cannot press and a second table cannot drift.
    #[must_use]
    pub(in crate::editor::session) fn effective_binding(
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
    /// `reverse` is a `HashMap`, so a command with several bindings previously answered with
    /// whichever key the iteration happened to reach first — meaning shipped hint text could
    /// name a different key between runs. The smallest key is chosen so the answer is stable and
    /// agrees with `EffectiveBinding::primary()`. Use `effective_binding` when every key matters.
    #[must_use]
    pub fn binding_for(&self, context: &str, command: &str) -> Option<&str> {
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
    pub fn inspect(&self, context: &str, command: &str) -> Option<KeymapInspection> {
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
    pub fn inspect_id(&self, id: &str) -> Option<KeymapInspection> {
        self.inspections
            .iter()
            .find(|inspection| inspection.id == id)
            .cloned()
    }

    #[must_use]
    pub fn inspect_key(&self, context: &str, keys: &str) -> Option<KeymapInspection> {
        self.inspections
            .iter()
            .find(|inspection| {
                inspection.context == context
                    && (inspection.keys == keys || inspection.source_keys == keys)
            })
            .cloned()
    }

    #[must_use]
    pub fn conflicts(&self) -> &[String] {
        &self.conflicts
    }

    #[must_use]
    pub fn groups(&self) -> &[ResolvedKeyGroup] {
        &self.groups
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
    pub fn hints_for_prefix(
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

    pub(in crate::editor::session) fn binding_for_action(
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
