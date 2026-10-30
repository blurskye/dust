# Dust tutorial

The card at the bottom right is your teacher. It gives you one thing to try at a time,
puts the cursor on the text to practise on, and moves on by itself once you've done it.
`:TutorNext` skips a step, `:TutorBack` goes back, `:TutorQuit` stops, and `:Tutor` picks up
where you left. This file is a copy made for you: edit it, break it. It also works as a
reference: read it with `j` and `k`, or jump between chapters with `/## ` and `n`.

`<leader>` is the space bar. When you press it and wait, a hint box lists what can follow,
so you never need to remember a whole key sequence: press `space`, read, continue.

Chapters

 1. Moving around
 2. Changing text
 3. Visual mode, registers, macros
 4. Search and replace
 5. Files: finder, explorer, recents
 6. Buffers, tabs, windows
 7. The terminal
 8. Code: LSP, completion, diagnostics, formatting
 9. Tools: language servers, formatters, debuggers
10. Debugging
11. Git: signs, hunks, lazygit, worktrees, pull requests
12. The agent panel
13. Projects, sessions and recovery
14. Look and feel
15. Plugins and configuration
16. Getting help

---

## 1. Moving around

Dust is a modal editor like Vim. You start in Normal mode, where keys move and act. You
type text in Insert mode. `esc` always brings you back to Normal mode. The mode is shown
at the left of the status line.

    h  left      j  down      k  up      l  right

Try it: move the cursor onto the X in the line below with `h j k l`.

    The X marks the spot.

Bigger moves:

    w  next word         b  previous word      e  end of word
    0  line start        ^  first non-blank    $  line end
    gg first line        G  last line
    ctrl-d / ctrl-u  half page down / up
    ctrl-f / ctrl-b  page down / up
    %  matching bracket
    ctrl-o / tab     back / forward through the places you jumped from

Try it: put the cursor on the first `(` and press `%`.

    call(first, (second))

Most moves take a count: `3w` moves three words, `5j` five lines.

Jump anywhere on screen with flash: press `s`, type two letters of where you want to go,
then the label that appears there.

Try it: put the cursor here, press `s`, type `fl` and the label next to "flash".

    You can reach this flash word in three keys.

On a line, `f` and `t` jump to a character: `fa` goes to the next "a", `ta` just before it.
`;` repeats the jump, `,` repeats it backwards.

Try it: press `f,` to jump to the comma.

    First part, second part.

`<leader>sj` lists the places you jumped from, `<leader>sm` the marks, and `<leader>fr`
the files you had open recently.

## 2. Changing text

Enter Insert mode with:

    i  before the cursor    a  after the cursor
    I  at line start        A  at line end
    o  new line below       O  new line above

Type, then press `esc`.

Try it: delete the extra letters with `x`.

    Thhe cow jumpped over thee moon.

Try it: add the missing word to the line below so it reads "Dust is a fast editor".

    Dust is a editor

Try it: finish this line with `A`.

    Dust runs in a

Try it: open a new line with `o`.

    Open a line below this one.

Deleting and changing work as operator + motion:

    x    delete a character        dw   delete a word
    dd   delete the line           d$   delete to line end (also D)
    cw   change a word             cc   change the line
    ciw  change the word you are in
    ci"  change inside quotes      da(  delete around parentheses
    dif  delete a function's body  cia  change an argument
    r    replace one character     R    replace mode
    J    join with the next line   ~    toggle case
    >>   indent                    <<   unindent
    .    repeat the last change
    u    undo                      ctrl-r  redo

Try it: delete the repeated words.

    This line has has some some repeated repeated words.

Try it: delete this line with `dd`, bring it back with `u`, delete it again with `ctrl-r`.

    DELETE THIS WHOLE LINE

Try it: change "red" to "green" with `ciw`.

    This is a red apple.

Try it: with the cursor inside the quotes, press `ci"` and type a new greeting.

    let greeting = "hello there";

Try it: fix the first letter of "Dusk" with `r`, and join the two lines after it with `J`.

    Dusk is fast.
    one line,
    joined.

Try it: copy this line with `yy` and paste it with `p`.

    copy me

Comments and surrounds:

    gcc        toggle the comment on this line
    gc3j       comment this line and three below
    gsa iw "   surround a word with quotes
    gsd "      delete surrounding quotes
    gsr " '    replace " with '

Try it: comment this line out with `gcc`.

    comment me

Try it: surround the word `value` with parentheses (`gsaiw)`), then remove them (`gsd)`).

    total = value * 2

Copy and paste: `yy` copies a line, `yw` a word, `p` pastes after, `P` before.

## 3. Visual mode, registers, macros

