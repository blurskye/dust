//! Theme discovery, selection and persistence through the application picker.

use crate::app::App;
use crate::app::picker::PickerOrigin;
use crate::app::picker::PickerPayload;
use crate::ui::PickerSurfaceKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

impl App {
    pub(in crate::app) fn process_theme_results(&mut self) {
        if let Some(Err(error)) = self.configuration.poll_theme_restore() {
            self.show_error(error);
        }
        self.close_stale_picker();
        if let Some(rows) = self
            .configuration
            .poll_theme_picker(self.search.theme_picker_key())
        {
            if !rows.loading
                && let Some(wanted) = self.configuration.take_wanted_theme()
            {
                self.apply_wanted_theme(&wanted, &rows);
                return;
            }
            let previous = self.search.picker_bookmark();
            self.present_theme_picker(rows);
            self.search.restore_picker_view(previous, false);
            self.sync_picker_ui();
        }
    }
}

impl App {
    fn apply_wanted_theme(&mut self, wanted: &str, rows: &crate::app::configuration::ThemeRows) {
        // "Rosé Pine", "rose pine" and "rose-pine" all name the same theme.
        let plain = |name: &str| name.to_lowercase().replace('é', "e").replace(' ', "-");
        let key = rows
            .items
            .iter()
            .find(|(_, name, _)| plain(name) == plain(wanted))
            .map(|(key, _, _)| key);
        let applied = key.map(|key| self.configuration.apply_theme(key));
        self.close_picker();
        match applied {
            Some(Ok(name)) => self.finish_theme_choice(&name),
            Some(Err(error)) => self.show_error(error),
            None => self.show_error(format!("no theme named {wanted}: :colorscheme lists them")),
        }
    }
}

impl App {
    pub(in crate::app) fn finish_theme_choice(&mut self, name: &str) {
        // The choice goes into config.toml like any other setting; once the reload lands, the
        // configured theme replaces the applied one. Without a config file it lives in the
        // editor state instead.
        let saved = self.configuration.user_settings_path().is_some_and(|path| {
            crate::config::write_user_setting(
                &path,
                "ui.theme",
                Some(toml::Value::String(name.into())),
            )
            .and_then(|()| {
                self.configuration
                    .request_reload()
                    .map_err(|error| error.to_string())
            })
            .map_err(|error| self.show_error(error))
            .is_ok()
        });
        let saving = saved || self.configuration.settings().persistence.enabled;
        if saving {
            self.persistence.request_save();
        }
        let message = format!(
            "Theme {}{}",
            crate::config::theme_display_name(name),
            if saving { "" } else { " (not saved)" }
        );
        if self.ui.settings.is_some() {
            self.refresh_settings();
            self.settings_status(message, false);
        } else {
            self.notify_message(message, false);
        }
    }
}

impl App {
    pub(in crate::app) fn present_theme_picker(
        &mut self,
        rows: crate::app::configuration::ThemeRows,
    ) {
        let items = rows
            .items
            .into_iter()
            .enumerate()
            .map(|(index, (key, name, origin))| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("theme:{index}")),
                        label: name,
                        detail: if index == 0 {
                            "Current".into()
                        } else {
                            String::new()
                        },
                        source: origin,
                        action: PickerAction::None,
                    },
                    PickerPayload::Theme(key),
                )
            })
            .collect();
        self.install_surface_picker_state(
            PickerSurfaceKind::Theme,
            "Dust themes".into(),
            items,
            String::new(),
            !rows.loading,
            PickerOrigin::General,
        );
        if !rows.diagnostics.is_empty() {
            self.show_error(rows.diagnostics.join("; "));
        }
    }
}
