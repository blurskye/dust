//! Parsing of command-line input and arguments.

use std::collections::BTreeMap;

use crate::editor::CommandError;

use crate::editor::CommandInvocation;

use crate::editor::CommandValue;

use crate::editor::InvocationSource;
use thiserror::Error;

/// The range and the delimited parts of `:s/a/b/g`, with an optional range: `%`, `'<,'>`, `5,9`,
/// `.,$`. Anything else isn't a substitution.
fn substitute_parts(input: &str) -> Option<(&str, Vec<String>)> {
    let name_at = input
        .find(|character: char| !(character.is_ascii_digit() || "%.,$'<>".contains(character)))?;
    let (range, rest) = input.split_at(name_at);
    let rest = rest
        .strip_prefix("substitute")
        .or_else(|| rest.strip_prefix('s'))?;
    let delimiter = rest
        .chars()
        .next()
        .filter(|character| !character.is_alphanumeric() && !character.is_whitespace())?;
    Some((
        range,
        split_unescaped(&rest[delimiter.len_utf8()..], delimiter),
    ))
}

fn parse_substitute(input: &str) -> Option<Result<CommandInvocation, ExParseError>> {
    let (range, parts) = substitute_parts(input)?;
    let mut parts = parts.into_iter();
    let pattern = parts.next().unwrap_or_default();
    let replacement = parts.next().unwrap_or_default();
    let flags = parts.next().unwrap_or_default().trim_end().to_owned();
    if parts.next().is_some() {
        return Some(Err(ExParseError::UnexpectedArguments(input.into())));
    }
    Some(
        command("edit.substitute_lines")
            .map(|invocation| {
                invocation
                    .with_argument("range", CommandValue::String(range.to_owned()))
                    .with_argument("pattern", CommandValue::String(pattern))
                    .with_argument("replacement", CommandValue::String(replacement))
                    .with_argument("flags", CommandValue::String(flags))
            })
            .map_err(Into::into),
    )
}

/// `:g/pattern/d` deletes every line that matches, `:v/pattern/d` every line that doesn't.
/// Without a range they cover the whole file, as in Vim.
fn parse_global(input: &str) -> Option<Result<CommandInvocation, ExParseError>> {
    let name_at = input
        .find(|character: char| !(character.is_ascii_digit() || "%.,$'<>".contains(character)))?;
    let (range, rest) = input.split_at(name_at);
    let (invert, rest) = [
        ("vglobal", true),
        ("global!", true),
        ("global", false),
        ("g!", true),
        ("v", true),
        ("g", false),
    ]
    .into_iter()
    .find_map(|(name, invert)| rest.strip_prefix(name).map(|rest| (invert, rest)))?;
    let delimiter = rest
        .chars()
        .next()
        .filter(|character| !character.is_alphanumeric() && !character.is_whitespace())?;
    let mut parts = split_unescaped(&rest[delimiter.len_utf8()..], delimiter).into_iter();
    let pattern = parts.next().unwrap_or_default();
    let action = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Some(Err(ExParseError::UnexpectedArguments(input.into())));
    }
    if pattern.is_empty() {
        return Some(Err(ExParseError::MissingArgument("pattern")));
    }
    if !matches!(action.trim(), "d" | "delete") {
        return Some(Err(ExParseError::UnexpectedArguments(format!(
            "{action}: only `d` works after :g and :v"
        ))));
    }
    Some(
        command("edit.global_delete")
            .map(|invocation| {
                invocation
                    .with_argument("range", CommandValue::String(range.to_owned()))
                    .with_argument("pattern", CommandValue::String(pattern))
                    .with_argument("invert", CommandValue::Bool(invert))
            })
            .map_err(Into::into),
    )
}

/// The pattern of a `:s` while it is being typed, so its matches can show before it runs.
#[must_use]
pub fn substitute_pattern(input: &str) -> Option<String> {
    let input = input.trim_start().trim_start_matches(':').trim_start();
    let (_, parts) = substitute_parts(input)?;
    let pattern = parts.into_iter().next()?;
    Some(substitution_pattern(&pattern))
}

