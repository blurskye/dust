use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn open_live_workspace_search(&mut self) {
        self.close_picker();
        self.workspace_search_live = true;
        self.workspace_search_query.clear();
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            self.message("workspace_search_title", &[]),
            Vec::new(),
            String::new(),
            true,
        );
    }

    pub(in crate::editor::session) fn restart_live_workspace_search(&mut self, query: String) {
        self.workspace_search_query.clone_from(&query);
        if query.is_empty() {
            self.workspace_search.cancel();
            self.workspace_search_generation = None;
            self.workspace_search_workflow = None;
            self.workspace_search_sequence = 0;
            self.workspace_search_matches.clear();
            self.workspace_search_intent = None;
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                Vec::new(),
                query,
                true,
            );
            return;
        }
        self.start_workspace_search(query, None);
    }

    pub(in crate::editor::session) fn start_workspace_file_picker(&mut self) {
        self.close_picker();
        let configured = &self.configuration.settings.workspace.search;
        let root_revision = self.workspace.root_revision();
        let policy = WorkspaceFilePolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|r| r.identity.canonical_path.clone())
                .collect(),
            root_revision,
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: configured.maximum_files,
            maximum_path_bytes: 16 * 1024 * 1024,
            batch_size: configured.batch_size,
            event_queue_capacity: configured.event_queue_capacity,
        };
        match self.workspace_files.start(policy) {
            Ok(generation) => {
                self.workspace_file_generation = Some(generation);
                self.workspace_file_root_revision = Some(root_revision);
                self.workspace_file_entries.clear();
                self.install_picker("Files".into(), Vec::new());
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn process_workspace_file_events(&mut self) {
        if self.workspace_file_generation.is_some()
            && self.workspace_file_root_revision != Some(self.workspace.root_revision())
        {
            self.workspace_files.cancel();
            self.workspace_file_generation = None;
            self.workspace_file_entries.clear();
            return;
        }
        while let Some(event) = self.workspace_files.try_event() {
            match event {
                WorkspaceFileEvent::Batch {
                    generation,
                    root_revision,
                    entries,
                } if Some(generation) == self.workspace_file_generation
                    && Some(root_revision) == self.workspace_file_root_revision =>
                {
                    self.workspace_file_entries.extend(entries)
                }
                WorkspaceFileEvent::Complete {
                    generation,
                    root_revision,
                    ..
                } if Some(generation) == self.workspace_file_generation
                    && Some(root_revision) == self.workspace_file_root_revision =>
                {
                    self.workspace_file_generation = None;
                    let multi_root = self.workspace.roots().len() > 1;
                    let items = self
                        .workspace_file_entries
                        .iter()
                        .map(|entry| {
                            (
                                PickerItem {
                                    id: PickerItemId(entry.id.clone()),
                                    label: entry.relative_path.display().to_string(),
                                    detail: String::new(),
                                    source: if multi_root {
                                        entry.root.file_name().map_or_else(String::new, |name| {
                                            name.to_string_lossy().into_owned()
                                        })
                                    } else {
                                        String::new()
                                    },
                                    action: PickerAction::None,
                                },
                                PickerPayload::MatcherSource(MatcherSource {
                                    path: entry.canonical_path.clone(),
                                    line: 0,
                                    column: 0,
                                }),
                            )
                        })
                        .collect();
                    match compose_workspace_file_items(items) {
                        Ok(items) => self.install_picker("Files".into(), items),
                        Err(error) => self.show_error(error.to_string()),
                    }
                }
                WorkspaceFileEvent::Failed {
                    generation, error, ..
                } if Some(generation) == self.workspace_file_generation => {
                    self.workspace_file_generation = None;
                    self.workspace_file_entries.clear();
                    self.show_error(error)
                }
                WorkspaceFileEvent::Cancelled { generation, .. }
                    if Some(generation) == self.workspace_file_generation =>
                {
                    self.workspace_file_generation = None;
                    self.workspace_file_entries.clear()
                }
                _ => {}
            }
        }
    }

    pub(in crate::editor::session) fn start_workspace_search(
        &mut self,
        query: String,
        replacement: Option<String>,
    ) {
        let configured = &self.configuration.settings.workspace.search;
        self.workspace_search_query.clone_from(&query);
        let policy = WorkspaceSearchPolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            query,
            regex: configured.regex,
            smart_case: configured.smart_case,
            whole_word: configured.whole_word,
            include_globs: configured.include_globs.clone(),
            exclude_globs: configured.exclude_globs.clone(),
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: configured.maximum_files,
            maximum_results: configured.maximum_results,
            maximum_file_bytes: configured.maximum_file_bytes,
            maximum_total_bytes: configured.maximum_total_bytes,
            maximum_preview_bytes: configured.maximum_preview_bytes,
            batch_size: configured.batch_size,
            event_queue_capacity: configured.event_queue_capacity,
        };
        match self.workspace_search.start(policy) {
            Ok(generation) => {
                use crate::workspace::search_workflow::*;
                let spec = SearchWorkflowSpec {
                    id: SearchWorkflowId(generation),
                    source: SearchSourceKind::Grep,
                    matcher: SearchMatcherKind::Literal,
                    presenter: SearchPresenterKind::Picker,
                    maximum_items: configured.maximum_results.max(1),
                    maximum_bytes: usize::try_from(configured.maximum_total_bytes)
                        .unwrap_or(ABS_MAX_BYTES)
                        .clamp(1, ABS_MAX_BYTES),
                };
                let workflow = SearchWorkflowBroker::try_new(spec).and_then(|mut broker| {
                    broker.begin(
                        SearchRequestId(generation),
                        SearchStreamGeneration(generation),
                        "",
                    )?;
                    Ok(broker)
                });
                let Ok(workflow) = workflow else {
                    self.workspace_search.cancel();
                    self.show_error("workspace search workflow limits are invalid");
                    return;
                };
                self.workspace_search_workflow = Some(workflow);
                self.workspace_search_sequence = 0;
                self.workspace_search_generation = Some(generation);
                self.workspace_search_root_revision = Some(self.workspace.root_revision());
                self.workspace_search_matches.clear();
                self.workspace_search_intent = Some(
                    replacement.map_or(WorkspaceSearchIntent::Find, WorkspaceSearchIntent::Replace),
                );
                if self.workspace_search_live {
                    self.install_surface_picker_state(
                        PickerSurfaceKind::Generic,
                        self.message("workspace_search_title", &[]),
                        Vec::new(),
                        self.workspace_search_query.clone(),
                        false,
                    );
                    self.ui.notification = None;
                } else {
                    self.close_picker();
                    self.ui.notification = Some(self.message("workspace_search_started", &[]));
                    self.ui.notification_error = false;
                }
            }
            Err(error) => self.show_error(self.message(
                "workspace_search_failed",
                &[("diagnostic", error.to_string())],
            )),
        }
    }

    pub(in crate::editor::session) fn process_workspace_search_events(&mut self) {
        if self.workspace_search_generation.is_some()
            && self.workspace_search_root_revision != Some(self.workspace.root_revision())
        {
            self.workspace_search.cancel();
            self.workspace_search_generation = None;
            self.workspace_search_workflow = None;
            self.workspace_search_sequence = 0;
            self.workspace_search_intent = None;
            self.workspace_search_matches.clear();
            self.workspace_search_root_revision = None;
        }
        while let Some(event) = self.workspace_search.try_event() {
            match event {
                WorkspaceSearchEvent::Batch {
                    generation,
                    matches,
                } if self.workspace_search_generation == Some(generation)
                    && self.workspace_search_root_revision
                        == Some(self.workspace.root_revision()) =>
                {
                    use crate::workspace::search_workflow::*;
                    let items = matches
                        .iter()
                        .map(|found| SearchWorkflowItem {
                            stable_key: found.id.clone(),
                            title: found.relative_path.display().to_string(),
                            detail: found.line_text.clone(),
                            search_text: format!(
                                "{} {}",
                                found.relative_path.display(),
                                found.line_text
                            ),
                            action: SearchHostAction::OpenLocation {
                                uri: file_uri(&found.canonical_path),
                            },
                        })
                        .collect();
                    let chunk = SearchWorkflowChunk {
                        request: SearchRequestId(generation),
                        generation: SearchStreamGeneration(generation),
                        sequence: self.workspace_search_sequence,
                        items,
                        complete: false,
                    };
                    if self
                        .workspace_search_workflow
                        .as_mut()
                        .is_none_or(|broker| broker.accept(chunk).is_err())
                    {
                        self.workspace_search.cancel();
                        self.workspace_search_generation = None;
                        self.workspace_search_workflow = None;
                        self.workspace_search_sequence = 0;
                        self.workspace_search_root_revision = None;
                        self.workspace_search_intent = None;
                        self.workspace_search_matches.clear();
                        self.show_error("workspace search workflow rejected a result batch");
                        continue;
                    }
                    self.workspace_search_sequence =
                        self.workspace_search_sequence.saturating_add(1);
                    self.workspace_search_matches.extend(matches);
                    if self.workspace_search_live {
                        self.open_workspace_search_picker();
                    }
                }
                WorkspaceSearchEvent::Complete {
                    generation,
                    summary,
                } if self.workspace_search_generation == Some(generation)
                    && self.workspace_search_root_revision
                        == Some(self.workspace.root_revision()) =>
                {
                    use crate::workspace::search_workflow::*;
                    let terminal = SearchWorkflowChunk {
                        request: SearchRequestId(generation),
                        generation: SearchStreamGeneration(generation),
                        sequence: self.workspace_search_sequence,
                        items: Vec::new(),
                        complete: true,
                    };
                    if self
                        .workspace_search_workflow
                        .as_mut()
                        .is_none_or(|broker| broker.accept(terminal).is_err())
                    {
                        self.workspace_search_generation = None;
                        self.workspace_search_workflow = None;
                        self.workspace_search_sequence = 0;
                        self.workspace_search_root_revision = None;
                        self.workspace_search_intent = None;
                        self.workspace_search_matches.clear();
                        self.show_error("workspace search workflow rejected completion");
                        continue;
                    }
                    self.workspace_search_generation = None;
                    let intent = self.workspace_search_intent.take();
                    match intent {
                        Some(WorkspaceSearchIntent::Find) => {
                            if self.workspace_search_live
                                || !self.workspace_search_matches.is_empty()
                            {
                                self.open_workspace_search_picker();
                                if self.workspace_search_live {
                                    self.ui.notification = None;
                                } else {
                                    self.ui.notification = Some(self.message(
                                        "workspace_search_complete",
                                        &[
                                            (
                                                "count",
                                                self.workspace_search_matches.len().to_string(),
                                            ),
                                            ("files", summary.files_searched.to_string()),
                                        ],
                                    ));
                                    self.ui.notification_error = false;
                                }
                            } else {
                                self.show_error(self.message("workspace_search_empty", &[]));
                            }
                        }
                        Some(WorkspaceSearchIntent::Replace(replacement)) => {
                            if self.workspace_search_matches.is_empty() {
                                self.show_error(self.message("workspace_search_empty", &[]));
                            } else {
                                self.begin_workspace_search_replacement(replacement);
                            }
                        }
                        None => {}
                    }
                }
                WorkspaceSearchEvent::Failed { generation, error }
                    if self.workspace_search_generation == Some(generation)
                        && self.workspace_search_root_revision
                            == Some(self.workspace.root_revision()) =>
                {
                    self.workspace_search_generation = None;
                    self.workspace_search_workflow = None;
                    self.workspace_search_sequence = 0;
                    self.workspace_search_root_revision = None;
                    self.workspace_search_intent = None;
                    self.workspace_search_matches.clear();
                    self.show_error(
                        self.message("workspace_search_failed", &[("diagnostic", error)]),
                    );
                }
                WorkspaceSearchEvent::Cancelled { generation }
                    if self.workspace_search_generation == Some(generation)
                        && self.workspace_search_root_revision
                            == Some(self.workspace.root_revision()) =>
                {
                    self.workspace_search_generation = None;
                    self.workspace_search_workflow = None;
                    self.workspace_search_sequence = 0;
                    self.workspace_search_root_revision = None;
                    self.workspace_search_intent = None;
                    self.workspace_search_matches.clear();
                }
                _ => {}
            }
        }
    }

    pub(in crate::editor::session) fn open_workspace_search_picker(&mut self) {
        let Some(snapshot) = self
            .workspace_search_workflow
            .as_ref()
            .and_then(|broker| broker.snapshot_if_changed(0))
        else {
            self.show_error("workspace search workflow projection is unavailable");
            return;
        };
        let mut matches = self
            .workspace_search_matches
            .iter()
            .cloned()
            .map(|found| (found.id.clone(), found))
            .collect::<std::collections::BTreeMap<_, _>>();
        let multi_root = self.workspace.roots().len() > 1;
        let items = snapshot
            .items
            .into_iter()
            .filter_map(|item| matches.remove(&item.stable_key))
            .map(|found| {
                let item = PickerItem {
                    id: PickerItemId(found.id.clone()),
                    label: self.message(
                        "workspace_search_row",
                        &[
                            ("path", found.relative_path.display().to_string()),
                            ("line", found.line.saturating_add(1).to_string()),
                            ("column", found.column.saturating_add(1).to_string()),
                        ],
                    ),
                    detail: found.line_text.trim().to_owned(),
                    source: if multi_root {
                        found
                            .root
                            .file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
                    } else {
                        String::new()
                    },
                    action: PickerAction::None,
                };
                (item, PickerPayload::WorkspaceSearch(found))
            })
            .collect();
        if self.workspace_search_live {
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                items,
                self.workspace_search_query.clone(),
                self.workspace_search_generation.is_none(),
            );
        } else {
            self.install_picker(self.message("workspace_search_title", &[]), items);
        }
    }

    pub(in crate::editor::session) fn begin_workspace_search_replacement(
        &mut self,
        replacement: String,
    ) {
        if self.pending_workspace_proposal.is_some() || self.workspace_proposal.is_some() {
            self.show_error(self.message("workspace_edit_busy", &[]));
            return;
        }
        let mut grouped = BTreeMap::<PathBuf, (Vec<u8>, Vec<TextEdit>)>::new();
        for found in &self.workspace_search_matches {
            let entry = grouped
                .entry(found.canonical_path.clone())
                .or_insert_with(|| (found.snapshot.to_vec(), Vec::new()));
            if entry.0.as_slice() != found.snapshot.as_ref() {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            }
            let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            else {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            };
            entry.1.push(TextEdit::new(range, replacement.clone()));
        }
        let mut snapshots = Vec::with_capacity(grouped.len());
        let mut drafts = Vec::with_capacity(grouped.len());
        for (path, (searched_bytes, edits)) in grouped {
            let revision = if let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                if buffer.text.to_bytes() != searched_bytes {
                    self.show_error(self.message("workspace_search_stale_result", &[]));
                    return;
                }
                Some(buffer.text.revision())
            } else {
                None
            };
            snapshots.push(FileSnapshot {
                canonical_path: path.clone(),
                revision,
                bytes: searched_bytes,
            });
            drafts.push(WorkspaceEditDraft {
                canonical_path: path,
                expected_revision: revision,
                edits,
            });
        }
        let id = ProposalId(self.next_proposal);
        self.next_proposal = self.next_proposal.saturating_add(1);
        match WorkspaceEditProposal::build(
            id,
            self.message("workspace_replace_label", &[]),
            &self.workspace,
            drafts,
            snapshots,
        ) {
            Ok(proposal) => {
                self.workspace_proposal_response = None;
                self.workspace_proposal_root_revision = Some(self.workspace.root_revision());
                self.workspace_proposal = Some(proposal);
                self.show_workspace_proposal();
            }
            Err(error) => self.show_error(self.message(
                "workspace_edit_failed",
                &[("diagnostic", error.to_string())],
            )),
        }
    }
}
