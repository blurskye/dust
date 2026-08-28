#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetTabstop {
    pub index: u32,
    pub start: usize,
    pub end: usize,
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
    let bytes = source.as_bytes();
    let mut index = 0usize;
    let mut text = String::new();
    let mut stops = Vec::new();
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 1 < bytes.len()
            && matches!(bytes[index + 1], b'$' | b'}' | b'\\')
        {
            text.push(bytes[index + 1] as char);
            index += 2;
            continue;
        }
        if bytes[index] != b'$' {
            let character = source[index..]
                .chars()
                .next()
                .ok_or("snippet UTF-8 boundary")?;
            text.push(character);
            index += character.len_utf8();
            continue;
        }
        index += 1;
        if index >= bytes.len() {
            text.push('$');
            break;
        }
        if bytes[index].is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1
            }
            let number = source[start..index]
                .parse::<u32>()
                .map_err(|_| "invalid tabstop")?;
            stops.push((number, text.len(), text.len()));
            continue;
        }
        if bytes[index] == b'{' {
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1
            }
            if start == index {
                return Err("snippet placeholder has no index".into());
            }
            let number = source[start..index]
                .parse::<u32>()
                .map_err(|_| "invalid placeholder")?;
            let stop_position = stops.len();
            stops.push((number, text.len(), text.len()));
            if index >= bytes.len() {
                return Err("unterminated snippet placeholder".into());
            }
            match bytes[index] {
                b'}' => index += 1,
                b':' => {
                    index += 1;
                    let start = index;
                    while index < bytes.len() && bytes[index] != b'}' {
                        index += 1
                    }
                    if index >= bytes.len() {
                        return Err("unterminated snippet default".into());
                    }
                    text.push_str(&source[start..index]);
                    index += 1
                }
                b'|' => {
                    index += 1;
                    let start = index;
                    while index + 1 < bytes.len()
                        && !(bytes[index] == b'|' && bytes[index + 1] == b'}')
                    {
                        index += 1
                    }
                    if index + 1 >= bytes.len() {
                        return Err("unterminated snippet choice".into());
                    }
                    let choice = source[start..index].split(',').next().unwrap_or_default();
                    text.push_str(choice);
                    index += 2
                }
                _ => return Err("unsupported snippet placeholder".into()),
            }
            stops[stop_position].2 = text.len();
            continue;
        }
        text.push('$');
    }
    stops.sort_by_key(|(number, start, _)| ((*number == 0), *number, *start));
    let first = stops.first().map(|(_, start, _)| *start);
    Ok(ParsedSnippet {
        text,
        first_tabstop: first,
        tabstops: stops
            .into_iter()
            .map(|(index, start, end)| SnippetTabstop { index, start, end })
            .collect(),
    })
}
