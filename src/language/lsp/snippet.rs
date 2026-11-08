//! LSP snippet parsing and tab-stop expansion.

use std::collections::HashMap;

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
        )
        .expect("snippet tabstop endpoints are ordered")
    }
}
/// A transformed copy of a tabstop, such as `${1/(.*)/${1:/upcase}/}`, which follows edits to
/// the tabstop while the snippet is active.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetTransform {
    pub index: u32,
    pub start: usize,
    pub end: usize,
    pub depth: usize,
    /// How many tabstops come before it, to order it against a tabstop at the same position.
    pub order: usize,
    rule: Transform,
    maximum_bytes: usize,
}
impl SnippetTransform {
    pub(crate) fn range(&self) -> crate::editor::text::ByteRange {
        crate::editor::text::ByteRange::new(
            crate::editor::text::ByteOffset(self.start),
            crate::editor::text::ByteOffset(self.end),
        )
        .expect("snippet transform endpoints are ordered")
    }
    /// The copy's text for the tabstop's current `value`.
    pub fn apply(&self, value: &str) -> Result<String, String> {
        self.rule.apply(value, self.maximum_bytes)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSnippet {
    pub text: String,
    pub first_tabstop: Option<usize>,
    pub tabstops: Vec<SnippetTabstop>,
    pub transforms: Vec<SnippetTransform>,
}

pub fn parse_snippet(source: &str, maximum_bytes: usize) -> Result<ParsedSnippet, String> {
    parse_snippet_with_variables(source, maximum_bytes, &HashMap::new())
}

pub fn parse_snippet_with_variables(
    source: &str,
    maximum_bytes: usize,
    variables: &HashMap<String, String>,
) -> Result<ParsedSnippet, String> {
    if source.len() > maximum_bytes {
        return Err("snippet exceeds byte limit".into());
    }
    // TextEdit performs this same normalization when the completion is inserted.
    let normalized = crate::editor::text::TextEdit::new(
        crate::editor::text::ByteRange::empty(crate::editor::text::ByteOffset(0)),
        source,
    );
    let mut parser = Parser {
        source: normalized.replacement(),
        cursor: 0,
    };
    let pieces = parser.parse(false, 0)?;
    let mut resolver = Resolver {
        definitions: HashMap::new(),
        values: HashMap::new(),
        active: Vec::new(),
        unknown_variables: HashMap::new(),
        highest_index: 0,
        variables,
        maximum_bytes,
    };
    resolver.collect(&pieces);
    let mut text = String::new();
    let mut tabstops = Vec::new();
    let mut transforms = Vec::new();
    resolver.expand(&pieces, &mut text, &mut tabstops, &mut transforms, 0)?;
    tabstops.sort_by_key(|stop| (stop.index == 0, stop.index, stop.start));
    Ok(ParsedSnippet {
        first_tabstop: tabstops.first().map(|stop| stop.start),
        text,
        tabstops,
        transforms,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Key {
    Tabstop(u32),
    Variable(String),
}
#[derive(Clone)]
enum Piece {
    Text(String),
    Value(Key, Option<Vec<Piece>>),
    Transform(Key, Transform),
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Transform {
    pattern: String,
    replacement: String,
    options: String,
}
struct Parser<'a> {
    source: &'a str,
    cursor: usize,
}
impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.cursor).copied()
    }
    fn character(&mut self) -> Result<char, String> {
        let character = self.source[self.cursor..]
            .chars()
            .next()
            .ok_or("unexpected snippet end")?;
        self.cursor += character.len_utf8();
        Ok(character)
    }
    fn parse(&mut self, nested: bool, depth: usize) -> Result<Vec<Piece>, String> {
        if depth > 32 {
            return Err("snippet nesting exceeds limit".into());
        }
        let mut pieces = Vec::new();
        let mut literal = String::new();
        while let Some(byte) = self.peek() {
            if byte == b'}' && nested {
                self.cursor += 1;
                if !literal.is_empty() {
                    pieces.push(Piece::Text(literal));
                }
                return Ok(pieces);
            }
            if byte == b'\\'
                && self
                    .source
                    .as_bytes()
                    .get(self.cursor + 1)
                    .is_some_and(|b| matches!(b, b'$' | b'}' | b'\\'))
            {
                self.cursor += 1;
                literal.push(self.character()?);
                continue;
            }
            if byte != b'$' {
                literal.push(self.character()?);
                continue;
            }
            self.cursor += 1;
            let braced = self.peek() == Some(b'{');
            if braced {
                self.cursor += 1;
            }
            let start = self.cursor;
            let numeric = self.peek().is_some_and(|b| b.is_ascii_digit());
            if numeric {
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.cursor += 1;
                }
            } else if self
                .peek()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            {
                while self
                    .peek()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    self.cursor += 1;
                }
            }
            if start == self.cursor {
                if braced {
                    return Err("snippet placeholder has no index or variable".into());
                }
                literal.push('$');
                continue;
            }
            if !literal.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut literal)));
            }
            let key = if numeric {
                Key::Tabstop(
                    self.source[start..self.cursor]
                        .parse()
                        .map_err(|_| "invalid tabstop")?,
                )
            } else {
                Key::Variable(self.source[start..self.cursor].into())
            };
            let piece = if !braced {
                Piece::Value(key, None)
            } else {
                match self.peek() {
                    Some(b'}') => {
                        self.cursor += 1;
                        Piece::Value(key, None)
                    }
                    Some(b':') => {
                        self.cursor += 1;
                        Piece::Value(key, Some(self.parse(true, depth + 1)?))
                    }
                    Some(b'|') if numeric => {
                        self.cursor += 1;
                        let choice = self.choice()?;
                        Piece::Value(key, Some(vec![Piece::Text(choice)]))
                    }
                    Some(b'/') => {
                        self.cursor += 1;
                        let pattern = self.segment(false)?;
                        let replacement = self.segment(true)?;
                        let start = self.cursor;
                        while self.peek().is_some_and(|b| b != b'}') {
                            self.cursor += 1;
                        }
                        let options = self.source[start..self.cursor].into();
                        if self.peek() != Some(b'}') {
                            return Err("unterminated snippet transform".into());
                        }
                        self.cursor += 1;
                        Piece::Transform(
                            key,
                            Transform {
                                pattern,
                                replacement,
                                options,
                            },
                        )
                    }
                    _ => return Err("invalid snippet placeholder".into()),
                }
            };
            pieces.push(piece);
        }
        if nested {
            return Err("unterminated snippet default".into());
        }
        if !literal.is_empty() {
            pieces.push(Piece::Text(literal));
        }
        Ok(pieces)
    }
    fn choice(&mut self) -> Result<String, String> {
        let mut choice = String::new();
        let mut first = true;
        loop {
            match self.peek() {
                Some(b'|') if self.source.as_bytes().get(self.cursor + 1) == Some(&b'}') => {
                    self.cursor += 2;
                    return Ok(choice);
                }
                Some(b',') => {
                    first = false;
                    self.cursor += 1;
                }
                Some(_) => {
                    if self.peek() == Some(b'\\')
                        && self
                            .source
                            .as_bytes()
                            .get(self.cursor + 1)
                            .is_some_and(|b| matches!(b, b',' | b'|' | b'\\'))
                    {
                        self.cursor += 1;
                    }
                    let character = self.character()?;
                    if first {
                        choice.push(character);
                    }
                }
                None => return Err("unterminated snippet choice".into()),
            }
        }
    }
    fn segment(&mut self, format: bool) -> Result<String, String> {
        let mut value = String::new();
        let mut braces = 0usize;
        while let Some(byte) = self.peek() {
            if byte == b'/' && braces == 0 {
                self.cursor += 1;
                return Ok(value);
            }
            if byte == b'\\' {
                self.cursor += 1;
                let next = self.character()?;
                if next != '/' {
                    value.push('\\');
                }
                value.push(next);
                continue;
            }
            if format {
                if byte == b'$' && self.source.as_bytes().get(self.cursor + 1) == Some(&b'{') {
                    braces += 1;
                } else if byte == b'}' {
                    braces = braces.saturating_sub(1);
                }
            }
            value.push(self.character()?);
        }
        Err("unterminated snippet transform".into())
    }
}

