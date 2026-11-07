# Editable directory

Shows editable documents, the files grant, ordinary select confirmation and native
saving. [Build and install](../README.md), then run `:plugin.oil.open` in a workspace.
Each line is `ID<TAB>name`. Keep the ID while renaming, delete a line to delete an
entry, or add a bare filename to create it (a trailing `/` creates a directory).
Save with Ctrl-S and choose Apply changes. Cancel leaves the buffer dirty.

Only immediate workspace children are listed. Renames cannot overwrite; deleting
a directory is nonrecursive. Moving names onto existing names requires an
intermediate save. File operations are sequential, not a filesystem transaction:
a failure is visible and keeps the document dirty; reopen the listing after a
partial failure. Native dirty-file checks and files-grant path checks still apply.
