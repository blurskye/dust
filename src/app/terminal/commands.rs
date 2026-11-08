//! Command registration and execution for terminal sessions and editor windows.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::picker::PickerPayload;
use crate::app::terminal::TerminalInput;
use crate::app::windows::NativeSurfaceTarget;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
// Remaining editor/window routing; terminal data and algorithms live in the owner.
use crate::process::PtyId;

use crate::app::commands::integer_arg;
use crate::app::commands::optional_bool_arg;
use crate::app::commands::optional_integer_arg;
use crate::app::commands::optional_string_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "terminal.open",
        "Open terminal profile",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("profile", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::TerminalOpen(
                optional_string_arg(invocation, "profile").map(str::to_owned),
            ))
        },
    )?;
    register(
        registry,
        "terminal.toggle",
        "Open or toggle focus for the bottom terminal",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalToggle,
    )?;
    register(
        registry,
        "terminal.list",
        "List terminals",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalList,
    )?;
    register(
        registry,
        "terminal.focus",
        "Focus terminal",
        ContextRequirement::Application,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::TerminalFocus(
                u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                    CommandError::InvalidArguments("terminal id must be non-negative".into())
                })?,
            ))
        },
    )?;
    register(
        registry,
        "terminal.focus_active",
        "Focus the visible terminal panel",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalFocusActive,
    )?;
    register(
        registry,
        "terminal.cycle",
        "Cycle the selected terminal",
        ContextRequirement::Application,
        ArgumentSchema::default().field("direction", ArgumentType::Integer, true),
        |invocation| {
            let direction = i8::try_from(integer_arg(invocation, "direction")?).map_err(|_| {
                CommandError::InvalidArguments("terminal direction is outside i8 bounds".into())
            })?;
            Ok(AppEffect::TerminalCycle(direction))
        },
    )?;
    register(
        registry,
        "terminal.resize",
        "Resize the bottom terminal panel by rows",
        ContextRequirement::Application,
        ArgumentSchema::default().field("rows", ArgumentType::Integer, true),
        |invocation| {
            let rows = i16::try_from(integer_arg(invocation, "rows")?).map_err(|_| {
                CommandError::InvalidArguments("terminal resize is outside i16 bounds".into())
            })?;
            Ok(AppEffect::TerminalResize(rows))
        },
    )?;
    register(
        registry,
        "terminal.dock",
        "Dock the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("edge", ArgumentType::String, false)
            .field("size", ArgumentType::Integer, false),
        |invocation| {
            let size = u16::try_from(optional_integer_arg(invocation, "size").unwrap_or(12))
                .map_err(|_| {
                    CommandError::InvalidArguments(
                        "terminal dock size is outside u16 bounds".into(),
                    )
                })?;
            Ok(AppEffect::TerminalDock {
                edge: optional_string_arg(invocation, "edge")
                    .unwrap_or("bottom")
                    .to_owned(),
                size,
            })
        },
    )?;
    register(
        registry,
        "terminal.float",
        "Float the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("x", ArgumentType::Integer, false)
            .field("y", ArgumentType::Integer, false)
            .field("width", ArgumentType::Integer, false)
            .field("height", ArgumentType::Integer, false),
        |invocation| {
            if ["x", "y", "width", "height"]
                .iter()
                .all(|name| optional_integer_arg(invocation, name).is_none())
            {
                return Ok(AppEffect::TerminalFloat(None));
            }
            let bounded = |name| {
                u16::try_from(integer_arg(invocation, name)?).map_err(|_| {
                    CommandError::InvalidArguments(format!(
                        "terminal float {name} is outside u16 bounds"
                    ))
                })
            };
            Ok(AppEffect::TerminalFloat(Some(crate::windows::WindowRect {
                x: bounded("x")?,
                y: bounded("y")?,
                width: bounded("width")?,
                height: bounded("height")?,
            })))
        },
    )?;
    register(
        registry,
        "terminal.fullscreen",
        "Fullscreen the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalFullscreen,
    )?;
    register(
        registry,
        "terminal.hide",
        "Hide the selected terminal window without closing its process",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalHide,
    )?;
    register(
        registry,
        "terminal.send",
        "Send terminal text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TerminalSend(
                string_arg(invocation, "text")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "terminal.mode.normal",
        "Enter terminal normal mode",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalModeNormal,
    )?;
    register(
        registry,
        "terminal.mode.input",
        "Return to terminal input mode",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalModeInput,
    )?;
    register(
        registry,
        "terminal.normal.move",
        "Move the terminal normal-mode cursor",
        ContextRequirement::Application,
        ArgumentSchema::default().field("motion", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TerminalNormalMove(
                string_arg(invocation, "motion")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "terminal.visual.start",
        "Start a terminal visual selection",
        ContextRequirement::Application,
        ArgumentSchema::default().field("linewise", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::TerminalVisualStart(
                optional_bool_arg(invocation, "linewise").unwrap_or(false),
            ))
        },
    )?;
    register(
        registry,
        "terminal.yank",
        "Yank terminal text into Dust registers",
        ContextRequirement::Application,
        ArgumentSchema::default().field("line", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::TerminalYank {
                line: optional_bool_arg(invocation, "line").unwrap_or(false),
            })
        },
    )?;
    register(
        registry,
        "terminal.unfocus",
        "Return focus from terminal to editor",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalUnfocus,
    )?;
    register(
        registry,
        "terminal.close",
        "Close focused terminal",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalClose,
    )?;
    register(
        registry,
        "terminal.quit",
        "Stop the focused terminal without asking",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalQuit,
    )?;
    Ok(())
}

