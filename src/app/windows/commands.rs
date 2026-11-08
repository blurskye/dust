//! Command registration and execution for window commands, focus and application state.

use crate::app::commands::AppEffect;
use crate::app::commands::integer_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::app::windows::LayoutAction;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use crate::windows::SpatialDirection;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "window.list",
        "List all native and extension windows",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| Ok(AppEffect::WindowList),
    )?;
    register(
        registry,
        "window.clone",
        "Clone a supported window with independent view state",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("axis", ArgumentType::String, true),
        |invocation| {
            let axis = match string_arg(invocation, "axis")? {
                "horizontal" => crate::windows::TileAxis::Horizontal,
                "vertical" => crate::windows::TileAxis::Vertical,
                _ => {
                    return Err(CommandError::InvalidArguments(
                        "window clone axis must be horizontal or vertical".into(),
                    ));
                }
            };
            Ok(AppEffect::WindowClone {
                target: string_arg(invocation, "target")?.to_owned(),
                axis,
            })
        },
    )?;
    register(
        registry,
        "window.dock",
        "Dock an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("edge", ArgumentType::String, true)
            .field("size", ArgumentType::Integer, true),
        |invocation| {
            let size = u16::try_from(integer_arg(invocation, "size")?).map_err(|_| {
                CommandError::InvalidArguments("window dock size is outside u16 bounds".into())
            })?;
            Ok(AppEffect::WindowDock {
                target: string_arg(invocation, "target")?.to_owned(),
                edge: string_arg(invocation, "edge")?.to_owned(),
                size,
            })
        },
    )?;
    register(
        registry,
        "window.float",
        "Float an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("x", ArgumentType::Integer, true)
            .field("y", ArgumentType::Integer, true)
            .field("width", ArgumentType::Integer, true)
            .field("height", ArgumentType::Integer, true),
        |invocation| {
            let bounded = |name| {
                u16::try_from(integer_arg(invocation, name)?).map_err(|_| {
                    CommandError::InvalidArguments(format!(
                        "window float {name} is outside u16 bounds"
                    ))
                })
            };
            Ok(AppEffect::WindowFloat {
                target: string_arg(invocation, "target")?.to_owned(),
                x: bounded("x")?,
                y: bounded("y")?,
                width: bounded("width")?,
                height: bounded("height")?,
            })
        },
    )?;
    for (id, title, build) in [
        (
            "window.fullscreen",
            "Fullscreen an ordinary native window",
            AppEffect::WindowFullscreen as fn(String) -> AppEffect,
        ),
        (
            "window.hide",
            "Hide an ordinary native window",
            AppEffect::WindowHide,
        ),
        (
            "window.show",
            "Show an ordinary native window",
            AppEffect::WindowShow,
        ),
        (
            "window.close",
            "Close an ordinary native window",
            AppEffect::WindowClose,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default().field("target", ArgumentType::String, true),
            move |invocation| Ok(build(string_arg(invocation, "target")?.to_owned())),
        )?;
    }
    register(
        registry,
        "window.resize",
        "Resize an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("amount", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::WindowResize {
                target: string_arg(invocation, "target")?.to_owned(),
                amount: i16::try_from(integer_arg(invocation, "amount")?).map_err(|_| {
                    CommandError::InvalidArguments("window resize is outside i16 bounds".into())
                })?,
            })
        },
    )?;
    register(
        registry,
        "window.resize.directional",
        "Resize the focused window in one direction",
        ContextRequirement::Application,
        ArgumentSchema::default().field("direction", ArgumentType::String, true),
        |invocation| {
            let direction = match string_arg(invocation, "direction")? {
                "left" => SpatialDirection::Left,
                "down" => SpatialDirection::Down,
                "up" => SpatialDirection::Up,
                "right" => SpatialDirection::Right,
                _ => {
                    return Err(CommandError::InvalidArguments(
                        "window resize direction must be left, down, up, or right".into(),
                    ));
                }
            };
            Ok(AppEffect::WindowResizeDirectional(
                direction,
                invocation.count.map_or(1, |count| count.get()),
            ))
        },
    )?;
    register(
        registry,
        "window.move",
        "Move an ordinary floating native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("x", ArgumentType::Integer, true)
            .field("y", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::WindowMove {
                target: string_arg(invocation, "target")?.to_owned(),
                x: u16::try_from(integer_arg(invocation, "x")?).map_err(|_| {
                    CommandError::InvalidArguments("window x is outside u16 bounds".into())
                })?,
                y: u16::try_from(integer_arg(invocation, "y")?).map_err(|_| {
                    CommandError::InvalidArguments("window y is outside u16 bounds".into())
                })?,
            })
        },
    )?;
    register(
        registry,
        "window.swap",
        "Swap two ordinary native window placements",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("first", ArgumentType::String, true)
            .field("second", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WindowSwap {
                first: string_arg(invocation, "first")?.to_owned(),
                second: string_arg(invocation, "second")?.to_owned(),
            })
        },
    )?;
    for (id, title, effect) in [
        (
            "surface.confirmation.select_yes",
            "Answer yes",
            AppEffect::PromptConfirmationSelect(crate::ui::ConfirmationChoice::Yes),
        ),
        (
            "surface.confirmation.select_no",
            "Answer no",
            AppEffect::PromptConfirmationSelect(crate::ui::ConfirmationChoice::No),
        ),
        (
            "surface.confirmation.toggle",
            "Toggle confirmation choice",
            AppEffect::PromptConfirmationToggle,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    for (id, title, direction) in [
        (
            "surface.focus.left",
            "Focus surface to the left",
            SpatialDirection::Left,
        ),
        (
            "surface.focus.down",
            "Focus surface below",
            SpatialDirection::Down,
        ),
        (
            "surface.focus.up",
            "Focus surface above",
            SpatialDirection::Up,
        ),
        (
            "surface.focus.right",
            "Focus surface to the right",
            SpatialDirection::Right,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::SurfaceFocus(direction),
        )?;
    }
    for (id, title, effect) in [
        (
            "surface.retained.focus.next",
            "Focus next retained UI node",
            AppEffect::RetainedUiFocus(1),
        ),
        (
            "surface.retained.focus.previous",
            "Focus previous retained UI node",
            AppEffect::RetainedUiFocus(-1),
        ),
        (
            "surface.retained.scroll.down",
            "Scroll retained UI down",
            AppEffect::RetainedUiScroll(1),
        ),
        (
            "surface.retained.scroll.up",
            "Scroll retained UI up",
            AppEffect::RetainedUiScroll(-1),
        ),
        (
            "surface.retained.activate",
            "Activate focused retained UI node",
            AppEffect::RetainedUiActivate,
        ),
        (
            "surface.retained.mouse.select",
            "Select retained UI node at mouse position",
            AppEffect::RetainedUiMouseSelect,
        ),
        (
            "dashboard.select.next",
            "Move to the next dashboard item",
            AppEffect::DashboardSelect(1),
        ),
        (
            "dashboard.select.previous",
            "Move to the previous dashboard item",
            AppEffect::DashboardSelect(-1),
        ),
        (
            "dashboard.open",
            "Run the dashboard item under the cursor",
            AppEffect::DashboardOpen,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    for (id, title, at_mouse) in [
        (
            "surface.private.activate",
            "Activate private surface action at cursor",
            false,
        ),
        (
            "surface.private.mouse.activate",
            "Activate private surface action at mouse",
            true,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::PrivateUiActivate { at_mouse },
        )?;
    }
    for (id, title, action) in [
        (
            "pane.split.horizontal",
            "Split pane horizontally",
            LayoutAction::Split(crate::windows::TileAxis::Horizontal),
        ),
        (
            "pane.split.vertical",
            "Split pane vertically",
            LayoutAction::Split(crate::windows::TileAxis::Vertical),
        ),
        ("pane.close", "Close pane", LayoutAction::ClosePane),
        (
            "pane.resize.grow",
            "Grow focused pane",
            LayoutAction::ResizePane(1),
        ),
        (
            "pane.resize.shrink",
            "Shrink focused pane",
            LayoutAction::ResizePane(-1),
        ),
        (
            "pane.focus.next",
            "Focus next pane",
            LayoutAction::FocusPane(1),
        ),
        (
            "pane.focus.previous",
            "Focus previous pane",
            LayoutAction::FocusPane(-1),
        ),
        ("tab.new", "Open new tab", LayoutAction::NewTab),
        ("tab.close", "Close tab", LayoutAction::CloseTab),
        ("pane.zoom", "Zoom the pane", LayoutAction::Zoom),
        ("tab.next", "Focus next tab", LayoutAction::FocusTab(1)),
        (
            "tab.previous",
            "Focus previous tab",
            LayoutAction::FocusTab(-1),
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::View,
            ArgumentSchema::default(),
            move |invocation| {
                let count = invocation.count.map_or(1, |count| count.get());
                let steps = isize::try_from(count).unwrap_or(isize::MAX);
                let action = match action {
                    LayoutAction::ResizePane(direction) => {
                        LayoutAction::ResizePane(direction * i8::try_from(count).unwrap_or(i8::MAX))
                    }
                    LayoutAction::FocusPane(direction) => {
                        LayoutAction::FocusPane(direction * steps)
                    }
                    LayoutAction::FocusTab(1) if invocation.count.is_some() => {
                        LayoutAction::FocusTabNumber(usize::try_from(count).unwrap_or(usize::MAX))
                    }
                    LayoutAction::FocusTab(direction) => LayoutAction::FocusTab(direction * steps),
                    action => action,
                };
                AppEffect::Layout(action)
            },
        )?;
    }
    Ok(())
}

use crate::app::App;
use crate::app::picker::PickerPayload;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

use crate::editor::text::TextBuffer;

impl App {
    pub(in crate::app) fn process_windows_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::NewTab => {
                // An untouched [No Name] tab is reused, so ctrl-t never piles up empty tabs;
                // the file picked from the finder replaces it.
                let unused = self.state.buffers.iter().find(|(_, buffer)| {
                    buffer.path.is_none()
                        && !buffer.dirty
                        && buffer.text.len_bytes() == 0
                        && buffer.origin == crate::editor::BufferOrigin::FileOrScratch
                });
                match unused.map(|(id, _)| id) {
                    Some(buffer) => {
                        if let Err(error) = self.state.show_buffer(buffer) {
                            self.show_error(error);
                        }
                    }
                    None => {
                        if let Err(error) = self.state.open_text(TextBuffer::from_text(""), None) {
                            self.show_error(error);
                            return Ok(());
                        }
                    }
                }
                self.set_dashboard_visibility(false);
                self.start_workspace_file_picker();
            }
            AppEffect::Layout(action) => self.apply_layout_action(action),
            AppEffect::SurfaceFocus(direction) => self.focus_surface(direction),
            AppEffect::WindowClone { target, axis } => {
                if let Err(error) = self.clone_graph_window(&target, axis) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowList => {
                let graph = self.window_session.graph();
                let items = graph
                    .windows()
                    .filter_map(|window| {
                        let content = graph.content_for_window(window.id)?;
                        Some((
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("window:{}", window.id.0)),
                                label: content.title.clone(),
                                detail: format!(
                                    "{} · {:?} · {:?} · tab {}",
                                    content.content_type.as_str(),
                                    content.lifecycle,
                                    window.placement,
                                    window.tab.0
                                ),
                                source: content.owner.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Window(window.id),
                        ))
                    })
                    .collect();
                self.install_picker("Windows".into(), items);
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
                let result =
                    self.place_graph_window(&target, WindowPlacement::Docked { edge, size });
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
                let result = self.place_graph_window(
                    &target,
                    WindowPlacement::Floating(WindowRect {
                        x,
                        y,
                        width,
                        height,
                    }),
                );
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFullscreen(target) => {
                let result = self.place_graph_window(&target, WindowPlacement::Fullscreen);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowHide(target) => {
                let result = self.place_graph_window(&target, WindowPlacement::Hidden);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowShow(target) => {
                let result = self.show_graph_window(&target);
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
            AppEffect::WindowResizeDirectional(direction, count) => {
                if let Err(error) = self.resize_focused_window(direction, count) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowMove { target, x, y } => {
                let result = self.move_graph_window(&target, x, y);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowSwap { first, second } => {
                if let Err(error) = self.swap_graph_windows(&first, &second) {
                    self.show_error(error);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
