//! The guided tutor. `:Tutor` puts the user's layout aside, moves the editor into a throwaway
//! practice project and walks through the lessons in `assets/defaults/tutor.toml` in a bar above
//! the status line. Each step sets its scene (file, cursor, windows) and moves on once it is
//! done: the command it teaches ran, or the step's file reads as asked. While the user is
//! outside the practice project the tutor waits and its bar is gone. `:TutorQuit` or the last
//! step moves the editor back and restores the layout; the practice project is deleted.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{InteractiveApplication, PersistedSession, RestoreContext};
use crate::editor::text::{ByteOffset, ByteRange};
use crate::editor::{BufferId, CommandInvocation, CommandValue, EditorMode, InvocationSource};
use crate::ui::UiSyntaxSpan;

const LESSONS: &str = include_str!("../../../../assets/defaults/tutor.toml");
const PRACTICE: &str = include_str!("../../../../assets/defaults/tutor/practice.md");
const MAIN: &str = include_str!("../../../../assets/defaults/tutor/src/main.rs");
const GREET: &str = include_str!("../../../../assets/defaults/tutor/src/greet.rs");
const MANIFEST: &str = "[package]\nname = \"practice\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const FOOTER: &str = ":TutorNext skip · :TutorBack back · :TutorQuit quit";
/// How long the bar says a step is done before the next one comes.
const PRAISE: Duration = Duration::from_millis(700);

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
    /// A line that must be there at least twice, as after a copy and paste.
    twice: Option<String>,
    /// A line that must be indented further than the practice blocks are.
    indented: Option<String>,
    cursor: Option<String>,
    open: Option<String>,
    pub(super) at: Option<String>,
    mark: Option<String>,
    split: Option<bool>,
    explorer: Option<bool>,
    terminal: Option<bool>,
}

impl Step {
    fn checks_text(&self) -> bool {
        self.line.is_some()
            || self.gone.is_some()
            || self.twice.is_some()
            || self.indented.is_some()
            || self.cursor.is_some()
    }

    /// The practice file the step's text checks and cursor are about.
    fn file(&self) -> &str {
        self.open.as_deref().unwrap_or("practice.md")
    }

    /// The file the step brings to the front when it starts.
    fn scene_file(&self) -> Option<&str> {
        (self.open.is_some() || self.at.is_some() || self.mark.is_some()).then(|| self.file())
    }
}

pub(super) fn lessons() -> Result<Vec<Step>, String> {
    toml::from_str::<Lessons>(LESSONS)
        .map(|lessons| lessons.step)
        .map_err(|error| format!("the tutor lessons are broken: {error}"))
}

/// greet.rs as the git lessons find it: changed since the commit.
fn changed_greet() -> String {
    GREET.replacen("Hello, {name}!", "Hello there, {name}!", 1)
}

/// The practice project's files as they are committed.
fn practice_files() -> [(&'static str, &'static str); 5] {
    [
        ("practice.md", PRACTICE),
        ("Cargo.toml", MANIFEST),
        (".gitignore", "target/\n"),
        ("src/main.rs", MAIN),
        ("src/greet.rs", GREET),
    ]
}

/// Write the practice project to a new temporary folder and commit it, then change greet.rs so
/// the git lessons have something to show.
fn create_practice_project() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let folder = tempfile::Builder::new().prefix("dust-tutor-").tempdir()?;
    for (name, text) in practice_files() {
        let path = folder.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
    }
    let repository = git2::Repository::init(folder.path())?;
    let mut index = repository.index()?;
    index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    let tree = repository.find_tree(index.write_tree()?)?;
    let signature = git2::Signature::now("Dust tutor", "tutor@dust.invalid")?;
    repository.commit(Some("HEAD"), &signature, &signature, "Practice project", &tree, &[])?;
    std::fs::write(folder.path().join("src/greet.rs"), changed_greet())?;
    Ok(folder)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    /// The editor is moving into the practice project.
    Entering,
    Running,
    /// The editor is moving back to the user's project.
    Leaving,
}