struct Resolver<'a> {
    definitions: HashMap<u32, &'a [Piece]>,
    values: HashMap<u32, String>,
    active: Vec<u32>,
    unknown_variables: HashMap<String, u32>,
    highest_index: u32,
    variables: &'a HashMap<String, String>,
    maximum_bytes: usize,
}
impl<'a> Resolver<'a> {
    fn collect(&mut self, pieces: &'a [Piece]) {
        for piece in pieces {
            if let Piece::Value(Key::Tabstop(index), _) | Piece::Transform(Key::Tabstop(index), _) =
                piece
            {
                self.highest_index = self.highest_index.max(*index);
            }
            if let Piece::Value(key, Some(default)) = piece {
                if let Key::Tabstop(index) = key {
                    self.definitions.entry(*index).or_insert(default);
                }
                self.collect(default);
            }
        }
    }
    fn value(&mut self, key: &Key, depth: usize) -> Result<String, String> {
        match key {
            Key::Variable(name) => Ok(self.variables.get(name).cloned().unwrap_or_default()),
            Key::Tabstop(index) => {
                if let Some(value) = self.values.get(index) {
                    return Ok(value.clone());
                }
                if self.active.contains(index) {
                    return Err("cyclic snippet placeholders".into());
                }
                self.active.push(*index);
                let mut value = String::new();
                if let Some(default) = self.definitions.get(index).copied() {
                    self.expand(
                        default,
                        &mut value,
                        &mut Vec::new(),
                        &mut Vec::new(),
                        depth + 1,
                    )?;
                }
                self.active.pop();
                self.values.insert(*index, value.clone());
                Ok(value)
            }
        }
    }
    fn expand(
        &mut self,
        pieces: &[Piece],
        text: &mut String,
        stops: &mut Vec<SnippetTabstop>,
        transforms: &mut Vec<SnippetTransform>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("snippet nesting exceeds limit".into());
        }
        for piece in pieces {
            match piece {
                Piece::Text(literal) => text.push_str(literal),
                Piece::Transform(key, transform) => {
                    let value = self.value(key, depth)?;
                    let start = text.len();
                    text.push_str(&transform.apply(&value, self.maximum_bytes)?);
                    if let Key::Tabstop(index) = key {
                        transforms.push(SnippetTransform {
                            index: *index,
                            start,
                            end: text.len(),
                            depth,
                            order: stops.len(),
                            rule: transform.clone(),
                            maximum_bytes: self.maximum_bytes,
                        });
                    }
                }
                Piece::Value(key, default) => {
                    let start = text.len();
                    match key {
                        Key::Tabstop(index) => {
                            let stop = stops.len();
                            stops.push(SnippetTabstop {
                                index: *index,
                                start,
                                end: start,
                                depth,
                                order: stop,
                            });
                            if let Some(default) = default {
                                self.expand(default, text, stops, transforms, depth + 1)?;
                            } else {
                                text.push_str(&self.value(key, depth)?);
                            }
                            stops[stop].end = text.len();
                        }
                        Key::Variable(name) => {
                            if let Some(value) = self.variables.get(name) {
                                text.push_str(value);
                            } else if let Some(default) = default {
                                self.expand(default, text, stops, transforms, depth + 1)?;
                            } else {
                                // Unknown variables become editable placeholders, as in TextMate.
                                text.push_str(name);
                                let index = if let Some(index) = self.unknown_variables.get(name) {
                                    *index
                                } else {
                                    self.highest_index = self
                                        .highest_index
                                        .checked_add(1)
                                        .ok_or("too many snippet tabstops")?;
                                    self.unknown_variables
                                        .insert(name.clone(), self.highest_index);
                                    self.highest_index
                                };
                                stops.push(SnippetTabstop {
                                    index,
                                    start,
                                    end: text.len(),
                                    depth,
                                    order: stops.len(),
                                });
                            }
                        }
                    }
                }
            }
            if text.len() > self.maximum_bytes {
                return Err("expanded snippet exceeds byte limit".into());
            }
        }
        Ok(())
    }
}

