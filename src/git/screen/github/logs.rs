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
            if let (Some(stamp), Some(start), Some(end)) = (stamp, start, end)
                && stamp >= start
                && stamp <= end
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
pub(super) fn display(line: &str) -> String {
    let line = line.trim_start_matches('\u{feff}');
    let line = line
        .split_once(' ')
        .filter(|(stamp, _)| {
            stamp.len() >= 20 && stamp.as_bytes().get(10) == Some(&b'T') && stamp.ends_with('Z')
        })
        .map_or(line, |(_, text)| text);
    let mut chars = line.chars().peekable();
    let mut text = String::new();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
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
    text.replace("##[group]", "▸ ")
        .replace("##[endgroup]", "")
        .replace("##[error]", "error: ")
        .replace("##[warning]", "warning: ")
        .replace("##[command]", "$ ")
}
