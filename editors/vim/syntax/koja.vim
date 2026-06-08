" Vim syntax file
" Language: Koja
" Maintainer: Henry Popp

if exists("b:current_syntax")
  finish
endif

" --- Syntax sync (:help syn-sync) --------------------------------------------
" Default sync guesses from a line above the viewport; long \"\"\" docstrings
" confuse that guess when jumping (G, tags, large scrolls), so highlighting
" toggles wrong until redraw. Sync from buffer start — fine for typical .koja
" sizes; for huge files, try: syn sync minlines=300 (or higher) instead.
syn sync fromstart

" --- Keywords ---------------------------------------------------------------

syn keyword kojaKeyword     after alias as break const end enum extend fn for impl
syn keyword kojaKeyword     in priv protocol receive return spawn struct type
syn keyword kojaConditional cond else if match unless when
syn keyword kojaRepeat      for loop while
syn keyword kojaOperatorKw  and not or
syn keyword kojaBoolean     false true
syn keyword kojaSelf        self
syn keyword kojaBinaryMod   signed unsigned big little byte

" --- Annotations ------------------------------------------------------------

syn match kojaAnnotation    /@\w\+/

" --- Types (PascalCase identifiers) -----------------------------------------

syn keyword kojaPrimitiveType Binary Bits Bool Float Float32 Float64 Int Int8 Int16 Int32 Int64 String UInt8 UInt16 UInt32 UInt64
syn match kojaType          /\<[A-Z][A-Za-z0-9]*\>/

" --- Constants (ALL_CAPS identifiers) ---------------------------------------

syn match kojaModuleConst   /\<[A-Z][A-Z0-9_]\{1,}\>/

" --- Numbers ----------------------------------------------------------------

syn match kojaNumber        /\<\d[0-9_]*\>/
syn match kojaNumber        /\<\d[0-9_]*\.\d[0-9_]*\>/
syn match kojaNumber        /\<0x[0-9a-fA-F_]\+\>/
syn match kojaNumber        /\<0b[01_]\+\>/

" --- Strings ----------------------------------------------------------------

" Vim does not support priority= on :syn region (E475); keyword vs region
" priority is fixed (:help syn-priority — keywords beat regions). Suppressing
" keywords inside \"\"\" needs contained keywords or a different approach.
syn region kojaString       start=/"/ skip=/\\"/ end=/"/ contains=kojaInterpolation,kojaEscape oneline
syn region kojaMultiString  start=/"""/ end=/"""/ contains=kojaInterpolation,kojaEscape
syn match  kojaEscape       /\\[nrt\\"#]/ contained
syn region kojaInterpolation matchgroup=kojaInterpDelim start=/#{/ end=/}/ contained contains=TOP

" --- Package qualifiers -----------------------------------------------------
" Packages are PascalCase (`Net`, `HTTP`, `JSON`, `Crypto`, `Global`, ...) and
" only ever appear as the head of a dotted path: `Net.TCPSocket`,
" `HTTP.Headers.new()`, `alias JSON.Encoder as JSONEncoder`. Match a PascalCase
" identifier followed by `.PascalCase` (lookahead via \ze keeps the dot out).

syn match kojaModuleQualifier /\<[A-Z][A-Za-z0-9]*\ze\.[A-Z]/

" --- Typed assignments (x: Type = value) ------------------------------------
" Require a real type head after ':' (PascalCase type name or `fn`), so prose
" like `key: value` in docstrings does not match.

syn match kojaTypeSep         /:/ contained
syn match kojaTypedAssign     /\<\l\w\+\s*:\s*\([A-Z][A-Za-z0-9]*\|fn\)/ contains=kojaTypeSep

" --- Operators --------------------------------------------------------------

syn match kojaOperator      /->/
syn match kojaOperator      /<</
syn match kojaOperator      />>/
syn match kojaOperator      /<>/
syn match kojaOperator      /::/
syn match kojaOperator      /|/
syn match kojaOperator      /[+\-*/%]=/
syn match kojaOperator      /[!=]=\|[<>]=/

" --- Comments ---------------------------------------------------------------

syn match kojaComment       /#.*$/ contains=kojaTodo
syn keyword kojaTodo        TODO FIXME XXX NOTE HACK contained

" --- Highlight links --------------------------------------------------------

hi def link kojaKeyword       Keyword
hi def link kojaConditional   Conditional
hi def link kojaRepeat        Repeat
hi def link kojaOperatorKw    Keyword
hi def link kojaBoolean       Boolean
hi def link kojaSelf          Constant
hi def link kojaBinaryMod     Number
hi def link kojaPrimitiveType  Type
hi def link kojaType          Type
hi def link kojaModuleConst   Constant
hi def link kojaNumber        Number
hi def link kojaString        String
hi def link kojaMultiString   String
hi def link kojaEscape        SpecialChar
hi def link kojaInterpolation Normal
hi def link kojaInterpDelim   Special
hi def link kojaModuleQualifier Include
hi def link kojaOperator      Operator
hi def link kojaTypeSep       Operator
hi def link kojaAnnotation    PreProc
hi def link kojaComment       Comment
hi def link kojaTodo          Todo

let b:current_syntax = "koja"
