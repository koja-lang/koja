# Koja

[![CI](https://github.com/koja-lang/koja/actions/workflows/ci.yml/badge.svg)](https://github.com/koja-lang/koja/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/koja-lang/koja)](https://github.com/koja-lang/koja/releases)
[![Last Updated](https://img.shields.io/github/last-commit/koja-lang/koja.svg)](https://github.com/koja-lang/koja/commits/main)

Koja is a statically typed language for building readable, reliable
services. It pairs Ruby-inspired syntax and Erlang-style concurrency
with Swift-style value semantics and a Rust-inspired type system.
Develop quickly with the interpreter, then ship native LLVM binaries
with deterministic memory management and no garbage collector.

For the full language specification, see [LANGUAGE.md](LANGUAGE.md).

## Installation

The easiest way to install Koja is with [asdf](https://asdf-vm.com). This
ships the `koja` compiler and `koja-lsp` language server for Apple Silicon
and x86-64 and arm64 Linux.

```sh
asdf plugin add koja https://github.com/koja-lang/asdf-koja.git
asdf install koja latest
asdf set --home koja latest
```

Prebuilt tarballs are also attached to every
[release](https://github.com/koja-lang/koja/releases). For other
platforms, or to build from source, see
[INSTALLING.md](INSTALLING.md).

## Getting Started

Write a hello world script and run it:

```sh
echo 'IO.puts("hello, world!")' > hello.kojs
koja run hello.kojs
```

## Language Overview

The snippets below are `.kojs` scripts. Statements run from the top
of the file. Compiled programs (`koja.toml` projects) instead start
from a type implementing `Process`. See [LANGUAGE.md](LANGUAGE.md#concurrency).

### Data and Pattern Matching

```koja
struct User
  name: String
end

enum Event
  Joined(User)
  Left(String)
end

fn describe(event: Event) -> String
  match event
    Event.Joined(user) -> "#{user.name} joined"
    Event.Left(name) -> "#{name} left"
  end
end

describe(Event.Joined(User{name: "Henry"})).print()
```

### Value Semantics

Assignments produce independent values. Heap-backed storage is shared
internally and copied only when one value changes.

```koja
struct Config
  name: String
end

original = Config{name: "development"}
copy = original

copy.name = "production"

original.name.print() # "development"
copy.name.print()     # "production"
```

### Binary Pattern Matching

```koja
fn describe_packet(packet: Binary) -> String
  match packet
    <<tag::8, length::16, payload: Binary>> ->
      "tag #{tag}: #{length} bytes (#{payload.byte_size()} available)"

    _ ->
      "invalid packet"
  end
end

describe_packet(<<1, 5::16, "hello">>).print()
```

### Typed Processes

```koja
alias Process.Step
alias Process.StopReason

enum CounterMsg
  Add(Int)
end

struct Counter
  value: Int
end

impl Process<Int, CounterMsg, Int> for Counter
  fn start(initial: Int) -> Result<Self, StopReason>
    Result.Ok(Counter{value: initial})
  end

  fn handle(self, msg: CounterMsg, from: Option<ReplyTo<Int>>) -> Step<Self>
    match msg
      CounterMsg.Add(amount) ->
        next = self.value + amount
        ReplyTo.reply(from, next)
        Step.Continue(Counter{value: next})
    end
  end
end

counter = spawn Counter.start(40)
counter.cast(CounterMsg.Add(1))

match counter.call(CounterMsg.Add(1), 1000)
  Result.Ok(value) -> value.print()
  Result.Err(_) -> "counter unavailable".print()
end
```

Explore more in the runnable [language tour](examples/tour.kojs), or read
the complete [language reference](LANGUAGE.md).

## Editor Extensions

- **Vim** - [`vim-koja`](https://github.com/koja-lang/vim-koja) (syntax and indentation)
- **VS Code** - [`vscode-koja`](https://github.com/koja-lang/vscode-koja)
- **Zed** - [`zed-koja`](https://github.com/koja-lang/zed-koja)

Any editor with a tree-sitter or LSP client can integrate directly
against the grammar and `koja-lsp`.

## Contributing

Working on the compiler requires building from source. See
[INSTALLING.md](INSTALLING.md) for toolchain setup (Rust 1.85+, LLVM 18).

### Testing

Build and run the test suite.

```sh
cargo build -p koja-runtime-posix
cargo build --release -p koja-runtime-posix
cargo test --workspace -- --test-threads=4
```

### Formatting

Koja source files can be formatted with the built-in formatter.

```sh
koja format --write <file.koja>
```

Compiler formatting and lint checks use Cargo:

```sh
cargo fmt --check
cargo clippy --workspace
```

See [the CI workflow](.github/workflows/ci.yml) for the complete compiler,
language, and standard library test matrix.

## License

Copyright (c) 2026 Henry Popp

This project is MIT licensed. See the [LICENSE](LICENSE) for details.
