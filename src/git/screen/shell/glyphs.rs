//! Every glyph Dusty Git paints, in one table (DESIGN.md §4.3). ASCII fallbacks keep
//! the same cell count as the Unicode glyph wherever a column depends on it.
use crate::config::ExplorerIconMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Glyph {
    Repo,
    Branch,
    Remote,
    Tag,
    CommitHead,
    Commit,
    CommitMerge,
    Stash,
    ChangesPane,
    HistoryPane,
    PullRequest,
    Issue,
    Actions,
    /// A workflow (the Actions panes' titles, §4.3); `Actions` stays the tab's status dot.
    Workflow,
    Notification,
    Cursor,
    Mark,
    Partial,
    FoldClosed,
    FoldOpen,
    Ahead,
    Behind,
    Running,
    RangeEdge,
    ReceiptOk,
    ReceiptErr,
    Warning,
    ClipLeft,
    ClipRight,
    Search,
    // Forge (§4.3): lifecycle, checks, reviews and the reader's metadata icons.
    PullMerged,
    /// The pull request reader's Merge tab when the row is too tight for its name.
    IssueClosed,
    NotPlanned,
    CheckPassed,
    CheckFailed,
    CheckPending,
    CheckSkipped,
    CheckUnknown,
    ReviewRequested,
    /// An approving review: not the CI pass mark, so the two never read alike (r11 GL-08).
    Approved,
    /// A review requesting changes: not the CI failure mark (DESIGN.md §4.3.1, r15 gitlab-08).
    ChangesRequested,
    /// A commit on a forge screen (Commits' day heading): not an open issue's `○` (§4.3.1).
    ForgeCommit,
    Comment,
    Person,
    Milestone,
    Link,
    Dot,
    Unchecked,
    Checked,
    File,
}

/// The Nerd Font check (nf-md-check). Nerd fonts built on Fira Code draw U+2713 as a
/// shallow tick that reads as a square root (√), so check marks use the font's own icon.
const CHECK: &str = "\u{f012c}";
/// U+2714 in text presentation: U+2713 is a hairline in most monospace fonts and reads as
/// `√` beside bold text (DESIGN.md §4.3 status glyphs).
const HEAVY_CHECK: &str = "\u{2714}";

pub(crate) fn glyph(glyph: Glyph, mode: ExplorerIconMode) -> &'static str {
    use ExplorerIconMode::{Ascii, NerdFont, Unicode};
    use Glyph::*;
    match (glyph, mode) {
        (Repo, NerdFont) => "󰊢",
        (Repo, Unicode) => "◆",
        (Repo, Ascii) => "#",
        (Branch, NerdFont) => "󰘬",
        (Branch, Unicode) => "⎇",
        (Branch, Ascii) => "Y",
        (Remote, NerdFont) => "󰒍",
        (Remote, Unicode) => "⇅",
        (Remote, Ascii) => "@",
        (Tag, NerdFont) => "󰓹",
        (Tag, Unicode) => "◇",
        (Tag, Ascii) => "t",
        (CommitHead, Ascii) => "*",
        (CommitHead, _) => "●",
        (Commit, Ascii) => "o",
        (Commit, _) => "○",
        (CommitMerge, Ascii) => "M",
        (CommitMerge, _) => "◆",
        (Stash, NerdFont) => "󰆓",
        (Stash, Unicode) => "≡",
        (Stash, Ascii) => "=",
        (ChangesPane, NerdFont) => "󰊢",
        (ChangesPane, Unicode) => "±",
        (ChangesPane, Ascii) => "+",
        (HistoryPane, NerdFont) => "󰜘",
        (HistoryPane, Unicode) => "⌁",
        (HistoryPane, Ascii) => "~",
        (PullRequest, NerdFont) => "",
        (PullRequest, Unicode) => "⎇",
        (PullRequest, Ascii) => "P",
        (Issue, Ascii) => "o",
        (Issue, _) => "○",
        // GitHub's play-in-a-circle: a `●` here read as one more running dot beside the tab's
        // own `●` running mark (r16 notif-actions-05).
        (Actions, NerdFont) => "\u{f144}",
        (Actions, Ascii) => "*",
        (Actions, _) => "●",
        (Workflow, NerdFont) => "󰑮",
        (Workflow, Unicode) => "▷",
        (Workflow, Ascii) => "*",
        (Notification, NerdFont) => "󰂚",
        (Notification, Unicode) => "●",
        (Notification, Ascii) => "!",
        (Cursor, Ascii) => ">",
        // A focus edge, not an arrow: it sits under the native bar cursor.
        (Cursor, _) => "▎",
        (Mark, Ascii) => "*",
        (Mark, _) => "✓",
        (Partial, Ascii) => "%",
        (Partial, _) => "◐",
        (FoldClosed, Ascii) => ">",
        (FoldClosed, _) => "▸",
        (FoldOpen, Ascii) => "v",
        (FoldOpen, _) => "▾",
        (Ahead, Ascii) => "^",
        (Ahead, _) => "↑",
        (Behind, Ascii) => "v",
        (Behind, _) => "↓",
        (Running, Ascii) => "~",
        (Running, _) => "⟳",
        (RangeEdge, Ascii) => "|",
        (RangeEdge, _) => "▎",
        (ReceiptOk, Ascii) => "+",
        (ReceiptOk, NerdFont) => CHECK,
        (ReceiptOk, _) => HEAVY_CHECK,
        (ReceiptErr, Ascii) => "x",
        (ReceiptErr, _) => "✕",
        (Warning, Ascii) => "!",
        (Warning, _) => "⚠",
        (ClipLeft, Ascii) => "<",
        (ClipLeft, _) => "‹",
        (ClipRight, Ascii) => ">",
        (ClipRight, _) => "›",
        (Search, NerdFont) => "󰍉",
        (Search, _) => "/",
        (PullMerged, NerdFont) => "󰘭",
        (PullMerged, Unicode) => "⎇",
        (PullMerged, Ascii) => "P",
        (IssueClosed, Ascii) => "x",
        (IssueClosed, NerdFont) => CHECK,
        (IssueClosed, _) => HEAVY_CHECK,
        (NotPlanned, Ascii) => "-",
        (NotPlanned, _) => "⊘",
        (CheckPassed, Ascii) => "+",
        (CheckPassed, NerdFont) => CHECK,
        (CheckPassed, _) => HEAVY_CHECK,
        (CheckFailed, Ascii) => "x",
        (CheckFailed, _) => "✕",
        (CheckPending, Ascii) => ".",
        // One clock everywhere a check waits or a review is still owed.
        (CheckPending, NerdFont) => "\u{f0150}",
        (CheckPending, _) => "◷",
        (CheckSkipped, Ascii) => "-",
        (CheckSkipped, _) => "⊘",
        // No checks reported: a dash, not a question mark in the icon column (r11).
        (CheckUnknown, Ascii) => "-",
        (CheckUnknown, _) => "–",
        (ReviewRequested, Ascii) => "o",
        (ReviewRequested, _) => "◌",
        (Approved, Ascii) => "v",
        (Approved, NerdFont) => "\u{f0513}",
        (Approved, _) => "✓",
        (ChangesRequested, Ascii) => "~",
        (ChangesRequested, _) => "✎",
        (ForgeCommit, Ascii) => "c",
        (ForgeCommit, _) => "◦",
        (Comment, NerdFont) => "󰆈",
        (Comment, Unicode) => "❝",
        (Comment, Ascii) => "\"",
        (Person, NerdFont) => "󰀄",
        (Person, Unicode) => "☺",
        (Person, Ascii) => "@",
        (Milestone, NerdFont) => "",
        (Milestone, Unicode) => "◈",
        (Milestone, Ascii) => "m",
        (Link, Ascii) => ">",
        (Link, _) => "↗",
        (Dot, Ascii) => "*",
        (Dot, _) => "●",
        (Unchecked, Ascii) => "o",
        (Unchecked, _) => "☐",
        (Checked, Ascii) => "x",
        (Checked, _) => "☑",
        (File, NerdFont) => "󰈙",
        (File, Unicode) => "◇",
        (File, Ascii) => "-",
    }
}