/// Translate Vim word boundaries without changing escaped backslashes.
pub(crate) fn substitution_pattern(pattern: &str) -> String {
    let mut converted = String::with_capacity(pattern.len());
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        converted.push(character);
        if character == '\\' {
            match characters.next() {
                Some('<') => converted.push_str("b{start}"),
                Some('>') => converted.push_str("b{end}"),
                Some(next) => converted.push(next),
                None => {}
            }
        }
    }
    converted
}

/// Range, pattern, replacement and flags of a `:s` being typed, once its replacement has begun.
#[must_use]
pub fn typed_substitution(input: &str) -> Option<(String, String, String, String)> {
    let input = input.trim_start().trim_start_matches(':').trim_start();
    let (range, parts) = substitute_parts(input)?;
    let mut parts = parts.into_iter();
    let pattern = parts.next()?;
    let replacement = parts.next()?;
    Some((
        range.to_owned(),
        pattern,
        replacement,
        parts.next().unwrap_or_default().trim_end().to_owned(),
    ))
}

/// Split delimiters while preserving their literal meaning in patterns and replacements.
fn split_unescaped(text: &str, delimiter: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        let pattern = parts.len() == 1;
        let current = parts.last_mut().expect("starts with one part");
        if character == '\\' {
            match characters.next() {
                Some(next) if next == delimiter => {
                    if pattern {
                        current.push_str(&regex::escape(&next.to_string()));
                    } else {
                        if matches!(next, '&' | '\\') {
                            current.push('\\');
                        }
                        current.push(next);
                    }
                }
                Some(next) => {
                    current.push('\\');
                    current.push(next);
                }
                None => current.push('\\'),
            }
        } else if character == delimiter {
            parts.push(String::new());
        } else {
            current.push(character);
        }
    }
    parts
}

