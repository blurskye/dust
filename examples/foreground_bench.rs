use std::{path::PathBuf, time::Instant};

use dust::config::load_bundled_config;
use dust::editor::EditorState;
use dust::editor::text::TextBuffer;
use dust::ui::{UiState, render};

fn main() {
    const WIDTH: u16 = 120;
    const HEIGHT: u16 = 40;
    const ITERATIONS: usize = 1_000;

    let source = (0..10_000)
        .map(|line| format!("fn measured_line_{line}() {{ println!(\"{line}\"); }}\n"))
        .collect::<String>();
    let mut state = EditorState::new(PathBuf::from("/benchmark"));
    let (_, view) = state
        .open_text(
            TextBuffer::from_text(&source),
            Some(PathBuf::from("bench.rs")),
        )
        .expect("benchmark buffer");
    let loaded = load_bundled_config();
    let config = loaded.config;
    let dashboard = loaded.dashboard;
    let ui = UiState::default();

    let startup = Instant::now();
    let first = render(&state, &ui, &config, &dashboard, WIDTH, HEIGHT);
    std::hint::black_box(first);
    let first_frame = startup.elapsed();

    let render_start = Instant::now();
    for iteration in 0..ITERATIONS {
        let line = iteration % 10_000;
        state.views[view].cursor = state.buffers[state.views[view].buffer]
            .text
            .line_to_byte(line)
            .expect("benchmark line");
        state.views[view].scroll_line = line.saturating_sub(HEIGHT as usize / 2);
        std::hint::black_box(render(&state, &ui, &config, &dashboard, WIDTH, HEIGHT));
    }
    let render_elapsed = render_start.elapsed();
    let average = render_elapsed / u32::try_from(ITERATIONS).expect("iteration count");

    println!("dust_foreground_benchmark");
    println!("target_os={}", std::env::consts::OS);
    println!("target_arch={}", std::env::consts::ARCH);
    println!(
        "profile={}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    println!("terminal={}x{}", WIDTH, HEIGHT);
    println!("buffer_lines=10000");
    println!("iterations={ITERATIONS}");
    println!("first_frame_us={}", first_frame.as_micros());
    println!("average_render_us={}", average.as_micros());
    println!("scope=state_to_cell_grid_only; excludes terminal flush and process startup");
}
