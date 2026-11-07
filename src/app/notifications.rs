//! Notifications, expiry and history.

use crate::app::App;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

/// Owns notification insertion, expiry, history and telemetry writers.
/// The renderer receives a projection, never this owner's mutable state.
pub(super) struct Notifications {
    current: Option<(bool, String)>,
    /// A notice to take down when what it announced is over.
    passing: Option<String>,
    /// The current notice is a warning: something degraded, but the work went on.
    warning: bool,
    history: VecDeque<(bool, String)>,
    deadline: Option<Instant>,
    replacement: Option<std::sync::mpsc::SyncSender<NoticeRequest>>,
    replaced: bool,
}

/// Notifications keep native history even when another surface presents them.
pub(in crate::app) struct NoticeRequest {
    pub message: String,
    pub authority: crate::editor::CommandAuthority,
    pub error: bool,
    pub warning: bool,
}

impl Notifications {
    pub(in crate::app) fn replace(
        &mut self,
        replacement: Option<std::sync::mpsc::SyncSender<NoticeRequest>>,
    ) {
        self.replacement = replacement;
        self.replaced = false;
    }

    fn present(
        &mut self,
        message: &str,
        error: bool,
        warning: bool,
        authority: &crate::editor::CommandAuthority,
    ) {
        self.replaced = self.replacement.as_ref().is_some_and(|replacement| {
            replacement
                .try_send(NoticeRequest {
                    message: message.to_owned(),
                    authority: authority.clone(),
                    error,
                    warning,
                })
                .is_ok()
        });
    }

    pub(in crate::app) fn new() -> Self {
        Self {
            current: None,
            passing: None,
            warning: false,
            history: VecDeque::new(),
            deadline: None,
            replacement: None,
            replaced: false,
        }
    }

    pub(in crate::app) fn notify(
        &mut self,
        message: String,
        error: bool,
        now: Instant,
        timeout: Duration,
        authority: &crate::editor::CommandAuthority,
    ) {
        // Repeated status messages do not perpetually postpone expiry.
        if self
            .current
            .as_ref()
            .is_none_or(|(previous_error, previous)| {
                *previous_error != error || previous != &message
            })
        {
            if self
                .history
                .back()
                .is_none_or(|(previous_error, previous)| {
                    *previous_error != error || previous != &message
                })
            {
                self.history.push_back((error, message.clone()));
                if self.history.len() > 200 {
                    self.history.pop_front();
                }
            }
            self.deadline = (!timeout.is_zero()).then_some(now + timeout);
        }
        self.present(&message, error, false, authority);
        self.current = Some((error, message));
        self.warning = false;
    }

    pub(super) fn warn(
        &mut self,
        message: String,
        now: Instant,
        timeout: Duration,
        authority: &crate::editor::CommandAuthority,
    ) {
        // Record once; warning is part of the replacement's presentation too.
        let replacement = self.replacement.take();
        self.notify(message.clone(), false, now, timeout, authority);
        self.replacement = replacement;
        self.present(&message, false, true, authority);
        self.warning = true;
    }

    pub(super) fn is_warning(&self) -> bool {
        self.current.is_some() && self.warning
    }

    pub(in crate::app) fn clear(&mut self) {
        self.current = None;
        self.deadline = None;
    }

    /// Remembers the current notice as one that stops being true, such as "starting".
    pub(in crate::app) fn mark_current_passing(&mut self) {
        self.passing = self.current.as_ref().map(|(_, message)| message.clone());
    }

    /// Takes down the passing notice once it has passed, unless another has replaced it.
    pub(in crate::app) fn clear_passing(&mut self) {
        if self.passing.take().is_some_and(|passing| {
            self.current
                .as_ref()
                .is_some_and(|(_, current)| *current == passing)
        }) {
            self.clear();
        }
    }

    pub(in crate::app) fn tick(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.clear();
            true
        } else {
            false
        }
    }

    pub(in crate::app) fn current(&self) -> Option<(bool, &str)> {
        self.current
            .as_ref()
            .map(|(error, message)| (*error, message.as_str()))
    }

    pub(in crate::app) fn history(&self) -> impl DoubleEndedIterator<Item = &(bool, String)> {
        self.history.iter()
    }
}

/// How long a notice stays on screen.
const NOTICE_TIMEOUT: Duration = Duration::from_secs(4);

impl App {
    pub(in crate::app) fn notify_message(&mut self, message: String, error: bool) {
        self.notices.notify(
            message,
            error,
            self.clock.monotonic(),
            NOTICE_TIMEOUT,
            &self.effect_authority,
        );
        self.project_notice();
    }
}

impl App {
    /// A small yellow notice: something is missing, but what could be shown is shown.
    pub(in crate::app) fn notify_warning(&mut self, message: String) {
        self.notices.warn(
            message,
            self.clock.monotonic(),
            NOTICE_TIMEOUT,
            &self.effect_authority,
        );
        self.project_notice();
    }
}

impl App {
    pub(in crate::app) fn clear_notice(&mut self) {
        self.notices.clear();
        self.project_notice();
    }
}

