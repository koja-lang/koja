# Koja

> A language for humans and AI.

Koja is a statically typed, compiled language targeting native binaries via LLVM. It combines Ruby-inspired syntax with Rust-grade ownership semantics and an Erlang-style concurrency model.

For the full language specification, see [LANGUAGE.md](LANGUAGE.md).

## Project Dependencies

- Rust 1.85+
- LLVM 18

## Getting Started

1. Install LLVM 18.

```sh
brew install llvm@18
```

2. Clone the repository.

```sh
git clone https://github.com/hpopp/koja-lang && cd koja-lang/koja
```

3. Build the compiler.

```sh
LLVM_SYS_181_PREFIX=/opt/homebrew/opt/llvm@18 \
LIBRARY_PATH="/opt/homebrew/lib:$LIBRARY_PATH" \
cargo build
```

4. Run the hello world example.

```sh
./target/debug/koja run examples/hello.koja
```

## Language Overview

### Functions

```koja
fn add(a: Int32, b: Int32) -> Int32
  a + b
end

fn main
  add(2, 3).print()
end
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

fn main
  p = Point{x: 3, y: 4}
  p.distance_squared().print()
end
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

fn main
  identity(42).print()
  identity("hello").print()
end
```

### Ownership and Move Semantics

```koja
struct Config
  name: String
end

fn consume(move c: Config) -> String
  c.name
end

fn borrow(c: Config) -> String
  c.name
end

fn main
  c = Config{name: "test"}
  borrow(c)         # borrows -- c is still live
  c.name.print()
  consume(c)         # moves -- c is consumed
end
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

fn main
  double = fn (n: Int32) -> Int32 n * 2 end
  apply(5, double).print()
end
```

### Collections and Iteration

```koja
fn main
  list: List<Int32> = List.new().append(1).append(2).append(3)

  for item in list
    item.print()
  end
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

fn main
  x = 5
  y = x > 2 ? "big" : "small"
  y.print()
  classify(200).print()
end
```

## Contributing

### Testing

Build and run the test suite.

```sh
cargo build && ./target/debug/koja run tests/test_build.koja
```

### Formatting

Koja source files can be formatted with the built-in formatter.

```sh
./target/debug/koja format --write <file.koja>
```

## License

Copyright (c) 2026 Henry Popp

This project is MIT licensed. See the [LICENSE](LICENSE) for details.
