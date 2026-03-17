# Expo Language Reference

Expo is a statically typed, compiled language targeting native binaries via LLVM. It combines Ruby-inspired syntax with Rust-grade ownership semantics and an Erlang-style concurrency model. The compiler is a Rust workspace; the language compiles to native code with no runtime garbage collector.

See [MEMORY.md](MEMORY.md) for the full ownership and memory strategy. See [CONCURRENCY.md](CONCURRENCY.md) for the task and actor concurrency model.

---

## Lexical Structure

### Comments

Line comments start with `#` and extend to the end of the line. There are no block comments.

```expo
# This is a comment
x = 42  # inline comment
```

### Identifiers

- **Values** use `snake_case`: variables, functions, parameters, fields.
- **Types** use `PascalCase`: structs, enums, protocols, type parameters, primitives.
- Identifiers may contain `?` (conventionally for boolean-returning functions like `empty?()`, `some?()`).

### Keywords

```
and, arena, await, break, cond, const, else, end, enum, false, fn, for,
if, impl, import, in, loop, match, move, not, or, priv, protocol,
receive, return, self, shared, spawn, struct, true, unless, when
```

`or` and `and` are valid as function and field names after `.` (e.g. `x.or(default)`).

### Operators

Precedence from lowest to highest:

| Precedence | Operators                   |
| ---------- | --------------------------- |
| 1          | `or`                        |
| 2          | `and`                       |
| 3          | `not` (prefix)              |
| 4          | `==` `!=` `<` `>` `<=` `>=` |
| 5          | `+` `-`                     |
| 6          | `*` `/` `%`                 |
| 7          | `-` (unary negation)        |
| 8          | `.field` `.fn()` `()`       |

Assignment operators: `=`, `+=`, `-=`, `*=`, `/=`.

### Numeric Literals

```expo
42          # decimal integer
3.14        # floating point
0xFF        # hexadecimal
0b1010      # binary
1_000_000   # underscore separators (ignored)
0xFF_FF     # underscores in hex
```

Numeric literals coerce to any same-category type annotation. Integer literals coerce to any integer type (`x: UInt8 = 4`). Float literals coerce to any float type (`f: Float32 = 3.14`). Cross-category coercion (int to float or vice versa) is an error.

### Line Continuation

Newlines terminate statements. Line continuation is implicit after binary operators, `.`, and `,`.

---

## Types

### Primitive Types

| Type      | Description                               |
| --------- | ----------------------------------------- |
| `Int`     | 64-bit signed integer (alias for `Int64`) |
| `Int8`    | 8-bit signed integer                      |
| `Int16`   | 16-bit signed integer                     |
| `Int32`   | 32-bit signed integer                     |
| `UInt8`   | 8-bit unsigned integer                    |
| `UInt16`  | 16-bit unsigned integer                   |
| `UInt32`  | 32-bit unsigned integer                   |
| `UInt64`  | 64-bit unsigned integer                   |
| `Float`   | 64-bit IEEE 754 (alias for `Float64`)     |
| `Float32` | 32-bit IEEE 754                           |
| `Bool`    | `true` or `false`                         |
| `String`  | UTF-8 string                              |
| `()`      | Unit type (empty value)                   |

All primitives and `Bool` are **copy types** -- assignment duplicates the value. `String`, structs, and enums are **move types** -- assignment transfers ownership.

### Unit Expression

`()` is the unit value. Use `else -> ()` in `cond` for side-effect-only fallthrough.

---

## Variables and Assignment

Variables are declared by assignment. No `let`, `var`, or `mut` keywords.

```expo
x = 42
name = "expo"
```

### Type Annotations

Optional type annotations follow the variable name with a colon:

```expo
x: Int32 = 42
z: Option<Int32> = Option.None
list: List<Int32> = List.new()
```

Annotations are required when the type cannot be inferred (e.g. generic enum unit variants like `Option.None`).

### Compound Assignment

```expo
x += 1
x -= 2
x *= 3
x /= 4
```

### Ownership on Assignment

