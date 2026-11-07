# Autopairs

Shows pre-insert wrappers, checked native edits, cursor placement and shared undo.
[Build and install](../README.md), disable built-in `editor.autopairs`, then run
`:plugin.autopairs.enable`. In Insert mode try `(x)`, quotes, and Backspace inside
`[]`. Escape, `u`, and `.` exercise native undo and repeat; macros use the same keys.
The guest intentionally pairs quotes even inside words: it is an editing API
example, not a language-specific quote heuristic.
