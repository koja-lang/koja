; Bracket pairs for matching, autoclose, and rainbow.

("(" @open ")" @close)
("[" @open "]" @close)
("{" @open "}" @close)
("<<" @open ">>" @close)
("\"" @open "\"" @close)

; Block-like constructs. The `end` keyword closes any of these in Expo,
; so we list every opening keyword alongside the literal `"end"`.

("struct" @open "end" @close)
("enum" @open "end" @close)
("impl" @open "end" @close)
("extend" @open "end" @close)
("protocol" @open "end" @close)
("fn" @open "end" @close)
("if" @open "end" @close)
("unless" @open "end" @close)
("match" @open "end" @close)
("cond" @open "end" @close)
("for" @open "end" @close)
("while" @open "end" @close)
("loop" @open "end" @close)
("receive" @open "end" @close)
