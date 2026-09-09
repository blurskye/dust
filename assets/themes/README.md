# Dust themes

Dust is the original, unchanged default palette. The chooser displays friendly
names; `[ui].theme` uses the file stem below. All themes ship in the binary and
work with `--clean`, without downloads or plugins.

| Display name | Configuration name |
| --- | --- |
| Dust | `dust` |
| Catppuccin Latte (light) | `catppuccin-latte` |
| Catppuccin Frappé | `catppuccin-frappe` |
| Catppuccin Macchiato | `catppuccin-macchiato` |
| Catppuccin Mocha | `catppuccin-mocha` |
| Gruvbox Dark | `gruvbox-dark` |
| Nord | `nord` |

These are Dust semantic-role adaptations of the upstream palettes, not imports
of executable editor themes or promises of identical highlighting. Each TOML
file supplies all existing semantic styles and explicit truecolor, xterm-256
and ANSI-16 fallbacks. ANSI fallbacks prioritize foreground/background contrast;
terminal palette customization can still affect the result.

## Palette sources and licenses

- **Catppuccin**, copyright (c) 2021 Catppuccin, MIT:
  [palette.json at 07d02aa110ef9eb7e7427afca5c73ba9cf7f8ebd](https://github.com/catppuccin/palette/blob/07d02aa110ef9eb7e7427afca5c73ba9cf7f8ebd/palette.json).
  [License](../../LICENSES/MIT-catppuccin.txt).
- **Nord**, copyright (c) 2016-present Sven Greb, MIT:
  [nord.css at 1cef71605416a222e57225b544540ce0fcec18d4](https://github.com/nordtheme/nord/blob/1cef71605416a222e57225b544540ce0fcec18d4/src/nord.css).
  [License](../../LICENSES/MIT-nord.txt).
- **Gruvbox**, originally by morhetz; palette taken from gruvbox.nvim,
  copyright (c) 2021 NpX, MIT:
  [gruvbox.lua at 154eb5ff5b96d0641307113fa385eaf0d36d9796](https://github.com/ellisonleao/gruvbox.nvim/blob/154eb5ff5b96d0641307113fa385eaf0d36d9796/lua/gruvbox.lua).
  [License](../../LICENSES/MIT-gruvbox.txt).

Role mapping uses each palette's base/text for editor content, secondary
surfaces for panels/selection, and its red/yellow/green/blue colors for semantic
error/warning/success/information. Catppuccin uses mauve keywords, green strings,
peach numbers and yellow types; Gruvbox and Nord use their corresponding accents.
No upstream runtime or additional dependency is included.
