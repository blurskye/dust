//! The guided tutor. `:Tutor` asks whether to pick up where the user stopped or start over in
//! Rust, TypeScript or Python, puts the user's layout aside, moves the editor into a throwaway
//! practice project in that language and walks through the lessons in
//! `assets/defaults/tutor.toml` on a card above the status line. Each step sets its scene (file,
//! cursor, windows), says exactly which keys to press and moves on once it is done: the command
//! it teaches ran, or the step's file reads as asked. While the user is outside the practice
//! project the tutor waits and its card is gone. `:TutorQuit` or the last step moves the editor
//! back and restores the layout; the practice project is deleted.

use std::path::Path;

use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;

use crate::app::App;

use crate::persistence::PersistedSession;

use crate::workspace::PickerAction;

use crate::workspace::PickerItem;

use crate::workspace::PickerItemId;

use crate::app::picker::PickerPayload;

use crate::app::persistence::RestoreContext;
use crate::editor::BufferId;
use crate::editor::CommandInvocation;
use crate::editor::CommandValue;
use crate::editor::EditorMode;
use crate::editor::InvocationSource;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::ui::UiSyntaxSpan;

const LESSONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/defaults/tutor.toml"
));
/// The editing practice lines, the same in every language, inside a comment of the practice file.
const PRACTICE_LINES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/defaults/tutor/practice.txt"
));
const FOOTER: &str = ":TutorNext skip · :TutorBack back · :TutorQuit quit";
/// How long the card says a step is done before the next one comes.
const PRAISE: Duration = Duration::from_millis(1200);
/// The pointer before the step's instruction, one frame every `POINTER_FRAME`.
const POINTER: [&str; 6] = [">  ", ">> ", ">>>", " >>", "  >", "   "];
const POINTER_FRAME: Duration = Duration::from_millis(160);

/// The language of the practice project.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum Language {
    Rust,
    TypeScript,
    Python,
}

impl Language {
    const ALL: [Self; 3] = [Self::Rust, Self::TypeScript, Self::Python];

    pub(in crate::app) fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::Python => "Python",
        }
    }

    /// How the language is written in the saved progress.
    pub(in crate::app) fn id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Python => "python",
        }
    }

    fn server(self) -> &'static str {
        match self {
            Self::Rust => "rust-analyzer",
            Self::TypeScript => "typescript-language-server",
            Self::Python => "pyright",
        }
    }

    /// The practice file, then the main and greet files of the code lessons.
    fn practice_names(self) -> [&'static str; 3] {
        match self {
            Self::Rust => ["practice.rs", "src/main.rs", "src/greet.rs"],
            Self::TypeScript => ["practice.ts", "src/main.ts", "src/greet.ts"],
            Self::Python => ["practice.py", "src/main.py", "src/greet.py"],
        }
    }

    /// The practice project's files as they are committed; the practice file still holds the
    /// `PRACTICE_LINES` placeholder.
    fn files(self) -> &'static [(&'static str, &'static str)] {
        macro_rules! practice_file {
            ($name:literal, $path:literal) => {
                (
                    $name,
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/assets/defaults/tutor/",
                        $path
                    )),
                )
            };
        }
        match self {
            Self::Rust => &[
                practice_file!("practice.rs", "rust/practice.rs"),
                practice_file!("src/main.rs", "rust/src/main.rs"),
                practice_file!("src/greet.rs", "rust/src/greet.rs"),
                practice_file!("Cargo.toml", "rust/Cargo.toml.in"),
                practice_file!(".gitignore", "rust/gitignore"),
            ],
            Self::TypeScript => &[
                practice_file!("practice.ts", "typescript/practice.ts"),
                practice_file!("src/main.ts", "typescript/src/main.ts"),
                practice_file!("src/greet.ts", "typescript/src/greet.ts"),
                practice_file!("package.json", "typescript/package.json"),
                practice_file!("tsconfig.json", "typescript/tsconfig.json"),
                practice_file!(".gitignore", "typescript/gitignore"),
            ],
            Self::Python => &[
                practice_file!("practice.py", "python/practice.py"),
                practice_file!("src/main.py", "python/src/main.py"),
                practice_file!("src/greet.py", "python/src/greet.py"),
                practice_file!("pyproject.toml", "python/pyproject.toml"),
                practice_file!(".gitignore", "python/gitignore"),
            ],
        }
    }

    /// The practice project's files with the practice lines in place.
    fn practice_files(self) -> impl Iterator<Item = (&'static str, String)> {
        self.files()
            .iter()
            .map(|(name, text)| (*name, text.replacen("PRACTICE_LINES\n", PRACTICE_LINES, 1)))
    }

    /// The greet file as the git lessons find it: changed since the commit.
    fn changed_greet(self) -> (&'static str, String) {
        let name = self.practice_names()[2];
        let (_, text) = self
            .files()
            .iter()
            .find(|(file, _)| *file == name)
            .expect("every language has a greet file");
        (name, text.replacen("Hello, ", "Hello there, ", 1))
    }
}

