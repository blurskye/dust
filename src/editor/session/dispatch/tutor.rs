//! The interactive tutor. A card shows the current lesson step and moves on once the step is
//! done: the command it teaches ran, or the practice file now reads as asked. The lessons are
//! `assets/defaults/tutor.toml`; the practice file is `assets/defaults/tutor.md`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::InteractiveApplication;
use crate::editor::text::ByteOffset;
use crate::editor::{BufferId, CommandInvocation, CommandValue, EditorMode};
use crate::ui::window::WindowPlacement;

const LESSONS: &str = include_str!("../../../../assets/defaults/tutor.toml");
const PRACTICE: &str = include_str!("../../../../assets/defaults/tutor.md");
const FOOTER: &str = ":TutorNext skip · :TutorBack back · :TutorQuit stop";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lessons {
    step: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Step {
    chapter: String,
    text: String,
    #[serde(default)]
    pub(super) done: Vec<String>,
    line: Option<String>,
    gone: Option<String>,
    cursor: Option<String>,
    pub(super) at: Option<String>,
}

impl Step {
    fn checks_text(&self) -> bool {
        self.line.is_some() || self.gone.is_some() || self.cursor.is_some()
    }
}

pub(super) fn lessons() -> Result<Vec<Step>, String> {
    toml::from_str::<Lessons>(LESSONS)
        .map(|lessons| lessons.step)
        .map_err(|error| format!("the tutor lessons are broken: {error}"))
}

pub(super) struct Tutor {
    steps: Vec<Step>,
    /// The step on the card, while the tutor runs.
    current: Option<usize>,
    /// The step's command ran since the last check.
    observed: bool,
    /// The previous step was just finished, so the card says so.
    praised: bool,
    /// The step's `at` line still has to be shown; dropped at the next key so a late jump
    /// never pulls the cursor away from what the user is doing.
    jump: bool,
    /// Editor revision the text checks last looked at.
    checked: Option<u64>,
    practice: PathBuf,
    progress: PathBuf,
}

impl Tutor {
    pub(super) fn new(state_directory: &Path) -> Self {
        Self {
            steps: Vec::new(),
            current: None,
            observed: false,
            praised: false,
            jump: false,
            checked: None,
            practice: state_directory.join("tutor.md"),
            progress: state_directory.join("tutor-step"),
        }
    }

    fn step(&self) -> Option<&Step> {
        self.current.and_then(|index| self.steps.get(index))
    }
}

/// `pattern` is a command id followed by `key=value` arguments the invocation must carry.
fn invocation_matches(pattern: &str, invocation: &CommandInvocation) -> bool {
    let mut parts = pattern.split_whitespace();
    parts.next() == Some(invocation.id.as_str())
        && parts.all(|pair| {
            pair.split_once('=').is_some_and(|(key, value)| {
                invocation.arguments.get(key).is_some_and(|actual| match actual {
                    CommandValue::String(text) => text == value,
                    CommandValue::Bool(flag) => flag.to_string() == value,
                    CommandValue::Integer(number) => number.to_string() == value,
                })
            })
        })
}

impl InteractiveApplication {
    /// `:Tutor`: open the practice file and show the step the user stopped at.
    pub(in crate::editor::session) fn open_tutor(&mut self) {
        if self.tutor.steps.is_empty() {
            match lessons() {
                Ok(steps) => self.tutor.steps = steps,
                Err(error) => return self.show_error(error),
            }
        }
        // An open practice buffer keeps the user's edits; otherwise start from a fresh copy.
        if self.tutor_buffer().is_none() {
            let path = self.tutor.practice.clone();
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, PRACTICE));
            if let Err(error) = written {
                return self.show_error(format!("could not write the tutorial: {error}"));
            }
        }
        self.set_dashboard_visibility(false);
        self.start_open(self.tutor.practice.clone());
        let saved = std::fs::read_to_string(&self.tutor.progress)
            .ok()
            .and_then(|text| text.trim().parse::<usize>().ok())
            .filter(|step| *step < self.tutor.steps.len())
            .unwrap_or(0);
        self.tutor_go(saved, false);
    }

    pub(in crate::editor::session) fn tutor_next(&mut self) {
        if let Some(index) = self.tutor.current {
            self.tutor_go(index + 1, false);
        } else {
            self.notify_message("The tutor is not running: :Tutor starts it.".into(), false);
        }
    }

    pub(in crate::editor::session) fn tutor_back(&mut self) {
        if let Some(index) = self.tutor.current {
            self.tutor_go(index.saturating_sub(1), false);
        }
    }

    pub(in crate::editor::session) fn tutor_quit(&mut self) {
        if self.tutor.current.take().is_some() {
            self.ui.tutor = None;
            self.notify_message("Tutor paused: :Tutor picks up here.".into(), false);
        }
    }

    /// Called for every command before it runs.
    pub(super) fn tutor_observe(&mut self, invocation: &CommandInvocation) {
        let Some(step) = self.tutor.step() else {
            return;
        };
        if step.done.iter().any(|pattern| invocation_matches(pattern, invocation)) {
            self.tutor.observed = true;
        }
        if !invocation.id.as_str().starts_with("session.tutor") {
            self.tutor.jump = false;
        }
    }

    /// Called once per main loop pass: finish the step when it is done, and keep the card in
    /// sync with what is on screen.
    pub(super) fn tutor_tick(&mut self) {
        let Some(index) = self.tutor.current else {
            return;
        };
        self.tutor_show_line();
        let observed = std::mem::take(&mut self.tutor.observed);
        let done = if self.tutor.steps[index].checks_text() {
            let revision = self.state.revision;
            self.tutor.checked != Some(revision) && {
                self.tutor.checked = Some(revision);
                self.tutor_text_done(index)
            }
        } else {
            observed
        };
        if done {
            self.tutor_go(index + 1, true);
        } else {
            self.sync_tutor_card();
        }
    }

    fn tutor_go(&mut self, index: usize, praised: bool) {
        self.tutor.observed = false;
        self.tutor.checked = None;
        if index >= self.tutor.steps.len() {
            self.tutor.current = None;
            self.ui.tutor = None;
            let _ = std::fs::remove_file(&self.tutor.progress);
            self.notify_message("Tutor finished. :Tutor starts it again.".into(), false);
            return;
        }
        self.tutor.current = Some(index);
        self.tutor.praised = praised;
        self.tutor.jump = self.tutor.steps[index].at.is_some();
        let _ = std::fs::write(&self.tutor.progress, index.to_string());
        self.tutor_show_line();
        self.sync_tutor_card();
    }

    fn tutor_show_line(&mut self) {
        if self.tutor.jump && self.tutor_jump() {
            self.tutor.jump = false;
        }
    }

    fn sync_tutor_card(&mut self) {
        let card = self.tutor.current.and_then(|index| {
            // A floating terminal such as lazygit has the screen; the card waits behind it.
            let floating = self.window_session.focused_terminal().is_some_and(|id| {
                matches!(
                    self.window_session.native_ids().terminal_placement(id),
                    Some(WindowPlacement::Floating(_))
                )
            });
            let step = &self.tutor.steps[index];
            (!floating).then(|| crate::ui::TutorCard {
                title: format!(
                    "Tutor {}/{} · {}",
                    index + 1,
                    self.tutor.steps.len(),
                    step.chapter
                ),
                text: step.text.clone(),
                praised: self.tutor.praised,
                footer: FOOTER.into(),
            })
        });
        self.ui.set(card, |ui| &mut ui.tutor);
    }

    fn tutor_buffer(&self) -> Option<BufferId> {
        let canonical = std::fs::canonicalize(&self.tutor.practice).ok();
        self.state.buffers.iter().find_map(|(id, buffer)| {
            let path = buffer.path.as_deref()?;
            (path == self.tutor.practice || Some(path) == canonical.as_deref()).then_some(id)
        })
    }

    fn tutor_text_done(&self, index: usize) -> bool {
        let step = &self.tutor.steps[index];
        let Some(buffer) = self.tutor_buffer() else {
            return false;
        };
        let active = self.state.active_view.and_then(|view| self.state.views.get(view));
        let text = &self.state.buffers[buffer].text;
        if let Some(expected) = &step.cursor {
            let Some(view) = active.filter(|view| view.buffer == buffer) else {
                return false;
            };
            let end = ByteOffset((view.cursor.get() + expected.len()).min(text.len_bytes()));
            let under = text
                .slice(crate::editor::text::ByteRange {
                    start: view.cursor,
                    end,
                })
                .unwrap_or_default();
            if under != *expected {
                return false;
            }
        }
        if step.line.is_none() && step.gone.is_none() {
            return true;
        }
        // Text is judged once the user is back in Normal mode, not halfway through typing.
        if active.is_some_and(|view| view.mode != EditorMode::Normal) {
            return false;
        }
        let lines: Vec<String> = (0..text.len_lines())
            .filter_map(|line| text.line(line).ok())
            .collect();
        let has = |wanted: &str| lines.iter().any(|line| line.trim() == wanted);
        step.line.as_deref().is_none_or(has) && step.gone.as_deref().is_none_or(|gone| !has(gone))
    }

    /// Put the cursor on the step's practice line, when the practice file is in front.
    fn tutor_jump(&mut self) -> bool {
        let Some(index) = self.tutor.current else {
            return true;
        };
        let Some(at) = self.tutor.steps[index].at.clone() else {
            return true;
        };
        let Some(buffer) = self.tutor_buffer() else {
            return false;
        };
        let Some(view_id) = self.state.active_view else {
            return false;
        };
        let Some(view) = self.state.views.get(view_id) else {
            return false;
        };
        if view.buffer != buffer || view.mode != EditorMode::Normal {
            return false;
        }
        let text = &self.state.buffers[buffer].text;
        let Some((line, indent)) = (0..text.len_lines()).find_map(|line| {
            let content = text.line(line).ok()?;
            (content.trim() == at)
                .then(|| (line, content.len() - content.trim_start().len()))
        }) else {
            return true;
        };
        let Ok(start) = text.line_to_byte(line) else {
            return true;
        };
        let view = &mut self.state.views[view_id];
        view.cursor = ByteOffset(start.get() + indent);
        view.preferred_column = None;
        view.selection = None;
        view.scroll_line = line.saturating_sub(6);
        view.scroll_wrap_row = 0;
        self.state.mark_editor_changed();
        self.ensure_cursor_visible();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lesson_checks_a_real_command_and_practice_line() {
        let steps = lessons().expect("lessons parse");
        assert!(steps.len() > 40);
        let mut registry = crate::editor::CommandRegistry::default();
        crate::editor::register_builtin_commands(&mut registry).expect("builtin commands");
        super::super::register_app_commands(&mut registry).expect("app commands");
        let practice: Vec<&str> = PRACTICE.lines().map(str::trim).collect();
        for step in &steps {
            for pattern in &step.done {
                let id = pattern.split_whitespace().next().unwrap_or_default();
                assert!(
                    registry.specs().any(|spec| spec.id.as_str() == id),
                    "tutor step checks the unknown command {id}"
                );
            }
            for line in step.at.iter().chain(&step.gone) {
                assert!(practice.contains(&line.as_str()), "tutor.md has no line {line:?}");
            }
        }
    }

    #[test]
    fn patterns_match_the_command_and_its_arguments() {
        let invocation = CommandInvocation::new("input.motion", crate::editor::InvocationSource::Keymap)
            .expect("id")
            .with_argument("motion", CommandValue::String("down".into()))
            .with_argument("linewise", CommandValue::Bool(false));
        assert!(invocation_matches("input.motion", &invocation));
        assert!(invocation_matches("input.motion motion=down", &invocation));
        assert!(invocation_matches("input.motion motion=down linewise=false", &invocation));
        assert!(!invocation_matches("input.motion motion=up", &invocation));
        assert!(!invocation_matches("input.find.begin", &invocation));
    }
}
