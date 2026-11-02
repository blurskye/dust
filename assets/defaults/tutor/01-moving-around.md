# 1. Moving around

Dust is a modal editor like Vim. You start in Normal mode, where keys
move and act. You type text in Insert mode. `esc` always brings you back
to Normal mode. The mode is shown at the left of the status line.

    h  left      j  down      k  up      l  right

Try it: move the cursor onto the X in the line below.

    The X marks the spot.

Bigger moves:

    w  next word         b  previous word      e  end of word
    0  line start        ^  first non-blank    $  line end
    gg first line        G  last line
    ctrl-d / ctrl-u  half page down / up
    %  matching bracket
    ctrl-o / tab     back / forward through the places you jumped from

Most moves take a count: `3w` moves three words, `5j` five lines.

Jump anywhere on screen with flash: `s`, two letters, then the label.

    You can reach this flash word in three keys.

On a line, `f` and `t` jump to a character; `;` and `,` repeat.

    First part, second part.

`%` jumps to the matching bracket.

    call(first, (second))