/// The practice project while the tutor uses it. Dropping it deletes the folder.
struct Practice {
    _folder: tempfile::TempDir,
    root: PathBuf,
    /// The user's project and layout, put back when the tutor ends.
    home: PathBuf,
    layout: Option<PersistedSession>,
    dashboard: bool,
    stage: Stage,
}

pub(super) struct Tutor {
    steps: Vec<Step>,
    /// The step in the bar, while the tutor runs.
    current: Option<usize>,
    /// The step's command ran since the last check.
    observed: bool,
    /// When the current step was done; the next one comes after `PRAISE`.
    done_at: Option<Instant>,
    /// The step's scene still has to place the cursor: it does once its file is in front in
    /// Normal mode.
    jump: bool,
    target: Option<UiSyntaxSpan>,
    /// Editor revision the text checks last looked at.
    checked: Option<u64>,
    progress: PathBuf,
    practice: Option<Practice>,
}

impl Tutor {
    pub(super) fn new(state_directory: &Path) -> Self {
        Self {
            steps: Vec::new(),
            current: None,
            observed: false,
            done_at: None,
            jump: false,
            target: None,
            checked: None,
            progress: state_directory.join("tutor-step"),
            practice: None,
        }
    }

    fn step(&self) -> Option<&Step> {
        self.current.and_then(|index| self.steps.get(index))
    }

    fn saved_step(&self) -> usize {
        std::fs::read_to_string(&self.progress)
            .ok()
            .and_then(|text| text.trim().parse::<usize>().ok())
            .filter(|step| *step < self.steps.len())
            .unwrap_or(0)
    }

    /// The user's own layout while the tutor has the editor in its practice project.
    pub(super) fn saved_layout(&self) -> Option<&Option<PersistedSession>> {
        self.practice.as_ref().map(|practice| &practice.layout)
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
    /// `:Tutor`: move into a fresh practice project and show the step the user stopped at, or
    /// go back into the practice project if the tutor already runs.
    pub(in crate::editor::session) fn open_tutor(&mut self) {
        if self.tutor.steps.is_empty() {
            match lessons() {
                Ok(steps) => self.tutor.steps = steps,
                Err(error) => return self.show_error(error),
            }
        }
        if let Some(practice) = &self.tutor.practice {
            match practice.stage {
                Stage::Running if self.state.workspace_root != practice.root => {
                    let root = practice.root.clone();
                    self.switch_workspace_root(root);
                }
                Stage::Running => {
                    let index = self.tutor.current.unwrap_or_else(|| self.tutor.saved_step());
                    let back = practice.root.join(self.tutor.steps[index].file());
                    self.tutor_go(index, false);
                    // Coming back from a file of the user's own shows the step's file again.
                    if !self.tutor_inside() {
                        self.start_open(back);
                    }
                }
                Stage::Entering | Stage::Leaving => {}
            }
            return;
        }
        let layout = match self.try_capture_session() {
            Ok(layout) => layout,
            Err(error) => return self.show_error(format!("could not keep your layout: {error}")),
        };
        let folder = match create_practice_project() {
            Ok(folder) => folder,
            Err(error) => {
                return self.show_error(format!("could not make the practice project: {error}"));
            }
        };
        let root = folder.path().canonicalize().unwrap_or_else(|_| folder.path().to_path_buf());
        let home = self
            .state
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.state.workspace_root.clone());
        self.switch_workspace_root(root.clone());
        // Unsaved files block the move; the error says so and the tutor doesn't start.
        if !self.workspace.root_operation_pending() {
            return;
        }
        self.tutor.practice = Some(Practice {
            _folder: folder,
            root,
            home,
            layout,
            dashboard: self.ui.dashboard,
            stage: Stage::Entering,
        });
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
        if self.tutor.practice.is_none() {
            self.notify_message("The tutor is not running: :Tutor starts it.".into(), false);
            return;
        }
        self.notify_message("Tutor stopped: :Tutor picks up at this step.".into(), false);
        self.tutor_leave();
    }

