//! Plugin keymaps and window rules validated against the accepted configuration.

use crate::app::commands::AppEffect;
use crate::app::configuration::Configuration;
use crate::app::configuration::ContributionCandidate;
use crate::app::configuration::ResolvedKeymaps;
use crate::app::configuration::compile_keymaps_with_plugin_catalog;
use crate::app::configuration::plugin_owner_from_id;
use crate::config::WindowPlacementConfig;
use crate::config::WindowsConfig;
use crate::editor::CommandRegistry;
use crate::plugins::PluginContributionCatalog;
use crate::plugins::PluginWindowPlacementKind;
use std::sync::Arc;

impl Configuration {
    fn compile_contributions(
        &self,
        commands: &CommandRegistry<AppEffect>,
        catalog: &PluginContributionCatalog,
        base: bool,
    ) -> Result<ResolvedKeymaps, String> {
        compile_keymaps_with_plugin_catalog(
            commands,
            if base {
                self.base_keymaps()
            } else {
                self.raw_keymaps()
            },
            catalog,
            &self.settings().input.keymaps,
        )
        .map_err(|errors| errors.join("; "))
    }

    pub(in crate::app) fn prepare_base_bindings(
        &self,
        commands: &CommandRegistry<AppEffect>,
        catalog: &PluginContributionCatalog,
        deferred: bool,
    ) -> Result<ContributionCandidate, String> {
        Ok(ContributionCandidate {
            stamp: self.stamp.clone(),
            catalog: catalog.version(),
            keymaps: Some(self.compile_contributions(commands, catalog, true)?),
            windows: self.settings().windows.clone(),
            deferred,
        })
    }

    pub(in crate::app) fn prepare_plugin_contributions(
        &self,
        commands: &CommandRegistry<AppEffect>,
        catalog: &PluginContributionCatalog,
    ) -> Result<ContributionCandidate, String> {
        crate::app::configuration::validation::validate_command_references(
            commands,
            self.settings(),
            self.dashboard(),
            &std::collections::BTreeSet::new(),
        )?;
        let keymaps = self.compile_contributions(commands, catalog, false)?;
        let windows = Self::resolve_plugin_window_rules(
            self.raw_windows(),
            self.expected_plugin_indexes(),
            catalog,
        )?;
        Ok(ContributionCandidate {
            stamp: self.stamp.clone(),
            catalog: catalog.version(),
            keymaps: Some(keymaps),
            windows,
            deferred: self.deferred_plugin_keymaps(),
        })
    }

    pub(in crate::app) fn prepare_builtin_rules(
        &self,
        catalog: &PluginContributionCatalog,
    ) -> ContributionCandidate {
        ContributionCandidate {
            stamp: self.stamp.clone(),
            catalog: catalog.version(),
            keymaps: None,
            windows: WindowsConfig {
                rules: self
                    .raw_windows()
                    .rules
                    .iter()
                    .filter(|rule| !rule.content_type.starts_with("plugin."))
                    .cloned()
                    .collect(),
            },
            deferred: self.deferred_plugin_keymaps(),
        }
    }

    pub(in crate::app) fn prepare_builtin_contributions(
        &self,
        commands: &CommandRegistry<AppEffect>,
        catalog: &PluginContributionCatalog,
        deferred: bool,
    ) -> Result<ContributionCandidate, String> {
        let mut candidate = self.prepare_builtin_rules(catalog);
        candidate.keymaps = Some(self.compile_contributions(commands, catalog, true)?);
        candidate.deferred = deferred;
        Ok(candidate)
    }

    pub(in crate::app) fn check_contributions(
        &self,
        candidate: &ContributionCandidate,
        catalog: &PluginContributionCatalog,
    ) -> Result<(), String> {
        self.check_stamp(&candidate.stamp)?;
        Self::check_catalog(&candidate.catalog, catalog)
    }

