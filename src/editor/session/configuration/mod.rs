use crate::config::{
    ConfigurationWorker, ConfigurationWorkerError, ConfigurationWorkerResult, LineNumbers,
    LoadedConfig,
};
use crate::editor::input::{
    InputContextLayer, InputContextRegistry, KeyEvent, KeymapBindings, KeymapLayer, KeymapSequence,
    MappingResolution,
};
use crate::editor::{
    AppEffect, CommandError, CommandInvocation, CommandRegistry, CommandValue, InvocationSource,
    register_app_commands, register_builtin_commands,
};
use crate::extensions::PluginContributionCatalog;
use crate::ui::DashboardAction;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod model;
/// The enum name is needed by tests and by prose; production reaches it through the method it
/// returns from, so the alias is gated rather than left to warn as an unused import.
#[cfg(test)]
pub(in crate::editor::session) use model::EffectiveBinding;
pub(in crate::editor::session) use model::ReverseBinding;
pub use model::{
    ConfigurationSources, KeymapInspection, KeymapOperation, ResolvedConfiguration,
    ResolvedKeyGroup, ResolvedKeymaps,
};
mod resolve;
pub use resolve::validate_loaded_configuration;
pub(in crate::editor::session) use resolve::{
    configuration_generation_is_newer, configured_byte_offset_at_display_column,
    configured_grapheme_width, configured_text_width, dashboard_actions, resolve_configuration,
};
mod validation;
pub(in crate::editor::session) use validation::{
    configuration_value, invocation_fingerprint, keymap_entries, validate_configured_commands,
};
mod compile;
pub(in crate::editor::session) use compile::{
    KeymapCompilation, compile_keymap_entries, display_key_sequence, literal_key_sequence,
};
pub use compile::{
    compile_keymaps, compile_keymaps_with_plugin_catalog, compile_keymaps_with_plugin_index,
    reload_keymaps,
};
mod contributions;
mod themes;
pub(in crate::editor::session) use themes::{ThemeKey, ThemeRows};

fn configured_context(
    contexts: &InputContextRegistry,
    context: &str,
    mode: &str,
) -> Result<String, String> {
    contexts
        .resolve(context, mode)
        .map_err(|error| error.to_string())
}

/// Accepted policy and reload admission. No caller receives a mutable policy view.
pub(super) struct ConfigurationSession {
    accepted: ResolvedConfiguration,
    /// Settings come from the user's config.toml rather than only the bundled defaults.
    user_configuration: bool,
    themes: themes::Themes,
    stamp: Arc<()>,
    worker: ConfigurationWorker,
    generation: u64,
    binding_revision: u64,
    /// Why the last reload was refused over its keymaps, which are then not in effect.
    rejected_keymaps: Option<String>,
    /// The line numbers that `<leader>ul` hid.
    hidden_line_numbers: Option<LineNumbers>,
    zen: Option<ZenRestore>,
}

/// LazyVim's `<leader>u` toggles. Each flips an accepted setting until the next reload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewToggle {
    Wrap,
    LineNumbers,
    RelativeNumbers,
    IndentGuides,
    GitSigns,
    FormatOnSave,
}

/// What zen mode turned off, put back when it ends.
struct ZenRestore {
    line_numbers: LineNumbers,
    git_signs: bool,
    indent_guides: bool,
    explorer_open: bool,
}

/// A fully resolved candidate; only this module can construct or modify it.
pub(in crate::editor::session) struct ConfigurationCandidate {
    resolved: ResolvedConfiguration,
    stamp: Arc<()>,
    catalog: crate::extensions::PluginCatalogVersion,
}