/// Where `:Tutor` starts, as picked in its first list.
#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct TutorStart {
    language: Language,
    step: usize,
}

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
    /// The one thing to do now.
    press: String,
    #[serde(default)]
    pub(super) done: Vec<String>,
    line: Option<String>,
    gone: Option<String>,
    /// A line that must be there at least twice, as after a copy and paste.
    twice: Option<String>,
    /// A line that must be indented further than the practice blocks are.
    indented: Option<String>,
    commented: Option<String>,
    cursor: Option<String>,
    open: Option<String>,
    pub(super) at: Option<String>,
    mark: Option<String>,
    split: Option<bool>,
    explorer: Option<bool>,
    terminal: Option<bool>,
    /// A fresh project needs the preceding lesson's state before this step can be resumed.
    #[serde(default)]
    resume_from_previous: bool,
    /// The language's practice file.
    #[serde(skip)]
    practice: &'static str,
    /// How far in the `indented` line starts in the practice file as it is made.
    #[serde(skip)]
    indent: usize,
}

impl Step {
    fn checks_text(&self) -> bool {
        self.line.is_some()
            || self.gone.is_some()
            || self.twice.is_some()
            || self.indented.is_some()
            || self.commented.is_some()
            || self.cursor.is_some()
    }

    /// The practice file the step's text checks and cursor are about.
    pub(in crate::app) fn file(&self) -> &str {
        self.open.as_deref().unwrap_or(self.practice)
    }

    /// The file the step brings to the front when it starts.
    fn scene_file(&self) -> Option<&str> {
        (self.open.is_some() || self.at.is_some() || self.mark.is_some()).then(|| self.file())
    }
}

/// The steps for a practice project in `language`.
pub(super) fn lessons(language: Language) -> Result<Vec<Step>, String> {
    let mut steps = toml::from_str::<Lessons>(LESSONS)
        .map(|lessons| lessons.step)
        .map_err(|error| format!("the tutor lessons are broken: {error}"))?;
    let [practice, main, greet] = language.practice_names();
    let fill = |text: &str| {
        text.replace("{language}", language.name())
            .replace("{server}", language.server())
            .replace("{practice}", practice)
            .replace("{main}", main)
            .replace("{greet}", greet)
    };
    let files = language.practice_files().collect::<Vec<_>>();
    for step in &mut steps {
        step.text = fill(&step.text);
        step.press = fill(&step.press);
        step.open = step.open.as_deref().map(fill);
        step.practice = practice;
        if let (Some(wanted), Some((_, text))) = (
            &step.indented,
            files.iter().find(|(name, _)| *name == step.file()),
        ) {
            step.indent = text
                .lines()
                .find(|line| line.contains(wanted.as_str()))
                .map(|line| line.len() - line.trim_start().len())
                .ok_or_else(|| format!("{} has no line {wanted:?}", step.file()))?;
        }
    }
    Ok(steps)
}

/// Write the practice project to a new temporary folder and commit it, then change the greet
/// file so the git lessons have something to show.
fn create_practice_project(
    language: Language,
) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let folder = tempfile::Builder::new().prefix("dust-tutor-").tempdir()?;
    for (name, text) in language.practice_files() {
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
    repository.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "Practice project",
        &tree,
        &[],
    )?;
    let (greet, changed) = language.changed_greet();
    std::fs::write(folder.path().join(greet), changed)?;
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
    language: Language,
    /// The step to show once the editor is in the practice project.
    start: usize,
}

pub(super) struct Tutor {
    steps: Vec<Step>,
    /// The step on the card, while the tutor runs.
    current: Option<usize>,
    /// The step's command ran since the last check.
    observed: bool,
    /// When the current step was done; the next one comes after `PRAISE`.
    done_at: Option<Instant>,
    /// When the current step came, for the pointer's frames.
    shown_at: Instant,
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
    pub(in crate::app) fn new(state_directory: &Path) -> Self {
        Self {
            steps: Vec::new(),
            current: None,
            observed: false,
            done_at: None,
            shown_at: Instant::now(),
            jump: false,
            target: None,
            checked: None,
            progress: state_directory.join("tutor-step"),
            practice: None,
        }
    }

    pub(in crate::app) fn step(&self) -> Option<&Step> {
        self.current.and_then(|index| self.steps.get(index))
    }

    /// The step and language the user stopped at, saved as "12 python".
    fn saved_start(&self) -> Option<TutorStart> {
        let text = std::fs::read_to_string(&self.progress).ok()?;
        let mut parts = text.split_whitespace();
        let step = parts.next()?.parse::<usize>().ok()?;
        let language = parts
            .next()
            .and_then(|id| {
                Language::ALL
                    .into_iter()
                    .find(|language| language.id() == id)
            })
            .unwrap_or(Language::Rust);
        Some(TutorStart { language, step })
    }

    fn save_progress(&self, step: usize) -> std::io::Result<()> {
        if let Some(practice) = &self.practice {
            std::fs::write(&self.progress, format!("{step} {}", practice.language.id()))?;
        }
        Ok(())
    }

    /// The user's own layout while the tutor has the editor in its practice project.
    pub(in crate::app) fn saved_layout(&self) -> Option<&Option<PersistedSession>> {
        self.practice.as_ref().map(|practice| &practice.layout)
    }
}

