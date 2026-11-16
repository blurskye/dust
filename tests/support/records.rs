//! Finding list records in a rendered frame. A record reads like GitHub's: the title on one
//! row and `#13 opened 3h ago by maya` on the row beneath it, or `#13 Title` on one row when
//! the list is too short for two. Include with `#[path = "support/records.rs"] mod records;`.
#![allow(dead_code)]

/// Whether `line` names `#number` (and not `#number0`).
pub fn names(line: &str, number: u64) -> bool {
    let sigil = format!("#{number}");
    line.match_indices(&sigil).any(|(at, _)| {
        !line[at + sigil.len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit())
    })
}

/// The indices of the lines in `frame` where record `#number` titled `title` starts.
pub fn record_rows(frame: &str, number: u64, title: &str) -> Vec<usize> {
    let lines = frame.lines().collect::<Vec<_>>();
    let one_row = format!("#{number} {title}");
    (0..lines.len())
        .filter(|&y| {
            let line = lines[y];
            line.contains(&one_row)
                || (line.contains(title)
                    && lines.get(y + 1).is_some_and(|below| names(below, number)))
        })
        .collect()
}

/// Whether `frame` lists record `#number` titled `title`.
pub fn lists(frame: &str, number: u64, title: &str) -> bool {
    !record_rows(frame, number, title).is_empty()
}
