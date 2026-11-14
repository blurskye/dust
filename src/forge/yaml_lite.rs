//! A strict reader for the small YAML files forge CLIs keep their sign-ins in (glab's
//! `config.yml`, tea's `config.yml`), FORGE.md §2.1. It understands block mappings, block
//! sequences, plain and quoted scalars, empty `[]`/`{}` and comments — the shapes those tools
//! write — and refuses everything else (anchors, aliases, tags, block scalars, flow collections,
//! tabs, multi-line scalars, duplicate keys). A refusal means "skip this store": the caller falls
//! back to the next credential source, so a format surprise makes Dusty slower, never wrong.

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Yaml {
    Null,
    Scalar(String),
    List(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Scalar(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_list(&self) -> &[Yaml] {
        match self {
            Yaml::List(items) => items,
            _ => &[],
        }
    }

    pub fn entries(&self) -> &[(String, Yaml)] {
        match self {
            Yaml::Map(entries) => entries,
            _ => &[],
        }
    }

    /// A non-empty scalar at `key`.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.get(key)?
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YamlError {
    pub line: usize,
    pub reason: &'static str,
}

impl std::fmt::Display for YamlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the line's text: it may hold a token.
        write!(formatter, "line {}: {}", self.line, self.reason)
    }
}

#[derive(Clone, Debug)]
struct Line {
    number: usize,
    indent: usize,
    text: String,
}

fn error<T>(line: usize, reason: &'static str) -> Result<T, YamlError> {
    Err(YamlError { line, reason })
}

/// Drop a comment: `#` at the start or after whitespace, outside quotes.
fn strip_comment(text: &str, number: usize) -> Result<&str, YamlError> {
    let mut quote = None;
    let mut previous = ' ';
    for (at, character) in text.char_indices() {
        match quote {
            Some('"') if character == '\\' => {}
            Some(open) if character == open && !(open == '"' && previous == '\\') => quote = None,
            Some(_) => {}
            None if character == '#' && previous.is_whitespace() => return Ok(&text[..at]),
            None if matches!(character, '"' | '\'')
                && (previous.is_whitespace() || previous == ':') =>
            {
                quote = Some(character)
            }
            None => {}
        }
        previous = if previous == '\\' && character == '\\' {
            ' '
        } else {
            character
        };
    }
    if quote.is_some() {
        return error(number, "unterminated quote");
    }
    Ok(text)
}

fn lines(source: &str) -> Result<Vec<Line>, YamlError> {
    let mut lines = Vec::new();
    let mut started = false;
    for (index, raw) in source.lines().enumerate() {
        let number = index + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        if raw[indent..].starts_with('\t') {
            return error(number, "tab indentation");
        }
        let text = strip_comment(&raw[indent..], number)?.trim_end();
        if text.is_empty() {
            continue;
        }
        if indent == 0 && (text == "---" || text.starts_with("%")) {
            if started || text.starts_with('%') {
                return error(number, "multiple documents or directives");
            }
            started = true;
            continue;
        }
        if indent == 0 && text == "..." {
            break;
        }
        started = true;
        lines.push(Line {
            number,
            indent,
            text: text.to_owned(),
        });
    }
    Ok(lines)
}

fn unquote_double(body: &str, number: usize) -> Result<String, YamlError> {
    let mut text = String::new();
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            text.push(character);
            continue;
        }
        text.push(match characters.next() {
            Some('\\') => '\\',
            Some('"') => '"',
            Some('/') => '/',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('0') => '\0',
            Some(' ') => ' ',
            _ => return error(number, "unsupported escape"),
        });
    }
    Ok(text)
}

/// A quoted scalar at the start of `text`; returns it and the rest after the closing quote.
fn quoted(text: &str, number: usize) -> Result<(String, &str), YamlError> {
    let open = text.chars().next().unwrap_or(' ');
    let body = &text[1..];
    if open == '"' {
        let mut escaped = false;
        for (at, character) in body.char_indices() {
            match character {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    return Ok((unquote_double(&body[..at], number)?, &body[at + 1..]));
                }
                _ => escaped = false,
            }
        }
    } else {
        let mut at = 0;
        let bytes = body.as_bytes();
        while at < bytes.len() {
            if bytes[at] == b'\'' {
                if bytes.get(at + 1) == Some(&b'\'') {
                    at += 2;
                    continue;
                }
                return Ok((body[..at].replace("''", "'"), &body[at + 1..]));
            }
            at += 1;
        }
    }
    error(number, "unterminated quote")
}

