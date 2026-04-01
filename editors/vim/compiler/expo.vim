" Vim compiler file
" Language: Expo
" Maintainer: Henry Popp

if exists("current_compiler")
  finish
endif
let current_compiler = "expo"

CompilerSet makeprg=expo\ check\ --no-color\ %

" Expo diagnostic output (from expo-driver/src/diagnostics.rs):
"
"   error: use of moved value `p1`
"    --> path/to/file.expo:5:12
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