pub(in crate::editor::session) struct ContributionCandidate {
    stamp: Arc<()>,
    catalog: crate::extensions::PluginCatalogVersion,
    keymaps: Option<ResolvedKeymaps>,
    windows: crate::config::WindowsConfig,
    deferred: bool,
}
pub(in crate::editor::session) struct WindowRuleSource {
    stamp: Arc<()>,
    catalog: crate::extensions::PluginCatalogVersion,
    windows: crate::config::WindowsConfig,
}
pub(in crate::editor::session) struct ContributionReceipt(WindowRuleSource);
impl WindowRuleSource {
    pub(in crate::editor::session) fn accepts(&self, receipt: &ContributionReceipt) -> bool {
        Arc::ptr_eq(&self.stamp, &receipt.0.stamp)
            && self.catalog == receipt.0.catalog
            && self.windows == receipt.0.windows
    }
}
impl ContributionCandidate {
    pub(in crate::editor::session) fn window_source(&self) -> WindowRuleSource {
        WindowRuleSource {
            stamp: self.stamp.clone(),
            catalog: self.catalog.clone(),
            windows: self.windows.clone(),
        }
    }
    pub(in crate::editor::session) fn windows(&self) -> &crate::config::WindowsConfig {
        &self.windows
    }
}
impl ConfigurationCandidate {
    pub(in crate::editor::session) fn window_contribution(&self) -> ContributionCandidate {
        ContributionCandidate {
            stamp: self.stamp.clone(),
            catalog: self.catalog.clone(),
            keymaps: None,
            windows: self.resolved.settings.windows.clone(),
            deferred: self.resolved.deferred_plugin_keymaps,
        }
    }
    pub(in crate::editor::session) fn view(&self) -> &ResolvedConfiguration {
        &self.resolved
    }
}

