((expression_content) @injection.content
  (#set! injection.language "javascript"))

(isscript_element
  (raw_text) @injection.content
  (#set! injection.language "javascript"))

(script_element
  (raw_text) @injection.content
  (#set! injection.language "javascript"))

(style_element
  (raw_text) @injection.content
  (#set! injection.language "css"))
