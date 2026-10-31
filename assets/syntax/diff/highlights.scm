; From tree-sitter-diff 0.1.0 (queries/highlights.scm).
; These scopes are arbitrary and line up with good colors for the
; `tree-sitter highlight` command. Adapt them as you see fit.

[(addition) (new_file)] @string
[(deletion) (old_file)] @keyword

(commit) @constant
(location) @attribute
(command) @variable.builtin
