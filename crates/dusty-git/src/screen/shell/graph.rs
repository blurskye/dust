//! Commit graph lanes: the lane assignment `git log --graph` and
//! lazygit use, one row per commit, two cells per lane.

/// One painted cell: a lane glyph and the lane colour index it belongs to.
pub type Cell = (char, u8);

/// `NODE` marks the commit's own cell; the painter picks ●/○/◆ for it.
pub const NODE: char = '*';

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphRow {
    pub cells: Vec<Option<Cell>>,
}

impl GraphRow {
    /// Plain text of the row, for tests and screen dumps.
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.cells
            .iter()
            .map(|cell| cell.map_or(' ', |(glyph, _)| glyph))
            .collect::<String>()
            .trim_end()
            .to_owned()
    }
}

/// `commits` is newest first, each `(id, parents)` as `git log --format=%H%x09%P` prints.
pub fn rows<S: AsRef<str>>(commits: &[(S, Vec<S>)]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<(String, u8)>> = Vec::new();
    let mut colours = 0u8;
    let mut next_colour = || {
        let colour = colours;
        colours = colours.wrapping_add(1);
        colour
    };
    let mut result = Vec::with_capacity(commits.len());
    for (id, parents) in commits {
        let id = id.as_ref();
        let matching = lanes
            .iter()
            .enumerate()
            .filter(|(_, lane)| lane.as_ref().is_some_and(|(want, _)| want == id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let column = match matching.first() {
            Some(&column) => column,
            None => {
                let colour = next_colour();
                let free = lanes.iter().position(Option::is_none);
                let column = free.unwrap_or(lanes.len());
                if column == lanes.len() {
                    lanes.push(None);
                }
                lanes[column] = Some((id.to_owned(), colour));
                column
            }
        };
        let colour = lanes[column].as_ref().map_or(0, |(_, colour)| *colour);
        let mut cells: Vec<Option<Cell>> = vec![None; lanes.len() * 2];
        for (index, lane) in lanes.iter().enumerate() {
            if let Some((_, lane_colour)) = lane
                && index != column
                && !matching.contains(&index)
            {
                cells[index * 2] = Some(('│', *lane_colour));
            }
        }
        // Children waiting on this commit in other lanes join it here.
        for &other in matching.iter().skip(1) {
            let lane_colour = lanes[other].as_ref().map_or(0, |(_, colour)| *colour);
            cells[other * 2] = Some((if other > column { '╯' } else { '╰' }, lane_colour));
            connect(&mut cells, column, other, lane_colour);
            lanes[other] = None;
        }
        cells[column * 2] = Some((NODE, colour));
        let mut parents = parents.iter().map(AsRef::as_ref);
        lanes[column] = parents.next().map(|first| (first.to_owned(), colour));
        for parent in parents {
            let existing = lanes
                .iter()
                .position(|lane| lane.as_ref().is_some_and(|(want, _)| want == parent));
            let (target, lane_colour, glyph) = match existing {
                Some(target) => {
                    let lane_colour = lanes[target].as_ref().map_or(0, |(_, colour)| *colour);
                    (target, lane_colour, if target > column { '┤' } else { '├' })
                }
                None => {
                    let lane_colour = next_colour();
                    let target = lanes
                        .iter()
                        .enumerate()
                        .position(|(index, lane)| index > column && lane.is_none())
                        .unwrap_or(lanes.len());
                    if target == lanes.len() {
                        lanes.push(None);
                    }
                    lanes[target] = Some((parent.to_owned(), lane_colour));
                    (target, lane_colour, if target > column { '╮' } else { '╭' })
                }
            };
            if cells.len() < (target + 1) * 2 {
                cells.resize((target + 1) * 2, None);
            }
            cells[target * 2] = Some((glyph, lane_colour));
            connect(&mut cells, column, target, lane_colour);
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        while cells.last().is_some_and(Option::is_none) {
            cells.pop();
        }
        result.push(GraphRow { cells });
    }
    result
}

/// Horizontal run between two lanes, crossing any lane that passes between them.
fn connect(cells: &mut [Option<Cell>], from: usize, to: usize, colour: u8) {
    let (low, high) = (from.min(to), from.max(to));
    for cell in &mut cells[low * 2 + 1..high * 2] {
        *cell = match *cell {
            Some(('│', passing)) => Some(('┼', passing)),
            None => Some(('─', colour)),
            other => other,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(commits: &[(&str, &[&str])]) -> Vec<String> {
        let commits = commits
            .iter()
            .map(|(id, parents)| (*id, parents.to_vec()))
            .collect::<Vec<_>>();
        rows(&commits).iter().map(GraphRow::text).collect()
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        assert_eq!(
            graph(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]),
            ["*", "*", "*"]
        );
    }

    #[test]
    fn a_fork_opens_a_second_lane_and_closes_where_it_branched() {
        // fix/backoff (d → c) and feat/search (s) both fork from main (m).
        assert_eq!(
            graph(&[
                ("d", &["c"]),
                ("c", &["m"]),
                ("s", &["m"]),
                ("m", &["r"]),
                ("r", &[]),
            ]),
            ["*", "*", "│ *", "*─╯", "*"]
        );
    }

    #[test]
    fn a_merge_opens_the_second_parent_to_the_right() {
        assert_eq!(
            graph(&[
                ("merge", &["main", "topic"]),
                ("topic", &["base"]),
                ("main", &["base"]),
                ("base", &[]),
            ]),
            ["*─╮", "│ *", "* │", "*─╯"]
        );
    }

    #[test]
    fn a_lane_crossing_a_connector_is_kept_visible() {
        let rows = graph(&[
            ("a", &["x"]),
            ("b", &["y"]),
            ("m", &["z", "x"]),
            ("x", &[]),
            ("y", &[]),
            ("z", &[]),
        ]);
        assert_eq!(rows[2], "├─┼─*");
    }

    #[test]
    fn lane_colours_follow_segments_not_columns() {
        let commits = vec![("s", vec!["m"]), ("d", vec!["m"]), ("m", vec![])];
        let rows = rows(&commits);
        let tip = rows[1].cells[2].unwrap();
        assert_eq!(tip, (NODE, 1));
        assert_eq!(rows[2].cells[2], Some(('╯', 1)));
    }
}
