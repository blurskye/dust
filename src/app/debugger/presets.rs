//! Project launch presets, adapter installation and Cargo executable selection.
#[cfg(test)]
#[path = "presets_tests.rs"]
mod tests;
use crate::app::App;
use crate::app::TrustedAction;
use crate::app::picker::PickerPayload;
use crate::app::workspace::WorkspaceVersion;
use crate::process::{JobEvent, JobId, JobPolicy, JobRequest, JobStream, JobSupervisor};
use crate::tools::{ToolEntry, ToolStatus};
use crate::workspace::{PickerAction, PickerItem, PickerItemId};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preset {
    id: String,
    label: String,
    languages: Vec<String>,
    markers: Vec<String>,
    adapters: Vec<String>,
    target: Target,
    runtime: Option<String>,
    #[serde(default)]
    build: Vec<String>,
    #[serde(default)]
    list_tests: Vec<String>,
    #[serde(default)]
    test_arguments: Vec<String>,
    #[serde(default)]
    arguments: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Target {
    Binary,
    File,
    Module,
    Package,
}

#[derive(Clone, Debug)]
pub(in crate::app) struct PresetLaunch {
    preset: Preset,
    document: PathBuf,
    root: PathBuf,
    adapter: String,
    program: Option<PathBuf>,
    test: Option<String>,
    scope: WorkspaceVersion,
    revision: u64,
}

pub(super) struct CargoBuild {
    launch: PresetLaunch,
    job: JobId,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
    listing_tests: bool,
}

fn bundled_presets() -> Vec<Preset> {
    #[derive(Deserialize)]
    struct Presets {
        preset: Vec<Preset>,
    }
    toml::from_str::<Presets>(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/defaults/debug-presets.toml"
    )))
    .expect("bundled debug presets are valid")
    .preset
}

fn project_root(document: &Path, workspace: &Path, markers: &[String]) -> Option<PathBuf> {
    document
        .parent()?
        .ancestors()
        .take_while(|p| p.starts_with(workspace))
        .find(|p| markers.iter().any(|marker| p.join(marker).is_file()))
        .map(Path::to_path_buf)
}

fn select_adapter(preset: &Preset, tools: &[ToolEntry]) -> String {
    preset
        .adapters
        .iter()
        .find(|id| {
            tools.iter().any(|entry| {
                entry.definition.id == **id && matches!(entry.status, ToolStatus::Installed(_))
            })
        })
        .unwrap_or(&preset.adapters[0])
        .clone()
}

fn replace_file(value: &mut serde_json::Value, path: &Path) {
    match value {
        serde_json::Value::String(text) if text == "{file}" => {
            *text = path.to_string_lossy().into_owned()
        }
        serde_json::Value::Array(values) => values.iter_mut().for_each(|v| replace_file(v, path)),
        _ => {}
    }
}

fn cargo_executables(bytes: &[u8]) -> Result<Vec<(String, PathBuf)>, String> {
    let mut executables = BTreeMap::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: serde_json::Value = serde_json::from_slice(line)
            .map_err(|error| format!("invalid Cargo output: {error}"))?;
        if value["reason"] != "compiler-artifact" {
            continue;
        }
        if let (Some(path), Some(name)) = (
            value["executable"].as_str(),
            value["target"]["name"].as_str(),
        ) {
            executables.insert(PathBuf::from(path), name.to_owned());
        }
    }
    Ok(executables
        .into_iter()
        .map(|(path, name)| (name, path))
        .collect())
}

fn rust_test_names(output: &[u8], name: &str) -> Vec<String> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| {
            let test = line.strip_suffix(": test")?;
            (test.rsplit("::").next() == Some(name)).then(|| test.to_owned())
        })
        .collect()
}