Assignment moves ownership for non-copy types. The original variable is no longer usable:

```expo
p1 = Point{x: 1, y: 2}
p2 = p1
# p1 is moved -- using it here is a compile error
```

Reassignment brings a variable back to live:

```expo
p1 = Point{x: 3, y: 4}  # p1 is live again
```

---

## Functions

Functions are declared with `fn`. The last expression is the implicit return value.

```expo
fn add(a: Int32, b: Int32) -> Int32
  a + b
end
```

Functions without a return type return `()`. Parameters require explicit types. Return type annotation is required if the function returns a value.

### Private Functions

`priv fn` makes a function inaccessible from other modules:

```expo
priv fn helper(x: Int32) -> Int32
  x * 2
end
```

### `return`

Explicit `return` is available for early exits:

```expo
fn find(items: List<Int32>, target: Int32) -> Bool
  for item in items
    if item == target
      return true
    end
  end
  false
end
```

### Parameters and Ownership

Parameters borrow by default (read-only). Use `move` to take ownership:

```expo
fn borrow(c: Config) -> String
  c.name                 # read-only access
end

fn consume(move c: Config) -> String
  c.name                 # owns c, caller loses access
end
```

`move` only appears in the function signature, never at the call site. The compiler infers moves from the callee's signature.

---

## Control Flow

### `if` / `else`

```expo
if x > 3
  print("greater")
else
  print("not greater")
end
```

`if`/`else` can be used as value-producing expressions when both branches produce values.

### `while`

```expo
i = 0
while i < 10
  print(i)
  i += 1
end
```

### `loop` / `break`

```expo
i = 0
loop
  if i >= 5
    break
  end
  i += 1
end
```

### `for` ... `in`

Iterates over any type implementing the `Enumerable<T>` protocol:

```expo
list: List<Int32> = List.new()
list = list.push(1)
list = list.push(2)
list = list.push(3)

for item in list
  print(item)
end
```

Desugars to an indexed `while` loop using `Enumerable`'s `length` and `get` functions.

### `match`

Pattern matching with exhaustiveness checking:

```expo
result = match x
  1 -> "one"
  2 -> "two"
  _ -> "other"
end
```

Patterns: literals, wildcards (`_`), variable bindings, nested patterns, enum variant destructuring. Guards use `when`:

```expo
match x
  Option.Some(v) when v > 5 -> "big"
  Option.Some(_) -> "small"
  Option.None -> "none"
end
```

`match` is value-producing when all arms produce values.

### `cond`

Multi-branch conditional, like a chain of `if`/`else if`. Requires `else` arm:

```expo
fn classify(n: Int32) -> String
  cond
    n > 100 -> "big"
    n > 10 -> "medium"
    else -> "small"
  end
end
```

`cond` is value-producing when all arms (including `else`) produce values.

### Ternary

```expo
y = x > 2 ? "big" : "small"
```

Nested ternaries are disallowed.

---

## Strings

### Single-Line Strings

```expo
"hello world"
"tab:\there"
"quote: \"yes\""
"backslash: \\"
```

Escape sequences: `\"`, `\\`, `\n`, `\t`, `\#`.

### String Interpolation

```expo
name = "expo"
print("hello #{name}")
print("1 + 2 = #{1 + 2}")
```

Interpolation expressions are enclosed in `#{}` and can contain any expression.

### Multiline Strings

Triple-quoted strings with automatic dedent based on closing delimiter position:

```expo
msg = """
  first line
  second line
  """
```

Multiline strings support the same escape sequences and interpolation as single-line strings.

---

## Structs

### Declaration

```expo
struct Point
  x: Int32
  y: Int32
end
```

### Construction

```expo
p = Point{x: 1, y: 2}
```

Short structs format inline. Long structs break across lines with trailing commas:

```expo
config = Config{
  name: "production",
  port: 8080,
  debug: false,
}
```

### Field Access

```expo
print(p.x)
print(p.y)
```

### Impl Functions

Attach functions to a type via `impl` blocks:

```expo
impl Point
  fn distance_squared(self) -> Int32
    self.x * self.x + self.y * self.y
  end
end

print(p.distance_squared())
```

