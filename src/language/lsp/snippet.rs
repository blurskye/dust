#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetTabstop {
    pub index: u32,
    pub start: usize,
    pub end: usize,
    pub depth: usize,
    pub order: usize,
}
impl SnippetTabstop {
    pub(crate) fn range(&self) -> crate::editor::text::ByteRange {
        crate::editor::text::ByteRange::new(
            crate::editor::text::ByteOffset(self.start),
            crate::editor::text::ByteOffset(self.end),
        ).expect("snippet tabstop endpoints are ordered")
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSnippet {
    pub text: String,
    pub first_tabstop: Option<usize>,
    pub tabstops: Vec<SnippetTabstop>,
}
pub fn parse_snippet(source: &str, maximum_bytes: usize) -> Result<ParsedSnippet, String> {
    if source.len() > maximum_bytes {
        return Err("snippet exceeds byte ceiling".into());
    }
    let mut values = std::collections::HashMap::new();
    expand(
        source,
        &mut 0,
        false,
        0,
        &mut values,
        &mut Vec::new(),
        &mut String::new(),
        maximum_bytes,
    )?;
    let mut text = String::new();
    let mut tabstops = Vec::new();
    expand(
        source,
        &mut 0,
        false,
        0,
        &mut values,
        &mut tabstops,
        &mut text,
        maximum_bytes,
    )?;
    tabstops.sort_by_key(|stop| (stop.index == 0, stop.index, stop.start));
    Ok(ParsedSnippet {
        first_tabstop: tabstops.first().map(|stop| stop.start),
        text,
        tabstops,
    })
}

#[allow(clippy::too_many_arguments)]
fn expand(
    source: &str,
    cursor: &mut usize,
    nested: bool,
    depth: usize,
    values: &mut std::collections::HashMap<u32, String>,
    stops: &mut Vec<SnippetTabstop>,
    text: &mut String,
    maximum_bytes: usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err("snippet nesting exceeds limit".into());
    }
    let bytes = source.as_bytes();
    while *cursor < bytes.len() {
        if text.len() > maximum_bytes {
            return Err("expanded snippet exceeds byte ceiling".into());
        }
        let byte = bytes[*cursor];
        if byte == b'}' && nested {
            *cursor += 1;
            return Ok(());
        }
        if byte == b'\\'
            && bytes
                .get(*cursor + 1)
                .is_some_and(|b| matches!(b, b'$' | b'}' | b'\\'))
        {
            text.push(bytes[*cursor + 1] as char);
            *cursor += 2;
            continue;
        }
        if byte != b'$' {
            let character = source[*cursor..]
                .chars()
                .next()
                .ok_or("snippet UTF-8 boundary")?;
            text.push(character);
            *cursor += character.len_utf8();
            continue;
        }
        *cursor += 1;
        let braced = bytes.get(*cursor) == Some(&b'{');
        if braced {
            *cursor += 1;
        }
        let start = *cursor;
        while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
            *cursor += 1;
        }
        if start == *cursor {
            if braced {
                return Err("snippet placeholder has no index".into());
            }
            text.push('$');
            continue;
        }
        let number = source[start..*cursor]
            .parse::<u32>()
            .map_err(|_| "invalid tabstop")?;
        let position = text.len();
        let stop = stops.len();
        stops.push(SnippetTabstop {
            index: number,
            start: position,
            end: position,
            depth,
            order: stop,
        });
        let default = braced && bytes.get(*cursor) == Some(&b':');
        if default {
            *cursor += 1;
            expand(
                source,
                cursor,
                true,
                depth + 1,
                values,
                stops,
                text,
                maximum_bytes,
            )?;
            values.insert(number, text[position..].to_owned());
        } else if braced && bytes.get(*cursor) == Some(&b'|') {
            *cursor += 1;
            let mut choice = String::new();
            let mut first = true;
            loop {
                match bytes.get(*cursor) {
                    Some(b'|') if bytes.get(*cursor + 1) == Some(&b'}') => {
                        *cursor += 2;
                        break;
                    }
                    Some(b',') => {
                        first = false;
                        *cursor += 1;
                    }
                    Some(_) => {
                        if bytes[*cursor] == b'\\' && *cursor + 1 < bytes.len() {
                            *cursor += 1;
                        }
                        let character = source[*cursor..]
                            .chars()
                            .next()
                            .ok_or("snippet choice boundary")?;
                        if first {
                            choice.push(character);
                        }
                        *cursor += character.len_utf8();
                    }
                    None => return Err("unterminated snippet choice".into()),
                }
            }
            text.push_str(&choice);
            values.insert(number, choice);
        } else {
            if braced {
                if bytes.get(*cursor) != Some(&b'}') {
                    return Err("unsupported snippet placeholder".into());
                }
                *cursor += 1;
            }
            if let Some(value) = values.get(&number) {
                text.push_str(value);
            }
        }
        stops[stop].end = text.len();
    }
    if nested {
        return Err("unterminated snippet default".into());
    }
    if text.len() > maximum_bytes {
        return Err("expanded snippet exceeds byte ceiling".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_defaults_and_mirrors_expand_with_linked_ranges() {
        let parsed = parse_snippet("call(${1:${2:value}})$0", 1024).unwrap();
        assert_eq!(parsed.text, "call(value)");
        assert_eq!((parsed.tabstops[0].start, parsed.tabstops[0].end), (5, 10));
        assert_eq!(
            (
                parsed.tabstops[1].index,
                parsed.tabstops[1].start,
                parsed.tabstops[1].end
            ),
            (2, 5, 10)
        );
        let parsed = parse_snippet("${1:name} = $1;$0", 1024).unwrap();
        assert_eq!(parsed.text, "name = name;");
        assert_eq!((parsed.tabstops[1].start, parsed.tabstops[1].end), (7, 11));
        assert_eq!(
            parse_snippet("$1 = ${1:name}", 1024).unwrap().text,
            "name = name"
        );
        assert_eq!(parse_snippet("${1:a\\}b}", 1024).unwrap().text, "a}b");
    }
}
