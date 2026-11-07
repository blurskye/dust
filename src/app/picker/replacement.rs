//! Select replacement requests retain native row actions and stale-source checks.

use crate::app::App;
use crate::app::picker::PickerEpoch;
use crate::ui::PickerUiState;
use crate::workspace::PickerItemId;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

#[allow(dead_code)] // Presentation adapters consume this on their own worker.
pub(in crate::app) struct SelectRequest {
    pub picker: PickerUiState,
    pub reply: SyncSender<Option<PickerItemId>>,
}

#[derive(Default)]
pub(super) struct SelectReplacement {
    requests: Option<SyncSender<SelectRequest>>,
    pending: Option<(PickerEpoch, Receiver<Option<PickerItemId>>, Instant)>,
}

impl App {
    #[allow(dead_code)]
    pub(in crate::app) fn replace_select(&mut self, requests: Option<SyncSender<SelectRequest>>) {
        let replacement = &mut self.search.picker.replacement;
        replacement.requests = requests;
        replacement.pending = None;
        self.sync_picker_ui();
    }

    pub(super) fn request_select_replacement(&mut self) {
        self.search.picker.replacement.pending = None;
        if self.search.help_is_open() {
            return;
        }
        let Some(picker) = self.search.picker_projection() else {
            return;
        };
        let Some(epoch) = self.search.picker_epoch().cloned() else {
            return;
        };
        let Some(requests) = &self.search.picker.replacement.requests else {
            return;
        };
        let (reply, receiver) = std::sync::mpsc::sync_channel(1);
        if requests.try_send(SelectRequest { picker, reply }).is_ok() {
            self.search.picker.replacement.pending =
                Some((epoch, receiver, Instant::now() + Duration::from_secs(60)));
            self.ui.picker = None;
        }
    }

    pub(in crate::app) fn select_replacement_pending(&self) -> bool {
        self.search.picker.replacement.pending.is_some()
    }

    pub(super) fn cancel_select_replacement(&mut self) {
        self.search.picker.replacement.pending = None;
    }

    pub(in crate::app) fn process_select_replacement(&mut self) {
        let Some((epoch, receiver, deadline)) = self.search.picker.replacement.pending.take()
        else {
            return;
        };
        if !self.search.owns_picker_epoch(&epoch) {
            return;
        }
        match receiver.try_recv() {
            Ok(Some(id)) => {
                let valid = self.search.picker_projection().is_some_and(|picker| {
                    picker
                        .rows
                        .iter()
                        .any(|row| row.id == id.0 && row.kind == crate::ui::PickerRowKind::Entry)
                });
                if valid {
                    self.search.select_picker_item(&id);
                    self.accept_picker();
                } else {
                    self.sync_picker_ui();
                    self.show_error("select replacement returned an unknown item");
                }
            }
            Ok(None) => self.close_picker(),
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                self.search.picker.replacement.pending = Some((epoch, receiver, deadline));
            }
            Err(_) => self.sync_picker_ui(),
        }
    }
}
