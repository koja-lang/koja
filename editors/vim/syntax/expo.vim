" Vim syntax file
" Language: Expo
" Maintainer: Henry Popp

if exists("b:current_syntax")
  finish
endif

" --- Syntax sync (:help syn-sync) --------------------------------------------
" Default sync guesses from a line above the viewport; long \"\"\" docstrings
" confuse that guess when jumping (G, tags, large scrolls), so highlighting
" toggles wrong until redraw. Sync from buffer start — fine for typical .expo
" sizes; for huge files, try: syn sync minlines=300 (or higher) instead.
syn sync fromstart

" --- Keywords ---------------------------------------------------------------

syn keyword expoKeyword     after alias as break const end enum extend fn for impl
syn keyword expoKeyword     in move priv protocol receive return spawn struct type
syn keyword expoConditional cond else if match unless when
syn keyword expoRepeat      for loop while
syn keyword expoOperatorKw  and not or
syn keyword expoBoolean     false true
syn keyword expoSelf        self
syn keyword expoBinaryMod   signed unsigned big little byte

" --- Annotations ------------------------------------------------------------

syn match expoAnnotation    /@\w\+/

" --- Types (PascalCase identifiers) -----------------------------------------

syn keyword expoPrimitiveType Binary Bits Bool Float Float32 Float64 Int Int8 Int16 Int32 Int64 String UInt8 UInt16 UInt32 UInt64
syn match expoType          /\<[A-Z][A-Za-z0-9]*\>/

" --- Constants (ALL_CAPS identifiers) ---------------------------------------

syn match expoModuleConst   /\<[A-Z][A-Z0-9_]\{1,}\>/

" --- Numbers ----------------------------------------------------------------

syn match expoNumber        /\<\d[0-9_]*\>/
syn match expoNumber        /\<\d[0-9_]*\.\d[0-9_]*\>/
syn match expoNumber        /\<0x[0-9a-fA-F_]\+\>/
syn match expoNumber        /\<0b[01_]\+\>/

" --- Strings ----------------------------------------------------------------

" Vim does not support priority= on :syn region (E475); keyword vs region
" priority is fixed (:help syn-priority — keywords beat regions). Suppressing
" keywords inside \"\"\" needs contained keywords or a different approach.
syn region expoString       start=/"/ skip=/\\"/ end=/"/ contains=expoInterpolation,expoEscape oneline
syn region expoMultiString  start=/"""/ end=/"""/ contains=expoInterpolation,expoEscape
syn match  expoEscape       /\\[nrt\\"#]/ contained
syn region expoInterpolation matchgroup=expoInterpDelim start=/#{/ end=/}/ contained contains=TOP

" --- Package qualifiers -----------------------------------------------------
" Packages are PascalCase (`Net`, `HTTP`, `JSON`, `Crypto`, `Global`, ...) and
" only ever appear as the head of a dotted path: `Net.TCPSocket`,
" `HTTP.Headers.new()`, `alias JSON.Encoder as JSONEncoder`. Match a PascalCase
" identifier followed by `.PascalCase` (lookahead via \ze keeps the dot out).

syn match expoModuleQualifier /\<[A-Z][A-Za-z0-9]*\ze\.[A-Z]/

" --- Typed assignments (x: Type = value) ------------------------------------
" Require a real type head after ':' (PascalCase type name or `fn`), so prose
" like `key: value` in docstrings does not match.

syn match expoTypeSep         /:/ contained
syn match expoTypedAssign     /\<\l\w\+\s*:\s*\([A-Z][A-Za-z0-9]*\|fn\)/ contains=expoTypeSep

" --- Operators --------------------------------------------------------------

syn match expoOperator      /->/
syn match expoOperator      /<</
syn match expoOperator      />>/
syn match expoOperator      /<>/
syn match expoOperator      /::/
syn match expoOperator      /|/
syn match expoOperator      /[+\-*/%]=/
syn match expoOperator      /[!=]=\|[<>]=/

" --- Comments ---------------------------------------------------------------

syn match expoComment       /#.*$/ contains=expoTodo
syn keyword expoTodo        TODO FIXME XXX NOTE HACK contained

" --- Highlight links --------------------------------------------------------

hi def link expoKeyword       Keyword
hi def link expoConditional   Conditional
hi def link expoRepeat        Repeat
hi def link expoOperatorKw    Keyword
hi def link expoBoolean       Boolean
hi def link expoSelf          Constant
hi def link expoBinaryMod     Number
hi def link expoPrimitiveType  Type
hi def link expoType          Type
hi def link expoModuleConst   Constant
hi def link expoNumber        Number
hi def link expoString        String
hi def link expoMultiString   String
hi def link expoEscape        SpecialChar
hi def link expoInterpolation Normal
hi def link expoInterpDelim   Special
hi def link expoModuleQualifier Include
hi def link expoOperator      Operator
hi def link expoTypeSep       Operator
hi def link expoAnnotation    PreProc
hi def link expoComment       Comment
hi def link expoTodo          Todo

let b:current_syntax = "expo"