impl Transform {
    fn apply(&self, value: &str, maximum_bytes: usize) -> Result<String, String> {
        // Snippet transforms use JavaScript regex syntax, which has lookaround and
        // backreferences.
        let mut builder = fancy_regex::RegexBuilder::new(&self.pattern);
        for option in self.options.chars() {
            match option {
                'g' | 'u' => {}
                'i' => {
                    builder.case_insensitive(true);
                }
                'm' => {
                    builder.multi_line(true);
                }
                's' => {
                    builder.dot_matches_new_line(true);
                }
                _ => return Err("unsupported snippet regex option".into()),
            }
        }
        // A pattern it cannot compile or run leaves the value as it is instead of refusing the
        // whole snippet.
        let Ok(regex) = builder.build() else {
            return Ok(value.to_owned());
        };
        let mut output = String::new();
        let mut end = 0;
        for captures in regex.captures_iter(value) {
            let Ok(captures) = captures else {
                return Ok(value.to_owned());
            };
            let matched = captures.get(0).expect("whole match");
            output.push_str(&value[end..matched.start()]);
            output.push_str(&format_capture(
                &self.replacement,
                &captures,
                maximum_bytes,
            )?);
            if output.len() > maximum_bytes {
                return Err("expanded snippet exceeds byte limit".into());
            }
            end = matched.end();
            if !self.options.contains('g') {
                break;
            }
        }
        output.push_str(&value[end..]);
        if output.len() > maximum_bytes {
            return Err("expanded snippet exceeds byte limit".into());
        }
        Ok(output)
    }
}

