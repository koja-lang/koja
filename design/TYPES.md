# Type System Design

Design notes for Expo's type system: algebraic foundations, planned features,
and open questions. Covers both the theoretical framework and concrete future
additions. For implemented type system features, see
[ROADMAP.md](ROADMAP.md) "Design exploration" section.

Extracted from [archive/20260403-PROJECT.md](archive/20260403-PROJECT.md)
and [ROADMAP.md](ROADMAP.md).

---

## Type algebra

### The four algebraic types

Expo's type system maps to the four operations of type algebra:

| Keyword  | Algebra               | Type theory | Role                |
| -------- | --------------------- | ----------- | ------------------- |
| `struct` | \* (multiply/product) | Product     | Combine all fields  |
| `enum`   | + (addition/sum)      | Coproduct   | Choose one variant  |
| `fn`     | ^ (exponential)       | Exponential | Map input to output |
| `alias`  | = (redirect)          | --          | Name existing types |

All three type-constructing keywords (`struct`, `enum`, `fn`) produce
first-class types on equal footing. `Type::Function` is a type alongside
`Type::Struct` and `Type::Enum`. Function values can be stored in variables,
passed as arguments, and returned.

A struct with zero fields is the unit type (algebraically, the empty product
is 1). This handles the "namespace for functions" case without a separate
keyword:

```expo
struct IO
  fn puts(message: String)
    print(message <> "\n")
  end
end
```

`IO.puts("hello")` resolves the same way as `List.map(items, f)`.

Two additional keywords name existing types without declaring new ones:

- **`alias NodeId = UInt64`** -- transparent redirect. `NodeId` IS `UInt64`,
  interchangeable everywhere.
- **`union Pet = Cat | Dog | Fish`** -- anonymous sum of existing types.
  A new type in the registry, supports `impl`.

### Keywords

Six keywords, each with exactly one job:

| Keyword  | Role                                               |
| -------- | -------------------------------------------------- |
| `struct` | Product type (0+ fields) with inline functions     |
| `enum`   | Sum type (declared variants) with inline functions |
| `fn`     | Function (named or anonymous/closure)              |
| `alias`  | Transparent type redirect                          |
| `union`  | Anonymous sum of existing types                    |
| `impl`   | External extension / protocol conformance          |

### Inline functions

Functions live inside type bodies (inspired by Swift). The type declaration
and its core API are one unit -- you see the type and what it does together:

```expo
struct User
  name: String
  email: String

  fn greet(self) -> String
    "Hello, #{self.name}"
  end

  fn new(name: String, email: String) -> Self
    User{name: name, email: email}
  end
end
```

```expo
enum Option<T>
  Some(T)
  None

  fn unwrap(self) -> T
    match self
      Option.Some(v) -> v
      Option.None -> panic("unwrap on None")
    end
  end

  fn map<U>(self, f: fn(T) -> U) -> Option<U>
    match self
      Option.Some(v) -> Option.Some(f(v))
      Option.None -> Option.None
    end
  end
end
```