`self` borrows by default (read-only). Use `move self` for mutating functions that return the modified value:

```expo
impl List<T>
  fn push(move self, item: T) -> List<T>
    # owns self, can mutate
    self
  end
end

list = list.push(42)  # move in, get back
```

### Static Functions

Functions in `impl` blocks without `self` are called on the type directly:

```expo
impl List<T>
  fn new() -> List<T>
    # ...
  end
end

list: List<Int32> = List.new()
```

---

## Enums

### Variants

Enums support unit, tuple, and struct variants:

```expo
enum Direction
  North
  South
  East
  West
end

enum Shape
  Circle(Int32)
  Rect(Int32, Int32)
end
```

### Construction

```expo
d = Direction.North
s = Shape.Circle(5)
```

Within a `match` arm on the same enum, the type prefix can be omitted for unit variants:

```expo
fn opposite(dir: Direction) -> String
  match dir
    North -> "south"
    South -> "north"
    East -> "west"
    West -> "east"
  end
end
```

### Pattern Matching

```expo
fn area(s: Shape) -> Int32
  match s
    Shape.Circle(r) -> r * r * 3
    Shape.Rect(w, h) -> w * h
  end
end
```

---

## Generics

### Generic Functions

```expo
fn identity<T>(x: T) -> T
  x
end

print(identity(42))
print(identity("hello"))
```

Type arguments are inferred at call sites from arguments and type annotations.

### Generic Structs

```expo
struct Pair<A, B>
  first: A
  second: B
end

p = Pair{first: 10, second: "hello"}
```

### Generic Enums

```expo
enum Option<T>
  Some(T)
  None
end
```

Generic enum unit variants require a type annotation for inference:

```expo
z: Option<Int32> = Option.None
```

### Annotation-Driven Inference

Type annotations on variables drive generic type inference:

```expo
list: List<Int32> = List.new()  # infers T = Int32
```

### Implementation

Generics compile via monomorphization -- the compiler generates specialized native code for each concrete type instantiation. Unused instantiations produce no binary output.

---

## Protocols

Protocols define behavioral contracts. Types implement protocols via `impl Protocol for Type`.

```expo
protocol Display
  fn display(self) -> String
end

struct Point
  x: Int32
  y: Int32
end

impl Display for Point
  fn display(self) -> String
    "Point"
  end
end
```

The compiler validates completeness (all protocol functions must be implemented) and signature compatibility. `priv fn` helpers are allowed in impl blocks. `@doc` annotations are supported on protocol declarations.

### Dispatch

Protocol dispatch is static via monomorphization -- no vtables, no dynamic dispatch.

---

## Closures

### Block Closures

Closures use `fn (...) -> T ... end` syntax, mirroring function signatures:

```expo
double = fn (x: Int32) -> Int32 x * 2 end

add = fn (a: Int32, b: Int32) -> Int32
  a + b
end
```

### Capture Semantics

Closures capture variables from their enclosing scope:

- Copy types (primitives, `Bool`) are duplicated.
- Move types (structs, enums, `String`) are moved -- the original variable is consumed.

```expo
multiplier = 3
triple = fn (x: Int32) -> Int32
  x * multiplier    # multiplier is copied (Int32 is a copy type)
end
```

Captured closures use heap-allocated environment structs that are automatically freed when the closure goes out of scope.

---

## Function Types

Function types are written as `fn(ParamTypes) -> ReturnType`:

```expo
fn apply(x: Int32, f: fn(Int32) -> Int32) -> Int32
  f(x)
end

print(apply(5, fn (n: Int32) -> Int32 n * 2 end))
```

### `move` in Function Types

`fn(T) -> U` borrows `T`. `fn(move T) -> U` takes ownership of `T`:

```expo
fn map<U>(move self, f: fn(move T) -> U) -> Option<U>
```

---

## Ownership and Borrowing

Expo uses single-owner move semantics with borrow-by-default function parameters. There is no garbage collector, no `Box`, `Rc`, or `Arc` in user code, and no lifetime annotations.

