# Named review mode

Shows a named mode, underline cursor, scoped key layer and pushed statusline segment.
[Build and install](../README.md), then run `:plugin.mode.enter`. `h`/`l` move through
native commands; `q` or Escape leaves and removes the segment. Unmapped keys use
the Normal editing policy. Drawing never calls the guest. Disabling the guest
also removes its mode and chrome.
