# Koja Language Reference

Koja is a statically typed, compiled language targeting native binaries via LLVM, with no garbage collector. It combines a Rust-inspired type system, Swift-style value semantics, an Erlang-style concurrency model, and Ruby-inspired syntax. The compiler itself is implemented as a Rust workspace.

---

## Table of Contents

- [Lexical Structure](#lexical-structure): Comments, Identifiers, Keywords, Operators, Numeric Literals, Line Continuation
- [Variables and Constants](#variables-and-constants): Assignment, Type Annotations, Compound Assignment, Constants
- [Value Semantics](#value-semantics): Rules, Copy Cost
- [Functions](#functions): Declaration, Parameters, `return`, Private Declarations
- [Closures and Function Types](#closures-and-function-types): Block Closures, Short Closures, Capture Semantics, Function Types
- [Control Flow](#control-flow): `if`/`else`, `unless`, `while`, `loop`/`break`, `for`...`in`, Ternary, `cond`, Definite Assignment
- [Types](#types): Primitives, Builtin Declarations, Numeric Widening, Arithmetic Faults, Unit, Strings, Structs, Enums, Nested Types, Union Types, Tuples, Generics
- [Pattern Matching](#pattern-matching): `match`, OR Patterns
- [Error Handling](#error-handling): `! E` Signatures, `fail`, `try`, Error Unions, `rescue`
- [Protocols](#protocols): Behavioral Contracts, Impl Blocks, Static Dispatch
- [Packages](#packages): Transparent Files, Visibility, Aliases, Dependencies
- [Concurrency](#concurrency): `Task`, Processes, Lifecycle, `Ref`, `ReplyTo`, `spawn`/`receive`
- [Annotations](#annotations): `@deprecated`, `@doc`, `@test`
- [C FFI](#c-ffi): `@extern "C"`, `CPtr<T>`, `CString`
- [Standard Library](#standard-library): Core Types, Collections, String Functions, Binary/Bits, File I/O, Parsing, URI, Base, Path, Protocols
- [Tooling](#tooling): CLI Commands, Custom Tasks, LSP, Formatter

---

## Lexical Structure

### Comments

Line comments start with `#` and extend to the end of the line. There are no block comments.

```koja
# This is a comment
x = 42 # inline comment
```

### Identifiers

- **Values** use `snake_case`: variables, functions, parameters, fields.
- **Types** use `PascalCase`: structs, enums, protocols, type parameters, primitives.
- Identifiers may contain `?` (conventionally for boolean-returning functions like `empty?()`, `some?()`).

### Keywords

```
after, alias, break, builtin, cond, const, else, end, enum, extend,
fail, false, fn, for, if, impl, in, loop, match, not, priv,
protocol, receive, rescue, return, self, spawn, struct, true, try,
type, unless, when, while
```

`and` and `or` are operator-identifiers, not reserved keywords. They act as infix boolean operators in expressions (`a and b`, `x or y`) but can also be used as function or field names (for example, `option.or(default)`).

### Operators

Precedence from lowest to highest:

| Precedence | Operators                   |
| ---------- | --------------------------- |
| 1          | `rescue`                    |
| 2          | `or`                        |
| 3          | `and`                       |
| 4          | `not` (prefix)              |
| 5          | `==` `!=` `<` `>` `<=` `>=` |
| 6          | `+` `-` `<>`                |
| 7          | `*` `/` `%`                 |
| 8          | `-` (unary negation)        |
| 9          | `.field` `.fn()` `()`       |

`and` and `or` evaluate left to right and short-circuit. `a and b`
evaluates `b` only when `a` is `true`. `a or b` evaluates `b` only when
`a` is `false`. Both operands are still typechecked as `Bool`.

`<>` concatenates `String`, `Binary`, and `Bits` values. Both operands must be the same type, with no cross-type mixing.

Assignment operators: `=`, `+=`, `-=`, `*=`, `/=`.

### Numeric Literals

```koja
42 # decimal integer
3.14 # floating point
0xFF # hexadecimal
0b1010 # binary
1_000_000 # underscore separators (ignored)
0xFF_FF # underscores in hex
```

Numeric literals coerce to any same-category type annotation. Integer literals coerce to any integer type (`x: UInt8 = 4`). Float literals coerce to any float type (`f: Float32 = 3.14`). Cross-category coercion (int to float or vice versa) is an error. Non-literal sized values widen implicitly into `Int` / `Float`. See [Numeric Widening](#numeric-widening).

A literal must fit its type. An integer literal outside the target's range is a compile-time error, and so is a float literal whose magnitude is too large for a 64-bit float (one that would round to infinity). `Float` values are always finite (see [Arithmetic Faults](#arithmetic-faults)).

### Line Continuation

Newlines terminate statements. Line continuation is implicit after binary operators, `.`, and `,`. A line starting with `and`, `or`, `rescue`, or the ternary `?` also continues the previous expression, so wrapped conditions lead each continuation line with the operator.

```koja
if request.valid? and request.authorized? and request.body.present?
  and request.rate_limit_ok?

  handle(request)
end
```

---

## Variables and Constants

Assignment creates a variable or rebinds an existing variable. There are no `let`, `var`, or `mut` keywords. See [Value Semantics](#value-semantics) for copy behavior.

```koja
x = 42
name = "koja"
```

A variable must be assigned before it is read. See [Definite Assignment](#definite-assignment) for control-flow rules.

### Type Annotations

Optional type annotations follow the variable name with a colon:

```koja
x: Int32 = 42
z: Option<Int32> = Option.None
list: List<Int32> = List.new()
```

Annotations are required when no surrounding context determines the type,
such as a bare `Option.None` assignment.

### Compound Assignment

```koja
x += 1
x -= 2
x *= 3
x /= 4
```

### Constants

Package-level constants are declared with `const`. Values can be literals (int, float, string, bool), binary literals whose segments are all literals, enum unit variants, or struct literals whose fields are all constant expressions:

```koja
const MAX = 100
const PI = 3.14
const NAME = "koja"
const DEBUG = false
const SYNC = <<0x53::8, 4::32>>
const HEADING = Direction.North
const ORIGIN = Point{x: 0, y: 0}
```

An optional type annotation is supported for generic inference:

```koja
const EMPTY: Option<Int> = Option.None
```

Constants are inlined at every usage site.

Within a package, constants are read by bare name (`MAX`). Constants from the auto-imported `Global` package also resolve bare (`STDOUT`). Public constants in other packages are read through the package namespace (`Mathlib.PI`).

---

## Value Semantics

Koja uses value semantics. Every binding, parameter, return, and field is an independent value, with memory managed automatically by the runtime. There are no moves, borrows, or lifetimes. Using a value never invalidates it.

### Rules

1. Assignment copies.
2. Function and closure parameters are passed by value.
3. There is no aliasing. Mutating one binding never affects another.
4. A value is usable for as long as it is in scope.

> Memory note: heap-backed values (strings, collections, composites) are reclaimed by reference counting. Blocks are shared while live and freed deterministically at scope exit when the last owner drops. This is scope-bound, not a garbage collector. There are no pauses and no background collector. See the README for production-readiness status.

### Copy Cost

All types copy on assignment, and the result is always an independent value. What a copy costs depends on the representation:

- Numeric primitives, `Bool`, `()`, and function pointers copy bit-for-bit.
- `String`, `Binary`, and `Bits` share one reference-counted buffer, so a copy costs nothing regardless of size.
- Structs and enums copy their top-level fields, and each heap-backed field follows these same rules. Recursive constituents live in reference-counted boxes that copies share, so copying a persistent tree touches only the root and an update touches only the changed path, never the whole structure.
- `List`, `Map`, and `Set` copy their backing buffer, so a collection copy is O(n) today. The LLVM backend skips the copy when the old value provably dies at the mutation site. Thus, compiled `xs = xs.append(x)` rebind loops build a collection in linear time. The interpreter preserves the same behavior but can still copy each loop iteration.

None of this is observable in behavior. Mutation always builds the mutated binding's own value, no binding ever observes another's changes, and a copy is always an independent value:

```koja
a = 42
b = a # b is an independent copy
```

---

## Functions

Functions are declared with `fn`. The last expression is the implicit return value.

```koja
fn add(a: Int32, b: Int32) -> Int32
  a + b
end
```

Functions without a return type return `()`. Parameters require explicit types. Return type annotation is required if the function returns a value.

A fallible function declares an error type after `!`, as in `-> Int ! ParseError`. This is notation for returning `Result<Int, ParseError>`. See [Error Handling](#error-handling).

A compiled program's entry point is a type implementing the `Process` protocol, named by `entry` in `koja.toml`. There is no `fn main`. Scripts (`.kojs`) execute top-level statements directly. Functions may be declared at the top level or inside struct, enum, and `impl` bodies. See [Structs](#structs), [Protocols](#protocols), and [Static Functions](#static-functions).

### Parameters

Parameters are passed by value. The callee receives its own independent copy of each argument:

```koja
fn describe(c: Config) -> String
  c.name # operates on the callee's own copy
end
```

There is no parameter-passing modifier. Every parameter is a value.

### Default Parameters

A parameter can declare a default value with `=`. Required parameters must come before defaulted ones:

```koja
fn greet(name: String, punctuation: String = "!") -> String
  name <> punctuation
end

greet("Koja").print()
greet("Koja", "?").print()
```

A function with defaults is callable at every arity from its required parameter count through its total parameter count. The compiler builds adapter functions for omitted trailing arguments.

Default expressions are independent callee-scope expressions. They cannot refer to `self` or any parameter in the same declaration.

Protocol declarations own defaults. Implementations inherit the callable arities and cannot repeat or redefine them.

Two declarations with the same qualified name collide when they share an arity, even if the parameter types differ. Separate declarations may share a name only when their arities differ, and their default arity ranges must not overlap:

```koja
fn pick(x: Int) -> Int
  x
end

fn pick(x: Int, y: Int) -> Int
  x + y
end
```

See [Function Arity](design/FUNCTION-ARITY.md) for the full model.

### `return`

Explicit `return` is available for early exits:

```koja
fn find(items: List<Int32>, target: Int32) -> Bool
  for item in items
    if item == target
      return true
    end
  end

  false
end
```

`return` is a statement. It cannot appear inside another expression.

Every explicit `return` is typechecked against the declared return type with the same rules as the trailing expression, including numeric literal coercion (`return 5` in a `-> Int8` function produces an `Int8`). A bare `return` in a function that declares a return type is an error, and `return <value>` in a function that returns `Unit` is an error. A `return` whose value diverges (such as `return Kernel.panic("boom")`) is accepted in any function.

Scripts (`.kojs`) have no return channel. A bare `return` at the top level ends the script early as a normal exit (exit code 0), while `return <value>` is a compile error. Use `Kernel.exit(code)` to set an exit code, or print the value.

```koja
if args.empty?()
  IO.puts("nothing to do")
  return
end
```

### Private Declarations

`priv` restricts a declaration's visibility based on where it appears:

- A top-level `priv` declaration (`fn`, `struct`, `enum`, `const`, `type`,
  `protocol`) is **package-private**: it's usable from any file in the same
  package, but rejected from any other package.
- A `priv fn` declared inside a `struct`, `enum`, or `impl` body is
  **type-private**: it's callable from any other function on the same target
  type (whether declared in the type's decl block, an `extend Type` block,
  or an `impl Protocol for Type` block), but rejected everywhere else.

```koja
priv fn helper(x: Int32) -> Int32 # package-private
  x * 2
end

priv const RETRY_LIMIT: Int32 = 3 # package-private

priv struct Bucket # package-private
  count: Int32
end

struct Counter
  value: Int32

  fn increment(self) -> Counter
    Counter{value: self.tick()} # ok: same type
  end

  priv fn tick(self) -> Int32 # type-private to Counter
    self.value + 1
  end
end
```

A public declaration cannot leak a private type through its signature. A public function whose parameter or return type names a private type, or a public struct field, enum variant payload, type alias, or protocol function that mentions one, is a compile error. Callers outside the package could see the type but never name it, so the compiler rejects the leak at the declaration site.

`@doc` on a private declaration is also a compile error. Private items never appear in generated documentation, so use regular `#` comments instead.

---

## Closures and Function Types

### Block Closures

Closures use `fn (...) -> T ... end` syntax, mirroring function signatures:

```koja
double = fn (x: Int32) -> Int32 x * 2 end

add =
  fn (a: Int32, b: Int32) -> Int32
    # the last expression is the return value
    a + b
  end
```

Closure parameters are passed by value, like function parameters:

```koja
measure = fn (data: String) -> Int data.length() end
```

### Short Closures

Short closures use `param -> expr` syntax as direct call arguments, with parameter types inferred from the call:

```koja
option.map(x -> x + 1)
list.filter(n -> n > 3)
names.map(name -> name.upcase())
```

Both positional and named arguments accept the short form, including arguments to generic functions. Use the block form outside a call argument or when the closure needs multiple parameters or statements.

### Capture Semantics

Closures capture variables from their enclosing scope by value. Each captured variable is copied into the closure's environment when the closure is created, so later rebinding does not affect the closure's copy:

```koja
multiplier = 3

triple =
  fn (x: Int) -> Int
    x * multiplier # captures a copy of multiplier
  end

multiplier = 10 # does not affect triple
triple(5).print() # 15
```

Captured closures use heap-allocated environment structs.

### Function Types

Function types are written as `fn (ParamTypes) -> ReturnType`:

```koja
fn apply(x: Int32, f: fn (Int32) -> Int32) -> Int32
  f(x)
end

apply(5, fn (n: Int32) -> Int32 n * 2 end).print()
```

### Named Functions as Values

A named function reference uses `&name/arity`. The arity includes `self`.

```koja
fn double(x: Int) -> Int
  x * 2
end

f = &double/1 # same package
g = &Mathlib.square/1 # another package
h = &Counter.increment/2 # unbound instance function: fn (Counter, Int) -> Counter
t = &Point.translate/3 # static method: fn (Point, Int, Int) -> Point
apply(5, f).print()
```

Every named function value uses mandatory `&name/arity`, including a single-arity function. A bare function name is not a function value.

The arity selects one exact overload, including an adapter for default parameters. Generic functions cannot be referenced directly because there is no call site to infer their type arguments. Wrap a generic or adapted call in a closure.

---

## Control Flow

### `if` / `else`

```koja
if x > 3
  "greater".print()
else
  "not greater".print()
end
```

`if`/`else` can be used as value-producing expressions when both branches produce values.

There is no `else if`. For multi-way branching, use [`cond`](#cond).

### `unless`

`unless` executes its body when the condition is `false`. It is a
single-branch conditional and does not accept `else`.

```koja
unless ready?
  "not ready".print()
end
```

### `while`

```koja
i = 0

while i < 10
  i.print()
  i += 1
end
```

### `loop` / `break`

```koja
i = 0

loop
  if i >= 5
    break
  end

  i += 1
end
```

`break` is a statement. It cannot appear inside another expression.

### `for` ... `in`

Iterates over any type that implements `Enumeration<T, Cursor>`:

```koja
list: List<Int32> = List.new()
list = list.append(1)
list = list.append(2)
list = list.append(3)

for item in list
  item.print()
end
```

The loop variable binds directly to each element. The source stays unchanged while a separate cursor advances.

`for` requires a declared `Enumeration` conformance. Functions named `cursor` and `next` do not provide structural conformance.

### Ternary

```koja
y = x > 2 ? "big" : "small"
```

Nested ternaries are disallowed.

### `cond`

Multi-branch conditional. Koja has no `else if`, so `cond` is the idiomatic way to chain conditions. Requires an `else` arm:

```koja
fn classify(n: Int32) -> String
  cond
    n > 100 -> "big"
    n > 10 -> "medium"
    else -> "small"
  end
end
```

`cond` is value-producing when all arms (including `else`) produce values.

Arms can use any boolean expression, including function calls:

```koja
cond
  c.digit?() -> handle_digit(c)
  c.whitespace?() -> skip_whitespace()
  c == "+" -> handle_plus()
  else -> handle_unknown(c)
end
```

### Definite Assignment

A variable must be assigned before it is read on every path. Assignment in a loop does not guarantee a value because the loop might not run:

```koja
fn last_doubled(limit: Int) -> Int
  i = 0

  while i < limit
    doubled = i * 2
    i += 1
  end

  doubled # error when `limit` is zero or negative
end
```

Assignment in every branch does guarantee a value:

```koja
fn choose(flag: Bool) -> Int
  if flag
    choice = 1
  else
    choice = 2
  end

  choice
end
```

A branch that always exits early (`return`, `break`, `Kernel.panic`) does not count against the other branches. Only reads are checked, so assigning to the variable again after the branch or loop is always valid. When a value depends on a branch, assign a default first or use the expression form.

---

## Types

### Primitive Types

| Type      | Description                                        |
| --------- | -------------------------------------------------- |
| `Int`     | 64-bit signed integer (alias for `Int64`)          |
| `Int8`    | 8-bit signed integer                               |
| `Int16`   | 16-bit signed integer                              |
| `Int32`   | 32-bit signed integer                              |
| `Int64`   | 64-bit signed integer (same as `Int`)              |
| `UInt8`   | 8-bit unsigned integer                             |
| `UInt16`  | 16-bit unsigned integer                            |
| `UInt32`  | 32-bit unsigned integer                            |
| `UInt64`  | 64-bit unsigned integer                            |
| `Float`   | 64-bit IEEE 754, finite-only (alias for `Float64`) |
| `Float32` | 32-bit IEEE 754, finite-only                       |
| `Bool`    | `true` or `false`                                  |
| `String`  | UTF-8 string                                       |
| `Binary`  | Arbitrary byte sequence                            |
| `Bits`    | Arbitrary bit sequence                             |
| `()`      | Unit type (empty value)                            |

Every `String` is valid UTF-8 and carries an authoritative byte length.
U+0000 is a valid character. Trailing NUL storage is never used to
determine a string's contents.

Every `Float` and `Float32` is finite. NaN and the infinities are not
representable in Koja. Every operation that would produce one traps
instead (see [Arithmetic Faults](#arithmetic-faults)), the same way
every `String` is valid UTF-8 by construction. Float equality is
therefore a true equivalence relation, and comparisons are total.

All types follow the same [value semantics](#value-semantics). Their representations affect copy cost, not behavior.

### Builtin Declarations

The compiler owns the representation of the primitive types and the
core collections (`List<T>`, `Map<K, V>`, `Set<T>`, `CPtr<T>`). The
stdlib declares each one with the `builtin` keyword, which anchors its
`@doc` comment and its functions:

```koja
@doc """
A UTF-8 string.
"""
builtin String
  @intrinsic
  fn length(self) -> Int
end
```

A `builtin` body admits only functions, never fields or nested type
bodies. Builtin types are always public, and they cannot be
constructed with struct-literal syntax. Declaring a builtin name the
compiler does not provide is a compile error, so user code cannot
mint new builtins. `impl` and `extend` blocks target builtins the
same way they target structs and enums.

### Numeric Widening

Sized numeric values widen implicitly into their hub type, and only into their hub type. `Int8`, `Int16`, `Int32`, `UInt8`, `UInt16`, and `UInt32` widen to `Int` (signed sources sign-extend, unsigned sources zero-extend). `Float32` widens to `Float`. The conversion is always lossless.

```koja
fn count(n: Int) -> Int
  n
end

small: Int32 = -7
count(small) # Int32 widens to Int, value stays -7
```

Widening applies wherever a value flows into a typed slot: call arguments, struct fields, enum payloads, return values, annotated bindings, and constant initializers. It does **not** apply to:

- **Binary operators**: operands must be the same width. `Int32 + Int` is an error. Widen explicitly first.
- **Sideways conversions**: `Int8` does not widen to `Int16`, `UInt8` does not widen to `UInt16`. Each source type has exactly one implicit target.
- **`UInt64`**: it does not fit in `Int`. Use the checked `to_int` function.
- **Generic inference**: `T` binds to the actual type. `identity(small)` infers `T = Int32`, not `Int`.
- **Narrowing or cross-category conversion**: `Int` never implicitly becomes `Int32`, and ints never become floats.

The inverse direction is explicit and checked. `Int` provides `to_int8`, `to_int16`, `to_int32`, `to_uint8`, `to_uint16`, `to_uint32`, and `to_uint64`, each declared `-> TargetType ! NumericConversionError` and failing with `NumericConversionError.OutOfRange` when the value does not fit. `UInt64.to_int` is the checked bridge back to the hub, and `Float.to_float32` rounds to the nearest representable value, with `OutOfRange` for magnitudes too large for a 32-bit float:

```koja
match 300.to_int8()
  Result.Ok(v) -> v.print()
  Result.Err(e) -> "does not fit".print() # 300 > Int8.max
end
```

Sized-to-sized conversions route through `Int`: widen up implicitly, then narrow down explicitly.

### Arithmetic Faults

Arithmetic never wraps, saturates, or produces a non-finite float. An operation without a representable result panics with an `ArithmeticError` (Erlang's `badarith`, not C's undefined behavior). The panic is identical on both backends and in `--release` builds, and it follows the standard crash path. The faulting process crashes (`ExitReason.Crashed`), and a fault in the root process exits the program non-zero.

| Operation                  | Fault                                        |
| -------------------------- | -------------------------------------------- |
| Int `+` `-` `*`, unary `-` | result does not fit the operand type's width |
| Int `/` `%`                | zero divisor, or `MIN / -1`                  |
| `bsl` / `bsr`              | shift count outside `0 <= n < bit width`     |
| Float `+` `-` `*` `/` `%`  | IEEE result is non-finite (NaN or infinity)  |

Integer faults are checked at the operand's declared width and signedness. `UInt8` arithmetic traps past 255, not past `Int.max`. Comparisons never fault.

The float row is what makes the finite-only invariant airtight. `1.0 / 0.0` and `0.0 / 0.0` trap instead of minting `inf` / `NaN`. The remaining boundaries are closed to match. Float literals that would round to infinity are compile-time errors, `Float.parse` classifies them as `OutOfRange`, `Float.to_float32` is checked, and a non-finite float returned by an `@extern "C"` call traps at the call site.

```koja
a = 9223372036854775807
a + 1 # panics: integer overflow in +

b = 0.0
1.0 / b # panics: non-finite float result in /
```

### Unit Expression

`()` is the unit value. Use `else -> ()` in `cond` for side-effect-only fallthrough.

### Strings

#### Single-Line Strings

```koja
"hello world"
"tab:\there"
"quote: \"yes\""
"backslash: \\"
```

Escape sequences: `\"`, `\\`, `\n`, `\r`, `\t`, `\#`.

#### String Interpolation

```koja
name = "koja"
"hello #{name}".print()
"1 + 2 = #{1 + 2}".print()
```

Interpolation expressions are enclosed in `#{}` and can contain any expression.

#### Multiline Strings

Triple-quoted strings with automatic dedent based on closing delimiter position:

```koja
msg =
  """
  first line
  second line
  """
```

Content must start on the line after the opening `"""`. The closing `"""` must
be the first non-whitespace token on its line, but other syntax can follow it.
The closing delimiter's column sets the dedent width. In a direct assignment,
the opener can follow `=` or start on the next line. The formatter preserves
that choice:

```koja
x =
  """
  example text
  """

y = """
example text
"""
```

Multiline strings support the same escape sequences and interpolation as single-line strings.

### Structs

#### Declaration

```koja
struct Point
  x: Int32
  y: Int32
end
```

The header can also declare protocol conformances (`struct Point: Display, Hash`). See [Protocols](#protocols).

#### Construction

```koja
p = Point{x: 1, y: 2}
```

Short structs format inline. Long structs break across lines with trailing commas:

```koja
config = Config{
  name: "production",
  region: "us-east-1",
  port: 8080,
  debug: false,
  verbose: true,
}
```

#### Default Field Values

A field can declare a default value. A construction that omits the field uses the default:

```koja
struct Config
  host: String = "localhost"
  port: Int = 5432
  name: String
end

c = Config{name: "app"} # host and port fill from the defaults
Config{} # error: `name` has no default
```

Default values are limited to side-effect-free expressions: literals (no interpolation), negated numerics, unit enum variants, binary literals, and struct, list, map, or set literals of those. The compiler checks each default against the field type at the declaration. A default cannot use an `alias` shorthand. Write the qualified name.

The default expression evaluates at each construction that omits the field. This makes generic defaults work: a `List<T>` field can default to `[]` and an `Option<T>` field to `Option.None`:

```koja
struct Stack<T>
  items: List<T> = []
  top: Option<T> = Option.None
end

s: Stack<Int> = Stack{}
```

Struct variants of enums take defaults the same way:

```koja
enum Shape
  Rect{width: Int, height: Int = 2}
end

Shape.Rect{width: 4} # height fills with 2
```

#### Field Access

Field access reads an independent field value:

```koja
p.x.print()
p.y.print()
```

This rule also applies to chained access and function calls:

```koja
c.name.length()
```

Field assignment transforms the current field value and writes the result back:

```koja
c.name = c.name.upcase()
```

#### Inline Functions

Functions can be defined directly inside `struct` bodies alongside fields:

```koja
struct Point
  x: Int32
  y: Int32

  fn distance_squared(self) -> Int32
    self.x * self.x + self.y * self.y
  end

  fn origin -> Self
    Point{x: 0, y: 0}
  end
end

p = Point{x: 3, y: 4}
p.distance_squared().print()
Point.origin().x.print()
```

Functions receive `self` by value. A "mutating" function does not change the receiver in place. It computes a new value and returns it, and the caller rebinds:

```koja
struct Counter
  value: Int

  fn increment(self) -> Self
    Counter{value: self.value + 1}
  end
end

c = Counter{value: 0}
c = c.increment() # rebind to the returned value
```

`Self` is a shorthand for the enclosing type in return positions. Use it instead of repeating the type name.

#### Extend Blocks

`extend` blocks attach additional inherent functions to an existing type, analogous to Swift extensions. Use `extend` for adding functions from outside the type's own declaration. `impl` is reserved for protocol conformance (`impl Protocol for Type`).

```koja
extend Point
  fn translate(self, dx: Int32, dy: Int32) -> Self
    self.x += dx
    self.y += dy
    self
  end
end
```

Functions declared in an `extend` block have ambient visibility. They're callable from any package that can name the target type. Collisions on the same function name across `extend` blocks targeting the same type are a compile error.

#### Static Functions

Functions without `self` (either inline or in `extend` blocks) are called on the type directly:

```koja
struct Config
  port: Int

  fn default -> Self
    Config{port: 8080}
  end
end

config = Config.default()
```

#### Concrete Extend Specialization

`extend` blocks can target a specific instantiation of a generic type. Functions defined in a specialized extend are only available when the type argument matches:

```koja
extend CPtr<UInt8>
  fn to_cstring(self) -> CString
    CString{ptr: self, len: strlen(self)}
  end
end
```

`to_cstring` is only available on `CPtr<UInt8>`, not on `CPtr<Int32>` or other instantiations. Calling a specialized function on the wrong type argument produces a compile error with a hint showing which specialization provides the function.

This pointer conversion is distinct from checked
`String.to_cstring()`. It assumes a readable NUL-terminated C buffer
and computes `CString.len` with `strlen`.

Mixing concrete types and type parameters in the same `extend` block is not allowed:

```koja
# Error: mixes concrete types and type parameters
extend Map<String, V>
  fn lookup(self, key: String) -> Option<V>
    self.get(key)
  end
end
```

### Enums

#### Variants

Enums support unit, tuple, and struct variants:

```koja
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

#### Construction

```koja
d = Direction.North
s = Shape.Circle(5)
```

Struct-variant fields can declare default values. See [Default Field Values](#default-field-values).

Within a `match` arm on the same enum, the type prefix can be omitted for unit variants:

```koja
fn opposite(dir: Direction) -> String
  match dir
    North -> "south"
    South -> "north"
    East -> "west"
    West -> "east"
  end
end
```

#### Inline Functions

Enums can also define functions directly in their body:

```koja
enum Direction
  North
  South
  East
  West

  fn label(self) -> String
    match self
      Direction.North -> "north"
      Direction.South -> "south"
      Direction.East -> "east"
      Direction.West -> "west"
    end
  end
end
```

#### Recursive Enums

Enums can reference themselves through generic containers like `List<T>`:

```koja
enum Expr
  Num(Int)
  Add(Expr, Expr)
  Mul(List<Expr>)
end
```

### Nested Types

A struct or enum can own other types. Declare the nested type inside the owner's body, or at the top level with a qualified name. The two forms are equivalent:

```koja
struct Supervisor
  strategy: Supervisor.Strategy

  enum Strategy
    OneForAll
    OneForOne
    RestForOne
  end
end
```

The equivalent qualified top-level form (declare one or the other, not both):

```koja
struct Supervisor
  strategy: Supervisor.Strategy
end

enum Supervisor.Strategy
  OneForAll
  OneForOne
  RestForOne
end
```

The nested type is always referenced by its qualified name, `Supervisor.Strategy`, even inside the owner's own body. Construction, pattern matching, generics, `extend` blocks, and protocol impls all work on nested types:

```koja
s = Supervisor{strategy: Supervisor.Strategy.OneForOne}

match s.strategy
  Supervisor.Strategy.OneForOne -> "one for one".print()
  _ -> "other".print()
end
```

Nesting is a namespacing device only. The nested type does not inherit the owner's type parameters, and `priv` on a nested type means package-private as usual.

### Union Types

A value that can be one of several types. Use `|` between types:

```koja
fn display(item: Post | Comment | Ad) -> String
  match item
    _ -> "an item"
  end
end
```

Use `type` to name a union:

```koja
type Pet = Cat | Dog | Fish
```

A member type widens to the union automatically:

```koja
c = Cat{name: "Whiskers"}
pet: Pet = c
```

Order doesn't matter. `Post | Comment` and `Comment | Post` are the same type.

### Tuples

An anonymous, fixed-size grouping of values. Tuples are structural. Two tuple types are the same type exactly when their element types match, position by position.

```koja
point = (3, 9)
entry: (String, Int) = ("alice", 42)
nested = (1, (2.5, false))
```

Tuples need at least two elements. `()` is the unit value, and `(x)` is a parenthesized expression, not a tuple. Trailing commas are not allowed.

There is no positional access (`t.0`). Take a tuple apart with a destructuring assignment:

```koja
(name, score) = entry
(_, score) = entry # wildcard skips an element
(a, (b, c)) = nested # nesting works
```

Every element pattern must be irrefutable: a binding, a wildcard, or a nested tuple of those. Use `match` for refutable patterns:

```koja
match point
  (0, 0) -> "origin"
  (x, 0) when x > 0 -> "positive x axis"
  (_, y) -> "somewhere at y = #{y}"
end
```

Tuples support `==`/`!=` (element-wise, when every element does), `format()`, `print()`, and string interpolation:

```koja
(1, "one").print() # (1, "one")
```

A tuple containing a closure- or union-typed element (at any nesting depth) is not comparable, since closures and union values cannot be compared for equality. `==`/`!=` on such a tuple is a compile error, and the tuple does not satisfy a `T: Equality` bound. `format()` and `print()` still work, rendering opaque elements as `"..."`.

Tuples work as function returns, generic type arguments, struct fields, and union members:

```koja
fn lookup(key: String) -> (Int, String) | NotFound
  # ...
end

match lookup("a")
  hit: (Int, String) ->
    (n, name) = hit
    name

  missing: NotFound ->
    missing.key
end
```

### Generics

#### Generic Functions

```koja
fn identity<T>(x: T) -> T
  x
end

identity(42).print()
identity("hello").print()
```

Type arguments are inferred at call sites from arguments and type annotations.

#### Generic Structs

```koja
struct Entry<K, V>
  key: K
  value: V
end

entry = Entry{key: "answer", value: 42}
```

Generic struct literals like `Entry{key: k, value: v}` infer their type parameters from the field values when each type parameter appears in at least one field. A type annotation on the binding is only required when no field uniquely binds a parameter, for example a struct that only mentions some of its parameters in its fields' types.

#### Generic Enums

```koja
enum Option<T>
  Some(T)
  None
end
```

Generic enum unit variants infer from an enclosing expected type. Expected
types come from annotations, function and closure returns, control-flow
arms, struct fields, and generic call returns:

```koja
z: Option<Int32> = Option.None

fn empty_label -> (Int, Option<String>)
  (1, Option.None)
end
```

A context-free unit variant still requires an annotation.

#### Annotation-Driven Inference

Type annotations on variables drive generic type inference:

```koja
list: List<Int32> = List.new() # infers T = Int32
```

#### Implementation

Generics compile via monomorphization. The compiler generates specialized native code for each concrete type instantiation. Unused instantiations produce no binary output.

---

## Pattern Matching

### `match`

Pattern matching with exhaustiveness checking:

```koja
result =
  match x
    1 -> "one"
    2 -> "two"
    _ -> "other"
  end
```

Patterns: literals (integers, floats, booleans, strings), wildcards (`_`), variable bindings, nested patterns, enum and struct destructuring. Guards use `when`:

```koja
match x
  Option.Some(v) when v > 5 -> "big"
  Option.Some(_) -> "small"
  Option.None -> "none"
end
```

An enum variant counts as exhaustively covered only when its payload
patterns match every payload value. A literal or nested pattern such as
`Option.Some(Color.Red)` does not cover every `Some`. Multiple partial
payload arms are not combined, so bind the payload and use an inner
`match`, or add a full payload arm such as `Option.Some(_)`.

Struct destructuring works for both plain structs and enum-struct variants. Field syntax is always `name: pattern`. There is no shorthand form. To bind a field under its own name, write `x: x`. Unlisted fields are implicit wildcards, and an empty `{}` matches any value of that type:

```koja
struct Point
  x: Int
  y: Int
end

match p
  Point{x: 0, y: 0} -> "origin"
  Point{x: 5} -> "x is five" # y is unconstrained
  Point{x: x, y: y} -> "(#{x}, #{y})"
end

# Enum-struct variants follow the same rules.
match shape
  Shape.Rect{width: w, height: h} -> w * h
  Shape.Circle{radius: r} -> r * r * 314 / 100
end
```

String literals can be used as patterns:

```koja
fn classify(c: String) -> String
  match c
    "0" -> "zero"
    "1" -> "one"
    _ -> "other"
  end
end
```

OR patterns combine multiple patterns in a single arm with `|`:

```koja
match n
  1 | 2 | 3 -> "small"
  4 | 5 | 6 -> "medium"
  _ -> "large"
end
```

Variable bindings inside OR patterns are disallowed.

`match` is value-producing when all arms produce values.

Matching only reads the subject. The matched variable can be used inside arms and after the `match` expression like any other binding.

---

## Error Handling

Recoverable errors are values: a fallible function returns [`Result<T, E>`](#resultt-e). The error channel notation is sugar over that type, not a second mechanism. Bugs are a separate channel entirely: they crash the process (see [Concurrency](#concurrency)) and are never catchable in-process.

### `! E` Signatures

`-> T ! E` declares a function that produces a `T` or fails with an `E`. It is pure notation for `-> Result<T, E>`, and callers see an ordinary `Result`:

```koja
fn parse_port(raw: String) -> Int ! ParseError
  # ...
end

outcome = parse_port("8080") # outcome: Result<Int, ParseError>
```

Inside a `!`-spelled function, success values are unwrapped: `return value` and the trailing expression check against `T` and wrap in `Result.Ok` automatically. Writing `Result.Ok(...)` by hand in return position is a compile error pointing at the auto-wrap rule.

A fallible function with no meaningful return value omits the return type, just like its infallible counterpart. A bare `! E` declares a unit success (`Result<(), E>`), and the body returns `Result.Ok(())` when it falls off the end:

```koja
fn log_line(message: String) ! WriteError
  try append(message)
end
```

The `!` spelling is opt-in per declaration. A function declared `-> Result<T, E>` keeps its explicit `Result.Ok` / `Result.Err` returns and compiles exactly as before.

### `fail`

`fail expr` exits the function with an error. It is sugar for `return Result.Err(expr)` and goes anywhere `return` does: a statement of its own or a match arm tail, never embedded in a larger expression.

```koja
fn read_config(path: String) -> Config ! ConfigError
  unless File.exists?(path)
    fail ConfigError.Missing(path)
  end
  # ...
end
```

### `try`

`try expr` unwraps a `Result`: an `Ok` value flows through, an `Err` propagates out of the enclosing function.

```koja
fn load(path: String) -> Server ! ConfigError
  config = try read_config(path)
  port = try parse_port(config.port) # error type must fit the declared `E`
  Server{config: config, port: port}
end
```

The subject must produce a `Result`, and the enclosing function (or closure) must declare an error type for the propagated error to fit into, under either spelling. For an `Option`, name the error first: `try option.or_err(error)`.

### Error Unions

Errors compose with ordinary [union types](#union-types). A function calling into two error domains declares the union, and each propagated or failed error widens into it without conversion ceremony:

```koja
fn fetch_user(id: Int) -> User ! HTTP.Error | ParseError
  response = try HTTP.get(user_url(id)) # HTTP.Error widens
  try parse_user(response.body) # ParseError widens
end
```

A `type` alias names a recurring union: `type AppError = HTTP.Error | ParseError`. Callers match on the union member to route errors (see [Union Types](#union-types)).

### `rescue`

`expr rescue e -> handler` handles one expression's error inline. The `Ok` value flows through, and the handler receives the error and must produce the same success type or diverge (`fail` or a panic):

```koja
port = parse_port(raw) rescue _ -> 8080

socket = TCPSocket.connect(host, port)
  rescue e -> fail Error.ConnectFailed(e.message())

limits = fetch_limits(url) rescue e -> Kernel.panic("config unavailable: #{e}")
```

`rescue` works on any `Result` regardless of the enclosing function's spelling. It binds looser than any operator, so the whole chain to its left is the subject. Use `_` to ignore the error.

### Combinators

`try` / `fail` / `rescue` are the control-flow surface. `Result`'s functions remain for outcomes treated as data, results held in collections, returned by `Task.await`, or stored in fields, where propagation cannot reach. See [`Result<T, E>`](#resultt-e).

---

## Protocols

Protocols define behavioral contracts. A struct or enum lists its protocols after a colon in its header, and the functions in its body satisfy the contract:

```koja
protocol Greeter
  fn greet(self) -> String
end

struct Cat: Greeter, Description
  name: String

  fn greet(self) -> String
    "meow, I'm #{self.name}"
  end

  fn describe(self) -> String
    "a cat named #{self.name}"
  end
end
```

The compiler checks completeness and signature compatibility, and synthesizes any default-bodied functions the type omits. If the body has a function whose name is a near miss of an omitted default, the compiler warns about the likely typo. Protocol methods may declare default parameters. Implementations inherit those callable arities and cannot repeat the defaults. Entry processes are declared this way (`struct App: Process<(), (), ()>`, see [Packages](#packages)). Protocol declarations accept `@doc` and `@deprecated`.

`Debug` and `Equality` are auto-derived for every type, so listing one is only an override. It suppresses the derived implementation, and the body must supply `format` / `equals?`:

```koja
struct Token: Debug
  secret: String

  fn format(self) -> String
    "Token(redacted)"
  end
end
```

`Self` inside a protocol is sugar for an implicit first type parameter, filled in by each conforming type. A function signature that mentions `Self` resolves it to the concrete implementer. User-declared protocol type parameters (e.g. `protocol Eq<T>`) follow the `Self` slot, and the name `Self` cannot be declared explicitly.

### Impl Blocks

A conformance can also live in a separate `impl Protocol for Type` block:

```koja
impl Greeter for Cat
  fn greet(self) -> String
    "meow, I'm #{self.name}"
  end
end
```

The two forms are equivalent and check identically. Declaring the same conformance in both is a duplicate-conformance error.

The impl block is the isolated-contract form. It rejects public functions the protocol does not declare (`priv fn` helpers are allowed). Use it when a conformance's functions would crowd the type body.

The protocol and the type can both come from other packages. A serialization package can implement its own `Encodable` for `String`, and your application can implement that same `Encodable` for a struct that a third-party package defines.

```koja
protocol Encodable
  fn to_wire(self) -> String
end

impl Encodable for String
  fn to_wire(self) -> String
    self
  end
end
```

The compiler checks the whole program for conflicts. If two packages implement the same protocol for the same type, or give one type two functions with the same name, the build fails with an error at the conflicting declaration.

A protocol can also be implemented for one concrete instantiation of a generic type, even a generic type from another package:

```koja
impl Encodable for List<Int>
  fn to_wire(self) -> String
    "#{self.length()} ints"
  end
end
```

The conformance covers `List<Int>` only. A bound like `T: Encodable` accepts `List<Int>` and rejects `List<String>`. A generic type can carry at most one impl per protocol, because every instantiation shares one set of function names.

An impl can also keep the target's type parameters open, with an optional condition on each. The condition uses the same inline bound syntax as function generics:

```koja
impl Encodable for List<T: Encodable>
  fn to_wire(self) -> String
    result = "["

    for item in self
      result = result <> item.to_wire()
    end

    result <> "]"
  end
end
```

The conformance covers every `List` whose element type is itself `Encodable`, at any nesting depth. `List<Int>` qualifies once `Int` does, and so does `List<List<Int>>`. Inside the body, the condition is in force, so `item.to_wire()` dispatches through it. Without a condition (`impl Encodable for List<T>`), the conformance covers every instantiation. Conditions attach to the target's own type parameters, so a concrete argument cannot carry one (`impl Encodable for List<Int: Encodable>` is an error).

### Trait Bounds

Generic type parameters can be constrained to types implementing specific protocols using `:` syntax:

```koja
fn say_hello<T: Greeter>(animal: T) -> String
  animal.greet()
end
```

Multiple bounds use `&`. It is valid only in bound lists, not in general type positions:

```koja
fn describe_and_greet<T: Greeter & Description>(animal: T) -> String
  animal.describe() <> " says " <> animal.greet()
end
```

Generic protocol bounds can include type arguments. The arguments can use other type parameters from the same declaration:

```koja
fn count_items<T, Cursor, E: Enumeration<T, Cursor>>(source: E) -> Int
  count = 0

  for _ in source
    count += 1
  end

  count
end
```

Bounds are verified at call sites. If a concrete type doesn't implement a required protocol, the compiler emits an error:

```
type `Cat` does not implement protocol `Description` (required by type parameter `T` in `myapp.describe_and_greet`)
```

Inside the function body, protocol functions can be called directly on bounded type parameters. The compiler resolves the function through the protocol's signature.

Unbounded type parameters (`<T>`) remain valid and backwards compatible.

### Dispatch

Protocol dispatch is static via monomorphization. No vtables, no dynamic dispatch.

---

## Packages

A package is the unit of code organization, defined by a `koja.toml` manifest. Files within a package are transparent. They share one namespace, and every top-level declaration (type, function, constant) is visible from every other file in the package. Files carry no namespace of their own, and there are no imports:

```koja
# src/helper.koja
fn add(a: Int, b: Int) -> Int
  a + b
end

# src/app.koja
alias Process.Step
alias Process.StopReason

struct App: Process<(), (), ()>
  fn start(config: ()) -> Self ! StopReason
    App{}
  end

  fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>
    Step.Continue(self)
  end

  fn run(self) -> StopReason
    add(3, 4).print()
    StopReason.Normal
  end
end
```

Other packages (the qualified standard library and dependencies) are reached through their package namespace: `JSON.decode(...)`, `Net.TCPSocket`, `HTTP.get(...)`, `Mathlib.PI`.

A package has two names. The manifest `name` is its lowercase snake_case identity, used for the `deps/` directory, dependency keys, lockfile entries, and the default binary name. Its **namespace** is the PascalCase name code uses for qualified access, derived from `name` (`my_app` -> `MyApp`). When the derivation isn't right (acronyms, unusual casing), declare it explicitly:

```toml
[project]
name = "http"
namespace = "HTTP"
version = "0.1.0"
```

### Visibility

Access control is at the declaration level (`priv`), not the file level:

- A top-level `priv` declaration (`fn`, `struct`, `enum`, `const`, `type`,
  `protocol`) is **package-private**: usable from any file in the same
  package, rejected from other packages.
- `priv fn` declared inside a `struct`, `enum`, `extend`, or `impl` body
  is **type-private**: callable from any other function on the same target
  type (across the decl block and any `extend` or `impl Protocol for Type`
  block on that type), rejected everywhere else.

See [Private Declarations](#private-declarations) for examples.

### Aliases

When using types from qualified standard library packages or dependency packages, `alias` creates a file-private shorthand:

```koja
alias Net.TCPSocket
alias JSON.Value

conn = TCPSocket.connect("example.com", 80)
```

`alias Net.TCPSocket` makes `TCPSocket` available as a local name. `alias JSON.Value` makes `Value` available as a local name. Aliases are scoped to the declaring file and don't affect other files.

Aliases name types only. Package-level functions are called with qualified syntax directly, no alias needed:

```koja
response = HTTP.get("https://example.com")
```

### Standard library visibility

The auto-imported `Global` package provides core types (`Option`, `Result`, `List`, `Map`, `Set`, `Process`, `IO`, `File`, `URI`, `Base`, `Path`, etc.) with no alias needed. Domain-specific packages require qualified access:

- **`Crypto`**: `SHA1`, `SHA256`, `SHA384`, `SHA512`, `HMAC`, `Certificate`, `PrivateKey`, `PEMError`
- **`JSON`**: `Value`, `Encoding`, `EncodeOptions`, `encode`, `decode`
- **`Net`**: `TCPSocket`, `TCPListener`, `UDPSocket`, `Socket`, `IPAddress`, `SocketAddress`, `SocketKind`, `SocketError`, `TLSSession`, `TLSConfig`, `TLSIdentity`, `TrustStore`, `TLSError`, `VerificationError`

Use `alias Crypto.SHA256` or `alias Net.TCPSocket` to access them.

### Dependencies

Packages declare dependencies in `koja.toml`, by local path or by git repository pinned to an exact ref:

```toml
[dependencies]
postgres = { github = "koja-lang/postgres", tag = "v0.1.0" }
vendored = { git = "https://git.example.com/vendored.git", branch = "main" }
greeter = { path = "libs/greeter" }
```

Each dependency declares exactly one of `path`, `git`, or `github` (an `owner/repo` shorthand for `https://github.com/owner/repo`). Git dependencies accept at most one of `tag`, `branch`, or `rev`. With none, the remote's default branch is used. There is no version solver. A ref resolves to a commit, and one version of a package name exists per build.

`koja deps get` is the only command that touches the network. It resolves each ref to a commit SHA, records the pin in `koja.lock` (committed, so builds are reproducible), caches a mirror clone under `~/.koja/cache`, and copies the pinned tree into the project's `deps/` directory (gitignored and read-only, always regenerable). Dependencies of dependencies resolve transitively, and the root project's lockfile is the only one consulted.

Every other command is offline. `build`, `check`, `run`, `test`, and `doc` verify `koja.lock` against the manifest and re-materialize `deps/` from the local cache when needed. A manifest edit that outdates the lock, or a pin missing from the cache, is a hard error naming the fix rather than a silent fetch:

```
error: dependency `postgres` is not pinned in koja.lock (koja.toml changed?), run `koja deps get`
```

`koja deps` prints each dependency with its pin and local state. `koja deps update [name]` re-resolves refs against their remotes (moving a `branch` pin forward). `koja deps clean` removes `deps/`. With `--cache` it also purges the global mirror cache.

Private repositories work through the ambient git configuration: SSH agents for `git@` URLs, credential helpers for https, and `insteadOf` rewrites in CI. Credentials never appear in `koja.toml` or `koja.lock`.

---

## Concurrency

Koja uses a message-passing actor model inspired by Erlang/Elixir. Processes have isolated memory and communicate exclusively through typed messages. Messages are passed by value (each process receives its own copy), so there is no shared mutable state.

Process timeout and delay values are measured in milliseconds. Negative
values behave as zero.

### `Task<R>`

The simplest way to run concurrent work. Wraps a closure, runs it in a spawned process, and returns the result:

```koja
ref = Task.async(fn () -> Int expensive_computation() end)
result = Task.await(ref) # Result<Int, Process.CallError>, times out after 5000ms
```

`Task.async(fn)` spawns the closure and returns a `Ref<(), R>`. `Task.await(ref)` sends a unit message and waits for the reply.

### `Process<C, M, R>` Protocol

For stateful, long-lived processes, implement the `Process` protocol. `C` is the config type, `M` is the message type, `R` is the reply type.

```koja
protocol Process<C, M, R>
  fn start(config: C) -> Self ! Process.StopReason

  fn handle(self, msg: M, from: Option<ReplyTo<R>>) -> Process.Step<Self>

  fn handle_signal(self, event: Process.Lifecycle) -> Process.Step<Self>

  fn run(self) -> Process.StopReason
end
```

The helper types are nested under `Process` (`Process.Step`, `Process.StopReason`, `Process.Lifecycle`, `Process.CallError`). Idiomatic code shortens them with file-local aliases (`alias Process.Step`), which the examples below assume.

`start` builds the initial state from config in the child process context, before the receive loop begins. Return the state to begin running, or `fail reason` to abort startup.

`handle` returns `Step<Self>`. Return `Step.Continue(self)` to keep running with updated state, or `Step.Done(reason)` (with a `StopReason` of `Normal` or `Shutdown`) to stop.

`handle_signal` has a default implementation that stops on `Shutdown`/`Interrupt` and continues on `Reload`. Override it for graceful drain or hot config reload.

`run` has a default implementation that enters a receive loop, dispatching business messages to `handle` and lifecycle events to `handle_signal`, and stopping when either returns `Step.Done`:

```koja
fn run(self) -> StopReason
  receive
    envelope: (M, Option<ReplyTo<R>>) ->
      (msg, from) = envelope

      match self.handle(msg, from)
        Step.Continue(next) -> next.run()
        Step.Done(reason) -> reason
      end

    event: Lifecycle ->
      match self.handle_signal(event)
        Step.Continue(next) -> next.run()
        Step.Done(reason) -> reason
      end
  end
end
```

A complete process example:

```koja
alias Process.Step
alias Process.StopReason

enum CounterMsg
  Increment
  Decrement
end

struct Counter: Process<Counter, CounterMsg, Int>
  count: Int

  fn start(config: Counter) -> Self ! StopReason
    config
  end

  fn handle(self, msg: CounterMsg, from: Option<ReplyTo<Int>>) -> Step<Self>
    next_count =
      match msg
        CounterMsg.Increment -> self.count + 1
        CounterMsg.Decrement -> self.count - 1
      end

    ReplyTo.reply(from, next_count)
    Step.Continue(Counter{count: next_count})
  end
end

ref = spawn Counter.start(Counter{count: 0})
ref.cast(CounterMsg.Increment)
count = ref.call(CounterMsg.Increment, 5000)
```

### Lifecycle and StopReason

`Process.Lifecycle` abstracts OS signals into a platform-agnostic enum:

```koja
enum Process.Lifecycle
  Shutdown # SIGTERM
  Interrupt # SIGINT
  Reload # SIGHUP
end
```

`Process.StopReason` represents intentional process termination:

```koja
enum Process.StopReason
  Normal # process finished its work
  Shutdown # process was told to stop
end
```

The runtime maps the entry process's final `StopReason` to the OS exit code: `Normal` exits 0, `Shutdown` exits 1.

`Process.ExitReason` is what a monitoring process sees when a watched process stops:

```koja
enum Process.ExitReason
  Normal
  Shutdown
  Killed
  Crashed(Process.CrashInfo) # CrashInfo carries the panic message and backtrace
end
```

### `Ref<M, R>`

`spawn` returns a typed handle to the running process. `M` is the message type the process accepts, and `R` is the reply type.

```koja
struct Ref<M, R>
  id: Int
end
```

Operations on a process handle:

- `cast(msg: M)`: fire-and-forget. The handler receives `from = Option.None`.
- `call(msg: M, timeout: Int) -> Result<R, Process.CallError>`: sends a message and blocks up to `timeout` milliseconds for a reply. Returns `Result.Ok(reply)` on success, `Result.Err(CallError.Timeout)` if the process didn't reply in time, or `Result.Err(CallError.ProcessDown)` if the process is dead. A dead callee resolves the call promptly, even when it dies mid-wait, without waiting out the timeout.
- `signal(event: Process.Lifecycle)`: sends a lifecycle signal to the process (e.g. `Lifecycle.Shutdown`). Delivered to `handle_signal`.
- `kill()`: immediately terminates the process. No signal is sent.
- `alive?() -> Bool`: returns `true` if the process is still running.
- `send_after(msg: M, delay_ms: Int)`: schedules `msg` for delivery after `delay_ms` milliseconds. The message is copied immediately. Delivery happens asynchronously when the timer fires. Useful for periodic ticks and timeouts inside a process loop.

`Ref.self_ref()` returns a typed handle to the current process. It must be called from within a running process (inside `start`, `handle`, or `handle_signal`). The type parameters are inferred from the binding's annotation:

```koja
me: Ref<TickMsg, String> = Ref.self_ref()
me.send_after(TickMsg.Tick, 1000)
```

```koja
ref.cast(CounterMsg.Increment)
result = ref.call(CounterMsg.Increment, 5000)
ref.signal(Process.Lifecycle.Shutdown)
```

### `ReplyTo<R>` and `reply`

When a process receives a `call`, the handler gets a `ReplyTo<R>` channel to send the response back. The type `R` is enforced at compile time. The channel carries the caller's process id plus a correlation token minted per call, so stale replies from earlier timed-out calls are discarded instead of delivered to the next call.

```koja
struct ReplyTo<R>
  id: Int
  token: Int
end
```

- `send(reply: R) -> ReplyTo.Delivery`: sends the reply back to the caller. Returns `Delivered`, or `Expired` if the caller already gave up on its `call`. The result is advisory and most handlers ignore it.

`ReplyTo.reply(from, value)` is a convenience on `ReplyTo<R>` that handles the common pattern of replying only when a caller is present (skips silently for `cast` messages):

```koja
extend ReplyTo<R>
  fn reply(from: Option<ReplyTo<R>>, value: R) -> Option<ReplyTo.Delivery>
end
```

Call it with the handler's `from` parameter directly:

```koja
ReplyTo.reply(from, self.count)
```

### `spawn` and `receive`

The underlying keywords that power the process model. `spawn` creates a new lightweight process and returns a `Ref`. `receive` blocks the current process until a message arrives:

```koja
receive
  envelope: (M, Option<ReplyTo<R>>) ->
    # unpack, then handle the message
    (msg, from) = envelope
end
```

An optional `after` clause bounds the wait. If no message arrives within the timeout (in milliseconds), the `after` body runs instead. The timeout is any `Int` expression:

```koja
receive
  envelope: (M, Option<ReplyTo<R>>) ->
    # unpack, then handle the message
    (msg, from) = envelope
after 5000
  # no message within 5 seconds
end
```

In most cases you won't use `receive` directly. The `Process` protocol's default `run` implementation handles it for you.

---

## Annotations

An annotation is `@name` with an optional payload, placed before a
declaration. Payloads are strings (single-line `"..."` or multiline
`"""..."""`, interchangeable) or the literal `false`. By convention,
annotations that carry prose (`@deprecated`, `@doc`) use the multiline form,
and short labels like `@test` descriptions stay on one line.

The FFI annotations `@extern` and `@link` are covered in [C FFI](#c-ffi).

### `@deprecated`

Marks a declaration as deprecated. Every use produces a compile warning:

```koja
@deprecated """
Use `checksum32` instead. It handles inputs longer than 64 KiB.
"""
fn checksum(data: Binary) -> Int32
  # ...
end
```

```
warning: `checksum` is deprecated: Use `checksum32` instead. It handles inputs longer than 64 KiB.
```

The message is required and should tell the caller what to use instead. Bare
`@deprecated` is a compile error.

`@deprecated` is accepted on functions (top-level, inline, and `impl`/`extend`
members), structs, enums, constants, type aliases, and protocols, including
`priv` declarations. Warnings fire at every resolved use site (calls, type
positions, construction, patterns, constant reads), except inside the
deprecated declaration itself and inside `impl`/`extend` blocks whose target
is deprecated, so deprecating a type does not flag its own functions.

### `@doc`

Documents a function, struct, or enum:

```koja
@doc """
Adds two integers.
"""
fn add(a: Int32, b: Int32) -> Int32
  a + b
end
```

`@doc false` excludes an item from generated documentation.

`@doc` on a `priv` declaration is a compile error, since private items never appear in generated documentation.

Doc strings support Markdown and are rendered by `koja doc`.

### `@test`

Marks a function as a test case. `koja test` discovers and runs all
`@test`-annotated functions in `src/` and `test/` directories. Test
functions return `Result<Bool, String>`. Any `Result.Ok` passes, while
`Result.Err(message)` fails with the given message.

```koja
struct AdditionTest
  @test "adds two integers"
  fn test_addition -> Result<Bool, String>
    result = add(2, 3)

    unless result == 5
      return Result.Err("expected 5, got #{result}")
    end

    Result.Ok(true)
  end
end
```

An optional string after `@test` provides a description printed during the
test run. The runner reports every discovered test even when some fail.

---

## C FFI

Koja can call C functions via the `@extern "C"` annotation. FFI declarations live on structs (types are namespaces). No `unsafe` keyword. Safety is the wrapper author's responsibility.

### Declaring Extern Functions

`@extern "C"` on a function marks it as a C declaration. `@link "libname"` tells the linker which library provides the symbol (`-l libname`). Extern functions live inside structs, which serve as namespaces.

```koja
struct FFI
  @extern "C" @link "mylib"
  fn add_numbers(a: Int32, b: Int32) -> Int32

  @extern "C" @link "mylib"
  fn fill_buffer(buf: CPtr<Int32>, count: Int32, value: Int32)
end

result = FFI.add_numbers(3, 4)
result.print()
```

Extern functions have no body. Parameter and return types must be FFI-compatible: explicit-width primitives (`Int32`, `UInt8`, `Float32`, etc.), `Bool`, `CPtr<T>`, or `()`. Extern functions can coexist with normal Koja functions in the same struct. Use `priv fn` on the extern declarations and expose safe public wrappers.

A `Float32` / `Float64` value returned by an extern call is checked at the call site. A NaN or infinity handed back by C panics with an `ArithmeticError` (`non-finite float returned by <name>`), keeping the finite-only float invariant intact across the FFI boundary (see [Arithmetic Faults](#arithmetic-faults)).

Declare C return types at their true width and let [numeric widening](#numeric-widening) do the rest. A C `int` bound as `Int32` flows directly into `Int` contexts with correct sign extension, so negative error codes survive the trip. Reading a C `int` as `Int` would zero-extend the upper 32 bits and corrupt negative values.

### Symbol Naming

When the C symbol name differs from the Koja function name, use `@link "lib:symbol"` to specify the C symbol after a colon:

```koja
struct Crypto
  @extern "C" @link "crypto:EVP_sha256"
  priv fn evp_sha256 -> CPtr<UInt8>

  @extern "C" @link "crypto:SHA256"
  priv fn sha256_raw(data: CPtr<UInt8>, len: Int64, out: CPtr<UInt8>)
    -> CPtr<UInt8>
end
```

`@link "crypto"` (without a colon) uses the Koja function name as the C symbol. `@link "crypto:SHA256"` links to the C symbol `SHA256` while the Koja function name is `sha256_raw`. This keeps all Koja function names in proper `snake_case` regardless of the C library's naming conventions.

### `CPtr<T>`

A raw C pointer type. `Copy` semantics (just a machine word). No ownership tracking. The compiler will not auto-free memory behind a `CPtr<T>`.

```koja
struct CPtr<T>
  fn null -> CPtr<T>
  fn alloc(count: Int) -> CPtr<T>
  fn free(self)
  fn offset(self, n: Int) -> CPtr<T>
  fn read(self) -> T
  fn write(self, value: T)
  fn null?(self) -> Bool
  fn address(self) -> Int
end
```

`alloc` and `free` use C's `malloc` and `free`. All functions are compiler intrinsics. `address` returns the raw address as an `Int` bit pattern (0 for null). `CPtr<T>` implements `Debug` by rendering that address as 16 hex digits: `ptr.format()` gives `CPtr(0x00006000023a4f10)` and a null pointer gives `CPtr(0x0)`.

```koja
buf: CPtr<Int32> = CPtr.alloc(4)
buf.write(42)
buf.read().print()
buf.free()

null_ptr: CPtr<Int32> = CPtr.null()
null_ptr.null?().print()
```

Type annotations on the variable drive generic inference for static functions like `CPtr.alloc()` and `CPtr.null()`.

`CPtr<UInt8>` additionally provides the two ways to get a pointer to a `Binary`'s bytes:

- `CPtr.borrow(bytes: Binary) -> CPtr<UInt8>`: zero-cost view of the binary's payload. The result cannot be bound to a variable, returned, or stored. It may only be consumed within the statement that borrows it (as a call argument or chained receiver), where the source `Binary` is guaranteed to be live.
- `CPtr.copy(bytes: Binary) -> CPtr<UInt8>`: malloc'd owned copy of the bytes. Nameable like any value. The caller frees it. Use this when a C API retains the pointer past the call.

```koja
digest: CPtr<UInt8> = CPtr.alloc(32)
FFI.blake3_hash(CPtr.borrow(data), data.byte_size(), digest) # fine

p = CPtr.borrow(data) # compile error: a borrowed pointer cannot be bound
owned = CPtr.copy(data) # owned copy, free it when C is done
```

### `CString`

A pointer-and-length descriptor for a null-terminated C string. It does
not encode ownership. `String.to_cstring()` allocates owned memory, while
`CPtr<UInt8>.to_cstring()` wraps an existing pointer without allocating.

```koja
struct CString
  ptr: CPtr<UInt8>
  len: Int
end

enum CString.ConversionError
  InteriorNul
  InvalidLength
  InvalidUTF8
  NullPointer
end
```

Convert between Koja strings and C strings:

```koja
name = "hello"
cs = name.to_cstring().unwrap()
cs.len.print()

back = cs.to_string().unwrap()
(back == name).print()

cs.free()
```

`String.to_cstring() -> CString ! CString.ConversionError`
allocates a null-terminated copy via `malloc` and rejects `String`
values containing U+0000 with `InteriorNul`.
`CString.to_string() -> String ! CString.ConversionError` copies
exactly `len` bytes and rejects invalid lengths, pointers, and UTF-8.
It does not consume or free the C buffer. Call `free()` only when the
descriptor owns malloc-compatible storage.

### Passing Pointers to C

`CPtr<T>` is accepted in `@extern "C"` signatures, enabling pointer-passing FFI:

```koja
struct FFI
  @extern "C" @link "mylib"
  fn fill_array(buf: CPtr<Int32>, count: Int32, value: Int32)

  @extern "C" @link "mylib"
  fn sum_array(buf: CPtr<Int32>, count: Int32) -> Int32
end

buf: CPtr<Int32> = CPtr.alloc(4)
FFI.fill_array(buf, 4, 10)
total = FFI.sum_array(buf, 4)
total.print()
buf.free()
```

For string-accepting C functions, pass `cs.ptr` (the `CPtr<UInt8>`) and `cs.len`:

```koja
cs = "hello".to_cstring().unwrap()
FFI.some_c_function(cs.ptr, cs.len)
cs.free()
```

For byte-accepting C functions, borrow a pointer to the `Binary` at the call site:

```koja
FFI.consume_bytes(CPtr.borrow(data), data.byte_size())
```

Pointers passed to C are valid for the duration of the call. A C function that keeps the pointer past the call needs `CPtr.copy` (an owned copy the caller frees).

---

## Standard Library

The following types and functions are available in every file with no alias needed.

### `Kernel`

Core runtime operations.

#### `Kernel.exit(code: Int)`

Terminates the process immediately with the given exit code. `0` indicates success, and any non-zero value indicates failure. Never returns, so a match arm or function body may end in `Kernel.exit(...)` regardless of the type the surrounding code expects.

```koja
Kernel.exit(0)
```

#### `Kernel.panic(message: String)`

Aborts the process with the given message and a symbolicated stack trace. Never returns. Used internally by `unwrap()` on `Option.None` and `Result.Err`.

```koja
Kernel.panic("something went wrong")
```

### `Option<T>`

```koja
enum Option<T>
  Some(T)
  None
end
```

Functions: `unwrap()`, `or(default)`, `or_err(error)`, `some?()`, `none?()`, `map(fn (T) -> U)`, `then(fn (T) -> Option<U>)`.

`or_err(error)` bridges to `Result`: `Some(v)` becomes `Ok(v)` and `None` becomes `Err(error)`, ready for [`try`](#try).

```koja
x = Option.Some(42)
x.unwrap().print() # 42
x.or(0).print() # 42
x.some?().print() # true

y: Option<Int> = Option.None
y.or(99).print() # 99

mapped = x.map(fn (v: Int) -> Int v * 10 end)
mapped.unwrap().print() # 420
```

### `Result<T, E>`

```koja
enum Result<T, E>
  Ok(T)
  Err(E)
end
```

Functions: `unwrap()`, `or(default)`, `ok?()`, `err?()`, `ok()`, `err()`, `map(fn (T) -> U)`, `map_err(fn (E) -> F)`.

```koja
ok: Result<Int32, Int32> = Result.Ok(42)
ok.unwrap().print() # 42

err: Result<Int32, Int32> = Result.Err(1)
err.or(99).print() # 99
```

For unwrap-or-propagate control flow, prefer `try` / `fail` / `rescue` over combinator chains. See [Error Handling](#error-handling).

### `Range`

An inclusive range with `start` and `stop` endpoints.

```koja
struct Range
  start: Int
  stop: Int
end
```

Used by `String.slice` for substring extraction:

```koja
greeting = "hello world"
hello = greeting.slice(Range{start: 0, stop: 4})
hello.print() # "hello"
```

### `List<T>`

Dynamically-sized, heap-backed collection. Compiler intrinsic backed by C's `malloc`/`realloc`/`free`.

```koja
list: List<Int32> = List.new()
list = list.append(10)
list = list.append(20)

list.length().print() # 2
list.get(0).unwrap().print() # 10
list.empty?().print() # false
```

`append` returns a new list with the element added (rebind with `list = list.append(x)`). The original is unchanged. `get` returns `Option<T>` (`None` for out-of-bounds).

Functions:

- `new() -> List<T>`: creates an empty list.
- `append(self, item: T) -> List<T>`: appends an element.
- `last(self) -> Option<T>`: returns the last element, or `None` if empty.
- `length(self) -> Int`: returns the number of elements.
- `get(self, index: Int) -> Option<T>`: returns the element at `index`, or `None` if out of bounds.
- `empty?(self) -> Bool`: returns `true` if the list has no elements.
- `map(self, f: fn (T) -> U) -> List<U>`: returns a new list with `f` applied to each element.
- `filter(self, f: fn (T) -> Bool) -> List<T>`: returns elements for which `f` returns `true`.
- `any?(self, f: fn (T) -> Bool) -> Bool`: returns `true` if `f` returns `true` for at least one element.
- `all?(self, f: fn (T) -> Bool) -> Bool`: returns `true` if `f` returns `true` for every element. Returns `true` for an empty list.
- `pop(self) -> (Option<T>, List<T>)`: returns the last element and remaining list.

```koja
nums = [1, 2, 3, 4, 5]
doubled = nums.map(fn (n: Int) -> Int n * 2 end)
evens = nums.filter(fn (n: Int) -> Bool n % 2 == 0 end)
has_big = nums.any?(fn (n: Int) -> Bool n > 3 end)
all_pos = nums.all?(fn (n: Int) -> Bool n > 0 end)
```

`==` compares lists element by element. Two lists are equal when they have the same length and the elements at each index are equal. The conformance is conditional (`impl Equality for List<T: Equality>`), so a list of closures is not comparable and does not satisfy a `T: Equality` bound.

List literals (`[a, b, c]`) are backed by the `ListLiteral<T>` protocol. See [Literal Protocols](#literal-protocols).

### `Map<K, V>`

A generic hash map. Keys must implement `Hash` and `Equality`. Uses open addressing with linear probing.

```koja
m: Map<String, Int> = Map.new()
m = m.put("a", 1)
m = m.put("b", 2)

m.get("a").unwrap().print() # 1
m.has?("b").print() # true
m.length().print() # 2

for (key, value) in m
  "#{key}: #{value}".print()
end
```

Functions:

- `new() -> Map<K, V>`: creates an empty map.
- `put(self, key: K, value: V) -> Map<K, V>`: inserts or updates a key-value pair.
- `get(self, key: K) -> Option<V>`: returns `Option.Some(value)` if the key exists, `Option.None` otherwise.
- `has?(self, key: K) -> Bool`: returns `true` if the key exists.
- `remove(self, key: K) -> Map<K, V>`: removes the entry for the key. Returns the map unchanged if the key is absent.
- `length(self) -> Int`: returns the number of entries.
- `empty?(self) -> Bool`: returns `true` if the map has no entries.

`for` yields `(K, V)` entries. Iteration order is unspecified.

`==` compares maps by key and value. Insertion order does not affect equality.

Map literals (`[key: value, ...]`) are backed by the `MapLiteral<K, V>` protocol. See [Literal Protocols](#literal-protocols).

### `Set<T>`

A generic hash set of unique elements. Elements must implement `Hash` and `Equality`. Uses open addressing with linear probing.

```koja
s: Set<Int> = Set.new()
s = s.insert(1)
s = s.insert(2)
s = s.insert(1)

s.length().print() # 2
s.has?(1).print() # true

for item in s
  item.print()
end
```

Functions:

- `new() -> Set<T>`: creates an empty set.
- `insert(self, item: T) -> Set<T>`: adds an element. Returns unchanged if already present.
- `has?(self, item: T) -> Bool`: returns `true` if the element exists.
- `remove(self, item: T) -> Set<T>`: removes the element. Returns unchanged if absent.
- `length(self) -> Int`: returns the number of elements.
- `empty?(self) -> Bool`: returns `true` if the set has no elements.

`for` yields each element once. Iteration order is unspecified.

`==` compares sets by membership. Insertion order does not affect equality.

`Set<T>` implements `ListLiteral<T>`, so list literal syntax constructs a set when the target type is `Set<T>`:

```koja
names: Set<String> = ["alice", "bob", "alice"] # Set with 2 elements
```

### String Functions

`String` implements `Enumeration<String, Int>`, so `for` iterates Unicode characters:

```koja
for c in "hello"
  c.print()
end
```

Functions:

- `length(self) -> Int`: returns the number of Unicode codepoints.
- `get(self, index: Int) -> Option<String>`: returns the single-character string at the given index, or `None` if out of bounds.
- `alpha?(self) -> Bool`: returns `true` if the string contains only ASCII alphabetic characters (a-z, A-Z).
- `at(self, index: Int) -> Option<String>`: alias for `get`.
- `byte_length(self) -> Int`: returns the number of bytes in the UTF-8 encoding.
- `codepoints(self) -> List<String>`: returns each Unicode codepoint as a single-character string in a list.
- `contains?(self, other: String) -> Bool`: returns `true` if the string contains `other` as a substring.
- `digit?(self) -> Bool`: returns `true` if the string contains only numeric characters (`0`-`9`).
- `downcase(self) -> String`: returns a copy with ASCII uppercase letters converted to lowercase.
- `empty?(self) -> Bool`: returns `true` if the string has zero length.
- `ends_with?(self, suffix: String) -> Bool`: returns `true` if the string ends with `suffix`.
- `graphemes(self) -> List<String>`: returns each grapheme cluster as a string in a list. Currently equivalent to `codepoints()`.
- `join(parts: List<String>, separator: String) -> String`: static. Joins a list of strings with `separator` between each element.
- `replace(self, old: String, new: String) -> String`: replaces all occurrences of `old` with `new`.
- `reverse(self) -> String`: returns a copy with the codepoints in reverse order.
- `slice(self, range: Range) -> String`: returns a substring spanning the given inclusive range of character indices. Clamps out-of-bounds endpoints.
- `split(self, separator: String) -> List<String>`: splits on each occurrence of `separator`. An empty separator splits into individual characters.
- `starts_with?(self, prefix: String) -> Bool`: returns `true` if the string starts with `prefix`.
- `to_binary(self) -> Binary`: zero-cost conversion to `Binary` (every valid UTF-8 string is a valid byte sequence).
- `to_float(self) -> Float ! NumericConversionError`: parses the string as a 64-bit float (see [Parsing](#parsing)).
- `to_int(self) -> Int ! NumericConversionError`: parses the string as a 64-bit signed integer (see [Parsing](#parsing)).
- `trim(self) -> String`: returns a copy with leading and trailing whitespace removed.
- `trim_end(self) -> String`: returns a copy with trailing whitespace removed.
- `trim_start(self) -> String`: returns a copy with leading whitespace removed.
- `upcase(self) -> String`: returns a copy with ASCII lowercase letters converted to uppercase.
- `whitespace?(self) -> Bool`: returns `true` if the string contains only whitespace characters (space, `\n`, `\r`, `\t`).

```koja
s = "hello world"
s.length().print() # 11
s.get(0).unwrap().print() # "h"
s.contains?("world").print() # true
s.starts_with?("hello").print() # true
s.split(" ").length().print() # 2
s.upcase().print() # "HELLO WORLD"
s.slice(Range{start: 0, stop: 4}).print() # "hello"
"  hello  ".trim().print() # "hello"
```

`String` also implements `Equality` (content comparison via `==`) and `Hash` (FNV-1a).

### Binary and Bits

`Binary` represents an arbitrary byte sequence. `Bits` represents an arbitrary bit sequence. Both are heap-backed value types (copied by reference-counted share like `String`).

#### Literals

Binary and bitstring literals use `<<>>` syntax with comma-separated segments:

```koja
header = <<0x48, 0x65, 0x6C, 0x6C, 0x6F>>
wide = <<0x0102::16>>
le = <<0x0102::16 little>>
neg = <<-1::8 signed>>
msg = <<0x01, port::16>>
```

Segment modifiers: `::N` (bit width), `::N byte` (byte width), `signed`/`unsigned`, `big`/`little`, type annotations (`: Float32`, `: Int16`). Byte-aligned totals produce `Binary`, non-byte-aligned produce `Bits`. String literals can appear as segments for protocol framing.

`Binary`-typed values splice their bytes into the literal, so a framed message builds in one expression. A bare segment is a splice whenever its value is `Binary`-typed. `payload: Binary` spells it out explicitly. Splices take no width or endianness modifiers, and the fixed-width segments around a splice must total whole bytes:

```koja
frame = <<0x51, (payload.byte_size() + 4)::32, payload>>
```

#### Pattern Matching

Binary patterns destructure byte sequences in `match`:

```koja
match packet
  <<tag::8, length::16, rest: Binary>> -> handle(tag, rest)
  _ -> "no match".print()
end
```

Greedy rest capture with `rest: Binary` consumes all remaining bytes. Patterns that don't match the data length fall through to the next arm.

Float-extract segments (`x: Float32` in a pattern) are not supported yet. When they land, a segment decoding to NaN or infinity will fail the match and fall through to the next arm, Erlang-style, preserving the finite-only float invariant (see [Arithmetic Faults](#arithmetic-faults)).

#### Functions

- `at(self, index: Int) -> Option<Int>`: returns the byte at `index` as an `Int` in `0..255`, or `Option.None` out of bounds. O(1). Prefer this over `String.get` for scanning large inputs (`String.get` is O(n) per call because it counts UTF-8 codepoints from the start).
- `byte_size(self) -> Int`: returns the number of bytes.
- `slice(self, range: Range) -> Binary`: copies the inclusive byte range `[start, stop]`. Endpoints clamp to the binary's bounds.
- `to_bits(self) -> Bits`: zero-cost widening from bytes to bits.
- `to_string(self) -> String ! String.ConversionError`: attempts to interpret bytes as UTF-8, failing with `InvalidUTF8` when decoding fails.

`Binary` implements `Equality` (length plus byte comparison, so `a == b` works) and `Hash`, making it usable as a `Map` key or `Set` element. Its `Debug` rendering is the byte-list form `<<83, 0, 0, 0, 4>>`, truncated with a trailing `...` past 64 bytes.

`Bits` functions:

- `bit_size(self) -> Int`: returns the number of bits.
- `byte_at(self, index: Int) -> Option<Int>`: returns storage byte `index` as an `Int` in `0..255`, or `Option.None` out of bounds. Bytes hold bits MSB-first with zeroed trailing padding, and the bitstring occupies `ceil(bit_size / 8)` bytes. O(1).

`Bits` also implements `Equality` (bit length plus bit comparison) and `Hash`, so it works as a `Map` key or `Set` element. Its `Debug` rendering is the round-trippable literal form: whole bytes as decimals, then any trailing partial byte as `value::width`, e.g. `<<72, 101, 5::3>>`. Truncation past 64 bytes matches `Binary`.

#### Conversion Functions

- `String.to_binary(self) -> Binary`: zero-cost widening from UTF-8 string to bytes.
- `CPtr<UInt8>.to_binary(self, len: Int) -> Binary`: creates a `Binary` by copying `len` bytes from the pointer. The pointer is not freed. A negative length panics.
- `Bits.to_binary(self) -> Binary ! String`: narrows bits to bytes. Fails if the bit length is not divisible by 8.

```koja
bin = "hello".to_binary()
bits = bin.to_bits()
roundtrip = bits.to_binary().unwrap().to_string().unwrap()
roundtrip.print() # "hello"
```

### File I/O

#### `Fd`

A raw file descriptor for low-level I/O:

```koja
struct Fd
  descriptor: Int
end
```

Functions:

- `read(self, count: Int) -> String ! String`: reads and validates up to `count` bytes as UTF-8.
- `read_binary(self, count: Int) -> Binary ! String`: reads up to `count` arbitrary bytes.
- `write(self, data: Binary | String) -> Int ! String`: writes data, returns bytes written.
- `close(self) -> String ! String`: closes the descriptor.

#### `File`

Higher-level file operations wrapping `Fd`:

```koja
struct File
  fd: Fd
end
```

Functions:

- `File.open(path: String, mode: FileMode) -> File ! String`: opens a file with the given mode (`FileMode.Read`, `FileMode.Write`, `FileMode.Append`).
- `File.read(path: String) -> String ! String`: reads an entire file as UTF-8 text (opens, reads, closes).
- `File.read_binary(path: String) -> Binary ! String`: reads an entire file as arbitrary bytes.
- `File.write(path: String, content: Binary | String) -> String ! String`: writes text or arbitrary bytes (creates or truncates).
- `File.exists?(path: String) -> Bool`: returns true if a file or directory exists at the path.
- `File.dir?(path: String) -> Bool`: returns true only for directories (`exists?` covers both).
- `File.delete(path: String) -> String ! String`: deletes a file.
- `File.rename(source: String, destination: String) -> String ! String`: renames (moves) a file.
- `File.mkdir(path: String) -> String ! String`: creates a single directory, erroring if the parent is missing or the path already exists.
- `File.mkdir_p(path: String) -> String ! String`: creates a directory and any missing parents (like `mkdir -p`), succeeding if it already exists.
- `File.rmdir(path: String) -> String ! String`: removes an empty directory.
- `close(self) -> String ! String`: closes the file handle.

```koja
content = File.read("config.txt").unwrap()
content.print()
```

### Environment

- `System.get_env(key: String) -> Option<String>`: returns a UTF-8 host value or `Option.None` when absent.
- `System.set_env(key: String, value: String)`: sets a UTF-8 environment value.

Both functions panic when a key or value contains U+0000.
`System.get_env` also panics if the host value is not valid UTF-8.

### Console I/O

`IO` provides ergonomic console input/output. `STDIN`, `STDOUT`, and `STDERR` are available as `Fd` constants for low-level access.

Functions:

- `IO.puts(message: String)`: writes to stdout with a trailing newline.
- `IO.warn(message: String)`: writes to stderr with a trailing newline.
- `IO.write(message: String)`: writes to stdout without a trailing newline.
- `IO.gets(prompt: String) -> String`: prints `prompt` and reads a line from stdin (without the trailing newline).

```koja
IO.puts("hello")
name = IO.gets("What is your name? ")
IO.puts("Hello, #{name}!")
```

### Parsing

Static functions on `Int` and `Float` for parsing strings:

- `Int.parse(input: String) -> Int ! NumericConversionError`: parses a string as a 64-bit signed integer.
- `Float.parse(input: String) -> Float ! NumericConversionError`: parses a string as a 64-bit float.

Failures distinguish malformed text from values that don't fit: `NumericConversionError.InvalidFormat` for text that isn't a number, `NumericConversionError.OutOfRange` for a well-formed number outside the target's range (an integer overflowing 64 bits, or a float magnitude like `1e999` that would round to infinity). Only finite floats parse. There is no literal syntax for infinities or NaN. This is the same error enum the checked narrowing functions use (see [Numeric Widening](#numeric-widening)).

```koja
x = Int.parse("42").unwrap()
x.print() # 42

y = Float.parse("3.14").unwrap()
y.print() # 3.14

match Int.parse("99999999999999999999")
  Result.Ok(_) -> ()
  Result.Err(e) -> e.print() # OutOfRange
end
```

### `URI`

An RFC 3986 URI, parsed into its components. Fields hold the encoded (wire-form) text exactly as it appears in the URI. Every URI has a path (possibly empty), so `path` is not optional:

```koja
struct URI
  fragment: Option<String>
  host: Option<String>
  path: String
  port: Option<Int>
  query: Option<String>
  scheme: Option<String>
  userinfo: Option<String>
end
```

Functions:

- `URI.parse(input: String) -> URI ! URI.Error`: parses and validates an absolute or relative URI. The scheme is lowercased, and a known scheme's default port fills `port` when the input has none. Errors carry the offending part of the input.
- `to_string(self) -> String`: reassembles the URI, omitting the port when it equals the scheme's default.
- `URI.encode(input: String) -> String`: percent-encodes every character that is neither reserved nor unreserved.
- `URI.decode(input: String) -> String ! URI.Error`: percent-unescapes, rejecting malformed `%XX` sequences and invalid UTF-8.
- `URI.default_port(scheme: String) -> Option<Int>`: the well-known port for a scheme (`"https"` gives `443`), or `Option.None`.

`URI` implements `Equality` (component-wise) and `Debug` (`format` renders the assembled URI string, so interpolation produces the URL).

```koja
uri = URI.parse("https://example.com/pkg?v=1").unwrap()
uri.host.unwrap().print() # "example.com"
uri.port.unwrap().print() # 443
"fetching #{uri}".print() # "fetching https://example.com/pkg?v=1"

URI.encode("put it+й").print() # "put%20it+%D0%B9"
```

### `Base`

RFC 4648 encoding and decoding: base16 (hex), base64, and url-safe base64. Encoders accept either a `String` (encoded as its UTF-8 bytes) or a `Binary`, and return the encoded text. Decoders take a `String` and return the decoded bytes, or a `Base.Error` (`InvalidCharacter` with the offending character, `InvalidLength`, or `InvalidPadding`).

- `Base.encode16(data: Binary | String) -> String`: lowercase hex, two characters per byte.
- `Base.decode16(text: String) -> Binary ! Base.Error`: accepts both cases.
- `Base.encode64(data: Binary | String) -> String`: standard `+/` alphabet, padded with `=`.
- `Base.decode64(text: String) -> Binary ! Base.Error`
- `Base.url_encode64(data: Binary | String) -> String`: url-safe `-_` alphabet, padded with `=`.
- `Base.url_decode64(text: String) -> Binary ! Base.Error`

Base64 decoders accept both padded and unpadded input, but `=` may only appear as final padding:

```koja
Base.encode64("foobar").print() # "Zm9vYmFy"
Base.decode64("Zm9vYg==").unwrap().print() # <<102, 111, 111, 98>>
Base.decode64("Zm9vYg").unwrap().print() # <<102, 111, 111, 98>>
Base.encode16(<<0, 15, 255>>).print() # "000fff"
Base.url_encode64(<<251, 239>>).print() # "--8="
```

### `Checksum`

Checksums detect accidental corruption in binary data. They do not provide cryptographic authentication.

- `Checksum.crc32(data: Binary) -> UInt32`: computes CRC-32/ISO-HDLC.
- `Checksum.crc32c(data: Binary) -> UInt32`: computes CRC-32/ISCSI, also known as CRC-32C or Castagnoli.

```koja
Checksum.crc32("123456789".to_binary()) == 0xCBF43926
Checksum.crc32c("123456789".to_binary()) == 0xE3069283
```

### JSON package

`JSON.Value` represents a JSON value tree. Contextual literals can build nested arrays and objects directly:

```koja
payload: JSON.Value = [
  "name": "Koja",
  "active": true,
  "scores": [10, 20, 30],
  "metadata": [:],
]
```

JSON objects keep entry order and duplicate names. Use `JSON.Value.Null` for JSON null.

`JSON.Encoding` converts a type to `JSON.Value` through `to_json`. `JSON.Value`, `Bool`, `Int`, `Float`, `String`, and `List<T: JSON.Encoding>` conform.

```koja
struct Point
  x: Int
  y: Int
end

impl JSON.Encoding for Point
  fn to_json(self) -> JSON.Value
    value: JSON.Value = ["x": self.x.to_json(), "y": self.y.to_json()]
    value
  end
end

text = JSON.encode(Point{x: 3, y: 4})
pretty = JSON.encode(payload, JSON.EncodeOptions{pretty?: true})
decoded = JSON.decode(text)
```

`JSON.encode` accepts an optional `JSON.EncodeOptions` argument. The `pretty?: Bool = false` field selects indented output, as in `JSON.encode(value, JSON.EncodeOptions{pretty?: true})`.

`JSON.decode` returns `JSON.Value ! String`. Typed decoding is not part of this API.

### `Path`

POSIX path manipulation, modeled on Elixir's `Path`. All functions are pure string operations except `expand`, which reads the current working directory and `HOME`. None of them touch the file system, so `..` resolution is lexical and assumes no symlinks.

- `Path.absolute?(path: String) -> Bool`: `true` when the path starts with `/`.
- `Path.basename(path: String) -> String`: last component, ignoring a trailing slash. The root `/` has an empty basename.
- `Path.dirname(path: String) -> String`: directory component. A path without a separator gives `.`, and a trailing slash counts as a separator (`"foo/bar/"` gives `"foo/bar"`).
- `Path.extname(path: String) -> String`: extension of the last component including the dot, or `""`. A leading-dot file such as `.gitignore` has no extension.
- `Path.rootname(path: String) -> String`: the path with its extension stripped.
- `Path.join(parts: List<String>) -> String`: joins segments, collapsing duplicate separators and stripping a trailing slash. Empty segments are skipped, and an empty list gives `""`.
- `Path.split(path: String) -> List<String>`: path components. An absolute path's first component is `"/"`, and `""` gives an empty list.
- `Path.expand(path: String) -> String`: absolute path with `.` and `..` resolved. A leading `~` or `~/` expands to `HOME` (left literal when unset), and relative paths resolve against the working directory.
- `Path.relative_to(path: String, base: String) -> String`: path from `base` to `path`. Two relative paths give a minimal path that may walk up with `..`, two absolute paths only strip a shared prefix, and `path` is returned (normalized) when `base` is not a prefix or the kinds are mixed.

```koja
Path.join(["/usr", "local/", "bin"]).print() # "/usr/local/bin"
Path.extname("archive.tar.gz").print() # ".gz"
Path.expand("/foo/bar/../baz").print() # "/foo/baz"
Path.split("/foo/bar").print() # ["/", "foo", "bar"]
Path.relative_to("tmp/foo/bar", "tmp/bat").print() # "../foo/bar"
```

### `Enumeration<T, Cursor>` Protocol

```koja
protocol Enumeration<T, Cursor>
  fn cursor(self) -> Cursor

  fn next(self, cursor: Cursor) -> Option<(T, Cursor)>
end
```

Any type that implements `Enumeration<T, Cursor>` can be used with `for`. `List`, `String`, `Range`, `Map`, and `Set` conform.

`cursor` returns the initial traversal state. `next` returns an element and the next cursor, or `None` when traversal ends.

The source remains unchanged. Cursor types are implementation details, and callers must not interpret opaque cursors.

### `Equality` Protocol

```koja
protocol Equality
  fn equals?(self, other: Self) -> Bool
end
```

Powers the `==` and `!=` operators. Implemented for all numeric types, `Bool`, `String`, `Binary`, and `Bits`. `List<T>` implements it conditionally, element-wise, when `T` implements `Equality`.

### `Hash` Protocol

```koja
protocol Hash
  fn hash(self) -> Int
end
```

Required for keys in `Map<K, V>` and elements in `Set<T>`. Implemented for all numeric types, `Bool`, `String`, `Binary`, and `Bits`. Integers use SplitMix64, and strings and binaries use FNV-1a.

### `Bitwise` Protocol

```koja
protocol Bitwise
  fn band(self, other: Self) -> Self

  fn bor(self, other: Self) -> Self

  fn bxor(self, other: Self) -> Self

  fn bnot(self) -> Self

  fn bsl(self, n: Int) -> Self

  fn bsr(self, n: Int) -> Self
end
```

Bitwise operations are functions rather than symbolic operators. Koja reserves `<<`/`>>` for binary literals, `|` for union types, and `&` for protocol composition in trait bounds. All integer types implement `Bitwise`.

`bsl` and `bsr` panic when the shift count is negative or at least the receiver's bit width (`1.bsl(64)` on an `Int`), matching the [arithmetic fault](#arithmetic-faults) contract. The other four operations never fault.

```koja
flags = 0b1010
(flags.band(0b1100)).print() # 8  (0b1000)
flags.bor(0b0001).print() # 11 (0b1011)
1.bsl(4).print() # 16
16.bsr(4).print() # 1
```

### `Debug` Protocol

```koja
protocol Debug
  fn format(self) -> String

  fn print(self) # default: IO.puts(self.format())

  fn inspect(self) -> Self # default: prints, then returns self
end
```

`format` returns a round-trippable string representation of the value. `print` writes that string to stdout (via `IO.puts`) and returns `()`. `inspect` is the chainable variant. It prints and returns `self`, useful for tap-style debugging in the middle of an expression. The compiler auto-derives `Debug` for all types: primitives via intrinsics, enums as `VariantName` or `VariantName(payload)`, structs as `TypeName{field: value, ...}`. Generic types derive the same full field-by-field body as concrete ones. Fields whose type the derive does not render (`CPtr<T>`, function values) render as a literal `"..."` placeholder, though `CPtr<T>` itself has a real `Debug` impl that shows its hex address when formatted directly. Implementing `format` is enough to get `print` and `inspect` for free. Custom implementations can override the derived one via `impl Debug for MyType`.

`Debug.format` for `String` is round-trippable. It wraps the contents in double quotes and escapes `\`, `"`, `\n`, `\r`, `\t`. That means `.print()` shows top-level strings quoted, and aggregates render their `String` fields quoted too:

```koja
p = Point{x: 1, y: 2}
p.print() # Point{x: 1, y: 2}
"point is #{p}".print() # "point is Point{x: 1, y: 2}"
"n = #{42}".print() # "n = 42"
"hello".print() # "hello"
User{name: "alice"}.print() # User{name: "alice"}
```

For raw, unquoted output use `IO.puts` directly (it writes its `String` argument verbatim and adds a newline):

```koja
IO.puts("hello") # hello
IO.puts(p.format()) # Point{x: 1, y: 2}
```

### Literal Protocols

Literal protocols let custom types opt into contextual literal syntax. A conversion applies only to a literal expression, not to a variable or another expression.

Scalar protocols receive the canonical literal value and return `Self`:

```koja
protocol BoolLiteral
  fn from_bool(value: Bool) -> Self
end

protocol IntLiteral
  fn from_int(value: Int) -> Self
end

protocol FloatLiteral
  fn from_float(value: Float) -> Self
end

protocol StringLiteral
  fn from_string(value: String) -> Self
end
```

Negated numeric literals and interpolated strings also use these protocols. Sized numeric literal fitting stays separate, so `x: UInt8 = 4` still materializes a `UInt8` directly.

**`ListLiteral<T>`**: the compiler builds a `List<T>` from `[a, b, c]` and passes it to `from_list`:

```koja
protocol ListLiteral<T>
  fn from_list(list: List<T>) -> Self
end
```

`List<T>` and `Set<T>` implement `ListLiteral<T>`.

**`MapLiteral<K, V>`**: the compiler passes `[k: v, ...]` as an ordered list of entry tuples:

```koja
protocol MapLiteral<K, V>
  fn from_entries(entries: List<(K, V)>) -> Self
end
```

Entry order and duplicate keys remain available to the conformer. The default `Map<K, V>` carrier still lowers directly to `Map.new().put(...)` without an intermediate entry list.

Collection element, key, and value types come from the selected conformance. A non-generic type can therefore implement `ListLiteral<Item>` or `MapLiteral<Key, Value>`.

---

## Tooling

| Command       | Description                                      |
| ------------- | ------------------------------------------------ |
| `koja new`    | Scaffold a new project directory                 |
| `koja build`  | Compile to a native binary via LLVM              |
| `koja run`    | Build and execute in one step                    |
| `koja check`  | Type check without compiling                     |
| `koja test`   | Run `@test`-annotated functions                  |
| `koja tasks`  | List tasks from the project, deps, and toolchain |
| `koja deps`   | Fetch and inspect dependencies (`get`, `update`) |
| `koja format` | Opinionated code formatter (`--check` for CI)    |
| `koja doc`    | Generate static HTML documentation               |
| `koja lex`    | Dump tokens                                      |
| `koja parse`  | Dump AST                                         |

### Project Selection

Project-aware commands use the `koja.toml` in the current working directory by default. Use the global `-S, --project <directory>` option to select another project:

```sh
koja run -S ../my_app
koja test --project ../my_app
```

The selector controls the manifest, sources, dependencies, build directory, default documentation output, and diagnostic paths. It does not change the command or launched program working directory. Relative file operations in the program still use the caller's working directory.

The selector works with project-mode `build`, `check`, `run`, `shell`, `test`, `tasks`, `deps`, `format`, and `doc` commands. Do not combine it with a standalone source file or explicit `format` or `doc` paths.

### Project Scaffolding

`koja new <name>` creates a project directory with the following structure:

```
my_app/
  koja.toml
  src/
    app.koja
```

The `koja.toml` file defines the project configuration:

```toml
[project]
entry = "App"
koja = "0.17"
name = "my_app"
version = "0.1.0"
```

Fields:

- `name`: package identity, lowercase snake_case (used as the binary output name and the dependency key).
- `namespace`: PascalCase name code uses for qualified access. Optional, derived from `name` when omitted (`my_app` -> `MyApp`).
- `version`: semantic version string.
- `entry`: the type implementing `Process` that the program starts (required for `build`/`run`).
- `src`: source directories (default `["src"]`).
- `test`: test directories (default `["test"]`).
- `koja`: minimum compiler version, e.g. `koja = "0.17.0"`. A bare version, no operators. An older compiler refuses the package (and any package depending on it) with an error naming both versions.

A `[dependencies]` table declares path and git dependencies (see [Dependencies](#dependencies)), and a `[tasks]` table exports custom CLI tasks (see [Custom Tasks](#custom-tasks)).

### Custom Tasks

A package exports CLI tasks in its `koja.toml`, mapping a task name to a type implementing the `Koja.Task` protocol. Task names are prefixed with the package's `name`, so who provides a task is always visible and names never collide across the dependency graph:

```toml
[tasks]
"postgres.migrate" = "Migrate"
```

The type's `run` receives everything after `--` on the command line. Failing (via `fail` or a propagated `try`) prints the error to stderr and exits non-zero:

```koja
struct Migrate
end

impl Koja.Task for Migrate
  fn run(args: List<String>) ! String
    IO.puts("running migrations")
  end
end
```

Tasks run with `koja run <task.name> [-- args]` and are invocable from any project that depends on the exporting package. `koja tasks` lists every task in scope:

```
$ koja tasks
koja.new
myapp.seed
postgres.migrate
$ koja run postgres.migrate -- --dry-run
```

`Koja.Task` lives in the qualified `Koja` stdlib package, the toolchain's API surface. Like `koja test`, task runs execute through the standard `Process` pipeline: the driver synthesizes a process entry that calls the task type's `run` with the arguments.

The toolchain exports its own tasks through the `Koja` package, so they are in scope everywhere -- even outside a project. `koja new` is an alias for `koja run koja.new`.

### Language Server (LSP)

Real-time diagnostics, document formatting, hover (type signatures + `@doc`), and go-to-definition. Integrates with VS Code / Cursor via a bundled extension.

### Formatter

Zero-config, opinionated. `koja format` reformats in place (the whole project with no arguments, like `mix format`), and `koja format --check` exits non-zero if formatting differs. The formatter handles escape re-encoding for round-trip correctness and preserves annotations.