fn format_capture(
    format: &str,
    captures: &fancy_regex::Captures<'_, str>,
    maximum_bytes: usize,
) -> Result<String, String> {
    let mut parser = Parser {
        source: format,
        cursor: 0,
    };
    let mut output = String::new();
    while let Some(byte) = parser.peek() {
        if byte == b'\\' {
            parser.cursor += 1;
            output.push(parser.character()?);
            continue;
        }
        if byte != b'$' {
            output.push(parser.character()?);
            continue;
        }
        parser.cursor += 1;
        let braced = parser.peek() == Some(b'{');
        if braced {
            parser.cursor += 1;
        }
        let start = parser.cursor;
        while parser.peek().is_some_and(|b| b.is_ascii_digit()) {
            parser.cursor += 1;
        }
        if start == parser.cursor {
            output.push('$');
            continue;
        }
        let index: usize = format[start..parser.cursor]
            .parse()
            .map_err(|_| "invalid transform capture")?;
        let value = captures.get(index).map_or("", |matched| matched.as_str());
        if braced && parser.peek() == Some(b':') {
            parser.cursor += 1;
            let start = parser.cursor;
            while parser.peek().is_some_and(|b| b != b'}') {
                parser.character()?;
            }
            let modifier = &format[start..parser.cursor];
            let replacement = if let Some(case) = modifier.strip_prefix('/') {
                change_case(value, case)?
            } else if let Some(yes) = modifier.strip_prefix('+') {
                if value.is_empty() {
                    String::new()
                } else {
                    yes.into()
                }
            } else if let Some(condition) = modifier.strip_prefix('?') {
                let (yes, no) = condition
                    .split_once(':')
                    .ok_or("invalid conditional snippet format")?;
                if value.is_empty() {
                    no.into()
                } else {
                    yes.into()
                }
            } else {
                let no = modifier.strip_prefix('-').unwrap_or(modifier);
                if value.is_empty() {
                    no.into()
                } else {
                    value.into()
                }
            };
            output.push_str(&replacement);
        } else {
            output.push_str(value);
        }
        if braced {
            if parser.peek() != Some(b'}') {
                return Err("unterminated transform format".into());
            }
            parser.cursor += 1;
        }
        if output.len() > maximum_bytes {
            return Err("expanded snippet exceeds byte limit".into());
        }
    }
    Ok(output)
}

fn change_case(value: &str, case: &str) -> Result<String, String> {
    fn capitalize(value: &str) -> String {
        let mut chars = value.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
    }
    Ok(match case {
        "upcase" => value.to_uppercase(),
        "downcase" => value.to_lowercase(),
        "capitalize" => capitalize(value),
        "camelcase" | "pascalcase" => {
            let mut words = value
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty());
            let first = words.next().unwrap_or("");
            let mut result = if case == "camelcase" {
                first.to_lowercase()
            } else {
                capitalize(first)
            };
            for word in words {
                result.push_str(&capitalize(&word.to_lowercase()));
            }
            result
        }
        _ => return Err("unsupported snippet case modifier".into()),
    })
}