fn cursor_test(
    language: &str,
    source: &str,
    cursor: usize,
    objects: &[crate::editor::SyntaxObject],
) -> Option<(&'static str, String)> {
    use crate::editor::SyntaxObjectKind;
    let function = objects
        .iter()
        .filter(|object| {
            object.kind == SyntaxObjectKind::Function
                && object.outer.0 <= cursor
                && cursor < object.outer.1
        })
        .min_by_key(|object| object.outer.1 - object.outer.0)?;
    let header = source
        .get(function.outer.0..function.outer.1)?
        .split_once('(')?
        .0;
    let name = header.split_whitespace().last()?;
    match language {
        "python" if name.starts_with("test_") => {
            let mut classes = objects
                .iter()
                .filter(|object| {
                    object.kind == SyntaxObjectKind::Class
                        && object.outer.0 < function.outer.0
                        && object.outer.1 >= function.outer.1
                })
                .collect::<Vec<_>>();
            classes.sort_by_key(|object| object.outer.0);
            let mut parts = Vec::new();
            for class in classes {
                let declaration = source.get(class.outer.0..class.outer.1)?;
                let name = declaration
                    .strip_prefix("class ")?
                    .split(['(', ':'])
                    .next()?
                    .trim();
                if !name.starts_with("Test") {
                    return None;
                }
                parts.push(name);
            }
            parts.push(name);
            Some(("python-tests", parts.join("::")))
        }
        "go" if name.starts_with("Test") && !name[4..].starts_with(char::is_lowercase) => {
            Some(("go-tests", name.to_owned()))
        }
        "rust" => Some(("rust-tests", name.to_owned())),
        _ => None,
    }
}

