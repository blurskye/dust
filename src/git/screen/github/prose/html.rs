//! The inline HTML GitHub lets through a comment, as reader text: `<br>` breaks the line,
//! `<img alt>` reads `[image: alt]`, and every other tag (`<sub>`, `<sup>`, `<a>`, `<span>`,
//! `<b>`…) is dropped with its text kept. Code spans and indented code keep every byte.
use std::sync::LazyLock;

/// A table row's `<br>`: a break inside its cell, not a new row.
pub(super) const CELL_BREAK: char = '\u{2028}';

static TAG: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^<(/?)([A-Za-z][A-Za-z0-9-]*)(\s[^<>]*)?/?>").unwrap());
static ALT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)\balt\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).unwrap()
});

/// `text` (no fenced code in it) with its HTML tags read as GitHub renders them.
pub(super) fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.starts_with("    ") || line.starts_with('\t') {
            out.push_str(line);
            continue;
        }
        let row = line.trim_start().starts_with('|');
        line_tags(&mut out, line, row);
    }
    out
}

fn line_tags(out: &mut String, line: &str, row: bool) {
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        let Some(next) = rest.find(['`', '<']) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..next]);
        at += next;
        let rest = &line[at..];
        if rest.starts_with('`') {
            // A code span runs to the next run of as many backticks; it keeps its tags.
            let run = rest.len() - rest.trim_start_matches('`').len();
            let fence = &rest[..run];
            let close = rest[run..]
                .match_indices(fence)
                .find(|(offset, _)| {
                    let after = run + offset + run;
                    !rest[after..].starts_with('`') && !rest[..run + offset].ends_with('`')
                })
                .map(|(offset, _)| run + offset + run);
            let end = close.unwrap_or(run);
            out.push_str(&rest[..end]);
            at += end;
            continue;
        }
        let Some(tag) = TAG.captures(rest) else {
            out.push('<');
            at += 1;
            continue;
        };
        let name = tag[2].to_ascii_lowercase();
        let closing = !tag[1].is_empty();
        match name.as_str() {
            "br" => out.push(if row { CELL_BREAK } else { '\n' }),
            "img" if !closing => {
                let alt = tag
                    .get(3)
                    .and_then(|attributes| ALT.captures(attributes.as_str()))
                    .and_then(|alt| alt.get(1).or_else(|| alt.get(2)).or_else(|| alt.get(3)))
                    .map(|alt| alt.as_str().trim().to_owned())
                    .filter(|alt| !alt.is_empty());
                match alt {
                    Some(alt) => out.push_str(&format!("[image: {alt}]")),
                    None => out.push_str("[image]"),
                }
            }
            _ => {}
        }
        at += tag[0].len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_tags_read_as_text_and_others_keep_their_text() {
        assert_eq!(
            inline("one<br>two<br/>three<BR />four"),
            "one\ntwo\nthree\nfour"
        );
        assert_eq!(inline("H<sub>2</sub>O and x<sup>2</sup>"), "H2O and x2");
        assert_eq!(
            inline(r#"<img src="https://x/y.png" alt="Build status"> and <img src=x.png>"#),
            "[image: Build status] and [image]"
        );
        assert_eq!(inline("<img alt='logo' src='a.png'/>"), "[image: logo]");
        assert_eq!(
            inline(r#"<a href="https://x">the docs</a>, <span class="x">kept</span>"#),
            "the docs, kept"
        );
        assert_eq!(inline("<kbd>Ctrl</kbd>+<kbd>C</kbd>"), "Ctrl+C");
    }

    #[test]
    fn code_autolinks_and_comparisons_stay() {
        assert_eq!(
            inline("`Vec<String>` and ``a <b> `c` ``"),
            "`Vec<String>` and ``a <b> `c` ``"
        );
        assert_eq!(
            inline("    let x: Vec<u8> = vec![];"),
            "    let x: Vec<u8> = vec![];"
        );
        assert_eq!(
            inline("see <https://orbit.example> if a < b"),
            "see <https://orbit.example> if a < b"
        );
        assert_eq!(
            inline("an unmatched ` backtick <b>bold</b>"),
            "an unmatched ` backtick bold"
        );
    }

    #[test]
    fn a_break_inside_a_table_row_stays_in_its_cell() {
        assert_eq!(inline("| a<br>b | c |"), format!("| a{CELL_BREAK}b | c |"));
    }
}
