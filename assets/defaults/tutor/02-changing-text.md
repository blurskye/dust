# 2. Changing text

Enter Insert mode, type, then press `esc`:

    i  before the cursor    a  after the cursor
    I  at line start        A  at line end
    o  new line below       O  new line above

Operators work with a motion or a text object:

    x    delete a character        dw   delete a word
    dd   delete the line           D    delete to line end
    cw   change a word             ciw  change the word you are in
    ci"  change inside quotes      r    replace one character
    J    join with the next line   .    repeat the last change
    u    undo                      ctrl-r  redo
    yy   copy the line             p    paste after
    gcc  toggle a comment          gsa  add a surround

Practice lines:

    Thhe cow jumpped over thee moon.

    Dust is a editor

    Dust runs in a

    Open a line below this one.

    This line has has some some repeated repeated words.

    DELETE THIS WHOLE LINE

    This is a red apple.

    let greeting = "hello there";

    Dusk is fast.
    one line,
    joined.

    copy me

    comment me

    total = value * 2