// Remaining editor/window routing; terminal data and algorithms live in the owner.

// Remaining editor/window routing; terminal data and algorithms live in the owner.

impl App {
    pub(in crate::app) fn process_terminal_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::TerminalOpen(profile) => {
                let profile = profile.unwrap_or_else(|| {
                    self.configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone()
                });
                self.request_terminal_open(&profile, None);
            }
            AppEffect::TerminalToggle => {
                // This is the panel's terminal. A side terminal, such as a docked agent, keeps its
                // window, so from one this opens the panel beside it.
                let focused = self
                    .window_session
                    .focused_terminal()
                    .map(PtyId)
                    .filter(|id| self.terminal_session.contains(*id))
                    .filter(|id| !self.terminal_session.side(*id));
                if let Some(id) = focused {
                    if let Err(error) = self.apply_native_window_placement(
                        NativeSurfaceTarget::Terminal(id.0),
                        WindowPlacement::Hidden,
                    ) {
                        self.show_error(error);
                    }
                } else if let Some(id) = self.terminal_session.panel() {
                    self.reveal_terminal(id);
                } else {
                    let profile = self
                        .configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone();
                    self.request_terminal_open(&profile, None);
                }
            }
            AppEffect::TerminalList => {
                let items = self
                    .terminal_session
                    .choices()
                    .into_iter()
                    .enumerate()
                    .map(|(index, (key, label, detail))| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("terminal:{index}")),
                                label,
                                detail,
                                source: "terminal".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Terminal(key),
                        )
                    })
                    .collect();
                self.install_picker("Terminals".into(), items);
            }
            AppEffect::TerminalFocus(id) => {
                let id = PtyId(id);
                if self.terminal_session.select(id) {
                    self.reveal_terminal(id);
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalFocusActive => {
                if self.terminal_session.selected().is_some() {
                    self.set_terminal_focus(true);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalCycle(direction) => {
                let focused = self.window_session.terminal_focused();
                let hidden = self.terminal_window_rect().is_none();
                if !self.terminal_session.cycle(i64::from(direction)) {
                    self.show_error(self.message("terminal_missing", &[]));
                } else if let Some(id) = self
                    .terminal_session
                    .selected()
                    .filter(|_| focused || hidden)
                {
                    self.reveal_terminal(id);
                } else {
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                }
            }
            AppEffect::TerminalResize(rows) => {
                if let Some(id) = self.displayed_terminal() {
                    let result = if self.compositor_projection().is_none() {
                        Err("universal compositor projection is missing or stale".into())
                    } else {
                        self.window_session
                            .resize_terminal(id.0, rows, &mut self.state)
                    };
                    if let Err(error) = result {
                        self.show_error(error);
                    }
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                    self.sync_explorer_ui();
                    self.ensure_cursor_visible();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalDock { edge, size } => {
                let edge = match edge.as_str() {
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    "top" => DockEdge::Top,
                    "bottom" => DockEdge::Bottom,
                    _ => {
                        self.show_error("terminal dock edge must be left, right, top, or bottom");
                        return Ok(());
                    }
                };
                self.place_active_terminal(WindowPlacement::Docked { edge, size });
            }
            AppEffect::TerminalFloat(rect) => {
                let rect = rect.unwrap_or_else(|| {
                    let (columns, rows) =
                        (self.window_session.width(), self.window_session.height());
                    let (width, height) = (
                        (u32::from(columns) * 4 / 5) as u16,
                        (u32::from(rows) * 4 / 5) as u16,
                    );
                    WindowRect {
                        x: (columns - width) / 2,
                        y: (rows - height) / 2,
                        width,
                        height,
                    }
                });
                self.place_active_terminal(WindowPlacement::Floating(rect));
            }
            AppEffect::TerminalFullscreen => {
                self.place_active_terminal(WindowPlacement::Fullscreen)
            }
            AppEffect::TerminalHide => self.place_active_terminal(WindowPlacement::Hidden),
            AppEffect::TerminalUnfocus => {
                self.set_terminal_focus(false);
                self.sync_terminal_panel();
                self.ensure_cursor_visible();
            }
            AppEffect::TerminalSend(text) => {
                if let Some(id) = self.terminal_session.selected()
                    && let Err(error) = self.terminal_session.send(
                        id,
                        match text.as_str() {
                            "\x1b[A" => TerminalInput::Key(crate::ui::PtyInputKey::Up),
                            "\x1b[B" => TerminalInput::Key(crate::ui::PtyInputKey::Down),
                            "\x1b[C" => TerminalInput::Key(crate::ui::PtyInputKey::Right),
                            "\x1b[D" => TerminalInput::Key(crate::ui::PtyInputKey::Left),
                            "\x1b[H" => TerminalInput::Key(crate::ui::PtyInputKey::Home),
                            "\x1b[F" => TerminalInput::Key(crate::ui::PtyInputKey::End),
                            "\x1b[5~" => TerminalInput::Key(crate::ui::PtyInputKey::PageUp),
                            "\x1b[6~" => TerminalInput::Key(crate::ui::PtyInputKey::PageDown),
                            _ => TerminalInput::Text(&text),
                        },
                        &self.workspace,
                    )
                {
                    self.show_error(error);
                }
            }
            AppEffect::TerminalModeNormal => self.enter_terminal_normal_mode(),
            AppEffect::TerminalModeInput => self.enter_terminal_input_mode(),
            AppEffect::TerminalNormalMove(motion) => {
                self.move_terminal_normal_cursor(&motion);
            }
            AppEffect::TerminalVisualStart(linewise) => {
                self.start_terminal_visual(linewise);
            }
            AppEffect::TerminalYank { line } => self.yank_terminal_text(line),
            AppEffect::TerminalClose => {
                if let Some(id) = self.displayed_terminal() {
                    self.close_terminal(id);
                } else {
                    self.terminal_session.cancel_open();
                }
            }
            AppEffect::TerminalQuit => {
                let Some(id) = self.displayed_terminal() else {
                    return Ok(());
                };
                if self.terminal_session.stopping(id) {
                    return Ok(());
                }
                if let Err(error) = self
                    .window_session
                    .require_native_stop(NativeSurfaceTarget::Terminal(id.0))
                {
                    self.show_error(error);
                    return Ok(());
                }
                self.terminal_session.cancel_open();
                // The neighbouring tab takes over while this one stops.
                self.terminal_session.select(id);
                self.terminal_session.cycle(-1);
                match self.terminal_session.selected().filter(|next| *next != id) {
                    Some(next) => self.reveal_terminal(next),
                    None => self.set_terminal_focus(false),
                }
                self.stop_or_remove_terminal(id, true);
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::terminal::commands::*;

    #[test]
    fn partial_float_rectangles_are_rejected() {
        let mut registry = CommandRegistry::default();
        register_commands(&mut registry).unwrap();
        let state = crate::editor::EditorState::new("/workspace".into());
        let invocation = crate::editor::CommandInvocation::new(
            "terminal.float",
            crate::editor::InvocationSource::Test,
        )
        .unwrap()
        .with_argument("height", crate::editor::CommandValue::Integer(10));
        assert!(registry.execute(&state, &invocation).is_err());
    }
}