`v` selects characters, `V` whole lines, `ctrl-v` a block. Move to grow the selection,
then act on it: `d` delete, `c` change, `y` copy, `p` paste over, `>` indent, `gc` comment,
`J` join, `U` upper case, `u` lower case, `~` toggle case. `o` jumps to the other end.

Try it: select these three lines with `V` and `2j`, then indent them with `>`.

    alpha
    beta
    gamma

Copies also go to the system clipboard, and `p` pastes what other programs copied.
Registers keep more: prefix a copy or paste with `"` and a letter. `"ayy` copies the line
into register a, `"ap` pastes it.

Macros: `qa` records into register a, `q` stops, `@a` replays, `@@` replays the last one,
`10@a` replays ten times.

Try it: record a macro that turns `item` into `- item` and moves down, then replay it.
`qa` `I- ` `esc` `j` `q`, then `3@a`.

    first
    second
    third
    fourth

## 4. Search and replace

In this file:

    /word     search forward          ?word   search backward
    n  N      next / previous match   *  #    search the word under the cursor

Try it: change every "cat" below to "dog": `/cat` `enter`, `cw` `dog` `esc`, then `n` `.`
to jump to the next one and repeat the change.

    The cat sat. The cat ran. Every cat naps.

Across the project:

    <leader>/  or  <leader>sg   search every file (ripgrep); results open in a list
    <leader>sr                  search and replace across files, with a preview
    <leader>st                  list TODO, FIXME and HACK comments

In any list: type to filter, `up`/`down` to move, `enter` to open,
`esc` to switch to Normal mode in the list (then `j`/`k`), `esc` again to close.
`ctrl-q` sends the results to a list you can keep open.

## 5. Files: finder, explorer, recents

    <leader>space  or  <leader>ff   find a file by name (fuzzy)
    <leader>fr                      recent files
    <leader>fn                      new empty file
    :e path                         open a path
    :w                              save, :wq save and quit, :q quit

The file tree:

    <leader>e    show or hide the explorer

In the explorer:

    j k          move              l  or enter   open a file, expand a folder
    h            collapse / go to the parent
    a            add a file (end the name with / for a folder)
    r            rename            d  delete      y  copy the path
    c  m  p      copy, move, paste
    s            open in a split
    .            show hidden files  I  show ignored files
    R            refresh            esc  back to the editor
    /            search the whole tree from the bar at the top: type, enter keeps
                 the matches for j k and enter, esc clears the search

`ctrl-h` and `ctrl-l` move between the explorer and the editor, like any window.

## 6. Buffers, tabs, windows

Every open file is a buffer, shown as a tab on the line at the top.

    ctrl-t        new tab: an empty buffer with the file finder open
    ctrl-q        close this tab (asks if there are unsaved changes)
    H  L          previous / next tab         [b  ]b  the same
    <leader>,     pick an open buffer
    <leader>bd    close the buffer            <leader>bp  pin it
    :bd  :bn  :bp   the Vim forms

Only one empty tab at a time: `ctrl-t` reuses an empty one instead of piling them up.

Windows split the screen:

    <leader>|  or  ctrl-w v     split side by side
    <leader>-  or  ctrl-w s     split top and bottom
    ctrl-h ctrl-j ctrl-k ctrl-l move between windows (and the explorer, terminal, agent)
    ctrl-arrows                 resize
    <leader>wd  or  ctrl-w q    close the window
    <leader>wm                  zoom the window to full screen and back

Workspace tabs hold whole layouts: `<leader><tab><tab>` new, `<leader><tab>]` and
`<leader><tab>[` next and previous, `<leader><tab>d` close, `gt` and `gT` cycle.

## 7. The terminal

    ctrl-/        open or hide the terminal at the bottom

In the terminal everything goes to the shell. `esc esc` switches to Normal mode, where you
can scroll with `j`/`k`, `gg`/`G`, select with `v` and copy with `y`; `i` goes back to
typing. `ctrl-shift-h` and `ctrl-shift-l` cycle through terminals. `ctrl-k` leaves it for
the window above.

`:taskrun`, `:test` and `:tasks` run the project's tasks and tests and show their output.

## 8. Code: LSP, completion, diagnostics, formatting

Open a source file in a project and its language server starts on its own (once you trust
the workspace and the server is installed, see chapter 9).

    K            documentation for the symbol under the cursor
    gd           go to definition          gD  declaration
    gr           references                gI  implementation
    gy           type definition           gK  signature help
    <leader>ss   symbols in this file      <leader>sS  symbols in the project
    <leader>ca   code actions (quick fixes, refactors)
    <leader>cf   format the file           <leader>cl  language server status
    <leader>uh   inlay hints on / off

Completion pops up while you type in Insert mode. `tab`/`shift-tab` or the arrows move,
`enter` accepts, `esc` closes it, `ctrl-space` asks for it.

