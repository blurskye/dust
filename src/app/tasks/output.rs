//! Task output text and compiler source locations.

use crate::app::picker::MatcherSource;
use crate::editor::text::PositionEncoding;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

pub(super) fn plain_line(line: &str) -> String {
    static ESCAPES: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))").unwrap()
    });
    ESCAPES
        .replace_all(line, "")
        .replace('\t', "    ")
        .chars()
        .filter(|c| !c.is_control())
        .take(4096)
        .collect()
}

pub(super) fn source_location(line: &str, cwd: &Path) -> Option<MatcherSource> {
    static PYTHON: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"^\s*File "(?P<path>[^"]+)", line (?P<line>[0-9]+)"#).unwrap()
    });
    static TSC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?P<path>.+?)\((?P<line>[0-9]+),(?P<column>[0-9]+)\):?\s").unwrap()
    });
    static COLON: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^\s*(?:-->|:::)?\s*(?P<path>.+?):(?P<line>[0-9]+)(?::(?P<column>[0-9]+))?(?:[:\s]|$)",
        )
        .unwrap()
    });
    let (captures, encoding) = if let Some(captures) = PYTHON.captures(line) {
        (captures, PositionEncoding::Utf8)
    } else if let Some(captures) = TSC.captures(line) {
        (captures, PositionEncoding::Utf16)
    } else {
        (COLON.captures(line)?, PositionEncoding::Utf8)
    };
    let path = captures.name("path")?.as_str().trim();
    // Python's pseudo-files (stdin, frozen modules) are not source files.
    if path.is_empty() || path.starts_with('<') || path.contains("://") {
        return None;
    }
    let line = captures
        .name("line")?
        .as_str()
        .parse::<usize>()
        .ok()?
        .checked_sub(1)?;
    let column = captures.name("column").map_or(Some(0), |column| {
        column.as_str().parse::<usize>().ok()?.checked_sub(1)
    })?;
    Some(MatcherSource {
        path: cwd.join(path),
        line,
        column,
        encoding,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_locations_use_the_tasks_directory_and_zero_based_positions() {
        for (text, path, position) in [
            ("  --> src/main.rs:12:7", "src/main.rs", (11, 6)),
            ("  ::: src/lib.rs:2:1", "src/lib.rs", (1, 0)),
            (
                "src/main.ts(4,9): error TS2322: wrong type",
                "src/main.ts",
                (3, 8),
            ),
            (
                "src/main.ts:4:9 - error TS2322: wrong type",
                "src/main.ts",
                (3, 8),
            ),
            ("./main.go:6:2: undefined: name", "./main.go", (5, 1)),
            ("main.go:6: missing return", "main.go", (5, 0)),
            (
                "  File \"lib/my module.py\", line 8, in run",
                "lib/my module.py",
                (7, 0),
            ),
            ("/tmp/source.c:3:1: error", "/tmp/source.c", (2, 0)),
        ] {
            let source = source_location(text, Path::new("/workspace/subdir")).unwrap();
            assert_eq!(
                source.path,
                Path::new("/workspace/subdir").join(path),
                "{text}"
            );
            assert_eq!((source.line, source.column), position, "{text}");
        }
        for text in [
            "normal output",
            "file.rs:0:1",
            "file.rs:1:0",
            "File \"<stdin>\", line 2",
            "https://host:42",
        ] {
            assert!(
                source_location(text, Path::new("/workspace")).is_none(),
                "{text}"
            );
        }
    }

    #[test]
    fn colored_output_is_readable_and_jumpable() {
        let line = plain_line("\x1b[31m  -->\x1b[0m src/main.rs:3:2\twrong\x1b]0;title\x07");
        assert_eq!(line, "  --> src/main.rs:3:2    wrong");
        let source = source_location(&line, Path::new("/workspace")).unwrap();
        assert_eq!(source.path, Path::new("/workspace/src/main.rs"));
        assert_eq!((source.line, source.column), (2, 1));
    }
}
