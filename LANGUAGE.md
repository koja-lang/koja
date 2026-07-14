# Koja Language Reference

Koja is a statically typed, compiled language targeting native binaries via LLVM, with no garbage collector. It combines a Rust-inspired type system, Swift-style value semantics, an Erlang-style concurrency model, and Ruby-inspired syntax. The compiler itself is implemented as a Rust workspace.

---

## Table of Contents

- [Lexical Structure](#lexical-structure): Comments, Identifiers, Keywords, Operators, Numeric Literals, Line Continuation
- [Variables and Constants](#variables-and-constants): Assignment, Type Annotations, Compound Assignment, Constants
- [Functions](#functions): Declaration, Private Declarations, `return`, Parameters
- [Control Flow](#control-flow): `if`/`else`, `while`, `loop`/`break`, `for`...`in`, Ternary
- [Types](#types): Primitives, Numeric Widening, Arithmetic Faults, Unit, Strings, Structs, Enums, Union Types, Generics
- [Pattern Matching](#pattern-matching): `match`, OR Patterns, `cond`
- [Closures and Function Types](#closures-and-function-types): Block Closures, Short Closures, Capture Semantics, Function Types
- [Value Semantics](#value-semantics): Rules, Copy Cost, Field Access
- [Protocols](#protocols): Behavioral Contracts, Static Dispatch
- [Packages](#packages): Transparent Files, Visibility, Aliases
- [Concurrency](#concurrency): Processes, `spawn`/`receive`, `Ref`, `ReplyTo`, `Task`
- [Standard Library](#standard-library): Built-in Functions, Core Types, Collections, String Methods, Binary/Bits, File I/O, Parsing, URI, Protocols
- [C FFI](#c-ffi): `@extern "C"`, `CPtr<T>`, `CString`
- [Annotations](#annotations): `@doc`
- [Tooling](#tooling): CLI Commands, LSP, Formatter

---

## Lexical Structure

### Comments

Line comments start with `#` and extend to the end of the line. There are no block comments.

```koja
# This is a comment
x = 42  # inline comment
```

### Identifiers

- **Values** use `snake_case`: variables, functions, parameters, fields.
- **Types** use `PascalCase`: structs, enums, protocols, type parameters, primitives.
- Identifiers may contain `?` (conventionally for boolean-returning functions like `empty?()`, `some?()`).

### Keywords

```
alias, break, cond, const, else, end, enum, false, fn, for,
if, impl, in, loop, match, not, priv, protocol,
receive, return, self, spawn, struct, true, type, unless, when
```

`and` and `or` are operator-identifiers, not reserved keywords. They act as infix boolean operators in expressions (`a and b`, `x or y`) but can also be used freely as method names, function names, or field names (e.g., `option.or(default)`).

### Operators

Precedence from lowest to highest:

| Precedence | Operators                   |
| ---------- | --------------------------- |
| 1          | `or`                        |
| 2          | `and`                       |
| 3          | `not` (prefix)              |
| 4          | `==` `!=` `<` `>` `<=` `>=` |
| 5          | `+` `-` `<>`                |
| 6          | `*` `/` `%`                 |
| 7          | `-` (unary negation)        |
| 8          | `.field` `.fn()` `()`       |

`and` and `or` evaluate left to right and short-circuit. `a and b`
evaluates `b` only when `a` is `true`. `a or b` evaluates `b` only when
`a` is `false`. Both operands are still typechecked as `Bool`.

`<>` concatenates `String`, `Binary`, and `Bits` values. Both operands must be the same type, with no cross-type mixing.

Assignment operators: `=`, `+=`, `-=`, `*=`, `/=`.

### Numeric Literals

```koja
42          # decimal integer
3.14        # floating point
0xFF        # hexadecimal
0b1010      # binary
1_000_000   # underscore separators (ignored)
0xFF_FF     # underscores in hex
```

Numeric literals coerce to any same-category type annotation. Integer literals coerce to any integer type (`x: UInt8 = 4`). Float literals coerce to any float type (`f: Float32 = 3.14`). Cross-category coercion (int to float or vice versa) is an error. Non-literal sized values widen implicitly into `Int` / `Float`. See [Numeric Widening](#numeric-widening).

A literal must fit its type. An integer literal outside the target's range is a compile-time error, and so is a float literal whose magnitude is too large for a 64-bit float (one that would round to infinity). `Float` values are always finite (see [Arithmetic Faults](#arithmetic-faults)).

### Line Continuation

Newlines terminate statements. Line continuation is implicit after binary operators, `.`, and `,`. A line starting with `and`, `or`, or the ternary `?` also continues the previous expression, so wrapped conditions lead each continuation line with the operator.

```koja
if request.valid? and request.authorized?
  and request.body.present?

  handle(request)
end
```

---

## Variables and Constants

Variables are declared by assignment. No `let`, `var`, or `mut` keywords.

```koja
x = 42
name = "koja"
```

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

### Assignment and Value Semantics

Every binding holds an independent value. Assignment copies, and the original stays usable:

```koja
p1 = Point{x: 1, y: 2}
p2 = p1
p1.x    # still valid, p2 is an independent copy
```

Copies are observably independent for every type. Mutating one binding never affects another, and a value remains usable for as long as it is in scope.

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

---

## Functions

Functions are declared with `fn`. The last expression is the implicit return value.

```koja
fn add(a: Int32, b: Int32) -> Int32
  a + b
end
```

Functions without a return type return `()`. Parameters require explicit types. Return type annotation is required if the function returns a value.

A compiled program's entry point is a type implementing the `Process` protocol, named by `entry` in `koja.toml`. There is no `fn main`. Scripts (`.kojs`) execute top-level statements directly. Most functions are declared inside `impl` blocks on a struct or enum. See [Impl Functions](#impl-functions) and [Static Functions](#static-functions).

### Private Declarations

`priv` restricts a declaration's visibility based on where it appears:

- A top-level `priv` declaration (`fn`, `struct`, `enum`, `const`, `type`,
  `protocol`) is **package-private**: it's usable from any file in the same
  package, but rejected from any other package.
- A `priv fn` declared inside a `struct`, `enum`, or `impl` body is
  **type-private**: it's callable from any other method on the same target
  type (whether declared in the type's decl block, an `extend Type` block,
  or an `impl Protocol for Type` block), but rejected everywhere else.

```koja
priv fn helper(x: Int32) -> Int32    # package-private
  x * 2
end

priv const RETRY_LIMIT: Int32 = 3    # package-private

priv struct Bucket                   # package-private
  count: Int32
end

struct Counter
  value: Int32

  fn increment(self) -> Counter
    Counter { value: self.tick() }    # ok: same type
  end

  priv fn tick(self) -> Int32         # type-private to Counter
    self.value + 1
  end
end
```

A public declaration cannot leak a private type through its signature. A public function whose parameter or return type names a private type, or a public struct field, enum variant payload, type alias, or protocol method that mentions one, is a compile error. Callers outside the package could see the type but never name it, so the compiler rejects the leak at the declaration site.

`@doc` on a private declaration is also a compile error. Private items never appear in generated documentation, so use regular `#` comments instead.

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

### Parameters

Parameters are passed by value. The callee receives its own independent copy and the caller's binding stays usable afterward:

```koja
fn describe(c: Config) -> String
  c.name                 # operates on a private copy
end
```

There is no parameter-passing modifier. Every parameter is a value. (Earlier releases had a `move` keyword. It has been removed, since value semantics make it meaningless.)

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

### `for` ... `in`

Iterates over any type implementing the `Enumeration<T>` protocol:

```koja
list: List<Int32> = List.new()
list = list.append(1)
list = list.append(2)
list = list.append(3)

for item in list
  item.print()
end
```

The loop variable is bound directly to each element. No unwrapping needed.

### Ternary

```koja
y = x > 2 ? "big" : "small"
```

Nested ternaries are disallowed.

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

All types have value semantics. Assignment produces an independent copy. Numeric primitives and `Bool` copy bit-for-bit. `String`, `Binary`, `Bits`, `List`, `Map`, `Set`, structs, and enums copy their contents. The distinction is only one of cost, never of semantics.

### Numeric Widening

Sized numeric values widen implicitly into their hub type, and only into their hub type. `Int8`, `Int16`, `Int32`, `UInt8`, `UInt16`, and `UInt32` widen to `Int` (signed sources sign-extend, unsigned sources zero-extend). `Float32` widens to `Float`. The conversion is always lossless.

```koja
fn count(n: Int) -> Int
  n
end

small: Int32 = -7
count(small)        # Int32 widens to Int, value stays -7
```

Widening applies wherever a value flows into a typed slot: call arguments, struct fields, enum payloads, return values, annotated bindings, and constant initializers. It does **not** apply to:

- **Binary operators**: operands must be the same width. `Int32 + Int` is an error. Widen explicitly first.
- **Sideways conversions**: `Int8` does not widen to `Int16`, `UInt8` does not widen to `UInt16`. Each source type has exactly one implicit target.
- **`UInt64`**: it does not fit in `Int`. Use the checked `to_int` method.
- **Generic inference**: `T` binds to the actual type. `identity(small)` infers `T = Int32`, not `Int`.
- **Narrowing or cross-category conversion**: `Int` never implicitly becomes `Int32`, and ints never become floats.

The inverse direction is explicit and checked. `Int` provides `to_int8`, `to_int16`, `to_int32`, `to_uint8`, `to_uint16`, `to_uint32`, and `to_uint64`, each returning `Result<TargetType, NumericConversionError>`, with `Result.Err(NumericConversionError.OutOfRange)` when the value does not fit. `UInt64.to_int` is the checked bridge back to the hub, and `Float.to_float32` rounds to the nearest representable value, with `OutOfRange` for magnitudes too large for a 32-bit float:

```koja
match 300.to_int8()
  Result.Ok(v) -> v.print()
  Result.Err(e) -> "does not fit".print()   # 300 > Int8.max
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
a + 1        # panics: integer overflow in +

b = 0.0
1.0 / b      # panics: non-finite float result in /
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
msg = """
  first line
  second line
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

#### Construction

```koja
p = Point{x: 1, y: 2}
```

Short structs format inline. Long structs break across lines with trailing commas:

```koja
config = Config{
  name: "production",
  port: 8080,
  debug: false,
}
```

#### Field Access

```koja
p.x.print()
p.y.print()
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

Methods receive `self` by value. A "mutating" method does not change the receiver in place. It computes a new value and returns it, and the caller rebinds:

```koja
struct Counter
  value: Int

  fn increment(self) -> Self
    Counter{value: self.value + 1}
  end
end

c = Counter{value: 0}
c = c.increment()   # rebind to the returned value
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

Methods declared in an `extend` block have ambient visibility. They're callable from any package that can name the target type. Collisions on the same method name across `extend` blocks targeting the same type are a compile error.

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

`extend` blocks can target a specific instantiation of a generic type. Methods defined in a specialized extend are only available when the type argument matches:

```koja
extend CPtr<UInt8>
  fn to_cstring(self) -> CString
    CString{ptr: self, len: strlen(self)}
  end
end
```

`to_cstring` is only available on `CPtr<UInt8>`, not on `CPtr<Int32>` or other instantiations. Calling a specialized method on the wrong type argument produces a compile error with a hint showing which specialization provides the method.

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
struct Pair<A, B>
  first: A
  second: B
end

p = Pair{first: 10, second: "hello"}
```

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

fn empty_pair() -> Pair<Int, Option<String>>
  Pair.new(1, Option.None)
end
```

A context-free unit variant still requires an annotation.

#### Annotation-Driven Inference

Type annotations on variables drive generic type inference:

```koja
list: List<Int32> = List.new()  # infers T = Int32
```

#### Implementation

Generics compile via monomorphization. The compiler generates specialized native code for each concrete type instantiation. Unused instantiations produce no binary output.

---

## Pattern Matching

### `match`

Pattern matching with exhaustiveness checking:

```koja
result = match x
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
  Point{x: 5}       -> "x is five"   # y is unconstrained
  Point{x: x, y: y} -> "(#{x}, #{y})"
end

# Enum-struct variants follow the same rules.
match shape
  Shape.Rect{width: w, height: h} -> w * h
  Shape.Circle{radius: r}         -> r * r * 314 / 100
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

`match` reads the matched value without consuming it. The original variable remains live inside all arms and after the `match` expression.

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

Arms can use any boolean expression, including method calls:

```koja
cond
  c.digit?() -> handle_digit(c)
  c.whitespace?() -> skip_whitespace()
  c == "+" -> handle_plus()
  else -> handle_unknown(c)
end
```

---

## Closures and Function Types

### Block Closures

Closures use `fn (...) -> T ... end` syntax, mirroring function signatures:

```koja
double = fn (x: Int32) -> Int32 x * 2 end

add = fn (a: Int32, b: Int32) -> Int32
  a + b
end
```

Closure parameters are passed by value, like function parameters:

```koja
measure = fn (data: String) -> Int data.length() end
```

### Short Closures

Short closures use `param -> expr` syntax with parameter types inferred from the calling context:

```koja
option.map(x -> x + 1)
list.filter(n -> n > 3)
names.map(name -> name.upcase())
```

Works at inline call sites including generic methods. For multi-parameter or multi-statement closures, use the block form above.

### Capture Semantics

Closures capture variables from their enclosing scope by value. Each captured variable is copied into the closure's environment, and the original stays usable:

```koja
multiplier = 3
triple = fn (x: Int32) -> Int32
  x * multiplier    # multiplier is copied into the closure
end
multiplier.print()  # still live
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

---

## Value Semantics

Koja uses value semantics. Every binding, parameter, return, and field is an independent value, with memory managed automatically by the runtime.

### Rules

1. Assignment copies. The source remains usable.
2. Function and closure parameters are passed by value. The caller's binding survives the call.
3. There is no aliasing. Mutating one binding never affects another.
4. A value is usable for as long as it is in scope.

> Memory note: heap-backed values (strings, collections, composites) are reclaimed by reference counting. Blocks are shared while live and freed deterministically at scope exit when the last owner drops. This is scope-bound, not a garbage collector. There are no pauses and no background collector. See the README for production-readiness status.

### Copy Cost

All types copy on assignment. Numeric primitives, `Bool`, `()`, and function pointers copy bit-for-bit. `String`, `Binary`, `Bits`, `List`, `Map`, `Set`, structs, and enums are heap-backed, but the copy is cheap. The block is reference-counted and shared, with a copy made lazily only if a shared value is mutated (copy-on-write). The result is always an independent value:

```koja
a = 42
b = a     # a is still live
```

### Field Access

Field access reads a value out of the struct without disturbing the owner. You can read fields freely:

```koja
struct Wrapper
  name: String
  count: Int
end

w = Wrapper{name: "hello", count: 1}
w.name.print()    # w is still live
w.count.print()
w.name.print()
```

This extends to chained access and method calls:

```koja
w.name.length()   # reads name, calls length. w is still live
```

To mutate a field, use reassignment. The right-hand side transforms the current field value and the result is written back:

```koja
w.name = w.name.upcase()
w.name.print()              # "HELLO"
```

---

## Protocols

Protocols define behavioral contracts. Types implement protocols via `impl Protocol for Type`.

```koja
protocol Greeter
  fn greet(self) -> String
end

struct Cat
  name: String
end

impl Greeter for Cat
  fn greet(self) -> String
    "meow, I'm #{self.name}"
  end
end
```

The compiler validates completeness (all protocol functions must be implemented) and signature compatibility. `priv fn` helpers are allowed in impl blocks. `@doc` annotations are supported on protocol declarations.

`Self` inside a protocol declaration is syntactic sugar for an implicit first type parameter on the protocol. It is the slot every conforming type fills in via `impl Protocol for ConcreteType`. Methods that mention `Self` in their signature (return type, non-receiver param) treat it as that synthetic param. In an `impl Protocol for ConcreteType` block, the synthetic param resolves to `ConcreteType` and the method's `Self` ends up typed as the concrete implementer. User-declared protocol type parameters (e.g. `protocol Eq<T>`) are appended after the synthetic `Self` slot. The name `Self` is reserved on protocols and cannot also be declared explicitly.

### Trait Bounds

Generic type parameters can be constrained to types implementing specific protocols using `:` syntax:

```koja
fn say_hello<T: Greeter>(animal: T) -> String
  animal.greet()
end
```

Multiple bounds use `&` (the protocol composition operator, complementing `|` for union types):

```koja
fn describe_and_greet<T: Greeter & Description>(animal: T) -> String
  animal.describe() <> " says " <> animal.greet()
end
```

Bounds are verified at call sites. If a concrete type doesn't implement a required protocol, the compiler emits an error:

```
type `Cat` does not implement protocol `Description` (required by type parameter `T` in `describe_and_greet`)
```

Inside the function body, protocol methods can be called directly on bounded type parameters. The compiler resolves the method through the protocol's signature.

Unbounded type parameters (`<T>`) remain valid and backwards compatible.

### Dispatch

Protocol dispatch is static via monomorphization. No vtables, no dynamic dispatch.

---

## Packages

A package is the unit of code organization, defined by a `koja.toml` manifest. Files within a package are transparent: they share one namespace, and every top-level declaration (type, function, constant) is visible from every other file in the package. Files carry no namespace of their own, and there are no imports:

```koja
# src/helper.koja
fn add(a: Int, b: Int) -> Int
  a + b
end

# src/app.koja
struct App
end

impl Process<(), (), ()> for App
  fn start(config: ()) -> Result<Self, StopReason>
    Result.Ok(App{})
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

Other packages (the qualified standard library and dependencies) are reached through their package name: `JSON.Decoder`, `Net.TCPSocket`, `HTTP.get(...)`.

### Visibility

Access control is at the declaration level (`priv`), not the file level:

- A top-level `priv` declaration (`fn`, `struct`, `enum`, `const`, `type`,
  `protocol`) is **package-private**: usable from any file in the same
  package, rejected from other packages.
- `priv fn` declared inside a `struct`, `enum`, `extend`, or `impl` body
  is **type-private**: callable from any other method on the same target
  type (across the decl block and any `extend` or `impl Protocol for Type`
  block on that type), rejected everywhere else.

See [Private Declarations](#private-declarations) for examples.

### Aliases

When using types from qualified standard library packages or dependency packages, `alias` creates a file-private shorthand:

```koja
alias Net.TCPSocket
alias JSON.Decoder
alias JSON.Encoder as JSONEncoder

conn = TCPSocket.connect("example.com", 80)
```

`alias Net.TCPSocket` makes `TCPSocket` available as a local name. `alias JSON.Encoder as JSONEncoder` binds a custom local name. Aliases are scoped to the declaring file and don't affect other files.

Aliases name types only. Package-level functions are called with qualified syntax directly, no alias needed:

```koja
response = HTTP.get("https://example.com")
```

### Standard library visibility

The auto-imported `Global` package provides core types (`Option`, `Result`, `List`, `Map`, `Set`, `Process`, `IO`, `File`, `URI`, etc.) with no alias needed. Domain-specific packages require qualified access:

- **`Crypto`**: `SHA1`, `SHA256`, `SHA384`, `SHA512`, `HMAC`, `Certificate`, `PrivateKey`, `PEMError`
- **`Net`**: `TCPSocket`, `TCPListener`, `UDPSocket`, `Socket`, `IPAddress`, `SocketAddress`, `SocketKind`, `SocketError`, `TLSSession`, `TLSConfig`, `TLSIdentity`, `TrustStore`, `TLSError`, `VerificationError`

Use `alias Crypto.SHA256` or `alias Net.TCPSocket` to access them.

---

## Concurrency

Koja uses a message-passing actor model inspired by Erlang/Elixir. Processes have isolated memory and communicate exclusively through typed messages. Messages are passed by value (each process receives its own copy), so there is no shared mutable state.

Process timeout and delay values are measured in milliseconds. Negative
values behave as zero.

### `Task<R>`

The simplest way to run concurrent work. Wraps a closure, runs it in a spawned process, and returns the result:

```koja
ref = Task.async(fn -> expensive_computation() end)
result = Task.await(ref)  # Result<R, CallError>, times out after 5000ms
```

`Task.async(fn)` spawns the closure and returns a `Ref<(), R>`. `Task.await(ref)` sends a unit message and waits for the reply.

### `Process<C, M, R>` Protocol

For stateful, long-lived processes, implement the `Process` protocol. `C` is the config type, `M` is the message type, `R` is the reply type.

```koja
protocol Process<C, M, R>
  fn start(config: C) -> Result<Self, StopReason>
  fn handle(self, msg: M, from: Option<ReplyTo<R>>) -> Step<Self>
  fn handle_signal(self, event: Lifecycle) -> Step<Self>
  fn run(self) -> StopReason
end
```

`start` builds the initial state from config in the child process context, before the receive loop begins. Return `Result.Ok(self)` to begin running, or `Result.Err(reason)` to abort startup.

`handle` returns `Step<Self>`. Return `Step.Continue(self)` to keep running with updated state, or `Step.Done(reason)` (with a `StopReason` of `Normal` or `Shutdown`) to stop.

`handle_signal` has a default implementation that stops on `Shutdown`/`Interrupt` and continues on `Reload`. Override it for graceful drain or hot config reload.

`run` has a default implementation that enters a receive loop, dispatching business messages to `handle` and lifecycle events to `handle_signal`, and stopping when either returns `Step.Done`:

```koja
fn run(self) -> StopReason
  receive
    pair: Pair<M, Option<ReplyTo<R>>> ->
      match self.handle(pair.first, pair.second)
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
enum CounterMsg
  Increment
  Decrement
end

struct Counter
  count: Int
end

impl Process<Counter, CounterMsg, Int> for Counter
  fn start(config: Counter) -> Result<Self, StopReason>
    Result.Ok(config)
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

`Lifecycle` abstracts OS signals into a platform-agnostic enum:

```koja
enum Lifecycle
  Shutdown    # SIGTERM
  Interrupt   # SIGINT
  Reload      # SIGHUP
end
```

`StopReason` represents intentional process termination:

```koja
enum StopReason
  Normal      # process finished its work
  Shutdown    # process was told to stop
end
```

The `ExitStatus` protocol maps a `StopReason` to an OS exit code (only relevant for the entry process):

```koja
protocol ExitStatus
  fn code(self) -> Int
end
```

`ExitReason` is what a supervisor sees when a child stops:

```koja
enum ExitReason
  Normal
  Shutdown
  Crashed(String)
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
- `call(msg: M, timeout: Int) -> Result<R, CallError>`: sends a message and blocks up to `timeout` milliseconds for a reply. Returns `Result.Ok(reply)` on success, `Result.Err(CallError.Timeout)` if the process didn't reply in time, or `Result.Err(CallError.ProcessDown)` if the process is dead.
- `signal(event: Lifecycle)`: sends a lifecycle signal to the process (e.g. `Lifecycle.Shutdown`). Delivered to `handle_signal`.
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
ref.signal(Lifecycle.Shutdown)
```

### `ReplyTo<R>` and `reply`

When a process receives a `call`, the handler gets a `ReplyTo<R>` channel to send the response back. The type `R` is enforced at compile time. The channel carries the caller's process id plus a correlation token minted per call, so stale replies from earlier timed-out calls are discarded instead of delivered to the next call.

```koja
struct ReplyTo<R>
  id: Int
  token: Int
end
```

- `send(reply: R)`: sends the reply back to the caller.

`ReplyTo.reply(from, value)` is a convenience on `ReplyTo<R>` that handles the common pattern of replying only when a caller is present (skips silently for `cast` messages):

```koja
extend ReplyTo<R>
  fn reply(from: Option<ReplyTo<R>>, value: R)
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
  pair: Pair<M, Option<ReplyTo<R>>> ->
    # handle the message
end
```

An optional `after` clause bounds the wait: if no message arrives within the timeout (in milliseconds), the `after` body runs instead. The timeout is any `Int` expression:

```koja
receive
  pair: Pair<M, Option<ReplyTo<R>>> ->
    # handle the message
after 5000
  # no message within 5 seconds
end
```

In most cases you won't use `receive` directly. The `Process` protocol's default `run` implementation handles it for you.

---

## Standard Library

The following types and functions are available in every file with no alias needed.

### Built-in Functions

> **Note:** Koja uses value semantics. Every binding, parameter,
> return, and field is an independent value. Assigning or passing a
> value already yields an independent copy (cheaply, via reference-
> counted copy-on-write under the hood), so there is no `clone()`:
> just assign or pass the value.

### `Kernel`

Core runtime operations.

#### `Kernel.exit(code: Int)`

Terminates the process immediately with the given exit code. `0` indicates success, and any non-zero value indicates failure.

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

Functions: `unwrap()`, `or(default)`, `some?()`, `none?()`, `map(fn (T) -> U)`, `then(fn (T) -> Option<U>)`.

```koja
x = Option.Some(42)
x.unwrap().print()       # 42
x.or(0).print()          # 42
x.some?().print()        # true

y: Option<Int> = Option.None
y.or(99).print()          # 99

mapped = x.map(fn (v: Int) -> Int v * 10 end)
mapped.unwrap().print()   # 420
```

### `Result<T, E>`

```koja
enum Result<T, E>
  Ok(T)
  Err(E)
end
```

Functions: `unwrap()`, `or(default)`, `ok?()`, `err?()`, `map(fn (T) -> U)`, `then(fn (T) -> Result<U, E>)`.

```koja
ok: Result<Int32, Int32> = Result.Ok(42)
ok.unwrap().print()       # 42

err: Result<Int32, Int32> = Result.Err(1)
err.or(99).print()        # 99
```

### `Pair<A, B>`

```koja
struct Pair<A, B>
  first: A
  second: B
end
```

Fields: `first`, `second`.

```koja
p: Pair<Int, String> = Pair{first: 10, second: "hello"}
p.first.print()    # 10
p.second.print()   # hello
```

Generic struct literals like `Pair{first: x, second: y}` infer their type parameters from the field values when each field's expected type-param appears in at least one field. A type annotation on the binding (`p: Pair<Int, String> = ...`) is only required when no positional field uniquely binds a parameter, for example a struct that only mentions some of its parameters in its fields' types.

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
hello.print()  # "hello"
```

### `List<T>`

Dynamically-sized, heap-backed collection. Compiler intrinsic backed by C's `malloc`/`realloc`/`free`.

```koja
list: List<Int32> = List.new()
list = list.append(10)
list = list.append(20)

list.length().print()   # 2
list.get(0).unwrap().print()  # 10
list.empty?().print()   # false
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

```koja
nums = [1, 2, 3, 4, 5]
doubled = nums.map(fn (n: Int) -> Int n * 2 end)
evens = nums.filter(fn (n: Int) -> Bool n % 2 == 0 end)
has_big = nums.any?(fn (n: Int) -> Bool n > 3 end)
all_pos = nums.all?(fn (n: Int) -> Bool n > 0 end)
```

List literals (`[a, b, c]`) are backed by the `ListLiteral<T>` protocol. See [Literal Protocols](#literal-protocols).

### `Map<K, V>`

A generic hash map. Keys must implement `Hash` and `Equality`. Uses open addressing with linear probing.

```koja
m: Map<String, Int> = Map.new()
m = m.put("a", 1)
m = m.put("b", 2)

m.get("a").unwrap().print()  # 1
m.has?("b").print()          # true
m.length().print()           # 2
```

Functions:

- `new() -> Map<K, V>`: creates an empty map.
- `put(self, key: K, value: V) -> Map<K, V>`: inserts or updates a key-value pair.
- `get(self, key: K) -> Option<V>`: returns `Option.Some(value)` if the key exists, `Option.None` otherwise.
- `has?(self, key: K) -> Bool`: returns `true` if the key exists.
- `remove(self, key: K) -> Map<K, V>`: removes the entry for the key. Returns the map unchanged if the key is absent.
- `length(self) -> Int`: returns the number of entries.
- `empty?(self) -> Bool`: returns `true` if the map has no entries.

`Map` does not currently support iteration. To iterate over entries, use `List<Pair<K, V>>` as an ordered key-value collection instead.

Map literals (`[key: value, ...]`) are backed by the `MapLiteral<K, V>` protocol. See [Literal Protocols](#literal-protocols).

### `Set<T>`

A generic hash set of unique elements. Elements must implement `Hash` and `Equality`. Uses open addressing with linear probing.

```koja
s: Set<Int> = Set.new()
s = s.insert(1)
s = s.insert(2)
s = s.insert(1)

s.length().print()   # 2
s.has?(1).print()     # true
```

Functions:

- `new() -> Set<T>`: creates an empty set.
- `insert(self, item: T) -> Set<T>`: adds an element. Returns unchanged if already present.
- `has?(self, item: T) -> Bool`: returns `true` if the element exists.
- `remove(self, item: T) -> Set<T>`: removes the element. Returns unchanged if absent.
- `length(self) -> Int`: returns the number of elements.
- `empty?(self) -> Bool`: returns `true` if the set has no elements.

`Set<T>` implements `ListLiteral<T>`, so list literal syntax constructs a set when the target type is `Set<T>`:

```koja
names: Set<String> = ["alice", "bob", "alice"]  # Set with 3 elements
```

### String Methods

`String` implements `Enumeration<String>`, so strings can be iterated character-by-character with `for`:

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
- `to_float(self) -> Result<Float, NumericConversionError>`: parses the string as a 64-bit float (see [Parsing](#parsing)).
- `to_int(self) -> Result<Int, NumericConversionError>`: parses the string as a 64-bit signed integer (see [Parsing](#parsing)).
- `trim(self) -> String`: returns a copy with leading and trailing whitespace removed.
- `trim_end(self) -> String`: returns a copy with trailing whitespace removed.
- `trim_start(self) -> String`: returns a copy with leading whitespace removed.
- `upcase(self) -> String`: returns a copy with ASCII lowercase letters converted to uppercase.
- `whitespace?(self) -> Bool`: returns `true` if the string contains only whitespace characters (space, `\n`, `\r`, `\t`).

```koja
s = "hello world"
s.length().print()                            # 11
s.get(0).unwrap().print()                     # "h"
s.contains?("world").print()                  # true
s.starts_with?("hello").print()               # true
s.split(" ").length().print()                 # 2
s.upcase().print()                            # "HELLO WORLD"
s.slice(Range{start: 0, stop: 4}).print()     # "hello"
"  hello  ".trim().print()                    # "hello"
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

`Binary`-typed values splice their bytes into the literal, so a framed message builds in one expression. A bare segment is a splice whenever its value is `Binary`-typed; `payload: Binary` spells it out explicitly. Splices take no width or endianness modifiers, and the fixed-width segments around a splice must total whole bytes:

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
- `to_string(self) -> Result<String, String.ConversionError>`: attempts to interpret bytes as UTF-8, returning `InvalidUTF8` when decoding fails.

#### Conversion Functions

- `String.to_binary(self) -> Binary`: zero-cost widening from UTF-8 string to bytes.
- `CPtr<UInt8>.to_binary(self, len: Int) -> Binary`: creates a `Binary` by copying `len` bytes from the pointer. The pointer is not freed. A negative length panics.
- `Bits.to_binary(self) -> Result<Binary, String>`: narrows bits to bytes. Returns `Result.Err` if the bit length is not divisible by 8.

```koja
bin = "hello".to_binary()
bits = bin.to_bits()
roundtrip = bits.to_binary().unwrap().to_string().unwrap()
roundtrip.print()  # "hello"
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

- `read(self, count: Int) -> Result<String, String>`: reads and validates up to `count` bytes as UTF-8.
- `read_binary(self, count: Int) -> Result<Binary, String>`: reads up to `count` arbitrary bytes.
- `write(self, data: Binary | String) -> Result<Int, String>`: writes data, returns bytes written.
- `close(self) -> Result<String, String>`: closes the descriptor.

#### `File`

Higher-level file operations wrapping `Fd`:

```koja
struct File
  fd: Fd
end
```

Functions:

- `File.open(path: String, mode: FileMode) -> Result<File, String>`: opens a file with the given mode (`FileMode.Read`, `FileMode.Write`, `FileMode.Append`).
- `File.read(path: String) -> Result<String, String>`: reads an entire file as UTF-8 text (opens, reads, closes).
- `File.read_binary(path: String) -> Result<Binary, String>`: reads an entire file as arbitrary bytes.
- `File.write(path: String, content: Binary | String) -> Result<String, String>`: writes text or arbitrary bytes (creates or truncates).
- `File.exists?(path: String) -> Bool`: returns true if the file exists.
- `File.delete(path: String) -> Result<String, String>`: deletes a file.
- `File.rename(source: String, destination: String) -> Result<String, String>`: renames (moves) a file.
- `close(self) -> Result<String, String>`: closes the file handle.

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

- `Int.parse(input: String) -> Result<Int, NumericConversionError>`: parses a string as a 64-bit signed integer.
- `Float.parse(input: String) -> Result<Float, NumericConversionError>`: parses a string as a 64-bit float.

Failures distinguish malformed text from values that don't fit: `NumericConversionError.InvalidFormat` for text that isn't a number, `NumericConversionError.OutOfRange` for a well-formed number outside the target's range (an integer overflowing 64 bits, or a float magnitude like `1e999` that would round to infinity). Only finite floats parse. There is no literal syntax for infinities or NaN. This is the same error enum the checked narrowing methods use (see [Numeric Widening](#numeric-widening)).

```koja
x = Int.parse("42").unwrap()
x.print()  # 42

y = Float.parse("3.14").unwrap()
y.print()  # 3.14

match Int.parse("99999999999999999999")
  Result.Ok(_) -> ()
  Result.Err(e) -> e.print()  # OutOfRange
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

- `URI.parse(input: String) -> Result<URI, URI.Error>`: parses and validates an absolute or relative URI. The scheme is lowercased, and a known scheme's default port fills `port` when the input has none. Errors carry the offending part of the input.
- `to_string(self) -> String`: reassembles the URI, omitting the port when it equals the scheme's default.
- `URI.encode(input: String) -> String`: percent-encodes every character that is neither reserved nor unreserved.
- `URI.decode(input: String) -> Result<String, URI.Error>`: percent-unescapes, rejecting malformed `%XX` sequences and invalid UTF-8.
- `URI.default_port(scheme: String) -> Option<Int>`: the well-known port for a scheme (`"https"` gives `443`), or `Option.None`.

`URI` implements `Equality` (component-wise) and `Debug` (`format` renders the assembled URI string, so interpolation produces the URL).

```koja
uri = URI.parse("https://example.com/pkg?v=1").unwrap()
uri.host.unwrap().print()      # "example.com"
uri.port.unwrap().print()      # 443
"fetching #{uri}".print()      # "fetching https://example.com/pkg?v=1"

URI.encode("put it+й").print() # "put%20it+%D0%B9"
```

### `Enumeration<T>` Protocol

```koja
protocol Enumeration<T>
  fn length(self) -> Int
  fn get(self, index: Int) -> Option<T>
end
```

Any type implementing `Enumeration<T>` can be used with `for` loops. `List<T>` and `String` implement this protocol. `get` returns `Option<T>` instead of panicking on out-of-bounds access. `for` loops unwrap the `Option` automatically.

### `Equality` Protocol

```koja
protocol Equality
  fn eq(self, other: Self) -> Bool
end
```

Powers the `==` and `!=` operators. Implemented for all numeric types, `Bool`, and `String`.

### `Hash` Protocol

```koja
protocol Hash
  fn hash(self) -> Int
end
```

Required for keys in `Map<K, V>` and elements in `Set<T>`. Implemented for all numeric types, `Bool`, and `String`. Integers use SplitMix64, and strings use FNV-1a.

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

Bitwise operations are methods rather than symbolic operators. Koja reserves `<<`/`>>` for binary literals, `|` for union types, and `&` for protocol composition in trait bounds. All integer types implement `Bitwise`.

`bsl` and `bsr` panic when the shift count is negative or at least the receiver's bit width (`1.bsl(64)` on an `Int`), matching the [arithmetic fault](#arithmetic-faults) contract. The other four operations never fault.

```koja
flags = 0b1010
(flags.band(0b1100)).print()  # 8  (0b1000)
flags.bor(0b0001).print()   # 11 (0b1011)
1.bsl(4).print()             # 16
16.bsr(4).print()            # 1
```

### `Debug` Protocol

```koja
protocol Debug
  fn format(self) -> String
  fn print(self)                # default: IO.puts(self.format())
  fn inspect(self) -> Self # default: prints, then returns self
end
```

`format` returns a round-trippable string representation of the value. `print` writes that string to stdout (via `IO.puts`). The receiver stays live and the call returns `()`. `inspect` is the chainable variant. It prints and returns `self`, useful for tap-style debugging in the middle of an expression. The compiler auto-derives `Debug` for all types: primitives via intrinsics, enums as `VariantName` or `VariantName(payload)`, structs as `TypeName{field: value, ...}`. Generic types derive the same full field-by-field body as concrete ones. Fields whose type has no meaningful rendering (`CPtr<T>`, `Binary`, `Bits`, function values) render as a literal `"..."` placeholder. Implementing `format` is enough to get `print` and `inspect` for free. Custom implementations can override the derived one via `impl Debug for MyType`.

`Debug.format` for `String` is round-trippable: it wraps the contents in double quotes and escapes `\`, `"`, `\n`, `\r`, `\t`. That means `.print()` shows top-level strings quoted, and aggregates render their `String` fields quoted too:

```koja
p = Point{x: 1, y: 2}
p.print()                       # Point{x: 1, y: 2}
"point is #{p}".print()         # "point is Point{x: 1, y: 2}"
"n = #{42}".print()             # "n = 42"
"hello".print()                 # "hello"
User{name: "alice"}.print()     # User{name: "alice"}
```

For raw, unquoted output use `IO.puts` directly (it writes its `String` argument verbatim and adds a newline):

```koja
IO.puts("hello")                # hello
IO.puts(p.format())             # Point{x: 1, y: 2}
```

### Literal Protocols

List and map literals are backed by protocols, allowing custom types to opt into literal syntax.

**`ListLiteral<T>`**: the compiler builds a `List<T>` from `[a, b, c]` and passes it to `from_list`:

```koja
protocol ListLiteral<T>
  fn from_list(list: List<T>) -> Self
end
```

`List<T>` and `Set<T>` implement `ListLiteral<T>`.

**`MapLiteral<K, V>`**: the compiler builds a `Map<K, V>` from `[k: v, ...]` and passes it to `from_map`:

```koja
protocol MapLiteral<K, V>
  fn from_map(map: Map<K, V>) -> Self
end
```

`Map<K, V>` implements `MapLiteral<K, V>`.

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

A `Float32` / `Float64` value returned by an extern call is checked at the call site: a NaN or infinity handed back by C panics with an `ArithmeticError` (`non-finite float returned by <name>`), keeping the finite-only float invariant intact across the FFI boundary (see [Arithmetic Faults](#arithmetic-faults)).

Declare C return types at their true width and let [numeric widening](#numeric-widening) do the rest. A C `int` bound as `Int32` flows directly into `Int` contexts with correct sign extension, so negative error codes survive the trip. Reading a C `int` as `Int` would zero-extend the upper 32 bits and corrupt negative values.

### Symbol Naming

When the C symbol name differs from the Koja function name, use `@link "lib:symbol"` to specify the C symbol after a colon:

```koja
struct Crypto
  @extern "C" @link "crypto:EVP_sha256"
  priv fn evp_sha256() -> CPtr<UInt8>

  @extern "C" @link "crypto:SHA256"
  priv fn sha256_raw(data: CPtr<UInt8>, len: Int64, out: CPtr<UInt8>) -> CPtr<UInt8>
end
```

`@link "crypto"` (without a colon) uses the Koja function name as the C symbol. `@link "crypto:SHA256"` links to the C symbol `SHA256` while the Koja function name is `sha256_raw`. This keeps all Koja function names in proper `snake_case` regardless of the C library's naming conventions.

### `CPtr<T>`

A raw C pointer type. `Copy` semantics (just a machine word). No ownership tracking. The compiler will not auto-free memory behind a `CPtr<T>`.

```koja
struct CPtr<T>
  fn null() -> CPtr<T>
  fn alloc(count: Int) -> CPtr<T>
  fn free(self)
  fn offset(self, n: Int) -> CPtr<T>
  fn read(self) -> T
  fn write(self, value: T)
  fn null?(self) -> Bool
end
```

`alloc` and `free` use C's `malloc` and `free`. All methods are compiler intrinsics.

```koja
buf: CPtr<Int32> = CPtr.alloc(4)
buf.write(42)
buf.read().print()
buf.free()

null_ptr: CPtr<Int32> = CPtr.null()
null_ptr.null?().print()
```

Type annotations on the variable drive generic inference for static methods like `CPtr.alloc()` and `CPtr.null()`.

`CPtr<UInt8>` additionally provides the two ways to get a pointer to a `Binary`'s bytes:

- `CPtr.borrow(bytes: Binary) -> CPtr<UInt8>`: zero-cost view of the binary's payload. The result cannot be bound to a variable, returned, or stored. It may only be consumed within the statement that borrows it (as a call argument or chained receiver), where the source `Binary` is guaranteed to be live.
- `CPtr.copy(bytes: Binary) -> CPtr<UInt8>`: malloc'd owned copy of the bytes. Nameable like any value. The caller frees it. Use this when a C API retains the pointer past the call.

```koja
digest = CPtr.alloc(32)
FFI.blake3_hash(CPtr.borrow(data), data.byte_size(), digest)  # fine

p = CPtr.borrow(data)  # compile error: a borrowed pointer cannot be bound
owned = CPtr.copy(data)  # owned copy, free it when C is done
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

`String.to_cstring() -> Result<CString, CString.ConversionError>`
allocates a null-terminated copy via `malloc` and rejects `String`
values containing U+0000 with `InteriorNul`.
`CString.to_string() -> Result<String, CString.ConversionError>` copies
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

## Annotations

### `@doc`

Documents a function, struct, or enum:

```koja
@doc "Adds two integers."
fn add(a: Int32, b: Int32) -> Int32
  a + b
end
```

`@doc false` excludes an item from generated documentation.

`@doc` on a `priv` declaration is a compile error, since private items never appear in generated documentation.

Doc strings support Markdown and are rendered by `koja doc`.

### `@test`

Marks a function as a test case. `koja test` discovers and runs all `@test`-annotated functions in `src/` and `test/` directories.

```koja
@test
fn test_addition
  result = add(2, 3)
  assert(result == 5, "expected 5")
end

@test "handles negative numbers"
fn test_negative
  result = add(-1, 1)
  assert(result == 0, "expected 0")
end
```

An optional string after `@test` provides a description printed during the test run. Tests abort on first failure.

---

## Tooling

| Command       | Description                                       |
| ------------- | ------------------------------------------------- |
| `koja new`    | Scaffold a new project directory                  |
| `koja build`  | Compile to a native binary via LLVM               |
| `koja run`    | Build and execute in one step                     |
| `koja check`  | Type check without compiling                      |
| `koja test`   | Run `@test`-annotated functions                   |
| `koja format` | Opinionated code formatter (`--write`, `--check`) |
| `koja doc`    | Generate static HTML documentation                |
| `koja lex`    | Dump tokens                                       |
| `koja parse`  | Dump AST                                          |

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
name = "my_app"
version = "0.1.0"
```

Fields:

- `name`: project name (used as the binary output name).
- `version`: semantic version string.
- `entry`: the type implementing `Process` that the program starts (required for `build`/`run`).
- `src`: source directories (default `["src"]`).
- `test`: test directories (default `["test"]`).

### Language Server (LSP)

Real-time diagnostics, document formatting, hover (type signatures + `@doc`), and go-to-definition. Integrates with VS Code / Cursor via a bundled extension.

### Formatter

Zero-config, opinionated. `koja format --write` reformats in place, `koja format --check` exits non-zero if formatting differs. The formatter handles escape re-encoding for round-trip correctness and preserves annotations.