fn scalar(text: &str, number: usize) -> Result<Yaml, YamlError> {
    let text = text.trim();
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Ok(Yaml::Null),
        "[]" => return Ok(Yaml::List(Vec::new())),
        "{}" => return Ok(Yaml::Map(Vec::new())),
        _ => {}
    }
    if text.starts_with(['"', '\'']) {
        let (value, rest) = quoted(text, number)?;
        if !rest.trim().is_empty() {
            return error(number, "text after a quoted scalar");
        }
        return Ok(Yaml::Scalar(value));
    }
    if text.starts_with([
        '&', '*', '!', '|', '>', '[', '{', '%', '@', '`', '?', ']', '}',
    ]) {
        return error(number, "unsupported YAML feature");
    }
    if text.contains(": ") || text.ends_with(':') {
        return error(number, "mapping inside a scalar");
    }
    Ok(Yaml::Scalar(text.to_owned()))
}

/// `key: rest` → (key, rest); `None` when the line is not a mapping entry.
fn split_key(text: &str, number: usize) -> Result<Option<(String, String)>, YamlError> {
    if text.starts_with(['"', '\'']) {
        let (key, rest) = quoted(text, number)?;
        let Some(rest) = rest.strip_prefix(':') else {
            return Ok(None);
        };
        if !(rest.is_empty() || rest.starts_with(' ')) {
            return error(number, "missing space after a key");
        }
        return Ok(Some((key, rest.trim().to_owned())));
    }
    let found = text
        .find(": ")
        .or_else(|| text.ends_with(':').then(|| text.len() - 1));
    let Some(at) = found else {
        return Ok(None);
    };
    let key = text[..at].trim();
    if key.is_empty() || key.starts_with(['&', '*', '!', '?', '[', '{', '-', '|', '>']) {
        return error(number, "unsupported key");
    }
    Ok(Some((key.to_owned(), text[at + 1..].trim().to_owned())))
}

fn sequence_item(text: &str) -> Option<&str> {
    if text == "-" {
        Some("")
    } else {
        text.strip_prefix("- ")
    }
}

struct Parser {
    lines: Vec<Line>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Line> {
        self.lines.get(self.at)
    }

    fn block(&mut self, indent: usize) -> Result<Yaml, YamlError> {
        let Some(line) = self.peek() else {
            return Ok(Yaml::Null);
        };
        if sequence_item(&line.text).is_some() {
            self.sequence(indent)
        } else {
            self.mapping(indent)
        }
    }

    /// The value after `key:` with nothing on its line: a deeper block, an indentless
    /// sequence, or null.
    fn nested(&mut self, indent: usize) -> Result<Yaml, YamlError> {
        match self.peek() {
            Some(line) if line.indent > indent => {
                let child = line.indent;
                self.block(child)
            }
            Some(line) if line.indent == indent && sequence_item(&line.text).is_some() => {
                self.sequence(indent)
            }
            _ => Ok(Yaml::Null),
        }
    }

