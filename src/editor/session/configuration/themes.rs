//! Theme preview is a presentation override, never a global configuration reload.
use super::ConfigurationSession;
use crate::config::{ThemeCatalog, ThemeChoice, ThemeConfig, ThemeRead};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct ThemeKey {
    epoch: Arc<()>,
    index: usize,
}
#[derive(Default)]
pub(super) struct Themes {
    applied: Option<Arc<ThemeChoice>>,
    interaction: Option<ThemeInteraction>,
}
struct ThemeInteraction {
    epoch: Arc<()>,
    source: Arc<()>,
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
pub(in crate::editor::session) struct ThemeRows {
    pub(in crate::editor::session) items: Vec<(ThemeKey, String, String)>,
    pub(in crate::editor::session) loading: bool,
    pub(in crate::editor::session) diagnostics: Vec<String>,
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
                        choice.theme.name.clone(),
                        if index == 0 {
                            "Current session theme".into()
                        } else {
                            choice.path.display().to_string()
                        },
                    )
                })
                .collect(),
            loading: self.pending.is_some(),
            diagnostics,
        }
    }
}
impl ConfigurationSession {
    pub(in crate::editor::session) fn begin_theme_picker(&mut self) -> Result<ThemeRows, String> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let directory = self
            .accepted
            .sources
            .settings
            .is_absolute()
            .then(|| self.accepted.sources.settings.with_file_name("themes"));
        self.worker
            .request_themes(ThemeRead {
                directory,
                cancelled: cancelled.clone(),
                reply,
            })
            .map_err(|e| e.to_string())?;
        let current = self.themes.applied.clone().unwrap_or_else(|| {
            Arc::new(ThemeChoice {
                theme: self.accepted.theme.clone(),
                path: self.accepted.sources.theme.clone(),
            })
        });
        let interaction = ThemeInteraction {
            epoch: Arc::new(()),
            source: self.stamp.clone(),
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
                && Arc::ptr_eq(&i.source, &self.stamp)
                && i.revision == self.binding_revision
                && key.index < i.choices.len()
        })
    }
    pub(in crate::editor::session) fn poll_theme_picker(
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
        interaction
            .choices
            .extend(result.choices.into_iter().map(Arc::new));
        Some(interaction.rows(result.diagnostics))
    }
    pub(in crate::editor::session) fn preview_theme(&mut self, key: Option<&ThemeKey>) {
        let selected = key
            .filter(|key| self.theme_key_current(key))
            .map(|key| key.index);
        if let Some(interaction) = &mut self.themes.interaction {
            interaction.selected = selected;
        }
    }
    pub(in crate::editor::session) fn apply_theme(
        &mut self,
        key: &ThemeKey,
    ) -> Result<String, String> {
        if !self.theme_key_current(key) {
            return Err("Theme selection belongs to a retired configuration or picker".into());
        }
        let choice = self.themes.interaction.as_ref().unwrap().choices[key.index].clone();
        let name = choice.theme.name.clone();
        self.themes.applied = Some(choice);
        self.cancel_theme_picker();
        Ok(name)
    }
    pub(in crate::editor::session) fn cancel_theme_picker(&mut self) {
        self.themes.interaction = None;
    }
    pub(super) fn effective_theme(&self) -> &ThemeConfig {
        self.theme_override()
            .map_or(&self.accepted.theme, |choice| &choice.theme)
    }
    fn theme_override(&self) -> Option<&ThemeChoice> {
        self.themes
            .interaction
            .as_ref()
            .filter(|i| Arc::ptr_eq(&i.source, &self.stamp) && i.revision == self.binding_revision)
            .and_then(|i| i.selected.and_then(|index| i.choices.get(index)))
            .map(Arc::as_ref)
            .or(self.themes.applied.as_deref())
    }
    pub(super) fn explain_theme(&self, key: &str) -> Option<crate::config::ConfigExplanation> {
        let choice = self.theme_override()?;
        let value = if key == "ui.theme" {
            toml::Value::String(choice.theme.name.clone())
        } else {
            let segments = crate::config::config_key_segments(key.strip_prefix("theme.")?)?;
            super::model::resolved_toml_value(
                toml::Value::try_from(&choice.theme).ok()?,
                &segments,
            )?
        };
        let source = format!(
            "session-only theme from {} (not saved)",
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
