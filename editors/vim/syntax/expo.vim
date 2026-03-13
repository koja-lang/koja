" Vim syntax file
" Language: Expo
" Maintainer: Henry Popp

if exists("b:current_syntax")
  finish
endif

" --- Keywords ---------------------------------------------------------------

syn keyword expoKeyword     fn priv end move spawn await return break
syn keyword expoKeyword     impl for type shared import in
syn keyword expoConditional if else unless match cond when
syn keyword expoRepeat      for loop while
syn keyword expoOperatorKw  and or not
syn keyword expoStructure   struct enum arena receive
syn keyword expoBoolean     true false
syn keyword expoConstant    none
syn keyword expoSelf        self

" --- Annotations ------------------------------------------------------------

syn match expoAnnotation    /@\w\+/

" --- Types (PascalCase identifiers) -----------------------------------------

syn keyword expoPrimitiveType bool f32 f64 i8 i16 i32 i64 string u8 u16 u32 u64
syn match expoType          /\<[A-Z][A-Za-z0-9]*\>/

" --- Constants (ALL_CAPS identifiers) ---------------------------------------

syn match expoModuleConst   /\<[A-Z][A-Z0-9_]\{1,}\>/

" --- Numbers ----------------------------------------------------------------

syn match expoNumber        /\<\d[0-9_]*\>/
syn match expoNumber        /\<\d[0-9_]*\.\d[0-9_]*\>/
syn match expoNumber        /\<0x[0-9a-fA-F_]\+\>/
syn match expoNumber        /\<0b[01_]\+\>/

" --- Strings ----------------------------------------------------------------

syn region expoString       start=/"/ skip=/\\"/ end=/"/ contains=expoInterpolation,expoEscape oneline
syn region expoMultiString  start=/"""/ end=/"""/       contains=expoInterpolation,expoEscape
syn match  expoEscape       /\\[nrt\\"]/ contained
syn region expoInterpolation matchgroup=expoInterpDelim start=/#{/ end=/}/ contained contains=TOP

" --- Operators --------------------------------------------------------------

syn match expoOperator      /|>/
syn match expoOperator      /->/
syn match expoOperator      /[+\-*/%]=\?/
syn match expoOperator      /[!=]=\|[<>]=\?/
syn match expoOperator      /=/
syn match expoOperator      /?/

" --- Comments ---------------------------------------------------------------

syn match expoComment       /#.*$/ contains=expoTodo
syn keyword expoTodo        TODO FIXME XXX NOTE HACK contained

" --- Highlight links --------------------------------------------------------

hi def link expoKeyword       Keyword
hi def link expoConditional   Conditional
hi def link expoRepeat        Repeat
hi def link expoOperatorKw    Keyword
hi def link expoStructure     Structure
hi def link expoBoolean       Boolean
hi def link expoConstant      Constant
hi def link expoSelf          Constant
hi def link expoPrimitiveType  Type
hi def link expoType          Type
hi def link expoModuleConst   Constant
hi def link expoNumber        Number
hi def link expoString        String
hi def link expoMultiString   String
hi def link expoEscape        SpecialChar
hi def link expoInterpolation Normal
hi def link expoInterpDelim   Special
hi def link expoOperator      Operator
hi def link expoAnnotation    PreProc
hi def link expoComment       Comment
hi def link expoTodo          Todo

let b:current_syntax = "expo"
