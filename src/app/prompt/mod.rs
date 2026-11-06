use std::collections::BTreeMap;

/// `:` completions, Vim style: one row per command, named by its shortest alias that fits
/// what is typed, so `w`, `q` and `e` come first. Labels match too. Commands without an alias
/// follow under their dotted id; plumbing only keys use appears once the input has a `.`.
pub(in crate::app) fn command_completions<'a>(
    policy: &crate::config::Config,
    input: &str,
    commands: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let query = input.to_lowercase();
    // Rank: typed prefix first, then shorter, then lowercase, then alphabetical.
    let rank = |alias: &str, prefix: bool| {
        (
            !prefix,
            alias.len(),
            alias.chars().any(char::is_uppercase),
            alias.to_owned(),
        )
    };
    let mut best = BTreeMap::<String, (bool, usize, bool, String)>::new();
    for (alias, command_id) in &policy.input.command_aliases {
        let label = policy.command_labels.get(command_id).unwrap_or(command_id);
        let prefix = alias.to_lowercase().starts_with(&query);
        if !prefix && !label.to_lowercase().contains(&query) {
            continue;
        }
        let candidate = rank(alias, prefix);
        best.entry(command_id.clone())
            .and_modify(|current| {
                if candidate < *current {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    let hidden = &policy.command_palette.hidden;
    for id in commands {
        let label = policy.command_labels.get(id).map_or(id, String::as_str);
        let plumbing = hidden.iter().any(|prefix| id.starts_with(prefix.as_str()));
        if best.contains_key(id)
            || (plumbing && !query.contains('.'))
            || !(id.contains(&query) || label.to_lowercase().contains(&query))
        {
            continue;
        }
        best.insert(id.to_owned(), rank(id, id.starts_with(&query)));
    }
    let mut rows = best
        .into_iter()
        .map(|(command_id, rank)| {
            let label = policy
                .command_labels
                .get(&command_id)
                .cloned()
                .unwrap_or(command_id);
            (rank, label)
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows.into_iter()
        .map(|(rank, label)| (rank.3, label))
        .collect()
}

/// Commands whose argument is a path, which the command line completes from files.
pub(in crate::app) fn takes_path(command: &str) -> bool {
    matches!(command, "file.open" | "file.save" | "workspace.root.add")
}

pub(in crate::app) mod interaction;

#[cfg(test)]
pub(in crate::app) mod project_tests;

#[cfg(test)]
pub(in crate::app) mod surfaces_tests;

mod commands;
