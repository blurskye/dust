# Two ordered wrappers

[Build and install](../README.md) both `wrappersa` and `wrappersb`. Run
`:plugin.wrappersb.enable`, then `:plugin.wrappersa.enable`. Type in Insert mode.
Notification history (`<leader>n`) shows A-before, B-before, B-after, A-after for
each insertion, regardless of registration order. Both call the same native
`edit.insert_text`; later typed keys wait until the chain completes.
Undo, dot and macros record the completed semantic command, not the notifications.
