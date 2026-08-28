pub fn sanitize_markdown(source: &str, maximum_chars: usize) -> String {
    let mut out = String::new();
    let mut escape = false;
    let mut csi = false;
    for character in source.chars() {
        if out.chars().count() >= maximum_chars {
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
    }
    out
}
