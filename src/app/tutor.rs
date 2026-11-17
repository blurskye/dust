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
const FOOTER: &str = ":TutorNext skip · :TutorBack · :TutorChapters · :TutorQuit";
/// How often a step that waits on the repository looks at it.
const STATE_CHECK: Duration = Duration::from_millis(200);
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

    /// The program that runs the practice project's tests.
    fn runner(self) -> &'static str {
        match self {
            Self::Rust => "cargo",
            Self::TypeScript => "node",
            Self::Python => "python3",
        }
    }

    /// The arguments that run the practice project's tests with `runner`.
    fn test_arguments(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["test"],
            Self::TypeScript => &["--test"],
            Self::Python => &["-m", "unittest", "discover", "-s", "src"],
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
                practice_file!("README.md", "README.md"),
                practice_file!("notes.txt", "notes.txt"),
            ],
            Self::TypeScript => &[
                practice_file!("practice.ts", "typescript/practice.ts"),
                practice_file!("src/main.ts", "typescript/src/main.ts"),
                practice_file!("src/greet.ts", "typescript/src/greet.ts"),
                practice_file!("src/greet.test.mjs", "typescript/src/greet.test.mjs"),
                practice_file!("package.json", "typescript/package.json"),
                practice_file!("tsconfig.json", "typescript/tsconfig.json"),
                practice_file!(".gitignore", "typescript/gitignore"),
                practice_file!("README.md", "README.md"),
                practice_file!("notes.txt", "notes.txt"),
            ],
            Self::Python => &[
                practice_file!("practice.py", "python/practice.py"),
                practice_file!("src/main.py", "python/src/main.py"),
                practice_file!("src/greet.py", "python/src/greet.py"),
                practice_file!("src/test_greet.py", "python/src/test_greet.py"),
                practice_file!("pyproject.toml", "python/pyproject.toml"),
                practice_file!(".gitignore", "python/gitignore"),
                practice_file!("README.md", "README.md"),
                practice_file!("notes.txt", "notes.txt"),
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
    /// What the step can't work without: "server" (the language server), "debugger" (a debug
    /// adapter for the language), "agent" (a coding agent program) or programs on PATH,
    /// separated by spaces.
    /// Without it the card says why and the step waits for :TutorNext.
    needs: Option<String>,
    /// Something the practice project must have before the step: "conflict" (a merge stopped
    /// on a conflict in notes.txt) or "tasks" (the practice tasks in the configuration).
    setup: Option<String>,
    /// A file with changes staged in the index.
    staged: Option<String>,
    /// A file that is the same as in the last commit, in the index and on disk.
    clean: Option<String>,
    /// The keys that do the step, which the tests press: characters stand for themselves and
    /// `<name>` for a named key, `<leader>` for the leader and `<lt>` for `<`.
    #[serde(default)]
    pub(super) keys: String,
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
            .replace("{runner}", language.runner())
            .replace("{practice}", practice)
            .replace("{main}", main)
            .replace("{greet}", greet)
    };
    let files = language.practice_files().collect::<Vec<_>>();
    for step in &mut steps {
        step.text = fill(&step.text);
        step.press = fill(&step.press);
        step.open = step.open.as_deref().map(fill);
        step.needs = step.needs.as_deref().map(fill);
        step.staged = step.staged.as_deref().map(fill);
        step.clean = step.clean.as_deref().map(fill);
        step.keys = fill(&step.keys);
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
) -> Result<crate::files::TempTree, Box<dyn std::error::Error>> {
    let folder = crate::files::TempTree::new("dust-tutor-")?;
    for (name, text) in language.practice_files() {
        let path = folder.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
    }
    let repository = git2::Repository::init(folder.path())?;
    // Commits made in the lessons, such as finishing a merge, need a name to sign with.
    let mut config = repository.config()?;
    config.set_str("user.name", "Dust tutor")?;
    config.set_str("user.email", "tutor@dust.invalid")?;
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

/// The line of notes.txt both sides of the practice merge change.
const CONFLICT_LINE: &str = "Favourite colour: red";

/// Stop a merge on a conflict in notes.txt, as after `git merge colours`: the current branch
/// made the colour blue and the `colours` branch green. A merge already stopped stays as it is.
fn start_practice_conflict(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let repository = git2::Repository::open(root)?;
    if repository.state() != git2::RepositoryState::Clean {
        return Ok(());
    }
    let notes = Path::new("notes.txt");
    let signature = git2::Signature::now("Dust tutor", "tutor@dust.invalid")?;
    let head = repository.head()?.peel_to_commit()?;
    let original = repository
        .find_blob(head.tree()?.get_path(notes)?.id())?
        .content()
        .to_vec();
    let original = String::from_utf8(original)?;
    if !original.contains(CONFLICT_LINE) {
        return Err("notes.txt has changed since the practice project was made".into());
    }
    let commit = |text: &str, message: &str| -> Result<git2::Oid, git2::Error> {
        let blob = repository.blob(text.as_bytes())?;
        let mut tree = repository.treebuilder(Some(&head.tree()?))?;
        tree.insert(notes, blob, 0o100_644)?;
        let tree = repository.find_tree(tree.write()?)?;
        repository.commit(None, &signature, &signature, message, &tree, &[&head])
    };
    let theirs = original.replacen(CONFLICT_LINE, "Favourite colour: green", 1);
    let theirs = commit(&theirs, "Make the colour green")?;
    repository.branch("colours", &repository.find_commit(theirs)?, true)?;
    let ours_text = original.replacen(CONFLICT_LINE, "Favourite colour: blue", 1);
    let ours = commit(&ours_text, "Make the colour blue")?;
    repository
        .head()?
        .set_target(ours, "tutor: make the colour blue")?;
    std::fs::write(root.join(notes), &ours_text)?;
    let mut index = repository.index()?;
    index.add_path(notes)?;
    index.write()?;
    // `git merge --abort` goes back to where the merge started.
    repository.reference("ORIG_HEAD", ours, true, "tutor: merge colours")?;
    let annotated = repository.find_annotated_commit(theirs)?;
    repository.merge(&[&annotated], None, None)?;
    Ok(())
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
    _folder: crate::files::TempTree,
    root: PathBuf,
    /// The user's project and layout, put back when the tutor ends.
    home: PathBuf,
    layout: Option<PersistedSession>,
    dashboard: bool,
    stage: Stage,
    language: Language,
    /// The step to show once the editor is in the practice project.
    start: usize,
    /// The practice tasks in the configuration, with the user's own task each one replaced.
    tasks: Vec<(&'static str, Option<crate::config::TaskConfig>)>,
}

/// The practice tasks, with their programs found on PATH: `status` lists the changed files,
/// `todo` is a linter that finds TODO notes and `tests` runs the language's tests. A task whose
/// program isn't installed is left out.
fn practice_tasks(language: Language) -> Vec<(&'static str, crate::config::TaskConfig)> {
    use crate::config::TaskConfig;
    use crate::config::TaskKind;
    let task = |kind, program: &str, arguments: &[&str], matcher| {
        Some(TaskConfig {
            kind,
            executable: crate::process::discover_executable(program)?,
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            cwd: None,
            environment: std::collections::BTreeMap::new(),
            dependencies: Vec::new(),
            timeout_ms: Some(120_000),
            stdin: false,
            matcher,
        })
    };
    let todo = crate::config::ProblemMatcherConfig {
        pattern: r"(?m)^(?P<path>[^:\n]+):(?P<line>\d+):(?P<message>.*)$".into(),
        path_group: "path".into(),
        line_group: "line".into(),
        column_group: None,
        column_encoding: None,
        severity_group: None,
        message_group: "message".into(),
        name_group: None,
        status_group: None,
        duration_group: None,
    };
    [
        (
            "status",
            task(TaskKind::Task, "git", &["status", "--short"], None),
        ),
        (
            "todo",
            task(TaskKind::Linter, "git", &["grep", "-n", "TODO"], Some(todo)),
        ),
        (
            "tests",
            task(
                TaskKind::Test,
                language.runner(),
                language.test_arguments(),
                None,
            ),
        ),
    ]
    .into_iter()
    .filter_map(|(name, task)| Some((name, task?)))
    .collect()
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
    /// The steps the user has done, for the chapters' progress.
    completed: std::collections::BTreeSet<usize>,
    /// Error notices so far; a command that raised one doesn't count as the step done.
    pub(in crate::app) errors: u64,
    /// When the step's git state was last looked at.
    state_checked: Option<Instant>,
    /// The commands run during the step, and whether each ran without an error, for the
    /// tests to say what happened when a step wasn't done.
    #[cfg(test)]
    pub(in crate::app) seen: Vec<String>,
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
            completed: std::collections::BTreeSet::new(),
            errors: 0,
            state_checked: None,
            #[cfg(test)]
            seen: Vec::new(),
        }
    }

    pub(in crate::app) fn step(&self) -> Option<&Step> {
        self.current.and_then(|index| self.steps.get(index))
    }

    /// The step and language the user stopped at and the steps done, saved as
    /// "12 python 0,1,2,5".
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

    /// The steps done, as saved with the progress.
    fn saved_completed(&self) -> std::collections::BTreeSet<usize> {
        std::fs::read_to_string(&self.progress)
            .ok()
            .and_then(|text| {
                text.split_whitespace().nth(2).map(|done| {
                    done.split(',')
                        .filter_map(|step| step.parse::<usize>().ok())
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    fn save_progress(&self, step: usize) -> std::io::Result<()> {
        if let Some(practice) = &self.practice {
            let done = self
                .completed
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let line = format!("{step} {} {done}", practice.language.id());
            std::fs::write(&self.progress, line.trim_end())?;
        }
        Ok(())
    }

    /// The chapters in order: each one's name and its steps.
    fn chapters(&self) -> Vec<(&str, std::ops::Range<usize>)> {
        let mut chapters: Vec<(&str, std::ops::Range<usize>)> = Vec::new();
        for (index, step) in self.steps.iter().enumerate() {
            match chapters.last_mut() {
                Some((name, range)) if *name == step.chapter => range.end = index + 1,
                _ => chapters.push((&step.chapter, index..index + 1)),
            }
        }
        chapters
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
        if let Some(practice) = &self.tutor.practice {
            // A chapter picked from :TutorChapters.
            if practice.stage == Stage::Running && self.tutor_in_project() {
                return self.tutor_go(start.step.min(self.tutor.steps.len()), false);
            }
            return self.open_tutor();
        }
        match lessons(start.language) {
            Ok(steps) => self.tutor.steps = steps,
            Err(error) => return self.show_error(error),
        }
        self.tutor.completed = if start.step == 0 {
            std::collections::BTreeSet::new()
        } else {
            self.tutor.saved_completed()
        };
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
        let root = crate::files::canonicalize(folder.path())
            .unwrap_or_else(|_| folder.path().to_path_buf());
        let home = crate::files::canonicalize(&self.state.workspace_root)
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
            tasks: Vec::new(),
        });
    }

    pub(in crate::app) fn tutor_next(&mut self) {
        if let Some(index) = self.tutor.current {
            self.tutor_go(index + 1, false);
        } else {
            self.notify_message("The tutor is not running: :Tutor starts it.".into(), false);
        }
    }

    /// `:TutorChapters`: every chapter with how much of it is done; `enter` goes to its first
    /// step not done yet.
    pub(in crate::app) fn open_tutor_chapters(&mut self) {
        let (Some(current), Some(practice)) = (self.tutor.current, &self.tutor.practice) else {
            self.notify_message("The tutor is not running: :Tutor starts it.".into(), false);
            return;
        };
        let language = practice.language;
        let chapters = self.tutor.chapters();
        let count = chapters.len();
        let mut selected = None;
        let items = chapters
            .into_iter()
            .enumerate()
            .map(|(number, (name, range))| {
                let done = range
                    .clone()
                    .filter(|step| self.tutor.completed.contains(step))
                    .count();
                let first = range
                    .clone()
                    .find(|step| !self.tutor.completed.contains(step))
                    .unwrap_or(range.start);
                let id = PickerItemId(format!("tutor-chapter:{number}"));
                if range.contains(&current) {
                    selected = Some(id.clone());
                }
                (
                    PickerItem {
                        badge: if done == range.len() {
                            "✓".into()
                        } else if range.contains(&current) {
                            "here".into()
                        } else {
                            String::new()
                        },
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: format!("{}. {name}", number + 1),
                        detail: format!(
                            "{done}/{} done · steps {}-{}",
                            range.len(),
                            range.start + 1,
                            range.end
                        ),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Tutor(TutorStart {
                        language,
                        step: first,
                    }),
                )
            })
            .collect();
        self.install_surface_picker(
            crate::ui::PickerSurfaceKind::Choice,
            format!("Tutor chapters ({count})"),
            items,
        );
        if let Some(selected) = selected {
            self.search.select_picker_item(&selected);
        }
        self.search
            .set_picker_footer("Enter go there · Esc Esc close".to_owned());
        self.sync_picker_ui();
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

    /// Called for every command that ran without an error; `errors` is how many error notices
    /// there were before it ran, so one that reported an error doesn't count.
    pub(in crate::app) fn tutor_observe(&mut self, invocation: &CommandInvocation, errors: u64) {
        #[cfg(test)]
        self.tutor.seen.push(format!(
            "{}{}",
            invocation.id.as_str(),
            if self.tutor.errors == errors {
                ""
            } else {
                " (error)"
            }
        ));
        let Some(step) = self.tutor.step() else {
            return;
        };
        if self.tutor.errors == errors
            && step
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
        let missing = self.tutor_missing(index).is_some();
        let step = &self.tutor.steps[index];
        let trust = step.done.iter().any(|command| command == "workspace.trust");
        let git = step.staged.is_some() || step.clean.is_some();
        let text = step.checks_text();
        // Trust is persisted asynchronously after confirmation; dispatching :trust is not
        // enough, since the user can decline or the write can fail.
        let done = if missing {
            false
        } else if trust {
            self.workspace.trust() == crate::workspace::WorkspaceTrust::Trusted
        } else if git {
            // The index and the files on disk change in the background after the command.
            let due = self
                .tutor
                .state_checked
                .is_none_or(|checked| checked.elapsed() >= STATE_CHECK);
            due && {
                self.tutor.state_checked = Some(Instant::now());
                self.tutor_git_done(index)
            }
        } else if text {
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
            self.tutor.completed.insert(index);
            if let Err(error) = self.tutor.save_progress(index) {
                self.show_error(format!("could not save tutor progress: {error}"));
            }
        }
        self.sync_tutor_card();
    }

    /// Why step `index` can't be done here, or `None` when it can.
    pub(in crate::app) fn tutor_missing(&self, index: usize) -> Option<String> {
        let step = self.tutor.steps.get(index)?;
        let need = step.needs.as_deref()?;
        let language = self.tutor.practice.as_ref()?.language;
        match need {
            "server" => {
                let ready = self
                    .language
                    .sessions()
                    .values()
                    .any(|session| session.capabilities.is_some());
                (!ready).then(|| {
                    format!(
                        "{} isn't running here, so nothing can answer this yet. `<leader>` `c` `m` installs it and it starts by itself; `:TutorNext` skips this step.",
                        language.server()
                    )
                })
            }
            "debugger" => (!self.tutor_debugger_ready()).then(|| {
                format!(
                    "No {} debugger is installed, so there is no program to stop in. `<leader>` `c` `m` installs one; `:TutorNext` skips this step.",
                    language.name()
                )
            }),
            "agent" => (!self.any_agent_installed()).then(|| {
                "No coding agent (claude, codex, pi, opencode, gemini) is installed, so no session can start here. Install one and `<leader>` `c` `m` adds its adapter; `:TutorNext` skips this step.".to_owned()
            }),
            programs => programs
                .split_whitespace()
                .find(|program| self.tooling.path_executable(program).is_none())
                .map(|program| {
                    format!(
                        "`{program}` isn't installed, so this can't run here. Install it to try this; `:TutorNext` skips this step."
                    )
                }),
        }
    }

    /// The step's file is staged, or the same as in the last commit.
    fn tutor_git_done(&self, index: usize) -> bool {
        let (Some(practice), Some(step)) = (&self.tutor.practice, self.tutor.steps.get(index))
        else {
            return false;
        };
        let Ok(repository) = git2::Repository::open(&practice.root) else {
            return false;
        };
        let status = |file: &str| repository.status_file(Path::new(file)).ok();
        let index_changes = git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_DELETED
            | git2::Status::INDEX_RENAMED;
        step.staged
            .as_deref()
            .is_none_or(|file| status(file).is_some_and(|status| status.intersects(index_changes)))
            && step
                .clean
                .as_deref()
                .is_none_or(|file| status(file).is_some_and(|status| status.is_empty()))
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
        #[cfg(test)]
        self.tutor.seen.clear();
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
        match step.setup.as_deref() {
            Some("conflict") => {
                if let Err(error) = start_practice_conflict(&root) {
                    self.show_error(format!(
                        "could not stop a practice merge on a conflict: {error}"
                    ));
                }
                self.start_git_status_refresh_silent();
            }
            Some("tasks") => self.add_practice_tasks(),
            _ => {}
        }
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

    /// Put the practice tasks in the configuration while the tutor runs.
    fn add_practice_tasks(&mut self) {
        let Some(language) = self
            .tutor
            .practice
            .as_ref()
            .map(|practice| practice.language)
        else {
            return;
        };
        let mut changed = false;
        for (name, task) in practice_tasks(language) {
            if self.configuration.settings().tasks.get(name) == Some(&task) {
                continue;
            }
            let replaced = self.configuration.replace_task(name, Some(task));
            changed = true;
            if let Some(practice) = &mut self.tutor.practice
                && !practice.tasks.iter().any(|(added, _)| *added == name)
            {
                practice.tasks.push((name, replaced));
            }
        }
        if changed {
            self.rebuild_matcher_cache();
        }
    }

    /// A debug adapter for the practice language is installed.
    fn tutor_debugger_ready(&self) -> bool {
        let Some(language) = self
            .tutor
            .practice
            .as_ref()
            .map(|practice| practice.language)
        else {
            return false;
        };
        self.tooling.entries().iter().any(|entry| {
            entry.definition.kind == crate::tools::ToolKind::Dap
                && entry
                    .definition
                    .languages
                    .iter()
                    .any(|name| name == language.id())
                && match &entry.status {
                    crate::tools::ToolStatus::Installed(path) => Some(path.clone()),
                    _ => self
                        .tooling
                        .path_executable(&entry.definition.executable)
                        .map(|(path, _)| path),
                }
                .is_some_and(|path| crate::debugger::adapter_starts(&path))
        })
    }

    /// One of the configured coding agents can start.
    fn any_agent_installed(&self) -> bool {
        self.configuration
            .settings()
            .agents
            .profiles
            .values()
            .any(|profile| self.agent_program(&profile.program).is_some())
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
        if !practice.tasks.is_empty() {
            for (name, task) in practice.tasks {
                self.configuration.replace_task(name, task);
            }
            self.rebuild_matcher_cache();
        }
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
        let chapters = self.tutor.chapters();
        let card = place.map(|(index, (place, away, line))| {
            let step = &self.tutor.steps[index];
            let (number, (_, range)) = chapters
                .iter()
                .enumerate()
                .find(|(_, (_, range))| range.contains(&index))
                .expect("every step is in a chapter");
            // A step that can't be done here says why where the card says where it happens.
            let (place, away) = match self.tutor_missing(index) {
                Some(missing) if !away && self.tutor.done_at.is_none() => (missing, true),
                _ => (place, away),
            };
            crate::ui::TutorCard {
                title: format!(
                    "Tutor {}/{} · {}/{} {} · {}/{}",
                    index + 1,
                    self.tutor.steps.len(),
                    number + 1,
                    chapters.len(),
                    step.chapter,
                    index - range.start + 1,
                    range.len()
                ),
                text: step.text.replace("<leader>", leader),
                press: step.press.replace("<leader>", leader),
                pointer: pointer.into(),
                praised: self.tutor.done_at.is_some(),
                footer: FOOTER.into(),
                target: self.tutor.target.clone(),
                place: place.replace("<leader>", leader),
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
            AppEffect::TutorChapters => self.open_tutor_chapters(),
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
                ("agent.model", "agent.tab.new"),
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
    fn syntax_lessons_work_in_every_practice_language() {
        use crate::app::editing::tests::{press_keys, wait_syntax};
        for language in Language::ALL {
            let mut app = app();
            let first = lessons(language)
                .unwrap()
                .iter()
                .position(|step| step.chapter == "Syntax editing")
                .unwrap();
            app.start_tutor(TutorStart {
                language,
                step: first,
            });
            enter(&mut app);
            crate::app::tests::wait_vfs(&mut app);
            app.tutor_tick();
            let buffer = app.state.active_buffer_id().unwrap();
            wait_syntax(&mut app, buffer);
            press_keys(&mut app, &["/", "1", "1", "enter"]);
            press_keys(&mut app, &["ctrl-space", "ctrl-space", "backspace"]);
            app.tutor_tick();
            assert!(app.tutor.done_at.is_some(), "{language:?}: grow/shrink");
            app.tutor_next();
            press_keys(&mut app, &["escape"]);
            app.tutor_tick();
            press_keys(&mut app, &["/", "2", "2", "enter", "v", "i", "a"]);
            app.tutor_tick();
            assert!(app.tutor.done_at.is_some(), "{language:?}: argument object");
            app.tutor_next();
            press_keys(&mut app, &["escape"]);
            app.tutor_tick();
            press_keys(&mut app, &["/", "1", "1", "enter", "space", "c", ">"]);
            app.tutor_tick();
            assert!(app.tutor.done_at.is_some(), "{language:?}: swap");
            assert!(
                app.state.buffers[buffer]
                    .text
                    .to_string()
                    .contains("(22, 11, 33)")
            );
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
            // A step judged by what changed, not by a command, only needs the key to exist.
            assert!(
                step.done.is_empty()
                    || step
                        .done
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

    /// The key presses a step's `keys` stand for: characters stand for themselves and
    /// `<name>` for a named key, with `<leader>` the leader key and `<lt>` a `<`.
    /// The keys of a step's `keys` script: characters stand for themselves, `<name>` is a named
    /// key (`<escape>`, `<ctrl-o>`, `<leader>`, `<lt>` for `<`) and `<wait>`, `None` here, lets
    /// the background work the keys started finish before the next key.
    fn key_script(keys: &str, leader: &str) -> Vec<Option<crate::editor::input::KeyEvent>> {
        use crate::editor::input::KeyEvent;
        let mut events = Vec::new();
        let mut rest = keys;
        while let Some(character) = rest.chars().next() {
            if character == '<'
                && let Some(end) = rest.find('>')
                && end > 1
            {
                let name = &rest[1..end];
                events.push(match name {
                    "wait" => None,
                    "leader" => Some(leader.parse().expect("leader key")),
                    "lt" => Some(KeyEvent::character('<')),
                    name => Some(
                        name.parse()
                            .unwrap_or_else(|_| panic!("unknown key <{name}> in {keys:?}")),
                    ),
                });
                rest = &rest[end + 1..];
                continue;
            }
            events.push(Some(KeyEvent::character(character)));
            rest = &rest[character.len_utf8()..];
        }
        events
    }

    /// One pass of the main loop's background work, then the tutor's tick.
    /// Presses a step's keys; a `<wait>` lets the app work for a moment, as a person reads.
    fn press(app: &mut App, keys: &str, leader: &str) {
        for key in key_script(keys, leader) {
            let Some(key) = key else {
                let until = Instant::now() + Duration::from_millis(1500);
                while Instant::now() < until {
                    pump(app);
                    std::thread::sleep(Duration::from_millis(5));
                }
                continue;
            };
            app.handle_event(crate::ui::UiEvent::Key {
                key,
                kind: crate::ui::KeyInputKind::Press,
            });
            pump(app);
        }
    }

    fn pump(app: &mut App) {
        app.queue_startup_reads();
        app.process_vfs_results();
        app.start_git_status_refresh_silent();
        app.process_git_status_events();
        app.process_git_hunk_events();
        app.process_git_sign_events();
        app.process_git_mutation_events();
        app.process_git_branch_events();
        app.process_workspace_file_events();
        app.process_workspace_search_events();
        app.process_explorer_watch_events();
        app.process_explorer_result();
        app.process_trust_events();
        app.process_configuration_result();
        app.process_theme_results();
        app.process_workspace_approval();
        app.process_disk_changes();
        app.process_dap_events();
        app.process_execution_preflight();
        app.process_pty_events();
        app.process_agent_events();
        app.process_job_events();
        app.process_syntax();
        app.process_persistence_events();
        app.tutor_tick();
    }

    /// An app whose practice project starts no language server, so the steps that need one
    /// take the path they take on a machine without it.
    fn app_without_servers() -> App {
        let mut app = app();
        let mut config = app.configuration.fixture_snapshot();
        config.settings.lsp.enabled = false;
        config.settings.lsp.auto_start = false;
        config.settings.lsp.auto_install = false;
        app.configuration.install_fixture(config);
        app
    }

    /// Walk the whole tutor in `language` with each step's own keys: every step must be done
    /// by them, or, when what it needs isn't here, say so on the card and wait for :TutorNext.
    fn walk_every_lesson(language: Language) {
        let mut app = app_without_servers();
        // A screen, so prompts and confirmations have room to show and be answered.
        app.handle_event(crate::ui::UiEvent::Resize(120, 40));
        app.start_tutor(TutorStart { language, step: 0 });
        enter(&mut app);
        let root = app.workspace.active_root().unwrap();
        let identity = crate::files::RootIdentity::capture(&app.state.workspace_root).unwrap();
        app.workspace.fixture_identity(root, identity).unwrap();
        // The new identity drops the file the first step was opening; open it again.
        app.tutor_go(0, false);
        let leader = app.configuration.settings().input.keymaps.leader.clone();
        let count = app.tutor.steps.len();
        let mut skipped = Vec::new();
        let mut failures = Vec::new();
        for index in 0..count {
            assert_eq!(app.tutor.current, Some(index), "{language:?}");
            // The scene: its file open and the cursor on its line.
            // A step that starts in another mode (after Visual mode, say) puts the cursor on
            // its line only once its keys go back to Normal mode.
            // Git work started by the step before (staging, say) finishes first, as it would
            // while a person reads the next card.
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut settled = None;
            let mut git_seen = false;
            loop {
                pump(&mut app);
                let git_busy = app.git.mutation_pending()
                    || app.git.status_pending()
                    || app.git.hunks_pending();
                git_seen |= git_busy;
                if !app.documents.has_work() && !git_busy {
                    let since = *settled.get_or_insert_with(Instant::now);
                    let quiet = !app.tutor.jump && !git_seen;
                    if quiet || since.elapsed() > Duration::from_millis(300) {
                        break;
                    }
                } else {
                    settled = None;
                }
                assert!(
                    Instant::now() < deadline,
                    "{language:?} step {}: the scene never settled",
                    index + 1
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            let step = &app.tutor.steps[index];
            let what = format!(
                "{language:?} step {} ({}): {}",
                index + 1,
                step.chapter,
                step.press
            );
            assert!(!step.keys.is_empty(), "{what} has no keys");
            if let Some(missing) = app.tutor_missing(index) {
                let card = app.ui.tutor.as_ref().expect("card");
                assert!(card.away, "{what}");
                assert!(card.place.contains(":TutorNext"), "{what}: {missing}");
                skipped.push(index + 1);
                app.tutor_next();
                continue;
            }
            let keys = step.keys.clone();
            let read_only = step.done.is_empty()
                && [
                    &step.line,
                    &step.gone,
                    &step.twice,
                    &step.indented,
                    &step.commented,
                ]
                .iter()
                .all(|check| check.is_none())
                && step.cursor.is_none()
                && step.staged.is_none()
                && step.clean.is_none();
            press(&mut app, &keys, &leader);
            if read_only {
                // A step with nothing to check is read and left with :TutorNext, and leaving
                // the last one moves back out of the practice project in the background
                // (slow on a Windows runner), or asks first while its terminals run.
                let deadline = Instant::now() + Duration::from_secs(60);
                let asked = |app: &App| {
                    app.notices
                        .history()
                        .any(|(_, text)| text.contains(":TutorQuit"))
                };
                while app.tutor.practice.is_some()
                    && app.tutor.current == Some(index)
                    && !(index + 1 == count && asked(&app))
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(5));
                    pump(&mut app);
                }
                // Terminals still running in the practice project make leaving ask first,
                // and `:TutorQuit` goes ahead, as the refusal says.
                if index + 1 == count && app.tutor.practice.is_some() {
                    let notice = format!("{:?}", app.notices.history().collect::<Vec<_>>());
                    assert!(notice.contains(":TutorQuit"), "{what}: {notice}");
                    for key in key_script(":TutorQuit<enter>", &leader)
                        .into_iter()
                        .flatten()
                    {
                        app.handle_event(crate::ui::UiEvent::Key {
                            key,
                            kind: crate::ui::KeyInputKind::Press,
                        });
                        pump(&mut app);
                    }
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while app.tutor.practice.is_some() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(5));
                        pump(&mut app);
                    }
                }
                if app.tutor.practice.is_some() && app.tutor.current == Some(index) {
                    failures.push(format!(
                        "{what}: still here. Notice: {:?}, ran {:?}, context {}",
                        app.ui.notification,
                        app.tutor.seen,
                        app.active_input_context()
                    ));
                }
                continue;
            }
            let deadline = Instant::now() + Duration::from_secs(8);
            while !app.tutor.completed.contains(&index) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                pump(&mut app);
            }
            // A debug adapter that dies on its own (lldb-dap on a Windows runner, now and then,
            // with nothing on stderr) is not the lesson's doing: the step is tried once more, as
            // a person would press F5 again.
            let adapter_died =
                app.notices.history().rev().take(6).any(|(_, text)| {
                    text.starts_with("debug adapter ") && text.contains(" failed: ")
                });
            if !app.tutor.completed.contains(&index) && adapter_died {
                let console = app
                    .debugger_session
                    .console()
                    .map(|entry| entry.text.clone())
                    .collect::<Vec<_>>();
                eprintln!(
                    "{what}: the debug adapter died; trying the step again. Its console: {console:?}"
                );
                press(&mut app, &keys, &leader);
                let deadline = Instant::now() + Duration::from_secs(8);
                while !app.tutor.completed.contains(&index) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                    pump(&mut app);
                }
            }
            if !app.tutor.completed.contains(&index) {
                failures.push(format!(
                    "{what}: not done. Notice: {:?}, recent {:?}, ran {:?}, mode {:?}, context {}",
                    app.ui.notification,
                    app.notices.history().rev().take(6).collect::<Vec<_>>(),
                    app.tutor.seen,
                    app.state
                        .active_view()
                        .and_then(|view| app.state.views.get(view))
                        .map(|view| view.mode),
                    app.active_input_context()
                ));
            }
            // A step done during a `<wait>` may already have moved on after its praise.
            if app.tutor.current == Some(index) {
                app.tutor_go(index + 1, true);
            }
        }
        assert!(
            failures.is_empty(),
            "{} steps failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
        // The last step's :TutorNext finishes the tutor and leaves the practice project.
        assert!(
            app.tutor.practice.is_none(),
            "{language:?}: the tutor never finished"
        );
        println!("{language:?}: skipped steps without their tools: {skipped:?}");
    }

    #[test]
    fn every_rust_lesson_is_done_by_its_keys() {
        walk_every_lesson(Language::Rust);
    }

    #[test]
    fn every_python_lesson_is_done_by_its_keys() {
        walk_every_lesson(Language::Python);
    }

    #[test]
    fn every_typescript_lesson_is_done_by_its_keys() {
        walk_every_lesson(Language::TypeScript);
    }

    #[test]
    fn chapters_show_progress_and_jump_to_the_first_step_not_done() {
        let mut app = app();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step: 0,
        });
        enter(&mut app);
        let chapters = app
            .tutor
            .chapters()
            .into_iter()
            .map(|(name, range)| (name.to_owned(), range))
            .collect::<Vec<_>>();
        assert!(chapters.len() > 20);
        // Every chapter is one run of steps: no name comes back later.
        let mut names = chapters.iter().map(|(name, _)| name).collect::<Vec<_>>();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), chapters.len());
        let title = app.ui.tutor.as_ref().unwrap().title.clone();
        assert!(title.starts_with("Tutor 1/"), "{title}");
        assert!(
            title.contains(&format!("1/{} Welcome", chapters.len())),
            "{title}"
        );
        let (_, third) = &chapters[2];
        app.tutor.completed.insert(third.start);
        app.process_effect(AppEffect::TutorChapters);
        assert!(app.ui.picker.is_some());
        for _ in 0..2 {
            crate::app::editing::tests::press(&mut app, "down");
        }
        crate::app::editing::tests::press(&mut app, "enter");
        app.tutor_tick();
        assert_eq!(app.tutor.current, Some(third.start + 1));
        let title = app.ui.tutor.as_ref().unwrap().title.clone();
        assert!(title.contains(&format!("2/{}", third.len())), "{title}");
        // The progress keeps what was done.
        let saved = std::fs::read_to_string(&app.tutor.progress).unwrap();
        assert!(
            saved.ends_with(&format!(" rust {}", third.start)),
            "{saved}"
        );
        assert!(app.tutor.saved_completed().contains(&third.start));
    }

    #[test]
    fn a_step_that_needs_a_server_says_so_until_one_is_ready() {
        let mut app = app_without_servers();
        let step = lessons(Language::Rust)
            .unwrap()
            .iter()
            .position(|step| step.needs.as_deref() == Some("server"))
            .unwrap();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step,
        });
        enter(&mut app);
        let index = app.tutor.current.unwrap();
        assert!(app.tutor.steps[index].needs.is_some() || index < step);
        app.tutor_go(step, false);
        let card = app.ui.tutor.as_ref().unwrap();
        assert!(
            card.away && card.place.contains("rust-analyzer"),
            "{}",
            card.place
        );
        // The command alone doesn't do a step that can't work here.
        app.tutor.observed = true;
        app.tutor_tick();
        assert!(app.tutor.done_at.is_none());
        app.tutor_next();
        assert_eq!(app.tutor.current, Some(step + 1));
    }

    #[test]
    fn a_command_that_reports_an_error_does_not_do_the_step() {
        let mut app = app();
        app.start_tutor(TutorStart {
            language: Language::Rust,
            step: 0,
        });
        enter(&mut app);
        let invocation = CommandInvocation::new("input.motion", InvocationSource::Keymap)
            .unwrap()
            .with_argument("motion", CommandValue::String("down".into()));
        let before = app.tutor.errors;
        app.show_error("something went wrong");
        app.tutor_observe(&invocation, before);
        assert!(!app.tutor.observed);
        app.tutor_observe(&invocation, app.tutor.errors);
        assert!(app.tutor.observed);
    }

    #[test]
    fn the_practice_conflict_stops_a_merge_on_notes() {
        for language in Language::ALL {
            let folder = create_practice_project(language).expect("practice project");
            start_practice_conflict(folder.path()).expect("conflict");
            // Once stopped it stays as it is.
            start_practice_conflict(folder.path()).expect("conflict again");
            let repository = git2::Repository::open(folder.path()).unwrap();
            assert_eq!(repository.state(), git2::RepositoryState::Merge);
            assert!(repository.index().unwrap().has_conflicts());
            assert!(repository.find_reference("ORIG_HEAD").is_ok());
            let notes = std::fs::read_to_string(folder.path().join("notes.txt")).unwrap();
            assert!(
                notes.contains("<<<<<<< ")
                    && notes.contains("Favourite colour: blue")
                    && notes.contains("Favourite colour: green"),
                "{notes}"
            );
            // The greet file keeps its change for the git lessons.
            let greet = language.practice_names()[2];
            let status = repository.status_file(Path::new(greet)).unwrap();
            assert!(status.contains(git2::Status::WT_MODIFIED));
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