Diagnostics show at the end of lines and in the sign column:

    ]d  [d       next / previous diagnostic
    ]e  [e       errors only               ]w  [w   warnings only
    <leader>xx   all problems in a list

## 9. Tools: language servers, formatters, debuggers

    <leader>cm   the tool manager (like Mason)

It lists language servers, formatters, linters and debug adapters by category (`tab`
switches). `i` installs, `u` uninstalls, `R` restarts a running server, `x` stops it,
`enter` shows details. `:health` checks that everything Dust needs is in place.

## 10. Debugging

    <leader>db   toggle a breakpoint on this line   <leader>dB  conditional breakpoint
    <leader>ds   start debugging                    <leader>dc  continue
    <leader>dO   step over    <leader>di  step into  <leader>do  step out
    <leader>dp   pause        <leader>dt  stop       <leader>dR  restart
    <leader>dr   the debug console                   <leader>dl  all breakpoints

While paused, the current line is marked and the debug console shows the stack and
variables.

## 11. Git: signs, hunks, lazygit, worktrees, pull requests

Changed lines are marked in the sign column.

    ]h  [h       next / previous change (hunk)
    <leader>ghp  preview the hunk    <leader>ghs  stage it
    <leader>ghu  unstage it          <leader>ghr  reset it
    <leader>gs   git status: `s` stages, `u` unstages, `tab` switches staged / unstaged
    <leader>gb   blame this line
    <leader>gB   open the file on the forge   <leader>gY  copy that link

lazygit does the rest, in a window on top of everything:

    <leader>gg   lazygit for the project    <leader>gf  history of this file
    <leader>gl   the log

Pressing `e` on a file in lazygit opens it in this Dust.

    <leader>gw   worktrees: list, create, switch, remove
    <leader>gx   merge conflicts, side by side
    <leader>gpp  pull requests (gh or tea)   <leader>gpv  view one   <leader>gpd  its diff
    <leader>gpr  review it                 <leader>gpc  its comments   <leader>gpa  comment

## 12. The agent panel

Dust runs coding agents (Claude, Codex, OpenCode, pi, Gemini) over ACP in a panel beside
your code.

    <leader>an   start an agent (pick which)    <leader>at  show or hide the panel
    <leader>aa   ask the agent, with the selection if there is one
    <leader>af   add this file to the next question
    <leader>ad   add this file's diagnostics

In the panel you type in an input box. It has Vim modes too: `esc` goes to Normal mode
(`h l w b e 0 $ x dd D cc A I o`, `j`/`k` and `gg`/`G` scroll, `i` back to typing).
`esc` in Normal mode stops a running turn.

    enter         send                     shift-enter  new line
    /             slash commands (with tab to complete)
    ctrl-t        a new tab: pick its agent       ctrl-q  close the tab
    alt-]  alt-[  next / previous agent tab (H and L in Normal mode)
    ctrl-p        model, or another agent   ctrl-o  mode and options    shift-tab  thinking level
    ctrl-r        resume an earlier conversation (they are kept by Dust)
    ctrl-h        back to the code

Agents can start subagents through Dust's own `agent` tool. Each subagent gets its own tab,
and the main tab lists them with their state. For pi, add the tool once with
`pi mcp add dust --exposure direct -- dust --agent-tools`.

## 13. Projects, sessions and recovery

A folder you open is a workspace. Dust asks once whether to trust it before running its
language servers, tasks or debuggers: `:trust` and `:untrust` change that.

    <leader>fp   project folders in this workspace   :rootadd path  add one
    :sessionsave / :sessionload   save or load the open files and layout

When you quit, Dust remembers the open files and puts them back next time. If Dust or your
machine crashes, unsaved changes are kept in a recovery journal: `:recovery` shows them and
`:recover` brings the latest back.

## 14. Look and feel

    <leader>uC   pick a theme (live preview)
    <leader>uw   wrap long lines      <leader>ul  line numbers   <leader>uL  relative numbers
    <leader>ug   indent guides        <leader>uG  git signs
    <leader>uz   zen mode             <leader>n   notification history

## 15. Plugins and configuration

Your settings live in `~/.config/dust/config.toml` and your keys in
`~/.config/dust/keymaps.toml`. They only need what you change: everything else comes
from Dust's defaults. Dust reloads them when they change.

`:plugins` lists installed plugins, `:pluginvalidate` checks one, `:plugindisable` turns
one off. Plugins are WebAssembly components and run sandboxed.

## 16. Getting help

    <leader>?    what you can do right here, with the keys
    F1           every command by name with its key, searchable (also <leader>:)
    <leader>sC   the same
    :health      what works and what is missing
    :messages    earlier notifications

That's the whole tour. Close this tab with `ctrl-q`, and come back with `:Tutor` any time.