pub fn parse_ex(
    input: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<CommandInvocation, ExParseError> {
    let input = input.trim_start();
    let input = input.strip_prefix(':').unwrap_or(input).trim_start();
    if input.trim_end().is_empty() {
        return Err(ExParseError::Empty);
    }
    if let Some(invocation) = parse_substitute(input).or_else(|| parse_global(input)) {
        return invocation;
    }
    let raw_input = input;
    let input = input.trim_end();
    // `:12` goes to line 12 and `:$` to the last line, as `12G` and `G` do.
    if input == "$" || input.bytes().all(|byte| byte.is_ascii_digit()) {
        let mut invocation = command("view.cursor.move")?
            .with_argument("motion", CommandValue::String("document_end".into()));
        invocation.count = input
            .parse::<u64>()
            .ok()
            .map(|line| line.max(1))
            .and_then(std::num::NonZeroU64::new);
        return Ok(invocation);
    }
    // Visual mode opens the line with its range; other commands don't use it.
    let input = raw_input
        .strip_prefix("'<,'>")
        .map_or(raw_input, str::trim_start);
    let (raw_name, raw_arguments) = input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(name, arguments)| {
            (name, arguments.trim_start())
        });
    let arguments = raw_arguments.trim_end();
    let force = raw_name.ends_with('!');
    let name = raw_name.strip_suffix('!').unwrap_or(raw_name);

    let mut invocation = if name == "set" {
        if !arguments.is_empty() {
            return Err(ExParseError::UnexpectedArguments(arguments.into()));
        }
        command("config.open")?
    } else if name == "package" {
        let (action, rest) = arguments
            .split_once(char::is_whitespace)
            .map_or((arguments, ""), |(action, rest)| (action, rest.trim()));
        match action {
            "search" if !rest.is_empty() => command("package.registry.search")?
                .with_argument("query", CommandValue::String(rest.into())),
            "list" | "" if rest.is_empty() => command("package.list")?,
            "verify" if rest.is_empty() => command("package.verify")?,
            "rollback" if rest.is_empty() => command("package.rollback")?,
            "gc" if rest.is_empty() => command("package.gc")?,
            "remove" if !rest.is_empty() => {
                command("package.remove")?.with_argument("name", CommandValue::String(rest.into()))
            }
            _ => {
                return Err(ExParseError::UnexpectedArguments(format!(
                    "{arguments}: use search list verify rollback gc remove"
                )));
            }
        }
    } else if name == "config" {
        match arguments.split_once(char::is_whitespace) {
            Some(("explain", key)) if !key.trim().is_empty() => command("config.explain")?
                .with_argument("key", CommandValue::String(key.trim().into())),
            None if arguments == "reload" => command("config.reload")?,
            _ => return Err(ExParseError::UnexpectedArguments(arguments.into())),
        }
    } else if name == "keymap" {
        let mut parts = Vec::new();
        let mut rest = arguments;
        while !rest.is_empty() {
            let (part, _, remaining) = argument(rest)?;
            parts.push(part);
            rest = remaining;
        }
        let mut parts = parts.iter().map(String::as_str);
        match parts.next() {
            Some("inspect") => {
                let mut invocation = command("keymap.inspect")?;
                if let Some(key_or_id) = parts.next() {
                    invocation = if let Some(id) = key_or_id.strip_prefix("id=") {
                        if id.is_empty() {
                            return Err(ExParseError::UnexpectedArguments(arguments.into()));
                        }
                        invocation.with_argument("id", CommandValue::String(id.into()))
                    } else {
                        invocation.with_argument("key", CommandValue::String(key_or_id.into()))
                    };
                }
                if let Some(context) = parts.next() {
                    invocation =
                        invocation.with_argument("context", CommandValue::String(context.into()));
                }
                if parts.next().is_some() {
                    return Err(ExParseError::UnexpectedArguments(arguments.into()));
                }
                invocation
            }
            Some("conflicts") if parts.next().is_none() => command("keymap.conflicts")?,
            _ => return Err(ExParseError::UnexpectedArguments(arguments.into())),
        }
    } else {
        let command_id = aliases
            .get(name)
            .map(String::as_str)
            .or_else(|| name.contains('.').then_some(name))
            .ok_or_else(|| ExParseError::UnknownCommand(name.into()))?;
        match command_id {
            "file.save" => {
                let mut invocation =
                    command(command_id)?.with_argument("force", CommandValue::Bool(force));
                if !arguments.is_empty() {
                    invocation = invocation
                        .with_argument("path", CommandValue::String(arguments.to_owned()));
                }
                invocation
            }
            "editor.quit" | "file.reload" | "buffer.close" => {
                if !arguments.is_empty() {
                    return Err(ExParseError::UnexpectedArguments(arguments.into()));
                }
                command(command_id)?.with_argument("force", CommandValue::Bool(force))
            }
            "file.save_quit" => {
                if !arguments.is_empty() {
                    return Err(ExParseError::UnexpectedArguments(arguments.into()));
                }
                command(command_id)?.with_argument("force", CommandValue::Bool(force))
            }
            "workspace.root.add" | "explorer.create" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("path"));
                }
                command(command_id)?
                    .with_argument("path", CommandValue::String(arguments.to_owned()))
            }
            "explorer.rename" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("target"));
                }
                command(command_id)?
                    .with_argument("target", CommandValue::String(arguments.to_owned()))
            }
            "workspace.root.activate" | "workspace.root.remove" | "terminal.focus" => {
                let id = arguments
                    .parse::<i64>()
                    .map_err(|_| ExParseError::UnexpectedArguments(arguments.into()))?;
                command(command_id)?.with_argument("id", CommandValue::Integer(id))
            }
            "terminal.open" => {
                let invocation = command(command_id)?;
                if arguments.is_empty() {
                    invocation
                } else {
                    invocation.with_argument("profile", CommandValue::String(arguments.to_owned()))
                }
            }
            "terminal.send" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("text"));
                }
                command(command_id)?
                    .with_argument("text", CommandValue::String(arguments.to_owned()))
            }
            "terminal.resize" => {
                let rows = arguments
                    .parse::<i64>()
                    .map_err(|_| ExParseError::UnexpectedArguments(arguments.into()))?;
                command(command_id)?.with_argument("rows", CommandValue::Integer(rows))
            }
            "terminal.cycle" => {
                let direction = arguments
                    .parse::<i64>()
                    .map_err(|_| ExParseError::UnexpectedArguments(arguments.into()))?;
                command(command_id)?.with_argument("direction", CommandValue::Integer(direction))
            }
            "package.registry.search" if !arguments.is_empty() && !arguments.contains('=') => {
                command(command_id)?
                    .with_argument("query", CommandValue::String(arguments.to_owned()))
            }
            "theme.select" if !arguments.is_empty() => command(command_id)?
                .with_argument("name", CommandValue::String(arguments.to_owned())),
            // As in Vim, `:e` with no file reloads the current one.
            "file.open" if arguments.is_empty() => {
                command("file.reload")?.with_argument("force", CommandValue::Bool(force))
            }
            "file.open" => {
                let invocation = command(command_id)?
                    .with_argument("path", CommandValue::String(arguments.to_owned()));
                if force {
                    invocation.with_argument("force", CommandValue::Bool(true))
                } else {
                    invocation
                }
            }
            "dap.session.start" => {
                let invocation = command(command_id)?;
                if arguments.is_empty() {
                    invocation
                } else {
                    invocation.with_argument("program", CommandValue::String(arguments.to_owned()))
                }
            }
            "lsp.rename" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("name"));
                }
                command(command_id)?
                    .with_argument("name", CommandValue::String(arguments.to_owned()))
            }
            "lsp.workspace_symbols" | "workspace.search" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("query"));
                }
                command(command_id)?
                    .with_argument("query", CommandValue::String(arguments.to_owned()))
            }
            "dap.evaluate" | "dap.watch.add" => {
                // An expression has spaces of its own, so all of it is the argument.
                let expression = arguments.strip_prefix("expression=").unwrap_or(arguments);
                if expression.is_empty() {
                    return Err(ExParseError::MissingArgument("expression"));
                }
                command(command_id)?
                    .with_argument("expression", CommandValue::String(expression.to_owned()))
            }
            "workspace.git.worktree.create" => {
                let mut parts = arguments.split_whitespace();
                let branch = parts
                    .next()
                    .ok_or(ExParseError::MissingArgument("branch"))?;
                let mut invocation = command(command_id)?
                    .with_argument("branch", CommandValue::String(branch.into()));
                if let Some(path) = parts.next() {
                    invocation =
                        invocation.with_argument("path", CommandValue::String(path.into()));
                }
                if parts.next().is_some() {
                    return Err(ExParseError::UnexpectedArguments(arguments.into()));
                }
                invocation
            }
            "workspace.replace" => {
                let (query, _, replacement) = argument(raw_arguments)?;
                if query.is_empty() {
                    return Err(ExParseError::MissingArgument("query"));
                }
                if replacement.is_empty() {
                    return Err(ExParseError::MissingArgument("replacement"));
                }
                let replacement = if replacement.starts_with(['\'', '"']) {
                    let (value, _, rest) = argument(replacement)?;
                    if !rest.is_empty() {
                        return Err(ExParseError::UnexpectedArguments(rest.into()));
                    }
                    value
                } else {
                    replacement.to_owned()
                };
                command(command_id)?
                    .with_argument("query", CommandValue::String(query))
                    .with_argument("replacement", CommandValue::String(replacement))
            }
            _ => {
                if force {
                    return Err(ExParseError::UnexpectedArguments(arguments.into()));
                }
                let mut invocation = command(command_id)?;
                let mut rest = arguments;
                while !rest.is_empty() {
                    let (argument, quoted, remaining) = argument(rest)?;
                    rest = remaining;
                    let Some((name, value)) = argument.split_once('=') else {
                        return Err(ExParseError::UnexpectedArguments(arguments.into()));
                    };
                    if name.is_empty() || (value.is_empty() && !quoted) {
                        return Err(ExParseError::UnexpectedArguments(arguments.into()));
                    }
                    let value = match value {
                        value if quoted => CommandValue::String(value.into()),
                        "true" => CommandValue::Bool(true),
                        "false" => CommandValue::Bool(false),
                        value => value.parse::<i64>().map_or_else(
                            |_| CommandValue::String(value.into()),
                            CommandValue::Integer,
                        ),
                    };
                    invocation = invocation.with_argument(name, value);
                }
                invocation
            }
        }
    };
    invocation.source = InvocationSource::CommandLine;
    Ok(invocation)
}

