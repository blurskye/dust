-- Prints a Neovim colorscheme's syntax highlight groups as a Dust theme `[highlights]` table:
--   nvim --clean --headless --cmd 'set rtp^=<plugin dir>' -l scripts/nvim-highlights.lua <colorscheme> [dark|light]
-- With no colorscheme it prints Neovim's default links, the base of assets/defaults/highlights.toml.
local legacy = {}
for name in ([[Comment Constant String Character Number Boolean Float Identifier Function Statement
  Conditional Repeat Label Operator Keyword Exception PreProc Include Define Macro PreCondit Type
  StorageClass Structure Typedef Special SpecialChar Tag Delimiter SpecialComment Debug Underlined
  Ignore Error Todo Added Changed Removed Title DiffAdd DiffChange DiffDelete DiffText
  DiagnosticError DiagnosticWarn DiagnosticInfo DiagnosticHint DiagnosticOk
  DiagnosticDeprecated DiagnosticUnnecessary]]):gmatch('%S+') do
  legacy[name] = true
end
vim.o.termguicolors = true
local scheme, background = arg[1], arg[2]
if background then
  vim.o.background = background
end
if scheme then
  vim.cmd.colorscheme(scheme)
end
local groups = vim.api.nvim_get_hl(0, {})
-- Code groups, plus whatever they link to, such as gruvbox's `GruvboxFg1`.
local wanted = {}
local function want(name)
  if not wanted[name] and groups[name] then
    wanted[name] = true
    if groups[name].link then
      want(groups[name].link)
    end
  end
end
for name in pairs(groups) do
  if name:sub(1, 1) == '@' or legacy[name] then
    want(name)
  end
end
local names = vim.tbl_keys(wanted)
table.sort(names)
local lines = {}
for _, name in ipairs(names) do
  local group, fields = groups[name], {}
  if group.link then
    table.insert(fields, ('link = "%s"'):format(group.link))
  else
    for _, key in ipairs({ 'fg', 'bg' }) do
      -- Normal's background is the editor's; Dust takes it from `[styles]`.
      if group[key] and scheme and not (name == 'Normal' and key == 'bg') then
        table.insert(fields, ('%s = "#%06x"'):format(key, group[key]))
      end
    end
    for _, key in ipairs({ 'bold', 'italic', 'underline', 'undercurl', 'strikethrough', 'reverse' }) do
      if group[key] then
        table.insert(fields, key .. ' = true')
      end
    end
  end
  table.insert(lines, ('"%s" = { %s }'):format(name, table.concat(fields, ', ')))
end
io.write(table.concat(lines, '\n'), '\n')
