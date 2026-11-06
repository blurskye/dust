//! Sanitization of language-server Markdown.

pub fn sanitize_markdown(source: &str, maximum_chars: usize) -> String {
    let mut out = String::new();
    let mut escape = false;
    let mut csi = false;
    let mut emitted = 0;
    for character in source.chars() {
        if emitted >= maximum_chars {
            break;
        }
        if escape {
            if !csi {
                csi = character == '[';
                if !csi {
                    escape = false;
                }
            } else if ('@'..='~').contains(&character) {
                escape = false;
                csi = false;
            }
            continue;
        }
        if character == '\u{1b}' {
            escape = true;
            csi = false;
            continue;
        }
        if character.is_control() && !matches!(character, '\n' | '\t') {
            continue;
        }
        out.push(character);
        emitted += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::language::lsp::markdown::sanitize_markdown;

    #[test]
    fn markdown_limit_counts_emitted_unicode_characters_only() {
        assert_eq!(
            sanitize_markdown("\u{1b}[31mé\0\r\u{1b}[0m界\n\tx", 4),
            "é界\n\t"
        );
        assert_eq!(sanitize_markdown("anything", 0), "");
        assert_eq!(
            sanitize_markdown(&"é".repeat(65_536), 60_000)
                .chars()
                .count(),
            60_000
        );
    }
}