### Rules

1. Every heap-allocated value has exactly one owner.
2. Assignment **moves** ownership for non-copy types. The source becomes unusable.
3. Function parameters **borrow by default** (read-only). Use `move` to take ownership.
4. Borrows are always read-only. There is no `&mut T`.
5. `move` only appears in function/closure signatures, never at call sites.
6. When the owner goes out of scope, the value is dropped (memory freed).

### `clone()`

`clone()` is available on all types. It produces a new owned copy without moving the original:

```expo
p = Point{x: 10, y: 20}
q = p.clone()
consume(q)    # q is moved
print(p.x)    # p is still live
```

### Drop Insertion

The compiler inserts deterministic cleanup at scope boundaries. `List<T>` backing buffers and captured closure environments are freed automatically.

### Copy Types

All numeric primitives, `Bool`, `()`, and function pointers are copy types. Assignment duplicates the value:

```expo
a = 42
b = a     # a is still live
```

See [MEMORY.md](MEMORY.md) for the full ownership and memory strategy.

---

## Constants

Module-level constants are declared with `const`. Values must be compile-time literals (int, float, string, bool):

```expo
const MAX = 100
const PI = 3.14
const NAME = "expo"
const DEBUG = false
```

Constants are inlined at every usage site.

---

## Modules and Imports

Each file is a module. Import with `import`:

```expo
import helper

fn main
  print(helper.add(3, 4))
end
```

Nested modules use dot-separated paths: `import what.util` resolves to `what/util.expo`.

Functions can be called qualified (`helper.add(3, 4)`) or unqualified (`add(3, 4)`) if imported. Duplicate names from different imports produce a compile error.

### Visibility

All modules are importable. Access control is at the function level (`priv fn`), not the module level. Use `@moduledoc false` to signal "internal, don't depend on this."

---

## Annotations

### `@doc`

Documents a function, struct, or enum:

```expo
@doc "Adds two integers."
fn add(a: Int32, b: Int32) -> Int32
  a + b
end
```

`@doc false` excludes an item from generated documentation.

### `@moduledoc`

Documents a module (placed at the top of the file):

```expo
@moduledoc "Math utility functions."
```

`@moduledoc false` excludes the entire module from documentation.

Doc strings support Markdown and are rendered by `expo doc`.

---

## Standard Library

The following types are auto-imported from `std.kernel` into every module.

### `Option<T>`

```expo
enum Option<T>
  Some(T)
  None
end
```

Functions: `unwrap()`, `or(default)`, `some?()`, `none?()`, `map(fn(move T) -> U)`, `then(fn(move T) -> Option<U>)`.

```expo
x = Option.Some(42)
print(x.unwrap())       # 42
print(x.or(0))          # 42
print(x.some?())        # true

y: Option<Int32> = Option.None
print(y.or(99))          # 99

mapped = x.map(fn (v: Int32) -> Int32 v * 10 end)
print(mapped.unwrap())   # 420
```

### `Result<T, E>`

```expo
enum Result<T, E>
  Ok(T)
  Err(E)
end
```

Functions: `unwrap()`, `or(default)`, `ok?()`, `err?()`, `map(fn(move T) -> U)`, `then(fn(move T) -> Result<U, E>)`.

```expo
ok: Result<Int32, Int32> = Result.Ok(42)
print(ok.unwrap())       # 42

err: Result<Int32, Int32> = Result.Err(1)
print(err.or(99))        # 99
```

### `Pair<A, B>`

```expo
struct Pair<A, B>
  first: A
  second: B
end
```

Fields: `first`, `second`.

```expo
p = Pair{first: 10, second: "hello"}
print(p.first)    # 10
print(p.second)   # hello
```

### `List<T>`

Dynamically-sized, heap-backed collection. Compiler intrinsic backed by C's `malloc`/`realloc`/`free`.

```expo
list: List<Int32> = List.new()
list = list.push(10)
list = list.push(20)

print(list.length())   # 2
print(list.get(0))     # 10
print(list.empty?())   # false
```