/// Read one argument, quoting either the whole token or the value after `=`.
/// Backslashes in quotes escape only the quote itself or another backslash.
fn argument(input: &str) -> Result<(String, bool, &str), ExParseError> {
    let mut value = String::new();
    let mut quote = None;
    let mut quoted = false;
    let mut characters = input.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else if character == '\\'
                && characters
                    .peek()
                    .is_some_and(|(_, next)| *next == delimiter || *next == '\\')
            {
                value.push(characters.next().expect("peeked character").1);
            } else {
                value.push(character);
            }
        } else if matches!(character, '\'' | '"')
            && (value.is_empty()
                || value
                    .split_once('=')
                    .is_some_and(|(_, value)| value.is_empty()))
        {
            quote = Some(character);
            quoted = true;
        } else if character.is_whitespace() {
            return Ok((value, quoted, input[index..].trim_start()));
        } else {
            value.push(character);
        }
    }
    if quote.is_some() {
        return Err(ExParseError::UnexpectedArguments(input.into()));
    }
    Ok((value, quoted, ""))
}

fn command(id: &str) -> Result<CommandInvocation, CommandError> {
    CommandInvocation::new(id, InvocationSource::CommandLine)
}

#[derive(Debug, Error)]
pub enum ExParseError {
    #[error("command line is empty")]
    Empty,
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("command requires argument `{0}`")]
    MissingArgument(&'static str),
    #[error("unexpected command arguments `{0}`")]
    UnexpectedArguments(String),
    #[error(transparent)]
    InvalidCommand(#[from] CommandError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::editor::CommandValue;

    use crate::editor::input::command_line::parse_ex;

    fn aliases() -> BTreeMap<String, String> {
        [
            ("w", "file.save"),
            ("write", "file.save"),
            ("q", "editor.quit"),
            ("quit", "editor.quit"),
            ("wq", "file.save_quit"),
            ("x", "file.save_quit"),
            ("e", "file.open"),
            ("edit", "file.open"),
            ("new", "file.new"),
            ("reload", "file.reload"),
            ("bd", "buffer.close"),
            ("health", "health.open"),
        ]
        .into_iter()
        .map(|(alias, command)| (alias.into(), command.into()))
        .collect()
    }

    #[test]
    fn delimited_commands_reject_surplus_fields() {
        for input in ["s/a/b/g/garbage", "s/a/b/g/", "g/a/d/garbage", "v/a/d/"] {
            assert!(parse_ex(input, &aliases()).is_err(), "{input}");
        }
        for input in ["s/a/b/", "s/a/b/g", "g/a/d", "v/a/delete", r"s/a/b\/c/g"] {
            assert!(parse_ex(input, &aliases()).is_ok(), "{input}");
        }
    }

    #[test]
    fn substitute_preview_preserves_escaped_word_boundaries() {
        for (pattern, expected) in [
            (r"\<cat\>", r"\b{start}cat\b{end}"),
            (r"\\<", r"\\<"),
            (r"\\>", r"\\>"),
            (r"\\\<cat", r"\\\b{start}cat"),
        ] {
            assert_eq!(
                crate::editor::input::command_line::substitute_pattern(&format!("s/{pattern}/x/")),
                Some(expected.into())
            );
        }
    }

    #[test]
    fn substitute_execution_keeps_the_preview_replacement_whitespace() {
        for input in [":s/a/b ", "  : s/a/b  ", "s/a/ ", "s/a/b\t"] {
            let invocation = parse_ex(input, &aliases()).unwrap();
            let (_, _, replacement, _) =
                crate::editor::input::command_line::typed_substitution(input).unwrap();
            assert_eq!(
                invocation.arguments["replacement"],
                CommandValue::String(replacement)
            );
        }
        assert_eq!(
            parse_ex("  : q!  ", &aliases()).unwrap().id.as_str(),
            "editor.quit"
        );
        assert_eq!(
            parse_ex("s/a/b/g  ", &aliases()).unwrap().arguments["flags"],
            CommandValue::String("g".into())
        );
        for input in ["", "  ", ":  "] {
            assert!(matches!(
                parse_ex(input, &aliases()),
                Err(crate::editor::input::command_line::ExParseError::Empty)
            ));
        }
    }

    #[test]
    fn special_commands_reject_unused_arguments() {
        for input in ["set editor.tab_width=8", "package list nonsense"] {
            assert!(parse_ex(input, &aliases()).is_err(), "{input}");
        }
        for (input, expected) in [
            ("set", "config.open"),
            ("package list", "package.list"),
            ("package", "package.list"),
        ] {
            assert_eq!(parse_ex(input, &aliases()).unwrap().id.as_str(), expected);
        }
    }

    #[test]
    fn keymap_inspection_accepts_quoted_sequences() {
        for input in [
            r#"keymap inspect "g g" editor.normal"#,
            "keymap inspect 'g g' editor.normal",
        ] {
            let invocation = parse_ex(input, &aliases()).unwrap();
            assert_eq!(
                invocation.arguments["key"],
                CommandValue::String("g g".into())
            );
            assert_eq!(
                invocation.arguments["context"],
                CommandValue::String("editor.normal".into())
            );
        }
        for input in [
            "keymap inspect 'g g",
            "keymap inspect 'g g' editor.normal extra",
        ] {
            assert!(parse_ex(input, &aliases()).is_err(), "{input}");
        }
    }

    #[test]
    fn workspace_replacements_accept_empty_and_whitespace_strings() {
        for (replacement, expected) in [
            (r#""""#, ""),
            ("''", ""),
            (r#""  ""#, "  "),
            ("two words  ", "two words  "),
        ] {
            let invocation = parse_ex(
                &format!("workspace.replace needle {replacement}"),
                &aliases(),
            )
            .unwrap();
            assert_eq!(
                invocation.arguments["query"],
                CommandValue::String("needle".into())
            );
            assert_eq!(
                invocation.arguments["replacement"],
                CommandValue::String(expected.into())
            );
        }
        let invocation = parse_ex(r#"workspace.replace "two words" """#, &aliases()).unwrap();
        assert_eq!(
            invocation.arguments["query"],
            CommandValue::String("two words".into())
        );
        assert!(parse_ex("workspace.replace needle", &aliases()).is_err());
    }

    #[test]
    fn generic_quoted_arguments_remain_strings() {
        for value in [r#""1""#, "'1'"] {
            let invocation =
                parse_ex(&format!("navigation.mark.jump name={value}"), &aliases()).unwrap();
            assert_eq!(
                invocation.arguments["name"],
                CommandValue::String("1".into())
            );
        }
        let invocation =
            parse_ex(r#"dap.watch.remove expression="total * 10""#, &aliases()).unwrap();
        assert_eq!(
            invocation.arguments["expression"],
            CommandValue::String("total * 10".into())
        );
        let invocation = parse_ex(
            r#"lint.run name="" label='true' path="a\\b\"c" retries=2 enabled=true"#,
            &aliases(),
        )
        .unwrap();
        assert_eq!(
            invocation.arguments["name"],
            CommandValue::String(String::new())
        );
        assert_eq!(
            invocation.arguments["label"],
            CommandValue::String("true".into())
        );
        assert_eq!(
            invocation.arguments["path"],
            CommandValue::String("a\\b\"c".into())
        );
        assert_eq!(invocation.arguments["retries"], CommandValue::Integer(2));
        assert_eq!(invocation.arguments["enabled"], CommandValue::Bool(true));
        for (input, name, expected) in [
            ("lint.run name=don't", "name", "don't"),
            (
                r#"dap.watch.remove expression=map["key"]"#,
                "expression",
                r#"map["key"]"#,
            ),
        ] {
            let invocation = parse_ex(input, &aliases()).unwrap();
            assert_eq!(
                invocation.arguments[name],
                CommandValue::String(expected.into())
            );
        }
        for input in [
            "lint.run name=",
            "lint.run name='unfinished",
            "lint.run =value",
        ] {
            assert!(parse_ex(input, &aliases()).is_err(), "{input}");
        }
    }

    #[test]
    fn escaped_delimiters_keep_regex_and_replacement_meaning() {
        let global = parse_ex(r"g.\..d", &aliases()).unwrap();
        assert_eq!(
            global.arguments["pattern"],
            CommandValue::String(r"\.".into())
        );
        let substitute = parse_ex(r"s.\..x.", &aliases()).unwrap();
        assert_eq!(
            substitute.arguments["pattern"],
            CommandValue::String(r"\.".into())
        );
        let substitute = parse_ex(r"s&x&\&&", &aliases()).unwrap();
        assert_eq!(
            substitute.arguments["replacement"],
            CommandValue::String(r"\&".into())
        );
        let substitute = parse_ex(r"s/a\/b/c\/d/", &aliases()).unwrap();
        assert_eq!(
            substitute.arguments["pattern"],
            CommandValue::String("a/b".into())
        );
        assert_eq!(
            substitute.arguments["replacement"],
            CommandValue::String("c/d".into())
        );
    }

    #[test]
    fn a_line_number_goes_to_that_line() {
        let line = parse_ex(":5", &aliases()).expect("line");
        assert_eq!(line.id.as_str(), "view.cursor.move");
        assert_eq!(line.count.map(std::num::NonZeroU64::get), Some(5));
        let last = parse_ex("$", &aliases()).expect("last line");
        assert_eq!(
            last.arguments["motion"],
            CommandValue::String("document_end".into())
        );
        assert_eq!(last.count, None);
    }

    #[test]
    fn parses_configured_aliases_and_bang() {
        let aliases = aliases();
        let quit = parse_ex(":q!", &aliases).expect("quit");
        assert_eq!(quit.id.as_str(), "editor.quit");
        assert_eq!(quit.arguments.get("force"), Some(&CommandValue::Bool(true)));
        let watch = parse_ex("dap.watch.add total * 10", &aliases).expect("watch");
        assert_eq!(
            watch.arguments.get("expression"),
            Some(&CommandValue::String("total * 10".into()))
        );
        let search = parse_ex("package.registry.search theme", &aliases).expect("search");
        assert_eq!(
            search.arguments["query"],
            CommandValue::String("theme".into())
        );
        let named = parse_ex("package.registry.search query=theme", &aliases).expect("named");
        assert_eq!(
            named.arguments["query"],
            CommandValue::String("theme".into())
        );
        let edit = parse_ex("edit src/main.rs", &aliases).expect("edit");
        assert_eq!(edit.id.as_str(), "file.open");
        assert_eq!(
            edit.arguments.get("path"),
            Some(&CommandValue::String("src/main.rs".into()))
        );
        let save_as = parse_ex("w! copy.rs", &aliases).expect("save as");
        assert_eq!(save_as.id.as_str(), "file.save");
        assert_eq!(
            save_as.arguments.get("path"),
            Some(&CommandValue::String("copy.rs".into()))
        );
        assert_eq!(
            save_as.arguments.get("force"),
            Some(&CommandValue::Bool(true))
        );
        assert_eq!(
            parse_ex("reload!", &aliases).expect("reload").id.as_str(),
            "file.reload"
        );
        let explain = parse_ex("config explain ui.theme", &aliases).expect("config explain");
        assert_eq!(explain.id.as_str(), "config.explain");
        assert_eq!(
            explain.arguments.get("key"),
            Some(&CommandValue::String("ui.theme".into()))
        );
        assert_eq!(
            parse_ex("config reload", &aliases)
                .expect("config reload")
                .id
                .as_str(),
            "config.reload"
        );
        assert_eq!(
            parse_ex("keymap conflicts", &aliases)
                .expect("keymap conflicts")
                .id
                .as_str(),
            "keymap.conflicts"
        );
        let inspect = parse_ex(
            "keymap inspect id=builtin.dashboard.surface.editor-quit.q",
            &aliases,
        )
        .expect("keymap id inspection");
        assert_eq!(inspect.id.as_str(), "keymap.inspect");
        assert_eq!(
            inspect.arguments.get("id"),
            Some(&CommandValue::String(
                "builtin.dashboard.surface.editor-quit.q".into()
            ))
        );
        let search = parse_ex("workspace.search two words", &aliases).expect("workspace search");
        assert_eq!(
            search.arguments.get("query"),
            Some(&CommandValue::String("two words".into()))
        );
        let replace =
            parse_ex("workspace.replace needle two words", &aliases).expect("workspace replace");
        assert_eq!(
            replace.arguments.get("replacement"),
            Some(&CommandValue::String("two words".into()))
        );
        let debug =
            parse_ex("dap.session.start target/debug/app", &aliases).expect("debug program");
        assert_eq!(debug.id.as_str(), "dap.session.start");
        assert_eq!(
            debug.arguments.get("program"),
            Some(&CommandValue::String("target/debug/app".into()))
        );
        let generic = parse_ex("lint.run name=check retries=2 enabled=true", &aliases)
            .expect("typed generic arguments");
        assert_eq!(
            generic.arguments.get("name"),
            Some(&CommandValue::String("check".into()))
        );
        assert_eq!(
            generic.arguments.get("retries"),
            Some(&CommandValue::Integer(2))
        );
        assert_eq!(
            generic.arguments.get("enabled"),
            Some(&CommandValue::Bool(true))
        );
        let terminal = parse_ex("terminal.open shell", &aliases).expect("terminal profile");
        assert_eq!(terminal.id.as_str(), "terminal.open");
        assert_eq!(
            terminal.arguments.get("profile"),
            Some(&CommandValue::String("shell".into()))
        );
        let default_terminal = parse_ex("terminal.open", &aliases).expect("default terminal");
        assert_eq!(default_terminal.id.as_str(), "terminal.open");
        assert!(default_terminal.arguments.is_empty());
        let resize = parse_ex("terminal.resize -1", &aliases).expect("terminal resize");
        assert_eq!(
            resize.arguments.get("rows"),
            Some(&CommandValue::Integer(-1))
        );
    }

    #[test]
    fn rejects_unknown_and_missing_path() {
        let aliases = aliases();
        assert!(parse_ex("wat", &aliases).is_err());
        for (line, force) in [("e", false), ("e!", true)] {
            let reload = parse_ex(line, &aliases).expect("reload");
            assert_eq!(reload.id.as_str(), "file.reload");
            assert_eq!(
                reload.arguments.get("force"),
                Some(&CommandValue::Bool(force))
            );
        }
        let theme = parse_ex("theme.select nord", &aliases).expect("named theme");
        assert_eq!(
            theme.arguments.get("name"),
            Some(&CommandValue::String("nord".into()))
        );
        let forced = parse_ex("e! a.txt", &aliases).expect("forced open");
        assert_eq!(forced.id.as_str(), "file.open");
        assert_eq!(
            forced.arguments.get("force"),
            Some(&CommandValue::Bool(true))
        );
    }
}
