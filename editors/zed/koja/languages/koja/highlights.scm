; ============================================================================
; Syntax highlighting for Koja
; ----------------------------------------------------------------------------
; Capture names map to Zed theme tokens. Order matters: later captures
; override earlier ones, so generic captures (e.g. `@variable`) come
; first and specific ones (e.g. `@type`) come last.
; ============================================================================

; ---------------------------------------------------------------------------
; Comments and literals
; ---------------------------------------------------------------------------

(comment) @comment

(integer) @number
(float) @number
(boolean) @boolean

(string) @string
(multiline_string) @string
(escape) @string.escape
(interpolation
  "#{" @punctuation.special
  "}" @punctuation.special)
(format_spec) @string.special

; ---------------------------------------------------------------------------
; Keywords
; ---------------------------------------------------------------------------

[
  "alias"
  "as"
  "const"
  "enum"
  "extend"
  "fn"
  "impl"
  "priv"
  "protocol"
  "struct"
  "type"
] @keyword

[
  "cond"
  "else"
  "for"
  "if"
  "in"
  "loop"
  "match"
  "receive"
  "return"
  "spawn"
  "unless"
  "when"
  "while"
  "after"
] @keyword

(break_statement) @keyword

[
  "move"
] @keyword

[
  "self"
] @variable.builtin

(self_expression) @variable.builtin
(self_type) @type.builtin
(self_parameter) @variable.builtin

; ---------------------------------------------------------------------------
; Operators and punctuation
; ---------------------------------------------------------------------------

[
  "+"
  "-"
  "*"
  "/"
  "%"
  "=="
  "!="
  "<"
  ">"
  "<="
  ">="
  "="
  "+="
  "-="
  "*="
  "/="
  "%="
  "->"
  "<>"
  "|"
  "&"
] @operator

"not" @keyword

; `and` and `or` are aliased to the virtual `operator` node when used
; as boolean operators. Highlight them as keywords by matching the
; node text.
((operator) @keyword
  (#match? @keyword "^(and|or)$"))

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
  "<<"
  ">>"
] @punctuation.bracket

[
  ","
  "."
  ":"
  "@"
] @punctuation.delimiter

"end" @keyword

; ---------------------------------------------------------------------------
; Types
; ---------------------------------------------------------------------------

(type_identifier) @type
(named_type (type_identifier) @type)
(generic_type name: (named_type) @type)
(type_parameter name: (type_identifier) @type.parameter)
(type_alias_declaration name: (type_identifier) @type.definition)

(struct_declaration name: (type_identifier) @type.definition)
(enum_declaration name: (type_identifier) @type.definition)
(protocol_declaration name: (type_identifier) @type.definition)
(enum_variant name: (type_identifier) @constructor)

; Function-pointer type keyword
(function_type "fn" @keyword)

; ---------------------------------------------------------------------------
; Functions
; ---------------------------------------------------------------------------

(function_declaration name: (identifier) @function)
(priv_function name: (identifier) @function)
(extern_function_declaration name: (identifier) @function)
(protocol_method name: (identifier) @function)

(call callee: (identifier) @function.call)
(method_call method: (identifier) @function.method)

; ---------------------------------------------------------------------------
; Variables, parameters, fields
; ---------------------------------------------------------------------------

(parameter name: (identifier) @variable.parameter)
(closure_param name: (identifier) @variable.parameter)
(type_parameter name: (type_identifier) @type.parameter)

(field_access field: (identifier) @property)
(field_init name: (identifier) @property)
(field_pattern name: (identifier) @property)
(struct_field name: (identifier) @property)

(binding_pattern (identifier) @variable)
(typed_binding_pattern name: (identifier) @variable)

(wildcard) @variable.builtin

; ---------------------------------------------------------------------------
; Constants
; ---------------------------------------------------------------------------

(const_declaration name: (identifier) @constant)
(const_declaration name: (type_identifier) @constant)

; All-uppercase identifiers used as expressions are conventional constants
; (e.g. `MAX_SIZE`, `PI`); `type_identifier` covers them via the lexer.
((type_identifier) @constant
  (#match? @constant "^[A-Z][A-Z0-9_]*$"))

; ---------------------------------------------------------------------------
; Constructors and enum patterns
; ---------------------------------------------------------------------------

(constructor_pattern name: (type_identifier) @constructor)
(struct_pattern type: (type_identifier) @type)
(struct_construction type: (named_type) @type)
(enum_construction
  type: (_) @type
  variant: (type_identifier) @constructor)
(enum_pattern variant: (type_identifier) @constructor)

; ---------------------------------------------------------------------------
; Annotations
; ---------------------------------------------------------------------------

(annotation
  "@" @attribute
  name: (identifier) @attribute)