`impl` is reserved for **external additions**: protocol conformance and
cross-file extensions (Swift's `extension` pattern). Core functions go
inline; `impl` extends from outside.

```expo
impl Display for User
  fn to_string(self) -> String
    "User(#{self.name})"
  end
end
```

### Type composability

The three type-constructing keywords compose with each other in every
direction:

- A **struct** can hold structs, enums, and functions as fields
- An **enum** can hold structs, enums, and functions as variant payloads
- A **fn** can take and return structs, enums, and other functions

Every type-constructor nests inside every other. This isn't accidental --
it falls out of all three being first-class types on equal footing.

The enum-with-function-payload case is especially interesting. An enum
variant that carries a function creates a tagged callable:

```expo
enum Step<T>
  Run(fn(T) -> Result<T, String>)
  Validate(fn(T) -> Bool, String)

  fn execute(self, data: T) -> Result<T, String>
    match self
      Step.Run(f) -> f(data)
      Step.Validate(pred, msg) ->
        if pred(data)
          Result.Ok(data)
        else
          Result.Err(msg)
        end
    end
  end
end
```

This enables typed pipeline patterns (see "Command pattern" below) as
library code rather than language primitives.

### Named vs anonymous forms

Each algebraic type has a named (declared) form and an anonymous (inline) form:

| Algebra     | Named (declared)       | Anonymous (inline)                  | Term    |
| ----------- | ---------------------- | ----------------------------------- | ------- |
| Product     | `struct User`          | `{name: String, age: Int}`          | record  |
| Sum         | `enum Option<T>`       | `union Pet = Cat \| Dog`            | union   |
| Exponential | `fn name(...) ... end` | `x -> expr` / `fn (...) -> ... end` | closure |
| Unit        | `struct IO` (0 fields) | `()` (unit literal)                 |         |

Every type-constructor now has both forms. The algebra is complete.

### Documentation categorization

Generated docs categorize by data shape, not by a generic "type" label:

- **Structs**: List, Request, User (types with fields)
- **Enums**: Option, Result (types with variants)
- **Modules**: IO, Math, Path (structs with no fields, functions only)

The doc generator infers the category from `fields.is_empty()` in the
type registry.

---

## Records (not implemented)

Records are the anonymous form of structs, just as closures are the
anonymous form of functions:

    record : struct :: closure : named function

Records fill the product slot that tuples occupy in other languages,
without the readability cost of positional access. Fields are named, so
`{x: Int, y: Int}` is self-documenting in a way `(Int, Int)` never is.

```expo
fn parse_header(raw: String) -> {name: String, value: String}
  // ...
end

result = parse_header(line)
print(result.name)
```

No one-off `struct HeaderPair` cluttering the module for a shape used in
one place.

**Structural, not nominal.** Records are matched by shape (field names +
types). Two `{x: Int, y: Int}` are the same type regardless of where
they appear. Named structs stay nominal -- `struct Point` and
`struct Vec2` with identical fields are different types. The rule is
clear: named = nominal, record = structural.

**No methods.** Records have no body, so no inline functions. If you need
methods, name it with `struct`. This creates a natural gradient: records
for lightweight data, structs for API-bearing types.

**`alias` as the bridge.** `alias Coordinates = {x: Float, y: Float}`
gives a record a name without making it nominal. Still structural, but
readable at call sites.

The recursive typechecker (see [COMPILER.md](COMPILER.md)) handles records
naturally -- field-level analysis tracks by field name, not by a registered
type name. A record flowing through a pipeline has the same definite
assignment tracking as a named struct.

---

## `union` keyword (not implemented)

Union types are the anonymous form of enums, just as closures are the
anonymous form of functions:

    union : enum :: closure : named function

A closure is an anonymous function. A union type is an anonymous sum -- a
sum without named variants, where existing types serve as discriminators.

The `union` keyword declares a real type in the registry:

```expo
union Pet = Cat | Dog | Fish

impl Display for Pet
  fn to_string(self) -> String
    match self
      p: Cat -> "cat"
      p: Dog -> "dog"
      p: Fish -> "fish"
    end
  end
end
```

Currently, anonymous unions use inline `A | B` syntax without the `union`
keyword. The `union` keyword would promote these to named, `impl`-able
types.

---

## Struct field defaults and trailing keyword syntax (open)

- **Open**: struct fields with default values (`struct Opts timeout: Int = 5000 end`).
  Enables partial construction -- only override the fields you care about.
- **Open**: trailing keyword syntax at call sites as sugar for opts struct
  construction. `pid.call(msg, timeout: 30000)` desugars to
  `pid.call(msg, CallOpts{timeout: 30000})`. Combined with struct field defaults,
  this gives typed, compile-checked keyword arguments -- Elixir's `Keyword.t()`
  opts pattern but with type safety (invalid keys and wrong value types are
  compile errors).
- **Motivating use case**: `Ref.call` needs an optional timeout with a sensible
  default. Without this feature, every call site must specify the timeout
  explicitly. Useful far beyond concurrency -- any function with optional
  configuration benefits (HTTP clients, query builders, formatters).

---

## Type system philosophy

- **Decided**: enums and structs have equal capabilities -- fractal design where the same features available to `Option<T>` (a built-in enum) are available to any user-defined enum. No two-tier type system.
- **Decided**: if types get inline functions, both structs and enums support them. An enum is semantically a one-field struct with a tagged union type -- the distinction is surface syntax, not fundamental.
- **Open**: whether inline functions in type bodies are restricted to `self`-taking functions only (instance methods), or also allow non-`self` functions (static/factory -- which makes the type act as a namespace).

---

## Namespace unification: modules as pseudotypes (exploration)

- **Context**: the `TypeInfo` refactor unified struct/enum/primitive function storage into a single `ctx.types` registry. This eliminates duplicated dispatch logic but still treats module-qualified calls (`Module.function()`) differently from type-qualified calls (`Type.function()`).
- **Observation**: a module file is structurally similar to a type -- it defines imports, structs, enums, and functions. A type (struct/enum/primitive) defines functions (and possibly nested types in the future). Both are namespaces that own functions.
- **Design exploration**: treat modules as pseudotypes in the same `TypeInfo` registry, with a new `TypeKind::Module` variant. Module-qualified calls (`Http.get(url)`) and static type calls (`Option.some(x)`) would resolve through the same lookup path: `ctx.types.get(qualifier).and_then(|ti| ti.functions.get(name))`.
- **Benefits**: single resolution path for all qualified calls, recursive namespace model (modules contain types, types contain functions), forward-compatible with nested types or module re-exports.
- **Risks**: modules currently carry full `TypeContext` in `imported_modules` (with their own types, functions, etc.), which is richer than `TypeInfo`. Flattening this into `TypeInfo` may lose expressiveness. The `imported_modules` map also handles transitive imports and visibility scoping.
- **Status**: not planned for immediate implementation. The current `TypeInfo` registry is designed to be forward-compatible -- adding `TypeKind::Module` later would not require re-architecture.

---

## Literal protocols (partially implemented)

- **Concept**: all literal syntax (`42`, `"hello"`, `[...]`, `[k:v]`, `(a,b)`) backed by protocols, not special-cased types. Any type can opt into literal construction by implementing the protocol.
- **Protocol family**: `IntLiteral`, `FloatLiteral`, `StringLiteral`, `ListLiteral<T>`, `MapLiteral<K,V>`, `PairLiteral<A,B>`.
- **Default types**: `Int`, `Float`, `String`, `List<T>`, `Map<K,V>`, `Pair<A,B>` when no type annotation is present.
- **Infallible**: literal protocols return `Self`, not `Result`. Fallible parsing (e.g. from untrusted input) uses regular functions that return `Result`.
- **Pair syntax**: `(a, b)` may return via `PairLiteral<A, B>` -- only pairs (arity 2). 3+ values use named structs.
- **Implemented**: `ListLiteral<T>` with `from_list(move list: List<T>) -> Self` -- `List<T>` and `Set<T>` implement it. Defined in `std.kernel`.
- **Implemented**: `MapLiteral<K, V>` with `from_map(move map: Map<K, V>) -> Self` -- `Map<K, V>` implements it as identity. `[key: value]` syntax and `[:]` for empty maps.
- **Planned**: `IntLiteral`, `FloatLiteral` (enables custom `Decimal` type from float literals), `StringLiteral`, `PairLiteral<A,B>`.
- **Fractal design**: user-defined types and built-in types have identical access to literal syntax. No two-tier system.

---

## Identifier priming (future)

Trailing prime notation (`'`) would allow `end'` as a field name and `Self'` or `String'` as enum variant names without ambiguity. Grammar change: append `[ "'" ]` to both `IDENT` and `TYPE_IDENT` rules (trailing-only, single prime). Surfaced by the `expo-ast` self-hosting port: `Span.end` had to become `Span.stop`, and enum variants like `Self`, `String`, `Bool`, `Int`, `Float` needed descriptive renames (`SelfReceiver`, `StringVal`, `BoolLit`, etc.). Leading `'` stays invalid, so `'wrongstring'` is always a syntax error. Lexer-only change, no parser/typecheck/codegen impact.

---

## Iterator protocol redesign (not implemented)

`Enumeration<T>` requires `length()` + `get(index)`, locking `for` to
index-based while loops. This precludes lazy iteration, streaming, and any
non-random-access collection (maps, linked lists, generators).

Pre-v1.0, replace with an `Iterator<T>` protocol using
`next(move self) -> Option<Pair<T, Self>>`. `get` now returns `Option<T>`.
Codegen change is contained to `compile_for` in `loops.rs`; List/String
impls wrap existing index-based access in iterator state.

The current `for` loop hides the `Option` from the user (unwraps
automatically since iteration is bounds-checked). With lazy iteration,
`Option` becomes the termination mechanism -- `for` desugars to
`loop { match iter.next() ... }` and `None` breaks the loop.

See also [GAPS.md](GAPS.md) for the current limitation.

---

## Command pattern: library type, not keyword

The `Command` type will be a stdlib construct built on enum function
variants and the `Step<T>` pipeline pattern. No dedicated `command` keyword.

The guarantees it provides reduce to a single general-purpose analysis:
**cross-function definite assignment**. If the typechecker can track which
struct fields have been meaningfully initialized at each point in a
pipeline, it can verify that no step reads a field before a prior step sets it.

Example: a `Pipeline<T>` built from `Step<T>` enum variants (see "Type
composability" above):

```expo
Pipeline.new(Registration{email: email, password_hash: "", user_id: 0})
  .step(Step.Validate(r -> r.email != "", "email required"))
  .step(Step.Run(r -> hash_password(r)))
  .step(Step.Run(r -> create_user(r)))
  .run()
```

If the typechecker holds all function bodies in memory (not just
signatures), it can recursively walk into `hash_password` and
`create_user` to determine which fields each function reads and writes.
It can then verify that `create_user` doesn't read `password_hash`
before `hash_password` sets it.

This is the same class of analysis the typechecker already performs for
variable move tracking (`Live`/`Moved`/`MaybeMoved`), extended to struct
field initialization across function boundaries.

The self-hosted compiler's architecture (see [COMPILER.md](COMPILER.md))
enables this. The Rust bootstrap compiler only holds function _signatures_
during checking -- bodies are checked independently. The self-hosted
compiler's immutable `TypeContext` with all code in memory allows the
checker to walk into called functions and trace data flow.

The key insight: this analysis benefits ALL code, not just command
pipelines. Any function that receives a partially-initialized struct
gets the same safety.

Depends on: enum function variants, self-hosted compiler.

---

## Decided

- **Inline functions**: functions live inside `struct` and `enum` bodies
  (Swift model). `impl` is reserved for external extensions and protocol
  conformance.
- **No 0-D `type` keyword**: `struct IO` (no fields) serves as a namespace.
  An empty product is algebraically the unit type. No separate keyword needed.
- **`alias` replaces `type X = Y`** for transparent redirects.
- **`union` replaces `type X = A | B`** for anonymous sums. Union types are
  real entries in the type registry, supporting `impl`.
- **File = module**: no `module` keyword. The file provides the module
  boundary. One idea per file by convention.
- **`impl` across files**: `impl Display for User` can appear in a different
  file from `struct User`. A type is declared in one file, but `impl` blocks
  can appear anywhere (protocol conformance, extensions from other files).
- **Trait bounds use `&`**: generic type params can be bounded with
  `<T: Protocol>` or `<T: Proto1 & Proto2>`. `&` is the protocol
  composition operator, complementing `|` for union types. `&` has no
  other meaning in Expo (no references, no address-of). Mirrors
  TypeScript and Swift's `&` for intersection/composition.
- **`protocol` keyword for composition**: named protocol compositions
  use `protocol Storable = Readable & Writable`. The result is a
  protocol, so the keyword stays in the protocol family. No `type`
  keyword needed.
- **`type` keyword removed**: every type declaration has a specific
  keyword: `struct` (product), `enum` (sum with variants), `union`
  (anonymous sum), `protocol` (contract + composition), `alias`
  (transparent redirect). `type` was the catch-all; now each blade
  has its own handle. The `type` keyword in the lexer should be
  replaced with `union` once the migration is done.
- **`|` and `&` don't mix**: `|` composes types (union), `&` composes
  protocols (intersection). `Cat & Dog` and `Debug | Hash` are compile
  errors. This avoids diamond-inheritance-style confusion and keeps
  both operators simple.
- **No `&` for references or mutability**: Expo uses `move` for
  ownership transfer and borrows by default. `&` is purely a
  type-level composition operator. This decision is final.

---

## Open questions

- **`impl` on `alias` types**: `alias Handler = fn(Request) -> Response`.
  Should `impl Handler` work? Function types are structural, not nominal.
  Naming them via `alias` doesn't change their structure.
- **`impl` on `union` types**: if all constituents of
  `union Pet = Cat | Dog | Fish` implement `Display`, does `Pet`
  automatically implement `Display`? Auto-forwarding is appealing but has
  edge cases (what if only 2 of 3 implement it, what if implementations
  conflict). The alternative is requiring an explicit `impl Display for Pet`
  with an exhaustive match.
- **Record width subtyping**: is `{foo: String, bar: Int, baz: Bool}`
  assignable to `{foo: String, bar: Int}`? TypeScript says yes (wider records
  are subtypes of narrower ones). Most ML-family languages say no (exact match).
  Exact match is simpler and more predictable. Width subtyping is more flexible
  but makes type errors harder to diagnose.
- **Circular type references**: currently banned by cycle detection. With the
  unified registry, circular type references could theoretically be handled the
  same way as recursive struct types (`Type::Indirect`), but this adds
  significant complexity.
