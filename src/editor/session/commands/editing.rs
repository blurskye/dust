use crate::editor::session::ViewToggle;

use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    FocusDirection, LayoutAction, bool_arg, one_character_arg, optional_bool_arg, register,
    string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "input.mapping.cancel",
        "Cancel pending mapping",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CancelPendingMapping,
    )?;
    register(
        registry,
        "editor.flash",
        "Jump to a visible match by its label",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::FlashBegin,
    )?;
    register(
        registry,
        "edit.repeat_last",
        "Repeat last change",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |invocation| AppEffect::RepeatLast(invocation.count),
    )?;
    register(
        registry,
        "macro.record.start",
        "Start macro recording",
        ContextRequirement::View,
        ArgumentSchema::default().field("register", ArgumentType::String, true),
        |invocation| {
            let name = one_character_arg(invocation, "register")?;
            Ok(AppEffect::MacroRecordStart(name))
        },
    )?;
    register(
        registry,
        "macro.record.stop",
        "Stop macro recording",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::MacroRecordStop,
    )?;
    register(
        registry,
        "macro.replay",
        "Replay macro",
        ContextRequirement::View,
        ArgumentSchema::default().field("register", ArgumentType::String, true),
        |invocation| {
            let name = one_character_arg(invocation, "register")?;
            Ok(AppEffect::MacroReplay {
                register: name,
                count: invocation.count.map_or(1, |count| count.get()),
            })
        },
    )?;
    register(
        registry,
        "view.scroll",
        "Scroll view",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("kind", ArgumentType::String, true)
            .field("at_mouse", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::Scroll {
                kind: string_arg(invocation, "kind")?.into(),
                count: invocation.count.map_or(1, |count| count.get()),
                at_mouse: optional_bool_arg(invocation, "at_mouse").unwrap_or(false),
            })
        },
    )?;
    register(
        registry,
        "view.cursor.place_mouse",
        "Place cursor at mouse position",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::PlaceMouseCursor,
    )?;
    register(
        registry,
        "view.selection.extend_mouse",
        "Extend selection to mouse position",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::ExtendMouseSelection,
    )?;
    for (id, title, direction) in [
        (
            "surface.focus.left",
            "Focus surface to the left",
            FocusDirection::Left,
        ),
        (
            "surface.focus.down",
            "Focus surface below",
            FocusDirection::Down,
        ),
        (
            "surface.focus.up",
            "Focus surface above",
            FocusDirection::Up,
        ),
        (
            "surface.focus.right",
            "Focus surface to the right",
            FocusDirection::Right,
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
            LayoutAction::Split(crate::editor::Axis::Horizontal),
        ),
        (
            "pane.split.vertical",
            "Split pane vertically",
            LayoutAction::Split(crate::editor::Axis::Vertical),
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
                    LayoutAction::ResizePane(direction) => LayoutAction::ResizePane(
                        direction * i8::try_from(count).unwrap_or(i8::MAX),
                    ),
                    LayoutAction::FocusPane(direction) => LayoutAction::FocusPane(direction * steps),
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
    for (id, title, toggle) in [
        ("view.wrap.toggle", "Toggle line wrap", ViewToggle::Wrap),
        (
            "view.line_numbers.toggle",
            "Toggle line numbers",
            ViewToggle::LineNumbers,
        ),
        (
            "view.relative_numbers.toggle",
            "Toggle relative line numbers",
            ViewToggle::RelativeNumbers,
        ),
        (
            "view.indent_guides.toggle",
            "Toggle indent guides",
            ViewToggle::IndentGuides,
        ),
        (
            "view.git_signs.toggle",
            "Toggle git signs",
            ViewToggle::GitSigns,
        ),
        (
            "view.format_on_save.toggle",
            "Toggle format on save",
            ViewToggle::FormatOnSave,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::ViewToggle(toggle),
        )?;
    }
    register(
        registry,
        "view.zen.toggle",
        "Toggle zen mode",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::Zen,
    )?;
    register(
        registry,
        "search.word",
        "Search word under cursor",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("forward", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::SearchWord {
                forward: bool_arg(invocation, "forward")?,
                count: invocation.count.map_or(1, |count| count.get()),
            })
        },
    )?;
    register(
        registry,
        "edit.substitute_rest",
        "Replace every remaining match",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| Ok(AppEffect::SubstituteAll),
    )?;
    register(
        registry,
        "search.select_next",
        "Select the next search match",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |invocation| Ok(AppEffect::SearchSelectNext(invocation.count.map_or(1, |count| count.get()))),
    )?;
    register(
        registry,
        "search.repeat",
        "Repeat search",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("reverse", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::SearchRepeat {
                reverse: bool_arg(invocation, "reverse")?,
                count: invocation.count.map_or(1, |count| count.get()),
            })
        },
    )?;
    Ok(())
}
