//! Command registration and execution for document opening, saving and recovery.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::documents::LargeFileOverride;
use crate::app::documents::mode_name_large;

use crate::app::commands::bool_arg;
use crate::app::commands::optional_bool_arg;
use crate::app::commands::optional_string_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use std::path::PathBuf;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "edit.substitute_lines",
        "Substitute in lines",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("range", ArgumentType::String, true)
            .field("pattern", ArgumentType::String, true)
            .field("replacement", ArgumentType::String, true)
            .field("flags", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::Substitute {
                range: string_arg(invocation, "range")?.to_owned(),
                pattern: string_arg(invocation, "pattern")?.to_owned(),
                replacement: string_arg(invocation, "replacement")?.to_owned(),
                flags: string_arg(invocation, "flags")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "edit.global_delete",
        "Delete matching lines",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("range", ArgumentType::String, true)
            .field("pattern", ArgumentType::String, true)
            .field("invert", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::GlobalDelete {
                range: string_arg(invocation, "range")?.to_owned(),
                pattern: string_arg(invocation, "pattern")?.to_owned(),
                invert: bool_arg(invocation, "invert")?,
            })
        },
    )?;
    register(
        registry,
        "file.save",
        "Save file",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("path", ArgumentType::String, false)
            .field("force", ArgumentType::Bool, false),
        |invocation| AppEffect::Save {
            quit_after: false,
            path: optional_string_arg(invocation, "path").map(PathBuf::from),
            force: optional_bool_arg(invocation, "force").unwrap_or(false),
        },
    )?;
    register(
        registry,
        "file.save_quit",
        "Save and quit",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| AppEffect::Save {
            quit_after: true,
            path: None,
            force: optional_bool_arg(invocation, "force").unwrap_or(false),
        },
    )?;
    for (id, title, quit_after) in [
        ("file.save_all", "Save all files", false),
        ("file.save_all_quit", "Save all files and quit", true),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::SaveAll { quit_after },
        )?;
    }
    register(
        registry,
        "file.markdown_preview",
        "Preview Markdown",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::MarkdownPreview,
    )?;
    register(
        registry,
        "file.open",
        "Open file by path",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("path", ArgumentType::String, true)
            .field("force", ArgumentType::Bool, false),
        |invocation| {
            let path = PathBuf::from(string_arg(invocation, "path")?);
            Ok(if optional_bool_arg(invocation, "force") == Some(true) {
                AppEffect::ForceOpenPath(path)
            } else {
                AppEffect::OpenPath(path)
            })
        },
    )?;
    register(
        registry,
        "file.reload",
        "Reload file",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::Reload {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    register(
        registry,
        "buffer.close",
        "Close buffer",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::CloseBuffer {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    for (id, title, offset) in [
        ("buffer.next", "Show next buffer", 1),
        ("buffer.previous", "Show previous buffer", -1),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |invocation| {
                AppEffect::CycleBuffer(
                    offset
                        * isize::try_from(invocation.count.map_or(1, |count| count.get()))
                            .unwrap_or(isize::MAX),
                )
            },
        )?;
    }
    register(
        registry,
        "buffer.pin.toggle",
        "Toggle active buffer pin",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::ToggleBufferPin,
    )?;
    for (id, title, direction) in [
        ("buffer.move_left", "Move buffer left", -1),
        ("buffer.move_right", "Move buffer right", 1),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Buffer,
            ArgumentSchema::default(),
            move |_| AppEffect::MoveBufferLine(direction),
        )?;
    }
    register(
        registry,
        "file.new",
        "New buffer",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::NewBuffer,
    )?;
    register(
        registry,
        "buffer.new_tab",
        "New tab: an empty buffer with the file finder over it",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::NewTab,
    )?;
    register(
        registry,
        "buffer.large_file.set",
        "Set large-file mode",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("mode", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LargeFileSet(
                string_arg(invocation, "mode")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "buffer.large_file.status",
        "Show large-file mode",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::LargeFileStatus,
    )?;
    register(
        registry,
        "buffer.virtual.open",
        "Open a read-only plugin virtual document",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("source", ArgumentType::String, true)
            .field("key", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::VirtualDocumentOpen {
                source: string_arg(invocation, "source")?.into(),
                key: string_arg(invocation, "key")?.into(),
            })
        },
    )?;
    register(
        registry,
        "buffer.virtual.refresh",
        "Refresh focused read-only virtual document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::VirtualDocumentRefresh,
    )?;
    Ok(())
}

use crate::editor::text::TextBuffer;

use crate::app::documents::create_user_file;

use crate::language::lsp::LspMarkupContent;
use crate::language::lsp::LspMarkupKind;
use crate::ui::DocumentationPopupKind;

impl App {
    pub(in crate::app) fn process_documents_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Save {
                quit_after,
                path,
                force,
            } => {
                if path.is_some() || force || !self.begin_format_on_save(quit_after) {
                    self.start_save(quit_after, path, force);
                }
            }
            AppEffect::SaveAll { quit_after } => self.save_all(quit_after),
            AppEffect::OpenPath(path) => self.edit_path(path),
            AppEffect::ForceOpenPath(path) => {
                let wanted = crate::files::canonicalize(self.state.workspace_root.join(&path)).ok();
                let current = self
                    .state
                    .active_buffer_id()
                    .and_then(|id| self.state.buffers[id].path.as_ref())
                    .and_then(|file| crate::files::canonicalize(file).ok());
                if wanted.is_some() && wanted == current {
                    self.start_reload(true);
                } else {
                    self.edit_path(path);
                }
            }
            AppEffect::OpenUserFile(path) => match create_user_file(&path) {
                Ok(()) => self.start_open(path),
                Err(error) => self.show_error(format!("{}: {error}", path.display())),
            },
            AppEffect::Reload { force } => self.start_reload(force),
            AppEffect::CloseBuffer { force } => self.close_active_buffer(force),
            AppEffect::CycleBuffer(offset) => {
                self.project_window_content_tabs();
                let order = self.window_session.content_tab_buffers(&self.state);
                match self.state.cycle_buffer(&order, offset, true) {
                    Ok(_) => {
                        self.set_dashboard_visibility(false);
                    }
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::ToggleBufferPin => self.toggle_active_buffer_pin(),
            AppEffect::MoveBufferLine(direction) => self.move_active_buffer_line(direction),
            AppEffect::NewBuffer => {
                if let Err(error) = self.state.open_text(TextBuffer::from_text(""), None) {
                    self.show_error(error);
                    return Ok(());
                }
                self.set_dashboard_visibility(false);
                self.notify_message(self.message("new_scratch", &[]), false);
            }
            AppEffect::LargeFileSet(mode) => {
                let Some(buffer) = self
                    .state
                    .active_view()
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let mode = match mode.as_str() {
                    "auto" => LargeFileOverride::Auto,
                    "on" => LargeFileOverride::On,
                    "off" => LargeFileOverride::Off,
                    _ => {
                        self.show_error(self.message("large_file_invalid", &[]));
                        return Ok(());
                    }
                };
                self.documents.set_large_file_mode(
                    buffer,
                    self.state.buffers[buffer].path.as_deref(),
                    mode,
                );
                self.process_large_file_modes();
                self.notify_message(
                    self.message(
                        "large_file_changed",
                        &[("mode", mode_name_large(mode).into())],
                    ),
                    false,
                );
            }
            AppEffect::LargeFileStatus => {
                let Some(buffer) = self
                    .state
                    .active_view()
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let configured = self.documents.configured_mode(buffer);
                let effective = if self.effective_large_file(buffer) {
                    "on"
                } else {
                    "off"
                };
                self.notify_message(
                    self.message(
                        "large_file_status",
                        &[
                            ("mode", mode_name_large(configured).into()),
                            ("effective", effective.into()),
                        ],
                    ),
                    false,
                );
            }
            AppEffect::WalkJumps(back) => self.walk_jumps(back),
            AppEffect::RecoveryInspect => self.open_recovery_picker(),
            AppEffect::MarkdownPreview => {
                let Some(buffer) = self
                    .state
                    .active_buffer_id()
                    .map(|id| &self.state.buffers[id])
                else {
                    return Ok(());
                };
                let title = buffer.path.as_ref().map_or_else(
                    || "Markdown".to_owned(),
                    |path| self.workspace_relative_label(path),
                );
                let value = buffer.text.to_string();
                self.present_documentation_popup(
                    DocumentationPopupKind::Git,
                    title,
                    "Markdown".into(),
                    None,
                    vec![LspMarkupContent {
                        kind: LspMarkupKind::Markdown,
                        value,
                    }],
                );
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