/// Graph lane glyphs; ASCII substitutes cell for cell.
pub(crate) fn lane(symbol: char, mode: ExplorerIconMode) -> &'static str {
    let ascii = mode == ExplorerIconMode::Ascii;
    match symbol {
        '│' if ascii => "|",
        '│' => "│",
        '─' if ascii => "-",
        '─' => "─",
        '╮' if ascii => "\\",
        '╮' => "╮",
        '╯' if ascii => "/",
        '╯' => "╯",
        '╭' if ascii => "/",
        '╭' => "╭",
        '╰' if ascii => "\\",
        '╰' => "╰",
        '┼' if ascii => "+",
        '┼' => "┼",
        '…' if ascii => ".",
        '…' => "…",
        _ => " ",
    }
}

/// A file-type glyph and the highlight group that colours it.
/// Nerd Font `nf-md-file`: the file glyph for paths the icon table does not know.
pub(crate) const GENERIC_FILE: &str = "\u{f0214}";

pub(crate) fn file(
    path: &std::path::Path,
    mode: ExplorerIconMode,
) -> (&'static str, crate::config::Highlight) {
    use crate::config::Highlight;
    use crate::files::ExplorerIconColor;
    let icon = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(crate::files::file_icon);
    let role = match icon.map(|icon| icon.color) {
        Some(ExplorerIconColor::Azure | ExplorerIconColor::Blue | ExplorerIconColor::Cyan) => {
            Highlight::FUNCTION
        }
        Some(ExplorerIconColor::Green) => Highlight::STRING,
        Some(ExplorerIconColor::Orange | ExplorerIconColor::Yellow) => Highlight::NUMBER,
        Some(ExplorerIconColor::Purple | ExplorerIconColor::Red) => Highlight::KEYWORD,
        _ => Highlight::TYPE,
    };
    let glyph = match mode {
        // A path with no mapping (`notes`, `data.xyz`) gets a plain file glyph, so the icon
        // column is never an empty slot in a list where every sibling has one.
        ExplorerIconMode::NerdFont => icon.map_or(GENERIC_FILE, |icon| icon.glyph),
        ExplorerIconMode::Unicode => "◇",
        ExplorerIconMode::Ascii => "-",
    };
    (glyph, role)
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn every_file_gets_a_glyph_and_toml_has_its_own() {
        let nerd = ExplorerIconMode::NerdFont;
        let toml = file(std::path::Path::new("pyproject.toml"), nerd).0;
        assert!(!toml.is_empty() && toml != GENERIC_FILE, "{toml:?}");
        assert_eq!(toml, file(std::path::Path::new("Cargo.toml"), nerd).0);
        for path in ["notes", "data.unknownext", "dir/.envrc", "Makefile.am"] {
            let glyph = file(std::path::Path::new(path), nerd).0;
            assert!(!glyph.trim().is_empty(), "{path}: {glyph:?}");
        }
        assert_eq!(file(std::path::Path::new("notes"), nerd).0, GENERIC_FILE);
    }
}
