# Expo Language Reference

Expo is a statically typed, compiled language targeting native binaries via LLVM. It combines Ruby-inspired syntax with Rust-grade ownership semantics and an Erlang-style concurrency model. The compiler is a Rust workspace; the language compiles to native code with no runtime garbage collector.

---

## Table of Contents

- [Lexical Structure](#lexical-structure) -- Comments, Identifiers, Keywords, Operators, Numeric Literals, Line Continuation
- [Variables and Constants](#variables-and-constants) -- Assignment, Type Annotations, Compound Assignment, Constants
- [Functions](#functions) -- Declaration, Private Functions, `return`, Parameters and Ownership
- [Control Flow](#control-flow) -- `if`/`else`, `while`, `loop`/`break`, `for`...`in`, Ternary
- [Types](#types) -- Primitives, Unit, Strings, Structs, Enums, Union Types, Generics
- [Pattern Matching](#pattern-matching) -- `match`, OR Patterns, `cond`
- [Closures and Function Types](#closures-and-function-types) -- Block Closures, Short Closures, Capture Semantics, Function Types
- [Ownership and Borrowing](#ownership-and-borrowing) -- Rules, `clone()`, Drop Insertion, Copy Types
- [Protocols](#protocols) -- Behavioral Contracts, Static Dispatch
- [Modules](#modules) -- Transparent Files, Visibility
- [Concurrency](#concurrency) -- Processes, `spawn`/`receive`, `Ref`, `ReplyTo`, `Task`
- [Standard Library](#standard-library) -- Built-in Functions, Core Types, Collections, String Methods, Binary/Bits, File I/O, Parsing, Protocols
- [Annotations](#annotations) -- `@doc`
- [Planned Features](#planned-features) -- Arena Blocks, Display, Struct Destructuring, Trait Bounds, `command`
- [Tooling](#tooling) -- CLI Commands, LSP, Formatter

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
arena, break, cond, const, else, end, enum, false, fn, for,
if, impl, in, loop, match, move, not, priv, protocol,
receive, return, self, shared, spawn, struct, true, type, unless, when
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

`<>` concatenates `String`, `Binary`, and `Bits` values. Both operands must be the same type -- no cross-type mixing.

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

## Variables and Constants

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

### Constants

Module-level constants are declared with `const`. Values can be literals (int, float, string, bool), enum unit variants, or struct literals whose fields are all constant expressions:

```expo
const MAX = 100
const PI = 3.14
const NAME = "expo"
const DEBUG = false
const HEADING = Direction.North
const ORIGIN = Point{x: 0, y: 0}
```

An optional type annotation is supported for generic inference:

```expo
const EMPTY: Option<Int> = Option.None
```

Constants are inlined at every usage site.

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

Iterates over any type implementing the `Enumeration<T>` protocol:

```expo
list: List<Int32> = List.new()
list = list.append(1)
list = list.append(2)
list = list.append(3)

for item in list
  print(item)
end
```

The loop variable is bound directly to each element -- no unwrapping needed.

### Ternary

```expo
y = x > 2 ? "big" : "small"
```

Nested ternaries are disallowed.

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
| `Binary`  | Arbitrary byte sequence                   |
| `Bits`    | Arbitrary bit sequence                    |
| `()`      | Unit type (empty value)                   |

All numeric primitives and `Bool` are **copy types** -- assignment duplicates the value. `String`, `Binary`, `Bits`, structs, and enums are **move types** -- assignment transfers ownership.

### Unit Expression

`()` is the unit value. Use `else -> ()` in `cond` for side-effect-only fallthrough.

### Strings

#### Single-Line Strings

```expo
"hello world"
"tab:\there"
"quote: \"yes\""
"backslash: \\"
```

Escape sequences: `\"`, `\\`, `\n`, `\r`, `\t`, `\#`.

#### String Interpolation

```expo
name = "expo"
print("hello #{name}")
print("1 + 2 = #{1 + 2}")
```

Interpolation expressions are enclosed in `#{}` and can contain any expression.

#### Multiline Strings

Triple-quoted strings with automatic dedent based on closing delimiter position:

```expo
msg = """
  first line
  second line
  """
```

Multiline strings support the same escape sequences and interpolation as single-line strings.

### Structs

#### Declaration

```expo
struct Point
  x: Int32
  y: Int32
end
```

#### Construction

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

#### Field Access

```expo
print(p.x)
print(p.y)
```

#### Impl Functions

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
  fn append(move self, item: T) -> List<T>
    # owns self, can mutate
    self
  end
end

list = list.append(42)  # move in, get back
```

#### Static Functions

Functions in `impl` blocks without `self` are called on the type directly:

```expo
impl List<T>
  fn new() -> List<T>
    # ...
  end
end

list: List<Int32> = List.new()
```

### Enums

#### Variants

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

#### Construction

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

### Union Types

A value that can be one of several types. Use `|` between types:

```expo
fn display(item: Post | Comment | Ad) -> String
  match item
    _ -> "an item"
  end
end
```

Use `type` to name a union:

```expo
type Pet = Cat | Dog | Fish
```

A member type widens to the union automatically:

```expo
c = Cat{name: "Whiskers"}
pet: Pet = c
```

Order doesn't matter -- `Post | Comment` and `Comment | Post` are the same type.

### Generics

#### Generic Functions

```expo
fn identity<T>(x: T) -> T
  x
end

print(identity(42))
print(identity("hello"))
```

Type arguments are inferred at call sites from arguments and type annotations.

#### Generic Structs

```expo
struct Pair<A, B>
  first: A
  second: B
end

p = Pair{first: 10, second: "hello"}
```

#### Generic Enums

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

#### Annotation-Driven Inference

Type annotations on variables drive generic type inference:

```expo
list: List<Int32> = List.new()  # infers T = Int32
```

#### Implementation

Generics compile via monomorphization -- the compiler generates specialized native code for each concrete type instantiation. Unused instantiations produce no binary output.

---

## Pattern Matching

### `match`

Pattern matching with exhaustiveness checking:

```expo
result = match x
  1 -> "one"
  2 -> "two"
  _ -> "other"
end
```

Patterns: literals (integers, floats, booleans, strings), wildcards (`_`), variable bindings, nested patterns, enum variant destructuring. Guards use `when`:

```expo
match x
  Option.Some(v) when v > 5 -> "big"
  Option.Some(_) -> "small"
  Option.None -> "none"
end
```

String literals can be used as patterns:

```expo
fn classify(c: String) -> String
  match c
    "0" -> "zero"
    "1" -> "one"
    _ -> "other"
  end
end
```

OR patterns combine multiple patterns in a single arm with `|`:

```expo
match n
  1 | 2 | 3 -> "small"
  4 | 5 | 6 -> "medium"
  _ -> "large"
end
```

Variable bindings inside OR patterns are disallowed.

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

---

## Closures and Function Types

### Block Closures

Closures use `fn (...) -> T ... end` syntax, mirroring function signatures:

```expo
double = fn (x: Int32) -> Int32 x * 2 end

add = fn (a: Int32, b: Int32) -> Int32
  a + b
end
```

### Short Closures

Short closures use `param -> expr` syntax with parameter types inferred from the calling context:

```expo
option.map(x -> x + 1)
list.filter(n -> n > 3)
names.map(name -> name.upcase())
```

Works at inline call sites including generic methods. For multi-parameter or multi-statement closures, use the block form above.

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

### Function Types

Function types are written as `fn(ParamTypes) -> ReturnType`:

```expo
fn apply(x: Int32, f: fn(Int32) -> Int32) -> Int32
  f(x)
end

print(apply(5, fn (n: Int32) -> Int32 n * 2 end))
```

#### `move` in Function Types

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

### Field Access

Field access is always a borrow -- it never moves the struct or the field. You can read fields freely without consuming the owner:

```expo
struct Wrapper
  name: String
  count: Int
end

w = Wrapper{name: "hello", count: 1}
print(w.name)    # borrows name
print(w.count)   # borrows count
print(w.name)    # w is still live -- no move occurred
```

This extends to chained access and method calls. Calling a borrow-`self` method through a field borrows the field through the struct:

```expo
w.name.length()   # borrows name, calls length -- w is still live
```

To mutate a field, use reassignment. The right-hand side borrows the field, transforms it, and the result is written back:

```expo
w.name = w.name.upcase()
print(w.name)              # "HELLO"
```

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

## Modules

Each `.expo` file is a module. In a project (defined by `expo.toml`), all types and public functions across all files are visible in every file -- no imports needed.

```expo
# src/helper.expo
fn add(a: Int, b: Int) -> Int
  a + b
end

# src/main.expo
fn main
  print(Helper.add(3, 4))
end
```

### Visibility

Access control is at the function level (`priv fn`), not the module level. Private functions are only callable within the file that defines them.

---

## Concurrency

Expo uses a message-passing actor model inspired by Erlang/Elixir. Processes have isolated memory and communicate exclusively through typed messages. Messages are moved (ownership transfer, zero-copy) -- there is no shared mutable state.

### `Task<R>`

The simplest way to run concurrent work. Wraps a closure, runs it in a spawned process, and returns the result:

```expo
ref = Task.async(fn -> expensive_computation() end)
result = Task.await(ref)  # Option<R>, times out after 5000ms
```

`Task.async(fn)` spawns the closure and returns a `Ref<(), R>`. `Task.await(ref)` sends a unit message and waits for the reply.

### `Process<C, M, R>` Protocol

For stateful, long-lived processes, implement the `Process` protocol. `C` is the config type, `M` is the message type, `R` is the reply type.

```expo
protocol Process<C, M, R>
  fn new(config: C) -> Self
  fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Self
  fn run(move self)
end
```

`run` has a default implementation that enters a receive loop, dispatching each incoming message to `handle`:

```expo
fn run(move self)
  receive
    pair: Pair<M, Option<ReplyTo<R>>> ->
      self.handle(pair.first, pair.second).run()
  end
end
```

A complete process example:

```expo
enum CounterMsg
  Increment
  Decrement
end

struct Counter
  count: Int
end

impl Process<Counter, CounterMsg, Int> for Counter
  fn new(config: Counter) -> Self
    config
  end

  fn handle(move self, msg: CounterMsg, from: Option<ReplyTo<Int>>) -> Self
    match msg
      CounterMsg.Increment -> self.count += 1
      CounterMsg.Decrement -> self.count -= 1
    end
    reply(from, self.count)
    self
  end
end

ref = spawn Counter.new(Counter{count: 0})
ref.cast(CounterMsg.Increment)
count = ref.call(CounterMsg.Increment, 5000)
```

### `Ref<M, R>`

`spawn` returns a typed handle to the running process. `M` is the message type the process accepts; `R` is the reply type.

```expo
struct Ref<M, R>
  id: Int
end
```

Two ways to send messages:

- `cast(msg: M)` -- fire-and-forget. The handler receives `from = Option.None`.
- `call(msg: M, timeout: Int) -> Option<R>` -- sends a message and blocks up to `timeout` milliseconds for a reply. Returns `Option.Some(reply)` on success, `Option.None` on timeout.

```expo
ref.cast(CounterMsg.Increment)
result = ref.call(CounterMsg.Increment, 5000)
```

### `ReplyTo<R>` and `reply`

When a process receives a `call`, the handler gets a `ReplyTo<R>` channel to send the response back. The type `R` is enforced at compile time.

```expo
struct ReplyTo<R>
  id: Int
end
```

- `send(reply: R)` -- sends the reply back to the caller.

The `reply` convenience function handles the common pattern of replying only when a caller is present (skips silently for `cast` messages):

```expo
fn reply<R>(from: Option<ReplyTo<R>>, value: R)
```

### `spawn` and `receive`

The underlying keywords that power the process model. `spawn` creates a new lightweight process and returns a `Ref`. `receive` blocks the current process until a message arrives:

```expo
receive
  pair: Pair<M, Option<ReplyTo<R>>> ->
    # handle the message
end
```

In most cases you won't use `receive` directly -- the `Process` protocol's default `run` implementation handles it for you.

---

## Standard Library

The following types and functions are available in every module.

### Built-in Functions

#### `print()`

Polymorphic print function. Supports all primitive types. Outputs to stdout with a trailing newline.

```expo
print(42)
print("hello")
print(true)       # prints "true", not "1"
```

#### `panic()`

Prints a message to stderr and aborts the process:

```expo
panic("something went wrong")
```

Used internally by `unwrap()` on `Option.None` and `Result.Err`.

#### `clone()`

Available on all types. Produces a new owned value:

```expo
copy = original.clone()
```

### `Option<T>`

```expo
enum Option<T>
  Some(T)
  None
end
```

Functions: `unwrap()`, `or(default)`, `some?()`, `none?()`, `map(fn(T) -> U)`, `then(fn(T) -> Option<U>)`.

```expo
x = Option.Some(42)
print(x.unwrap())       # 42
print(x.or(0))          # 42
print(x.some?())        # true

y: Option<Int> = Option.None
print(y.or(99))          # 99

mapped = x.map(fn (v: Int) -> Int v * 10 end)
print(mapped.unwrap())   # 420
```

### `Result<T, E>`

```expo
enum Result<T, E>
  Ok(T)
  Err(E)
end
```

Functions: `unwrap()`, `or(default)`, `ok?()`, `err?()`, `map(fn(T) -> U)`, `then(fn(T) -> Result<U, E>)`.

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

### `Range`

An inclusive range with `start` and `stop` endpoints.

```expo
struct Range
  start: Int
  stop: Int
end
```

Used by `String.slice` for substring extraction:

```expo
greeting = "hello world"
hello = greeting.slice(Range{start: 0, stop: 4})
print(hello)  # "hello"
```

### `List<T>`

Dynamically-sized, heap-backed collection. Compiler intrinsic backed by C's `malloc`/`realloc`/`free`.

```expo
list: List<Int32> = List.new()
list = list.append(10)
list = list.append(20)

print(list.length())   # 2
print(list.get(0).unwrap())  # 10
print(list.empty?())   # false
```

`append` uses `move self` semantics -- it returns the updated list. `get` returns `Option<T>` (`None` for out-of-bounds).

Functions:

- `new() -> List<T>` -- creates an empty list.
- `append(move self, item: T) -> List<T>` -- appends an element.
- `last(self) -> Option<T>` -- returns the last element, or `None` if empty.
- `length(self) -> Int` -- returns the number of elements.
- `get(self, index: Int) -> Option<T>` -- returns the element at `index`, or `None` if out of bounds.
- `empty?(self) -> Bool` -- returns `true` if the list has no elements.
- `map(self, f: fn(T) -> U) -> List<U>` -- returns a new list with `f` applied to each element.
- `filter(self, f: fn(T) -> Bool) -> List<T>` -- returns elements for which `f` returns `true`.
- `any?(self, f: fn(T) -> Bool) -> Bool` -- returns `true` if `f` returns `true` for at least one element.
- `all?(self, f: fn(T) -> Bool) -> Bool` -- returns `true` if `f` returns `true` for every element. Returns `true` for an empty list.

```expo
nums = [1, 2, 3, 4, 5]
doubled = nums.map(fn (n: Int) -> Int n * 2 end)
evens = nums.filter(fn (n: Int) -> Bool n % 2 == 0 end)
has_big = nums.any?(fn (n: Int) -> Bool n > 3 end)
all_pos = nums.all?(fn (n: Int) -> Bool n > 0 end)
```

List literals (`[a, b, c]`) are backed by the `ListLiteral<T>` protocol. See [Literal Protocols](#literal-protocols).

### `Map<K, V>`

A generic hash map. Keys must implement `Hash` and `Equality`. Uses open addressing with linear probing.

```expo
m: Map<String, Int> = Map.new()
m = m.put("a", 1)
m = m.put("b", 2)

print(m.get("a").unwrap())  # 1
print(m.has?("b"))          # true
print(m.length())           # 2
```

Functions:

- `new() -> Map<K, V>` -- creates an empty map.
- `put(move self, key: K, value: V) -> Map<K, V>` -- inserts or updates a key-value pair.
- `get(self, key: K) -> Option<V>` -- returns `Option.Some(value)` if the key exists, `Option.None` otherwise.
- `has?(self, key: K) -> Bool` -- returns `true` if the key exists.
- `remove(move self, key: K) -> Map<K, V>` -- removes the entry for the key. Returns the map unchanged if the key is absent.
- `length(self) -> Int` -- returns the number of entries.
- `empty?(self) -> Bool` -- returns `true` if the map has no entries.

Map literals (`[key: value, ...]`) are backed by the `MapLiteral<K, V>` protocol. See [Literal Protocols](#literal-protocols).

### `Set<T>`

A generic hash set of unique elements. Elements must implement `Hash` and `Equality`. Uses open addressing with linear probing.

```expo
s: Set<Int> = Set.new()
s = s.insert(1)
s = s.insert(2)
s = s.insert(1)

print(s.length())   # 2
print(s.has?(1))     # true
```

Functions:

- `new() -> Set<T>` -- creates an empty set.
- `insert(move self, item: T) -> Set<T>` -- adds an element. Returns unchanged if already present.
- `has?(self, item: T) -> Bool` -- returns `true` if the element exists.
- `remove(move self, item: T) -> Set<T>` -- removes the element. Returns unchanged if absent.
- `length(self) -> Int` -- returns the number of elements.
- `empty?(self) -> Bool` -- returns `true` if the set has no elements.

`Set<T>` implements `ListLiteral<T>`, so list literal syntax constructs a set when the target type is `Set<T>`:

```expo
names: Set<String> = ["alice", "bob", "alice"]  # Set with 2 elements
```

### String Methods

`String` implements `Enumeration<String>`, so strings can be iterated character-by-character with `for`:

```expo
for c in "hello"
  print(c)
end
```

Functions:

- `length(self) -> Int` -- returns the number of Unicode codepoints.
- `get(self, index: Int) -> Option<String>` -- returns the single-character string at the given index, or `None` if out of bounds.
- `alpha?(self) -> Bool` -- returns `true` if the string contains only ASCII alphabetic characters (a-z, A-Z).
- `at(self, index: Int) -> Option<String>` -- alias for `get`.
- `byte_length(self) -> Int` -- returns the number of bytes in the UTF-8 encoding.
- `codepoints(self) -> List<String>` -- returns each Unicode codepoint as a single-character string in a list.
- `contains?(self, other: String) -> Bool` -- returns `true` if the string contains `other` as a substring.
- `digit?(self) -> Bool` -- returns `true` if the string contains only numeric characters (`0`-`9`).
- `downcase(self) -> String` -- returns a copy with ASCII uppercase letters converted to lowercase.
- `empty?(self) -> Bool` -- returns `true` if the string has zero length.
- `ends_with?(self, suffix: String) -> Bool` -- returns `true` if the string ends with `suffix`.
- `graphemes(self) -> List<String>` -- returns each grapheme cluster as a string in a list. Currently equivalent to `codepoints()`.
- `join(parts: List<String>, separator: String) -> String` -- static. Joins a list of strings with `separator` between each element.
- `replace(self, old: String, new: String) -> String` -- replaces all occurrences of `old` with `new`.
- `reverse(self) -> String` -- returns a copy with the codepoints in reverse order.
- `slice(self, range: Range) -> String` -- returns a substring spanning the given inclusive range of character indices. Clamps out-of-bounds endpoints.
- `split(self, separator: String) -> List<String>` -- splits on each occurrence of `separator`. An empty separator splits into individual characters.
- `starts_with?(self, prefix: String) -> Bool` -- returns `true` if the string starts with `prefix`.
- `to_binary(self) -> Binary` -- zero-cost conversion to `Binary` (every valid UTF-8 string is a valid byte sequence).
- `to_float(self) -> Result<Float, String>` -- parses the string as a 64-bit float.
- `to_int(self) -> Result<Int, String>` -- parses the string as a 64-bit signed integer.
- `trim(self) -> String` -- returns a copy with leading and trailing whitespace removed.
- `trim_end(self) -> String` -- returns a copy with trailing whitespace removed.
- `trim_start(self) -> String` -- returns a copy with leading whitespace removed.
- `upcase(self) -> String` -- returns a copy with ASCII lowercase letters converted to uppercase.
- `whitespace?(self) -> Bool` -- returns `true` if the string contains only whitespace characters (space, `\n`, `\r`, `\t`).

```expo
s = "hello world"
print(s.length())                            # 11
print(s.get(0).unwrap())                     # "h"
print(s.contains?("world"))                  # true
print(s.starts_with?("hello"))               # true
print(s.split(" ").length())                 # 2
print(s.upcase())                            # "HELLO WORLD"
print(s.slice(Range{start: 0, stop: 4}))     # "hello"
print("  hello  ".trim())                    # "hello"
```

`String` also implements `Equality` (content comparison via `==`) and `Hash` (FNV-1a).

### Binary and Bits

`Binary` represents an arbitrary byte sequence. `Bits` represents an arbitrary bit sequence. Both are move types.

#### Literals

Binary and bitstring literals use `<<>>` syntax with comma-separated segments:

```expo
header = <<0x48, 0x65, 0x6C, 0x6C, 0x6F>>
wide = <<0x0102::16>>
le = <<0x0102::16 little>>
neg = <<-1::8 signed>>
msg = <<0x01, port::16>>
```

Segment modifiers: `::N` (bit width), `::N byte` (byte width), `signed`/`unsigned`, `big`/`little`, type annotations (`: Float32`, `: Int16`). Byte-aligned totals produce `Binary`, non-byte-aligned produce `Bits`. String literals can appear as segments for protocol framing.

#### Pattern Matching

Binary patterns destructure byte sequences in `match`:

```expo
match packet
  <<tag::8, length::16, rest: Binary>> -> handle(tag, rest)
  _ -> print("no match")
end
```

Greedy rest capture with `rest: Binary` consumes all remaining bytes. Patterns that don't match the data length fall through to the next arm.

#### Conversion Functions

- `String.to_binary(self) -> Binary` -- zero-cost widening from UTF-8 string to bytes.
- `Binary.to_string(self) -> Result<String, String>` -- attempts to interpret bytes as UTF-8. Returns `Result.Err` with a diagnostic if invalid.
- `Binary.to_bits(self) -> Bits` -- zero-cost widening from bytes to bits.
- `Bits.to_binary(self) -> Result<Binary, String>` -- narrows bits to bytes. Returns `Result.Err` if the bit length is not divisible by 8.

```expo
bin = "hello".to_binary()
bits = bin.to_bits()
roundtrip = bits.to_binary().unwrap().to_string().unwrap()
print(roundtrip)  # "hello"
```

### File I/O

#### `Fd`

A raw file descriptor for low-level I/O:

```expo
struct Fd
  descriptor: Int
end
```

Functions:

- `read(self, count: Int) -> Result<String, String>` -- reads up to `count` bytes.
- `write(self, data: String) -> Result<Int, String>` -- writes data, returns bytes written.
- `close(move self) -> Result<String, String>` -- closes the descriptor.

#### `File`

Higher-level file operations wrapping `Fd`:

```expo
struct File
  fd: Fd
end
```

Functions:

- `File.open(path: String, mode: FileMode) -> Result<File, String>` -- opens a file with the given mode (`FileMode.Read`, `FileMode.Write`, `FileMode.Append`).
- `File.read(path: String) -> Result<String, String>` -- reads an entire file as a string (opens, reads, closes).
- `File.write(path: String, content: String) -> Result<String, String>` -- writes content to a file (creates or truncates).
- `File.exists?(path: String) -> Bool` -- returns true if the file exists.
- `File.delete(path: String) -> Result<String, String>` -- deletes a file.
- `File.rename(source: String, destination: String) -> Result<String, String>` -- renames (moves) a file.
- `close(move self) -> Result<String, String>` -- closes the file handle.

```expo
content = File.read("config.txt").unwrap()
print(content)
```

### Parsing

Static functions on `Int` and `Float` for parsing strings:

- `Int.parse(input: String) -> Result<Int, String>` -- parses a string as a 64-bit signed integer.
- `Float.parse(input: String) -> Result<Float, String>` -- parses a string as a 64-bit float.

```expo
x = Int.parse("42").unwrap()
print(x)  # 42

y = Float.parse("3.14").unwrap()
print(y)  # 3.14

err = Int.parse("nope")
print(err.err?())  # true
```

### `Enumeration<T>` Protocol

```expo
protocol Enumeration<T>
  fn length(self) -> Int
  fn get(self, index: Int) -> Option<T>
end
```

Any type implementing `Enumeration<T>` can be used with `for` loops. `List<T>` and `String` implement this protocol. `get` returns `Option<T>` instead of panicking on out-of-bounds access. `for` loops unwrap the `Option` automatically.

### `Equality` Protocol

```expo
protocol Equality
  fn eq(self, other: Self) -> Bool
end
```

Powers the `==` and `!=` operators. Implemented for all numeric types, `Bool`, and `String`.

### `Hash` Protocol

```expo
protocol Hash
  fn hash(self) -> Int
end
```

Required for keys in `Map<K, V>` and elements in `Set<T>`. Implemented for all numeric types, `Bool`, and `String`. Integers use SplitMix64; strings use FNV-1a.

### `Bitwise` Protocol

```expo
protocol Bitwise
  fn band(self, other: Self) -> Self
  fn bor(self, other: Self) -> Self
  fn bxor(self, other: Self) -> Self
  fn bnot(self) -> Self
  fn bsl(self, n: Int) -> Self
  fn bsr(self, n: Int) -> Self
end
```

Bitwise operations are methods rather than symbolic operators. Expo reserves `<<`/`>>` for binary literals, `|` for union types, and `&` is unused. All integer types implement `Bitwise`.

```expo
flags = 0b1010
print(flags.band(0b1100))  # 8  (0b1000)
print(flags.bor(0b0001))   # 11 (0b1011)
print(1.bsl(4))             # 16
print(16.bsr(4))            # 1
```

### Literal Protocols

List and map literals are backed by protocols, allowing custom types to opt into literal syntax.

**`ListLiteral<T>`** -- the compiler builds a `List<T>` from `[a, b, c]` and passes it to `from_list`:

```expo
protocol ListLiteral<T>
  fn from_list(move list: List<T>) -> Self
end
```

`List<T>` and `Set<T>` implement `ListLiteral<T>`.

**`MapLiteral<K, V>`** -- the compiler builds a `Map<K, V>` from `[k: v, ...]` and passes it to `from_map`:

```expo
protocol MapLiteral<K, V>
  fn from_map(move map: Map<K, V>) -> Self
end
```

`Map<K, V>` implements `MapLiteral<K, V>`.

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

Doc strings support Markdown and are rendered by `expo doc`.

---

## Planned Features

The following features are designed but not yet compiled to native code. They are parsed and/or type-checked but await codegen implementation.

### `arena` Blocks

Bump-allocated regions with bulk-free semantics:

```expo
result = arena
  # all allocations in here are bulk-freed at block exit
  # only explicitly cloned values escape
end
```

### `Display` Protocol

Auto-derived string representation for all types. `print()` will dispatch through `Display` instead of hardcoding format specifiers per type.

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

Language-native typed pipelines for backend business logic with step-ordered type safety and exhaustive data flow checking.

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
