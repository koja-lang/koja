# Miscellaneous Ideas

This file is a memory aid for ideas that are worth retaining but do not yet
warrant a design document or roadmap commitment. An entry here is not an
accepted design, scheduled work, or a release promise.

When an idea becomes concrete, move it into a focused design document. Add it
to [ROADMAP.md](ROADMAP.md) only when its release scope is known. Delete ideas
that no longer serve the language.

## Arity overloading (separately declared same-name functions)

Erlang-style `foo/2` and `foo/3` as distinct functions with distinct bodies.
Deferred in favor of default parameter values, which cover trailing optional
arguments from one declaration. The unique cases B-style arities add:

- The parameter meaning shifts with arity, as in `range(stop)` against
  `range(start, stop)`. Defaults cannot express this because they fill from
  the right.
- The return type shifts with arity, as in `get(key) -> Option<V>` against
  `get(key, default) -> V`. Distinct names like `get` and `get_or` document
  this better in a typed language.
- Each arity has a genuinely different implementation.

Revisit if stdlib or ecosystem work keeps producing awkward `foo`,
`foo_from`, `foo_with` name clusters that are really one concept. The
`&name/arity` reference syntax works the same whether the arities come from
one declaration or several, so this extension stays syntax-compatible. If
adopted, a defaulted function claims its whole arity range and any separate
declaration inside that range is a compile error.

## Anonymous records

Named-field companions to anonymous tuples, for example
`{name: "x", count: 3}` without a struct declaration. Deferred from 0.17 and
gated on evidence: revisit only if ecosystem work shows that mid-pipeline
projections (JSON responses, database rows, `map` outputs) are a recurring
pain that anonymous tuples and cheap nominal structs do not cover.

The main arguments against: declaring a struct in Koja is nearly free, a
declared struct is where `@doc`, defaults, `impl` blocks, and derives hang,
and structural typing (width subtyping, record flow rules) is a large
type-system commitment in a nominal, monomorphized compiler. Decoding JSON
and rows into declared structs is the intended practice. Struct field
defaults and possible trailing keyword syntax cover the options-struct use
case.

## C-compatible structs

Support an explicitly C-compatible struct layout for passing records by value
across the FFI boundary. A future design must define field mapping, alignment,
padding, nested layouts, target ABI differences, and which Koja field types are
valid.

The old `@compat "C"` sketch is preserved in
[archive/20260722-FFI.md](archive/20260722-FFI.md). The annotation name and
surface syntax are not decided.

## C callbacks

Support passing Koja callables where a C API expects a function pointer. Bare
noncapturing functions are the narrowest possible starting point because a C
function pointer has no environment.

Capturing closures require a trampoline and explicit answers for:

- captured environment lifetime
- C userdata representation
- callbacks retained after the original foreign call returns
- entry from foreign OS threads
- Koja process and scheduler context
- non-atomic process-local reference counts
- panic containment at the C boundary

No callback may unwind a Koja panic through C. The runtime also cannot execute
ordinary process-local Koja code on an unattached foreign thread. A design
should begin from the needs of a real wrapper package rather than promise
general closure conversion.