/// `pattern` is a command id followed by `key=value` arguments the invocation must carry.
fn invocation_matches(pattern: &str, invocation: &CommandInvocation) -> bool {
    let mut parts = pattern.split_whitespace();
    parts.next() == Some(invocation.id.as_str())
        && parts.all(|pair| {
            pair.split_once('=').is_some_and(|(key, value)| {
                invocation
                    .arguments
                    .get(key)
                    .is_some_and(|actual| match actual {
                        CommandValue::String(text) => text == value,
                        CommandValue::Bool(flag) => flag.to_string() == value,
                        CommandValue::Integer(number) => number.to_string() == value,
                        CommandValue::Number(number) => number.to_string() == value,
                        CommandValue::List(_) | CommandValue::Record(_) => false,
                    })
            })
        })
}

impl App {
    /// `:Tutor`: go back into the practice project if the tutor already runs; otherwise ask
    /// whether to pick up at the saved step or start over, and in which language.
    pub(in crate::app) fn open_tutor(&mut self) {
        if let Some(practice) = &self.tutor.practice {
            match practice.stage {
                Stage::Running if self.state.workspace_root != practice.root => {
                    let root = practice.root.clone();
                    self.switch_workspace_root(root);
                }
                Stage::Running => {
                    let index = self.tutor.current.unwrap_or(0);
                    let back = practice.root.join(self.tutor.steps[index].file());
                    self.tutor_go(index, false);
                    // Coming back from a file of the user's own shows the step's file again.
                    if !self.tutor_inside() {
                        self.start_open(back);
                    }
                }
                Stage::Entering => {
                    let root = practice.root.clone();
                    self.switch_workspace_root(root);
                }
                Stage::Leaving => {}
            }
            return;
        }
        let steps = match lessons(Language::Rust) {
            Ok(steps) => steps,
            Err(error) => return self.show_error(error),
        };
        // Each choice leads with its language's file icon when the font has them.
        let icons = self.configuration.settings().ui.explorer.icon_mode
            == crate::config::ExplorerIconMode::NerdFont;
        let row = |id: String, label: String, detail: String, start: TutorStart| {
            let file = start.language.practice_names()[0].to_owned();
            let icon = crate::files::file_icon(&file).filter(|_| icons);
            (
                PickerItem {
                    badge: icon.map_or_else(String::new, |icon| icon.glyph.to_owned()),
                    kind: crate::workspace::PickerItemKind::Entry,
                    id: PickerItemId(id),
                    label,
                    detail,
                    source: if icon.is_some() { file } else { String::new() },
                    action: PickerAction::None,
                },
                PickerPayload::Tutor(start),
            )
        };
        let saved = self
            .tutor
            .saved_start()
            .filter(|start| start.step < steps.len());
        let mut items = Vec::new();
        if let Some(start) = saved {
            items.push(row(
                "tutor:continue".into(),
                "Continue where you left off".into(),
                format!(
                    "step {} of {} · {} · {}",
                    start.step + 1,
                    steps.len(),
                    steps[start.step].chapter,
                    start.language.name()
                ),
                start,
            ));
        }
        for language in Language::ALL {
            let label = if saved.is_some() {
                format!("Start over in {}", language.name())
            } else {
                format!("Learn in {}", language.name())
            };
            items.push(row(
                format!("tutor:{}", language.id()),
                label,
                format!("practice in a small {} project", language.name()),
                TutorStart { language, step: 0 },
            ));
        }
        self.install_surface_picker(
            crate::ui::PickerSurfaceKind::Choice,
            "Tutor".to_owned(),
            items,
        );
        self.search
            .set_picker_footer("Enter start · Esc not now".to_owned());
        self.sync_picker_ui();
    }