`push` uses `move self` semantics -- it returns the updated list. Out-of-bounds `get` panics.

### `Enumerable<T>` Protocol

```expo
protocol Enumerable<T>
  fn length(self) -> Int32
  fn get(self, index: Int32) -> T
end
```

Any type implementing `Enumerable<T>` can be used with `for` loops. `List<T>` implements this protocol.

---

## Built-in Functions

### `print()`

Polymorphic print function. Supports all primitive types. Outputs to stdout with a trailing newline.

```expo
print(42)
print("hello")
print(true)       # prints "true", not "1"
```

### `panic()`

Prints a message to stderr and aborts the process:

```expo
panic("something went wrong")
```

Used internally by `unwrap()` on `Option.None` and `Result.Err`.

### `clone()`

Available on all types. Produces a new owned value:

```expo
copy = original.clone()
```

---

## Planned Features

The following features are designed but not yet compiled to native code. They are parsed and/or type-checked but await codegen implementation.

### Tasks (Structured Concurrency)

Lightweight concurrent computations. Stackless state machines with structured lifetimes -- tasks cannot outlive their spawner.

```expo
handle = spawn fn -> fetch_user(id) end
user = await handle
```

Tasks can borrow from the parent scope (zero-copy reads) because structured concurrency guarantees the parent outlives the task. See [CONCURRENCY.md](CONCURRENCY.md).

### Actors

Long-lived concurrent entities with isolated memory, typed mailboxes, and supervision. The building block for stateful services.

```expo
actor Counter
  state count: Int32 = 0

  receive Increment ->
    @count += 1
  end
end
```

Messages are moved (ownership transfer, zero-copy). Actors are preemptively scheduled by a work-stealing runtime. See [CONCURRENCY.md](CONCURRENCY.md).

### `arena` Blocks

Bump-allocated regions with bulk-free semantics:

```expo
result = arena
  # all allocations in here are bulk-freed at block exit
  # only explicitly cloned values escape
end
```

### `Map<K, V>` and `Set<T>`

Built-in generic collection types backed by native implementations. Will implement `Enumerable` for use with `for` loops.

### Inline Closures

Short closure syntax with inferred parameter types:

```expo
option.map(x -> x + 1)
```

Requires closure-specific type inference from calling context. Parsed but not compiled.

### `Display` Protocol

Auto-derived string representation for all types. `print()` will dispatch through `Display` instead of hardcoding format specifiers per type.

### Literal Protocols

All literal syntax backed by protocols (`FromInt`, `FromFloat`, `FromString`, `FromList<T>`, `FromEntries<K,V>`, `FromPair<A,B>`). Any type can opt into literal construction by implementing the protocol.

### Struct Destructuring

Irrefutable struct destructuring on assignment:

```expo
Config{name, port} = load_config()
```

Compile-time verified exhaustive. Enum destructuring uses `match`.

### Trait Bounds

Bounds on generic type parameters:

```expo
fn foo<T: Display>(x: T) -> String
  x.display()
end
```

### `command` Construct

Language-native typed pipelines for backend business logic with step-ordered type safety and exhaustive data flow checking. See [ROADMAP.md](ROADMAP.md).

---

## Tooling

| Command       | Description                                       |
| ------------- | ------------------------------------------------- |
| `expo build`  | Compile to a native binary via LLVM               |
| `expo run`    | Build and execute in one step                     |
| `expo check`  | Type check without compiling                      |
| `expo format` | Opinionated code formatter (`--write`, `--check`) |
| `expo doc`    | Generate static HTML documentation                |
| `expo lex`    | Dump tokens                                       |
| `expo parse`  | Dump AST                                          |

### Language Server (LSP)

Real-time diagnostics, document formatting, hover (type signatures + `@doc`), and go-to-definition. Integrates with VS Code / Cursor via a bundled extension.

### Formatter

Zero-config, opinionated. `expo format --write` reformats in place, `expo format --check` exits non-zero if formatting differs. The formatter handles escape re-encoding for round-trip correctness and preserves annotations.
