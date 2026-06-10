" Vim compiler file
" Language: Koja
" Maintainer: Henry Popp

if exists("current_compiler")
  finish
endif
let current_compiler = "koja"

CompilerSet makeprg=koja\ check\ --no-color\ %

" Koja diagnostic output (from koja-driver/src/diagnostics.rs):
"
"   error: type mismatch: expected `Int`, found `String`
"    --> path/to/file.koja:5:12
"     |
"   5 | some source line
"     |     ^^^
"
" Lines 1-2 carry the useful data; the rest is context for humans.
CompilerSet errorformat=
      \%Eerror:\ %m,
      \%Wwarning:\ %m,
      \%Cnote:\ %m,
      \%C\ %#-->\ %f:%l:%c,
      \%-G%.%#
