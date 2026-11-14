//! Stale-while-revalidate list merging (FORGE.md §7.2): fresh rows replace cached ones without
//! moving the reader. The cursor stays on the same item by id, rows that appear are inserted
//! where the server puts them while the viewport keeps its top row, changed rows update in
//! place (reported so the view can flash them once), and rows that vanished are kept where
//! they were, marked, until the user leaves the list or refreshes explicitly.

#[derive(Clone, Debug, PartialEq)]
pub struct Merged<T> {
    pub rows: Vec<T>,
    /// Index of the cursor's item in `rows` (the same id as before when it survived).
    pub cursor: Option<usize>,
    /// The new index of the row that was at the top of the viewport.
    pub scroll: usize,
    /// Rows that appeared before the old top row (the view shifts by this many).
    pub inserted_above: usize,
    /// Ids of new rows.
    pub inserted: Vec<String>,
    /// Ids of rows whose content changed.
    pub changed: Vec<String>,
    /// Ids of rows kept from the cache although the server no longer lists them.
    pub gone: Vec<String>,
}

/// Merge `fresh` over `old`. `cursor` is the id under the cursor, `scroll` the old index of the
/// viewport's top row. With `keep_gone`, rows missing from `fresh` stay after their nearest
/// surviving predecessor; `mark` turns such a row into its "gone" presentation.
pub fn rows<T: Clone + PartialEq>(
    old: &[T],
    fresh: &[T],
    id: impl Fn(&T) -> String,
    cursor: Option<&str>,
    scroll: usize,
    keep_gone: bool,
    mark: impl Fn(&T) -> T,
) -> Merged<T> {
    let old_ids = old.iter().map(&id).collect::<Vec<_>>();
    let fresh_ids = fresh.iter().map(&id).collect::<Vec<_>>();
    let mut merged = Merged {
        rows: Vec::new(),
        cursor: None,
        scroll: 0,
        inserted_above: 0,
        inserted: Vec::new(),
        changed: Vec::new(),
        gone: Vec::new(),
    };
    let mut rows = fresh.to_vec();
    let mut ids = fresh_ids.clone();
    for (index, row) in fresh.iter().enumerate() {
        match old_ids
            .iter()
            .position(|old_id| *old_id == fresh_ids[index])
        {
            None => merged.inserted.push(fresh_ids[index].clone()),
            Some(position) if old[position] != *row => {
                merged.changed.push(fresh_ids[index].clone());
            }
            Some(_) => {}
        }
    }
    if keep_gone {
        for (position, row) in old.iter().enumerate() {
            if fresh_ids.contains(&old_ids[position]) {
                continue;
            }
            // After the nearest earlier old row that is still present (or at the top).
            let at = old_ids[..position]
                .iter()
                .rev()
                .find_map(|previous| ids.iter().position(|id| id == previous))
                .map_or(0, |found| found + 1);
            rows.insert(at, mark(row));
            ids.insert(at, old_ids[position].clone());
            merged.gone.push(old_ids[position].clone());
        }
    }
    merged.cursor = cursor.and_then(|cursor| ids.iter().position(|id| id == cursor));
    let top = old_ids.get(scroll);
    merged.scroll = top
        .and_then(|top| ids.iter().position(|id| id == top))
        .unwrap_or_else(|| scroll.min(ids.len().saturating_sub(1)));
    merged.inserted_above = ids[..merged.scroll.min(ids.len())]
        .iter()
        .filter(|id| merged.inserted.contains(id))
        .count();
    merged.rows = rows;
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(rows: &[(u32, &str)]) -> Vec<u32> {
        rows.iter().map(|row| row.0).collect()
    }

    fn merge(
        old: &[(u32, &'static str)],
        fresh: &[(u32, &'static str)],
        cursor: u32,
        scroll: usize,
    ) -> Merged<(u32, &'static str)> {
        rows(
            old,
            fresh,
            |row| row.0.to_string(),
            Some(&cursor.to_string()),
            scroll,
            true,
            |row| (row.0, "gone"),
        )
    }

    #[test]
    fn cursor_follows_its_item_when_rows_arrive_above() {
        let old = [(5, "e"), (4, "d"), (3, "c")];
        let fresh = [(7, "g"), (6, "f"), (5, "e"), (4, "d"), (3, "c")];
        let merged = merge(&old, &fresh, 4, 1);
        assert_eq!(merged.cursor, Some(3));
        assert_eq!(merged.rows[3].0, 4);
        assert_eq!(merged.scroll, 3);
        assert_eq!(merged.inserted_above, 2);
        assert_eq!(merged.inserted, ["7", "6"]);
    }

    #[test]
    fn changed_rows_update_in_place_and_vanished_rows_stay_marked() {
        let old = [(3, "c"), (2, "b"), (1, "a")];
        let fresh = [(3, "C"), (1, "a")];
        let merged = merge(&old, &fresh, 2, 0);
        assert_eq!(ids(&merged.rows), [3, 2, 1]);
        assert_eq!(merged.rows[0].1, "C");
        assert_eq!(merged.rows[1].1, "gone");
        assert_eq!(merged.cursor, Some(1));
        assert_eq!(merged.changed, ["3"]);
        assert_eq!(merged.gone, ["2"]);
    }

    #[test]
    fn identical_lists_change_nothing() {
        let old = [(2, "b"), (1, "a")];
        let merged = merge(&old, &old, 1, 0);
        assert_eq!(merged.rows, old);
        assert!(merged.inserted.is_empty() && merged.changed.is_empty() && merged.gone.is_empty());
        assert_eq!(merged.cursor, Some(1));
    }

    #[test]
    fn without_keep_gone_vanished_rows_drop_and_cursor_is_lost() {
        let old = [(2, "b"), (1, "a")];
        let fresh = [(1, "a")];
        let merged = rows(
            &old,
            &fresh,
            |row| row.0.to_string(),
            Some("2"),
            0,
            false,
            Clone::clone,
        );
        assert_eq!(merged.rows, fresh);
        assert_eq!(merged.cursor, None);
    }
}