#[cfg(test)]
mod tests {
    use crate::language::lsp::snippet::*;
    #[test]
    fn snippet_tabstops_use_normalized_line_endings() {
        let parsed = parse_snippet("a\r\n${1:b}$0", 1024).unwrap();
        assert_eq!(parsed.text, "a\nb");
        assert_eq!((parsed.tabstops[0].start, parsed.tabstops[0].end), (2, 3));
        assert_eq!((parsed.tabstops[1].start, parsed.tabstops[1].end), (3, 3));
        let parsed = parse_snippet("${1:a\rb} $1$0", 1024).unwrap();
        assert_eq!(parsed.text, "a\nb a\nb");
        assert_eq!((parsed.tabstops[1].start, parsed.tabstops[1].end), (4, 7));
    }

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
    #[test]
    fn transitive_forward_references_resolve_before_emitting_ranges() {
        let parsed = parse_snippet("$1 ${1:$2} ${2:$3} ${3:name}", 1024).unwrap();
        assert_eq!(parsed.text, "name name name name");
        for stop in parsed.tabstops {
            assert_eq!(&parsed.text[stop.start..stop.end], "name");
        }
        assert!(parse_snippet("${1:$2}${2:$1}", 1024).is_err());
        assert!(parse_snippet("${1:$1}", 1024).is_err());
        assert!(parse_snippet("$1$1${1:abcd}", 10).is_err());
    }
    #[test]
    fn nested_defaults_expand_without_repeatedly_resolving_their_children() {
        let mut source = "value".to_owned();
        for index in (1..=25).rev() {
            source = format!("${{{index}:{source}}}");
        }
        let parsed = parse_snippet(&source, 4096).unwrap();
        assert_eq!(parsed.text, "value");
        assert_eq!(parsed.tabstops.len(), 25);
    }
    #[test]
    fn transform_replacements_obey_the_expansion_limit() {
        let source = format!(
            "${{1:{}}} ${{1/(.*)/{}/}}",
            "x".repeat(1024),
            "$1".repeat(1024)
        );
        assert!(parse_snippet(&source, 4096).is_err());
    }
    #[test]
    fn transformed_tabstop_copies_report_their_ranges() {
        let parsed = parse_snippet(
            "${1:ab} ${1/(.*)/${1:/upcase}/} ${1/a(?=b)/x/} ${1/(?<=a)b/X/} ${1/(/x/}",
            1024,
        )
        .unwrap();
        assert_eq!(
            parsed.text, "ab AB xb aX ab",
            "lookaround works, and an invalid pattern keeps the value"
        );
        assert_eq!(
            parsed
                .transforms
                .iter()
                .map(|transform| (transform.index, transform.start, transform.end))
                .collect::<Vec<_>>(),
            vec![(1, 3, 5), (1, 6, 8), (1, 9, 11), (1, 12, 14)]
        );
        assert_eq!(parsed.transforms[0].apply("cd").unwrap(), "CD");
    }
    #[test]
    fn tabstop_ranges_use_normalized_newlines() {
        let parsed = parse_snippet("${1:a}\r\n${2:b}cd\r$0", 1024).unwrap();
        assert_eq!(parsed.text, "a\nbcd\n");
        assert_eq!((parsed.tabstops[1].start, parsed.tabstops[1].end), (2, 3));
        assert_eq!(parsed.tabstops[2].start, 6);
    }
    #[test]
    fn variables_defaults_choices_and_transforms_follow_snippet_grammar() {
        let variables = HashMap::from([("TM_FILENAME".into(), "hello_world.rs".into())]);
        let source = "${TM_FILENAME} $TM_FILENAME ${MISSING:untitled} $UNKNOWN ${1|one\\,two,three|} ${1/(.*)/${1:/upcase}/} ${TM_FILENAME/(.*)\\..+$/${1:/pascalcase}/}";
        let parsed = parse_snippet_with_variables(source, 4096, &variables).unwrap();
        assert_eq!(
            parsed.text,
            "hello_world.rs hello_world.rs untitled UNKNOWN one,two ONE,TWO HelloWorld"
        );
        assert_eq!(
            parse_snippet("${TM_FILENAME:untitled}", 1024).unwrap().text,
            "untitled"
        );
        let parsed = parse_snippet(
            "${1:abc123} ${1/([a-z]+)|(\\d+)/${1:+letters}${2:?digits:none}/g}",
            1024,
        )
        .unwrap();
        assert_eq!(parsed.text, "abc123 lettersnonedigits");
        assert_eq!(
            parse_snippet("${1:AbC} ${1/a/x/i}", 1024).unwrap().text,
            "AbC xbC"
        );
        assert_eq!(
            parse_snippet("${1:a/b} ${1/a\\/b/c/}", 1024).unwrap().text,
            "a/b c"
        );
        assert_eq!(
            parse_snippet("${1:foo} ${1/z/x/}", 1024).unwrap().text,
            "foo foo"
        );
        assert_eq!(
            parse_snippet("${1:foo} ${1/(foo)?/${1:-absent}/}", 1024)
                .unwrap()
                .text,
            "foo foo"
        );
        assert_eq!(
            parse_snippet("${1:bar} ${1/(foo)?/${1:-absent}/}", 1024)
                .unwrap()
                .text,
            "bar absentbar"
        );
        let variables = HashMap::from([("TM_SELECTED_TEXT".into(), String::new())]);
        assert_eq!(
            parse_snippet_with_variables("${TM_SELECTED_TEXT:body}", 1024, &variables)
                .unwrap()
                .text,
            ""
        );
        assert_eq!(
            parse_snippet_with_variables("$TM_SELECTED_TEXT", 1024, &variables)
                .unwrap()
                .text,
            ""
        );
        assert!(parse_snippet("${1/foo/bar", 1024).is_err());
    }
}
