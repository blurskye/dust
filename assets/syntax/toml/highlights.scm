; Properties
;-----------

; Upstream captures the whole pair, which painted every value in the key's colour.
; Dust captures the keys alone, so strings, numbers and booleans keep their own.
(table
  [(bare_key) (quoted_key)] @keyword)

(table
  (dotted_key
    [(bare_key) (quoted_key)] @keyword))

(table_array_element
  [(bare_key) (quoted_key)] @keyword)

(table_array_element
  (dotted_key
    [(bare_key) (quoted_key)] @keyword))

(pair
  [(bare_key) (quoted_key)] @type)

(pair
  (dotted_key
    [(bare_key) (quoted_key)] @type))

; Literals
;---------

(boolean) @boolean

(comment) @comment

(string) @string

[
  (integer)
  (float)
] @number

[
  (offset_date_time)
  (local_date_time)
  (local_date)
  (local_time)
] @string.special

; Punctuation
;------------

[
  "."
  ","
] @punctuation.delimiter

"=" @operator

[
  "["
  "]"
  "[["
  "]]"
  "{"
  "}"
] @punctuation.bracket