    /// Called for every command that ran without an error.
    pub(super) fn tutor_observe(&mut self, invocation: &CommandInvocation) {
        let Some(step) = self.tutor.step() else {
            return;
        };
        if step.done.iter().any(|pattern| invocation_matches(pattern, invocation)) {
            self.tutor.observed = true;
        }
    }

    /// Called once per main loop pass: follow the moves in and out of the practice project,
    /// finish the step when it is done and keep the bar in sync with what is on screen.
    pub(super) fn tutor_tick(&mut self) {
        let Some(practice) = &mut self.tutor.practice else {
            return;
        };
        if self.workspace.root_operation_pending() {
            return;
        }
        match practice.stage {
            Stage::Entering if self.state.workspace_root == practice.root => {
                practice.stage = Stage::Running;
                let index = self.tutor.saved_step();
                self.tutor_go(index, false);
            }
            // The move failed and said why.
            Stage::Entering => self.tutor.practice = None,
            Stage::Leaving if self.state.workspace_root == practice.home => self.tutor_restore(),
            Stage::Leaving => {
                practice.stage = Stage::Running;
                self.show_error("could not leave the practice project: :TutorQuit tries again");
            }
            Stage::Running => self.tutor_step_tick(),
        }
    }

    fn tutor_step_tick(&mut self) {
        let Some(index) = self.tutor.current else {
            return;
        };
        if !self.tutor_inside() {
            self.tutor.observed = false;
            self.ui.set(None, |ui| &mut ui.tutor);
            return;
        }
        if let Some(done_at) = self.tutor.done_at {
            if done_at.elapsed() >= PRAISE {
                self.tutor_go(index + 1, true);
            }
            return;
        }
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
            self.tutor.done_at = Some(Instant::now());
        }
        self.sync_tutor_bar();
    }

    /// The editor shows the practice project: its folder is the workspace and the file in
    /// front, if it is a file, is one of its files.
    fn tutor_inside(&self) -> bool {
        let Some(practice) = &self.tutor.practice else {
            return false;
        };
        self.state.workspace_root == practice.root
            && self
                .state
                .active_buffer_id()
                .and_then(|id| self.state.buffers[id].path.as_deref())
                .is_none_or(|path| path.starts_with(&practice.root))
    }

    fn tutor_go(&mut self, index: usize, praised: bool) {
        self.tutor.observed = false;
        self.tutor.done_at = None;
        self.tutor.target = None;
        if index >= self.tutor.steps.len() {
            let _ = std::fs::remove_file(&self.tutor.progress);
            self.notify_message("Tutor finished. :Tutor starts it again.".into(), false);
            self.tutor_leave();
            return;
        }
        self.tutor.current = Some(index);
        let _ = std::fs::write(&self.tutor.progress, index.to_string());
        self.tutor_scene(index);
        // The scene's own commands don't count as the user doing the step.
        self.tutor.observed = false;
        // A step reached by finishing the last one is checked at once; one picked with
        // :TutorBack or :TutorNext waits for the next change, so a step already done in the
        // practice text doesn't skip itself.
        self.tutor.checked = (!praised).then_some(self.state.revision);
        self.tutor_show_line();
        self.sync_tutor_bar();
    }

    /// Set up the windows and file the step starts from.
    fn tutor_scene(&mut self, index: usize) {
        let Some(root) = self.tutor.practice.as_ref().map(|practice| practice.root.clone()) else {
            return;
        };
        let step = &self.tutor.steps[index];
        let (split, explorer, terminal) = (step.split, step.explorer, step.terminal);
        let file = step.scene_file().map(|file| root.join(file));
        self.tutor.jump = step.at.is_some() || step.mark.is_some();
        if self.ui.dashboard {
            self.set_dashboard_visibility(false);
        }
        if split == Some(false) {
            if self.state.is_zoomed() {
                self.tutor_command("pane.zoom");
            }
            for _ in 0..self.state.views.len() {
                if self.state.active_tab_views().len() <= 1 {
                    break;
                }
                self.tutor_command("pane.close");
            }
        }
        if split == Some(true) && self.state.active_tab_views().len() <= 1 {
            self.tutor_command("pane.split.vertical");
        }
        match terminal {
            Some(true) if self.ui.terminal_panel.is_none() => self.tutor_command("terminal.toggle"),
            Some(false) if self.ui.terminal_panel.is_some() => self.tutor_command("terminal.hide"),
            _ => {}
        }
        match explorer {
            Some(true) if self.ui.explorer.is_none() => self.open_explorer(),
            Some(false) if self.ui.explorer.is_some() => self.tutor_command("explorer.close"),
            _ => {}
        }
        if let Some(path) = file {
            let in_front = self
                .state
                .active_buffer_id()
                .and_then(|id| self.state.buffers[id].path.as_deref())
                == Some(path.as_path());
            if !in_front {
                self.start_open(path);
            }
        }
    }

    fn tutor_command(&mut self, id: &str) {
        match CommandInvocation::new(id, InvocationSource::Keymap) {
            Ok(invocation) => self.invoke(invocation),
            Err(error) => self.show_error(error.to_string()),
        }
    }

    /// Leave the practice project: its edits are saved there, the editor moves back to the
    /// user's project and `tutor_restore` puts the layout back.
    fn tutor_leave(&mut self) {
        let Some(practice) = &self.tutor.practice else {
            return;
        };
        if practice.stage != Stage::Running {
            return;
        }
        let (root, home) = (practice.root.clone(), practice.home.clone());
        self.tutor.current = None;
        self.ui.set(None, |ui| &mut ui.tutor);
        // Unsaved practice files would block the move; they are thrown away with the folder.
        let unsaved = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.dirty)
            .filter_map(|(id, buffer)| {
                let path = buffer.path.clone().filter(|path| path.starts_with(&root))?;
                Some((id, path, buffer.text.revision(), buffer.text.to_bytes()))
            })
            .collect::<Vec<_>>();
        for (id, path, revision, bytes) in unsaved {
            if std::fs::write(&path, &bytes).is_ok() {
                self.state.mark_buffer_saved_as(id, revision, bytes, Some(path));
            }
        }
        if let Some(practice) = &mut self.tutor.practice {
            practice.stage = Stage::Leaving;
        }
        if self.state.workspace_root != home {
            self.switch_workspace_root(home);
        }
    }

    /// Back in the user's project: put their layout back and delete the practice project.
    fn tutor_restore(&mut self) {
        let Some(practice) = self.tutor.practice.take() else {
            return;
        };
        if let Err(error) = self.workspace.forget_folder(&practice.root) {
            self.show_workspace_failure(error);
        }
        if let Some(layout) = practice.layout {
            self.persistence.allow_restore(RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            });
            self.restore_session(layout);
        }
        if practice.dashboard {
            self.set_dashboard_visibility(true);
        }
    }

    fn tutor_show_line(&mut self) {
        if self.tutor.jump && self.tutor_jump() {
            self.tutor.jump = false;
        }
    }

    fn sync_tutor_bar(&mut self) {
        let bar = self.tutor.current.filter(|_| self.tutor_inside()).map(|index| {
            let step = &self.tutor.steps[index];
            crate::ui::TutorBar {
                title: format!("Tutor {}/{} · {}", index + 1, self.tutor.steps.len(), step.chapter),
                text: step.text.clone(),
                praised: self.tutor.done_at.is_some(),
                footer: FOOTER.into(),
                target: self.tutor.target.clone(),
            }
        });
        self.ui.set(bar, |ui| &mut ui.tutor);
    }

    fn tutor_buffer(&self, file: &str) -> Option<BufferId> {
        let path = self.tutor.practice.as_ref()?.root.join(file);
        self.state
            .buffers
            .iter()
            .find_map(|(id, buffer)| (buffer.path.as_deref() == Some(path.as_path())).then_some(id))
    }

    fn tutor_text_done(&self, index: usize) -> bool {
        let step = &self.tutor.steps[index];
        let Some(buffer) = self.tutor_buffer(step.file()) else {
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
                .slice(ByteRange {
                    start: view.cursor,
                    end,
                })
                .unwrap_or_default();
            if under != *expected {
                return false;
            }
        }
        if step.line.is_none()
            && step.gone.is_none()
            && step.twice.is_none()
            && step.indented.is_none()
        {
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
        let twice = |wanted: &str| lines.iter().filter(|line| line.trim() == wanted).count() >= 2;
        // Practice lines sit four columns in; more than that is indented.
        let indented = |wanted: &str| {
            lines.iter().any(|line| {
                line.trim() == wanted
                    && line.len() - line.trim_start().len() > 4
            })
        };
        step.line.as_deref().is_none_or(has)
            && step.gone.as_deref().is_none_or(|gone| !has(gone))
            && step.twice.as_deref().is_none_or(twice)
            && step.indented.as_deref().is_none_or(indented)
    }

    /// Put the cursor on the step's `at` line and mark its `mark` text, once the step's file is
    /// in front in Normal mode.
    fn tutor_jump(&mut self) -> bool {
        let Some(index) = self.tutor.current else {
            return true;
        };
        let step = &self.tutor.steps[index];
        let (at, mark) = (step.at.clone(), step.mark.clone());
        let Some(buffer) = self.tutor_buffer(step.file()) else {
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
        let found = at.and_then(|at| {
            (0..text.len_lines()).find_map(|line| {
                let content = text.line(line).ok()?;
                (content.trim() == at).then(|| (line, content.len() - content.trim_start().len()))
            })
        });
        let from = found
            .and_then(|(line, _)| text.line_to_byte(line).ok())
            .unwrap_or(ByteOffset(0));
        self.tutor.target = mark.and_then(|mark| {
            let content = text.to_string();
            let start = from.get() + content.get(from.get()..)?.find(&mark)?;
            Some(UiSyntaxSpan {
                buffer,
                revision: text.revision(),
                range: ByteRange {
                    start: ByteOffset(start),
                    end: ByteOffset(start + mark.len()),
                },
                role: crate::language::syntax::Highlight::VARIABLE,
            })
        });
        if let Some((line, indent)) = found {
            let view = &mut self.state.views[view_id];
            view.cursor = ByteOffset(from.get() + indent);
            view.preferred_column = None;
            view.selection = None;
            view.scroll_line = line.saturating_sub(6);
            view.scroll_wrap_row = 0;
            self.state.mark_editor_changed();
            self.ensure_cursor_visible();
        }
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
        let greet = changed_greet();
        let file_text = |name: &str| {
            if name == "src/greet.rs" {
                return greet.as_str();
            }
            practice_files()
                .into_iter()
                .find(|(file, _)| *file == name)
                .map(|(_, text)| text)
                .unwrap_or_else(|| panic!("the practice project has no {name}"))
        };
        for step in &steps {
            for pattern in &step.done {
                let id = pattern.split_whitespace().next().unwrap_or_default();
                assert!(
                    registry.specs().any(|spec| spec.id.as_str() == id),
                    "tutor step checks the unknown command {id}"
                );
            }
            let text = file_text(step.file());
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            for line in step.at.iter().chain(&step.gone) {
                assert!(lines.contains(&line.as_str()), "{} has no line {line:?}", step.file());
            }
            if let Some(mark) = &step.mark {
                assert!(text.contains(mark.as_str()), "{} has no {mark:?}", step.file());
            }
        }
    }

    #[test]
    fn the_practice_project_is_a_repository_with_one_changed_file() {
        let folder = create_practice_project().expect("practice project");
        let repository = git2::Repository::open(folder.path()).expect("repository");
        let statuses = repository.statuses(None).expect("status");
        let changed = statuses
            .iter()
            .filter_map(|entry| entry.path().map(str::to_owned))
            .collect::<Vec<_>>();
        assert_eq!(changed, ["src/greet.rs"]);
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
