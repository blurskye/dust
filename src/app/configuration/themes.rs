//! Theme preview is a presentation override, never a global configuration reload.
use crate::app::configuration::Configuration;
use crate::config::ThemeCatalog;
use crate::config::ThemeChoice;
use crate::config::ThemeConfig;
use crate::config::ThemeRead;
use crate::config::ThemeReadSource;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;

#[derive(Clone, Debug)]
pub(in crate::app) struct ThemeKey {
    epoch: Arc<()>,
    index: usize,
}
#[derive(Default)]
pub(super) struct Themes {
    applied: Option<Arc<ThemeChoice>>,
    interaction: Option<ThemeInteraction>,
    restore_source: Option<Arc<()>>,
    restore: Option<ThemeRestore>,
    /// The theme `:colorscheme name` asked for, applied once the list has loaded.
    wanted: Option<String>,
}
impl Themes {
    /// Keep a selection only if it differs from the freshly loaded configured theme.
    pub(in crate::app) fn reset(&mut self, keep_applied: bool, configured_name: &str) {
        let applied = self
            .applied
            .take()
            .filter(|choice| keep_applied && choice.theme.name != configured_name);
        *self = Self {
            applied,
            ..Self::default()
        };
    }
}
struct ThemeRestore {
    source: Arc<()>,
    receiver: Receiver<ThemeCatalog>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for ThemeRestore {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
struct ThemeInteraction {
    epoch: Arc<()>,
    revision: u64,
    choices: Vec<Arc<ThemeChoice>>,
    selected: Option<usize>,
    pending: Option<Receiver<ThemeCatalog>>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for ThemeInteraction {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
pub(in crate::app) struct ThemeRows {
    pub(in crate::app) items: Vec<(ThemeKey, String, String)>,
    pub(in crate::app) loading: bool,
    pub(in crate::app) diagnostics: Vec<String>,
}
impl ThemeInteraction {
    fn rows(&self, diagnostics: Vec<String>) -> ThemeRows {
        ThemeRows {
            items: self
                .choices
                .iter()
                .enumerate()
                .map(|(index, choice)| {
                    (
                        ThemeKey {
                            epoch: self.epoch.clone(),
                            index,
                        },
                        crate::config::theme_display_name(&choice.theme.name),
                        if choice.path.to_string_lossy().starts_with("<bundled:") {
                            "Builtin".into()
                        } else if choice.path.to_string_lossy().starts_with("<editor-state:") {
                            "Saved".into()
                        } else {
                            "Installed".into()
                        },
                    )
                })
                .collect(),
            loading: self.pending.is_some(),
            diagnostics,
        }
    }
}
fn themes_directory(source: &std::path::Path, user_config: &std::path::Path) -> std::path::PathBuf {
    if source.to_string_lossy().starts_with("<bundled:") {
        user_config.with_file_name("themes")
    } else {
        source.with_file_name("themes")
    }
}

impl Configuration {
    pub(in crate::app) fn begin_theme_restore(&mut self) {
        self.themes.restore = None;
        self.themes.restore_source = self
            .themes
            .interaction
            .is_none()
            .then(|| self.stamp.clone());
    }
    pub(in crate::app) fn restore_theme(&mut self, snapshot: Option<&str>) -> Result<(), String> {
        let Some(source) = self
            .themes
            .restore_source
            .take()
            .filter(|source| Arc::ptr_eq(source, &self.stamp))
        else {
            return Ok(());
        };
        let Some(snapshot) = snapshot else {
            return Ok(());
        };
        if snapshot.len() > 128 * 1024 {
            return Err("saved theme exceeds 128 KiB; configured theme retained".into());
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.worker
            .request_themes(ThemeRead {
                source: ThemeReadSource::Saved(snapshot.into()),
                cancelled: cancelled.clone(),
                reply,
            })
            .map_err(|e| e.to_string())?;
        self.themes.restore = Some(ThemeRestore {
            source,
            receiver,
            cancelled,
        });
        Ok(())
    }
    pub(in crate::app) fn poll_theme_restore(&mut self) -> Option<Result<(), String>> {
        let restore = self.themes.restore.as_ref()?;
        if !Arc::ptr_eq(&restore.source, &self.stamp) {
            self.themes.restore = None;
            return None;
        }
        let result = match restore.receiver.try_recv() {
            Ok(catalog) => catalog,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => ThemeCatalog {
                choices: vec![],
                diagnostics: vec!["saved-theme worker stopped; configured theme retained".into()],
            },
        };
        self.themes.restore = None;
        if let Some(choice) = result.choices.into_iter().next() {
            self.themes.applied = Some(Arc::new(choice));
            Some(Ok(()))
        } else {
            Some(Err(result.diagnostics.join("; ")))
        }
    }
    pub(in crate::app) fn theme_state(&self) -> Result<Option<String>, String> {
        self.themes
            .applied
            .as_ref()
            .map(|choice| {
                let snapshot = crate::config::theme_snapshot(choice)?;
                if snapshot.len() > 128 * 1024
                    || choice.theme.name.len() > 96
                    || choice.theme.name.is_empty()
                    || choice.theme.name.chars().any(char::is_control)
                {
                    return Err("theme exceeds editor-state limits; previous state retained".into());
                }
                Ok(snapshot)
            })
            .transpose()
    }
    pub(in crate::app) fn begin_theme_picker(&mut self) -> Result<ThemeRows, String> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let directory = Some(themes_directory(
            &self.accepted.sources.settings,
            &crate::config::user_config_path(),
        ));
        self.worker
            .request_themes(ThemeRead {
                source: ThemeReadSource::Directory(directory),
                cancelled: cancelled.clone(),
                reply,
            })
            .map_err(|e| e.to_string())?;
        self.themes.restore_source = None;
        self.themes.restore = None;
        let current = self.themes.applied.clone().unwrap_or_else(|| {
            Arc::new(ThemeChoice {
                theme: self.accepted.theme.clone(),
                path: self.accepted.sources.theme.clone(),
            })
        });
        let interaction = ThemeInteraction {
            epoch: Arc::new(()),
            revision: self.binding_revision,
            choices: vec![current],
            selected: None,
            pending: Some(receiver),
            cancelled,
        };
        let rows = interaction.rows(Vec::new());
        self.themes.interaction = Some(interaction);
        Ok(rows)
    }
    fn theme_key_current(&self, key: &ThemeKey) -> bool {
        self.themes.interaction.as_ref().is_some_and(|i| {
            Arc::ptr_eq(&i.epoch, &key.epoch)
                && i.revision == self.binding_revision
                && key.index < i.choices.len()
        })
    }
    pub(in crate::app) fn poll_theme_picker(
        &mut self,
        key: Option<&ThemeKey>,
    ) -> Option<ThemeRows> {
        if !key.is_some_and(|key| self.theme_key_current(key)) {
            self.cancel_theme_picker();
            return None;
        }
        let interaction = self.themes.interaction.as_mut()?;
        let result = match interaction.pending.as_ref()?.try_recv() {
            Ok(catalog) => catalog,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => ThemeCatalog {
                choices: Vec::new(),
                diagnostics: vec!["Theme discovery worker stopped; previous theme retained".into()],
            },
        };
        interaction.pending = None;
        for choice in result.choices {
            if let Some(index) = interaction
                .choices
                .iter()
                .position(|existing| existing.theme.name == choice.theme.name)
            {
                interaction.choices[index] = Arc::new(choice);
            } else {
                interaction.choices.push(Arc::new(choice));
            }
        }
        Some(interaction.rows(result.diagnostics))
    }
    pub(in crate::app) fn preview_theme(&mut self, key: Option<&ThemeKey>) {
        let selected = key
            .filter(|key| self.theme_key_current(key))
            .map(|key| key.index);
        if let Some(interaction) = &mut self.themes.interaction {
            interaction.selected = selected;
        }
    }
    pub(in crate::app) fn apply_theme(&mut self, key: &ThemeKey) -> Result<String, String> {
        if !self.theme_key_current(key) {
            return Err("Theme selection belongs to an outdated configuration or picker".into());
        }
        let choice = self.themes.interaction.as_ref().unwrap().choices[key.index].clone();
        let name = choice.theme.name.clone();
        self.themes.applied = Some(choice);
        self.cancel_theme_picker();
        Ok(name)
    }
    pub(in crate::app) fn want_theme(&mut self, name: Option<String>) {
        self.themes.wanted = name;
    }
    pub(in crate::app) fn take_wanted_theme(&mut self) -> Option<String> {
        self.themes.wanted.take()
    }
    pub(in crate::app) fn cancel_theme_picker(&mut self) {
        self.themes.interaction = None;
    }
    pub(in crate::app) fn effective_theme(&self) -> &ThemeConfig {
        self.theme_override()
            .map_or(&self.accepted.theme, |choice| &choice.theme)
    }
    fn theme_override(&self) -> Option<&ThemeChoice> {
        self.themes
            .interaction
            .as_ref()
            .filter(|i| i.revision == self.binding_revision)
            .and_then(|i| i.selected.and_then(|index| i.choices.get(index)))
            .map(Arc::as_ref)
            .or(self.themes.applied.as_deref())
    }
    pub(in crate::app) fn explain_theme(
        &self,
        key: &str,
    ) -> Option<crate::config::ConfigExplanation> {
        let choice = self.theme_override()?;
        let value = if key == "ui.theme" {
            toml::Value::String(choice.theme.name.clone())
        } else {
            let segments = crate::config::config_key_segments(key.strip_prefix("theme.")?)?;
            crate::app::configuration::resolve::resolved_toml_value(
                toml::Value::try_from(&choice.theme).ok()?,
                &segments,
            )?
        };
        let preview = self.themes.interaction.as_ref().is_some_and(|interaction| {
            interaction.revision == self.binding_revision && interaction.selected.is_some()
        });
        let source = format!(
            "{} from {}",
            if preview {
                "theme preview (not saved)"
            } else {
                "editor-state theme selection"
            },
            choice.path.display()
        );
        Some(crate::config::ConfigExplanation {
            key: key.into(),
            value: value.to_string(),
            source: source.clone(),
            contributors: vec![source],
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reloaded_configured_theme_replaces_an_applied_snapshot() {
        let theme = crate::config::load_bundled_config().theme;
        let choice = std::sync::Arc::new(crate::config::ThemeChoice {
            theme: theme.clone(),
            path: "installed.toml".into(),
        });
        let mut themes = crate::app::configuration::themes::Themes {
            applied: Some(choice.clone()),
            ..Default::default()
        };
        themes.reset(true, "another-theme");
        assert!(themes.applied.is_some());
        themes.reset(true, &theme.name);
        assert!(themes.applied.is_none());
        themes.applied = Some(choice);
        themes.reset(false, "another-theme");
        assert!(themes.applied.is_none());
    }

    #[test]
    fn installed_themes_do_not_depend_on_config_provenance() {
        use std::path::Path;
        use std::path::PathBuf;
        assert_eq!(
            crate::app::configuration::themes::themes_directory(
                Path::new("<bundled:config>"),
                Path::new("/config/dust/config.toml")
            ),
            PathBuf::from("/config/dust/themes")
        );
        assert_eq!(
            crate::app::configuration::themes::themes_directory(
                Path::new("relative/config.toml"),
                Path::new("/other/config.toml")
            ),
            PathBuf::from("relative/themes")
        );
    }
}
