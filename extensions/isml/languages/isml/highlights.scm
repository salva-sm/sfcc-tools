(doctype) @constant
(entity) @constant

(tag_name) @tag
(erroneous_end_tag_name) @tag

; ISML tags are control flow rather than markup, so they get the keyword colour.
; Listed after the generic rule on purpose: the later pattern wins.
((tag_name) @keyword
  (#match? @keyword "^[iI][sS][a-zA-Z]"))

(attribute_name) @attribute
(attribute_value) @string

[
  "\""
  "'"
] @string

(comment) @comment
(iscomment_element (raw_text) @comment)

[
  "<"
  ">"
  "</"
  "/>"
] @punctuation.bracket

[
  "${"
  "}"
] @punctuation.special

"=" @operator
