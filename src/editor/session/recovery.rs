use super::*;

impl InteractiveApplication {
    pub(super) fn recovery_path(&self, buffer: crate::editor::BufferId) -> PathBuf {
        let identifier = format!("{buffer:?}")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        self.recovery_directory
            .join(format!("buffer-{identifier}.toml"))
    }

    pub(super) fn schedule_recovery_work(&mut self) {
        if !self.configuration.settings.recovery.enabled {
            return;
        }
        if !self.recovery_discovery_queued
            && let Ok(id) = self.vfs.discover_recovery(
                self.recovery_directory.clone(),
                self.configuration
                    .settings
                    .recovery
                    .maximum_discovered_records,
            )
        {
            self.recovery_discovery_queued = true;
            self.pending_vfs.insert(id, PendingVfs::RecoveryDiscovery);
        }
        if self.configuration.settings.recovery.cleanup_enabled
            && !self.recovery_cleanup_queued
            && let Ok(id) = self.vfs.cleanup_recovery(
                self.recovery_directory.clone(),
                self.configuration
                    .settings
                    .recovery
                    .maximum_retained_records,
                self.configuration.settings.recovery.maximum_age_days,
            )
        {
            self.recovery_cleanup_queued = true;
            self.pending_vfs.insert(id, PendingVfs::RecoveryCleanup);
        }
        let now = Instant::now();
        let debounce =
            Duration::from_millis(self.configuration.settings.recovery.write_debounce_ms);
        let buffers = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| !buffer.is_virtual_read_only())
            .map(|(id, buffer)| (id, buffer.dirty, buffer.text.revision()))
            .collect::<Vec<_>>();
        for (buffer, dirty, revision) in buffers {
            if dirty {
                let already_queued = self.recovery_revisions.get(&buffer) == Some(&revision)
                    || self.recovery_pruned_revisions.get(&buffer) == Some(&revision);
                let already_scheduled = self
                    .recovery_scheduled
                    .get(&buffer)
                    .is_some_and(|(scheduled, _)| *scheduled == revision);
                if !already_queued && !already_scheduled {
                    self.recovery_scheduled
                        .insert(buffer, (revision, now + debounce));
                }
            } else {
                self.recovery_scheduled.remove(&buffer);
                self.recovery_pruned_revisions.remove(&buffer);
                if self.configuration.settings.recovery.delete_clean_journals
                    && self.recovery_revisions.contains_key(&buffer)
                {
                    let journal = self.recovery_path(buffer);
                    let delete_queued = self.pending_vfs.values().any(|pending| {
                        matches!(pending, PendingVfs::RecoveryDelete { path, .. } if path == &journal)
                    });
                    if !delete_queued && let Ok(id) = self.vfs.delete(journal.clone()) {
                        self.pending_vfs.insert(
                            id,
                            PendingVfs::RecoveryDelete {
                                path: journal,
                                buffer: Some((buffer, self.recovery_revisions[&buffer])),
                            },
                        );
                    }
                }
            }
        }
        let due = self
            .recovery_scheduled
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(buffer, (revision, _))| (*buffer, *revision))
            .collect::<Vec<_>>();
        for (buffer, revision) in due {
            let Some(buffer_state) = self.state.buffers.get(buffer) else {
                self.recovery_scheduled.remove(&buffer);
                continue;
            };
            if !buffer_state.dirty || buffer_state.text.revision() != revision {
                self.recovery_scheduled.remove(&buffer);
                continue;
            }
            let record = RecoveryRecord::new(
                buffer_state.path.as_deref(),
                revision.0,
                buffer_state.text.to_string(),
            );
            let journal = self.recovery_path(buffer);
            if let Ok(id) = self.vfs.write_recovery(journal, record) {
                self.recovery_scheduled.remove(&buffer);
                self.recovery_pruned_revisions.remove(&buffer);
                self.recovery_revisions.insert(buffer, revision);
                self.pending_vfs
                    .insert(id, PendingVfs::RecoveryWrite { buffer, revision });
            }
        }
    }

    pub(super) fn queue_dirty_recovery_before_exit(&mut self) {
        if !self.configuration.settings.recovery.enabled {
            return;
        }
        let now = Instant::now();
        for (buffer, state) in &self.state.buffers {
            if state.dirty
                && self.recovery_revisions.get(&buffer) != Some(&state.text.revision())
                && self.recovery_pruned_revisions.get(&buffer) != Some(&state.text.revision())
            {
                self.recovery_scheduled
                    .insert(buffer, (state.text.revision(), now));
            }
        }
        self.schedule_recovery_work();
    }
}
