; From nvim-treesitter/nvim-treesitter
(code_span) @markup.raw

(emphasis) @markup.italic

(strong_emphasis) @markup.strong

(strikethrough) @markup.strikethrough

[
  (backslash_escape)
  (hard_line_break)
] @string.escape

(inline_link
  [
    "["
    "]"
    "("
    (link_destination)
    ")"
  ] @markup.link)

[
  (link_label)
  (link_text)
  (link_title)
  (image_description)
] @markup.link.label

[
  (link_destination)
  (uri_autolink)
  (email_autolink)
] @markup.link.url

(image
  [
    "!"
    "["
    "]"
    "("
    (link_destination)
    ")"
  ] @markup.link)

(full_reference_link
  [
    "["
    "]"
    (link_label)
  ] @markup.link)

(collapsed_reference_link
  [
    "["
    "]"
  ] @markup.link)

(shortcut_link
  [
    "["
    "]"
  ] @markup.link)

[
  (entity_reference)
  (numeric_character_reference)
] @character.special
