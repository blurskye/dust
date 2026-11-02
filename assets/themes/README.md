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

Each file has two parts. `[styles]` colours Dust's own surfaces (panels, status
line, explorer) with explicit truecolor, xterm-256 and ANSI-16 fallbacks.
`[highlights]` colours code with Neovim's highlight groups, in the shape
`nvim_set_hl` takes: `"@function" = { fg = "#7aa2f7" }`, `"@lsp.type.property" =
{ link = "@property" }`, with `bold`, `italic`, `underline`, `strikethrough` and
`reverse`. Groups resolve as in Neovim: the theme's definition, then Neovim's
default (`assets/defaults/highlights.toml`), then the parent group, so
`@function.method.call` falls back to `@function.method` and `@function`.
Language-server tokens colour only when their `@lsp.type.*` group has a colour.
The 256 and 16-colour fallbacks for `[highlights]` are the nearest palette colour.

To port a Neovim colorscheme, dump its groups and paste them under `[highlights]`:

    nvim --clean --headless --cmd 'set rtp^=<plugin dir>' \
      -l scripts/nvim-highlights.lua <colorscheme> [dark|light]

The bundled `[highlights]` tables were dumped this way from the plugins below;
Dust runs no Lua.

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

Highlights come from these Neovim plugins, under their own licenses:
[tokyonight.nvim at cdc07ac](https://github.com/folke/tokyonight.nvim/tree/cdc07ac78467a233fd62c493de29a17e0cf2b2b6) (Apache-2.0),
[catppuccin/nvim at edefef7](https://github.com/catppuccin/nvim/tree/edefef779ab08ce1a4a404713e3012b0d202bd35) (MIT, [license](../../LICENSES/MIT-catppuccin.txt)),
[rose-pine/neovim at ff48305](https://github.com/rose-pine/neovim/tree/ff483051a47e27d84bdef47703538df1ed9f4a47) (MIT, [license](../../LICENSES/MIT-rose-pine-nvim.txt)),
[gruvbox.nvim at 154eb5f](https://github.com/ellisonleao/gruvbox.nvim/tree/154eb5ff5b96d0641307113fa385eaf0d36d9796) (MIT, [license](../../LICENSES/MIT-gruvbox.txt)) and
[gbprod/nord.nvim at 87394d4](https://github.com/gbprod/nord.nvim/tree/87394d4fc35c901bbe38326a78d31ab1ead826b6) (WTFPL, [license](../../LICENSES/WTFPL-nord-nvim.txt)).
The Dust theme's `[highlights]` are its own.

Dust plugins do **not yet** have a theme-contribution API. To add a custom theme
today, place a Dust-format TOML file beside user configuration in `themes/`.
Neovim theme plugins don't run directly; dump their groups as above. Palette fetching for these
bundled ports happened during development; opening the chooser makes no network
request and does not install code.
