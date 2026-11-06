//! Benchmarks bounded workspace search against disposable source files.
use std::fs;
use std::time::Instant;

use dust::files::WorkspaceSearchEvent;
use dust::files::WorkspaceSearchPolicy;
use dust::files::WorkspaceSearchWorker;
use tempfile::tempdir;

fn main() {
    let root = tempdir().expect("temporary benchmark workspace");
    for directory in 0..100 {
        let path = root.path().join(format!("module-{directory:03}"));
        fs::create_dir(&path).expect("benchmark directory");
        for file in 0..100 {
            let marker = if file == 42 { " dust-needle" } else { "" };
            fs::write(
                path.join(format!("source-{file:03}.rs")),
                format!("fn item_{directory}_{file}() {{}}{marker}\n"),
            )
            .expect("benchmark file");
        }
    }
    let policy = WorkspaceSearchPolicy {
        roots: vec![root.path().to_path_buf()],
        query: "dust-needle".into(),
        regex: false,
        smart_case: true,
        whole_word: false,
        include_globs: vec!["**/*.rs".into()],
        exclude_globs: Vec::new(),
        include_hidden: false,
        include_ignored: false,
        maximum_files: 20_000,
        maximum_results: 1_000,
        maximum_file_bytes: 1024 * 1024,
        maximum_total_bytes: 128 * 1024 * 1024,
        maximum_preview_bytes: 4_096,
        batch_size: 128,
        event_queue_capacity: 8,
    };
    let started = Instant::now();
    let mut service = WorkspaceSearchWorker::default();
    service.start(policy).expect("start search");
    let mut matches = 0;
    loop {
        match service.try_event() {
            Some(WorkspaceSearchEvent::Batch { matches: batch, .. }) => {
                matches += batch.len();
            }
            Some(WorkspaceSearchEvent::Complete { summary, .. }) => {
                println!("dust_workspace_search_benchmark");
                println!("files_considered={}", summary.files_considered);
                println!("files_searched={}", summary.files_searched);
                println!("matches={matches}");
                println!("elapsed_ms={}", started.elapsed().as_millis());
                println!("worker=bounded_async; ignored=true; glob=**/*.rs");
                break;
            }
            Some(WorkspaceSearchEvent::Failed { error, .. }) => panic!("search failed: {error}"),
            Some(WorkspaceSearchEvent::Cancelled { .. }) => panic!("search cancelled"),
            None => std::thread::yield_now(),
        }
    }
}