    fn mapping(&mut self, indent: usize) -> Result<Yaml, YamlError> {
        let mut entries: Vec<(String, Yaml)> = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent < indent {
                break;
            }
            let number = line.number;
            if line.indent > indent {
                return error(number, "unexpected indentation");
            }
            if sequence_item(&line.text).is_some() {
                return error(number, "sequence item inside a mapping");
            }
            let Some((key, rest)) = split_key(&line.text.clone(), number)? else {
                return error(number, "expected key: value");
            };
            self.at += 1;
            let value = if rest.is_empty() {
                self.nested(indent)?
            } else {
                scalar(&rest, number)?
            };
            if entries.iter().any(|(name, _)| *name == key) {
                return error(number, "duplicate key");
            }
            entries.push((key, value));
        }
        Ok(Yaml::Map(entries))
    }

    fn sequence(&mut self, indent: usize) -> Result<Yaml, YamlError> {
        let mut items = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent < indent {
                break;
            }
            let number = line.number;
            if line.indent > indent {
                return error(number, "unexpected indentation");
            }
            let Some(content) = sequence_item(&line.text) else {
                break;
            };
            let content = content.to_owned();
            if content.trim().is_empty() {
                self.at += 1;
                items.push(self.nested(indent)?);
                continue;
            }
            let offset = line.text.len() - content.len();
            let column = indent + offset + (content.len() - content.trim_start().len());
            let content = content.trim_start().to_owned();
            if sequence_item(&content).is_some() || split_key(&content, number)?.is_some() {
                // `- key: value` opens a mapping (or `- - x` a sequence) at the content column.
                self.lines[self.at] = Line {
                    number,
                    indent: column,
                    text: content,
                };
                items.push(self.block(column)?);
            } else {
                self.at += 1;
                items.push(scalar(&content, number)?);
            }
        }
        Ok(Yaml::List(items))
    }
}

pub fn parse(source: &str) -> Result<Yaml, YamlError> {
    let lines = lines(source)?;
    let Some(first) = lines.first() else {
        return Ok(Yaml::Null);
    };
    let indent = first.indent;
    let mut parser = Parser { lines, at: 0 };
    let value = parser.block(indent)?;
    if let Some(line) = parser.peek() {
        return error(line.number, "unexpected indentation");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glab_config_reads_hosts_and_skips_comments() {
        let source = "# What protocol to use [ssh, https]\ngit_protocol: ssh\neditor:\nhost: gitlab.com\nhosts:\n    gitlab.com:\n        # Your token, see https://docs.gitlab.com/#tokens\n        api_protocol: https\n        token: glpat-abc # trailing\n        user: maya\n    git.corp.example:8443:\n        token: 'it''s'\n        url: https://x/#frag\n";
        let value = parse(source).unwrap();
        assert_eq!(value.get("editor"), Some(&Yaml::Null));
        let hosts = value.get("hosts").unwrap();
        assert_eq!(
            hosts.get("gitlab.com").unwrap().text("token"),
            Some("glpat-abc")
        );
        let corp = hosts.get("git.corp.example:8443").unwrap();
        assert_eq!(corp.text("token"), Some("it's"));
        assert_eq!(corp.text("url"), Some("https://x/#frag"));
    }

    #[test]
    fn tea_logins_are_a_sequence_of_mappings() {
        let source = "logins:\n    - name: cb\n      url: https://codeberg.org\n      token: \"abc\\\"def\"\n      default: true\n    - name: home\n      url: https://git.example\npreferences:\n    editor: false\n    flag_defaults: {}\n";
        let value = parse(source).unwrap();
        let logins = value.get("logins").unwrap().as_list();
        assert_eq!(logins.len(), 2);
        assert_eq!(logins[0].text("token"), Some("abc\"def"));
        assert_eq!(logins[1].text("name"), Some("home"));
        // Indentless sequences, as some writers emit them.
        let value = parse("logins:\n- name: a\n  url: https://a\n- name: b\n").unwrap();
        assert_eq!(value.get("logins").unwrap().as_list().len(), 2);
    }

    #[test]
    fn surprises_are_refused_without_echoing_the_line() {
        for source in [
            "a: &anchor x\n",
            "a: *alias\n",
            "token: !!binary abc\n",
            "body: |\n  text\n",
            "list: [a, b]\n",
            "a:\n\tb: c\n",
            "a: b\na: c\n",
            "a: b\n  c: d\n",
            "a: \"open\n",
            "---\na: 1\n---\nb: 2\n",
            "a: x: y\n",
        ] {
            parse(source).expect_err(source);
        }
        let failure = parse("token: !secret-value\n").unwrap_err();
        assert!(!failure.to_string().contains("secret-value"));
    }
}