    /// Move into a fresh practice project in the picked language and show the picked step.
    pub(in crate::app) fn start_tutor(&mut self, start: TutorStart) {
        if self.tutor.practice.is_some() {
            return self.open_tutor();
        }
        match lessons(start.language) {
            Ok(steps) => self.tutor.steps = steps,
            Err(error) => return self.show_error(error),
        }
        // Session restoration cannot replace dirty buffers, including unnamed scratch tabs.
        if self.state.buffers.values().any(|buffer| buffer.dirty) {
            return self.show_error("save or close unsaved buffers before starting the tutor");
        }
        let layout = match self.try_capture_session() {
            Ok(layout) => layout,
            Err(error) => return self.show_error(format!("could not keep your layout: {error}")),
        };
        let folder = match create_practice_project(start.language) {
            Ok(folder) => folder,
            Err(error) => {
                return self.show_error(format!("could not make the practice project: {error}"));
            }
        };
        let root = folder
            .path()
            .canonicalize()
            .unwrap_or_else(|_| folder.path().to_path_buf());
        let home = self
            .state
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.state.workspace_root.clone());
        self.switch_workspace_root(root.clone());
        // Keep the same project while confirming that running terminals may stop.
        if !self.workspace.root_operation_pending()
            && self.switch_ends_terminals.as_ref() != Some(&root)
        {
            return;
        }
        let mut step = start.step.min(self.tutor.steps.len().saturating_sub(1));
        while step > 0 && self.tutor.steps[step].resume_from_previous {
            step -= 1;
        }
        self.tutor.practice = Some(Practice {
            _folder: folder,
            root,
            home,
            layout,
            dashboard: self.ui.dashboard,
            stage: Stage::Entering,
            language: start.language,
            start: step,
        });
    }

    pub(in crate::app) fn tutor_next(&mut self) {
        if let Some(index) = self.tutor.current {
            self.tutor_go(index + 1, false);
        } else {
            self.notify_message("The tutor is not running: :Tutor starts it.".into(), false);
        }
    }

    pub(in crate::app) fn tutor_back(&mut self) {
        if let Some(index) = self.tutor.current {
            self.tutor_go(index.saturating_sub(1), false);
        }
    }

    pub(in crate::app) fn tutor_quit(&mut self) {
        if self.tutor.practice.is_none() {
            self.notify_message("The tutor is not running: :Tutor starts it.".into(), false);
            return;
        }
        self.notify_message("Tutor stopped: :Tutor picks up at this step.".into(), false);
        self.tutor_leave();
    }

    /// Called for every command that ran without an error.
    pub(in crate::app) fn tutor_observe(&mut self, invocation: &CommandInvocation) {
        let Some(step) = self.tutor.step() else {
            return;
        };
        if step
            .done
            .iter()
            .any(|pattern| invocation_matches(pattern, invocation))
        {
            self.tutor.observed = true;
        }
    }

    /// Called once per main loop pass: follow the moves in and out of the practice project,
    /// finish the step when it is done and keep the card in sync with what is on screen.
    pub(in crate::app) fn tutor_tick(&mut self) {
        let Some(practice) = &mut self.tutor.practice else {
            return;
        };
        if self.workspace.root_operation_pending() {
            return;
        }
        match practice.stage {
            Stage::Entering if self.state.workspace_root == practice.root => {
                practice.stage = Stage::Running;
                let index = practice.start;
                self.tutor_go(index, false);
            }
            // The move failed and said why.
            Stage::Entering if self.switch_ends_terminals.as_ref() == Some(&practice.root) => {}
            Stage::Entering => self.tutor.practice = None,
            Stage::Leaving if self.state.workspace_root == practice.home => self.tutor_restore(),
            Stage::Leaving => {
                // A refused or failed switch keeps the practice buffers alive, so their
                // unsaved edits must still be marked dirty.
                for buffer in self.state.buffers.values_mut() {
                    if buffer
                        .path
                        .as_ref()
                        .is_some_and(|path| path.starts_with(&practice.root))
                    {
                        buffer.refresh_dirty();
                    }
                }
                self.state.mark_window_changed();
                practice.stage = Stage::Running;
                self.sync_tutor_card();
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
            // A file opened from outside the practice project keeps the card, which says how
            // to get back.
            self.sync_tutor_card();
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
        // Trust is persisted asynchronously after confirmation; dispatching :trust is not
        // enough, since the user can decline or the write can fail.
        let done = if self.tutor.steps[index]
            .done
            .iter()
            .any(|command| command == "workspace.trust")
        {
            self.workspace.trust() == crate::workspace::WorkspaceTrust::Trusted
        } else if self.tutor.steps[index].checks_text() {
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
        self.sync_tutor_card();
    }

    /// The practice project's folder is the workspace.
    fn tutor_in_project(&self) -> bool {
        self.tutor
            .practice
            .as_ref()
            .is_some_and(|practice| self.state.workspace_root == practice.root)
    }

    /// The editor shows the practice project: its folder is the workspace and the file in
    /// front, if it is a file, is one of its files.
    fn tutor_inside(&self) -> bool {
        let Some(practice) = &self.tutor.practice else {
            return false;
        };
        self.tutor_in_project()
            && self
                .state
                .active_buffer_id()
                .and_then(|id| self.state.buffers[id].path.as_deref())
                .is_none_or(|path| path.starts_with(&practice.root))
    }

    fn tutor_go(&mut self, index: usize, praised: bool) {
        if !self.tutor_in_project() {
            self.notify_message(
                "The tutor is paused: :Tutor returns to the practice project.".into(),
                false,
            );
            return;
        }
        self.tutor.observed = false;
        self.tutor.done_at = None;
        self.tutor.shown_at = Instant::now();
        self.tutor.target = None;
        if index >= self.tutor.steps.len() {
            let _ = std::fs::remove_file(&self.tutor.progress);
            self.notify_message("Tutor finished. :Tutor starts it again.".into(), false);
            self.tutor_leave();
            return;
        }
        self.tutor.current = Some(index);
        let saved = self.tutor.save_progress(index);
        self.tutor_scene(index);
        // The scene's own commands don't count as the user doing the step.
        self.tutor.observed = false;
        // A step reached by finishing the last one is checked at once; one picked with
        // :TutorBack or :TutorNext waits for the next change, so a step already done in the
        // practice text doesn't skip itself.
        self.tutor.checked = (!praised).then_some(self.state.revision);
        self.tutor_show_line();
        self.sync_tutor_card();
        if let Err(error) = saved {
            self.show_error(format!("could not save tutor progress: {error}"));
        }
    }

    /// Set up the windows and file the step starts from.
    fn tutor_scene(&mut self, index: usize) {
        let Some(root) = self
            .tutor
            .practice
            .as_ref()
            .map(|practice| practice.root.clone())
        else {
            return;
        };
        let step = &self.tutor.steps[index];
        let (split, explorer, terminal) = (step.split, step.explorer, step.terminal);
        // A step without a file of its own still needs a practice file in front, such as when
        // the tutor picks up in the middle of a chapter.
        // A practice file in any window of the tab will do, so a step after the agent panel or
        // the terminal took focus leaves the focus there.
        let in_practice = self.state.active_tab_views().into_iter().any(|view| {
            self.state
                .views
                .get(view)
                .and_then(|view| self.state.buffers.get(view.buffer))
                .and_then(|buffer| buffer.path.as_deref())
                .is_some_and(|path| path.starts_with(&root))
        });
        let file = step
            .scene_file()
            .or((!in_practice).then(|| step.file()))
            .map(|file| root.join(file));
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

    /// Leave the practice project: discard disposable edits, move back to the user's project
    /// and let `tutor_restore` put the layout back.
    fn tutor_leave(&mut self) {
        let Some(practice) = &self.tutor.practice else {
            return;
        };
        if practice.stage == Stage::Entering && self.state.workspace_root != practice.root {
            let root = practice.root.clone();
            self.workspace.cancel_root_request(&root);
            if self.switch_ends_terminals.as_ref() == Some(&root) {
                self.switch_ends_terminals = None;
            }
            self.tutor.practice = None;
            return;
        }
        if practice.stage == Stage::Leaving {
            return;
        }
        let (root, home) = (practice.root.clone(), practice.home.clone());
        self.ui.set(None, |ui| &mut ui.tutor);
        // Unsaved practice files would block the move; they are thrown away with the folder.
        let unsaved = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.dirty)
            .filter(|(_, buffer)| {
                buffer
                    .path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&root))
            })
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for id in unsaved {
            self.state.set_buffer_dirty(id, false);
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
        self.tutor.current = None;
        let Some(practice) = self.tutor.practice.take() else {
            return;
        };
        self.close_agent_sessions_in_root(&practice.root);
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

    fn sync_tutor_card(&mut self) {
        let leader = self.configuration.settings().input.keymaps.leader.clone();
        let leader = leader.trim_matches(|c| c == '<' || c == '>');
        let frame = self.tutor.shown_at.elapsed().as_millis() / POINTER_FRAME.as_millis();
        let pointer = POINTER[usize::try_from(frame).unwrap_or(0) % POINTER.len()];
        let place = self
            .tutor
            .current
            .filter(|_| self.tutor_in_project())
            .map(|index| (index, self.tutor_place(index)));
        // Coming back to the step's file puts the cursor on its line again.
        if let Some((index, (_, true, _))) = &place {
            let step = &self.tutor.steps[*index];
            self.tutor.jump = step.at.is_some() || step.mark.is_some();
        }
        let card = place.map(|(index, (place, away, line))| {
            let step = &self.tutor.steps[index];
            crate::ui::TutorCard {
                title: format!(
                    "Tutor {}/{} · {}",
                    index + 1,
                    self.tutor.steps.len(),
                    step.chapter
                ),
                text: step.text.replace("<leader>", leader),
                press: step.press.replace("<leader>", leader),
                pointer: pointer.into(),
                praised: self.tutor.done_at.is_some(),
                footer: FOOTER.into(),
                target: self.tutor.target.clone(),
                place,
                away,
                line,
            }
        });
        self.ui.set(card, |ui| &mut ui.tutor);
    }

    /// Where step `index` happens for the card: its file and line, or, when another file is in
    /// front, how to get back to it.
    fn tutor_place(&self, index: usize) -> (String, bool, Option<(BufferId, usize)>) {
        let step = &self.tutor.steps[index];
        if !(step.checks_text() || step.at.is_some() || step.mark.is_some()) {
            return (String::new(), false, None);
        }
        let file = step.file();
        let buffer = self.tutor_buffer(file);
        let front = self.state.active_buffer_id();
        let front_is_file = front.is_some_and(|id| self.state.buffers[id].path.is_some());
        if front_is_file && front != buffer && self.tutor.done_at.is_none() {
            return (
                format!("This step is in {file}: type `:e {file}` and `enter` to go back."),
                true,
                None,
            );
        }
        let line = buffer.and_then(|buffer| {
            let text = &self.state.buffers[buffer].text;
            let wanted = step.at.as_deref().or(step.mark.as_deref())?;
            let line = (0..text.len_lines()).find(|line| {
                text.line(*line)
                    .is_ok_and(|content| content.contains(wanted))
            })?;
            Some((buffer, line))
        });
        let place = match line {
            Some((_, line)) => format!("{file}, line {}", line + 1),
            None => file.to_owned(),
        };
        (place, false, line)
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
        let active = self
            .state
            .active_view()
            .and_then(|view| self.state.views.get(view));
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
            && step.commented.is_none()
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
        let has = |wanted: &str| lines.iter().any(|line| line.contains(wanted));
        let twice = |wanted: &str| lines.iter().filter(|line| line.contains(wanted)).count() >= 2;
        let indented = |wanted: &str| {
            lines.iter().any(|line| {
                line.contains(wanted) && line.len() - line.trim_start().len() > step.indent
            })
        };
        let commented = |wanted: &str| {
            lines.iter().any(|line| {
                let line = line.trim_start();
                line.contains(wanted) && (line.starts_with("//") || line.starts_with('#'))
            })
        };
        step.line.as_deref().is_none_or(has)
            && step.gone.as_deref().is_none_or(|gone| !has(gone))
            && step.twice.as_deref().is_none_or(twice)
            && step.indented.as_deref().is_none_or(indented)
            && step.commented.as_deref().is_none_or(commented)
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
        let Some(view_id) = self.state.active_view() else {
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
                content
                    .contains(at.as_str())
                    .then(|| (line, content.len() - content.trim_start().len()))
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
                role: crate::config::Highlight::VARIABLE,
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

use crate::app::commands::AppEffect;
use crate::app::commands::register;
use crate::editor::ArgumentSchema;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "session.tutor",
        "Start the interactive tutor, or pick up where you left it",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::Tutor),
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_tutor_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Tutor => self.open_tutor(),
            AppEffect::TutorNext => self.tutor_next(),
            AppEffect::TutorBack => self.tutor_back(),
            AppEffect::TutorQuit => self.tutor_quit(),
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tutor::*;

    pub(in crate::app) fn app() -> App {
        App::new(
            crate::app::startup::StartupTarget::Dashboard {
                root: tempfile::tempdir().unwrap().keep(),
            },
            crate::config::load_bundled_config(),
            false,
            tempfile::tempdir().unwrap().keep(),
            false,
            false,
            std::sync::Arc::new(crate::clock::SystemClock),
        )
        .unwrap()
    }

    fn running_terminal(app: &mut App) {
        app.terminal_session.fixture(
            crate::process::PtyId(1),
            crate::app::terminal::TerminalFixture {
                profile: "shell".into(),
                screen: crate::ui::PtyScreen::new(80, 8, 100),
                running: true,
                exit_code: None,
                truncated: false,
            },
            app.workspace.version(),
        );
        app.terminal_session
            .fixture_cwd(crate::process::PtyId(1), app.state.workspace_root.clone());
    }

    pub(in crate::app) fn enter(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.workspace.root_operation_pending() {
            app.process_trust_events();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        app.tutor_tick();
    }

    #[test]
    fn tutor_retries_the_same_practice_root_after_terminal_confirmation() {
        let mut app = app();
        running_terminal(&mut app);
        let start = TutorStart {
            language: Language::Rust,
            step: 0,
        };
        app.start_tutor(start);
        let root = app.tutor.practice.as_ref().unwrap().root.clone();
        assert!(!app.workspace.root_operation_pending());
        app.tutor_tick();
        assert_eq!(app.tutor.practice.as_ref().unwrap().root, root);
        app.start_tutor(start);
        assert!(app.workspace.root_operation_pending());
        assert_eq!(app.tutor.practice.as_ref().unwrap().root, root);
    }

    #[test]
    fn tutor_quit_cancels_entry_and_terminal_confirmation() {
        for terminal in [false, true] {
            let mut app = app();
            let home = app.state.workspace_root.clone();
            if terminal {
                running_terminal(&mut app);
            }
            app.start_tutor(TutorStart {
                language: Language::Rust,
                step: 0,
            });
            let root = app.tutor.practice.as_ref().unwrap().root.clone();
            assert_eq!(app.workspace.root_operation_pending(), !terminal);
            app.tutor_quit();
            assert!(app.tutor.practice.is_none());
            assert!(app.switch_ends_terminals.is_none());
            assert!(!app.workspace.root_operation_pending());
            assert_eq!(app.state.workspace_root, home);
            assert!(!root.exists());
            app.open_tutor();
            assert!(app.ui.picker.is_some());
            assert!(app.tutor.practice.is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn tutor_exit_discards_practice_edits_without_following_replaced_paths() {
        let mut app = app();
        let home = app.state.workspace_root.clone();
        let original = home.join("original.txt");
        std::fs::write(&original, "keep this").unwrap();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step: 0,
        });
        enter(&mut app);
        let root = app.tutor.practice.as_ref().unwrap().root.clone();
        let path = root.join("replaced.txt");
        std::os::unix::fs::symlink(&original, &path).unwrap();
        let (buffer, _) = app
            .state
            .open_text(
                crate::editor::text::TextBuffer::from_text("practice edits"),
                Some(path),
            )
            .unwrap();
        app.state.set_buffer_dirty(buffer, true);
        app.tutor_quit();
        enter(&mut app);
        assert_eq!(std::fs::read_to_string(original).unwrap(), "keep this");
        assert_eq!(app.state.workspace_root, home);
        assert!(app.tutor.practice.is_none());
        assert!(!root.exists());
    }

    #[test]
    fn tutor_rejects_dirty_scratch_buffers_before_putting_the_layout_aside() {
        let mut app = app();
        let home = app.state.workspace_root.clone();
        let path = home.join("notes.txt");
        std::fs::write(&path, "saved notes").unwrap();
        app.state
            .open_text(
                crate::editor::text::TextBuffer::from_text("saved notes"),
                Some(path),
            )
            .unwrap();
        let (scratch, _) = app
            .state
            .open_text(
                crate::editor::text::TextBuffer::from_text("unsaved notes"),
                None,
            )
            .unwrap();
        app.state.set_buffer_dirty(scratch, true);
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step: 0,
        });
        assert!(app.tutor.practice.is_none());
        assert!(!app.workspace.root_operation_pending());
        assert_eq!(app.state.workspace_root, home);
        assert_eq!(app.state.buffers[scratch].text.to_string(), "unsaved notes");
        assert!(app.state.buffers[scratch].dirty);
        assert!(
            app.ui
                .notification
                .as_deref()
                .unwrap()
                .contains("unsaved buffers")
        );
    }

    #[test]
    fn tutor_reports_progress_write_failures_without_stopping_the_lesson() {
        let mut app = app();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step: 0,
        });
        enter(&mut app);
        std::fs::remove_file(&app.tutor.progress).unwrap();
        std::fs::create_dir(&app.tutor.progress).unwrap();
        app.tutor_next();
        assert_eq!(app.tutor.current, Some(1));
        assert!(app.ui.tutor.is_some());
        assert!(
            app.ui
                .notification
                .as_deref()
                .unwrap()
                .contains("could not save tutor progress:")
        );
    }

    #[test]
    fn paused_tutor_navigation_leaves_the_other_workspace_unchanged() {
        let mut app = app();
        let home = app.state.workspace_root.clone();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step: 2,
        });
        enter(&mut app);
        app.switch_workspace_root(home.clone());
        enter(&mut app);
        assert_eq!(app.state.workspace_root, home);
        let revision = app.state.revision;
        let views = app.state.views.len();
        let progress = std::fs::read_to_string(&app.tutor.progress).unwrap();
        app.tutor_next();
        app.tutor_back();
        assert_eq!(app.tutor.current, Some(2));
        assert_eq!(app.state.revision, revision);
        assert_eq!(app.state.views.len(), views);
        assert_eq!(
            std::fs::read_to_string(&app.tutor.progress).unwrap(),
            progress
        );
        assert_eq!(app.state.workspace_root, home);
        app.open_tutor();
        enter(&mut app);
        app.tutor_next();
        assert_eq!(app.tutor.current, Some(3));
    }

    #[test]
    fn tutor_trust_lesson_waits_for_confirmed_persisted_trust() {
        let mut app = app();
        let step = lessons(Language::Rust)
            .unwrap()
            .iter()
            .position(|step| step.done.iter().any(|done| done == "workspace.trust"))
            .unwrap();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step,
        });
        enter(&mut app);
        let root = app.workspace.active_root().unwrap();
        let identity = crate::files::RootIdentity::capture(&app.state.workspace_root).unwrap();
        app.workspace.fixture_identity(root, identity).unwrap();
        app.tutor_command("workspace.trust");
        assert!(app.input.prompt().is_some());
        app.tutor_tick();
        assert!(app.tutor.done_at.is_none());
        app.close_prompt();
        app.tutor_tick();
        assert!(app.tutor.done_at.is_none());
        app.tutor_command("workspace.trust");
        app.confirm_workspace_trust();
        app.close_prompt();
        app.tutor_tick();
        assert!(app.tutor.done_at.is_none(), "trust is still being saved");
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.workspace.trust() != crate::workspace::WorkspaceTrust::Trusted {
            app.process_trust_events();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        app.tutor_tick();
        assert!(app.tutor.done_at.is_some());
    }

    #[test]
    fn refused_tutor_exit_keeps_the_lesson_and_card() {
        let mut app = app();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step: 2,
        });
        enter(&mut app);
        assert_eq!(app.tutor.current, Some(2));
        let path = app
            .tutor
            .practice
            .as_ref()
            .unwrap()
            .root
            .join("practice.rs");
        let text =
            crate::editor::text::TextBuffer::from_text(&std::fs::read_to_string(&path).unwrap());
        let (buffer, _) = app.state.open_text(text, Some(path)).unwrap();
        app.tutor_command("mode.insert");
        app.handle_key_in_context(
            "editor.insert",
            crate::editor::input::KeyEvent::plain(crate::editor::input::KeyCode::Char('x')),
        );
        let edited = app.state.buffers[buffer].text.to_string();
        assert!(app.state.buffers[buffer].dirty);
        running_terminal(&mut app);
        app.tutor_leave();
        app.tutor_tick();
        assert_eq!(app.state.buffers[buffer].text.to_string(), edited);
        assert!(app.state.buffers[buffer].dirty);
        assert_eq!(app.tutor.current, Some(2));
        assert!(app.ui.tutor.is_some());
        assert_eq!(app.tutor.practice.as_ref().unwrap().stage, Stage::Running);
    }

    #[test]
    fn resuming_undo_and_redo_starts_at_the_prerequisite_edit() {
        for command in ["history.undo", "history.redo"] {
            let mut app = app();
            let steps = lessons(Language::Rust).unwrap();
            let step = steps
                .iter()
                .position(|step| step.done.iter().any(|done| done == command))
                .unwrap();
            app.start_tutor(TutorStart {
                language: Language::Rust,
                step,
            });
            let resumed = app.tutor.practice.as_ref().unwrap().start;
            assert!(resumed < step);
            assert!(app.tutor.steps[resumed].gone.is_some());
        }
    }

    #[test]
    fn resuming_dependent_lessons_replays_their_prerequisites() {
        for language in Language::ALL {
            for (dependent, prerequisite) in [
                ("The dog sat. The dog ran. Every dog naps.", "/cat"),
                ("agent.model", "agent.new"),
                ("terminal.mode.normal", "workspace.trust"),
            ] {
                let mut app = app();
                let steps = lessons(language).unwrap();
                let step = steps
                    .iter()
                    .position(|step| {
                        step.line.as_deref() == Some(dependent)
                            || step.done.iter().any(|done| done == dependent)
                    })
                    .unwrap();
                app.start_tutor(TutorStart { language, step });
                let resumed = app.tutor.practice.as_ref().unwrap().start;
                assert!(resumed < step);
                let first = &app.tutor.steps[resumed];
                assert!(
                    first.press.contains(prerequisite)
                        || first.done.iter().any(|done| done == prerequisite)
                );
                assert!(!first.resume_from_previous);
            }
        }
    }

    #[test]
    fn rust_practice_manifest_is_emitted_with_its_cargo_name() {
        let files = Language::Rust.practice_files().collect::<Vec<_>>();
        let manifest = files
            .iter()
            .find(|(name, _)| *name == "Cargo.toml")
            .unwrap();
        let parsed: toml::Value = toml::from_str(&manifest.1).unwrap();
        assert!(parsed.get("package").is_some());
        assert!(!files.iter().any(|(name, _)| *name == "Cargo.toml.in"));
    }

    #[test]
    fn every_lesson_checks_a_real_command_and_practice_line() {
        let mut registry = crate::editor::CommandRegistry::default();
        crate::editor::register_builtin_commands(&mut registry).expect("builtin commands");
        crate::app::commands::register_app_commands(&mut registry).expect("app commands");
        for language in Language::ALL {
            let steps = lessons(language).expect("lessons parse");
            assert!(steps.len() > 40);
            let mut files = language.practice_files().collect::<Vec<_>>();
            let (greet, changed) = language.changed_greet();
            files.retain(|(name, _)| *name != greet);
            files.push((greet, changed));
            for step in &steps {
                assert!(
                    !step.press.is_empty() && !step.press.contains('{'),
                    "{}",
                    step.press
                );
                for pattern in &step.done {
                    let id = pattern.split_whitespace().next().unwrap_or_default();
                    assert!(
                        registry.specs().any(|spec| spec.id.as_str() == id),
                        "tutor step checks the unknown command {id}"
                    );
                }
                let text = files
                    .iter()
                    .find(|(name, _)| *name == step.file())
                    .map(|(_, text)| text.as_str())
                    .unwrap_or_else(|| {
                        panic!("the {language:?} practice project has no {}", step.file())
                    });
                let has = |wanted: &str| text.lines().any(|line| line.contains(wanted));
                for wanted in step.at.iter().chain(&step.gone).chain(&step.commented) {
                    assert!(has(wanted), "{} has no line {wanted:?}", step.file());
                }
                if let Some(mark) = &step.mark {
                    assert!(
                        text.contains(mark.as_str()),
                        "{} has no {mark:?}",
                        step.file()
                    );
                }
                if let Some(wanted) = &step.line {
                    assert!(
                        !has(wanted),
                        "{language:?}: {wanted:?} is already in {}",
                        step.file()
                    );
                }
            }
        }
    }

    /// "Press `<leader>` `f` `b` ..." must be a leader sequence the default keymap binds to one
    /// of the step's commands.
    #[test]
    fn every_leader_key_a_step_teaches_runs_its_command() {
        let keymaps = crate::config::load_bundled_config().keymaps.entries;
        for step in lessons(Language::Rust).expect("lessons parse") {
            let mut parts = step.press.split('`');
            let mut keys = Vec::new();
            while let (Some(text), Some(key)) = (parts.next(), parts.next()) {
                if key == "<leader>" {
                    keys = vec!["<leader>".to_owned()];
                } else if !keys.is_empty()
                    && (text.trim().is_empty() || keys.len() == 1 && text.trim() == "then")
                {
                    keys.push(key.to_owned());
                } else if !keys.is_empty() {
                    break;
                }
            }
            if keys.is_empty() {
                continue;
            }
            let sequence = keys.join(" ");
            let command = keymaps
                .iter()
                .find(|entry| entry.keys.as_deref() == Some(sequence.as_str()))
                .and_then(|entry| entry.command.clone())
                .unwrap_or_else(|| panic!("no key {sequence} for step {:?}", step.press));
            assert!(
                step.done
                    .iter()
                    .any(|pattern| pattern.split_whitespace().next() == Some(command.as_str())),
                "{sequence} runs {command}, but step {:?} waits for {:?}",
                step.press,
                step.done
            );
        }
    }

    #[test]
    fn the_practice_project_is_a_repository_with_one_changed_file() {
        for language in Language::ALL {
            let folder = create_practice_project(language).expect("practice project");
            let repository = git2::Repository::open(folder.path()).expect("repository");
            let statuses = repository.statuses(None).expect("status");
            let changed = statuses
                .iter()
                .filter_map(|entry| entry.path().map(str::to_owned))
                .collect::<Vec<_>>();
            assert_eq!(changed, [language.practice_names()[2]]);
            let practice =
                std::fs::read_to_string(folder.path().join(language.practice_names()[0]))
                    .expect("practice file");
            assert!(
                practice.contains("    The X marks the spot.\n")
                    && !practice.contains("PRACTICE_LINES")
            );
        }
    }

    #[test]
    fn patterns_match_the_command_and_its_arguments() {
        let invocation =
            CommandInvocation::new("input.motion", crate::editor::InvocationSource::Keymap)
                .expect("id")
                .with_argument("motion", CommandValue::String("down".into()))
                .with_argument("linewise", CommandValue::Bool(false));
        assert!(invocation_matches("input.motion", &invocation));
        assert!(invocation_matches("input.motion motion=down", &invocation));
        assert!(invocation_matches(
            "input.motion motion=down linewise=false",
            &invocation
        ));
        assert!(!invocation_matches("input.motion motion=up", &invocation));
        assert!(!invocation_matches("input.find.begin", &invocation));
    }
}
