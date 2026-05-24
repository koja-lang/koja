" Vim indent file
" Language: Koja
" Maintainer: Henry Popp

if exists("b:did_indent")
  finish
endif
let b:did_indent = 1

setlocal indentexpr=GetKojaIndent()
setlocal indentkeys=0{,0},!^F,o,O,e
setlocal indentkeys+=0=end,0=else,0=after,0=when

setlocal autoindent

let b:undo_indent = "setlocal indentexpr< indentkeys< autoindent<"

if exists("*GetKojaIndent")
  finish
endif

let s:block_openers = '\v^\s*(fn|priv\s+fn|struct|enum|impl|extend|protocol|if|unless|match|cond|for|loop|while|receive|after|else|when)\>'
let s:arrow_end     = '\v\-\>\s*(#.*)?$'
let s:dedent_kws    = '\v^\s*(end|else|after|when)\>'
let s:single_line   = '\v<(if|unless)>.*<end>\s*$'

function! s:PrevCodeLine(lnum) abort
  let l:n = a:lnum
  while l:n > 0
    let l:n = prevnonblank(l:n - 1)
    if l:n == 0
      return 0
    endif
    let l:line = getline(l:n)
    if l:line !~# '^\s*#'
      return l:n
    endif
  endwhile
  return 0
endfunction

function! GetKojaIndent() abort
  let l:clnum = v:lnum
  let l:cline = getline(l:clnum)

  let l:plnum = s:PrevCodeLine(l:clnum)
  if l:plnum == 0
    return 0
  endif

  let l:pline = getline(l:plnum)
  let l:ind = indent(l:plnum)
  let l:sw = shiftwidth()

  " Single-line guard: `if cond ... end` on one line — don't indent.
  if l:pline =~# s:single_line
    " no change
  elseif l:pline =~# s:block_openers || l:pline =~# s:arrow_end
    let l:ind += l:sw
  endif

  if l:cline =~# s:dedent_kws
    let l:ind -= l:sw
  endif

  return l:ind < 0 ? 0 : l:ind
endfunction
