# Buffer-word completion

Shows one source supplying both the native menu and inline ghost text.
[Build and install](../README.md), run `:plugin.completion.enable`, then type a prefix
of another word in the buffer. Tab accepts through native completion; Escape and
undo work normally. The source returns at most 128 unique words with UTF-8 byte
replacement ranges, not code-point offsets. No private popup or acceptance keymap.
