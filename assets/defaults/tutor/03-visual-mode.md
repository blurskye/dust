# 3. Visual mode, registers, macros

`v` selects characters, `V` whole lines, `ctrl-v` a block. Move to grow
the selection, then act on it: `d` delete, `c` change, `y` copy, `>`
indent, `gc` comment, `U` upper case. `o` jumps to the other end.

    alpha
    beta
    gamma

`"ayy` copies the line into register a, `"ap` pastes it. Copies also go
to the system clipboard.

Macros: `qa` records into register a, `q` stops, `@a` replays, `3@a`
replays three times.

    first
    second
    third
    fourth