impl App {
    pub(in crate::app) fn project_notice(&mut self) {
        let current = if self.notices.replaced {
            None
        } else {
            self.notices.current()
        };
        self.ui.notification = current.map(|(_, message)| message.to_owned());
        self.ui.notification_error = current.is_some_and(|(error, _)| error);
        self.ui.notification_warning = self.notices.is_warning();
    }
}

impl App {
    pub(in crate::app) fn update_notification_timer(&mut self) {
        if self.notices.tick(self.clock.monotonic()) {
            self.project_notice();
        }
    }
}

impl App {
    /// A path as the reader knows it in the project, or whole when it lies outside.
    /// A short name for a notification: outside the workspace a full path would fill the toast.
    pub(in crate::app) fn notice_file_label(&self, path: &std::path::Path) -> String {
        let label = self.workspace_relative_label(path);
        if std::path::Path::new(&label).is_absolute() {
            path.file_name()
                .map_or(label, |name| name.to_string_lossy().into_owned())
        } else {
            label
        }
    }
}

impl App {
    pub(in crate::app) fn show_error(&mut self, error: impl std::fmt::Display) {
        self.notify_message(error.to_string(), true);
    }
}
use crate::app::commands::AppEffect;

use crate::app::picker::PickerPayload;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

impl App {
    pub(in crate::app) fn process_notifications_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::NotificationHistory => {
                let items = self
                    .notices
                    .history()
                    .rev()
                    .enumerate()
                    .map(|(index, (error, message))| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("notification:{index}")),
                                label: message.clone(),
                                detail: if *error {
                                    "error".into()
                                } else {
                                    "info".into()
                                },
                                source: "application".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Notice {
                                title: "Notification".into(),
                                text: message.clone(),
                            },
                        )
                    })
                    .collect();
                self.clear_notice();
                self.install_picker("Notifications".into(), items);
            }
            AppEffect::NotificationDismiss => {
                self.clear_notice();
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::notifications::Notifications;
    use crate::editor::CommandAuthority::Host;
    use std::time::Duration;
    use std::time::Instant;

    #[test]
    fn replacement_receives_native_notices_and_preserves_history_with_fallback() {
        let now = Instant::now();
        let mut notices = Notifications::new();
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        notices.replace(Some(sender));
        notices.notify("saved".into(), false, now, Duration::ZERO, &Host);
        let request = receiver.try_recv().unwrap();
        assert_eq!(request.message, "saved");
        assert!(!request.error);
        assert!(!request.warning);
        assert!(notices.replaced);
        notices.warn("offline".into(), now, Duration::ZERO, &Host);
        let warning = receiver.try_recv().unwrap();
        assert!(warning.warning);
        assert_eq!(notices.history().count(), 2);
        drop(receiver);
        notices.notify("error".into(), true, now, Duration::ZERO, &Host);
        assert!(!notices.replaced);
        assert_eq!(notices.current(), Some((true, "error")));
        assert_eq!(notices.history().count(), 3);
    }

    #[test]
    fn explicit_insertion_owns_history_and_expiry_without_observing_ui() {
        let now = Instant::now();
        let mut notices = Notifications::new();
        notices.notify("first".into(), false, now, Duration::from_secs(1), &Host);
        notices.notify(
            "first".into(),
            false,
            now + Duration::from_millis(900),
            Duration::from_secs(1),
            &Host,
        );
        assert_eq!(notices.history().count(), 1);
        assert!(!notices.tick(now + Duration::from_millis(999)));
        assert!(notices.tick(now + Duration::from_secs(1)));
        assert_eq!(notices.current(), None);
        assert_eq!(notices.history().count(), 1);
        notices.notify("error".into(), true, now, Duration::ZERO, &Host);
        assert!(!notices.tick(now + Duration::from_secs(60)));
        assert_eq!(notices.current(), Some((true, "error")));
        notices.clear();
        assert_eq!(notices.current(), None);
        notices.warn("offline".into(), now, Duration::ZERO, &Host);
        assert!(notices.is_warning());
        notices.notify("back".into(), false, now, Duration::ZERO, &Host);
        assert!(!notices.is_warning());
    }

    #[test]
    fn changed_severity_records_a_new_notice_and_restarts_expiry() {
        let now = Instant::now();
        let mut notices = Notifications::new();
        notices.notify("offline".into(), false, now, Duration::from_secs(1), &Host);
        notices.notify(
            "offline".into(),
            true,
            now + Duration::from_millis(900),
            Duration::from_secs(1),
            &Host,
        );
        assert_eq!(notices.current(), Some((true, "offline")));
        assert_eq!(
            notices.history().cloned().collect::<Vec<_>>(),
            vec![(false, "offline".into()), (true, "offline".into()),]
        );
        assert!(!notices.tick(now + Duration::from_secs(1)));
        assert!(notices.tick(now + Duration::from_millis(1900)));
    }

    #[test]
    fn history_retains_only_the_latest_two_hundred_notices() {
        let now = Instant::now();
        let mut notices = Notifications::new();
        for index in 0..205 {
            notices.notify(index.to_string(), false, now, Duration::ZERO, &Host);
        }
        assert_eq!(notices.history().count(), 200);
        assert_eq!(notices.history().next().unwrap().1, "5");
        assert_eq!(notices.history().next_back().unwrap().1, "204");
    }
}
