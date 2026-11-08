//! Changed byte spans between text snapshots.

/// Only what differs between the texts, so cursors and marks elsewhere stay where they are.
/// Replaces the whole text when a line diff can't describe the change, as with binary content.
pub(crate) fn changed_spans(before: &str, after: &str) -> Vec<(std::ops::Range<usize>, String)> {
    if before == after {
        return Vec::new();
    }
    let spans = line_diff_spans(before, after).filter(|spans| {
        let mut rebuilt = String::with_capacity(after.len());
        let mut position = 0;
        for (range, text) in spans {
            rebuilt.push_str(&before[position..range.start]);
            rebuilt.push_str(text);
            position = range.end;
        }
        rebuilt.push_str(&before[position..]);
        !spans.is_empty() && rebuilt == after
    });
    spans.unwrap_or_else(|| vec![(0..before.len(), after.to_owned())])
}

fn line_diff_spans(before: &str, after: &str) -> Option<Vec<(std::ops::Range<usize>, String)>> {
    let mut options = git2::DiffOptions::new();
    options.context_lines(0);
    let patch = git2::Patch::from_buffers(
        before.as_bytes(),
        None,
        after.as_bytes(),
        None,
        Some(&mut options),
    )
    .ok()?;
    let mut line_starts = vec![0];
    line_starts.extend(before.match_indices('\n').map(|(index, _)| index + 1));
    let line_start = |line: usize| line_starts.get(line).copied().unwrap_or(before.len());
    let mut spans = Vec::new();
    for index in 0..patch.num_hunks() {
        let (hunk, lines) = patch.hunk(index).ok()?;
        let removed = usize::try_from(hunk.old_lines()).ok()?;
        // With nothing removed, the hunk's old line is the one it inserts after.
        let first = usize::try_from(hunk.old_start())
            .ok()?
            .saturating_sub(usize::from(removed > 0));
        let mut added = Vec::new();
        for line in 0..lines {
            let line = patch.line_in_hunk(index, line).ok()?;
            if line.origin() == '+' {
                added.extend_from_slice(line.content());
            }
        }
        let added = String::from_utf8(added).ok()?;
        let (start, end) = (line_start(first), line_start(first.saturating_add(removed)));
        let old = before.get(start..end)?;
        let mut prefix = old
            .bytes()
            .zip(added.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(prefix) || !added.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let mut suffix = old[prefix..]
            .bytes()
            .rev()
            .zip(added[prefix..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - suffix)
            || !added.is_char_boundary(added.len() - suffix)
        {
            suffix -= 1;
        }
        spans.push((
            start + prefix..end - suffix,
            added[prefix..added.len() - suffix].to_owned(),
        ));
    }
    Some(spans)
}
