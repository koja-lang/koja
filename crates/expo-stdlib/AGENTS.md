# expo-stdlib

Embeds `.expo` stdlib sources into the compiler binary.

## Key files

- `build.rs` -- Auto-discovers `.expo` files under `expo/lib/` packages, generates `stdlib_gen.rs` with `include_str!` constants and a `STDLIB_SOURCES` array
- `lib.rs` -- Re-exports the generated constants

## Stdlib source layout

Sources live in `expo/lib/`, organized by package:

- `std/src/` -- Auto-imported core: kernel.expo (Option, Result, List, Map, Set, Pair, Range), string.expo, list.expo, map.expo, set.expo, process.expo, debug.expo, io.expo, fd.expo, cptr.expo, cstring.expo, bitwise.expo, system.expo, time.expo
- `net/src/` -- TCP/UDP/Socket types (qualified: `net.TCPSocket`)
- `crypto/src/` -- SHA family, HMAC (qualified: `crypto.SHA256`)
- `json/src/` -- JSON encoder/decoder (qualified: `json.Value`)
- `http/src/` -- HTTP types and parser (qualified: `http.Request`)

## Adding new stdlib modules

1. Create the `.expo` file in the appropriate `expo/lib/<package>/src/` directory
2. Run `cargo build -p expo-stdlib` -- the build script auto-discovers it
3. Register any new types/functions in `expo-typecheck` collect pass if they need special handling
