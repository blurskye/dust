use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_core_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Quit { force } => {
                if !force && self.state.buffers.iter().any(|(_, buffer)| buffer.dirty) {
                    self.show_error(self.message("unsaved_quit", &[]));
                } else {
                    if force {
                        self.queue_dirty_recovery_before_exit();
                    }
                    self.should_quit = true;
                }
            }
            AppEffect::Save {
                quit_after,
                path,
                force,
            } => self.start_save(quit_after, path, force),
            AppEffect::OpenPath(path) => self.start_open(path),
            AppEffect::Reload { force } => self.start_reload(force),
            AppEffect::CloseBuffer { force } => self.close_active_buffer(force),
            AppEffect::CycleBuffer(offset) => match self.state.cycle_buffer(
                offset,
                self.configuration.settings.editor.buffer_navigation_wrap,
            ) {
                Ok(_) => {
                    self.ui.dashboard = false;
                }
                Err(error) => self.show_error(error),
            },
            AppEffect::ToggleBufferPin => self.toggle_active_buffer_pin(),
            AppEffect::MoveBufferLine(direction) => self.move_active_buffer_line(direction),
            AppEffect::NewBuffer => {
                self.state.open_text(TextBuffer::from_text(""), None);
                self.ui.dashboard = false;
                self.ui.notification = Some(self.message("new_scratch", &[]));
                self.ui.notification_error = false;
            }
            AppEffect::OpenWorkspaceFilePicker => self.start_workspace_file_picker(),
            AppEffect::GitStatusRefresh => self.start_git_status_refresh(),
            AppEffect::GitStatusOpen => self.open_git_status_picker(),
            AppEffect::GitHunksOpen => self.start_git_hunks(),
            AppEffect::GitHunkStage => self.stage_selected_git_hunk(),
            AppEffect::GitHunkReset => self.request_reset_selected_git_hunk(),
            AppEffect::OpenFilePrompt => {
                let alias = self
                    .configuration
                    .settings
                    .input
                    .command_aliases
                    .iter()
                    .find(|(_, command)| command.as_str() == "file.open")
                    .map_or("file.open", |(alias, _)| alias.as_str());
                self.open_prompt(PromptKind::Command, format!("{alias} "));
            }
            AppEffect::OpenPrompt { kind, initial } => self.open_prompt(kind, initial),
            AppEffect::PromptInsert(text) => self.prompt_insert(&text),
            AppEffect::PromptBackspace => self.prompt_backspace(),
            AppEffect::PromptMove(movement) => self.prompt_move(movement),
            AppEffect::PromptHistory(direction) => self.prompt_history(direction),
            AppEffect::PromptComplete(direction) => self.prompt_complete(direction),
            AppEffect::PromptSubmit => self.submit_prompt(),
            AppEffect::PromptCancel => self.close_prompt(),
            AppEffect::SearchRepeat { reverse } => {
                let forward = if reverse {
                    !self.last_search_forward
                } else {
                    self.last_search_forward
                };
                self.perform_search(forward);
            }
            AppEffect::SearchWord { forward } => self.search_word_under_cursor(forward),
            AppEffect::Scroll {
                kind,
                count,
                at_mouse,
            } => {
                if at_mouse {
                    self.focus_mouse_pane();
                }
                self.scroll_view(&kind, count);
            }
            AppEffect::PlaceMouseCursor => self.apply_mouse_cursor(false),
            AppEffect::ExtendMouseSelection => self.apply_mouse_cursor(true),
            AppEffect::Layout(action) => self.apply_layout_action(action),
            AppEffect::SurfaceFocus(direction) => self.focus_surface(direction),
            AppEffect::RetainedUiFocus(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Focus(delta));
            }
            AppEffect::RetainedUiScroll(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Scroll(delta));
            }
            AppEffect::RetainedUiActivate => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Activate);
            }
            AppEffect::RetainedUiMouseSelect => self.select_retained_ui_at_mouse(),
            AppEffect::PrivateUiActivate { at_mouse } => {
                self.activate_private_surface_action(at_mouse);
            }
            AppEffect::WindowDock { target, edge, size } => {
                let edge = match edge.as_str() {
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    "top" => DockEdge::Top,
                    "bottom" => DockEdge::Bottom,
                    _ => {
                        self.show_error("window dock edge must be left, right, top, or bottom");
                        return Ok(());
                    }
                };
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(
                            target,
                            WindowPlacement::Docked { edge, size },
                        )
                    });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFloat {
                target,
                x,
                y,
                width,
                height,
            } => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(
                            target,
                            WindowPlacement::Floating(WindowRect {
                                x,
                                y,
                                width,
                                height,
                            }),
                        )
                    });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFullscreen(target) => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(target, WindowPlacement::Fullscreen)
                    });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowHide(target) => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(target, WindowPlacement::Hidden)
                    });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowShow(target) => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| self.show_native_window(target));
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowClose(target) => {
                if let Err(error) = self.close_graph_window(&target) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowResize { target, amount } => {
                if let Err(error) = self.resize_graph_window(&target, amount) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowResizeDirectional(direction) => {
                if let Err(error) = self.resize_focused_window(direction) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowMove { target, x, y } => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| self.move_native_window(target, x, y));
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowSwap { first, second } => {
                if let Err(error) = self.swap_graph_windows(&first, &second) {
                    self.show_error(error);
                }
            }
            AppEffect::RepeatLast => self.repeat_last_change(),
            AppEffect::MacroRecordStart(register) => {
                self.recording_macro = Some((register, Vec::new()));
                self.ui.notification =
                    Some(self.message("macro_recording", &[("register", register.to_string())]));
                self.ui.notification_error = false;
            }
            AppEffect::MacroRecordStop => {
                if let Some((register, invocations)) = self.recording_macro.take() {
                    self.state.registers.set_macro(register, invocations);
                    self.ui.notification =
                        Some(self.message("macro_recorded", &[("register", register.to_string())]));
                    self.ui.notification_error = false;
                } else {
                    self.show_error(self.message("no_macro_recording", &[]));
                }
            }
            AppEffect::MacroReplay { register, count } => {
                if self.replay_depth == 0 {
                    self.replay_steps = 0;
                }
                for _ in 0..count {
                    self.replay_macro(register);
                    if self.ui.notification_error {
                        break;
                    }
                }
            }
            AppEffect::Grammar(input) => self.process_grammar_input(input),
            AppEffect::CancelPendingMapping => self.cancel_pending_mapping(),
            effect => return Err(effect),
        }
        Ok(())
    }
}