    pub(in crate::app) fn commit_contributions(
        &mut self,
        candidate: ContributionCandidate,
        catalog: &PluginContributionCatalog,
    ) -> Result<crate::app::configuration::AcceptedContributions, String> {
        self.check_contributions(&candidate, catalog)?;
        let accepted = crate::app::configuration::AcceptedContributions(candidate.window_source());
        if let Some(keymaps) = candidate.keymaps {
            self.accepted.keymaps = keymaps;
            self.accepted.deferred_plugin_keymaps = candidate.deferred;
            self.binding_revision = self.binding_revision.saturating_add(1);
        }
        self.accepted.settings.windows = candidate.windows;
        self.stamp = Arc::new(());
        Ok(accepted)
    }

    pub(in crate::app) fn resolve_plugin_window_rules(
        windows: &WindowsConfig,
        expected: &std::collections::BTreeSet<String>,
        catalog: &PluginContributionCatalog,
    ) -> Result<WindowsConfig, String> {
        for rule in &windows.rules {
            let Some(owner) = plugin_owner_from_id(&rule.content_type) else {
                continue;
            };
            if !expected.contains(owner) {
                return Err(format!(
                    "plugin window selector `{}` does not name an enabled plugin",
                    rule.content_type
                ));
            }
            let index = catalog.get(owner).ok_or_else(|| {
                format!(
                    "plugin window selector `{}` is not indexed",
                    rule.content_type
                )
            })?;
            let declaration = index
                .content_types
                .iter()
                .find(|content| content.id == rule.content_type)
                .ok_or_else(|| {
                    format!(
                        "plugin window selector `{}` was not statically declared",
                        rule.content_type
                    )
                })?;
            let requested = match &rule.placement {
                WindowPlacementConfig::Automatic => None,
                WindowPlacementConfig::Docked { .. } => Some(PluginWindowPlacementKind::Docked),
                WindowPlacementConfig::Floating { .. } => Some(PluginWindowPlacementKind::Floating),
                WindowPlacementConfig::Fullscreen => Some(PluginWindowPlacementKind::Fullscreen),
                WindowPlacementConfig::Hidden => Some(PluginWindowPlacementKind::Hidden),
            };
            if requested
                .is_some_and(|placement| !declaration.allowed_placements.contains(&placement))
            {
                return Err(format!(
                    "plugin window selector `{}` requests a disallowed placement",
                    rule.content_type
                ));
            }
        }
        Ok(windows.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::commands::register_app_commands;
    use crate::app::configuration::Configuration;
    use crate::editor::CommandRegistry;
    use crate::editor::register_builtin_commands;
    use crate::plugins::PluginContributionCatalog;

    #[test]
    fn policy_candidates_reject_foreign_and_changed_owners_without_publishing() {
        let mut commands = CommandRegistry::default();
        register_builtin_commands(&mut commands).unwrap();
        register_app_commands(&mut commands).unwrap();
        let (resolved, _) = crate::app::configuration::resolve_configuration(
            &commands,
            crate::config::load_bundled_config(),
            true,
        )
        .unwrap();
        let mut first = Configuration::new(resolved.clone(), false, None);
        let mut other = Configuration::new(resolved, false, None);
        let catalog = PluginContributionCatalog::default();
        let before = other.settings().windows.clone();
        let revision = other.binding_revision();
        assert!(
            other
                .commit_contributions(first.prepare_builtin_rules(&catalog), &catalog)
                .is_err()
        );
        assert_eq!(other.settings().windows, before);
        assert_eq!(other.binding_revision(), revision);
        let candidate = first
            .prepare_plugin_contributions(&commands, &catalog)
            .unwrap();
        let reload = first
            .prepare(crate::config::load_bundled_config(), &commands, &catalog)
            .unwrap();
        first.toggle_inlay_hints();
        assert!(first.commit_contributions(candidate, &catalog).is_err());
        assert!(first.stage_watch(&reload, &catalog).is_err());
        assert!(first.commit(reload, &catalog).is_err());
        let candidate = first
            .prepare_plugin_contributions(&commands, &catalog)
            .unwrap();
        let revision = first.binding_revision();
        first.commit_contributions(candidate, &catalog).unwrap();
        assert_eq!(first.binding_revision(), revision + 1);
    }
}
