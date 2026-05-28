# koja-stdlib

Embeds `.koja` stdlib sources into the compiler binary.

## Key files

- `build.rs` -- Auto-discovers `.koja` files under `koja/lib/` packages, generates `stdlib_gen.rs` with `include_str!` constants and a `STDLIB_SOURCES` array
- `lib.rs` -- Re-exports the generated constants

## Stdlib source layout

Sources live in `koja/lib/`, organized by package:

- `std/src/` -- Auto-imported core: kernel.koja (Option, Result, List, Map, Set, Pair, Range), string.koja, list.koja, map.koja, set.koja, process.koja, debug.koja, io.koja, fd.koja, cptr.koja, cstring.koja, bitwise.koja, system.koja, time.koja
- `net/src/` -- TCP/UDP/Socket types (qualified: `net.TCPSocket`)
- `crypto/src/` -- SHA family, HMAC (qualified: `crypto.SHA256`)
- `json/src/` -- JSON encoder/decoder (qualified: `json.Value`)
- `http/src/` -- HTTP types and parser (qualified: `http.Request`)

## Adding new stdlib modules

1. Create the `.koja` file in the appropriate `koja/lib/<package>/src/` directory
2. Run `cargo build -p koja-stdlib` -- the build script auto-discovers it
3. Register any new types/functions in `koja-typecheck` collect pass if they need special handling
