; Language injections.
;
; Currently we only inject for `@doc` annotations whose value is a
; multiline string — those are conventionally Markdown documentation.

(annotation
  name: (identifier) @_name
  value: (multiline_string) @injection.content
  (#eq? @_name "doc")
  (#set! injection.language "markdown"))
