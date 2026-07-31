use std::collections::BTreeMap;

use crate::editing::{CommandError, CommandInvocation, CommandValue, InvocationSource};
use thiserror::Error;

pub fn parse_ex(
    input: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<CommandInvocation, ExParseError> {
    let input = input
        .trim()
        .strip_prefix(':')
        .unwrap_or(input.trim())
        .trim();
    if input.is_empty() {
        return Err(ExParseError::Empty);
    }
    let (raw_name, arguments) = input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(name, arguments)| (name, arguments.trim()));
    let force = raw_name.ends_with('!');
    let name = raw_name.strip_suffix('!').unwrap_or(raw_name);

    let mut invocation = if name == "config" {
        match arguments.split_once(char::is_whitespace) {
            Some(("explain", key)) if !key.trim().is_empty() => command("config.explain")?
                .with_argument("key", CommandValue::String(key.trim().into())),
            None if arguments == "reload" => command("config.reload")?,
            _ => return Err(ExParseError::UnexpectedArguments(arguments.into())),
        }
    } else if name == "keymap" {
        let mut parts = arguments.split_whitespace();
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
            "workspace.root.add" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("path"));
                }
                command(command_id)?
                    .with_argument("path", CommandValue::String(arguments.to_owned()))
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
            "file.open" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("path"));
                }
                command(command_id)?
                    .with_argument("path", CommandValue::String(arguments.to_owned()))
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
            "workspace.trust.confirm" => {
                if arguments.is_empty() {
                    return Err(ExParseError::MissingArgument("phrase"));
                }
                command(command_id)?
                    .with_argument("phrase", CommandValue::String(arguments.to_owned()))
            }
            "workspace.replace" => {
                let Some((query, replacement)) = arguments.split_once(char::is_whitespace) else {
                    return Err(ExParseError::MissingArgument("replacement"));
                };
                if query.is_empty() || replacement.trim().is_empty() {
                    return Err(ExParseError::MissingArgument("replacement"));
                }
                command(command_id)?
                    .with_argument("query", CommandValue::String(query.into()))
                    .with_argument(
                        "replacement",
                        CommandValue::String(replacement.trim().into()),
                    )
            }
            _ => {
                if force || !arguments.is_empty() {
                    return Err(ExParseError::UnexpectedArguments(arguments.into()));
                }
                command(command_id)?
            }
        }
    };
    invocation.source = InvocationSource::CommandLine;
    Ok(invocation)
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

    use crate::editing::CommandValue;

    use super::parse_ex;

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
    fn parses_configured_aliases_and_bang() {
        let aliases = aliases();
        let quit = parse_ex(":q!", &aliases).expect("quit");
        assert_eq!(quit.id.as_str(), "editor.quit");
        assert_eq!(quit.arguments.get("force"), Some(&CommandValue::Bool(true)));
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
        assert!(parse_ex("e", &aliases).is_err());
    }
}
