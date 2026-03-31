# Project System & Module Unification

Design notes for A3b (project system + test runner) and the module unification
that emerged from working through it. These decisions are interconnected -- the
project system requires a coherent module/import model, and getting that right
requires unifying how modules and types resolve.

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

### Records

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

The recursive typechecker (see COMPILER.md) handles records naturally --
field-level analysis tracks by field name, not by a registered type name.
A record flowing through a pipeline has the same definite assignment
tracking as a named struct.

### Union types

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

### Documentation categorization

Generated docs categorize by data shape, not by a generic "type" label:

- **Structs**: List, Request, User (types with fields)
- **Enums**: Option, Result (types with variants)
- **Modules**: IO, Math, Path (structs with no fields, functions only)

The doc generator infers the category from `fields.is_empty()` in the
type registry.

---

## Unifying the type registry

### Current state (the problem)

Two parallel registries handle qualified calls:

- `ctx.imported_modules["io"].functions["puts"]` -- module-qualified calls
- `ctx.types["List"].functions["map"]` -- type-qualified calls

`infer_method_call` tries `imported_modules` first, then `ctx.types`. Two code
paths for the same fundamental operation: `Name.function()`.

This split doesn't mirror the conceptual reality. Modules and types are both
namespaces that own functions. The implementation should reflect that.

### Target state

One registry: `ctx.types`. One resolution path.

- `ctx.types["IO"]` = `TypeKind::Struct { fields: [] }` (namespace, no fields)
- `ctx.types["List"]` = `TypeKind::Struct { fields: [...] }` (product)
- `ctx.types["Option"]` = `TypeKind::Enum { variants: [...] }` (sum)

`imported_modules` is removed. All qualified calls resolve through:

```
ctx.types.get(name)?.functions.get(method)
```

Everything is PascalCase. `IO.puts()`, `List.map()`, `Option.some()`.

### What changes

- **`TypeKind`**: no new variant needed. `TypeKind::Struct { fields: vec![] }`
  covers the namespace case (empty struct = 0-D type). Doc generator infers
  "Module" category from `fields.is_empty()`.
- **`infer_method_call`**: the module-qualified branch is removed. One path
  through `ctx.types`.
- **`clone_public_context` / `merge_all_public`**: currently skip constants,
  protocols, and type aliases. These gaps must be fixed regardless of the
  unification.
- **Import resolution**: imported modules register their exported types in
  `ctx.types`, not in `imported_modules`.

---

## Import system

### Decoupling file paths from type names

The import path is a **filesystem locator**. The type name comes from the
**source declaration**. These are decoupled:

```
import std.io
  filesystem: find std/io.expo
  source declares: struct IO (with inline functions)
  result: IO is registered in ctx.types
```

```
import std.string
  filesystem: find std/string.expo
  source defines: struct String ...
  result: String is registered in ctx.types
```

Filenames are lowercase (filesystem convention). Type names are PascalCase
(language convention). No implicit filename-to-PascalCase conversion -- the
source declares its own name.

### What `import` does

`import std.io` tells the driver to find and compile `std/io.expo`. The types
and functions defined in that file are registered in the importer's `ctx.types`.

- `import std.io` -- brings `IO` (and any other types in that file) into scope
- `import std.io.IO` -- brings only `IO` into scope (cherry-pick)
- `import std.io.*` -- brings all public symbols into unqualified scope

### Stdlib

Stdlib modules are embedded via `include_str!`. The import resolver handles
`std.*` prefixes by mapping them to embedded sources instead of disk files.

Auto-import policy:

- **Kernel types** (`Option`, `Result`, `Pair`, `Bool`, `Int`, `Float`, `String`,
  `List`, `Map`, `Set`) are auto-imported -- available everywhere without
  explicit import.
- **Other stdlib modules** (`IO`, `File`, `Fd`, `Bitwise`) require
  `import std.io`, `import std.fd`, etc. for access.
- Kernel **functions** (`print`) remain unqualified.

