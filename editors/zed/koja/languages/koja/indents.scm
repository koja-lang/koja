; Auto-indent rules.
;
; Zed's `@indent` capture marks nodes whose children should sit one
; level deeper; `@end` marks the closing token whose line should
; align with the opener.

[
  (block)
  (parameters)
  (argument_list)
  (type_arguments)
  (type_parameters)
  (list)
  (map)
  (struct_construction)
  (enum_construction)
] @indent

[
  "end"
  ")"
  "]"
  "}"
  ">>"
] @end
