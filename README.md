# Expo

> A language for humans and AI.

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
git clone https://github.com/hpopp/expo-lang && cd expo-lang/expo
```

3. Build the compiler.

```sh
LLVM_SYS_181_PREFIX=/opt/homebrew/opt/llvm@18 \
LIBRARY_PATH="/opt/homebrew/lib:$LIBRARY_PATH" \
cargo build
```

4. Run the hello world example.

```sh
./target/debug/expo run examples/hello.expo
```

## Language Overview

### Functions

```expo
fn add(a: i32, b: i32) -> i32
  a + b
end

fn main()
  print(add(2, 3))
end
```

### Structs and Functions

```expo
struct Point
  x: i32
  y: i32
end

impl Point
  fn distance_squared(self) -> i32
    self.x * self.x + self.y * self.y
  end
end

fn main()
  p = Point(x: 3, y: 4)
  print(p.distance_squared())
end
```

### Control Flow

```expo
fn main()
  sum = 0
  i = 1
  while i <= 10
    sum += i
    i += 1
  end
  print(sum)

  if sum > 50
    print("big")
  else
    print("small")
  end
end
```

## Contributing

### Testing

Build and run the test suite.

```sh
cargo build && ./target/debug/expo run tests/test_build.expo
```

### Formatting

Expo source files can be formatted with the built-in formatter.

```sh
./target/debug/expo format --write <file.expo>
```

## License

Copyright (c) 2026 Henry Popp

This project is MIT licensed. See the [LICENSE](LICENSE) for details.