impl ConfigurationSession {
    pub(super) fn new(accepted: ResolvedConfiguration, user_configuration: bool) -> Self {
        let worker =
            ConfigurationWorker::start(user_configuration, accepted.settings.ui.theme.clone());
        Self {
            accepted,
            user_configuration,
            themes: themes::Themes::default(),
            stamp: Arc::new(()),
            worker,
            generation: 0,
            binding_revision: 0,
            rejected_keymaps: None,
            hidden_line_numbers: None,
            zen: None,
        }
    }
    pub(super) fn settings(&self) -> &crate::config::Config {
        &self.accepted.settings
    }
    /// The config.toml the Settings page writes, or `None` when Dust runs on the bundled
    /// defaults only.
    pub(super) fn user_settings_path(&self) -> Option<PathBuf> {
        self.user_configuration
            .then(crate::config::user_config_path)
    }
    /// Keys the user's config.toml sets.
    pub(super) fn user_setting_keys(&self) -> &BTreeSet<String> {
        &self.accepted.user_setting_keys
    }
    pub(super) fn dashboard(&self) -> &crate::config::DashboardConfig {
        &self.accepted.dashboard
    }
    pub(super) fn theme(&self) -> &crate::config::ThemeConfig {
        self.effective_theme()
    }
    pub(super) fn note_rejected_keymaps(&mut self, diagnostic: Option<String>) {
        self.rejected_keymaps = diagnostic;
    }
    pub(super) fn rejected_keymaps(&self) -> Option<&str> {
        self.rejected_keymaps.as_deref()
    }
    pub(super) fn keymaps(&self) -> &ResolvedKeymaps {
        &self.accepted.keymaps
    }
    pub(super) fn raw_keymaps(&self) -> &crate::config::LoadedKeymaps {
        &self.accepted.raw_keymaps
    }
    pub(super) fn base_keymaps(&self) -> &crate::config::LoadedKeymaps {
        &self.accepted.base_keymaps
    }
    pub(super) fn raw_windows(&self) -> &crate::config::WindowsConfig {
        &self.accepted.raw_windows
    }
    pub(super) fn expected_plugin_indexes(&self) -> &BTreeSet<String> {
        &self.accepted.expected_plugin_indexes
    }
    pub(super) fn deferred_plugin_keymaps(&self) -> bool {
        self.accepted.deferred_plugin_keymaps
    }
    pub(super) fn complete_plugin_indexes(&mut self) {
        self.accepted.deferred_plugin_keymaps = false;
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn binding_revision_ref(&self) -> &u64 {
        &self.binding_revision
    }
    pub(super) fn binding_revision(&self) -> u64 {
        self.binding_revision
    }
    pub(super) fn explain(&self, key: &str) -> Option<crate::config::ConfigExplanation> {
        self.explain_theme(key)
            .or_else(|| self.accepted.explain(key))
    }
    pub(super) fn dashboard_actions(&self) -> Vec<DashboardAction> {
        dashboard_actions(&self.accepted)
    }
    /// The command of the `index`th shown dashboard item, as its key would run it.
    pub(super) fn dashboard_invocation(&self, index: usize) -> Option<CommandInvocation> {
        let action = self
            .accepted
            .dashboard
            .action
            .iter()
            .filter(|action| action.visible)
            .nth(index)?;
        let mut invocation =
            CommandInvocation::new(&action.command, InvocationSource::Keymap).ok()?;
        for (name, value) in &action.arguments {
            invocation
                .arguments
                .insert(name.clone(), configuration_value(value).ok()?);
        }
        Some(invocation)
    }
    pub(super) fn request_reload(&self) -> Result<(), ConfigurationWorkerError> {
        self.worker.request_reload()
    }
    pub(super) fn next_result(
        &mut self,
    ) -> Result<Option<ConfigurationWorkerResult>, ConfigurationWorkerError> {
        let Some(result) = self.worker.try_result()? else {
            return Ok(None);
        };
        if !configuration_generation_is_newer(self.generation, result.generation) {
            return Ok(None);
        }
        self.generation = result.generation;
        // Preserve cancellation on observed reload generations, including rejected reloads.
        self.binding_revision = self.binding_revision.saturating_add(1);
        Ok(Some(result))
    }
    pub(super) fn retain_watch(&self) {
        let _ = self
            .worker
            .configure_watch(self.accepted.settings.ui.theme.clone());
    }
    pub(super) fn prepare(
        &self,
        loaded: LoadedConfig,
        commands: &CommandRegistry<AppEffect>,
        catalog: &PluginContributionCatalog,
    ) -> Result<ConfigurationCandidate, String> {
        loaded.config.validate()?;
        let mut resolved = match resolve_configuration(commands, loaded, true)? {
            (resolved, None) => resolved,
            (_, Some(error)) => return Err(error),
        };
        if resolved.settings.plugins != self.accepted.settings.plugins {
            return Err(
                "live plugin topology reload is not yet staged; previous configuration retained"
                    .into(),
            );
        }
        resolved.settings.windows = Self::resolve_plugin_window_rules(
            &resolved.raw_windows,
            &resolved.expected_plugin_indexes,
            catalog,
        )?;
        resolved.keymaps = compile_keymaps_with_plugin_catalog(
            commands,
            &resolved.raw_keymaps,
            catalog,
            &resolved.settings.input.keymaps,
        )
        .map_err(|errors| errors.join("; "))?;
        Ok(ConfigurationCandidate {
            resolved,
            catalog: catalog.version(),
            stamp: self.stamp.clone(),
        })
    }
    pub(super) fn stage_watch(
        &self,
        candidate: &ConfigurationCandidate,
        catalog: &PluginContributionCatalog,
    ) -> Result<(), String> {
        self.check_stamp(&candidate.stamp)?;
        Self::check_catalog(&candidate.catalog, catalog)?;
        self.worker
            .configure_watch(candidate.resolved.settings.ui.theme.clone())
            .map_err(|error| error.to_string())
    }
    fn check_catalog(
        expected: &crate::extensions::PluginCatalogVersion,
        current: &PluginContributionCatalog,
    ) -> Result<(), String> {
        if *expected == current.version() {
            Ok(())
        } else {
            Err("configuration candidate catalogue revision is stale".into())
        }
    }
    fn check_stamp(&self, stamp: &Arc<()>) -> Result<(), String> {
        if Arc::ptr_eq(&self.stamp, stamp) {
            Ok(())
        } else {
            Err("configuration candidate is stale or belongs to another session".into())
        }
    }
    pub(super) fn commit(
        &mut self,
        candidate: ConfigurationCandidate,
        catalog: &PluginContributionCatalog,
    ) -> Result<ContributionReceipt, String> {
        self.check_stamp(&candidate.stamp)?;
        Self::check_catalog(&candidate.catalog, catalog)?;
        // A theme picked in the theme picker stays while config.toml keeps the same theme.
        let keep_theme = candidate.resolved.settings.ui.theme == self.accepted.settings.ui.theme;
        let receipt = ContributionReceipt(candidate.window_contribution().window_source());
        self.accepted = candidate.resolved;
        if let Some(zen) = &mut self.zen {
            let editor = &mut self.accepted.settings.editor;
            zen.line_numbers = editor.line_numbers;
            zen.git_signs = editor.git_signs.enabled;
            zen.indent_guides = editor.indent_guides.enabled;
            editor.line_numbers = LineNumbers::Off;
            editor.git_signs.enabled = false;
            editor.indent_guides.enabled = false;
        }
        self.hidden_line_numbers = None;
        self.themes.reset(keep_theme);
        self.stamp = Arc::new(());
        self.binding_revision = self.binding_revision.saturating_add(1);
        Ok(receipt)
    }
    #[cfg(test)]
    pub(super) fn install_bindings(&mut self, keymaps: ResolvedKeymaps, deferred: bool) {
        self.accepted.keymaps = keymaps;
        self.accepted.deferred_plugin_keymaps = deferred;
        self.stamp = Arc::new(());
        self.binding_revision = self.binding_revision.saturating_add(1);
    }
    /// Flip one view setting and return whether it is now on.
    pub(super) fn toggle_view(&mut self, toggle: ViewToggle) -> bool {
        self.stamp = Arc::new(());
        let editor = &mut self.accepted.settings.editor;
        match toggle {
            ViewToggle::Wrap => {
                editor.wrap = !editor.wrap;
                editor.wrap
            }
            ViewToggle::LineNumbers if editor.line_numbers == LineNumbers::Off => {
                editor.line_numbers = self.hidden_line_numbers.take().unwrap_or_default();
                true
            }
            ViewToggle::LineNumbers => {
                self.hidden_line_numbers = Some(editor.line_numbers);
                editor.line_numbers = LineNumbers::Off;
                false
            }
            ViewToggle::RelativeNumbers => {
                editor.line_numbers = if editor.line_numbers == LineNumbers::Absolute {
                    LineNumbers::Relative
                } else {
                    LineNumbers::Absolute
                };
                editor.line_numbers == LineNumbers::Relative
            }
            ViewToggle::IndentGuides => {
                editor.indent_guides.enabled = !editor.indent_guides.enabled;
                editor.indent_guides.enabled
            }
            ViewToggle::GitSigns => {
                editor.git_signs.enabled = !editor.git_signs.enabled;
                editor.git_signs.enabled
            }
            ViewToggle::FormatOnSave => {
                let lsp = &mut self.accepted.settings.lsp;
                lsp.format_on_save = !lsp.format_on_save;
                lsp.format_on_save
            }
        }
    }
    /// Enter zen mode, hiding line numbers, git signs and indent guides, or leave it and put
    /// them back. Leaving returns whether the explorer was open when zen mode began.
    pub(super) fn toggle_zen(&mut self, explorer_open: bool) -> Option<bool> {
        self.stamp = Arc::new(());
        let editor = &mut self.accepted.settings.editor;
        if let Some(zen) = self.zen.take() {
            editor.line_numbers = zen.line_numbers;
            editor.git_signs.enabled = zen.git_signs;
            editor.indent_guides.enabled = zen.indent_guides;
            return Some(zen.explorer_open);
        }
        self.zen = Some(ZenRestore {
            line_numbers: editor.line_numbers,
            git_signs: editor.git_signs.enabled,
            indent_guides: editor.indent_guides.enabled,
            explorer_open,
        });
        editor.line_numbers = LineNumbers::Off;
        editor.git_signs.enabled = false;
        editor.indent_guides.enabled = false;
        None
    }
    pub(super) fn toggle_inlay_hints(&mut self) -> bool {
        self.stamp = Arc::new(());
        self.accepted.settings.lsp.inlay_hints = !self.accepted.settings.lsp.inlay_hints;
        self.accepted.settings.lsp.inlay_hints
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn fixture_snapshot(&self) -> ResolvedConfiguration {
        self.accepted.clone()
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn install_fixture(&mut self, candidate: ResolvedConfiguration) {
        candidate
            .settings
            .validate()
            .expect("valid owned fixture policy");
        self.accepted = candidate;
        self.themes = themes::Themes::default();
        self.stamp = Arc::new(());
        self.binding_revision = self.binding_revision.saturating_add(1);
    }
}

pub(in crate::editor::session) fn plugin_owner_from_id(id: &str) -> Option<&str> {
    let mut segments = id.split('.');
    (segments.next()? == "plugin")
        .then(|| segments.next())
        .flatten()
        .filter(|owner| !owner.is_empty())
}
