use std::io::{Error, ErrorKind};

pub(crate) fn io_reason(error: &Error) -> String {
    match error.kind() {
        ErrorKind::PermissionDenied => " · permission denied",
        ErrorKind::NotFound => " · file not found",
        ErrorKind::StorageFull => " · disk is full",
        ErrorKind::ReadOnlyFilesystem => " · read-only disk",
        ErrorKind::AlreadyExists => " · already exists",
        _ => "",
    }
    .into()
}

pub(crate) fn git_reason(stderr: &str) -> String {
    let Some(line) = stderr.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    let line = line
        .strip_prefix("fatal:")
        .or_else(|| line.strip_prefix("error:"))
        .unwrap_or(line)
        .trim();
    if line.is_empty() {
        return String::new();
    }
    if line.chars().count() <= 60 {
        return format!(" · {line}");
    }
    let prefix: String = line.chars().take(60).collect();
    let end = prefix.rfind(char::is_whitespace).unwrap_or(0);
    format!(" · {}…", prefix[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_short_and_only_known_io_errors_are_shown() {
        assert_eq!(
            io_reason(&Error::from(ErrorKind::PermissionDenied)),
            " · permission denied"
        );
        assert_eq!(io_reason(&Error::from(ErrorKind::Other)), "");
        assert_eq!(
            git_reason("\n fatal: cannot lock the index\nmore details"),
            " · cannot lock the index"
        );
        assert_eq!(
            git_reason(
                "error: one two three four five six seven eight nine ten eleven twelve thirteen fourteen"
            ),
            " · one two three four five six seven eight nine ten eleven…"
        );
        assert_eq!(git_reason("\n \n"), "");
        assert_eq!(git_reason("fatal: \n"), "");
        assert_eq!(git_reason("error:   bad thing"), " · bad thing");
    }
}
