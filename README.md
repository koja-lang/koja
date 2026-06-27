# Koja

[![CI](https://github.com/koja-lang/koja/actions/workflows/ci.yml/badge.svg)](https://github.com/koja-lang/koja/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/koja-lang/koja)](https://github.com/koja-lang/koja/releases)
[![Last Updated](https://img.shields.io/github/last-commit/koja-lang/koja.svg)](https://github.com/koja-lang/koja/commits/main)

Koja is a statically typed, compiled language targeting native binaries via LLVM, with no garbage collector. It combines a Rust-inspired type system, Swift-style value semantics, an Erlang-style concurrency model, and Ruby-inspired syntax.

For the full language specification, see [LANGUAGE.md](LANGUAGE.md).

## Installation

The easiest way to install Koja is with [asdf](https://asdf-vm.com). This ships prebuilt binaries for macOS arm64 and Linux x86_64 — the `koja` compiler and the `koja-lsp` language server, kept in lockstep:

```sh
asdf plugin add koja https://github.com/koja-lang/asdf-koja.git
asdf install koja latest
asdf set --home koja latest
```

Prebuilt tarballs are also attached to every [release](https://github.com/koja-lang/koja/releases). For other platforms, or to build from source, see [INSTALLING.md](INSTALLING.md).

## Getting Started

Write a hello world script and run it:

```sh
echo 'IO.puts("hello, world!")' > hello.kojs
koja run hello.kojs
```

## Editor Extensions

- **Vim** — [`editors/vim`](editors/vim) (syntax and indentation only; no language server)
- **VS Code** — [`vscode-koja`](https://github.com/koja-lang/vscode-koja)
- **Zed** — [`zed-koja`](https://github.com/koja-lang/zed-koja)

Any editor with a tree-sitter or LSP client can integrate directly against the grammar and `koja-lsp`.

## Language Overview

The snippets below are `.kojs` scripts — statements run from the top
of the file. Compiled programs (`koja.toml` projects) instead start
from a type implementing `Process`; see
[LANGUAGE.md](LANGUAGE.md#modules).

### Functions

```koja
fn add(a: Int32, b: Int32) -> Int32
  a + b
end

add(2, 3).print()
```

### Structs and Functions

```koja
struct Point
  x: Int32
  y: Int32
end

extend Point
  fn distance_squared(self) -> Int32
    self.x * self.x + self.y * self.y
  end
end

p = Point{x: 3, y: 4}
p.distance_squared().print()
```

### Enums and Pattern Matching

```koja
enum Shape
  Circle(Int32)
  Rect(Int32, Int32)
end

fn area(s: Shape) -> Int32
  match s
    Shape.Circle(r) -> r * r * 3
    Shape.Rect(w, h) -> w * h
  end
end
```

### Generics

```koja
fn identity<T>(x: T) -> T
  x
end

identity(42).print()
identity("hello").print()
```

### Values

Variables, parameters, and return values are independent values:
assigning or passing one hands off a logically separate copy. Copies are
cheap -- heap-backed values like `String`, `Binary`, and collections are
shared under the hood and only duplicated when mutated.

```koja
struct Config
  name: String
end

fn describe(c: Config) -> String
  c.name
end

c = Config{name: "test"}
describe(c).print()   # c is passed by value
c.name.print()        # and still usable here
```

### Protocols

```koja
protocol Greeter
  fn greet(self) -> String
end

impl Greeter for Point
  fn greet(self) -> String
    "(#{self.x}, #{self.y})"
  end
end
```

### Closures and Higher-Order Functions

```koja
fn apply(x: Int32, f: fn(Int32) -> Int32) -> Int32
  f(x)
end

double = fn (n: Int32) -> Int32 n * 2 end
apply(5, double).print()
```

### Collections and Iteration

```koja
list: List<Int32> = List.new().append(1).append(2).append(3)

for item in list
  item.print()
end
```

### Control Flow

```koja
fn classify(n: Int32) -> String
  cond
    n > 100 -> "big"
    n > 10 -> "medium"
    else -> "small"
  end
end

x = 5
y = x > 2 ? "big" : "small"
y.print()
classify(200).print()
```

## Contributing

Working on the compiler requires building from source — see [INSTALLING.md](INSTALLING.md) for toolchain setup (Rust 1.85+, LLVM 18).

### Testing

Build and run the test suite.

```sh
cargo build && ./target/debug/koja run tests/test_build.kojs
```

### Formatting

Koja source files can be formatted with the built-in formatter.

```sh
./target/debug/koja format --write <file.koja>
```

## License

Copyright (c) 2026 Henry Popp

This project is MIT licensed. See the [LICENSE](LICENSE) for details.