impl App {
    fn detected_debug_presets(&self) -> Vec<PresetLaunch> {
        let document = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers.get(id))
            .and_then(|buffer| buffer.path.clone());
        let workspace = document
            .as_deref()
            .and_then(|path| self.workspace.root_for_canonical_path(path))
            .or_else(|| {
                self.workspace
                    .roots()
                    .iter()
                    .find(|root| Some(root.id) == self.workspace.active_root())
            });
        let Some(workspace) = workspace else {
            return Vec::new();
        };
        let workspace = &workspace.identity.canonical_path;
        let language = document
            .as_deref()
            .and_then(crate::language::lsp::language_id_for_path);
        bundled_presets()
            .into_iter()
            .filter_map(|mut preset| {
                let source = document.as_deref().unwrap_or(workspace);
                let root = project_root(source, workspace, &preset.markers).or_else(|| {
                    preset
                        .markers
                        .iter()
                        .any(|m| workspace.join(m).is_file())
                        .then(|| workspace.clone())
                });
                let matches_language =
                    language.is_some_and(|lang| preset.languages.iter().any(|l| l == lang));
                if !matches_language && root.is_none() {
                    return None;
                }
                // File/package presets need a matching source; Cargo can start from the dashboard.
                if !matches!(preset.target, Target::Binary) && !matches_language {
                    return None;
                }
                let root = root.unwrap_or_else(|| workspace.clone());
                if matches!(preset.target, Target::Binary) && !root.join("Cargo.toml").is_file() {
                    return None;
                }
                let document = document.clone().unwrap_or_else(|| root.join("Cargo.toml"));
                for value in preset.arguments.values_mut() {
                    replace_file(value, &document);
                }
                Some(PresetLaunch {
                    adapter: select_adapter(&preset, self.tooling.entries()),
                    preset,
                    document,
                    root,
                    program: None,
                    test: None,
                    scope: self.workspace.version(),
                    revision: self.configuration.binding_revision(),
                })
            })
            .collect()
    }

    pub(in crate::app) fn debug_cursor_test(&mut self) {
        let Some(view) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view];
        let buffer = &self.state.buffers[view.buffer];
        let Some(path) = buffer.path.as_deref() else {
            self.show_error("Save the test file with :w, then press Space t d");
            return;
        };
        let language = crate::language::lsp::language_id_for_path(path).unwrap_or("");
        let source = buffer.text.to_string();
        let test = cursor_test(
            language,
            &source,
            view.cursor.0,
            buffer.current_syntax_objects(),
        );
        let Some((preset, test)) = test else {
            self.show_error("Place the cursor inside a Rust, Python pytest or Go Test function, then Space t d; Space d s opens all debug launches");
            return;
        };
        let Some(mut launch) = self
            .detected_debug_presets()
            .into_iter()
            .find(|p| p.preset.id == preset)
        else {
            self.show_error("No test launch found; open the project with :e, then Space t d");
            return;
        };
        if language == "rust" {
            launch.test = Some(test);
            self.start_debug_preset(launch);
            return;
        }
        let args = launch
            .preset
            .test_arguments
            .iter()
            .map(|arg| {
                arg.replace("{file}", &path.to_string_lossy())
                    .replace("{test}", &test)
            })
            .collect::<Vec<_>>();
        launch
            .preset
            .arguments
            .insert("args".into(), serde_json::json!(args));
        self.start_debug_preset(launch);
    }

    pub(in crate::app) fn open_debug_launch_picker(&mut self) {
        let mut rows = self
            .configuration
            .settings()
            .debug
            .sessions
            .iter()
            .map(|(name, session)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("debug-config:{name}")),
                        label: name.clone(),
                        detail: session.adapter.clone(),
                        source: "Saved".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::DebugConfiguration(name.clone()),
                )
            })
            .collect::<Vec<_>>();
        for launch in self.detected_debug_presets() {
            let entry = self
                .tooling
                .entries()
                .iter()
                .find(|e| e.definition.id == launch.adapter);
            let detail = match entry.map(|entry| &entry.status) {
                Some(ToolStatus::Installed(_)) => format!("{} · Enter: start", launch.adapter),
                Some(ToolStatus::Installing { .. }) => {
                    format!("{} installing · Enter: start when ready", launch.adapter)
                }
                _ if entry.is_some_and(|e| e.definition.install.is_some()) => {
                    format!("{} missing · Enter: install and start", launch.adapter)
                }
                _ => format!(
                    "{} missing · install on PATH, then Space d s",
                    launch.adapter
                ),
            };
            rows.push((
                PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                    id: PickerItemId(format!("debug-preset:{}", launch.preset.id)),
                    label: launch.preset.label.clone(),
                    detail,
                    source: "Detected".into(),
                    action: PickerAction::None,
                },
                PickerPayload::DebugLaunch(Box::new(launch)),
            ));
        }
        if rows.is_empty() {
            self.show_error("No debug preset: open a Rust, Python, JavaScript, TypeScript or Go file with :e, then press Space d s; or add [debug.sessions] in config.toml");
            return;
        }
        self.install_picker("Debug launch".into(), rows);
    }

    pub(in crate::app) fn start_debug_preset(&mut self, launch: PresetLaunch) {
        if !self.require_effect_access(crate::editor::CommandAccess::Process) {
            return;
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        if launch.scope != self.workspace.version()
            || launch.revision != self.configuration.binding_revision()
        {
            self.show_error("Debug selection changed; press Space d s to choose again");
            return;
        }
        if self
            .state
            .buffers
            .iter()
            .any(|(_, buffer)| buffer.dirty && buffer.path.as_ref() == Some(&launch.document))
        {
            self.show_error("Save the debug source with :w, then press Space d s");
            return;
        }
        if let Some(runtime) = &launch.preset.runtime
            && crate::tools::discover_executable(runtime).is_none()
        {
            self.show_error(format!(
                "{runtime} is missing: install it on PATH, then press Space d s"
            ));
            return;
        }
        self.debugger_session.refresh(
            &self.configuration.settings().debug,
            self.workspace.version(),
        );
        self.debugger_session.cancel_preset_launch();
        let entry = self
            .tooling
            .entries()
            .iter()
            .find(|entry| entry.definition.id == launch.adapter);
        match entry.map(|entry| &entry.status) {
            Some(ToolStatus::Installed(_)) => {}
            Some(ToolStatus::Installing { .. }) => {
                self.debugger_session.install_launch = Some(launch);
                return;
            }
            _ => {
                if entry.is_none_or(|entry| entry.definition.install.is_none()) {
                    self.show_error(format!("{} is missing: install it on PATH, then press Space d s; Space c m opens Tools", launch.adapter));
                    return;
                }
                self.install_tool_by_id(&launch.adapter, None);
                if self.tooling.entries().iter().any(|e| {
                    e.definition.id == launch.adapter
                        && matches!(e.status, ToolStatus::Installing { .. })
                }) {
                    self.debugger_session.install_launch = Some(launch);
                }
                return;
            }
        }
        if (!launch.preset.build.is_empty() && launch.program.is_none()) || launch.test.is_some() {
            self.build_debug_preset(launch);
            return;
        }
        self.launch_debug_preset(launch);
    }

    fn build_debug_preset(&mut self, launch: PresetLaunch) {
        let listing_tests = launch.program.is_some() && launch.test.is_some();
        let executable = if listing_tests {
            let path = launch.program.as_ref().expect("test binary");
            match crate::files::canonicalize(path) {
                Ok(path) if self.workspace.require_path(&path).is_ok() => path,
                _ => {
                    self.show_error("Test binary is missing or outside the workspace; rebuild inside the project, then Space t d");
                    return;
                }
            }
        } else {
            let Some(cargo) = crate::tools::discover_executable("cargo") else {
                self.show_error(
                    "Cargo is missing: install the Rust toolchain, then press Space d s",
                );
                return;
            };
            cargo
        };
        let arguments = if listing_tests {
            &launch.preset.list_tests
        } else {
            &launch.preset.build
        };
        let Some(root) = self.workspace.root_for_canonical_path(&launch.root) else {
            self.show_error(
                "Debug project is outside the workspace; open it with :e, then Space d s",
            );
            return;
        };
        let jobs = self
            .debugger_session
            .build_jobs
            .get_or_insert_with(|| JobSupervisor::new(64));
        let result = jobs.reserve().and_then(|reservation| {
            reservation.submit_scoped(
                JobRequest {
                    program: executable,
                    argv0: None,
                    arguments: arguments.iter().map(Into::into).collect(),
                    cwd: launch.root.clone(),
                    environment: std::env::vars_os()
                        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
                        .collect(),
                    root_revision: self.workspace.root_revision(),
                    stdin: None,
                    policy: JobPolicy::default(),
                },
                root.identity.clone(),
            )
        });
        match result {
            Ok(job) => {
                self.notify_message(
                    format!(
                        "{} {}; Space d t cancels",
                        if listing_tests {
                            "Listing tests in"
                        } else {
                            "Building"
                        },
                        launch.preset.label
                    ),
                    false,
                );
                self.debugger_session.cargo_build = Some(CargoBuild {
                    launch,
                    job,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    truncated: false,
                    listing_tests,
                });
            }
            Err(error) => self.show_error(error),
        }
    }

    fn launch_debug_preset(&mut self, launch: PresetLaunch) {
        let program = match launch.preset.target {
            Target::Binary => launch.program.clone(),
            Target::File | Target::Module => Some(launch.document.clone()),
            Target::Package => launch.document.parent().map(Path::to_path_buf),
        };
        let result = self.debugger_session.request_debug(
            (None, Some(&launch.adapter)),
            program,
            Some((&launch.document, &launch.preset.languages[0])),
            (
                &self.configuration.settings().debug,
                &self.configuration.settings().terminals,
                self.configuration.binding_revision(),
            ),
            &self.workspace,
            self.tooling.entries(),
        );
        match result {
            Ok(intent) => {
                self.debugger_session.apply_preset(
                    &launch.root,
                    launch.preset.arguments.clone(),
                    matches!(launch.preset.target, Target::Module),
                );
                self.debugger_session.last_preset = Some(launch);
                self.run_trusted(TrustedAction::StartDebug(intent));
            }
            Err(error) => self.show_error(format!("{error}; press Space d s to choose a launch")),
        }
    }

    fn select_rust_cursor_test(&mut self, mut launch: PresetLaunch, output: &[u8]) {
        let name = launch.test.take().expect("cursor test name");
        let names = rust_test_names(output, &name);
        if names.is_empty() {
            self.show_error("No matching Rust test in this executable; press Space t d and choose its test target");
            return;
        }
        let mut launches = names
            .into_iter()
            .map(|name| {
                let mut launch = launch.clone();
                let arguments = launch
                    .preset
                    .test_arguments
                    .iter()
                    .map(|arg| {
                        if arg == "{test}" {
                            name.clone()
                        } else {
                            arg.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                launch
                    .preset
                    .arguments
                    .insert("args".into(), serde_json::json!(arguments));
                (name, launch)
            })
            .collect::<Vec<_>>();
        if launches.len() == 1 {
            self.start_debug_preset(launches.remove(0).1);
            return;
        }
        let rows = launches
            .into_iter()
            .map(|(name, launch)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("debug-test:{name}")),
                        label: name,
                        detail: "Enter: debug this exact test".into(),
                        source: "Cargo".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::DebugLaunch(Box::new(launch)),
                )
            })
            .collect();
        self.install_picker("Debug test".into(), rows);
    }

    pub(super) fn poll_debug_presets(&mut self) {
        if let Some(launch) = self.debugger_session.install_launch.take() {
            match self
                .tooling
                .entries()
                .iter()
                .find(|e| e.definition.id == launch.adapter)
                .map(|e| &e.status)
            {
                Some(ToolStatus::Installing { .. }) if launch.scope == self.workspace.version() => {
                    self.debugger_session.install_launch = Some(launch)
                }
                Some(ToolStatus::Installed(_)) => self.start_debug_preset(launch),
                // The Tools owner reports installer failures once.
                _ => {}
            }
        }
        let Some(jobs) = self.debugger_session.build_jobs.as_mut() else {
            return;
        };
        let events = jobs.poll();
        let Some(build) = self.debugger_session.cargo_build.as_mut() else {
            return;
        };
        if build.launch.scope != self.workspace.version()
            || build.launch.revision != self.configuration.binding_revision()
            || self.workspace.execution_allowed().is_err()
        {
            let _ = jobs.cancel(build.job);
            self.debugger_session.cargo_build = None;
            self.show_error("Debug build cancelled: workspace or settings changed; use :trust if needed, then Space d s");
            return;
        }
        let mut finished = None;
        for event in events {
            match event {
                JobEvent::Output {
                    job,
                    stream,
                    bytes,
                    truncated,
                } if job == build.job => {
                    let output = if stream == JobStream::Stdout {
                        &mut build.stdout
                    } else {
                        &mut build.stderr
                    };
                    let remaining = (16 * 1024 * 1024usize).saturating_sub(output.len());
                    build.truncated |= truncated || bytes.len() > remaining;
                    output.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
                }
                JobEvent::Exited {
                    job,
                    code,
                    timed_out,
                } if job == build.job => finished = Some(code == Some(0) && !timed_out),
                JobEvent::Failed { job, message } if job == build.job => {
                    build.stderr.extend_from_slice(message.as_bytes());
                    finished = Some(false);
                }
                _ => {}
            }
        }
        if let Some(success) = finished {
            let build = self
                .debugger_session
                .cargo_build
                .take()
                .expect("finished build");
            if !success || build.truncated {
                self.show_error(format!("Debug build failed: {}. Fix the build in the terminal (Ctrl-/), then Space d s", String::from_utf8_lossy(&build.stderr).chars().take(1500).collect::<String>()));
                return;
            }
            if build.listing_tests {
                self.select_rust_cursor_test(build.launch, &build.stdout);
                return;
            }
            match cargo_executables(&build.stdout) {
                Ok(executables) if executables.is_empty() => self.show_error("Cargo produced no executable; add a binary or test target, then press Space d s"),
                Ok(mut executables) if executables.len() == 1 => {
                    let mut launch = build.launch;
                    launch.program = Some(executables.remove(0).1);
                    self.start_debug_preset(launch);
                }
                Ok(executables) => {
                    let rows = executables.into_iter().map(|(name, path)| {
                        let mut launch = build.launch.clone();
                        launch.program = Some(path.clone());
                        (PickerItem {
                            badge: String::new(), kind: crate::workspace::PickerItemKind::Entry,
                            id: PickerItemId(format!("debug-executable:{}", path.display())), label: name,
                            detail: path.display().to_string(), source: "Cargo".into(), action: PickerAction::None,
                        }, PickerPayload::DebugLaunch(Box::new(launch)))
                    }).collect();
                    self.install_picker("Debug executable".into(), rows);
                }
                Err(error) => self.show_error(format!("{error}; retry with Space d s")),
            }
        }
    }
}

impl super::Debugger {
    pub(super) fn cancel_preset_launch(&mut self) {
        self.install_launch = None;
        if let Some(build) = self.cargo_build.take()
            && let Some(jobs) = self.build_jobs.as_mut()
        {
            let _ = jobs.cancel(build.job);
        }
    }
}