### Unified module sources

The resolver knows about multiple module sources:

- **Stdlib**: `std.*` prefixes map to embedded source strings
- **Project**: unprefixed paths map to files under `src` dirs
- **Packages** (future): package prefixes map to fetched dependencies

---

## Project file: `project.expo`

### Format: struct literal

```expo
Project{
  name: "my_app",
  src: ["src", "lib"],
  test: ["test"],
}
```

The driver parses the file with the existing parser, walks the AST for a
`StructConstruction` node named `Project`, and extracts field values directly
from the AST (string literals, list literals). No typechecker or codegen
needed -- just AST pattern matching.

`Project` is not a registered type. The driver recognizes it by convention.

### Fields

| Field   | Type           | Default    | Purpose                        |
| ------- | -------------- | ---------- | ------------------------------ |
| `name`  | `String`       | (required) | Project name                   |
| `src`   | `List<String>` | `["src"]`  | Source directories             |
| `test`  | `List<String>` | `["test"]` | Test directories               |
| `entry` | `String`       | `"main"`   | Entry module (for executables) |

Extends naturally: future fields like `deps`, `build_backend` are just more
struct fields.

### Driver behavior

- On `expo build` / `expo test` / `expo check`: look for `project.expo` in the
  current directory.
- If found: parse it, extract config, set project root to the `project.expo`
  directory.
