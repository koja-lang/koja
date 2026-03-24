# Project System & Module Unification

Design notes for A3b (project system + test runner) and the module unification
that emerged from working through it. These decisions are interconnected -- the
project system requires a coherent module/import model, and getting that right
requires unifying how modules and types resolve.

---

## Type algebra

### Data dimensionality: 0-D, 1-D, 2-D

Expo has three keywords for declaring named types. They differ not in
mechanism but in **data dimensionality**:

- **`type IO`** -- 0-D. No data. A point. Just a name with functions attached.
- **`enum Option<T>`** -- 1-D. One axis of variation: _which_ variant. A sum type.
- **`struct User`** -- 2-D. Multiple axes simultaneously: all fields at once. A product type.

All three are types. The keyword communicates the data shape. Functions (via
`impl`) are orthogonal -- they attach to any dimensionality. In type theory
terms: unit, coproduct, and product.

This maps directly to algebraic data types:

| Keyword  | Dimension | Algebra               | Role               |
| -------- | --------- | --------------------- | ------------------ |
| `type`   | 0-D       | 1 (identity)          | Empty namespace    |
| `enum`   | 1-D       | + (addition/sum)      | Choose one variant |
| `struct` | 2-D       | \* (multiply/product) | Combine all fields |

Higher dimensions compose from these primitives. "3-D" (sum of products) is
just an enum with struct variants -- already supported, no new keyword needed.
There is no 4-D because all algebraic data types are built from `+` and `*`,
the same way all polynomials are built from addition and multiplication.
The keyword set is algebraically complete.

This means:

- `IO.puts()`, `Option.some()`, `User.new()` are all the same operation:
  call a function owned by a type.
- The implementation should reflect this: one registry, one resolution path.
- The keywords exist for **human readers and documentation**, not because the
  mechanism differs.

### Named vs anonymous forms

Each algebraic operation has a named (declared) form and an anonymous (inline)
form, paralleling how functions have named and anonymous forms:

| Dimension     | Named (declared) | Anonymous (inline)                   |
| ------------- | ---------------- | ------------------------------------ |
| 0-D           | `type IO`        | `()` (unit literal)                  |
| 1-D (sum)     | `enum Option<T>` | `String \| Int` (union type)         |
| 2-D (product) | `struct User`    | _(deliberately absent -- no tuples)_ |
| Code          | `fn name(...)`   | `fn (...) -> ... end` (closure)      |

The anonymous product slot (tuples) is empty **by design**. Tuples let you
avoid naming fields, and unnamed fields degrade readability. `Pair<A, B>` with
`.first` / `.second` covers the 2-arity case. Anything beyond that should be a
struct with named fields.

Note: tuples aren't quite the symmetric counterpart to unions anyway. A union
uses _types_ as discriminators (`s: String`); the symmetric anonymous product
would use _types_ as accessors, not positions. This type-indexed product breaks
when two fields share a type, making it impractical. The hole is unfillable
with something clean, which reinforces the decision to leave it empty.

### Union types and `impl`

Union types (`type Stringish = String | Int`) are the anonymous form of enums.
The analogy is precise:

    union type : enum :: closure : function

A closure is an anonymous function. A union type is an anonymous sum type -- a
sum without named variants, where existing types serve as discriminators.

Currently `type Name = A | B` is a transparent alias. For union types to
support `impl` (a roadmap requirement), they must become real entries in the
type registry. The unified model forces this: if everything resolves through
`ctx.types`, then union types must be proper types too.

The `type` keyword handles three cases, distinguished by syntax:

- `type IO` (no `=`) -- 0-D declaration (namespace for functions)
- `type NodeId = UInt64` (single type after `=`) -- alias (transparent redirect)
- `type Stringish = String | Int` (union after `=`) -- union type (real entry,
  supports `impl`)

### `type` as the 0-D declaration

`type IO` declares a zero-dimensional type -- a named namespace for functions.
No new keyword required; `type` already exists for aliases. The parser
disambiguates: `type Name =` is an alias or union, `type Name` (no `=`) is a
declaration.

```expo
type IO

impl IO
  fn puts(message: String)
    print(message)
    print("\n")
  end
end
```

`IO.puts("hello")` resolves through the same path as `List.map(items, f)`.

### Documentation categorization

Generated docs categorize by data shape, not by a generic "type" label:

- **Structs**: List, Request, User (types with fields)
- **Enums**: Option, Result (types with variants)
- **Modules**: IO, Math, Path (types with no data shape, functions only)

The doc generator knows the difference from the type's shape in the registry.

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

- `ctx.types["IO"]` = `TypeKind::Struct { fields: [] }` (0-D type)
- `ctx.types["List"]` = `TypeKind::Struct { fields: [...] }` (2-D type)
- `ctx.types["Option"]` = `TypeKind::Enum { variants: [...] }` (1-D type)

`imported_modules` is removed. All qualified calls resolve through:

```
ctx.types.get(name)?.functions.get(method)
```

Everything is PascalCase. `IO.puts()`, `List.map()`, `Option.some()`.

### What changes

- **`TypeKind`**: no new variant needed. `TypeKind::Struct { fields: vec![] }`
  covers the 0-D case. Alternatively, add `TypeKind::Module` purely for doc
  generation purposes (the resolution path is identical either way).
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
  source declares: type IO
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

The conceptual model says modules, structs, and enums are all namespaces that
own functions. The implementation should have one registry and one resolution
path -- not two parallel mechanisms that happen to do the same thing.

### "A module should represent one idea" (instantaneous complexity)

Each type is one idea. `struct User` is the idea of a user. `enum Option<T>` is
the idea of optional values. `type IO` is the idea of I/O operations. Files are
containers for ideas; types are the ideas themselves.

The namespace boundary is the **idea** (the type), not the file, not the package.
Files organize code on disk. Types organize code in the program.

### Fractal consistency

At every scope, the same pattern applies:

- A type owns functions: `Type.function()`
- A function does one thing
- A project has one role

The qualified-call syntax `Name.function()` works identically regardless of
whether `Name` is a struct, enum, or module. No special cases.

---

## Open questions

- **`impl` across files**: can `impl IO` appear in a different file from
  `type IO`? Currently `impl` can extend any type. This should continue to
  work -- a type is declared in one file, but `impl` blocks can appear anywhere
  (protocol conformance, extensions).
- **Multiple types per file**: a file can define multiple types. When you
  `import` that file, all its public types come into scope. No restriction
  on one-type-per-file.
- **Entry file**: the entry module (`main.expo`) may have top-level statements
  (the program body). How does this interact with the "everything is a type"
  model? The entry file is special -- it runs code, not just declares types.
- **Circular imports**: currently banned by cycle detection. With the unified
  registry, circular type references could theoretically be handled the same
  way as recursive struct types (`Type::Indirect`), but this adds significant
  complexity and is not needed for A3b.
- **Test runner**: `@test` annotated functions with `expo test`. Discovery
  scans `test` dirs from `project.expo`. Implementation details deferred to
  the implementation phase.
