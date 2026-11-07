//! Debug tests for editor surfaces and frame composition.

#[test]
fn the_debug_arrangement_is_coordinated_and_the_source_is_never_squeezed_out() {
    use crate::ui::render::debug::DEBUG_SOURCE_MIN_WIDTH;
    use crate::ui::render::debug::DebugLayoutMode;
    use crate::ui::render::debug::DebugPane;
    use crate::ui::render::debug::DebugPanesUiState;
    use crate::ui::render::debug::debug_layout;
    use crate::ui::render::debug::render_debug_panes;
    let panes = DebugPanesUiState {
        selected: DebugPane::Watches,
        target: "owned: attached to processId 4242".into(),
        status: String::new(),
        variables: Vec::new(),
        threads: vec!["main (1)".into()],
        stack: vec!["main:1".into()],
        breakpoints: vec!["src/main.rs:3".into()],
        watches: vec!["count".into()],
        console: vec!["count = 1".into()],
    };

    // WIDE: inspectors on BOTH sides, console BELOW, and the source dominant in the centre.
    let layout = debug_layout(120, 40).expect("a wide terminal has room for the arrangement");
    assert_eq!(layout.mode, DebugLayoutMode::Wide);
    let left = layout
        .left
        .expect("sessions/threads/stack/breakpoints on one side");
    let right = layout.right.expect("variables/watches on the other");
    let console = layout.console.expect("the console below");
    assert_eq!(left.x, 0, "the left inspector starts at the left edge");
    assert_eq!(
        right.x,
        layout.source.x + layout.source.width,
        "the right inspector follows the source"
    );
    assert_eq!(
        console.y,
        layout.source.y + layout.source.height,
        "the console is BELOW the source"
    );
    assert_eq!(
        (console.x, console.width),
        (0, 120),
        "the console spans the terminal"
    );
    assert!(
        layout.source.width > left.width && layout.source.width > right.width,
        "the source is the DOMINANT region: {} vs {} and {}",
        layout.source.width,
        left.width,
        right.width
    );
    assert!(
        layout.source.width >= DEBUG_SOURCE_MIN_WIDTH,
        "the source keeps the readable minimum"
    );

    // And the panels are DRAWN in the regions the arrangement reserved, not over the source.
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(120, 40),
        cursor: None,
    };
    render_debug_panes(&mut frame, layout, &panes);
    assert!(
        frame.grid.row_text(0).contains("Debug"),
        "the left inspector is titled"
    );
    assert!(
        frame.grid.row_text(0).contains("Watches"),
        "the right inspector is titled"
    );
    assert!(
        frame.grid.row_text(console.y).contains("Console"),
        "the console is titled below the source"
    );
    let source_row = frame.grid.row_text(0);
    let source_cell = source_row
        .chars()
        .nth(layout.source.x as usize)
        .expect("the source region exists");
    assert_eq!(
        source_cell, ' ',
        "the source region is empty of panel content: the panels cannot cover it"
    );

    // NARROW: the side panes are REPLACED, the source keeps its width, one inspector at a time.
    let narrow = debug_layout(60, 24).expect("a narrow terminal still has a layout");
    assert_eq!(narrow.mode, DebugLayoutMode::Narrow);
    assert!(
        narrow.left.is_none() && narrow.right.is_none(),
        "panes are replaced rather than compressed in"
    );
    assert_eq!(narrow.source.width, 60, "the source keeps its FULL width");
    assert!(narrow.source.width >= DEBUG_SOURCE_MIN_WIDTH);
    let strip = narrow.console.expect("one selectable inspector remains");
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(60, 24),
        cursor: None,
    };
    render_debug_panes(&mut frame, narrow, &panes);
    // The selector may WRAP, so every row of the strip counts as the selector.
    let selector: String = (strip.y..strip.y + strip.height)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join(" ");
    for inspector in DebugPane::ALL {
        assert!(
            selector.contains(inspector.label()),
            "narrow terminals make inspectors SELECTABLE: {} is missing from {selector:?}",
            inspector.label()
        );
    }
    assert!(
        selector.contains("[Watches]"),
        "the selected inspector is marked: {selector:?}"
    );
    assert!(
        !frame.grid.row_text(0).contains("Debug"),
        "no side pane is drawn in a narrow terminal"
    );
}