- If not found: fall back to current behavior (entry file's parent as root).
- Module discovery: scan `src` dirs for `.expo` files to build the module graph.

---

## Design principles applied

### "Code should mirror reality"

The conceptual model says structs and enums are all namespaces that own
functions. The implementation should have one registry and one resolution
path -- not two parallel mechanisms that happen to do the same thing.

### Instantaneous complexity

Software complexity exists at four scopes (see
[Tackling Instantaneous Complexity](https://blog.codedge.io/write-better-software-tackling-instantaneous-complexity/)):

- A **function** should do one thing.
- A **module** should represent one idea.
- A **project** should have one role.
- A **system** should have one mission.

In Expo, these map to:

- **Function**: `fn` -- does one thing.
- **Module**: a file. Contains types and functions around one idea.
  `@moduledoc` documents it. No `module` keyword -- the file IS the module.
- **Type**: `struct` or `enum` with inline functions. The namespace boundary
  within a module. Types are the ideas; files organize them on disk.
- **Project**: `project.expo` defines the build unit.

### Fractal consistency

At every scope, the same pattern applies:

- A type owns functions: `Type.function()`
- A function does one thing
- A file represents one idea
- A project has one role

The qualified-call syntax `Name.function()` works identically regardless of
whether `Name` is a struct or enum. No special cases.

---

## Command pattern: library, not keyword

The ROADMAP (see "Future: `command` construct") proposes a `command`
keyword for typed, composable pipelines with compile-time guarantees:
step-ordered type safety, exhaustive data flow, automatic error types.

These guarantees reduce to a single general-purpose analysis: **cross-function
definite assignment**. If the typechecker can track which struct fields have
been meaningfully initialized at each point in a pipeline, it can verify that
no step reads a field before a prior step sets it.

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

The self-hosted compiler's architecture (see COMPILER.md) enables this.
The Rust bootstrap compiler only holds function _signatures_ during
checking -- bodies are checked independently. The self-hosted compiler's
immutable `TypeContext` with all code in memory allows the checker to
walk into called functions and trace data flow.

The key insight: this analysis benefits ALL code, not just command
pipelines. Any function that receives a partially-initialized struct
gets the same safety. The `command` keyword's guarantees become a
natural consequence of a smarter typechecker, not a special-purpose
language feature.

Conclusion: `command` functionality is achievable as a stdlib construct
(`std.command` or equivalent) backed by general-purpose definite
assignment analysis. No dedicated keyword needed.

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
  boundary. `@moduledoc` documents it. One idea per file by convention.
- **`impl` across files**: `impl Display for User` can appear in a different
  file from `struct User`. A type is declared in one file, but `impl` blocks
  can appear anywhere (protocol conformance, extensions from other files).
- **Command as library, not keyword**: the `command` construct from the
  ROADMAP is achievable as a stdlib pattern (`enum Step<T>` with function
  payloads + `Pipeline<T>`) backed by cross-function definite assignment
  analysis in the self-hosted typechecker. No dedicated keyword needed.

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
- **Multiple types per file**: a file can define multiple types. When you
  `import` that file, all its public types come into scope. No restriction
  on one-type-per-file. Tension with "one idea per file" -- is this a problem
  in practice?
- **Entry file**: the entry module (`main.expo`) may have top-level statements
  (the program body). `fn main` is a top-level function, not attached to any
  type. This is fine -- not everything needs a type.
- **Circular imports**: currently banned by cycle detection. With the unified
  registry, circular type references could theoretically be handled the same
  way as recursive struct types (`Type::Indirect`), but this adds significant
  complexity and is not needed for A3b.
- **Test runner**: `@test` annotated functions with `expo test`. Discovery
  scans `test` dirs from `project.expo`. Implementation details deferred to
  the implementation phase.
- **Record width subtyping**: is `{foo: String, bar: Int, baz: Bool}`
  assignable to `{foo: String, bar: Int}`? TypeScript says yes (wider records
  are subtypes of narrower ones). Most ML-family languages say no (exact match).
  Exact match is simpler and more predictable. Width subtyping is more flexible
  but makes type errors harder to diagnose.

---

## JSON decoding: pipeline pattern over `mapN` combinators

JSON decoding typically uses `mapN` combinators (`map2`, `map3`, ..., `map8`)
to combine N decoders of different types into one result. This has an arity
problem: you need a separate function for each parameter count.

Expo can avoid `mapN` entirely by leveraging `JsonValue` as the
heterogeneous carrier. The key insight: `JsonValue` (an enum) already holds
String, Int, Float, Bool, etc. in a type-safe way. There's no need to
extract typed values during the validation phase. Validate the structure
first, extract typed values once at construction time.

This collapses JSON decoding into the `Step<T>` pipeline pattern where
`T = Decoder` (a struct carrying the source JSON object + accumulated
errors). Every step has the same type signature (`Decoder -> Decoder`),
so there's no arity explosion:

```expo
Decoder.from(json)
  .require("name", JsonType.String)
  .require("age", JsonType.Number)
  .require("email", JsonType.String)
  .validate("age", fn (v) -> v.as_int() > 0, "must be positive")
  .validate("email", fn (v) -> valid_email?(v.as_string()), "invalid format")
  .build(fn (obj) ->
    User{
      name: obj.get("name").unwrap().as_string(),
      age: obj.get("age").unwrap().as_int(),
      email: obj.get("email").unwrap().as_string(),
    }
  end)
```

How it works:

- `.require(field, type)` checks the field exists and has the expected JSON
  type. Passes through on success, appends a `DecodeError` on failure.
- `.validate(field, check, msg)` runs a domain validation predicate on an
  already-validated field. Appends an error on failure.
- `.build(construct)` returns `Result<T, List<DecodeError>>`. The construct
  closure only runs if all validations passed, so `unwrap()` / `as_string()`
  calls are guaranteed safe.

Error accumulation is natural: every step is independent, so all field
errors are collected in one pass. A single API response surfaces every
problem at once (missing fields, wrong types, domain violations).

The trade-off: the `build` closure uses `unwrap()` which the compiler
can't statically verify. This is the same safety model as Ecto changesets
-- validation ensures correctness, construction trusts it. The invariant
is easy to maintain because validation and construction are adjacent in
the same pipeline.

No new language features required. Uses enums, closures, `move self`
method chaining, generics, and `Result` -- all of which exist today.
Recursive enums inside generic containers (`List<JsonValue>`,
`Map<String, JsonValue>`) compile and run correctly.
