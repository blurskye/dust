//! GitHub CLI can label an entire job UNKNOWN STEP. Keep those logs visible;
//! use the API's step timestamps when available, and always offer the full job.
use super::{Value, string};

pub(super) fn step_lines<'a>(raw: &'a str, step: &Value) -> (Vec<&'a str>, bool) {
    let mut exact = Vec::new();
    let mut by_time = Vec::new();
    let start = string(step, "startedAt").get(..19);
    let end = string(step, "completedAt").get(..19);
    for line in raw.lines() {
        let (name, text) = unpack(line);
        if name == string(step, "name") {
            exact.push(text);
        } else if name == "UNKNOWN STEP" || name.is_empty() {
            let stamp = text.trim_start_matches('\u{feff}').get(..19);
            // A step still running has no end yet: its lines run to the log's end.
            if let (Some(stamp), Some(start)) = (stamp, start)
                && stamp >= start
                && end.is_none_or(|end| stamp <= end)
            {
                by_time.push(text);
            }
        }
    }
    if !exact.is_empty() {
        (exact, false)
    } else {
        (by_time, true)
    }
}
pub(super) fn unpack(line: &str) -> (&str, &str) {
    let mut parts = line.splitn(3, '\t');
    let _job = parts.next();
    match (parts.next(), parts.next()) {
        (Some(name), Some(text)) => (name, text),
        _ => ("", line),
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LineKind {
    Text,
    GroupHeading,
}

pub(super) fn line_kind(line: &str) -> LineKind {
    if payload(line).starts_with("##[group]") {
        LineKind::GroupHeading
    } else {
        LineKind::Text
    }
}

pub(super) fn payload(line: &str) -> &str {
    let line = line.trim_start_matches('\u{feff}');
    line.split_once(' ')
        .filter(|(stamp, _)| {
            stamp.len() >= 20 && stamp.as_bytes().get(10) == Some(&b'T') && stamp.ends_with('Z')
        })
        .map_or(line, |(_, text)| text)
}

pub(super) fn display(line: &str) -> String {
    scan(line, |_, _| {})
        .replace("##[group]", "")
        .replace("##[endgroup]", "")
        .replace("##[error]", "error: ")
        .replace("##[warning]", "warning: ")
        .replace("##[command]", "$ ")
}

/// A colour and weight a log line asked for with SGR escapes: `color` is one of the sixteen
/// terminal colours (0–7 normal, 8–15 bright), `None` the default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Sgr {
    pub color: Option<u8>,
    pub bold: bool,
}

impl Sgr {
    fn apply(&mut self, params: &str) {
        let mut codes = params
            .split([';', ':'])
            .map(|code| code.parse::<u16>().unwrap_or(0));
        if params.is_empty() {
            *self = Self::default();
            return;
        }
        while let Some(code) = codes.next() {
            match code {
                0 => *self = Self::default(),
                1 => self.bold = true,
                22 => self.bold = false,
                30..=37 => self.color = Some((code - 30) as u8),
                90..=97 => self.color = Some((code - 90) as u8 + 8),
                39 => self.color = None,
                38 => match codes.next() {
                    Some(5) => {
                        self.color = codes.next().filter(|n| *n < 16).map(|n| n as u8);
                    }
                    Some(2) => {
                        // A true colour keeps the default role; the cells stay themed.
                        self.color = None;
                        break;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// The display text of a log line with the colours its SGR escapes asked for, as byte ranges
/// of that text (colours are rendered, never shown as `^[[36;1m`). Markers the runner
/// prints (`##[error]`, `##[command]`) read as in `display`.
pub(super) fn styled(line: &str) -> (String, Vec<(std::ops::Range<usize>, Sgr)>) {
    let line = payload(line);
    let (prefix, rest) = [
        ("##[group]", ""),
        ("##[endgroup]", ""),
        ("##[error]", "error: "),
        ("##[warning]", "warning: "),
        ("##[command]", "$ "),
    ]
    .iter()
    .find_map(|(marker, shown)| line.strip_prefix(marker).map(|rest| (*shown, rest)))
    .unwrap_or(("", line));
    let mut state = Sgr::default();
    let mut marks: Vec<(usize, Sgr)> = vec![(0, state)];
    let body = scan(rest, |params, at| {
        state.apply(params);
        marks.push((at + prefix.len(), state));
    });
    let text = format!("{prefix}{body}");
    if text.contains("##[") {
        return (display(line), Vec::new());
    }
    let mut ranges = Vec::new();
    for (index, (start, sgr)) in marks.iter().enumerate() {
        let end = marks.get(index + 1).map_or(text.len(), |(next, _)| *next);
        if *start < end && *sgr != Sgr::default() {
            ranges.push((*start..end, *sgr));
        }
    }
    (text, ranges)
}

/// The printable text of a log line: complete SGR escapes (raw or gh's caret form) are passed
/// to `sgr` with the byte offset they apply from; every other control is dropped.
fn scan(line: &str, mut sgr: impl FnMut(&str, usize)) -> String {
    let mut chars = payload(line).chars().peekable();
    let mut text = String::new();
    'display: while let Some(c) = chars.next() {
        // gh renders ESC as printable ^[ before returning job logs. Consume only
        // complete caret-SGR tokens; never decode caret text into live controls.
        if c == '^' {
            let mut probe = chars.clone();
            if probe.next() == Some('[') && probe.next() == Some('[') {
                let mut params = String::new();
                while let Some(c) = probe.next() {
                    match c {
                        '0'..='9' | ';' | ':' => params.push(c),
                        'm' => {
                            chars = probe;
                            sgr(&params, text.len());
                            continue 'display;
                        }
                        _ => break,
                    }
                }
            }
        }
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            if c == 'm'
                                && params
                                    .chars()
                                    .all(|c| c.is_ascii_digit() || c == ';' || c == ':')
                            {
                                sgr(&params, text.len());
                            }
                            break;
                        }
                        params.push(c);
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\t' {
            text.push_str("    ");
        } else if !c.is_control() {
            text.push(c);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{LineKind, Sgr, display, line_kind, step_lines, styled, unpack};

    #[test]
    fn sgr_colours_become_ranges_of_the_display_text() {
        let raw =
            "2026-11-14T05:00:04.0000000Z test retries_uploads ... \x1b[31mFAILED\x1b[0m done";
        let (text, ranges) = styled(raw);
        assert_eq!(text, "test retries_uploads ... FAILED done");
        assert_eq!(text, display(raw));
        assert_eq!(
            ranges,
            vec![(
                25..31,
                Sgr {
                    color: Some(1),
                    bold: false
                }
            )]
        );
        let (text, ranges) = styled("^[[36;1mcargo test^[[0m ok");
        assert_eq!(text, "cargo test ok");
        assert_eq!(
            ranges,
            vec![(
                0..10,
                Sgr {
                    color: Some(6),
                    bold: true
                }
            )]
        );
        let (text, ranges) = styled("##[error]Process completed with exit code 101.");
        assert_eq!(text, "error: Process completed with exit code 101.");
        assert!(ranges.is_empty());
        let (text, ranges) = styled("\x1b[92mok\x1b[39m plain \x1b[38;5;3mwarn");
        assert_eq!(text, "ok plain warn");
        assert_eq!(
            ranges[0],
            (
                0..2,
                Sgr {
                    color: Some(10),
                    bold: false
                }
            )
        );
        assert_eq!(
            ranges[1],
            (
                9..13,
                Sgr {
                    color: Some(3),
                    bold: false
                }
            )
        );
        assert!(!styled("a\x1b[2Jb").0.contains('\x1b'));
    }

    #[test]
    fn public_command_caret_sgr_matches_raw_esc_normalization() {
        for (stamp, command) in [
            (
                "2026-11-11T00:02:24.4347716Z",
                "sudo -n rm -rf -- /usr/local/lib/android /usr/share/dotnet > /dev/null 2>&1 &",
            ),
            (
                "2026-11-11T00:02:24.4745186Z",
                "mkdir -p  ./dependabot-job-1694251669-1794355336",
            ),
        ] {
            let caret = format!("{stamp} ^[[36;1m{command}^[[0m");
            let raw = format!("{stamp} \x1b[36;1m{command}\x1b[0m");
            assert_eq!(display(&caret), command);
            assert_eq!(display(&raw), command);
        }
    }

    #[test]
    fn complete_caret_sgr_supports_default_and_extended_parameters() {
        for sgr in [
            "^[[m",
            "^[[0m",
            "^[[36;1m",
            "^[[38;2;10;20;30m",
            "^[[38:2::10:20:30m",
        ] {
            assert_eq!(display(&format!("left{sgr}right")), "leftright");
        }
        assert_eq!(display("^[[31m^[[1mred^[[0m"), "red");
        // gh's lossy caret encoding cannot distinguish a literal complete SGR
        // spelling from an escaped control. Only this declared syntax is removed.
        assert_eq!(display("literal ^[[31m spelling"), "literal  spelling");
    }

    #[test]
    fn ordinary_carets_brackets_and_incomplete_sgr_are_not_decoded() {
        for literal in [
            "^",
            "^^",
            "[x]",
            "^[A-Z]",
            "^[[",
            "^[hello",
            "[36;1m",
            "echo [a-z] ^ caret",
            "^[[36;1",
            "^[[12a",
            "^[[?25m",
            "^[[٣m",
            r"\x1b[31m",
            r"\u001b[31m",
            "^[]52;c;inert^G",
        ] {
            assert_eq!(display(literal), literal);
        }
        let long_incomplete = format!("^[[{}", "1".repeat(32_768));
        assert_eq!(display(&long_incomplete), long_incomplete);
    }

    #[test]
    fn payload_tabs_and_unicode_survive_the_display_projection() {
        let raw =
            "job\tUNKNOWN STEP\t2026-11-11T00:02:24.1Z ^[[36;1ma\t界 👩\u{200d}💻 e\u{301}\t^[[0m";
        let (name, payload) = unpack(raw);
        assert_eq!(name, "UNKNOWN STEP");
        assert_eq!(display(payload), "a    界 👩\u{200d}💻 e\u{301}    ");
    }

    #[test]
    fn raw_csi_and_osc_are_removed_without_emitting_controls() {
        for raw in [
            "before\x1b[2Jafter",
            "before\x1b[?25lafter",
            "before\x1b]8;;https://example.invalid\x07after",
            "before\x1b]52;c;not-a-clipboard-write\x1b\\after",
        ] {
            assert_eq!(display(raw), "beforeafter");
        }
        assert_eq!(display("\x1b]unterminated"), "");
        assert_eq!(display("\x1b[31"), "");
        assert_eq!(display("\x1b"), "");
        let controls = "a\0\x07\x08\r\x7f\u{009b}\u{009d}\u{009c}b";
        assert_eq!(display(controls), "ab");
        let normalized = display("^[[31m\x1b[2J\x1b]52;c;hidden\x07safe\t界");
        assert_eq!(normalized, "safe    界");
        assert!(!normalized.chars().any(char::is_control));
    }

    #[test]
    fn group_heading_kind_has_no_interactive_disclosure_marker() {
        let raw = "\u{feff}2026-11-11T00:02:24.1Z ##[group]Operating System";
        assert_eq!(line_kind(raw), LineKind::GroupHeading);
        assert_eq!(display(raw), "Operating System");
        assert_eq!(line_kind("##[group]Runner"), LineKind::GroupHeading);
        assert_eq!(display("##[group]Runner"), "Runner");
        assert_eq!(line_kind("##[endgroup]"), LineKind::Text);
        assert_eq!(display("##[endgroup]"), "");
        assert_eq!(line_kind("ordinary ##[group] text"), LineKind::Text);
        assert_eq!(line_kind("^[A-Z]"), LineKind::Text);
        assert_eq!(display("##[warning]careful"), "warning: careful");
        assert_eq!(display("##[error]failed"), "error: failed");
        assert_eq!(display("##[command]echo ok"), "$ echo ok");
    }

    #[test]
    fn same_second_step_slices_remain_approximate_and_overlapping() {
        let raw = concat!(
            "job\tUNKNOWN STEP\t2026-11-11T00:02:24.1Z cleanup\n",
            "job\tUNKNOWN STEP\t2026-11-11T00:02:24.2Z mkdir\n",
            "job\tUNKNOWN STEP\t2026-11-11T00:02:24.9Z dependabot\n",
            "job\tUNKNOWN STEP\t2026-11-11T00:03:33.1Z final\n",
        );
        let short = serde_json::json!({
            "name": "Create job directory",
            "startedAt": "2026-11-11T00:02:24Z",
            "completedAt": "2026-11-11T00:02:24Z",
        });
        let failed = serde_json::json!({
            "name": "Run Dependabot",
            "startedAt": "2026-11-11T00:02:24Z",
            "completedAt": "2026-11-11T00:03:33Z",
        });
        let (short_lines, short_approximate) = step_lines(raw, &short);
        let (failed_lines, failed_approximate) = step_lines(raw, &failed);
        assert!(short_approximate && failed_approximate);
        assert_eq!(short_lines.len(), 3);
        assert_eq!(failed_lines.len(), 4);
        assert_eq!(short_lines.as_slice(), &failed_lines[..3]);
    }

    #[test]
    fn named_step_priority_and_raw_payloads_remain_unchanged() {
        let raw = concat!(
            "job\tUNKNOWN STEP\t2026-11-11T00:02:24.1Z other\n",
            "job\tRun Dependabot\t2026-11-11T00:02:24.2Z ^[[36;1mnamed^[[0m\n",
        );
        let step = serde_json::json!({
            "name": "Run Dependabot",
            "startedAt": "2026-11-11T00:02:24Z",
            "completedAt": "2026-11-11T00:03:33Z",
        });
        let (lines, approximate) = step_lines(raw, &step);
        assert!(!approximate);
        assert_eq!(lines, ["2026-11-11T00:02:24.2Z ^[[36;1mnamed^[[0m"]);
        assert_eq!(display(lines[0]), "named");
        assert!(raw.contains("^[[36;1mnamed^[[0m"));
    }
}
