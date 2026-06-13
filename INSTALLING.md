# Installing the Koja Compiler

This document covers installing prebuilt Koja binaries and building the compiler from source on macOS and Linux. For a quick overview of the language, see [README.md](README.md).

## Prebuilt binaries

Releases from v0.12.1 onward ship prebuilt binaries for macOS arm64 (Apple Silicon) and Linux x86_64 (glibc). Each tarball contains the `koja` compiler and the `koja-lsp` language server. If you're on one of these platforms and don't plan to modify the compiler, this is the recommended path — no Rust or LLVM toolchain required.

### Via asdf (recommended)

```sh
asdf plugin add koja https://github.com/koja-lang/asdf-koja.git
asdf install koja latest
asdf set --home koja latest
koja --version
```

### Manual download

Grab the tarball and `.sha256` sidecar for your platform from the [releases page](https://github.com/koja-lang/koja/releases), then:

```sh
shasum -a 256 -c koja-v0.12.1-darwin-arm64.tar.gz.sha256
tar -xzf koja-v0.12.1-darwin-arm64.tar.gz
cp koja-v0.12.1-darwin-arm64/{koja,koja-lsp} ~/.local/bin/
```

(Use `sha256sum -c` on Linux, and substitute `linux-x86_64` for `darwin-arm64`.)

## Building from source

On other platforms, or to work on the compiler itself, build from source as described in the sections below.

### Requirements

- Rust 1.85 or newer (1.94.1+ recommended — some transitive crates push the practical minimum upward; if you see MSRV errors, run `rustup update stable` or `rustup override set 1.94.1` inside the repo)
- LLVM 18 (specifically 18.1.x — `llvm-sys` will not accept other versions)
- A C toolchain (`cc` / `clang`)

The compiler links LLVM 18 statically through [`llvm-sys`](https://crates.io/crates/llvm-sys), so the LLVM development libraries must be installed and discoverable at build time.

#### Toolchain compatibility

Whatever installed your Rust toolchain also chose your `libstdc++`, and `llvm-sys` will static-link LLVM against that same `libstdc++`. Mixing ecosystems for those two pieces produces a hard-to-diagnose SIGSEGV deep inside the LLVM X86 backend the first time codegen runs. Stick to one of the coherent combinations:

| Rust source                   | LLVM source                       | Status                               |
| ----------------------------- | --------------------------------- | ------------------------------------ |
| Homebrew (macOS or Linuxbrew) | Homebrew (`brew install llvm@18`) | Supported                            |
| rustup                        | apt (`llvm-18-dev`)               | Supported, simplest on Debian/Ubuntu |
| rustup                        | Homebrew                          | Supported                            |
| Homebrew                      | apt                               | **Not supported**                    |

If you previously installed Rust via Homebrew and want to switch to apt LLVM, `brew uninstall rust` and reinstall via [rustup](https://rustup.rs) first.

### macOS (Homebrew)

This is the primary supported configuration.

```sh
brew install llvm@18
git clone https://github.com/koja-lang/koja && cd koja

export LLVM_SYS_181_PREFIX="$(brew --prefix llvm@18)"
export LIBRARY_PATH="$(brew --prefix)/lib:$LIBRARY_PATH"

cargo build -p koja-runtime
cargo build -p koja-runtime --release
cargo build --release -p koja-driver

cp target/release/koja ~/.local/bin/koja
codesign --force -s - ~/.local/bin/koja
```

The two-step `koja-runtime` build is intentional: the driver's `build.rs` searches for `libkoja_runtime.a` in both the debug and release build directories. `codesign` is required on macOS so the kernel doesn't kill the binary on launch.

### Linux (Debian / Ubuntu — rustup + apt)

The simplest path on Linux. Rust comes from rustup (self-contained, doesn't link the system `libstdc++`), and LLVM comes from apt.
Install rustup if you don't already have it:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
```

Install LLVM 18 and every dev dependency `llvm-sys` will need to link:

```sh
sudo apt install -y \
    build-essential cmake pkg-config gdb \
    llvm-18-dev libpolly-18-dev clang-18 libclang-18-dev \
    zlib1g-dev libzstd-dev libtinfo-dev libxml2-dev libffi-dev libz3-dev
```

Pin `llvm-sys` to the apt-installed LLVM and build:

```sh
git clone https://github.com/koja-lang/koja && cd koja

export LLVM_SYS_181_PREFIX="$(llvm-config-18 --prefix)"

cargo build -p koja-runtime
cargo build -p koja-runtime --release
cargo build --release -p koja-driver

cp target/release/koja ~/.local/bin/koja
```

### Linux (Linuxbrew)

Use this path if your Rust toolchain also comes from Homebrew. The build flow mirrors macOS — `brew --prefix` resolves to `/home/linuxbrew/.linuxbrew` instead of `/opt/homebrew`.

```sh
brew install llvm@18
git clone https://github.com/koja-lang/koja && cd koja

export LLVM_SYS_181_PREFIX="$(brew --prefix llvm@18)"
export LIBRARY_PATH="$(brew --prefix)/lib:$LIBRARY_PATH"

cargo build -p koja-runtime
cargo build -p koja-runtime --release
cargo build --release -p koja-driver

cp target/release/koja ~/.local/bin/koja
```

### Verifying the install

```sh
koja run examples/hello.kojs
```

You should see `hello, world!` printed to stdout.

### Troubleshooting

#### `cannot find -lPolly`

Debian splits Polly out of the main LLVM package:

```sh
sudo apt install -y libpolly-18-dev
cargo clean -p llvm-sys
```

#### `cannot find -lz` / `-lzstd` / `-ltinfo` / `-lxml2` / `-lffi` / `-lz3`

LLVM 18 was built against these system libraries, and the linker re-resolves them when you link the compiler. Install whichever development package is missing:

| Linker arg | Debian package |
| ---------- | -------------- |
| `-lz`      | `zlib1g-dev`   |
| `-lzstd`   | `libzstd-dev`  |
| `-ltinfo`  | `libtinfo-dev` |
| `-lxml2`   | `libxml2-dev`  |
| `-lffi`    | `libffi-dev`   |
| `-lz3`     | `libz3-dev`    |

The full apt install line above pre-installs all of these.

#### `command not found: clang` (during `boring-sys` build)

`boring-sys` uses `bindgen`, which loads `libclang.so` to parse C headers. Install both:

```sh
sudo apt install -y clang-18 libclang-18-dev
```

For tooling consistency, match the LLVM version (`-18`) rather than installing the unversioned `clang` metapackage.

#### SIGSEGV inside `llvm::X86ReadAdvanceTable` when running `koja run`

You almost certainly have Homebrew Rust + apt LLVM (the unsupported combination from the toolchain compatibility table). Symptoms are a bare `Segmentation fault` with no Rust output, killing the host driver before it produces the user binary. `koja check` and `koja parse` succeed because they don't reach LLVM.

Confirm the diagnosis:

```sh
which rustc                                       # /home/linuxbrew/.linuxbrew/bin/rustc → bad
ldd ./target/release/koja | grep -iE 'brew|llvm'  # mixed paths → bad
```

Fix by collapsing to a single ecosystem. The cleanest is rustup + apt LLVM:

```sh
brew uninstall rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo clean
cargo build --release
```

Alternatively, if you want to stay on Linuxbrew, install Homebrew LLVM and remove the apt one:

```sh
brew install llvm@18
sudo apt remove --purge llvm-18-dev
unset LLVM_SYS_181_PREFIX
cargo clean
cargo build --release
```

#### Stale `llvm-sys` artifacts after switching LLVM versions

`llvm-sys`'s build script does **not** rerun when you change `LLVM_SYS_181_PREFIX`, so a `cargo build` after `apt install llvm-18-dev` reuses the previously-compiled artifacts and ignores the new install. Force a rebuild:

```sh
cargo clean -p llvm-sys
# or, if that doesn't take:
cargo clean
```

#### Type errors involving `*const i8` / `*mut i8`

Linux aarch64 (e.g. a Debian VM on Apple Silicon) defines `c_char` as `u8`, not `i8`. The repo's runtime crate uses `c_char` everywhere; if you see this in your own modifications, prefer `*const c_char` over `*const i8` in any FFI cast.

#### `dsymutil` warnings on Linux

Harmless — the macOS-only `dsymutil` invocation is gated out at compile time on Linux. If you see a stray reference, you're on stale source; pull and rebuild.
