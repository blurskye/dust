//! Plugin tests for editor surfaces and frame composition.

#[test]
fn retained_plugin_surface_rows_are_strictly_clipped_to_the_projected_window() {
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(12, 5),
        cursor: None,
    };
    let surface = crate::ui::render::PluginSurfaceUiState {
        handle: 9,
        window: crate::windows::WindowId(99),
        tree_revision: 1,
        scroll: 0,
        rows: vec![
            crate::ui::render::PluginSurfaceRow {
                node: 1,
                depth: 0,
                text: "abcdefgh".into(),
                style: crate::ui::Style::Accent,
                actionable: false,
                focused: false,
            },
            crate::ui::render::PluginSurfaceRow {
                node: 2,
                depth: 1,
                text: "button".into(),
                style: crate::ui::Style::Selection,
                actionable: true,
                focused: true,
            },
        ],
    };
    crate::ui::render::render_plugin_surface(
        &mut frame,
        &surface,
        crate::windows::WindowRect {
            x: 3,
            y: 1,
            width: 4,
            height: 2,
        },
    );
    assert_eq!(&frame.grid.row_text(1)[3..7], "abcd");
    assert_eq!(frame.grid.get(7, 1).expect("outside right").symbol, " ");
    assert_eq!(frame.grid.get(2, 2).expect("outside left").symbol, " ");
    assert_eq!(frame.grid.get(3, 3).expect("outside bottom").symbol, " ");
}
