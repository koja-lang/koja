# Conformance Headers

Design for declaring protocol conformance on the type declaration itself,
with protocol methods living in the type body. Proposed 2026-07-16 during
the pooler build. Status: agreed design, not yet implemented.

---

## Motivation

The most common declaration shape in Koja is a struct plus exactly one
`impl Process<...> for` block:

```koja
struct MyProcess
end

impl Process<Config, Msg, Reply> for MyProcess
  fn start(config: Config) -> Result<Self, StopReason> ... end
  fn handle(self, msg: Msg, from: Option<ReplyTo<Reply>>) -> Step<Self> ... end
end
```

Every stateful process in the language pays for a second block that
repeats the type name. Koja already lets inherent methods live either in
the type body or in an `impl Type` block. Protocols are the exception,
they only get the block form. The header completes the symmetry.

## Syntax

A colon after the type name (and generic params) introduces a
comma-separated conformance list:

```koja
struct Server<T>: Process<Config<T>, Msg<T>, Reply<T>>, Debug
  available: List<T>
  create: fn () -> Result<T, String>
  waiting: List<ReplyTo<Reply<T>>>

  fn start(config: Config<T>) -> Result<Self, StopReason> ... end
  fn handle(self, msg: Msg<T>, from: Option<ReplyTo<Reply<T>>>) -> Step<Self> ... end
  fn format(self) -> String ... end

  priv fn checkout(self, from: Option<ReplyTo<Reply<T>>>) -> Self ... end
end
```

`enum` gets the same header. `priv struct Foo: Proto` composes
orthogonally, and the existing "public signature cannot mention a private
type" rule already covers a private protocol on a public type.

Grammar:

```ebnf
struct_decl = "struct", TYPE_IDENT, [generic_params], [":", conformance_list], body, "end";
conformance_list = type_ref, {",", type_ref};
```

Parsing is unambiguous. Commas inside `Process<C, M, R>` are consumed by
the balanced type-expression parser (impl headers already parse it as a
unit), so top-level commas cleanly separate list entries.

### Comma vs `&` (decided)

Declaration headers use commas, following Swift convention. `&` remains
exclusively the protocol-composition operator in type and bound positions
(`fn foo<T: Hash & Equality>`). The two answer different questions: a
header lists independent conformance facts (each checked and diagnosed
separately), while `&` describes a single constraint. `&` in a header is
rejected with a hint: "use a comma-separated conformance list (`&`
composes protocols in type positions)".

## Semantics

The header declares the conformance and the body's methods satisfy it.

- Completeness and signature checks run against the body's fn members,
  reusing the same protocol-definition comparison the impl path uses.
  One diagnostic set per listed protocol.
- Default-bodied protocol methods synthesize when omitted, same as in an
  impl block.
- The body stays a normal body: fields, inherent fns, protocol fns, and
  `priv` helpers mix freely. There is no extra-method rejection in the
  body (unlike `impl Protocol for` blocks, which stay strict).
- One body fn may satisfy the same-named requirement of multiple listed
  protocols. The block form cannot express this (two impl blocks
  declaring the same method name collide).
- `impl Protocol for Type` is unchanged and remains the form for
  retroactive conformance (types you do not own) and for conformance you
  want organized separately. Declaring the same conformance in both the
  header and an impl block is a duplicate-conformance error.

### Typo hazard and mitigation

The strict impl block rejects public extras, which protects overrides of
default-bodied methods from near-miss typos (`fn pritn` next to a `Debug`
conformance would otherwise silently become an inherent method while the
default `print` synthesizes). The header form deliberately trades that
strictness for terseness. Mitigation: when a header-conformant type
leaves a default method unimplemented and the body contains a fn within
small edit distance of its name, warn with "did you mean `print`?". The
two forms then have an honest split: impl block is the isolated-contract
form, header is the convenient form.

## Implementation sketch

Contained to the front half of the pipeline. IR and both backends are
untouched, conformance is a typecheck concept and the monomorphized
methods come out identical.

- **Parser** (`koja-parser/src/decl.rs`): accept the optional header list
  on `struct` and `enum`. Store it on the decl node as a list of
  `TypeExpr`s.
- **Lift** (`koja-typecheck/src/pipeline/lift_signatures/`): for each
  listed protocol, synthesize the same conformance record
  `record_target_conformance` produces for an impl block, sourcing the
  member set from the body's fns. Must not grow a parallel copy of
  `verify_protocol_conformance`: the header path desugars into the same
  member-set + protocol-definition check.
- **Formatter** (`koja-fmt`): header wrapping when the list is long
  (break after the colon, one protocol per line, same packing rules as
  argument lists).
- **LSP** (`koja-lsp`): go-to-definition on header protocol refs,
  conformance in hover.
- **Docs** (`koja-doc`): render header conformance on the type page,
  merged with impl-block conformance.
- **Debug derive**: unchanged, `Debug` auto-derives unless the user
  implements it, whether via header or impl block.

## Open questions

- Should the near-miss warning also apply to strict impl blocks that
  omit a default method? (Cheap to share once the edit-distance helper
  exists.)
- Header conformance on `type` unions is out of scope. Unions conform
  structurally through their members today and nothing changes.
