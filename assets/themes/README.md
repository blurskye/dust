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
| Tokyo Night | `tokyo-night` |
| Tokyo Night Storm | `tokyo-night-storm` |
| Tokyo Night Moon | `tokyo-night-moon` |
| Tokyo Night Day (light) | `tokyo-night-day` |
| Rosé Pine | `rose-pine` |
| Rosé Pine Moon | `rose-pine-moon` |
| Rosé Pine Dawn (light) | `rose-pine-dawn` |

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

- **Tokyo Night**, folke/tokyonight.nvim contributors, Apache-2.0:
  [generated palette data at cdc07ac78467a233fd62c493de29a17e0cf2b2b6](https://github.com/folke/tokyonight.nvim/tree/cdc07ac78467a233fd62c493de29a17e0cf2b2b6/extras/helix).
  The `[palette]` tables of `tokyonight_{night,storm,moon,day}.toml` are adapted
  to Dust roles; no Lua is executed. [License](../../LICENSES/Apache-2.0.txt).
- **Rosé Pine**, copyright (c) mvllow, MIT:
  [palette.json at 92af52b465ab6e47437aca223c9b8d3009a2023b](https://github.com/rose-pine/rose-pine-palette/blob/92af52b465ab6e47437aca223c9b8d3009a2023b/palette.json).
  [License](../../LICENSES/MIT-rose-pine.txt).

Role mapping uses each palette's base/text for editor content, secondary
surfaces for panels/selection, and its red/yellow/green/blue colors for semantic
error/warning/success/information. Catppuccin uses mauve keywords, green strings,
peach numbers and yellow types; Gruvbox and Nord use their corresponding accents.
Tokyo Night uses its corresponding accents. Rosé Pine uses foam for information,
strings and additions, love for errors, gold for warnings/types, iris for keywords,
and rose for numbers. These are role adaptations, not exact upstream syntax mappings.
No upstream runtime or additional dependency is included.

Dust plugins do **not yet** have a theme-contribution API. To add a custom theme
today, place a Dust-format TOML file beside user configuration in `themes/`.
Existing Neovim theme plugins cannot run directly. Palette fetching for these
bundled ports happened during development; opening the chooser makes no network
request and does not install code.
