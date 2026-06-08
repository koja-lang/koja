" Vim filetype plugin
" Language: Koja
" Maintainer: Henry Popp

if exists("b:did_ftplugin")
  finish
endif
let b:did_ftplugin = 1

setlocal expandtab
setlocal tabstop=2
setlocal shiftwidth=2
setlocal softtabstop=2
setlocal commentstring=#\ %s
setlocal comments=:#
setlocal suffixesadd=.koja,.kojs

compiler koja

" matchit support — jump between block delimiters with %
if !exists("loaded_matchit")
  silent! packadd matchit
endif
if exists("loaded_matchit")
  let b:match_words =
        \ '\<fn\>:\<end\>,'
        \ . '\<struct\>:\<end\>,'
        \ . '\<enum\>:\<end\>,'
        \ . '\<impl\>:\<end\>,'
        \ . '\<extend\>:\<end\>,'
        \ . '\<protocol\>:\<end\>,'
        \ . '\<if\>:\<else\>:\<end\>,'
        \ . '\<unless\>:\<end\>,'
        \ . '\<match\>:\<end\>,'
        \ . '\<cond\>:\<end\>,'
        \ . '\<for\>:\<end\>,'
        \ . '\<loop\>:\<end\>,'
        \ . '\<while\>:\<end\>,'
        \ . '\<receive\>:\<after\>:\<end\>'
  let b:match_skip = 's:comment\|string'
endif

let b:undo_ftplugin = "setlocal expandtab< tabstop< shiftwidth< softtabstop<"
      \ . " commentstring< comments< suffixesadd<"
      \ . " | unlet! b:match_words b:match_skip"
